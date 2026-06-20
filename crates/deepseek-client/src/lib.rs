use futures_util::StreamExt;
use mdga_shared::{ApiKeyStatus, RawUsage};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod vision;
pub use vision::{analyze_image, VisionImage};

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

/// 内置预设供应商的官方 OpenAI 兼容端点（base_url，不含 /chat/completions 后缀）。
///
/// 输入预设标识（deepseek/zhipu/moonshot/qwen），输出官方 base_url；未知预设返回 None
/// （custom 预设不在此表，必须由用户显式填 base_url）。清单可按需增补。
pub fn preset_base_url(preset: &str) -> Option<&'static str> {
    match preset {
        "deepseek" => Some("https://api.deepseek.com"),
        "zhipu" => Some("https://open.bigmodel.cn/api/paas/v4"),
        "moonshot" => Some("https://api.moonshot.cn/v1"),
        "qwen" => Some("https://dashscope.aliyuncs.com/compatible-mode/v1"),
        // 0.0.71:硅基流动(OpenAI 兼容聚合,可接多家开源模型)。Anthropic 暂不加 preset——主对话循环
        // chat_stream 仅 OpenAI 格式(api_format=anthropic 只用于 test_connection/probe/vision),加 anthropic
        // preset 会让用户以为能当主模型却在循环里失败,留待主循环支持 anthropic 格式后再补。
        "siliconflow" => Some("https://api.siliconflow.cn/v1"),
        _ => None,
    }
}

/// 解析最终生效的 base_url：优先用显式 base_url（自定义覆盖），为空时回退到 preset 官方端点。
///
/// 输入可空的 base_url 与可空的 preset；任一可得即返回去掉尾部斜杠的串；都缺失返回 None
/// （上层应据此报「未配置」错误）。
pub fn resolve_base_url(base_url: Option<&str>, preset: Option<&str>) -> Option<String> {
    let explicit = base_url.map(str::trim).filter(|s| !s.is_empty());
    if let Some(url) = explicit {
        return Some(url.trim_end_matches('/').to_string());
    }
    preset
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(preset_base_url)
        .map(|url| url.trim_end_matches('/').to_string())
}

/// 解析最终 chat/completions 端点：兼容用户填「基址」或「完整端点」两种输入，避免重复拼接路径。
///
/// 各平台 API 文档给的端点写法不一：有的给基址（`https://api.deepseek.com`），有的给完整端点
/// （`https://api.siliconflow.cn/v1/chat/completions`）。用户照文档整段粘贴时若仍盲目追加，会拼成
/// `…/chat/completions/chat/completions` 导致 404/参数错误。故：已以 `/chat/completions` 结尾就原样用，
/// 否则才追加。
pub(crate) fn chat_completions_url(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_string()
    } else {
        format!("{base}/chat/completions")
    }
}

/// 解析 Anthropic messages 端点：与 [`chat_completions_url`] 同理，兼容「基址 / 含 /v1 / 完整端点」三种输入，
/// 避免把 `https://api.anthropic.com/v1/messages` 拼成 `…/v1/messages/v1/messages`，或把 `…/v1` 拼成 `…/v1/v1/messages`。
pub(crate) fn anthropic_messages_url(base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if base.ends_with("/messages") {
        base.to_string()
    } else {
        // 去掉可能已带的尾部 /v1，再统一补 /v1/messages。
        format!("{}/v1/messages", base.trim_end_matches("/v1"))
    }
}

/// 对供应商做一次最小连通性探测（Plan19 C-A「测试连接」）。
///
/// 输入 base_url、api_key、model、api_format（openai|anthropic）；用极小请求（prompt "ping"、
/// max_tokens 极小）打一次端点：`anthropic` 走 `/v1/messages`（x-api-key + anthropic-version，纯文本 user 消息），
/// 否则走 `/chat/completions`（Bearer）。base_url 复用既有的 [`chat_completions_url`] / [`anthropic_messages_url`]
/// 容错拼接。成功（HTTP 2xx）返回 Ok(())，失败用 [`classify_api_error`] 人话化为 DeepSeekError。
/// 本方法不重试、不流式，只判连通与鉴权是否可用。
pub async fn test_connection(
    base_url: &str,
    api_key: &str,
    model: &str,
    api_format: &str,
) -> Result<(), DeepSeekError> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(DeepSeekError::BadRequest(
            "Base URL 未配置：请填写供应商端点".to_string(),
        ));
    }
    if api_key.trim().is_empty() {
        return Err(DeepSeekError::MissingApiKey);
    }
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(DeepSeekError::Http)?;

    let response = if api_format == "anthropic" {
        // Anthropic：max_tokens 必填，取最小值 1；system 省略，纯文本 user 消息。
        let body = serde_json::json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{ "role": "user", "content": "ping" }]
        });
        client
            .post(anthropic_messages_url(base))
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await?
    } else {
        // OpenAI 兼容：非流式，max_tokens 极小，纯文本 user 消息。
        let body = serde_json::json!({
            "model": model,
            "stream": false,
            "max_tokens": 1,
            "messages": [{ "role": "user", "content": "ping" }]
        });
        client
            .post(chat_completions_url(base))
            .bearer_auth(api_key)
            .json(&body)
            .send()
            .await?
    };

    if response.status().is_success() {
        Ok(())
    } else {
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        Err(classify_api_error(status, &text))
    }
}

/// 对供应商做一次「工具调用」冒烟探测（Plan25 契约 C-1 / #3）。
///
/// 输入 base_url、api_key、model、api_format（openai|anthropic）；发一个极小请求，提供一个
/// trivial 函数工具（name="ping"，无必填参数），`max_tokens` 取小。判定逻辑：
/// **成功 = 响应里出现原生 tool_calls，或正文能被兜底解析（`parse_dsml_tool_calls`，含本 Plan
/// 新增的泄漏格式）恢复出至少一个 tool_call** → 返回 Ok(true)；模型只回文本、不调用工具 →
/// 返回 Ok(false)；网络/鉴权/参数等错误 → 返回 Err（用 [`classify_api_error`] 人话化）。
///
/// openai 格式走 `/chat/completions`（messages + tools 字段 + tool_choice="auto"，Bearer 鉴权），
/// anthropic 走 `/v1/messages`（tools 字段为 Anthropic schema，x-api-key + anthropic-version 鉴权）。
/// base_url 复用既有 [`chat_completions_url`] / [`anthropic_messages_url`] 容错拼接。本方法不重试、不流式。
pub async fn probe_tool_call(
    base_url: &str,
    api_key: &str,
    model: &str,
    api_format: &str,
) -> Result<bool, DeepSeekError> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(DeepSeekError::BadRequest(
            "Base URL 未配置：请填写供应商端点".to_string(),
        ));
    }
    if api_key.trim().is_empty() {
        return Err(DeepSeekError::MissingApiKey);
    }
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(DeepSeekError::Http)?;

    // 引导模型「无脑」调用 ping：尽量逼出 tool_call，降低只回文本的概率。
    let prompt = "Call the `ping` tool now. Do not reply with text.";

    let value = if api_format == "anthropic" {
        // Anthropic：tools 用 input_schema（JSON Schema），max_tokens 必填、取小；tool_choice 让其优先调用。
        let body = serde_json::json!({
            "model": model,
            "max_tokens": 64,
            "messages": [{ "role": "user", "content": prompt }],
            "tools": [{
                "name": "ping",
                "description": "A trivial no-op probe tool. Call it to acknowledge.",
                "input_schema": { "type": "object", "properties": {} }
            }]
        });
        let response = client
            .post(anthropic_messages_url(base))
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            return Err(classify_api_error(status, &text));
        }
        let value = response.json::<serde_json::Value>().await?;
        return Ok(anthropic_response_has_tool_call(&value));
    } else {
        // OpenAI 兼容：tools 用 function schema，tool_choice="auto"，max_tokens 取小。
        let body = serde_json::json!({
            "model": model,
            "stream": false,
            "max_tokens": 64,
            "messages": [{ "role": "user", "content": prompt }],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "ping",
                    "description": "A trivial no-op probe tool. Call it to acknowledge.",
                    "parameters": { "type": "object", "properties": {} }
                }
            }],
            "tool_choice": "auto"
        });
        let response = client
            .post(chat_completions_url(base))
            .bearer_auth(api_key)
            .json(&body)
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            return Err(classify_api_error(status, &text));
        }
        response.json::<serde_json::Value>().await?
    };

    Ok(openai_response_has_tool_call(&value))
}

/// 判断 OpenAI 兼容响应是否「调用了工具」：先看原生 `choices[0].message.tool_calls`，
/// 再退回正文 `choices[0].message.content` 走兜底解析恢复。
fn openai_response_has_tool_call(value: &serde_json::Value) -> bool {
    if value
        .pointer("/choices/0/message/tool_calls")
        .and_then(|v| v.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false)
    {
        return true;
    }
    value
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .map(|c| !parse_dsml_tool_calls(c).is_empty())
        .unwrap_or(false)
}

/// 判断 Anthropic 响应是否「调用了工具」：先看 `content` 数组里是否含 `type=="tool_use"` 块，
/// 再把其中的文本块拼起来走兜底解析恢复（兼容把工具调用泄漏进文本的情况）。
fn anthropic_response_has_tool_call(value: &serde_json::Value) -> bool {
    let Some(blocks) = value.get("content").and_then(|v| v.as_array()) else {
        return false;
    };
    let mut text = String::new();
    for block in blocks {
        match block.get("type").and_then(|v| v.as_str()) {
            Some("tool_use") => return true,
            Some("text") => {
                if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                    text.push_str(t);
                    text.push('\n');
                }
            }
            _ => {}
        }
    }
    !parse_dsml_tool_calls(&text).is_empty()
}

impl DeepSeekError {
    /// 判断该错误是否为瞬时、可重试的错误。
    ///
    /// 网络收发失败、服务端 5xx、限流 429 属于可重试；认证失败、余额不足、参数错误、
    /// 上下文超限属于确定性错误，重试无意义。用于让长任务的 Agent 循环容忍偶发网络抖动。
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            DeepSeekError::Http(_) | DeepSeekError::ServerError | DeepSeekError::RateLimited
        )
    }
}

/// 流式回调推送的增量分片：区分「外显正文」与「推理过程」两类。
///
/// Plan27 C1（#1a 推理可见）：DeepSeek 等模型的 SSE 流里，`delta.content` 是面向用户的
/// 最终回答正文，`delta.reasoning_content` 是模型的思考过程。二者需分流到不同的前端事件
/// （`chat-chunk` / `chat-reasoning`），故回调形参由 `&str` 升级为本枚举携带来源标签。
/// 借用底层缓冲区字符串切片，生命周期 `'a` 不超过单次回调调用，零拷贝。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamChunk<'a> {
    /// 外显正文增量（仍走防泄漏守卫，剔除工具调用标记后才外显）。
    Content(&'a str),
    /// 推理过程增量（原样流出，不走防泄漏守卫）。
    Reasoning(&'a str),
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

/// 根据「主 provider 是否已配置且 key 非空」给出 API Key 状态（Plan17 D3：不再读环境变量）。
///
/// 输入主 provider 的 api_key（None 表示未配置任何主 provider），输出脱敏状态；
/// 本方法不返回、不记录、不持久化 Key 明文。
pub fn detect_api_key_status(api_key: Option<&str>) -> ApiKeyStatus {
    match api_key {
        Some(v) if !v.trim().is_empty() => ApiKeyStatus::Configured,
        _ => ApiKeyStatus::Missing,
    }
}

/// 将 HTTP 状态码和响应体映射为可理解的 DeepSeekError。
///
/// 输入状态码和原始响应文本，输出分类错误；
/// 本方法不重试，重试策略由调用方决定。
pub(crate) fn classify_api_error(status: u16, body: &str) -> DeepSeekError {
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

/// 向 OpenAI 兼容端点发起流式聊天请求，通过回调逐块推送内容，完成后返回原始 usage。
///
/// 输入 base_url（已解析的官方/自定义端点）、API Key、消息列表和模型名；每收到分片调用一次
/// `on_chunk`，参数为 [`StreamChunk`]：正文增量为 `Content`、推理过程增量为 `Reasoning`；
/// 流结束后返回服务端原始 usage（若缺失则返回 None）；错误时流中断，`on_chunk` 不再被调用。
pub async fn chat_stream<F>(
    base_url: &str,
    api_key: &str,
    messages: Vec<ChatMessage>,
    model: &str,
    mut on_chunk: F,
) -> Result<Option<RawUsage>, DeepSeekError>
where
    F: FnMut(StreamChunk<'_>),
{
    // 流式请求只限连接超时，不设总超时（长回复的流式读取可能远超固定时长）。
    let client = Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(DeepSeekError::Http)?;

    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true }
    });

    let response = client
        .post(chat_completions_url(base_url))
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

                    // 推理 chunk：从 choices[0].delta.reasoning_content 取文本，原样流出（不走守卫）
                    if let Some(reasoning) = value
                        .pointer("/choices/0/delta/reasoning_content")
                        .and_then(|v| v.as_str())
                    {
                        if !reasoning.is_empty() {
                            on_chunk(StreamChunk::Reasoning(reasoning));
                        }
                    }

                    // 内容 chunk：从 choices[0].delta.content 取文本
                    if let Some(content) = value
                        .pointer("/choices/0/delta/content")
                        .and_then(|v| v.as_str())
                    {
                        if !content.is_empty() {
                            on_chunk(StreamChunk::Content(content));
                        }
                    }
                }
            }
        }
    }

    Ok(captured_usage)
}

/// 查找正文中工具调用标记（DSML / ToolCall 各变体）的最早字节位置；无标记返回 None。
fn tool_markup_index(content: &str) -> Option<usize> {
    let stripped = strip_dsml_bars(content);
    // 在去竖线后的串里找标记，再映射回原串大致位置不可靠；改为直接在原串找各变体前缀。
    ["<DSML", "<ToolCall", "<\u{FF5C}"]
        .iter()
        .filter_map(|m| content.find(m))
        .min()
        .or_else(|| {
            // 去竖线后才暴露的 <DSML（原串是 <｜DSML）：用 stripped 命中则保守地从首个 '<' 截断
            if stripped.contains("<DSML") || stripped.contains("<ToolCall") {
                content.find('<')
            } else {
                None
            }
        })
}

/// 带工具的流式聊天：边流边把**安全的**叙述内容通过回调推送（token 级），
/// 同时累积 delta.tool_calls，结束后返回结构化结果。
///
/// 回调参数为 [`StreamChunk`]：正文增量为 `Content`、推理过程增量为 `Reasoning`。
/// 内置防泄漏守卫：一旦检测到正文出现工具调用标记（DSML / `<ToolCall>`），停止外显后续内容，
/// 把整段标记留给上层 DSML 兜底解析，避免标记 token 流到界面。完整 content 仍在返回值里供解析。
/// 推理增量（`Reasoning`）原样流出，**不**走防泄漏守卫。
pub async fn chat_stream_with_tools<F>(
    base_url: &str,
    api_key: &str,
    messages: Vec<serde_json::Value>,
    model: &str,
    tools: Vec<serde_json::Value>,
    mut on_content: F,
) -> Result<ChatCompletionResult, DeepSeekError>
where
    F: FnMut(StreamChunk<'_>),
{
    const GUARD: usize = 12; // 末尾保留窗口，防止正在形成的标记被提前外显
    let client = Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(DeepSeekError::Http)?;
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true }
    });
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(tools);
        body["tool_choice"] = serde_json::json!("auto");
    }

    let response = client
        .post(chat_completions_url(base_url))
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        return Err(classify_api_error(status, &text));
    }

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut content_full = String::new();
    let mut emitted_bytes = 0usize; // 已外显的 content 字节数
    let mut leaked = false;
    let mut usage: Option<RawUsage> = None;
    // tool_calls 累积：按 index 收集 id / name / arguments 片段
    let mut tool_acc: Vec<(String, String, String)> = Vec::new();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        buffer.push_str(&String::from_utf8_lossy(&bytes));
        loop {
            let Some(pos) = buffer.find('\n') else { break };
            let line = buffer[..pos].trim().to_string();
            buffer = buffer[pos + 1..].to_string();
            let Some(data) = line.strip_prefix("data: ") else { continue };
            if data == "[DONE]" {
                break;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else { continue };
            if let Some(usage_val) = value.get("usage") {
                if !usage_val.is_null() {
                    usage = Some(parse_raw_usage(usage_val, data));
                }
            }
            // 累积 tool_calls 片段
            if let Some(calls) = value
                .pointer("/choices/0/delta/tool_calls")
                .and_then(|v| v.as_array())
            {
                for call in calls {
                    let idx = call.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    while tool_acc.len() <= idx {
                        tool_acc.push((String::new(), String::new(), String::new()));
                    }
                    if let Some(id) = call.get("id").and_then(|v| v.as_str()) {
                        if !id.is_empty() {
                            tool_acc[idx].0 = id.to_string();
                        }
                    }
                    if let Some(name) = call.pointer("/function/name").and_then(|v| v.as_str()) {
                        if !name.is_empty() {
                            tool_acc[idx].1 = name.to_string();
                        }
                    }
                    if let Some(args) = call.pointer("/function/arguments").and_then(|v| v.as_str()) {
                        tool_acc[idx].2.push_str(args);
                    }
                }
            }
            // 推理片段：原样外显，不走防泄漏守卫
            if let Some(reasoning) = value
                .pointer("/choices/0/delta/reasoning_content")
                .and_then(|v| v.as_str())
            {
                if !reasoning.is_empty() {
                    on_content(StreamChunk::Reasoning(reasoning));
                }
            }
            // 内容片段：守卫式外显
            if let Some(delta) = value
                .pointer("/choices/0/delta/content")
                .and_then(|v| v.as_str())
            {
                if !delta.is_empty() {
                    content_full.push_str(delta);
                    if !leaked {
                        if let Some(marker) = tool_markup_index(&content_full) {
                            // 命中标记：外显到标记前，之后停止外显
                            if marker > emitted_bytes {
                                on_content(StreamChunk::Content(&content_full[emitted_bytes..marker]));
                            }
                            emitted_bytes = content_full.len();
                            leaked = true;
                        } else {
                            // 未命中：外显到 len-GUARD（在字符边界上）
                            let safe_end = content_full.len().saturating_sub(GUARD);
                            if safe_end > emitted_bytes && content_full.is_char_boundary(safe_end) {
                                on_content(StreamChunk::Content(&content_full[emitted_bytes..safe_end]));
                                emitted_bytes = safe_end;
                            }
                        }
                    }
                }
            }
        }
    }
    // 收尾：未泄漏时把保留窗口里的安全尾巴补发
    if !leaked && content_full.len() > emitted_bytes {
        on_content(StreamChunk::Content(&content_full[emitted_bytes..]));
    }

    let tool_calls: Vec<ToolCall> = tool_acc
        .into_iter()
        .enumerate()
        .filter(|(_, (_, name, _))| !name.is_empty())
        .map(|(i, (id, name, args))| ToolCall {
            id: if id.is_empty() { format!("call_{i}") } else { id },
            kind: "function".to_string(),
            function: ToolFunctionCall { name, arguments: args },
        })
        .collect();

    let assistant_message = serde_json::json!({
        "role": "assistant",
        "content": if content_full.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(content_full.clone()) },
        "tool_calls": tool_calls,
    });

    Ok(ChatCompletionResult {
        content: if content_full.is_empty() { None } else { Some(content_full) },
        tool_calls,
        assistant_message,
        usage,
    })
}

/// 向 DeepSeek API 发起一次非流式聊天请求，可携带 tool schema。
///
/// 输入 API Key、消息 JSON、模型名和可选工具列表；输出 assistant 内容、tool calls 和 usage。
/// 本方法不执行工具，只负责解析模型意图。
pub async fn chat_completion(
    base_url: &str,
    api_key: &str,
    messages: Vec<serde_json::Value>,
    model: &str,
    tools: Option<Vec<serde_json::Value>>,
) -> Result<ChatCompletionResult, DeepSeekError> {
    // 必须设置总超时：服务端 hang 时若无超时会无限等待，前端表现为「正在思考」永不出 token。
    // 超时映射为可重试的 Http 错误，由上层退避重试。
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(DeepSeekError::Http)?;
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
        .post(chat_completions_url(base_url))
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

/// 单一币种的余额明细（金额为字符串，保留服务端原始精度）。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceInfo {
    pub currency: String,
    pub total_balance: String,
    pub granted_balance: String,
    pub topped_up_balance: String,
}

/// 账户余额信息（DeepSeek 公开账户接口仅提供余额，无更多用户资料）。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserBalance {
    pub is_available: bool,
    pub balance_infos: Vec<BalanceInfo>,
}

/// 查询 DeepSeek 账户余额。
///
/// 输入 API Key；GET /user/balance（Bearer 认证），返回可用状态与各币种余额明细。
/// 这是 DeepSeek 公开 API 唯一的账户信息接口，没有用户名等更多资料。
pub async fn get_user_balance(api_key: &str) -> Result<UserBalance, DeepSeekError> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(DeepSeekError::Http)?;
    let response = client
        .get("https://api.deepseek.com/user/balance")
        .bearer_auth(api_key)
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        return Err(classify_api_error(status, &body));
    }
    let value = response.json::<serde_json::Value>().await?;
    parse_user_balance(&value)
}

/// 解析 OpenAI 兼容的 `/models` 端点（base 不含 /models 后缀时自动追加，含则原样用）。
///
/// 与 [`chat_completions_url`] 同款容错：用户可能填基址（`https://api.deepseek.com`）或
/// 已带 `/v1` 的串；已以 `/models` 结尾就原样用，否则追加 `/models`。
pub(crate) fn models_url(base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if base.ends_with("/models") {
        base.to_string()
    } else {
        format!("{base}/models")
    }
}

/// GET {base}/models 列出该端点可用模型 id（0.0.60「拉取可用模型」）。
///
/// 输入已解析的 base_url 与 api_key；走 OpenAI 兼容约定：Bearer 鉴权，解析
/// `{"data":[{"id":...}]}`（也接受裸数组 `[{"id":...}]`）为模型 id 列表。10–12s 超时。
/// 失败（端点无 /models、网络、鉴权、解析）返回 Err，由上层回退到手动输入。
/// **绝不记录 api_key**：仅作为 Bearer 头使用。
pub async fn fetch_models(base_url: &str, api_key: &str) -> Result<Vec<String>, DeepSeekError> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(DeepSeekError::BadRequest(
            "Base URL 未配置：请填写供应商端点".to_string(),
        ));
    }
    if api_key.trim().is_empty() {
        return Err(DeepSeekError::MissingApiKey);
    }
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(DeepSeekError::Http)?;
    let response = client
        .get(models_url(base))
        .bearer_auth(api_key)
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        return Err(classify_api_error(status, &body));
    }
    let value = response.json::<serde_json::Value>().await?;
    let ids = parse_model_ids(&value);
    if ids.is_empty() {
        return Err(DeepSeekError::BadRequest(
            "该端点 /models 未返回任何模型，请手动输入模型 ID".to_string(),
        ));
    }
    Ok(ids)
}

/// 从 /models 响应里抽出模型 id 列表：优先 `data` 数组，否则把顶层裸数组当作条目数组；
/// 每个条目取其 `id` 字段（字符串），去重保序。
fn parse_model_ids(value: &serde_json::Value) -> Vec<String> {
    let items = value
        .get("data")
        .and_then(|v| v.as_array())
        .or_else(|| value.as_array());
    let Some(items) = items else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for item in items {
        // 条目可能是对象 {"id":...}，少数端点直接给字符串。
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .or_else(|| item.as_str());
        if let Some(id) = id {
            let id = id.trim();
            if !id.is_empty() && seen.insert(id.to_string()) {
                out.push(id.to_string());
            }
        }
    }
    out
}

/// 把 /user/balance 的原始 JSON（snake_case）解析为 UserBalance，缺失字段安全降级。
fn parse_user_balance(value: &serde_json::Value) -> Result<UserBalance, DeepSeekError> {
    let is_available = value
        .get("is_available")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let balance_infos = value
        .get("balance_infos")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .map(|item| BalanceInfo {
                    currency: item.get("currency").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    total_balance: item.get("total_balance").and_then(|v| v.as_str()).unwrap_or("0").to_string(),
                    granted_balance: item.get("granted_balance").and_then(|v| v.as_str()).unwrap_or("0").to_string(),
                    topped_up_balance: item.get("topped_up_balance").and_then(|v| v.as_str()).unwrap_or("0").to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(UserBalance { is_available, balance_infos })
}

/// 将服务端 usage JSON 解析为 RawUsage。
///
/// 输入 usage 字段的 serde_json::Value 和原始 JSON 字符串，输出标准化结构；
/// 缺失字段保留为 0，raw_json 保存完整原始字符串供审计。
pub(crate) fn parse_raw_usage(usage: &serde_json::Value, raw_data: &str) -> RawUsage {
    let prompt_tokens = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    // DeepSeek 私有字段在场则原样;否则按 OpenAI 标准推导命中/未命中,保证 hit+miss==prompt_tokens。
    // 非 DeepSeek(zhipu/siliconflow/qwen 等 OpenAI 兼容端点)只回标准 prompt_tokens(至多
    // prompt_tokens_details.cached_tokens),若不兜底则 hit=miss=0 → 输入费=0 且分级选档恒落最低档。
    let ds_hit = usage.get("prompt_cache_hit_tokens").and_then(|v| v.as_u64());
    let ds_miss = usage.get("prompt_cache_miss_tokens").and_then(|v| v.as_u64());
    let (prompt_cache_hit_tokens, prompt_cache_miss_tokens) = match (ds_hit, ds_miss) {
        (Some(h), Some(m)) => (h, m), // DeepSeek 原样
        _ => {
            // OpenAI 兼容端点:cached_tokens 作命中,其余按 prompt_tokens 推导未命中。
            let cached = usage
                .pointer("/prompt_tokens_details/cached_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let hit = cached.min(prompt_tokens);
            (hit, prompt_tokens.saturating_sub(hit))
        }
    };
    RawUsage {
        prompt_tokens,
        completion_tokens: usage
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        total_tokens: usage.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
        prompt_cache_hit_tokens,
        prompt_cache_miss_tokens,
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

/// 从 DeepSeek 泄漏到正文的 DSML 工具标记中恢复 tool calls。
///
/// 输入 assistant 文本内容，输出可交给 Host 执行的 ToolCall 列表。DeepSeek 有时不会返回
/// OpenAI-compatible `tool_calls` 字段，而是把 DSML 标记直接输出到正文。不同模型版本会用
/// 不同数量的全角竖线（`｜DSML｜` 或 `｜｜DSML｜｜`），因此先归一化掉所有竖线再解析，
/// 让解析对竖线数量保持容忍。
///
/// 注意：归一化会移除正文中所有全角竖线 U+FF5C；DSML 兜底路径下文件路径/内容极少包含该字符，
/// 这是可接受的取舍。原生 tool_calls 路径不受影响。
pub fn parse_dsml_tool_calls(raw_content: &str) -> Vec<ToolCall> {
    let content = strip_dsml_bars(raw_content);
    // 先尝试 DSML 变体；没有命中时再尝试 XML 风格的 <ToolCall name="..."> 变体
    //（实测 DeepSeek 偶尔会输出这种格式，工具名/参数同样可恢复执行）。
    let calls = parse_markup_tool_calls(
        &content,
        "<DSMLinvoke name=\"",
        "</DSMLinvoke>",
        "<DSMLparameter name=\"",
        "</DSMLparameter>",
    );
    if !calls.is_empty() {
        return calls;
    }
    let calls = parse_markup_tool_calls(
        &content,
        "<ToolCall name=\"",
        "</ToolCall>",
        "<parameter name=\"",
        "</parameter>",
    );
    if !calls.is_empty() {
        return calls;
    }
    // DSML / <ToolCall> 都未命中时，再尝试「函数调用泄漏到正文」的常见宽松格式
    //（代码块 JSON、<tool_call>{json}</tool_call>、<function=NAME>{json}</function>）。
    // 此处用原始正文而非去竖线后的串：这些格式不含全角竖线，且 JSON 值里的竖线应原样保留。
    parse_leaked_tool_calls(raw_content)
}

/// 宽松识别「函数调用泄漏到正文」的常见格式并恢复为 ToolCall（Plan25 #3 兜底泛化）。
///
/// 在原生 tool_calls 与 DSML/`<ToolCall>` 路径都落空后兜底，支持三类格式：
/// ① Markdown 代码块 JSON（` ```json {...} ``` ` 或无语言标注的 ``` 围栏）：
///    单个 `{"name":..,"arguments":..}`、`{"name":..,"parameters":..}`、`{"function":{...}}`，
///    或 OpenAI 风格 `{"tool_calls":[{...}]}`；
/// ② `<tool_call>{json}</tool_call>`（json 同 ① 的单调用形态）；
/// ③ `<function=NAME>{json args}</function>`（标签名即工具名，体为参数对象）。
///
/// arguments 既支持对象（序列化为紧凑 JSON 串）也支持字符串（已是 JSON 串则原样透传）。
/// 任一格式恢复出 ≥1 个调用即返回，互不破坏：先扫描显式标签，再扫描代码块/裸 JSON。
pub fn parse_leaked_tool_calls(content: &str) -> Vec<ToolCall> {
    let mut calls = Vec::new();

    // ② <tool_call>{json}</tool_call>
    extract_tagged_json(content, "<tool_call>", "</tool_call>", |json, calls| {
        push_calls_from_json(json, calls);
    }, &mut calls);

    // ③ <function=NAME>{json args}</function>
    parse_function_tag_calls(content, &mut calls);

    if !calls.is_empty() {
        return calls;
    }

    // ① 代码块围栏内的 JSON：```json ... ``` 或裸 ``` ... ```
    for block in extract_code_block_jsons(content) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(block.trim()) {
            push_calls_from_value(&value, &mut calls);
        }
    }
    if !calls.is_empty() {
        return calls;
    }

    // 兜底的兜底：正文里没有围栏，但夹着一段裸 JSON 对象（含 name/tool_calls）。
    for json in extract_bare_json_objects(content) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&json) {
            push_calls_from_value(&value, &mut calls);
            if !calls.is_empty() {
                break;
            }
        }
    }

    calls
}

/// 解析 `<function=NAME>{json args}</function>` 形态：标签 `=` 后到 `>` 之间是工具名，体是参数对象。
fn parse_function_tag_calls(content: &str, calls: &mut Vec<ToolCall>) {
    const OPEN: &str = "<function=";
    const CLOSE: &str = "</function>";
    let mut cursor = 0;
    while let Some(offset) = content[cursor..].find(OPEN) {
        let name_start = cursor + offset + OPEN.len();
        let Some(gt_offset) = content[name_start..].find('>') else {
            break;
        };
        let name = content[name_start..name_start + gt_offset].trim();
        let body_start = name_start + gt_offset + 1;
        let Some(close_offset) = content[body_start..].find(CLOSE) else {
            break;
        };
        let body = content[body_start..body_start + close_offset].trim();
        if !name.is_empty() {
            let arguments = json_args_to_string(body);
            calls.push(make_recovered_call(calls.len(), name, arguments));
        }
        cursor = body_start + close_offset + CLOSE.len();
    }
}

/// 扫描所有 `<open>...</close>` 区段，对每段内容回调；用于 `<tool_call>{json}</tool_call>`。
fn extract_tagged_json(
    content: &str,
    open: &str,
    close: &str,
    mut on_body: impl FnMut(&str, &mut Vec<ToolCall>),
    calls: &mut Vec<ToolCall>,
) {
    let mut cursor = 0;
    while let Some(offset) = content[cursor..].find(open) {
        let body_start = cursor + offset + open.len();
        let Some(close_offset) = content[body_start..].find(close) else {
            break;
        };
        let body = &content[body_start..body_start + close_offset];
        on_body(body.trim(), calls);
        cursor = body_start + close_offset + close.len();
    }
}

/// 从 JSON 文本里恢复调用（用于 `<tool_call>` 体）。
fn push_calls_from_json(json: &str, calls: &mut Vec<ToolCall>) {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(json) {
        push_calls_from_value(&value, calls);
    }
}

/// 从一个 JSON 值里恢复调用，识别以下形态并归一为 ToolCall：
/// - `{"tool_calls":[{...}, ...]}`：逐个递归处理数组元素；
/// - `{"function":{"name":..,"arguments"/"parameters":..}}`（OpenAI 单调用对象）；
/// - `{"name":..,"arguments"/"parameters":..}`（裸函数调用）。
fn push_calls_from_value(value: &serde_json::Value, calls: &mut Vec<ToolCall>) {
    // OpenAI 风格批量：{"tool_calls":[...]}
    if let Some(arr) = value.get("tool_calls").and_then(|v| v.as_array()) {
        for item in arr {
            push_calls_from_value(item, calls);
        }
        return;
    }
    // 单调用对象，name 可能在顶层或嵌套在 function 里。
    let func = value.get("function").unwrap_or(value);
    let Some(name) = func.get("name").and_then(|v| v.as_str()) else {
        return;
    };
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    // arguments 优先，其次 parameters（不同模型/封装命名不一）。
    let args_value = func
        .get("arguments")
        .or_else(|| func.get("parameters"))
        .or_else(|| func.get("args"));
    let arguments = match args_value {
        Some(serde_json::Value::String(s)) => json_args_to_string(s),
        Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()),
        None => "{}".to_string(),
    };
    calls.push(make_recovered_call(calls.len(), name, arguments));
}

/// 把「参数体文本」归一为 JSON 串：若本身是合法 JSON（对象/串等）则压缩重序列化，
/// 否则按字符串字面量包装（极少见，保证下游 `serde_json::from_str` 不致 panic）。
fn json_args_to_string(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "{}".to_string();
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        // 已是合法 JSON：若为字符串值则原样取出（它本身可能就是 JSON 串），否则压缩重序列化。
        Ok(serde_json::Value::String(s)) => s,
        Ok(v) => serde_json::to_string(&v).unwrap_or_else(|_| trimmed.to_string()),
        Err(_) => serde_json::Value::String(trimmed.to_string()).to_string(),
    }
}

/// 构造一个「兜底恢复」的 ToolCall，id 以 leak_call_N 区分于原生/DSML 路径。
fn make_recovered_call(index: usize, name: &str, arguments: String) -> ToolCall {
    ToolCall {
        id: format!("leak_call_{index}"),
        kind: "function".to_string(),
        function: ToolFunctionCall {
            name: name.to_string(),
            arguments,
        },
    }
}

/// 抽出所有 Markdown 代码块围栏（```lang ... ```）内部文本，按出现顺序返回。
/// 兼容 ```json、```JSON、无语言标注的裸 ``` 三种围栏；不要求闭合（截断响应也尽量恢复）。
fn extract_code_block_jsons(content: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = content[cursor..].find("```") {
        let after_fence = cursor + offset + 3;
        // 跳过围栏后到行尾的语言标注（json / JSON / 空）。
        let line_end = content[after_fence..]
            .find('\n')
            .map(|p| after_fence + p + 1)
            .unwrap_or(content.len());
        let body_start = line_end;
        // 找闭合围栏；没有则取到结尾（容忍截断）。
        let (body_end, next) = match content[body_start..].find("```") {
            Some(p) => (body_start + p, body_start + p + 3),
            None => (content.len(), content.len()),
        };
        if body_end > body_start {
            blocks.push(content[body_start..body_end].to_string());
        }
        cursor = next;
    }
    blocks
}

/// 从正文里粗略切出顶层 `{...}` JSON 对象片段（按花括号配对，跳过字符串内的括号）。
/// 仅用于「正文夹裸 JSON」的最后兜底，返回所有平衡的顶层对象文本。
fn extract_bare_json_objects(content: &str) -> Vec<String> {
    let bytes = content.as_bytes();
    let mut objects = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            // 从此处开始做花括号配对扫描。
            let start = i;
            let mut depth = 0usize;
            let mut in_str = false;
            let mut escaped = false;
            let mut j = i;
            while j < bytes.len() {
                let c = bytes[j];
                if in_str {
                    if escaped {
                        escaped = false;
                    } else if c == b'\\' {
                        escaped = true;
                    } else if c == b'"' {
                        in_str = false;
                    }
                } else {
                    match c {
                        b'"' => in_str = true,
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                objects.push(content[start..=j].to_string());
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                j += 1;
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    objects
}

/// 通用的「标记式工具调用」解析器：按给定的 invoke/parameter 起止标记从正文恢复 ToolCall。
fn parse_markup_tool_calls(
    content: &str,
    invoke_marker: &str,
    invoke_end: &str,
    param_marker: &str,
    param_end: &str,
) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    let mut cursor = 0;

    while let Some(offset) = content[cursor..].find(invoke_marker) {
        let start = cursor + offset + invoke_marker.len();
        let Some(name_end_offset) = content[start..].find('"') else {
            break;
        };
        let name = &content[start..start + name_end_offset];
        let Some(open_end_offset) = content[start + name_end_offset..].find('>') else {
            break;
        };
        let body_start = start + name_end_offset + open_end_offset + 1;
        let Some(body_end_offset) = content[body_start..].find(invoke_end) else {
            break;
        };
        let body_end = body_start + body_end_offset;
        let params =
            parse_markup_parameters(&content[body_start..body_end], param_marker, param_end);
        let Ok(arguments) = serde_json::to_string(&params) else {
            cursor = body_end + invoke_end.len();
            continue;
        };

        calls.push(ToolCall {
            id: format!("dsml_call_{}", calls.len()),
            kind: "function".to_string(),
            function: ToolFunctionCall {
                name: name.to_string(),
                arguments,
            },
        });
        cursor = body_end + invoke_end.len();
    }

    calls
}

/// 移除全角竖线 U+FF5C，使 `<｜DSML｜...>` 与 `<｜｜DSML｜｜...>` 归一化为 `<DSML...>`。
fn strip_dsml_bars(content: &str) -> String {
    content.replace('\u{FF5C}', "")
}

/// 从将要展示给用户的正文中清除工具调用标记（DSML 与 <ToolCall> 两种变体），避免泄漏成可见文本。
///
/// DeepSeek 偶尔把工具调用直接吐进正文（尤其在不带 tools 的收尾请求里）。工具调用
/// 总是出现在叙述之后，因此从第一个标记处截断，保留前面的自然语言叙述，丢弃整段标记。
/// 正文中若没有标记则原样返回。
pub fn strip_dsml_markup(content: &str) -> String {
    let normalized = strip_dsml_bars(content);
    let cut = [normalized.find("<DSML"), normalized.find("<ToolCall")]
        .into_iter()
        .flatten()
        .min();
    match cut {
        Some(pos) => normalized[..pos].trim_end().to_string(),
        None => content.to_string(),
    }
}

fn parse_markup_parameters(
    body: &str,
    param_marker: &str,
    param_end: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut params = serde_json::Map::new();
    let mut cursor = 0;

    while let Some(offset) = body[cursor..].find(param_marker) {
        let start = cursor + offset + param_marker.len();
        let Some(name_end_offset) = body[start..].find('"') else {
            break;
        };
        let name = &body[start..start + name_end_offset];
        let Some(open_end_offset) = body[start + name_end_offset..].find('>') else {
            break;
        };
        let value_start = start + name_end_offset + open_end_offset + 1;
        let Some(value_end_offset) = body[value_start..].find(param_end) else {
            break;
        };
        let value_end = value_start + value_end_offset;
        let value = normalize_dsml_parameter(name, &body[value_start..value_end]);
        params.insert(name.to_string(), serde_json::Value::String(value));
        cursor = value_end + param_end.len();
    }

    params
}

fn normalize_dsml_parameter(name: &str, value: &str) -> String {
    let trimmed = value.trim();
    if name == "path" {
        trimmed.trim_start_matches(['\\', '/']).to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_base_url_covers_known_presets() {
        // 0.0.71:硅基流动(OpenAI 兼容)preset 解析到官方端点;未知 preset(含 anthropic,主循环暂不支持)为 None。
        assert_eq!(preset_base_url("siliconflow"), Some("https://api.siliconflow.cn/v1"));
        assert_eq!(preset_base_url("deepseek"), Some("https://api.deepseek.com"));
        assert_eq!(preset_base_url("anthropic"), None);
        assert_eq!(preset_base_url("custom"), None);
        // resolve:显式 base_url 覆盖 preset;空 base_url 回退 preset 官方端点。
        assert_eq!(
            resolve_base_url(None, Some("siliconflow")).as_deref(),
            Some("https://api.siliconflow.cn/v1")
        );
    }

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

    #[test]
    fn parse_raw_usage_openai_with_cached_tokens() {
        // OpenAI 兼容端点:有 prompt_tokens_details.cached_tokens → 命中=cached,未命中=余下。
        let usage = serde_json::json!({
            "prompt_tokens": 5000,
            "prompt_tokens_details": { "cached_tokens": 1000 }
        });
        let r = parse_raw_usage(&usage, "{}");
        assert_eq!(r.prompt_tokens, 5000);
        assert_eq!(r.prompt_cache_hit_tokens, 1000);
        assert_eq!(r.prompt_cache_miss_tokens, 4000);
    }

    #[test]
    fn parse_raw_usage_openai_plain_prompt_tokens() {
        // 仅 prompt_tokens(无 details):命中=0,未命中=全部 prompt_tokens(不再漏算输入费)。
        let usage = serde_json::json!({ "prompt_tokens": 5000 });
        let r = parse_raw_usage(&usage, "{}");
        assert_eq!(r.prompt_tokens, 5000);
        assert_eq!(r.prompt_cache_hit_tokens, 0);
        assert_eq!(r.prompt_cache_miss_tokens, 5000);
    }

    #[test]
    fn parse_raw_usage_deepseek_private_fields_unchanged() {
        // DeepSeek 私有字段在场 → 原样使用,不被 OpenAI 兜底覆盖。
        let usage = serde_json::json!({
            "prompt_tokens": 5000,
            "prompt_cache_hit_tokens": 1000,
            "prompt_cache_miss_tokens": 4000
        });
        let r = parse_raw_usage(&usage, "{}");
        assert_eq!(r.prompt_tokens, 5000);
        assert_eq!(r.prompt_cache_hit_tokens, 1000);
        assert_eq!(r.prompt_cache_miss_tokens, 4000);
    }

    #[test]
    fn parses_single_bar_dsml_tool_call_from_content() {
        let content = r#"我会修改文件。
<｜DSML｜tool_calls><｜DSML｜invoke name="write_file"><｜DSML｜parameter name="path" string="true">\helloworld.txt</｜DSML｜parameter><｜DSML｜parameter name="content" string="true">123456</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls>"#;

        let calls = parse_dsml_tool_calls(content);

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "write_file");
        assert_eq!(calls[0].function.arguments, r#"{"content":"123456","path":"helloworld.txt"}"#);
    }

    #[test]
    fn parses_double_bar_dsml_tool_call_from_content() {
        // 真实 DeepSeek 输出使用双竖线，旧解析器（硬编码单竖线）在此会漏掉，导致工具调用泄漏成正文。
        let content = "好的。\n<｜｜DSML｜｜tool_calls> <｜｜DSML｜｜invoke name=\"edit_file\"> <｜｜DSML｜｜parameter name=\"path\" string=\"true\">helloworld.txt</｜｜DSML｜｜parameter> <｜｜DSML｜｜parameter name=\"oldText\" string=\"true\">helloworld</｜｜DSML｜｜parameter> <｜｜DSML｜｜parameter name=\"newText\" string=\"true\">123456</｜｜DSML｜｜parameter> </｜｜DSML｜｜invoke> </｜｜DSML｜｜tool_calls>";

        let calls = parse_dsml_tool_calls(content);

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "edit_file");
        let parsed: serde_json::Value =
            serde_json::from_str(&calls[0].function.arguments).expect("arguments should be json");
        assert_eq!(parsed["path"], "helloworld.txt");
        assert_eq!(parsed["oldText"], "helloworld");
        assert_eq!(parsed["newText"], "123456");
    }

    #[test]
    fn parses_user_balance_response() {
        let raw = serde_json::json!({
            "is_available": true,
            "balance_infos": [{
                "currency": "CNY",
                "total_balance": "110.00",
                "granted_balance": "10.00",
                "topped_up_balance": "100.00"
            }]
        });
        let balance = parse_user_balance(&raw).expect("should parse");
        assert!(balance.is_available);
        assert_eq!(balance.balance_infos.len(), 1);
        assert_eq!(balance.balance_infos[0].currency, "CNY");
        assert_eq!(balance.balance_infos[0].total_balance, "110.00");
        assert_eq!(balance.balance_infos[0].topped_up_balance, "100.00");
    }

    #[test]
    fn parses_user_balance_with_missing_fields() {
        let balance = parse_user_balance(&serde_json::json!({})).expect("should parse empty");
        assert!(!balance.is_available);
        assert!(balance.balance_infos.is_empty());
    }

    #[test]
    fn parses_model_ids_from_data_array_and_bare_array() {
        // OpenAI 形态：{"data":[{"id":...}]}，保序去重。
        let v = serde_json::json!({
            "object": "list",
            "data": [
                { "id": "deepseek-chat", "object": "model" },
                { "id": "deepseek-reasoner" },
                { "id": "deepseek-chat" }
            ]
        });
        assert_eq!(parse_model_ids(&v), vec!["deepseek-chat", "deepseek-reasoner"]);
        // 裸数组形态。
        let v2 = serde_json::json!([{ "id": "glm-4.6" }, { "id": "glm-4-flash" }]);
        assert_eq!(parse_model_ids(&v2), vec!["glm-4.6", "glm-4-flash"]);
        // 无 data 且非数组 ⇒ 空。
        assert!(parse_model_ids(&serde_json::json!({ "error": "x" })).is_empty());
    }

    #[test]
    fn models_url_tolerates_base_and_full_endpoint() {
        assert_eq!(models_url("https://api.deepseek.com"), "https://api.deepseek.com/models");
        assert_eq!(models_url("https://api.deepseek.com/"), "https://api.deepseek.com/models");
        assert_eq!(
            models_url("https://x/v1/models"),
            "https://x/v1/models",
            "已含 /models 后缀则原样用"
        );
    }

    #[test]
    fn classifies_retryable_errors() {
        assert!(DeepSeekError::ServerError.is_retryable());
        assert!(DeepSeekError::RateLimited.is_retryable());
        assert!(!DeepSeekError::Unauthorized.is_retryable());
        assert!(!DeepSeekError::InsufficientBalance.is_retryable());
        assert!(!DeepSeekError::ContextLengthExceeded.is_retryable());
        assert!(!DeepSeekError::BadRequest("x".to_string()).is_retryable());
    }

    #[test]
    fn strips_leaked_dsml_markup_keeping_narration() {
        // 模拟撞上限收尾时，模型把叙述 + DSML 调用一起吐进正文的情况。
        let content = "让我直接重写整个文件，移除所有 emoji 字符：\n\n<｜｜DSML｜｜tool_calls> <｜｜DSML｜｜invoke name=\"read_file\"> <｜｜DSML｜｜parameter name=\"path\" string=\"true\">src/CalculatorApp/calculator.py</｜｜DSML｜｜parameter> </｜｜DSML｜｜invoke> </｜｜DSML｜｜tool_calls>";

        let cleaned = strip_dsml_markup(content);

        assert_eq!(cleaned, "让我直接重写整个文件，移除所有 emoji 字符：");
        assert!(!cleaned.contains("DSML"));
    }

    #[test]
    fn strip_dsml_markup_returns_plain_text_untouched() {
        let content = "这是一段普通回复，没有任何工具标记。";
        assert_eq!(strip_dsml_markup(content), content);
    }

    #[test]
    fn parses_xmlish_toolcall_variant_from_content() {
        // 真实泄漏样本（0.0.17 dev 实测）：模型用 <ToolCall name="..."> XML 风格输出工具调用。
        let content = r#"好的，我来给 README.md 文件中增加一行 helloworld。

<ToolCall name="search_file"> <parameter name="target_directory" string="true">/</parameter> <parameter name="pattern" string="true">README.md</parameter> <parameter name="recursive" string="false">false</parameter> </ToolCall>"#;

        let calls = parse_dsml_tool_calls(content);

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "search_file");
        let parsed: serde_json::Value =
            serde_json::from_str(&calls[0].function.arguments).expect("arguments should be json");
        assert_eq!(parsed["pattern"], "README.md");
        assert_eq!(parsed["recursive"], "false");
    }

    #[test]
    fn tool_markup_index_detects_variants() {
        assert!(tool_markup_index("纯叙述，没有标记").is_none());
        assert!(tool_markup_index("先看文件 <DSMLinvoke").is_some());
        assert!(tool_markup_index("好的 <ToolCall name=").is_some());
        // 单/双竖线 DSML 变体
        assert!(tool_markup_index("好的 <\u{FF5C}DSML\u{FF5C}invoke").is_some());
        // 命中位置应在叙述之后
        let idx = tool_markup_index("叙述 <ToolCall x").unwrap();
        assert_eq!(&"叙述 <ToolCall x"[..idx], "叙述 ");
    }

    #[test]
    fn strips_leaked_toolcall_markup_keeping_narration() {
        let content = "我先找到文件：\n\n<ToolCall name=\"search_file\"> <parameter name=\"pattern\" string=\"true\">README.md</parameter> </ToolCall>";
        let cleaned = strip_dsml_markup(content);
        assert_eq!(cleaned, "我先找到文件：");
        assert!(!cleaned.contains("ToolCall"));
    }

    #[test]
    fn parses_multiple_dsml_tool_calls_from_content() {
        let content = "<｜｜DSML｜｜invoke name=\"read_file\"><｜｜DSML｜｜parameter name=\"path\" string=\"true\">a.txt</｜｜DSML｜｜parameter></｜｜DSML｜｜invoke><｜｜DSML｜｜invoke name=\"delete_file\"><｜｜DSML｜｜parameter name=\"path\" string=\"true\">b.txt</｜｜DSML｜｜parameter></｜｜DSML｜｜invoke>";

        let calls = parse_dsml_tool_calls(content);

        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(calls[1].function.name, "delete_file");
    }

    // ===== Plan25 #3 兜底泛化：泄漏格式恢复单测 =====

    #[test]
    fn recovers_code_block_json_with_object_arguments() {
        // ```json {"name":..,"arguments":{对象}} ```：arguments 为对象。
        let content = "我来读取文件：\n\n```json\n{\"name\":\"read_file\",\"arguments\":{\"path\":\"a.txt\"}}\n```";
        let calls = parse_dsml_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "read_file");
        let parsed: serde_json::Value =
            serde_json::from_str(&calls[0].function.arguments).expect("args should be json");
        assert_eq!(parsed["path"], "a.txt");
    }

    #[test]
    fn recovers_code_block_json_with_string_arguments() {
        // ```json {"name":..,"arguments":"{json串}"} ```：arguments 为字符串（内含 JSON）。
        let content =
            "```json\n{\"name\":\"write_file\",\"arguments\":\"{\\\"path\\\":\\\"b.txt\\\",\\\"content\\\":\\\"hi\\\"}\"}\n```";
        let calls = parse_dsml_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "write_file");
        let parsed: serde_json::Value =
            serde_json::from_str(&calls[0].function.arguments).expect("args should be json");
        assert_eq!(parsed["path"], "b.txt");
        assert_eq!(parsed["content"], "hi");
    }

    #[test]
    fn recovers_code_block_openai_tool_calls_envelope() {
        // ```json {"tool_calls":[{...}]} ```：OpenAI 批量信封。
        let content = "```json\n{\"tool_calls\":[{\"type\":\"function\",\"function\":{\"name\":\"list_dir\",\"arguments\":{\"path\":\".\"}}},{\"function\":{\"name\":\"read_file\",\"arguments\":{\"path\":\"x\"}}}]}\n```";
        let calls = parse_dsml_tool_calls(content);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.name, "list_dir");
        assert_eq!(calls[1].function.name, "read_file");
    }

    #[test]
    fn recovers_code_block_without_lang_tag() {
        // 裸 ``` 围栏（无 json 语言标注）也应恢复。
        let content = "```\n{\"name\":\"ping\",\"arguments\":{}}\n```";
        let calls = parse_dsml_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "ping");
        assert_eq!(calls[0].function.arguments, "{}");
    }

    #[test]
    fn recovers_tool_call_tag_with_object_arguments() {
        // <tool_call>{json}</tool_call>：arguments 为对象。
        let content = "好的。<tool_call>{\"name\":\"read_file\",\"arguments\":{\"path\":\"c.txt\"}}</tool_call>";
        let calls = parse_dsml_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "read_file");
        let parsed: serde_json::Value =
            serde_json::from_str(&calls[0].function.arguments).expect("args should be json");
        assert_eq!(parsed["path"], "c.txt");
    }

    #[test]
    fn recovers_tool_call_tag_with_string_arguments() {
        // <tool_call>{json}</tool_call>：arguments 为字符串形态。
        let content = "<tool_call>{\"name\":\"write_file\",\"arguments\":\"{\\\"path\\\":\\\"d.txt\\\"}\"}</tool_call>";
        let calls = parse_dsml_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "write_file");
        let parsed: serde_json::Value =
            serde_json::from_str(&calls[0].function.arguments).expect("args should be json");
        assert_eq!(parsed["path"], "d.txt");
    }

    #[test]
    fn recovers_function_tag_with_object_args() {
        // <function=NAME>{json args}</function>：体即参数对象。
        let content = "调用：<function=read_file>{\"path\":\"e.txt\"}</function>";
        let calls = parse_dsml_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "read_file");
        let parsed: serde_json::Value =
            serde_json::from_str(&calls[0].function.arguments).expect("args should be json");
        assert_eq!(parsed["path"], "e.txt");
    }

    #[test]
    fn recovers_function_tag_with_empty_args() {
        // <function=NAME></function>：空体应归一为空对象参数。
        let content = "<function=ping></function>";
        let calls = parse_dsml_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "ping");
        assert_eq!(calls[0].function.arguments, "{}");
    }

    #[test]
    fn recovers_function_tag_with_string_arg_body() {
        // <function=NAME>"{json串}"</function>：体是被引号包裹的 JSON 串（字符串形态）。
        let content = "<function=write_file>\"{\\\"path\\\":\\\"f.txt\\\"}\"</function>";
        let calls = parse_dsml_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "write_file");
        let parsed: serde_json::Value =
            serde_json::from_str(&calls[0].function.arguments).expect("args should be json");
        assert_eq!(parsed["path"], "f.txt");
    }

    #[test]
    fn recovers_bare_json_object_without_fence() {
        // 正文夹裸 JSON（无围栏、无标签）的最后兜底。
        let content = "这是结果 {\"name\":\"delete_file\",\"arguments\":{\"path\":\"g.txt\"}} 完成。";
        let calls = parse_dsml_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "delete_file");
        let parsed: serde_json::Value =
            serde_json::from_str(&calls[0].function.arguments).expect("args should be json");
        assert_eq!(parsed["path"], "g.txt");
    }

    #[test]
    fn native_and_dsml_paths_take_precedence_over_leaked() {
        // DSML 路径命中时不应再走泄漏兜底（优先级：原生 > DSML/ToolCall > 泄漏格式）。
        // 这里构造同时含 DSML 与代码块 JSON 的正文，断言只恢复出 DSML 的那一个。
        let content = "<｜｜DSML｜｜invoke name=\"dsml_win\"><｜｜DSML｜｜parameter name=\"path\" string=\"true\">a.txt</｜｜DSML｜｜parameter></｜｜DSML｜｜invoke>\n```json\n{\"name\":\"leaked_lose\",\"arguments\":{}}\n```";
        let calls = parse_dsml_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "dsml_win");
    }

    #[test]
    fn plain_text_yields_no_leaked_calls() {
        // 纯文本不应误恢复出任何调用。
        let content = "这只是一段普通文本，提到 read_file 但没有调用结构。";
        assert!(parse_dsml_tool_calls(content).is_empty());
    }

    // ===== Plan25 C-1 probe 判定逻辑单测（纯解析，不打网络） =====

    #[test]
    fn openai_probe_detects_native_tool_calls() {
        let value = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "c1", "type": "function",
                        "function": { "name": "ping", "arguments": "{}" }
                    }]
                }
            }]
        });
        assert!(openai_response_has_tool_call(&value));
    }

    #[test]
    fn openai_probe_recovers_from_leaked_content() {
        let value = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "```json\n{\"name\":\"ping\",\"arguments\":{}}\n```"
                }
            }]
        });
        assert!(openai_response_has_tool_call(&value));
    }

    #[test]
    fn openai_probe_text_only_is_false() {
        let value = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": "pong，我不会调用工具。" }
            }]
        });
        assert!(!openai_response_has_tool_call(&value));
    }

    #[test]
    fn anthropic_probe_detects_tool_use_block() {
        let value = serde_json::json!({
            "content": [
                { "type": "text", "text": "ok" },
                { "type": "tool_use", "id": "t1", "name": "ping", "input": {} }
            ]
        });
        assert!(anthropic_response_has_tool_call(&value));
    }

    #[test]
    fn anthropic_probe_recovers_from_leaked_text() {
        let value = serde_json::json!({
            "content": [
                { "type": "text", "text": "<tool_call>{\"name\":\"ping\",\"arguments\":{}}</tool_call>" }
            ]
        });
        assert!(anthropic_response_has_tool_call(&value));
    }

    #[test]
    fn anthropic_probe_text_only_is_false() {
        let value = serde_json::json!({
            "content": [ { "type": "text", "text": "我只回文本。" } ]
        });
        assert!(!anthropic_response_has_tool_call(&value));
    }
}
