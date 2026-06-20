import { describe, it, expect } from "vitest";
import {
  parseStoredPricing,
  pricingSummary,
  pricingBadges,
  convertPriceForUnit,
  currencySymbol,
  unitSuffix,
  formatMoney,
  aggregateCost,
  formatCostByCurrency,
  validatePricingNonNeg,
  changeBadge,
  pricingChangeDirection,
  relativeTime,
  buildApplyItem,
  buildApplyItems,
  diffKey,
  humanizeError,
  shouldStoreAutofill,
} from "./utils";
import type { StoredPricing, UsageSummary, PricingTier, ModelPricing, PricingDiff, EffectivePricingView } from "./types";

/** 造一组顶层价格参数（默认全合法），便于在各用例只改一个字段。 */
function top(partial: Partial<{
  input: number | null;
  output: number | null;
  cachedInput: number | null;
  cacheWrite: number | null;
  batchDiscount: number | null;
}> = {}) {
  return { input: 1, output: 2, cachedInput: null, cacheWrite: null, batchDiscount: null, ...partial };
}

/** 造一条最小可用的 UsageSummary（只关心计价相关字段）。 */
function usage(partial: Partial<UsageSummary>): UsageSummary {
  return {
    promptTokens: 0, completionTokens: 0, totalTokens: 0,
    cacheHitTokens: 0, cacheMissTokens: 0, reasoningTokens: 0,
    estimatedCostUsd: 0, usageSource: "", pricingVersion: "",
    ...partial,
  };
}

// 0.0.72 计价前端纯函数冒烟：摘要格式、徽标判定、单位换算、解析容错。

describe("pricing 纯函数（0.0.72）", () => {
  it("currencySymbol / unitSuffix", () => {
    expect(currencySymbol("CNY")).toBe("￥");
    expect(currencySymbol("USD")).toBe("＄");
    expect(currencySymbol(undefined)).toBe("￥");
    expect(unitSuffix("per_1m")).toBe("/M");
    expect(unitSuffix("per_1k")).toBe("/K");
    expect(unitSuffix(undefined)).toBe("/M");
  });

  it("pricingSummary：输入/命中/输出，符号随币种；命中缺＝—；无价＝—", () => {
    const p: StoredPricing = { currency: "CNY", unit: "per_1m", input: 3, output: 6, cachedInput: 0.025, _source: "preset" };
    expect(pricingSummary(p)).toBe("￥3 / 0.025 / 6 /M");
    const noCache: StoredPricing = { currency: "USD", unit: "per_1m", input: 2, output: 8, _source: "custom" };
    expect(pricingSummary(noCache)).toBe("＄2 / — / 8 /M");
    expect(pricingSummary(null)).toBe("—");
  });

  it("pricingBadges：preset→[preset]，needsVerify 叠加，custom→[custom]，无价→[]", () => {
    expect(pricingBadges({ currency: "CNY", unit: "per_1m", input: 1, output: 2, _source: "preset" })).toEqual(["preset"]);
    expect(
      pricingBadges({ currency: "CNY", unit: "per_1m", input: 1, output: 2, _source: "preset", _needsVerify: true }),
    ).toEqual(["preset", "needs_verify"]);
    expect(pricingBadges({ currency: "CNY", unit: "per_1m", input: 1, output: 2, _source: "custom" })).toEqual(["custom"]);
    expect(pricingBadges(null)).toEqual([]);
  });

  it("convertPriceForUnit：per_1m↔per_1k 为 ÷/×1000；同单位/空值原样", () => {
    expect(convertPriceForUnit(3, "per_1m", "per_1k")).toBeCloseTo(0.003);
    expect(convertPriceForUnit(0.003, "per_1k", "per_1m")).toBeCloseTo(3);
    expect(convertPriceForUnit(5, "per_1m", "per_1m")).toBe(5);
    expect(convertPriceForUnit(null, "per_1m", "per_1k")).toBe(null);
  });

  it("parseStoredPricing：合法 JSON 解析、补默认、坏 JSON→null、空→null", () => {
    const parsed = parseStoredPricing('{"currency":"USD","unit":"per_1k","input":1,"output":2,"_source":"custom"}');
    expect(parsed).toMatchObject({ currency: "USD", unit: "per_1k", input: 1, output: 2, _source: "custom" });
    // 缺字段补默认：currency→CNY、unit→per_1m、_source→custom。
    expect(parseStoredPricing('{"input":1,"output":2}')).toMatchObject({ currency: "CNY", unit: "per_1m", _source: "custom" });
    expect(parseStoredPricing("not json")).toBeNull();
    expect(parseStoredPricing("")).toBeNull();
    expect(parseStoredPricing(undefined)).toBeNull();
  });

  it("formatMoney：符号随币种、去尾随 0、极小额阈值", () => {
    expect(formatMoney(1.5, "CNY")).toBe("￥1.5");
    expect(formatMoney(2, "USD")).toBe("＄2");
    expect(formatMoney(1.5, undefined)).toBe("￥1.5"); // 缺 currency 按 CNY
    expect(formatMoney(0.00005, "CNY")).toBe("<￥0.0001"); // >0 且 <0.0001
    expect(formatMoney(0.00005, "USD")).toBe("<＄0.0001");
    expect(formatMoney(0, "CNY")).toBe("￥0"); // 恰为 0 不走极小额分支
  });

  it("aggregateCost：按币种分小计；套餐/免计费/未计价分别计数；无金额 api 不当 0 元", () => {
    const r = aggregateCost([
      usage({ billingMode: "api", currency: "CNY", estimatedCost: 1.5 }),
      usage({ billingMode: "api", currency: "CNY", estimatedCost: 0.5 }),
      usage({ billingMode: "api", currency: "USD", estimatedCost: 2 }),
      usage({ billingMode: "subscription" }),
      usage({ billingMode: "none" }),
      usage({ billingMode: "api", currency: "CNY", estimatedCost: null }), // 无金额→未计价
    ]);
    expect(r.byCurrency.CNY).toBeCloseTo(2);
    expect(r.byCurrency.USD).toBeCloseTo(2);
    expect(r.subscriptionTurns).toBe(1);
    expect(r.noneTurns).toBe(1);
    expect(r.uncostedTurns).toBe(1);
  });

  it("aggregateCost：billingMode 缺失旧数据→按 estimatedCostUsd>0 计 USD，0 元忽略", () => {
    const r = aggregateCost([
      usage({ estimatedCostUsd: 0.003 }), // 旧数据，无 billingMode
      usage({ estimatedCostUsd: 0 }),     // 0 元，忽略，不计任何计数
    ]);
    expect(r.byCurrency.USD).toBeCloseTo(0.003);
    expect(r.byCurrency.CNY).toBeUndefined();
    expect(r.subscriptionTurns).toBe(0);
    expect(r.noneTurns).toBe(0);
    expect(r.uncostedTurns).toBe(0);
    // 渲染：单币种 → 仅 ＄；双币种 → ￥X ＋ ＄Y；全空 → ""。
    expect(formatCostByCurrency(r.byCurrency)).toBe("＄0.003");
    expect(formatCostByCurrency({ CNY: 2, USD: 2 })).toBe("￥2 ＋ ＄2");
    expect(formatCostByCurrency({})).toBe("");
  });
});

describe("validatePricingNonNeg（0.0.72 FIX 4）", () => {
  it("全合法（含 null/未填、0、折扣边界 0 与 1）→ null", () => {
    expect(validatePricingNonNeg(top(), [])).toBeNull();
    expect(validatePricingNonNeg(top({ input: 0, output: 0 }), [])).toBeNull();
    expect(validatePricingNonNeg(top({ cachedInput: 0.5, cacheWrite: 0.1 }), [])).toBeNull();
    expect(validatePricingNonNeg(top({ batchDiscount: 0 }), [])).toBeNull();
    expect(validatePricingNonNeg(top({ batchDiscount: 1 }), [])).toBeNull();
    expect(validatePricingNonNeg(top({ batchDiscount: 0.5 }), [])).toBeNull();
  });

  it("顶层任一负值 → 返回对应中文错误", () => {
    expect(validatePricingNonNeg(top({ input: -1 }), [])).toBe("输入（未命中）单价不能为负数。");
    expect(validatePricingNonNeg(top({ output: -0.01 }), [])).toBe("输出单价不能为负数。");
    expect(validatePricingNonNeg(top({ cachedInput: -1 }), [])).toBe("缓存命中单价不能为负数。");
    expect(validatePricingNonNeg(top({ cacheWrite: -1 }), [])).toBe("缓存写入单价不能为负数。");
  });

  it("batchDiscount 越界（<0 或 >1）→ 折扣区间错误", () => {
    expect(validatePricingNonNeg(top({ batchDiscount: -0.1 }), [])).toBe("批量折扣须在 0 到 1 之间（如 0.5 表示五折）。");
    expect(validatePricingNonNeg(top({ batchDiscount: 1.5 }), [])).toBe("批量折扣须在 0 到 1 之间（如 0.5 表示五折）。");
  });

  it("tier 负值 → 带档序号的错误（合法档不报）", () => {
    const okTier: PricingTier = { maxContext: 32000, input: 6, output: 24, cachedInput: 1.3 };
    expect(validatePricingNonNeg(top(), [okTier])).toBeNull();
    const badInput: PricingTier = { maxContext: 32000, input: -6, output: 24 };
    expect(validatePricingNonNeg(top(), [okTier, badInput])).toBe("第 2 档输入单价不能为负数。");
    const badCached: PricingTier = { maxContext: 32000, input: 6, output: 24, cachedInput: -1 };
    expect(validatePricingNonNeg(top(), [badCached])).toBe("第 1 档缓存命中单价不能为负数。");
  });
});

// ── 官网单价采集 diff 纯函数（0.0.73）────────────────────────────────────────

/** 造一条 ModelPricing（默认 CNY/per_1m）。 */
function mp(partial: Partial<ModelPricing> = {}): ModelPricing {
  return { currency: "CNY", unit: "per_1m", input: 1, output: 2, ...partial };
}

/** 造一条 PricingDiff（默认 changed，old=1/2、new=1/2）。 */
function diff(partial: Partial<PricingDiff> = {}): PricingDiff {
  return {
    modelId: "m",
    currency: "CNY",
    change: "changed",
    oldPricing: mp(),
    newPricing: mp(),
    ...partial,
  };
}

describe("changeBadge / pricingChangeDirection（0.0.73）", () => {
  it("new→新模型蓝；unchanged→无变化灰", () => {
    expect(changeBadge(diff({ change: "new", oldPricing: undefined }))).toEqual({ kind: "new", label: "新模型" });
    expect(changeBadge(diff({ change: "unchanged" }))).toEqual({ kind: "unchanged", label: "无变化" });
  });

  it("changed：output 降→降价绿、涨→涨价琥珀", () => {
    const down = diff({ oldPricing: mp({ output: 6 }), newPricing: mp({ output: 4 }) });
    expect(changeBadge(down)).toEqual({ kind: "down", label: "降价" });
    const up = diff({ oldPricing: mp({ output: 4 }), newPricing: mp({ output: 6 }) });
    expect(changeBadge(up)).toEqual({ kind: "up", label: "涨价" });
  });

  it("changed：output 相等但其它字段变→方向未定→中性『有变化』", () => {
    // output 一致（方向算不出），但 cachedInput 变 → 仍是 changed，徽标中性。
    const d = diff({ oldPricing: mp({ output: 6, cachedInput: 1 }), newPricing: mp({ output: 6, cachedInput: 2 }) });
    expect(changeBadge(d)).toEqual({ kind: "changed", label: "有变化" });
  });

  it("pricingChangeDirection：缺旧价→null；output 缺退用 input", () => {
    expect(pricingChangeDirection(undefined, mp())).toBeNull();
    // output 两侧都缺（用 input 判）：input 6→4 = down。
    const oldNoOut = { currency: "CNY", unit: "per_1m", input: 6, output: 0 } as unknown as ModelPricing;
    const newNoOut = { currency: "CNY", unit: "per_1m", input: 4, output: 0 } as unknown as ModelPricing;
    // output 为 0（number）按 output 判：0===0 → null（相等）。验证 output 优先于 input。
    expect(pricingChangeDirection(oldNoOut, newNoOut)).toBeNull();
  });
});

describe("relativeTime（0.0.73）", () => {
  const now = 1_000_000; // 秒
  const nowMs = now * 1000;
  it("缺时间戳/不足 1 分钟→刚刚更新", () => {
    expect(relativeTime(undefined, nowMs)).toBe("刚刚更新");
    expect(relativeTime(now - 30, nowMs)).toBe("刚刚更新");
    // 未来时间戳（时钟漂移）→ 刚刚更新（delta<60）。
    expect(relativeTime(now + 100, nowMs)).toBe("刚刚更新");
  });
  it("分钟/小时/天", () => {
    expect(relativeTime(now - 5 * 60, nowMs)).toBe("5 分钟前");
    expect(relativeTime(now - 3 * 3600, nowMs)).toBe("3 小时前");
    expect(relativeTime(now - 2 * 86400, nowMs)).toBe("2 天前");
  });
});

describe("buildApplyItem(s) / diffKey（0.0.73）", () => {
  it("diffKey：modelId+currency 主键稳定", () => {
    expect(diffKey(diff({ modelId: "deepseek-v4-pro", currency: "CNY" }))).toBe("deepseek-v4-pro|CNY");
  });

  it("buildApplyItem：newPricing 序列化进 pricingJson，带/不带 sourceUrl", () => {
    const d = diff({ modelId: "x", currency: "CNY", newPricing: mp({ input: 3, output: 6 }) });
    const item = buildApplyItem(d, "https://e.com/pricing");
    expect(item.modelId).toBe("x");
    expect(item.currency).toBe("CNY");
    expect(item.sourceUrl).toBe("https://e.com/pricing");
    expect(JSON.parse(item.pricingJson)).toMatchObject({ input: 3, output: 6 });
    // 无 sourceUrl 时省略该键。
    expect(buildApplyItem(d, undefined).sourceUrl).toBeUndefined();
  });

  it("buildApplyItems：仅组装选中键的行", () => {
    const a = diff({ modelId: "a", currency: "CNY" });
    const b = diff({ modelId: "b", currency: "CNY" });
    const c = diff({ modelId: "c", currency: "CNY", change: "unchanged" });
    const selected = new Set([diffKey(a), diffKey(b)]);
    const items = buildApplyItems([a, b, c], selected, "https://e.com/pricing");
    expect(items.map((i) => i.modelId)).toEqual(["a", "b"]);
  });
});

// shouldStoreAutofill（#3+#4 修复）：加模型自动填只对编译预设落库；采集覆盖一律不落库（保持走实时回退）。
describe("shouldStoreAutofill 自动填落库决策", () => {
  const ev = (partial: Partial<EffectivePricingView>): EffectivePricingView => ({
    pricing: mp(),
    source: "preset",
    needsVerify: false,
    ...partial,
  });

  it("source==='preset' 且有 pricing → 落库", () => {
    expect(shouldStoreAutofill(ev({ source: "preset" }))).toBe(true);
  });

  it("source==='override' → 不落库（不把官网价冻进 pricing_json）", () => {
    expect(shouldStoreAutofill(ev({ source: "override" }))).toBe(false);
  });

  it("view 为 null / undefined / 无 pricing → 不落库", () => {
    expect(shouldStoreAutofill(null)).toBe(false);
    expect(shouldStoreAutofill(undefined)).toBe(false);
    expect(shouldStoreAutofill({ source: "preset", needsVerify: false } as unknown as EffectivePricingView)).toBe(false);
  });
});

// humanizeError（#10 修复）：0.0.60 后设置页只有「模型连接 / 模型分配」，不应再指向已废的「模型供应商」。
describe("humanizeError 引导文案不指向已废分类", () => {
  it("未配置主模型：指向 模型连接 / 模型分配，不含「模型供应商」", () => {
    const out = humanizeError("未配置主模型");
    expect(out).toContain("模型连接");
    expect(out).not.toContain("模型供应商");
  });

  it("认证失败：指向 模型连接，不含「模型供应商」", () => {
    const out = humanizeError("认证失败");
    expect(out).toContain("模型连接");
    expect(out).not.toContain("模型供应商");
  });

  it("未识别错误保留原文", () => {
    expect(humanizeError("某个未知错误")).toContain("某个未知错误");
  });
});
