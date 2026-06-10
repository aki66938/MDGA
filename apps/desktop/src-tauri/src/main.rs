use mdga_deepseek_client::{chat_stream, detect_api_key_status, ChatMessage};
use mdga_shared::ApiKeyStatus;
use mdga_token_accounting::{compute_cost_summary, deepseek_v3_pricing};
use tauri::{AppHandle, Emitter};

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

    // 计算费用摘要并发给前端，usage 缺失时仍发送以便前端标注"未知"
    if let Some(raw) = raw_usage {
        let summary = compute_cost_summary(&raw, &deepseek_v3_pricing());
        let _ = app.emit("chat-usage", summary);
    }

    let _ = app.emit("chat-done", ());
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_deepseek_api_key_status,
            send_message
        ])
        .run(tauri::generate_context!())
        .expect("failed to run MDGA desktop app");
}
