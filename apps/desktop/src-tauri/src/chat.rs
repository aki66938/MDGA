//! 与 DeepSeek 模型对话的桥接层：带退避重试的补全 / 流式工具轮、同族备用模型切换、
//! 工具调用兜底解析、ChatMessage ↔ wire 消息互转。
//!
//! 从 main.rs 抽出（Plan16）：纯代码搬移，无行为变更。

use mdga_deepseek_client::{
    chat_completion, chat_stream_with_tools, parse_dsml_tool_calls, ChatMessage, StreamChunk,
    ToolCall,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

/// 带退避重试的 chat_completion，容忍偶发网络抖动 / 5xx / 限流，避免一次瞬时失败打断整轮长任务。
///
/// 可重试错误（网络收发失败、服务端错误、429）按 0.5s→1s→2s 退避重试，最多 4 次；
/// 确定性错误（认证、余额、参数、上下文超限）立即返回不重试。重试时向前端推送提示。
pub(crate) async fn chat_completion_with_retry(
    base_url: &str,
    api_key: &str,
    messages: Vec<serde_json::Value>,
    model: &str,
    tools: Option<Vec<serde_json::Value>>,
    app: &AppHandle,
) -> Result<mdga_deepseek_client::ChatCompletionResult, String> {
    const MAX_ATTEMPTS: u32 = 4;
    let mut attempt = 0;
    loop {
        attempt += 1;
        match chat_completion(base_url, api_key, messages.clone(), model, tools.clone(), None).await {
            Ok(result) => return Ok(result),
            Err(err) if err.is_retryable() && attempt < MAX_ATTEMPTS => {
                let delay_ms = 500u64 * 2u64.pow(attempt - 1);
                let _ = app.emit(
                    "chat-chunk",
                    format!("\n\n（网络波动，正在重试（第 {attempt}/{} 次）…）\n\n", MAX_ATTEMPTS - 1),
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            Err(err) => return Err(err.to_string()),
        }
    }
}

/// 带退避重试的流式工具轮：边流边把叙述 token 推到前端（"chat-chunk"），同时累积 tool_calls。
///
/// 仅在「尚未外显任何内容」时才对可重试错误重试，避免重试导致内容重复外显；
/// 一旦已开始流式输出再失败，则直接返回错误。
/// 返回同族备用模型：flash↔pro 互为 fallback，持续失败（如 overload）时切换求生。
pub(crate) fn fallback_model_for(model: &str) -> Option<&'static str> {
    match model {
        "deepseek-v4-flash" => Some("deepseek-v4-pro"),
        "deepseek-v4-pro" => Some("deepseek-v4-flash"),
        _ => None,
    }
}

/// 用户点停止时,stream_round_with_retry 返回此标记串;调用方据此 return Ok(已完成 usage),
/// 而非当作错误走 ?(那会丢已流式显示的内容、走前端 catch 不落库)。
pub(crate) const STREAM_CANCELLED: &str = "__MDGA_STREAM_CANCELLED__";

/// 轮询等待 cancel 置位,供 tokio::select! 与流式请求赛跑。
pub(crate) async fn wait_for_cancel(cancel: &Arc<AtomicBool>) {
    loop {
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn stream_round_with_retry(
    base_url: &str,
    api_key: &str,
    messages: Vec<serde_json::Value>,
    model: &str,
    tools: Vec<serde_json::Value>,
    // 思考深度（B-4）：方言相关思考字段，透传给 chat_stream_with_tools 注入请求体（None 时不注入）。
    thinking_extra: Option<&serde_json::Value>,
    app: &AppHandle,
    cancel: &Arc<AtomicBool>,
) -> Result<mdga_deepseek_client::ChatCompletionResult, String> {
    const MAX_ATTEMPTS: u32 = 4;
    let mut model = model.to_string();
    let mut fallback_used = false;
    let mut attempt = 0;
    loop {
        attempt += 1;
        // 流式开始前先看一眼:已取消则直接停,不再发起本轮请求。
        if cancel.load(Ordering::SeqCst) {
            return Err(STREAM_CANCELLED.to_string());
        }
        let emitted = Arc::new(AtomicBool::new(false));
        let emitted_cb = emitted.clone();
        let app_cb = app.clone();
        // 让流式请求与 cancel 轮询赛跑:用户点停止 → wait_for_cancel 先完成 → select! drop 掉流式
        // future(HTTP 连接随之中断),当轮立即停下,返回取消标记(已 emit 的增量不丢)。
        let result = tokio::select! {
            r = chat_stream_with_tools(
                base_url,
                api_key,
                messages.clone(),
                &model,
                tools.clone(),
                thinking_extra,
                move |chunk| {
                    // Plan27 C1（#1a）：正文增量走 "chat-chunk"，推理过程增量走 "chat-reasoning"。
                    emitted_cb.store(true, Ordering::SeqCst);
                    match chunk {
                        StreamChunk::Content(c) => {
                            let _ = app_cb.emit("chat-chunk", c.to_string());
                        }
                        StreamChunk::Reasoning(r) => {
                            let _ = app_cb.emit("chat-reasoning", r.to_string());
                        }
                    }
                },
            ) => r,
            _ = wait_for_cancel(cancel) => {
                return Err(STREAM_CANCELLED.to_string());
            }
        };
        match result {
            Ok(value) => return Ok(value),
            Err(err) if err.is_retryable() && !emitted.load(Ordering::SeqCst) => {
                if attempt < MAX_ATTEMPTS {
                    let delay_ms = 500u64 * 2u64.pow(attempt - 1);
                    let _ = app.emit(
                        "chat-chunk",
                        format!("\n\n（网络波动，正在重试（第 {attempt}/{} 次）…）\n\n", MAX_ATTEMPTS - 1),
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                } else if !fallback_used {
                    // 主模型多次失败（overload/限流）：切同族备用模型再试一轮。
                    match fallback_model_for(&model) {
                        Some(fb) => {
                            let _ = app.emit(
                                "chat-chunk",
                                format!("\n\n（主模型持续不可用，已切换备用模型 {fb} 重试…）\n\n"),
                            );
                            model = fb.to_string();
                            fallback_used = true;
                            attempt = 0;
                        }
                        None => return Err(err.to_string()),
                    }
                } else {
                    return Err(err.to_string());
                }
            }
            Err(err) => return Err(err.to_string()),
        }
    }
}

pub(crate) fn recover_tool_calls_from_content(content: &str) -> Vec<ToolCall> {
    parse_dsml_tool_calls(content)
}

pub(crate) fn assistant_message_for_tool_calls(
    assistant_message: serde_json::Value,
    // 思考深度（C）：本轮 reasoning_content（仅当上层 echo 策略为 Resend 时才传 Some）。
    // 非空时嵌进返回的 assistant JSON（满足部分模型「多轮工具须回传 reasoning」契约）；
    // None / 空时不加任何字段，保持原有行为。
    reasoning_content: Option<&str>,
    tool_calls: &[ToolCall],
) -> serde_json::Value {
    // 思考深度（C）：把非空 reasoning 嵌进给定的 assistant JSON 对象（只附加字段，不动 tool_calls 规整逻辑）。
    fn with_reasoning(mut msg: serde_json::Value, reasoning: Option<&str>) -> serde_json::Value {
        if let (Some(rc), Some(map)) = (reasoning, msg.as_object_mut()) {
            if !rc.is_empty() {
                map.insert(
                    "reasoning_content".to_string(),
                    serde_json::Value::String(rc.to_string()),
                );
            }
        }
        msg
    }

    // 0.0.69 修正:早返条件须是「存在且**非空**」的 tool_calls。流式客户端恒写 `"tool_calls": []`(即便
    // 为空),旧版 `.is_some()` 对空数组也命中早返,致 DSML/正文兜底恢复出的 tool_calls **没挂回** assistant
    // ——wire 出现无配对 tool_call_id 的 tool 消息(撞 Anthropic/OpenAI 400),还被快照持久化、续接每轮重放
    // 卡死。改判非空:仅当模型真给了原生 tool_calls 才原样返回,否则用恢复出的 tool_calls 重建 assistant 消息。
    if assistant_message
        .get("tool_calls")
        .and_then(|calls| calls.as_array())
        .is_some_and(|calls| !calls.is_empty())
    {
        return with_reasoning(assistant_message, reasoning_content);
    }

    with_reasoning(
        serde_json::json!({
            "role": "assistant",
            "content": null,
            "tool_calls": tool_calls
        }),
        reasoning_content,
    )
}

pub(crate) fn chat_messages_to_wire(messages: Vec<ChatMessage>) -> Vec<serde_json::Value> {
    messages
        .into_iter()
        .map(|message| {
            serde_json::json!({
                "role": message.role,
                "content": message.content,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdga_deepseek_client::ToolFunctionCall;

    fn tc(id: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            kind: "function".to_string(),
            function: ToolFunctionCall {
                name: "run_command".to_string(),
                arguments: "{}".to_string(),
            },
        }
    }

    #[test]
    fn dsml_recovered_tool_calls_attach_when_native_empty() {
        // 0.0.69 回归:流式客户端恒写 tool_calls:[](空数组),旧版 is_some() 误命中早返 → DSML/正文恢复出的
        // calls 丢失,wire 出现无配对 tool_call_id 撞 400。现应把恢复出的 calls 挂回重建的 assistant。
        let native_empty =
            serde_json::json!({"role":"assistant","content":"<ToolCall>...","tool_calls":[]});
        let out = assistant_message_for_tool_calls(native_empty, None, &[tc("dsml_call_0")]);
        let calls = out
            .get("tool_calls")
            .and_then(|c| c.as_array())
            .expect("应挂回 tool_calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "dsml_call_0");
    }

    #[test]
    fn native_nonempty_tool_calls_preserved() {
        // 模型真给了原生 tool_calls:原样返回,不被恢复值覆盖。
        let native = serde_json::json!({"role":"assistant","content":null,
            "tool_calls":[{"id":"native_0","type":"function","function":{"name":"read_file","arguments":"{}"}}]});
        let out = assistant_message_for_tool_calls(native, None, &[tc("should_not_use")]);
        assert_eq!(out["tool_calls"].as_array().unwrap()[0]["id"], "native_0");
    }

    #[test]
    fn reasoning_embedded_when_resend() {
        // 思考深度（C）：echo=Resend 时传 Some(reasoning)，应嵌进 assistant JSON。
        let native = serde_json::json!({"role":"assistant","content":null,
            "tool_calls":[{"id":"n0","type":"function","function":{"name":"read_file","arguments":"{}"}}]});
        let out = assistant_message_for_tool_calls(native, Some("我的思考过程"), &[tc("x")]);
        assert_eq!(out["reasoning_content"], "我的思考过程");
        // tool_calls 规整逻辑不受影响。
        assert_eq!(out["tool_calls"].as_array().unwrap()[0]["id"], "n0");
    }

    #[test]
    fn reasoning_omitted_when_none_or_empty() {
        // None / 空串都不应加 reasoning_content 字段（保持原行为）。
        let native = serde_json::json!({"role":"assistant","content":"<ToolCall>...","tool_calls":[]});
        let out = assistant_message_for_tool_calls(native.clone(), None, &[tc("dsml_call_0")]);
        assert!(out.get("reasoning_content").is_none());
        let out2 = assistant_message_for_tool_calls(native, Some(""), &[tc("dsml_call_0")]);
        assert!(out2.get("reasoning_content").is_none());
    }
}
