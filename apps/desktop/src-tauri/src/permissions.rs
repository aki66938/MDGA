//! 权限裁决与用户交互：能力分级、规则匹配（allow/deny + glob）、门控决策、审批弹窗、
//! ask_user 结构化提问、拒绝回灌。
//!
//! 从 main.rs 抽出（Plan16 阶段2）：纯逻辑搬移，无行为变更。feed_tool_denial 复用
//! main.rs 的 record_tool_event（活动事件落库后续再独立）。

use crate::record_tool_event;
use crate::state::{AppState, APPROVAL_SEQ, QUESTION_SEQ};
use mdga_sandbox_runtime::{
    decide_tool_access, is_low_risk_command, SessionSecurityContext, ToolCapability, ToolDecision,
};
use mdga_tool_runtime::RunCommandRequest;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;

/// 单次工具调用的权限门控结果。
pub(crate) enum ToolGate {
    /// 直接放行执行。
    Allow,
    /// 需要用户逐次审批。
    Ask,
    /// 当前权限模式直接拒绝，附带原因。
    Deny(String),
}

/// 把工具名映射到能力等级（只读 / 写 / 删 / 命令 / 网络）；未知工具报错。
pub(crate) fn tool_capability_for_name(tool_name: &str) -> Result<ToolCapability, String> {
    // MCP 外部工具统一按网络能力裁决：Workspace Auto 下逐次审批，Full Access 放行。
    if tool_name.starts_with("mcp_") {
        return Ok(ToolCapability::NetworkAccess);
    }
    match tool_name {
        // 只读或纯 UI / 后台控制类工具：自动放行，不打断用户。remember 仅追加项目记忆文件，低风险。
        // code_overview（Plan28 P0-2，Lane B 新增）只读取并统计代码结构，与 read_file / search_text 同列。
        // repo_map（R2）只解析源码构建符号地图，同属只读。
        // code_search（R2 L 阶段）本地语义检索源码、回代码块,默认离线无副作用,同属只读。
        //   P2/0.0.58:当**用户在设置里显式开启** embedding 重排且已配置主 provider 时,它会额外向
        //   **用户自己已配置、已信任的主 provider**(与 chat 同一端点、同一 key)发一次 /embeddings 请求,
        //   仅用于对本地候选重排。网络目的地与凭据均非模型可控,仍属用户既有信任域;且默认关闭、失败静默
        //   回退本地,故仍按 FileRead 自动放行,不因可选增强而改变离线默认场景(占绝大多数)的权限 UX。
        // repo_wiki（R11）从 repo_map 分析派生仓库 wiki：build 仅写 .mdga/wiki 派生缓存、不碰用户
        // 源码,query 纯读,故与 repo_map 同列只读自动放行（派生数据可随时重建）。
        // git_status/git_diff/git_log 为只读 git 工具，与 read_file/search_text 同列自动放行（R4）。
        // lsp_* 为只读 LSP 工具（仅查询语言服务器、不改文件），同列自动放行（R1）。
        // render_artifact（0.0.67 起；0.0.74 改名）：后端惰性、零副作用,仅把代码交前端在沙箱 iframe 渲染,同列自动放行。
        "list_dir" | "read_file" | "stat_path" | "search_text" | "glob_files" | "code_overview"
        | "repo_map" | "code_search" | "repo_wiki" | "render_artifact" | "todo_write" | "ask_user"
        | "run_subtask"
        // run_parallel_subtasks（P1/0.0.58）与 run_subtask 同档：编排器入口本身只读 git 状态并派发，
        // 每个并行写子代理的写/删/命令仍在其 loop 内逐次门控+检查点，故入口自动放行、不重复审批。
        | "run_parallel_subtasks"
        | "load_skill"
        | "remember"
        | "list_shells" | "get_shell_output" | "kill_shell" | "get_task_output" | "kill_task"
        | "list_tasks" | "git_status" | "git_diff" | "git_log" | "lsp_definition"
        | "lsp_references" | "lsp_hover" | "lsp_diagnostics" => Ok(ToolCapability::FileRead),
        // git_add/git_commit/git_branch 改动暂存区/引用，与文件写同档（R4）：默认模式自动放行、
        // AskEveryTime 逐次审批、Restricted 拒绝；都在工作区内、可审计、可回滚。
        "create_file" | "write_file" | "edit_file" | "apply_patch" | "apply_multi_patch"
        | "make_dir" | "move_path" | "git_add" | "git_commit" | "git_branch" => {
            Ok(ToolCapability::FileWrite)
        }
        "delete_file" | "delete_dir" => Ok(ToolCapability::FileDelete),
        // 注册 MCP 会拉起外部进程/网络服务，按命令执行级别裁决（FullAccess 或审批）。
        "run_command" | "add_mcp_server" => Ok(ToolCapability::CommandRun),
        // git_push/git_pr 为远端/网络操作（推送到远端、经 gh 创建 PR），按 NetworkAccess 裁决
        // （R4 后续）：Workspace Auto 逐次审批、Full Access 放行；绝不自动放行。git_push 永不 force。
        "web_fetch" | "web_search" | "list_mcp_resources" | "read_mcp_resource" | "git_push"
        | "git_pr" => Ok(ToolCapability::NetworkAccess),
        // R7：浏览器 / computer-use 工具——无头 Chrome 驱动会触达网络（即便多为 localhost），
        // 按 NetworkAccess 裁决：Workspace Auto 逐次审批、Full Access 放行。
        "browser_navigate" | "browser_screenshot" | "browser_click" | "browser_fill"
        | "browser_read_text" | "browser_console" => Ok(ToolCapability::NetworkAccess),
        other => Err(format!("未知工具: {other}")),
    }
}

/// 由工具名与参数推导「总是允许」规则串：命令取前两个 token 作前缀，其余工具按工具名。
fn permission_rule_for(tool_name: &str, arguments: &str) -> String {
    if tool_name == "run_command" {
        if let Ok(request) = serde_json::from_str::<RunCommandRequest>(arguments) {
            let prefix: Vec<&str> = request.command.split_whitespace().take(2).collect();
            if !prefix.is_empty() {
                return format!("cmd:{}", prefix.join(" "));
            }
        }
        return String::new();
    }
    format!("tool:{tool_name}")
}

/// 极简 glob 匹配。0.0.68 加固:`**/` 前缀按「**零或多段**」语义处理——`**/.env` 既匹配 `src/.env`
/// 也匹配工作区根目录的裸 `.env`(否则 `**`→`*` 折叠后末段 `/.env` 要求路径含 `/`,会漏掉最常见的
/// 根目录 .env,使 `deny:read_file:**/.env` 这条最自然的规则保护不到根 .env)。
fn glob_match(pattern: &str, text: &str) -> bool {
    if let Some(rest) = pattern.strip_prefix("**/") {
        // 既试「至少一段 + /rest」(glob_match_inner 原语义),也试「rest 直接匹配 text」(零段)。
        return glob_match_inner(pattern, text) || glob_match_inner(rest, text);
    }
    glob_match_inner(pattern, text)
}

/// glob 匹配核心：`*` 与 `**` 都视为「任意字符序列（含 /）」，支持 `src/**`、`*.env`。
fn glob_match_inner(pattern: &str, text: &str) -> bool {
    // 把 ** 折叠为 *，再按 * 切段，依次顺序匹配（首段需前缀对齐、末段需后缀对齐）。
    let pat = pattern.replace("**", "*");
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pattern == text; // 无通配，精确匹配
    }
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !text.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if i == parts.len() - 1 {
            return text[pos..].ends_with(part);
        } else {
            match text[pos..].find(part) {
                Some(idx) => pos += idx + part.len(),
                None => return false,
            }
        }
    }
    true
}

/// 单条权限规则对本次调用的裁决：deny 命中返回 Some(false)，allow 命中返回 Some(true)，未命中 None。
///
/// 规则格式：`[allow:|deny:]<body>`（无前缀默认 allow，向后兼容旧规则）。body 为：
/// `cmd:<前缀>`（run_command 命令前缀）| `tool:<工具名>`（按工具名）| `<工具名>:<glob>`（工具+路径 glob）。
/// 路径归一化(0.0.68):trim + 反斜杠折正斜杠 + 小写(Windows FS 大小写不敏感)。
fn norm_path(s: &str) -> String {
    s.trim().replace('\\', "/").to_lowercase()
}

/// 0.0.71 symlink / 穿越兜底:把模型给的路径展开成**多个待匹配候选**(均已归一化),deny glob 命中任一即拦:
/// ① 原始归一化;② 词法解析 `.`/`..` 段(无 I/O,挡 `a/../.env`、`./.env` 穿越);③ 若该路径在工作区内
/// **真实存在**,canonicalize(跟随符号链接)后的工作区相对路径(挡 `link→.env` 读 link 实指 .env)。
/// 解析失败 / 新文件 / 解析出工作区外 → 跳过该候选,靠 ①②。仅在确有 `<tool>:<glob>` 规则命中工具时才走到这里。
fn deny_candidate_paths(workspace_root: &str, raw_path: &str) -> Vec<String> {
    let base = norm_path(raw_path);
    let mut out = vec![base.clone()];
    // ② 词法 . / .. 解析(在归一化后的正斜杠形式上)。
    let mut stack: Vec<&str> = Vec::new();
    for seg in base.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            s => stack.push(s),
        }
    }
    let lexical = stack.join("/");
    if !lexical.is_empty() && lexical != base {
        out.push(lexical);
    }
    // ③ canonicalize(真实存在才有;跟随 symlink)。
    if !workspace_root.trim().is_empty() {
        let joined = std::path::Path::new(workspace_root).join(raw_path.trim());
        if let (Ok(canon), Ok(ws_canon)) = (
            std::fs::canonicalize(&joined),
            std::fs::canonicalize(workspace_root.trim()),
        ) {
            if let Ok(rel) = canon.strip_prefix(&ws_canon) {
                let rel_norm = norm_path(&rel.to_string_lossy());
                if !rel_norm.is_empty() && !out.contains(&rel_norm) {
                    out.push(rel_norm);
                }
            }
        }
    }
    out
}

fn rule_decision(
    rule: &str,
    tool_name: &str,
    arguments: &str,
    workspace_root: &str,
) -> Option<bool> {
    let (effect, body) = if let Some(r) = rule.strip_prefix("deny:") {
        (false, r)
    } else if let Some(r) = rule.strip_prefix("allow:") {
        (true, r)
    } else {
        (true, rule)
    };

    if let Some(prefix) = body.strip_prefix("cmd:") {
        if tool_name == "run_command" {
            if let Ok(request) = serde_json::from_str::<RunCommandRequest>(arguments) {
                let cmd = request.command.trim().to_lowercase();
                let p = prefix.trim().to_lowercase();
                if !p.is_empty() && (cmd == p || cmd.starts_with(&format!("{p} "))) {
                    return Some(effect);
                }
            }
        }
        return None;
    }
    if let Some(name) = body.strip_prefix("tool:") {
        return if name == tool_name { Some(effect) } else { None };
    }
    // <toolname>:<glob> —— 工具名 + 路径 glob
    if let Some((rtool, glob)) = body.split_once(':') {
        if rtool == tool_name {
            let path = serde_json::from_str::<serde_json::Value>(arguments)
                .ok()
                .and_then(|v| {
                    ["path", "from", "to"]
                        .iter()
                        .find_map(|k| v.get(*k).and_then(|x| x.as_str()).map(str::to_string))
                })
                .unwrap_or_default();
            // 0.0.68/0.0.71 加固:对路径的多个归一化候选(原始 / 词法解析 .. / canonicalize 跟随 symlink)
            // 逐一与 glob 匹配,命中任一即生效。挡大小写 / 反斜杠 / 尾空格 / `a/../.env` 穿越 / `link→.env` 符号链接绕过。
            let glob_n = norm_path(glob);
            if deny_candidate_paths(workspace_root, &path)
                .iter()
                .any(|cand| glob_match(&glob_n, cand))
            {
                return Some(effect);
            }
        }
    }
    None
}

/// 汇总权限规则：deny 优先。返回 Some(false)=显式拒绝，Some(true)=显式放行，None=无规则覆盖。
fn permission_rules_decision(
    rules: &[String],
    tool_name: &str,
    arguments: &str,
    workspace_root: &str,
) -> Option<bool> {
    let mut allowed = false;
    for rule in rules {
        match rule_decision(rule, tool_name, arguments, workspace_root) {
            Some(false) => return Some(false), // deny 立即否决
            Some(true) => allowed = true,
            None => {}
        }
    }
    if allowed {
        Some(true)
    } else {
        None
    }
}

/// 对单次工具调用做权限门控。
///
/// run_command 在 Workspace Auto 下，若命令属于低风险白名单则直接放行，否则按能力裁决；
/// 裁决为「需审批」时先查用户保存的「总是允许」规则，命中则免审批放行。
pub(crate) fn gate_tool_decision(
    context: &SessionSecurityContext,
    tool_name: &str,
    arguments: &str,
    rules: &[String],
) -> ToolGate {
    let capability = match tool_capability_for_name(tool_name) {
        Ok(capability) => capability,
        Err(message) => return ToolGate::Deny(message),
    };

    // 用户显式 deny 规则最高优先级：任何模式下都拒绝。
    if permission_rules_decision(rules, tool_name, arguments, &context.workspace_root) == Some(false) {
        return ToolGate::Deny("已被用户的拒绝规则阻止".to_string());
    }

    // 只读联网工具（web_search / web_fetch）在默认模式下自动放行（Plan21 #8）：
    // 仅检索/抓取、无本地副作用，无需每次打断用户。放在 deny 检查之后、能力矩阵裁决之前。
    // 注意：MCP 工具 / list_mcp_resources / read_mcp_resource 不在此列，仍按能力矩阵走审批。
    if matches!(tool_name, "web_search" | "web_fetch")
        && matches!(context.permission_mode, mdga_shared::PermissionMode::WorkspaceAuto)
    {
        return ToolGate::Allow;
    }

    // 低风险命令白名单：Workspace Auto 下免审批直接执行常见检查 / 构建 / 测试命令。
    if tool_name == "run_command"
        && matches!(context.permission_mode, mdga_shared::PermissionMode::WorkspaceAuto)
    {
        if let Ok(request) = serde_json::from_str::<RunCommandRequest>(arguments) {
            if is_low_risk_command(&request.command) {
                return ToolGate::Allow;
            }
        }
    }

    match decide_tool_access(context, capability) {
        ToolDecision::Allow => ToolGate::Allow,
        ToolDecision::AskUser => {
            if permission_rules_decision(rules, tool_name, arguments, &context.workspace_root) == Some(true) {
                ToolGate::Allow
            } else {
                ToolGate::Ask
            }
        }
        ToolDecision::Deny => ToolGate::Deny("当前权限模式不允许此操作".to_string()),
    }
}

/// 向前端发起一次审批请求并等待用户决定。
///
/// 生成唯一 action_id，注册 oneshot 通道（附「总是允许」规则串），emit "approval-request"，
/// 然后 await 前端通过 respond_approval 命令送回的结果。通道异常时默认拒绝（安全优先）。
///
/// `irreversible == true`（Plan21 #2b）表示该次写/删操作快照失败、无法回退：在事件里带上
/// `irreversible` 标志，并在 preview 顶部前置「⚠ 此操作不可回退」提示，确保用户即使在不识别
/// 该标志的前端上也能看到风险。
pub(crate) async fn request_tool_approval(
    app: &AppHandle,
    tool_name: &str,
    arguments: &str,
    irreversible: bool,
) -> bool {
    let action_id = format!("act-{}", APPROVAL_SEQ.fetch_add(1, Ordering::SeqCst));
    let rule = permission_rule_for(tool_name, arguments);
    let (sender, receiver) = oneshot::channel::<bool>();
    {
        let state = app.state::<AppState>();
        let mut approvals = state.approvals.lock().expect("approvals mutex poisoned");
        approvals.insert(action_id.clone(), (sender, rule));
    }

    let mut preview = approval_preview(tool_name, arguments);
    if irreversible {
        // 前置不可回退提示，与下方正文用空行隔开；preview 为空时也单独成段。
        preview = if preview.is_empty() {
            "⚠ 此操作不可回退（无法快照原内容，撤销后无法恢复）。".to_string()
        } else {
            format!("⚠ 此操作不可回退（无法快照原内容，撤销后无法恢复）。\n\n{preview}")
        };
    }

    let _ = app.emit(
        "approval-request",
        serde_json::json!({
            "actionId": action_id,
            "toolName": tool_name,
            "target": approval_target(arguments),
            "preview": preview,
            "irreversible": irreversible,
        }),
    );

    receiver.await.unwrap_or(false)
}

/// 生成唯一 question_id，注册 oneshot 通道，emit "ask-user-request"（携带结构化问题），
/// 然后 await 前端通过 respond_ask_user 命令送回的答案 JSON。通道异常 / 用户取消时返回空串。
async fn request_user_answer(app: &AppHandle, questions: &serde_json::Value) -> String {
    let question_id = format!("ask-{}", QUESTION_SEQ.fetch_add(1, Ordering::SeqCst));
    let (sender, receiver) = oneshot::channel::<String>();
    {
        let state = app.state::<AppState>();
        let mut map = state
            .ask_questions
            .lock()
            .expect("ask_questions mutex poisoned");
        map.insert(question_id.clone(), sender);
    }

    let _ = app.emit(
        "ask-user-request",
        serde_json::json!({
            "questionId": question_id,
            "questions": questions,
        }),
    );

    receiver.await.unwrap_or_default()
}

/// ask_user 工具：需求不明确且靠读文件 / 工具也无法判断时，把 1-4 个结构化问题弹给用户，
/// 阻塞等待选择（前端自动附「Other」自定义项），返回用户答案 JSON 供模型据此继续。
pub(crate) async fn execute_ask_user(
    app: &AppHandle,
    arguments: &str,
) -> Result<serde_json::Value, String> {
    let parsed = serde_json::from_str::<serde_json::Value>(arguments)
        .map_err(|err| format!("工具参数解析失败: {err}"))?;
    let questions = parsed
        .get("questions")
        .cloned()
        .ok_or_else(|| "ask_user 缺少 questions".to_string())?;
    let is_nonempty_array = questions
        .as_array()
        .map(|a| !a.is_empty() && a.len() <= 4)
        .unwrap_or(false);
    if !is_nonempty_array {
        return Err("ask_user 的 questions 必须是 1-4 个问题的数组".to_string());
    }

    let answer = request_user_answer(app, &questions).await;
    if answer.trim().is_empty() {
        return Err("用户未作出选择（已取消提问）。请根据已有信息继续，或换一种方式推进。".to_string());
    }
    // 前端回送的是答案 JSON（每题 -> 选择数组 / 自定义文本）；解析失败则按纯文本兜底。
    let answers = serde_json::from_str::<serde_json::Value>(&answer)
        .unwrap_or(serde_json::Value::String(answer));
    Ok(serde_json::json!({ "answers": answers }))
}

/// 提取审批弹窗的「动作内容预览」（Plan19 C-C）：让用户「看清再点允许」。
///
/// run_command → 命令全文;write_file/create_file → 写入内容(前 40 行或 ~2KB,超出标注截断);
/// 编辑/patch 类 → diff/patch 文本;其它工具 → 空串。
fn approval_preview(tool_name: &str, arguments: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return String::new();
    };
    let get = |key: &str| value.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let raw = match tool_name {
        "run_command" => get("command"),
        "git_commit" => get("message"),
        "git_pr" => {
            let title = get("title");
            let base = get("base");
            if base.is_empty() {
                title
            } else {
                format!("[base: {base}] {title}")
            }
        }
        "write_file" | "create_file" => get("content"),
        "apply_patch" | "edit_file" => {
            let diff = get("diff");
            if diff.is_empty() {
                get("patch")
            } else {
                diff
            }
        }
        _ => String::new(),
    };
    truncate_preview(&raw)
}

/// 预览体积上限:取前 40 行且不超过 ~2KB,超出在末尾标注省略。
fn truncate_preview(text: &str) -> String {
    const MAX_BYTES: usize = 2048;
    const MAX_LINES: usize = 40;
    if text.is_empty() {
        return String::new();
    }
    let by_lines: String = text.lines().take(MAX_LINES).collect::<Vec<_>>().join("\n");
    let line_truncated = text.lines().count() > MAX_LINES;
    let (mut out, byte_truncated) = if by_lines.len() > MAX_BYTES {
        // 按字符边界安全截断到 ~2KB。
        let mut end = MAX_BYTES;
        while end > 0 && !by_lines.is_char_boundary(end) {
            end -= 1;
        }
        (by_lines[..end].to_string(), true)
    } else {
        (by_lines, false)
    };
    if line_truncated || byte_truncated {
        out.push_str("\n…（内容过长，仅预览前一部分）");
    }
    out
}

/// 从工具参数中提取审批展示用的目标（path / from / command）。
/// apply_multi_patch 无单一 path：汇总其 files[].path 作目标串，让审批弹窗能显示受影响文件。
fn approval_target(arguments: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return String::new();
    };
    if let Some(found) = ["path", "from", "command"]
        .iter()
        .find_map(|key| value.get(*key).and_then(|v| v.as_str()).map(str::to_string))
    {
        return found;
    }
    if let Some(files) = value.get("files").and_then(|v| v.as_array()) {
        let paths: Vec<String> = files
            .iter()
            .filter_map(|f| f.get("path").and_then(|v| v.as_str()).map(str::to_string))
            .collect();
        if !paths.is_empty() {
            return paths.join(", ");
        }
    }
    String::new()
}

/// 记录一次工具被拒绝（权限拒绝或用户拒绝），并把拒绝结果回灌给模型，让它换方案或说明。
#[allow(clippy::too_many_arguments)]
pub(crate) fn feed_tool_denial(
    app: &AppHandle,
    conversation_id: &str,
    tool_name: &str,
    arguments: &str,
    workspace_path: &str,
    reason: &str,
    tool_call_id: &str,
    wire_messages: &mut Vec<serde_json::Value>,
) {
    record_tool_event(
        app,
        conversation_id,
        "tool_denied",
        tool_name,
        "denied",
        arguments,
        None,
        Some(reason),
        workspace_path,
    );
    wire_messages.push(serde_json::json!({
        "role": "tool",
        "tool_call_id": tool_call_id,
        "content": serde_json::json!({ "ok": false, "error": reason }).to_string()
    }));
}

#[cfg(test)]
mod tests {
    use super::{glob_match, permission_rules_decision};

    #[test]
    fn glob_match_handles_wildcards() {
        assert!(glob_match("**/.env", "src/config/.env"));
        assert!(glob_match("**/.env", ".env")); // 0.0.68:**/ 零段,命中根目录裸 .env
        assert!(glob_match("src/**", "src/a/b.rs"));
        assert!(glob_match("*.env", ".env"));
        assert!(glob_match("*.env", "prod.env"));
        assert!(!glob_match("*.env", "env.txt"));
        assert!(glob_match("exact.txt", "exact.txt"));
        assert!(!glob_match("exact.txt", "other.txt"));
    }

    #[test]
    fn permission_rules_deny_takes_precedence() {
        let rules = vec![
            "allow:tool:read_file".to_string(),
            "deny:read_file:**/.env".to_string(),
        ];
        // 读普通文件：allow 命中
        assert_eq!(
            permission_rules_decision(&rules, "read_file", "{\"path\":\"src/main.rs\"}", ""),
            Some(true)
        );
        // 读 .env：deny 命中，优先否决
        assert_eq!(
            permission_rules_decision(&rules, "read_file", "{\"path\":\"src/.env\"}", ""),
            Some(false)
        );
        // 0.0.68/0.0.71 加固:大小写 / 反斜杠 / 根目录裸 .env / 尾空格 / `a/../.env` 词法穿越 都不能绕过
        // deny:**/.env(canonicalize 跟随的 symlink 绕过另有真机测试)。
        for p in [
            "{\"path\":\"src/.ENV\"}",
            "{\"path\":\"config\\\\.env\"}",
            "{\"path\":\"CONFIG\\\\.Env\"}",
            "{\"path\":\".env\"}",   // 根目录裸 .env(Fix 3:**/ 零段)
            "{\"path\":\".env \"}",  // 尾空格(Fix 2:trim 对齐读侧)
            "{\"path\":\"src/sub/../.env\"}", // 0.0.71:词法 .. 穿越 → src/.env
            "{\"path\":\"./.env\"}",          // 0.0.71:前导 ./
        ] {
            assert_eq!(
                permission_rules_decision(&rules, "read_file", p, ""),
                Some(false),
                "deny:**/.env 应拦住等价写法: {p}"
            );
        }
        // 旧式裸规则向后兼容（视为 allow）
        assert_eq!(
            permission_rules_decision(&["tool:write_file".to_string()], "write_file", "{}", ""),
            Some(true)
        );
        // 命令前缀规则
        assert_eq!(
            permission_rules_decision(&["cmd:git push".to_string()], "run_command", "{\"command\":\"git push origin\"}", ""),
            Some(true)
        );
    }

    #[test]
    fn deny_resolves_symlink_target() {
        // 0.0.71:link → .env 的符号链接,读 link 应被 deny:**/.env 拦(canonicalize 跟随)。
        // Windows 建 symlink 需开发者模式/管理员,创建失败则**跳过**(非失败),不阻断 CI/无权限环境。
        use std::io::Write;
        let ws = std::env::temp_dir().join(format!("mdga-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        if std::fs::create_dir_all(&ws).is_err() {
            return;
        }
        let env_file = ws.join(".env");
        if let Ok(mut f) = std::fs::File::create(&env_file) {
            let _ = f.write_all(b"SECRET=x");
        }
        let link = ws.join("innocent.txt");
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&env_file, &link).is_ok();
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(&env_file, &link).is_ok();
        if !made {
            let _ = std::fs::remove_dir_all(&ws);
            return; // 无权限创建 symlink:跳过
        }
        let rules = vec!["deny:read_file:**/.env".to_string()];
        let args = serde_json::json!({ "path": "innocent.txt" }).to_string();
        let decision =
            permission_rules_decision(&rules, "read_file", &args, ws.to_str().unwrap_or(""));
        let _ = std::fs::remove_dir_all(&ws);
        assert_eq!(
            decision,
            Some(false),
            "读经 symlink 指向 .env 的 innocent.txt 应被 deny:**/.env 拦"
        );
    }
}
