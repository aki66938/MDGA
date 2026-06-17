//! 最小但正确的 LSP 客户端：在工作区目录下拉起一个语言服务器子进程，
//! 走 stdio 上的 Content-Length 帧化 JSON-RPC：
//! initialize → initialized → textDocument/didOpen → 发请求 →（复用）→ shutdown/exit → kill 兜底。
//!
//! 设计（R1 池化）：会话本身是**长寿命**的——一次握手后可被进程级池子（见 `crate::pool`）
//! 复用于多次工具调用，省掉每次 ~20s 的冷启动索引。为此：
//!   - 每次工具操作前调用 [`LspSession::begin_op`] **重置硬超时**（deadline 是「单次操作」而非
//!     「会话终生」的预算）；
//!   - 文档用 [`LspSession::sync_document`] 同步：首次 `didOpen`、之后 `didChange`（递增 version），
//!     保证磁盘改动后结果正确（不复用陈旧快照）；
//!   - [`LspSession::is_alive`] 探活，发现子进程已退出/读线程断开即不再复用。
//! 整条操作仍有硬超时；超时 / Drop / 池子回收都强杀子进程，杜绝泄漏与挂死。
//! 安全：spawn 的是 PATH 解析出的**绝对路径**（见 `crate::which`，防 cwd 同名劫持）、
//! cwd=工作区、擦除子进程密钥环境变量、服务器程序硬编码（见 server.rs）。

use crate::framing::{encode_frame, read_frame};
use crate::server::ServerSpec;
use crate::{scrub_secret_env, which, LspError};
use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// 单次 LSP 操作的硬超时（含首次服务器索引）。rust-analyzer 冷启动可能偏慢。
/// 注意：这是**每次操作**的预算，由 [`LspSession::begin_op`] 在复用会话时重置。
const OP_TIMEOUT: Duration = Duration::from_secs(45);
/// 单次「等待某个 id 的响应」轮询的额外耐心（在 OP_TIMEOUT 之内）。
const READ_TIMEOUT: Duration = Duration::from_secs(40);

/// 一个已连接的 LSP 会话；Drop 时强制杀掉子进程，保证不泄漏。
///
/// 会话可被 `crate::pool` 跨多次工具调用复用：复用前 `begin_op` 重置 deadline，
/// 复用时 `sync_document` 决定 didOpen / didChange。
pub struct LspSession {
    child: Child,
    stdin: ChildStdin,
    /// 后台读线程：把每一帧响应通过 channel 送回主线程，避免 read 阻塞导致无法超时。
    rx: mpsc::Receiver<Result<serde_json::Value, LspError>>,
    reader_handle: Option<std::thread::JoinHandle<()>>,
    next_id: i64,
    root_uri: String,
    deadline: Instant,
    /// 已打开文档的 uri → 当前 LSP 文档 version（用于 didOpen vs didChange 判定）。
    open_docs: HashMap<String, i64>,
    /// 读线程是否报告过流结束/错误（一旦为真，会话不可再复用）。
    reader_failed: bool,
}

impl Drop for LspSession {
    fn drop(&mut self) {
        // 杀掉**整棵进程树**（不只是直接子进程）。
        //
        // 关键：部分服务器经 shim 间接启动——Windows 上 npm 装的 `*.cmd` 垫片会先起 `cmd.exe`，
        // 再由它 fork `node`。`Child::kill()` 只杀直接子进程（cmd.exe），**孤儿 node 仍持有 stdout
        // 管道写端**，于是后台读线程的阻塞 `read` 永远等不到 EOF，`join()` 便永久挂死。
        // 因此必须按 PID 递归杀整棵树，孤儿随之退出、管道关闭、读线程见 EOF 退出，join 才能返回。
        kill_process_tree(self.child.id());
        let _ = self.child.kill(); // 兜底直接子进程
        let _ = self.child.wait();
        if let Some(h) = self.reader_handle.take() {
            let _ = h.join();
        }
    }
}

/// 递归杀掉以 `pid` 为根的整棵进程树（含 shim 间接拉起的孙进程）。best-effort。
#[cfg(windows)]
fn kill_process_tree(pid: u32) {
    // taskkill /T 递归终止子孙，/F 强制。隐藏窗口、忽略输出与退出码（best-effort）。
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// 类 Unix：依赖 `Child::kill()` 杀直接子进程即可（语言服务器一般不经 shim 间接启动）。
#[cfg(not(windows))]
fn kill_process_tree(_pid: u32) {
    // no-op：非 Windows 下由调用方的 child.kill() 处理；如未来需杀进程组可在此扩展。
}

impl LspSession {
    /// 在 `workspace`（已规范化的绝对路径）下启动 `spec` 指定的服务器并完成 initialize 握手。
    ///
    /// abs-path 加固：先把 `spec.command`（编译期常量名）在 PATH 中解析为**绝对路径**再 spawn，
    /// 避免 OS 把工作区 cwd 也纳入查找而执行到同名的恶意可执行。解析不到 → `ServerUnavailable`。
    pub fn start(spec: &ServerSpec, workspace: &Path) -> Result<Self, LspError> {
        // 把程序名解析为 PATH 中的绝对路径（不含 cwd）。找不到即「未安装」。
        let exe = which::resolve_in_path(spec.command).ok_or_else(|| {
            LspError::ServerUnavailable(format!(
                "未找到语言服务器 `{}`（请确认已安装且在 PATH 中）",
                spec.command
            ))
        })?;

        let mut builder = Command::new(&exe);
        builder
            .args(spec.args)
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // 安全：从语言服务器子进程环境擦除密钥（API Key / token 等），与 run_command/git 一致。
        scrub_secret_env(&mut builder);

        let mut child = builder.spawn().map_err(|e| {
            LspError::ServerUnavailable(format!(
                "无法启动语言服务器 `{}`（{}）: {e}",
                spec.command,
                exe.display()
            ))
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| LspError::Protocol("无法获取服务器 stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LspError::Protocol("无法获取服务器 stdout".to_string()))?;

        let (tx, rx) = mpsc::channel();
        let reader_handle = spawn_reader(stdout, tx);

        let root_uri = path_to_file_uri(workspace);
        let mut session = LspSession {
            child,
            stdin,
            rx,
            reader_handle: Some(reader_handle),
            next_id: 1,
            root_uri,
            deadline: Instant::now() + OP_TIMEOUT,
            open_docs: HashMap::new(),
            reader_failed: false,
        };
        session.handshake(spec)?;
        Ok(session)
    }

    /// 开始一次新操作：重置硬超时预算。复用池中长寿命会话前必须调用，
    /// 否则会沿用上次操作早已过期的 deadline 而立刻超时。
    pub fn begin_op(&mut self) {
        self.deadline = Instant::now() + OP_TIMEOUT;
    }

    /// 探活：子进程是否仍在运行且读线程未报错。池子据此决定是否复用。
    pub fn is_alive(&mut self) -> bool {
        if self.reader_failed {
            return false;
        }
        // try_wait: Ok(Some(_)) 表示已退出；Ok(None) 表示仍在运行。
        !matches!(self.child.try_wait(), Ok(Some(_)) | Err(_))
    }

    /// 同步目标文档到服务器：首次见到该 uri 用 `didOpen`，之后用 `didChange`（递增 version）。
    ///
    /// 这是「磁盘改动后结果仍正确」的关键：复用会话时若文件已在外部被改写，必须把**最新**
    /// 全文喂给服务器（didChange 全量替换），否则会基于陈旧快照给出错误的 hover/definition。
    pub fn sync_document(
        &mut self,
        uri: &str,
        language_id: &str,
        text: &str,
    ) -> Result<(), LspError> {
        // 复用会话前先把闲置期间堆积的帧排空，并**回应**其中的 server→client 请求。
        // 关键正确性：若会话曾闲置在池中，服务器可能发来需回应的请求（如 workspace/configuration、
        // window/workDoneProgress/create）。这些请求无人应答会让服务器卡住其索引/响应管线，
        // 导致下一次复用时双方互等而**死锁**。这里一次性清账，避免复用后挂死。
        self.drain_pending();

        match self.open_docs.get(uri).copied() {
            None => {
                self.did_open(uri, language_id, text)?;
                self.open_docs.insert(uri.to_string(), 1);
            }
            Some(prev) => {
                let version = prev + 1;
                self.did_change(uri, version, text)?;
                self.open_docs.insert(uri.to_string(), version);
            }
        }
        Ok(())
    }

    /// 非阻塞排空读通道里已堆积的帧：丢弃通知，但对 server→client 请求回空响应（避免对端阻塞）。
    /// 仅处理「当下已就绪」的帧（`try_recv`），不等待；通道断开则标记会话失败。
    fn drain_pending(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(Ok(msg)) => self.reply_if_server_request(&msg),
                Ok(Err(_)) => {
                    self.reader_failed = true;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.reader_failed = true;
                    break;
                }
            }
        }
    }

    /// 若帧是 server→client 请求（同时含 method 与 id），回一个空响应；否则忽略。
    fn reply_if_server_request(&mut self, msg: &serde_json::Value) {
        if msg.get("method").is_some() {
            if let Some(req_id) = msg.get("id") {
                let reply = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": null
                });
                let _ = self.send(&reply);
            }
        }
    }

    /// didChange 通知：以全量内容替换文档（contentChanges 仅含 `{ text }`，无 range）。
    fn did_change(&mut self, uri: &str, version: i64, text: &str) -> Result<(), LspError> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [ { "text": text } ]
            }
        });
        self.send(&msg)
    }

    /// initialize → 等响应 → initialized 通知。
    fn handshake(&mut self, _spec: &ServerSpec) -> Result<(), LspError> {
        let root_uri = self.root_uri.clone();
        let id = self.alloc_id();
        let init = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "clientInfo": { "name": "mdga-lsp", "version": "0.1.0" },
                "rootUri": root_uri,
                "workspaceFolders": [ { "uri": root_uri, "name": "workspace" } ],
                "capabilities": {
                    "textDocument": {
                        "hover": { "contentFormat": ["plaintext", "markdown"] },
                        "definition": { "linkSupport": false },
                        "references": {},
                        "publishDiagnostics": { "relatedInformation": false },
                        "synchronization": { "didSave": false, "willSave": false }
                    },
                    "workspace": { "workspaceFolders": true }
                }
            }
        });
        self.send(&init)?;
        let _ = self.await_response(id)?;

        let initialized = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        });
        self.send(&initialized)?;
        Ok(())
    }

    /// 打开目标文档（didOpen 通知），把文件全文喂给服务器。
    pub fn did_open(&mut self, uri: &str, language_id: &str, text: &str) -> Result<(), LspError> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": text
                }
            }
        });
        self.send(&msg)
    }

    /// 发一条带 id 的请求并等其响应的 `result`（错误时返回 Protocol 错误）。
    pub fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, LspError> {
        let id = self.alloc_id();
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        self.send(&msg)?;
        self.await_response(id)
    }

    /// 发请求并在「服务器仍在索引、暂时给空结果」时有限重试，直到拿到非空结果或截止。
    ///
    /// 语言服务器（尤其 rust-analyzer）在 didOpen 后需要时间建立索引；过早请求会得到
    /// `null`/空数组。这里以小步退避重试（受整体 deadline 约束），让结果稳定而不挂死：
    /// 仍为空也照常返回（视为「确无结果」），绝不无限等待。`is_empty` 由调用方判定空。
    pub fn request_until_ready(
        &mut self,
        method: &str,
        params: serde_json::Value,
        is_empty: impl Fn(&serde_json::Value) -> bool,
    ) -> Result<serde_json::Value, LspError> {
        // 在整体 deadline 内重试；每次失败后退避，给索引留时间。
        let retry_deadline = (Instant::now() + Duration::from_secs(30)).min(self.deadline);
        let mut last = self.request(method, params.clone())?;
        while is_empty(&last) && Instant::now() < retry_deadline {
            std::thread::sleep(Duration::from_millis(400));
            last = self.request(method, params.clone())?;
        }
        Ok(last)
    }

    /// 收集 publishDiagnostics 通知直到指定 uri 的诊断到达或短暂静默期满。
    ///
    /// 诊断是服务器**主动推送**的通知（无请求 id），故单独处理：在剩余时间内读帧，
    /// 命中目标 uri 即返回；其它通知/响应丢弃。一段时间收不到则返回空（视为「无诊断」）。
    pub fn collect_diagnostics(&mut self, uri: &str) -> Result<serde_json::Value, LspError> {
        // 诊断可能多次发布（逐步细化）；取最后一次命中目标 uri 的。给一个相对宽松的静默窗口。
        let diag_deadline = (Instant::now() + Duration::from_secs(20)).min(self.deadline);
        let mut last: Option<serde_json::Value> = None;
        loop {
            let now = Instant::now();
            if now >= diag_deadline {
                break;
            }
            match self.rx.recv_timeout(diag_deadline - now) {
                Ok(Ok(msg)) => {
                    if msg.get("method").and_then(|m| m.as_str())
                        == Some("textDocument/publishDiagnostics")
                    {
                        if let Some(params) = msg.get("params") {
                            if params.get("uri").and_then(|u| u.as_str()) == Some(uri) {
                                last = Some(params.clone());
                                // rust-analyzer 常先发空诊断再发实际诊断；若已拿到非空，多等一拍即可收口。
                                if !params
                                    .get("diagnostics")
                                    .and_then(|d| d.as_array())
                                    .map(|a| a.is_empty())
                                    .unwrap_or(true)
                                {
                                    break;
                                }
                            }
                        }
                    } else {
                        // 其它通知忽略；但 server→client 请求必须回应，否则对端可能卡住后续推送。
                        self.reply_if_server_request(&msg);
                    }
                }
                Ok(Err(e)) => {
                    self.reader_failed = true;
                    return Err(e);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.reader_failed = true;
                    return Err(LspError::Protocol(
                        "语言服务器在等待诊断时关闭了连接".to_string(),
                    ));
                }
            }
        }
        Ok(last.unwrap_or_else(|| serde_json::json!({ "uri": uri, "diagnostics": [] })))
    }

    /// 礼貌关闭：shutdown 请求 + exit 通知（best-effort，失败忽略——Drop 会强杀兜底）。
    ///
    /// 池化后常规路径不再主动调用它（会话由池子复用、由 Drop/reaper 强杀回收，无泄漏）；
    /// 保留作为「需要优雅下线某会话」的显式入口与文档化能力。
    #[allow(dead_code)]
    pub fn shutdown(&mut self) {
        let id = self.alloc_id();
        let shutdown = serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "shutdown", "params": null
        });
        if self.send(&shutdown).is_ok() {
            let _ = self.await_response(id);
        }
        let exit = serde_json::json!({ "jsonrpc": "2.0", "method": "exit" });
        let _ = self.send(&exit);
    }

    fn alloc_id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn send(&mut self, value: &serde_json::Value) -> Result<(), LspError> {
        let frame = encode_frame(value);
        self.stdin
            .write_all(&frame)
            .and_then(|_| self.stdin.flush())
            .map_err(|e| LspError::Protocol(format!("向语言服务器写入失败: {e}")))
    }

    /// 等待指定 id 的响应帧；中途的通知（含 server→client 请求）按需回应/丢弃。
    /// 受整体 deadline 与 READ_TIMEOUT 双重约束，超时即报错（Drop 强杀子进程）。
    fn await_response(&mut self, id: i64) -> Result<serde_json::Value, LspError> {
        let read_deadline = (Instant::now() + READ_TIMEOUT).min(self.deadline);
        loop {
            let now = Instant::now();
            if now >= read_deadline {
                return Err(LspError::Timeout);
            }
            let msg = match self.rx.recv_timeout(read_deadline - now) {
                Ok(Ok(msg)) => msg,
                Ok(Err(e)) => {
                    self.reader_failed = true;
                    return Err(e);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => return Err(LspError::Timeout),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.reader_failed = true;
                    return Err(LspError::Protocol(
                        "语言服务器在等待响应时关闭了连接".to_string(),
                    ));
                }
            };

            // server → client 请求（带 id 且带 method）：用空响应礼貌应答，避免对端阻塞。
            if msg.get("method").is_some() {
                if let Some(req_id) = msg.get("id") {
                    let reply = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": null
                    });
                    let _ = self.send(&reply);
                }
                continue; // 通知或 server 请求，非我们等的响应
            }

            // 响应：匹配 id。
            if msg.get("id").and_then(|v| v.as_i64()) == Some(id) {
                if let Some(err) = msg.get("error") {
                    let m = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("未知 LSP 错误");
                    return Err(LspError::Protocol(format!("语言服务器返回错误: {m}")));
                }
                return Ok(msg.get("result").cloned().unwrap_or(serde_json::Value::Null));
            }
            // 其它 id 的响应（理论上不该有，单飞请求）：忽略继续等。
        }
    }
}

/// 后台线程：循环从服务器 stdout 读帧，把每帧（或错误）送回 channel。EOF/错误后退出。
fn spawn_reader(
    stdout: ChildStdout,
    tx: mpsc::Sender<Result<serde_json::Value, LspError>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_frame(&mut reader) {
                Ok(msg) => {
                    if tx.send(Ok(msg)).is_err() {
                        break; // 接收端已 drop
                    }
                }
                Err(LspError::Protocol(_)) => {
                    // EOF 或解析失败：把错误送回一次后退出（接收端据此报错）。
                    let _ = tx.send(Err(LspError::Protocol(
                        "语言服务器输出流结束".to_string(),
                    )));
                    break;
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    break;
                }
            }
        }
    })
}

/// 把绝对路径转成 `file://` URI（跨平台；Windows 盘符前补 `/`，反斜杠转正斜杠）。
///
/// 注意：Windows 上 `canonicalize()` 会返回带 `\\?\` 的扩展长度路径（verbatim），
/// 语言服务器不接受这种形式（rust-analyzer 会回 "url is not a file"），必须先剥掉前缀。
pub fn path_to_file_uri(path: &Path) -> String {
    let mut s = path.to_string_lossy().replace('\\', "/");
    // 剥离 Windows verbatim 前缀：//?/C:/... → C:/...；//?/UNC/server/share → //server/share。
    if let Some(rest) = s.strip_prefix("//?/UNC/") {
        s = format!("//{rest}");
    } else if let Some(rest) = s.strip_prefix("//?/") {
        s = rest.to_string();
    }
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        // Windows: C:/foo → file:///C:/foo
        format!("file:///{s}")
    }
}

/// 把工作区 + 相对路径拼成目标文件的 file:// URI（不做存在性检查，调用方先校验）。
pub fn file_uri_for(workspace: &Path, relative: &Path) -> String {
    let full: PathBuf = workspace.join(relative);
    path_to_file_uri(&full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uri_has_scheme() {
        let uri = path_to_file_uri(Path::new("/home/u/p.rs"));
        assert!(uri.starts_with("file://"), "got {uri}");
        assert!(uri.contains("/home/u/p.rs"));
    }

    #[test]
    fn windows_path_gets_triple_slash() {
        let uri = path_to_file_uri(Path::new("C:\\Users\\a\\b.rs"));
        assert_eq!(uri, "file:///C:/Users/a/b.rs");
    }
}
