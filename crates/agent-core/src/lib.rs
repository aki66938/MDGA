//! mdga-agent-core：可移植的 Agent 内核逻辑（Plan28 P3-9）。
//!
//! 承载**不依赖 Tauri AppHandle/emit/State/DB** 的纯逻辑，供桌面端调用、可独立单测：
//! - [`prompt`]：灵魂文件常量（身份锚定 / 工具纪律 / 行为准则）。
//! - [`messages`]：工作区上下文消息构建 + 项目长期记忆读取。
//! - [`compaction`]：上下文压缩软上限常量与推导。
//! - [`verification`]：写后验证命令探测（识别 Cargo.toml/package.json/.mdga/diagnostics，不执行）。
//! - [`usage`]：token usage 合并。
//!
//! 这些条目从桌面端整体迁入，逻辑一字不改，仅换位置并提升可见性；桌面端改为 `use mdga_agent_core::...`。

use serde::{Deserialize, Serialize};

mod compaction;
mod messages;
mod prompt;
mod usage;
mod verification;

// 内核公开面：桌面端按既有引用路径 `mdga_agent_core::<名>` 直接取用。
pub use compaction::{context_soft_limit_for, CONTEXT_SOFT_LIMIT_TOKENS};
pub use messages::{messages_with_workspace_context, read_workspace_memory};
pub use prompt::{CODE_OF_CONDUCT, IDENTITY_ANCHOR, TOOL_DISCIPLINE};
pub use usage::merge_usage;
pub use verification::{detect_verification_command, read_diagnostics_command};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TaskStatus {
    Created,
    Planning,
    AwaitingApproval,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

/// 判断任务是否已经进入终态。
///
/// 输入任务状态，输出是否不可继续推进；本方法不修改任务，只用于状态机判断。
pub fn is_terminal_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
    )
}
