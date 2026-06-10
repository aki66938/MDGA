export type ApiKeyStatus =
  | { state: "configured" }
  | { state: "missing" }
  | { state: "connection_failed" };

export type PermissionMode = "restricted" | "ask_every_time" | "workspace_auto" | "full_access";

export type ActivitySummaryInput = {
  type: "command.completed" | "file.changed" | "tool.completed";
  count: number;
};

export type TokenPricing = {
  inputCacheHitPer1m: number;
  inputCacheMissPer1m: number;
  outputPer1m: number;
};

export type TokenCostInput = {
  promptCacheHitTokens: number;
  promptCacheMissTokens: number;
  completionTokens: number;
  pricing: TokenPricing;
};

export function getApiKeyStatusLabel(status: ApiKeyStatus): string {
  switch (status.state) {
    case "configured":
      return "已配置";
    case "missing":
      return "未检测到 DEEPSEEK_API_KEY";
    case "connection_failed":
      return "连接测试失败";
  }
}

export function getPermissionModeLabel(mode: PermissionMode): string {
  switch (mode) {
    case "restricted":
      return "受限模式";
    case "ask_every_time":
      return "每次询问";
    case "workspace_auto":
      return "工作区自动";
    case "full_access":
      return "完全访问";
  }
}

export function formatActivitySummary(input: ActivitySummaryInput): string {
  switch (input.type) {
    case "command.completed":
      return `已运行 ${input.count} 条命令`;
    case "file.changed":
      return `已编辑 ${input.count} 个文件`;
    case "tool.completed":
      return `已完成 ${input.count} 次工具调用`;
  }
}

export function calculateEstimatedCost(input: TokenCostInput): number {
  const hitCost = (input.promptCacheHitTokens / 1_000_000) * input.pricing.inputCacheHitPer1m;
  const missCost = (input.promptCacheMissTokens / 1_000_000) * input.pricing.inputCacheMissPer1m;
  const outputCost = (input.completionTokens / 1_000_000) * input.pricing.outputPer1m;

  return hitCost + missCost + outputCost;
}
