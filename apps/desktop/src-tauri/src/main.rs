use mdga_deepseek_client::{
    chat_stream, detect_api_key_status, get_user_balance, strip_dsml_markup, ChatMessage,
    UserBalance,
};
use mdga_sandbox_runtime::{session_security_context, NetworkMode, SessionSecurityContext};
use mdga_shared::{ApiKeyStatus, PermissionMode};
use mdga_storage::{
    add_mcp_server, add_permission_rule, clear_active_workspace, create_conversation,
    delete_conversation, delete_messages, get_active_workspace,
    create_conversation_with_workspace, get_activity_events, get_conversation, get_messages,
    init_db, list_conversations, list_file_checkpoints, list_mcp_servers,
    list_permission_rules, mark_checkpoint_reverted, record_activity_event,
    remove_mcp_server, remove_permission_rule, save_active_workspace,
    save_message,
    set_conversation_archived, set_conversation_pinned, set_mcp_server_enabled, update_title,
    ActivityEventRecord, Conversation, FileCheckpoint, StoredMessage, Workspace,
};
use mdga_token_accounting::{compute_cost_summary, deepseek_pricing_for_model};
use mdga_tool_runtime::{
    create_file, delete_dir, delete_file, edit_file, glob_files, list_dir, make_dir, move_path,
    read_file, run_command, search_text, stat_path, write_file, CreateFileRequest, DeleteDirRequest,
    DeleteFileRequest, EditFileRequest, GlobFilesRequest, ListDirRequest, MakeDirRequest,
    MovePathRequest, ReadFileRequest, RunCommandRequest, SearchTextRequest, StatPathRequest,
    WriteFileRequest,
};

/// Agent 工具循环单次会话内允许的最大工具轮数。作为防失控的硬上限兜底；
/// 真正的终止由「模型不再调用工具」自然触发，提高到 20 让多步开发任务不被过早截断。
/// 触发上下文压缩的软上限默认值（以上一次响应返回的 prompt_tokens 为准）。
/// DeepSeek V4 Flash / Pro 官方标称 1M 上下文，故取 800K：在接近上限前压缩，
/// 留约 200K headroom 给模型输出与当轮工具结果，避免顶满 1M 触发服务端退化。
/// 可用环境变量 MDGA_CONTEXT_SOFT_LIMIT 覆盖（便于低阈值压测验证压缩机制）。
pub(crate) const CONTEXT_SOFT_LIMIT_TOKENS: u64 = 800_000;
/// 摘要压缩时保留最近 N 条 wire 消息原文，更早的历史压缩成任务进度摘要。
pub(crate) const KEEP_RECENT_WIRE_MESSAGES: usize = 8;
/// 压缩时保留最近 N 次工具结果全文，更早的大体积结果替换为短桩。
const KEEP_RECENT_TOOL_RESULTS: usize = 3;
/// 仅压缩正文超过该字符数的旧工具结果；小结果不动，避免无谓信息损失。
const TOOL_RESULT_STUB_THRESHOLD: usize = 1_500;
/// 工具结果被压缩后替换成的短桩内容。
pub(crate) const COMPACTED_TOOL_STUB: &str =
    "{\"ok\":true,\"note\":\"[此前的工具结果已省略以节省上下文；如需该文件/目录/命令的最新内容，请重新调用对应工具读取]\"}";
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_updater::UpdaterExt;

mod state;
use state::{AppState, BgTask, BG_TASK_SEQ};

mod mcp;
use mcp::{
    collect_mcp_bindings, execute_add_mcp_server, execute_mcp_resource_tool, execute_mcp_tool,
    spawn_mcp_connect, McpBinding,
};

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
    state: State<'_, AppState>,
    conversation_id: String,
    messages: Vec<ChatMessage>,
    model: String,
    permission_mode: String,
    plan_mode: Option<bool>,
) -> Result<(), String> {
    let api_key = std::env::var("DEEPSEEK_API_KEY")
        .map_err(|_| "DEEPSEEK_API_KEY 未配置".to_string())?;
    let plan_mode = plan_mode.unwrap_or(false);
    let (conversation, permission_rules) = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let conversation = get_conversation(&db, &conversation_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "会话不存在".to_string())?;
        let rules = list_permission_rules(&db).unwrap_or_default();
        (conversation, rules)
    };
    // 工作区已绑定时生成 repo map 与长期记忆，注入项目结构摘要和持久约定供模型开局认知。
    // repo map 按会话缓存：首轮生成后复用，保持 system 前缀字节稳定以提升 prompt 缓存命中。
    let repo_map = conversation
        .workspace_path
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .map(|path| {
            let mut maps = state.repo_maps.lock().expect("repo_maps mutex poisoned");
            maps.entry(conversation_id.clone())
                .or_insert_with(|| mdga_tool_runtime::workspace_map(path))
                .clone()
        });
    let workspace_memory = conversation
        .workspace_path
        .as_deref()
        .and_then(read_workspace_memory);
    let skills = conversation
        .workspace_path
        .as_deref()
        .map(load_workspace_skills)
        .unwrap_or_default();
    let mut messages = messages_with_workspace_context(
        messages,
        conversation.workspace_path.as_deref(),
        conversation.workspace_name.as_deref(),
        repo_map.as_deref(),
        workspace_memory.as_deref(),
        &skills,
    );
    // 计划模式：要求模型只产出分步计划并等待确认，本轮不提供工具。
    if plan_mode {
        messages.insert(0, ChatMessage {
            role: "system".to_string(),
            content: "用户开启了计划模式：请基于需求给出清晰的分步执行计划（目标、步骤、涉及文件、风险点），然后停止并等待用户确认。本轮不要执行任何实际操作。".to_string(),
        });
    }
    let permission = permission_mode_from_str(&permission_mode);

    // 注册本轮会话的取消令牌，供 cancel_agent 命令置位、工具循环检查。
    let cancel_token = Arc::new(AtomicBool::new(false));
    {
        let mut cancels = state.cancels.lock().map_err(|e| e.to_string())?;
        cancels.insert(conversation_id.clone(), cancel_token.clone());
    }

    let result = if plan_mode {
        // 计划模式走纯流式（无工具），让模型把计划直接流给用户审阅。
        chat_stream(&api_key, messages, &model, |chunk| {
            let _ = app.emit("chat-chunk", chunk);
        })
        .await
        .map_err(|e| e.to_string())
    } else if let Some(workspace_path) = conversation.workspace_path.as_deref() {
        let mcp_bindings = collect_mcp_bindings(&app);
        chat_with_builtin_tools(
            &api_key,
            messages,
            &model,
            workspace_path,
            permission,
            &conversation_id,
            &app,
            cancel_token.clone(),
            permission_rules,
            mcp_bindings,
        )
        .await
    } else {
        chat_stream(&api_key, messages, &model, |chunk| {
            let _ = app.emit("chat-chunk", chunk);
        })
        .await
        .map_err(|e| e.to_string())
    };

    // 无论成功或失败都要清理取消令牌与残留的 steering 队列，避免影响下一轮。
    {
        if let Ok(mut cancels) = state.cancels.lock() {
            cancels.remove(&conversation_id);
        }
        if let Ok(mut steering) = state.steering.lock() {
            steering.remove(&conversation_id);
        }
    }

    let raw_usage = result?;

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
    parts_json: Option<String>,
) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    save_message(
        &db,
        &conversation_id,
        &role,
        &content,
        usage_json.as_deref(),
        parts_json.as_deref(),
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

/// 设置会话置顶状态；置顶会话在列表中排在最前。
#[tauri::command]
fn pin_conversation(
    state: State<AppState>,
    conversation_id: String,
    pinned: bool,
) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    set_conversation_pinned(&db, &conversation_id, pinned).map_err(|e| e.to_string())
}

/// 查询 DeepSeek 账户余额，供设置页展示。从环境变量读取 API Key，不缓存、不持久化。
#[tauri::command]
async fn get_account_balance() -> Result<UserBalance, String> {
    let api_key = std::env::var("DEEPSEEK_API_KEY")
        .map_err(|_| "DEEPSEEK_API_KEY 未配置".to_string())?;
    get_user_balance(&api_key).await.map_err(|e| e.to_string())
}

/// 返回应用信息（版本号、数据目录路径），供设置页展示。
#[tauri::command]
fn get_app_info(app: AppHandle) -> Result<serde_json::Value, String> {
    let version = app.package_info().version.to_string();
    let data_dir = app
        .path()
        .app_data_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    Ok(serde_json::json!({ "version": version, "dataDir": data_dir }))
}

/// 设置会话归档状态；归档会话移入侧边栏「已归档」区，数据不删除。
#[tauri::command]
fn archive_conversation(
    state: State<AppState>,
    conversation_id: String,
    archived: bool,
) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    set_conversation_archived(&db, &conversation_id, archived).map_err(|e| e.to_string())
}

/// 返回指定会话的全部文件变更检查点（含已回退的），供「变更记录」面板展示。
#[tauri::command]
fn get_checkpoints(
    state: State<AppState>,
    conversation_id: String,
) -> Result<Vec<FileCheckpoint>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    list_file_checkpoints(&db, &conversation_id).map_err(|e| e.to_string())
}

/// 回退到指定检查点之前：把该检查点及其后的所有可回退变更按倒序撤销（CC 的 rewind）。
/// 返回成功回退的条数；不可回退的变更跳过。
#[tauri::command]
fn revert_to_checkpoint(
    state: State<AppState>,
    conversation_id: String,
    checkpoint_id: String,
) -> Result<usize, String> {
    let (workspace, targets) = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let conversation = get_conversation(&db, &conversation_id)
            .map_err(|e| e.to_string())?
            .ok_or("会话不存在")?;
        let workspace = conversation
            .workspace_path
            .ok_or("该会话未绑定工作区")?;
        let all = list_file_checkpoints(&db, &conversation_id).map_err(|e| e.to_string())?;
        let target_seq = all
            .iter()
            .find(|c| c.id == checkpoint_id)
            .map(|c| c.seq)
            .ok_or("检查点不存在")?;
        let mut targets: Vec<FileCheckpoint> = all
            .into_iter()
            .filter(|c| c.seq >= target_seq && !c.reverted && c.revertible)
            .collect();
        // 按序号倒序撤销，后发生的变更先回退。
        targets.sort_by(|a, b| b.seq.cmp(&a.seq));
        (workspace, targets)
    };

    let mut reverted = 0;
    for checkpoint in &targets {
        if apply_checkpoint_revert(&workspace, checkpoint).is_ok() {
            let db = state.db.lock().map_err(|e| e.to_string())?;
            let _ = mark_checkpoint_reverted(&db, &checkpoint.id);
            reverted += 1;
        }
    }
    Ok(reverted)
}

/// 手动压缩会话历史（/compact）：把全部消息摘要成一条任务备忘录替换原文。
#[tauri::command]
async fn compact_history(
    app: AppHandle,
    state: State<'_, AppState>,
    conversation_id: String,
    model: String,
) -> Result<(), String> {
    let api_key = std::env::var("DEEPSEEK_API_KEY")
        .map_err(|_| "DEEPSEEK_API_KEY 未配置".to_string())?;
    let messages = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        get_messages(&db, &conversation_id).map_err(|e| e.to_string())?
    };
    if messages.len() < 4 {
        return Err("当前会话消息较少，无需压缩".to_string());
    }

    let transcript: String = messages
        .iter()
        .map(|m| {
            let body: String = m.content.chars().take(800).collect();
            format!("[{}] {}", m.role, body)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = format!(
        "你是对话压缩器。请把下面这段用户与 AI 助手的完整对话压缩成简明的中文备忘录，\
保留：1）用户的总体目标与关键需求；2）已完成的事项与结论；3）重要决策与原因；\
4）涉及的文件清单；5）当前进度与待办。只输出备忘录本身。\n\n{transcript}"
    );
    let result = chat_completion_with_retry(
        &api_key,
        vec![serde_json::json!({ "role": "user", "content": prompt })],
        &model,
        None,
        &app,
    )
    .await?;
    let summary = result.content.unwrap_or_default();
    if summary.trim().is_empty() {
        return Err("压缩失败：模型未返回摘要".to_string());
    }

    let db = state.db.lock().map_err(|e| e.to_string())?;
    delete_messages(&db, &conversation_id).map_err(|e| e.to_string())?;
    save_message(
        &db,
        &conversation_id,
        "assistant",
        &format!("📋 对话已手动压缩（/compact），以下为此前内容的摘要：\n\n{summary}"),
        None,
        None,
    )
    .map_err(|e| e.to_string())
}

/// 列出工作区 .mdga/commands/*.md 自定义斜杠命令（name + description + body）。
/// 命令体中的 $ARGUMENTS 由前端替换为用户在 /name 后输入的参数。
#[tauri::command]
fn list_custom_commands(
    state: State<AppState>,
    conversation_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    let workspace = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        get_conversation(&db, &conversation_id)
            .map_err(|e| e.to_string())?
            .and_then(|c| c.workspace_path)
    };
    let Some(workspace) = workspace else {
        return Ok(Vec::new());
    };
    let dir = std::path::Path::new(&workspace).join(".mdga").join("commands");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut cmds = Vec::new();
    for entry in entries.flatten().take(50) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        let Ok(raw) = std::fs::read_to_string(&path) else { continue };
        // 解析 frontmatter description（可选），其余为命令体。
        let (description, body) = parse_command_frontmatter(&raw);
        cmds.push(serde_json::json!({
            "name": format!("/{stem}"),
            "description": description,
            "body": body,
        }));
    }
    Ok(cmds)
}

/// 从命令 markdown 解析 frontmatter 的 description，返回 (description, body)。
fn parse_command_frontmatter(raw: &str) -> (String, String) {
    let trimmed = raw.trim_start();
    if let Some(rest) = trimmed.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            let front = &rest[..end];
            let body = rest[end + 4..].trim_start().to_string();
            let desc = front
                .lines()
                .find_map(|l| l.trim().strip_prefix("description:").map(|d| d.trim().to_string()))
                .unwrap_or_default();
            return (desc, body);
        }
    }
    // 无 frontmatter：首行非空作描述，全文作命令体
    let desc = raw.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string();
    (desc, raw.trim().to_string())
}

/// 平铺列出当前会话工作区内的文件相对路径，供输入框 @文件引用补全。
#[tauri::command]
fn list_workspace_files(
    state: State<AppState>,
    conversation_id: String,
) -> Result<Vec<String>, String> {
    let workspace = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        get_conversation(&db, &conversation_id)
            .map_err(|e| e.to_string())?
            .and_then(|c| c.workspace_path)
            .ok_or("该会话未绑定工作区")?
    };
    Ok(mdga_tool_runtime::workspace_file_list(&workspace, 500))
}

/// 读取命令沙箱开关状态。
#[tauri::command]
fn get_command_sandbox(state: State<AppState>) -> bool {
    state.command_sandbox.load(Ordering::SeqCst)
}

/// 设置命令沙箱开关。开启时前台命令在受限令牌沙箱中执行（降权 + 进程清理 + 密钥擦除）。
#[tauri::command]
fn set_command_sandbox(state: State<AppState>, enabled: bool) {
    state.command_sandbox.store(enabled, Ordering::SeqCst);
}

/// 导出单个会话为 Markdown 文件（数据治理：用户可导出/备份）。
#[tauri::command]
fn export_conversation(
    state: State<AppState>,
    conversation_id: String,
    path: String,
) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let conv = get_conversation(&db, &conversation_id)
        .map_err(|e| e.to_string())?
        .ok_or("会话不存在")?;
    let messages = get_messages(&db, &conversation_id).map_err(|e| e.to_string())?;
    let mut md = format!("# {}\n\n", conv.title);
    if let Some(ws) = conv.workspace_path.as_deref() {
        md.push_str(&format!("> 工作区：{ws}\n\n"));
    }
    for m in messages {
        let who = if m.role == "user" { "用户" } else { "助手" };
        md.push_str(&format!("## {who}\n\n{}\n\n", m.content));
    }
    std::fs::write(&path, md).map_err(|e| format!("写入失败: {e}"))
}

/// 导出全部会话的 token 账本为 CSV（数据治理 + 对账）。
#[tauri::command]
fn export_token_ledger(state: State<AppState>, path: String) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let convs = list_conversations(&db).map_err(|e| e.to_string())?;
    let mut csv = String::from(
        "conversation_id,title,role,total_tokens,prompt_tokens,completion_tokens,estimated_cost_usd,created_at\n",
    );
    for conv in convs {
        let messages = get_messages(&db, &conv.id).map_err(|e| e.to_string())?;
        for m in messages {
            let Some(usage_json) = m.usage_json.as_deref() else { continue };
            let v: serde_json::Value = serde_json::from_str(usage_json).unwrap_or_default();
            let g = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
            let cost = v.get("estimatedCostUsd").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let title = conv.title.replace([',', '\n', '"'], " ");
            csv.push_str(&format!(
                "{},{},{},{},{},{},{:.6},{}\n",
                conv.id, title, m.role, g("totalTokens"), g("promptTokens"),
                g("completionTokens"), cost, m.created_at
            ));
        }
    }
    std::fs::write(&path, csv).map_err(|e| format!("写入失败: {e}"))
}

/// 清除全部会话与消息（数据治理：用户主动删除本地数据）。
#[tauri::command]
fn clear_all_conversations(state: State<AppState>) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    mdga_storage::delete_all_conversations(&db).map_err(|e| e.to_string())
}

/// 读取单次任务 token 预算（0 = 不限）。
#[tauri::command]
fn get_task_budget(state: State<AppState>) -> u64 {
    state.task_token_budget.load(Ordering::SeqCst)
}

/// 设置单次任务 token 预算；超出后工具循环暂停并提示。
#[tauri::command]
fn set_task_budget(state: State<AppState>, budget: u64) {
    state.task_token_budget.store(budget, Ordering::SeqCst);
}

/// 列出全部权限规则（allow / deny），供设置页管理。
#[tauri::command]
fn get_permission_rules(state: State<AppState>) -> Result<Vec<String>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    list_permission_rules(&db).map_err(|e| e.to_string())
}

/// 新增一条权限规则（如 `deny:read_file:**/.env`、`allow:cmd:git push`）。
#[tauri::command]
fn create_permission_rule(state: State<AppState>, rule: String) -> Result<(), String> {
    let rule = rule.trim();
    if rule.is_empty() {
        return Err("规则不能为空".to_string());
    }
    let db = state.db.lock().map_err(|e| e.to_string())?;
    add_permission_rule(&db, rule).map_err(|e| e.to_string())
}

/// 删除一条权限规则。
#[tauri::command]
fn delete_permission_rule(state: State<AppState>, rule: String) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    remove_permission_rule(&db, &rule).map_err(|e| e.to_string())
}

/// 列出 MCP server 配置及连接状态（connected/disconnected + 工具数）。
#[tauri::command]
fn get_mcp_servers(state: State<AppState>) -> Result<Vec<serde_json::Value>, String> {
    let records = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        list_mcp_servers(&db).map_err(|e| e.to_string())?
    };
    let connected = state.mcp.lock().map_err(|e| e.to_string())?;
    Ok(records
        .into_iter()
        .map(|r| {
            let client = connected.get(&r.id);
            serde_json::json!({
                "id": r.id,
                "name": r.name,
                "command": r.command,
                "enabled": r.enabled,
                "connected": client.is_some(),
                "toolCount": client.map(|c| c.tools.len()).unwrap_or(0),
            })
        })
        .collect())
}

/// 新增 MCP server 配置并立即后台尝试连接。
#[tauri::command]
#[allow(non_snake_case)]
fn create_mcp_server(
    app: AppHandle,
    state: State<AppState>,
    name: String,
    command: String,
    authToken: Option<String>,
) -> Result<(), String> {
    let name = name.trim();
    let command = command.trim();
    if name.is_empty() || command.is_empty() {
        return Err("名称与启动命令/URL 不能为空".to_string());
    }
    let token = authToken.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let record = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        add_mcp_server(&db, name, command, token).map_err(|e| e.to_string())?
    };
    spawn_mcp_connect(&app, record);
    Ok(())
}

/// 启用 / 停用一个 MCP server：停用立即断开，启用立即后台重连。
#[tauri::command]
fn toggle_mcp_server(
    app: AppHandle,
    state: State<AppState>,
    server_id: String,
    enabled: bool,
) -> Result<(), String> {
    let record = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        set_mcp_server_enabled(&db, &server_id, enabled).map_err(|e| e.to_string())?;
        list_mcp_servers(&db)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|r| r.id == server_id)
    };
    if enabled {
        if let Some(record) = record {
            spawn_mcp_connect(&app, record);
        }
    } else if let Ok(mut map) = state.mcp.lock() {
        map.remove(&server_id); // Drop 时杀子进程
    }
    Ok(())
}

/// 删除一个 MCP server 配置并断开连接。
#[tauri::command]
fn delete_mcp_server(state: State<AppState>, server_id: String) -> Result<(), String> {
    {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        remove_mcp_server(&db, &server_id).map_err(|e| e.to_string())?;
    }
    if let Ok(mut map) = state.mcp.lock() {
        map.remove(&server_id);
    }
    Ok(())
}

/// 导入本地文档并抽取纯文本（TXT/MD/CSV/JSON/PDF/DOCX），供发送给模型问答。
#[tauri::command]
fn import_file_text(path: String) -> Result<serde_json::Value, String> {
    const MAX_IMPORT_CHARS: usize = 100_000;
    let file_path = std::path::Path::new(&path);
    if !file_path.is_file() {
        return Err("文件不存在".to_string());
    }
    let name = file_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = file_path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let text = match ext.as_str() {
        "txt" | "md" | "markdown" | "csv" | "json" | "log" | "xml" | "html" | "toml"
        | "yaml" | "yml" => std::fs::read_to_string(file_path)
            .map_err(|e| format!("读取文件失败: {e}"))?,
        "pdf" => pdf_extract::extract_text(file_path)
            .map_err(|e| format!("PDF 解析失败: {e}"))?,
        "docx" => extract_docx_text(file_path)?,
        other => {
            return Err(format!(
                "暂不支持 .{other} 格式（支持 txt/md/csv/json/pdf/docx 等文本类文档；图片需视觉模型，后续版本支持）"
            ));
        }
    };

    let truncated = text.chars().count() > MAX_IMPORT_CHARS;
    let capped: String = text.chars().take(MAX_IMPORT_CHARS).collect();
    Ok(serde_json::json!({ "name": name, "text": capped, "truncated": truncated }))
}

/// 从 .docx（zip 内 word/document.xml）抽取纯文本：拼接所有 <w:t> 文本节点，段落换行。
fn extract_docx_text(path: &std::path::Path) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("打开文件失败: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("DOCX 解析失败: {e}"))?;
    let mut doc = archive
        .by_name("word/document.xml")
        .map_err(|_| "DOCX 缺少 document.xml".to_string())?;
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut doc, &mut xml).map_err(|e| format!("读取失败: {e}"))?;

    // 轻量抽取：<w:p> 段落分行，<w:t> 文本节点取内容；不引入完整 XML 解析依赖。
    let mut out = String::new();
    let paragraphs = xml.split("</w:p>");
    for paragraph in paragraphs {
        let mut cursor = 0;
        let bytes = paragraph;
        while let Some(start) = bytes[cursor..].find("<w:t") {
            let tag_start = cursor + start;
            let Some(open_end) = bytes[tag_start..].find('>') else { break };
            let text_start = tag_start + open_end + 1;
            let Some(close) = bytes[text_start..].find("</w:t>") else { break };
            out.push_str(&bytes[text_start..text_start + close]);
            cursor = text_start + close + 6;
        }
        if !out.ends_with('\n') && !out.is_empty() {
            out.push('\n');
        }
    }
    Ok(out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'"))
}

/// 扫描工作区 .mdga/skills/*/SKILL.md，返回技能名与描述（首行 frontmatter 或首段）。
fn load_workspace_skills(workspace: &str) -> Vec<(String, String)> {
    let skills_dir = std::path::Path::new(workspace).join(".mdga").join("skills");
    let Ok(entries) = std::fs::read_dir(&skills_dir) else {
        return Vec::new();
    };
    let mut skills = Vec::new();
    for entry in entries.flatten().take(30) {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let skill_md = dir.join("SKILL.md");
        let Ok(content) = std::fs::read_to_string(&skill_md) else {
            continue;
        };
        // 描述：取 frontmatter 的 description: 行，否则取第一行非空文本。
        let description = content
            .lines()
            .find_map(|line| line.trim().strip_prefix("description:").map(|d| d.trim().to_string()))
            .or_else(|| {
                content
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty() && !l.starts_with("---") && !l.starts_with('#'))
                    .map(str::to_string)
            })
            .unwrap_or_default();
        skills.push((name, description));
    }
    skills
}

/// load_skill 工具：按名加载工作区技能的完整 SKILL.md 内容（按需注入，CC 渐进披露同款）。
fn execute_load_skill(workspace: &str, arguments: &str) -> Result<serde_json::Value, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(arguments).map_err(|e| format!("工具参数解析失败: {e}"))?;
    let name = parsed
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("load_skill 缺少 name")?;
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("技能名只允许字母数字、横线与下划线".to_string());
    }
    let path = std::path::Path::new(workspace)
        .join(".mdga")
        .join("skills")
        .join(name)
        .join("SKILL.md");
    let content = std::fs::read_to_string(&path).map_err(|_| format!("技能 {name} 不存在"))?;
    let capped: String = content.chars().take(32_000).collect();
    Ok(serde_json::json!({ "name": name, "skill": capped }))
}

/// 返回指定会话的所有工具 Activity Event，按时间正序，供前端展示历史过程面板。
#[tauri::command]
fn get_conversation_events(
    state: State<AppState>,
    conversation_id: String,
) -> Result<Vec<ActivityEventRecord>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    get_activity_events(&db, &conversation_id).map_err(|e| e.to_string())
}

/// 在 Agent 运行中排队一条插话消息（steering）。下一轮循环开始时作为 user 消息注入，
/// 让用户无需打断即可纠偏 / 追加要求。返回当前队列长度。
#[tauri::command]
fn queue_steering(
    state: State<AppState>,
    conversation_id: String,
    text: String,
) -> Result<usize, String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("插话内容不能为空".to_string());
    }
    let mut steering = state.steering.lock().map_err(|e| e.to_string())?;
    let queue = steering.entry(conversation_id).or_default();
    queue.push(text);
    Ok(queue.len())
}

/// 请求中断指定会话正在运行的 Agent 工具循环。
///
/// 置位该会话的取消标志；循环在下一个检查点安全收尾，已执行的工具结果保留。
/// 若该会话当前没有运行中的 Agent，则为无操作。
#[tauri::command]
fn cancel_agent(state: State<AppState>, conversation_id: String) -> Result<(), String> {
    let cancels = state.cancels.lock().map_err(|e| e.to_string())?;
    if let Some(token) = cancels.get(&conversation_id) {
        token.store(true, Ordering::SeqCst);
    }
    Ok(())
}

/// 前端对一次高风险动作审批请求作出回应（允许 / 拒绝 / 总是允许）。
///
/// 通过 action_id 找到对应的 oneshot 通道并发送结果，唤醒正在等待的工具循环；
/// remember=true 且批准时，把该动作的规则写入 permission_rules 表，后续同类动作免审批。
#[tauri::command]
fn respond_approval(
    state: State<AppState>,
    action_id: String,
    approved: bool,
    remember: Option<bool>,
) -> Result<(), String> {
    let entry = {
        let mut approvals = state.approvals.lock().map_err(|e| e.to_string())?;
        approvals.remove(&action_id)
    };
    if let Some((sender, rule)) = entry {
        if approved && remember.unwrap_or(false) && !rule.is_empty() {
            if let Ok(db) = state.db.lock() {
                let _ = add_permission_rule(&db, &rule);
            }
        }
        let _ = sender.send(approved);
    }
    Ok(())
}

/// 前端对一次 ask_user 结构化提问作出回应，把答案 JSON（已含用户选择 / 自定义文本）
/// 通过 question_id 对应的 oneshot 通道送回正在等待的工具循环。
#[tauri::command]
fn respond_ask_user(
    state: State<AppState>,
    question_id: String,
    answer: String,
) -> Result<(), String> {
    let sender = {
        let mut map = state.ask_questions.lock().map_err(|e| e.to_string())?;
        map.remove(&question_id)
    };
    if let Some(sender) = sender {
        let _ = sender.send(answer);
    }
    Ok(())
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

fn messages_with_workspace_context(
    messages: Vec<ChatMessage>,
    workspace_path: Option<&str>,
    workspace_name: Option<&str>,
    repo_map: Option<&str>,
    workspace_memory: Option<&str>,
    skills: &[(String, String)],
) -> Vec<ChatMessage> {
    // 把后端可信的 session 工作区快照注入模型上下文，使 DeepSeek 能回答当前工作区问题。
    let Some(path) = workspace_path.filter(|path| !path.trim().is_empty()) else {
        // 纯聊天会话（未绑定工作区）：明确告知模型没有任何工具，防止它凭训练记忆
        // 幻觉输出 <ToolCall>/DSML 等工具调用标记（0.0.17 dev 实测出现过）。
        let mut injected = Vec::with_capacity(messages.len() + 1);
        injected.push(ChatMessage {
            role: "system".to_string(),
            content: "当前会话未绑定工作区，你没有任何本地文件、目录或命令工具可用。\
如果用户要求读写文件、列目录、修改代码或执行命令，请直接告知：需要点击「+ 新对话」并选择工作区后才能执行本地操作。\
绝对不要输出任何工具调用标记（如 <ToolCall>、DSML 标记等），也不要假装已经执行了本地操作。"
                .to_string(),
        });
        injected.extend(messages);
        return injected;
    };
    let name = workspace_name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("未命名工作区");
    let mut injected = Vec::with_capacity(messages.len() + 2);
    // 身份锚定：明确 MDGA 不是 Claude Code，配置在 .mdga/，防止模型沿用 .claude 等训练记忆里的约定。
    injected.push(ChatMessage {
        role: "system".to_string(),
        content: "你是 MDGA（Make DeepSeek Great Again）桌面 Agent 的内置助手，运行在 MDGA 应用里。\
你不是 Claude Code，也不是 Codex，不要沿用它们的约定：本应用的配置目录是工作区下的 .mdga/（不是 .claude/，MDGA 没有也不读取 .claude 目录及其中的 settings.json）。\
MDGA 的可扩展配置都在 .mdga/ 下：技能 .mdga/skills/<名>/SKILL.md，钩子 .mdga/hooks.json，自定义斜杠命令 .mdga/commands/<名>.md，自定义子代理 .mdga/agents/<类型>.md，诊断命令 .mdga/diagnostics；项目长期记忆是工作区根目录的 MDGA.md。\
安装/配置 MCP 服务器不是通过编辑任何配置文件——请调用 add_mcp_server 工具注册（它会写入 MDGA 的服务器表并立即连接、其工具随后即可调用），或让用户在「设置 → MCP 服务器」添加。绝不要去查找或编辑 .claude/settings.json 之类文件，那对 MDGA 完全无效。".to_string(),
    });
    injected.push(ChatMessage {
        role: "system".to_string(),
        content: format!(
            "你正在 MDGA 桌面端中运行。本轮会话绑定的工作区名称是 {name}，工作区路径是 {path}。\
除非用户明确授权越界，否则你应假定所有本地文件任务都发生在该工作区内。\
当用户询问你当前所在的工作区或工作目录时，应直接回答这个路径；不要声称自己没有工作区。\
当用户要求列目录、读取文件、创建文件、修改文件或删除文件时，必须分别调用 list_dir、read_file、\
create_file、write_file 或 delete_file 工具完成真实本地操作；不要只给出代码示例，\
不要建议用户手动操作，也不要在没有工具结果时声称文件已处理。"
        ),
    });
    injected.push(ChatMessage {
        role: "system".to_string(),
        content: "工具调用规则：所有本地文件和命令操作必须通过工具完成，不能只在正文中声称已经完成。可用工具包括 list_dir、read_file、create_file、write_file、edit_file、delete_file、make_dir、move_path、delete_dir、stat_path、search_text、run_command。修改已有文件时优先使用 edit_file，并提供 oldText/newText；只有需要完整覆盖文件时才使用 write_file。移动或重命名文件用 move_path，不要用 create+delete 模拟。执行前需要了解目录、文件存在性或代码位置时，先使用 list_dir、stat_path 或 search_text。run_command 用于列目录、git status、构建或测试等命令：低风险命令（cargo check/test、npm test/run build、git status/diff、dir 等）在 Workspace Auto 下可直接执行，其余命令需 Full Access 或用户审批。每一步都要基于真实工具结果继续；若某次工具因权限被拒绝或用户拒绝，应说明情况或改用被允许的方式，不要重复硬闯。若某次工具调用失败，请阅读返回的 error，判断是参数、路径还是环境问题，调整后重试或换用其他工具，不要原样重复同一次失败调用。对于多步骤任务，请先调用 todo_write 列出步骤清单并随进度更新状态（同一时刻只有一项 in_progress），让用户实时看到进度。当需求确实含糊、且靠读文件或运行工具也无法判断、继续就会做错方向时，用 ask_user 给出 1-4 个结构化选项让用户选择，而不是擅自假设；能自己查清的事不要问。需要在大型代码库做只读调查（找实现、理结构、读懂模块）时，优先调用 run_subtask 委托独立子代理，避免主对话上下文膨胀。长时间运行的命令（启动服务、watch 等）用 run_command 的 background=true，它会立即返回 shellId；之后用 get_shell_output 轮询其输出与状态、用 kill_shell 终止、用 list_shells 查看所有后台进程。用户消息中的 @相对路径 表示工作区文件引用，直接用 read_file 读取即可。需要查阅在线文档、报错信息或你不确定的最新资料时，用 web_search 搜索、再用 web_fetch 抓取相关 URL 的正文，不要凭记忆臆测。遇到值得跨会话记住的项目约定、关键路径或踩过的坑，用 remember 写入项目长期记忆（精炼、可复用的事实才记，临时细节不要记）。".to_string(),
    });
    // repo map：开局注入工作区结构摘要，让模型无需逐层 list_dir 就了解项目骨架。
    if let Some(map) = repo_map.filter(|map| !map.trim().is_empty()) {
        injected.push(ChatMessage {
            role: "system".to_string(),
            content: format!(
                "当前工作区结构摘要（已忽略 .git/node_modules/target 等噪声目录，可能有省略）：\n{map}\n\
需要查看更深层目录或文件内容时，再调用 list_dir / read_file。"
            ),
        });
    }
    // 项目长期记忆：工作区根目录 MDGA.md（对标 CLAUDE.md / AGENTS.md），每次请求注入，
    // 永不被上下文压缩冲掉，承载项目目标、规范与架构约定。
    if let Some(memory) = workspace_memory.filter(|m| !m.trim().is_empty()) {
        injected.push(ChatMessage {
            role: "system".to_string(),
            content: format!(
                "项目长期记忆（来自工作区根目录的 MDGA.md，跨会话持久有效，优先遵循其中的目标与约定）：\n{memory}"
            ),
        });
    }
    // 技能列表（渐进披露）：只注入名称与描述，完整说明由模型按需调用 load_skill 加载。
    if !skills.is_empty() {
        let list = skills
            .iter()
            .map(|(name, desc)| format!("- {name}：{desc}"))
            .collect::<Vec<_>>()
            .join("\n");
        injected.push(ChatMessage {
            role: "system".to_string(),
            content: format!(
                "当前工作区可用技能（来自 .mdga/skills/）。当任务与某项技能匹配时，先调用 load_skill 加载其完整说明再执行：\n{list}"
            ),
        });
    }
    injected.extend(messages);
    injected
}

/// 读取工作区根目录的 MDGA.md 作为项目长期记忆；不存在或为空时返回 None。
/// 上限 16K 字符，防止超大记忆文件挤占上下文。
fn read_workspace_memory(workspace_root: &str) -> Option<String> {
    let path = std::path::Path::new(workspace_root).join("MDGA.md");
    let content = std::fs::read_to_string(path).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(16_000).collect())
}

/// 将前端权限模式字符串映射为后端枚举，未知值回退到最安全的 Restricted。
fn permission_mode_from_str(value: &str) -> PermissionMode {
    match value {
        "ask_every_time" => PermissionMode::AskEveryTime,
        "workspace_auto" => PermissionMode::WorkspaceAuto,
        "full_access" => PermissionMode::FullAccess,
        _ => PermissionMode::Restricted,
    }
}

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
use permissions::{
    execute_ask_user, feed_tool_denial, gate_tool_decision, request_tool_approval,
    tool_capability_for_name, ToolGate,
};

// ── 文件变更检查点与 diff ────────────────────────────────────────────────

mod checkpoint;
use checkpoint::{
    apply_checkpoint_revert, capture_checkpoint_before, persist_checkpoint, post_execution_diff,
};

// ── todo / 后台命令 / 子任务 ─────────────────────────────────────────────

// ── Hooks 生命周期系统 ──────────────────────────────────────────────────
//
mod hooks;
use hooks::{read_diagnostics_command, run_post_tool_hooks, run_pre_tool_hooks};

/// remember 工具：把一条值得跨会话记住的事实追加到工作区 MDGA.md 的「自动记忆」区。
///
/// 让 Agent 在工作中自主沉淀经验（项目约定、踩过的坑、关键路径），下次会话自动注入。
/// 去重：同样内容已存在则不重复追加。
fn execute_remember(workspace: &str, arguments: &str) -> Result<serde_json::Value, String> {
    const SECTION: &str = "## 自动记忆（由 Agent 维护）";
    let parsed: serde_json::Value =
        serde_json::from_str(arguments).map_err(|e| format!("工具参数解析失败: {e}"))?;
    let fact = parsed
        .get("fact")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("remember 缺少 fact")?;
    if fact.chars().count() > 500 {
        return Err("单条记忆过长（上限 500 字符），请精炼".to_string());
    }
    let path = std::path::Path::new(workspace).join("MDGA.md");
    let mut content = std::fs::read_to_string(&path).unwrap_or_default();
    let entry = format!("- {fact}");
    if content.contains(&entry) {
        return Ok(serde_json::json!({ "note": "该记忆已存在，未重复添加" }));
    }
    if content.contains(SECTION) {
        // 在 section 标题后插入
        content = content.replacen(SECTION, &format!("{SECTION}\n{entry}"), 1);
    } else {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!("\n{SECTION}\n{entry}\n"));
    }
    std::fs::write(&path, content).map_err(|e| format!("写入 MDGA.md 失败: {e}"))?;
    Ok(serde_json::json!({ "note": "已记入 MDGA.md，下次会话自动生效", "fact": fact }))
}

/// todo_write 工具：更新任务清单并推送给前端实时展示，不触碰文件系统。
fn execute_todo_write(app: &AppHandle, arguments: &str) -> Result<serde_json::Value, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(arguments).map_err(|e| format!("工具参数解析失败: {e}"))?;
    let items = parsed
        .get("items")
        .and_then(|v| v.as_array())
        .ok_or("todo_write 需要 items 数组")?;
    if items.len() > 50 {
        return Err("todo 项过多（上限 50）".to_string());
    }
    let _ = app.emit("todo-update", serde_json::Value::Array(items.clone()));
    Ok(serde_json::json!({
        "count": items.len(),
        "note": "任务清单已更新并实时展示给用户"
    }))
}

mod web;
use web::{execute_web_fetch, execute_web_search};

/// 可并行执行的只读工具集合（无副作用，并发安全）。
const PARALLEL_READONLY_TOOLS: &[&str] =
    &["read_file", "list_dir", "search_text", "glob_files", "stat_path", "web_fetch", "web_search"];

/// 执行一个只读工具调用（同步文件工具或异步 web 工具），供并行批量执行。
async fn execute_readonly_call(
    security_context: &SessionSecurityContext,
    tool_name: &str,
    arguments: &str,
) -> Result<serde_json::Value, String> {
    match tool_name {
        "web_fetch" => execute_web_fetch(arguments).await,
        "web_search" => execute_web_search(arguments).await,
        _ => execute_builtin_tool_call(security_context, tool_name, arguments),
    }
}

mod command_run;
use command_run::{execute_bg_shell_tool, execute_run_command_tool};

/// 子任务探索代理可用的只读工具集合。
fn read_only_tool_schemas() -> Vec<serde_json::Value> {
    const READ_ONLY: &[&str] = &["list_dir", "read_file", "search_text", "glob_files", "stat_path"];
    all_builtin_tool_schemas()
        .into_iter()
        .filter(|schema| {
            schema
                .pointer("/function/name")
                .and_then(|n| n.as_str())
                .map(|name| READ_ONLY.contains(&name))
                .unwrap_or(false)
        })
        .collect()
}

const SUBTASK_MAX_ROUNDS: usize = 15;

/// run_subtask 工具：用独立上下文跑一个只读探索子代理，返回简明报告与消耗的 usage。
/// 子代理只能 list/read/search/stat，工具事件以 `sub:` 前缀推给前端展示。
async fn execute_run_subtask(
    api_key: &str,
    model: &str,
    workspace_path: &str,
    arguments: &str,
    app: &AppHandle,
    conversation_id: &str,
) -> (Result<serde_json::Value, String>, Option<mdga_shared::RawUsage>) {
    let parsed: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(value) => value,
        Err(e) => return (Err(format!("工具参数解析失败: {e}")), None),
    };
    let Some(description) = parsed.get("description").and_then(|v| v.as_str()) else {
        return (Err("run_subtask 缺少 description".to_string()), None);
    };
    // 自定义子代理类型：agentType 指向 .mdga/agents/<type>.md，其内容作为子代理 system prompt。
    let custom_agent = parsed
        .get("agentType")
        .and_then(|v| v.as_str())
        .filter(|t| t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .and_then(|t| {
            std::fs::read_to_string(
                std::path::Path::new(workspace_path).join(".mdga").join("agents").join(format!("{t}.md")),
            )
            .ok()
        });
    let system_prompt = match custom_agent {
        Some(def) => format!(
            "你是一个只读探索子代理，工作区路径是 {workspace_path}。你只能使用 list_dir、read_file、search_text、glob_files、stat_path 这些只读工具，禁止写入或命令执行。以下是你的角色定义：\n{def}\n完成后输出简明中文报告。"
        ),
        None => format!(
            "你是一个只读探索子代理，工作区路径是 {workspace_path}。你只能使用 list_dir、read_file、search_text、glob_files、stat_path 这些只读工具调查代码与文件，禁止任何写入或命令执行。完成调查后，输出一份简明、信息密度高的中文报告，直接回答委托内容，不要寒暄。"
        ),
    };

    let background = parsed.get("background").and_then(|v| v.as_bool()).unwrap_or(false);
    if background {
        // 注册后台任务并 spawn 独立 loop；立即返回 taskId，主循环不等待。
        let task_id = format!("task-{}", BG_TASK_SEQ.fetch_add(1, Ordering::SeqCst));
        let report = Arc::new(Mutex::new(String::new()));
        let status = Arc::new(Mutex::new("running".to_string()));
        let usage_slot: Arc<Mutex<Option<mdga_shared::RawUsage>>> = Arc::new(Mutex::new(None));
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let st = app.state::<AppState>();
            let mut tasks = st.bg_tasks.lock().expect("bg_tasks mutex poisoned");
            tasks.insert(
                task_id.clone(),
                BgTask {
                    description: description.to_string(),
                    report: report.clone(),
                    status: status.clone(),
                    usage: usage_slot.clone(),
                    settled: Arc::new(AtomicBool::new(false)),
                    cancel: cancel.clone(),
                },
            );
        }
        let api_key = api_key.to_string();
        let model = model.to_string();
        let workspace_owned = workspace_path.to_string();
        let conversation_owned = conversation_id.to_string();
        let description_owned = description.to_string();
        let app_bg = app.clone();
        let task_id_done = task_id.clone();
        tauri::async_runtime::spawn(async move {
            let (text, sub_usage) = run_subtask_loop(
                &api_key,
                &model,
                &workspace_owned,
                &system_prompt,
                &description_owned,
                &app_bg,
                &conversation_owned,
                Some(cancel.clone()),
            )
            .await;
            let final_status = if cancel.load(Ordering::SeqCst) { "killed" } else { "done" };
            if let Ok(mut r) = report.lock() {
                *r = text;
            }
            if let Ok(mut u) = usage_slot.lock() {
                *u = sub_usage;
            }
            if let Ok(mut s) = status.lock() {
                *s = final_status.to_string();
            }
            let _ = app_bg.emit(
                "background-task-done",
                serde_json::json!({ "taskId": task_id_done, "status": final_status }),
            );
        });
        return (
            Ok(serde_json::json!({
                "background": true,
                "taskId": task_id,
                "note": "子代理已在后台启动。用 get_task_output 轮询报告/状态、kill_task 终止；你无需等待，继续后续步骤。"
            })),
            None,
        );
    }

    // 前台：同步等待子代理 loop 完成，usage 并入当轮账本。
    let (report, usage) = run_subtask_loop(
        api_key,
        model,
        workspace_path,
        &system_prompt,
        description,
        app,
        conversation_id,
        None,
    )
    .await;
    (Ok(serde_json::json!({ "report": report })), usage)
}

/// 子代理工具循环主体（前台/后台共用）。后台模式下每轮检查 cancel 以支持 kill_task。
#[allow(clippy::too_many_arguments)]
async fn run_subtask_loop(
    api_key: &str,
    model: &str,
    workspace_path: &str,
    system_prompt: &str,
    description: &str,
    app: &AppHandle,
    conversation_id: &str,
    cancel: Option<Arc<AtomicBool>>,
) -> (String, Option<mdga_shared::RawUsage>) {
    let security_context = match session_security_context(
        workspace_path.to_string(),
        PermissionMode::Restricted,
        NetworkMode::Disabled,
    ) {
        Ok(ctx) => ctx,
        Err(e) => return (format!("子代理初始化失败: {e}"), None),
    };

    let mut wire = vec![
        serde_json::json!({ "role": "system", "content": system_prompt }),
        serde_json::json!({ "role": "user", "content": description }),
    ];
    let mut usage: Option<mdga_shared::RawUsage> = None;
    let mut report = String::new();

    for _ in 0..SUBTASK_MAX_ROUNDS {
        if cancel.as_ref().map(|c| c.load(Ordering::SeqCst)).unwrap_or(false) {
            break;
        }
        let completion = match chat_completion_with_retry(
            api_key,
            wire.clone(),
            model,
            Some(read_only_tool_schemas()),
            app,
        )
        .await
        {
            Ok(result) => result,
            Err(e) => return (format!("子代理出错: {e}"), usage),
        };
        usage = merge_usage(usage, completion.usage.clone());

        let tool_calls = if completion.tool_calls.is_empty() {
            completion
                .content
                .as_deref()
                .map(recover_tool_calls_from_content)
                .unwrap_or_default()
        } else {
            completion.tool_calls.clone()
        };

        if tool_calls.is_empty() {
            report = completion
                .content
                .as_deref()
                .map(strip_dsml_markup)
                .unwrap_or_default();
            break;
        }

        wire.push(assistant_message_for_tool_calls(
            completion.assistant_message.clone(),
            &tool_calls,
        ));
        for call in &tool_calls {
            let name = call.function.name.as_str();
            let display_name = format!("sub:{name}");
            record_tool_event(
                app,
                conversation_id,
                "tool_started",
                &display_name,
                "running",
                &call.function.arguments,
                None,
                None,
                workspace_path,
            );
            let result = if matches!(
                name,
                "list_dir" | "read_file" | "search_text" | "glob_files" | "stat_path"
            ) {
                execute_builtin_tool_call(&security_context, name, &call.function.arguments)
            } else {
                Err("子任务仅允许只读工具".to_string())
            };
            let (output, status, error) = match &result {
                Ok(value) => (
                    serde_json::json!({ "ok": true, "result": value }),
                    "succeeded",
                    None,
                ),
                Err(message) => (
                    serde_json::json!({ "ok": false, "error": message }),
                    "failed",
                    Some(message.clone()),
                ),
            };
            let output_str = output.to_string();
            record_tool_event(
                app,
                conversation_id,
                if status == "succeeded" { "tool_succeeded" } else { "tool_failed" },
                &display_name,
                status,
                &call.function.arguments,
                Some(&output_str),
                error.as_deref(),
                workspace_path,
            );
            wire.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": output_str
            }));
        }
    }

    if report.trim().is_empty() {
        report = "（子任务在限定轮数内未给出报告）".to_string();
    }
    let capped: String = report.chars().take(8_000).collect();
    (capped, usage)
}

/// 后台子代理任务工具：get_task_output / kill_task / list_tasks。
/// get_task_output 在任务完成且未结算时返回其 usage，供主循环并入会话账本（只结算一次）。
async fn execute_bg_task_tool(
    app: &AppHandle,
    tool_name: &str,
    arguments: &str,
) -> (Result<serde_json::Value, String>, Option<mdga_shared::RawUsage>) {
    let parsed: serde_json::Value = serde_json::from_str(arguments).unwrap_or(serde_json::json!({}));
    match tool_name {
        "list_tasks" => {
            let st = app.state::<AppState>();
            let list: Vec<serde_json::Value> = match st.bg_tasks.lock() {
                Ok(tasks) => tasks
                    .iter()
                    .map(|(id, t)| {
                        serde_json::json!({
                            "taskId": id,
                            "description": t.description,
                            "status": t.status.lock().map(|s| s.clone()).unwrap_or_default(),
                        })
                    })
                    .collect(),
                Err(_) => Vec::new(),
            };
            (Ok(serde_json::json!({ "tasks": list })), None)
        }
        "kill_task" => {
            let Some(id) = parsed.get("taskId").and_then(|v| v.as_str()) else {
                return (Err("kill_task 缺少 taskId".to_string()), None);
            };
            let st = app.state::<AppState>();
            let cancel = st.bg_tasks.lock().ok().and_then(|t| t.get(id).map(|t| t.cancel.clone()));
            match cancel {
                Some(cancel) => {
                    cancel.store(true, Ordering::SeqCst);
                    (
                        Ok(serde_json::json!({ "taskId": id, "note": "已请求终止该后台子代理" })),
                        None,
                    )
                }
                None => (Err(format!("taskId 不存在: {id}")), None),
            }
        }
        "get_task_output" => {
            let Some(id) = parsed.get("taskId").and_then(|v| v.as_str()) else {
                return (Err("get_task_output 缺少 taskId".to_string()), None);
            };
            let block = parsed.get("block").and_then(|v| v.as_bool()).unwrap_or(false);
            let timeout_secs = parsed
                .get("timeoutSecs")
                .and_then(|v| v.as_u64())
                .unwrap_or(30)
                .min(120);

            let task = {
                let st = app.state::<AppState>();
                st.bg_tasks.lock().ok().and_then(|t| t.get(id).cloned())
            };
            let Some(task) = task else {
                return (Err(format!("taskId 不存在: {id}")), None);
            };

            if block {
                let deadline_ms = timeout_secs * 1000;
                let mut waited = 0u64;
                loop {
                    let running = task.status.lock().map(|s| *s == "running").unwrap_or(false);
                    if !running || waited >= deadline_ms {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    waited += 500;
                }
            }

            let status = task.status.lock().map(|s| s.clone()).unwrap_or_default();
            let report = task.report.lock().map(|r| r.clone()).unwrap_or_default();
            // 完成且未结算：取出 usage 并入账本（swap 保证只结算一次）。
            let settled_usage = if status != "running" && !task.settled.swap(true, Ordering::SeqCst) {
                task.usage.lock().ok().and_then(|u| u.clone())
            } else {
                None
            };
            (
                Ok(serde_json::json!({ "taskId": id, "status": status, "report": report })),
                settled_usage,
            )
        }
        other => (Err(format!("未知后台任务工具: {other}")), None),
    }
}

mod compaction;
use compaction::{
    compact_tool_outputs, context_soft_limit_tokens, maybe_persist_large_output,
    summarize_wire_history,
};

mod chat;
use chat::{
    assistant_message_for_tool_calls, chat_completion_with_retry, chat_messages_to_wire,
    recover_tool_calls_from_content, stream_round_with_retry,
};

/// Agent 工具循环：每轮带工具问模型、执行返回的工具、把结果回灌，直到模型不再调用工具
/// （自然终止）或用户中断。不设轮数上限——上下文自动压缩兜底体积，取消按钮兜底失控；
/// 所有工具执行前都经 SessionSecurityContext 裁决。
#[allow(clippy::too_many_arguments)]
async fn chat_with_builtin_tools(
    api_key: &str,
    messages: Vec<ChatMessage>,
    model: &str,
    workspace_path: &str,
    permission_mode: PermissionMode,
    conversation_id: &str,
    app: &AppHandle,
    cancel: Arc<AtomicBool>,
    permission_rules: Vec<String>,
    mcp_bindings: Vec<McpBinding>,
) -> Result<Option<mdga_shared::RawUsage>, String> {
    let security_context = session_security_context(
        workspace_path.to_string(),
        permission_mode,
        NetworkMode::Disabled,
    )
    .map_err(|e| e.to_string())?;
    // 工具 schema：Built-in + 已连接 MCP server 的外部工具。
    let tool_schemas: Vec<serde_json::Value> = all_builtin_tool_schemas()
        .into_iter()
        .chain(mcp_bindings.iter().map(|b| b.schema.clone()))
        .collect();
    let mut wire_messages = chat_messages_to_wire(messages);
    let mut usage: Option<mdga_shared::RawUsage> = None;
    // 上一次响应返回的 prompt_tokens，作为当前上下文体积的真实信号，驱动轮内压缩。
    let mut last_prompt_tokens: u64 = 0;
    // 诊断反馈环：记录本轮是否发生文件改动 + 是否已跑过诊断（最多一轮，防循环）。
    let mut edits_made = false;
    let mut diagnostics_ran = false;

    let mut round: usize = 0;
    loop {
        round += 1;
        // 轮次之间检查取消：用户点击停止后安全收尾，保留已执行的工具结果。
        if cancel.load(Ordering::SeqCst) {
            let _ = app.emit("chat-chunk", "\n\n（已中断）".to_string());
            return Ok(usage);
        }

        // Steering：取出用户在运行中排队的插话，作为 user 消息注入本轮，让模型即时纠偏。
        let steering_msgs: Vec<String> = {
            let state = app.state::<AppState>();
            state
                .steering
                .lock()
                .ok()
                .and_then(|mut map| map.get_mut(conversation_id).map(std::mem::take))
                .unwrap_or_default()
        };
        for msg in steering_msgs {
            wire_messages.push(serde_json::json!({ "role": "user", "content": msg }));
            let _ = app.emit("steering-injected", &msg);
        }

        // 两级上下文压缩：超软上限先把较早的大体积工具结果换成短桩（机械、零成本）；
        // 若已无桩可压仍超限，触发摘要压缩（auto-compact），把旧历史压成任务进度摘要。
        let soft_limit = context_soft_limit_tokens();
        if last_prompt_tokens > soft_limit {
            let compacted = compact_tool_outputs(
                &mut wire_messages,
                KEEP_RECENT_TOOL_RESULTS,
                TOOL_RESULT_STUB_THRESHOLD,
            );
            if compacted > 0 {
                let _ = app.emit(
                    "context-compacted",
                    serde_json::json!({ "kind": "stub", "count": compacted }),
                );
            } else {
                let _ = app.emit("agent-status", serde_json::json!({ "state": "compacting" }));
                let (new_wire, summary_usage) =
                    summarize_wire_history(api_key, model, std::mem::take(&mut wire_messages), app)
                        .await?;
                wire_messages = new_wire;
                usage = merge_usage(usage, summary_usage);
                // 重置体积信号，待下一次响应的真实 usage 刷新，避免连续重复触发。
                last_prompt_tokens = 0;
                let _ = app.emit(
                    "context-compacted",
                    serde_json::json!({ "kind": "summary" }),
                );
            }
        }

        // 推送轮次进度与思考状态，让前端展示「第 N 轮 · 思考中」而非黑盒等待。
        let _ = app.emit("agent-round", round);
        let _ = app.emit(
            "agent-status",
            serde_json::json!({ "state": "thinking", "round": round }),
        );

        // 流式获取本轮结果：叙述 token 边流边显（内置标记防泄漏守卫），同时累积 tool_calls。
        let completion =
            stream_round_with_retry(api_key, wire_messages.clone(), model, tool_schemas.clone(), app)
                .await?;
        usage = merge_usage(usage, completion.usage.clone());
        // 成本预算：累计 total_tokens 超过预算则暂停（防失控烧 token）。
        let budget = app.state::<AppState>().task_token_budget.load(Ordering::SeqCst);
        if budget > 0 {
            if let Some(u) = usage.as_ref() {
                if u.total_tokens >= budget {
                    let _ = app.emit(
                        "chat-chunk",
                        format!(
                            "\n\n（已达本次任务 token 预算 {budget}，已暂停以控制费用。如需继续，请回复\"继续\"或在设置中调高预算。）"
                        ),
                    );
                    return Ok(usage);
                }
            }
        }
        if let Some(round_usage) = completion.usage.as_ref() {
            last_prompt_tokens = round_usage.prompt_tokens;
            // 推送上下文用量，前端常驻显示占用百分比（对标 CC/Codex 的 context 指示器）。
            let _ = app.emit(
                "context-usage",
                serde_json::json!({
                    "promptTokens": round_usage.prompt_tokens,
                    "softLimit": soft_limit
                }),
            );
        }

        // tool_calls 优先取结构化（流式 delta 累积），为空时从正文兜底解析 DSML / <ToolCall> 变体。
        // 叙述内容已在流式过程中实时外显（守卫防标记泄漏），此处不再重复 emit。
        let tool_calls = if !completion.tool_calls.is_empty() {
            completion.tool_calls.clone()
        } else {
            completion
                .content
                .as_deref()
                .map(recover_tool_calls_from_content)
                .unwrap_or_default()
        };

        // 模型不再调用工具：本轮叙述即最终回复。收尾前若发生过改动且配置了诊断命令，
        // 自动跑一次（typecheck/lint）；有错则回灌让 agent 修复后再收尾（最多一轮）。
        if tool_calls.is_empty() {
            if edits_made && !diagnostics_ran {
                if let Some(cmd) = read_diagnostics_command(workspace_path) {
                    diagnostics_ran = true;
                    let _ = app.emit("agent-status", serde_json::json!({ "state": "thinking", "round": round }));
                    let _ = app.emit("chat-chunk", "\n\n（正在运行诊断检查…）\n\n".to_string());
                    if let Ok(result) = mdga_tool_runtime::run_command(
                        workspace_path,
                        RunCommandRequest { command: cmd.clone(), timeout_secs: Some(180), background: false },
                    ) {
                        let failed = result.exit_code.unwrap_or(0) != 0 || result.timed_out;
                        if failed {
                            let out: String = format!("{}\n{}", result.stdout, result.stderr)
                                .chars().take(6000).collect();
                            wire_messages.push(serde_json::json!({
                                "role": "user",
                                "content": format!("诊断命令 `{cmd}` 报告了问题，请修复后再结束：\n{out}")
                            }));
                            continue; // 回到循环让 agent 修
                        }
                    }
                }
            }
            return Ok(usage);
        }

        wire_messages.push(assistant_message_for_tool_calls(
            completion.assistant_message,
            &tool_calls,
        ));

        // 并行快路径：当本轮多个调用全部是「自动放行的只读工具」时并发执行（读多文件 / 抓多 URL 提速）。
        let all_parallel_readonly = tool_calls.len() > 1
            && tool_calls.iter().all(|call| {
                PARALLEL_READONLY_TOOLS.contains(&call.function.name.as_str())
                    && matches!(
                        gate_tool_decision(
                            &security_context,
                            &call.function.name,
                            &call.function.arguments,
                            &permission_rules,
                        ),
                        ToolGate::Allow
                    )
            });
        if all_parallel_readonly {
            if cancel.load(Ordering::SeqCst) {
                let _ = app.emit("chat-chunk", "\n\n（已中断）".to_string());
                return Ok(usage);
            }
            // 先发运行事件（卡片同时出现），再并发执行。
            for call in &tool_calls {
                record_tool_event(
                    app, conversation_id, "tool_started", &call.function.name, "running",
                    &call.function.arguments, None, None, workspace_path,
                );
            }
            let results = futures_util::future::join_all(tool_calls.iter().map(|call| {
                let sec = &security_context;
                async move {
                    execute_readonly_call(sec, &call.function.name, &call.function.arguments).await
                }
            }))
            .await;
            for (call, result) in tool_calls.iter().zip(results.into_iter()) {
                let (output, status, error) = match &result {
                    Ok(value) => (serde_json::json!({ "ok": true, "result": value }), "succeeded", None),
                    Err(message) => (
                        serde_json::json!({ "ok": false, "error": message, "hint": "工具执行失败，请阅读 error 调整后重试或换用其他工具。" }),
                        "failed",
                        Some(message.clone()),
                    ),
                };
                let output_str = output.to_string();
                record_tool_event(
                    app, conversation_id,
                    if status == "succeeded" { "tool_succeeded" } else { "tool_failed" },
                    &call.function.name, status, &call.function.arguments,
                    Some(&output_str), error.as_deref(), workspace_path,
                );
                wire_messages.push(serde_json::json!({
                    "role": "tool", "tool_call_id": call.id, "content": maybe_persist_large_output(workspace_path, &output_str)
                }));
            }
            continue;
        }

        for call in tool_calls {
            // 工具执行前检查取消：避免停止后仍继续执行剩余工具。
            if cancel.load(Ordering::SeqCst) {
                let _ = app.emit("chat-chunk", "\n\n（已中断）".to_string());
                return Ok(usage);
            }
            let tool_name = call.function.name.clone();
            let arguments = call.function.arguments.clone();

            // 权限门控：白名单命令与「总是允许」规则直接放行，否则按权限模式放行 / 审批 / 拒绝。
            let decision =
                gate_tool_decision(&security_context, &tool_name, &arguments, &permission_rules);
            let proceed = match decision {
                ToolGate::Allow => true,
                ToolGate::Deny(reason) => {
                    feed_tool_denial(
                        app,
                        conversation_id,
                        &tool_name,
                        &arguments,
                        workspace_path,
                        &reason,
                        &call.id,
                        &mut wire_messages,
                    );
                    false
                }
                ToolGate::Ask => {
                    let approved =
                        request_tool_approval(app, &tool_name, &arguments).await;
                    if !approved {
                        feed_tool_denial(
                            app,
                            conversation_id,
                            &tool_name,
                            &arguments,
                            workspace_path,
                            "用户拒绝了该操作",
                            &call.id,
                            &mut wire_messages,
                        );
                    }
                    approved
                }
            };
            if !proceed {
                continue;
            }

            // PreToolUse 钩子：用户定义的执行前校验，退出码非 0 则阻断（原因回灌模型）。
            if let Some(reason) = run_pre_tool_hooks(workspace_path, &tool_name, &arguments) {
                feed_tool_denial(
                    app, conversation_id, &tool_name, &arguments, workspace_path,
                    &reason, &call.id, &mut wire_messages,
                );
                continue;
            }

            record_tool_event(
                app,
                conversation_id,
                "tool_started",
                &tool_name,
                "running",
                &arguments,
                None,
                None,
                workspace_path,
            );

            // 写类工具执行前捕获回退快照（rewind 用），必须先于执行。
            let capture = capture_checkpoint_before(workspace_path, &tool_name, &arguments);

            // 特殊工具走专用执行器：MCP 外部工具 / todo / 技能 / 子任务 / 命令（流式 + 后台）。
            let mcp_binding = mcp_bindings.iter().find(|b| b.fn_name == tool_name);
            let result = if let Some(binding) = mcp_binding {
                execute_mcp_tool(app, binding, &arguments)
            } else {
                match tool_name.as_str() {
                "todo_write" => execute_todo_write(app, &arguments),
                "ask_user" => execute_ask_user(app, &arguments).await,
                "load_skill" => execute_load_skill(workspace_path, &arguments),
                "remember" => execute_remember(workspace_path, &arguments),
                "add_mcp_server" => execute_add_mcp_server(app, &arguments),
                "web_fetch" => execute_web_fetch(&arguments).await,
                "web_search" => execute_web_search(&arguments).await,
                "list_shells" | "get_shell_output" | "kill_shell" => {
                    execute_bg_shell_tool(app, &tool_name, &arguments)
                }
                "run_command" => execute_run_command_tool(app, &security_context, &arguments),
                "run_subtask" => {
                    let _ = app.emit(
                        "agent-status",
                        serde_json::json!({ "state": "thinking", "round": round }),
                    );
                    let (sub_result, sub_usage) = execute_run_subtask(
                        api_key,
                        model,
                        workspace_path,
                        &arguments,
                        app,
                        conversation_id,
                    )
                    .await;
                    usage = merge_usage(usage, sub_usage);
                    sub_result
                }
                "list_mcp_resources" | "read_mcp_resource" => {
                    execute_mcp_resource_tool(app, &tool_name, &arguments)
                }
                "get_task_output" | "kill_task" | "list_tasks" => {
                    let (task_result, task_usage) =
                        execute_bg_task_tool(app, &tool_name, &arguments).await;
                    usage = merge_usage(usage, task_usage);
                    task_result
                }
                _ => execute_builtin_tool_call(&security_context, &tool_name, &arguments),
                }
            };

            let (output, status, error) = match &result {
                Ok(value) => {
                    let mut out = serde_json::json!({ "ok": true, "result": value });
                    // 标记本轮发生过文件改动（驱动收尾前的诊断检查）。
                    if capture.is_some() {
                        edits_made = true;
                    }
                    // 文本写类工具：附加行级 diff 供 UI 展示，并把回退快照落库。
                    if let Some(cap) = capture.as_ref() {
                        if let Some((diff, added, removed)) =
                            post_execution_diff(workspace_path, &tool_name, &arguments, cap)
                        {
                            out["diff"] = serde_json::Value::String(diff);
                            out["added"] = serde_json::json!(added);
                            out["removed"] = serde_json::json!(removed);
                        }
                        persist_checkpoint(app, conversation_id, &tool_name, cap);
                    }
                    (out, "succeeded", None)
                }
                Err(message) => (
                    serde_json::json!({
                        "ok": false,
                        "error": message,
                        "hint": "工具执行失败。请阅读 error 判断是参数错误、路径不存在还是命令/环境问题，据此调整后重试或改用其他工具/写法；不要原样重复同一次失败的调用。"
                    }),
                    "failed",
                    Some(message.clone()),
                ),
            };
            let output_str = output.to_string();
            record_tool_event(
                app,
                conversation_id,
                if status == "succeeded" { "tool_succeeded" } else { "tool_failed" },
                &tool_name,
                status,
                &arguments,
                Some(&output_str),
                error.as_deref(),
                workspace_path,
            );

            wire_messages.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": maybe_persist_large_output(workspace_path, &output_str)
            }));

            // PostToolUse 钩子：工具成功后运行用户定义的后处理（如自动格式化 / 跑测试），信息性不阻断。
            if status == "succeeded" {
                run_post_tool_hooks(app, workspace_path, &tool_name, &arguments);
            }
        }
    }
}

fn all_builtin_tool_schemas() -> Vec<serde_json::Value> {
    vec![
        file_tool_schema(
            "create_file",
            "Create a new text file inside the current MDGA conversation workspace. Use this when the user asks to create a file. The path must be relative to the workspace.",
            &["path", "content"],
        ),
        file_tool_schema(
            "write_file",
            "Write or overwrite a full UTF-8 text file inside the current MDGA conversation workspace. Use this only when the user asks to replace the whole file.",
            &["path", "content"],
        ),
        file_tool_schema(
            "edit_file",
            "Edit an existing UTF-8 text file by replacing oldText with newText. Prefer this for code or text modifications.",
            &["path", "oldText", "newText"],
        ),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a UTF-8 text file inside the workspace. Returns up to ~1500 lines by default with total line count and has_more. For large files, page through with offset (0-based start line) and limit (lines).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path inside the workspace." },
                        "offset": { "type": "integer", "description": "0-based start line. Use with has_more to page large files. Default 0." },
                        "limit": { "type": "integer", "description": "Max lines to return (<= 4000). Default ~1500." }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        }),
        file_tool_schema(
            "delete_file",
            "Delete a single file inside the current MDGA conversation workspace. Use this when the user asks to remove a file.",
            &["path"],
        ),
        file_tool_schema(
            "list_dir",
            "List entries in a directory inside the current MDGA conversation workspace. Use this when the user asks what files or folders exist.",
            &["path"],
        ),
        file_tool_schema(
            "make_dir",
            "Create a directory inside the current MDGA conversation workspace.",
            &["path"],
        ),
        file_tool_schema(
            "stat_path",
            "Return whether a relative workspace path exists and whether it is a file or directory.",
            &["path"],
        ),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "search_text",
                "description": "Search file CONTENTS recursively inside a workspace directory (ripgrep-style, gitignore-aware, skips hidden/ignored files). Use this to find where text/code appears. For finding files by NAME/path pattern, use glob_files instead.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative directory to search in (e.g. \".\" for workspace root)." },
                        "query": { "type": "string", "description": "Search pattern (literal substring, or regex when isRegex=true)." },
                        "isRegex": { "type": "boolean", "description": "Interpret query as a regular expression. Default false." },
                        "outputMode": { "type": "string", "enum": ["content", "files_with_matches", "count"], "description": "content = matching lines (default); files_with_matches = just file paths; count = matches per file." },
                        "caseInsensitive": { "type": "boolean", "description": "Case-insensitive match (-i). Default false." },
                        "multiline": { "type": "boolean", "description": "Allow the pattern to span lines (. matches newlines). Default false." },
                        "context": { "type": "integer", "description": "Lines of context before AND after each match (-C). content mode only." },
                        "beforeContext": { "type": "integer", "description": "Lines of context before each match (-B). content mode only." },
                        "afterContext": { "type": "integer", "description": "Lines of context after each match (-A). content mode only." },
                        "fileType": { "type": "string", "description": "Restrict to a file type, e.g. \"rs\", \"ts\", \"py\", \"json\"." },
                        "glob": { "type": "string", "description": "Restrict to files whose name/path matches this glob, e.g. \"*.rs\" or \"src/**\"." },
                        "maxResults": { "type": "integer", "description": "Cap on returned matches/files/counts." }
                    },
                    "required": ["path", "query"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "glob_files",
                "description": "Find files by NAME/path glob pattern inside the workspace (gitignore-aware), returned newest-first. Use this to locate files (e.g. all \"*.rs\", everything under \"src/**\") rather than searching their contents.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Glob: supports * ? and ** (e.g. \"**/*.rs\", \"src/**\", \"*.toml\"). A pattern without \"/\" matches by file name in any directory." },
                        "path": { "type": "string", "description": "Relative directory to start from. Defaults to workspace root." },
                        "maxResults": { "type": "integer", "description": "Cap on returned file paths." }
                    },
                    "required": ["pattern"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "move_path",
                "description": "Move or rename a file or directory inside the current MDGA conversation workspace. Use this for moving/renaming instead of create+delete.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "from": { "type": "string", "description": "Existing relative source path inside the workspace." },
                        "to": { "type": "string", "description": "New relative destination path inside the workspace. Must not already exist." }
                    },
                    "required": ["from", "to"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "delete_dir",
                "description": "Delete a directory inside the current MDGA conversation workspace. Set recursive=true to delete a non-empty directory.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative directory path inside the workspace. Cannot be the workspace root." },
                        "recursive": { "type": "boolean", "description": "Delete recursively including contents. Required true for non-empty directories. Defaults to false." }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "run_command",
                "description": "Run a single PowerShell command in the workspace directory. Low-risk commands (cargo check/test, npm test, git status/diff, dir) run directly under Workspace Auto; others need Full Access or user approval. Set background=true for long-running commands (servers, watchers): it returns immediately and the result is reported later.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "The PowerShell command line to execute." },
                        "timeoutSecs": { "type": "integer", "description": "Optional timeout in seconds, default 120, max 600." },
                        "background": { "type": "boolean", "description": "Run in background and return immediately. Defaults to false." }
                    },
                    "required": ["command"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_shell_output",
                "description": "Poll a background shell's accumulated output and status (running/done/killed/error). Use with the shellId returned by run_command background=true.",
                "parameters": { "type": "object", "properties": { "shellId": { "type": "string" } }, "required": ["shellId"], "additionalProperties": false }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "kill_shell",
                "description": "Terminate a running background shell by shellId.",
                "parameters": { "type": "object", "properties": { "shellId": { "type": "string" } }, "required": ["shellId"], "additionalProperties": false }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "list_shells",
                "description": "List all background shells with their id, command and status.",
                "parameters": { "type": "object", "properties": {}, "additionalProperties": false }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "todo_write",
                "description": "Maintain a visible todo list for the current multi-step task. Call this at the start of a complex task to plan steps, and update statuses as you progress (exactly one item in_progress at a time). The list is shown to the user as live progress.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "items": {
                            "type": "array",
                            "description": "The full todo list, replacing the previous one.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "text": { "type": "string", "description": "Short description of the step." },
                                    "status": { "type": "string", "enum": ["pending", "in_progress", "done"], "description": "Current status of this step." }
                                },
                                "required": ["text", "status"]
                            }
                        }
                    },
                    "required": ["items"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "ask_user",
                "description": "Ask the user 1-4 structured multiple-choice questions when requirements are genuinely ambiguous and guessing would risk doing the wrong work. The UI renders clickable option cards; an 'Other' free-text choice is always added automatically, and questions can allow multiple selections. Prefer this over assuming. Do NOT use it for anything you can determine yourself by reading files or running tools — only for real decisions that are the user's to make.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "questions": {
                            "type": "array",
                            "description": "1 to 4 questions to ask at once.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "question": { "type": "string", "description": "The full, specific question, ending with a question mark." },
                                    "header": { "type": "string", "description": "Very short label (<= 12 chars) shown as a chip, e.g. 'Library', 'Approach'." },
                                    "multiSelect": { "type": "boolean", "description": "Allow selecting multiple options. Defaults to false." },
                                    "options": {
                                        "type": "array",
                                        "description": "2 to 4 mutually-exclusive choices ('Other' is added automatically; do not add it yourself).",
                                        "items": {
                                            "type": "object",
                                            "properties": {
                                                "label": { "type": "string", "description": "Concise option text (1-5 words)." },
                                                "description": { "type": "string", "description": "What this option means or implies (trade-offs)." }
                                            },
                                            "required": ["label"]
                                        }
                                    }
                                },
                                "required": ["question", "options"]
                            }
                        }
                    },
                    "required": ["questions"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "load_skill",
                "description": "Load the full instructions of a workspace skill (from .mdga/skills/<name>/SKILL.md). Call this when the available-skills list (in system context) has a skill matching the current task.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Skill directory name from the available-skills list." }
                    },
                    "required": ["name"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "web_fetch",
                "description": "Fetch a web page or document by URL and return its readable text content. Use this to read documentation, articles, error references, or any known URL. http/https only.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "The http/https URL to fetch." }
                    },
                    "required": ["url"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web and get a list of result titles, URLs and snippets. Use this when you need to find current information, documentation, or solutions you don't already know. Follow up with web_fetch on the most relevant URLs.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "The search query." }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "add_mcp_server",
                "description": "Register and connect an MCP server in MDGA's real mechanism (not by editing config files). Use this when the user asks you to install/add an MCP server for yourself. command is either a stdio launch command (e.g. 'npx -y @modelcontextprotocol/server-memory') or an http(s):// URL. After it connects, its tools become callable as mcp_<server>_<tool>.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Short server name, e.g. memory, github." },
                        "command": { "type": "string", "description": "stdio launch command or http(s):// URL." },
                        "authToken": { "type": "string", "description": "Optional Bearer token for HTTP servers." }
                    },
                    "required": ["name", "command"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "remember",
                "description": "Persist a concise fact worth remembering across sessions (project convention, a gotcha you hit, a key file path). It is appended to the workspace MDGA.md and auto-injected in future sessions. Use sparingly for durable, reusable facts — not transient details.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "fact": { "type": "string", "description": "One concise fact to remember (<= 500 chars)." }
                    },
                    "required": ["fact"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "run_subtask",
                "description": "Delegate a focused READ-ONLY exploration subtask (e.g. 'find where X is implemented', 'summarize how module Y works') to a sub-agent with its own fresh context. The sub-agent can only list/read/search files and returns a concise text report. Use this to investigate large codebases without bloating the main conversation. Optionally set agentType to use a custom agent role from .mdga/agents/<type>.md. Set background=true to run it asynchronously: it returns a taskId immediately so you can keep working; poll progress with get_task_output and stop it with kill_task.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "description": { "type": "string", "description": "Clear, self-contained description of what to investigate and what the report should contain." },
                        "agentType": { "type": "string", "description": "Optional custom agent type name (loads .mdga/agents/<type>.md as the sub-agent role)." },
                        "background": { "type": "boolean", "description": "Run asynchronously: return a taskId immediately instead of blocking. Poll with get_task_output, stop with kill_task. Default false." }
                    },
                    "required": ["description"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_task_output",
                "description": "Poll a background sub-agent task (started by run_subtask background=true) for its accumulated report and status (running/done/killed/error). Set block=true to wait until it finishes or timeoutSecs elapses.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "taskId": { "type": "string", "description": "The taskId returned by run_subtask background=true." },
                        "block": { "type": "boolean", "description": "Wait for completion (up to timeoutSecs) instead of returning immediately. Default false." },
                        "timeoutSecs": { "type": "integer", "description": "Max seconds to wait when block=true. Default 30, max 120." }
                    },
                    "required": ["taskId"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "kill_task",
                "description": "Stop a running background sub-agent task by taskId.",
                "parameters": { "type": "object", "properties": { "taskId": { "type": "string" } }, "required": ["taskId"], "additionalProperties": false }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "list_tasks",
                "description": "List all background sub-agent tasks with their id, description and status.",
                "parameters": { "type": "object", "properties": {}, "additionalProperties": false }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "list_mcp_resources",
                "description": "List resources exposed by connected MCP servers (resources/list). Optionally filter by server name. Returns each resource's uri, name and mimeType.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "server": { "type": "string", "description": "Optional MCP server name to filter by; omit to list across all connected servers." }
                    },
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "read_mcp_resource",
                "description": "Read a resource from a connected MCP server (resources/read), returning its text content.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "server": { "type": "string", "description": "MCP server name that exposes the resource." },
                        "uri": { "type": "string", "description": "The resource URI to read (from list_mcp_resources)." }
                    },
                    "required": ["server", "uri"],
                    "additionalProperties": false
                }
            }
        }),
    ]
}

fn file_tool_schema(name: &str, description: &str, required: &[&str]) -> serde_json::Value {
    let mut properties = serde_json::json!({
        "path": {
            "type": "string",
            "description": "Relative path inside the current workspace. Use . for workspace root."
        }
    });
    if required.contains(&"content") {
        properties["content"] = serde_json::json!({
            "type": "string",
            "description": "UTF-8 text content to write. Use an empty string when the user asks for an empty file."
        });
    }
    if required.contains(&"oldText") {
        properties["oldText"] = serde_json::json!({
            "type": "string",
            "description": "Exact existing text to replace. It should be unique unless replaceAll is true."
        });
    }
    if required.contains(&"newText") {
        properties["newText"] = serde_json::json!({
            "type": "string",
            "description": "Replacement text."
        });
        properties["replaceAll"] = serde_json::json!({
            "type": "boolean",
            "description": "Replace every match. Defaults to false."
        });
    }
    if required.contains(&"query") {
        properties["query"] = serde_json::json!({
            "type": "string",
            "description": "Text or regex to search for. Search is gitignore-aware (skips ignored/hidden files)."
        });
        properties["maxResults"] = serde_json::json!({
            "type": "integer",
            "description": "Maximum number of matches to return, up to 50."
        });
        properties["isRegex"] = serde_json::json!({
            "type": "boolean",
            "description": "Treat query as a regular expression. Defaults to false (literal substring)."
        });
    }

    serde_json::json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false
            }
        }
    })
}

fn execute_builtin_tool_call(
    security_context: &SessionSecurityContext,
    tool_name: &str,
    arguments: &str,
) -> Result<serde_json::Value, String> {
    // 权限门控已在工具循环（gate_tool_decision）完成，这里只做工具名校验与真实执行。
    tool_capability_for_name(tool_name)?;
    let workspace_path = security_context.workspace_root.as_str();

    match tool_name {
        "create_file" => execute_create_file_tool_call(workspace_path, arguments),
        "write_file" => {
            let request = serde_json::from_str::<WriteFileRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(write_file(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "edit_file" => {
            let request = serde_json::from_str::<EditFileRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(edit_file(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "read_file" => {
            let request = serde_json::from_str::<ReadFileRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(read_file(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "delete_file" => {
            let request = serde_json::from_str::<DeleteFileRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(delete_file(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "list_dir" => {
            let request = serde_json::from_str::<ListDirRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(list_dir(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "make_dir" => {
            let request = serde_json::from_str::<MakeDirRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(make_dir(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "stat_path" => {
            let request = serde_json::from_str::<StatPathRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(stat_path(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "search_text" => {
            let request = serde_json::from_str::<SearchTextRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(search_text(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "glob_files" => {
            let request = serde_json::from_str::<GlobFilesRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(glob_files(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "move_path" => {
            let request = serde_json::from_str::<MovePathRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(move_path(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "delete_dir" => {
            let request = serde_json::from_str::<DeleteDirRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(delete_dir(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "run_command" => {
            let request = serde_json::from_str::<RunCommandRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(run_command(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        other => Err(format!("未知工具: {other}")),
    }
}

fn execute_create_file_tool_call(
    workspace_path: &str,
    arguments: &str,
) -> Result<serde_json::Value, String> {
    let request = serde_json::from_str::<CreateFileRequest>(arguments)
        .map_err(|err| format!("工具参数解析失败: {err}"))?;
    let result = create_file(workspace_path, request).map_err(|err| err.to_string())?;
    serde_json::to_value(result).map_err(|err| err.to_string())
}

fn merge_usage(
    first: Option<mdga_shared::RawUsage>,
    second: Option<mdga_shared::RawUsage>,
) -> Option<mdga_shared::RawUsage> {
    match (first, second) {
        (None, None) => None,
        (Some(usage), None) | (None, Some(usage)) => Some(usage),
        (Some(a), Some(b)) => Some(mdga_shared::RawUsage {
            prompt_tokens: a.prompt_tokens + b.prompt_tokens,
            completion_tokens: a.completion_tokens + b.completion_tokens,
            total_tokens: a.total_tokens + b.total_tokens,
            prompt_cache_hit_tokens: a.prompt_cache_hit_tokens + b.prompt_cache_hit_tokens,
            prompt_cache_miss_tokens: a.prompt_cache_miss_tokens + b.prompt_cache_miss_tokens,
            reasoning_tokens: a.reasoning_tokens + b.reasoning_tokens,
            raw_json: serde_json::json!([a.raw_json, b.raw_json]).to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepends_workspace_context_to_deepseek_messages() {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "你是否清楚现在所在的工作区路径是什么".to_string(),
        }];

        let injected = messages_with_workspace_context(
            messages,
            Some("C:\\workspace\\demo"),
            Some("MDGA"),
            None,
            None,
            &[],
        );

        // injected[0] 是 MDGA 身份锚定消息。
        assert_eq!(injected[0].role, "system");
        assert!(injected[0].content.contains("MDGA"));
        assert!(injected[0].content.contains(".mdga"));
        assert_eq!(injected[1].role, "system");
        assert!(injected[1].content.contains("C:\\workspace\\demo"));
        assert!(injected[1].content.contains("MDGA"));
        assert!(injected[1].content.contains("除非用户明确授权越界"));
        assert!(injected[1].content.contains("必须分别调用"));
        assert!(injected[1].content.contains("read_file"));
        assert!(injected[1].content.contains("write_file"));
        assert!(injected[1].content.contains("delete_file"));
        assert!(injected[1].content.contains("list_dir"));
        assert_eq!(injected[2].role, "system");
        assert!(injected[2].content.contains("edit_file"));
        assert!(injected[2].content.contains("search_text"));
        assert_eq!(injected[3].role, "user");
    }


    #[test]
    fn injects_repo_map_when_provided() {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "项目结构是什么".to_string(),
        }];

        let injected = messages_with_workspace_context(
            messages,
            Some("C:\\workspace\\demo"),
            Some("MDGA"),
            Some("src/\n  main.rs\nCargo.toml"),
            None,
            &[],
        );

        // sys(身份) + sys(workspace) + sys(tools) + sys(repo map) + user
        assert_eq!(injected.len(), 5);
        assert_eq!(injected[3].role, "system");
        assert!(injected[3].content.contains("工作区结构摘要"));
        assert!(injected[3].content.contains("main.rs"));
        assert_eq!(injected[4].role, "user");
    }

    #[test]
    fn injects_workspace_memory_when_provided() {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "继续开发".to_string(),
        }];

        let injected = messages_with_workspace_context(
            messages,
            Some("C:\\workspace\\demo"),
            Some("MDGA"),
            None,
            Some("项目目标：做一个计算器。代码规范：KISS。"),
            &[],
        );

        // sys(身份) + sys(workspace) + sys(tools) + sys(memory) + user
        assert_eq!(injected.len(), 5);
        assert_eq!(injected[3].role, "system");
        assert!(injected[3].content.contains("项目长期记忆"));
        assert!(injected[3].content.contains("做一个计算器"));
    }

    #[test]
    fn executes_create_file_tool_call_inside_workspace() {
        let workspace = std::env::temp_dir().join(format!(
            "mdga-desktop-tool-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time should be available")
                .as_nanos()
        ));
        std::fs::create_dir_all(&workspace).expect("workspace should be created");

        let output = execute_create_file_tool_call(
            workspace.to_str().expect("workspace should be utf8"),
            r#"{"path":"test.txt","content":""}"#,
        )
        .expect("tool call should execute");

        assert_eq!(output["relativePath"], "test.txt");
        assert!(workspace.join("test.txt").is_file());

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn executes_write_read_delete_and_list_tool_calls_inside_workspace() {
        let workspace = std::env::temp_dir().join(format!(
            "mdga-desktop-tool-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time should be available")
                .as_nanos()
        ));
        std::fs::create_dir_all(&workspace).expect("workspace should be created");

        let workspace_path = workspace.to_str().expect("workspace should be utf8");
        let security_context = session_security_context(
            workspace_path.to_string(),
            PermissionMode::WorkspaceAuto,
            NetworkMode::Disabled,
        )
        .expect("security context should build");
        execute_builtin_tool_call(
            &security_context,
            "write_file",
            r#"{"path":"note.txt","content":"123456"}"#,
        )
        .expect("write tool should execute");
        let read_output = execute_builtin_tool_call(
            &security_context,
            "read_file",
            r#"{"path":"note.txt"}"#,
        )
        .expect("read tool should execute");
        let list_output = execute_builtin_tool_call(
            &security_context,
            "list_dir",
            r#"{"path":"."}"#,
        )
        .expect("list tool should execute");
        execute_builtin_tool_call(
            &security_context,
            "edit_file",
            r#"{"path":"note.txt","oldText":"123456","newText":"abcdef"}"#,
        )
        .expect("edit tool should execute");
        execute_builtin_tool_call(
            &security_context,
            "make_dir",
            r#"{"path":"src"}"#,
        )
        .expect("mkdir tool should execute");
        let stat_output = execute_builtin_tool_call(
            &security_context,
            "stat_path",
            r#"{"path":"src"}"#,
        )
        .expect("stat tool should execute");
        let search_output = execute_builtin_tool_call(
            &security_context,
            "search_text",
            r#"{"path":".","query":"abcdef","maxResults":10}"#,
        )
        .expect("search tool should execute");
        execute_builtin_tool_call(
            &security_context,
            "delete_file",
            r#"{"path":"note.txt"}"#,
        )
        .expect("delete tool should execute");

        assert_eq!(read_output["content"], "123456");
        assert_eq!(list_output["entries"][0]["name"], "note.txt");
        assert_eq!(stat_output["kind"], "directory");
        assert_eq!(search_output["matches"][0]["path"], "note.txt");
        assert!(!workspace.join("note.txt").exists());

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn recovers_deepseek_dsml_tool_calls_from_text_content() {
        let content = r#"<｜DSML｜tool_calls><｜DSML｜invoke name="write_file"><｜DSML｜parameter name="path" string="true">\helloworld.txt</｜DSML｜parameter><｜DSML｜parameter name="content" string="true">123456</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls>"#;

        let calls = recover_tool_calls_from_content(content);

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "write_file");
        assert_eq!(calls[0].function.arguments, r#"{"content":"123456","path":"helloworld.txt"}"#);
    }
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
            get_conversations,
            load_messages,
            persist_message,
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
            get_conversation_events,
            cancel_agent,
            queue_steering,
            respond_approval,
            respond_ask_user,
            get_workspace,
            set_workspace_path,
            clear_workspace,
            check_update,
            install_update,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run MDGA desktop app");
}
