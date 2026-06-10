use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum NetworkMode {
    Disabled,
    AllowListed,
    FullAccess,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SandboxPolicy {
    pub workspace_root: String,
    pub network_mode: NetworkMode,
}

/// 判断沙箱策略是否声明了工作区根目录。
///
/// 输入沙箱策略，输出该策略是否拥有最小执行边界；本方法不启动进程。
pub fn has_workspace_boundary(policy: &SandboxPolicy) -> bool {
    !policy.workspace_root.trim().is_empty()
}
