use mdga_shared::PermissionMode;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ToolCapability {
    FileList,
    FileRead,
    FileWrite,
    FileDelete,
    CommandRun,
    NetworkAccess,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ToolDecision {
    Allow,
    AskUser,
    Deny,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSecurityContext {
    pub workspace_root: String,
    pub permission_mode: PermissionMode,
    pub network_mode: NetworkMode,
    pub approval_policy: String,
}

#[derive(Debug, Error)]
pub enum SandboxRuntimeError {
    #[error("会话缺少工作区边界")]
    MissingWorkspace,
    #[error("工具能力需要用户审批")]
    ApprovalRequired,
    #[error("当前权限模式不允许此工具能力")]
    CapabilityDenied,
}

/// 判断沙箱策略是否声明了工作区根目录。
///
/// 输入沙箱策略，输出该策略是否拥有最小执行边界；本方法不启动进程。
pub fn has_workspace_boundary(policy: &SandboxPolicy) -> bool {
    !policy.workspace_root.trim().is_empty()
}

/// 构造会话级安全上下文。
///
/// 输入用户在新对话时绑定的工作区路径、权限模式和网络模式；输出 Host 可信的执行边界。
/// 模型只能读取该上下文描述，不能修改它；所有本地工具执行前都必须先经过该上下文裁决。
pub fn session_security_context(
    workspace_root: impl Into<String>,
    permission_mode: PermissionMode,
    network_mode: NetworkMode,
) -> Result<SessionSecurityContext, SandboxRuntimeError> {
    let workspace_root = workspace_root.into();
    if workspace_root.trim().is_empty() {
        return Err(SandboxRuntimeError::MissingWorkspace);
    }

    Ok(SessionSecurityContext {
        workspace_root,
        permission_mode,
        network_mode,
        approval_policy: "host-enforced".to_string(),
    })
}

/// 为单个工具能力做权限裁决。
///
/// 输入 Host 可信安全上下文和工具能力；输出允许、需要审批或拒绝。
/// 该方法不执行工具，只负责把产品权限模式转换成稳定的运行时决策。
pub fn decide_tool_access(
    context: &SessionSecurityContext,
    capability: ToolCapability,
) -> ToolDecision {
    match context.permission_mode {
        PermissionMode::Restricted => match capability {
            ToolCapability::FileList | ToolCapability::FileRead => ToolDecision::Allow,
            _ => ToolDecision::Deny,
        },
        PermissionMode::AskEveryTime => ToolDecision::AskUser,
        PermissionMode::WorkspaceAuto => match capability {
            ToolCapability::FileList
            | ToolCapability::FileRead
            | ToolCapability::FileWrite
            | ToolCapability::FileDelete => ToolDecision::Allow,
            ToolCapability::CommandRun | ToolCapability::NetworkAccess => ToolDecision::AskUser,
        },
        PermissionMode::FullAccess => ToolDecision::Allow,
    }
}

/// 校验工具能力是否可以在当前会话中直接执行。
///
/// 输入安全上下文和工具能力；允许时返回 Ok，拒绝或需要审批时返回明确错误。
/// 桌面端当前还没有审批 UI，因此 AskUser 会被转成 ApprovalRequired。
pub fn ensure_tool_allowed(
    context: &SessionSecurityContext,
    capability: ToolCapability,
) -> Result<(), SandboxRuntimeError> {
    match decide_tool_access(context, capability) {
        ToolDecision::Allow => Ok(()),
        ToolDecision::AskUser => Err(SandboxRuntimeError::ApprovalRequired),
        ToolDecision::Deny => Err(SandboxRuntimeError::CapabilityDenied),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_auto_allows_workspace_file_tools() {
        let context = session_security_context(
            "C:\\Users\\AIT\\Desktop\\MDGA",
            PermissionMode::WorkspaceAuto,
            NetworkMode::Disabled,
        )
        .expect("context should build");

        assert_eq!(
            decide_tool_access(&context, ToolCapability::FileWrite),
            ToolDecision::Allow
        );
        assert_eq!(
            decide_tool_access(&context, ToolCapability::FileDelete),
            ToolDecision::Allow
        );
    }

    #[test]
    fn restricted_mode_denies_mutating_file_tools() {
        let context = session_security_context(
            "C:\\Users\\AIT\\Desktop\\MDGA",
            PermissionMode::Restricted,
            NetworkMode::Disabled,
        )
        .expect("context should build");

        assert_eq!(
            decide_tool_access(&context, ToolCapability::FileRead),
            ToolDecision::Allow
        );
        assert_eq!(
            decide_tool_access(&context, ToolCapability::FileWrite),
            ToolDecision::Deny
        );
    }

    #[test]
    fn ask_every_time_requires_host_approval() {
        let context = session_security_context(
            "C:\\Users\\AIT\\Desktop\\MDGA",
            PermissionMode::AskEveryTime,
            NetworkMode::Disabled,
        )
        .expect("context should build");

        assert_eq!(
            ensure_tool_allowed(&context, ToolCapability::FileRead)
                .expect_err("approval should be required")
                .to_string(),
            "工具能力需要用户审批"
        );
    }

    #[test]
    fn rejects_missing_workspace_boundary() {
        let err = session_security_context("", PermissionMode::WorkspaceAuto, NetworkMode::Disabled)
            .expect_err("workspace is required");

        assert_eq!(err.to_string(), "会话缺少工作区边界");
    }
}
