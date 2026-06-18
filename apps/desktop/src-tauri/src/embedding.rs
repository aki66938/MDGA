//! 可选 provider embedding 重排后端（P2 / 0.0.58）。
//!
//! 默认**关闭**:不配置/不启用时,`code_search` 行为与 0.0.57 逐字节一致(纯本地 BM25 + PageRank)。
//! 仅当用户显式启用(设置项 `code_search_embedding=on` 或单次工具参数 `useEmbedding=true`)时,
//! 才在本地候选之上叠加一层 provider embedding 余弦重排——且任一失败(无 /embeddings 端点、网络/鉴权
//! 错误、超时、维度不一致)都**静默回退**纯本地排名,绝不破坏 code_search、绝不挂起、绝不打印 api key。
//!
//! ## 与 DB 解耦(沿用 LSP 配置快照同款做法)
//! `code_search` 在 `execute_builtin_tool_call` 里执行,那里没有 DB 句柄。这里用进程级 `RwLock`
//! 缓存「已解析的 embedding provider 接入参数 + 全局开关」:
//!
//! - 启动时由 main.rs 从 DB 播种([`refresh_embedding_config`]);
//! - 设置页保存模型 provider / 开关时由命令层刷新([`refresh_embedding_config`])。
//!
//! 工具执行只读该快照,零 DB 往返;未配置/未启用＝默认关闭。
//!
//! ## 端点
//! 复用主 provider(role=main)的 OpenAI 兼容接入:base_url 经 `resolve_base_url` 解析(preset 官方端点
//! 或自定义覆盖),POST `{base}/embeddings`,Bearer 鉴权,body `{ "model": .., "input": text }`,
//! 取 `data[0].embedding`。model 取设置项里用户填的 embedding 模型名,留空则用一个温和默认。

use mdga_codemap::Embedder;
use std::time::Duration;

/// 设置项键:全局 embedding 重排开关(值为 "on" 视为开启,其余/缺失＝关闭)。
pub(crate) const EMBEDDING_ENABLED_KEY: &str = "code_search_embedding";
/// 设置项键:embedding 模型名(留空走 [`DEFAULT_EMBEDDING_MODEL`])。
pub(crate) const EMBEDDING_MODEL_KEY: &str = "code_search_embedding_model";

/// 缺省 embedding 模型名(OpenAI 兼容生态最常见的通用名;provider 不支持时会自然走失败回退)。
const DEFAULT_EMBEDDING_MODEL: &str = "text-embedding-3-small";
/// 单次 embedding 请求的总超时(秒):宁可超时回退本地,绝不让 code_search 挂起。
const EMBED_TIMEOUT_SECS: u64 = 12;
/// 连接超时(秒)。
const EMBED_CONNECT_TIMEOUT_SECS: u64 = 6;

/// 已解析、可直接发请求的 embedding 接入参数。`None` 快照＝功能关闭。
#[derive(Clone, Debug)]
pub(crate) struct EmbeddingConfig {
    /// 已解析的 base_url(不含 /embeddings 后缀;preset 或自定义覆盖均已展开)。
    base_url: String,
    /// 用户自己的 provider api key(绝不打印/记录)。
    api_key: String,
    /// embedding 模型名。
    model: String,
}

/// 进程级快照:`Some` 表示「已配置且全局开关为 on」;`None`＝关闭(默认)。
static EMBEDDING_CONFIG: std::sync::RwLock<Option<EmbeddingConfig>> = std::sync::RwLock::new(None);

/// 写入当前生效的 embedding 配置快照(启动播种 / 设置保存后刷新调用)。`None` 关闭功能。
pub(crate) fn set_embedding_config(config: Option<EmbeddingConfig>) {
    if let Ok(mut guard) = EMBEDDING_CONFIG.write() {
        *guard = config;
    }
}

/// 取当前生效快照(无锁失败/未配置＝关闭)。
fn embedding_config_snapshot() -> Option<EmbeddingConfig> {
    EMBEDDING_CONFIG.read().ok().and_then(|g| g.clone())
}

/// 从一份主 provider 与设置值解析 embedding 配置:仅当全局开关为 on 且 provider 可解析出 base_url
/// 与非空 key 时返回 `Some`;否则 `None`(功能关闭)。不发起任何网络。
///
/// 入参刻意是「已从 DB 取出的原始值」,使本函数纯函数化、易测、与 storage 解耦。
pub(crate) fn resolve_embedding_config(
    enabled_setting: Option<&str>,
    model_setting: Option<&str>,
    provider_base_url: Option<&str>,
    provider_preset: Option<&str>,
    provider_api_key: &str,
) -> Option<EmbeddingConfig> {
    // 全局开关:仅 "on" 开启(大小写不敏感);其余/缺失＝关闭(默认离线)。
    if !enabled_setting
        .map(|s| s.trim().eq_ignore_ascii_case("on"))
        .unwrap_or(false)
    {
        return None;
    }
    let base_url =
        mdga_deepseek_client::resolve_base_url(provider_base_url, provider_preset)?;
    if base_url.trim().is_empty() || provider_api_key.trim().is_empty() {
        return None;
    }
    let model = model_setting
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_EMBEDDING_MODEL)
        .to_string();
    Some(EmbeddingConfig {
        base_url,
        api_key: provider_api_key.to_string(),
        model,
    })
}

/// 从 DB 读取主 provider 与 embedding 设置,解析并刷新进程级快照。
///
/// 启动播种与设置保存后均调用此函数:任一读失败 / 未启用 / provider 不可解析,都把快照置为 `None`
/// (功能关闭、回退本地),绝不报错冒泡。不发起网络。
pub(crate) fn refresh_embedding_config(conn: &rusqlite::Connection) {
    let enabled = mdga_storage::get_setting(conn, EMBEDDING_ENABLED_KEY)
        .ok()
        .flatten();
    let model = mdga_storage::get_setting(conn, EMBEDDING_MODEL_KEY)
        .ok()
        .flatten();
    // 0.0.59：embedding 端点/凭据从 ROLE_EMBED 解析（未配置 embed ⇒ 回退 main，与从前逐字节一致）。
    // 仅 provider 的 base_url/preset/api_key 取自该解析结果；embedding 模型名仍取上面的设置项。
    let provider = mdga_storage::resolve_role_provider(conn, mdga_storage::ROLE_EMBED)
        .ok()
        .flatten();
    let config = match provider {
        Some(p) => resolve_embedding_config(
            enabled.as_deref(),
            model.as_deref(),
            p.base_url.as_deref(),
            p.preset.as_deref(),
            &p.api_key,
        ),
        // 没有主 provider 时:即便开关 on 也无端点可用 → 关闭。
        None => None,
    };
    set_embedding_config(config);
}

/// 解析 `/embeddings` 端点:兼容用户填「基址」或「完整端点」,避免重复拼接(同 deepseek-client 的
/// chat_completions_url 容错思路)。
fn embeddings_url(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with("/embeddings") {
        base.to_string()
    } else {
        format!("{base}/embeddings")
    }
}

/// 若功能已启用且 provider 可用,返回一个可用于 `code_search_with_embedder` 的 embedder;否则 `None`
/// (调用方据此走纯本地路径)。`use_embedding_arg` 是单次工具参数覆盖:为 `Some(false)` 时即便全局
/// 开启也强制本次走本地(让模型可逐次选择只用离线)。
pub(crate) fn active_embedder(use_embedding_arg: Option<bool>) -> Option<ProviderEmbedder> {
    if use_embedding_arg == Some(false) {
        return None;
    }
    embedding_config_snapshot().map(ProviderEmbedder::new)
}

/// 基于用户 provider 的 OpenAI 兼容 `/embeddings` 端点的 [`Embedder`] 实现。
///
/// 失败语义:`embed` 永不 panic、永不挂起(硬超时),任一错误(无端点 404、鉴权、网络、超时、
/// 解析失败、维度异常)都返回 `None` → 让 codemap 静默回退该块/整体本地排名。绝不打印 api key。
pub(crate) struct ProviderEmbedder {
    config: EmbeddingConfig,
}

impl ProviderEmbedder {
    fn new(config: EmbeddingConfig) -> Self {
        Self { config }
    }

    /// 同步执行一次 embeddings 请求,返回向量;任一失败返回 None。
    ///
    /// 在专用 OS 线程里建一个 current-thread tokio runtime 跑 async reqwest——因为
    /// `code_search` 可能在已有 tokio runtime 的线程上同步调用,直接 block_on 会 panic;
    /// 隔离线程 + 自带 runtime 既避免该 panic,又用 join 同步等待结果(带硬超时,不会挂起)。
    fn embed_once(&self, text: &str) -> Option<Vec<f32>> {
        let url = embeddings_url(&self.config.base_url);
        let api_key = self.config.api_key.clone();
        let model = self.config.model.clone();
        let input = text.to_string();

        let handle = std::thread::spawn(move || -> Option<Vec<f32>> {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            rt.block_on(async move {
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_secs(EMBED_TIMEOUT_SECS))
                    .connect_timeout(Duration::from_secs(EMBED_CONNECT_TIMEOUT_SECS))
                    .build()
                    .ok()?;
                let body = serde_json::json!({ "model": model, "input": input });
                let resp = client
                    .post(&url)
                    .bearer_auth(&api_key)
                    .json(&body)
                    .send()
                    .await
                    .ok()?;
                if !resp.status().is_success() {
                    // 无 /embeddings 端点(404)、鉴权失败(401)等:不读 body 内容(避免误打日志),
                    // 直接降级。错误细节对 code_search 无用——它只需「能不能拿到向量」。
                    return None;
                }
                let value = resp.json::<serde_json::Value>().await.ok()?;
                parse_embedding(&value)
            })
        });
        // join 失败(线程 panic,理论上不会)亦视作不可用。
        handle.join().ok().flatten()
    }
}

impl Embedder for ProviderEmbedder {
    fn embed(&self, text: &str) -> Option<Vec<f32>> {
        if text.trim().is_empty() {
            return None;
        }
        self.embed_once(text)
    }

    fn dim(&self) -> usize {
        // 维度由 provider 决定,事先未知;返回 0 表示「不预设」。codemap 仅用 query 与块向量的
        // 实际长度做一致性校验,不依赖此值。
        0
    }
}

/// 从 OpenAI 兼容 embeddings 响应中取第一条向量:`{ "data": [ { "embedding": [..] } ] }`。
/// 任意结构不符返回 None。
fn parse_embedding(value: &serde_json::Value) -> Option<Vec<f32>> {
    let arr = value
        .get("data")?
        .as_array()?
        .first()?
        .get("embedding")?
        .as_array()?;
    if arr.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        out.push(v.as_f64()? as f32);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_by_default_when_setting_absent() {
        // 无设置项 → 关闭(默认离线),即便 provider 完整。
        assert!(resolve_embedding_config(None, None, None, Some("deepseek"), "sk-x").is_none());
        // 显式 off 同样关闭。
        assert!(
            resolve_embedding_config(Some("off"), None, None, Some("deepseek"), "sk-x").is_none()
        );
    }

    #[test]
    fn enabled_resolves_preset_base_and_default_model() {
        let cfg = resolve_embedding_config(Some("on"), None, None, Some("deepseek"), "sk-x")
            .expect("on + 可解析 provider 应得配置");
        assert_eq!(cfg.base_url, "https://api.deepseek.com");
        assert_eq!(cfg.model, DEFAULT_EMBEDDING_MODEL);
        assert_eq!(cfg.api_key, "sk-x");
    }

    #[test]
    fn enabled_honors_custom_base_and_model() {
        let cfg = resolve_embedding_config(
            Some("ON"), // 大小写不敏感
            Some("my-embed-model"),
            Some("https://proxy.local/v1/"),
            Some("custom"),
            "sk-y",
        )
        .expect("自定义覆盖应生效");
        assert_eq!(cfg.base_url, "https://proxy.local/v1"); // 去尾斜杠
        assert_eq!(cfg.model, "my-embed-model");
    }

    #[test]
    fn enabled_but_unresolvable_provider_is_off() {
        // 开关 on 但无 base_url 也无 preset → 解析不出端点 → 关闭(回退本地)。
        assert!(resolve_embedding_config(Some("on"), None, None, None, "sk-x").is_none());
        // 开关 on 但 key 为空 → 关闭。
        assert!(
            resolve_embedding_config(Some("on"), None, None, Some("deepseek"), "  ").is_none()
        );
    }

    #[test]
    fn embeddings_url_is_tolerant() {
        assert_eq!(embeddings_url("https://api.deepseek.com"), "https://api.deepseek.com/embeddings");
        assert_eq!(embeddings_url("https://api.deepseek.com/"), "https://api.deepseek.com/embeddings");
        // 已是完整端点:原样,不重复拼接。
        assert_eq!(
            embeddings_url("https://x.test/v1/embeddings"),
            "https://x.test/v1/embeddings"
        );
    }

    #[test]
    fn parse_embedding_extracts_first_vector() {
        let v = serde_json::json!({ "data": [ { "embedding": [0.1, 0.2, 0.3] } ] });
        assert_eq!(parse_embedding(&v), Some(vec![0.1f32, 0.2, 0.3]));
        // 结构不符 → None。
        assert!(parse_embedding(&serde_json::json!({ "data": [] })).is_none());
        assert!(parse_embedding(&serde_json::json!({})).is_none());
        assert!(parse_embedding(&serde_json::json!({ "data": [ { "embedding": [] } ] })).is_none());
    }
}
