//! 路径分量净化：把任意目录路径（可能含 / \ .. : 通配等）映射成**单个安全文件名**，
//! 用作 .mdga/wiki/ 下的 markdown 文件名。
//!
//! 安全不变量（这是安全产品的边界，绝不放松）：
//!   - 产出永远是**单层文件名**，不含路径分隔符（/ 或 \）→ 无法逃出 wiki 目录。
//!   - 不含 `..` 这种父目录引用、不含盘符冒号、不含控制字符与 Windows 保留字符。
//!   - 任意输入都映射到非空、长度有界的名字（空/全非法 → 占位名）。
//! 因此无论 codemap 给出什么目录路径，都不可能写到 .mdga/wiki/ 之外。

/// 把一个工作区相对目录路径净化为 wiki 区段的 markdown 文件名（不含扩展名）。
///
/// 例：`src/api/v2` → `src__api__v2`；`.` → `_root`；`..\evil` → `evil`（`..` 被剥离）。
pub fn dir_to_doc_stem(directory: &str) -> String {
    if directory.is_empty() || directory == "." {
        return "_root".to_string();
    }

    // 统一分隔符后按段处理：剔除空段、`.`、`..`，每段再逐字符净化。
    let mut segments: Vec<String> = Vec::new();
    for raw_seg in directory.split(['/', '\\']) {
        let seg = raw_seg.trim();
        if seg.is_empty() || seg == "." || seg == ".." {
            // `..`/`.` 直接丢弃：既不上跳也不保留，从根本上杜绝越界。
            continue;
        }
        let cleaned = sanitize_segment(seg);
        if !cleaned.is_empty() {
            segments.push(cleaned);
        }
    }

    if segments.is_empty() {
        return "_root".to_string();
    }

    // 用双下划线连接各段，既保留可读层级、又确保结果是单层文件名（无分隔符）。
    let mut name = segments.join("__");
    // 长度封顶，避免极深路径生成超长文件名（部分文件系统单名上限 255）。
    const MAX_LEN: usize = 120;
    if name.chars().count() > MAX_LEN {
        name = name.chars().take(MAX_LEN).collect();
    }
    name
}

/// 逐字符净化单个路径段：仅保留 [A-Za-z0-9._-]，其余（含冒号、通配、控制字符、空白）→ 下划线。
/// 这样段内既不可能含分隔符，也不可能含 Windows 保留字符 `< > : " | ? *`。
fn sanitize_segment(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for ch in seg.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    // 段不能仅由 `.` 组成（避免出现 `.`/`..` 形态的怪文件名）。
    if out.chars().all(|c| c == '.') {
        return "_".to_string();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_and_empty_map_to_placeholder() {
        assert_eq!(dir_to_doc_stem("."), "_root");
        assert_eq!(dir_to_doc_stem(""), "_root");
    }

    #[test]
    fn nested_dir_becomes_single_flat_name() {
        let stem = dir_to_doc_stem("src/api/v2");
        assert_eq!(stem, "src__api__v2");
        // 关键安全断言：产出不含任何路径分隔符。
        assert!(!stem.contains('/'));
        assert!(!stem.contains('\\'));
    }

    /// 路径穿越尝试：`..`、绝对盘符、反斜杠注入都必须被中和，产出仍是单层安全名。
    #[test]
    fn traversal_attempts_are_neutralized() {
        for evil in [
            "../../etc/passwd",
            "..\\..\\windows\\system32",
            "C:\\Windows",
            "/abs/root",
            "a/../../../b",
            "foo/../bar",
        ] {
            let stem = dir_to_doc_stem(evil);
            assert!(!stem.contains('/'), "{evil} → {stem} 不应含 /");
            assert!(!stem.contains('\\'), "{evil} → {stem} 不应含 \\");
            assert!(!stem.contains(".."), "{evil} → {stem} 不应含 ..");
            assert!(!stem.contains(':'), "{evil} → {stem} 不应含盘符冒号");
            assert!(!stem.is_empty(), "{evil} → 不应为空");
        }
    }

    #[test]
    fn windows_reserved_and_wildcard_chars_become_underscore() {
        let stem = dir_to_doc_stem("we*ird?<dir>|name");
        assert!(!stem.contains('*'));
        assert!(!stem.contains('?'));
        assert!(!stem.contains('<'));
        assert!(!stem.contains('>'));
        assert!(!stem.contains('|'));
    }

    #[test]
    fn all_illegal_segments_fall_back_to_root() {
        // 仅由 .. 与分隔符组成 → 无有效段 → 占位名。
        assert_eq!(dir_to_doc_stem("../.."), "_root");
    }

    #[test]
    fn overlong_path_is_truncated() {
        let deep = (0..100).map(|i| format!("seg{i}")).collect::<Vec<_>>().join("/");
        let stem = dir_to_doc_stem(&deep);
        assert!(stem.chars().count() <= 120, "应被长度封顶");
    }
}
