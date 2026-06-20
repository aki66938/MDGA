import { describe, it, expect } from "vitest";
import { buildSrcdoc } from "./widget";

// 0.0.70 widget 安全 wrapper 冒烟:锁住 buildSrcdoc 的隔离不变量,防有人改回 CDN / 删 CSP / 加 unsafe-eval。
// (注:jsdom 不真执行 sandbox iframe 脚本,故这里只做结构断言;真渲染冒烟靠本机半自动。)
describe("widget buildSrcdoc 安全 wrapper", () => {
  const out = buildSrcdoc("<p id='ok'>RENDER_OK_SMOKE</p>", { "--color-text": "#000" });

  it("CSP 零外联:default-src/connect-src 'none'、子资源仅 data:/blob:、无外部 CDN、无 unsafe-eval", () => {
    expect(out).toContain("Content-Security-Policy");
    expect(out).toContain("default-src 'none'");
    expect(out).toContain("connect-src 'none'");
    expect(out).toMatch(/img-src data: blob:/);
    // 关键回归点:绝不放行外部 CDN / 字体域,绝不开 unsafe-eval。
    expect(out).not.toContain("cdnjs.cloudflare.com");
    expect(out).not.toContain("cdn.jsdelivr.net");
    expect(out).not.toContain("fonts.googleapis.com");
    expect(out).not.toContain("unsafe-eval");
  });

  it("fail-closed 自检在场(探测 Tauri 桥并真调一次 invoke)+ agent code 原样嵌入", () => {
    expect(out).toContain("__TAURI_INTERNALS__"); // 功能式探针:探 IPC 桥
    expect(out).toContain("get_app_info"); // 0.0.68:真调一次只读命令看是否打通
    expect(out).toContain("RENDER_OK_SMOKE"); // agent code 嵌入文档体
  });

  it("结构合法:单一 CSP meta、doctype/html/body 闭合", () => {
    expect(out.startsWith("<!doctype html>")).toBe(true);
    expect(out).toContain("</body></html>");
    expect((out.match(/Content-Security-Policy/g) || []).length).toBe(1);
  });
});
