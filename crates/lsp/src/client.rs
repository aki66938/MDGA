//! 最小但正确的 LSP 客户端：在工作区目录下拉起一个语言服务器子进程，
//! 走 stdio 上的 Content-Length 帧化 JSON-RPC：
//! initialize → initialized → textDocument/didOpen → 发一条请求 → shutdown/exit → kill 兜底。
//!
//! 设计取舍（v1）：每次工具调用拉起一个**一次性**服务器实例、做完即关。语言服务器（尤其
//! rust-analyzer）首次索引较慢，故超时给得较宽；整条操作有硬超时，超时/Drop 都强杀子进程，
//! 杜绝泄漏与挂死。安全：cwd=工作区、擦除子进程密钥环境变量、服务器程序硬编码（见 server.rs）。

use crate::framing::{encode_frame, read_frame};
use crate::server::ServerSpec;
use crate::{scrub_secret_env, LspError};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// 整条 LSP 操作的硬超时（含服务器启动与首次索引）。rust-analyzer 冷启动可能偏慢。
const OP_TIMEOUT: Duration = Duration::from_secs(45);
/// 单次「等待某个 id 的响应」轮询的额外耐心（在 OP_TIMEOUT 之内）。
const READ_TIMEOUT: Duration = Duration::from_secs(40);

/// 一个已连接的 LSP 会话；Drop 时强制杀掉子进程，保证不泄漏。
pub struct LspSession {
    child: Child,
    stdin: ChildStdin,
    /// 后台读线程：把每一帧响应通过 channel 送回主线程，避免 read 阻塞导致无法超时。
    rx: mpsc::Receiver<Result<serde_json::Value, LspError>>,
    reader_handle: Option<std::thread::JoinHandle<()>>,
    next_id: i64,
    root_uri: String,
    deadline: Instant,
}

impl Drop for LspSession {
    fn drop(&mut self) {
        // 无论如何都杀掉子进程（即使已 shutdown，也兜底避免僵尸/泄漏）。
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(h) = self.reader_handle.take() {
            let _ = h.join();
        }
    }
}

impl LspSession {
    /// 在 `workspace`（已规范化的绝对路径）下启动 `spec` 指定的服务器并完成 initialize 握手。
    pub fn start(spec: &ServerSpec, workspace: &Path) -> Result<Self, LspError> {
        let mut builder = Command::new(spec.command);
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
                "无法启动语言服务器 `{}`（请确认已安装且在 PATH 中）: {e}",
                spec.command
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
        };
        session.handshake(spec)?;
        Ok(session)
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
                    }
                }
                Ok(Err(e)) => return Err(e),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(LspError::Protocol(
                        "语言服务器在等待诊断时关闭了连接".to_string(),
                    ))
                }
            }
        }
        Ok(last.unwrap_or_else(|| serde_json::json!({ "uri": uri, "diagnostics": [] })))
    }

    /// 礼貌关闭：shutdown 请求 + exit 通知（best-effort，失败忽略——Drop 会强杀兜底）。
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
                Ok(Err(e)) => return Err(e),
                Err(mpsc::RecvTimeoutError::Timeout) => return Err(LspError::Timeout),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(LspError::Protocol(
                        "语言服务器在等待响应时关闭了连接".to_string(),
                    ))
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
