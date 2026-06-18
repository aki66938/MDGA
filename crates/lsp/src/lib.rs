//! mdga-lsp（R1）：用 Language Server Protocol 给 Agent 编译器级代码智能。
//!
//! 提供 4 个**只读**工具：
//! - `lsp_definition`：某位置符号的定义跳转
//! - `lsp_references`：某位置符号的全部引用
//! - `lsp_hover`：某位置的类型/签名/文档
//! - `lsp_diagnostics`：某文件的错误/警告
//!
//! 实现：按文件扩展名从**硬编码精选注册表**解析语言服务器（见 `server.rs`，含 Rust/TS-JS/
//! Python/Go/C-C++/Ruby/PHP/Lua），把服务器**程序名**经 `which` 解析为 PATH 中的**绝对路径**
//! 再在工作区目录下拉起子进程，走 stdio 上 Content-Length 帧化的 JSON-RPC，做
//! initialize→initialized→didOpen→请求。会话由**进程级池子**（`pool.rs`）跨多次调用复用，
//! 省掉冷启动索引；空闲超时被 reaper 回收，Drop / 超时 / 回收都强杀子进程。
//!
//! 安全（强约束，对齐 tool-runtime 的 git/run_command）：
//! - 路径用 `validate_relative_path`/`canonical_workspace` 同款逻辑做工作区内校验（拒绝 `..` 与绝对路径）；
//! - 服务器子进程 cwd=工作区、擦除密钥环境变量（API Key 等）；
//! - 服务器程序与参数**全部硬编码**（精选注册表），绝不接受 config/模型/工作区输入的任意命令；
//! - 服务器二进制只在 **PATH 目录**里 which 式解析为绝对路径（不含 cwd），防工作区同名劫持；
//! - 每次操作有硬超时，超时/Drop/池回收都强杀子进程（无泄漏、无挂死）；
//! - 缺少服务器二进制 → 清晰的 `ServerUnavailable` 错误，绝不挂起。

mod client;
mod framing;
mod pool;
mod server;
mod which;

use client::{file_uri_for, LspSession};
use serde::{Deserialize, Serialize};
use server::resolve_server;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use thiserror::Error;

pub use server::{known_servers, is_known_kind, KnownServer};

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
    #[error("语言服务器 `{0}` 已在设置中被禁用")]
    ServerDisabled(String),
    #[error("LSP 协议错误: {0}")]
    Protocol(String),
    #[error("LSP 操作超时")]
    Timeout,
    #[error("文件系统错误: {0}")]
    Io(#[from] std::io::Error),
}

// ── 用户配置（启用开关 + 可选路径覆盖）────────────────────────────────────────
//
// 安全边界（强约束）：配置只能调节**已知**服务器的两件事——是否启用、二进制在哪。它**不能**新增
// 一条服务器命令：命令身份（command/args/扩展名）恒由 `server::REGISTRY` 编译期常量决定。`path_override`
// 是人类用户在设置里显式录入的本地路径（绝非模型/工作区派生），解析时仅作为「已知二进制在哪」的提示，
// 并在使用前校验其为一个**已存在的文件**；为空则回退到默认的 PATH 解析行为（与从前完全一致）。

/// 单个已知服务器的用户设置。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LspServerSetting {
    /// 是否启用该已知服务器（false=禁用，相关 lsp_* 工具对其语言报「已禁用」错误而非挂死）。
    pub enabled: bool,
    /// 可选的二进制**绝对路径**覆盖（人类用户显式录入）。`Some` 且指向已存在文件时直接用它启动，
    /// 跳过 PATH 解析；`None`/空 时回退默认 PATH 解析。绝不接受相对路径或不存在的路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_override: Option<String>,
}

/// 全部已知服务器的用户配置：键为服务器 `kind`（见 `server::known_servers`）。
///
/// 缺省语义（向后兼容）：某 kind 在表中**缺席**＝启用且无路径覆盖（即与从前的纯 PATH 解析一致）。
/// 因此空配置 = 全部启用、全走 PATH，行为不变。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct LspServerConfig {
    pub servers: HashMap<String, LspServerSetting>,
}

impl LspServerConfig {
    /// 该 kind 是否启用（缺省＝启用）。
    fn is_enabled(&self, kind: &str) -> bool {
        self.servers.get(kind).map(|s| s.enabled).unwrap_or(true)
    }

    /// 该 kind 的路径覆盖（trim 后非空才视为有效；缺省＝无覆盖）。
    fn path_override(&self, kind: &str) -> Option<String> {
        self.servers
            .get(kind)
            .and_then(|s| s.path_override.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }
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

/// 校验用户录入的二进制路径覆盖：必须是一个**已存在的文件**（绝对/相对皆可，由用户负责）。
///
/// 安全说明：该路径是人类用户在应用设置里**显式录入**的本地二进制位置（绝非模型/工作区派生），
/// 它只回答「这个**已知**服务器的二进制在哪」。我们在使用前确认它确为现存文件，避免把一个不存在
/// 或目录路径喂给 spawn。它**不**改变要启动的命令身份——命令身份恒由注册表常量决定。
fn validate_override_path(path: &str) -> Result<PathBuf, LspError> {
    let p = Path::new(path.trim());
    if !p.exists() {
        return Err(LspError::ServerUnavailable(format!(
            "设置中为该语言服务器指定的路径不存在: {path}"
        )));
    }
    if !p.is_file() {
        return Err(LspError::ServerUnavailable(format!(
            "设置中为该语言服务器指定的路径不是文件: {path}"
        )));
    }
    Ok(p.to_path_buf())
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

/// 一次工具调用的「已就绪上下文」：会话（来自池复用或新建）+ 归还所需的 key + 目标文件信息。
struct Prepared {
    session: LspSession,
    /// 用于用完归还到池子的键（规范化工作区 + 命令 + 参数）。
    key: pool::PoolKey,
    /// 目标文件 file:// URI。
    uri: String,
    /// 规范化工作区根（绝对路径）。
    workspace: PathBuf,
    /// 安全的工作区相对路径。
    relative: PathBuf,
}

/// 共用准备：校验路径 → 解析服务器 → **从池借出或新建**会话 → 重置超时 → 同步目标文件全文。
///
/// 池化要点：同 (工作区, 服务器命令+参数) 的会话被复用，省掉冷启动索引。复用时通过
/// `sync_document`（首次 didOpen、之后 didChange 全量替换）把**磁盘最新内容**喂给服务器，
/// 保证文件在外部被改写后结果依然正确（不基于陈旧快照）。
fn open_session(
    workspace_root: impl AsRef<Path>,
    path: &str,
    config: &LspServerConfig,
) -> Result<Prepared, LspError> {
    let (workspace, relative, target) = resolve_existing_file(workspace_root, path)?;
    let spec = resolve_server(&normalize_relative(&relative))?;

    // 用户设置门禁：被显式禁用的已知服务器直接报错（不挂死）。缺省＝启用。
    if !config.is_enabled(spec.kind) {
        return Err(LspError::ServerDisabled(spec.kind.to_string()));
    }
    // 路径覆盖（人类显式录入）：非空则校验为已存在文件，作为该已知二进制的绝对路径直接启动。
    // 为空回退默认 PATH 解析。注意：覆盖只决定「在哪」，命令身份仍是注册表常量。
    let exe_override = match config.path_override(spec.kind) {
        Some(p) => Some(validate_override_path(&p)?),
        None => None,
    };
    let text = read_doc_text(&target)?;
    let uri = file_uri_for(&workspace, &relative);

    // 池键并入路径覆盖指纹：覆盖路径不同应视作不同会话，避免复用到指向另一个二进制的旧会话。
    let key = pool::PoolKey::new(
        &workspace.to_string_lossy(),
        &format!(
            "{}\u{0}{}",
            spec.command,
            exe_override.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
        ),
        spec.args,
    );

    // 先尝试从池借出长寿命会话；借不到（未命中/已死）再新建。
    let mut session = match pool::checkout(&key) {
        Some(s) => s,
        None => match &exe_override {
            Some(exe) => LspSession::start_with_exe(&spec, exe, &workspace)?,
            None => LspSession::start(&spec, &workspace)?,
        },
    };
    // 复用会话的 deadline 早已过期，必须重置；新建会话重置也无害。
    session.begin_op();
    // 首次 didOpen / 复用则 didChange（喂磁盘最新全文）。
    session.sync_document(&uri, spec.language_id, &text)?;

    Ok(Prepared {
        session,
        key,
        uri,
        workspace,
        relative,
    })
}

/// 一次操作成功收尾：把会话归还到池子以便后续复用（取代旧的 `shutdown()`）。
///
/// 仅在操作**成功**路径调用；出错时直接 Drop 会话（强杀子进程），不归还可能已损坏的会话。
fn finish(session: LspSession, key: pool::PoolKey) {
    pool::checkin(key, session);
}

/// lsp_definition：某位置符号的定义跳转。沿用默认配置（全部启用、走 PATH）。
pub fn lsp_definition(
    workspace_root: impl AsRef<Path>,
    request: LspPositionRequest,
) -> Result<LspLocationsResult, LspError> {
    lsp_definition_with_config(workspace_root, request, &LspServerConfig::default())
}

/// lsp_definition（配置感知版）：按用户设置门禁/路径覆盖解析服务器。
pub fn lsp_definition_with_config(
    workspace_root: impl AsRef<Path>,
    request: LspPositionRequest,
    config: &LspServerConfig,
) -> Result<LspLocationsResult, LspError> {
    let Prepared {
        mut session,
        key,
        uri,
        workspace,
        ..
    } = open_session(&workspace_root, &request.path, config)?;
    let result = session.request_until_ready(
        "textDocument/definition",
        position_params(&uri, request.line, request.character),
        is_empty_locations,
    )?;
    finish(session, key);
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

/// lsp_references：某位置符号的全部引用（含声明）。沿用默认配置。
pub fn lsp_references(
    workspace_root: impl AsRef<Path>,
    request: LspPositionRequest,
) -> Result<LspLocationsResult, LspError> {
    lsp_references_with_config(workspace_root, request, &LspServerConfig::default())
}

/// lsp_references（配置感知版）。
pub fn lsp_references_with_config(
    workspace_root: impl AsRef<Path>,
    request: LspPositionRequest,
    config: &LspServerConfig,
) -> Result<LspLocationsResult, LspError> {
    let Prepared {
        mut session,
        key,
        uri,
        workspace,
        ..
    } = open_session(&workspace_root, &request.path, config)?;
    let mut params = position_params(&uri, request.line, request.character);
    params["context"] = serde_json::json!({ "includeDeclaration": true });
    let result =
        session.request_until_ready("textDocument/references", params, is_empty_locations)?;
    finish(session, key);
    let locations = parse_locations(&result, &workspace);
    Ok(LspLocationsResult {
        count: locations.len(),
        locations,
    })
}

/// lsp_hover：某位置的类型/签名/文档。沿用默认配置。
pub fn lsp_hover(
    workspace_root: impl AsRef<Path>,
    request: LspPositionRequest,
) -> Result<LspHoverResult, LspError> {
    lsp_hover_with_config(workspace_root, request, &LspServerConfig::default())
}

/// lsp_hover（配置感知版）。
pub fn lsp_hover_with_config(
    workspace_root: impl AsRef<Path>,
    request: LspPositionRequest,
    config: &LspServerConfig,
) -> Result<LspHoverResult, LspError> {
    let Prepared {
        mut session,
        key,
        uri,
        ..
    } = open_session(&workspace_root, &request.path, config)?;
    let result = session.request_until_ready(
        "textDocument/hover",
        position_params(&uri, request.line, request.character),
        |v| v.is_null() || parse_hover(v).is_empty(),
    )?;
    finish(session, key);
    let contents = parse_hover(&result);
    Ok(LspHoverResult {
        found: !contents.is_empty(),
        contents,
    })
}

/// lsp_diagnostics：某文件的错误/警告（收集 publishDiagnostics 推送）。沿用默认配置。
pub fn lsp_diagnostics(
    workspace_root: impl AsRef<Path>,
    request: LspDiagnosticsRequest,
) -> Result<LspDiagnosticsResult, LspError> {
    lsp_diagnostics_with_config(workspace_root, request, &LspServerConfig::default())
}

/// lsp_diagnostics（配置感知版）。
pub fn lsp_diagnostics_with_config(
    workspace_root: impl AsRef<Path>,
    request: LspDiagnosticsRequest,
    config: &LspServerConfig,
) -> Result<LspDiagnosticsResult, LspError> {
    let Prepared {
        mut session,
        key,
        uri,
        relative,
        ..
    } = open_session(&workspace_root, &request.path, config)?;
    let raw = session.collect_diagnostics(&uri)?;
    finish(session, key);
    let diagnostics = parse_diagnostics(&raw);
    Ok(LspDiagnosticsResult {
        path: normalize_relative(&relative),
        count: diagnostics.len(),
        diagnostics,
    })
}

// ── 池子诊断/可观测入口（供 e2e 测试验证复用，与运行时无副作用） ───────────────

/// 当前进程级 LSP 会话池中常驻（空闲）会话数。测试/诊断用。
pub fn pool_pooled_count() -> usize {
    pool::pooled_count()
}

/// 立即回收所有空闲超过给定秒数的会话，返回被回收数。`0` 表示回收全部空闲会话。
/// 暴露给测试以验证空闲会话**可被回收**（不必真等 5 分钟 TTL）；reaper 平时按 5min 自动跑。
pub fn pool_reap_idle_secs(secs: u64) -> usize {
    pool::reap_idle_with_ttl(std::time::Duration::from_secs(secs))
}

// ── 响应解析（把裸 LSP JSON 转成结构化结果） ─────────────────────────────────

fn position_params(uri: &str, line: u32, character: u32) -> serde_json::Value {
    serde_json::json!({
        "textDocument": { "uri": uri },
        "position": { "line": line, "character": character }
    })
}

/// 把 file:// URI 转回工作区相对路径；落在工作区外则回退为去掉 scheme 的路径。
///
/// 注意：不同服务器对 Windows 盘符的编码不一：有的发 `file:///C:/x`（裸冒号、大写盘符），
/// 有的发 `file:///c%3A/x`（**百分号编码的冒号** + 小写盘符，pyright 即如此）。因此必须
/// **先 percent-decode 再做盘符前导斜杠处理**，否则 `%3A` 不等于 `:`，会漏掉去前导斜杠那步，
/// 导致相对化失败（path 残留 `/c:/...`）。
fn uri_to_relative(uri: &str, workspace: &Path) -> String {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    // 先解码（还原 %3A→: / %20→空格 等）并正斜杠化；盘符判定要在解码后做。
    let decoded = percent_decode(raw).replace('\\', "/");
    // Windows: /C:/x → C:/x，去掉盘符前的前导斜杠（兼容大小写盘符与已解码的冒号）。
    let decoded = if decoded.starts_with('/')
        && decoded.len() > 2
        && decoded.as_bytes()[2] == b':'
        && decoded[1..2].chars().all(|c| c.is_ascii_alphabetic())
    {
        decoded[1..].to_string()
    } else {
        decoded
    };
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
    fn config_defaults_enable_all_and_no_override() {
        // 空配置：任何 kind 都启用、无路径覆盖（与从前 PATH 解析行为一致）。
        let cfg = LspServerConfig::default();
        assert!(cfg.is_enabled("rust-analyzer"));
        assert!(cfg.is_enabled("anything-missing"));
        assert!(cfg.path_override("rust-analyzer").is_none());
    }

    #[test]
    fn config_disable_and_override_honored() {
        let mut cfg = LspServerConfig::default();
        cfg.servers.insert(
            "gopls".to_string(),
            LspServerSetting { enabled: false, path_override: None },
        );
        cfg.servers.insert(
            "rust-analyzer".to_string(),
            LspServerSetting {
                enabled: true,
                path_override: Some("  /opt/ra/rust-analyzer  ".to_string()),
            },
        );
        // 显式禁用生效。
        assert!(!cfg.is_enabled("gopls"));
        // 路径覆盖 trim 后取用。
        assert_eq!(
            cfg.path_override("rust-analyzer").as_deref(),
            Some("/opt/ra/rust-analyzer")
        );
        // 空白路径视为无覆盖。
        cfg.servers.insert(
            "clangd".to_string(),
            LspServerSetting { enabled: true, path_override: Some("   ".to_string()) },
        );
        assert!(cfg.path_override("clangd").is_none());
    }

    #[test]
    fn config_roundtrips_as_transparent_map() {
        // 透明序列化：直接是 { kind: {enabled, pathOverride} } 形状，便于前端/存储交换。
        let mut cfg = LspServerConfig::default();
        cfg.servers.insert(
            "pyright".to_string(),
            LspServerSetting { enabled: false, path_override: Some("/x/py".to_string()) },
        );
        let json = serde_json::to_string(&cfg).unwrap();
        let back: LspServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
        assert!(json.contains("pyright"));
        assert!(json.contains("pathOverride"));
    }

    #[test]
    fn validate_override_rejects_missing_file() {
        assert!(validate_override_path("/definitely/not/here/ra").is_err());
    }

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
    fn uri_relative_handles_percent_encoded_lowercase_drive() {
        // pyright 风格：百分号编码冒号 + 小写盘符。必须先解码再去盘符前导斜杠，才能相对化。
        let ws = Path::new("C:\\ws\\proj");
        let rel = uri_to_relative("file:///c%3A/ws/proj/app.py", ws);
        assert_eq!(rel, "app.py", "应相对化（解码 %3A 后去前导斜杠），实际 {rel}");
        // 大小写盘符混用也应相对化。
        let rel2 = uri_to_relative("file:///C%3A/ws/proj/sub/mod.py", ws);
        assert_eq!(rel2, "sub/mod.py", "got {rel2}");
    }

    #[test]
    fn percent_decode_basic() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("C%3A/x"), "C:/x");
        assert_eq!(percent_decode("plain"), "plain");
    }
}
