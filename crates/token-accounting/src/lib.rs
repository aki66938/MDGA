//! Token 计价与成本汇总。
//!
//! ⚠️ 价格说明（Plan28 P2-8）：下方价格为**手工维护的快照**（当前 2026-06 版，来源见各函数注释），
//! **无自动更新机制**——DeepSeek 官方调价后需在此**新增一个版本**（不覆盖历史，version 字段已写入每条
//! token 记录以便账单回放）。多供应商场景：本表仅覆盖 DeepSeek;非 DeepSeek 主供应商的成本前端按
//! 「—」展示（不按 DeepSeek 价误导,见 Plan21 #5），如需精确计费需后续引入 per-provider 价表。

use mdga_shared::RawUsage;
use serde::{Deserialize, Serialize};

pub mod presets;
pub use presets::{canonical_model_id, lookup_preset, PresetEntry};

pub mod thinking;
pub use thinking::{
    build_thinking_profile, ReasoningEcho, StopEmit, ThinkingDialect, ThinkingProfile,
    ThinkingStop,
};

/// 单次请求的价格快照，版本化保存以支持历史费用回放。
///
/// 价格单位为美元 / 百万 token；每次价格调整新增版本，不覆盖历史记录。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PricingSnapshot {
    /// 价格版本标识，写入每条 token 记录，用于账单对照。
    pub version: String,
    pub input_cache_hit_per_1m: f64,
    pub input_cache_miss_per_1m: f64,
    pub output_per_1m: f64,
}

/// 当前内置的 DeepSeek V4 Flash 价格快照（2026-06 版）。
///
/// 来源：https://api-docs.deepseek.com/quick_start/pricing
/// 价格单位：美元 / 百万 token。
pub fn deepseek_v4_flash_pricing() -> PricingSnapshot {
    PricingSnapshot {
        version: "deepseek-v4-flash-2026-06".to_string(),
        input_cache_hit_per_1m: 0.0028,
        input_cache_miss_per_1m: 0.14,
        output_per_1m: 0.28,
    }
}

/// 当前内置的 DeepSeek V4 Pro 价格快照（2026-06 版）。
///
/// 输入为空，输出 V4 Pro 的价格快照；本方法不联网更新价格，价格更新需新增版本。
pub fn deepseek_v4_pro_pricing() -> PricingSnapshot {
    PricingSnapshot {
        version: "deepseek-v4-pro-2026-06".to_string(),
        input_cache_hit_per_1m: 0.003625,
        input_cache_miss_per_1m: 0.435,
        output_per_1m: 0.87,
    }
}

/// 根据 DeepSeek 模型 ID 选择对应价格快照。
///
/// 输入模型 ID，输出当前内置价格快照；废弃兼容别名按官方说明映射到 V4 Flash。
pub fn deepseek_pricing_for_model(model: &str) -> PricingSnapshot {
    match model {
        "deepseek-v4-pro" => deepseek_v4_pro_pricing(),
        "deepseek-v4-flash" | "deepseek-chat" | "deepseek-reasoner" => {
            deepseek_v4_flash_pricing()
        }
        _ => deepseek_v4_flash_pricing(),
    }
}

/// 单次请求的标准化 token 用量，用于 UI 展示和账单对照。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub prompt_cache_hit_tokens: u64,
    pub prompt_cache_miss_tokens: u64,
    pub completion_tokens: u64,
}

/// 单次请求经过费用计算后的完整摘要，由前端直接展示。
///
/// 字段兼容性说明（0.0.72 P）：旧字段 `estimated_cost_usd` 保留不动，老的
/// `compute_cost_summary` 路径仍按 USD/百万 token 填它；新计价内核改填
/// `estimated_cost`/`currency`/`billing_mode` 三个新字段，并把 `estimated_cost_usd`
/// 置为 `estimated_cost.unwrap_or(0.0)`，使两条路径产出同形结构、workspace 不破坏编译。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CostSummary {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cache_hit_tokens: u64,
    pub cache_miss_tokens: u64,
    pub reasoning_tokens: u64,
    /// 估算费用，单位美元。**旧字段，保留向后兼容**；无价/订阅/免计费时为 0.0。
    pub estimated_cost_usd: f64,
    /// 估算费用（新）。无价 / 订阅 / 免计费 = None，前端显「—」。
    pub estimated_cost: Option<f64>,
    /// 币种，如 "CNY" | "USD"；无金额时为 None。
    pub currency: Option<String>,
    /// 计费方式：api | subscription | none。
    pub billing_mode: String,
    /// usage 来源：deepseek_usage | missing。
    pub usage_source: String,
    pub pricing_version: String,
}

/// 计费方式。
///
/// - `Api`：按量计费，按填入的单价结算（无单价则金额为 None）。
/// - `Subscription`：订阅套餐，单次请求不单独计费（金额为 None）。
/// - `None`：本地 / 免计费（金额为 None）。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BillingMode {
    Api,
    Subscription,
    None,
}

impl BillingMode {
    /// 返回该计费方式写入 `CostSummary.billing_mode` 的稳定字符串。
    fn as_str(self) -> &'static str {
        match self {
            BillingMode::Api => "api",
            BillingMode::Subscription => "subscription",
            BillingMode::None => "none",
        }
    }
}

/// 单档分级价格。`max_context` 为该档可承载的最大本轮 prompt tokens（升序排列，选档时取
/// 第一个 `max_context >= prompt_tokens` 的档；都不满足取最后一档）。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceTier {
    pub max_context: u64,
    pub input: f64,
    pub output: f64,
    pub cached_input: Option<f64>,
}

/// 模型计价定义，供前端 `pricing_json` 反序列化。
///
/// `input`/`output`/`cached_input`/`cache_write` 均以 `unit` 表达的单位计（per_1m 或 per_1k）。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPricing {
    /// 币种："CNY" | "USD"。
    pub currency: String,
    /// 单位："per_1m" | "per_1k"。
    pub unit: String,
    /// 缓存未命中价。
    pub input: f64,
    /// 输出价。
    pub output: f64,
    /// 缓存命中价；None 表示命中按 `input` 计。
    pub cached_input: Option<f64>,
    /// 单值写缓存价（Qwen / 部分智谱模型用）。**仅存储/展示，结算不应用**——见
    /// [`compute_cost_summary_priced`] 文档。
    pub cache_write: Option<f64>,
    /// 批量折扣乘数，如 0.5；None 视为 1.0。
    pub batch_discount: Option<f64>,
    /// 分级价格；非空时按本轮 prompt tokens 选档。
    pub tiers: Option<Vec<PriceTier>>,
}

/// 根据 DeepSeek token 用量和价格快照估算本次请求费用。
///
/// 输入标准化 usage 与价格版本，输出估算费用（美元）；
/// 本方法不读取官方账单，也不修改历史价格版本。
pub fn estimate_cost(usage: &TokenUsage, pricing: &PricingSnapshot) -> f64 {
    let hit = usage.prompt_cache_hit_tokens as f64 / 1_000_000.0 * pricing.input_cache_hit_per_1m;
    let miss =
        usage.prompt_cache_miss_tokens as f64 / 1_000_000.0 * pricing.input_cache_miss_per_1m;
    let out = usage.completion_tokens as f64 / 1_000_000.0 * pricing.output_per_1m;
    hit + miss + out
}

/// 将 DeepSeek 原始 usage 转换为带费用的摘要。
///
/// 输入服务端原始 usage 和当前价格快照，输出前端可直接展示的 CostSummary；
/// 本方法不写入数据库，调用方负责持久化。
pub fn compute_cost_summary(raw: &RawUsage, pricing: &PricingSnapshot) -> CostSummary {
    let usage = TokenUsage {
        prompt_cache_hit_tokens: raw.prompt_cache_hit_tokens,
        prompt_cache_miss_tokens: raw.prompt_cache_miss_tokens,
        completion_tokens: raw.completion_tokens,
    };
    let cost = estimate_cost(&usage, pricing);

    let usage_source = if raw.raw_json.is_empty() {
        "missing".to_string()
    } else {
        "deepseek_usage".to_string()
    };

    CostSummary {
        prompt_tokens: raw.prompt_tokens,
        completion_tokens: raw.completion_tokens,
        total_tokens: raw.total_tokens,
        cache_hit_tokens: raw.prompt_cache_hit_tokens,
        cache_miss_tokens: raw.prompt_cache_miss_tokens,
        reasoning_tokens: raw.reasoning_tokens,
        estimated_cost_usd: cost,
        // 旧路径也填新字段，使两条路径产出同形结构：DeepSeek 快照恒为 USD/按量计费。
        estimated_cost: Some(cost),
        currency: Some("USD".to_string()),
        billing_mode: BillingMode::Api.as_str().to_string(),
        usage_source,
        pricing_version: pricing.version.clone(),
    }
}

/// 单位归一化因子：把任意单位换算成 per_1m。per_1k → 1000.0，其余（per_1m）→ 1.0。
fn unit_factor(unit: &str) -> f64 {
    if unit == "per_1k" {
        1000.0
    } else {
        1.0
    }
}

/// 按本轮 prompt tokens 在分级价中选档。
///
/// 选第一个 `max_context >= prompt_tokens` 的档（要求升序）；都不满足取最后一档。
/// 返回 `(input, output, cached_input)`，覆盖顶层同名值。tiers 为空 / None 时返回顶层值。
fn select_rates(pricing: &ModelPricing, prompt_tokens: u64) -> (f64, f64, Option<f64>) {
    if let Some(tiers) = pricing.tiers.as_ref() {
        if !tiers.is_empty() {
            let sel = tiers
                .iter()
                .find(|t| t.max_context >= prompt_tokens)
                .unwrap_or_else(|| tiers.last().expect("non-empty checked above"));
            return (sel.input, sel.output, sel.cached_input);
        }
    }
    (pricing.input, pricing.output, pricing.cached_input)
}

/// 计价内核：按计费方式与单价定义结算单次请求费用。
///
/// 规则：
/// - `mode = None`（本地免计费）：金额 = None，billing_mode = "none"。
/// - `mode = Subscription`（订阅套餐）：金额 = None，billing_mode = "subscription"。
/// - `mode = Api`：
///   - `pricing = None`（未填单价）：金额 = None（前端显「—」），billing_mode = "api"。
///   - `pricing = Some`：按单位归一化（per_1k→×1000 换算成 per_1m）、按本轮 prompt tokens
///     选档、分缓存命中/未命中/输出三段计费，最后乘 `batch_discount`（None=1.0）。
///
/// **cache_write 取舍**：`RawUsage` 没有「写入缓存的 token 数」字段，故即使 `pricing.cache_write`
/// 有值，本函数**也不应用**它（没有写 token 计数无法乘，绝不凭空臆造）。该字段仅供存储 / 展示，
/// 待上游 usage 提供写缓存 token 计数后再接入结算。
pub fn compute_cost_summary_priced(
    raw: &RawUsage,
    mode: BillingMode,
    pricing: Option<&ModelPricing>,
) -> CostSummary {
    let usage_source = if raw.raw_json.is_empty() {
        "missing".to_string()
    } else {
        "deepseek_usage".to_string()
    };

    // 计算金额与币种：仅 Api + Some(pricing) 才出金额。
    let (estimated_cost, currency) = match (mode, pricing) {
        (BillingMode::Api, Some(p)) => {
            let factor = unit_factor(&p.unit);
            // 防御兜底:对只回 prompt_tokens、不分命中/未命中的端点(OpenAI 兼容,未经 parse_raw_usage
            // 兜底的路径),若 hit==0 && miss==0 但 prompt_tokens>0,则把全部 prompt_tokens 记为未命中,
            // 避免输入费被整段算成 0、且分级选档恒落最低档。成本公式与选档都用这对局部 hit/miss。
            let hit = raw.prompt_cache_hit_tokens;
            let mut miss = raw.prompt_cache_miss_tokens;
            if hit == 0 && miss == 0 && raw.prompt_tokens > 0 {
                miss = raw.prompt_tokens;
            }
            let prompt_tokens = hit + miss;
            let (sel_in, sel_out, sel_cached) = select_rates(p, prompt_tokens);

            let miss_rate = sel_in * factor / 1e6;
            let hit_rate = sel_cached.unwrap_or(sel_in) * factor / 1e6;
            let out_rate = sel_out * factor / 1e6;

            let mut cost = miss as f64 * miss_rate
                + hit as f64 * hit_rate
                + raw.completion_tokens as f64 * out_rate;
            cost *= p.batch_discount.unwrap_or(1.0);

            (Some(cost), Some(p.currency.clone()))
        }
        // None 模式、订阅模式、或 Api 但未填单价：均无金额。
        _ => (None, None),
    };

    CostSummary {
        prompt_tokens: raw.prompt_tokens,
        completion_tokens: raw.completion_tokens,
        total_tokens: raw.total_tokens,
        cache_hit_tokens: raw.prompt_cache_hit_tokens,
        cache_miss_tokens: raw.prompt_cache_miss_tokens,
        reasoning_tokens: raw.reasoning_tokens,
        estimated_cost_usd: estimated_cost.unwrap_or(0.0),
        estimated_cost,
        currency,
        billing_mode: mode.as_str().to_string(),
        usage_source,
        pricing_version: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个 RawUsage：给定命中 / 未命中 / 输出 tokens；prompt/total 由前两者推出。
    fn raw(hit: u64, miss: u64, out: u64) -> RawUsage {
        RawUsage {
            prompt_tokens: hit + miss,
            completion_tokens: out,
            total_tokens: hit + miss + out,
            prompt_cache_hit_tokens: hit,
            prompt_cache_miss_tokens: miss,
            reasoning_tokens: 0,
            raw_json: "{\"x\":1}".to_string(),
        }
    }

    /// glm-5.1 分级价：≤32k / 32k-200k / 超界。
    fn glm51() -> ModelPricing {
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
        }
    }

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected {b}, got {a}");
    }

    #[test]
    fn tier_selection_picks_correct_band() {
        let p = glm51();

        // 落在 ≤32k 档：prompt = 10_000（全 miss）。
        let r = raw(0, 10_000, 0);
        let s = compute_cost_summary_priced(&r, BillingMode::Api, Some(&p));
        // miss_rate = 6/1e6；cost = 10_000 * 6/1e6 = 0.06
        approx(s.estimated_cost.unwrap(), 10_000.0 * 6.0 / 1e6);

        // 落在 32k-200k 档：prompt = 100_000。
        let r = raw(0, 100_000, 0);
        let s = compute_cost_summary_priced(&r, BillingMode::Api, Some(&p));
        // 该档 input=8 → cost = 100_000 * 8/1e6 = 0.8
        approx(s.estimated_cost.unwrap(), 100_000.0 * 8.0 / 1e6);

        // 超界（>200k）：取最后一档（input=8）。
        let r = raw(0, 300_000, 0);
        let s = compute_cost_summary_priced(&r, BillingMode::Api, Some(&p));
        approx(s.estimated_cost.unwrap(), 300_000.0 * 8.0 / 1e6);
    }

    /// 构造一个「只回 prompt_tokens」的 RawUsage:hit=miss=0,但 prompt_tokens/total 自填。
    /// 模拟 OpenAI 兼容端点未经 parse_raw_usage 兜底的输入,验证结算层的防御兜底。
    fn raw_prompt_only(prompt: u64, out: u64) -> RawUsage {
        RawUsage {
            prompt_tokens: prompt,
            completion_tokens: out,
            total_tokens: prompt + out,
            prompt_cache_hit_tokens: 0,
            prompt_cache_miss_tokens: 0,
            reasoning_tokens: 0,
            raw_json: "{\"x\":1}".to_string(),
        }
    }

    #[test]
    fn prompt_only_usage_does_not_zero_input_cost() {
        // 普通(无 tiers)定价:即便 hit=miss=0,只要 prompt_tokens>0,输入费按全部 prompt_tokens 计。
        // deepseek-v4-pro CNY: in 3 / cached 0.025 / out 6。
        let p = ModelPricing {
            currency: "CNY".to_string(),
            unit: "per_1m".to_string(),
            input: 3.0,
            output: 6.0,
            cached_input: Some(0.025),
            cache_write: None,
            batch_discount: None,
            tiers: None,
        };
        let r = raw_prompt_only(5000, 1000);
        let s = compute_cost_summary_priced(&r, BillingMode::Api, Some(&p));
        // 全部 5000 记为未命中:5000*3/1e6 + 输出 1000*6/1e6,绝不为 0。
        let expected = 5000.0 * 3.0 / 1e6 + 1000.0 * 6.0 / 1e6;
        approx(s.estimated_cost.unwrap(), expected);
        assert!(s.estimated_cost.unwrap() > 0.0, "输入费不应被整段算成 0");
    }

    #[test]
    fn prompt_only_usage_selects_correct_tier() {
        // 分级定价(glm-5.1):prompt_tokens=5000 应落 ≤32k 档(input=6),而非因 hit+miss==0 恒落最低/错档。
        // 注:5000 同样落第一档,关键验证「选档用 prompt_tokens 而非 0」——用 100_000 验证跨档。
        let p = glm51();

        // 5000 → ≤32k 档,input=6。
        let r = raw_prompt_only(5000, 0);
        let s = compute_cost_summary_priced(&r, BillingMode::Api, Some(&p));
        approx(s.estimated_cost.unwrap(), 5000.0 * 6.0 / 1e6);

        // 100_000 → 32k-200k 档,input=8;若选档误用 0 则会落最低档(6)而算错。
        let r = raw_prompt_only(100_000, 0);
        let s = compute_cost_summary_priced(&r, BillingMode::Api, Some(&p));
        approx(s.estimated_cost.unwrap(), 100_000.0 * 8.0 / 1e6);
    }

    #[test]
    fn hit_miss_out_billed_separately() {
        // deepseek-v4-pro CNY: in 3 / cached 0.025 / out 6。
        let p = ModelPricing {
            currency: "CNY".to_string(),
            unit: "per_1m".to_string(),
            input: 3.0,
            output: 6.0,
            cached_input: Some(0.025),
            cache_write: None,
            batch_discount: None,
            tiers: None,
        };
        let r = raw(1000, 2000, 500);
        let s = compute_cost_summary_priced(&r, BillingMode::Api, Some(&p));
        let expected = 2000.0 * 3.0 / 1e6 + 1000.0 * 0.025 / 1e6 + 500.0 * 6.0 / 1e6;
        approx(s.estimated_cost.unwrap(), expected);
        assert_eq!(s.currency.as_deref(), Some("CNY"));
        assert_eq!(s.billing_mode, "api");
    }

    #[test]
    fn cached_none_falls_back_to_input_rate() {
        // cached_input = None → 命中按 input 计。
        let p = ModelPricing {
            currency: "USD".to_string(),
            unit: "per_1m".to_string(),
            input: 4.0,
            output: 18.0,
            cached_input: None,
            cache_write: None,
            batch_discount: None,
            tiers: None,
        };
        let r = raw(1000, 0, 0);
        let s = compute_cost_summary_priced(&r, BillingMode::Api, Some(&p));
        // 命中按 input(4) 计：1000 * 4/1e6
        approx(s.estimated_cost.unwrap(), 1000.0 * 4.0 / 1e6);
    }

    #[test]
    fn per_1k_normalizes_to_1000x_of_per_1m() {
        let base = ModelPricing {
            currency: "USD".to_string(),
            unit: "per_1m".to_string(),
            input: 3.0,
            output: 6.0,
            cached_input: Some(0.5),
            cache_write: None,
            batch_discount: None,
            tiers: None,
        };
        let mut k = base.clone();
        k.unit = "per_1k".to_string();

        let r = raw(1000, 2000, 500);
        let s_m = compute_cost_summary_priced(&r, BillingMode::Api, Some(&base));
        let s_k = compute_cost_summary_priced(&r, BillingMode::Api, Some(&k));
        approx(s_k.estimated_cost.unwrap(), s_m.estimated_cost.unwrap() * 1000.0);
    }

    #[test]
    fn batch_discount_halves_cost() {
        let p = ModelPricing {
            currency: "USD".to_string(),
            unit: "per_1m".to_string(),
            input: 3.0,
            output: 6.0,
            cached_input: Some(0.5),
            cache_write: None,
            batch_discount: Some(0.5),
            tiers: None,
        };
        let mut full = p.clone();
        full.batch_discount = None;

        let r = raw(1000, 2000, 500);
        let s_full = compute_cost_summary_priced(&r, BillingMode::Api, Some(&full));
        let s_half = compute_cost_summary_priced(&r, BillingMode::Api, Some(&p));
        approx(s_half.estimated_cost.unwrap(), s_full.estimated_cost.unwrap() * 0.5);
    }

    #[test]
    fn subscription_and_none_have_no_amount() {
        let p = ModelPricing {
            currency: "CNY".to_string(),
            unit: "per_1m".to_string(),
            input: 3.0,
            output: 6.0,
            cached_input: Some(0.025),
            cache_write: None,
            batch_discount: None,
            tiers: None,
        };
        let r = raw(1000, 2000, 500);

        let s_sub = compute_cost_summary_priced(&r, BillingMode::Subscription, Some(&p));
        assert!(s_sub.estimated_cost.is_none());
        assert!(s_sub.currency.is_none());
        assert_eq!(s_sub.billing_mode, "subscription");
        assert_eq!(s_sub.estimated_cost_usd, 0.0);

        let s_none = compute_cost_summary_priced(&r, BillingMode::None, Some(&p));
        assert!(s_none.estimated_cost.is_none());
        assert_eq!(s_none.billing_mode, "none");
    }

    #[test]
    fn api_without_pricing_has_no_amount() {
        let r = raw(1000, 2000, 500);
        let s = compute_cost_summary_priced(&r, BillingMode::Api, None);
        assert!(s.estimated_cost.is_none());
        assert!(s.currency.is_none());
        assert_eq!(s.billing_mode, "api");
        assert_eq!(s.estimated_cost_usd, 0.0);
    }

    #[test]
    fn billing_mode_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&BillingMode::Api).unwrap(),
            "\"api\""
        );
        assert_eq!(
            serde_json::to_string(&BillingMode::Subscription).unwrap(),
            "\"subscription\""
        );
        assert_eq!(
            serde_json::to_string(&BillingMode::None).unwrap(),
            "\"none\""
        );
    }
}
