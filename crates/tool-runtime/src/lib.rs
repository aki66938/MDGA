#[cfg(windows)]
mod sandbox_win;
#[cfg(windows)]
mod appcontainer_win;

// R4：git 原生工具（壳调 git CLI + 结构化解析）。types/fns 全部从此再导出。
mod git;
pub use git::*;

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use thiserror::Error;

const MAX_READ_BYTES: u64 = 1024 * 1024;
/// read_file 默认返回行数（未指定 limit 时），平衡上下文体积与一次读够。
const DEFAULT_READ_LINES: usize = 1500;
/// read_file 单次返回行数硬上限。
const MAX_READ_LINES: usize = 4000;
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
    /// 起始行（0 基），用于分页读取大文件。默认 0。
    #[serde(default)]
    pub offset: usize,
    /// 最多读取的行数。为 0 时使用默认上限 DEFAULT_READ_LINES。
    #[serde(default)]
    pub limit: usize,
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

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchTextRequest {
    pub path: String,
    pub query: String,
    #[serde(default = "default_search_limit")]
    pub max_results: usize,
    /// query 是否按正则解释；false 时按字面子串匹配。默认 false。
    #[serde(default)]
    pub is_regex: bool,
    /// 输出模式：content（默认，匹配行+上下文）| files_with_matches（仅文件名）| count（每文件计数）。
    #[serde(default)]
    pub output_mode: Option<String>,
    /// 大小写不敏感（-i）。
    #[serde(default)]
    pub case_insensitive: bool,
    /// 跨行匹配：. 匹配换行、模式可跨行（regex (?s)）。
    #[serde(default)]
    pub multiline: bool,
    /// 匹配行的前置上下文行数（-B）。
    #[serde(default)]
    pub before_context: usize,
    /// 匹配行的后置上下文行数（-A）。
    #[serde(default)]
    pub after_context: usize,
    /// 同时设置前后上下文行数（-C，优先于 before/after）。
    #[serde(default)]
    pub context: usize,
    /// 按文件类型过滤（如 "rs"、"ts"、"py"），映射到扩展名集合。
    #[serde(default)]
    pub file_type: Option<String>,
    /// 按文件名 glob 过滤（如 "*.rs"、"src/**"）。
    #[serde(default)]
    pub glob: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobFilesRequest {
    /// 文件名 glob 模式：支持 * ? ** （如 "**/*.rs"、"src/**"、"*.toml"）。
    pub pattern: String,
    /// 起始相对目录，默认工作区根。
    #[serde(default)]
    pub path: Option<String>,
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
    /// 后台运行：由调用方（桌面端）处理，run_command 本身不感知；保留字段用于参数解析。
    #[serde(default)]
    pub background: bool,
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
    /// 文件总行数（分页判断用）。
    pub total_lines: usize,
    /// 本次返回的起始行（0 基）。
    pub start_line: usize,
    /// 本次返回的行数。
    pub returned_lines: usize,
    /// 是否还有更多行未返回（提示模型用更大 offset 继续读）。
    pub has_more: bool,
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

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchTextResult {
    pub relative_path: String,
    /// 实际生效的输出模式。
    pub output_mode: String,
    /// content 模式：匹配行（含可选上下文）。
    #[serde(default)]
    pub matches: Vec<TextMatch>,
    /// files_with_matches 模式：命中文件相对路径列表。
    #[serde(default)]
    pub files: Vec<String>,
    /// count 模式：每个文件的匹配行数。
    #[serde(default)]
    pub counts: Vec<FileMatchCount>,
    /// 是否因达到 max_results 上限而截断。
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMatchCount {
    pub path: String,
    pub count: usize,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobFilesResult {
    pub relative_path: String,
    pub files: Vec<String>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeOverviewRequest {
    /// 相对工作区路径；`.` = 工作区根，可为文件或目录。
    pub path: String,
}

/// 单种语言的聚合统计（按扩展名归类后逐文件累加）。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageStat {
    /// 该语言的文件数。
    pub files: usize,
    /// 该语言的总 LOC（按本结果的 loc 口径，见 CodeOverviewResult.note）。
    pub loc: usize,
    /// 公开/导出符号计数（按语言正则启发式，匹配不到给 0）。
    pub symbols: usize,
    /// 测试计数（按语言测试标记正则启发式）。
    pub tests: usize,
}

/// 单个文件的轻量统计（仅在「最大若干文件」榜单里回传，控制体积）。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileStat {
    pub path: String,
    pub language: String,
    pub loc: usize,
    pub symbols: usize,
    pub tests: usize,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeOverviewResult {
    pub ok: bool,
    /// 规范化后的相对路径。
    pub path: String,
    /// 目标是否为单文件（true）还是目录（false）。
    pub is_file: bool,
    /// 实际统计到的文件数（扫描后、上限内）。
    pub total_files: usize,
    /// 全部语言合计 LOC。
    pub loc: usize,
    /// 按语言聚合。key 为语言短名（rs/ts/js/py/go/java/c/cpp/rb/php/cs/kt/swift/other…）。
    pub by_language: std::collections::BTreeMap<String, LanguageStat>,
    /// 命中的构建/依赖文件相对路径（去重、排序）。
    pub build_files: Vec<String>,
    /// 检测到的包/crate 根：每个构建清单文件对应一个条目（路径 + 类型）。
    pub packages: Vec<PackageEntry>,
    /// 建议的「求真」命令字符串（按检测到的构建系统给出，仅字符串、不执行）。
    pub suggested_verify_commands: Vec<String>,
    /// 体积最大的前 N 个文件（LOC 降序），目录大时也只回传榜单而非逐文件全列。
    pub largest_files: Vec<FileStat>,
    /// 是否因文件数/字节上限触发了截断（统计为部分样本）。
    pub truncated: bool,
    /// 行数口径与其它说明（人读）。
    pub note: String,
}

/// 一个检测到的包/工程根：构建清单文件的相对路径与其构建系统类型。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageEntry {
    pub path: String,
    /// 构建系统类型：cargo / npm / python / go / maven / gradle / dotnet / ruby / php-composer…
    pub kind: String,
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
    /// 实际生效的沙箱层:"appcontainer"(首选) / "restricted"(受限令牌) / None(未沙箱)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_layer: Option<String>,
    /// true 表示本应走 AppContainer,因自检未过或执行失败降级到了受限令牌沙箱。
    #[serde(default)]
    pub sandbox_degraded: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextMatch {
    pub path: String,
    pub line: usize,
    pub preview: String,
    /// 前置上下文行（-B/-C 时填充）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub before: Vec<String>,
    /// 后置上下文行（-A/-C 时填充）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<String>,
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
    let full = read_utf8_file(&target, MAX_READ_BYTES)?;

    // 分页：按行切片，offset 起、最多 limit 行（默认 DEFAULT_READ_LINES），让大文件可分段读取。
    let lines: Vec<&str> = full.lines().collect();
    let total_lines = lines.len();
    let start_line = request.offset.min(total_lines);
    let limit = if request.limit == 0 {
        DEFAULT_READ_LINES
    } else {
        request.limit.min(MAX_READ_LINES)
    };
    let end = start_line.saturating_add(limit).min(total_lines);
    let content = lines[start_line..end].join("\n");
    let returned_lines = end - start_line;

    Ok(ReadFileResult {
        relative_path: normalize_relative_path(&relative),
        absolute_path: target.to_string_lossy().to_string(),
        bytes_read: content.len() as u64,
        content,
        total_lines,
        start_line,
        returned_lines,
        has_more: end < total_lines,
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

    let output_mode = match request.output_mode.as_deref() {
        Some("files_with_matches") => "files_with_matches",
        Some("count") => "count",
        _ => "content",
    };
    // content 上限保守、files/count 可放宽（单条目轻量）。
    let max_results = request.max_results.clamp(1, DEFAULT_SEARCH_LIMIT * 4);
    let (before_ctx, after_ctx) = if request.context > 0 {
        (request.context, request.context)
    } else {
        (request.before_context, request.after_context)
    };

    // 匹配器：字面/正则 + 大小写不敏感(i) + 跨行(s) 标志。
    let mut pattern = if request.is_regex { query.to_string() } else { regex::escape(query) };
    let mut flags = String::new();
    if request.case_insensitive {
        flags.push('i');
    }
    if request.multiline {
        flags.push('s');
    }
    if !flags.is_empty() {
        pattern = format!("(?{flags}){pattern}");
    }
    let matcher = regex::Regex::new(&pattern)
        .map_err(|e| ToolRuntimeError::CommandFailed(format!("正则无效: {e}")))?;

    let exts = request.file_type.as_deref().map(file_type_extensions);
    let glob = request.glob.as_deref().map(compile_glob).transpose()?;

    let mut matches: Vec<TextMatch> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut counts: Vec<FileMatchCount> = Vec::new();
    let mut truncated = false;

    let walker = ignore::WalkBuilder::new(&target)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .require_git(false)
        .parents(true)
        .build();
    'walk: for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let rel = path
            .strip_prefix(&target)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        // 文件类型 / glob 过滤
        if let Some(exts) = &exts {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !exts.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
                continue;
            }
        }
        if let Some(g) = &glob {
            if !glob_hit(g, &rel) {
                continue;
            }
        }
        let Ok(meta) = path.metadata() else { continue };
        if meta.len() > MAX_SEARCH_FILE_BYTES {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(path) else { continue };

        if request.multiline {
            // 跨行模式：在全文匹配，命中即视为该文件匹配；content 模式定位首个匹配起始行。
            if matcher.is_match(&content) {
                match output_mode {
                    "files_with_matches" => files.push(rel.clone()),
                    "count" => counts.push(FileMatchCount {
                        path: rel.clone(),
                        count: matcher.find_iter(&content).count(),
                    }),
                    _ => {
                        if let Some(m) = matcher.find(&content) {
                            let line_no = content[..m.start()].lines().count().max(1);
                            let preview = content[m.start()..]
                                .lines()
                                .next()
                                .unwrap_or("")
                                .chars()
                                .take(200)
                                .collect();
                            matches.push(TextMatch {
                                path: rel.clone(),
                                line: line_no,
                                preview,
                                before: Vec::new(),
                                after: Vec::new(),
                            });
                        }
                    }
                }
                let hit = match output_mode {
                    "files_with_matches" => files.len() >= max_results,
                    "count" => counts.len() >= max_results,
                    _ => matches.len() >= max_results,
                };
                if hit {
                    truncated = true;
                    break 'walk;
                }
            }
            continue;
        }

        // 逐行模式
        let lines: Vec<&str> = content.lines().collect();
        let mut file_count = 0usize;
        for (idx, line) in lines.iter().enumerate() {
            if !matcher.is_match(line) {
                continue;
            }
            file_count += 1;
            match output_mode {
                "content" => {
                    let before = if before_ctx > 0 {
                        lines[idx.saturating_sub(before_ctx)..idx]
                            .iter()
                            .map(|s| s.chars().take(200).collect())
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let after = if after_ctx > 0 {
                        let end = (idx + 1 + after_ctx).min(lines.len());
                        lines[idx + 1..end]
                            .iter()
                            .map(|s| s.chars().take(200).collect())
                            .collect()
                    } else {
                        Vec::new()
                    };
                    matches.push(TextMatch {
                        path: rel.clone(),
                        line: idx + 1,
                        preview: line.chars().take(200).collect(),
                        before,
                        after,
                    });
                    if matches.len() >= max_results {
                        truncated = true;
                        break 'walk;
                    }
                }
                "files_with_matches" => {
                    files.push(rel.clone());
                    if files.len() >= max_results {
                        truncated = true;
                        break 'walk;
                    }
                    break; // 该文件已记录，进入下一个文件
                }
                _ => {}
            }
        }
        if output_mode == "count" && file_count > 0 {
            counts.push(FileMatchCount { path: rel.clone(), count: file_count });
            if counts.len() >= max_results {
                truncated = true;
                break 'walk;
            }
        }
    }

    Ok(SearchTextResult {
        relative_path: normalize_relative_path(&relative),
        output_mode: output_mode.to_string(),
        matches,
        files,
        counts,
        truncated,
    })
}

/// 把 ripgrep 风格的 type 名映射到扩展名集合。
fn file_type_extensions(t: &str) -> Vec<String> {
    let owned = |arr: &[&str]| arr.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match t.trim().to_ascii_lowercase().as_str() {
        "rust" | "rs" => owned(&["rs"]),
        "ts" | "typescript" => owned(&["ts", "tsx"]),
        "js" | "javascript" => owned(&["js", "jsx", "mjs", "cjs"]),
        "py" | "python" => owned(&["py"]),
        "go" => owned(&["go"]),
        "java" => owned(&["java"]),
        "c" => owned(&["c", "h"]),
        "cpp" | "c++" => owned(&["cpp", "cc", "cxx", "hpp", "hh"]),
        "json" => owned(&["json"]),
        "toml" => owned(&["toml"]),
        "md" | "markdown" => owned(&["md", "markdown"]),
        "css" => owned(&["css", "scss", "less"]),
        "html" => owned(&["html", "htm"]),
        "yaml" | "yml" => owned(&["yaml", "yml"]),
        other => vec![other.to_string()],
    }
}

/// 把 glob 模式（* ? **）编译为 (正则, 是否仅匹配 basename)。
/// 不含 '/' 的模式按文件名匹配（如 "*.rs" 命中任意目录下的 .rs）；含 '/' 的按相对路径匹配。
fn compile_glob(pattern: &str) -> Result<(regex::Regex, bool), ToolRuntimeError> {
    let basename_only = !pattern.contains('/');
    let re = regex::Regex::new(&glob_to_regex(pattern))
        .map_err(|e| ToolRuntimeError::CommandFailed(format!("glob 无效: {e}")))?;
    Ok((re, basename_only))
}

fn glob_to_regex(glob: &str) -> String {
    let chars: Vec<char> = glob.chars().collect();
    let mut re = String::from("^");
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    re.push_str(".*"); // ** 跨目录
                    i += 1;
                    if i + 1 < chars.len() && chars[i + 1] == '/' {
                        i += 1;
                    }
                } else {
                    re.push_str("[^/]*");
                }
            }
            '?' => re.push_str("[^/]"),
            c @ ('.' | '+' | '(' | ')' | '|' | '^' | '$' | '{' | '}' | '[' | ']' | '\\') => {
                re.push('\\');
                re.push(c);
            }
            c => re.push(c),
        }
        i += 1;
    }
    re.push('$');
    re
}

fn glob_hit(g: &(regex::Regex, bool), rel: &str) -> bool {
    let target = if g.1 { rel.rsplit('/').next().unwrap_or(rel) } else { rel };
    g.0.is_match(target)
}

/// 按文件名 glob 在工作区内快速枚举文件（gitignore 感知），返回相对路径（按修改时间倒序）。
pub fn glob_files(
    workspace_root: impl AsRef<Path>,
    request: GlobFilesRequest,
) -> Result<GlobFilesResult, ToolRuntimeError> {
    let pattern = request.pattern.trim();
    if pattern.is_empty() {
        return Err(ToolRuntimeError::EmptyQuery);
    }
    let start = request.path.as_deref().filter(|p| !p.trim().is_empty()).unwrap_or(".");
    let (_workspace, relative, target) = resolve_existing_path(workspace_root, start)?;
    if !target.is_dir() {
        return Err(ToolRuntimeError::NotADirectory);
    }
    let max_results = request.max_results.clamp(1, DEFAULT_SEARCH_LIMIT * 20);
    let g = compile_glob(pattern)?;
    let mut hits: Vec<(std::time::SystemTime, String)> = Vec::new();
    let mut truncated = false;
    let walker = ignore::WalkBuilder::new(&target)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .require_git(false)
        .parents(true)
        .build();
    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let rel = path
            .strip_prefix(&target)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if !glob_hit(&g, &rel) {
            continue;
        }
        let mtime = path
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        hits.push((mtime, rel));
        if hits.len() > max_results {
            truncated = true;
        }
    }
    hits.sort_by(|a, b| b.0.cmp(&a.0));
    hits.truncate(max_results);
    Ok(GlobFilesResult {
        relative_path: normalize_relative_path(&relative),
        files: hits.into_iter().map(|(_, p)| p).collect(),
        truncated,
    })
}

// ── code_overview（Plan28 P0-2，语言无关「求真」工具） ────────────────────────

/// code_overview 扫描的文件数上限：超过后停止统计并标 truncated（避免巨目录卡死）。
const OVERVIEW_MAX_FILES: usize = 4000;
/// 单文件参与统计的字节上限：超过的文件计入 total_files 但不读内容（与 search 一致，跳过大文件）。
const OVERVIEW_MAX_FILE_BYTES: u64 = 512 * 1024;
/// 累计读取字节上限：所有被读文件的字节之和封顶，超过即停止并标 truncated。
const OVERVIEW_MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
/// largest_files 榜单长度（控制回传体积）。
const OVERVIEW_TOP_FILES: usize = 10;

/// 语言无关的代码概览：对文件或目录统计 LOC / 公开符号 / 测试数，并识别构建文件。
///
/// 输入工作区根与相对路径（`.` = 根，可为文件或目录）；遍历目录时复用 ignore/gitignore 引擎、
/// 跳过隐藏与被忽略文件，与 search_text/glob_files 行为一致。按扩展名归类语言，逐文件用正则做
/// 「公开/导出符号」与「测试标记」启发式计数（尽力而为，匹配不到给 0，不报错）。识别 Cargo.toml /
/// package.json / pyproject.toml 等构建清单，给出建议的求真命令（仅字符串，不执行）。大目录有
/// 文件数/字节上限，超限给出聚合 + 截断标记，不回传巨串。
///
/// LOC 口径：**非空行**（去掉纯空白行后的行数）。
pub fn code_overview(
    workspace_root: impl AsRef<Path>,
    request: CodeOverviewRequest,
) -> Result<CodeOverviewResult, ToolRuntimeError> {
    let (workspace, relative, target) = resolve_existing_path(workspace_root, &request.path)?;
    let is_file = target.is_file();

    let mut by_language: std::collections::BTreeMap<String, LanguageStat> =
        std::collections::BTreeMap::new();
    let mut build_files: Vec<String> = Vec::new();
    let mut packages: Vec<PackageEntry> = Vec::new();
    let mut file_stats: Vec<FileStat> = Vec::new();
    let mut total_files = 0usize;
    let mut total_loc = 0usize;
    let mut total_bytes = 0u64;
    let mut truncated = false;

    // 统计单个文件并并入聚合：识别构建文件、累加语言聚合、追加文件榜单，返回读取的字节数（未读=0）。
    // 写成局部闭包仅借用聚合容器，预算变量（total_files/total_bytes）留在循环里判断，避免一并被借走。
    let absorb = |rel: &str,
                      abs: &Path,
                      by_language: &mut std::collections::BTreeMap<String, LanguageStat>,
                      build_files: &mut Vec<String>,
                      packages: &mut Vec<PackageEntry>,
                      stats: &mut Vec<FileStat>|
     -> (usize, u64) {
        let lang = language_for_path(rel);
        // 构建/依赖文件识别（按文件名，归到对应构建系统）。
        if let Some(kind) = build_file_kind(rel) {
            build_files.push(rel.to_string());
            packages.push(PackageEntry { path: rel.to_string(), kind: kind.to_string() });
        }
        let Ok(meta) = abs.metadata() else { return (0, 0) };
        if meta.len() > OVERVIEW_MAX_FILE_BYTES {
            // 大文件不读内容，但仍计入语言文件数（事实：存在该语言的大文件）。
            by_language.entry(lang.to_string()).or_default().files += 1;
            return (0, 0);
        }
        let Ok(content) = std::fs::read_to_string(abs) else { return (0, 0) };
        let loc = content.lines().filter(|l| !l.trim().is_empty()).count();
        let symbols = count_public_symbols(lang, &content);
        let tests = count_tests(lang, &content);
        let entry = by_language.entry(lang.to_string()).or_default();
        entry.files += 1;
        entry.loc += loc;
        entry.symbols += symbols;
        entry.tests += tests;
        stats.push(FileStat {
            path: rel.to_string(),
            language: lang.to_string(),
            loc,
            symbols,
            tests,
        });
        (loc, meta.len())
    };

    if is_file {
        let rel = normalize_relative_path(&relative);
        total_files = 1;
        // 单文件分支不参与字节预算循环，丢弃返回的字节数（仅累加 LOC）。
        let (loc, _bytes) = absorb(
            &rel,
            &target,
            &mut by_language,
            &mut build_files,
            &mut packages,
            &mut file_stats,
        );
        total_loc += loc;
    } else {
        let walker = ignore::WalkBuilder::new(&target)
            .hidden(true)
            .git_ignore(true)
            .git_global(false)
            .require_git(false)
            .parents(true)
            .build();
        for entry in walker.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if total_files >= OVERVIEW_MAX_FILES || total_bytes >= OVERVIEW_MAX_TOTAL_BYTES {
                truncated = true;
                break;
            }
            // 相对路径以「目标目录」为基准，与 search_text/glob_files 输出一致。
            let rel = path
                .strip_prefix(&target)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            total_files += 1;
            let (loc, bytes) = absorb(
                &rel,
                path,
                &mut by_language,
                &mut build_files,
                &mut packages,
                &mut file_stats,
            );
            total_loc += loc;
            total_bytes += bytes;
        }
    }

    // 仓库根/大目录聚合时，向祖先方向补查根级构建清单（目标目录本身可能不含 Cargo.toml，
    // 而其父级是 workspace 根）。仅当目标在工作区内时向上回溯到工作区根，命中即补登记。
    detect_ancestor_build_files(&workspace, &target, &mut build_files, &mut packages);

    // 去重 + 排序，保持输出稳定且不重复。
    build_files.sort();
    build_files.dedup();
    packages.sort_by(|a, b| a.path.cmp(&b.path));
    packages.dedup_by(|a, b| a.path == b.path);

    // 最大文件榜（LOC 降序，取前 N）。
    file_stats.sort_by(|a, b| b.loc.cmp(&a.loc).then_with(|| a.path.cmp(&b.path)));
    let largest_files: Vec<FileStat> = file_stats.into_iter().take(OVERVIEW_TOP_FILES).collect();

    let suggested_verify_commands = suggested_commands(&packages);

    let note = format!(
        "LOC 口径=非空行（已去空白行）。symbols/tests 为按语言正则的启发式计数（尽力而为，未匹配=0，非 AST）。\
         目录遍历尊重 .gitignore 并跳过隐藏文件；单文件超过 {} KiB 不读内容（仅计文件数），\
         扫描文件数上限 {} / 累计字节上限 {} MiB，超限会置 truncated。largest_files 仅列 LOC 最大的前 {} 个文件。",
        OVERVIEW_MAX_FILE_BYTES / 1024,
        OVERVIEW_MAX_FILES,
        OVERVIEW_MAX_TOTAL_BYTES / (1024 * 1024),
        OVERVIEW_TOP_FILES,
    );

    Ok(CodeOverviewResult {
        ok: true,
        path: normalize_relative_path(&relative),
        is_file,
        total_files,
        loc: total_loc,
        by_language,
        build_files,
        packages,
        suggested_verify_commands,
        largest_files,
        truncated,
        note,
    })
}

/// 按扩展名把文件归类为语言短名；未知归 "other"。
fn language_for_path(rel: &str) -> &'static str {
    let ext = rel.rsplit('.').next().filter(|e| !e.contains('/')).unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        "rs" => "rs",
        "ts" | "tsx" => "ts",
        "js" | "jsx" | "mjs" | "cjs" => "js",
        "py" | "pyi" => "py",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "rb" => "rb",
        "php" => "php",
        "cs" => "cs",
        "kt" | "kts" => "kt",
        "swift" => "swift",
        _ => "other",
    }
}

/// 识别构建/依赖清单文件，返回其构建系统类型；非构建文件返回 None。
///
/// 按 basename 匹配（含 *.csproj 这类后缀模式），覆盖常见生态。
fn build_file_kind(rel: &str) -> Option<&'static str> {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    match name {
        "Cargo.toml" => Some("cargo"),
        "package.json" => Some("npm"),
        "pyproject.toml" | "setup.py" | "requirements.txt" | "Pipfile" => Some("python"),
        "go.mod" => Some("go"),
        "pom.xml" => Some("maven"),
        "build.gradle" | "build.gradle.kts" | "settings.gradle" | "settings.gradle.kts" => {
            Some("gradle")
        }
        "Gemfile" => Some("ruby"),
        "composer.json" => Some("php-composer"),
        _ => {
            if name.ends_with(".csproj") || name.ends_with(".fsproj") || name.ends_with(".sln") {
                Some("dotnet")
            } else {
                None
            }
        }
    }
}

/// 从目标路径向工作区根回溯，补登记沿途的根级构建清单（如 Cargo.toml / package.json）。
///
/// 仓库根聚合时，目标目录自身可能不含清单（如统计 `crates/foo/src`），但其祖先含；
/// 把这些祖先清单一并登记，便于给出「整工程」求真命令。只看常见清单文件名，不读内容。
fn detect_ancestor_build_files(
    workspace: &Path,
    target: &Path,
    build_files: &mut Vec<String>,
    packages: &mut Vec<PackageEntry>,
) {
    // 从 target（若为文件取其父目录）向上，直到 workspace（含）为止。
    let mut dir = if target.is_file() { target.parent() } else { Some(target) };
    const MANIFESTS: &[&str] = &[
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "setup.py",
        "requirements.txt",
        "go.mod",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "Gemfile",
        "composer.json",
    ];
    while let Some(d) = dir {
        for m in MANIFESTS {
            let candidate = d.join(m);
            if candidate.is_file() {
                if let Some(kind) = build_file_kind(m) {
                    // 用相对工作区的路径登记，便于人读与去重。
                    let rel = candidate
                        .strip_prefix(workspace)
                        .unwrap_or(&candidate)
                        .to_string_lossy()
                        .replace('\\', "/");
                    build_files.push(rel.clone());
                    packages.push(PackageEntry { path: rel, kind: kind.to_string() });
                }
            }
        }
        if d == workspace {
            break;
        }
        dir = d.parent();
    }
}

/// 按检测到的构建系统集合，给出建议的「求真」命令字符串（去重、稳定顺序，仅字符串、不执行）。
fn suggested_commands(packages: &[PackageEntry]) -> Vec<String> {
    let mut kinds: Vec<&str> = packages.iter().map(|p| p.kind.as_str()).collect();
    kinds.sort_unstable();
    kinds.dedup();
    let mut cmds: Vec<String> = Vec::new();
    for kind in kinds {
        match kind {
            "cargo" => {
                cmds.push("cargo test --workspace".to_string());
                cmds.push("cargo build --workspace".to_string());
            }
            "npm" => {
                cmds.push("npm test".to_string());
                cmds.push("npm run build".to_string());
            }
            "python" => cmds.push("pytest".to_string()),
            "go" => {
                cmds.push("go test ./...".to_string());
                cmds.push("go build ./...".to_string());
            }
            "maven" => cmds.push("mvn test".to_string()),
            "gradle" => cmds.push("gradle test".to_string()),
            "dotnet" => cmds.push("dotnet test".to_string()),
            "ruby" => cmds.push("bundle exec rake test".to_string()),
            "php-composer" => cmds.push("composer test".to_string()),
            _ => {}
        }
    }
    cmds.dedup();
    cmds
}

/// 公开/导出符号计数（按语言正则启发式）。逐行扫描，未知语言返回 0。
fn count_public_symbols(lang: &str, content: &str) -> usize {
    let pattern = match lang {
        // Rust：pub fn/struct/enum/trait/mod/const/type（含 pub(crate) 等可见性修饰）。
        "rs" => r"^\s*pub(\s*\([^)]*\))?\s+(fn|struct|enum|trait|mod|const|type|static|union)\b",
        // TS/JS：export 开头，或顶层 function/class（export default 也算）。
        "ts" | "js" => {
            r"^\s*export\s+|^\s*export\s+default\s+|^\s*(export\s+)?(async\s+)?function\s+|^\s*(export\s+)?(abstract\s+)?class\s+"
        }
        // Python：顶层或缩进的 def / class。
        "py" => r"^\s*(def|class)\s+\w",
        // Go：func / type（Go 没有 pub 关键字，导出靠大写，这里统计全部定义作规模信号）。
        "go" => r"^func\s+|^type\s+\w",
        // Java/Kotlin/C#：class/interface/enum（可带 public 修饰）。
        "java" | "kt" | "cs" => {
            r"^\s*(public\s+|internal\s+|open\s+)?(abstract\s+|final\s+|sealed\s+)?(class|interface|enum|object)\s+"
        }
        // Swift：func/class/struct/enum/protocol（可带 public/open）。
        "swift" => {
            r"^\s*(public\s+|open\s+)?(func|class|struct|enum|protocol)\s+"
        }
        // Ruby：def/class/module。
        "rb" => r"^\s*(def|class|module)\s+",
        // PHP：function/class/interface/trait（可带 public）。
        "php" => r"^\s*(public\s+|abstract\s+|final\s+)?(function|class|interface|trait)\s+",
        // C/C++：函数定义启发式——形如 `type name(...) {`，行尾带 `{`，排除控制流关键字。
        "c" | "cpp" => {
            r"^[A-Za-z_][\w\s\*&:<>,]*\b[A-Za-z_]\w*\s*\([^;]*\)\s*(const\s*)?\{"
        }
        _ => return 0,
    };
    count_line_matches(pattern, content)
}

/// 测试计数（按语言测试标记正则启发式）。未知语言返回 0。
fn count_tests(lang: &str, content: &str) -> usize {
    match lang {
        // Rust：#[test] / #[tokio::test] / #[async_std::test] 等属性。
        "rs" => count_matches_anywhere(r"#\[\s*(\w+::)?test\b", content),
        // JS/TS：it( / test( / describe( 调用（含 .only/.each 变体前缀）。
        "ts" | "js" => count_matches_anywhere(r"\b(it|test|describe)\s*(\.\w+)?\s*\(", content),
        // Python：def test_ 函数 或 class Test 开头。
        "py" => count_line_matches(r"^\s*(def\s+test_\w*|class\s+Test\w*)", content),
        // Go：func TestXxx(t *testing.T) —— 以 func Test 开头即计。
        "go" => count_line_matches(r"^func\s+Test\w*\s*\(", content),
        // Java/Kotlin/C#：@Test 注解。
        "java" | "kt" | "cs" => count_matches_anywhere(r"@Test\b", content),
        // Swift：XCTest 的 func testXxx。
        "swift" => count_line_matches(r"^\s*func\s+test\w*\s*\(", content),
        // Ruby：RSpec/minitest 的 it/describe，或 def test_。
        "rb" => count_matches_anywhere(r"\b(it|describe)\s+|^\s*def\s+test_\w*", content),
        // PHP：PHPUnit 的 function testXxx 或 @test 注解。
        "php" => count_matches_anywhere(r"function\s+test\w*\s*\(|@test\b", content),
        _ => 0,
    }
}

/// 逐行匹配计数：每行至多计 1（按行锚定的模式用此）。正则非法时返回 0（启发式不报错）。
fn count_line_matches(pattern: &str, content: &str) -> usize {
    let Ok(re) = regex::Regex::new(pattern) else { return 0 };
    content.lines().filter(|line| re.is_match(line)).count()
}

/// 全文出现次数计数：统计 pattern 的全部不重叠匹配（行内可能多次的标记/调用用此）。
fn count_matches_anywhere(pattern: &str, content: &str) -> usize {
    let Ok(re) = regex::Regex::new(pattern) else { return 0 };
    re.find_iter(content).count()
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
    run_command_streaming(workspace_root, request, None, None, false, false)
}

/// 行级流式输出回调：命令每产生一行 stdout/stderr 调用一次，供 UI 实时展示。
pub type CommandLineCallback = std::sync::Arc<dyn Fn(String) + Send + Sync>;
/// 外部取消标志：置 true 时杀掉命令进程（供后台 shell 的 kill 操作）。
pub type CommandCancel = std::sync::Arc<std::sync::atomic::AtomicBool>;

/// 文件隔离-A:对工作区做 AppContainer ACL 授权是否会扰动?——被 dev-watch(改文件触发 rebuild/HMR)或
/// 过大(首次把 ACE 传播到每个文件会卡几十秒)。命中返回降级原因。结果按 workspace 进程级缓存,避免每命令
/// 重复预探。grant 传播不管 .gitignore,故预探数全部文件(含 target/node_modules),反映真实传播成本。
#[cfg(windows)]
fn workspace_grant_would_disrupt(workspace: &Path) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<std::path::PathBuf, Option<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = workspace.to_path_buf();
    if let Ok(map) = cache.lock() {
        if let Some(cached) = map.get(&key) {
            return cached.clone();
        }
    }
    let decision = compute_workspace_disruption(workspace);
    if let Ok(mut map) = cache.lock() {
        map.insert(key, decision.clone());
    }
    decision
}

#[cfg(windows)]
fn compute_workspace_disruption(workspace: &Path) -> Option<String> {
    // 1. dev-watch 启发式:工作区根或其直接子目录是会被文件监视器盯着的项目(Tauri/Vite/Next dev)。
    if let Some(marker) = dev_watch_marker(workspace) {
        return Some(format!(
            "工作区疑似在 dev 文件监视下({marker});AppContainer 授权会改文件触发 rebuild/HMR,已降级受限令牌沙箱"
        ));
    }
    // 2. 过大:首次授权要把 ACE 传播到每个文件。数全部文件(不跳 .gitignore,因 grant 也不跳),超预算早停。
    const GRANT_FILE_BUDGET: usize = 50_000;
    let mut count = 0usize;
    for entry in ignore::WalkBuilder::new(workspace)
        .git_ignore(false)
        .hidden(false)
        .build()
        .flatten()
    {
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            count += 1;
            if count > GRANT_FILE_BUDGET {
                return Some(format!(
                    "工作区文件数超 {GRANT_FILE_BUDGET};AppContainer 首次授权传播 ACL 会卡顿,已降级受限令牌沙箱(无路径隔离)"
                ));
            }
        }
    }
    None
}

/// dev-watch 启发式:工作区根或其直接子目录含 Tauri/Vite/Next 等会被 dev server 监视的项目特征文件。
/// 命中返回标志名(供日志)。纯路径检测,不保证项目"正在跑 dev";误报顶多损失路径隔离(安全侧)。
#[cfg(windows)]
fn dev_watch_marker(workspace: &Path) -> Option<String> {
    fn markers_in(dir: &Path) -> Option<String> {
        let probes: &[(&str, &str)] = &[
            ("src-tauri/tauri.conf.json", "Tauri"),
            ("vite.config.js", "Vite"),
            ("vite.config.ts", "Vite"),
            ("vite.config.mjs", "Vite"),
            ("next.config.js", "Next.js"),
            ("next.config.ts", "Next.js"),
            ("next.config.mjs", "Next.js"),
        ];
        probes
            .iter()
            .find(|(rel, _)| dir.join(rel).exists())
            .map(|(_, name)| name.to_string())
    }
    // 有界递归(深度 ≤3,0.0.47):catch 嵌套在 monorepo 子目录里的工程(如 apps/desktop/src-tauri,根下第 2 层)。
    // 只看「根 + 1 层」会漏掉深层嵌套工程、误判为可隔离 → 跑 AppContainer 改源码 ACL 触发 watcher rebuild。
    // 跳过 node_modules/target/.git 等海量目录,保证探测自身快(几十毫秒级)。
    fn search(dir: &Path, depth: u32) -> Option<String> {
        if let Some(m) = markers_in(dir) {
            return Some(m);
        }
        if depth == 0 {
            return None;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return None;
        };
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if matches!(
                name.as_ref(),
                "node_modules" | "target" | ".git" | "dist" | "build" | ".next" | ".svelte-kit"
            ) {
                continue;
            }
            if let Some(m) = search(&entry.path(), depth - 1) {
                return Some(m);
            }
        }
        None
    }
    search(workspace, 3)
}

/// 与 run_command 相同，但可选地把命令输出逐行回调给调用方，并可选地接受取消标志。
///
/// 回调在读取线程中触发，调用方需保证回调自身线程安全且不阻塞过久。
pub fn run_command_streaming(
    workspace_root: impl AsRef<Path>,
    request: RunCommandRequest,
    on_line: Option<CommandLineCallback>,
    cancel: Option<CommandCancel>,
    sandbox: bool,
    allow_network: bool,
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

    // 沙箱开启时优先走 AppContainer（真文件路径 + 网络隔离 + native 输出经 ConPTY 回传，见 appcontainer_win）：
    // AppContainer 令牌 Low IL + 默认拒绝文件/网络，仅显式授权工作区可读写；native console 程序输出经
    // 伪控制台 + lpacCom/LowBoxConsoleEnabled 回传。该 Windows 版本不支持 AppContainer 时 fail-closed
    // 降级到受限令牌沙箱（剥权 + Job Object + 擦密钥，绝不裸跑）。非 Windows 平台无沙箱能力则报错。
    if sandbox {
        #[cfg(windows)]
        {
            // 文件隔离-A 先判:工作区被 dev-watch 或过大时,AppContainer 首次授权会改工作区每个文件 SD
            // (触发 tauri/vite watcher rebuild + 大 repo 卡顿)。fail-soft 降级受限令牌沙箱(不碰用户 ACL)。
            // 放在自检之前(0.0.47):注定降级的工作区直接走受限沙箱,跳过 AppContainer 自检(建容器 profile +
            // ConPTY 探针,有固定开销)——省掉这类工作区首条命令的等待;自检改为仅在真要上 AppContainer 时才惰性跑。
            if let Some(reason) = workspace_grant_would_disrupt(&workspace) {
                eprintln!("[sandbox] {reason}");
                return sandbox_win::run_in_restricted_sandbox(
                    &workspace, command, timeout, on_line, cancel,
                )
                .map(|mut r| {
                    r.sandbox_degraded = true;
                    r
                });
            }
            // 运行时能力自检(每进程一次,缓存):实测本机 AppContainer 能否回传 native 输出。各版本 Windows 对
            // LowBoxConsoleEnabled/ConPTY 支持不一,实测为准而非赌版本号;不通过即降级。仅非降级工作区才需要。
            static APPCONTAINER_OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            let use_ac = *APPCONTAINER_OK.get_or_init(appcontainer_win::appcontainer_self_test);
            if use_ac {
                match appcontainer_win::run_in_appcontainer_sandbox(
                    &workspace,
                    command,
                    timeout,
                    on_line.clone(),
                    cancel.clone(),
                    allow_network,
                ) {
                    Ok(result) => return Ok(result),
                    Err(e) => {
                        eprintln!("[sandbox] AppContainer 执行失败,降级到受限令牌沙箱: {e}");
                        return sandbox_win::run_in_restricted_sandbox(
                            &workspace, command, timeout, on_line, cancel,
                        )
                        .map(|mut r| {
                            r.sandbox_degraded = true;
                            r
                        });
                    }
                }
            } else {
                // 本机 AppContainer 自检未通过(版本不支持 / 能力缺失 / 输出不回传),fail-closed 走受限令牌沙箱。
                eprintln!("[sandbox] AppContainer 自检未通过,使用受限令牌沙箱");
                return sandbox_win::run_in_restricted_sandbox(
                    &workspace, command, timeout, on_line, cancel,
                )
                .map(|mut r| {
                    r.sandbox_degraded = true;
                    r
                });
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (&on_line, &cancel, allow_network);
            return Err(ToolRuntimeError::CommandFailed(
                "命令沙箱仅在 Windows 上可用；请在设置中关闭沙箱后再执行命令".to_string(),
            ));
        }
    }

    let mut builder = Command::new("powershell");
    builder
        .args(["-NoProfile", "-NonInteractive", "-Command", command])
        .current_dir(&workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // 沙箱加固：从子进程环境中擦除敏感凭据，防止 Agent 命令通过环境变量读取或外泄
    // API Key 等密钥（Plan06 / Plan09 要求：默认不把 DEEPSEEK_API_KEY 传给子进程）。
    scrub_secret_env(&mut builder);
    let mut child = builder
        .spawn()
        .map_err(|err| ToolRuntimeError::CommandFailed(err.to_string()))?;

    // 用独立线程抽干 stdout/stderr，避免管道缓冲填满导致死锁；有回调时逐行转发。
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let out_cb = on_line.clone();
    let err_cb = on_line;
    let stdout_handle = std::thread::spawn(move || drain_pipe_streaming(stdout_pipe, out_cb));
    let stderr_handle = std::thread::spawn(move || drain_pipe_streaming(stderr_pipe, err_cb));

    let started = Instant::now();
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                // 外部取消（后台 shell kill）：置位则杀进程收尾。
                if cancel
                    .as_ref()
                    .map(|c| c.load(std::sync::atomic::Ordering::SeqCst))
                    .unwrap_or(false)
                {
                    let _ = child.kill();
                    break;
                }
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
        sandbox_layer: None,
        sandbox_degraded: false,
    })
}

/// 从命令子进程的环境中移除敏感凭据变量（API Key / token / secret 等）。
///
/// 精确移除已知密钥变量，并按名称模式移除疑似密钥的变量；保留 PATH 等正常变量，
/// 不做 env_clear（清空会破坏 PATH 导致命令找不到），是性能/可用性与安全的平衡。
fn scrub_secret_env(builder: &mut Command) {
    const EXACT: &[&str] = &["DEEPSEEK_API_KEY"];
    for name in EXACT {
        builder.env_remove(name);
    }
    // 按模式移除：变量名含 API_KEY / SECRET / TOKEN / PASSWORD 的一律不传给子进程。
    let suspicious: Vec<String> = std::env::vars()
        .map(|(k, _)| k)
        .filter(|k| {
            let upper = k.to_uppercase();
            upper.contains("API_KEY")
                || upper.contains("APIKEY")
                || upper.contains("SECRET")
                || upper.contains("_TOKEN")
                || upper.contains("PASSWORD")
        })
        .collect();
    for name in suspicious {
        builder.env_remove(name);
    }
}

/// 逐行读取子进程管道：UTF-8 lossy 解码、截断到 MAX_COMMAND_OUTPUT_BYTES，
/// 可选地把每行实时回调给调用方。
fn drain_pipe_streaming(
    pipe: Option<impl Read>,
    on_line: Option<CommandLineCallback>,
) -> (String, bool) {
    let Some(pipe) = pipe else {
        return (String::new(), false);
    };
    let reader = std::io::BufReader::new(pipe);
    let mut collected = String::new();
    let mut truncated = false;
    let mut raw_line = Vec::new();
    let mut reader = reader;
    loop {
        raw_line.clear();
        match std::io::BufRead::read_until(&mut reader, b'\n', &mut raw_line) {
            Ok(0) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&raw_line).to_string();
                if let Some(cb) = on_line.as_ref() {
                    cb(line.trim_end_matches(['\r', '\n']).to_string());
                }
                if collected.len() < MAX_COMMAND_OUTPUT_BYTES {
                    collected.push_str(&line);
                    if collected.len() > MAX_COMMAND_OUTPUT_BYTES {
                        collected.truncate(MAX_COMMAND_OUTPUT_BYTES);
                        truncated = true;
                    }
                } else {
                    truncated = true;
                }
            }
            Err(_) => break,
        }
    }
    (collected, truncated)
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

// ── Workspace Map（项目结构摘要） ──────────────────────────────────────────

/// 生成 repo map 时忽略的噪声目录（构建产物、依赖、缓存等）。
const MAP_IGNORE_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "venv",
    "__pycache__",
    "coverage",
];
const MAP_MAX_ENTRIES: usize = 200;
const MAP_MAX_DEPTH: usize = 3;

/// 生成工作区结构摘要（紧凑目录树），用于在会话开局注入模型上下文。
///
/// 输入工作区根目录；输出多行目录树字符串，目录在前、按名称排序。忽略 `.git`、隐藏目录、
/// `node_modules`、`target` 等噪声目录，限制深度（{MAP_MAX_DEPTH}）与条目数（{MAP_MAX_ENTRIES}），
/// 超限时追加省略提示，避免上下文膨胀。只读取目录条目名，不读取文件内容。
pub fn workspace_map(workspace_root: &str) -> String {
    let root = Path::new(workspace_root);
    if !root.is_dir() {
        return String::new();
    }
    let mut lines = Vec::new();
    let mut count = 0usize;
    let mut truncated = false;
    walk_workspace_map(root, 0, &mut lines, &mut count, &mut truncated);
    if truncated {
        lines.push("…（结构过大，已省略部分条目）".to_string());
    }
    lines.join("\n")
}

fn walk_workspace_map(
    dir: &Path,
    depth: usize,
    lines: &mut Vec<String>,
    count: &mut usize,
    truncated: &mut bool,
) {
    if depth >= MAP_MAX_DEPTH || *truncated {
        return;
    }
    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(read_dir) => read_dir.flatten().collect(),
        Err(_) => return,
    };
    // 目录在前，再按名称（小写）排序，输出稳定可读。
    entries.sort_by_key(|entry| {
        let is_dir = entry.path().is_dir();
        (!is_dir, entry.file_name().to_string_lossy().to_lowercase())
    });

    for entry in entries {
        if *count >= MAP_MAX_ENTRIES {
            *truncated = true;
            return;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        let is_dir = path.is_dir();
        // 跳过噪声目录与隐藏目录（.git/.idea 等）；隐藏文件（.gitignore 等）保留。
        if is_dir && (name.starts_with('.') || MAP_IGNORE_DIRS.contains(&name.as_str())) {
            continue;
        }
        let indent = "  ".repeat(depth);
        if is_dir {
            lines.push(format!("{indent}{name}/"));
        } else {
            lines.push(format!("{indent}{name}"));
        }
        *count += 1;
        if is_dir {
            walk_workspace_map(&path, depth + 1, lines, count, truncated);
        }
    }
}

/// 平铺列出工作区内的文件相对路径（@文件引用补全用），忽略噪声目录，cap 限制总量。
pub fn workspace_file_list(workspace_root: &str, cap: usize) -> Vec<String> {
    let root = Path::new(workspace_root);
    if !root.is_dir() {
        return Vec::new();
    }
    let mut files = Vec::new();
    collect_files_flat(root, root, &mut files, cap);
    files
}

fn collect_files_flat(root: &Path, dir: &Path, files: &mut Vec<String>, cap: usize) {
    if files.len() >= cap {
        return;
    }
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = read_dir.flatten().collect();
    entries.sort_by_key(|entry| {
        let is_dir = entry.path().is_dir();
        (!is_dir, entry.file_name().to_string_lossy().to_lowercase())
    });
    for entry in entries {
        if files.len() >= cap {
            return;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if path.is_dir() {
            if name.starts_with('.') || MAP_IGNORE_DIRS.contains(&name.as_str()) {
                continue;
            }
            collect_files_flat(root, &path, files, cap);
        } else if let Ok(rel) = path.strip_prefix(root) {
            files.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(windows)]
    #[test]
    fn workspace_disrupt_detects_dev_watch() {
        let base = std::env::temp_dir().join(format!("mdga-disrupt-{}", std::process::id()));
        let plain = base.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(plain.join("a.txt"), "x").unwrap();
        assert!(
            compute_workspace_disruption(&plain).is_none(),
            "普通小目录不应降级"
        );
        let vite = base.join("vite_proj");
        std::fs::create_dir_all(&vite).unwrap();
        std::fs::write(vite.join("vite.config.ts"), "export default {}").unwrap();
        assert!(
            compute_workspace_disruption(&vite).is_some(),
            "Vite 项目应降级"
        );
        let mono = base.join("mono");
        std::fs::create_dir_all(mono.join("app/src-tauri")).unwrap();
        std::fs::write(mono.join("app/src-tauri/tauri.conf.json"), "{}").unwrap();
        assert!(
            dev_watch_marker(&mono).is_some(),
            "子目录 Tauri 项目应被检测到"
        );
        // 0.0.47:更深的嵌套(apps/desktop/src-tauri,根下第 2 层)也要被有界递归检测到。
        let deep = base.join("deep");
        std::fs::create_dir_all(deep.join("apps/desktop/src-tauri")).unwrap();
        std::fs::write(deep.join("apps/desktop/src-tauri/tauri.conf.json"), "{}").unwrap();
        assert!(
            dev_watch_marker(&deep).is_some(),
            "深层嵌套(apps/desktop)的 Tauri 项目应被有界递归检测到"
        );
        // node_modules 等海量目录内的配置文件不应被当作 dev-watch(探测应跳过这些目录)。
        let skip = base.join("skipdir");
        std::fs::create_dir_all(skip.join("node_modules/pkg")).unwrap();
        std::fs::write(skip.join("node_modules/pkg/vite.config.ts"), "export default {}").unwrap();
        assert!(
            dev_watch_marker(&skip).is_none(),
            "node_modules 内的配置不应被当作 dev-watch"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// B 修复回归(0.0.45):受限令牌沙箱(文件隔离-A 降级目标)应能跑 native 命令并回传输出,
    /// 而非进程启动即失败——此前 lpDesktop=NULL + deny-only Admin 致 echo/dir/whoami 全 exit -1073741502。
    #[cfg(windows)]
    #[test]
    fn restricted_sandbox_runs_native_command() {
        let ws = std::env::temp_dir().join(format!("mdga-restricted-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let r = sandbox_win::run_in_restricted_sandbox(
            &ws,
            "cmd /c echo RESTRICTED_OK",
            std::time::Duration::from_secs(20),
            None,
            None,
        );
        let _ = std::fs::remove_dir_all(&ws);
        let out = r.expect("受限令牌沙箱起进程失败");
        eprintln!("[restricted] exit={:?} stdout=[{}]", out.exit_code, out.stdout.trim());
        assert!(
            out.stdout.contains("RESTRICTED_OK"),
            "受限令牌沙箱应能跑 native 命令并回传输出(此前 exit -1073741502 启动失败):\n{}",
            out.stdout
        );
        assert_eq!(out.sandbox_layer.as_deref(), Some("restricted"));
    }

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
    fn workspace_map_lists_tree_and_skips_noise_dirs() {
        let workspace = temp_workspace();
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(workspace.join("src/main.rs"), "fn main(){}").unwrap();
        std::fs::write(workspace.join("Cargo.toml"), "[package]").unwrap();
        // 噪声目录应被忽略
        std::fs::create_dir_all(workspace.join("target/debug")).unwrap();
        std::fs::write(workspace.join("target/debug/app.exe"), "bin").unwrap();
        std::fs::create_dir_all(workspace.join(".git")).unwrap();
        std::fs::write(workspace.join(".git/config"), "x").unwrap();

        let map = workspace_map(workspace.to_str().unwrap());

        assert!(map.contains("src/"));
        assert!(map.contains("main.rs"));
        assert!(map.contains("Cargo.toml"));
        assert!(!map.contains("target"));
        assert!(!map.contains(".git"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn workspace_map_returns_empty_for_missing_dir() {
        assert_eq!(workspace_map("C:\\definitely\\not\\here\\mdga"), "");
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
                offset: 0,
                limit: 0,
            },
        )
        .expect("file should read");

        assert_eq!(result.relative_path, "note.txt");
        assert_eq!(result.content, "hello");
        assert_eq!(result.total_lines, 1);
        assert!(!result.has_more);

        // 分页：写多行后从 offset 读取
        std::fs::write(workspace.join("big.txt"), "l0\nl1\nl2\nl3\nl4").unwrap();
        let page = read_file(
            &workspace,
            ReadFileRequest { path: "big.txt".to_string(), offset: 1, limit: 2 },
        )
        .expect("paged read");
        assert_eq!(page.content, "l1\nl2");
        assert_eq!(page.total_lines, 5);
        assert_eq!(page.start_line, 1);
        assert_eq!(page.returned_lines, 2);
        assert!(page.has_more);

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
                is_regex: false,
                ..Default::default()
            },
        )
        .expect("text should search");

        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].path, "src/lib.rs");
        assert_eq!(result.matches[0].line, 2);

        // 正则匹配 + gitignore 感知：被 .gitignore 命中的文件不应被搜到
        std::fs::write(workspace.join(".gitignore"), "secret.txt\n").unwrap();
        std::fs::write(workspace.join("secret.txt"), "let token = 9;").unwrap();
        let re = search_text(
            &workspace,
            SearchTextRequest {
                path: ".".to_string(),
                query: r"let \w+ =".to_string(),
                max_results: 10,
                is_regex: true,
                ..Default::default()
            },
        )
        .expect("regex search");
        assert!(re.matches.iter().any(|m| m.path == "src/lib.rs"));
        assert!(!re.matches.iter().any(|m| m.path == "secret.txt"), "gitignore 命中应被跳过");

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn glob_files_matches_by_name_and_path() {
        let workspace = temp_workspace();
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(workspace.join("src/lib.rs"), "x").unwrap();
        std::fs::write(workspace.join("src/main.rs"), "x").unwrap();
        std::fs::write(workspace.join("README.md"), "x").unwrap();

        let rs = glob_files(
            &workspace,
            GlobFilesRequest { pattern: "*.rs".to_string(), max_results: 50, ..Default::default() },
        )
        .expect("glob should run");
        assert_eq!(rs.files.len(), 2, "*.rs 应命中嵌套的两个 .rs");
        assert!(rs.files.iter().all(|f| f.ends_with(".rs")));

        let nested = glob_files(
            &workspace,
            GlobFilesRequest { pattern: "src/**".to_string(), max_results: 50, ..Default::default() },
        )
        .expect("glob should run");
        assert!(nested.files.iter().any(|f| f == "src/lib.rs"));
        assert!(!nested.files.iter().any(|f| f == "README.md"), "src/** 不应命中根级文件");

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn search_text_count_and_files_modes() {
        let workspace = temp_workspace();
        std::fs::write(workspace.join("a.txt"), "foo\nfoo\nbar").unwrap();
        std::fs::write(workspace.join("b.txt"), "baz").unwrap();

        let counts = search_text(
            &workspace,
            SearchTextRequest {
                path: ".".to_string(),
                query: "foo".to_string(),
                max_results: 50,
                output_mode: Some("count".to_string()),
                ..Default::default()
            },
        )
        .expect("count search");
        assert_eq!(counts.output_mode, "count");
        assert!(counts.counts.iter().any(|c| c.path == "a.txt" && c.count == 2));

        let files = search_text(
            &workspace,
            SearchTextRequest {
                path: ".".to_string(),
                query: "foo".to_string(),
                max_results: 50,
                output_mode: Some("files_with_matches".to_string()),
                ..Default::default()
            },
        )
        .expect("files search");
        assert_eq!(files.files, vec!["a.txt".to_string()]);

        // 上下文行：-C 1 应带上下文。
        let ctx = search_text(
            &workspace,
            SearchTextRequest {
                path: ".".to_string(),
                query: "bar".to_string(),
                max_results: 50,
                context: 1,
                ..Default::default()
            },
        )
        .expect("context search");
        assert_eq!(ctx.matches.len(), 1);
        assert_eq!(ctx.matches[0].before, vec!["foo".to_string()]);

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
                background: false,
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
                background: false,
            },
        )
        .expect_err("empty command should be rejected");
        assert!(matches!(err, ToolRuntimeError::EmptyCommand));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn code_overview_counts_rust_symbols_tests_and_build_files() {
        let workspace = temp_workspace();
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        std::fs::write(
            workspace.join("src/lib.rs"),
            // 3 个公开符号（pub fn / pub struct / pub(crate) const）、2 个测试（#[test] / #[tokio::test]）。
            "pub fn alpha() {}\n\
             pub struct Beta;\n\
             pub(crate) const GAMMA: u32 = 1;\n\
             fn private_helper() {}\n\
             \n\
             #[test]\n\
             fn t1() {}\n\
             #[tokio::test]\n\
             async fn t2() {}\n",
        )
        .unwrap();

        let result = code_overview(
            &workspace,
            CodeOverviewRequest { path: ".".to_string() },
        )
        .expect("overview should run");

        assert!(result.ok);
        assert!(!result.is_file);
        let rs = result.by_language.get("rs").expect("rs language present");
        assert_eq!(rs.symbols, 3, "pub fn/struct/const 应计 3 个公开符号");
        assert_eq!(rs.tests, 2, "#[test] 与 #[tokio::test] 应计 2 个测试");
        assert!(rs.loc > 0);
        // Cargo.toml 应被识别为构建文件，并给出 cargo 求真命令。
        assert!(result.build_files.iter().any(|f| f == "Cargo.toml"));
        assert!(result
            .suggested_verify_commands
            .iter()
            .any(|c| c == "cargo test --workspace"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn code_overview_counts_typescript_exports_and_tests() {
        let workspace = temp_workspace();
        std::fs::write(
            workspace.join("package.json"),
            "{ \"name\": \"demo\", \"scripts\": { \"test\": \"vitest\" } }",
        )
        .unwrap();
        std::fs::write(
            workspace.join("util.ts"),
            // 2 个导出符号（export function / export const）；2 个测试调用（describe( / it(）。
            "export function add(a: number, b: number) { return a + b; }\n\
             export const NAME = \"demo\";\n\
             function helper() {}\n\
             describe(\"add\", () => {\n\
               it(\"adds\", () => {});\n\
             });\n",
        )
        .unwrap();

        let result = code_overview(
            &workspace,
            CodeOverviewRequest { path: ".".to_string() },
        )
        .expect("overview should run");

        let ts = result.by_language.get("ts").expect("ts language present");
        assert!(ts.symbols >= 2, "export function/const 应至少计 2 个符号，实得 {}", ts.symbols);
        assert_eq!(ts.tests, 2, "describe( 与 it( 应计 2 个测试");
        // package.json -> npm 求真命令。
        assert!(result.build_files.iter().any(|f| f == "package.json"));
        assert!(result.suggested_verify_commands.iter().any(|c| c == "npm test"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn code_overview_single_file_and_gitignore_aware() {
        let workspace = temp_workspace();
        std::fs::write(workspace.join(".gitignore"), "ignored.rs\n").unwrap();
        std::fs::write(workspace.join("kept.rs"), "pub fn kept() {}\n").unwrap();
        std::fs::write(workspace.join("ignored.rs"), "pub fn ignored() {}\n").unwrap();

        // 目录聚合：被 .gitignore 命中的文件不应进入统计。
        let dir = code_overview(
            &workspace,
            CodeOverviewRequest { path: ".".to_string() },
        )
        .expect("dir overview");
        let rs = dir.by_language.get("rs").expect("rs present");
        assert_eq!(rs.files, 1, "ignored.rs 应被 .gitignore 跳过");
        assert_eq!(rs.symbols, 1);

        // 单文件模式：is_file=true、只统计该文件。
        let single = code_overview(
            &workspace,
            CodeOverviewRequest { path: "kept.rs".to_string() },
        )
        .expect("single-file overview");
        assert!(single.is_file);
        assert_eq!(single.total_files, 1);
        assert_eq!(single.by_language.get("rs").unwrap().symbols, 1);

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn code_overview_aggregates_multiple_packages() {
        let workspace = temp_workspace();
        // 两个子包：一个 Cargo crate、一个 npm 包，验证多包检测与多套求真命令聚合。
        std::fs::create_dir_all(workspace.join("crates/core/src")).unwrap();
        std::fs::write(workspace.join("crates/core/Cargo.toml"), "[package]\nname=\"core\"\n").unwrap();
        std::fs::write(workspace.join("crates/core/src/lib.rs"), "pub fn x() {}\n").unwrap();
        std::fs::create_dir_all(workspace.join("web/src")).unwrap();
        std::fs::write(workspace.join("web/package.json"), "{ \"name\": \"web\" }").unwrap();
        std::fs::write(workspace.join("web/src/app.ts"), "export const A = 1;\n").unwrap();

        let result = code_overview(
            &workspace,
            CodeOverviewRequest { path: ".".to_string() },
        )
        .expect("repo-root overview");

        // 两套构建系统都应被检测到。
        assert!(result.packages.iter().any(|p| p.kind == "cargo"));
        assert!(result.packages.iter().any(|p| p.kind == "npm"));
        assert!(result.suggested_verify_commands.iter().any(|c| c == "cargo test --workspace"));
        assert!(result.suggested_verify_commands.iter().any(|c| c == "npm test"));
        // 两种语言都应出现在聚合里。
        assert!(result.by_language.contains_key("rs"));
        assert!(result.by_language.contains_key("ts"));

        let _ = std::fs::remove_dir_all(workspace);
    }
}
