// release 构建以 Windows GUI 子系统运行，避免双击安装版时弹出空白命令行黑框；
// debug 构建保留控制台，便于开发期看日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use mdga_storage::{init_db, list_mcp_servers, record_activity_event};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

// 注（0.0.61）：软上限推导 context_soft_limit_for 在 mdga-agent-core（compaction 子模块）。
// context_window 改为纯用户自定义后，不再有 app 默认常量（旧 CONTEXT_SOFT_LIMIT_TOKENS 已删除）；
// 主模型未填窗口 ⇒ 软上限为 None ⇒ 不做窗口驱动压缩。桌面端经 `mdga_agent_core::...` 引用。
/// 摘要压缩时保留最近 N 条 wire 消息原文，更早的历史压缩成任务进度摘要。
pub(crate) const KEEP_RECENT_WIRE_MESSAGES: usize = 8;
/// 工具结果被压缩后替换成的短桩内容。
pub(crate) const COMPACTED_TOOL_STUB: &str =
    "{\"ok\":true,\"note\":\"[此前的工具结果已省略以节省上下文；如需该文件/目录/命令的最新内容，请重新调用对应工具读取]\"}";

mod state;
use state::{AppState, ToolUsageAcc};

/// 把一次工具完成累加进进程内按工具归因累加器（0.0.75 第三栏·用量标签）。
///
/// 纯逻辑（不持锁、不碰 app）：便于单测「只计完成、次数 +1、token 体积 = 输出长度 / 4 累加」。
/// `status` 仅 "succeeded" / "failed" 计入（"running" 跳过避免重复、"denied" 无执行无输出）；
/// `output_json` 为 None（如 running）时按 0 token 处理。返回是否实际记了一笔（供调用方判断）。
fn accumulate_tool_usage(
    table: &mut std::collections::HashMap<String, std::collections::HashMap<String, ToolUsageAcc>>,
    conversation_id: &str,
    tool_name: &str,
    status: &str,
    output_json: Option<&str>,
) -> bool {
    if status != "succeeded" && status != "failed" {
        return false;
    }
    // 粗估输出 token 体积：字符数 / 4（与项目其他「体积近似」口径一致，非精确分词、非成本）。
    let est_tokens = output_json.map(|s| (s.chars().count() / 4) as u64).unwrap_or(0);
    let entry = table
        .entry(conversation_id.to_string())
        .or_default()
        .entry(tool_name.to_string())
        .or_default();
    entry.calls += 1;
    entry.output_tokens = entry.output_tokens.saturating_add(est_tokens);
    true
}

mod commands;
use commands::{
    archive_conversation, cancel_agent, check_update, clear_all_conversations, clear_workspace,
    compact_history, create_mcp_server, create_permission_rule, delete_last_assistant_message,
    delete_mcp_server,
    delete_permission_rule, export_conversation, export_token_ledger, get_account_balance,
    get_app_info, get_checkpoints, get_command_sandbox, get_conversation_events, get_conversations,
    get_deepseek_api_key_status, get_mcp_servers, get_permission_rules, get_task_budget,
    get_workspace, import_file_text, install_update, list_custom_commands, list_workspace_dir,
    list_workspace_files, open_external_url, probe_command_sandbox, read_image_base64,
    read_workspace_file, recent_denied_actions, search_conversations,
    load_messages, load_wire, new_conversation, new_conversation_with_workspace, persist_message,
    pin_conversation, queue_steering, remove_conversation, rename_conversation, respond_approval,
    respond_ask_user, revert_to_checkpoint, rewind_to_message, set_command_sandbox,
    set_conversation_workspace, set_task_budget, set_workspace_path, toggle_mcp_server,
};
use commands::{
    add_model, delete_connection, delete_model, fetch_available_models, get_app_setting,
    get_connection_monthly_usage, get_thinking_profile, get_usage_attribution, list_connections,
    list_models,
    list_models_for_connection, lookup_effective_pricing, lookup_model_preset, save_connection,
    set_app_setting, set_connection_billing, set_model_pricing, smoke_test_tool_call,
    smoke_test_tool_call_for_connection, test_connection, update_model,
};
use commands::{
    clear_role_assignment, get_lsp_known_servers, get_lsp_server_config, get_role_assignments,
    save_lsp_server_config, set_role_assignment,
};
use commands::{get_bg_activity_output, get_tool_usage, kill_bg_activity, list_bg_activity};
use commands::{
    get_okf_settings, okf_browse, okf_clear_overlay, okf_export, okf_external_add,
    okf_external_list, okf_external_remove, okf_get_concept_source, okf_publish, okf_read_concept,
    okf_set_overlay, set_okf_settings,
};

mod pricing_capture;
use pricing_capture::{apply_pricing_overrides, capture_official_pricing, reset_pricing_overrides};

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

    // 0.0.75 第三栏·用量标签：按工具归因的进程内轻量累加（仅工具**完成**时计一笔，避免 running 重复）。
    // conversation_id 在所有 record_tool_event 调用点都现成可得（本函数签名即带），故直接按会话归因。
    // 锁失败软跳过（与本函数「写库失败也只忽略」的容错口径一致），绝不 panic、绝不阻塞工具循环。
    if status == "succeeded" || status == "failed" {
        if let Ok(mut table) = state.tool_usage.lock() {
            accumulate_tool_usage(&mut table, conversation_id, tool_name, status, output_json);
        }
    }
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
mod embedding;
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
                tool_usage: Mutex::new(HashMap::new()),
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

            // R-uicfg：从 DB 播种 LSP 服务器配置到进程级运行时缓存，使 lsp_* 工具按用户设置解析。
            // 无配置/解析失败时回退默认（全部启用、走 PATH），与从前行为一致。
            {
                let state = app.state::<AppState>();
                let lsp_json = state
                    .db
                    .lock()
                    .ok()
                    .and_then(|db| mdga_storage::get_lsp_server_config_json(&db).ok().flatten());
                if let Some(json) = lsp_json {
                    if let Ok(cfg) = serde_json::from_str::<mdga_lsp::LspServerConfig>(&json) {
                        tools::set_lsp_server_config(cfg);
                    }
                }
            }

            // P2 / 0.0.58:从 DB 播种 code_search 的可选 embedding 重排配置。
            // 默认关闭(设置项缺失/非 on)——播种后快照为 None,code_search 行为与 0.0.57 一致。
            {
                let state = app.state::<AppState>();
                let _ = state
                    .db
                    .lock()
                    .ok()
                    .map(|db| embedding::refresh_embedding_config(&db));
            }

            // okf_read：从 DB 播种「已登记外部 OKF 包」列表到进程级快照，使该只读工具在无 DB 句柄的
            // 执行路径里仍能强制「只读已登记包」这道安全闸。无登记＝空表（list 回空提示、read 一律拒）。
            {
                let state = app.state::<AppState>();
                let _ = state
                    .db
                    .lock()
                    .ok()
                    .map(|db| tools::refresh_okf_external_bundles(&db));
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
            list_workspace_dir,
            read_workspace_file,
            get_mcp_servers,
            create_mcp_server,
            toggle_mcp_server,
            delete_mcp_server,
            get_permission_rules,
            create_permission_rule,
            delete_permission_rule,
            get_command_sandbox,
            set_command_sandbox,
            probe_command_sandbox,
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
            open_external_url,
            list_connections,
            save_connection,
            delete_connection,
            test_connection,
            list_models_for_connection,
            list_models,
            add_model,
            update_model,
            delete_model,
            lookup_model_preset,
            lookup_effective_pricing,
            get_thinking_profile,
            set_model_pricing,
            set_connection_billing,
            capture_official_pricing,
            apply_pricing_overrides,
            reset_pricing_overrides,
            get_connection_monthly_usage,
            get_usage_attribution,
            fetch_available_models,
            smoke_test_tool_call,
            smoke_test_tool_call_for_connection,
            get_role_assignments,
            set_role_assignment,
            clear_role_assignment,
            get_app_setting,
            set_app_setting,
            get_lsp_known_servers,
            get_lsp_server_config,
            save_lsp_server_config,
            list_bg_activity,
            kill_bg_activity,
            get_bg_activity_output,
            get_tool_usage,
            get_okf_settings,
            set_okf_settings,
            okf_external_add,
            okf_external_remove,
            okf_external_list,
            okf_browse,
            okf_read_concept,
            okf_get_concept_source,
            okf_set_overlay,
            okf_clear_overlay,
            okf_publish,
            okf_export,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run MDGA desktop app");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// 0.0.75 用量埋点：只计「完成」状态，running/denied 不计；次数 +1、体积 = 输出长度/4 累加。
    #[test]
    fn accumulate_only_counts_completed() {
        let mut table: HashMap<String, HashMap<String, ToolUsageAcc>> = HashMap::new();
        // running 不计（避免与后续 succeeded 重复计同一次调用）。
        assert!(!accumulate_tool_usage(&mut table, "c1", "read_file", "running", None));
        // denied 不计（没真执行、无输出）。
        assert!(!accumulate_tool_usage(&mut table, "c1", "read_file", "denied", Some("{}")));
        assert!(table.is_empty(), "未完成事件不应建任何条目");

        // succeeded 计一笔：8 字符 / 4 = 2 token。
        assert!(accumulate_tool_usage(&mut table, "c1", "read_file", "succeeded", Some("12345678")));
        // failed 也计一笔（活动量含失败调用）：4 字符 / 4 = 1 token。
        assert!(accumulate_tool_usage(&mut table, "c1", "read_file", "failed", Some("abcd")));
        let acc = &table["c1"]["read_file"];
        assert_eq!(acc.calls, 2);
        assert_eq!(acc.output_tokens, 3);
    }

    /// 按 (会话, 工具) 分桶；不同会话、不同工具互不串味；output_json 为 None 计 0 token。
    #[test]
    fn accumulate_buckets_by_conversation_and_tool() {
        let mut table: HashMap<String, HashMap<String, ToolUsageAcc>> = HashMap::new();
        accumulate_tool_usage(&mut table, "c1", "read_file", "succeeded", Some("aaaa")); // 1 token
        accumulate_tool_usage(&mut table, "c1", "run_command", "succeeded", None); // 0 token
        accumulate_tool_usage(&mut table, "c2", "read_file", "succeeded", Some("bbbbbbbb")); // 2 token

        assert_eq!(table["c1"]["read_file"].calls, 1);
        assert_eq!(table["c1"]["read_file"].output_tokens, 1);
        assert_eq!(table["c1"]["run_command"].calls, 1);
        assert_eq!(table["c1"]["run_command"].output_tokens, 0);
        // 不同会话独立累加，互不影响。
        assert_eq!(table["c2"]["read_file"].calls, 1);
        assert_eq!(table["c2"]["read_file"].output_tokens, 2);
    }
}
