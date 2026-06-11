use mdga_deepseek_client::{chat_stream, detect_api_key_status, ChatMessage};
use mdga_shared::ApiKeyStatus;
use mdga_storage::{
    clear_active_workspace, create_conversation, delete_conversation, get_active_workspace,
    create_conversation_with_workspace, get_messages, init_db, list_conversations,
    save_active_workspace, save_message, update_title, Conversation, StoredMessage, Workspace,
};
use mdga_token_accounting::{compute_cost_summary, deepseek_pricing_for_model};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_updater::UpdaterExt;

// ── 应用状态 ──────────────────────────────────────────────────────────────

struct AppState {
    db: Mutex<rusqlite::Connection>,
}

// ── DeepSeek ──────────────────────────────────────────────────────────────

#[tauri::command]
fn get_deepseek_api_key_status() -> ApiKeyStatus {
    detect_api_key_status(|name| std::env::var(name).ok())
}

/// 发起流式聊天请求。
///
/// 通过 "chat-chunk" 事件逐块推送内容；流结束后发送 "chat-usage" 事件；
/// 最后发送 "chat-done"。错误时返回字符串供前端展示。
#[tauri::command]
async fn send_message(
    app: AppHandle,
    messages: Vec<ChatMessage>,
    model: String,
) -> Result<(), String> {
    let api_key = std::env::var("DEEPSEEK_API_KEY")
        .map_err(|_| "DEEPSEEK_API_KEY 未配置".to_string())?;

    let raw_usage = chat_stream(
        &api_key,
        messages,
        &model,
        |chunk| {
            let _ = app.emit("chat-chunk", chunk);
        },
    )
    .await
    .map_err(|e| e.to_string())?;

    if let Some(raw) = raw_usage {
        let summary = compute_cost_summary(&raw, &deepseek_pricing_for_model(&model));
        let _ = app.emit("chat-usage", summary);
    }

    let _ = app.emit("chat-done", ());
    Ok(())
}

// ── 会话管理 ──────────────────────────────────────────────────────────────

/// 创建新会话，初始标题为"新对话"。
#[tauri::command]
fn new_conversation(state: State<AppState>) -> Result<Conversation, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    create_conversation(&db).map_err(|e| e.to_string())
}

/// 创建新会话，并将会话绑定到创建时选择的工作区快照。
///
/// 输入可选工作区路径；路径存在时校验目录并写入 conversation snapshot，未传路径时创建纯聊天会话。
#[tauri::command]
fn new_conversation_with_workspace(
    state: State<AppState>,
    workspace_path: Option<String>,
) -> Result<Conversation, String> {
    let workspace = match workspace_path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(path) => {
            let path_buf = std::path::PathBuf::from(path);
            if !path_buf.is_dir() {
                return Err("工作区路径不存在或不是目录".to_string());
            }
            let name = workspace_name_from_path(path);
            Some((path.to_string(), name))
        }
        None => None,
    };

    let db = state.db.lock().map_err(|e| e.to_string())?;
    create_conversation_with_workspace(
        &db,
        workspace.as_ref().map(|(path, _)| path.as_str()),
        workspace.as_ref().map(|(_, name)| name.as_str()),
    )
    .map_err(|e| e.to_string())
}

/// 返回所有会话列表，按最近更新时间倒序。
#[tauri::command]
fn get_conversations(state: State<AppState>) -> Result<Vec<Conversation>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    list_conversations(&db).map_err(|e| e.to_string())
}

/// 返回指定会话的所有消息，按时间正序。
#[tauri::command]
fn load_messages(
    state: State<AppState>,
    conversation_id: String,
) -> Result<Vec<StoredMessage>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    get_messages(&db, &conversation_id).map_err(|e| e.to_string())
}

/// 持久化一条消息到数据库。
///
/// 输入会话 ID、角色、内容和可选的 usage JSON 字符串；
/// 同步更新会话的 updated_at 字段，维持列表排序。
#[tauri::command]
fn persist_message(
    state: State<AppState>,
    conversation_id: String,
    role: String,
    content: String,
    usage_json: Option<String>,
) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    save_message(
        &db,
        &conversation_id,
        &role,
        &content,
        usage_json.as_deref(),
    )
    .map_err(|e| e.to_string())
}

/// 更新会话标题。
///
/// 输入会话 ID 和新标题；用于首条消息发送后自动设置有意义的标题。
#[tauri::command]
fn rename_conversation(
    state: State<AppState>,
    conversation_id: String,
    title: String,
) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    update_title(&db, &conversation_id, &title).map_err(|e| e.to_string())
}

/// 删除会话及其全部消息。
#[tauri::command]
fn remove_conversation(
    state: State<AppState>,
    conversation_id: String,
) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    delete_conversation(&db, &conversation_id).map_err(|e| e.to_string())
}

// ── 工作区管理 ─────────────────────────────────────────────────────────────

/// 返回当前用户授权的活动工作区。
///
/// 输入应用状态；输出当前 Workspace 或 None，用于前端展示 Agent 可操作目录边界。
#[tauri::command]
fn get_workspace(state: State<AppState>) -> Result<Option<Workspace>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    get_active_workspace(&db).map_err(|e| e.to_string())
}

/// 保存当前用户授权的工作区路径。
///
/// 输入本地目录路径；后端校验路径存在且为目录，写入 SQLite 后返回 Workspace。
#[tauri::command]
fn set_workspace_path(state: State<AppState>, path: String) -> Result<Workspace, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("工作区路径不能为空".to_string());
    }

    let path_buf = std::path::PathBuf::from(trimmed);
    if !path_buf.is_dir() {
        return Err("工作区路径不存在或不是目录".to_string());
    }

    let db = state.db.lock().map_err(|e| e.to_string())?;
    save_active_workspace(&db, trimmed).map_err(|e| e.to_string())
}

/// 清除当前工作区授权。
///
/// 输入应用状态；删除当前活动工作区记录，后续 Agent 文件能力应视为未授权。
#[tauri::command]
fn clear_workspace(state: State<AppState>) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    clear_active_workspace(&db).map_err(|e| e.to_string())
}

// ── 自动更新 ──────────────────────────────────────────────────────────────

/// 检查 GitHub Releases 是否有新版本。
///
/// 返回新版本号字符串，无更新返回 None，出错返回 Err。
#[tauri::command]
async fn check_update(app: AppHandle) -> Result<Option<String>, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    match updater.check().await {
        Ok(Some(update)) => Ok(Some(update.version.clone())),
        Ok(None) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// 下载并安装更新，完成后自动重启应用。
///
/// 必须在用户明确确认后调用；下载进度通过 "update-progress" 事件推送。
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
                // downloaded: usize, total: Option<u64>，统一转 u64 再计算百分比
                let pct = total.map(|t| downloaded as u64 * 100 / t).unwrap_or(0);
                let _ = app_clone.emit("update-progress", pct);
            },
            || {
                let _ = app.emit("update-ready", ());
            },
        )
        .await
        .map_err(|e| e.to_string())?;

    app.restart();
}

fn workspace_name_from_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(path)
        .to_string()
}

// ── 入口 ──────────────────────────────────────────────────────────────────

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let db = init_db(&data_dir.join("mdga.db"))
                .map_err(|e| format!("无法初始化数据库: {e}"))?;
            app.manage(AppState { db: Mutex::new(db) });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_deepseek_api_key_status,
            send_message,
            new_conversation,
            new_conversation_with_workspace,
            get_conversations,
            load_messages,
            persist_message,
            rename_conversation,
            remove_conversation,
            get_workspace,
            set_workspace_path,
            clear_workspace,
            check_update,
            install_update,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run MDGA desktop app");
}
