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

/// 无损下限护栏阈值（0.0.68）：主模型未填 context_window 时,程序仍在此 token 数之上做**只无损**压缩,
/// 防长任务无护栏地撞端点硬上限报错。取值偏保守(err low):宁可对大窗口未填的用户略早做**可恢复**的
/// 工具输出凝练,也要保护小窗口未填的用户不直接溢出。注:condense/stub 只动「较旧的大工具结果」、且丢前
/// 先归档进 .mdga/archive 可重读,故即便略早触发,损失也可恢复。
pub const CONTEXT_LOSSLESS_FLOOR_TOKENS: u64 = 24_000;

/// 一轮压缩的触发设置（0.0.68）。把「阈值」与「是否允许有损摘要」「是否来自真实窗口」一并返回,
/// 使压缩循环既能在无窗口时用下限护栏做无损压缩,又**绝不**在无窗口时擅自有损摘要 / 臆断窗口大小
/// （守 0.0.61「纯手动零内置」红线:护栏是内部的、不当作窗口显示给用户)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionTrigger {
    /// 触发压缩的 token 阈值（始终有值:真实窗口 / env / 下限护栏)。
    pub limit: u64,
    /// 是否允许第③级**有损摘要**(summarize_wire_history,一次模型调用)。仅真实窗口 / env 下为 true;
    /// 无窗口的下限护栏为 false——只做①凝练②短桩这两级无损(且可从归档重读)的压缩。
    pub allow_summary: bool,
    /// 阈值是否来自用户真实窗口 / env(true) 还是下限护栏(false)。前端指示器只认真实窗口
    /// （见 [`context_soft_limit_for`] 返回 None ⇒ 隐藏),护栏不显示为窗口。
    pub from_window: bool,
}

/// 推导一轮压缩的触发设置（0.0.68）：env 覆盖 / 主模型真实窗口 ⇒ 完整三级压缩(允许有损摘要);
/// 主模型未填窗口 ⇒ 保守下限护栏 [`CONTEXT_LOSSLESS_FLOOR_TOKENS`],只做无损 condense/stub。
pub fn context_compaction_trigger(context_window: Option<i64>) -> CompactionTrigger {
    if let Ok(v) = std::env::var("MDGA_CONTEXT_SOFT_LIMIT") {
        if let Ok(parsed) = v.parse::<u64>() {
            return CompactionTrigger { limit: parsed, allow_summary: true, from_window: true };
        }
    }
    match context_window {
        Some(cw) if cw > 0 => CompactionTrigger {
            limit: cw as u64,
            allow_summary: true,
            from_window: true,
        },
        _ => CompactionTrigger {
            limit: CONTEXT_LOSSLESS_FLOOR_TOKENS,
            allow_summary: false,
            from_window: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::context_soft_limit_for;

    /// 串行化所有读/写 MDGA_CONTEXT_SOFT_LIMIT 的测试:进程级 env 是共享的,env-override 测试会
    /// set/remove 它,与读 env 的测试并行跑会相互污染(曾真实导致偶发失败)。各 env 相关测试先取此锁。
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn soft_limit_uses_context_window_directly_or_none() {
        let _g = env_lock();
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
    fn compaction_trigger_floor_vs_window() {
        use super::{context_compaction_trigger, CONTEXT_LOSSLESS_FLOOR_TOKENS};
        let _g = env_lock();
        if std::env::var("MDGA_CONTEXT_SOFT_LIMIT").is_ok() {
            return;
        }
        // 真实窗口:阈值=窗口,允许有损摘要,来自窗口(指示器显示)。
        let w = context_compaction_trigger(Some(128_000));
        assert_eq!(w.limit, 128_000);
        assert!(w.allow_summary);
        assert!(w.from_window);
        // 未填窗口:走下限护栏,**只无损**(不允许摘要),非窗口(指示器隐藏)。
        for cw in [None, Some(0), Some(-1)] {
            let f = context_compaction_trigger(cw);
            assert_eq!(f.limit, CONTEXT_LOSSLESS_FLOOR_TOKENS);
            assert!(!f.allow_summary, "无窗口护栏不得有损摘要");
            assert!(!f.from_window, "护栏不得当作窗口显示");
        }
    }

    #[test]
    fn soft_limit_env_override_returns_some() {
        let _g = env_lock();
        // 显式校验 env 覆盖路径：设置 env ⇒ 无论 context_window 如何都返回 Some(env 值)。
        // 用进程级 env，故只在测试函数内临时设置并复原，避免污染其他用例。
        std::env::set_var("MDGA_CONTEXT_SOFT_LIMIT", "42");
        assert_eq!(context_soft_limit_for(None), Some(42));
        assert_eq!(context_soft_limit_for(Some(1_000_000)), Some(42));
        std::env::remove_var("MDGA_CONTEXT_SOFT_LIMIT");
    }
}
