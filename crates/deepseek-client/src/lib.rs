use futures_util::StreamExt;
use mdga_shared::{ApiKeyStatus, RawUsage};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DeepSeekError {
    #[error("DEEPSEEK_API_KEY 未配置")]
    MissingApiKey,
    #[error("认证失败，请检查 API Key 是否正确")]
    Unauthorized,
    #[error("余额不足，请前往 DeepSeek 平台充值")]
    InsufficientBalance,
    #[error("请求被限流，请稍后重试")]
    RateLimited,
    #[error("请求参数错误: {0}")]
    BadRequest(String),
    #[error("上下文长度超限")]
    ContextLengthExceeded,
    #[error("服务端错误，请稍后重试")]
    ServerError,
    #[error("网络连接失败: {0}")]
    Http(#[from] reqwest::Error),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunctionCall,
}

#[derive(Clone, Debug)]
pub struct ChatCompletionResult {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub assistant_message: serde_json::Value,
    pub usage: Option<RawUsage>,
}

/// 检测当前进程是否能读取 DeepSeek API Key。
///
/// 输入环境变量读取闭包，输出脱敏后的 API Key 状态；
/// 本方法不返回、不记录、不持久化 Key 明文。
pub fn detect_api_key_status(read_env: impl FnOnce(&str) -> Option<String>) -> ApiKeyStatus {
    match read_env("DEEPSEEK_API_KEY") {
        Some(v) if !v.trim().is_empty() => ApiKeyStatus::Configured,
        _ => ApiKeyStatus::Missing,
    }
}

/// 将 HTTP 状态码和响应体映射为可理解的 DeepSeekError。
///
/// 输入状态码和原始响应文本，输出分类错误；
/// 本方法不重试，重试策略由调用方决定。
fn classify_api_error(status: u16, body: &str) -> DeepSeekError {
    match status {
        401 => DeepSeekError::Unauthorized,
        402 => DeepSeekError::InsufficientBalance,
        429 => DeepSeekError::RateLimited,
        400 => {
            // 上下文超限会走 400
            if body.contains("context") || body.contains("length") {
                DeepSeekError::ContextLengthExceeded
            } else {
                DeepSeekError::BadRequest(body.chars().take(200).collect())
            }
        }
        500..=599 => DeepSeekError::ServerError,
        _ => DeepSeekError::BadRequest(body.chars().take(200).collect()),
    }
}

/// 向 DeepSeek API 发起流式聊天请求，通过回调逐块推送内容，完成后返回原始 usage。
///
/// 输入 API Key、消息列表和模型名；每收到内容 chunk 调用一次 on_chunk；
/// 流结束后返回服务端原始 usage（若缺失则返回 None）；
/// 错误时流中断，on_chunk 不再被调用。
pub async fn chat_stream<F>(
    api_key: &str,
    messages: Vec<ChatMessage>,
    model: &str,
    on_chunk: F,
) -> Result<Option<RawUsage>, DeepSeekError>
where
    F: Fn(String),
{
    let client = Client::new();

    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true }
    });

    let response = client
        .post("https://api.deepseek.com/chat/completions")
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        return Err(classify_api_error(status, &body));
    }

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut captured_usage: Option<RawUsage> = None;

    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        buffer.push_str(&String::from_utf8_lossy(&bytes));

        // 逐行解析 SSE，每条完整行单独处理
        loop {
            match buffer.find('\n') {
                None => break,
                Some(pos) => {
                    let line = buffer[..pos].trim().to_string();
                    buffer = buffer[pos + 1..].to_string();

                    let Some(data) = line.strip_prefix("data: ") else {
                        continue;
                    };

                    if data == "[DONE]" {
                        return Ok(captured_usage);
                    }

                    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
                        continue;
                    };

                    // 末尾 usage chunk（stream_options.include_usage）：choices 为空数组
                    if let Some(usage_val) = value.get("usage") {
                        if !usage_val.is_null() {
                            captured_usage = Some(parse_raw_usage(usage_val, data));
                        }
                    }

                    // 内容 chunk：从 choices[0].delta.content 取文本
                    if let Some(content) = value
                        .pointer("/choices/0/delta/content")
                        .and_then(|v| v.as_str())
                    {
                        if !content.is_empty() {
                            on_chunk(content.to_string());
                        }
                    }
                }
            }
        }
    }

    Ok(captured_usage)
}

/// 向 DeepSeek API 发起一次非流式聊天请求，可携带 tool schema。
///
/// 输入 API Key、消息 JSON、模型名和可选工具列表；输出 assistant 内容、tool calls 和 usage。
/// 本方法不执行工具，只负责解析模型意图。
pub async fn chat_completion(
    api_key: &str,
    messages: Vec<serde_json::Value>,
    model: &str,
    tools: Option<Vec<serde_json::Value>>,
) -> Result<ChatCompletionResult, DeepSeekError> {
    let client = Client::new();
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false
    });

    if let Some(tools) = tools.filter(|tools| !tools.is_empty()) {
        body["tools"] = serde_json::Value::Array(tools);
        body["tool_choice"] = serde_json::json!("auto");
    }

    let response = client
        .post("https://api.deepseek.com/chat/completions")
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        return Err(classify_api_error(status, &body));
    }

    let value = response.json::<serde_json::Value>().await?;
    parse_chat_completion_response(&value)
}

/// 将服务端 usage JSON 解析为 RawUsage。
///
/// 输入 usage 字段的 serde_json::Value 和原始 JSON 字符串，输出标准化结构；
/// 缺失字段保留为 0，raw_json 保存完整原始字符串供审计。
fn parse_raw_usage(usage: &serde_json::Value, raw_data: &str) -> RawUsage {
    RawUsage {
        prompt_tokens: usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
        completion_tokens: usage
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        total_tokens: usage.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
        prompt_cache_hit_tokens: usage
            .get("prompt_cache_hit_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        prompt_cache_miss_tokens: usage
            .get("prompt_cache_miss_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        reasoning_tokens: usage
            .pointer("/completion_tokens_details/reasoning_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        raw_json: raw_data.to_string(),
    }
}

fn parse_chat_completion_response(
    value: &serde_json::Value,
) -> Result<ChatCompletionResult, DeepSeekError> {
    let assistant_message = value
        .pointer("/choices/0/message")
        .cloned()
        .ok_or_else(|| DeepSeekError::BadRequest("响应缺少 choices[0].message".to_string()))?;
    let content = assistant_message
        .get("content")
        .and_then(|content| content.as_str())
        .map(str::to_string);
    let tool_calls = assistant_message
        .get("tool_calls")
        .cloned()
        .map(serde_json::from_value::<Vec<ToolCall>>)
        .transpose()
        .map_err(|err| DeepSeekError::BadRequest(err.to_string()))?
        .unwrap_or_default();
    let usage = value
        .get("usage")
        .filter(|usage| !usage.is_null())
        .map(|usage| parse_raw_usage(usage, &value.to_string()));

    Ok(ChatCompletionResult {
        content,
        tool_calls,
        assistant_message,
        usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_call_completion_response() {
        let raw = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "create_file",
                            "arguments": "{\"path\":\"test.txt\",\"content\":\"\"}"
                        }
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15,
                "prompt_cache_hit_tokens": 0,
                "prompt_cache_miss_tokens": 10
            }
        });

        let parsed = parse_chat_completion_response(&raw).expect("response should parse");

        assert_eq!(parsed.content, None);
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].id, "call_1");
        assert_eq!(parsed.tool_calls[0].function.name, "create_file");
        assert_eq!(parsed.usage.expect("usage should parse").total_tokens, 15);
    }
}
