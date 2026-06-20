// 纯工具函数（0.0.37 从 App.tsx 抽出，纯搬移，无逻辑改动）。

import type {
  Message,
  UsageSummary,
  PricingCurrency,
  PricingUnit,
  StoredPricing,
  PricingTier,
  ModelPricing,
  PricingDiff,
  ApplyItem,
  EffectivePricingView,
} from "./types";

/** token 数精简成 k/M（对标 Claude 的「576.5k / 1.0M」）。 */
export function fmtTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return `${n}`;
}

// ── 计价工具（Pricing，0.0.72）──────────────────────────────────────────────

/** 币种符号：CNY→￥｜USD→＄。 */
export function currencySymbol(currency: PricingCurrency | undefined): string {
  return currency === "USD" ? "＄" : "￥";
}

/** 单位后缀：per_1k→/K｜per_1m（默认）→/M。 */
export function unitSuffix(unit: PricingUnit | undefined): string {
  return unit === "per_1k" ? "/K" : "/M";
}

/**
 * 解析 pricing_json（StoredPricing）。失败/空串返回 null。
 * 容错：缺 currency/unit 时补默认（CNY / per_1m），缺 _source 时按 'custom'。
 */
export function parseStoredPricing(json: string | null | undefined): StoredPricing | null {
  if (!json || !json.trim()) return null;
  try {
    const o = JSON.parse(json) as Partial<StoredPricing>;
    if (o == null || typeof o !== "object") return null;
    return {
      ...o,
      currency: o.currency === "USD" ? "USD" : "CNY",
      unit: o.unit === "per_1k" ? "per_1k" : "per_1m",
      input: typeof o.input === "number" ? o.input : 0,
      output: typeof o.output === "number" ? o.output : 0,
      _source: o._source === "preset" ? "preset" : "custom",
    } as StoredPricing;
  } catch {
    return null;
  }
}

/** 把一个价格数字按习惯精简显示：整数不带小数，否则去掉尾随 0。null/undefined→「—」。 */
export function fmtPriceNum(n: number | null | undefined): string {
  if (n == null || Number.isNaN(n)) return "—";
  if (Number.isInteger(n)) return String(n);
  return String(n).replace(/\.?0+$/, "");
}

/**
 * 模型行的单价摘要：`￥3 / 0.025 / 6 /M`（输入 / 命中 / 输出，符号随币种）。
 * 命中（cachedInput）缺省时该位显「—」。无价格（null）整体返回「—」。
 */
export function pricingSummary(p: StoredPricing | null): string {
  if (!p) return "—";
  const sym = currencySymbol(p.currency);
  const cached = p.cachedInput != null ? fmtPriceNum(p.cachedInput) : "—";
  return `${sym}${fmtPriceNum(p.input)} / ${cached} / ${fmtPriceNum(p.output)} ${unitSuffix(p.unit)}`;
}

/** 单价徽标判定：预设（可改） / 待官网核对 / 自定义。无价格返回 null（不显徽标）。 */
export type PricingBadge = "preset" | "needs_verify" | "custom";

/**
 * 计算应展示的徽标集合（顺序：来源徽标在前，needsVerify 叠加在后）。
 * _source='preset' → ['preset']，若 _needsVerify 再追加 'needs_verify'；
 * _source='custom' → ['custom']；无价格 → []。
 */
export function pricingBadges(p: StoredPricing | null): PricingBadge[] {
  if (!p) return [];
  if (p._source === "preset") {
    return p._needsVerify ? ["preset", "needs_verify"] : ["preset"];
  }
  return ["custom"];
}

/** 切单位时换算可见数字（per_1m↔per_1k：×/÷1000）。null/undefined 原样返回。 */
export function convertPriceForUnit(
  value: number | null | undefined,
  from: PricingUnit,
  to: PricingUnit,
): number | null | undefined {
  if (value == null || from === to) return value;
  // per_1m → per_1k：每千的数更小，÷1000；反之 ×1000。
  const factor = from === "per_1m" && to === "per_1k" ? 1 / 1000 : 1000;
  return value * factor;
}

export function aggregateUsage(messages: Message[]): UsageSummary | null {
  const usages = messages
    .map((m) => m.usage)
    .filter((u): u is UsageSummary => Boolean(u));
  if (usages.length === 0) return null;
  const pricingVersions = new Set(usages.map((u) => u.pricingVersion));
  const usageSources = new Set(usages.map((u) => u.usageSource));
  return usages.reduce<UsageSummary>(
    (total, u) => ({
      promptTokens: total.promptTokens + u.promptTokens,
      completionTokens: total.completionTokens + u.completionTokens,
      totalTokens: total.totalTokens + u.totalTokens,
      cacheHitTokens: total.cacheHitTokens + u.cacheHitTokens,
      cacheMissTokens: total.cacheMissTokens + u.cacheMissTokens,
      reasoningTokens: total.reasoningTokens + u.reasoningTokens,
      estimatedCostUsd: total.estimatedCostUsd + u.estimatedCostUsd,
      usageSource: usageSources.size === 1 ? u.usageSource : "mixed",
      pricingVersion: pricingVersions.size === 1 ? u.pricingVersion : "mixed",
    }),
    {
      promptTokens: 0, completionTokens: 0, totalTokens: 0,
      cacheHitTokens: 0, cacheMissTokens: 0, reasoningTokens: 0,
      estimatedCostUsd: 0, usageSource: "", pricingVersion: "",
    }
  );
}

export function formatUsd(cost: number): string {
  if (cost < 0.0001 && cost > 0) return "<$0.0001";
  return `$${cost.toFixed(6).replace(/\.?0+$/, "")}`;
}

/**
 * 金额格式化（0.0.72）：按币种带符号（CNY→￥｜USD→＄），去尾随 0。
 * 极小额（>0 且 <0.0001）显 `<￥0.0001` / `<＄0.0001`，避免精度损失误显 0。
 * 与 formatUsd 同风格，但符号随 currency；currency 缺省按 CNY。
 */
export function formatMoney(amount: number, currency: PricingCurrency | undefined): string {
  const sym = currencySymbol(currency);
  if (amount < 0.0001 && amount > 0) return `<${sym}0.0001`;
  return `${sym}${amount.toFixed(6).replace(/\.?0+$/, "")}`;
}

/**
 * 会话成本聚合（0.0.72）：按币种把「api 且有金额」的各轮 estimatedCost 分别累加，
 * 并分别计数套餐内 / 免计费 / 已计价但无金额（未计价）的轮数。
 *
 * 判定优先级：
 * - billingMode==='subscription' → subscriptionTurns++（不计金额）。
 * - billingMode==='none' → noneTurns++（不计金额）。
 * - billingMode==='api'：有金额（estimatedCost!=null）→ 按 currency 累加；无金额 → uncostedTurns++。
 * - billingMode 缺失（旧数据回退）：estimatedCostUsd>0 → 按 USD 累加；否则忽略（不计入任何计数，不当 0 元）。
 *
 * 设计要点：无金额的 api 轮绝不当 0 元并进合计，而是单独计入 uncostedTurns。
 */
export type CostAggregate = {
  byCurrency: { CNY?: number; USD?: number };
  subscriptionTurns: number;
  noneTurns: number;
  uncostedTurns: number;
};

export function aggregateCost(usages: UsageSummary[]): CostAggregate {
  const byCurrency: { CNY?: number; USD?: number } = {};
  let subscriptionTurns = 0;
  let noneTurns = 0;
  let uncostedTurns = 0;

  const add = (cur: PricingCurrency, amount: number) => {
    byCurrency[cur] = (byCurrency[cur] ?? 0) + amount;
  };

  for (const u of usages) {
    if (u.billingMode === "subscription") {
      subscriptionTurns += 1;
      continue;
    }
    if (u.billingMode === "none") {
      noneTurns += 1;
      continue;
    }
    if (u.billingMode === "api") {
      if (u.estimatedCost != null) {
        add(u.currency === "USD" ? "USD" : "CNY", u.estimatedCost);
      } else {
        uncostedTurns += 1;
      }
      continue;
    }
    // billingMode 缺失：旧持久化用量，按原 USD 估算回退（>0 才计，避免把 0 元轮并入）。
    if (u.estimatedCostUsd > 0) {
      add("USD", u.estimatedCostUsd);
    }
  }

  return { byCurrency, subscriptionTurns, noneTurns, uncostedTurns };
}

/** 把 aggregateCost 的分币种合计渲染为 `￥X` / `￥X ＋ ＄Y`；全空返回 ""（调用方据此显套餐/免计费/未计价文案）。 */
export function formatCostByCurrency(byCurrency: { CNY?: number; USD?: number }): string {
  const parts: string[] = [];
  if (byCurrency.CNY != null) parts.push(formatMoney(byCurrency.CNY, "CNY"));
  if (byCurrency.USD != null) parts.push(formatMoney(byCurrency.USD, "USD"));
  return parts.join(" ＋ ");
}

/**
 * 计价非负校验（0.0.72）：动钱字段不允许负值。返回中文错误文案；全部合法返回 null。
 *
 * 规则：
 * - 顶层 input/output/cachedInput/cacheWrite 若有值须 `>= 0`。
 * - batchDiscount 若有值须落在 `[0, 1]`（折扣是乘数，超出区间无意义）。
 * - 每个 tier 的 input/output/cachedInput 若有值须 `>= 0`。
 *
 * 纯函数：只读入已解析的数值（null=未填，跳过），不触碰任何状态，便于单测。
 */
export function validatePricingNonNeg(
  top: {
    input: number | null;
    output: number | null;
    cachedInput: number | null;
    cacheWrite: number | null;
    batchDiscount: number | null;
  },
  tiers: PricingTier[],
): string | null {
  const nonNeg = (v: number | null | undefined, label: string): string | null =>
    v != null && v < 0 ? `${label}不能为负数。` : null;

  const checks = [
    nonNeg(top.input, "输入（未命中）单价"),
    nonNeg(top.output, "输出单价"),
    nonNeg(top.cachedInput, "缓存命中单价"),
    nonNeg(top.cacheWrite, "缓存写入单价"),
  ];
  for (const err of checks) {
    if (err) return err;
  }
  if (top.batchDiscount != null && (top.batchDiscount < 0 || top.batchDiscount > 1)) {
    return "批量折扣须在 0 到 1 之间（如 0.5 表示五折）。";
  }
  for (let i = 0; i < tiers.length; i += 1) {
    const t = tiers[i];
    const tierErr =
      nonNeg(t.input, `第 ${i + 1} 档输入单价`) ??
      nonNeg(t.output, `第 ${i + 1} 档输出单价`) ??
      nonNeg(t.cachedInput, `第 ${i + 1} 档缓存命中单价`);
    if (tierErr) return tierErr;
  }
  return null;
}

// ── 官网单价采集 diff（0.0.73）纯函数 ─────────────────────────────────────────

/**
 * 一条 diff 的展示徽标判定（0.0.73）：把 change + 涨/降方向归一为一个 kind + 文案。
 * - change='new' → { kind:'new', label:'新模型' }（蓝）。
 * - change='unchanged' → { kind:'unchanged', label:'无变化' }（灰，行禁用置灰）。
 * - change='changed'：比较 new.output 与 old.output（output 为 None 时退而比 input）判涨/降：
 *     新价更低 → { kind:'down', label:'降价' }（绿）；更高 → { kind:'up', label:'涨价' }（琥珀）；
 *     方向算不出（缺旧价/两值相等）→ { kind:'changed', label:'有变化' }（中性）。
 */
export type ChangeBadge = {
  kind: "new" | "up" | "down" | "changed" | "unchanged";
  label: string;
};

export function changeBadge(diff: PricingDiff): ChangeBadge {
  if (diff.change === "new") return { kind: "new", label: "新模型" };
  if (diff.change === "unchanged") return { kind: "unchanged", label: "无变化" };
  // changed：判方向。
  const dir = pricingChangeDirection(diff.oldPricing, diff.newPricing);
  if (dir === "down") return { kind: "down", label: "降价" };
  if (dir === "up") return { kind: "up", label: "涨价" };
  return { kind: "changed", label: "有变化" };
}

/**
 * 涨/降方向（0.0.73）：以输出单价为主判据（output 缺则退用输入单价）。
 * 缺旧价、或主判据两侧都取不到、或相等 → null（方向未定）。返回 'up'｜'down'｜null。
 */
export function pricingChangeDirection(
  oldP: ModelPricing | undefined,
  newP: ModelPricing,
): "up" | "down" | null {
  if (!oldP) return null;
  // 主判据 output；output 任一侧缺（理论不该有，防御）退用 input。
  const pick = (p: ModelPricing): number | null =>
    typeof p.output === "number" ? p.output : typeof p.input === "number" ? p.input : null;
  const a = pick(oldP);
  const b = pick(newP);
  if (a == null || b == null) return null;
  if (b < a) return "down";
  if (b > a) return "up";
  return null;
}

/**
 * 极简相对新鲜度（0.0.73）：把秒级时间戳转成「刚刚更新 / N 分钟前 / N 小时前 / N 天前」。
 * fetchedAt 缺/未来时间 → 「刚刚更新」。nowMs 可注入便于单测（默认 Date.now()）。
 */
export function relativeTime(fetchedAtSec: number | undefined, nowMs: number = Date.now()): string {
  if (fetchedAtSec == null) return "刚刚更新";
  const deltaSec = Math.floor(nowMs / 1000) - fetchedAtSec;
  if (deltaSec < 60) return "刚刚更新";
  const min = Math.floor(deltaSec / 60);
  if (min < 60) return `${min} 分钟前`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr} 小时前`;
  const day = Math.floor(hr / 24);
  return `${day} 天前`;
}

/**
 * 加模型自动填单价的「是否落库」决策（0.0.73 修正，#3+#4）：纯函数，便于单测。
 * 只有来源是编译预设快照（source==='preset'，且 view 非空、有 pricing）才落库；
 * 来源是采集覆盖（source==='override'）一律**不落库**——否则会把实时官网价冻进模型条目，
 * 使其不再跟随后续官网刷新、且徽标误显「自定义」。让模型保持 pricing_json 空走实时有效价回退。
 */
export function shouldStoreAutofill(
  view: EffectivePricingView | null | undefined,
): view is EffectivePricingView {
  if (!view || !view.pricing) return false;
  return view.source === "preset";
}

/** 把一条 diff 组装成 apply 入参（0.0.73）：newPricing 序列化进 pricingJson，带 sourceUrl。 */
export function buildApplyItem(diff: PricingDiff, sourceUrl: string | undefined): ApplyItem {
  return {
    modelId: diff.modelId,
    currency: diff.currency,
    pricingJson: JSON.stringify(diff.newPricing),
    ...(sourceUrl ? { sourceUrl } : {}),
  };
}

/** 把选中的 diff（按 modelId+currency 主键集合）组装成 apply 入参数组（0.0.73）。 */
export function buildApplyItems(
  diffs: PricingDiff[],
  selectedKeys: Set<string>,
  sourceUrl: string | undefined,
): ApplyItem[] {
  return diffs
    .filter((d) => selectedKeys.has(diffKey(d)))
    .map((d) => buildApplyItem(d, sourceUrl));
}

/** 一条 diff 的稳定主键（modelId + currency）：用作勾选集合的元素与 React key。 */
export function diffKey(diff: PricingDiff): string {
  return diff.modelId + "|" + diff.currency;
}

/** 查找正文中工具调用标记的最早位置（<ToolCall>、DSML 各变体）；无标记返回 -1。 */
export function findToolMarkupIndex(text: string): number {
  const markers = ["<ToolCall", "<DSML", "<｜DSML", "<｜｜DSML"];
  let min = -1;
  for (const marker of markers) {
    const idx = text.indexOf(marker);
    if (idx >= 0 && (min < 0 || idx < min)) min = idx;
  }
  return min;
}

/** 把后端原始错误串映射为面向用户的友好提示与建议动作；未识别的错误保留原文便于反馈。 */
export function humanizeError(raw: string): string {
  if (raw.includes("未配置主模型") || raw.includes("DEEPSEEK_API_KEY")) {
    return "未配置主模型：请在 设置 → 模型连接 添加连接与模型，再到 模型分配 指定主模型后发送。";
  }
  if (raw.includes("认证失败")) {
    return "API Key 无效：请在 设置 → 模型连接 检查 API Key 是否填写正确。";
  }
  if (raw.includes("余额不足")) {
    return "DeepSeek 账户余额不足：请前往 DeepSeek 开放平台充值后重试。";
  }
  if (raw.includes("限流")) {
    return "请求被限流：当前请求过于频繁，请稍等片刻再发送。";
  }
  if (raw.includes("上下文长度超限")) {
    return "上下文超出模型上限：建议新建会话继续，或精简本次输入后重试。";
  }
  if (raw.includes("网络连接失败") || raw.toLowerCase().includes("error sending request")) {
    return "网络连接失败：已自动重试仍未成功，请检查网络（或代理设置）后重新发送。";
  }
  if (raw.includes("会话不存在")) {
    return "会话状态异常：请新建一个会话后继续。";
  }
  if (raw.includes("工作区路径不存在")) {
    return "工作区不可用：所选目录不存在或已被移动，请重新选择工作区。";
  }
  return `出错了：${raw}`;
}

export function basenameFromPath(path: string): string {
  const normalized = path.replace(/[\\/]+$/, "");
  const parts = normalized.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}
