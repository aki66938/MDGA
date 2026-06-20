import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  open: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: mocks.listen
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: mocks.open
}));

describe("desktop MVP shell", () => {
  afterEach(() => {
    cleanup();
  });

  beforeEach(() => {
    Element.prototype.scrollIntoView = vi.fn();
    mocks.listen.mockResolvedValue(() => undefined);
    mocks.open.mockReset();
    mocks.invoke.mockImplementation((command: string) => {
      if (command === "get_deepseek_api_key_status") return Promise.resolve("Missing");
      if (command === "check_update") return Promise.resolve(null);
      if (command === "get_conversations") return Promise.resolve([]);
      return Promise.resolve([]);
    });
  });

  it("renders the app shell without crashing", () => {
    // 稳健冒烟:应用能挂载且顶部 MDGA 标识渲染。不再断言早期 MVP 的具体业务文案(DeepSeek 连接 / 写死的
    // V4 Flash/Pro 模型下拉等——多模型配置重构后已移除/迁入设置页),避免测试随 UI 演进持续失效(0.0.70)。
    render(<App />);
    expect(screen.getByRole("heading", { name: "MDGA" })).toBeTruthy();
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

  it("selects a workspace on the new conversation screen and binds it to the created conversation", async () => {
    mocks.open.mockResolvedValue("C:\\workspace\\demo");
    mocks.invoke.mockImplementation((command: string, args?: Record<string, unknown>) => {
      if (command === "get_deepseek_api_key_status") return Promise.resolve("Configured");
      if (command === "check_update") return Promise.resolve(null);
      if (command === "get_conversations") return Promise.resolve([]);
      if (command === "new_conversation_with_workspace") {
        expect(args?.workspacePath).toBe("C:\\workspace\\demo");
        return Promise.resolve({
          id: "conv-1",
          title: "新对话",
          workspacePath: args?.workspacePath,
          workspaceName: "demo",
          mode: "local_workspace",
          createdAt: 1,
          updatedAt: 1,
        });
      }
      if (command === "persist_message") return Promise.resolve(undefined);
      if (command === "rename_conversation") return Promise.resolve(undefined);
      if (command === "send_message") return Promise.resolve(undefined);
      return Promise.resolve([]);
    });

    render(<App />);

    // 0.0.70:composer 重构后工作区胶囊先开菜单,再点菜单项触发目录选择(旧版「点胶囊直接弹对话框」已变)。
    fireEvent.click(await screen.findByRole("button", { name: "选择工作区" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: /选择\/更换工作区/ }));

    // 选好工作区后,胶囊显示已选名 demo(草稿态;basenameFromPath("C:\\workspace\\demo")=demo)。
    // 旧版断言已移除的 "New conversation workspace" 带标签区域,改校验胶囊名,稳健不脆。
    await screen.findByText("demo");

    fireEvent.change(screen.getByLabelText("Message"), { target: { value: "帮我分析这个项目" } });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(mocks.invoke).toHaveBeenCalledWith(
      "new_conversation_with_workspace",
      { workspacePath: "C:\\workspace\\demo" }
    ));
    await waitFor(() => expect(mocks.invoke).toHaveBeenCalledWith(
      "send_message",
      expect.objectContaining({ conversationId: "conv-1" })
    ));
  });
});
