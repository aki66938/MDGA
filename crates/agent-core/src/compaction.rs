//! 上下文压缩阈值与软上限推导（纯逻辑部分）。
//!
//! 本轮（Plan28 P3-9）从桌面端迁入 agent-core：`CONTEXT_SOFT_LIMIT_TOKENS`（原 main.rs）
//! 与 `context_soft_limit_for`（原 compaction.rs）逻辑一字不改，仅提升为 `pub`。
//! 真正的摘要压缩 / 短桩压缩（依赖 wire 消息与模型调用）仍留桌面端。

/// 触发上下文压缩的软上限默认值（以上一次响应返回的 prompt_tokens 为准）。
/// DeepSeek V4 Flash / Pro 官方标称 1M 上下文，故取 800K：在接近上限前压缩，
/// 留约 200K headroom 给模型输出与当轮工具结果，避免顶满 1M 触发服务端退化。
/// 可用环境变量 MDGA_CONTEXT_SOFT_LIMIT 覆盖（便于低阈值压测验证压缩机制）。
pub const CONTEXT_SOFT_LIMIT_TOKENS: u64 = 800_000;

/// 按主供应商上下文窗口推导压缩软上限（Plan27 C2 #2）：
/// 优先级——环境变量 MDGA_CONTEXT_SOFT_LIMIT（压测）> 主 provider 的 context_window × 0.8（取整）>
/// 默认 [`CONTEXT_SOFT_LIMIT_TOKENS`]。这样非 DeepSeek 的小窗口模型也能在真实上限前触发压缩。
/// context_window 非正值（0 / 负数）视为未配置，回退默认。
pub fn context_soft_limit_for(context_window: Option<i64>) -> u64 {
    if let Ok(v) = std::env::var("MDGA_CONTEXT_SOFT_LIMIT") {
        if let Ok(parsed) = v.parse::<u64>() {
            return parsed;
        }
    }
    match context_window {
        Some(cw) if cw > 0 => (cw as u64) * 8 / 10,
        _ => CONTEXT_SOFT_LIMIT_TOKENS,
    }
}

#[cfg(test)]
mod tests {
    use super::{context_soft_limit_for, CONTEXT_SOFT_LIMIT_TOKENS};

    #[test]
    fn soft_limit_derives_from_context_window_or_falls_back() {
        // 仅在未设置压测 env 时校验推导逻辑，避免并行/CI 环境污染。
        if std::env::var("MDGA_CONTEXT_SOFT_LIMIT").is_ok() {
            return;
        }
        // 有 context_window：取 × 0.8（整除）。
        assert_eq!(context_soft_limit_for(Some(128_000)), 102_400);
        assert_eq!(context_soft_limit_for(Some(1_000_000)), 800_000);
        // None / 非正值：回退默认软上限。
        assert_eq!(context_soft_limit_for(None), CONTEXT_SOFT_LIMIT_TOKENS);
        assert_eq!(context_soft_limit_for(Some(0)), CONTEXT_SOFT_LIMIT_TOKENS);
        assert_eq!(context_soft_limit_for(Some(-5)), CONTEXT_SOFT_LIMIT_TOKENS);
    }
}
