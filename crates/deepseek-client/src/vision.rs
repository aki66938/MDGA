//! 视觉模型调用（Plan18 M18.1）：把图片 + 用户意图发给独立的视觉模型，取回文本分析。
//!
//! 同时兼容两种 API 格式（见 Plan18 §4 对照表），由 provider 配置的 `api_format` 决定分支：
//! - openai：POST {base_url}/chat/completions，Bearer 鉴权，image_url(data: URI)，取 choices[0].message.content
//! - anthropic：POST {base_url}/v1/messages，x-api-key + anthropic-version，image(base64 source)，取 content[0].text
//!
//! base_url 一律用用户在视觉 provider 里自填的值（视觉不强制走 preset 官方端点）。

use crate::DeepSeekError;
use reqwest::Client;

/// 单张图片：媒体类型（如 "image/png"）+ base64 编码（不含 data: 前缀）。
pub type VisionImage = (String, String);

/// Anthropic 格式必填的 max_tokens 默认值（视觉分析输出通常不长，2048 足够要点化总结）。
const ANTHROPIC_MAX_TOKENS: u32 = 2048;

/// 解析 Anthropic messages 端点：与 [`crate::chat_completions_url`] 同理，兼容「基址 / 含 /v1 / 完整端点」三种输入，
/// 避免把 `https://api.anthropic.com/v1/messages` 拼成 `…/v1/messages/v1/messages`，或把 `…/v1` 拼成 `…/v1/v1/messages`。
fn anthropic_messages_url(base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if base.ends_with("/messages") {
        base.to_string()
    } else {
        // 去掉可能已带的尾部 /v1，再统一补 /v1/messages。
        format!("{}/v1/messages", base.trim_end_matches("/v1"))
    }
}

/// 把图片 + 意图发给视觉模型，返回文本分析。
///
/// 输入视觉 provider 的 base_url（用户自填，为空报错）、api_key、model、api_format（openai|anthropic），
/// 以及桥接提示词的 system / user_text 与图片列表；按格式构造请求体、解析响应，返回模型文本。
/// 不重试（上层 send_message 容错降级），错误分类复用 DeepSeekError。
pub async fn analyze_image(
    base_url: &str,
    api_key: &str,
    model: &str,
    api_format: &str,
    system_prompt: &str,
    user_text: &str,
    images: &[VisionImage],
) -> Result<String, DeepSeekError> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(DeepSeekError::BadRequest(
            "视觉模型 Base URL 未配置：请在 设置 → 模型供应商 → 视觉 填写".to_string(),
        ));
    }
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(DeepSeekError::Http)?;

    if api_format == "anthropic" {
        let url = anthropic_messages_url(base);
        let body = build_anthropic_body(model, system_prompt, user_text, images);
        let response = client
            .post(url)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            return Err(crate::classify_api_error(status, &text));
        }
        let value = response.json::<serde_json::Value>().await?;
        parse_anthropic_response(&value)
    } else {
        let url = crate::chat_completions_url(base);
        let body = build_openai_body(model, system_prompt, user_text, images);
        let response = client
            .post(url)
            .bearer_auth(api_key)
            .json(&body)
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            return Err(crate::classify_api_error(status, &text));
        }
        let value = response.json::<serde_json::Value>().await?;
        parse_openai_response(&value)
    }
}

/// 构造 OpenAI 格式视觉请求体：messages content 用 text + image_url(data: URI)。
fn build_openai_body(
    model: &str,
    system_prompt: &str,
    user_text: &str,
    images: &[VisionImage],
) -> serde_json::Value {
    let mut content = vec![serde_json::json!({ "type": "text", "text": user_text })];
    for (media_type, data) in images {
        content.push(serde_json::json!({
            "type": "image_url",
            "image_url": { "url": format!("data:{media_type};base64,{data}") }
        }));
    }
    serde_json::json!({
        "model": model,
        "stream": false,
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": content }
        ]
    })
}

/// 构造 Anthropic 格式视觉请求体：顶层 system，messages content 用 text + image(base64 source)，
/// 必填 max_tokens。
fn build_anthropic_body(
    model: &str,
    system_prompt: &str,
    user_text: &str,
    images: &[VisionImage],
) -> serde_json::Value {
    let mut content = vec![serde_json::json!({ "type": "text", "text": user_text })];
    for (media_type, data) in images {
        content.push(serde_json::json!({
            "type": "image",
            "source": { "type": "base64", "media_type": media_type, "data": data }
        }));
    }
    serde_json::json!({
        "model": model,
        "max_tokens": ANTHROPIC_MAX_TOKENS,
        "system": system_prompt,
        "messages": [
            { "role": "user", "content": content }
        ]
    })
}

/// 从 OpenAI 响应取 choices[0].message.content。
fn parse_openai_response(value: &serde_json::Value) -> Result<String, DeepSeekError> {
    value
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| DeepSeekError::BadRequest("视觉响应缺少 choices[0].message.content".to_string()))
}

/// 从 Anthropic 响应取 content[0].text（content 为块数组，取首个 text 块）。
fn parse_anthropic_response(value: &serde_json::Value) -> Result<String, DeepSeekError> {
    value
        .get("content")
        .and_then(|v| v.as_array())
        .and_then(|blocks| blocks.iter().find_map(|b| b.get("text").and_then(|t| t.as_str())))
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| DeepSeekError::BadRequest("视觉响应缺少 content[].text".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_images() -> Vec<VisionImage> {
        vec![("image/png".to_string(), "AAAA".to_string())]
    }

    #[test]
    fn openai_body_uses_image_url_data_uri() {
        let body = build_openai_body("glm-4v", "你是视觉助手", "用户需求：实现成组件", &sample_images());
        assert_eq!(body["model"], "glm-4v");
        assert_eq!(body["stream"], false);
        // system 消息在前。
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "你是视觉助手");
        // user content：首块 text，次块 image_url(data: URI)。
        let content = &body["messages"][1]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "用户需求：实现成组件");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,AAAA");
        // 不应出现 anthropic 字段。
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("system").is_none());
    }

    #[test]
    fn anthropic_body_uses_base64_source_and_max_tokens() {
        let body = build_anthropic_body("claude-3-5-sonnet", "你是视觉助手", "用户需求：修这个报错", &sample_images());
        assert_eq!(body["model"], "claude-3-5-sonnet");
        // 顶层 system + 必填 max_tokens。
        assert_eq!(body["system"], "你是视觉助手");
        assert_eq!(body["max_tokens"], ANTHROPIC_MAX_TOKENS);
        // user content：text + image(base64 source)。
        let content = &body["messages"][0]["content"];
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/png");
        assert_eq!(content[1]["source"]["data"], "AAAA");
        // 不应出现 openai 字段。
        assert!(body.get("stream").is_none());
    }

    #[test]
    fn parses_openai_content() {
        let v = serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": "图中是登录表单" } }]
        });
        assert_eq!(parse_openai_response(&v).expect("ok"), "图中是登录表单");
    }

    #[test]
    fn parses_anthropic_text_block() {
        let v = serde_json::json!({
            "content": [{ "type": "text", "text": "第3行报错：未定义变量" }]
        });
        assert_eq!(parse_anthropic_response(&v).expect("ok"), "第3行报错：未定义变量");
    }

    #[test]
    fn empty_or_missing_content_is_error() {
        assert!(parse_openai_response(&serde_json::json!({})).is_err());
        assert!(parse_anthropic_response(&serde_json::json!({ "content": [] })).is_err());
        // 空白文本视为无效。
        let blank = serde_json::json!({ "choices": [{ "message": { "content": "  " } }] });
        assert!(parse_openai_response(&blank).is_err());
    }

    #[test]
    fn openai_url_handles_base_and_full_endpoint() {
        // 用户填基址 → 追加。
        assert_eq!(
            crate::chat_completions_url("https://api.deepseek.com"),
            "https://api.deepseek.com/chat/completions"
        );
        // 用户照文档粘贴完整端点 → 原样用，不重复拼接（硅基流动这类）。
        assert_eq!(
            crate::chat_completions_url("https://api.siliconflow.cn/v1/chat/completions"),
            "https://api.siliconflow.cn/v1/chat/completions"
        );
        // 含 /v1 基址 → 追加。
        assert_eq!(
            crate::chat_completions_url("https://api.siliconflow.cn/v1/"),
            "https://api.siliconflow.cn/v1/chat/completions"
        );
    }

    #[test]
    fn anthropic_url_handles_base_v1_and_full_endpoint() {
        // 仅基址 → /v1/messages。
        assert_eq!(
            anthropic_messages_url("https://api.anthropic.com"),
            "https://api.anthropic.com/v1/messages"
        );
        // 完整端点 → 原样用。
        assert_eq!(
            anthropic_messages_url("https://api.anthropic.com/v1/messages"),
            "https://api.anthropic.com/v1/messages"
        );
        // 含 /v1 → 不重复拼成 /v1/v1/messages。
        assert_eq!(
            anthropic_messages_url("https://proxy.example.com/v1"),
            "https://proxy.example.com/v1/messages"
        );
    }

    #[tokio::test]
    async fn empty_base_url_errors_without_request() {
        let err = analyze_image("  ", "k", "m", "openai", "s", "u", &sample_images())
            .await
            .expect_err("should error");
        assert!(matches!(err, DeepSeekError::BadRequest(_)));
    }
}
