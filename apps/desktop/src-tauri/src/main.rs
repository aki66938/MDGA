use mdga_deepseek_client::{chat_stream, detect_api_key_status, ChatMessage};
use mdga_shared::ApiKeyStatus;
use mdga_token_accounting::{compute_cost_summary, deepseek_v3_pricing};
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::UpdaterExt;

#[tauri::command]
fn get_deepseek_api_key_status() -> ApiKeyStatus {
    detect_api_key_status(|name| std::env::var(name).ok())
}

/// 发起流式聊天请求。
///
/// 通过 "chat-chunk" 事件逐块推送内容；流结束后发送 "chat-usage" 事件（含 token 用量和估算费用）；
/// 最后发送 "chat-done" 通知前端完成。错误时返回字符串，前端据此展示失败原因。
#[tauri::command]
async fn send_message(app: AppHandle, messages: Vec<ChatMessage>) -> Result<(), String> {
    let api_key = std::env::var("DEEPSEEK_API_KEY")
        .map_err(|_| "DEEPSEEK_API_KEY 未配置".to_string())?;

    let raw_usage = chat_stream(
        &api_key,
        messages,
        "deepseek-chat",
        |chunk| {
            let _ = app.emit("chat-chunk", chunk);
        },
    )
    .await
    .map_err(|e| e.to_string())?;

    if let Some(raw) = raw_usage {
        let summary = compute_cost_summary(&raw, &deepseek_v3_pricing());
        let _ = app.emit("chat-usage", summary);
    }

    let _ = app.emit("chat-done", ());
    Ok(())
}

/// 检查 GitHub Releases 是否有新版本。
///
/// 输入 AppHandle；若有新版本返回版本号字符串，无更新返回 None，出错返回 Err。
/// 实际下载与安装由前端在用户确认后调用 `install_update` 触发。
#[tauri::command]
async fn check_update(app: AppHandle) -> Result<Option<String>, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    match updater.check().await {
        Ok(Some(update)) => Ok(Some(update.version.clone())),
        Ok(None) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// 下载并安装更新，安装完成后自动重启应用。
///
/// 必须在用户明确确认后调用。下载进度通过 "update-progress" 事件推送给前端。
#[tauri::command]
async fn install_update(app: AppHandle) -> Result<(), String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "没有可用更新".to_string())?;

    let app_clone = app.clone();
    update
        .download_and_install(
            |downloaded, total| {
                let pct = total.map(|t| downloaded * 100 / t).unwrap_or(0);
                let _ = app_clone.emit("update-progress", pct);
            },
            || {
                let _ = app.emit("update-ready", ());
            },
        )
        .await
        .map_err(|e| e.to_string())?;

    tauri::process::restart(&app.env());
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            get_deepseek_api_key_status,
            send_message,
            check_update,
            install_update,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run MDGA desktop app");
}
