//! 子代理与后台任务：run_subtask 子代理（0.0.65 起**默认可干活**——可读/写/跑命令、受主链路权限门控
//! + 检查点保护；`mode:"read"` 或 background 时锁为只读探索沙箱）与后台任务控制工具
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

/// 纯函数（0.0.65）：据 `mode` 与是否请求 `background`，决定子任务的 `(write_mode, background)`。
///
/// 安全不变量：
/// - 默认（无 `mode`）= **可干活**（write_mode=true）——与主代理同权限、可读写跑命令、逐次审批 + 检查点；
/// - 显式 `mode:"read"` ⇒ 只读探索沙箱（write_mode=false）；
/// - 请求 `background` ⇒ **强制只读**（异步无法交互审批），故默认子任务遇 background 降为只读；
/// - 但显式 `mode:"write"` 始终可写且**忽略 background**（强制前台，保证每个写/命令都能被审批）。
fn resolve_subtask_modes(mode: Option<&str>, background_requested: bool) -> (bool, bool) {
    let write_mode = matches!(mode, Some("write"))
        || (!matches!(mode, Some("read")) && !background_requested);
    // background 只在只读子任务上成立：可写子任务一律前台，避免无人审批的后台写。
    let background = background_requested && !write_mode;
    (write_mode, background)
}

const SUBTASK_MAX_ROUNDS: usize = 15;

/// run_subtask 工具：用独立上下文跑一个探索 / 工作子代理，返回简明报告与消耗的 usage。
///
/// 可干活模式（0.0.65 起的**默认**；显式 `mode:"write"` 同此）：子代理可读 / 写 / 编辑 / 跑命令，
/// 继承主链路传入的 `permission`，每次写 / 删 / 命令类调用都复用主链路的门控（`gate_tool_decision`
/// + `request_tool_approval`,含「不可回退」强制审批）与检查点（`capture_checkpoint_before` /
/// `persist_checkpoint`），命令走沙箱感知路径（`execute_run_command_tool`，遵循 command_sandbox 开关），
/// **绝不绕过权限/沙箱**。
/// read 模式（显式 `mode:"read"` 或请求 background 时）：子代理只能 list/read/search/stat，强制 Restricted + 断网。
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
    // mode（0.0.65 翻转默认）：**默认「可干活」**——子代理可读 / 写 / 跑命令,与主代理**同一套权限门控
    // + 检查点**(每个写 / 删 / 命令逐次审批、可回退),避免「默认只读 → 跑不了命令」这个让用户困惑的陷阱。
    // 仅显式 `mode:"read"` 才锁成只读探索沙箱(Restricted + 断网 + 只读工具)。`background` 异步无法交互
    // 审批,故默认子任务一旦请求 background 自动降为只读;但**显式 `mode:"write"` 保持前台可写、忽略 background**。
    let mode_str = parsed.get("mode").and_then(|v| v.as_str());
    let background_requested = parsed.get("background").and_then(|v| v.as_bool()).unwrap_or(false);
    let (write_mode, background) = resolve_subtask_modes(mode_str, background_requested);
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

    // background 由 resolve_subtask_modes 算出（仅在只读子任务上为真；可写子任务强制前台以便逐次审批）。
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
        // 0.0.68：子代理上下文同样携带命令沙箱开关快照(与主代理一致)。
        app.state::<AppState>().command_sandbox.load(Ordering::SeqCst),
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
            // 思考深度（C）：子代理循环不做 reasoning 回传（无 profile/echo 上下文），传 None 保持原行为。
            None,
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
            // 不可回退（无法快照原内容，如 delete_dir / 删已存在文件快照失败）：与主链路一致——
            // 即便被自动放行(Allow)也强制一次「不可回退」额外审批，审批弹窗带警告标(0.0.65 补齐)。
            let irreversible = capture.as_ref().map(|c| !c.revertible).unwrap_or(false);
            if guarded {
                let decision = gate_tool_decision(&security_context, name, args, permission_rules);
                let proceed = match decision {
                    ToolGate::Allow => {
                        if irreversible {
                            let approved = request_tool_approval(app, name, args, true).await;
                            if !approved {
                                feed_tool_denial(
                                    app,
                                    conversation_id,
                                    &display_name,
                                    args,
                                    workspace_path,
                                    "用户拒绝了该不可回退操作",
                                    &call.id,
                                    &mut wire,
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
                        let approved = request_tool_approval(app, name, args, irreversible).await;
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
            // 但 run_command 必须走与主代理一致的**沙箱感知**路径（读用户 command_sandbox 开关 + 会话网络
            // 模式）——否则默认可干活子代理会在用户开了沙箱的情况下裸跑命令(隔离降级,与 R3 修复同因)。
            let result = if name == "run_command" {
                crate::command_run::execute_run_command_tool(app, &security_context, args)
            } else {
                execute_builtin_tool_call(&security_context, name, args)
            };
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

// ── R10：可写子代理的 git-worktree 隔离集成缝（GUARDED / opt-in，默认不接管现有路径） ──
//
// 现状：上面的 write 模式子代理与主链路**共用同一个工作区**，多个写子代理并行 fan-out 会互踩。
// 下面这个 helper 提供安全的隔离编排——但**它不被默认 run_subtask 路径调用**：默认写子代理仍走
// run_subtask_loop（原行为不变）。要启用隔离并行写，由调用方显式调 `run_isolated_write_subtask`。
//
// 为什么是显式 opt-in：把默认路径静默改道到隔离工作树是高风险的（合并冲突、清理时序、与主链路
// 检查点/权限门控的交互都需要更宽的契约）。本缝先把「隔离 + 合并 + 清理」这一最危险的 git 管道做对、
// 做实测，留作可被上层（如未来的并行 fan-out 编排器）调用的积木；端到端自动接管留作后续增量。

/// 一次隔离写子代理编排的结果：报告 + 消耗 + 合并结局（干净 / 冲突路径）。
///
/// `merge` 为 `None` 表示因（A）未开启自动合并或（B）子代理未在隔离分支上产生任何提交而未尝试合并；
/// `Some(MergeOutcome::Conflict { .. })` 表示合并冲突已被**结构化抛出**，隔离分支与改动均**保留**待
/// 人工处理（此时不清理工作树/分支，交由调用方决定），其余情况隔离工作树在函数返回时经 RAII 清理。
#[allow(dead_code)] // opt-in 积木的返回类型：默认路径不消费它，留给上层并行 fan-out 编排器。
pub(crate) struct IsolatedSubtaskResult {
    pub report: String,
    pub usage: Option<mdga_shared::RawUsage>,
    pub merge: Option<mdga_tool_runtime::MergeOutcome>,
    /// 冲突保留时，隔离分支名（供调用方提示用户人工合并）；非冲突保留时为 None。
    pub retained_branch: Option<String>,
}

/// GUARDED / opt-in：把一个**可写**子代理跑在隔离的 git 工作树里，完成后把它的改动合并回父分支，
/// 冲突一律向上抛出（绝不静默强解、绝不 force）。隔离工作树/临时分支由 RAII 守卫清理。
///
/// 流程：
/// 1. 在 `workspace_path`（须为 git 仓库工作树根）当前 HEAD 上创建隔离工作树 + 临时分支。
/// 2. 让 write 模式子代理 loop 跑在**隔离目录**里（其文件改动只落隔离工作树，不碰主工作区）。
/// 3. 子代理结束后，在隔离工作树里 `add -A && commit` 把改动固化到隔离分支（无改动则跳过提交与合并）。
/// 4. 若 `auto_merge` 且确有提交：把隔离分支合并回 `target_branch`（默认父仓库当前分支）。
///    - 干净合并：返回 `Clean`，随后 RAII 清理隔离工作树/分支。
///    - 冲突：返回 `Conflict { paths }` 并**保留**隔离工作树/分支（`std::mem::forget` 守卫），
///      交由调用方提示用户人工 `git merge`；绝不静默选边。
///
/// 安全：合并/清理全部复用 tool-runtime 的 `IsolatedWorktree`（绝对路径、`-c core.autocrlf=false`、
/// 绝不 force、绝不 `-X ours|theirs`）。本函数不改动权限门控语义——子代理 loop 内仍逐次门控 + 检查点。
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)] // opt-in 积木：默认路径不调用它，留给上层并行 fan-out 编排器接入。
pub(crate) async fn run_isolated_write_subtask(
    base_url: &str,
    api_key: &str,
    model: &str,
    workspace_path: &str,
    system_prompt: &str,
    description: &str,
    app: &AppHandle,
    conversation_id: &str,
    cancel: Option<Arc<AtomicBool>>,
    permission: PermissionMode,
    permission_rules: &[String],
    label: &str,
    auto_merge: bool,
    target_branch: Option<&str>,
    commit_message: &str,
) -> Result<IsolatedSubtaskResult, String> {
    // 1. 创建隔离工作树（失败即清晰报错，不残留半成品）。
    // 不需 mut：提交/合并都走 &self，清理交给 RAII Drop（或冲突路径上的 mem::forget）。
    let guard = mdga_tool_runtime::IsolatedWorktree::create(workspace_path, label)
        .map_err(|e| format!("创建隔离工作树失败: {e}"))?;
    let isolated_path = guard.path().to_string_lossy().to_string();

    // 2. 写子代理 loop 跑在隔离目录里（write_mode=true，继承主链路权限；门控/检查点在 loop 内逐次执行）。
    let (report, usage) = run_subtask_loop(
        base_url,
        api_key,
        model,
        &isolated_path,
        system_prompt,
        description,
        app,
        conversation_id,
        cancel,
        true,
        permission,
        permission_rules,
    )
    .await;

    // 3. 把隔离工作树里的改动固化到隔离分支；无改动则不提交、也不合并（guard 在返回时清理）。
    let committed = match guard.commit_all(commit_message) {
        Ok(_hash) => true,
        Err(e) => {
            // 「nothing to commit」属正常无改动；其余错误也只记入报告，不阻断（仍走清理）。
            let _ = e;
            false
        }
    };

    if !committed || !auto_merge {
        return Ok(IsolatedSubtaskResult {
            report,
            usage,
            merge: None,
            retained_branch: None,
        });
    }

    // 4. 合并回目标分支（默认父仓库当前分支）。冲突一律上抛，绝不强解。
    let outcome = match target_branch {
        Some(b) => guard.merge_into(b),
        None => {
            // 未指定目标：取父仓库当前分支作为合并目标。
            match current_repo_branch(workspace_path) {
                Ok(b) => guard.merge_into(&b),
                Err(e) => Err(e),
            }
        }
    };

    match outcome {
        Ok(mdga_tool_runtime::MergeOutcome::Conflict { paths }) => {
            // 冲突：保留隔离工作树 + 分支，交由用户人工处理；forget 守卫以阻止 Drop 清理。
            let branch = guard.branch().to_string();
            std::mem::forget(guard);
            Ok(IsolatedSubtaskResult {
                report,
                usage,
                merge: Some(mdga_tool_runtime::MergeOutcome::Conflict { paths }),
                retained_branch: Some(branch),
            })
        }
        Ok(clean) => {
            // 干净合并：guard 在返回时 RAII 清理隔离工作树/分支。
            Ok(IsolatedSubtaskResult {
                report,
                usage,
                merge: Some(clean),
                retained_branch: None,
            })
        }
        Err(e) => Err(format!("合并隔离分支失败: {e}")),
    }
}

/// 取某 git 仓库工作树当前分支名（detached 时返回错误，调用方据此要求显式指定 target）。
fn current_repo_branch(repo: &str) -> Result<String, mdga_tool_runtime::ToolRuntimeError> {
    // 复用 git_branch(list) 的 current 字段，避免在 subagent 里重造 git 解析。
    let res = mdga_tool_runtime::git_branch(
        repo,
        mdga_tool_runtime::GitBranchRequest {
            action: Some("list".to_string()),
            ..Default::default()
        },
    )?;
    res.current.ok_or_else(|| {
        mdga_tool_runtime::ToolRuntimeError::CommandFailed(
            "父仓库处于 detached HEAD，请显式指定合并目标分支".to_string(),
        )
    })
}

// ── P1（0.0.58）：并行可写子代理编排器（EXPLICIT / opt-in，默认 run_subtask 路径绝不改道） ──
//
// 这是建立在 R10 `IsolatedWorktree` 原语之上的**显式编排器**：把 N 个可写子代理各跑在自己的隔离
// git 工作树里（并发 fan-out，互不踩文件），全部结束后把它们的分支**串行**合并回父分支；任一分支
// 合并出现冲突，立刻**停止**后续合并并把「哪个子代理、冲突在哪些文件」结构化抛回给用户/模型——
// **绝不**自动选边、**绝不** force、**绝不** `-X ours|theirs`。冲突由原语的 `merge --abort` 还原父
// 工作树，编排器不再触碰它。
//
// 为什么独立入口、显式触发：默认 `run_subtask` 单子代理路径与主链路共用工作区、行为已稳定，静默
// 改道到并行隔离写是高风险的（合并时序、清理、与主链路检查点/门控的交互）。本编排器是一个**新的、
// 必须被显式调用**的内部函数（由上层 `run_parallel_subtasks` 工具触发），不被任何既有路径自动调用。
//
// 安全前置（pre-flight）：父仓库必须是 git 工作树、当前在某个**具名分支**（非 detached）、工作树
// **干净**。任一不满足，编排器**拒绝运行**并回清晰错误——绝不在脏树/分离头上强行合并污染用户工作区。

/// 单个并行子代理的运行 + 合并结局。
#[allow(dead_code)] // 经由 run_parallel_subtasks 工具消费；字段为结构化结果，部分仅在特定分支填充。
#[derive(Debug)]
pub(crate) struct ParallelSubtaskItem {
    /// 该子代理的人读标签（也是其隔离分支/目录名 slug 的来源）。
    pub label: String,
    /// 子代理产出的简明报告。
    pub report: String,
    /// 该子代理是否在隔离分支上产生了提交（无改动 => false，跳过合并）。
    pub committed: bool,
    /// 合并结局：`None`=未尝试合并（无提交，或前序已冲突而停止）；`Some(Clean)`=已干净并入父分支；
    /// `Some(Conflict{paths})`=合并冲突（已 abort 还原父工作树，列出冲突文件，待人工处理）。
    pub merge: Option<mdga_tool_runtime::MergeOutcome>,
    /// 冲突时保留的隔离分支名（供用户人工 `git merge`）；其余情况为 None（工作树/分支已 RAII 清理）。
    pub retained_branch: Option<String>,
}

/// 并行编排的整体结果：逐子代理明细 + 合并到的目标分支 + 合计 usage + 是否因冲突中止。
#[allow(dead_code)] // 由 run_parallel_subtasks 工具序列化回模型；为结构化编排报告。
#[derive(Debug)]
pub(crate) struct ParallelOrchestrationResult {
    pub target_branch: String,
    pub items: Vec<ParallelSubtaskItem>,
    pub usage: Option<mdga_shared::RawUsage>,
    /// 是否存在冲突导致提前停止合并（true 时其后子代理的 merge 为 None，分支被保留待人工处理）。
    pub stopped_on_conflict: bool,
    /// 是否因「合并错误」（非冲突，如 merge --abort 失败/锁/IO）停止：true 时父工作树**可能仍不干净**，
    /// 需人工 `git merge --abort` / 清理后再处理保留分支——故此时绝不向用户宣称「已还原干净」。
    pub stopped_on_error: bool,
}

/// 一个待编排的并行子代理：标签 + 委托描述。
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct ParallelSubtaskSpec {
    pub label: String,
    pub description: String,
}

/// 并发阶段每个子代理跑完后的中间产物（持有隔离工作树守卫，供后续串行合并/清理）。
struct PreparedSubtask {
    guard: mdga_tool_runtime::IsolatedWorktree,
    label: String,
    report: String,
    committed: bool,
}

/// 子代理 system prompt：可写隔离工作子代理（与 run_subtask write 模式同口径，但点明在隔离工作树里）。
fn isolated_write_system_prompt(workspace_path: &str) -> String {
    format!(
        "你是一个可写工作子代理，运行在一个**隔离的 git 工作树**目录里（路径 {workspace_path}），\
你的所有改动只发生在这个隔离目录、稍后会被合并回主分支。除只读工具（list_dir、read_file、\
search_text、glob_files、stat_path）外，你还可以使用 create_file、write_file、edit_file、\
apply_patch、make_dir、move_path、delete_file、delete_dir、run_command 真正改动文件或执行命令。\
每次写 / 删 / 命令操作都会经过用户权限门控与检查点保护。请严格聚焦被委托的范围、改前先读、\
优先用 edit_file/apply_patch 精确修改，**只动与你的任务直接相关的文件**（与其他并行子代理改\
同一文件会造成合并冲突）。完成后输出一份简明、信息密度高的中文报告，说明你做了哪些改动。"
    )
}

/// EXPLICIT / opt-in：并行可写子代理编排器。把 `specs` 里的每个子代理各跑在独立隔离工作树里
/// （并发），全部结束后**串行**把它们的分支合并回父分支；**任一冲突立即停止并向上抛出**。
///
/// 流程：
/// 1. **pre-flight**：校验 `workspace_path` 是 git 工作树、当前在具名分支（非 detached）、工作树干净。
///    任一不满足，直接回 `Err`（绝不在脏树/分离头上强行合并）。`specs` 为空也回 `Err`。
/// 2. **并发阶段**：为每个 spec 在当前 HEAD 上创建一个 `IsolatedWorktree`（各自独立分支+目录）。
///    任一创建失败，已建的守卫随作用域 RAII 清理后回 `Err`。随后用 `join_all` **并发**地把每个
///    可写子代理 loop 跑在它自己的隔离目录里（write_mode=true，门控/检查点在 loop 内逐次执行），
///    跑完在隔离工作树里 `add -A && commit` 把改动固化到隔离分支（无改动则 committed=false、不合并）。
/// 3. **串行合并阶段**：按 `specs` 顺序逐个 `merge_into(target)`：
///    - 干净：记 `Clean`，该守卫随后 RAII 清理（工作树/临时分支移除）。
///    - 冲突：记 `Conflict{paths}`，`mem::forget` 该守卫以**保留**其分支供人工处理，置
///      `stopped_on_conflict=true`，**停止**合并其余子代理（其余记 merge=None，并保留它们的分支
///      也供人工查看），整体仍 `Ok` 返回（冲突是结构化结果而非错误）。
///    - 其它 merge 错误：同样视为停止信号，保留剩余分支，记录错误进对应 item 报告。
///
/// 安全不变量：合并/清理全程复用 `IsolatedWorktree`（绝对路径、`-c core.autocrlf=false`、绝不
/// force、绝不 `-X ours|theirs`、冲突 `merge --abort` 还原父工作树）。本函数不改动权限门控语义。
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)] // 由 run_parallel_subtasks 工具显式调用；不被任何既有默认路径自动触发。
pub(crate) async fn run_parallel_write_subtasks(
    base_url: &str,
    api_key: &str,
    model: &str,
    workspace_path: &str,
    specs: &[ParallelSubtaskSpec],
    app: &AppHandle,
    conversation_id: &str,
    cancel: Option<Arc<AtomicBool>>,
    permission: PermissionMode,
    permission_rules: &[String],
    commit_message_prefix: &str,
) -> Result<ParallelOrchestrationResult, String> {
    if specs.is_empty() {
        return Err("并行编排至少需要一个子任务".to_string());
    }
    // 标签去重（用于稳定地把合并结果归位到 spec；slug 冲突由原语的唯一 nonce 兜底，这里只防空）。
    if specs.iter().any(|s| s.description.trim().is_empty()) {
        return Err("每个并行子任务都需要非空 description".to_string());
    }

    // 1. pre-flight：必须是 git 工作树、具名分支（非 detached）、工作树干净。
    let status = mdga_tool_runtime::git_status(
        workspace_path,
        mdga_tool_runtime::GitStatusRequest::default(),
    )
    .map_err(|e| format!("并行编排前置检查失败（无法读取 git 状态，需在 git 仓库内）: {e}"))?;
    let target_branch = match &status.branch {
        Some(b) => b.clone(),
        None => {
            return Err(
                "父仓库处于 detached HEAD，无法安全合并并行子代理结果；请先切到一个具名分支再试"
                    .to_string(),
            )
        }
    };
    if !status.clean {
        return Err(format!(
            "父仓库工作树不干净（有未提交改动），并行编排为避免污染你的改动而拒绝运行；\
请先提交或暂存当前改动后再发起并行子代理（目标分支 {target_branch}）"
        ));
    }

    // 2. 并发阶段：先创建全部隔离工作树（任一失败则已建的随 Vec 作用域 RAII 清理后回错）。
    let system_prompt = isolated_write_system_prompt(workspace_path);
    let mut guards: Vec<mdga_tool_runtime::IsolatedWorktree> = Vec::with_capacity(specs.len());
    for spec in specs {
        match mdga_tool_runtime::IsolatedWorktree::create(workspace_path, &spec.label) {
            Ok(g) => guards.push(g),
            Err(e) => {
                // guards 在此函数返回时 Drop，自动清理已建的隔离工作树/分支——不泄漏。
                return Err(format!(
                    "为并行子代理 '{}' 创建隔离工作树失败: {e}",
                    spec.label
                ));
            }
        }
    }

    // 并发跑每个可写子代理 loop（各自隔离目录）。join_all 等全部完成后再进入串行合并阶段。
    // 注意：cancel 在各子代理间共享（kill 主链路会一并停下所有并行子代理）。
    let isolated_paths: Vec<String> = guards
        .iter()
        .map(|g| g.path().to_string_lossy().to_string())
        .collect();
    let run_futs = specs.iter().zip(isolated_paths.iter()).map(|(spec, iso_path)| {
        let sp = system_prompt.clone();
        let desc = spec.description.clone();
        let perm = permission.clone();
        let rules = permission_rules.to_vec();
        let cancel = cancel.clone();
        async move {
            run_subtask_loop(
                base_url,
                api_key,
                model,
                iso_path,
                &sp,
                &desc,
                app,
                conversation_id,
                cancel,
                true, // write_mode：隔离工作树内可写，门控/检查点照常逐次执行
                perm,
                &rules,
            )
            .await
        }
    });
    let run_results = futures_util::future::join_all(run_futs).await;

    // 把每个子代理结果固化到其隔离分支（无改动则不提交、不参与合并）。
    let mut total_usage: Option<mdga_shared::RawUsage> = None;
    let mut prepared: Vec<PreparedSubtask> = Vec::with_capacity(specs.len());
    for ((spec, guard), (report, usage)) in specs.iter().zip(guards).zip(run_results) {
        total_usage = merge_usage(total_usage, usage);
        let commit_msg = format!("{commit_message_prefix}{}", spec.label);
        let committed = guard.commit_all(&commit_msg).is_ok();
        prepared.push(PreparedSubtask {
            guard,
            label: spec.label.clone(),
            report,
            committed,
        });
    }

    // 3. 串行合并阶段：逐个把已提交的隔离分支合并回 target_branch；任一冲突/错误即停止其余合并。
    let mut items: Vec<ParallelSubtaskItem> = Vec::with_capacity(prepared.len());
    let mut stopped = false;
    // 区分两种停止：真冲突（原语已 merge --abort 确认还原干净）vs 合并错误（abort 失败/锁/IO，
    // 父工作树可能仍不干净）——后者绝不能向用户谎报「已还原干净」。
    let mut merge_error = false;
    for p in prepared {
        if !p.committed {
            // 无改动：不合并。守卫在 item push 后随 p 被 Drop 清理（隔离工作树/分支移除）。
            items.push(ParallelSubtaskItem {
                label: p.label,
                report: p.report,
                committed: false,
                merge: None,
                retained_branch: None,
            });
            continue;
        }
        if stopped {
            // 已因前序冲突停止：保留本分支供人工查看，不再尝试合并（forget 守卫以阻止 Drop 清理）。
            let branch = p.guard.branch().to_string();
            std::mem::forget(p.guard);
            items.push(ParallelSubtaskItem {
                label: p.label,
                report: p.report,
                committed: true,
                merge: None,
                retained_branch: Some(branch),
            });
            continue;
        }
        match p.guard.merge_into(&target_branch) {
            Ok(mdga_tool_runtime::MergeOutcome::Conflict { paths }) => {
                // 冲突：原语已 merge --abort 还原父工作树。保留本隔离分支供人工处理，置停止信号。
                stopped = true;
                let branch = p.guard.branch().to_string();
                std::mem::forget(p.guard);
                items.push(ParallelSubtaskItem {
                    label: p.label,
                    report: p.report,
                    committed: true,
                    merge: Some(mdga_tool_runtime::MergeOutcome::Conflict { paths }),
                    retained_branch: Some(branch),
                });
            }
            Ok(clean) => {
                // 干净合并：守卫随 p 离开作用域 Drop 清理隔离工作树/分支。
                items.push(ParallelSubtaskItem {
                    label: p.label,
                    report: p.report,
                    committed: true,
                    merge: Some(clean),
                    retained_branch: None,
                });
            }
            Err(e) => {
                // 非冲突类合并错误（如 merge --abort 失败/锁/IO）：保守起见也视为停止信号，保留分支
                // 并记入报告；置 merge_error 让上层提示「父工作树可能不干净、需人工清理」，不谎报已还原。
                stopped = true;
                merge_error = true;
                let branch = p.guard.branch().to_string();
                std::mem::forget(p.guard);
                items.push(ParallelSubtaskItem {
                    label: p.label,
                    report: format!("{}\n[合并失败]: {e}", p.report),
                    committed: true,
                    merge: None,
                    retained_branch: Some(branch),
                });
            }
        }
    }

    Ok(ParallelOrchestrationResult {
        target_branch,
        items,
        usage: total_usage,
        stopped_on_conflict: stopped && !merge_error,
        stopped_on_error: merge_error,
    })
}

/// 并行编排器一次最多并发的子代理数（保守上限，防一次拉太多 worktree 拖垮磁盘/并发）。
const MAX_PARALLEL_SUBTASKS: usize = 4;

/// 纯函数：从 `run_parallel_subtasks` 的 JSON 参数解析出子任务规格（含上限/空值校验）。
///
/// 抽成纯函数便于单测「上限、空数组、缺/空 description、label 缺省」等校验分支（无需 AppHandle/LLM）。
/// 校验失败回 `Err(清晰中文原因)`；成功回归一化后的 spec 列表。
fn parse_parallel_subtask_specs(
    parsed: &serde_json::Value,
) -> Result<Vec<ParallelSubtaskSpec>, String> {
    let Some(arr) = parsed.get("subtasks").and_then(|v| v.as_array()) else {
        return Err("run_parallel_subtasks 缺少 subtasks 数组".to_string());
    };
    if arr.is_empty() {
        return Err("subtasks 不能为空".to_string());
    }
    if arr.len() > MAX_PARALLEL_SUBTASKS {
        return Err(format!(
            "并行子任务数 {} 超过上限 {MAX_PARALLEL_SUBTASKS}；请减少数量或分批",
            arr.len()
        ));
    }
    let mut specs: Vec<ParallelSubtaskSpec> = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let Some(description) = item.get("description").and_then(|v| v.as_str()) else {
            return Err(format!("subtasks[{i}] 缺少 description"));
        };
        if description.trim().is_empty() {
            return Err(format!("subtasks[{i}] 的 description 为空"));
        }
        // label 可选：缺省用序号；仅用于分支/目录命名（原语会再做 sanitize + 唯一 nonce）。
        let label = item
            .get("label")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("task-{}", i + 1));
        specs.push(ParallelSubtaskSpec {
            label,
            description: description.to_string(),
        });
    }
    Ok(specs)
}

/// `run_parallel_subtasks` 工具入口：解析参数 → 调用 `run_parallel_write_subtasks` 并行编排 →
/// 把结构化编排结果序列化回模型（含每个子代理的报告、是否合并、冲突文件、保留分支）。
///
/// 与 `execute_run_subtask` 同形：返回 `(Result<Value>, Option<usage>)`，usage 由调用点并入会话账本。
/// 这是一个**显式 opt-in** 入口——模型只有主动调用 `run_parallel_subtasks` 才会走并行隔离写，
/// 既有 `run_subtask`（单子代理，默认路径）行为完全不受影响。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_run_parallel_subtasks(
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
    let specs = match parse_parallel_subtask_specs(&parsed) {
        Ok(s) => s,
        Err(e) => return (Err(e), None),
    };

    let result = run_parallel_write_subtasks(
        base_url,
        api_key,
        model,
        workspace_path,
        &specs,
        app,
        conversation_id,
        Some(cancel.clone()),
        permission,
        &permission_rules,
        "subagent: ",
    )
    .await;

    match result {
        Ok(orch) => {
            let usage = orch.usage.clone();
            let items: Vec<serde_json::Value> = orch
                .items
                .iter()
                .map(|it| {
                    let (merge_state, conflict_paths) = match &it.merge {
                        Some(mdga_tool_runtime::MergeOutcome::Clean { .. }) => {
                            ("merged_clean", Vec::new())
                        }
                        Some(mdga_tool_runtime::MergeOutcome::Conflict { paths }) => {
                            ("conflict", paths.clone())
                        }
                        None => {
                            if it.committed {
                                // 有提交但 merge=None：因前序冲突而被跳过（分支已保留）。
                                ("skipped_after_conflict", Vec::new())
                            } else {
                                ("no_changes", Vec::new())
                            }
                        }
                    };
                    serde_json::json!({
                        "label": it.label,
                        "report": it.report,
                        "mergeState": merge_state,
                        "conflictPaths": conflict_paths,
                        "retainedBranch": it.retained_branch,
                    })
                })
                .collect();
            let note = if orch.stopped_on_error {
                // 合并错误（含 merge --abort 失败）：父工作树可能仍残留半合并/冲突状态——不谎报已还原。
                "合并过程中发生错误（非内容冲突，如 merge --abort 失败 / 索引锁 / IO）：已停止后续合并，\
但**父工作树可能仍残留半合并或冲突状态**。请先在目标工作区手动执行 `git merge --abort` 并 `git status` \
确认干净后，再查看各 subtask 报告里的 [合并失败] 原因与 retainedBranch 决定如何处理。"
            } else if orch.stopped_on_conflict {
                "存在合并冲突：已在第一个冲突处停止后续合并，父工作树被还原干净（绝未强解/选边）。\
请查看 conflictPaths 与 retainedBranch——可手动 `git merge <retainedBranch>` 解决冲突，\
或让对应子代理重做以避开冲突文件。冲突子代理及其后未合并的子代理分支均已保留待你处理。"
            } else {
                "全部子代理已干净合并回目标分支（或无改动）。隔离工作树/临时分支已清理。"
            };
            (
                Ok(serde_json::json!({
                    "targetBranch": orch.target_branch,
                    "stoppedOnConflict": orch.stopped_on_conflict,
                    "stoppedOnError": orch.stopped_on_error,
                    "subtasks": items,
                    "note": note,
                })),
                usage,
            )
        }
        Err(e) => (Err(e), None),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    // 0.0.65：run_subtask 模式判定（翻转默认为「可干活」）的安全不变量测试。
    #[test]
    fn subtask_mode_defaults_to_capable() {
        // 默认（无 mode、无 background）= 可干活、前台。
        assert_eq!(resolve_subtask_modes(None, false), (true, false));
        // 显式 write 同样可干活、前台。
        assert_eq!(resolve_subtask_modes(Some("write"), false), (true, false));
    }
    #[test]
    fn subtask_mode_read_is_readonly() {
        // 显式 read = 只读、前台。
        assert_eq!(resolve_subtask_modes(Some("read"), false), (false, false));
        // read + background = 只读、后台。
        assert_eq!(resolve_subtask_modes(Some("read"), true), (false, true));
    }
    #[test]
    fn subtask_background_forces_readonly() {
        // 默认 + 请求 background ⇒ 强制降为只读、真后台（异步无法审批）。
        assert_eq!(resolve_subtask_modes(None, true), (false, true));
        // 但显式 write + background ⇒ 仍可写、强制前台（忽略 background，保证可审批）。
        assert_eq!(resolve_subtask_modes(Some("write"), true), (true, false));
    }

    // P1（0.0.58）：并行编排器参数解析/校验的纯逻辑测试（无需 AppHandle/LLM/git）。

    #[test]
    fn parse_specs_requires_subtasks_array() {
        let v = serde_json::json!({});
        assert!(parse_parallel_subtask_specs(&v).is_err());
        let v = serde_json::json!({ "subtasks": "nope" });
        assert!(parse_parallel_subtask_specs(&v).is_err());
    }

    #[test]
    fn parse_specs_rejects_empty_array() {
        let v = serde_json::json!({ "subtasks": [] });
        let err = parse_parallel_subtask_specs(&v).unwrap_err();
        assert!(err.contains("不能为空"), "应报空数组: {err}");
    }

    #[test]
    fn parse_specs_enforces_max_cap() {
        // 超过 MAX_PARALLEL_SUBTASKS 应被拒（保守上限，防一次拉太多 worktree）。
        let items: Vec<serde_json::Value> = (0..(MAX_PARALLEL_SUBTASKS + 1))
            .map(|i| serde_json::json!({ "description": format!("do {i}") }))
            .collect();
        let v = serde_json::json!({ "subtasks": items });
        let err = parse_parallel_subtask_specs(&v).unwrap_err();
        assert!(err.contains("超过上限"), "应报超过上限: {err}");

        // 恰好等于上限：允许。
        let items: Vec<serde_json::Value> = (0..MAX_PARALLEL_SUBTASKS)
            .map(|i| serde_json::json!({ "description": format!("do {i}") }))
            .collect();
        let v = serde_json::json!({ "subtasks": items });
        let specs = parse_parallel_subtask_specs(&v).expect("恰好上限应通过");
        assert_eq!(specs.len(), MAX_PARALLEL_SUBTASKS);
    }

    #[test]
    fn parse_specs_rejects_missing_or_empty_description() {
        let v = serde_json::json!({ "subtasks": [{ "label": "x" }] });
        assert!(parse_parallel_subtask_specs(&v).is_err(), "缺 description 应拒");
        let v = serde_json::json!({ "subtasks": [{ "description": "   " }] });
        let err = parse_parallel_subtask_specs(&v).unwrap_err();
        assert!(err.contains("为空"), "空白 description 应拒: {err}");
    }

    #[test]
    fn parse_specs_defaults_label_and_keeps_description() {
        let v = serde_json::json!({
            "subtasks": [
                { "description": "task A" },
                { "label": "custom", "description": "task B" }
            ]
        });
        let specs = parse_parallel_subtask_specs(&v).expect("应解析");
        assert_eq!(specs.len(), 2);
        // 第一个无 label：缺省 task-1。
        assert_eq!(specs[0].label, "task-1");
        assert_eq!(specs[0].description, "task A");
        // 第二个用自定义 label。
        assert_eq!(specs[1].label, "custom");
        assert_eq!(specs[1].description, "task B");
    }

    #[test]
    fn parse_specs_blank_label_falls_back_to_index() {
        let v = serde_json::json!({
            "subtasks": [{ "label": "   ", "description": "x" }]
        });
        let specs = parse_parallel_subtask_specs(&v).expect("应解析");
        assert_eq!(specs[0].label, "task-1", "空白 label 应回退为序号");
    }
}
