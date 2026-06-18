//! 子代理与后台任务：run_subtask 只读探索子代理（前台/后台）与后台任务控制工具
//! （get_task_output / kill_task / list_tasks）。
//!
//! 从 main.rs 抽出（Plan16）：纯代码搬移，无行为变更。

use crate::chat::{
    assistant_message_for_tool_calls, chat_completion_with_retry, recover_tool_calls_from_content,
};
use crate::checkpoint::{capture_checkpoint_before, persist_checkpoint};
use crate::permissions::{
    feed_tool_denial, gate_tool_decision, request_tool_approval, ToolGate,
};
use crate::state::{AppState, BgTask, BG_TASK_SEQ};
use crate::tools::{all_builtin_tool_schemas, execute_builtin_tool_call};
use crate::record_tool_event;
// Plan28 P3-9：merge_usage 已迁入 agent-core。
use mdga_agent_core::merge_usage;
use mdga_deepseek_client::strip_dsml_markup;
use mdga_sandbox_runtime::{session_security_context, NetworkMode};
use mdga_shared::PermissionMode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

/// 子代理只读探索工具名单（read 模式 / write 模式共享的只读部分）。
const SUBTASK_READ_ONLY_TOOLS: &[&str] =
    &["list_dir", "read_file", "search_text", "glob_files", "stat_path"];

/// write 模式子代理额外开放的写 / 编辑 / 命令工具名单。
const SUBTASK_WRITE_TOOLS: &[&str] = &[
    "create_file",
    "write_file",
    "edit_file",
    "apply_patch",
    "apply_multi_patch",
    "make_dir",
    "move_path",
    "delete_file",
    "delete_dir",
    "run_command",
];

/// 子任务探索代理可用的只读工具集合（read 模式）。
fn read_only_tool_schemas() -> Vec<serde_json::Value> {
    tool_schemas_for_names(SUBTASK_READ_ONLY_TOOLS)
}

/// write 模式子代理工具集：只读工具 + 写 / 编辑 / 命令工具。
fn writable_tool_schemas() -> Vec<serde_json::Value> {
    let allowed: Vec<&str> = SUBTASK_READ_ONLY_TOOLS
        .iter()
        .chain(SUBTASK_WRITE_TOOLS.iter())
        .copied()
        .collect();
    tool_schemas_for_names(&allowed)
}

/// 从全部内建 schema 中筛出给定工具名集合的 schema。
fn tool_schemas_for_names(names: &[&str]) -> Vec<serde_json::Value> {
    all_builtin_tool_schemas()
        .into_iter()
        .filter(|schema| {
            schema
                .pointer("/function/name")
                .and_then(|n| n.as_str())
                .map(|name| names.contains(&name))
                .unwrap_or(false)
        })
        .collect()
}

/// 是否需要门控 + 检查点保护：写 / 删 / 命令类工具（write 模式下逐次审批与快照）。
fn is_guarded_write_tool(name: &str) -> bool {
    SUBTASK_WRITE_TOOLS.contains(&name)
}

const SUBTASK_MAX_ROUNDS: usize = 15;

/// run_subtask 工具：用独立上下文跑一个探索 / 工作子代理，返回简明报告与消耗的 usage。
///
/// read 模式（默认）：子代理只能 list/read/search/stat，强制 Restricted + 断网。
/// write 模式（Plan25 C-3）：子代理额外可写 / 编辑 / 跑命令，继承主链路传入的 `permission`，
/// 每次写 / 删 / 命令类调用都复用主链路的门控（`gate_tool_decision` + `request_tool_approval`）与
/// 检查点（`capture_checkpoint_before` / `persist_checkpoint`），绝不绕过权限。
/// 工具事件以 `sub:` 前缀推给前端展示。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_run_subtask(
    base_url: &str,
    api_key: &str,
    model: &str,
    workspace_path: &str,
    arguments: &str,
    app: &AppHandle,
    conversation_id: &str,
    permission: mdga_shared::PermissionMode,
    permission_rules: Vec<String>,
    cancel: &Arc<AtomicBool>,
) -> (Result<serde_json::Value, String>, Option<mdga_shared::RawUsage>) {
    let parsed: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(value) => value,
        Err(e) => return (Err(format!("工具参数解析失败: {e}")), None),
    };
    let Some(description) = parsed.get("description").and_then(|v| v.as_str()) else {
        return (Err("run_subtask 缺少 description".to_string()), None);
    };
    // mode：read（默认）只读探索；write 可写 / 跑命令（受权限与检查点保护）。
    let write_mode = matches!(parsed.get("mode").and_then(|v| v.as_str()), Some("write"));
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
    let system_prompt = if write_mode {
        match custom_agent {
            Some(def) => format!(
                "你是一个可写工作子代理，工作区路径是 {workspace_path}。除只读工具（list_dir、read_file、search_text、glob_files、stat_path）外，你还可以使用 create_file、write_file、edit_file、apply_patch、make_dir、move_path、delete_file、delete_dir、run_command 来真正改动文件或执行命令——但每次写 / 删 / 命令操作都会经过用户权限门控与检查点保护，请只在被委托的范围内动手、改前先读、优先精确编辑。以下是你的角色定义：\n{def}\n完成后输出简明中文报告，说明你做了哪些改动。"
            ),
            None => format!(
                "你是一个可写工作子代理，工作区路径是 {workspace_path}。除只读工具（list_dir、read_file、search_text、glob_files、stat_path）外，你还可以使用 create_file、write_file、edit_file、apply_patch、make_dir、move_path、delete_file、delete_dir、run_command 来真正改动文件或执行命令。每次写 / 删 / 命令操作都会经过用户权限门控与检查点保护，可能需要用户审批。请严格聚焦被委托的范围、改前先读、优先用 edit_file/apply_patch 做精确修改，不要做范围外的改动。完成后输出一份简明、信息密度高的中文报告，说明你做了哪些改动，不要寒暄。"
            ),
        }
    } else {
        match custom_agent {
            Some(def) => format!(
                "你是一个只读探索子代理，工作区路径是 {workspace_path}。你只能使用 list_dir、read_file、search_text、glob_files、stat_path 这些只读工具，禁止写入或命令执行。以下是你的角色定义：\n{def}\n完成后输出简明中文报告。"
            ),
            None => format!(
                "你是一个只读探索子代理，工作区路径是 {workspace_path}。你只能使用 list_dir、read_file、search_text、glob_files、stat_path 这些只读工具调查代码与文件，禁止任何写入或命令执行。完成调查后，输出一份简明、信息密度高的中文报告，直接回答委托内容，不要寒暄。"
            ),
        }
    };

    // 后台模式只允许 read：写模式忽略 background 强制前台，避免无人审批的后台写（Plan25 C-3）。
    let background =
        parsed.get("background").and_then(|v| v.as_bool()).unwrap_or(false) && !write_mode;
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
                    conversation_id: conversation_id.to_string(),
                    report: report.clone(),
                    status: status.clone(),
                    usage: usage_slot.clone(),
                    settled: Arc::new(AtomicBool::new(false)),
                    cancel: cancel.clone(),
                },
            );
        }
        let base_url = base_url.to_string();
        let api_key = api_key.to_string();
        let model = model.to_string();
        let workspace_owned = workspace_path.to_string();
        let conversation_owned = conversation_id.to_string();
        let description_owned = description.to_string();
        let app_bg = app.clone();
        let task_id_done = task_id.clone();
        tauri::async_runtime::spawn(async move {
            // 后台子代理恒为 read 模式（write 已在上面被强制前台），无需门控权限上下文。
            let (text, sub_usage) = run_subtask_loop(
                &base_url,
                &api_key,
                &model,
                &workspace_owned,
                &system_prompt,
                &description_owned,
                &app_bg,
                &conversation_owned,
                Some(cancel.clone()),
                false,
                PermissionMode::Restricted,
                &[],
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
    // write 模式继承主链路 permission 与 permission_rules，门控 + 检查点在 loop 内逐次执行。
    let (report, usage) = run_subtask_loop(
        base_url,
        api_key,
        model,
        workspace_path,
        &system_prompt,
        description,
        app,
        conversation_id,
        Some(cancel.clone()),
        write_mode,
        permission,
        &permission_rules,
    )
    .await;
    (Ok(serde_json::json!({ "report": report })), usage)
}

/// 子代理工具循环主体（前台/后台共用）。后台模式下每轮检查 cancel 以支持 kill_task。
///
/// `write_mode=false`（read）：强制 Restricted + 断网 + 只读工具白名单，工具直接执行。
/// `write_mode=true`（Plan25 C-3，仅前台）：用传入的 `permission` 建安全上下文（继承主链路权限），
/// 工具集含写 / 编辑 / 命令工具；每次写 / 删 / 命令类调用前复用主链路门控
/// （`gate_tool_decision` + `request_tool_approval`，Deny / 拒绝则 `feed_tool_denial` 回灌不执行）
/// 与检查点（`capture_checkpoint_before` / `persist_checkpoint`）；只读工具仍直接执行。
#[allow(clippy::too_many_arguments)]
async fn run_subtask_loop(
    base_url: &str,
    api_key: &str,
    model: &str,
    workspace_path: &str,
    system_prompt: &str,
    description: &str,
    app: &AppHandle,
    conversation_id: &str,
    cancel: Option<Arc<AtomicBool>>,
    write_mode: bool,
    permission: PermissionMode,
    permission_rules: &[String],
) -> (String, Option<mdga_shared::RawUsage>) {
    // read 模式恒为 Restricted + 断网；write 模式继承主链路 permission（网络与主链路一致为 Disabled）。
    let permission_mode = if write_mode { permission } else { PermissionMode::Restricted };
    let security_context = match session_security_context(
        workspace_path.to_string(),
        permission_mode,
        NetworkMode::Disabled,
    ) {
        Ok(ctx) => ctx,
        Err(e) => return (format!("子代理初始化失败: {e}"), None),
    };

    let tool_schemas = if write_mode {
        writable_tool_schemas()
    } else {
        read_only_tool_schemas()
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
            base_url,
            api_key,
            wire.clone(),
            model,
            Some(tool_schemas.clone()),
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
            let args = call.function.arguments.as_str();
            let display_name = format!("sub:{name}");

            // 工具准入：read 模式只放只读工具；write 模式放只读 + 写/编辑/命令工具。
            let tool_allowed = SUBTASK_READ_ONLY_TOOLS.contains(&name)
                || (write_mode && SUBTASK_WRITE_TOOLS.contains(&name));
            if !tool_allowed {
                let reason = if write_mode {
                    "该工具不在可写子代理的工具集内"
                } else {
                    "子任务仅允许只读工具"
                };
                feed_tool_denial(
                    app,
                    conversation_id,
                    &display_name,
                    args,
                    workspace_path,
                    reason,
                    &call.id,
                    &mut wire,
                );
                continue;
            }

            // write 模式下的写 / 删 / 命令类工具：先快照、再门控（Deny/拒绝回灌不执行），与主链路一致。
            let guarded = write_mode && is_guarded_write_tool(name);
            let capture = if guarded {
                capture_checkpoint_before(workspace_path, name, args)
            } else {
                None
            };
            if guarded {
                let decision = gate_tool_decision(&security_context, name, args, permission_rules);
                let proceed = match decision {
                    ToolGate::Allow => true,
                    ToolGate::Deny(reason) => {
                        feed_tool_denial(
                            app,
                            conversation_id,
                            &display_name,
                            args,
                            workspace_path,
                            &reason,
                            &call.id,
                            &mut wire,
                        );
                        false
                    }
                    ToolGate::Ask => {
                        let approved = request_tool_approval(app, name, args, false).await;
                        if !approved {
                            feed_tool_denial(
                                app,
                                conversation_id,
                                &display_name,
                                args,
                                workspace_path,
                                "用户拒绝了该操作",
                                &call.id,
                                &mut wire,
                            );
                        }
                        approved
                    }
                };
                if !proceed {
                    continue;
                }
            }

            record_tool_event(
                app,
                conversation_id,
                "tool_started",
                &display_name,
                "running",
                args,
                None,
                None,
                workspace_path,
            );
            // 只读工具与已放行的写工具统一走内建执行器（execute_builtin_tool_call 内部再做工具名校验）。
            let result = execute_builtin_tool_call(&security_context, name, args);
            let (output, status, error) = match &result {
                Ok(value) => {
                    // 写工具成功后落检查点（同主链路），供后续回退。
                    if let Some(cap) = capture.as_ref() {
                        persist_checkpoint(app, conversation_id, name, cap);
                    }
                    (
                        serde_json::json!({ "ok": true, "result": value }),
                        "succeeded",
                        None,
                    )
                }
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
                args,
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
pub(crate) async fn execute_bg_task_tool(
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
