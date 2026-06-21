//! 思考深度能力表（纯数据层，第 1 层）。
//!
//! 描述各供应商 / 模型「思考（reasoning）」契约的**类型化**能力表：用什么方言发送思考字段、
//! 多轮工具历史里 reasoning_content 是否回传、滑块有哪几档（更快 → 更聪明）及默认停哪。
//!
//! ⚠️ 设计约束：
//! - **纯 Rust 类型**，不依赖 `serde_json`、不联网、不发请求；给前端用的序列化由更上层做。
//! - 匹配 **绝不复用** `presets::canonical_model_id`——它会把 `deepseek-reasoner` / `deepseek-chat`
//!   别名成 `deepseek-v4-flash`，而它们的思考契约不同（前者强制单档、后者无思考），会被冲掉。
//!   本模块自带轻量匹配：小写化 + 必要时去前缀（`Pro/` / `deepseek-ai/` / `zai-org/` / `Qwen/`）后做子串判断。
//! - **查不到 => `None`**：上层据此隐藏入口，绝不瞎猜参数。

/// 思考字段方言：决定上层往请求体里写哪种形状的思考开关。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThinkingDialect {
    /// DeepSeek 官方 / 智谱 / Kimi：`{thinking:{type,[keep]}}`（可选 `reasoning_effort`）。
    ThinkingObject,
    /// 硅基 / 通义：`enable_thinking` + `thinking_budget`。
    EnableThinking,
    /// 硅基 DeepSeek-V4-Flash：仅 `reasoning_effort`。
    ReasoningEffortOnly,
}

/// 多轮工具历史里 reasoning_content 的回传策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReasoningEcho {
    /// 回传上一轮的 reasoning_content（多数模型要求带回以维持思考连续性）。
    Resend,
    /// 剔除上一轮的 reasoning_content（旧 deepseek-reasoner 要求不回传）。
    Omit,
}

/// 单档发往服务端的思考字段语义。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopEmit {
    /// 不发任何思考字段（跟随服务端默认）。
    FollowDefault,
    /// 关闭思考（方言相关：`thinking.type=disabled` 或 `enable_thinking=false`）。
    Off,
    /// 开启思考，并携带方言相关的强度 / 预算参数。
    On {
        /// `reasoning_effort` 取值（如 "high" / "max"）；None=不发该字段。
        reasoning_effort: Option<&'static str>,
        /// `thinking_budget` token 预算；None=不发该字段。
        thinking_budget: Option<u32>,
        /// 是否要求服务端保留全部思考（Kimi 的 `keep` 语义）。
        keep_all: bool,
    },
}

/// 滑块上的单个停靠点：展示标签 + 该档发往服务端的语义。
#[derive(Clone, Copy, Debug)]
pub struct ThinkingStop {
    /// 中文展示标签：`关闭` / `标准` / `深度` / `开启` / `思考(强制)`。
    pub label: &'static str,
    /// 该档发往服务端的语义。
    pub emit: StopEmit,
}

/// 某 (preset, model) 的完整思考能力档案。
#[derive(Clone, Debug)]
pub struct ThinkingProfile {
    /// 思考字段方言。
    pub dialect: ThinkingDialect,
    /// 有序停靠点：更快 → 更聪明。
    pub stops: Vec<ThinkingStop>,
    /// 滑块默认停靠的下标。
    pub default_index: usize,
    /// false=仅一档（如「思考(强制)」），前端渲染禁用滑块。
    pub adjustable: bool,
    /// 多轮工具历史里 reasoning_content 的回传策略。
    pub reasoning_echo: ReasoningEcho,
}

// ---- 轻量匹配辅助 ----

/// 规范化 model_id 用于子串匹配：小写化 + 去掉已知前缀（仅去一层；按出现顺序逐个剥）。
///
/// 去前缀仅为让带 org 前缀的硅基 id（如 `Pro/deepseek-ai/DeepSeek-V4-Flash`）与裸 id 一致命中；
/// **不**做别名归一（绝不把 deepseek-reasoner 改写成 flash）。子串判断本身对前缀不敏感，
/// 去前缀主要是为可读性与边界稳健（避免前缀里的字符干扰，如未来出现 `qwen/...-deepseek`）。
fn normalized_lower(model_id: &str) -> String {
    let mut s = model_id.trim().to_ascii_lowercase();
    // 已知前缀（均已小写）；逐个剥离，最多各剥一层，顺序无关因互不为对方前缀。
    for prefix in ["pro/", "deepseek-ai/", "zai-org/", "qwen/"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
        }
    }
    s
}

// ---- 各 preset 构造器 ----

/// deepseek 官方。
fn deepseek_profile(id: &str) -> Option<ThinkingProfile> {
    if id.contains("deepseek-v4") {
        // v4-pro / v4-flash：三档可调。
        Some(ThinkingProfile {
            dialect: ThinkingDialect::ThinkingObject,
            stops: vec![
                ThinkingStop { label: "关闭", emit: StopEmit::Off },
                ThinkingStop { label: "标准", emit: StopEmit::FollowDefault },
                ThinkingStop {
                    label: "深度",
                    emit: StopEmit::On {
                        reasoning_effort: Some("max"),
                        thinking_budget: None,
                        keep_all: false,
                    },
                },
            ],
            default_index: 1,
            adjustable: true,
            reasoning_echo: ReasoningEcho::Resend,
        })
    } else if id == "deepseek-reasoner" {
        // 旧 reasoner：强制单档，且历史 reasoning_content 须剔除。
        Some(ThinkingProfile {
            dialect: ThinkingDialect::ThinkingObject,
            stops: vec![ThinkingStop {
                label: "思考(强制)",
                emit: StopEmit::FollowDefault,
            }],
            default_index: 0,
            adjustable: false,
            reasoning_echo: ReasoningEcho::Omit,
        })
    } else {
        // deepseek-chat（旧·非思考）及其它 → None。
        None
    }
}

/// 智谱官方。注意匹配顺序：先具体（glm-5.2 / glm-5-turbo）后宽泛（glm-5）。
fn zhipu_profile(id: &str) -> Option<ThinkingProfile> {
    if id.contains("glm-5.2") {
        // GLM-5.2 默认即 max=深度。
        Some(ThinkingProfile {
            dialect: ThinkingDialect::ThinkingObject,
            stops: vec![
                ThinkingStop { label: "关闭", emit: StopEmit::Off },
                ThinkingStop {
                    label: "标准",
                    emit: StopEmit::On {
                        reasoning_effort: Some("high"),
                        thinking_budget: None,
                        keep_all: false,
                    },
                },
                ThinkingStop { label: "深度", emit: StopEmit::FollowDefault },
            ],
            default_index: 2,
            adjustable: true,
            reasoning_echo: ReasoningEcho::Resend,
        })
    } else if id.contains("glm-5-turbo") || id.contains("glm-4.6") {
        // glm-5-turbo / glm-4.6：开关两档。
        Some(zhipu_on_off())
    } else if id.contains("glm-5.1") || id.contains("glm-5") || id.contains("glm-4.7") {
        // glm-5.1 / glm-5（非 5.2/5-turbo，前面已分流）/ glm-4.7：开关两档。
        Some(zhipu_on_off())
    } else {
        None
    }
}

/// 智谱「关闭 / 开启」两档档案（多个分支共用）。
fn zhipu_on_off() -> ThinkingProfile {
    ThinkingProfile {
        dialect: ThinkingDialect::ThinkingObject,
        stops: vec![
            ThinkingStop { label: "关闭", emit: StopEmit::Off },
            ThinkingStop { label: "开启", emit: StopEmit::FollowDefault },
        ],
        default_index: 1,
        adjustable: true,
        reasoning_echo: ReasoningEcho::Resend,
    }
}

/// Moonshot / Kimi。匹配顺序：k2.7-code 先于 k2.6 先于 k2.5。
fn moonshot_profile(id: &str) -> Option<ThinkingProfile> {
    if id.contains("kimi-k2.7-code") {
        // k2.7-code（含 -highspeed）：强制单档，keep_all。
        Some(ThinkingProfile {
            dialect: ThinkingDialect::ThinkingObject,
            stops: vec![ThinkingStop {
                label: "思考(强制)",
                emit: StopEmit::On {
                    reasoning_effort: None,
                    thinking_budget: None,
                    keep_all: true,
                },
            }],
            default_index: 0,
            adjustable: false,
            reasoning_echo: ReasoningEcho::Resend,
        })
    } else if id.contains("kimi-k2.6") {
        // k2.6：关闭 / 开启，开启时 keep_all=true。
        Some(ThinkingProfile {
            dialect: ThinkingDialect::ThinkingObject,
            stops: vec![
                ThinkingStop { label: "关闭", emit: StopEmit::Off },
                ThinkingStop {
                    label: "开启",
                    emit: StopEmit::On {
                        reasoning_effort: None,
                        thinking_budget: None,
                        keep_all: true,
                    },
                },
            ],
            default_index: 1,
            adjustable: true,
            reasoning_echo: ReasoningEcho::Resend,
        })
    } else if id.contains("kimi-k2.5") {
        // k2.5：关闭 / 开启，开启时 keep_all=false。
        Some(ThinkingProfile {
            dialect: ThinkingDialect::ThinkingObject,
            stops: vec![
                ThinkingStop { label: "关闭", emit: StopEmit::Off },
                ThinkingStop {
                    label: "开启",
                    emit: StopEmit::On {
                        reasoning_effort: None,
                        thinking_budget: None,
                        keep_all: false,
                    },
                },
            ],
            default_index: 1,
            adjustable: true,
            reasoning_echo: ReasoningEcho::Resend,
        })
    } else {
        // 旧 moonshot-v1-* 等 → None。
        None
    }
}

/// 硅基流动。匹配顺序：先 v4-flash、再强制类（r1/qwq/glm-z1），最后混合类；
/// `glm-z1` 必须先于 `glm-5`/`glm-4` 判断避免误吞。
fn siliconflow_profile(id: &str) -> Option<ThinkingProfile> {
    if id.contains("deepseek-v4-flash") {
        // 仅 reasoning_effort，无关闭：标准(high) / 深度(max)。
        Some(ThinkingProfile {
            dialect: ThinkingDialect::ReasoningEffortOnly,
            stops: vec![
                ThinkingStop {
                    label: "标准",
                    emit: StopEmit::On {
                        reasoning_effort: Some("high"),
                        thinking_budget: None,
                        keep_all: false,
                    },
                },
                ThinkingStop {
                    label: "深度",
                    emit: StopEmit::On {
                        reasoning_effort: Some("max"),
                        thinking_budget: None,
                        keep_all: false,
                    },
                },
            ],
            default_index: 0,
            adjustable: true,
            reasoning_echo: ReasoningEcho::Resend,
        })
    } else if id.contains("deepseek-r1") || id.contains("qwq") || id.contains("glm-z1") {
        // 纯推理不可关：标准(4096) / 深度(32768)，无关闭。
        Some(ThinkingProfile {
            dialect: ThinkingDialect::EnableThinking,
            stops: vec![
                ThinkingStop {
                    label: "标准",
                    emit: StopEmit::On {
                        reasoning_effort: None,
                        thinking_budget: Some(4096),
                        keep_all: false,
                    },
                },
                ThinkingStop {
                    label: "深度",
                    emit: StopEmit::On {
                        reasoning_effort: None,
                        thinking_budget: Some(32768),
                        keep_all: false,
                    },
                },
            ],
            default_index: 0,
            adjustable: true,
            reasoning_echo: ReasoningEcho::Resend,
        })
    } else if id.contains("deepseek-v3.2")
        || id.contains("deepseek-v3.1-terminus")
        || id.contains("glm-5")
        || id.contains("glm-4.7")
        || id.contains("glm-4.6")
        || id.contains("qwen3")
    {
        // 混合思考：关闭 / 标准(4096) / 深度(32768)。
        // 注：qwen3 子串同时覆盖 qwen3.5（"qwen3.5".contains 实为 "qwen3" 子串命中）。
        Some(ThinkingProfile {
            dialect: ThinkingDialect::EnableThinking,
            stops: vec![
                ThinkingStop { label: "关闭", emit: StopEmit::Off },
                ThinkingStop {
                    label: "标准",
                    emit: StopEmit::On {
                        reasoning_effort: None,
                        thinking_budget: Some(4096),
                        keep_all: false,
                    },
                },
                ThinkingStop {
                    label: "深度",
                    emit: StopEmit::On {
                        reasoning_effort: None,
                        thinking_budget: Some(32768),
                        keep_all: false,
                    },
                },
            ],
            default_index: 1,
            adjustable: true,
            reasoning_echo: ReasoningEcho::Resend,
        })
    } else {
        None
    }
}

/// 构建某 (connection_preset, model_id) 的思考能力档案。
///
/// preset 大小写不敏感；model_id 小写化 + 去已知前缀后做子串匹配。
/// 查不到（含未知 / 未支持 preset，如 qwen / custom）一律返回 `None`——上层据此隐藏入口，绝不瞎猜参数。
pub fn build_thinking_profile(connection_preset: &str, model_id: &str) -> Option<ThinkingProfile> {
    let preset = connection_preset.trim().to_ascii_lowercase();
    let id = normalized_lower(model_id);

    match preset.as_str() {
        "deepseek" => deepseek_profile(&id),
        "zhipu" => zhipu_profile(&id),
        "moonshot" => moonshot_profile(&id),
        "siliconflow" => siliconflow_profile(&id),
        // qwen / custom / 未知 → None（首版不做；未知绝不瞎猜）。
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取出某档的 reasoning_effort（仅 On 档有意义）。
    fn effort(stop: &ThinkingStop) -> Option<&'static str> {
        match stop.emit {
            StopEmit::On { reasoning_effort, .. } => reasoning_effort,
            _ => None,
        }
    }

    fn keep_all(stop: &ThinkingStop) -> Option<bool> {
        match stop.emit {
            StopEmit::On { keep_all, .. } => Some(keep_all),
            _ => None,
        }
    }

    fn budget(stop: &ThinkingStop) -> Option<u32> {
        match stop.emit {
            StopEmit::On { thinking_budget, .. } => thinking_budget,
            _ => None,
        }
    }

    #[test]
    fn deepseek_v4_pro_three_stops_with_effort_and_resend() {
        let p = build_thinking_profile("deepseek", "deepseek-v4-pro").expect("v4-pro has profile");
        assert_eq!(p.dialect, ThinkingDialect::ThinkingObject);
        assert_eq!(p.stops.len(), 3);
        assert!(p.adjustable);
        assert_eq!(p.default_index, 1);
        assert_eq!(p.reasoning_echo, ReasoningEcho::Resend);
        // 档位：关闭 / 标准 / 深度。
        assert_eq!(p.stops[0].label, "关闭");
        assert_eq!(p.stops[0].emit, StopEmit::Off);
        assert_eq!(p.stops[1].label, "标准");
        assert_eq!(p.stops[1].emit, StopEmit::FollowDefault);
        assert_eq!(p.stops[2].label, "深度");
        assert_eq!(effort(&p.stops[2]), Some("max"));
    }

    #[test]
    fn deepseek_v4_flash_also_three_stops() {
        let p = build_thinking_profile("deepseek", "deepseek-v4-flash").expect("v4-flash profile");
        assert_eq!(p.stops.len(), 3);
        assert_eq!(p.dialect, ThinkingDialect::ThinkingObject);
    }

    #[test]
    fn deepseek_reasoner_single_stop_not_adjustable_omit() {
        let p =
            build_thinking_profile("deepseek", "deepseek-reasoner").expect("reasoner has profile");
        assert_eq!(p.stops.len(), 1);
        assert!(!p.adjustable);
        assert_eq!(p.default_index, 0);
        assert_eq!(p.stops[0].label, "思考(强制)");
        assert_eq!(p.reasoning_echo, ReasoningEcho::Omit);
    }

    #[test]
    fn deepseek_chat_is_none() {
        assert!(build_thinking_profile("deepseek", "deepseek-chat").is_none());
    }

    #[test]
    fn deepseek_unknown_is_none() {
        assert!(build_thinking_profile("deepseek", "deepseek-foo").is_none());
    }

    #[test]
    fn glm_52_default_index_is_2() {
        let p = build_thinking_profile("zhipu", "glm-5.2").expect("glm-5.2 has profile");
        assert_eq!(p.stops.len(), 3);
        assert_eq!(p.default_index, 2);
        assert_eq!(p.stops[2].label, "深度");
        assert_eq!(p.stops[2].emit, StopEmit::FollowDefault);
        // 标准档带 reasoning_effort=high。
        assert_eq!(effort(&p.stops[1]), Some("high"));
    }

    #[test]
    fn glm_51_two_stops() {
        let p = build_thinking_profile("zhipu", "glm-5.1").expect("glm-5.1 has profile");
        assert_eq!(p.stops.len(), 2);
        assert_eq!(p.default_index, 1);
        assert_eq!(p.stops[0].label, "关闭");
        assert_eq!(p.stops[1].label, "开启");
    }

    #[test]
    fn glm_5_not_swallowed_by_glm_52_rule() {
        // glm-5（裸）必须命中两档规则，而非 glm-5.2 的三档规则。
        let p = build_thinking_profile("zhipu", "glm-5").expect("glm-5 has profile");
        assert_eq!(p.stops.len(), 2, "glm-5 应命中两档规则，不被 glm-5.2 误吞");
        assert_eq!(p.default_index, 1);
    }

    #[test]
    fn glm_52_not_swallowed_by_glm_5_rule() {
        // glm-5.2 必须命中三档规则（先判更具体的 glm-5.2）。
        let p = build_thinking_profile("zhipu", "glm-5.2").expect("glm-5.2 has profile");
        assert_eq!(p.stops.len(), 3, "glm-5.2 必须命中三档规则，不被 glm-5 误吞");
    }

    #[test]
    fn glm_5_turbo_not_swallowed_by_glm_5_rule() {
        // glm-5-turbo 走两档分支（先判更具体），不应被 glm-5 提前误命中导致混淆。
        let p = build_thinking_profile("zhipu", "glm-5-turbo").expect("glm-5-turbo has profile");
        assert_eq!(p.stops.len(), 2);
        assert_eq!(p.stops[1].label, "开启");
    }

    #[test]
    fn kimi_k27_code_forced_single_keep_all() {
        let p =
            build_thinking_profile("moonshot", "kimi-k2.7-code").expect("k2.7-code has profile");
        assert_eq!(p.stops.len(), 1);
        assert!(!p.adjustable);
        assert_eq!(p.default_index, 0);
        assert_eq!(p.stops[0].label, "思考(强制)");
        assert_eq!(keep_all(&p.stops[0]), Some(true));
        assert_eq!(p.reasoning_echo, ReasoningEcho::Resend);
    }

    #[test]
    fn kimi_k27_code_highspeed_variant_hits() {
        let p = build_thinking_profile("moonshot", "kimi-k2.7-code-highspeed")
            .expect("k2.7-code-highspeed has profile");
        assert_eq!(p.stops.len(), 1);
        assert!(!p.adjustable);
    }

    #[test]
    fn kimi_k26_two_stops_keep_all_true() {
        let p = build_thinking_profile("moonshot", "kimi-k2.6").expect("k2.6 has profile");
        assert_eq!(p.stops.len(), 2);
        assert!(p.adjustable);
        assert_eq!(p.default_index, 1);
        assert_eq!(p.stops[0].label, "关闭");
        assert_eq!(p.stops[1].label, "开启");
        assert_eq!(keep_all(&p.stops[1]), Some(true));
    }

    #[test]
    fn kimi_k25_two_stops_keep_all_false() {
        let p = build_thinking_profile("moonshot", "kimi-k2.5").expect("k2.5 has profile");
        assert_eq!(p.stops.len(), 2);
        assert_eq!(keep_all(&p.stops[1]), Some(false));
    }

    #[test]
    fn moonshot_legacy_is_none() {
        assert!(build_thinking_profile("moonshot", "moonshot-v1-8k").is_none());
    }

    #[test]
    fn siliconflow_v4_flash_reasoning_effort_only_no_off() {
        let p = build_thinking_profile("siliconflow", "deepseek-ai/DeepSeek-V4-Flash")
            .expect("sf v4-flash has profile");
        assert_eq!(p.dialect, ThinkingDialect::ReasoningEffortOnly);
        assert_eq!(p.stops.len(), 2);
        // 无关闭档。
        assert!(p.stops.iter().all(|s| s.emit != StopEmit::Off));
        assert_eq!(effort(&p.stops[0]), Some("high"));
        assert_eq!(effort(&p.stops[1]), Some("max"));
        assert_eq!(p.default_index, 0);
    }

    #[test]
    fn siliconflow_glm5_three_stops_enable_thinking() {
        let p = build_thinking_profile("siliconflow", "Pro/zai-org/GLM-5")
            .expect("sf GLM-5 has profile");
        assert_eq!(p.dialect, ThinkingDialect::EnableThinking);
        assert_eq!(p.stops.len(), 3);
        assert_eq!(p.stops[0].label, "关闭");
        assert_eq!(p.stops[0].emit, StopEmit::Off);
        assert_eq!(budget(&p.stops[1]), Some(4096));
        assert_eq!(budget(&p.stops[2]), Some(32768));
        assert_eq!(p.default_index, 1);
    }

    #[test]
    fn siliconflow_r1_forced_no_off() {
        let p = build_thinking_profile("siliconflow", "deepseek-ai/DeepSeek-R1")
            .expect("sf R1 has profile");
        assert_eq!(p.dialect, ThinkingDialect::EnableThinking);
        assert_eq!(p.stops.len(), 2);
        assert!(p.stops.iter().all(|s| s.emit != StopEmit::Off));
        assert_eq!(budget(&p.stops[0]), Some(4096));
        assert_eq!(budget(&p.stops[1]), Some(32768));
    }

    #[test]
    fn siliconflow_glm_z1_not_swallowed_by_glm5() {
        // glm-z1 必须命中强制(无关闭)分支，而非 glm-5 的混合(有关闭)分支。
        let p = build_thinking_profile("siliconflow", "zai-org/GLM-Z1-9B")
            .expect("sf GLM-Z1 has profile");
        assert_eq!(p.stops.len(), 2);
        assert!(
            p.stops.iter().all(|s| s.emit != StopEmit::Off),
            "glm-z1 为强制推理，不应有关闭档"
        );
    }

    #[test]
    fn siliconflow_qwen3_and_qwen35_hit_mixed() {
        let p3 =
            build_thinking_profile("siliconflow", "Qwen/Qwen3-32B").expect("qwen3 has profile");
        assert_eq!(p3.stops.len(), 3);
        let p35 = build_thinking_profile("siliconflow", "Qwen/Qwen3.5-72B")
            .expect("qwen3.5 has profile");
        assert_eq!(p35.stops.len(), 3);
    }

    #[test]
    fn siliconflow_unknown_is_none() {
        assert!(build_thinking_profile("siliconflow", "some-org/Unknown-Model").is_none());
    }

    #[test]
    fn qwen_preset_always_none() {
        assert!(build_thinking_profile("qwen", "qwen3-max").is_none());
        assert!(build_thinking_profile("qwen", "anything").is_none());
    }

    #[test]
    fn custom_preset_is_none() {
        assert!(build_thinking_profile("custom", "deepseek-v4-pro").is_none());
        assert!(build_thinking_profile("", "deepseek-v4-pro").is_none());
        assert!(build_thinking_profile("UNKNOWN", "glm-5.2").is_none());
    }

    #[test]
    fn case_and_prefix_variants_match() {
        // 大小写变体。
        assert!(build_thinking_profile("deepseek", "DeepSeek-V4-Pro").is_some());
        assert!(build_thinking_profile("DeepSeek", "DEEPSEEK-V4-FLASH").is_some());
        // 前缀变体：Pro/deepseek-ai/...
        let p = build_thinking_profile("siliconflow", "Pro/deepseek-ai/DeepSeek-V4-Flash")
            .expect("prefixed v4-flash must match");
        assert_eq!(p.dialect, ThinkingDialect::ReasoningEffortOnly);
        assert!(p.stops.iter().all(|s| s.emit != StopEmit::Off));
    }
}
