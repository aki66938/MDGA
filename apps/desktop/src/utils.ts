// 纯工具函数（0.0.37 从 App.tsx 抽出，纯搬移，无逻辑改动）。

import type { Message, UsageSummary } from "./types";

/** token 数精简成 k/M（对标 Claude 的「576.5k / 1.0M」）。 */
export function fmtTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return `${n}`;
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
    return "未配置主模型：请在 设置 → 模型供应商 中填写 API Key 与模型后再发送。";
  }
  if (raw.includes("认证失败")) {
    return "API Key 无效：请在 设置 → 模型供应商 检查 API Key 是否填写正确。";
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
