//! 预设单价库（编译进程序）。
//!
//! 全部条目 `unit = "per_1m"`。价格为手工维护的快照，来源见各条 `source_url`；
//! `needs_verify = true` 的条目前端显「待官网核对」黄标。智谱（zhipu）条目因官网调价频繁，
//! 一律 needs_verify=true。

use crate::{ModelPricing, PriceTier};

/// 一条预设单价。`pricing.unit` 一律为 "per_1m"。
#[derive(Clone, Debug)]
pub struct PresetEntry {
    /// 连接预设标识："deepseek" | "zhipu" | "siliconflow"。
    pub connection_preset: &'static str,
    /// 匹配键（见 [`lookup_preset`] 匹配规则）。
    pub model_id: &'static str,
    /// 展示名。
    pub display_name: &'static str,
    /// 计价定义（unit 恒为 "per_1m"）。
    pub pricing: ModelPricing,
    /// 置信度："high" | "medium" | "low"。
    pub confidence: &'static str,
    /// true → 前端显「待官网核对」黄标。
    pub needs_verify: bool,
    /// 价格来源 URL。
    pub source_url: &'static str,
}

/// 构造一条 per_1m、无 tiers、无 cache_write、无 batch_discount 的 ModelPricing。
fn pm(currency: &str, input: f64, output: f64, cached_input: Option<f64>) -> ModelPricing {
    ModelPricing {
        currency: currency.to_string(),
        unit: "per_1m".to_string(),
        input,
        output,
        cached_input,
        cache_write: None,
        batch_discount: None,
        tiers: None,
    }
}

/// 计算 `needs_verify`：`confidence=="low" || connection_preset=="zhipu"` → true。
fn needs_verify(connection_preset: &str, confidence: &str) -> bool {
    confidence == "low" || connection_preset == "zhipu"
}

/// 规范化 model_id：trim，并去掉前导 `Pro/`（大小写不敏感，仅去一层）。
fn normalize_model_id(model_id: &str) -> String {
    let trimmed = model_id.trim();
    // char 安全:用 get(..4) 而非 trimmed[..4],避免多字节字符(CJK model_id)非字符边界处 panic。
    let without_pro = if trimmed.get(..4).map_or(false, |p| p.eq_ignore_ascii_case("pro/")) {
        &trimmed[4..]
    } else {
        trimmed
    };
    without_pro.trim().to_string()
}

/// 应用 DeepSeek 别名：connection_preset=="deepseek" 且 model_id ∈ {deepseek-chat,
/// deepseek-reasoner}（大小写不敏感）时，等同 "deepseek-v4-flash"。
fn apply_alias(connection_preset: &str, normalized: &str) -> String {
    if connection_preset == "deepseek"
        && (normalized.eq_ignore_ascii_case("deepseek-chat")
            || normalized.eq_ignore_ascii_case("deepseek-reasoner"))
    {
        "deepseek-v4-flash".to_string()
    } else {
        normalized.to_string()
    }
}

/// 在预设库中查找单价。
///
/// 匹配规则：`connection_preset` 小写相等；`model_id` 先规范化（去前导 `Pro/`、trim）后做
/// 大小写不敏感匹配；`currency` 大小写不敏感相等。DeepSeek 别名 deepseek-chat /
/// deepseek-reasoner 映射到 deepseek-v4-flash。
pub fn lookup_preset(
    connection_preset: &str,
    model_id: &str,
    currency: &str,
) -> Option<&'static PresetEntry> {
    let preset_lc = connection_preset.trim().to_ascii_lowercase();
    let normalized = normalize_model_id(model_id);
    let resolved = apply_alias(&preset_lc, &normalized);

    presets().iter().find(|e| {
        // 存表里的 model_id 同样规范化（去前导 Pro/、trim），让 "Pro/zai-org/GLM-5.1" 条目
        // 能被 "Pro/zai-org/GLM-5.1" 或 "zai-org/GLM-5.1" 查询命中（currency 负责消歧）。
        e.connection_preset == preset_lc
            && normalize_model_id(e.model_id).eq_ignore_ascii_case(&resolved)
            && e.pricing.currency.eq_ignore_ascii_case(currency)
    })
}

/// 全部预设条目（编译期惰性构造一次）。
fn presets() -> &'static [PresetEntry] {
    use std::sync::OnceLock;
    static PRESETS: OnceLock<Vec<PresetEntry>> = OnceLock::new();
    PRESETS.get_or_init(build_presets).as_slice()
}

/// 组装预设数据表。
fn build_presets() -> Vec<PresetEntry> {
    // 便捷构造器：连接预设小写、自动算 needs_verify。
    fn entry(
        connection_preset: &'static str,
        model_id: &'static str,
        display_name: &'static str,
        pricing: ModelPricing,
        confidence: &'static str,
        source_url: &'static str,
    ) -> PresetEntry {
        PresetEntry {
            connection_preset,
            model_id,
            display_name,
            needs_verify: needs_verify(connection_preset, confidence),
            pricing,
            confidence,
            source_url,
        }
    }

    const DS_SRC: &str = "https://api-docs.deepseek.com/zh-cn/quick_start/pricing";
    const ZHIPU_SRC: &str = "https://bigmodel.cn/pricing";
    const SF_CNY_SRC: &str = "https://siliconflow.cn/pricing";
    const SF_USD_SRC: &str = "https://www.siliconflow.com/pricing";

    vec![
        // ---- DeepSeek（preset=deepseek，均 high）----
        entry(
            "deepseek",
            "deepseek-v4-flash",
            "DeepSeek V4 Flash",
            pm("CNY", 1.0, 2.0, Some(0.02)),
            "high",
            DS_SRC,
        ),
        entry(
            "deepseek",
            "deepseek-v4-flash",
            "DeepSeek V4 Flash",
            pm("USD", 0.14, 0.28, Some(0.0028)),
            "high",
            DS_SRC,
        ),
        entry(
            "deepseek",
            "deepseek-v4-pro",
            "DeepSeek V4 Pro",
            pm("CNY", 3.0, 6.0, Some(0.025)),
            "high",
            DS_SRC,
        ),
        entry(
            "deepseek",
            "deepseek-v4-pro",
            "DeepSeek V4 Pro",
            pm("USD", 0.435, 0.87, Some(0.003625)),
            "high",
            DS_SRC,
        ),
        // ---- 智谱（preset=zhipu，needs_verify 全 true）----
        entry(
            "zhipu",
            "glm-5.1",
            "GLM-5.1",
            ModelPricing {
                currency: "CNY".to_string(),
                unit: "per_1m".to_string(),
                input: 6.0,
                output: 24.0,
                cached_input: Some(1.3),
                cache_write: None,
                batch_discount: None,
                tiers: Some(vec![
                    PriceTier {
                        max_context: 32_000,
                        input: 6.0,
                        output: 24.0,
                        cached_input: Some(1.3),
                    },
                    PriceTier {
                        max_context: 200_000,
                        input: 8.0,
                        output: 28.0,
                        cached_input: Some(2.0),
                    },
                ]),
            },
            "medium",
            ZHIPU_SRC,
        ),
        entry(
            "zhipu",
            "glm-5.2",
            "GLM-5.2",
            pm("CNY", 8.0, 28.0, Some(2.0)),
            "low",
            ZHIPU_SRC,
        ),
        entry(
            "zhipu",
            "glm-5.2",
            "GLM-5.2",
            pm("USD", 1.4, 4.4, Some(0.26)),
            "low",
            ZHIPU_SRC,
        ),
        entry(
            "zhipu",
            "glm-5",
            "GLM-5",
            pm("CNY", 4.0, 18.0, None),
            "low",
            ZHIPU_SRC,
        ),
        entry(
            "zhipu",
            "glm-5",
            "GLM-5",
            ModelPricing {
                currency: "USD".to_string(),
                unit: "per_1m".to_string(),
                input: 1.0,
                output: 3.2,
                cached_input: None,
                cache_write: Some(0.2),
                batch_discount: None,
                tiers: None,
            },
            "low",
            ZHIPU_SRC,
        ),
        entry(
            "zhipu",
            "glm-5-turbo",
            "GLM-5-Turbo",
            pm("CNY", 5.0, 22.0, None),
            "low",
            ZHIPU_SRC,
        ),
        // ---- 硅基流动（preset=siliconflow）----
        entry(
            "siliconflow",
            "deepseek-ai/DeepSeek-V4-Pro",
            "DeepSeek V4 Pro (SiliconFlow)",
            pm("CNY", 3.0, 6.0, Some(0.03)),
            "medium",
            SF_CNY_SRC,
        ),
        entry(
            "siliconflow",
            "deepseek-ai/DeepSeek-V4-Pro",
            "DeepSeek V4 Pro (SiliconFlow)",
            pm("USD", 1.6, 3.135, Some(0.135)),
            "low",
            SF_USD_SRC,
        ),
        entry(
            "siliconflow",
            "deepseek-ai/DeepSeek-V4-Flash",
            "DeepSeek V4 Flash (SiliconFlow)",
            pm("CNY", 1.0, 2.0, Some(0.02)),
            "medium",
            SF_CNY_SRC,
        ),
        entry(
            "siliconflow",
            "deepseek-ai/DeepSeek-V4-Flash",
            "DeepSeek V4 Flash (SiliconFlow)",
            pm("USD", 0.13, 0.28, Some(0.028)),
            "low",
            SF_USD_SRC,
        ),
        entry(
            "siliconflow",
            "deepseek-ai/DeepSeek-V3.2",
            "DeepSeek V3.2 (SiliconFlow)",
            pm("CNY", 2.0, 3.0, Some(0.2)),
            "high",
            SF_CNY_SRC,
        ),
        entry(
            "siliconflow",
            "deepseek-ai/DeepSeek-V3.2",
            "DeepSeek V3.2 (SiliconFlow)",
            pm("USD", 0.27, 0.42, Some(0.135)),
            "medium",
            SF_USD_SRC,
        ),
        entry(
            "siliconflow",
            "deepseek-ai/DeepSeek-V3.1-Terminus",
            "DeepSeek V3.1 Terminus (SiliconFlow)",
            pm("CNY", 4.0, 12.0, Some(0.4)),
            "high",
            SF_CNY_SRC,
        ),
        entry(
            "siliconflow",
            "deepseek-ai/DeepSeek-V3.1-Terminus",
            "DeepSeek V3.1 Terminus (SiliconFlow)",
            pm("USD", 0.27, 1.0, None),
            "low",
            SF_USD_SRC,
        ),
        entry(
            "siliconflow",
            "zai-org/GLM-5.2",
            "GLM-5.2 (SiliconFlow)",
            pm("CNY", 8.0, 28.0, Some(2.0)),
            "medium",
            SF_CNY_SRC,
        ),
        entry(
            "siliconflow",
            "zai-org/GLM-5.2",
            "GLM-5.2 (SiliconFlow)",
            pm("USD", 1.4, 4.4, Some(0.26)),
            "low",
            SF_USD_SRC,
        ),
        entry(
            "siliconflow",
            "Pro/zai-org/GLM-5.1",
            "GLM-5.1 (SiliconFlow Pro)",
            ModelPricing {
                currency: "CNY".to_string(),
                unit: "per_1m".to_string(),
                input: 8.0,
                output: 28.0,
                cached_input: Some(2.0),
                cache_write: None,
                batch_discount: None,
                tiers: Some(vec![
                    PriceTier {
                        max_context: 32_768,
                        input: 6.0,
                        output: 24.0,
                        cached_input: Some(1.3),
                    },
                    PriceTier {
                        max_context: 204_800,
                        input: 8.0,
                        output: 28.0,
                        cached_input: Some(2.0),
                    },
                ]),
            },
            "medium",
            SF_CNY_SRC,
        ),
        entry(
            "siliconflow",
            "zai-org/GLM-5.1",
            "GLM-5.1 (SiliconFlow)",
            pm("USD", 1.19, 4.3, Some(0.26)),
            "low",
            SF_USD_SRC,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_chat_alias_hits_flash() {
        let e = lookup_preset("deepseek", "deepseek-chat", "USD").expect("alias should hit flash");
        assert_eq!(e.model_id, "deepseek-v4-flash");
        assert_eq!(e.pricing.input, 0.14);
        assert_eq!(e.pricing.currency, "USD");

        let r =
            lookup_preset("deepseek", "deepseek-reasoner", "CNY").expect("alias should hit flash");
        assert_eq!(r.model_id, "deepseek-v4-flash");
        assert_eq!(r.pricing.input, 1.0);
    }

    #[test]
    fn pro_prefix_is_stripped_for_matching() {
        let e = lookup_preset("siliconflow", "Pro/zai-org/GLM-5.1", "CNY")
            .expect("Pro/ prefix should match zai-org/GLM-5.1");
        assert_eq!(e.model_id, "Pro/zai-org/GLM-5.1");
        assert!(e.pricing.tiers.is_some());
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let e = lookup_preset("DeepSeek", "DEEPSEEK-V4-PRO", "usd")
            .expect("case-insensitive match expected");
        assert_eq!(e.model_id, "deepseek-v4-pro");
        assert_eq!(e.pricing.currency, "USD");
    }

    #[test]
    fn zhipu_entries_need_verify() {
        let e = lookup_preset("zhipu", "glm-5.1", "CNY").expect("zhipu glm-5.1 CNY exists");
        assert!(e.needs_verify, "all zhipu entries must need verify");
        // medium confidence but zhipu forces needs_verify=true
        assert_eq!(e.confidence, "medium");
    }

    #[test]
    fn deepseek_high_entries_do_not_need_verify() {
        let e = lookup_preset("deepseek", "deepseek-v4-pro", "CNY").expect("exists");
        assert!(!e.needs_verify);
        assert_eq!(e.confidence, "high");
    }

    #[test]
    fn missing_entry_returns_none() {
        assert!(lookup_preset("deepseek", "no-such-model", "USD").is_none());
        assert!(lookup_preset("unknown-preset", "deepseek-v4-pro", "USD").is_none());
        // currency mismatch
        assert!(lookup_preset("zhipu", "glm-5-turbo", "USD").is_none());
    }

    #[test]
    fn normalize_model_id_handles_multibyte_chars() {
        // 多字节(CJK)model_id 不应在 trimmed[..4] 处 panic;短于 4 字节亦安全。
        assert_eq!(normalize_model_id("xx模型"), "xx模型");
        assert_eq!(normalize_model_id("智谱清言"), "智谱清言");
        assert_eq!(normalize_model_id("AB"), "AB");
        // 前导 Pro/ 仍正常去掉。
        assert_eq!(normalize_model_id("Pro/zai-org/GLM-5.1"), "zai-org/GLM-5.1");
    }

    #[test]
    fn lookup_preset_with_cjk_model_id_returns_none_not_panic() {
        // 结算路径无 catch_unwind;CJK model_id 必须返回 None 而非崩溃。
        assert!(lookup_preset("siliconflow", "智谱清言", "CNY").is_none());
    }
}
