//! Agent 主流程：send_message 命令、内置工具循环 chat_with_builtin_tools 与工具分发编排。
//! 依赖 chat / tools / subagent / permissions / checkpoint / compaction / mcp 等全部下游模块。
//!
//! 从 main.rs 抽出（Plan16，最后一步）：纯代码搬移与可见性调整，无行为变更。
//! Plan28 P3-9：工作区上下文注入 messages_with_workspace_context / 项目长期记忆读取
//! read_workspace_memory / 验证命令探测 detect_verification_command 等纯逻辑已迁入
//! mdga-agent-core，本文件改为 `use mdga_agent_core::...` 调用，仅保留耦合 Tauri 的编排链路。

use crate::chat::{
    assistant_message_for_tool_calls, chat_messages_to_wire, recover_tool_calls_from_content,
    stream_round_with_retry,
};
use crate::checkpoint::{capture_checkpoint_before, persist_checkpoint, post_execution_diff};
use crate::command_run::{execute_bg_shell_tool, execute_run_command_tool};
use crate::compaction::{
    compact_tool_outputs, maybe_persist_large_output, summarize_wire_history,
};
use crate::hooks::{run_post_tool_hooks, run_pre_tool_hooks};
use crate::mcp::{
    collect_mcp_bindings, execute_add_mcp_server, execute_mcp_resource_tool, execute_mcp_tool,
    McpBinding,
};
use crate::permissions::{
    execute_ask_user, feed_tool_denial, gate_tool_decision, request_tool_approval, ToolGate,
};
use crate::state::AppState;
use crate::subagent::{execute_bg_task_tool, execute_run_subtask};
use crate::tools::{
    all_builtin_tool_schemas, execute_builtin_tool_call, execute_load_skill, execute_readonly_call,
    execute_remember, execute_todo_write, load_workspace_skills, PARALLEL_READONLY_TOOLS,
};
use crate::web::{execute_web_fetch, execute_web_search};
use crate::{commands::permission_mode_from_str, record_tool_event};
// Plan28 P3-9：内核纯逻辑（消息构建 / 记忆读取 / 压缩软上限 / 验证探测 / usage 合并）已迁入 agent-core。
use mdga_agent_core::{
    context_soft_limit_for, detect_verification_plan, focused_command, format_verify_feedback,
    merge_usage, messages_with_workspace_context, parse_report, read_workspace_memory,
    report_signature, VerifyKind,
};
use mdga_deepseek_client::{analyze_image, chat_stream, resolve_base_url, ChatMessage, StreamChunk};
use mdga_sandbox_runtime::{session_security_context, NetworkMode};
use mdga_shared::PermissionMode;
use mdga_storage::{
    get_conversation, get_messages, get_model_provider, list_permission_rules,
    save_token_ledger_entry,
};
use mdga_token_accounting::{compute_cost_summary, deepseek_pricing_for_model};
use mdga_tool_runtime::RunCommandRequest;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

/// 前端随消息上送的一张图片：媒体类型（如 "image/png"）+ base64（不含 data: 前缀）。
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InboundImage {
    pub media_type: String,
    pub base64: String,
}

/// 视觉桥接的 system 提示词（Plan18 §3 桥接提示词设计）：意图驱动、要点化、供看不到图的文本模型用。
const VISION_BRIDGE_SYSTEM: &str = "你是视觉分析助手。根据给定的用户需求，仔细观察图片，提取与该需求直接相关的所有信息（布局 / 文字 / 数据 / 颜色 / 尺寸 / 结构 / 报错内容 / 代码等），以要点化、信息密集的中文输出，供一个看不到图片的文本模型据此完成需求。只描述与需求相关的内容，不泛泛而谈、不寒暄。";

/// 压缩时保留最近 N 次工具结果全文，更早的大体积结果替换为短桩。
const KEEP_RECENT_TOOL_RESULTS: usize = 3;
/// 仅压缩正文超过该字符数的旧工具结果；小结果不动，避免无谓信息损失。
const TOOL_RESULT_STUB_THRESHOLD: usize = 1_500;

/// 卡死检测阈值（Plan25 #5③）：连续「无成功工具且无新叙述」轮数，或「同一工具+同参连续失败」
/// 次数达到该值，判定为卡死/打转，emit 通知并暂停，提示用户介入。
const STUCK_THRESHOLD: usize = 3;
/// 验证回路最多自纠轮数（Plan25 #7 → R3 升级）：写类操作后自动跑「编译门→测试门」，失败结构化回灌让
/// 模型修复到绿，超此轮数放弃。R3 起从 2 上调到 5（有 doom-loop 停滞护栏兜底，不会空转烧轮）。
const VERIFY_MAX_ROUNDS: usize = 5;
/// 验证 doom-loop 护栏阈值（R3）：连续相同「失败签名」轮数达到此值即判定停滞，升级用户而非继续空转。
const VERIFY_STALL_THRESHOLD: usize = 2;

// ── DeepSeek ──────────────────────────────────────────────────────────────

/// 发起流式聊天请求。
///
/// 通过 "chat-chunk" 事件逐块推送内容；流结束后发送 "chat-usage" 事件；
/// 最后发送 "chat-done"。错误时返回字符串供前端展示。
#[tauri::command]
pub(crate) async fn send_message(
    app: AppHandle,
    state: State<'_, AppState>,
    conversation_id: String,
    messages: Vec<ChatMessage>,
    model: String,
    permission_mode: String,
    plan_mode: Option<bool>,
    execute_plan: Option<bool>,
    images: Option<Vec<InboundImage>>,
) -> Result<(), String> {
    let plan_mode = plan_mode.unwrap_or(false);
    // Plan25 C-4：「批准并执行」时为 true，本轮装配阶段注入「严格按上一条计划执行 + 先建 todo」system。
    let execute_plan = execute_plan.unwrap_or(false);
    let images = images.unwrap_or_default();
    let (conversation, permission_rules, base_url, api_key, model_id, context_window, vision_provider) = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let conversation = get_conversation(&db, &conversation_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "会话不存在".to_string())?;
        let rules = list_permission_rules(&db).unwrap_or_default();
        // 主模型 provider（Plan17 D3）：base_url/api_key 一律从 DB 取；base_url 为空时解析 preset 官方端点。
        // DB 无主 provider 即报错引导去设置页，不再回退环境变量。
        // Plan20 🔴1：model_id 一并取出，作为本轮唯一权威模型名——主链路 chat 与计价均以它为准，
        // 不再用入参 model（前端控制行写死的 DeepSeek 清单）决定模型，否则配非 DeepSeek 主供应商必失败。
        // Plan27 C2 #2：context_window 一并取出，用于推导上下文压缩软上限。
        let (base_url, api_key, model_id, context_window) = match get_model_provider(&db, "main") {
            Ok(Some(p)) => {
                let bu = resolve_base_url(p.base_url.as_deref(), p.preset.as_deref())
                    .ok_or_else(|| "未配置主模型：请在 设置 → 模型供应商 配置".to_string())?;
                (bu, p.api_key, p.model_id, p.context_window)
            }
            _ => return Err("未配置主模型：请在 设置 → 模型供应商 配置".to_string()),
        };
        // 视觉 provider（Plan18）：仅在本轮带图时才需要；未配置则下方走「拒图」降级。
        let vision_provider = if images.is_empty() {
            None
        } else {
            get_model_provider(&db, "vision").ok().flatten()
        };
        (conversation, rules, base_url, api_key, model_id, context_window, vision_provider)
    };
    // 入参 model 保留以不破坏前端命令签名，但本轮已不再用它决定模型（权威源为 model_id）。
    let _ = &model;

    // Plan21 #4：自动初看的视觉 usage 需并入工具预算累计起点。在记账前 clone 一份留到这里，
    // 后续作为 initial_usage 传入有工作区分支的 chat_with_builtin_tools。
    let mut vision_usage: Option<mdga_shared::RawUsage> = None;
    // ── 自动初看（Plan18 §3 ①）：带意图把图片过一遍视觉模型，产出文本分析注入主 agent ──
    // 仅当本轮带图时进入。无视觉 provider 时注入提示而非中断（前端门禁已先拦，这里是后端兜底）。
    let vision_injection: Option<String> = if images.is_empty() {
        None
    } else if let Some(vp) = vision_provider {
        // 视觉 base_url 直接用用户自填值（视觉不强制走 preset 官方端点）。
        let vbase = vp.base_url.clone().unwrap_or_default();
        // 本轮用户消息文本作为「看什么」的方向盘（取最后一条 user 消息）。
        let intent = messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let user_text = format!("用户需求：{intent}");
        let imgs: Vec<mdga_deepseek_client::VisionImage> = images
            .iter()
            .map(|i| (i.media_type.clone(), i.base64.clone()))
            .collect();
        let _ = app.emit("agent-status", serde_json::json!({ "state": "analyzing_image" }));
        match analyze_image(
            &vbase,
            &vp.api_key,
            &vp.model_id,
            &vp.api_format,
            VISION_BRIDGE_SYSTEM,
            &user_text,
            &imgs,
        )
        .await
        {
            Ok((analysis, usage)) => {
                // 视觉分析对用户可见（Plan19 C-B）：emit 事件，前端据此在发送中即时插入「视觉分析」卡片。
                let usage_val = serde_json::to_value(&usage).unwrap_or(serde_json::Value::Null);
                let _ = app.emit(
                    "vision-analysis",
                    serde_json::json!({
                        "conversationId": &conversation_id,
                        "count": images.len(),
                        "analysis": &analysis,
                        "usage": usage_val,
                    }),
                );
                // Plan21 #4：记账前留存一份视觉 usage，供并入工具预算累计起点。
                vision_usage = usage.clone();
                // 视觉 usage 单独记账（与主模型分开）：写入 token_ledger，kind="vision"，保证 CSV 导出含视觉开销。
                if let Some(u) = &usage {
                    if let Ok(db) = state.db.lock() {
                        let _ = save_token_ledger_entry(
                            &db,
                            &conversation_id,
                            "vision",
                            &serde_json::to_string(u).unwrap_or_default(),
                        );
                    }
                }
                Some(format!(
                    "[视觉分析] 用户上传了 {} 张图片，针对其需求，视觉模型识别如下：\n{analysis}\n请据此与用户需求继续。",
                    images.len()
                ))
            }
            // 容错：视觉失败注入提示但不中断主流程，让主 agent 知道图没看成。
            Err(e) => Some(format!(
                "[视觉分析] 用户上传了 {} 张图片，但视觉分析失败：{e}。请据可见的文本需求尽力继续，必要时请用户用文字补充图片内容。",
                images.len()
            )),
        }
    } else {
        Some(format!(
            "[视觉分析] 用户上传了 {} 张图片，但当前未配置视觉模型，无法识图。请提示用户在 设置 → 模型供应商 → 扩展 agent 的模态 配置视觉模型。",
            images.len()
        ))
    };
    // 工作区已绑定时生成 repo map 与长期记忆，注入项目结构摘要和持久约定供模型开局认知。
    // repo map 按会话缓存：首轮生成后复用，保持 system 前缀字节稳定以提升 prompt 缓存命中。
    let repo_map = conversation
        .workspace_path
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .map(|path| {
            // R2 性能修复:命中会话缓存只短暂持锁(显式作用域释放守卫)即返回;未命中时把全仓
            // walk + tree-sitter 解析(耗时)放到锁外做,最后再短暂持锁回写——避免持锁跑重活、
            // 把所有会话串行阻塞在这一把 state.repo_maps 锁上(首图生成期尤甚)。
            {
                let maps = state.repo_maps.lock().expect("repo_maps mutex poisoned");
                if let Some(cached) = maps.get(conversation_id.as_str()) {
                    return cached.clone();
                }
            }
            // 文件树摘要（结构）+ tree-sitter/PageRank 关键符号地图（语义骨架）：锁外构建,
            // 让模型开局既知目录结构,也知核心代码在哪、谁调用谁。
            let tree = mdga_tool_runtime::workspace_map(path);
            let codemap = mdga_codemap::repo_map_for_context(path, 1200);
            let built = if codemap.trim().is_empty() {
                tree
            } else {
                format!(
                    "{tree}\n\n关键符号地图（tree-sitter 抽取定义 + PageRank 引用排名，\
                     文件按重要度降序、附定义行号；非语义向量）：\n{codemap}"
                )
            };
            // 回写缓存(短暂持锁);并发下若他人已抢先写入,以已有的为准。
            let mut maps = state.repo_maps.lock().expect("repo_maps mutex poisoned");
            maps.entry(conversation_id.clone()).or_insert(built).clone()
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
    // 视觉分析注入（Plan18 §3 ②）：把「图的文本化」作为 system 消息放在用户消息之前，
    // 让主 agent 一开局就知道图里与需求相关的内容。放在 workspace context 之后、最末，
    // 紧贴最后的 user 消息，避免被前缀的工作区/技能说明冲淡。
    if let Some(injection) = vision_injection {
        // 插在末尾 user 消息之前：找到最后一个 user 消息的下标。
        let insert_at = messages
            .iter()
            .rposition(|m| m.role == "user")
            .unwrap_or(messages.len());
        messages.insert(insert_at, ChatMessage { role: "system".to_string(), content: injection });
    }
    // 计划模式：要求模型只产出分步计划并等待确认，本轮不提供工具。
    if plan_mode {
        messages.insert(0, ChatMessage {
            role: "system".to_string(),
            content: "用户开启了计划模式：请基于需求给出清晰的分步执行计划（目标、步骤、涉及文件、风险点），然后停止并等待用户确认。本轮不要执行任何实际操作。".to_string(),
        });
    } else if execute_plan {
        // Plan25 C-4「批准并执行」：用户已批准上一条分步计划。注入到末尾 user 消息之前，
        // 紧贴本轮「按计划执行」指令，要求严格照计划走并先用 todo_write 建清单随进度更新。
        let insert_at = messages
            .iter()
            .rposition(|m| m.role == "user")
            .unwrap_or(messages.len());
        messages.insert(insert_at, ChatMessage {
            role: "system".to_string(),
            content: "用户已批准你上一条给出的分步计划。请严格按该计划执行，开工前先用 todo_write 建立任务清单并随进度更新状态（同一时刻只有一项 in_progress），不要重新规划或偏离已批准的方案。".to_string(),
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
        tokio::select! {
            r = chat_stream(&base_url, &api_key, messages, &model_id, |chunk| {
                // Plan27 C1（#1a）：正文增量走 "chat-chunk"，推理过程增量走 "chat-reasoning"。
                match chunk {
                    StreamChunk::Content(c) => {
                        let _ = app.emit("chat-chunk", c.to_string());
                    }
                    StreamChunk::Reasoning(r) => {
                        let _ = app.emit("chat-reasoning", r.to_string());
                    }
                }
            }) => r.map_err(|e| e.to_string()),
            _ = crate::chat::wait_for_cancel(&cancel_token) => {
                let _ = app.emit("chat-chunk", "\n\n（已中断）".to_string());
                Ok(None)
            }
        }
    } else if let Some(workspace_path) = conversation.workspace_path.as_deref() {
        let mcp_bindings = collect_mcp_bindings(&app);
        chat_with_builtin_tools(
            &base_url,
            &api_key,
            messages,
            &model_id,
            workspace_path,
            permission,
            &conversation_id,
            &app,
            cancel_token.clone(),
            permission_rules,
            mcp_bindings,
            // Plan21 #4：把自动初看的视觉 usage 作为工具预算累计起点传入。
            vision_usage,
            // Plan27 C2 #2：主供应商上下文窗口，用于推导压缩软上限。
            context_window,
        )
        .await
    } else {
        tokio::select! {
            r = chat_stream(&base_url, &api_key, messages, &model_id, |chunk| {
                // Plan27 C1（#1a）：正文增量走 "chat-chunk"，推理过程增量走 "chat-reasoning"。
                match chunk {
                    StreamChunk::Content(c) => {
                        let _ = app.emit("chat-chunk", c.to_string());
                    }
                    StreamChunk::Reasoning(r) => {
                        let _ = app.emit("chat-reasoning", r.to_string());
                    }
                }
            }) => r.map_err(|e| e.to_string()),
            _ = crate::chat::wait_for_cancel(&cancel_token) => {
                let _ = app.emit("chat-chunk", "\n\n（已中断）".to_string());
                Ok(None)
            }
        }
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
        let summary = compute_cost_summary(&raw, &deepseek_pricing_for_model(&model_id));
        let _ = app.emit("chat-usage", summary);
    }

    let _ = app.emit("chat-done", ());
    Ok(())
}

/// 把 todo 清单（todo_write 的 items 数组）压成轻量文本，供每轮回灌提醒（Plan25 #5①）。
/// 每项取 status 与 content/title，未完成项优先呈现；整体截断防膨胀。
fn summarize_todos(items: &[serde_json::Value]) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
        let text = item
            .get("content")
            .or_else(|| item.get("title"))
            .or_else(|| item.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("(未命名步骤)");
        // 用符号标注状态，便于模型一眼分辨已完成/进行中/待办。
        let mark = match status {
            "completed" | "done" => "[x]",
            "in_progress" | "in-progress" => "[~]",
            _ => "[ ]",
        };
        lines.push(format!("{mark} {text}"));
    }
    let joined = lines.join("\n");
    joined.chars().take(2_000).collect()
}

/// 把 todo 清单落盘到 <workspace>/.mdga/tasks/current.json（Plan25 #5②）。失败忽略，不阻塞主链路。
fn persist_current_todos(workspace: &str, items: &[serde_json::Value]) {
    let dir = std::path::Path::new(workspace).join(".mdga").join("tasks");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let payload = serde_json::json!({
        "updatedAt": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        "items": items,
    });
    if let Ok(text) = serde_json::to_string_pretty(&payload) {
        let _ = std::fs::write(dir.join("current.json"), text);
    }
}

/// ask_vision 工具（Plan27 C3 #1c）的视觉追问 system 提示词：意图驱动、要点化、只答所问。
const VISION_FOLLOWUP_SYSTEM: &str = "你是视觉追问助手。请仔细重新观察会话中的图片，针对用户的具体追问给出精确、要点化的中文回答；只回答被问到的内容，不泛泛而谈、不寒暄。看不清或图中没有相关信息时如实说明。";

/// 执行 ask_vision 工具：取本会话历史图片 → 读视觉 provider → 调 analyze_image 精读追问。
///
/// 流程（Plan27 C3 #1c）：
/// 1）get_messages 取本会话历史，解析各消息 parts_json 中 type=="image" 的 part（取 mediaType/base64，
///    按出现顺序、去重）；无图返回提示「本会话没有图片可追问」。
/// 2）读 vision provider（role="vision"）；未配置返回提示去「设置 → 模型供应商」配置视觉模型。
/// 3）用 analyze_image(base_url, key, model, api_format, 视觉追问 system, question, &images) 精读，
///    返回 { ok:true, answer }；usage 由调用方并入本轮。失败返回 Err 让工具循环回灌错误。
async fn execute_ask_vision(
    app: &AppHandle,
    conversation_id: &str,
    arguments: &str,
) -> (Result<serde_json::Value, String>, Option<mdga_shared::RawUsage>) {
    // 解析 question 参数。
    let question = match serde_json::from_str::<serde_json::Value>(arguments) {
        Ok(v) => v
            .get("question")
            .and_then(|q| q.as_str())
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(str::to_string),
        Err(e) => return (Err(format!("工具参数解析失败: {e}")), None),
    };
    let Some(question) = question else {
        return (Err("ask_vision 缺少 question".to_string()), None);
    };

    // 从会话历史抽取图片（按出现顺序去重）与视觉 provider 配置，仅在持锁期间访问 DB。
    let state = app.state::<AppState>();
    let (images, vision_provider) = {
        let Ok(db) = state.db.lock() else {
            return (Err("数据库忙，请稍后重试".to_string()), None);
        };
        let messages = match get_messages(&db, conversation_id) {
            Ok(m) => m,
            Err(e) => return (Err(e.to_string()), None),
        };
        let images = collect_conversation_images(&messages);
        let vp = get_model_provider(&db, "vision").ok().flatten();
        (images, vp)
    };

    if images.is_empty() {
        // 无图：返回提示而非报错（工具成功，answer 即提示语）。
        return (
            Ok(serde_json::json!({
                "ok": true,
                "answer": "本会话没有图片可追问。请提示用户先上传图片后再使用 ask_vision。"
            })),
            None,
        );
    }
    let Some(vp) = vision_provider else {
        return (
            Ok(serde_json::json!({
                "ok": true,
                "answer": "当前未配置视觉模型，无法对图片追问。请提示用户在 设置 → 模型供应商 → 扩展 agent 的模态 配置视觉模型。"
            })),
            None,
        );
    };

    // 视觉 base_url 直接用用户自填值（视觉不强制走 preset 官方端点）。
    let vbase = vp.base_url.clone().unwrap_or_default();
    let _ = app.emit("agent-status", serde_json::json!({ "state": "analyzing_image" }));
    match analyze_image(
        &vbase,
        &vp.api_key,
        &vp.model_id,
        &vp.api_format,
        VISION_FOLLOWUP_SYSTEM,
        &question,
        &images,
    )
    .await
    {
        Ok((answer, usage)) => {
            // 视觉追问 usage 单独记账（与主模型分开），保证 CSV 导出含视觉开销。
            if let Some(u) = &usage {
                if let Ok(db) = state.db.lock() {
                    let _ = save_token_ledger_entry(
                        &db,
                        conversation_id,
                        "vision",
                        &serde_json::to_string(u).unwrap_or_default(),
                    );
                }
            }
            (Ok(serde_json::json!({ "ok": true, "answer": answer })), usage)
        }
        Err(e) => (Err(format!("视觉追问失败: {e}")), None),
    }
}

/// 从会话历史消息中按出现顺序提取图片（mediaType, base64），并对相同 base64 去重。
///
/// 解析每条消息的 parts_json（JSON 数组），取 type=="image" 的 part 的 mediaType / base64。
/// 解析失败或非数组的消息跳过；用于 ask_vision 重新喂图给视觉模型。
fn collect_conversation_images(
    messages: &[mdga_storage::StoredMessage],
) -> Vec<mdga_deepseek_client::VisionImage> {
    let mut images: Vec<mdga_deepseek_client::VisionImage> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for msg in messages {
        let Some(parts_json) = msg.parts_json.as_deref() else {
            continue;
        };
        let Ok(parts) = serde_json::from_str::<serde_json::Value>(parts_json) else {
            continue;
        };
        let Some(arr) = parts.as_array() else {
            continue;
        };
        for part in arr {
            if part.get("type").and_then(|t| t.as_str()) != Some("image") {
                continue;
            }
            let media_type = part.get("mediaType").and_then(|m| m.as_str());
            let base64 = part.get("base64").and_then(|b| b.as_str());
            if let (Some(media_type), Some(base64)) = (media_type, base64) {
                if base64.is_empty() || !seen.insert(base64.to_string()) {
                    continue; // 空或重复图片跳过
                }
                images.push((media_type.to_string(), base64.to_string()));
            }
        }
    }
    images
}

/// 把当前 wire 历史落库(每会话一行 UPSERT),供断额/崩溃后重建续接。best-effort,失败不影响主流程。
fn persist_wire(app: &AppHandle, conversation_id: &str, wire: &[serde_json::Value]) {
    let Ok(json) = serde_json::to_string(wire) else {
        return;
    };
    let state = app.state::<AppState>();
    let Ok(db) = state.db.lock() else {
        return;
    };
    let _ = mdga_storage::save_wire_snapshot(&db, conversation_id, &json);
}

/// P1.5:给已声明 tool_calls 却无对应 tool 结果的孤儿(中断在工具执行中途)补占位 tool 消息,
/// 使落库 wire 满足「每个 tool_use 必跟 tool_result」不变式(否则续接重放撞 Anthropic 400)。
/// 在 load_wire 读回时调用——崩溃不经 return,孤儿在读回侧补最稳。
pub(crate) fn finalize_wire(wire: &mut Vec<serde_json::Value>) {
    let mut answered = std::collections::HashSet::new();
    for m in wire.iter() {
        if m.get("role").and_then(|r| r.as_str()) == Some("tool") {
            if let Some(id) = m.get("tool_call_id").and_then(|i| i.as_str()) {
                answered.insert(id.to_string());
            }
        }
    }
    let mut orphans = Vec::new();
    for m in wire.iter() {
        if m.get("role").and_then(|r| r.as_str()) == Some("assistant") {
            if let Some(calls) = m.get("tool_calls").and_then(|c| c.as_array()) {
                for call in calls {
                    if let Some(id) = call.get("id").and_then(|i| i.as_str()) {
                        if !answered.contains(id) {
                            orphans.push(id.to_string());
                        }
                    }
                }
            }
        }
    }
    for id in orphans {
        wire.push(serde_json::json!({
            "role": "tool",
            "tool_call_id": id,
            "content": "(已中断,工具未完成)"
        }));
    }
}

/// Agent 工具循环：每轮带工具问模型、执行返回的工具、把结果回灌，直到模型不再调用工具
/// （自然终止）或用户中断。不设轮数上限——上下文自动压缩兜底体积，取消按钮兜底失控；
/// 所有工具执行前都经 SessionSecurityContext 裁决。
#[allow(clippy::too_many_arguments)]
async fn chat_with_builtin_tools(
    base_url: &str,
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
    // Plan21 #4：本轮在进入工具循环前已产生的 usage（如自动初看的视觉开销），作为预算累计起点，
    // 使下方预算判断（:491-503）覆盖视觉/前置开销，而不仅是主循环内的 token。
    initial_usage: Option<mdga_shared::RawUsage>,
    // Plan27 C2 #2：主供应商上下文窗口（tokens，可空）。有值时软上限按其 × 0.8 推导。
    context_window: Option<i64>,
) -> Result<Option<mdga_shared::RawUsage>, String> {
    let security_context = session_security_context(
        workspace_path.to_string(),
        // Plan25 C-3：clone 后再交给安全上下文，保留 permission_mode 供 run_subtask 调用点回传子代理。
        permission_mode.clone(),
        NetworkMode::Disabled,
    )
    .map_err(|e| e.to_string())?;
    // 工具 schema：Built-in + 已连接 MCP server 的外部工具。
    let tool_schemas: Vec<serde_json::Value> = all_builtin_tool_schemas()
        .into_iter()
        .chain(mcp_bindings.iter().map(|b| b.schema.clone()))
        .collect();
    let mut wire_messages = chat_messages_to_wire(messages);
    // 以传入的前置 usage 为初值（Plan21 #4），后续 merge_usage 在其上累加。
    let mut usage: Option<mdga_shared::RawUsage> = initial_usage;
    // 上一次响应返回的 prompt_tokens，作为当前上下文体积的真实信号，驱动轮内压缩。
    let mut last_prompt_tokens: u64 = 0;
    // 验证回路（Plan25 #7）：记录是否发生过写类工具改动 + 已进行的验证自纠轮数（上限 VERIFY_MAX_ROUNDS）。
    let mut edits_made = false;
    let mut verify_rounds: usize = 0;
    // R3 doom-loop 护栏 + 按影响选测的跨轮状态：上轮失败签名 / 连续停滞计数 / 上轮失败用例名。
    let mut verify_prev_sig: Option<String> = None;
    let mut verify_stall: usize = 0;
    let mut verify_failing: Vec<String> = Vec::new();
    // 长任务跟踪（Plan25 #5①②）：维护最近一次 todo_write 的清单，每轮在 wire 末尾 user 之前注入轻量提醒，
    // 并在每次 todo_write 成功后落盘 <workspace>/.mdga/tasks/current.json。
    let mut current_todos: Option<Vec<serde_json::Value>> = None;
    // 卡死检测（Plan25 #5③）：连续「无成功工具且无新叙述」轮数，与「同一工具+同参连续失败」计数。
    let mut no_progress_rounds: usize = 0;
    let mut last_failure_signature: Option<String> = None;
    let mut repeated_failure_count: usize = 0;

    // Plan27 C2 #2：本轮压缩软上限——主供应商有 context_window 则用其 × 0.8，否则回退默认（env 仍优先）。
    // 整轮恒定，故循环外算一次：既用于压缩触发判断，也作为 context-usage 事件的 softLimit。
    let soft_limit = context_soft_limit_for(context_window);

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
                    summarize_wire_history(base_url, api_key, model, std::mem::take(&mut wire_messages), app)
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

        // 长任务清单回灌（Plan25 #5①）：若已有 todo 清单，本轮临时在 wire 末尾追加一条轻量 system 提醒，
        // 让模型聚焦未完成项。只作用于本轮请求、不写入持久 wire_messages（避免逐轮累积冗余）。
        let request_messages = if let Some(items) = current_todos.as_ref() {
            let mut req = wire_messages.clone();
            req.push(serde_json::json!({
                "role": "system",
                "content": format!(
                    "当前任务清单（请聚焦未完成项，完成一项即用 todo_write 更新其状态，同一时刻只保留一项 in_progress）：\n{}",
                    summarize_todos(items)
                )
            }));
            req
        } else {
            wire_messages.clone()
        };

        // 边执行边落库:每轮发起请求前先落本轮起始 wire(含 steering/压缩),断额(下方 .await?)或崩溃后可重建。
        persist_wire(app, conversation_id, &wire_messages);
        // 流式获取本轮结果：叙述 token 边流边显，同时累积 tool_calls。传 cancel 使流式可被「停止」立即中断
        // (此前 cancel 只在轮间/工具前检查,卡在流式 await 时看不到,导致点停止要等响应收完才生效)。
        let completion = match stream_round_with_retry(
            base_url,
            api_key,
            request_messages,
            model,
            tool_schemas.clone(),
            app,
            &cancel,
        )
        .await
        {
            Ok(c) => c,
            Err(e) if e == crate::chat::STREAM_CANCELLED => {
                let _ = app.emit("chat-chunk", "\n\n（已中断）".to_string());
                return Ok(usage); // 保留已流式显示 + 已落库 wire,前端 chat-done 会持久化
            }
            Err(e) => return Err(e),
        };
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
        // 卡死检测（Plan25 #5③）：本轮是否产生了新叙述文本——把「无成功工具」与之结合判断打转。
        // 注意：若正文被兜底解析为 tool_calls，则不算「叙述」（它本质是工具调用）。
        let had_assistant_text = tool_calls.is_empty()
            && completion
                .content
                .as_deref()
                .map(|c| !c.trim().is_empty())
                .unwrap_or(false);

        // 模型不再调用工具：本轮叙述即最终回复。收尾前进入验证回路（Plan25 #7）：
        // 若本轮链路发生过写类工具改动、且能探测到验证手段（.mdga/diagnostics 或 cargo/npm 等），
        // 自动跑一次；失败则把输出作为新一轮 user 回灌让模型自纠（最多 VERIFY_MAX_ROUNDS 轮），
        // 通过 / 放弃后结束。验证回路用独立计数 verify_rounds，不与卡死检测（#5③）共用打转判断。
        if tool_calls.is_empty() {
            // R3 真 TDD 自修复回路：本轮发生过写改且未超轮数，按探测到的验证计划跑「编译门→测试门」，
            // 首个失败的门即结构化解析 + 回灌让模型修复到绿；doom-loop 护栏在失败集合长期不变时升级用户。
            if edits_made && verify_rounds < VERIFY_MAX_ROUNDS {
                if let Some(plan) = detect_verification_plan(workspace_path) {
                    verify_rounds += 1;
                    // 重置写改标记：下一轮只有再次发生写改才会再次触发验证，避免空转。
                    edits_made = false;
                    // R3 安全修复:自动验证命令必须遵循用户的「命令沙箱」开关 + 会话网络模式。
                    // 此前硬编码 run_command(sandbox=false) 裸跑——等于用户开了沙箱却仍把 cargo test /
                    // pytest / build(会执行 build.rs / 测试体 / conftest 等任意项目代码)放沙箱外自动跑。
                    let verify_sandbox =
                        app.state::<AppState>().command_sandbox.load(Ordering::SeqCst);
                    let verify_net = matches!(
                        security_context.network_mode,
                        NetworkMode::AllowListed | NetworkMode::FullAccess
                    );
                    let mut fed_back = false;
                    let _ = app.emit("agent-status", serde_json::json!({ "state": "thinking", "round": round }));
                    'steps: for step in &plan.steps {
                        // 按影响选测：测试门且上轮已有失败用例名 → 先只重跑这些失败（快）；否则全量。
                        let focused = focused_command(step, &verify_failing);
                        let is_focused = focused.is_some();
                        let cmd = focused.unwrap_or_else(|| step.command.clone());
                        let _ = app.emit("chat-chunk", format!("\n\n（正在运行验证：`{cmd}`…）\n\n"));
                        let result = match mdga_tool_runtime::run_command_streaming(
                            workspace_path,
                            RunCommandRequest { command: cmd.clone(), timeout_secs: Some(300), background: false },
                            None,
                            None,
                            verify_sandbox,
                            verify_net,
                        ) {
                            Ok(r) => r,
                            Err(_) => continue, // 命令起不来：跳过该门，不阻断收尾
                        };
                        let failed = result.exit_code.unwrap_or(0) != 0 || result.timed_out;
                        if !failed {
                            // 窄跑的测试门通过：复跑整套确认整体绿（防修复碰坏别的用例）。
                            if is_focused && step.kind == VerifyKind::Test {
                                let _ = app.emit("chat-chunk", format!("\n\n（失败用例已绿，复跑整套确认：`{}`…）\n\n", step.command));
                                if let Ok(full) = mdga_tool_runtime::run_command_streaming(
                                    workspace_path,
                                    RunCommandRequest { command: step.command.clone(), timeout_secs: Some(300), background: false },
                                    None,
                                    None,
                                    verify_sandbox,
                                    verify_net,
                                ) {
                                    let full_failed = full.exit_code.unwrap_or(0) != 0 || full.timed_out;
                                    if full_failed {
                                        let report = parse_report(step.framework, step.kind, &full.stdout, &full.stderr);
                                        verify_failing = report.failures.iter().map(|f| f.name.clone()).collect();
                                        let sig = report_signature(&report, full.exit_code, full.timed_out);
                                        if verify_stall_hit(&sig, &mut verify_prev_sig, &mut verify_stall) {
                                            emit_verify_stall(app, conversation_id, &step.command);
                                            return Ok(usage);
                                        }
                                        wire_messages.push(serde_json::json!({
                                            "role": "user",
                                            "content": format_verify_feedback(&step.command, &report),
                                        }));
                                        fed_back = true;
                                        break 'steps;
                                    }
                                }
                            }
                            if step.kind == VerifyKind::Test {
                                verify_failing.clear(); // 整套绿：清掉失败名
                            }
                            continue; // 该门通过，进入下一门
                        }
                        // 该门失败：结构化解析 + doom-loop 护栏 + 回灌，首个失败门即停（不跑后续门）。
                        let report = parse_report(step.framework, step.kind, &result.stdout, &result.stderr);
                        // 仅测试门的失败用例可供下轮窄跑；编译门失败清空（修好编译后应整套重跑）。
                        verify_failing = if step.kind == VerifyKind::Test {
                            report.failures.iter().map(|f| f.name.clone()).collect()
                        } else {
                            Vec::new()
                        };
                        let sig = report_signature(&report, result.exit_code, result.timed_out);
                        if verify_stall_hit(&sig, &mut verify_prev_sig, &mut verify_stall) {
                            emit_verify_stall(app, conversation_id, &cmd);
                            return Ok(usage);
                        }
                        wire_messages.push(serde_json::json!({
                            "role": "user",
                            "content": format_verify_feedback(&cmd, &report),
                        }));
                        fed_back = true;
                        break 'steps;
                    }
                    if fed_back {
                        continue; // 回到循环让 agent 修
                    }
                    // 所有门通过：验证绿，正常收尾。
                }
            }
            return Ok(usage);
        }

        wire_messages.push(assistant_message_for_tool_calls(
            completion.assistant_message,
            &tool_calls,
        ));
        // 边执行边落库:assistant 已声明 tool_calls,工具执行前先落——崩溃在工具执行中也能从 DB 重建到此。
        persist_wire(app, conversation_id, &wire_messages);

        // 卡死检测（Plan25 #5③）本轮状态：是否有任意工具成功执行 + 本轮最后一次失败的「工具+参数」签名。
        let mut round_had_success = false;
        let mut round_failure_signature: Option<String> = None;

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
                if result.is_ok() {
                    round_had_success = true;
                } else {
                    round_failure_signature =
                        Some(format!("{}|{}", call.function.name, call.function.arguments));
                }
                wire_messages.push(serde_json::json!({
                    "role": "tool", "tool_call_id": call.id, "content": maybe_persist_large_output(workspace_path, &output_str)
                }));
            }
            // 并行批次全部完成,落库(本轮 tool 结果已就绪,断额/崩溃可重建)。
            persist_wire(app, conversation_id, &wire_messages);
            // 卡死检测（Plan25 #5③）：并行只读批次同样纳入「打转」判断，命中即暂停。
            if detect_and_emit_stuck(
                app,
                conversation_id,
                had_assistant_text,
                round_had_success,
                &round_failure_signature,
                &mut no_progress_rounds,
                &mut last_failure_signature,
                &mut repeated_failure_count,
            ) {
                return Ok(usage);
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

            // 写类工具执行前捕获回退快照（rewind 用），必须先于执行。提前到门控前，
            // 以便据 capture.revertible 判断是否触发「不可回退强制审批」（Plan21 #2b）。
            let capture = capture_checkpoint_before(workspace_path, &tool_name, &arguments);
            // 该次是否为「写/删类且快照失败（不可回退）」：用于强制审批与审批文案标注。
            let irreversible = capture.as_ref().map(|c| !c.revertible).unwrap_or(false);

            // 权限门控：白名单命令与「总是允许」规则直接放行，否则按权限模式放行 / 审批 / 拒绝。
            let decision =
                gate_tool_decision(&security_context, &tool_name, &arguments, &permission_rules);
            let proceed = match decision {
                ToolGate::Allow => {
                    // Plan21 #2b：即便门控放行（如默认模式自动放行的写入），若本次不可回退
                    //（目标超大/二进制致快照失败、删目录等），也必须先发审批并标注不可回退；
                    //  放在 gate 之后、真正执行之前，覆盖 #2a 漏掉的「自动放行的不可回退覆盖」场景。
                    if irreversible {
                        let approved =
                            request_tool_approval(app, &tool_name, &arguments, true).await;
                        if !approved {
                            feed_tool_denial(
                                app,
                                conversation_id,
                                &tool_name,
                                &arguments,
                                workspace_path,
                                "用户拒绝了该操作（不可回退，已取消）",
                                &call.id,
                                &mut wire_messages,
                            );
                        }
                        approved
                    } else {
                        true
                    }
                }
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
                    // 需审批时一并把不可回退标志传给审批弹窗，让用户在审批界面即看到风险。
                    let approved =
                        request_tool_approval(app, &tool_name, &arguments, irreversible).await;
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
                "ask_vision" => {
                    // Plan27 C3（#1c）：对本会话已上传图片做针对性追问/精读。
                    let (vision_result, vision_usage) =
                        execute_ask_vision(app, conversation_id, &arguments).await;
                    usage = merge_usage(usage, vision_usage);
                    vision_result
                }
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
                    // Plan25 C-3：补传本轮权限模式与权限规则,供可写子代理(mode="write")复用主链路门控/检查点。
                    let (sub_result, sub_usage) = execute_run_subtask(
                        base_url,
                        api_key,
                        model,
                        workspace_path,
                        &arguments,
                        app,
                        conversation_id,
                        permission_mode.clone(),
                        permission_rules.clone(),
                        &cancel,
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
                    // 卡死检测（Plan25 #5③）：有工具成功即视为本轮有进展。
                    round_had_success = true;
                    let mut out = serde_json::json!({ "ok": true, "result": value });
                    // 标记本轮发生过文件改动（驱动收尾前的验证回路 #7）。
                    if capture.is_some() {
                        edits_made = true;
                    }
                    // 长任务跟踪（Plan25 #5①②）：todo_write 成功后更新内存清单并落盘 current.json（失败忽略）。
                    if tool_name == "todo_write" {
                        if let Some(items) = serde_json::from_str::<serde_json::Value>(&arguments)
                            .ok()
                            .and_then(|v| v.get("items").and_then(|i| i.as_array()).cloned())
                        {
                            persist_current_todos(workspace_path, &items);
                            current_todos = Some(items);
                        }
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
                Err(message) => {
                    // 卡死检测（Plan25 #5③）：记录本轮最后一次失败的「工具+参数」签名，
                    // 用于判断「同一工具+同参连续失败」打转。
                    round_failure_signature = Some(format!("{tool_name}|{arguments}"));
                    (
                        serde_json::json!({
                            "ok": false,
                            "error": message,
                            "hint": "工具执行失败。请阅读 error 判断是参数错误、路径不存在还是命令/环境问题，据此调整后重试或改用其他工具/写法；不要原样重复同一次失败的调用。"
                        }),
                        "failed",
                        Some(message.clone()),
                    )
                }
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
            // 边执行边落库:每个串行工具处理完即落,崩溃在下一个工具时 DB 已有此结果。
            persist_wire(app, conversation_id, &wire_messages);
        }

        // 卡死检测（Plan25 #5③）：本轮全部工具处理完后评估「打转」，命中即 emit 通知并暂停。
        if detect_and_emit_stuck(
            app,
            conversation_id,
            had_assistant_text,
            round_had_success,
            &round_failure_signature,
            &mut no_progress_rounds,
            &mut last_failure_signature,
            &mut repeated_failure_count,
        ) {
            return Ok(usage);
        }
    }
}

/// 卡死检测核心（Plan25 #5③）：基于本轮「是否有成功工具 / 是否有新叙述 / 失败签名」更新两个累计计数，
/// 任一计数达到 [`STUCK_THRESHOLD`] 即 emit 一条 `agent-stuck` 通知事件并向前端发暂停提示，返回 true 表示应暂停。
///
/// 两条独立判据：
/// ① 连续「无成功工具且无新叙述」轮数 —— 模型空转、既不推进也不交付。
/// ② 同一「工具+参数」连续失败次数 —— 原样重复同一次失败调用（撞墙）。
#[allow(clippy::too_many_arguments)]
fn detect_and_emit_stuck(
    app: &AppHandle,
    conversation_id: &str,
    had_assistant_text: bool,
    round_had_success: bool,
    round_failure_signature: &Option<String>,
    no_progress_rounds: &mut usize,
    last_failure_signature: &mut Option<String>,
    repeated_failure_count: &mut usize,
) -> bool {
    // ① 无进展轮数：本轮既无工具成功、也无新叙述则累加，否则清零。
    if round_had_success || had_assistant_text {
        *no_progress_rounds = 0;
    } else {
        *no_progress_rounds += 1;
    }
    // ② 同一工具+同参连续失败：与上一轮失败签名相同则累加，否则以本轮签名重置为 1（无失败则清零）。
    match round_failure_signature {
        Some(sig) => {
            if last_failure_signature.as_deref() == Some(sig.as_str()) {
                *repeated_failure_count += 1;
            } else {
                *last_failure_signature = Some(sig.clone());
                *repeated_failure_count = 1;
            }
        }
        None => {
            *last_failure_signature = None;
            *repeated_failure_count = 0;
        }
    }

    let stuck_no_progress = *no_progress_rounds >= STUCK_THRESHOLD;
    let stuck_repeated_failure = *repeated_failure_count >= STUCK_THRESHOLD;
    if stuck_no_progress || stuck_repeated_failure {
        let reason = if stuck_repeated_failure {
            "同一工具调用连续多次以相同参数失败"
        } else {
            "连续多轮无任何工具成功且无新进展"
        };
        let _ = app.emit(
            "agent-stuck",
            serde_json::json!({
                "conversationId": conversation_id,
                "reason": reason,
            }),
        );
        let _ = app.emit(
            "chat-chunk",
            format!("\n\n（检测到任务疑似卡住：{reason}，已暂停以等待你介入。可调整需求或提供更多信息后让我继续。）"),
        );
        return true;
    }
    false
}

/// R3 验证 doom-loop 护栏：比较本轮失败签名与上轮。签名不变累加停滞计数、达到
/// [`VERIFY_STALL_THRESHOLD`] 返回 true（应停止自纠并升级用户）；签名变化则视为有进展，
/// 以本轮签名重置停滞计数。
fn verify_stall_hit(sig: &str, prev: &mut Option<String>, stall: &mut usize) -> bool {
    match prev.as_deref() {
        Some(p) if p == sig => *stall += 1,
        _ => {
            *prev = Some(sig.to_string());
            *stall = 0;
        }
    }
    *stall >= VERIFY_STALL_THRESHOLD
}

/// R3 验证停滞升级：复用卡死的 `agent-stuck` 事件通道，emit 通知并向前端发提示，等待用户介入。
fn emit_verify_stall(app: &AppHandle, conversation_id: &str, command: &str) {
    let _ = app.emit(
        "agent-stuck",
        serde_json::json!({
            "conversationId": conversation_id,
            "reason": format!("验证停滞：`{command}` 连续多轮失败集合无变化"),
        }),
    );
    let _ = app.emit(
        "chat-chunk",
        format!("\n\n（检测到验证停滞：`{command}` 连续多轮报告同一组失败、未见收敛，已暂停以等待你介入。可调整需求或提供更多信息后让我继续。）"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // 注（Plan28 P3-9）：messages_with_workspace_context 的 3 个单测已随该函数迁入
    // mdga-agent-core（crates/agent-core/src/messages.rs），此处不再保留。

    #[test]
    fn verify_stall_guard_trips_on_repeated_signature() {
        // R3 doom-loop 护栏：同一失败签名连续累计达到 VERIFY_STALL_THRESHOLD 才判停滞。
        let mut prev = None;
        let mut stall = 0usize;
        // 第 1 轮：签名 A，记录、计数清零，不触发。
        assert!(!verify_stall_hit("F:a|b", &mut prev, &mut stall));
        // 第 2 轮：仍 A，stall=1，未达阈值（2）。
        assert!(!verify_stall_hit("F:a|b", &mut prev, &mut stall));
        // 第 3 轮：仍 A，stall=2，触发停滞。
        assert!(verify_stall_hit("F:a|b", &mut prev, &mut stall));
    }

    #[test]
    fn verify_stall_guard_resets_on_progress() {
        // 失败签名变化（有进展）应重置停滞计数，不会误判停滞。
        let mut prev = None;
        let mut stall = 0usize;
        assert!(!verify_stall_hit("F:a", &mut prev, &mut stall));
        assert!(!verify_stall_hit("F:a", &mut prev, &mut stall)); // stall=1
        assert!(!verify_stall_hit("F:b", &mut prev, &mut stall)); // 变化 → 重置为 0
        assert!(!verify_stall_hit("F:b", &mut prev, &mut stall)); // stall=1，仍未达阈值
        assert_eq!(stall, 1);
    }

    #[test]
    fn finalize_wire_pads_orphan_tool_call() {
        // assistant 声明 2 个 tool_call,只有 1 个有 tool 结果 → 另一个孤儿,应补占位 tool 消息。
        let mut wire = vec![
            serde_json::json!({"role": "user", "content": "hi"}),
            serde_json::json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [
                    {"id": "call_a", "function": {"name": "read_file", "arguments": "{}"}},
                    {"id": "call_b", "function": {"name": "run_command", "arguments": "{}"}}
                ]
            }),
            serde_json::json!({"role": "tool", "tool_call_id": "call_a", "content": "ok"}),
        ];
        finalize_wire(&mut wire);
        assert_eq!(wire.len(), 4, "应为孤儿 call_b 补一条占位 tool 消息");
        let last = wire.last().unwrap();
        assert_eq!(last["role"], "tool");
        assert_eq!(last["tool_call_id"], "call_b");
        assert!(last["content"].as_str().unwrap().contains("已中断"));
    }

    #[test]
    fn finalize_wire_noop_when_all_answered() {
        // 所有 tool_call 都有对应结果 → finalize 不应添加任何东西。
        let mut wire = vec![
            serde_json::json!({
                "role": "assistant", "content": null,
                "tool_calls": [{"id": "call_x", "function": {"name": "read_file", "arguments": "{}"}}]
            }),
            serde_json::json!({"role": "tool", "tool_call_id": "call_x", "content": "done"}),
        ];
        finalize_wire(&mut wire);
        assert_eq!(wire.len(), 2, "无孤儿时 finalize 不应改动");
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

        let output = crate::tools::execute_create_file_tool_call(
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
