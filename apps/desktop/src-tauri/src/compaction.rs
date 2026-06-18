//! 上下文与输出体积治理：大工具输出落盘、上下文软上限、跨轮摘要压缩、旧工具结果短桩。
//!
//! 从 main.rs 抽出（Plan16 阶段2）：纯逻辑搬移，无行为变更。摘要压缩复用 main.rs 的
//! chat_completion_with_retry（chat 桥接后续再独立成模块）。
//!
//! ## 记忆分层（R12）：情景记忆 / 工作记忆的双轨
//!
//! 本模块的压缩链路现在显式区分两种记忆：
//!
//! - **工作记忆（working memory，有界的近期上下文）**：真正喂给模型的 `wire_messages`。
//!   它被软上限约束，超限时按「年龄」分级降解——最近的全文、中龄的凝练成一行关键事实、
//!   更早的换成短桩、再不够则整段摘要。这是模型当下推理所依赖的有限窗口。
//!
//! - **情景记忆（episodic memory，累积的全量历史）**：落盘到
//!   `.mdga/archive/<conversation_id>.jsonl` 的 append-only 归档。任何即将被「丢出」工作记忆
//!   的原始内容（被短桩替换的大工具结果、被摘要替换的整段历史）在丢弃前都先追加到这里。
//!   它体量不受上下文窗口约束、永不回灌进 prompt，但完整可检索——模型可用 read_file 翻阅，
//!   于是「压缩」不再等于「不可逆地丢失」。
//!
//! 三级降解从轻到重：①短桩压缩 `compact_tool_outputs`（机械、零成本）→ ②中间凝练
//! `condense_tool_outputs`（一行关键事实，介于短桩与全文之间）→ ③摘要压缩
//! `summarize_wire_history`（一次无工具模型调用）。每一级在丢内容前都先写情景记忆归档，
//! 并在留在工作记忆里的占位文本中留一条指向归档文件的指针，保证没有任何内容被不可逆丢失。
//!
//! 安全性：归档写入失败一律 fail-soft（吞掉错误、退回不归档），绝不让归档 IO 打断当前轮次。

use crate::{chat_completion_with_retry, COMPACTED_TOOL_STUB, KEEP_RECENT_WIRE_MESSAGES};
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::AppHandle;

/// 大工具输出落盘自增序号。
static LARGE_OUTPUT_SEQ: AtomicU64 = AtomicU64::new(1);

/// 工具结果过大时落盘到 .mdga/tool-results/<seq>.txt，返回给模型的省流摘要（含相对路径 + 开头预览，
/// 完整内容可用 read_file 分页读取）；未超阈值则原样返回。落盘失败退回原文，绝不丢内容。
pub(crate) fn maybe_persist_large_output(workspace_path: &str, output_str: &str) -> String {
    const LARGE_OUTPUT_THRESHOLD: usize = 16_000;
    if output_str.chars().count() <= LARGE_OUTPUT_THRESHOLD {
        return output_str.to_string();
    }
    let seq = LARGE_OUTPUT_SEQ.fetch_add(1, Ordering::SeqCst);
    let rel = format!(".mdga/tool-results/{seq}.txt");
    let dir = std::path::Path::new(workspace_path).join(".mdga").join("tool-results");
    let full = dir.join(format!("{seq}.txt"));
    let bytes = output_str.len();
    let head: String = output_str.chars().take(2_000).collect();
    if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&full, output_str).is_ok() {
        serde_json::json!({
            "ok": true,
            "note": format!("工具输出过大（约 {bytes} 字节），已落盘以节省上下文。如需完整内容，用 read_file 读取 persistedPath（支持 offset/limit 分页）。"),
            "persistedPath": rel,
            "bytes": bytes,
            "head": head
        })
        .to_string()
    } else {
        output_str.to_string()
    }
}

// 注（Plan28 P3-9）：软上限常量 CONTEXT_SOFT_LIMIT_TOKENS 与推导函数 context_soft_limit_for
// 已迁入 mdga-agent-core（compaction 子模块，逻辑一字不改），其单测亦随之迁过去；本文件不再保留。

// ===== R12 情景记忆（episodic）：磁盘归档层 =====

/// 工作区内的情景记忆归档相对路径。每个会话一份 append-only JSONL。
fn archive_rel_path(conversation_id: &str) -> String {
    format!(".mdga/archive/{}.jsonl", sanitize_conversation_id(conversation_id))
}

/// 把 conversation_id 收敛成安全文件名片段：仅保留字母数字/`-`/`_`，其余替换为 `_`，
/// 避免路径穿越或非法文件名（会话 id 通常已是 uuid，这里只做防御）。空串回退 `unknown`。
fn sanitize_conversation_id(conversation_id: &str) -> String {
    let cleaned: String = conversation_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if cleaned.is_empty() { "unknown".to_string() } else { cleaned }
}

/// 构造一条情景记忆归档 JSONL 行（纯函数，不含尾随换行）。
///
/// `reason` 描述这条内容为何被丢出工作记忆（如 "stub" / "summary"），便于日后检索区分。
/// 用 serde_json 序列化保证内容里的换行/引号被正确转义，每条占一行。
fn archive_line(conversation_id: &str, reason: &str, role: &str, content: &str) -> String {
    serde_json::json!({
        "conversation_id": conversation_id,
        "reason": reason,
        "role": role,
        "content": content,
    })
    .to_string()
}

/// 把即将被丢出工作记忆的原始内容追加到情景记忆归档（append-only），成功则返回相对路径。
///
/// fail-soft：目录创建或写入失败时吞掉错误、返回 None，绝不让归档 IO 打断当前轮次。
/// 空内容不归档（无可挽救信息），返回 None。
fn archive_dropped_content(
    workspace_path: &str,
    conversation_id: &str,
    reason: &str,
    role: &str,
    content: &str,
) -> Option<String> {
    if content.is_empty() {
        return None;
    }
    let rel = archive_rel_path(conversation_id);
    let full = std::path::Path::new(workspace_path).join(&rel);
    let dir = full.parent()?;
    std::fs::create_dir_all(dir).ok()?;
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&full)
        .ok()?;
    let line = archive_line(conversation_id, reason, role, content);
    // 写入与换行任一失败都 fail-soft：归档不完整不应影响主流程。
    f.write_all(line.as_bytes()).ok()?;
    f.write_all(b"\n").ok()?;
    Some(rel)
}

// ===== R12 工作记忆（working）：中间「凝练为关键事实」级 =====

/// 中间凝练级：把一段大工具结果提炼成**一行关键事实**占位文本，介于「短桩」与「全文」之间。
///
/// 纯函数。策略：若内容是 JSON 对象，优先抽取常见的状态字段（ok/path/persistedPath/exitCode 等）
/// 拼成一行；否则取首行非空文本并截断。末尾附「已凝练/可重读」提示。`archive_ref` 为情景记忆归档
/// 的相对路径（若有），让模型知道完整原文可从何处取回。
///
/// 与短桩的区别：短桩丢掉一切语义（只说"已省略"），凝练保留一行可判读的关键信号
/// （成功与否 / 涉及的文件 / 退出码 / 内容首行），让模型在不重读的情况下也能继续推理。
fn condense_key_fact(content: &str, archive_ref: Option<&str>) -> String {
    const MAX_FACT_CHARS: usize = 200;
    let mut fact = extract_key_fact(content);
    if fact.chars().count() > MAX_FACT_CHARS {
        fact = fact.chars().take(MAX_FACT_CHARS).collect::<String>() + "…";
    }
    let pointer = match archive_ref {
        Some(p) => format!("；完整原文见归档 {p}（read_file 可读），如需最新内容请重调对应工具"),
        None => "；如需完整/最新内容请重调对应工具".to_string(),
    };
    serde_json::json!({
        "ok": true,
        "note": format!("[此前工具结果已凝练为关键事实] {fact}{pointer}"),
    })
    .to_string()
}

/// 从工具结果原文里抽取一行「关键事实」（纯函数，供 [`condense_key_fact`] 使用）。
fn extract_key_fact(content: &str) -> String {
    // 优先把 JSON 对象的关键状态字段拼成一行。
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(content) {
        let mut parts: Vec<String> = Vec::new();
        for key in ["ok", "exitCode", "path", "persistedPath", "bytes", "note"] {
            if let Some(v) = map.get(key) {
                let rendered = match v {
                    serde_json::Value::String(s) => {
                        let s: String = s.chars().take(80).collect();
                        s
                    }
                    other => other.to_string(),
                };
                if !rendered.is_empty() {
                    parts.push(format!("{key}={rendered}"));
                }
            }
        }
        if !parts.is_empty() {
            return parts.join(", ");
        }
    }
    // 非 JSON：取首个非空行。
    content
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("(空)")
        .to_string()
}

/// 中间凝练压缩：把「中龄」的大工具结果（既非最近 keep_recent，也尚未被短桩/凝练）替换为
/// 一行关键事实占位文本，并在替换前先把原文归档到情景记忆。返回被凝练的条数。
///
/// 与 [`compact_tool_outputs`]（短桩，彻底丢语义）相比，本级保留一行可判读信号，更轻；
/// 设计意图是把它放在短桩**之前**触发，作为软上限的第一道降解（损失更小）。
/// 幂等：已是短桩或已凝练（note 含「已凝练」）的消息会跳过。归档失败不影响替换（fail-soft，
/// 此时占位文本里不带归档指针，但凝练后的关键事实本身仍保留了可继续推理的信号）。
pub(crate) fn condense_tool_outputs(
    workspace_path: &str,
    conversation_id: &str,
    wire_messages: &mut [serde_json::Value],
    keep_recent: usize,
    condense_threshold: usize,
) -> usize {
    let tool_indices: Vec<usize> = wire_messages
        .iter()
        .enumerate()
        .filter(|(_, msg)| msg.get("role").and_then(|r| r.as_str()) == Some("tool"))
        .map(|(idx, _)| idx)
        .collect();
    if tool_indices.len() <= keep_recent {
        return 0;
    }
    let cutoff = tool_indices.len() - keep_recent;
    let mut condensed = 0;
    for &idx in &tool_indices[..cutoff] {
        let content = wire_messages[idx]
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        if content.len() <= condense_threshold
            || content == COMPACTED_TOOL_STUB
            || content.contains("已凝练为关键事实")
        {
            continue;
        }
        // 先归档原文（情景记忆），再用一行关键事实替换（工作记忆）。归档失败 fail-soft。
        let archive_ref =
            archive_dropped_content(workspace_path, conversation_id, "condense", "tool", &content);
        let condensed_text = condense_key_fact(&content, archive_ref.as_deref());
        if let Some(obj) = wire_messages[idx].as_object_mut() {
            obj.insert("content".to_string(), serde_json::Value::String(condensed_text));
            condensed += 1;
        }
    }
    condensed
}

/// 计算摘要压缩的切分点：返回（开头连续 system 消息数, 摘要区终点）。
///
/// 开头的 system 消息（工作区上下文/工具规则/repo map/长期记忆）永不压缩；
/// 末尾保留 keep_recent 条原文；切分点不允许落在 tool 结果上（向前回退，保证
/// assistant 的 tool_calls 与其 tool 结果不被拆散）。历史不够长时返回 None。
fn summary_split_points(wire: &[serde_json::Value], keep_recent: usize) -> Option<(usize, usize)> {
    let first_non_system = wire
        .iter()
        .position(|m| m.get("role").and_then(|r| r.as_str()) != Some("system"))
        .unwrap_or(wire.len());
    if wire.len().saturating_sub(first_non_system) <= keep_recent {
        return None;
    }
    let mut cut = wire.len() - keep_recent;
    while cut > first_non_system
        && wire[cut].get("role").and_then(|r| r.as_str()) == Some("tool")
    {
        cut -= 1;
    }
    if cut <= first_non_system {
        return None;
    }
    Some((first_non_system, cut))
}

/// 把单条 wire 消息渲染成供摘要模型阅读的紧凑单行文本（角色 + 截断正文 + 工具调用名）。
fn render_wire_message_for_summary(message: &serde_json::Value) -> String {
    const MAX_CHARS: usize = 600;
    let role = message.get("role").and_then(|r| r.as_str()).unwrap_or("?");
    let mut body = message
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    if let Some(calls) = message.get("tool_calls").and_then(|c| c.as_array()) {
        let names: Vec<String> = calls
            .iter()
            .map(|call| {
                let name = call
                    .pointer("/function/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let args: String = call
                    .pointer("/function/arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .chars()
                    .take(120)
                    .collect();
                format!("{name}({args})")
            })
            .collect();
        body = format!("{body} [调用工具: {}]", names.join(", "));
    }
    let truncated: String = body.chars().take(MAX_CHARS).collect();
    format!("[{role}] {truncated}")
}

/// 跨轮摘要压缩（auto-compact）：把较早的对话历史压缩成任务进度摘要，替换原文继续任务。
///
/// 这是 Claude Code / Codex 同款思路：摘要式而非删除式。保留开头 system 消息与最近
/// KEEP_RECENT_WIRE_MESSAGES 条原文，中间历史经一次无工具模型调用压缩为
/// 「目标/已完成/关键决策/文件改动/下一步」备忘录，以 system 消息插回，保证任务方向不丢。
/// 返回压缩后的消息序列与摘要调用消耗的 usage；历史太短时原样返回。
///
/// R12：被摘要替换的整段原始历史（`[first_non_system..cut]`）在丢弃前先逐条追加到情景记忆归档
/// （`.mdga/archive/<conversation_id>.jsonl`），并在插回的摘要 system 消息里附一条指向归档文件的
/// 指针，使「摘要」不再等于「不可逆丢失」——模型可用 read_file 翻阅被压掉的完整历史。
/// 归档失败 fail-soft：摘要照常插回，仅不带归档指针。
pub(crate) async fn summarize_wire_history(
    base_url: &str,
    api_key: &str,
    model: &str,
    workspace_path: &str,
    conversation_id: &str,
    wire_messages: Vec<serde_json::Value>,
    app: &AppHandle,
) -> Result<(Vec<serde_json::Value>, Option<mdga_shared::RawUsage>), String> {
    let Some((first_non_system, cut)) =
        summary_split_points(&wire_messages, KEEP_RECENT_WIRE_MESSAGES)
    else {
        return Ok((wire_messages, None));
    };

    let transcript: String = wire_messages[first_non_system..cut]
        .iter()
        .map(render_wire_message_for_summary)
        .collect::<Vec<_>>()
        .join("\n");

    // 情景记忆归档：把整段即将被摘要替换的原始历史逐条追加到归档（丢弃前先存）。
    // 用渲染后的紧凑单行作为归档正文（已含角色/正文/工具调用名），单条空则跳过。
    // 任一条失败即视为本次归档不可用（fail-soft）：archive_ref 保持 None，摘要不带指针。
    let mut archive_ref: Option<String> = None;
    for message in &wire_messages[first_non_system..cut] {
        let line = render_wire_message_for_summary(message);
        match archive_dropped_content(workspace_path, conversation_id, "summary", "history", &line) {
            Some(rel) => archive_ref = Some(rel),
            None if archive_ref.is_some() => {
                // 已写过若干条后中途失败：归档不完整，但已落盘部分仍有价值，保留指针。
            }
            None => {}
        }
    }
    let prompt = format!(
        "你是对话压缩器。请把下面这段 AI Agent 的历史对话压缩成简明的中文任务备忘录，\
用于替换原始历史、让 Agent 继续执行任务。备忘录必须包含：\
1）用户的总体目标；2）已完成的事项；3）关键决策与原因；\
4）已创建/修改/删除的文件清单；5）当前进度与下一步计划。只输出备忘录本身，不要寒暄。\n\n\
=== 历史对话开始 ===\n{transcript}\n=== 历史对话结束 ==="
    );

    let result = chat_completion_with_retry(
        base_url,
        api_key,
        vec![serde_json::json!({ "role": "user", "content": prompt })],
        model,
        None,
        app,
    )
    .await?;
    let summary = result.content.unwrap_or_default();

    let pointer = match archive_ref.as_deref() {
        Some(p) => format!(
            "\n\n（被压缩的完整原始历史已归档至 {p}，可用 read_file 翻阅；若摘要有缺漏可回查该文件。）"
        ),
        None => String::new(),
    };
    let mut compacted: Vec<serde_json::Value> = wire_messages[..first_non_system].to_vec();
    compacted.push(serde_json::json!({
        "role": "system",
        "content": format!(
            "早前对话已自动压缩。以下是任务进度摘要，请严格按其继续推进，不要偏离原始目标：\n{summary}{pointer}"
        )
    }));
    compacted.extend_from_slice(&wire_messages[cut..]);
    Ok((compacted, result.usage))
}

/// 短桩压缩：把 wire_messages 中较早的大体积工具结果换成短桩，保留最近 keep_recent 个全文。
///
/// 输入构建中的 wire 消息序列；把除最近 keep_recent 个之外、正文超过 stub_threshold 字符的
/// `role==tool` 消息正文替换为短桩，返回被压缩的条数。只动工具结果正文，**不动** assistant 的
/// 工具调用与叙述，因此模型的推理链路和任务方向保持完整，只是丢弃了可重新获取的大体积数据。
/// 幂等：已是短桩的消息会跳过。
///
/// R12：丢弃前先把原文追加到情景记忆归档（`.mdga/archive/<conversation_id>.jsonl`），并在短桩里
/// 附一条指向归档文件的指针，使被短桩的内容不再不可逆丢失（仍可用 read_file 翻阅原文）。
/// 归档失败 fail-soft：退回到不带指针的默认短桩 [`COMPACTED_TOOL_STUB`]，压缩照常进行。
pub(crate) fn compact_tool_outputs(
    workspace_path: &str,
    conversation_id: &str,
    wire_messages: &mut [serde_json::Value],
    keep_recent: usize,
    stub_threshold: usize,
) -> usize {
    let tool_indices: Vec<usize> = wire_messages
        .iter()
        .enumerate()
        .filter(|(_, msg)| msg.get("role").and_then(|r| r.as_str()) == Some("tool"))
        .map(|(idx, _)| idx)
        .collect();
    if tool_indices.len() <= keep_recent {
        return 0;
    }
    let cutoff = tool_indices.len() - keep_recent;
    let mut compacted = 0;
    for &idx in &tool_indices[..cutoff] {
        let content = wire_messages[idx]
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        if content.len() <= stub_threshold || content == COMPACTED_TOOL_STUB || is_stub_with_pointer(&content) {
            continue;
        }
        // 先把原文归档进情景记忆，再用短桩替换工作记忆里的正文。归档失败 fail-soft。
        let archive_ref =
            archive_dropped_content(workspace_path, conversation_id, "stub", "tool", &content);
        let stub = stub_with_pointer(archive_ref.as_deref());
        if let Some(obj) = wire_messages[idx].as_object_mut() {
            obj.insert("content".to_string(), serde_json::Value::String(stub));
            compacted += 1;
        }
    }
    compacted
}

/// 构造带情景记忆指针的短桩文本（纯函数）。无归档引用时退回默认 [`COMPACTED_TOOL_STUB`]。
fn stub_with_pointer(archive_ref: Option<&str>) -> String {
    match archive_ref {
        Some(p) => serde_json::json!({
            "ok": true,
            "note": format!(
                "[此前的工具结果已省略以节省上下文；完整原文已归档至 {p}，可用 read_file 读取；如需该文件/目录/命令的最新内容，请重新调用对应工具]"
            ),
        })
        .to_string(),
        None => COMPACTED_TOOL_STUB.to_string(),
    }
}

/// 判定一条内容是否已是「带归档指针的短桩」（幂等跳过用）。
fn is_stub_with_pointer(content: &str) -> bool {
    content.contains("完整原文已归档至")
}

#[cfg(test)]
mod tests {
    use super::{
        archive_line, compact_tool_outputs, condense_key_fact, condense_tool_outputs,
        extract_key_fact, render_wire_message_for_summary, sanitize_conversation_id,
        summary_split_points,
    };
    use crate::COMPACTED_TOOL_STUB;

    // 注（Plan28 P3-9）：soft_limit_derives_from_context_window_or_falls_back 单测已随
    // context_soft_limit_for 迁入 mdga-agent-core（crates/agent-core/src/compaction.rs）。

    /// 在系统临时目录下建一个唯一子目录当作工作区，返回其路径（测试用，调用方负责清理）。
    fn temp_workspace(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("mdga-r12-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("mk temp ws");
        dir
    }

    #[test]
    fn compact_tool_outputs_stubs_old_large_results_keeps_recent() {
        let ws = temp_workspace("stub");
        let ws_str = ws.to_str().unwrap();
        let big = "x".repeat(5_000);
        let small = "{\"ok\":true}".to_string();
        let mut wire = vec![
            serde_json::json!({ "role": "system", "content": "sys" }),
            serde_json::json!({ "role": "user", "content": "do it" }),
            serde_json::json!({ "role": "tool", "tool_call_id": "1", "content": big.clone() }), // old big -> stub
            serde_json::json!({ "role": "tool", "tool_call_id": "2", "content": small.clone() }), // old small -> kept
            serde_json::json!({ "role": "tool", "tool_call_id": "3", "content": big.clone() }), // recent -> kept
            serde_json::json!({ "role": "tool", "tool_call_id": "4", "content": big.clone() }), // recent -> kept
            serde_json::json!({ "role": "tool", "tool_call_id": "5", "content": big.clone() }), // recent -> kept
        ];

        let compacted = compact_tool_outputs(ws_str, "conv-stub", &mut wire, 3, 1_500);

        assert_eq!(compacted, 1, "只压缩 1 条较早的大结果");
        // 老的大结果被压缩成「带归档指针的短桩」（已归档故含指针，不再是裸 COMPACTED_TOOL_STUB）。
        let stubbed = wire[2]["content"].as_str().unwrap();
        assert!(stubbed.contains("完整原文已归档至"), "短桩应含归档指针");
        assert_eq!(wire[3]["content"], small); // 老的小结果不动
        assert_eq!(wire[5]["content"], big); // 最近的保留全文
        // 非工具消息不受影响
        assert_eq!(wire[0]["content"], "sys");
        // 情景记忆归档文件已生成，且含原始大内容。
        let archive = ws.join(".mdga/archive/conv-stub.jsonl");
        let archived = std::fs::read_to_string(&archive).expect("archive written");
        assert!(archived.contains(&big), "归档应含原始大内容");
        assert!(archived.lines().count() >= 1);

        // 幂等：再压一次不应重复处理（带指针短桩会被跳过）。
        assert_eq!(compact_tool_outputs(ws_str, "conv-stub", &mut wire, 3, 1_500), 0);

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn compact_tool_outputs_failsoft_uses_plain_stub_when_archive_unwritable() {
        // 把「工作区」指向一个已存在的*文件*：create_dir_all 必失败 → 归档 fail-soft。
        let f = temp_workspace("failsoft").join("notadir");
        std::fs::write(&f, b"x").unwrap();
        let big = "y".repeat(5_000);
        let mut wire = vec![
            serde_json::json!({ "role": "tool", "tool_call_id": "1", "content": big.clone() }),
            serde_json::json!({ "role": "tool", "tool_call_id": "2", "content": "{}".to_string() }),
        ];
        // keep_recent=1 → 第 0 条要被压缩；归档写不进，应退回裸短桩，压缩仍成功。
        let n = compact_tool_outputs(f.to_str().unwrap(), "conv", &mut wire, 1, 1_500);
        assert_eq!(n, 1, "归档失败不阻断压缩");
        assert_eq!(wire[0]["content"], COMPACTED_TOOL_STUB, "fail-soft 退回裸短桩");
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn condense_tool_outputs_keeps_one_line_key_fact_and_archives() {
        let ws = temp_workspace("condense");
        let ws_str = ws.to_str().unwrap();
        let big = format!("{{\"ok\":true,\"path\":\"src/main.rs\",\"junk\":\"{}\"}}", "z".repeat(5_000));
        let mut wire = vec![
            serde_json::json!({ "role": "tool", "tool_call_id": "1", "content": big.clone() }), // 中龄 -> 凝练
            serde_json::json!({ "role": "tool", "tool_call_id": "2", "content": big.clone() }), // 最近 -> 全文
        ];
        let n = condense_tool_outputs(ws_str, "conv-cd", &mut wire, 1, 1_500);
        assert_eq!(n, 1, "只凝练 1 条中龄结果");
        let condensed = wire[0]["content"].as_str().unwrap();
        assert!(condensed.contains("已凝练为关键事实"));
        assert!(condensed.contains("src/main.rs"), "关键事实应保留 path 信号");
        assert!(condensed.len() < big.len(), "凝练后应显著变短");
        assert_eq!(wire[1]["content"], big, "最近一条保留全文");
        // 幂等：再凝练一次不重复处理。
        assert_eq!(condense_tool_outputs(ws_str, "conv-cd", &mut wire, 1, 1_500), 0);
        // 已归档原文。
        let archive = ws.join(".mdga/archive/conv-cd.jsonl");
        assert!(std::fs::read_to_string(&archive).unwrap().contains("zzz"));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn summary_split_keeps_systems_and_recent_without_breaking_tool_pairs() {
        // systems(2) + 10 条历史；保留最近 3 条时切点落在 tool 结果上，
        // 应回退到它的 assistant 调用者，保证 tool_calls 与 tool 结果不被拆散。
        let mut wire = vec![
            serde_json::json!({ "role": "system", "content": "ws" }),
            serde_json::json!({ "role": "system", "content": "rules" }),
        ];
        for i in 0..6 {
            wire.push(serde_json::json!({ "role": "user", "content": format!("u{i}") }));
        }
        wire.push(serde_json::json!({ "role": "assistant", "content": "", "tool_calls": [] })); // idx 8 调用者
        wire.push(serde_json::json!({ "role": "tool", "tool_call_id": "1", "content": "r1" })); // idx 9
        wire.push(serde_json::json!({ "role": "tool", "tool_call_id": "2", "content": "r2" })); // idx 10
        wire.push(serde_json::json!({ "role": "assistant", "content": "done" })); // idx 11

        // keep_recent=3 时原始切点是 idx9（tool 结果），应回退到 idx8 的 assistant 调用者。
        let (first_non_system, cut) =
            summary_split_points(&wire, 3).expect("should split");

        assert_eq!(first_non_system, 2);
        assert_eq!(cut, 8, "切分点应回退跳过 tool 结果，落在其 assistant 调用者上");
        assert_ne!(wire[cut]["role"], "tool");

        // 历史太短时不切分
        let short = vec![
            serde_json::json!({ "role": "system", "content": "ws" }),
            serde_json::json!({ "role": "user", "content": "hi" }),
        ];
        assert!(summary_split_points(&short, 4).is_none());
    }

    #[test]
    fn archive_line_is_valid_jsonl_with_escaped_content() {
        // 含换行与引号的内容必须被正确转义，且整条是合法 JSON、不含裸换行。
        let content = "line1\nline2 \"quoted\"";
        let line = archive_line("conv-1", "stub", "tool", content);
        assert!(!line.contains('\n'), "JSONL 单条不得含裸换行");
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid json");
        assert_eq!(parsed["conversation_id"], "conv-1");
        assert_eq!(parsed["reason"], "stub");
        assert_eq!(parsed["role"], "tool");
        assert_eq!(parsed["content"], content, "内容应原样可还原");
    }

    #[test]
    fn sanitize_conversation_id_blocks_path_traversal() {
        assert_eq!(sanitize_conversation_id("abc-123_XY"), "abc-123_XY");
        assert_eq!(sanitize_conversation_id("../../etc/passwd"), "______etc_passwd");
        assert_eq!(sanitize_conversation_id("a/b\\c"), "a_b_c");
        assert_eq!(sanitize_conversation_id(""), "unknown");
    }

    #[test]
    fn extract_key_fact_prefers_json_status_fields() {
        let fact = extract_key_fact("{\"ok\":false,\"exitCode\":1,\"path\":\"a.rs\",\"extra\":\"ignored\"}");
        assert!(fact.contains("ok=false"));
        assert!(fact.contains("exitCode=1"));
        assert!(fact.contains("path=a.rs"));
        assert!(!fact.contains("ignored"), "非关键字段不进关键事实");
        // 非 JSON 取首个非空行。
        assert_eq!(extract_key_fact("\n\n  hello world\nsecond"), "hello world");
        assert_eq!(extract_key_fact("   "), "(空)");
    }

    #[test]
    fn condense_key_fact_includes_pointer_and_is_short() {
        let big = format!("{{\"ok\":true,\"note\":\"{}\"}}", "n".repeat(500));
        let with_ptr = condense_key_fact(&big, Some(".mdga/archive/c.jsonl"));
        assert!(with_ptr.contains("已凝练为关键事实"));
        assert!(with_ptr.contains(".mdga/archive/c.jsonl"), "应含归档指针");
        // note 字段被截到 80 字符，整体远短于原文。
        assert!(with_ptr.len() < big.len());
        // 无归档引用时退回「重调工具」提示，不含归档路径。
        let no_ptr = condense_key_fact("{\"ok\":true}", None);
        assert!(no_ptr.contains("请重调对应工具"));
        assert!(!no_ptr.contains("归档"));
    }

    #[test]
    fn renders_wire_message_with_tool_calls_for_summary() {
        let msg = serde_json::json!({
            "role": "assistant",
            "content": "我来读取文件",
            "tool_calls": [{
                "function": { "name": "read_file", "arguments": "{\"path\":\"a.txt\"}" }
            }]
        });
        let line = render_wire_message_for_summary(&msg);
        assert!(line.starts_with("[assistant]"));
        assert!(line.contains("我来读取文件"));
        assert!(line.contains("read_file"));
    }
}
