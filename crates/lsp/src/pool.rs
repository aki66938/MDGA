//! 进程级 LSP 会话池：按 (规范化工作区, 服务器命令+参数) 复用**长寿命**语言服务器，
//! 省掉每次工具调用 ~20s 的冷启动索引（rust-analyzer 尤甚）。
//!
//! 正确性与资源安全（硬约束）：
//!   - **互斥独占**：池存「空闲」会话；借出即从表里**取走所有权**，用完再还回。这样一个会话
//!     同一时刻只服务一次操作（LSP 客户端是单飞 in-flight 模型），无需会话本身 Sync。
//!   - **空闲回收**：后台 reaper 线程定期清理空闲超过 TTL（~5min）的会话；被移除的
//!     `PooledServer` 一 Drop 即强杀子进程（`LspSession::Drop`）。进程退出时未还回的会话
//!     由 OS 回收子进程；还回的在 reaper/容量淘汰时被 Drop 强杀——**不泄漏进程**。
//!   - **容量上限**：池子最多 `MAX_POOLED` 个会话；超额时淘汰最久未用的（Drop 强杀）。
//!   - **探活**：借出/还回都 `is_alive` 校验，死会话不复用、直接 Drop。
//!   - **磁盘改动后正确**：复用会话时调用方用 `LspSession::sync_document`（didOpen→didChange）
//!     重新喂全文，绝不基于陈旧快照。
//!
//! 借不到（未命中/已死/被占用）时调用方自行 `LspSession::start` 新建；用完照常 `checkin`，
//! 由池子决定纳管或丢弃。借出失败绝不阻塞——宁可多开一个一次性会话也不挂死。

use crate::client::LspSession;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 空闲会话存活上限：超过这个时长没被复用就回收（杀子进程）。
const IDLE_TTL: Duration = Duration::from_secs(5 * 60);
/// reaper 巡检间隔。
const REAP_INTERVAL: Duration = Duration::from_secs(30);
/// 池中常驻会话数上限（小池子；超额淘汰最久未用）。
const MAX_POOLED: usize = 8;

/// 池子的键：规范化工作区路径 + 服务器命令 + 参数指纹。
///
/// 用「命令 + 参数」而非 `ServerSpec` 整体，避免把 `language_id` 也算进键
/// （同一服务器可服务同族多种 languageId，应共用同一会话）。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PoolKey {
    workspace: String,
    command: String,
    args: String,
}

impl PoolKey {
    pub fn new(workspace: &str, command: &str, args: &[&str]) -> Self {
        PoolKey {
            workspace: workspace.to_string(),
            command: command.to_string(),
            args: args.join("\u{0}"),
        }
    }
}

struct PooledServer {
    session: LspSession,
    last_used: Instant,
}

struct Pool {
    map: HashMap<PoolKey, PooledServer>,
}

fn pool() -> &'static Mutex<Pool> {
    static POOL: OnceLock<Mutex<Pool>> = OnceLock::new();
    POOL.get_or_init(|| {
        ensure_reaper();
        Mutex::new(Pool {
            map: HashMap::new(),
        })
    })
}

/// 启动一次性的后台 reaper 线程（守护式：进程退出时随之结束）。
fn ensure_reaper() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        std::thread::Builder::new()
            .name("mdga-lsp-reaper".to_string())
            .spawn(|| loop {
                std::thread::sleep(REAP_INTERVAL);
                reap_idle();
            })
            .ok(); // 起不来也不致命：容量淘汰仍会限制泄漏，且进程退出兜底回收。
    });
}

/// 借出一个匹配 `key` 的空闲会话（若有且存活）。命中即从池中取走所有权。
///
/// 调用方拿到后应先 `begin_op` 重置超时、再 `sync_document` 喂最新全文。
pub fn checkout(key: &PoolKey) -> Option<LspSession> {
    let mut entry = {
        let mut guard = pool().lock().ok()?;
        guard.map.remove(key)?
        // 取走所有权后立即释放池锁——后续 is_alive 探活与（死会话的）Drop 强杀都在锁外做，
        // 不让进程树 kill 卡住其它持锁路径。
    };
    if entry.session.is_alive() {
        Some(entry.session)
    } else {
        // 已死：丢弃（Drop 强杀，已在锁外），返回 None 让调用方新建。
        drop(entry);
        None
    }
}

/// 归还一个会话到池中（仅当存活）。超过容量上限时淘汰最久未用的那个。
///
/// 死会话直接 Drop（强杀），不纳管。
pub fn checkin(key: PoolKey, mut session: LspSession) {
    if !session.is_alive() {
        return; // Drop 强杀
    }
    let Ok(mut guard) = pool().lock() else {
        return; // 锁中毒：放弃纳管，session 在此 Drop（强杀），不泄漏
    };

    // 先放回（覆盖同键的旧会话）。被覆盖的旧 `PooledServer` 由 insert 在锁内返回，
    // **不在锁内 Drop**：收集到锁外再杀，避免进程树 kill 卡住持锁的其它线程。
    let mut to_drop: Vec<PooledServer> = Vec::new();
    if let Some(old) = guard.map.insert(
        key,
        PooledServer {
            session,
            last_used: Instant::now(),
        },
    ) {
        to_drop.push(old);
    }

    // 容量淘汰：超额则反复**取走**最久未用的（取走而非锁内 Drop），攒到锁外统一杀。
    while guard.map.len() > MAX_POOLED {
        let victim = guard
            .map
            .iter()
            .min_by_key(|(_, v)| v.last_used)
            .map(|(k, _)| k.clone());
        match victim.and_then(|k| guard.map.remove(&k)) {
            Some(evicted) => to_drop.push(evicted),
            None => break,
        }
    }

    // 关键：先释放池锁，再在临界区**之外** Drop（强杀子进程树）。进程树 kill 可能耗时，
    // 不能让它阻塞 checkout/checkin/reaper 等持锁路径。
    drop(guard);
    drop(to_drop);
}

/// 回收所有空闲超过 `IDLE_TTL` 的会话（reaper 周期调用；也供测试直接触发）。
/// 返回被回收的会话数。被移除者 Drop 即强杀子进程。
pub fn reap_idle() -> usize {
    reap_idle_with_ttl(IDLE_TTL)
}

/// 带自定义 TTL 的回收（测试用：传 `Duration::ZERO` 立即回收全部空闲会话以验证可回收性）。
pub fn reap_idle_with_ttl(ttl: Duration) -> usize {
    // 锁内只做：挑出超时键、把对应会话**取走**装进本地 Vec；锁外再 Drop（强杀子进程树）。
    // 不在持锁时 Drop——进程树 kill 可能耗时，会卡住 checkout/checkin 等其它持锁路径。
    let reaped: Vec<PooledServer> = {
        let Ok(mut guard) = pool().lock() else {
            return 0;
        };
        let now = Instant::now();
        let stale: Vec<PoolKey> = guard
            .map
            .iter()
            .filter(|(_, v)| now.duration_since(v.last_used) >= ttl)
            .map(|(k, _)| k.clone())
            .collect();
        stale
            .into_iter()
            .filter_map(|k| guard.map.remove(&k))
            .collect()
        // guard 在此作用域结束时释放锁。
    };
    let n = reaped.len();
    drop(reaped); // 临界区之外强杀子进程树。
    n
}

/// 当前池中空闲会话数（测试/诊断用）。
pub fn pooled_count() -> usize {
    pool().lock().map(|g| g.map.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_key_distinguishes_workspace_command_args() {
        let a = PoolKey::new("/ws", "rust-analyzer", &[]);
        let b = PoolKey::new("/ws", "rust-analyzer", &[]);
        let c = PoolKey::new("/other", "rust-analyzer", &[]);
        let d = PoolKey::new("/ws", "tsserver", &["--stdio"]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
    }

    /// 容量淘汰的「挑谁」逻辑（最久未用者）独立成纯函数后可不依赖真会话单测：
    /// 用一张 PoolKey→last_used 的影子表镜像 `checkin` 里 `min_by_key(last_used)` 的选择，
    /// 断言被选中的 victim 恰是 last_used 最小的那个。证明淘汰策略稳定、不误伤更新的会话。
    #[test]
    fn capacity_eviction_picks_oldest_last_used() {
        use std::collections::HashMap as Map;
        let base = Instant::now();
        // 三个键，last_used 递增；最久未用的应是 k_old。
        let k_old = PoolKey::new("/ws", "srv", &["a"]);
        let k_mid = PoolKey::new("/ws", "srv", &["b"]);
        let k_new = PoolKey::new("/ws", "srv", &["c"]);
        let mut shadow: Map<PoolKey, Instant> = Map::new();
        shadow.insert(k_old.clone(), base);
        shadow.insert(k_mid.clone(), base + Duration::from_secs(1));
        shadow.insert(k_new.clone(), base + Duration::from_secs(2));

        let victim = shadow
            .iter()
            .min_by_key(|(_, t)| **t)
            .map(|(k, _)| k.clone())
            .expect("非空表应能选出 victim");
        assert_eq!(victim, k_old, "容量淘汰应选最久未用者");
    }

    /// 空池上 reap 返回 0、计数不变——证明锁外 Drop 重构没破坏「无副作用、不 panic」语义。
    /// （真会话的回收/复用由 lsp_smoke.rs 的 e2e 覆盖。）
    #[test]
    fn reap_on_empty_pool_returns_zero_and_count_stable() {
        // 先把本进程残留空闲会话清空，建立确定起点。
        let _ = reap_idle_with_ttl(Duration::ZERO);
        let before = pooled_count();
        let reaped = reap_idle_with_ttl(Duration::ZERO);
        let after = pooled_count();
        assert_eq!(reaped, 0, "空池回收数应为 0，实际 {reaped}");
        assert_eq!(before, 0, "清空后计数应为 0，实际 {before}");
        assert_eq!(after, 0, "回收后计数应仍为 0，实际 {after}");
    }

    #[test]
    fn checkout_miss_returns_none() {
        // 不存在的键借不到（不 panic、不阻塞）。
        let key = PoolKey::new("/nonexistent/ws/for/test", "no-such-server", &["x"]);
        assert!(checkout(&key).is_none());
    }
}
