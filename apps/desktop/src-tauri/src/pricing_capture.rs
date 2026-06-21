//! 官网单价采集管线（0.0.73 第三层）。
//!
//! 流程：点「官网单价」→ 抓官网定价页（无鉴权 GET）→ 裁表区/清洗 → 用用户已配「主模型」
//! 一次性非流式 chat 抽成结构化单价 → 校验护栏 → 与「现价」diff（**不写库**）→ 用户勾选后
//! `apply_pricing_overrides` 写覆盖层。
//!
//! 设计：把可错的纯逻辑（清洗 / 解析 / 校验 / diff）拆成纯函数（带单测），I/O 壳（HTTP 抓取、
//! LLM 调用、DB 写）只做编排。
//!
//! 安全：api_key 只在抽取 I/O 内部用作 Bearer 头（经 `mdga_deepseek_client::chat_completion`），
//! **绝不进 [`CaptureResult`] / 日志 / 任何返回前端的结构**。
//!
//! 口径（0.0.73 修复 1）：override 的存储/查询 **key** 一律用 [`canonical_model_id`] 规范化
//! （去前导 `Pro/`、deepseek-chat/reasoner→flash 别名、小写），与编译 `lookup_preset` 口径一致——
//! 写键 == 读键，消除「登记串与采集串大小写 / `Pro/` / 别名不一致时 override 写了却静默查不到」。
//! 展示给用户的 `PricingDiff.modelId` 仍用**抽取原串**（真实 API 模型串，可读）。currency 恒 "CNY"。

use crate::state::AppState;
use mdga_deepseek_client::chat_completion;
use mdga_storage::{
    clear_pricing_overrides, get_connection, get_pricing_override, now_ts, resolve_role_provider,
    upsert_pricing_override, ROLE_MAIN,
};
use mdga_token_accounting::{canonical_model_id, lookup_preset, ModelPricing, PriceTier};
use serde::{Deserialize, Serialize};
use tauri::State;

// ── 源清单（决策 D：只 CNY，只两家）────────────────────────────────────────────

/// 一个可采集源：官网定价页 url、币种、页面语言（仅供 prompt 提示，currency 由源注入）。
struct PricingSource {
    url: &'static str,
    currency: &'static str,
    lang: &'static str,
    /// 校验用锚点模型 id（真实 API 串，大小写不敏感比对）：抽取结果里必须出现，否则整批弃。
    anchor: &'static str,
}

/// 按连接 preset 查可采集源；不支持的 preset（zhipu 等）返回 None。
fn source_for_preset(preset: &str) -> Option<PricingSource> {
    match preset.trim().to_ascii_lowercase().as_str() {
        "deepseek" => Some(PricingSource {
            url: "https://api-docs.deepseek.com/zh-cn/quick_start/pricing",
            currency: "CNY",
            lang: "zh",
            anchor: "deepseek-v4-pro",
        }),
        "siliconflow" => Some(PricingSource {
            url: "https://siliconflow.cn/pricing",
            currency: "CNY",
            lang: "zh",
            anchor: "deepseek-ai/DeepSeek-V4-Pro",
        }),
        _ => None,
    }
}

// ── 返回结构（全 serde camelCase；不含任何凭据）──────────────────────────────────

/// 单条 diff：一个抽到的模型与「现价」的比对结果。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PricingDiff {
    /// 真实 API 模型串（原样，作为 override 主键的一部分）。
    pub model_id: String,
    pub currency: String,
    /// "new" | "changed" | "unchanged"。
    pub change: String,
    /// 现价（override 或编译快照解析得到）；new 时为 None。
    pub old_pricing: Option<ModelPricing>,
    /// 官网抽到的新价。
    pub new_pricing: ModelPricing,
}

/// 采集命令的统一返回（不写库；前端据此渲染 diff 勾选表）。
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CaptureResult {
    /// 该 preset 是否支持自动采集。
    pub supported: bool,
    /// 抓取+抽取+校验整体是否成功（supported=false 时无意义，恒 false）。
    pub ok: bool,
    /// 失败原因（人话）；成功时为 None。
    pub error: Option<String>,
    /// supported=false 时的提示文案。
    pub message: Option<String>,
    /// 采集源 url（成功时回填，供 apply 写 source_url）。
    pub source_url: Option<String>,
    /// 抓取/抽取时间戳（成功时回填）。
    pub fetched_at: Option<i64>,
    /// 清洗后正文是否被截断（超 EXTRACTION_TEXT_LIMIT）。true → 部分模型可能漏采，
    /// 前端据此显警示（结果不可视为完整）。失败/不支持时恒 false。
    pub truncated: bool,
    /// 逐模型 diff（changed/new/unchanged 都在；unchanged 由前端默认不勾）。
    pub diffs: Vec<PricingDiff>,
}

impl CaptureResult {
    fn unsupported(message: &str) -> Self {
        CaptureResult {
            supported: false,
            ok: false,
            error: None,
            message: Some(message.to_string()),
            source_url: None,
            fetched_at: None,
            truncated: false,
            diffs: Vec::new(),
        }
    }

    fn failed(error: String) -> Self {
        CaptureResult {
            supported: true,
            ok: false,
            error: Some(error),
            message: None,
            source_url: None,
            fetched_at: None,
            truncated: false,
            diffs: Vec::new(),
        }
    }
}

/// apply 命令的单条入参：前端把选中项的 newPricing 序列化回传。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApplyItem {
    /// 真实 API 模型串（原样写入 override 主键，遵守 BE-B 约束）。
    pub model_id: String,
    pub currency: String,
    /// newPricing 序列化后的 JSON 串（原样存进 override 的 pricing_json）。
    pub pricing_json: String,
    /// 采集源 url（可空；前端从 CaptureResult.sourceUrl 带回）。
    pub source_url: Option<String>,
}

// ── 命令 1：采集（I/O 壳）─────────────────────────────────────────────────────

/// 抓官网定价页 → 清洗 → 用主模型 LLM 抽成结构化单价 → 校验 → 与现价 diff。**不写库**。
///
/// 流程见模块文档。`connectionId` 用于取该连接的 preset（决定源 / 锚点 / 是否支持）；LLM 抽取
/// 用 `resolve_role_provider(main)` 的 base_url/api_key/model_id（一次性非流式）。
/// api_key 仅在抽取 I/O 内部用，绝不进返回值。
#[allow(non_snake_case)]
#[tauri::command]
pub(crate) async fn capture_official_pricing(
    state: State<'_, AppState>,
    connectionId: String,
) -> Result<CaptureResult, String> {
    // 1) 取连接 preset + 主模型 provider（锁作用域尽量短：读完即松锁，HTTP/LLM 在锁外 await）。
    let (preset, main_base, main_key, main_model) = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let conn = get_connection(&db, &connectionId)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "连接不存在".to_string())?;
        let preset = conn.preset.clone().unwrap_or_default();
        // 主模型 provider：抽取要用它的端点/密钥/模型。缺主模型则无法抽取。
        let main = resolve_role_provider(&db, ROLE_MAIN)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "未配置主模型，无法用 LLM 抽取官网单价".to_string())?;
        let base = mdga_deepseek_client::resolve_base_url(
            main.base_url.as_deref(),
            main.preset.as_deref(),
        )
        .ok_or_else(|| "主模型端点未配置：请填写 Base URL 或选择内置预设".to_string())?;
        (preset, base, main.api_key, main.model_id)
    };

    // 1') preset 不在源清单 → 明确不支持。
    let Some(source) = source_for_preset(&preset) else {
        return Ok(CaptureResult::unsupported(
            "该平台暂不支持自动采集，可手填或恢复预设",
        ));
    };

    // 2) 抓（I/O）：无鉴权 GET，普通 UA。
    let html = match fetch_pricing_html(source.url).await {
        Ok(h) => h,
        Err(e) => return Ok(CaptureResult::failed(format!("抓取失败：{e}"))),
    };

    // 3) 裁+清洗（纯函数）。
    let cleaned = clean_pricing_html(&html);
    // 大页截断检测：清洗后正文超上限 → prompt 只取前段，尾部模型可能漏采（仍报成功但置 truncated）。
    let truncated = was_truncated(&cleaned);

    // 4) LLM 抽（I/O）：一次性非流式 chat 完成（复用 deepseek-client，OpenAI 兼容）。
    let prompt = build_extraction_prompt(&cleaned, source.currency, source.lang);
    let messages = vec![serde_json::json!({ "role": "user", "content": prompt })];
    let llm_text = match chat_completion(&main_base, &main_key, messages, &main_model, None, None).await {
        Ok(r) => r.content.unwrap_or_default(),
        Err(e) => return Ok(CaptureResult::failed(format!("LLM 抽取失败：{e}"))),
    };

    // 5) 解析 + 校验（纯函数）。
    let models = match parse_extraction(&llm_text, source.currency) {
        Ok(m) => m,
        Err(e) => return Ok(CaptureResult::failed(format!("解析抽取结果失败：{e}"))),
    };
    if let Err(e) = validate_extraction(&models, source.anchor) {
        return Ok(CaptureResult::failed(format!(
            "校验未通过（以官网为准、保留现价）：{e}"
        )));
    }

    // 6) diff（纯函数；现价 = override 优先、编译兜底）。锁作用域：逐模型读 override。
    let diffs = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        models
            .into_iter()
            .map(|m| {
                let current = current_pricing_for(&db, &preset, &m.id_for_diff());
                diff_pricing(&preset, &m, current.as_ref())
            })
            .collect::<Vec<_>>()
    };

    Ok(CaptureResult {
        supported: true,
        ok: true,
        error: None,
        message: None,
        source_url: Some(source.url.to_string()),
        fetched_at: Some(now_ts()),
        truncated,
        diffs,
    })
}

/// 一个抽到的模型：携带真实 API 串 model_id 与解析好的 ModelPricing。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExtractedModel {
    pub model_id: String,
    pub pricing: ModelPricing,
}

impl ExtractedModel {
    fn id_for_diff(&self) -> String {
        self.model_id.clone()
    }
}

/// 读「现价」：override（解析成功）优先，否则编译快照。两者皆无 → None。供 diff 用。
fn current_pricing_for(
    db: &rusqlite::Connection,
    preset: &str,
    model_id: &str,
) -> Option<ModelPricing> {
    // override 用 canonical key 查（与写入侧一致），消除大小写 / Pro/ / 别名口径偏差。
    let key = canonical_model_id(preset, model_id);
    if let Ok(Some(o)) = get_pricing_override(db, preset, &key, "CNY") {
        if let Ok(p) = serde_json::from_str::<ModelPricing>(&o.pricing_json) {
            return Some(p);
        }
    }
    // 编译快照（lookup_preset 内部会规范化别名/Pro 前缀，作为兜底现价之一）。
    lookup_preset(preset, model_id, "CNY").map(|e| e.pricing.clone())
}

// ── 命令 2：应用（I/O 壳）─────────────────────────────────────────────────────

/// 把用户勾选的采集价逐条写入 override 覆盖层。返回写入条数。
///
/// `connectionPreset` 为该连接的 preset；`items` 每条 model_id 经 [`canonical_model_id`] 规范化后
/// 作为 override 存储 key（与读取侧一致：去 `Pro/`、deepseek 别名、小写），消除登记/采集口径偏差。
/// source_url 取 item 自带（前端从 CaptureResult.sourceUrl 带回），缺省按 preset 推。
#[allow(non_snake_case)]
#[tauri::command]
pub(crate) fn apply_pricing_overrides(
    state: State<AppState>,
    connectionPreset: String,
    items: Vec<ApplyItem>,
) -> Result<u32, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let fallback_url = source_for_preset(&connectionPreset).map(|s| s.url.to_string());
    let ts = now_ts();
    let mut written = 0u32;
    for item in &items {
        let src = item.source_url.clone().or_else(|| fallback_url.clone());
        // canonical key：与读取侧（current_pricing_for / lookup_effective_pricing /
        // effective_fallback_pricing）一致，保证写键==读键、与编译口径对齐。
        let key = canonical_model_id(&connectionPreset, &item.model_id);
        upsert_pricing_override(
            &db,
            &connectionPreset,
            &key,
            &item.currency,
            &item.pricing_json,
            src.as_deref(),
            ts,
        )
        .map_err(|e| e.to_string())?;
        written += 1;
    }
    Ok(written)
}

// ── 命令 3：连接级重置（恢复预设）────────────────────────────────────────────────

/// 删某连接 preset 下**全部**采集覆盖价，让有效价整体跌回编译快照。返回删除条数。
///
/// 对应 diff 面板「恢复预设」按钮（连接级）。不触碰模型自填的 pricing_json（那是另一层），
/// 也不触碰任何凭据；只清 `preset_pricing_overrides` 该 preset 的行。
#[allow(non_snake_case)]
#[tauri::command]
pub(crate) fn reset_pricing_overrides(
    state: State<AppState>,
    connectionPreset: String,
) -> Result<u32, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let removed = clear_pricing_overrides(&db, &connectionPreset).map_err(|e| e.to_string())?;
    Ok(removed as u32)
}

// ── I/O：抓取 ────────────────────────────────────────────────────────────────

/// 无鉴权 GET 定价页，返回 HTML 文本。普通 UA、30s 超时；非 2xx / 网络错 → Err（人话）。
async fn fetch_pricing_html(url: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/120.0 Safari/537.36",
        )
        .header("Accept", "text/html,application/xhtml+xml")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    resp.text().await.map_err(|e| e.to_string())
}

// ── 纯函数：清洗 ────────────────────────────────────────────────────────────

/// 清洗定价页 HTML：去 `<script>`/`<style>`/`<svg>`/HTML 注释，剥标签属性，折叠空白，缩减体积。
///
/// 目的是把含价目的文本/表格区交给 LLM，去掉无关脚本/样式/矢量图、并剥掉标签属性（Tailwind
/// class 等占满字符预算的主因），大幅提升价目文本密度（硅基 ~769KB 必须缩）。
/// 不做语义解析，只做安全的成对标签剔除 + 注释剔除 + 属性剥离 + 空白折叠；
/// **保留所有可见文本与标签名**（含 table/tr/td/th 结构），价目文本不丢。
pub(crate) fn clean_pricing_html(html: &str) -> String {
    let mut s = html.to_string();
    // 1) 去成对的脚本类/矢量标签（含内容）。大小写不敏感。
    for tag in ["script", "style", "svg", "noscript", "head"] {
        s = strip_tag_blocks(&s, tag);
    }
    // 2) 去 HTML 注释 <!-- ... -->（含可能内嵌的 JSON/脚本）。
    s = strip_html_comments(&s);
    // 3) 剥标签属性：`<td class="...大量 tailwind...">` → `<td>`，只留标签名，可见文本不动。
    s = strip_tag_attributes(&s);
    // 4) 折叠空白：把连续空白（含换行/制表）压成单个空格，去首尾空白。
    collapse_whitespace(&s)
}

/// 剥离 HTML 标签的属性，只保留标签名（与可选的前导 `/`）：`<td class="a b c">` → `<td>`、
/// `</div  >` → `</div>`、`<img ... />` → `<img>`。
///
/// 仅处理形如 `<` + 字母/`/` 起始的标签；非标签的 `<`（如 `<3`、文本里的 `<` 比较号）原样保留。
/// 不解析引号内的 `>`（定价页标签属性极少在引号内含裸 `>`，且本步只为压缩、不追求 HTML 完备）；
/// 标签内**可见文本不在标签里**，故剥属性绝不丢价目文本。
fn strip_tag_attributes(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'<' {
            // 看 `<` 后首个非空白字符：字母 → 开标签；`/` → 闭标签；否则非标签，原样保留 `<`。
            let after = bytes.get(i + 1).copied();
            let is_open = matches!(after, Some(b) if b.is_ascii_alphabetic());
            let is_close = after == Some(b'/');
            if is_open || is_close {
                // 找该标签的结束 `>`。找不到（截断尾）→ 原样吐出剩余并结束。
                if let Some(rel) = input[i..].find('>') {
                    let tag_end = i + rel; // `>` 的位置
                    let inner = &input[i + 1..tag_end]; // `<` 与 `>` 之间
                    // 提取「[/]标签名」：跳过可能的前导 `/`，取连续的标签名字符（字母/数字/-/:）。
                    let inner_trim = inner.trim_start();
                    let (slash, rest) = if let Some(r) = inner_trim.strip_prefix('/') {
                        ("/", r.trim_start())
                    } else {
                        ("", inner_trim)
                    };
                    let name: String = rest
                        .chars()
                        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == ':')
                        .collect();
                    if name.is_empty() {
                        // 取不出标签名（如 `< 3`）→ 原样保留这个 `<`，继续逐字符。
                        out.push('<');
                        i += 1;
                        continue;
                    }
                    out.push('<');
                    out.push_str(slash);
                    out.push_str(&name);
                    out.push('>');
                    i = tag_end + 1;
                    continue;
                } else {
                    // 无闭合 `>`：原样保留剩余。
                    out.push_str(&input[i..]);
                    break;
                }
            }
        }
        // 非标签字符（含非标签的 `<`）：原样保留。需按 char 推进以保证 UTF-8 边界。
        let ch = input[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 去掉所有 `<tag ...>...</tag>` 成对块（含内容），大小写不敏感；未闭合的尾块截到结尾。
fn strip_tag_blocks(input: &str, tag: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let open_prefix = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0usize;
    while let Some(rel) = lower[cursor..].find(&open_prefix) {
        let open_at = cursor + rel;
        // 确认是标签起始：`<tag` 后须是空白 / `>` / `/`（避免误伤 `<scripting>` 这类）。
        let after = lower[open_at + open_prefix.len()..].chars().next();
        let is_tag = matches!(after, Some(c) if c.is_whitespace() || c == '>' || c == '/')
            || after.is_none();
        if !is_tag {
            out.push_str(&input[cursor..open_at + open_prefix.len()]);
            cursor = open_at + open_prefix.len();
            continue;
        }
        // 保留 open_at 之前的内容。
        out.push_str(&input[cursor..open_at]);
        // 找闭合标签；找不到则丢弃到结尾。
        match lower[open_at..].find(&close) {
            Some(crel) => {
                cursor = open_at + crel + close.len();
            }
            None => {
                cursor = input.len();
            }
        }
    }
    out.push_str(&input[cursor..]);
    out
}

/// 去掉所有 `<!-- ... -->` 注释；未闭合的丢弃到结尾。
fn strip_html_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0usize;
    while let Some(rel) = input[cursor..].find("<!--") {
        let at = cursor + rel;
        out.push_str(&input[cursor..at]);
        match input[at..].find("-->") {
            Some(crel) => cursor = at + crel + 3,
            None => {
                cursor = input.len();
            }
        }
    }
    out.push_str(&input[cursor..]);
    out
}

/// 折叠空白：连续 ASCII 空白（空格/制表/换行/回车）压成单个空格，去首尾。
fn collapse_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_space = false;
    for ch in input.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

// ── 纯函数：prompt 构造 ──────────────────────────────────────────────────────

/// 抽取页文本上限（字符）：避免超长正文撑爆主模型上下文 / 拖慢一次性抽取。硅基大页裁到此长度。
const EXTRACTION_TEXT_LIMIT: usize = 24_000;

/// 清洗后正文是否超过抽取上限（即 prompt 会被截断、尾部模型可能漏采）。
/// 用 char 计数与 `build_extraction_prompt` 的 `.chars().take(LIMIT)` 同口径。
pub(crate) fn was_truncated(cleaned_text: &str) -> bool {
    cleaned_text.chars().count() > EXTRACTION_TEXT_LIMIT
}

/// 构造一次性抽取 prompt。currency 由源注入（不让 LLM 判），要求只输出 JSON 数组。
pub(crate) fn build_extraction_prompt(cleaned_text: &str, currency: &str, lang: &str) -> String {
    // 体积保护：硅基整页很大，截到上限（清洗后价目区通常落在前段；超长尾巴多为页脚/导航）。
    let body: String = cleaned_text.chars().take(EXTRACTION_TEXT_LIMIT).collect();
    format!(
        "你是定价信息抽取器。下面是一个大模型平台官方定价页面的纯文本（语言:{lang}）。\n\
         请从中抽出**每一个模型**的价格，输出**一个 JSON 数组**，数组里每个元素形如：\n\
         {{\n\
         \"modelId\": \"真实 API 调用串（不是展示名，如 deepseek-v4-pro 或 deepseek-ai/DeepSeek-V4-Pro）\",\n\
         \"input\": 缓存未命中的输入单价(数字),\n\
         \"cachedInput\": 缓存命中价(数字, 没有则 null),\n\
         \"output\": 输出单价(数字),\n\
         \"tiers\": [可选, 仅当该模型按上下文长度分级计价时给, 每档 {{\"maxContext\":数字,\"input\":数字,\"output\":数字,\"cachedInput\":数字或null}}]\n\
         }}\n\
         要求：\n\
         - modelId 必须是真实的 API 调用串，保持原样大小写与斜杠，不要用中文展示名。\n\
         - 单价单位按页面所示（通常为每百万 token）；不要换算、原样取数字。\n\
         - 不要判断币种（币种已知为 {currency}，无需输出 currency 字段）。\n\
         - 不分级的模型省略 tiers 字段。\n\
         - **只输出 JSON 数组本身，不要任何解释文字、不要 Markdown 代码围栏。**\n\
         \n\
         定价页文本：\n{body}"
    )
}

// ── 纯函数：解析 ────────────────────────────────────────────────────────────

/// LLM 抽取结果的单条原始形态（容忍 cachedInput/tiers 缺省）。
#[derive(Debug, Deserialize)]
struct RawExtracted {
    #[serde(rename = "modelId", alias = "model_id")]
    model_id: String,
    input: f64,
    output: f64,
    #[serde(default, rename = "cachedInput", alias = "cached_input")]
    cached_input: Option<f64>,
    #[serde(default)]
    tiers: Option<Vec<RawTier>>,
}

#[derive(Debug, Deserialize)]
struct RawTier {
    #[serde(rename = "maxContext", alias = "max_context")]
    max_context: u64,
    input: f64,
    output: f64,
    #[serde(default, rename = "cachedInput", alias = "cached_input")]
    cached_input: Option<f64>,
}

/// 鲁棒解析 LLM 抽取文本为 `Vec<ExtractedModel>`。
///
/// 容忍：① 裸 JSON 数组；② ```json 代码围栏包裹（先剥围栏再扫）；③ 前后/中间说明文字——包括
/// **前言里出现的占位方括号**（如「见后[]」「[占位]」）：从首个 `[` 起逐个平衡 `[...]` 块尝试
/// 反序列化为 `Vec<RawExtracted>`，**某块失败就继续扫下一个 `[`**，直到成功或文本耗尽。
/// currency 统一注入参数值、unit 注入 "per_1m"（页面口径为每百万 token；本版只采每百万）。
/// 全部候选块都解析不出目标数组 → Err。
pub(crate) fn parse_extraction(llm_text: &str, currency: &str) -> Result<Vec<ExtractedModel>, String> {
    // 先剥 ```json 围栏（若有），提高裸扫命中率；无围栏则原样。
    let stripped = strip_code_fences(llm_text);
    let raw = parse_first_valid_array(&stripped)
        .or_else(|| parse_first_valid_array(llm_text))
        .ok_or("未找到可解析为模型数组的 JSON")?;
    let models = raw
        .into_iter()
        .filter(|r| !r.model_id.trim().is_empty())
        .map(|r| {
            let tiers = r.tiers.map(|ts| {
                let mut tiers = ts
                    .into_iter()
                    .map(|t| PriceTier {
                        max_context: t.max_context,
                        input: t.input,
                        output: t.output,
                        cached_input: t.cached_input,
                    })
                    .collect::<Vec<_>>();
                // select_rates（token-accounting）假定 tiers 按 max_context 升序；LLM 可能乱序给出。
                tiers.sort_by_key(|t| t.max_context);
                tiers
            });
            ExtractedModel {
                model_id: r.model_id.trim().to_string(),
                pricing: ModelPricing {
                    currency: currency.to_string(),
                    unit: "per_1m".to_string(),
                    input: r.input,
                    output: r.output,
                    cached_input: r.cached_input,
                    cache_write: None,
                    batch_discount: None,
                    tiers,
                },
            }
        })
        .collect();
    Ok(models)
}

/// 逐个尝试 `text` 中每个平衡 `[...]` 块，返回第一个能反序列化为 `Vec<RawExtracted>` 的结果。
///
/// 从首个 `[` 起取平衡块；该块 `from_str` 失败（如前言里的占位 `[...]`、非目标数组）就**继续扫
/// 下一个 `[`** 重试，直到成功或文本耗尽。容忍前言含干扰方括号。空数组 `[]` 视为有效（→ 空 Vec）。
fn parse_first_valid_array(text: &str) -> Option<Vec<RawExtracted>> {
    let mut search_from = 0usize;
    while let Some(rel) = text[search_from..].find('[') {
        let start = search_from + rel;
        match balanced_array_block(text, start) {
            Some(end) => {
                let slice = &text[start..=end];
                if let Ok(v) = serde_json::from_str::<Vec<RawExtracted>>(slice) {
                    return Some(v);
                }
                // 该块不是目标数组：从这个 `[` 之后继续找下一个 `[`。
                search_from = start + 1;
            }
            // 从此 `[` 起没有平衡块（截断/未闭合）→ 后面更不可能，停止。
            None => return None,
        }
    }
    None
}

/// 从 `start`（须指向 `[`）起按方括号配对找平衡块，跳过字符串内的括号；返回闭合 `]` 的字节下标。
/// 不平衡（未闭合）→ None。
fn balanced_array_block(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.get(start) != Some(&b'[') {
        return None;
    }
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
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
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// 剥 ```json … ``` / ``` … ``` 代码围栏，返回围栏内内容；无围栏则原样返回。
/// 只取第一对围栏内的内容（LLM 输出通常只有一对）。
fn strip_code_fences(text: &str) -> String {
    let Some(open_rel) = text.find("```") else {
        return text.to_string();
    };
    // 跳过 ``` 与其后可选的语言标记行（如 `json`），定位到内容起点。
    let after_open = open_rel + 3;
    let rest = &text[after_open..];
    // 内容从围栏后第一个换行之后开始（语言标记同行）；无换行则从 ``` 后直接开始。
    let content_start = match rest.find('\n') {
        Some(nl) => after_open + nl + 1,
        None => after_open,
    };
    // 找闭合围栏。
    match text[content_start..].find("```") {
        Some(close_rel) => text[content_start..content_start + close_rel].to_string(),
        None => text[content_start..].to_string(),
    }
}

// ── 纯函数：校验 ────────────────────────────────────────────────────────────

/// 单价合理区间：> 0 且 < 10000（每百万 token 价；离谱即弃整批）。
fn price_in_range(v: f64) -> bool {
    v > 0.0 && v < 10_000.0
}

/// 校验抽取结果（不过则整批不产出 diff）：
/// ① 数量 > 0；
/// ② 锚点模型必须在内（大小写不敏感比对 model_id）；
/// ③ 每个价（input/output/cachedInput/各 tier）`> 0 且 < 10000`；
/// ④ tiers 非空时每档 `max_context > 0`（max_context==0 会让 select_rates 选档失真，整批弃）。
pub(crate) fn validate_extraction(models: &[ExtractedModel], anchor: &str) -> Result<(), String> {
    if models.is_empty() {
        return Err("未抽到任何模型".to_string());
    }
    let has_anchor = models
        .iter()
        .any(|m| m.model_id.eq_ignore_ascii_case(anchor));
    if !has_anchor {
        return Err(format!("锚点模型 {anchor} 未出现，疑似抽取错位"));
    }
    for m in models {
        let p = &m.pricing;
        if !price_in_range(p.input) || !price_in_range(p.output) {
            return Err(format!("{} 的输入/输出价超出合理区间", m.model_id));
        }
        if let Some(ci) = p.cached_input {
            if !price_in_range(ci) {
                return Err(format!("{} 的缓存命中价超出合理区间", m.model_id));
            }
        }
        if let Some(tiers) = &p.tiers {
            for t in tiers {
                if t.max_context == 0 {
                    return Err(format!("{} 的分级档 max_context 为 0（非法）", m.model_id));
                }
                if !price_in_range(t.input) || !price_in_range(t.output) {
                    return Err(format!("{} 的分级价超出合理区间", m.model_id));
                }
                if let Some(ci) = t.cached_input {
                    if !price_in_range(ci) {
                        return Err(format!("{} 的分级缓存命中价超出合理区间", m.model_id));
                    }
                }
            }
        }
    }
    Ok(())
}

// ── 纯函数：diff ────────────────────────────────────────────────────────────

/// 比对一个抽到的模型与「现价」，产出一条 [`PricingDiff`]。
///
/// 现价 `current` 由调用方按 override 优先、编译兜底取好（[`current_pricing_for`]）。
/// 分类：现价不存在 → "new"；任一比较字段不同 → "changed"；完全相同 → "unchanged"。
/// 比较字段：input/output/cachedInput/tiers（含 None vs Some 的差异）。
pub(crate) fn diff_pricing(
    _preset: &str,
    extracted: &ExtractedModel,
    current: Option<&ModelPricing>,
) -> PricingDiff {
    let change = match current {
        None => "new",
        Some(cur) => {
            if pricing_equivalent(cur, &extracted.pricing) {
                "unchanged"
            } else {
                "changed"
            }
        }
    };
    PricingDiff {
        model_id: extracted.model_id.clone(),
        currency: extracted.pricing.currency.clone(),
        change: change.to_string(),
        old_pricing: current.cloned(),
        new_pricing: extracted.pricing.clone(),
    }
}

/// 价格等价判定：只比对「钱」的字段（input/output/cachedInput/tiers），忽略 currency/unit
/// （diff 始终同币种 CNY、同单位 per_1m）与 cache_write/batch_discount（采集不产出，恒 None）。
fn pricing_equivalent(a: &ModelPricing, b: &ModelPricing) -> bool {
    if !f_eq(a.input, b.input) || !f_eq(a.output, b.output) {
        return false;
    }
    if !opt_f_eq(a.cached_input, b.cached_input) {
        return false;
    }
    match (&a.tiers, &b.tiers) {
        (None, None) => true,
        (Some(x), Some(y)) => {
            x.len() == y.len()
                && x.iter().zip(y.iter()).all(|(p, q)| {
                    p.max_context == q.max_context
                        && f_eq(p.input, q.input)
                        && f_eq(p.output, q.output)
                        && opt_f_eq(p.cached_input, q.cached_input)
                })
        }
        _ => false,
    }
}

/// 浮点近似相等（容忍 1e-9 噪声）。
fn f_eq(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// Option<f64> 近似相等（None==None；Some/None 不等）。
fn opt_f_eq(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => f_eq(x, y),
        _ => false,
    }
}

// ── 单测（纯函数）────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mdga_storage::init_db;

    /// 在系统临时目录建一个唯一 DB 文件，返回 (Connection, 路径)；调用方负责清理。
    fn temp_db() -> (rusqlite::Connection, std::path::PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir()
            .join(format!("mdga-pricing-cap-{}-{nanos}.db", std::process::id()));
        let conn = init_db(&path).expect("db init");
        (conn, path)
    }

    // —— canonical override 往返（修复 1）——

    #[test]
    fn override_round_trips_across_case_pro_alias_forms() {
        let (db, path) = temp_db();

        // 写：用 canonical key（apply_pricing_overrides 的写口径），登记串带 Pro/ + 大小写。
        let write_key = canonical_model_id("siliconflow", "Pro/zai-org/GLM-5.1");
        upsert_pricing_override(
            &db,
            "siliconflow",
            &write_key,
            "CNY",
            r#"{"currency":"CNY","unit":"per_1m","input":7.0,"output":25.0,"cachedInput":1.5}"#,
            None,
            1,
        )
        .expect("upsert");

        // 读：用形式不同的采集串（去 Pro/、大小写不同），current_pricing_for 内部按 canonical 查。
        // 三种形态都应命中同一条 override（input=7.0），而非退回编译快照。
        for probe in ["zai-org/GLM-5.1", "Pro/ZAI-ORG/glm-5.1", "  zai-org/glm-5.1  "] {
            let cur = current_pricing_for(&db, "siliconflow", probe)
                .unwrap_or_else(|| panic!("override should hit for probe {probe:?}"));
            assert_eq!(cur.input, 7.0, "probe {probe:?} must hit captured override");
            assert_eq!(cur.output, 25.0);
        }

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn override_round_trips_deepseek_alias() {
        let (db, path) = temp_db();

        // 用别名 deepseek-chat 登记 → canonical 收敛为 deepseek-v4-flash。
        let write_key = canonical_model_id("deepseek", "deepseek-chat");
        assert_eq!(write_key, "deepseek-v4-flash");
        upsert_pricing_override(
            &db,
            "deepseek",
            &write_key,
            "CNY",
            r#"{"currency":"CNY","unit":"per_1m","input":1.1,"output":2.2,"cachedInput":0.03}"#,
            None,
            1,
        )
        .expect("upsert");

        // 用真实串 deepseek-v4-flash 与另一别名 deepseek-reasoner 读，都应命中采集价（input=1.1）。
        for probe in ["deepseek-v4-flash", "DEEPSEEK-REASONER", "deepseek-chat"] {
            let cur = current_pricing_for(&db, "deepseek", probe)
                .unwrap_or_else(|| panic!("override should hit for probe {probe:?}"));
            assert_eq!(cur.input, 1.1, "probe {probe:?} must hit captured override");
        }

        let _ = std::fs::remove_file(path);
    }

    // —— clean_pricing_html ——

    #[test]
    fn clean_strips_script_style_and_collapses_whitespace() {
        let html = "<html><head><title>x</title></head><body>\n\
            <script>var leak='secret';\nlots of js();</script>\n\
            <style>.a{color:red}</style>\n\
            <table>\n  <tr><th>deepseek-v4-pro</th></tr>\n  <tr><td>¥3</td></tr>\n</table>\n\
            <svg><path d='M0 0'/></svg>\n\
            </body></html>";
        let cleaned = clean_pricing_html(html);
        // 脚本/样式被剔除。
        assert!(!cleaned.contains("var leak"));
        assert!(!cleaned.contains("secret"));
        assert!(!cleaned.contains("color:red"));
        assert!(!cleaned.contains("M0 0"));
        // 价目文本保留。
        assert!(cleaned.contains("deepseek-v4-pro"));
        assert!(cleaned.contains("¥3"));
        // 体积下降。
        assert!(cleaned.len() < html.len());
        // 空白折叠：无连续多空格。
        assert!(!cleaned.contains("  "));
    }

    #[test]
    fn clean_strips_comments() {
        let html = "<body><!-- hidden <script>x</script> note -->visible ¥6</body>";
        let cleaned = clean_pricing_html(html);
        assert!(!cleaned.contains("hidden"));
        assert!(cleaned.contains("visible ¥6"));
    }

    #[test]
    fn clean_does_not_eat_lookalike_tag() {
        // <scripting> 不应被 <script> 规则误伤（前缀须后跟空白/>//）。
        let html = "<body><scripting>keep me</scripting> ¥1</body>";
        let cleaned = clean_pricing_html(html);
        assert!(cleaned.contains("keep me"));
    }

    #[test]
    fn clean_strips_tag_attributes_keeps_prices_and_shrinks() {
        // 硅基风格：海量 Tailwind class 占满字符预算；剥属性后价目文本保留、体积大降。
        let bloat = "class=\"flex flex-row items-center justify-between gap-4 rounded-lg \
            border border-gray-200 px-6 py-4 text-sm font-medium text-gray-700 hover:bg-gray-50\"";
        let html = format!(
            "<div {bloat}><table {bloat}><tr {bloat}>\
             <td {bloat}>deepseek-ai/DeepSeek-V4-Pro</td><td {bloat}>¥3</td>\
             <td {bloat}>¥6</td></tr></table></div>"
        );
        let cleaned = clean_pricing_html(&html);
        // 价目文本（模型串 + 价）完整保留。
        assert!(cleaned.contains("deepseek-ai/DeepSeek-V4-Pro"));
        assert!(cleaned.contains("¥3"));
        assert!(cleaned.contains("¥6"));
        // 属性（class / tailwind 串）被剥光。
        assert!(!cleaned.contains("class"));
        assert!(!cleaned.contains("flex-row"));
        assert!(!cleaned.contains("hover:bg-gray-50"));
        // 结构标签名保留。
        assert!(cleaned.contains("<table>"));
        assert!(cleaned.contains("<td>"));
        assert!(cleaned.contains("</tr>"));
        // 体积显著下降（class 噪声远大于价目文本）。
        assert!(cleaned.len() < html.len() / 2, "cleaned={} html={}", cleaned.len(), html.len());
    }

    #[test]
    fn clean_preserves_non_tag_less_than() {
        // 文本里的比较号 `<`（非标签）不应被吃掉。
        let html = "<p>maxContext < 32000 时</p>";
        let cleaned = clean_pricing_html(html);
        assert!(cleaned.contains("< 32000"), "got: {cleaned}");
    }

    #[test]
    fn was_truncated_only_when_over_limit() {
        // 短文本 false。
        assert!(!was_truncated("short text"));
        assert!(!was_truncated(&"X".repeat(EXTRACTION_TEXT_LIMIT)));
        // 超长 true。
        assert!(was_truncated(&"X".repeat(EXTRACTION_TEXT_LIMIT + 1)));
        // 多字节按 char 计：EXTRACTION_TEXT_LIMIT 个汉字（每个 3 字节）字符数==上限 → false。
        assert!(!was_truncated(&"中".repeat(EXTRACTION_TEXT_LIMIT)));
        assert!(was_truncated(&"中".repeat(EXTRACTION_TEXT_LIMIT + 1)));
    }

    // —— parse_extraction ——

    #[test]
    fn parse_bare_json_array() {
        let txt = r#"[{"modelId":"deepseek-v4-pro","input":3,"output":6,"cachedInput":0.025}]"#;
        let models = parse_extraction(txt, "CNY").expect("parse ok");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model_id, "deepseek-v4-pro");
        assert_eq!(models[0].pricing.input, 3.0);
        assert_eq!(models[0].pricing.output, 6.0);
        assert_eq!(models[0].pricing.cached_input, Some(0.025));
        // 注入：currency=CNY、unit=per_1m。
        assert_eq!(models[0].pricing.currency, "CNY");
        assert_eq!(models[0].pricing.unit, "per_1m");
    }

    #[test]
    fn parse_fenced_json_array() {
        let txt = "好的，结果如下：\n```json\n[{\"modelId\":\"x\",\"input\":1,\"output\":2}]\n```\n";
        let models = parse_extraction(txt, "CNY").expect("parse ok");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model_id, "x");
        assert_eq!(models[0].pricing.cached_input, None);
    }

    #[test]
    fn parse_with_tiers() {
        let txt = r#"[{"modelId":"glm","input":6,"output":24,"cachedInput":1.3,
            "tiers":[{"maxContext":32000,"input":6,"output":24,"cachedInput":1.3},
                     {"maxContext":200000,"input":8,"output":28,"cachedInput":null}]}]"#;
        let models = parse_extraction(txt, "CNY").expect("parse ok");
        let tiers = models[0].pricing.tiers.as_ref().expect("has tiers");
        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].max_context, 32000);
        assert_eq!(tiers[1].cached_input, None);
        assert_eq!(tiers[1].input, 8.0);
    }

    #[test]
    fn parse_sorts_tiers_ascending_by_max_context() {
        // LLM 乱序给出（先大档后小档）；解析后应按 max_context 升序，供 select_rates 正确选档。
        let txt = r#"[{"modelId":"glm","input":8,"output":28,"cachedInput":2.0,
            "tiers":[{"maxContext":200000,"input":8,"output":28,"cachedInput":2.0},
                     {"maxContext":32000,"input":6,"output":24,"cachedInput":1.3}]}]"#;
        let models = parse_extraction(txt, "CNY").expect("parse ok");
        let tiers = models[0].pricing.tiers.as_ref().expect("has tiers");
        assert_eq!(tiers.len(), 2);
        // 升序：小档在前。
        assert_eq!(tiers[0].max_context, 32000);
        assert_eq!(tiers[0].input, 6.0);
        assert_eq!(tiers[1].max_context, 200000);
        assert_eq!(tiers[1].input, 8.0);
    }

    #[test]
    fn validate_zero_max_context_tier_is_err() {
        let mut m = mk("deepseek-v4-pro", 3.0, 6.0, Some(0.025));
        m.pricing.tiers = Some(vec![PriceTier {
            max_context: 0, // 非法：会让选档失真。
            input: 6.0,
            output: 24.0,
            cached_input: Some(1.3),
        }]);
        assert!(validate_extraction(&[m], "deepseek-v4-pro").is_err());
    }

    #[test]
    fn validate_positive_max_context_tier_is_ok() {
        let mut m = mk("deepseek-v4-pro", 3.0, 6.0, Some(0.025));
        m.pricing.tiers = Some(vec![PriceTier {
            max_context: 32_000,
            input: 6.0,
            output: 24.0,
            cached_input: Some(1.3),
        }]);
        assert!(validate_extraction(&[m], "deepseek-v4-pro").is_ok());
    }

    #[test]
    fn parse_cached_input_null() {
        let txt = r#"[{"modelId":"m","input":1,"output":2,"cachedInput":null}]"#;
        let models = parse_extraction(txt, "CNY").expect("parse ok");
        assert_eq!(models[0].pricing.cached_input, None);
    }

    #[test]
    fn parse_garbage_is_err() {
        assert!(parse_extraction("这里没有 json", "CNY").is_err());
        assert!(parse_extraction("", "CNY").is_err());
        // 有 `[` 但不是合法 JSON。
        assert!(parse_extraction("[not json at all", "CNY").is_err());
    }

    #[test]
    fn parse_skips_preamble_placeholder_brackets() {
        // 前言含占位方括号 `[占位]`/`[见后]`，其后才是真数组（裸，无围栏）。
        // 旧实现只取首个平衡块即整体失败；新实现应继续扫到真数组。
        let txt = "见后[占位] 详见下表[见后]，结果：\
            [{\"modelId\":\"deepseek-v4-pro\",\"input\":3,\"output\":6,\"cachedInput\":0.025}]";
        let models = parse_extraction(txt, "CNY").expect("should skip placeholders");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model_id, "deepseek-v4-pro");
        assert_eq!(models[0].pricing.input, 3.0);
    }

    #[test]
    fn parse_skips_placeholder_then_fenced_array() {
        // 前言占位方括号 + 真数组在 ```json 围栏内。
        let txt = "草稿[草稿]\n```json\n\
            [{\"modelId\":\"deepseek-ai/DeepSeek-V4-Pro\",\"input\":3,\"output\":6}]\n```\n完";
        let models = parse_extraction(txt, "CNY").expect("fenced array after placeholder");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model_id, "deepseek-ai/DeepSeek-V4-Pro");
    }

    #[test]
    fn parse_skips_wrong_shaped_array_then_finds_real() {
        // 第一个平衡数组是「非目标形态」（纯字符串数组），应跳过、继续找真模型数组。
        let txt = "候选模型: [\"a\",\"b\",\"c\"]\n实际定价: \
            [{\"modelId\":\"x\",\"input\":1,\"output\":2}]";
        let models = parse_extraction(txt, "CNY").expect("skip string array, find model array");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model_id, "x");
    }

    // —— validate_extraction ——

    fn mk(model_id: &str, input: f64, output: f64, cached: Option<f64>) -> ExtractedModel {
        ExtractedModel {
            model_id: model_id.to_string(),
            pricing: ModelPricing {
                currency: "CNY".to_string(),
                unit: "per_1m".to_string(),
                input,
                output,
                cached_input: cached,
                cache_write: None,
                batch_discount: None,
                tiers: None,
            },
        }
    }

    #[test]
    fn validate_ok_deepseek_anchor() {
        let models = vec![
            mk("deepseek-v4-flash", 1.0, 2.0, Some(0.02)),
            mk("deepseek-v4-pro", 3.0, 6.0, Some(0.025)),
        ];
        assert!(validate_extraction(&models, "deepseek-v4-pro").is_ok());
    }

    #[test]
    fn validate_ok_siliconflow_anchor_case_insensitive() {
        // 锚点比对大小写不敏感：抽到的大小写与锚点不同也应过。
        let models = vec![mk("deepseek-ai/deepseek-v4-pro", 3.0, 6.0, Some(0.03))];
        assert!(validate_extraction(&models, "deepseek-ai/DeepSeek-V4-Pro").is_ok());
    }

    #[test]
    fn validate_missing_anchor_is_err() {
        let models = vec![mk("some-other-model", 1.0, 2.0, None)];
        assert!(validate_extraction(&models, "deepseek-v4-pro").is_err());
    }

    #[test]
    fn validate_empty_is_err() {
        assert!(validate_extraction(&[], "deepseek-v4-pro").is_err());
    }

    #[test]
    fn validate_zero_price_is_err() {
        let models = vec![mk("deepseek-v4-pro", 0.0, 6.0, None)];
        assert!(validate_extraction(&models, "deepseek-v4-pro").is_err());
    }

    #[test]
    fn validate_absurd_price_is_err() {
        let models = vec![mk("deepseek-v4-pro", 99999.0, 6.0, None)];
        assert!(validate_extraction(&models, "deepseek-v4-pro").is_err());
    }

    #[test]
    fn validate_bad_cached_input_is_err() {
        let models = vec![mk("deepseek-v4-pro", 3.0, 6.0, Some(0.0))];
        assert!(validate_extraction(&models, "deepseek-v4-pro").is_err());
    }

    // —— diff_pricing ——

    fn pricing(input: f64, output: f64, cached: Option<f64>) -> ModelPricing {
        ModelPricing {
            currency: "CNY".to_string(),
            unit: "per_1m".to_string(),
            input,
            output,
            cached_input: cached,
            cache_write: None,
            batch_discount: None,
            tiers: None,
        }
    }

    #[test]
    fn diff_against_compiled_changed() {
        // 现价取自编译快照（deepseek-v4-pro CNY = 3/6/0.025）；抽到不同价 → changed。
        let compiled = lookup_preset("deepseek", "deepseek-v4-pro", "CNY")
            .expect("compiled exists")
            .pricing
            .clone();
        let extracted = mk("deepseek-v4-pro", 3.5, 6.5, Some(0.03));
        let d = diff_pricing("deepseek", &extracted, Some(&compiled));
        assert_eq!(d.change, "changed");
        assert!(d.old_pricing.is_some());
        assert_eq!(d.new_pricing.input, 3.5);
    }

    #[test]
    fn diff_against_compiled_unchanged() {
        let compiled = lookup_preset("deepseek", "deepseek-v4-pro", "CNY")
            .expect("compiled exists")
            .pricing
            .clone();
        // 抽到与编译完全一致 → unchanged（注意编译现价 3/6/0.025）。
        let extracted = mk("deepseek-v4-pro", 3.0, 6.0, Some(0.025));
        let d = diff_pricing("deepseek", &extracted, Some(&compiled));
        assert_eq!(d.change, "unchanged");
    }

    #[test]
    fn diff_new_model_when_no_current() {
        let extracted = mk("brand-new-model", 1.0, 2.0, None);
        let d = diff_pricing("deepseek", &extracted, None);
        assert_eq!(d.change, "new");
        assert!(d.old_pricing.is_none());
    }

    #[test]
    fn diff_against_override_current() {
        // 现价来自 override（解析出的 ModelPricing），价不同 → changed。
        let override_price = pricing(2.0, 4.0, Some(0.01));
        let extracted = mk("deepseek-ai/DeepSeek-V4-Pro", 3.0, 6.0, Some(0.03));
        let d = diff_pricing("siliconflow", &extracted, Some(&override_price));
        assert_eq!(d.change, "changed");
        assert_eq!(d.old_pricing.as_ref().unwrap().input, 2.0);
    }

    #[test]
    fn diff_cached_input_none_vs_some_is_changed() {
        // 逐字段比较：仅 cachedInput 由 None→Some 也算 changed。
        let cur = pricing(3.0, 6.0, None);
        let extracted = mk("deepseek-v4-pro", 3.0, 6.0, Some(0.025));
        let d = diff_pricing("deepseek", &extracted, Some(&cur));
        assert_eq!(d.change, "changed");
    }

    #[test]
    fn diff_tiers_difference_is_changed() {
        let mut cur = pricing(6.0, 24.0, Some(1.3));
        cur.tiers = Some(vec![PriceTier {
            max_context: 32000,
            input: 6.0,
            output: 24.0,
            cached_input: Some(1.3),
        }]);
        let mut new = mk("glm", 6.0, 24.0, Some(1.3));
        new.pricing.tiers = Some(vec![PriceTier {
            max_context: 32000,
            input: 7.0, // 改了一档输入价
            output: 24.0,
            cached_input: Some(1.3),
        }]);
        let d = diff_pricing("zhipu", &new, Some(&cur));
        assert_eq!(d.change, "changed");
    }

    // —— build_extraction_prompt ——

    #[test]
    fn prompt_injects_currency_and_truncates() {
        let big = "X".repeat(EXTRACTION_TEXT_LIMIT + 5000);
        let p = build_extraction_prompt(&big, "CNY", "zh");
        assert!(p.contains("CNY"));
        // 正文被截断：prompt 不应包含全部 5000 多余字符。
        assert!(p.len() < big.len() + 2000);
    }
}
