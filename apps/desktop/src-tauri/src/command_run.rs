//! 命令执行工具：run_command（前台流式 / 后台 shell）与后台 shell 控制工具
//! （list_shells / get_shell_output / kill_shell）。
//!
//! 从 main.rs 抽出（Plan16 阶段2）：纯函数搬移，无行为变更。

use crate::state::{AppState, BgShell, BG_SHELL_SEQ};
use mdga_sandbox_runtime::{NetworkMode, SessionSecurityContext};
use mdga_tool_runtime::RunCommandRequest;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

/// 构造把命令输出逐行推送到前端的回调（"command-output" 事件）。
pub(crate) fn command_line_callback(app: &AppHandle) -> mdga_tool_runtime::CommandLineCallback {
    let app = app.clone();
    std::sync::Arc::new(move |line: String| {
        let _ = app.emit("command-output", line);
    })
}

/// run_command 工具的桌面端执行：前台流式输出；background=true 时立即返回、
/// 后台线程跑完后通过 "background-command-done" 事件通知。
pub(crate) fn execute_run_command_tool(
    app: &AppHandle,
    security_context: &SessionSecurityContext,
    arguments: &str,
) -> Result<serde_json::Value, String> {
    let request = serde_json::from_str::<RunCommandRequest>(arguments)
        .map_err(|e| format!("工具参数解析失败: {e}"))?;
    let workspace = security_context.workspace_root.clone();
    // 沙箱设置:前台/后台一致遵循(R3 修复——此前后台硬编码不沙箱化,是 fail-open 漏洞)。
    let sandbox = app.state::<AppState>().command_sandbox.load(Ordering::SeqCst);
    // AppContainer 网络门控:务实映射——Disabled 断网;AllowListed/FullAccess 放行裸命令出站
    // (逐域名 allowlist 在 AppContainer 层无法对裸 socket 强制,仅约束应用内网络工具)。
    let allow_network = matches!(
        security_context.network_mode,
        NetworkMode::AllowListed | NetworkMode::FullAccess
    );
    // 0.0.68：统一经 CommandSandbox::for_session 推导策略,不再向 run_command_streaming 传裸 bool。
    let policy = mdga_tool_runtime::CommandSandbox::for_session(sandbox, allow_network);

    if request.background {
        let shell_id = format!("sh-{}", BG_SHELL_SEQ.fetch_add(1, Ordering::SeqCst));
        let output = Arc::new(Mutex::new(String::new()));
        let status = Arc::new(Mutex::new("running".to_string()));
        let cancel = Arc::new(AtomicBool::new(false));
        // 注册到 AppState，供 get_shell_output / kill_shell / list_shells 访问。
        {
            let st = app.state::<AppState>();
            let mut shells = st.bg_shells.lock().expect("bg_shells mutex poisoned");
            shells.insert(
                shell_id.clone(),
                BgShell {
                    command: request.command.clone(),
                    output: output.clone(),
                    status: status.clone(),
                    cancel: cancel.clone(),
                },
            );
        }
        let app_bg = app.clone();
        let command_label = request.command.clone();
        let out_buf = output.clone();
        let cancel_thread = cancel.clone();
        std::thread::spawn(move || {
            // 输出逐行累积到共享缓冲（尾部截断 32K），供轮询；同时实时推前端。
            let app_line = app_bg.clone();
            let cb: mdga_tool_runtime::CommandLineCallback = std::sync::Arc::new(move |line: String| {
                if let Ok(mut buf) = out_buf.lock() {
                    buf.push_str(&line);
                    buf.push('\n');
                    let len = buf.chars().count();
                    if len > 32_000 {
                        *buf = buf.chars().skip(len - 32_000).collect();
                    }
                }
                let _ = app_line.emit("command-output", line);
            });
            // R3 修复：后台命令与前台一致受沙箱设置约束（沙箱路径现已支持 cancel，可被 kill_shell 终止）。
            let outcome = mdga_tool_runtime::run_command_streaming(
                &workspace,
                RunCommandRequest { background: false, ..request },
                Some(cb),
                Some(cancel_thread.clone()),
                policy,
            );
            let final_status = if cancel_thread.load(Ordering::SeqCst) {
                "killed"
            } else if outcome.is_err() {
                "error"
            } else {
                "done"
            };
            if let Ok(mut s) = status.lock() {
                *s = final_status.to_string();
            }
            let _ = app_bg.emit(
                "background-command-done",
                serde_json::json!({ "command": command_label, "status": final_status }),
            );
        });
        return Ok(serde_json::json!({
            "background": true,
            "shellId": shell_id,
            "note": "命令已在后台启动。用 get_shell_output 轮询输出、kill_shell 终止；你无需等待，继续后续步骤。"
        }));
    }

    // 前台命令：按设置决定是否在受限令牌沙箱中执行（沙箱设置已在上方读取，前后台一致）。
    let cb = command_line_callback(app);
    serde_json::to_value(
        mdga_tool_runtime::run_command_streaming(&workspace, request, Some(cb), None, policy)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

/// 后台 shell 工具：get_shell_output / kill_shell / list_shells。从 AppState 注册表读取/控制。
pub(crate) fn execute_bg_shell_tool(
    app: &AppHandle,
    tool_name: &str,
    arguments: &str,
) -> Result<serde_json::Value, String> {
    let st = app.state::<AppState>();
    let shells = st.bg_shells.lock().map_err(|e| e.to_string())?;
    match tool_name {
        "list_shells" => {
            let list: Vec<serde_json::Value> = shells
                .iter()
                .map(|(id, sh)| {
                    serde_json::json!({
                        "shellId": id,
                        "command": sh.command,
                        "status": sh.status.lock().map(|s| s.clone()).unwrap_or_default(),
                    })
                })
                .collect();
            Ok(serde_json::json!({ "shells": list }))
        }
        "get_shell_output" | "kill_shell" => {
            let parsed: serde_json::Value =
                serde_json::from_str(arguments).map_err(|e| format!("工具参数解析失败: {e}"))?;
            let id = parsed.get("shellId").and_then(|v| v.as_str()).ok_or("缺少 shellId")?;
            let sh = shells.get(id).ok_or("shellId 不存在")?;
            if tool_name == "kill_shell" {
                // 置位逻辑等价于 AppState::set_shell_cancel（前端 kill_bg_activity 走 helper）；
                // 此处 sh 已在持有 shells 锁的情况下取出，直接 store 以避免对同一 Mutex 重入死锁。
                sh.cancel.store(true, Ordering::SeqCst);
                return Ok(serde_json::json!({ "shellId": id, "note": "已请求终止该后台命令" }));
            }
            let output = sh.output.lock().map(|o| o.clone()).unwrap_or_default();
            let status = sh.status.lock().map(|s| s.clone()).unwrap_or_default();
            Ok(serde_json::json!({ "shellId": id, "status": status, "output": output }))
        }
        other => Err(format!("未知后台 shell 工具: {other}")),
    }
}
