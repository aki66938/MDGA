import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { App } from "./App";

describe("desktop MVP shell", () => {
  it("renders the core MVP status regions", () => {
    render(<App />);

    expect(screen.getByRole("heading", { name: "MDGA" })).toBeTruthy();
    expect(screen.getByText("未检测到 DEEPSEEK_API_KEY")).toBeTruthy();
    expect(screen.getByText("受限模式")).toBeTruthy();
    expect(screen.getByText("Token 账本")).toBeTruthy();
    expect(screen.getByText("新对话")).toBeTruthy();
  });
});
