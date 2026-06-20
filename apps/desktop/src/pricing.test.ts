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
} from "./utils";
import type { StoredPricing, UsageSummary, PricingTier } from "./types";

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
