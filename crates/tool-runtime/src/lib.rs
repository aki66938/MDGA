use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use thiserror::Error;

const MAX_READ_BYTES: u64 = 256 * 1024;
const MAX_SEARCH_FILE_BYTES: u64 = 128 * 1024;
const DEFAULT_SEARCH_LIMIT: usize = 50;
const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 120;
const MAX_COMMAND_TIMEOUT_SECS: u64 = 600;
const MAX_COMMAND_OUTPUT_BYTES: usize = 64 * 1024;

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
pub struct ListDirRequest {
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadFileRequest {
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteFileRequest {
    pub path: String,
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteFileRequest {
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditFileRequest {
    pub path: String,
    pub old_text: String,
    pub new_text: String,
    #[serde(default)]
    pub replace_all: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MakeDirRequest {
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatPathRequest {
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchTextRequest {
    pub path: String,
    pub query: String,
    #[serde(default = "default_search_limit")]
    pub max_results: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MovePathRequest {
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteDirRequest {
    pub path: String,
    /// 必须显式为 true 才允许递归删除非空目录，避免模型误删。
    #[serde(default)]
    pub recursive: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCommandRequest {
    pub command: String,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileToolResult {
    pub relative_path: String,
    pub absolute_path: String,
    pub bytes_written: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteFileResult {
    pub relative_path: String,
    pub absolute_path: String,
    pub bytes_written: u64,
    pub previous_exists: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadFileResult {
    pub relative_path: String,
    pub absolute_path: String,
    pub content: String,
    pub bytes_read: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteFileResult {
    pub relative_path: String,
    pub absolute_path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditFileResult {
    pub relative_path: String,
    pub absolute_path: String,
    pub replacements: u64,
    pub bytes_written: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MakeDirResult {
    pub relative_path: String,
    pub absolute_path: String,
    pub created: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatPathResult {
    pub relative_path: String,
    pub absolute_path: String,
    pub kind: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListDirResult {
    pub relative_path: String,
    pub absolute_path: String,
    pub entries: Vec<DirEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    pub name: String,
    pub kind: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchTextResult {
    pub relative_path: String,
    pub matches: Vec<TextMatch>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MovePathResult {
    pub from: String,
    pub to: String,
    pub absolute_to: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteDirResult {
    pub relative_path: String,
    pub absolute_path: String,
    pub recursive: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCommandResult {
    pub command: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub timed_out: bool,
    pub duration_ms: u128,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextMatch {
    pub path: String,
    pub line: usize,
    pub preview: String,
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
    #[error("目标路径不存在")]
    PathNotFound,
    #[error("目标不是文件")]
    NotAFile,
    #[error("目标不是目录")]
    NotADirectory,
    #[error("文件过大，超过 {0} 字节限制")]
    FileTooLarge(u64),
    #[error("文件不是有效 UTF-8 文本")]
    NonUtf8Text,
    #[error("替换文本不能为空")]
    EmptyOldText,
    #[error("搜索文本不能为空")]
    EmptyQuery,
    #[error("未找到需要替换的文本")]
    PatternNotFound,
    #[error("替换文本出现多次，请提供更精确的 old_text 或启用 replace_all")]
    PatternNotUnique,
    #[error("目录非空，需显式 recursive=true 才能删除")]
    DirectoryNotEmpty,
    #[error("不能删除工作区根目录")]
    CannotDeleteWorkspaceRoot,
    #[error("命令不能为空")]
    EmptyCommand,
    #[error("命令执行失败: {0}")]
    CommandFailed(String),
    #[error("文件系统错误: {0}")]
    Io(#[from] std::io::Error),
}

/// 在指定工作区内创建新文件。
///
/// 输入工作区根目录和相对文件路径；输出真实写入结果。路径必须留在工作区内，
/// 禁止绝对路径和 `..` 逃逸；父目录会按需创建，目标已存在时拒绝覆盖。
pub fn create_file(
    workspace_root: impl AsRef<Path>,
    request: CreateFileRequest,
) -> Result<FileToolResult, ToolRuntimeError> {
    let (workspace, relative, target) = resolve_new_path(workspace_root, &request.path)?;
    if target.exists() {
        return Err(ToolRuntimeError::FileAlreadyExists);
    }
    ensure_parent_inside_workspace(&workspace, &target)?;
    std::fs::write(&target, request.content.as_bytes())?;
    let absolute_path = target.canonicalize()?;

    Ok(FileToolResult {
        relative_path: normalize_relative_path(&relative),
        absolute_path: absolute_path.to_string_lossy().to_string(),
        bytes_written: request.content.len() as u64,
    })
}

/// 列举工作区内目录。
///
/// 输入工作区根目录和相对目录路径；输出名称、类型和大小。仅允许访问 workspace 内目录。
pub fn list_dir(
    workspace_root: impl AsRef<Path>,
    request: ListDirRequest,
) -> Result<ListDirResult, ToolRuntimeError> {
    let (_workspace, relative, target) = resolve_existing_path(workspace_root, &request.path)?;
    if !target.is_dir() {
        return Err(ToolRuntimeError::NotADirectory);
    }

    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&target)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        let kind = if meta.is_dir() { "directory" } else { "file" };
        entries.push(DirEntry {
            name: entry.file_name().to_string_lossy().to_string(),
            kind: kind.to_string(),
            bytes: if meta.is_file() { meta.len() } else { 0 },
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(ListDirResult {
        relative_path: normalize_relative_path(&relative),
        absolute_path: target.to_string_lossy().to_string(),
        entries,
    })
}

/// 读取工作区内 UTF-8 文本文件。
///
/// 输入工作区根目录和相对文件路径；输出文本内容。第一版限制 256 KiB，避免误读大文件。
pub fn read_file(
    workspace_root: impl AsRef<Path>,
    request: ReadFileRequest,
) -> Result<ReadFileResult, ToolRuntimeError> {
    let (_workspace, relative, target) = resolve_existing_path(workspace_root, &request.path)?;
    let content = read_utf8_file(&target, MAX_READ_BYTES)?;

    Ok(ReadFileResult {
        relative_path: normalize_relative_path(&relative),
        absolute_path: target.to_string_lossy().to_string(),
        bytes_read: content.len() as u64,
        content,
    })
}

/// 写入工作区内 UTF-8 文本文件。
///
/// 输入工作区根目录和相对文件路径；输出写入结果。允许覆盖已有文件，但禁止目录和越界路径。
pub fn write_file(
    workspace_root: impl AsRef<Path>,
    request: WriteFileRequest,
) -> Result<WriteFileResult, ToolRuntimeError> {
    let (workspace, relative, target) = resolve_new_path(workspace_root, &request.path)?;
    let previous_exists = target.exists();
    if previous_exists {
        ensure_existing_file_inside_workspace(&workspace, &target)?;
    }
    ensure_parent_inside_workspace(&workspace, &target)?;
    std::fs::write(&target, request.content.as_bytes())?;
    let absolute_path = target.canonicalize()?;

    Ok(WriteFileResult {
        relative_path: normalize_relative_path(&relative),
        absolute_path: absolute_path.to_string_lossy().to_string(),
        bytes_written: request.content.len() as u64,
        previous_exists,
    })
}

/// 对工作区内 UTF-8 文本文件执行精确替换。
///
/// 输入目标文件、旧文本和新文本；默认要求旧文本只出现一次，避免 AI 模糊替换误伤多处代码。
pub fn edit_file(
    workspace_root: impl AsRef<Path>,
    request: EditFileRequest,
) -> Result<EditFileResult, ToolRuntimeError> {
    if request.old_text.is_empty() {
        return Err(ToolRuntimeError::EmptyOldText);
    }
    let (_workspace, relative, target) = resolve_existing_path(workspace_root, &request.path)?;
    let content = read_utf8_file(&target, MAX_READ_BYTES)?;
    let count = content.matches(&request.old_text).count();
    if count == 0 {
        return Err(ToolRuntimeError::PatternNotFound);
    }
    if count > 1 && !request.replace_all {
        return Err(ToolRuntimeError::PatternNotUnique);
    }

    let next = if request.replace_all {
        content.replace(&request.old_text, &request.new_text)
    } else {
        content.replacen(&request.old_text, &request.new_text, 1)
    };
    std::fs::write(&target, next.as_bytes())?;

    Ok(EditFileResult {
        relative_path: normalize_relative_path(&relative),
        absolute_path: target.to_string_lossy().to_string(),
        replacements: if request.replace_all { count as u64 } else { 1 },
        bytes_written: next.len() as u64,
    })
}

/// 删除工作区内单个文件。
///
/// 输入工作区根目录和相对文件路径；输出删除结果。第一版只允许删除文件，不允许删除目录。
pub fn delete_file(
    workspace_root: impl AsRef<Path>,
    request: DeleteFileRequest,
) -> Result<DeleteFileResult, ToolRuntimeError> {
    let (_workspace, relative, target) = resolve_existing_path(workspace_root, &request.path)?;
    if !target.is_file() {
        return Err(ToolRuntimeError::NotAFile);
    }
    let absolute_path = target.to_string_lossy().to_string();
    std::fs::remove_file(&target)?;

    Ok(DeleteFileResult {
        relative_path: normalize_relative_path(&relative),
        absolute_path,
    })
}

/// 在工作区内创建目录。
///
/// 输入工作区根目录和相对目录路径；输出目录创建结果。已有目录会返回 created=false。
pub fn make_dir(
    workspace_root: impl AsRef<Path>,
    request: MakeDirRequest,
) -> Result<MakeDirResult, ToolRuntimeError> {
    let (workspace, relative, target) = resolve_new_path(workspace_root, &request.path)?;
    let created = !target.exists();
    std::fs::create_dir_all(&target)?;
    let absolute_path = target.canonicalize()?;
    if !absolute_path.starts_with(&workspace) {
        return Err(ToolRuntimeError::PathOutsideWorkspace);
    }

    Ok(MakeDirResult {
        relative_path: normalize_relative_path(&relative),
        absolute_path: absolute_path.to_string_lossy().to_string(),
        created,
    })
}

/// 查询工作区内路径元信息。
///
/// 输入工作区根目录和相对路径；输出文件/目录类型与文件大小。
pub fn stat_path(
    workspace_root: impl AsRef<Path>,
    request: StatPathRequest,
) -> Result<StatPathResult, ToolRuntimeError> {
    let (_workspace, relative, target) = resolve_existing_path(workspace_root, &request.path)?;
    let meta = std::fs::metadata(&target)?;
    let kind = if meta.is_dir() { "directory" } else { "file" };

    Ok(StatPathResult {
        relative_path: normalize_relative_path(&relative),
        absolute_path: target.to_string_lossy().to_string(),
        kind: kind.to_string(),
        bytes: if meta.is_file() { meta.len() } else { 0 },
    })
}

/// 在工作区内递归搜索 UTF-8 文本。
///
/// 输入起始目录、查询字符串和最大结果数；输出匹配文件、行号和预览。二进制或过大文件会跳过。
pub fn search_text(
    workspace_root: impl AsRef<Path>,
    request: SearchTextRequest,
) -> Result<SearchTextResult, ToolRuntimeError> {
    let query = request.query.trim();
    if query.is_empty() {
        return Err(ToolRuntimeError::EmptyQuery);
    }
    let (_workspace, relative, target) = resolve_existing_path(workspace_root, &request.path)?;
    if !target.is_dir() {
        return Err(ToolRuntimeError::NotADirectory);
    }

    let mut matches = Vec::new();
    let max_results = request.max_results.clamp(1, DEFAULT_SEARCH_LIMIT);
    collect_text_matches(&target, &target, query, max_results, &mut matches)?;

    Ok(SearchTextResult {
        relative_path: normalize_relative_path(&relative),
        matches,
    })
}

/// 在工作区内移动或重命名文件/目录。
///
/// 输入源相对路径（必须存在）和目标相对路径（必须不存在）；源和目标都做越界守卫。
/// 同一工作区内为同设备，使用 rename 原子完成。
pub fn move_path(
    workspace_root: impl AsRef<Path>,
    request: MovePathRequest,
) -> Result<MovePathResult, ToolRuntimeError> {
    let (workspace, from_rel, from_target) = resolve_existing_path(&workspace_root, &request.from)?;
    let (_workspace, to_rel, to_target) = resolve_new_path(&workspace_root, &request.to)?;
    if to_target.exists() {
        return Err(ToolRuntimeError::FileAlreadyExists);
    }
    ensure_parent_inside_workspace(&workspace, &to_target)?;
    std::fs::rename(&from_target, &to_target)?;
    let absolute_to = to_target.canonicalize()?;
    if !absolute_to.starts_with(&workspace) {
        return Err(ToolRuntimeError::PathOutsideWorkspace);
    }

    Ok(MovePathResult {
        from: normalize_relative_path(&from_rel),
        to: normalize_relative_path(&to_rel),
        absolute_to: absolute_to.to_string_lossy().to_string(),
    })
}

/// 删除工作区内目录。
///
/// 输入相对目录路径；非空目录必须显式 recursive=true 才能删除，禁止删除工作区根目录。
pub fn delete_dir(
    workspace_root: impl AsRef<Path>,
    request: DeleteDirRequest,
) -> Result<DeleteDirResult, ToolRuntimeError> {
    let (workspace, relative, target) = resolve_existing_path(workspace_root, &request.path)?;
    if !target.is_dir() {
        return Err(ToolRuntimeError::NotADirectory);
    }
    if target == workspace {
        return Err(ToolRuntimeError::CannotDeleteWorkspaceRoot);
    }
    let absolute_path = target.to_string_lossy().to_string();

    let is_empty = std::fs::read_dir(&target)?.next().is_none();
    if !is_empty && !request.recursive {
        return Err(ToolRuntimeError::DirectoryNotEmpty);
    }
    if request.recursive {
        std::fs::remove_dir_all(&target)?;
    } else {
        std::fs::remove_dir(&target)?;
    }

    Ok(DeleteDirResult {
        relative_path: normalize_relative_path(&relative),
        absolute_path,
        recursive: request.recursive,
    })
}

/// 在工作区内执行一条 PowerShell 命令。
///
/// 输入命令字符串和可选超时秒数；cwd 固定为工作区根目录，默认超时 120 秒、上限 600 秒。
/// stdout/stderr 各自截断到 64 KiB，超时则杀进程并标记 timed_out。本方法不做权限裁决，
/// 权限由 Host 在调用前用 SessionSecurityContext 判定（仅 FullAccess 允许）。
pub fn run_command(
    workspace_root: impl AsRef<Path>,
    request: RunCommandRequest,
) -> Result<RunCommandResult, ToolRuntimeError> {
    let command = request.command.trim();
    if command.is_empty() {
        return Err(ToolRuntimeError::EmptyCommand);
    }
    let workspace = canonical_workspace(workspace_root)?;
    let timeout = Duration::from_secs(
        request
            .timeout_secs
            .unwrap_or(DEFAULT_COMMAND_TIMEOUT_SECS)
            .clamp(1, MAX_COMMAND_TIMEOUT_SECS),
    );

    let mut child = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", command])
        .current_dir(&workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| ToolRuntimeError::CommandFailed(err.to_string()))?;

    // 用独立线程抽干 stdout/stderr，避免管道缓冲填满导致死锁。
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_handle = std::thread::spawn(move || drain_pipe(stdout_pipe.as_mut()));
    let stderr_handle = std::thread::spawn(move || drain_pipe(stderr_pipe.as_mut()));

    let started = Instant::now();
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    timed_out = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => return Err(ToolRuntimeError::CommandFailed(err.to_string())),
        }
    }

    let status = child
        .wait()
        .map_err(|err| ToolRuntimeError::CommandFailed(err.to_string()))?;
    let duration_ms = started.elapsed().as_millis();
    let (stdout_raw, out_trunc) = stdout_handle.join().unwrap_or((String::new(), false));
    let (stderr_raw, err_trunc) = stderr_handle.join().unwrap_or((String::new(), false));

    Ok(RunCommandResult {
        command: command.to_string(),
        exit_code: status.code(),
        stdout: stdout_raw,
        stderr: stderr_raw,
        truncated: out_trunc || err_trunc,
        timed_out,
        duration_ms,
    })
}

/// 读取子进程管道，按 UTF-8 lossy 解码并截断到 MAX_COMMAND_OUTPUT_BYTES。
fn drain_pipe(pipe: Option<&mut impl Read>) -> (String, bool) {
    let Some(pipe) = pipe else {
        return (String::new(), false);
    };
    let mut buffer = Vec::new();
    if pipe.read_to_end(&mut buffer).is_err() {
        return (String::from_utf8_lossy(&buffer).to_string(), false);
    }
    let truncated = buffer.len() > MAX_COMMAND_OUTPUT_BYTES;
    if truncated {
        buffer.truncate(MAX_COMMAND_OUTPUT_BYTES);
    }
    (String::from_utf8_lossy(&buffer).to_string(), truncated)
}

fn default_search_limit() -> usize {
    DEFAULT_SEARCH_LIMIT
}

fn read_utf8_file(path: &Path, max_bytes: u64) -> Result<String, ToolRuntimeError> {
    if !path.is_file() {
        return Err(ToolRuntimeError::NotAFile);
    }
    let meta = std::fs::metadata(path)?;
    if meta.len() > max_bytes {
        return Err(ToolRuntimeError::FileTooLarge(max_bytes));
    }
    let bytes = std::fs::read(path)?;
    String::from_utf8(bytes).map_err(|_| ToolRuntimeError::NonUtf8Text)
}

fn collect_text_matches(
    search_root: &Path,
    current: &Path,
    query: &str,
    max_results: usize,
    matches: &mut Vec<TextMatch>,
) -> Result<(), ToolRuntimeError> {
    if matches.len() >= max_results {
        return Ok(());
    }

    for entry in std::fs::read_dir(current)? {
        if matches.len() >= max_results {
            break;
        }
        let entry = entry?;
        let path = entry.path();
        let meta = entry.metadata()?;
        if meta.is_dir() {
            collect_text_matches(search_root, &path, query, max_results, matches)?;
            continue;
        }
        if !meta.is_file() || meta.len() > MAX_SEARCH_FILE_BYTES {
            continue;
        }
        let Ok(content) = read_utf8_file(&path, MAX_SEARCH_FILE_BYTES) else {
            continue;
        };
        for (index, line) in content.lines().enumerate() {
            if matches.len() >= max_results {
                break;
            }
            if line.contains(query) {
                let relative = path
                    .strip_prefix(search_root)
                    .unwrap_or(&path)
                    .components()
                    .filter_map(|component| match component {
                        Component::Normal(part) => Some(part.to_string_lossy().to_string()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("/");
                matches.push(TextMatch {
                    path: relative,
                    line: index + 1,
                    preview: line.trim().chars().take(240).collect(),
                });
            }
        }
    }

    Ok(())
}

fn validate_relative_path(path: &str) -> Result<PathBuf, ToolRuntimeError> {
    let trimmed = path.trim();
    if trimmed == "." {
        return Ok(PathBuf::new());
    }
    if trimmed.is_empty() {
        return Err(ToolRuntimeError::EmptyPath);
    }

    let candidate = Path::new(trimmed);
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

    if safe.as_os_str().is_empty() && trimmed != "." {
        return Err(ToolRuntimeError::EmptyPath);
    }

    Ok(safe)
}

fn canonical_workspace(workspace_root: impl AsRef<Path>) -> Result<PathBuf, ToolRuntimeError> {
    workspace_root
        .as_ref()
        .canonicalize()
        .map_err(|err| ToolRuntimeError::WorkspaceUnavailable(err.to_string()))
}

fn resolve_new_path(
    workspace_root: impl AsRef<Path>,
    path: &str,
) -> Result<(PathBuf, PathBuf, PathBuf), ToolRuntimeError> {
    let relative = validate_relative_path(path)?;
    if relative.as_os_str().is_empty() {
        return Err(ToolRuntimeError::EmptyPath);
    }
    let workspace = canonical_workspace(workspace_root)?;
    let target = workspace.join(&relative);
    Ok((workspace, relative, target))
}

fn resolve_existing_path(
    workspace_root: impl AsRef<Path>,
    path: &str,
) -> Result<(PathBuf, PathBuf, PathBuf), ToolRuntimeError> {
    let relative = validate_relative_path(path)?;
    let workspace = canonical_workspace(workspace_root)?;
    let candidate = workspace.join(&relative);
    if !candidate.exists() {
        return Err(ToolRuntimeError::PathNotFound);
    }
    let target = candidate.canonicalize()?;
    if !target.starts_with(&workspace) {
        return Err(ToolRuntimeError::PathOutsideWorkspace);
    }
    Ok((workspace, relative, target))
}

fn ensure_parent_inside_workspace(
    workspace: &Path,
    target: &Path,
) -> Result<(), ToolRuntimeError> {
    let parent = target.parent().ok_or(ToolRuntimeError::PathOutsideWorkspace)?;
    std::fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    if !parent.starts_with(workspace) {
        return Err(ToolRuntimeError::PathOutsideWorkspace);
    }
    Ok(())
}

fn ensure_existing_file_inside_workspace(
    workspace: &Path,
    target: &Path,
) -> Result<(), ToolRuntimeError> {
    let existing = target.canonicalize()?;
    if !existing.starts_with(workspace) {
        return Err(ToolRuntimeError::PathOutsideWorkspace);
    }
    if !existing.is_file() {
        return Err(ToolRuntimeError::NotAFile);
    }
    Ok(())
}

fn normalize_relative_path(path: &Path) -> String {
    let normalized = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    if normalized.is_empty() {
        ".".to_string()
    } else {
        normalized
    }
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

    #[test]
    fn lists_directory_entries_inside_workspace() {
        let workspace = temp_workspace();
        std::fs::create_dir_all(workspace.join("docs")).expect("directory should be created");
        std::fs::write(workspace.join("docs/readme.md"), "hello").expect("file should be written");

        let result = list_dir(
            &workspace,
            ListDirRequest {
                path: "docs".to_string(),
            },
        )
        .expect("directory should list");

        assert_eq!(result.relative_path, "docs");
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].name, "readme.md");
        assert_eq!(result.entries[0].kind, "file");

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn reads_text_file_inside_workspace() {
        let workspace = temp_workspace();
        std::fs::write(workspace.join("note.txt"), "hello").expect("file should be written");

        let result = read_file(
            &workspace,
            ReadFileRequest {
                path: "note.txt".to_string(),
            },
        )
        .expect("file should read");

        assert_eq!(result.relative_path, "note.txt");
        assert_eq!(result.content, "hello");

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn writes_text_file_inside_workspace() {
        let workspace = temp_workspace();
        std::fs::write(workspace.join("note.txt"), "old").expect("file should be written");

        let result = write_file(
            &workspace,
            WriteFileRequest {
                path: "note.txt".to_string(),
                content: "123456".to_string(),
            },
        )
        .expect("file should write");

        assert_eq!(result.relative_path, "note.txt");
        assert!(result.previous_exists);
        assert_eq!(
            std::fs::read_to_string(workspace.join("note.txt")).expect("file should read"),
            "123456"
        );

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn edits_file_with_unique_replacement_inside_workspace() {
        let workspace = temp_workspace();
        std::fs::write(workspace.join("note.txt"), "hello world").expect("file should be written");

        let result = edit_file(
            &workspace,
            EditFileRequest {
                path: "note.txt".to_string(),
                old_text: "world".to_string(),
                new_text: "MDGA".to_string(),
                replace_all: false,
            },
        )
        .expect("file should edit");

        assert_eq!(result.replacements, 1);
        assert_eq!(
            std::fs::read_to_string(workspace.join("note.txt")).expect("file should read"),
            "hello MDGA"
        );

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn rejects_ambiguous_edit_without_replace_all() {
        let workspace = temp_workspace();
        std::fs::write(workspace.join("note.txt"), "same same").expect("file should be written");

        let err = edit_file(
            &workspace,
            EditFileRequest {
                path: "note.txt".to_string(),
                old_text: "same".to_string(),
                new_text: "next".to_string(),
                replace_all: false,
            },
        )
        .expect_err("ambiguous edit should fail");

        assert!(matches!(err, ToolRuntimeError::PatternNotUnique));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn makes_directory_and_stats_path_inside_workspace() {
        let workspace = temp_workspace();

        let mkdir = make_dir(
            &workspace,
            MakeDirRequest {
                path: "src/components".to_string(),
            },
        )
        .expect("directory should be created");
        let stat = stat_path(
            &workspace,
            StatPathRequest {
                path: "src/components".to_string(),
            },
        )
        .expect("directory should stat");

        assert!(mkdir.created);
        assert_eq!(stat.kind, "directory");

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn searches_text_inside_workspace() {
        let workspace = temp_workspace();
        std::fs::create_dir_all(workspace.join("src")).expect("directory should be created");
        std::fs::write(workspace.join("src/lib.rs"), "fn main() {}\nlet token = 1;")
            .expect("file should be written");

        let result = search_text(
            &workspace,
            SearchTextRequest {
                path: ".".to_string(),
                query: "token".to_string(),
                max_results: 10,
            },
        )
        .expect("text should search");

        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].path, "src/lib.rs");
        assert_eq!(result.matches[0].line, 2);

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn deletes_file_inside_workspace() {
        let workspace = temp_workspace();
        std::fs::write(workspace.join("note.txt"), "old").expect("file should be written");

        let result = delete_file(
            &workspace,
            DeleteFileRequest {
                path: "note.txt".to_string(),
            },
        )
        .expect("file should delete");

        assert_eq!(result.relative_path, "note.txt");
        assert!(!workspace.join("note.txt").exists());

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn moves_file_inside_workspace() {
        let workspace = temp_workspace();
        std::fs::write(workspace.join("a.txt"), "data").expect("file should be written");
        std::fs::create_dir_all(workspace.join("src")).expect("dir should be created");

        let result = move_path(
            &workspace,
            MovePathRequest {
                from: "a.txt".to_string(),
                to: "src/a.txt".to_string(),
            },
        )
        .expect("file should move");

        assert_eq!(result.from, "a.txt");
        assert_eq!(result.to, "src/a.txt");
        assert!(!workspace.join("a.txt").exists());
        assert_eq!(
            std::fs::read_to_string(workspace.join("src/a.txt")).expect("moved file should read"),
            "data"
        );

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn move_rejects_existing_destination() {
        let workspace = temp_workspace();
        std::fs::write(workspace.join("a.txt"), "1").expect("file should be written");
        std::fs::write(workspace.join("b.txt"), "2").expect("file should be written");

        let err = move_path(
            &workspace,
            MovePathRequest {
                from: "a.txt".to_string(),
                to: "b.txt".to_string(),
            },
        )
        .expect_err("existing destination should be rejected");

        assert!(matches!(err, ToolRuntimeError::FileAlreadyExists));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn delete_dir_requires_recursive_for_nonempty() {
        let workspace = temp_workspace();
        std::fs::create_dir_all(workspace.join("pkg")).expect("dir should be created");
        std::fs::write(workspace.join("pkg/file.txt"), "x").expect("file should be written");

        let err = delete_dir(
            &workspace,
            DeleteDirRequest {
                path: "pkg".to_string(),
                recursive: false,
            },
        )
        .expect_err("non-empty dir without recursive should fail");
        assert!(matches!(err, ToolRuntimeError::DirectoryNotEmpty));

        delete_dir(
            &workspace,
            DeleteDirRequest {
                path: "pkg".to_string(),
                recursive: true,
            },
        )
        .expect("recursive delete should succeed");
        assert!(!workspace.join("pkg").exists());

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn delete_dir_rejects_workspace_root() {
        let workspace = temp_workspace();

        let err = delete_dir(
            &workspace,
            DeleteDirRequest {
                path: ".".to_string(),
                recursive: true,
            },
        )
        .expect_err("deleting workspace root should be rejected");
        assert!(matches!(err, ToolRuntimeError::CannotDeleteWorkspaceRoot));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn run_command_executes_inside_workspace() {
        let workspace = temp_workspace();
        std::fs::write(workspace.join("marker.txt"), "hi").expect("file should be written");

        let result = run_command(
            &workspace,
            RunCommandRequest {
                command: "Get-ChildItem -Name".to_string(),
                timeout_secs: Some(30),
            },
        )
        .expect("command should execute");

        assert!(!result.timed_out);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.contains("marker.txt"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn run_command_rejects_empty() {
        let workspace = temp_workspace();
        let err = run_command(
            &workspace,
            RunCommandRequest {
                command: "   ".to_string(),
                timeout_secs: None,
            },
        )
        .expect_err("empty command should be rejected");
        assert!(matches!(err, ToolRuntimeError::EmptyCommand));

        let _ = std::fs::remove_dir_all(workspace);
    }
}
