//! mdga-lsp（R1）：用 Language Server Protocol 给 Agent 编译器级代码智能。
//!
//! 提供 4 个**只读**工具：
//! - `lsp_definition`：某位置符号的定义跳转
//! - `lsp_references`：某位置符号的全部引用
//! - `lsp_hover`：某位置的类型/签名/文档
//! - `lsp_diagnostics`：某文件的错误/警告
//!
//! 实现：按文件扩展名从**硬编码白名单**解析语言服务器（.rs→rust-analyzer；
//! .ts/.tsx/.js/.jsx/.mjs/.cjs→typescript-language-server --stdio；.py/.pyi→pyright-langserver --stdio），
//! 在工作区目录下拉起子进程，走 stdio 上 Content-Length 帧化的 JSON-RPC，做
//! initialize→initialized→didOpen→请求→shutdown，做完即关、Drop 强杀子进程。
//!
//! 安全（强约束，对齐 tool-runtime 的 git/run_command）：
//! - 路径用 `validate_relative_path`/`canonical_workspace` 同款逻辑做工作区内校验（拒绝 `..` 与绝对路径）；
//! - 服务器子进程 cwd=工作区、擦除密钥环境变量（API Key 等）；
//! - 服务器程序与参数**全部硬编码**，绝不接受 config/模型输入的任意命令；
//! - 整条操作有硬超时，超时/Drop 都强杀子进程（无泄漏、无挂死）；
//! - 缺少服务器二进制 → 清晰的 `ServerUnavailable` 错误，绝不挂起。

mod client;
mod framing;
mod server;

use client::{file_uri_for, LspSession};
use serde::{Deserialize, Serialize};
use server::resolve_server;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use thiserror::Error;

const MAX_DOC_BYTES: u64 = 4 * 1024 * 1024;

// ── 错误类型 ────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum LspError {
    #[error("工具路径必须位于当前工作区内")]
    PathOutsideWorkspace,
    #[error("工具路径不能为空")]
    EmptyPath,
    #[error("工作区路径不可用: {0}")]
    WorkspaceUnavailable(String),
    #[error("目标文件不存在")]
    PathNotFound,
    #[error("目标不是文件")]
    NotAFile,
    #[error("文件过大，超过 {0} 字节限制")]
    FileTooLarge(u64),
    #[error("文件不是有效 UTF-8 文本")]
    NonUtf8Text,
    #[error("不支持的语言: {0}")]
    Unsupported(String),
    #[error("语言服务器不可用: {0}")]
    ServerUnavailable(String),
    #[error("LSP 协议错误: {0}")]
    Protocol(String),
    #[error("LSP 操作超时")]
    Timeout,
    #[error("文件系统错误: {0}")]
    Io(#[from] std::io::Error),
}

// ── 请求类型（0 基行/列，工作区相对路径） ──────────────────────────────────

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LspPositionRequest {
    /// 工作区相对文件路径。
    pub path: String,
    /// 0 基行号。
    #[serde(default)]
    pub line: u32,
    /// 0 基列号（character offset）。
    #[serde(default)]
    pub character: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LspDiagnosticsRequest {
    /// 工作区相对文件路径。
    pub path: String,
}

// ── 结果类型（结构化 path/line/character/text，非裸 JSON） ───────────────────

/// 一个源码位置（0 基行/列 + 工作区相对路径，附该行文本预览）。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LspLocation {
    /// 工作区相对路径（命中工作区外时回退为绝对/原始路径）。
    pub path: String,
    /// 0 基行号。
    pub line: u32,
    /// 0 基列号。
    pub character: u32,
    /// 该位置所在行的文本（去首尾空白，截断到 200 字符）；取不到为空串。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LspLocationsResult {
    pub locations: Vec<LspLocation>,
    pub count: usize,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LspHoverResult {
    /// 渲染为纯文本的 hover 内容（类型/签名/文档）；无悬浮信息时为空串。
    pub contents: String,
    /// 是否有可用的 hover 信息。
    pub found: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LspDiagnostic {
    /// 起始 0 基行。
    pub line: u32,
    /// 起始 0 基列。
    pub character: u32,
    /// 严重度：error / warning / information / hint / unknown。
    pub severity: String,
    /// 诊断信息文本。
    pub message: String,
    /// 诊断来源（如 rustc、rust-analyzer、typescript）；无则空串。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LspDiagnosticsResult {
    pub path: String,
    pub diagnostics: Vec<LspDiagnostic>,
    pub count: usize,
}

// ── 安全/路径辅助（复用 tool-runtime 的同款模式） ─────────────────────────────

/// 从语言服务器子进程环境中移除敏感凭据变量（与 tool-runtime::scrub_secret_env 等价）。
pub(crate) fn scrub_secret_env(builder: &mut Command) {
    const EXACT: &[&str] = &["DEEPSEEK_API_KEY"];
    for name in EXACT {
        builder.env_remove(name);
    }
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

/// 校验一个工作区内相对路径（拒绝绝对路径 / `..` 逃逸），与 tool-runtime::validate_relative_path 等价。
fn validate_relative_path(path: &str) -> Result<PathBuf, LspError> {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "." {
        return Err(LspError::EmptyPath);
    }
    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        return Err(LspError::PathOutsideWorkspace);
    }
    let mut safe = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => safe.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(LspError::PathOutsideWorkspace);
            }
        }
    }
    if safe.as_os_str().is_empty() {
        return Err(LspError::EmptyPath);
    }
    Ok(safe)
}

/// 规范化工作区根（与 tool-runtime::canonical_workspace 等价）。
fn canonical_workspace(workspace_root: impl AsRef<Path>) -> Result<PathBuf, LspError> {
    workspace_root
        .as_ref()
        .canonicalize()
        .map_err(|err| LspError::WorkspaceUnavailable(err.to_string()))
}

/// 校验相对路径并解析到工作区内的已存在文件，返回（工作区根, 安全相对路径, 规范化文件绝对路径）。
fn resolve_existing_file(
    workspace_root: impl AsRef<Path>,
    path: &str,
) -> Result<(PathBuf, PathBuf, PathBuf), LspError> {
    let relative = validate_relative_path(path)?;
    let workspace = canonical_workspace(workspace_root)?;
    let candidate = workspace.join(&relative);
    if !candidate.exists() {
        return Err(LspError::PathNotFound);
    }
    let target = candidate
        .canonicalize()
        .map_err(|e| LspError::WorkspaceUnavailable(e.to_string()))?;
    if !target.starts_with(&workspace) {
        return Err(LspError::PathOutsideWorkspace);
    }
    if !target.is_file() {
        return Err(LspError::NotAFile);
    }
    Ok((workspace, relative, target))
}

fn read_doc_text(target: &Path) -> Result<String, LspError> {
    let meta = std::fs::metadata(target)?;
    if meta.len() > MAX_DOC_BYTES {
        return Err(LspError::FileTooLarge(MAX_DOC_BYTES));
    }
    let bytes = std::fs::read(target)?;
    String::from_utf8(bytes).map_err(|_| LspError::NonUtf8Text)
}

fn normalize_relative(path: &Path) -> String {
    let s: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(p) => Some(p.to_string_lossy().to_string()),
            _ => None,
        })
        .collect();
    s.join("/")
}

// ── 公共工具入口 ──────────────────────────────────────────────────────────────

/// 共用准备：校验路径 → 解析服务器 → 启动会话 → didOpen 目标文件。
/// 返回 (会话, 目标文件 uri, 工作区根, 安全相对路径)。
fn open_session(
    workspace_root: impl AsRef<Path>,
    path: &str,
) -> Result<(LspSession, String, PathBuf, PathBuf), LspError> {
    let (workspace, relative, target) = resolve_existing_file(workspace_root, path)?;
    let spec = resolve_server(&normalize_relative(&relative))?;
    let text = read_doc_text(&target)?;
    let uri = file_uri_for(&workspace, &relative);
    let mut session = LspSession::start(&spec, &workspace)?;
    session.did_open(&uri, spec.language_id, &text)?;
    Ok((session, uri, workspace, relative))
}

/// lsp_definition：某位置符号的定义跳转。
pub fn lsp_definition(
    workspace_root: impl AsRef<Path>,
    request: LspPositionRequest,
) -> Result<LspLocationsResult, LspError> {
    let (mut session, uri, workspace, _rel) = open_session(&workspace_root, &request.path)?;
    let result = session.request_until_ready(
        "textDocument/definition",
        position_params(&uri, request.line, request.character),
        is_empty_locations,
    );
    session.shutdown();
    let result = result?;
    let locations = parse_locations(&result, &workspace);
    Ok(LspLocationsResult {
        count: locations.len(),
        locations,
    })
}

/// 判定 definition/references 结果是否「空」（null 或空数组），用于索引就绪前的重试。
fn is_empty_locations(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => true,
        serde_json::Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

/// lsp_references：某位置符号的全部引用（含声明）。
pub fn lsp_references(
    workspace_root: impl AsRef<Path>,
    request: LspPositionRequest,
) -> Result<LspLocationsResult, LspError> {
    let (mut session, uri, workspace, _rel) = open_session(&workspace_root, &request.path)?;
    let mut params = position_params(&uri, request.line, request.character);
    params["context"] = serde_json::json!({ "includeDeclaration": true });
    let result =
        session.request_until_ready("textDocument/references", params, is_empty_locations);
    session.shutdown();
    let result = result?;
    let locations = parse_locations(&result, &workspace);
    Ok(LspLocationsResult {
        count: locations.len(),
        locations,
    })
}

/// lsp_hover：某位置的类型/签名/文档。
pub fn lsp_hover(
    workspace_root: impl AsRef<Path>,
    request: LspPositionRequest,
) -> Result<LspHoverResult, LspError> {
    let (mut session, uri, _ws, _rel) = open_session(&workspace_root, &request.path)?;
    let result = session.request_until_ready(
        "textDocument/hover",
        position_params(&uri, request.line, request.character),
        |v| v.is_null() || parse_hover(v).is_empty(),
    );
    session.shutdown();
    let result = result?;
    let contents = parse_hover(&result);
    Ok(LspHoverResult {
        found: !contents.is_empty(),
        contents,
    })
}

/// lsp_diagnostics：某文件的错误/警告（收集 publishDiagnostics 推送）。
pub fn lsp_diagnostics(
    workspace_root: impl AsRef<Path>,
    request: LspDiagnosticsRequest,
) -> Result<LspDiagnosticsResult, LspError> {
    let (mut session, uri, _ws, relative) = open_session(&workspace_root, &request.path)?;
    let raw = session.collect_diagnostics(&uri);
    session.shutdown();
    let raw = raw?;
    let diagnostics = parse_diagnostics(&raw);
    Ok(LspDiagnosticsResult {
        path: normalize_relative(&relative),
        count: diagnostics.len(),
        diagnostics,
    })
}

// ── 响应解析（把裸 LSP JSON 转成结构化结果） ─────────────────────────────────

fn position_params(uri: &str, line: u32, character: u32) -> serde_json::Value {
    serde_json::json!({
        "textDocument": { "uri": uri },
        "position": { "line": line, "character": character }
    })
}

/// 把 file:// URI 转回工作区相对路径；落在工作区外则回退为去掉 scheme 的路径。
fn uri_to_relative(uri: &str, workspace: &Path) -> String {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    // Windows: file:///C:/x → /C:/x，去掉前导斜杠。
    let raw = if raw.starts_with('/')
        && raw.len() > 2
        && raw.as_bytes()[2] == b':'
        && raw[1..2].chars().all(|c| c.is_ascii_alphabetic())
    {
        &raw[1..]
    } else {
        raw
    };
    let decoded = percent_decode(raw).replace('\\', "/");
    // 工作区根做同样的正斜杠化 + 剥离 Windows verbatim 前缀，才能与 URI 路径前缀比对。
    let ws = strip_verbatim(&workspace.to_string_lossy().replace('\\', "/"));
    // 大小写不敏感地比对前缀（Windows 盘符大小写不固定）。
    let decoded_cmp = decoded.to_ascii_lowercase();
    let ws_cmp = ws.to_ascii_lowercase();
    if let Some(stripped) = decoded_cmp.strip_prefix(&ws_cmp) {
        let rel = decoded[decoded.len() - stripped.len()..].trim_start_matches('/');
        if !rel.is_empty() {
            return rel.to_string();
        }
    }
    decoded
}

/// 剥离 Windows verbatim（`//?/`）前缀，返回普通形式（已正斜杠化的输入）。
fn strip_verbatim(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("//?/UNC/") {
        format!("//{rest}")
    } else if let Some(rest) = s.strip_prefix("//?/") {
        rest.to_string()
    } else {
        s.to_string()
    }
}

/// 极简 percent-decode：仅还原 LSP URI 常见的 `%20`/`%3A` 等转义（够用即可，非完备实现）。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 取某文件某 0 基行的文本预览（去首尾空白、截断 200 字符）；读不到返回空串。
fn line_preview(workspace: &Path, rel_path: &str, line: u32) -> String {
    let full = workspace.join(rel_path);
    let Ok(content) = std::fs::read_to_string(&full) else {
        return String::new();
    };
    content
        .lines()
        .nth(line as usize)
        .map(|l| l.trim().chars().take(200).collect())
        .unwrap_or_default()
}

/// 解析 definition/references 的结果：可能是单个 Location、Location 数组，或 LocationLink 数组。
fn parse_locations(result: &serde_json::Value, workspace: &Path) -> Vec<LspLocation> {
    let mut out = Vec::new();
    let items: Vec<&serde_json::Value> = match result {
        serde_json::Value::Array(arr) => arr.iter().collect(),
        serde_json::Value::Null => return out,
        single => vec![single],
    };
    for item in items {
        // Location: { uri, range }; LocationLink: { targetUri, targetRange/targetSelectionRange }
        let (uri, range) = if let Some(uri) = item.get("uri") {
            (uri.as_str(), item.get("range"))
        } else if let Some(uri) = item.get("targetUri") {
            (
                uri.as_str(),
                item.get("targetSelectionRange").or_else(|| item.get("targetRange")),
            )
        } else {
            (None, None)
        };
        let (Some(uri), Some(range)) = (uri, range) else {
            continue;
        };
        let start = range.get("start");
        let line = start
            .and_then(|s| s.get("line"))
            .and_then(|l| l.as_u64())
            .unwrap_or(0) as u32;
        let character = start
            .and_then(|s| s.get("character"))
            .and_then(|c| c.as_u64())
            .unwrap_or(0) as u32;
        let rel = uri_to_relative(uri, workspace);
        let text = line_preview(workspace, &rel, line);
        out.push(LspLocation {
            path: rel,
            line,
            character,
            text,
        });
    }
    out
}

/// 解析 hover 结果（MarkupContent / MarkedString / 其数组）为纯文本。
fn parse_hover(result: &serde_json::Value) -> String {
    let Some(contents) = result.get("contents") else {
        return String::new();
    };
    let text = marked_to_text(contents);
    text.trim().to_string()
}

fn marked_to_text(v: &serde_json::Value) -> String {
    match v {
        // MarkupContent: { kind, value }
        serde_json::Value::Object(map) => {
            // MarkupContent / 旧式 MarkedString { language, value } 都用 value 字段。
            map.get("value")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string()
        }
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => arr
            .iter()
            .map(marked_to_text)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

fn severity_str(sev: u64) -> &'static str {
    match sev {
        1 => "error",
        2 => "warning",
        3 => "information",
        4 => "hint",
        _ => "unknown",
    }
}

/// 解析 publishDiagnostics 的 params（{ uri, diagnostics: [...] }）为结构化诊断列表。
fn parse_diagnostics(params: &serde_json::Value) -> Vec<LspDiagnostic> {
    let mut out = Vec::new();
    let Some(arr) = params.get("diagnostics").and_then(|d| d.as_array()) else {
        return out;
    };
    for d in arr {
        let start = d.get("range").and_then(|r| r.get("start"));
        let line = start
            .and_then(|s| s.get("line"))
            .and_then(|l| l.as_u64())
            .unwrap_or(0) as u32;
        let character = start
            .and_then(|s| s.get("character"))
            .and_then(|c| c.as_u64())
            .unwrap_or(0) as u32;
        let severity = d
            .get("severity")
            .and_then(|s| s.as_u64())
            .map(severity_str)
            .unwrap_or("unknown")
            .to_string();
        let message = d
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let source = d
            .get("source")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        out.push(LspDiagnostic {
            line,
            character,
            severity,
            message,
            source,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_escape_and_absolute() {
        assert!(validate_relative_path("../etc/passwd").is_err());
        assert!(validate_relative_path("/abs/x.rs").is_err());
        assert!(validate_relative_path("").is_err());
        assert!(validate_relative_path(".").is_err());
        assert!(validate_relative_path("src/main.rs").is_ok());
        assert_eq!(
            normalize_relative(&validate_relative_path("src/main.rs").unwrap()),
            "src/main.rs"
        );
    }

    #[test]
    fn severity_mapping() {
        assert_eq!(severity_str(1), "error");
        assert_eq!(severity_str(2), "warning");
        assert_eq!(severity_str(9), "unknown");
    }

    #[test]
    fn parse_single_location() {
        let ws = Path::new("/ws");
        let result = serde_json::json!({
            "uri": "file:///ws/src/lib.rs",
            "range": { "start": { "line": 10, "character": 4 }, "end": { "line": 10, "character": 9 } }
        });
        let locs = parse_locations(&result, ws);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].path, "src/lib.rs");
        assert_eq!(locs[0].line, 10);
        assert_eq!(locs[0].character, 4);
    }

    #[test]
    fn parse_location_link_array() {
        let ws = Path::new("/ws");
        let result = serde_json::json!([{
            "targetUri": "file:///ws/a/b.rs",
            "targetSelectionRange": { "start": { "line": 2, "character": 0 } }
        }]);
        let locs = parse_locations(&result, ws);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].path, "a/b.rs");
        assert_eq!(locs[0].line, 2);
    }

    #[test]
    fn parse_null_locations_is_empty() {
        assert!(parse_locations(&serde_json::Value::Null, Path::new("/ws")).is_empty());
    }

    #[test]
    fn parse_hover_markup_and_marked_array() {
        let markup = serde_json::json!({ "contents": { "kind": "markdown", "value": "fn foo() -> i32" } });
        assert_eq!(parse_hover(&markup), "fn foo() -> i32");
        let arr = serde_json::json!({ "contents": ["line1", { "value": "line2" }] });
        assert_eq!(parse_hover(&arr), "line1\n\nline2");
        assert_eq!(parse_hover(&serde_json::json!({})), "");
    }

    #[test]
    fn parse_diagnostics_structured() {
        let params = serde_json::json!({
            "uri": "file:///ws/src/main.rs",
            "diagnostics": [
                { "range": { "start": { "line": 3, "character": 5 } }, "severity": 1, "message": "mismatched types", "source": "rustc" },
                { "range": { "start": { "line": 8, "character": 0 } }, "severity": 2, "message": "unused import" }
            ]
        });
        let ds = parse_diagnostics(&params);
        assert_eq!(ds.len(), 2);
        assert_eq!(ds[0].severity, "error");
        assert_eq!(ds[0].line, 3);
        assert_eq!(ds[0].source, "rustc");
        assert_eq!(ds[1].severity, "warning");
        assert_eq!(ds[1].source, "");
    }

    #[test]
    fn uri_relative_handles_windows_drive() {
        let ws = Path::new("C:\\ws");
        // 同盘下应转相对（注意：strip_prefix 比对需路径分隔一致，这里仅验证不 panic 且产出合理）。
        let rel = uri_to_relative("file:///C:/ws/src/a.rs", ws);
        assert!(rel.ends_with("src/a.rs"), "got {rel}");
    }

    #[test]
    fn percent_decode_basic() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("C%3A/x"), "C:/x");
        assert_eq!(percent_decode("plain"), "plain");
    }
}
