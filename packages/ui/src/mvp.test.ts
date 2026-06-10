import { describe, expect, it } from "vitest";
import {
  calculateEstimatedCost,
  formatActivitySummary,
  getApiKeyStatusLabel,
  getPermissionModeLabel
} from "./index";

describe("MVP protocol helpers", () => {
  it("shows DeepSeek API key status without exposing the key", () => {
    expect(getApiKeyStatusLabel({ state: "configured" })).toBe("已配置");
    expect(getApiKeyStatusLabel({ state: "missing" })).toBe("未检测到 DEEPSEEK_API_KEY");
    expect(getApiKeyStatusLabel({ state: "connection_failed" })).toBe("连接测试失败");
  });

  it("maps permission modes to product labels", () => {
    expect(getPermissionModeLabel("restricted")).toBe("受限模式");
    expect(getPermissionModeLabel("ask_every_time")).toBe("每次询问");
    expect(getPermissionModeLabel("workspace_auto")).toBe("工作区自动");
    expect(getPermissionModeLabel("full_access")).toBe("完全访问");
  });

  it("summarizes activity events for collapsed display", () => {
    expect(formatActivitySummary({ type: "command.completed", count: 3 })).toBe("已运行 3 条命令");
    expect(formatActivitySummary({ type: "file.changed", count: 2 })).toBe("已编辑 2 个文件");
    expect(formatActivitySummary({ type: "tool.completed", count: 1 })).toBe("已完成 1 次工具调用");
  });

  it("calculates DeepSeek token cost from cache hit, cache miss and output prices", () => {
    const cost = calculateEstimatedCost({
      promptCacheHitTokens: 1_000_000,
      promptCacheMissTokens: 2_000_000,
      completionTokens: 3_000_000,
      pricing: {
        inputCacheHitPer1m: 0.0028,
        inputCacheMissPer1m: 0.14,
        outputPer1m: 0.28
      }
    });

    expect(cost).toBeCloseTo(1.1228);
  });
});
