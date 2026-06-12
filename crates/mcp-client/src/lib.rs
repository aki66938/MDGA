//! 最小 MCP（Model Context Protocol）stdio 客户端。
//!
//! 职责：spawn MCP server 子进程，走换行分隔的 JSON-RPC 2.0 完成 initialize 握手、
//! tools/list 发现与 tools/call 调用。MDGA 中 MCP 是生态接入层：所有 MCP 工具调用
//! 仍必须经过桌面端的 Permission Manager 与 Activity Event 审计，本 crate 不做权限判断。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Error)]
pub enum McpError {
    #[error("无法启动 MCP server: {0}")]
    SpawnFailed(String),
    #[error("MCP 通信失败: {0}")]
    Io(String),
    #[error("MCP 请求超时")]
    Timeout,
    #[error("MCP server 返回错误: {0}")]
    ServerError(String),
}

/// MCP server 暴露的工具定义。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct McpToolDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// JSON Schema，直接转发给模型作为函数参数定义。
    #[serde(rename = "inputSchema", default)]
    pub input_schema: serde_json::Value,
}

/// 一个已连接的 MCP server 客户端。
///
/// 内部用读取线程把子进程 stdout 的 JSON-RPC 响应按 id 路由到等待中的请求；
/// 请求方通过 mpsc channel 带超时阻塞等待。Drop 时杀掉子进程。
pub struct McpClient {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    pending: Arc<Mutex<HashMap<u64, Sender<serde_json::Value>>>>,
    next_id: AtomicU64,
    pub server_name: String,
    pub tools: Vec<McpToolDef>,
}

impl McpClient {
    /// 启动并握手一个 MCP server。
    ///
    /// 输入展示名与完整命令行（经 Windows `cmd /C` 执行，兼容 npx/uvx 等 shim）；
    /// 成功后返回已完成 initialize + tools/list 的客户端。
    pub fn connect(server_name: &str, command_line: &str) -> Result<McpClient, McpError> {
        let mut child = Command::new("cmd")
            .args(["/C", command_line])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| McpError::SpawnFailed(e.to_string()))?;

        let stdin = child.stdin.take().ok_or_else(|| McpError::Io("无 stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| McpError::Io("无 stdout".into()))?;

        let pending: Arc<Mutex<HashMap<u64, Sender<serde_json::Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending_reader = pending.clone();

        // 读取线程：逐行解析 JSON-RPC，把带 id 的响应路由给等待者；通知消息忽略。
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                    continue;
                };
                if let Some(id) = value.get("id").and_then(|v| v.as_u64()) {
                    let sender = pending_reader.lock().ok().and_then(|mut map| map.remove(&id));
                    if let Some(sender) = sender {
                        let _ = sender.send(value);
                    }
                }
            }
        });

        let mut client = McpClient {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            server_name: server_name.to_string(),
            tools: Vec::new(),
        };

        // initialize 握手
        client.request(
            "initialize",
            serde_json::json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "MDGA", "version": env!("CARGO_PKG_VERSION") }
            }),
            HANDSHAKE_TIMEOUT,
        )?;
        client.notify("notifications/initialized", serde_json::json!({}))?;

        // 工具发现
        let tools_result = client.request("tools/list", serde_json::json!({}), HANDSHAKE_TIMEOUT)?;
        client.tools = tools_result
            .get("tools")
            .cloned()
            .map(serde_json::from_value::<Vec<McpToolDef>>)
            .transpose()
            .map_err(|e| McpError::Io(e.to_string()))?
            .unwrap_or_default();

        Ok(client)
    }

    /// 调用一个 MCP 工具，返回文本化的结果内容。
    pub fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<String, McpError> {
        let result = self.request(
            "tools/call",
            serde_json::json!({ "name": tool_name, "arguments": arguments }),
            DEFAULT_CALL_TIMEOUT,
        )?;
        let is_error = result.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);
        let text = result
            .get("content")
            .and_then(|c| c.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if is_error {
            return Err(McpError::ServerError(text));
        }
        Ok(text)
    }

    /// 发送 JSON-RPC 请求并阻塞等待响应（带超时）。
    fn request(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = channel::<serde_json::Value>();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(id, sender);
        }

        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        self.write_line(&message)?;

        let response = receiver.recv_timeout(timeout).map_err(|_| {
            if let Ok(mut pending) = self.pending.lock() {
                pending.remove(&id);
            }
            McpError::Timeout
        })?;

        if let Some(error) = response.get("error") {
            return Err(McpError::ServerError(error.to_string()));
        }
        Ok(response.get("result").cloned().unwrap_or(serde_json::Value::Null))
    }

    /// 发送 JSON-RPC 通知（无响应）。
    fn notify(&self, method: &str, params: serde_json::Value) -> Result<(), McpError> {
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        self.write_line(&message)
    }

    fn write_line(&self, message: &serde_json::Value) -> Result<(), McpError> {
        let mut stdin = self.stdin.lock().map_err(|e| McpError::Io(e.to_string()))?;
        let line = serde_json::to_string(message).map_err(|e| McpError::Io(e.to_string()))?;
        stdin
            .write_all(format!("{line}\n").as_bytes())
            .and_then(|_| stdin.flush())
            .map_err(|e| McpError::Io(e.to_string()))
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}

/// 把 MCP 工具名转为模型可用的函数名：`mcp_<server>_<tool>`，只保留合法字符并截断到 64。
pub fn function_name_for(server_name: &str, tool_name: &str) -> String {
    let sanitize = |s: &str| -> String {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
            .collect()
    };
    let mut name = format!("mcp_{}_{}", sanitize(server_name), sanitize(tool_name));
    name.truncate(64);
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_safe_function_names() {
        assert_eq!(function_name_for("github", "create_issue"), "mcp_github_create_issue");
        assert_eq!(function_name_for("my server!", "do/it"), "mcp_my_server__do_it");
        assert!(function_name_for(&"x".repeat(80), "tool").len() <= 64);
    }

    #[test]
    fn parses_tool_definitions() {
        let raw = serde_json::json!({
            "name": "read_issue",
            "description": "Read a GitHub issue",
            "inputSchema": { "type": "object", "properties": {} }
        });
        let def: McpToolDef = serde_json::from_value(raw).expect("should parse");
        assert_eq!(def.name, "read_issue");
        assert!(def.input_schema.is_object());
    }
}
