// 部件画布（0.0.67）：把 show_widget 工具产出的 agent 编写 HTML/SVG/JS 渲染进一个
// 强隔离的 iframe。安全模型（不可妥协）：
//   sandbox="allow-scripts"，**绝不**带 allow-same-origin —— 在同源上叠加 allow-same-origin
//   会让 frame 自行卸下 sandbox 逃逸。单独 allow-scripts ＝ null/opaque 源：无 cookie/存储、
//   读不到父 DOM、所有请求 Origin:null。CSP connect-src 'none' 再封死网络出口。
//   即便 Tauri 2.11.2 已修复 IPC 注入到 null 源 iframe（GHSA-57fm-592m-34r7），仍在 wrapper
//   里加 fail-closed 自检：一旦探测到 __TAURI__ 立刻拒绝渲染。

import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { WidgetPart } from "../types";

/** 把 agent code 包进固定可信 wrapper，构造 iframe 的 srcdoc。code 为 HTML/SVG/JS，**原样**插入（不转义）。 */
function buildSrcdoc(code: string, theme: Record<string, string>): string {
  // 严格 CSP：默认全封；脚本/样式仅放行 inline + 两个常用 CDN（图表库可用）；
  // connect-src 'none' ＝ 无任何网络出口；base/form/frame 全封。
  const csp =
    "default-src 'none'; " +
    "script-src 'unsafe-inline' 'unsafe-eval' https://cdnjs.cloudflare.com https://cdn.jsdelivr.net; " +
    "style-src 'unsafe-inline' https://fonts.googleapis.com; " +
    "img-src data: blob: https://cdnjs.cloudflare.com https://cdn.jsdelivr.net; " +
    "font-src https://fonts.gstatic.com data:; " +
    "connect-src 'none'; base-uri 'none'; form-action 'none'; frame-src 'none'";

  // 把宿主当前主题（已 resolve 的具体色值）注入为 widget 侧 CSS 变量，
  // 模型被告知使用这些变量名，light/dark 由宿主 getComputedStyle 决定。
  const vars = Object.entries(theme)
    .map(([k, v]) => `      ${k}: ${v};`)
    .join("\n");

  // fail-closed 自检：探测到任一 Tauri 桥接全局即拒绝渲染（隔离失败兜底）。
  const selfCheck =
    "if (window.__TAURI__ || window.__TAURI_INTERNALS__ || window.__TAURI_INVOKE_KEY__) {" +
    " document.body.innerHTML = '<p style=\"color:#c00;font:13px system-ui\">\\u26A0 widget \\u9694\\u79BB\\u5931\\u8D25\\uFF0C\\u5DF2\\u62D2\\u7EDD\\u6E32\\u67D3</p>';" +
    " throw new Error('isolation breach'); }";

  return (
    "<!doctype html><html><head><meta charset=\"utf-8\">" +
    `<meta http-equiv="Content-Security-Policy" content="${csp}">` +
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
    "  " + selfCheck + "\n" +
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
 * @param onSendPrompt  widget 内调用 sendPrompt(text) 时回灌到 agent 的发送函数（如同用户输入）。
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
        const text = typeof data.text === "string" ? data.text : "";
        if (text.trim() && onSendPrompt) onSendPrompt(text);
      } else if (data.__mdgaWidget === "openLink") {
        const url = typeof data.url === "string" ? data.url : "";
        if (url && window.confirm("在浏览器打开此链接？\n" + url)) {
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
