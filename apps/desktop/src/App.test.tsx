import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: mocks.listen
}));

describe("desktop MVP shell", () => {
  afterEach(() => {
    cleanup();
  });

  beforeEach(() => {
    Element.prototype.scrollIntoView = vi.fn();
    mocks.listen.mockResolvedValue(() => undefined);
    mocks.invoke.mockImplementation((command: string) => {
      if (command === "get_deepseek_api_key_status") return Promise.resolve("Missing");
      if (command === "check_update") return Promise.resolve(null);
      if (command === "get_conversations") return Promise.resolve([]);
      return Promise.resolve([]);
    });
  });

  it("renders the core MVP status regions", () => {
    render(<App />);

    expect(screen.getByRole("heading", { name: "MDGA" })).toBeTruthy();
    expect(screen.getByText("未检测到 DEEPSEEK_API_KEY")).toBeTruthy();
    expect(screen.getByText("受限模式")).toBeTruthy();
    expect(screen.getByText("Token 账本")).toBeTruthy();
    expect(screen.getByRole("button", { name: /新对话/ })).toBeTruthy();
    expect(screen.getByRole("combobox", { name: "模型选择" })).toBeTruthy();
    expect(screen.getByRole("option", { name: "DeepSeek V4 Flash" })).toBeTruthy();
    expect(screen.getByRole("option", { name: "DeepSeek V4 Pro" })).toBeTruthy();
  });

  it("aggregates persisted usage for the active conversation", async () => {
    const usage = {
      promptTokens: 120,
      completionTokens: 80,
      totalTokens: 200,
      cacheHitTokens: 40,
      cacheMissTokens: 80,
      reasoningTokens: 12,
      estimatedCostUsd: 0.00012,
      usageSource: "deepseek_usage",
      pricingVersion: "deepseek-v4-flash-2026-06",
    };

    mocks.invoke.mockImplementation((command: string) => {
      if (command === "get_deepseek_api_key_status") return Promise.resolve("Configured");
      if (command === "check_update") return Promise.resolve(null);
      if (command === "get_conversations") {
        return Promise.resolve([{ id: "conv-1", title: "账本测试", createdAt: 1, updatedAt: 2 }]);
      }
      if (command === "load_messages") {
        return Promise.resolve([
          {
            id: "msg-1",
            conversationId: "conv-1",
            role: "user",
            content: "统计一下",
            usageJson: null,
            createdAt: 1,
          },
          {
            id: "msg-2",
            conversationId: "conv-1",
            role: "assistant",
            content: "好的",
            usageJson: JSON.stringify(usage),
            createdAt: 2,
          },
        ]);
      }
      return Promise.resolve([]);
    });

    render(<App />);

    fireEvent.click(await screen.findByText("账本测试"));

    await waitFor(() => {
      const summary = screen.getByLabelText("Conversation token summary");
      expect(within(summary).getByText("会话累计")).toBeTruthy();
      expect(within(summary).getByText("200 tokens")).toBeTruthy();
      expect(within(summary).getByText("$0.00012")).toBeTruthy();
    });
  });
});
