//! 应用共享状态与后台句柄结构。
//!
//! 从 main.rs 抽出（Plan16 阶段1）：纯结构/静态量搬移，无行为变更。

use mdga_agent_core::{FileFingerprint, SequenceLoopDetector};
use mdga_mcp_client::McpClient;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

/// 审批请求自增序号，用于生成唯一 action_id。
pub(crate) static APPROVAL_SEQ: AtomicU64 = AtomicU64::new(1);
/// ask_user 结构化提问自增序号，生成唯一 question_id。
pub(crate) static QUESTION_SEQ: AtomicU64 = AtomicU64::new(1);
/// 后台 shell 自增序号，生成唯一 shell_id。
pub(crate) static BG_SHELL_SEQ: AtomicU64 = AtomicU64::new(1);
/// 后台子代理任务自增序号，生成唯一 task_id。
pub(crate) static BG_TASK_SEQ: AtomicU64 = AtomicU64::new(1);

/// Host 可信的应用状态：数据库、运行时句柄、各类按会话/任务索引的共享表。
pub(crate) struct AppState {
    pub(crate) db: Mutex<rusqlite::Connection>,
    /// 正在运行的 Agent 会话取消标志，按 conversation_id 索引。用户点击停止时置 true，
    /// 工具循环在轮次之间和工具执行前检查并安全收尾。
    pub(crate) cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// 等待用户审批的高风险动作，按 action_id 索引，附带该动作对应的「总是允许」规则串。
    /// respond_approval 命令收到前端决定后，通过 oneshot 通道唤醒正在 await 的工具循环；
    /// 用户勾选记住时把规则写入 permission_rules 表。
    pub(crate) approvals: Mutex<HashMap<String, (oneshot::Sender<bool>, String)>>,
    /// 等待用户回答的 ask_user 结构化提问，按 question_id 索引。respond_ask_user 命令收到
    /// 前端选择后，通过 oneshot 通道把答案 JSON 回送给正在 await 的工具循环。
    pub(crate) ask_questions: Mutex<HashMap<String, oneshot::Sender<String>>>,
    /// 已连接的 MCP server 客户端，按配置 id 索引。Arc 包裹以便在锁外调用。
    pub(crate) mcp: Mutex<HashMap<String, Arc<McpClient>>>,
    /// Steering：用户在 Agent 运行中排队的插话消息，按 conversation_id 索引。
    /// 工具循环在每轮开始时取出并作为 user 消息注入，实现「运行中纠偏」。
    pub(crate) steering: Mutex<HashMap<String, Vec<String>>>,
    /// repo map 按会话缓存：避免每轮重新遍历工作区，并让 system 前缀字节稳定，
    /// 最大化 DeepSeek prompt 缓存命中（缓存友好上下文）。
    pub(crate) repo_maps: Mutex<HashMap<String, String>>,
    /// 托管后台 shell：background=true 启动的命令，按 shell_id 索引，可轮询输出 / 杀进程。
    pub(crate) bg_shells: Mutex<HashMap<String, BgShell>>,
    /// 后台子代理任务：run_subtask background=true 启动的探索代理，按 task_id 索引，
    /// 可用 get_task_output 轮询报告/状态、kill_task 终止；完成时 usage 由首次 get_task_output 结算进账本。
    pub(crate) bg_tasks: Mutex<HashMap<String, BgTask>>,
    /// 命令沙箱开关（默认开）：前台 run_command 是否在受限令牌沙箱中执行。
    pub(crate) command_sandbox: AtomicBool,
    /// 单次任务 token 预算（累计 total_tokens 上限）；0 = 不限。超出则暂停工具循环。
    pub(crate) task_token_budget: AtomicU64,
    /// R6 循环护栏状态：按 conversation_id 索引的「陈旧读指纹表 + 序列级 doom-loop 检测器」。
    /// 工具循环在 read_file 成功后记录指纹、在写类编辑前比对、按每轮调用签名喂检测器。
    /// 会话结束（send_message 收尾）时清理，避免跨任务串味。
    pub(crate) loop_guards: Mutex<HashMap<String, ConversationLoopGuard>>,
    /// 0.0.75「第三栏·用量标签」按工具归因的**进程内**轻量埋点（不落库、不进热路径性能关键段）。
    /// 外层 key=conversation_id，内层 key=tool_name；记录该会话内每个工具的调用次数与输出 token 体积估算。
    ///
    /// 重要语义：这是「当前会话活动量」视图（calls + output_tokens 近似），**不是账单成本**——
    /// 工具本身不直接产生账单，真账单在 LLM 轮次/角色（token_ledger / messages.usage_json）。
    /// 进程内、会话级、**重启清空**（本期不持久化）。record_tool_event 在工具完成
    /// （succeeded/failed，不计 running，避免重复）时累加；锁失败一律软跳过，绝不 panic。
    pub(crate) tool_usage: Mutex<HashMap<String, HashMap<String, ToolUsageAcc>>>,
}

/// 单个工具在某会话内的累计活动量（进程内累加器，非账单成本）。
#[derive(Default, Clone)]
pub(crate) struct ToolUsageAcc {
    /// 该工具在本会话内的完成调用次数（succeeded + failed，不含 running/denied）。
    pub(crate) calls: u64,
    /// 该工具输出体积的粗估累加：每次完成时按 outputJson 字符串长度 / 4 估算 token 数。
    /// 仅作「该工具对上下文贡献的近似量级」参考，不是精确分词、更不是成本。
    pub(crate) output_tokens: u64,
}

/// 后台活动（子代理 / 命令）的共享读取与取消原语。
///
/// 抽出供 agent 工具（execute_bg_task_tool / execute_bg_shell_tool 的 kill 分支）与
/// 前端活动面板命令（list_bg_activity / kill_bg_activity / get_bg_activity_output）**共用同一套底层逻辑**，
/// 避免「拉取 / 停止」在工具侧与命令侧各写一份。锁失败一律软处理（返回空 / false），绝不 unwrap panic。
impl AppState {
    /// 克隆 bg_tasks 表的快照（(task_id, BgTask) 列表，BgTask: Clone 仅含 Arc 句柄）。
    /// 锁失败返回空 vec。调用方应在锁外读取各 Arc<Mutex<..>> 字段，避免长时间持锁。
    pub(crate) fn bg_tasks_snapshot(&self) -> Vec<(String, BgTask)> {
        match self.bg_tasks.lock() {
            Ok(tasks) => tasks.iter().map(|(id, t)| (id.clone(), t.clone())).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// 克隆 bg_shells 表的快照（(shell_id, BgShell) 列表，BgShell: Clone 仅含 Arc 句柄）。
    /// 锁失败返回空 vec。
    pub(crate) fn bg_shells_snapshot(&self) -> Vec<(String, BgShell)> {
        match self.bg_shells.lock() {
            Ok(shells) => shells.iter().map(|(id, s)| (id.clone(), s.clone())).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// 置位指定后台子代理的 cancel 标志（kill_task 的底层逻辑）。
    /// 找到并置位返回 true；id 不存在或锁失败返回 false。
    pub(crate) fn set_task_cancel(&self, id: &str) -> bool {
        let cancel = self
            .bg_tasks
            .lock()
            .ok()
            .and_then(|tasks| tasks.get(id).map(|t| t.cancel.clone()));
        match cancel {
            Some(flag) => {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                true
            }
            None => false,
        }
    }

    /// 置位指定后台命令的 cancel 标志（kill_shell 的底层逻辑）。
    /// 找到并置位返回 true；id 不存在或锁失败返回 false。
    pub(crate) fn set_shell_cancel(&self, id: &str) -> bool {
        let cancel = self
            .bg_shells
            .lock()
            .ok()
            .and_then(|shells| shells.get(id).map(|s| s.cancel.clone()));
        match cancel {
            Some(flag) => {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                true
            }
            None => false,
        }
    }
}

/// 一个会话的循环护栏状态（R6）。
///
/// - `read_fingerprints`：键为该文件在磁盘上的绝对路径（canonical），值为 read_file 当时的
///   mtime+size 指纹；写类编辑前据此判断底层文件是否在读后被改动（陈旧读）。
/// - `loop_detector`：序列级 doom-loop 检测器，吃每轮的 (tool,args) 调用签名，
///   命中「窗口循环重复」即让工具循环走既有 agent-stuck 暂停路径。
#[derive(Default)]
pub(crate) struct ConversationLoopGuard {
    pub(crate) read_fingerprints: HashMap<PathBuf, FileFingerprint>,
    pub(crate) loop_detector: SequenceLoopDetector,
}

/// 一个托管的后台 shell 进程状态。
#[derive(Clone)]
pub(crate) struct BgShell {
    pub(crate) command: String,
    pub(crate) output: Arc<Mutex<String>>,
    pub(crate) status: Arc<Mutex<String>>, // running | done | killed | error
    pub(crate) cancel: Arc<AtomicBool>,
}

/// 一个后台子代理任务的共享状态（仿 BgShell）。
#[derive(Clone)]
pub(crate) struct BgTask {
    pub(crate) description: String,
    /// 所属会话 id：cancel_agent（对话总开关）据此级联停掉该会话的所有后台子任务。
    pub(crate) conversation_id: String,
    pub(crate) report: Arc<Mutex<String>>,
    pub(crate) status: Arc<Mutex<String>>, // running | done | killed | error
    pub(crate) usage: Arc<Mutex<Option<mdga_shared::RawUsage>>>,
    /// usage 是否已被某次 get_task_output 结算进会话账本，避免重复计费。
    pub(crate) settled: Arc<AtomicBool>,
    pub(crate) cancel: Arc<AtomicBool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个仅供后台活动 helper 单测用的最小 AppState（内存 DB，其余表全空）。
    fn empty_state() -> AppState {
        AppState {
            db: Mutex::new(rusqlite::Connection::open_in_memory().expect("内存 DB")),
            cancels: Mutex::new(HashMap::new()),
            approvals: Mutex::new(HashMap::new()),
            ask_questions: Mutex::new(HashMap::new()),
            mcp: Mutex::new(HashMap::new()),
            steering: Mutex::new(HashMap::new()),
            repo_maps: Mutex::new(HashMap::new()),
            bg_shells: Mutex::new(HashMap::new()),
            bg_tasks: Mutex::new(HashMap::new()),
            command_sandbox: AtomicBool::new(true),
            task_token_budget: AtomicU64::new(0),
            loop_guards: Mutex::new(HashMap::new()),
            tool_usage: Mutex::new(HashMap::new()),
        }
    }

    fn a_task(conversation_id: &str) -> BgTask {
        BgTask {
            description: "explore".to_string(),
            conversation_id: conversation_id.to_string(),
            report: Arc::new(Mutex::new(String::new())),
            status: Arc::new(Mutex::new("running".to_string())),
            usage: Arc::new(Mutex::new(None)),
            settled: Arc::new(AtomicBool::new(false)),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    fn a_shell() -> BgShell {
        BgShell {
            command: "echo hi".to_string(),
            output: Arc::new(Mutex::new(String::new())),
            status: Arc::new(Mutex::new("running".to_string())),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn snapshots_empty_on_fresh_state() {
        let st = empty_state();
        assert!(st.bg_tasks_snapshot().is_empty());
        assert!(st.bg_shells_snapshot().is_empty());
    }

    #[test]
    fn snapshots_clone_registered_entries() {
        let st = empty_state();
        st.bg_tasks.lock().unwrap().insert("task-1".to_string(), a_task("conv-A"));
        st.bg_shells.lock().unwrap().insert("sh-1".to_string(), a_shell());
        let tasks = st.bg_tasks_snapshot();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].0, "task-1");
        assert_eq!(tasks[0].1.conversation_id, "conv-A");
        assert_eq!(st.bg_shells_snapshot().len(), 1);
    }

    #[test]
    fn set_task_cancel_flags_existing_and_reports_missing() {
        let st = empty_state();
        let task = a_task("conv-A");
        let flag = task.cancel.clone();
        st.bg_tasks.lock().unwrap().insert("task-1".to_string(), task);

        assert!(st.set_task_cancel("task-1"));
        assert!(flag.load(std::sync::atomic::Ordering::SeqCst));
        // 不存在的 id 返回 false，不 panic。
        assert!(!st.set_task_cancel("task-404"));
    }

    #[test]
    fn set_shell_cancel_flags_existing_and_reports_missing() {
        let st = empty_state();
        let shell = a_shell();
        let flag = shell.cancel.clone();
        st.bg_shells.lock().unwrap().insert("sh-1".to_string(), shell);

        assert!(st.set_shell_cancel("sh-1"));
        assert!(flag.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!st.set_shell_cancel("sh-404"));
    }
}
