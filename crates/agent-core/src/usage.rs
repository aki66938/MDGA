//! token usage 合并（纯 RawUsage 逐字段相加）。
//!
//! 本轮（Plan28 P3-9）从桌面端 main.rs 迁入 agent-core，逻辑一字不改，仅提升为 `pub`。

/// 把两份可选的 RawUsage 合并：任一为空取另一份，都有则逐字段相加并把两段 raw_json 包成数组。
pub fn merge_usage(
    first: Option<mdga_shared::RawUsage>,
    second: Option<mdga_shared::RawUsage>,
) -> Option<mdga_shared::RawUsage> {
    match (first, second) {
        (None, None) => None,
        (Some(usage), None) | (None, Some(usage)) => Some(usage),
        (Some(a), Some(b)) => Some(mdga_shared::RawUsage {
            prompt_tokens: a.prompt_tokens + b.prompt_tokens,
            completion_tokens: a.completion_tokens + b.completion_tokens,
            total_tokens: a.total_tokens + b.total_tokens,
            prompt_cache_hit_tokens: a.prompt_cache_hit_tokens + b.prompt_cache_hit_tokens,
            prompt_cache_miss_tokens: a.prompt_cache_miss_tokens + b.prompt_cache_miss_tokens,
            reasoning_tokens: a.reasoning_tokens + b.reasoning_tokens,
            raw_json: serde_json::json!([a.raw_json, b.raw_json]).to_string(),
        }),
    }
}
