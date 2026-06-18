//! 上下文压缩阈值与软上限推导（纯逻辑部分）。
//!
//! 0.0.61：context_window 改为**纯用户自定义**——不再有 app 强加的默认值，也不再 ×0.8。
//! 软上限直接取主模型用户填写的 context_window；主模型未填 ⇒ 返回 `None`，表示
//! **不做基于窗口的自动压缩**（程序不强加上限，由端点自身默认值兜底），前端指示器也随之隐藏。
//! 真正的摘要压缩 / 短桩压缩（依赖 wire 消息与模型调用）仍留桌面端。

/// 按主模型用户自定义的上下文窗口推导压缩软上限（0.0.61）：
/// 优先级——环境变量 MDGA_CONTEXT_SOFT_LIMIT（压测低阈值）> 主模型的 context_window（**直接**作为阈值，
/// 不再 ×0.8）> `None`（不做窗口驱动的压缩）。
/// 返回 `Some(limit)` 表示 app 管理的软上限；返回 `None` 表示主模型未填窗口、不应按窗口压缩
/// （也用于前端隐藏 context 指示器）。
/// context_window 非正值（0 / 负数）/ None 且无 env ⇒ 视为未配置，返回 `None`。
pub fn context_soft_limit_for(context_window: Option<i64>) -> Option<u64> {
    if let Ok(v) = std::env::var("MDGA_CONTEXT_SOFT_LIMIT") {
        if let Ok(parsed) = v.parse::<u64>() {
            return Some(parsed);
        }
    }
    match context_window {
        Some(cw) if cw > 0 => Some(cw as u64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::context_soft_limit_for;

    #[test]
    fn soft_limit_uses_context_window_directly_or_none() {
        // 仅在未设置压测 env 时校验推导逻辑，避免并行/CI 环境污染。
        if std::env::var("MDGA_CONTEXT_SOFT_LIMIT").is_ok() {
            return;
        }
        // 有 context_window：直接作为软上限（不再 ×0.8）。
        assert_eq!(context_soft_limit_for(Some(128_000)), Some(128_000));
        assert_eq!(context_soft_limit_for(Some(1_000_000)), Some(1_000_000));
        // None / 非正值：无 app 管理的软上限，返回 None（不做窗口驱动压缩）。
        assert_eq!(context_soft_limit_for(None), None);
        assert_eq!(context_soft_limit_for(Some(0)), None);
        assert_eq!(context_soft_limit_for(Some(-5)), None);
    }

    #[test]
    fn soft_limit_env_override_returns_some() {
        // 显式校验 env 覆盖路径：设置 env ⇒ 无论 context_window 如何都返回 Some(env 值)。
        // 用进程级 env，故只在测试函数内临时设置并复原，避免污染其他用例。
        std::env::set_var("MDGA_CONTEXT_SOFT_LIMIT", "42");
        assert_eq!(context_soft_limit_for(None), Some(42));
        assert_eq!(context_soft_limit_for(Some(1_000_000)), Some(42));
        std::env::remove_var("MDGA_CONTEXT_SOFT_LIMIT");
    }
}
