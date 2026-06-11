use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub capability: String,
}

/// 判断工具描述是否具备最小有效字段。
///
/// 输入工具描述，输出是否可注册；本方法不执行工具，也不判断用户权限。
pub fn is_valid_tool_descriptor(tool: &ToolDescriptor) -> bool {
    !tool.name.trim().is_empty() && !tool.capability.trim().is_empty()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateFileRequest {
    pub path: String,
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileToolResult {
    pub relative_path: String,
    pub absolute_path: String,
    pub bytes_written: u64,
}

#[derive(Debug, Error)]
pub enum ToolRuntimeError {
    #[error("工具路径必须位于当前工作区内")]
    PathOutsideWorkspace,
    #[error("工具路径不能为空")]
    EmptyPath,
    #[error("目标文件已存在")]
    FileAlreadyExists,
    #[error("工作区路径不可用: {0}")]
    WorkspaceUnavailable(String),
    #[error("文件系统错误: {0}")]
    Io(#[from] std::io::Error),
}

/// 在指定工作区内创建文件。
///
/// 输入工作区根目录和相对文件路径；输出真实写入结果。路径必须留在工作区内，禁止绝对路径和
/// `..` 逃逸；父目录会按需创建，目标已存在时拒绝覆盖。
pub fn create_file(
    workspace_root: impl AsRef<Path>,
    request: CreateFileRequest,
) -> Result<FileToolResult, ToolRuntimeError> {
    let relative_path = request.path.trim();
    if relative_path.is_empty() {
        return Err(ToolRuntimeError::EmptyPath);
    }

    let relative = validate_relative_path(relative_path)?;
    let workspace = workspace_root
        .as_ref()
        .canonicalize()
        .map_err(|err| ToolRuntimeError::WorkspaceUnavailable(err.to_string()))?;
    let target = workspace.join(&relative);

    if target.exists() {
        return Err(ToolRuntimeError::FileAlreadyExists);
    }

    let parent = target.parent().ok_or(ToolRuntimeError::PathOutsideWorkspace)?;
    std::fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    if !parent.starts_with(&workspace) {
        return Err(ToolRuntimeError::PathOutsideWorkspace);
    }

    std::fs::write(&target, request.content.as_bytes())?;
    let absolute_path = target.canonicalize()?;

    Ok(FileToolResult {
        relative_path: normalize_relative_path(&relative),
        absolute_path: absolute_path.to_string_lossy().to_string(),
        bytes_written: request.content.len() as u64,
    })
}

fn validate_relative_path(path: &str) -> Result<PathBuf, ToolRuntimeError> {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return Err(ToolRuntimeError::PathOutsideWorkspace);
    }

    let mut safe = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => safe.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ToolRuntimeError::PathOutsideWorkspace);
            }
        }
    }

    if safe.as_os_str().is_empty() {
        return Err(ToolRuntimeError::EmptyPath);
    }

    Ok(safe)
}

fn normalize_relative_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_workspace() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be available")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("mdga-tool-runtime-{nonce}"));
        std::fs::create_dir_all(&path).expect("workspace should be created");
        path
    }

    #[test]
    fn creates_file_inside_workspace() {
        let workspace = temp_workspace();

        let result = create_file(
            &workspace,
            CreateFileRequest {
                path: "notes/test.txt".to_string(),
                content: "hello MDGA".to_string(),
            },
        )
        .expect("file should be created");

        assert_eq!(result.relative_path, "notes/test.txt");
        assert!(workspace.join("notes/test.txt").is_file());
        assert_eq!(
            std::fs::read_to_string(workspace.join("notes/test.txt")).expect("file should read"),
            "hello MDGA"
        );

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn rejects_absolute_path() {
        let workspace = temp_workspace();
        let outside = workspace.join("outside.txt");

        let err = create_file(
            &workspace,
            CreateFileRequest {
                path: outside.to_string_lossy().to_string(),
                content: String::new(),
            },
        )
        .expect_err("absolute path should be rejected");

        assert!(matches!(err, ToolRuntimeError::PathOutsideWorkspace));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn rejects_parent_dir_escape() {
        let workspace = temp_workspace();

        let err = create_file(
            &workspace,
            CreateFileRequest {
                path: "../escape.txt".to_string(),
                content: String::new(),
            },
        )
        .expect_err("parent dir escape should be rejected");

        assert!(matches!(err, ToolRuntimeError::PathOutsideWorkspace));

        let _ = std::fs::remove_dir_all(workspace);
    }
}
