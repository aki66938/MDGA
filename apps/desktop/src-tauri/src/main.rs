// release 构建以 Windows GUI 子系统运行，避免双击安装版时弹出空白命令行黑框；
// debug 构建保留控制台，便于开发期看日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use mdga_storage::{init_db, list_mcp_servers, record_activity_event};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

// 注（Plan28 P3-9）：CONTEXT_SOFT_LIMIT_TOKENS 与软上限推导 context_soft_limit_for 已迁入
// mdga-agent-core（compaction 子模块）；本文件不再定义该常量，桌面端改 `mdga_agent_core::...` 引用。
/// 摘要压缩时保留最近 N 条 wire 消息原文，更早的历史压缩成任务进度摘要。
pub(crate) const KEEP_RECENT_WIRE_MESSAGES: usize = 8;
/// 工具结果被压缩后替换成的短桩内容。
pub(crate) const COMPACTED_TOOL_STUB: &str =
    "{\"ok\":true,\"note\":\"[此前的工具结果已省略以节省上下文；如需该文件/目录/命令的最新内容，请重新调用对应工具读取]\"}";

mod state;
use state::AppState;

mod commands;
use commands::{
    archive_conversation, cancel_agent, check_update, clear_all_conversations, clear_workspace,
    compact_history, create_mcp_server, create_permission_rule, delete_last_assistant_message,
    delete_mcp_server,
    delete_permission_rule, export_conversation, export_token_ledger, get_account_balance,
    get_app_info, get_checkpoints, get_command_sandbox, get_conversation_events, get_conversations,
    get_deepseek_api_key_status, get_mcp_servers, get_permission_rules, get_task_budget,
    get_workspace, import_file_text, install_update, list_custom_commands, list_workspace_files,
    read_image_base64, recent_denied_actions, search_conversations,
    load_messages, load_wire, new_conversation, new_conversation_with_workspace, persist_message,
    pin_conversation, queue_steering, remove_conversation, rename_conversation, respond_approval,
    respond_ask_user, revert_to_checkpoint, rewind_to_message, set_command_sandbox,
    set_conversation_workspace, set_task_budget, set_workspace_path, toggle_mcp_server,
};
use commands::{
    get_app_setting, get_model_provider_config, remove_model_provider, resolve_role_model_provider,
    save_model_provider, set_app_setting, smoke_test_tool_call, test_provider_connection,
};

// 注（Plan28 P3-9）：原 agent_prompt 模块（仅持有三个灵魂常量）已不再需要——常量权威定义
// 迁入 mdga-agent-core，消息构建也已迁过去；桌面端不再有 crate::agent_prompt::* 引用，故移除该模块。

mod agent_loop;
use agent_loop::send_message;

mod mcp;
use mcp::spawn_mcp_connect;

/// 记录一条工具 Activity Event，并同时推送给前端用于过程展示。
///
/// 输入会话 ID、工具名、状态、输入参数、可选输出/错误和工作区快照；
/// 写入失败不阻塞主流程，只忽略错误，保证一次工具失败不会拖垮整条对话。
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_tool_event(
    app: &AppHandle,
    conversation_id: &str,
    event_type: &str,
    tool_name: &str,
    status: &str,
    input_json: &str,
    output_json: Option<&str>,
    error_message: Option<&str>,
    workspace_path: &str,
) {
    let state = app.state::<AppState>();
    if let Ok(db) = state.db.lock() {
        let _ = record_activity_event(
            &db,
            conversation_id,
            event_type,
            Some(tool_name),
            status,
            Some(input_json),
            output_json,
            error_message,
            Some(workspace_path),
        );
    }
    let _ = app.emit(
        "tool-event",
        serde_json::json!({
            "toolName": tool_name,
            "status": status,
            "inputJson": input_json,
            "outputJson": output_json,
            "errorMessage": error_message,
        }),
    );
}

mod permissions;
mod checkpoint;
mod hooks;
mod web;
mod tools;
mod command_run;
mod subagent;
mod compaction;
mod chat;
// 已完成模块 compaction.rs 通过 `crate::chat_completion_with_retry` 引用，保留 crate 根再导出。
pub(crate) use chat::chat_completion_with_retry;

// 注（Plan28 P3-9）：merge_usage（纯 RawUsage 合并）已迁入 mdga-agent-core（usage 子模块）。
// 桌面端各调用点改为 `use mdga_agent_core::merge_usage;` 直接引用。

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
            app.manage(AppState {
                db: Mutex::new(db),
                cancels: Mutex::new(HashMap::new()),
                approvals: Mutex::new(HashMap::new()),
                ask_questions: Mutex::new(HashMap::new()),
                mcp: Mutex::new(HashMap::new()),
                steering: Mutex::new(HashMap::new()),
                repo_maps: Mutex::new(HashMap::new()),
                bg_shells: Mutex::new(HashMap::new()),
                bg_tasks: Mutex::new(HashMap::new()),
                command_sandbox: AtomicBool::new(true),
                task_token_budget: AtomicU64::new(0),
                loop_guards: Mutex::new(HashMap::new()),
            });

            // 启动时后台连接所有已启用的 MCP server，不阻塞窗口加载。
            {
                let state = app.state::<AppState>();
                let servers = state
                    .db
                    .lock()
                    .ok()
                    .and_then(|db| list_mcp_servers(&db).ok())
                    .unwrap_or_default();
                let handle = app.handle().clone();
                for record in servers.into_iter().filter(|s| s.enabled) {
                    spawn_mcp_connect(&handle, record);
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_deepseek_api_key_status,
            send_message,
            new_conversation,
            new_conversation_with_workspace,
            set_conversation_workspace,
            get_conversations,
            load_messages,
            persist_message,
            load_wire,
            rename_conversation,
            remove_conversation,
            pin_conversation,
            archive_conversation,
            get_app_info,
            get_account_balance,
            get_checkpoints,
            revert_to_checkpoint,
            compact_history,
            list_workspace_files,
            get_mcp_servers,
            create_mcp_server,
            toggle_mcp_server,
            delete_mcp_server,
            get_permission_rules,
            create_permission_rule,
            delete_permission_rule,
            get_command_sandbox,
            set_command_sandbox,
            get_task_budget,
            set_task_budget,
            export_conversation,
            export_token_ledger,
            clear_all_conversations,
            list_custom_commands,
            import_file_text,
            read_image_base64,
            get_conversation_events,
            delete_last_assistant_message,
            rewind_to_message,
            search_conversations,
            recent_denied_actions,
            cancel_agent,
            queue_steering,
            respond_approval,
            respond_ask_user,
            get_workspace,
            set_workspace_path,
            clear_workspace,
            check_update,
            install_update,
            get_model_provider_config,
            save_model_provider,
            remove_model_provider,
            resolve_role_model_provider,
            test_provider_connection,
            smoke_test_tool_call,
            get_app_setting,
            set_app_setting,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run MDGA desktop app");
}
