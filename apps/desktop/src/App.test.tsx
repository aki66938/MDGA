import { render, screen } from "@testing-library/react";
import { vi } from "vitest";
import { describe, expect, it } from "vitest";
import { App } from "./App";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(() => Promise.resolve([]))
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => undefined))
}));

describe("desktop MVP shell", () => {
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
});
