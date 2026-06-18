//! R6：Agent 循环的正确性/安全护栏的纯逻辑——可独立单测，不依赖 Tauri。
//!
//! 两块能力：
//! - [`FileFingerprint`] + [`is_stale`]：陈旧读检测。记录 read_file 当时文件的 mtime+size，
//!   写类编辑前比对磁盘现状；若底层文件在读后被改动（后台 shell / hook / steering 等），
//!   提示模型在基于旧内容编辑，让它自行决定是否重新 read_file（warn 而非硬拦）。
//! - [`SequenceLoopDetector`]：序列级 doom-loop 检测。在既有「单次相同调用」检测之外，
//!   识别最近 (tool,args) 调用里「长度 2..=N 的窗口连续重复 K 次」的死循环，
//!   命中即让调用方走既有 agent-stuck 暂停路径，迫使模型重新规划而非空转打转。

use std::path::Path;

/// 文件指纹：读取当时的修改时间（毫秒，可能取不到）与字节大小。
///
/// mtime 与 size 任一不同即视为「文件已变」。mtime 在个别平台/文件系统可能拿不到（None），
/// 此时退化为只比 size——比无检测强，且不会把「拿不到 mtime」误判成已改动。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileFingerprint {
    /// 文件修改时间，自 UNIX_EPOCH 起的毫秒。平台/文件系统不支持时为 None。
    pub mtime_ms: Option<u64>,
    /// 文件字节大小。
    pub size: u64,
}

impl FileFingerprint {
    /// 对一个绝对路径取指纹。路径不存在 / 非普通文件 / 元数据读取失败时返回 None
    /// （调用方据此跳过记录或跳过比对，宁可不检测也不误报）。
    pub fn of_path(abs_path: impl AsRef<Path>) -> Option<FileFingerprint> {
        let meta = std::fs::metadata(abs_path).ok()?;
        if !meta.is_file() {
            return None;
        }
        let size = meta.len();
        let mtime_ms = meta.modified().ok().and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_millis() as u64)
        });
        Some(FileFingerprint { mtime_ms, size })
    }
}

/// 陈旧读判定：记录的指纹与当前磁盘指纹是否「不一致」（文件在读后被改动）。
///
/// 规则：
/// - size 不同 → 已改动。
/// - size 相同且双方都有 mtime → 比 mtime，不同即已改动。
/// - 任一方缺 mtime → 仅凭 size 相等就视为「未变」（避免缺 mtime 误报）。
///
/// 输入两个指纹，输出 true 表示磁盘内容自记录以来很可能已变、应提醒重新读取。
pub fn is_stale(recorded: &FileFingerprint, current: &FileFingerprint) -> bool {
    if recorded.size != current.size {
        return true;
    }
    match (recorded.mtime_ms, current.mtime_ms) {
        (Some(a), Some(b)) => a != b,
        // 缺任一 mtime：size 又相等，保守地当作未变。
        _ => false,
    }
}

/// 序列级 doom-loop 检测器：在固定容量的环形历史里记录最近的调用签名，
/// 每次 [`record`](Self::record) 后判断「末尾是否恰好是一个长度 `w`（`2..=max_window`）
/// 的窗口连续重复了 `min_repeats` 次」。命中即认为 agent 在以固定节律的循环打转。
///
/// 例：签名序列 A B A B A B（窗口长 2、重复 3 次）或 A B C A B C A B C
///（窗口长 3、重复 3 次）会命中（默认 `min_repeats=3`）。
///
/// 与既有「单次相同调用连续失败」检测正交：那个只看「同一调用反复」，
/// 这个能抓「两三步组成的循环反复」（如 read→edit→read→edit… 反复无收敛）。
#[derive(Clone, Debug)]
pub struct SequenceLoopDetector {
    history: Vec<String>,
    /// 检测的最大窗口长度（窗口 2..=max_window）。
    max_window: usize,
    /// 一个窗口连续重复多少次判定为循环。
    min_repeats: usize,
    /// 历史保留的最大长度（环形截断），至少要能容纳 max_window*min_repeats。
    capacity: usize,
}

impl Default for SequenceLoopDetector {
    fn default() -> Self {
        // 默认：窗口 2~3，连续重复 3 次判循环；历史保留 16 条够覆盖 3×3 窗口且不膨胀。
        Self::new(3, 3, 16)
    }
}

impl SequenceLoopDetector {
    /// 自定义参数构造。`capacity` 会被抬到至少能容纳一次命中所需的 `max_window*min_repeats`。
    pub fn new(max_window: usize, min_repeats: usize, capacity: usize) -> Self {
        let max_window = max_window.max(1);
        let min_repeats = min_repeats.max(2);
        let capacity = capacity.max(max_window * min_repeats);
        SequenceLoopDetector {
            history: Vec::with_capacity(capacity),
            max_window,
            min_repeats,
            capacity,
        }
    }

    /// 记录一次调用签名并返回是否命中循环。
    ///
    /// 签名建议取 `format!("{tool}|{args}")`（与既有失败签名口径一致）。
    /// 命中后历史会被清空，避免同一暂停被反复触发；调用方应据返回 true 走暂停路径。
    pub fn record(&mut self, signature: impl Into<String>) -> bool {
        self.history.push(signature.into());
        // 环形截断：只保留最近 capacity 条。
        if self.history.len() > self.capacity {
            let overflow = self.history.len() - self.capacity;
            self.history.drain(0..overflow);
        }
        if self.detect() {
            self.history.clear();
            return true;
        }
        false
    }

    /// 纯检测：末尾是否存在「长度 w 的窗口连续重复 min_repeats 次」。
    fn detect(&self) -> bool {
        let n = self.history.len();
        for w in 1..=self.max_window {
            let needed = w * self.min_repeats;
            if n < needed {
                continue;
            }
            // 取末尾 needed 条，检查它是否由长度 w 的同一窗口拼成。
            let tail = &self.history[n - needed..];
            let window = &tail[0..w];
            let mut all_match = true;
            for rep in 1..self.min_repeats {
                let seg = &tail[rep * w..rep * w + w];
                if seg != window {
                    all_match = false;
                    break;
                }
            }
            if all_match {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 陈旧读 ──────────────────────────────────────────────────────────────

    #[test]
    fn stale_when_size_differs() {
        let a = FileFingerprint { mtime_ms: Some(100), size: 10 };
        let b = FileFingerprint { mtime_ms: Some(100), size: 11 };
        assert!(is_stale(&a, &b), "大小不同必判已改动");
    }

    #[test]
    fn stale_when_mtime_differs_same_size() {
        let a = FileFingerprint { mtime_ms: Some(100), size: 10 };
        let b = FileFingerprint { mtime_ms: Some(200), size: 10 };
        assert!(is_stale(&a, &b), "大小同但 mtime 变 → 已改动");
    }

    #[test]
    fn not_stale_when_identical() {
        let a = FileFingerprint { mtime_ms: Some(100), size: 10 };
        let b = FileFingerprint { mtime_ms: Some(100), size: 10 };
        assert!(!is_stale(&a, &b), "完全相同 → 未变");
    }

    #[test]
    fn not_stale_when_mtime_missing_but_size_equal() {
        // 缺 mtime 退化为只比 size：size 相等就保守当作未变，不误报。
        let a = FileFingerprint { mtime_ms: None, size: 10 };
        let b = FileFingerprint { mtime_ms: Some(999), size: 10 };
        assert!(!is_stale(&a, &b), "任一方缺 mtime 且 size 相等 → 不误判为已改动");
        let c = FileFingerprint { mtime_ms: None, size: 10 };
        assert!(!is_stale(&a, &c));
    }

    #[test]
    fn fingerprint_of_real_file_changes_on_rewrite() {
        // 真实文件：写后取指纹，改大小后再取，应判为已改动。
        let dir = std::env::temp_dir().join(format!(
            "mdga-r6-stale-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        std::fs::write(&f, b"hello").unwrap();
        let fp1 = FileFingerprint::of_path(&f).expect("应能取到指纹");
        assert_eq!(fp1.size, 5);
        // 改内容（大小不同）后再取，必判 stale（不依赖 mtime 时钟分辨率）。
        std::fs::write(&f, b"hello world!!").unwrap();
        let fp2 = FileFingerprint::of_path(&f).expect("应能取到指纹");
        assert!(is_stale(&fp1, &fp2), "重写后大小变化应判为陈旧");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fingerprint_none_for_missing_path() {
        let missing = std::env::temp_dir().join("mdga-r6-definitely-missing-xyz-12345");
        assert!(FileFingerprint::of_path(missing).is_none());
    }

    // ── 序列级 doom-loop ────────────────────────────────────────────────────

    #[test]
    fn detects_two_step_cycle() {
        // A B A B A B：窗口长 2 重复 3 次 → 命中（默认 min_repeats=3）。
        let mut d = SequenceLoopDetector::default();
        assert!(!d.record("A"));
        assert!(!d.record("B"));
        assert!(!d.record("A"));
        assert!(!d.record("B"));
        assert!(!d.record("A"));
        assert!(d.record("B"), "A B 重复 3 次应命中");
    }

    #[test]
    fn detects_three_step_cycle() {
        // A B C ×3：窗口长 3 重复 3 次 → 命中。
        let mut d = SequenceLoopDetector::default();
        let seq = ["A", "B", "C", "A", "B", "C", "A", "B"];
        for s in seq {
            assert!(!d.record(s));
        }
        assert!(d.record("C"), "A B C 重复 3 次应命中");
    }

    #[test]
    fn detects_single_step_cycle() {
        // A A A：窗口长 1 重复 3 次 → 命中（覆盖既有「同调用反复」语义，正交但兼容）。
        let mut d = SequenceLoopDetector::default();
        assert!(!d.record("A"));
        assert!(!d.record("A"));
        assert!(d.record("A"));
    }

    #[test]
    fn no_false_positive_on_progress() {
        // 不断变化的调用不应命中。
        let mut d = SequenceLoopDetector::default();
        for s in ["A", "B", "C", "D", "E", "F", "G", "H", "A", "B"] {
            assert!(!d.record(s), "持续推进的不同调用不应误判循环");
        }
    }

    #[test]
    fn near_miss_two_repeats_does_not_trip() {
        // A B A B（只重复 2 次）未达默认阈值（3），不命中。
        let mut d = SequenceLoopDetector::default();
        assert!(!d.record("A"));
        assert!(!d.record("B"));
        assert!(!d.record("A"));
        assert!(!d.record("B"));
    }

    #[test]
    fn resets_after_trip() {
        // 命中后历史清空：紧接的同样两步不会立刻再次命中，需重新积累。
        let mut d = SequenceLoopDetector::default();
        for s in ["A", "B", "A", "B", "A"] {
            d.record(s);
        }
        assert!(d.record("B"), "应在第 3 次重复命中");
        // 命中后清空，再来一组未达阈值不命中。
        assert!(!d.record("A"));
        assert!(!d.record("B"));
    }

    #[test]
    fn custom_params_two_repeats() {
        // 自定义 min_repeats=2：A B A B 即命中。
        let mut d = SequenceLoopDetector::new(3, 2, 16);
        assert!(!d.record("A"));
        assert!(!d.record("B"));
        assert!(!d.record("A"));
        assert!(d.record("B"));
    }
}
