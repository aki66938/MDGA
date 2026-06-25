// 互动卡片画布（0.0.67 起；0.0.74 改名 render_artifact）：把 render_artifact 工具产出的
// agent 编写 HTML/SVG/JS 渲染进一个强隔离的 iframe。安全模型（不可妥协）：
//   sandbox="allow-scripts"，**绝不**带 allow-same-origin —— 在同源上叠加 allow-same-origin
//   会让 frame 自行卸下 sandbox 逃逸。单独 allow-scripts ＝ null/opaque 源：无 cookie/存储、
//   读不到父 DOM、所有请求 Origin:null。
//   即便 Tauri 2.11.2 已修复 IPC 注入到 null 源 iframe（GHSA-57fm-592m-34r7），仍在 wrapper
//   里加 fail-closed 探针兜底。
//   0.0.68 修正（基于运行时实测）：WebView2 会把 Tauri 的 __TAURI_INTERNALS__ 注入到**所有** frame
//   （含本 null 源 iframe），所以旧版「探测到该全局存在即拒渲染」是**必然误报**——每个卡片都被挡。
//   实测：从 null 源 iframe 真去 invoke('get_app_info') 被 Tauri **当场拒绝**（"Origin header is not a
//   valid URL" + 缺 __TAURI_INVOKE_KEY__），即「全局存在但打不通」、隔离有效。故把探针从「看名字」改成
//   「看是否真能打通」：异步真调一次无害只读命令，**唯有它竟然 resolve 成功**（IPC 真打通=真出事）才
//   window.stop()+抹掉页面拒渲染；被拒/超时/无 invoke（正常隔离）则不动，卡片正常渲染。
//
// 网络出口（0.0.67 安全审查后修正——勿再误述为「零网络」）：
//   · 子资源出口（img/script/style/font 的 GET）已用「default-src 'none' + 仅 data:/blob:、零外部
//     主机」彻底封死——这是会偷「用户在卡片表单里输入的数据」的静默信道，必须关。
//   · connect-src 'none' 封 fetch/XHR/WebSocket/sendBeacon。
//   · 唯一**无法**在 CSP/sandbox 文档层阻断的残留通道是脚本「自导航」(location.href=外部URL)：它会
//     让整个卡片跳走/变白，**可见、一次性、噪声大**，不是静默信道；且能植入恶意卡片的 agent
//     本就已被 prompt 注入污染、另有渠道。我们接受该残留并如实记录，不假称已 100% 断网。
// 画布只是画布（0.0.80）：产物**没有任何**回灌聊天 / 打开外链的出口——sendPrompt / openLink 已彻底移除。
//   理由：卡片是 agent 编写、不可信的；它绝不应能「替用户说话」往会话里塞消息，也不应能驱动浏览器跳转。
//   产物→父 的 postMessage 只有这几类，全为父侧/用户驱动、**无任何对外副作用**（都进不了会话、开不了链接）：
//     · resize（自报高度，纯排版）· csp-violation（仅 dev 观测）
//     · export-image（仅当用户点「导出」、父侧带 per-render nonce 请求后回传图片）
//     · view-gesture（仅放大态：把落在产物上的 wheel 转发给父侧做「以光标为中心缩放」——**纯只读视图通道**，
//       只驱动父侧 CSS transform，不涉任何对外能力）。
//   父→产物 的 postMessage：theme（推主题色值）、view-mode（告知是否放大态）。均为纯展示控制。
//
// 0.0.74 第二步新增（同样守红线）：
//   · 放大画布查看器：纯父侧 CSS（外层 wrapper 切 position:fixed + 变换层 translate/scale），
//     **同一 iframe 节点不重挂、不重置 srcDoc**——节点不动＝不 reload＝产物交互状态保留。
//     不给 iframe 加 allow="fullscreen"、不动 sandbox/CSP。
//   · 复制/下载为 PNG（0.0.74 返工：弃 html2canvas + 一次性截图 iframe）：父侧读不到 null 源
//     iframe 内容，故由**展示 iframe 自身**按需把当前 DOM 用 foreignObject 包成 SVG → data:image/svg+xml
//     → <img> → canvas → toBlob('image/png') 导出（实测在 null 源 + ARTIFACT_CSP 下 canvas 不 taint，
//     因 CSP 禁外链恰好排除了会 taint 的路径）。html2canvas 因克隆子 iframe 跨不透明源读 contentDocument
//     SecurityError 而**弃用**（依赖已移除）。捕获当前态、完整内容尺寸（W/H 取 scrollWidth/Height）、
//     内联 :root 主题 CSS 变量进 foreignObject（否则 var(--…) 颜色解析不到）。
//     导出 IIFE 注入在**用户产物 code 之前**，持有一个**烤进闭包局部的 per-render nonce**（绝不挂
//     window、绝不在产物前可见），父侧校验回传消息的 来源 + nonce（#3 防产物伪造）+ 类型 + 大小后落地。
//   · dev CSP 可观测：buildSrcdoc 仅 dev 构建注入 securitypolicyviolation 监听，经
//     postMessage('csp-violation') 回传父侧 console.warn（帮定位「交互失效=模型引了被 CSP 封的东西」）。

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { LayoutGrid, ChevronDown, ChevronUp, Maximize, Copy, Download, X, Plus, Minus, PanelRight } from "lucide-react";
import type { ArtifactPart } from "../types";
import { ARTIFACT_RUNTIME_JS, isDeclarativeSpec } from "./artifact-runtime";

/** 展示 iframe 与截图 iframe 共用的严格 CSP（**单一来源**，防两处漂移）。
 *  default-src 'none' 全封；脚本/样式只放行 inline、**不放行任何外部主机**；子资源只允许 data:/blob:；
 *  connect-src 'none' 封 fetch/XHR/WS/sendBeacon；无 'unsafe-eval'。导出供测试断言两处同款。 */
export const ARTIFACT_CSP =
  "default-src 'none'; " +
  "script-src 'unsafe-inline'; " +
  "style-src 'unsafe-inline'; " +
  "img-src data: blob:; " +
  "font-src data:; " +
  "connect-src 'none'; base-uri 'none'; form-action 'none'; frame-src 'none'";

/** 展示 iframe 与截图 iframe 共用的 sandbox 值（**绝不**含 allow-same-origin）。导出供测试断言。 */
export const ARTIFACT_SANDBOX = "allow-scripts";

/** 导出回传的 PNG dataURL 大小上限（≈12MB）。超限丢弃，防恶意/巨幅产物撑爆父侧剪贴板/下载。 */
export const ARTIFACT_EXPORT_MAX_BYTES = 12 * 1024 * 1024;

/** 构造展示 iframe 内的「导出」可信 IIFE（0.0.74 返工）：把当前 DOM 用 foreignObject 包成 SVG →
 *  data:image/svg+xml → <img> → canvas → toBlob('image/png') 导出，回传父侧 postMessage('export-image')。
 *  关键安全点：
 *   · 此 IIFE 必须注入在**用户产物 code 之前**，使其闭包局部（含 nonce、抓住的 postMessage 引用）
 *     在后跑的产物代码里读不到——产物拿不到 nonce 就无法伪造合法回传（#3）。
 *   · nonce 由父侧 per-render 生成，作为**字面量**烤进该 IIFE 闭包局部（JSON.stringify 转义防注入），
 *     **绝不**挂 window、**绝不**在产物前以全局可见。
 *   · 仅响应父侧发来的 {__mdgaArtifact:'export-request'}；handler 内**同步**读取当前 DOM 再序列化，
 *     降低产物在同一事件里抢改 DOM 的机会。本 IIFE 作为先注册的 message 监听。
 *   · W/H 取 body 完整内容尺寸（scrollWidth/Height，覆盖完整产物、不裁剪不留白——修 #7）。
 *   · 把当前 :root 的主题 CSS 变量内联进 foreignObject 里的 <style>（否则 var(--…) 颜色解析不到）。
 *  导出供测试断言（nonce 烤入、SVG/HTML 分支、foreignObject 包装、零外链）。
 *  @param nonceLiteral  已 JSON.stringify 的 nonce 字符串字面量（含两端引号）。 */
export function buildExportScript(nonceLiteral: string): string {
  // 顶部尽早抓住需要的全局引用，减少被产物篡改面。
  return (
    "(function(){\n" +
    "  var NONCE=" + nonceLiteral + ";\n" +
    "  var PM=window.parent.postMessage.bind(window.parent);\n" +
    "  var XS=window.XMLSerializer;\n" +
    "  var DPR=Math.min(2, (window.devicePixelRatio||1));\n" +
    "  function ok(u){try{PM({__mdgaArtifact:'export-image', token:NONCE, dataUrl:String(u)}, '*');}catch(e){}}\n" +
    "  function fail(m){try{PM({__mdgaArtifact:'export-image', token:NONCE, error:String(m).slice(0,200)}, '*');}catch(e){}}\n" +
    // 把一个 SVG 字符串画到 canvas 再 toBlob('image/png')，读成 dataURL 回传。
    "  function rasterize(svgStr, w, h){try{\n" +
    "    var url='data:image/svg+xml;charset=utf-8,'+encodeURIComponent(svgStr);\n" +
    "    var img=new Image();\n" +
    "    img.onload=function(){try{\n" +
    "      var cv=document.createElement('canvas');\n" +
    "      cv.width=Math.max(1,Math.ceil(w*DPR)); cv.height=Math.max(1,Math.ceil(h*DPR));\n" +
    "      var ctx=cv.getContext('2d'); ctx.scale(DPR,DPR); ctx.drawImage(img,0,0,w,h);\n" +
    "      cv.toBlob(function(b){ if(!b){return fail('toBlob null');} var fr=new FileReader(); fr.onload=function(){ok(fr.result);}; fr.onerror=function(){fail('read fail');}; fr.readAsDataURL(b); },'image/png');\n" +
    "    }catch(e){fail(e&&e.message||e);}};\n" +
    "    img.onerror=function(){fail('img load fail');};\n" +
    "    img.src=url;\n" +
    "  }catch(e){fail(e&&e.message||e);}}\n" +
    // 收集当前 :root 的主题 CSS 变量，内联进导出文档（否则 var(--…) 颜色在导出图里解析不到）。
    // 注意：初始变量经 <style>:root{} 注入（不在 documentElement.style 内联上），故必须从
    // getComputedStyle 读已 resolve 的具体值；用一个固定变量名集合（与 readHostTheme 注入的一致）。
    "  var TVARS=['--color-text','--color-bg','--color-text-secondary','--color-text-tertiary','--border','--brand','--color-success','--color-danger','--on-brand'];\n" +
    "  function themeStyle(){try{\n" +
    "    var cs=getComputedStyle(document.documentElement); var decl='';\n" +
    "    for(var i=0;i<TVARS.length;i++){ var v=cs.getPropertyValue(TVARS[i]).trim(); if(v){ decl+=TVARS[i]+':'+v+';'; } }\n" +
    "    return decl?(':root{'+decl+'}'):'';\n" +
    "  }catch(e){return '';}}\n" +
    "  function doExport(){try{\n" +
    // SVG 分支：body 内就是单个 <svg> → 直接序列化该 <svg>。
    "    var only=null; var svgs=document.body.querySelectorAll('svg');\n" +
    "    if(svgs.length===1){ var bk=document.body.children; var nonWs=0; for(var j=0;j<bk.length;j++){ if(bk[j].tagName.toLowerCase()!=='script') nonWs++; } if(nonWs===1 && document.body.children.length>=1 && svgs[0].parentNode && svgs[0].parentNode.tagName && svgs[0].parentNode.tagName.toLowerCase()==='body'){ only=svgs[0]; } }\n" +
    "    if(only){\n" +
    "      var r=only.getBoundingClientRect();\n" +
    "      var sw=Math.max(1,Math.ceil(r.width||only.clientWidth||300));\n" +
    "      var sh=Math.max(1,Math.ceil(r.height||only.clientHeight||150));\n" +
    "      var ss=new XS().serializeToString(only);\n" +
    "      return rasterize(ss, sw, sh);\n" +
    "    }\n" +
    // HTML 分支：把 body 内容包进 foreignObject。W/H 取完整内容尺寸（修 #7 不裁剪不留白）。
    "    var de=document.documentElement;\n" +
    "    var w=Math.max(1, document.body.scrollWidth, de.scrollWidth, document.body.offsetWidth);\n" +
    "    var h=Math.max(1, document.body.scrollHeight, de.scrollHeight, document.body.offsetHeight);\n" +
    "    var html=document.body.innerHTML;\n" +
    "    var sty=themeStyle();\n" +
    "    var svg='<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"'+w+'\" height=\"'+h+'\">'\n" +
    "      +'<foreignObject x=\"0\" y=\"0\" width=\"'+w+'\" height=\"'+h+'\">'\n" +
    "      +'<div xmlns=\"http://www.w3.org/1999/xhtml\" style=\"width:'+w+'px;height:'+h+'px;\">'\n" +
    "      +'<style>'+sty+'</style>'+html+'</div></foreignObject></svg>';\n" +
    "    rasterize(svg, w, h);\n" +
    "  }catch(e){fail(e&&e.message||e);}}\n" +
    // 先注册 message 监听：仅认父侧 export-request，handler 内同步抓 DOM 再序列化。
    "  window.addEventListener('message', function(ev){ var d=ev&&ev.data; if(!d||d.__mdgaArtifact!=='export-request') return; doExport(); });\n" +
    "})();"
  );
}

/** 把 agent code 包进固定可信 wrapper，构造 iframe 的 srcdoc。code 为 HTML/SVG/JS，**原样**插入（不转义）。
 *  导出供 artifact.test.tsx 冒烟锁安全 wrapper(CSP 零外联 / 自检 / 无 CDN 等)防回归(0.0.70)。
 *  @param nonce  per-render 导出 nonce（父侧 crypto 生成），烤进导出 IIFE 闭包局部（产物读不到，#3 防伪造）。
 *  @param dev  仅 dev 构建传 true：注入 securitypolicyviolation 监听并经 postMessage 回传父侧；
 *              生产默认 false——不回传、无控制台噪声。 */
export function buildSrcdoc(code: string, theme: Record<string, string>, nonce: string, dev = false): string {
  // 严格 CSP：见 ARTIFACT_CSP 注释。两处 iframe 共用同一常量，防漂移。
  const csp = ARTIFACT_CSP;
  // nonce 作字面量烤进导出 IIFE 闭包：JSON.stringify 转义，防 nonce 里有引号等破坏脚本/注入。
  const exportScript = buildExportScript(JSON.stringify(nonce));

  // 把宿主当前主题（已 resolve 的具体色值）注入为卡片侧 CSS 变量，
  // 模型被告知使用这些变量名，light/dark 由宿主 getComputedStyle 决定。
  const vars = Object.entries(theme)
    .map(([k, v]) => `      ${k}: ${v};`)
    .join("\n");

  // fail-closed 功能式探针：放在 <head>、跑在 agent 代码之前。不再判断 Tauri 全局「是否存在」
  // （__TAURI_INTERNALS__ 必被注入，判存在=必然误报），而是**异步真调一次无害只读命令**：
  // 唯有它竟然 resolve 成功（IPC 真打通=隔离失败）才 window.stop()+抹掉页面拒渲染；
  // 被拒/超时/无 invoke（正常隔离，实测即此路）则不动，让卡片正常渲染。
  // 代价：正常情况下每次渲染会触发一次被 Tauri 拒绝的 invoke，dev 控制台会打一条 warning（生产无控制台）。
  const probe =
    "(function(){try{" +
    "var I=window.__TAURI_INTERNALS__;" +
    "if(!I||typeof I.invoke!=='function')return;" +
    "Promise.resolve(I.invoke('get_app_info',{})).then(function(){" +
    " try{window.stop();}catch(e){}" +
    " document.documentElement.innerHTML='<body style=\"margin:0\"><p style=\"color:#c00;font:13px system-ui;padding:12px\">\\u26A0 \\u5361\\u7247\\u9694\\u79BB\\u5931\\u8D25\\uFF08\\u68C0\\u6D4B\\u5230\\u53EF\\u8C03\\u7528\\u540E\\u7AEF\\uFF09\\uFF0C\\u5DF2\\u62D2\\u7EDD\\u6E32\\u67D3</p></body>';" +
    "},function(){});" + // rejected = 正常隔离，忽略
    "}catch(e){}})();";

  // dev CSP 可观测（0.0.74，仅 dev）：监听 securitypolicyviolation，把被 CSP 拦下的指令经 postMessage
  // 回传父侧 console.warn，帮定位「卡片交互失效＝模型引了被 CSP 封的资源/eval」。生产不注入此段（无控制台噪声）。
  const cspWatch = dev
    ? "<script>" +
      "document.addEventListener('securitypolicyviolation',function(e){try{" +
      "parent.postMessage({__mdgaArtifact:'csp-violation'," +
      "directive:String(e.effectiveDirective||e.violatedDirective||'')," +
      "blocked:String(e.blockedURI||'').slice(0,200)}, '*');" +
      "}catch(_){}}); </script>"
    : "";

  return (
    "<!doctype html><html><head><meta charset=\"utf-8\">" +
    `<meta http-equiv="Content-Security-Policy" content="${csp}">` +
    // 探针脚本置于 head、CSP meta 之后、其余内容之前（尽早起跑；其判定是异步的，不阻塞渲染）。
    "<script>" + probe + "</script>" +
    cspWatch +
    "<style>\n" +
    "    :root {\n" +
    vars +
    "\n    }\n" +
    "    html, body { margin: 0; }\n" +
    "    body { font-family: system-ui, sans-serif; color: var(--color-text); background: transparent; }\n" +
    "</style></head><body>\n" +
    // 导出 IIFE：置于用户产物 code **之前**，其闭包局部（含 nonce）对后跑的产物代码不可见（#3 防伪造）。
    "<script>\n" + exportScript + "\n</script>\n" +
    // 内联 UI 运行时（0.0.80）：在导出 IIFE 之后、产物 code 之前注入（同「先注册、不读 nonce」模式）。
    // 仅挂 window.UI/window.h，纯 DOM、无 eval/网络，CSP/sandbox 一字不动。给模型一套自带交互的组件。
    "<script>\n" + ARTIFACT_RUNTIME_JS + "\n</script>\n" +
    // 产物：若是「纯 JSON 声明式 spec」→ 交 UI.mountSpec 渲染（弱模型可控产出可交互稿，语法幻觉≈0）；
    // 否则当作 HTML/SVG/JS 原样插入（既有行为不变）。spec 经 JSON.stringify 后再把**每个** `<` 转义成
    // `\x3c`（JS 里 `\x3c` 即 `<`，故 spec 内容不变）：脚本文本里彻底不出现 `<` → 不可能形成 `</script` /
    // `<!--` / `<script` 等任何 script-data 状态序列,杜绝脚本标签越界(防御纵深,超出 raw-text 已足够的最低要求)。
    (isDeclarativeSpec(code)
      ? "<script>try{window.UI&&window.UI.mountSpec(" +
        JSON.stringify(code).replace(/</g, "\\x3c") +
        ");}catch(e){try{document.body.textContent='spec error: '+(e&&e.message||e);}catch(_){}}</script>"
      : code.trim().charCodeAt(0) === 0x7b
        ? // 看着像声明式 spec（以 `{` 开头）但 JSON 解析失败 → 多半生成时被截断/格式有误。
          // 干净占位,绝不把整坨原始 JSON 当 HTML 文本糊一屏(正是 0.0.80 真机暴露的问题)。
          "<div style='padding:14px;color:var(--color-text-secondary);font:13px/1.6 system-ui;'>⚠ 这份设计稿的数据不完整或格式有误，未能渲染（生成时可能被截断）。请让 AI 重新生成，或要求更简洁的版本。</div>"
        : code) +
    "\n<script>\n" +
    "(function(){\n" +
    // 画布只是画布：不暴露任何 sendPrompt / openLink 桥。产物只能自报高度、响应主题/导出请求。
    "  function rh(){ var h = Math.max(document.documentElement.scrollHeight, document.body.scrollHeight, document.body.offsetHeight); parent.postMessage({__mdgaArtifact:'resize', height: h}, '*'); }\n" +
    "  window.addEventListener('load', rh);\n" +
    "  try{ new ResizeObserver(rh).observe(document.body); }catch(e){}\n" +
    // 父→产物消息：theme（主题桥，0.0.74：切主题不再重建 srcDoc，只更新 :root CSS 变量，不触网）
    // 与 view-mode（放大态开关，0.0.80：仅用于决定是否转发 wheel 给父侧缩放）。仅认这两类。
    "  var __mx=false;\n" +
    "  window.addEventListener('message', function(ev){ var d = ev && ev.data; if(!d) return;\n" +
    "    if(d.__mdgaArtifact==='view-mode'){ __mx = !!d.maximized; return; }\n" +
    "    if(d.__mdgaArtifact==='theme' && d.vars){ try{ var r = document.documentElement; for(var k in d.vars){ if(Object.prototype.hasOwnProperty.call(d.vars,k)){ r.style.setProperty(k, String(d.vars[k])); } } }catch(e){} }\n" +
    "  });\n" +
    // 放大态滚轮转发（0.0.80）：iframe 是事件边界,父侧拿不到落在产物上的 wheel,故由产物把它转发给父侧
    // 用于「以光标为中心缩放视图」。这是**只读视图通道**——只驱动父侧 CSS transform,与 sendPrompt/openLink
    // 无任何关系,不能往会话注入/不能开链接。仅放大态(__mx)拦截并 preventDefault；内联态完全不动滚轮(照常滚动)。
    // 左键点击/输入/拖动等交互一律由产物自身消费,不受影响(放大态交互与内联零差异)。
    "  window.addEventListener('wheel', function(e){ if(!__mx) return; e.preventDefault(); parent.postMessage({__mdgaArtifact:'view-gesture', kind:'wheel', deltaY: e.deltaY, ctrlKey: !!e.ctrlKey, x: e.clientX, y: e.clientY}, '*'); }, {passive:false});\n" +
    "})();\n" +
    "</script>\n" +
    "</body></html>"
  );
}

/** 从宿主根元素读取已 resolve 的主题色值，映射成卡片侧变量名。 */
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

/** 高度由不可信卡片自报，父侧必须 clamp：下限防空卡，上限只防浏览器被超大值撑爆/崩溃，
 *  正常完整内容永不触上限（故卡片永不出滚动条）。导出供测试。 */
export const ARTIFACT_MIN_HEIGHT = 40;
export const ARTIFACT_MAX_HEIGHT = 50000;
export function clampArtifactHeight(h: number): number {
  if (!Number.isFinite(h)) return ARTIFACT_MIN_HEIGHT;
  return Math.max(ARTIFACT_MIN_HEIGHT, Math.min(ARTIFACT_MAX_HEIGHT, h));
}

// ── 放大查看器的纯变换逻辑（可单测）──────────────────────────────────────────

/** 缩放系数范围：下限防缩没、上限防爆。导出供测试。 */
export const ARTIFACT_MIN_SCALE = 0.2;
export const ARTIFACT_MAX_SCALE = 8;
export function clampScale(s: number): number {
  if (!Number.isFinite(s)) return 1;
  return Math.max(ARTIFACT_MIN_SCALE, Math.min(ARTIFACT_MAX_SCALE, s));
}

/** 画布查看器的变换状态：scale + 平移量（CSS px，应用在变换层上）。 */
export type CanvasTransform = { scale: number; tx: number; ty: number };

/**
 * 以光标为中心的缩放：保持光标下的「内容点」在缩放前后落在同一屏幕位置。
 * @param t        当前变换。
 * @param factor   本次缩放倍率（>1 放大、<1 缩小，如滚轮 1.1 / 0.9）。
 * @param px,py    光标相对**变换层原点（未变换坐标系）容器**的屏幕坐标（px）。
 * 推导：屏幕点 = origin + t.tx + scale*content。缩放后要 px 不变，则
 *   px = newTx + newScale*content，content=(px - t.tx)/t.scale ⇒
 *   newTx = px - newScale*(px - t.tx)/t.scale。ty 同理。导出供测试。
 */
export function zoomAtPoint(t: CanvasTransform, factor: number, px: number, py: number): CanvasTransform {
  const newScale = clampScale(t.scale * factor);
  // 真实生效倍率（被 clamp 后）：用它换算平移，避免触顶/触底时漂移。
  const eff = newScale / t.scale;
  const tx = px - eff * (px - t.tx);
  const ty = py - eff * (py - t.ty);
  return { scale: newScale, tx, ty };
}

/** 平移：在当前变换上叠加位移（拖拽 delta）。导出供测试。 */
export function panBy(t: CanvasTransform, dx: number, dy: number): CanvasTransform {
  return { scale: t.scale, tx: t.tx + dx, ty: t.ty + dy };
}

/** ctrl+wheel（Chromium/WebView2 触控板捏合、或 ctrl+鼠标滚轮）的平滑缩放因子（跨引擎）。
 *  触控板捏合发的是 `wheel` 且 `e.ctrlKey===true`，deltaY 连续而小——固定 1.1 太跳；改用
 *  `factor = exp(-deltaY * k)`（deltaY<0=放大→factor>1；deltaY>0=缩小→factor<1），再把
 *  factor 夹在合理区间防极端 deltaY 一帧爆缩/爆放。k 默认约 0.01。导出供测试。
 *  @param deltaY  WheelEvent.deltaY。
 *  @param k       灵敏度系数（默认 ARTIFACT_PINCH_WHEEL_K）。 */
export const ARTIFACT_PINCH_WHEEL_K = 0.01;
/** 单次 ctrl+wheel 平滑因子的夹取区间（防一帧巨幅 delta 把缩放炸飞）。 */
export const ARTIFACT_PINCH_FACTOR_MIN = 0.5;
export const ARTIFACT_PINCH_FACTOR_MAX = 2;
export function pinchWheelFactor(deltaY: number, k: number = ARTIFACT_PINCH_WHEEL_K): number {
  if (!Number.isFinite(deltaY)) return 1; // NaN/±Infinity 的 deltaY 视为无效输入 → 不缩放
  const f = Math.exp(-deltaY * k);
  if (Number.isNaN(f)) return 1;
  // exp 可能溢出到 +Infinity（极大负 deltaY=大幅放大）：Math.min 会把它夹到 MAX；
  // 极大正 deltaY 时 exp 趋 0 → 夹到 MIN。故只需排除 NaN，溢出由 clamp 兜住。
  return Math.max(ARTIFACT_PINCH_FACTOR_MIN, Math.min(ARTIFACT_PINCH_FACTOR_MAX, f));
}

/** 两个触点的欧氏距离（触屏双指捏合用）。导出供测试。 */
export function touchDistance(
  a: { clientX: number; clientY: number },
  b: { clientX: number; clientY: number }
): number {
  const dx = a.clientX - b.clientX;
  const dy = a.clientY - b.clientY;
  return Math.hypot(dx, dy);
}

/** 两个触点的中点（屏幕坐标；触屏双指捏合的缩放锚点）。导出供测试。 */
export function touchMidpoint(
  a: { clientX: number; clientY: number },
  b: { clientX: number; clientY: number }
): { x: number; y: number } {
  return { x: (a.clientX + b.clientX) / 2, y: (a.clientY + b.clientY) / 2 };
}

/** 由「当前双指距离 / 上次双指距离」算缩放因子；距离非法/上次为 0 时回退 1（不缩放）。导出供测试。 */
export function pinchDistanceFactor(prevDist: number, curDist: number): number {
  if (!Number.isFinite(prevDist) || !Number.isFinite(curDist) || prevDist <= 0 || curDist <= 0) return 1;
  return curDist / prevDist;
}

/** 复位变换（双击）。 */
export const IDENTITY_TRANSFORM: CanvasTransform = { scale: 1, tx: 0, ty: 0 };

/** 「点击 vs 拖拽平移」判定阈值（px）：指针按下到抬起的位移在此半径内视为单纯点击（可关查看器），
 *  超出则视为拖拽平移（不关）。导出供测试。 */
export const ARTIFACT_CLICK_SLOP = 5;

/**
 * 判定一次指针交互是「单纯点击」还是「拖拽平移」：按下→抬起的位移在 slop 半径内＝点击。
 * 用于放大态点画布空白处关闭查看器：点击才关，拖拽平移不关。导出供测试。
 * @param dx,dy  抬起相对按下的位移（px）。
 * @param slop   阈值半径（默认 ARTIFACT_CLICK_SLOP）。
 */
export function isClickNotDrag(dx: number, dy: number, slop: number = ARTIFACT_CLICK_SLOP): boolean {
  return Math.abs(dx) <= slop && Math.abs(dy) <= slop;
}

// ── 导图（export-image）消息校验（可单测）────────────────────────────────────

/** 父侧落地前对 export-image 消息的校验结果。 */
export type ExportImageCheck =
  | { ok: true; dataUrl: string }
  | { ok: false; reason: string };

/**
 * 校验展示 iframe 回传的 export-image 消息。来源（event.source===展示 iframe contentWindow）由调用处确认；
 * 此函数校验：**nonce**（#3：必须等于本次渲染烤进导出 IIFE 闭包的 token——产物代码读不到该闭包局部，
 * 无法伪造合法回传）、**类型**（必须 image/png 的 dataURL）、**大小**（≤上限）。
 * 把纯校验抽出便于单测（来源校验依赖运行时 contentWindow，在组件里做）。
 * @param data  消息体（含 token + dataUrl 或 error）。
 * @param expectedToken  本次渲染的 nonce（非空字符串）。data.token 必须与之严格相等，否则拒。
 * @param maxBytes  大小上限（默认 ARTIFACT_EXPORT_MAX_BYTES）。
 */
export function checkExportImage(
  data: { token?: unknown; dataUrl?: unknown; error?: unknown },
  expectedToken: string,
  maxBytes: number = ARTIFACT_EXPORT_MAX_BYTES
): ExportImageCheck {
  // nonce 校验**先于一切**（含 error 分支）：无合法 token 的消息一律视为伪造/陈旧，整条丢弃。
  // 产物代码跑在导出 IIFE 之后、读不到其闭包局部 nonce，故无法构造 token 匹配的伪造回传（#3）。
  if (!expectedToken || typeof data.token !== "string" || data.token !== expectedToken) {
    return { ok: false, reason: "bad token" };
  }
  if (typeof data.error === "string" && data.error) {
    return { ok: false, reason: "render-error: " + data.error.slice(0, 200) };
  }
  const url = data.dataUrl;
  if (typeof url !== "string" || !url) return { ok: false, reason: "no dataUrl" };
  // 类型：必须是 image/png 的 dataURL（限死，不接受任意 data:/外部 url）。
  if (!url.startsWith("data:image/png;base64,")) return { ok: false, reason: "not png dataUrl" };
  // 大小：base64 解码后近似字节数 = base64 长度 * 3/4（忽略 padding 的微小误差）。
  const b64 = url.slice("data:image/png;base64,".length);
  if (!b64) return { ok: false, reason: "empty payload" };
  const approxBytes = Math.floor((b64.length * 3) / 4);
  if (approxBytes > maxBytes) return { ok: false, reason: "too large" };
  return { ok: true, dataUrl: url };
}

/** 生成 per-render 导出 nonce（crypto 随机 → hex）。父侧每次构建 srcDoc 时生成、存 ref，
 *  烤进导出 IIFE 闭包局部，并用于校验回传的 token（#3 防产物伪造）。 */
export function generateExportNonce(): string {
  try {
    const a = new Uint8Array(16);
    crypto.getRandomValues(a);
    let s = "";
    for (let i = 0; i < a.length; i++) s += a[i].toString(16).padStart(2, "0");
    return s;
  } catch {
    // 极端兜底（无 crypto）：仍给一个不可被产物预知的值（时间+随机）。导出可用、安全性退化但不致命。
    return "n" + Date.now().toString(36) + Math.random().toString(36).slice(2);
  }
}

/** 把卡片标题等清洗成安全的下载文件名（#9）：去除/替换非法与路径字符、控制字符；trim；
 *  全空白或空 → 回退 'artifact'。不含扩展名（调用处再拼 '.png'）。导出供测试。 */
export function sanitizeFilename(name: string | undefined): string {
  const raw = typeof name === "string" ? name : "";
  // 替换 Windows/Unix 非法路径字符 / \ : * ? " < > | 及控制字符为 '-'，去掉首尾点/空白。
  // eslint-disable-next-line no-control-regex
  const cleaned = raw
    .replace(/[/\\:*?"<>|\x00-\x1f]/g, "-")
    .replace(/\s+/g, " ")
    .trim()
    .replace(/^[.\s]+|[.\s]+$/g, "")
    .slice(0, 120);
  return cleaned || "artifact";
}

/** dataURL → Blob（父窗口是安全上下文，可写剪贴板/触发下载；null 源 iframe 不可，故必须父侧转）。 */
function dataUrlToBlob(dataUrl: string): Blob {
  const comma = dataUrl.indexOf(",");
  const b64 = dataUrl.slice(comma + 1);
  const bin = atob(b64);
  const len = bin.length;
  const bytes = new Uint8Array(len);
  for (let i = 0; i < len; i++) bytes[i] = bin.charCodeAt(i);
  return new Blob([bytes], { type: "image/png" });
}

/**
 * 沙箱化的互动卡片。
 * @param part   ArtifactPart（code + 可选 title/kind）。
 * @param pushToast  全局 toast（成功/失败提示用）；可选——不传则降级为仅 console.warn。
 * @param onDock  「停靠到侧栏」回调（0.0.75）：可选——传入时工具栏多一个停靠图标按钮，点了把本产物
 *               拉到第三栏「产物」坞（复用同一 ArtifactCard 渲染，同安全模型）。不传则不显该按钮
 *               （停靠态自身复用本组件时即不传，避免坞里再显停靠按钮）。**纯 UI 回调，不碰任何隔离逻辑。**
 */
export function ArtifactCard({
  part,
  pushToast,
  onDock,
}: {
  part: ArtifactPart;
  pushToast?: (kind: "error" | "info", text: string) => void;
  onDock?: (part: ArtifactPart) => void;
}) {
  const iframeRef = useRef<HTMLIFrameElement>(null);
  const [height, setHeight] = useState(120);
  // 折叠：默认展开。折叠用 display:none 隐藏 iframe 容器、**不卸载**，保产物交互状态。
  const [expanded, setExpanded] = useState(true);
  // 放大查看器：开关 + 画布变换（scale/平移）。开/关只切外层 wrapper 的 CSS（position:fixed），
  // **不重挂 iframe 节点、不重置 srcDoc**——节点不动＝不 reload＝产物交互状态保留。
  const [maximized, setMaximized] = useState(false);
  const [transform, setTransform] = useState<CanvasTransform>(IDENTITY_TRANSFORM);
  // 导出进行中守卫（#防连点）：true 时忽略再次点击；收到回传或超时后解除。
  const [exporting, setExporting] = useState(false);

  // 画布拖拽平移：仅在放大态可用（内联卡片态保持 FE-1 现状，不 pan/zoom）。
  // downX/downY 记录按下起点（用于「点击 vs 拖拽」判定）；onBlank 记按下是否落在画布空白暗区
  //（=event.target 即 __canvas 本身，非 __stage/产物），单纯点击空白处才关查看器（修复 #4）。
  const panRef = useRef<{ active: boolean; x: number; y: number; downX: number; downY: number; onBlank: boolean }>({
    active: false,
    x: 0,
    y: 0,
    downX: 0,
    downY: 0,
    onBlank: false,
  });
  // 画布层 ref：滚轮以光标为中心缩放时，需要光标相对画布层原点的坐标。
  const canvasRef = useRef<HTMLDivElement>(null);
  // 变换层（产物所在的 __stage）ref：放大态用它的未变换尺寸（offsetWidth/Height）算居中初始平移。
  const stageRef = useRef<HTMLDivElement>(null);
  // per-render 导出 nonce：随 srcDoc 一同（仅 code 变时）生成、烤进导出 IIFE 闭包，并存此 ref 供回传校验（#3）。
  const nonceRef = useRef("");

  // srcDoc 只在 code 变化时构建（主题取构建时的初始值）：主题切换**不**重建 srcDoc，否则串变→iframe
  // reload→丢放大态与产物内部状态。主题变化改经下方 postMessage 推给 iframe（纯 CSS 变量更新）。
  // 同次构建固定一个 export nonce（烤进导出 IIFE 闭包局部 + 存 nonceRef，配合 #6 随 code 一起固定）。
  // 仅 dev 构建注入 CSP 违规可观测（import.meta.env.DEV：build 为 false，生产无控制台噪声）。
  // eslint-disable-next-line react-hooks/exhaustive-deps —— 故意只依赖 code：主题走 postMessage 桥，不进 srcDoc。
  const srcdoc = useMemo(() => {
    const nonce = generateExportNonce();
    nonceRef.current = nonce;
    return buildSrcdoc(part.code, readHostTheme(), nonce, import.meta.env.DEV);
  }, [part.code]);

  // ── 导出（复制/下载 = PNG）：请**展示 iframe 自身**用 foreignObject 导出当前态，回传后落地 ─────
  // 父侧读不到 null 源 iframe 内容，故由 iframe 内导出 IIFE 把当前 DOM 序列化成 PNG dataURL 受控回传。
  const runExport = useCallback(
    (mode: "copy" | "download") => {
      if (exporting) return;
      const win = iframeRef.current?.contentWindow;
      const token = nonceRef.current;
      if (!win || !token) {
        pushToast?.("error", "卡片尚未就绪，请稍后再试。");
        return;
      }
      setExporting(true);

      let done = false;
      const fail = (toastMsg: string, reason: string) => {
        if (done) return;
        done = true;
        cleanup();
        console.warn("[artifact] 导出失败：", reason);
        pushToast?.("error", toastMsg);
      };
      const cleanup = () => {
        window.removeEventListener("message", onMsg);
        clearTimeout(timer);
        setExporting(false);
      };

      const land = (dataUrl: string) => {
        if (done) return;
        done = true;
        cleanup();
        try {
          const blob = dataUrlToBlob(dataUrl);
          if (mode === "copy") {
            // #8：剪贴板写图能力 feature-detect（无 Clipboard API / 无 ClipboardItem → 明确提示，别静默）。
            if (!(navigator.clipboard && typeof navigator.clipboard.write === "function" && typeof window.ClipboardItem === "function")) {
              pushToast?.("error", "当前环境不支持复制图片到剪贴板，请改用「下载为图片」。");
              return;
            }
            const item = new ClipboardItem({ "image/png": blob });
            void navigator.clipboard
              .write([item])
              .then(() => pushToast?.("info", "已复制为图片"))
              .catch((e) => {
                console.warn("[artifact] 复制到剪贴板失败：", e);
                pushToast?.("error", "复制到剪贴板失败，请改用「下载为图片」。");
              });
          } else {
            const url = URL.createObjectURL(blob);
            const a = document.createElement("a");
            a.href = url;
            // #9：标题清洗成安全文件名（去非法/路径/控制字符，空白/空回退 'artifact'）。
            a.download = sanitizeFilename(part.title) + ".png";
            document.body.appendChild(a);
            a.click();
            a.remove();
            setTimeout(() => URL.revokeObjectURL(url), 1000);
            pushToast?.("info", "已下载为图片");
          }
        } catch (e) {
          console.warn("[artifact] 导出落地失败：", e);
          pushToast?.("error", "导出失败，请重试。");
        }
      };

      function onMsg(event: MessageEvent) {
        // 红线：只收来自**本展示 iframe** contentWindow 的消息（来源校验）。
        if (event.source !== iframeRef.current?.contentWindow) return;
        const data = event.data as { __mdgaArtifact?: string; token?: unknown; dataUrl?: unknown; error?: unknown };
        if (!data || data.__mdgaArtifact !== "export-image") return;
        // 校验四件：来源（上面）+ nonce + 类型 + 大小。nonce 失配（含产物伪造、陈旧回传）整条丢弃、不解守卫——
        // 由超时兜底解除，避免产物用错 token 的回传提前打断本次合法导出。
        const check = checkExportImage(data, token);
        if (!check.ok) {
          if (check.reason === "bad token") return; // 伪造/陈旧：静默忽略，等真回传或超时
          return fail("导出失败：" + check.reason, check.reason);
        }
        land(check.dataUrl);
      }

      window.addEventListener("message", onMsg);
      // 超时兜底（10s）：iframe 不回（产物吞了请求 / 导出异常）也解守卫，不泄漏监听。
      const timer = setTimeout(() => fail("导出超时，请重试。", "timeout"), 10000);
      // 发起导出请求（父→子）；导出 IIFE 在产物 code 之前注册了 message 监听。
      win.postMessage({ __mdgaArtifact: "export-request" }, "*");
    },
    [exporting, part.title, pushToast]
  );

  // 以画布坐标系内某点 (px,py) 为中心缩放。供「暗区原生 wheel」与「产物转发的 view-gesture wheel」共用，
  // 保证两条路径缩放语义一致。ctrl+wheel = 触控板捏合 → 按 deltaY 幅度平滑；普通滚轮 → 固定 1.1 因子。
  const applyWheelZoom = useCallback((deltaY: number, ctrlKey: boolean, px: number, py: number) => {
    const factor = ctrlKey ? pinchWheelFactor(deltaY) : deltaY < 0 ? 1.1 : 1 / 1.1;
    setTransform((t) => zoomAtPoint(t, factor, px, py));
  }, []);

  useEffect(() => {
    function onMessage(event: MessageEvent) {
      // 只接受来自**本展示 iframe** contentWindow 且带 __mdgaArtifact 标记的消息（防其它 frame / 扩展伪造）。
      // 注意：export-image（导出回传）在 runExport 内部用**独立、临时**监听处理（带 nonce 校验），此处不管。
      const win = iframeRef.current?.contentWindow;
      if (!win || event.source !== win) return;
      const data = event.data as {
        __mdgaArtifact?: string;
        height?: number;
        directive?: string;
        blocked?: string;
        kind?: string;
        deltaY?: number;
        ctrlKey?: boolean;
        x?: number;
        y?: number;
      };
      if (!data || typeof data.__mdgaArtifact !== "string") return;

      if (data.__mdgaArtifact === "resize") {
        // 高度全自适应：去掉 2000 上限，卡片随内容长高、永不出滚动条；clamp 上限仅防浏览器被
        // 恶意超大自报值撑爆（保留下限防空卡）。正常完整内容远不触上限。
        const h = typeof data.height === "number" ? data.height : 120;
        setHeight(clampArtifactHeight(h));
      } else if (data.__mdgaArtifact === "csp-violation") {
        // dev CSP 可观测（0.0.74）：仅 dev 注入的监听才会发来；父侧也校验来源（上面已确认 event.source）。
        // console.warn 帮定位「交互失效＝模型引了被 CSP 封的东西」。生产不会注入、故不会到这里。
        if (import.meta.env.DEV) {
          console.warn(
            "[artifact][CSP] 卡片内一条资源/指令被 CSP 拦下：",
            "directive=" + (typeof data.directive === "string" ? data.directive : "?"),
            typeof data.blocked === "string" && data.blocked ? "blocked=" + data.blocked : ""
          );
        }
      } else if (data.__mdgaArtifact === "view-gesture" && data.kind === "wheel") {
        // 0.0.80：放大态产物把落在其上的 wheel 转发来（产物现可交互、父侧拿不到这层 wheel）。**纯只读视图通道**：
        // 仅驱动父侧 CSS transform 缩放,不涉任何对外能力,与 sendPrompt/openLink 无关(那两者已不存在)。
        // 把产物坐标(相对其自身视口、未含父侧 scale)映射回画布坐标系：屏幕点 = iframe 屏幕左上 + 产物内坐标×当前 scale。
        const cv = canvasRef.current;
        const fr = iframeRef.current;
        if (!cv || !fr || typeof data.x !== "number" || typeof data.y !== "number") return;
        const crect = cv.getBoundingClientRect();
        const frect = fr.getBoundingClientRect();
        const scale = fr.clientWidth > 0 && frect.width > 0 ? frect.width / fr.clientWidth : 1; // 当前渲染 scale（稳健,不依赖闭包；双守分母与零宽，杜绝 scale=0/NaN）
        const px = frect.left + data.x * scale - crect.left;
        const py = frect.top + data.y * scale - crect.top;
        applyWheelZoom(typeof data.deltaY === "number" ? data.deltaY : 0, !!data.ctrlKey, px, py);
      }
      // 不再有 sendPrompt / openLink 分支：画布只是画布，产物没有回灌聊天 / 打开外链的出口。
    }
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [applyWheelZoom]);

  // 主题桥（0.0.74，修复 #6）：srcDoc 只随 code 构建（见上 useMemo），主题变化不再重建 srcDoc，
  // 改观测宿主 <html data-theme> 的变化，把新 resolve 的色值经 postMessage 推给 iframe 内监听，
  // 由其改写 :root CSS 变量。纯 CSS 变量更新、不重挂 iframe、不触网，故放大态/产物内部状态全保。
  useEffect(() => {
    const root = document.documentElement;
    const push = () => {
      const win = iframeRef.current?.contentWindow;
      if (!win) return;
      win.postMessage({ __mdgaArtifact: "theme", vars: readHostTheme() }, "*");
    };
    const obs = new MutationObserver((records) => {
      // 仅在 data-theme 真变了才推（避免无关 attr 变动空转）。
      if (records.some((r) => r.attributeName === "data-theme")) push();
    });
    obs.observe(root, { attributes: true, attributeFilter: ["data-theme"] });
    return () => obs.disconnect();
  }, []);

  // 视图模式桥（0.0.80）：把「是否放大态」推给 iframe 内监听，决定它是否转发 wheel 给父侧缩放。
  // 放大开关变化时推一次；iframe 因 code 变化重挂后由 <iframe onLoad> 再推一次当前态（见下方 onLoad）。
  useEffect(() => {
    const win = iframeRef.current?.contentWindow;
    if (!win) return;
    win.postMessage({ __mdgaArtifact: "view-mode", maximized }, "*");
  }, [maximized]);

  // 放大态：Esc 退出（还原 wrapper 内联样式 + 复位变换）。绑在 document 上，模态期间生效。
  useEffect(() => {
    if (!maximized) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        setMaximized(false);
        setTransform(IDENTITY_TRANSFORM);
      }
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [maximized]);

  const openMaximized = useCallback(() => {
    setTransform(IDENTITY_TRANSFORM);
    setMaximized(true);
  }, []);
  const closeMaximized = useCallback(() => {
    setMaximized(false);
    setTransform(IDENTITY_TRANSFORM);
  }, []);

  // 居中复位（0.0.80）：把产物在画布里水平+垂直居中（scale=1）。用未变换尺寸（offsetWidth/Height，不受
  // transform 影响）算初始平移。内容比视口高/宽时夹到 0（靠左上对齐，保证起点可见、可下拉/右移浏览），
  // 否则两轴居中。坐标系仍以 __stage 的 top:0/left:0 原点为基准，故缩放/平移数学一字不改。
  const recenter = useCallback(() => {
    const cv = canvasRef.current;
    const st = stageRef.current;
    if (!cv || !st) return;
    const cw = cv.clientWidth;
    const ch = cv.clientHeight;
    const sw = st.offsetWidth;
    const sh = st.offsetHeight;
    if (!cw || !sw) return;
    setTransform({ tx: Math.max(0, (cw - sw) / 2), ty: Math.max(0, (ch - sh) / 2), scale: 1 });
  }, []);

  // 进入放大态后立刻居中（useLayoutEffect：在浏览器绘制前测量+置位，避免先闪一下左上角）。
  useLayoutEffect(() => {
    if (maximized) recenter();
  }, [maximized, recenter]);

  // 滚轮以光标为中心缩放（**仅放大态**，作用于画布**暗区**）。修复 #5：React 的 onWheel 是被动监听，
  // e.preventDefault() 会被忽略 → 缩放时整页背景跟着滚。改用**原生非被动监听**（passive:false）才能真正
  // preventDefault。退出放大/卸载时移除。光标坐标取相对画布层原点。
  // 0.0.80：产物 iframe 放大态不再 pointer-events:none（产物可交互），故落在**产物之上**的 wheel 由产物
  // 自身转发（见 onMessage 的 view-gesture 分支）；此原生监听只覆盖落在暗区的 wheel。两路共用 applyWheelZoom。
  useEffect(() => {
    if (!maximized) return;
    const el = canvasRef.current;
    if (!el) return;
    const handler = (e: WheelEvent) => {
      e.preventDefault(); // 非被动监听里此调用真正阻止默认的整页滚动 / 浏览器捏合缩放
      const rect = el.getBoundingClientRect();
      applyWheelZoom(e.deltaY, e.ctrlKey, e.clientX - rect.left, e.clientY - rect.top);
    };
    el.addEventListener("wheel", handler, { passive: false });
    return () => el.removeEventListener("wheel", handler);
  }, [maximized, applyWheelZoom]);

  // gesture 事件（macOS WKWebView 捏合，0.0.75）：WebKit 捏合发 gesturestart/change/end（带 e.scale），
  // **不发 ctrl+wheel**，故须单独处理。这些事件仅 WebKit 触发（Chromium/WebView2 不发，挂了无害）。
  // TS 无标准 GestureEvent 类型 → 局部 any-ish 接口声明 + as 断言。退出放大/卸载即 removeEventListener。
  useEffect(() => {
    if (!maximized) return;
    const el = canvasRef.current;
    if (!el) return;
    // 局部最小接口（标准库未声明 GestureEvent；仅 WebKit 有）。center 取手势中心相对 canvas 原点。
    type GestureLike = Event & { scale?: number; clientX?: number; clientY?: number };
    let lastScale = 1;
    let cx = 0;
    let cy = 0;
    const start = (ev: Event) => {
      const e = ev as GestureLike;
      e.preventDefault();
      lastScale = typeof e.scale === "number" && e.scale > 0 ? e.scale : 1;
      const rect = el.getBoundingClientRect();
      // 有手势中心坐标就用它，否则退回画布中心。
      cx = typeof e.clientX === "number" ? e.clientX - rect.left : el.clientWidth / 2;
      cy = typeof e.clientY === "number" ? e.clientY - rect.top : el.clientHeight / 2;
    };
    const change = (ev: Event) => {
      const e = ev as GestureLike;
      e.preventDefault();
      const s = typeof e.scale === "number" && e.scale > 0 ? e.scale : lastScale;
      const factor = pinchDistanceFactor(lastScale, s);
      lastScale = s;
      setTransform((t) => zoomAtPoint(t, factor, cx, cy));
    };
    const end = (ev: Event) => {
      ev.preventDefault();
      lastScale = 1;
    };
    el.addEventListener("gesturestart", start, { passive: false });
    el.addEventListener("gesturechange", change, { passive: false });
    el.addEventListener("gestureend", end, { passive: false });
    return () => {
      el.removeEventListener("gesturestart", start);
      el.removeEventListener("gesturechange", change);
      el.removeEventListener("gestureend", end);
    };
  }, [maximized]);

  // 触屏双指捏合缩放（0.0.75，仅放大态）：两指时记初始双指距离，move 时按「当前距/上次距」围绕两指中点
  // zoomAtPoint；preventDefault 防页面缩放。单指 touch 不在此处理 → 仍走既有 pointer 平移（pointer 事件
  // 在触屏上也会触发，故单指平移已由 onPointerDown/Move/endPan 覆盖，这里只接管双指缩放避免冲突）。
  // 退出放大/卸载即移除。canvas CSS 已 touch-action:none，配合 preventDefault 杜绝浏览器自身缩放/滚动。
  useEffect(() => {
    if (!maximized) return;
    const el = canvasRef.current;
    if (!el) return;
    let pinching = false;
    let lastDist = 0;
    const onStart = (e: TouchEvent) => {
      if (e.touches.length === 2) {
        pinching = true;
        lastDist = touchDistance(e.touches[0], e.touches[1]);
        e.preventDefault(); // 阻止浏览器双指缩放页面
      }
    };
    const onMove = (e: TouchEvent) => {
      if (!pinching || e.touches.length < 2) return;
      e.preventDefault();
      const dist = touchDistance(e.touches[0], e.touches[1]);
      const factor = pinchDistanceFactor(lastDist, dist);
      lastDist = dist;
      const rect = el.getBoundingClientRect();
      const mid = touchMidpoint(e.touches[0], e.touches[1]);
      setTransform((t) => zoomAtPoint(t, factor, mid.x - rect.left, mid.y - rect.top));
    };
    const onEnd = (e: TouchEvent) => {
      // 任一指抬起 / 不足两指即结束本轮捏合（剩余单指交还 pointer 平移）。
      if (e.touches.length < 2) {
        pinching = false;
        lastDist = 0;
      }
    };
    el.addEventListener("touchstart", onStart, { passive: false });
    el.addEventListener("touchmove", onMove, { passive: false });
    el.addEventListener("touchend", onEnd, { passive: false });
    el.addEventListener("touchcancel", onEnd, { passive: false });
    return () => {
      el.removeEventListener("touchstart", onStart);
      el.removeEventListener("touchmove", onMove);
      el.removeEventListener("touchend", onEnd);
      el.removeEventListener("touchcancel", onEnd);
    };
  }, [maximized]);

  // 拖拽平移整个画布（**仅放大态**）。指针按下记录起点，移动累加 delta。
  // 0.0.80：产物 iframe 不再 pointer-events:none（产物放大态仍可交互）→ 落在**产物上**的指针事件被产物消费、
  // 不再冒泡到此处，故此平移只作用于画布**暗区**（产物之外）；在产物上拖拽 = 与产物交互（如拖滑块），符合预期。
  // onBlank 只在 target===__canvas（真暗区）才 true；产物上点击根本不触发本处理器，故空白单击关闭不被误触。
  const onPointerDown = useCallback(
    (e: React.PointerEvent) => {
      if (!maximized) return;
      // onBlank：按下落在画布**空白暗区**（event.target 即 __canvas 容器本身）才算——落在 __stage/产物
      //（其后代节点）则非空白。单纯点击空白处（非拖拽）将关闭查看器（见 endPan）。
      const onBlank = e.target === canvasRef.current;
      panRef.current = {
        active: true,
        x: e.clientX,
        y: e.clientY,
        downX: e.clientX,
        downY: e.clientY,
        onBlank,
      };
      (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
    },
    [maximized]
  );
  const onPointerMove = useCallback((e: React.PointerEvent) => {
    const p = panRef.current;
    if (!p.active) return;
    const dx = e.clientX - p.x;
    const dy = e.clientY - p.y;
    p.x = e.clientX;
    p.y = e.clientY;
    setTransform((t) => panBy(t, dx, dy));
  }, []);
  const endPan = useCallback(
    (e: React.PointerEvent) => {
      const p = panRef.current;
      if (!p.active) return;
      p.active = false;
      (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
      // 修复 #4：放大态下，在画布**空白暗区**做单纯点击（按下到抬起几乎没移动＝非拖拽平移）→ 关闭查看器。
      // 点产物本身（onBlank=false）或拖拽平移（位移超阈值）都不关。
      if (e.type === "pointerup" && p.onBlank && isClickNotDrag(e.clientX - p.downX, e.clientY - p.downY)) {
        closeMaximized();
      }
    },
    [closeMaximized]
  );

  const onZoomBtn = useCallback((factor: number) => {
    // 按钮缩放：以画布中心为锚点（无光标）。
    const el = canvasRef.current;
    const cx = el ? el.clientWidth / 2 : 0;
    const cy = el ? el.clientHeight / 2 : 0;
    setTransform((t) => zoomAtPoint(t, factor, cx, cy));
  }, []);

  return (
    <div className="artifact-card">
      <div
        className="artifact-card__head"
        role="button"
        tabIndex={0}
        aria-expanded={expanded}
        onClick={() => setExpanded((v) => !v)}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            setExpanded((v) => !v);
          }
        }}
      >
        <LayoutGrid size={14} className="artifact-card__icon" aria-hidden="true" />
        <span className="artifact-card__label">互动卡片</span>
        {part.title && <span className="artifact-card__subtitle">{part.title}</span>}
        <span className="artifact-card__actions">
          {/* actions 按钮：阻止冒泡，避免点了就把卡片折叠了。 */}
          <button
            type="button"
            className="artifact-card__btn"
            title="复制为图片"
            aria-label="复制为图片"
            disabled={exporting}
            onClick={(e) => {
              e.stopPropagation();
              runExport("copy");
            }}
          >
            <Copy size={14} />
          </button>
          <button
            type="button"
            className="artifact-card__btn"
            title="下载为图片"
            aria-label="下载为图片"
            disabled={exporting}
            onClick={(e) => {
              e.stopPropagation();
              runExport("download");
            }}
          >
            <Download size={14} />
          </button>
          <button
            type="button"
            className="artifact-card__btn"
            title="放大"
            aria-label="放大"
            onClick={(e) => {
              e.stopPropagation();
              openMaximized();
            }}
          >
            <Maximize size={14} />
          </button>
          {/* 停靠到侧栏（0.0.75）：仅在父侧传入 onDock 时显（坞内复用本组件时不传 → 不显）。
              纯 UI 回调，把本产物拉到第三栏「产物」坞复用渲染，不触任何隔离逻辑。 */}
          {onDock && (
            <button
              type="button"
              className="artifact-card__btn"
              title="停靠到侧栏"
              aria-label="停靠到侧栏"
              onClick={(e) => {
                e.stopPropagation();
                onDock(part);
              }}
            >
              <PanelRight size={14} />
            </button>
          )}
          <span className="artifact-card__chevron" aria-hidden="true">
            {expanded ? <ChevronUp size={15} /> : <ChevronDown size={15} />}
          </span>
        </span>
      </div>

      {/* 放大态的暗背景（仅放大时渲染；点击空白处关闭）。它是 __body 的**兄弟**节点，
          不包裹 iframe，故出现/消失不影响 iframe 的 DOM 位置（iframe 永不重挂）。 */}
      {maximized && <div className="artifact-viewer__backdrop" onClick={closeMaximized} aria-hidden="true" />}

      {/* 关键不重挂保证：iframe 始终在**同一条 DOM 路径**上渲染
          （artifact-card__body → __canvas → __stage → iframe），无论内联还是放大。
          放大只切 __body 上的 class（→ position:fixed; inset:0; z-index 高），节点不动＝不 reload＝
          产物交互状态保留。pan/zoom 的 transform 应用在 __stage；非放大态 transform 恒为 identity、
          且滚轮（原生非被动监听，仅放大态绑定）/拖拽处理（onPointerDown 的 maximized 守卫）皆早退，故内联态不 pan/zoom。
          折叠用 display:none 隐藏整个 __body、**不卸载** iframe。 */}
      <div
        className={"artifact-card__body" + (maximized ? " artifact-card__body--maximized" : "")}
        style={{ display: expanded || maximized ? "block" : "none" }}
      >
        <div
          className={"artifact-viewer__canvas" + (maximized ? " is-maximized" : "")}
          ref={canvasRef}
          // 滚轮缩放走原生非被动监听（见上方 useEffect），此处不挂 React onWheel（被动、preventDefault 失效）。
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={endPan}
          onPointerLeave={endPan}
          onDoubleClick={maximized ? recenter : undefined}
        >
          <div
            className="artifact-viewer__stage"
            ref={stageRef}
            style={
              maximized
                ? { transform: `translate(${transform.tx}px, ${transform.ty}px) scale(${transform.scale})` }
                : undefined
            }
          >
            <iframe
              // code 变化时整体重挂（避免旧 srcdoc/监听残留）。key 只随 code 变，放大开关**不**改 key 也不换路径。
              key={part.code}
              ref={iframeRef}
              className="artifact-card__frame"
              title={part.title ?? "互动卡片"}
              sandbox={ARTIFACT_SANDBOX}
              srcDoc={srcdoc}
              scrolling="no"
              onLoad={() => {
                // 重挂后（code 变化）补推当前主题 + 视图模式：避免新 iframe 丢主题、或不知自己正处于放大态。
                const win = iframeRef.current?.contentWindow;
                if (!win) return;
                win.postMessage({ __mdgaArtifact: "theme", vars: readHostTheme() }, "*");
                win.postMessage({ __mdgaArtifact: "view-mode", maximized }, "*");
              }}
              style={{
                width: "100%",
                height,
                border: "1px solid var(--border)",
                borderRadius: 8,
                background: "transparent",
              }}
            />
          </div>
        </div>

        {/* 模态 chrome（关闭 / 缩放控件 / 提示）：仅放大态渲染，且是 iframe 的**兄弟**（不包裹它）。 */}
        {maximized && (
          <>
            <button
              type="button"
              className="artifact-viewer__close"
              title="关闭（Esc）"
              aria-label="关闭"
              onClick={closeMaximized}
            >
              <X size={18} />
            </button>
            <div className="artifact-viewer__controls" onWheel={(e) => e.stopPropagation()}>
              <button type="button" aria-label="缩小" onClick={() => onZoomBtn(1 / 1.2)}>
                <Minus size={15} />
              </button>
              <span className="artifact-viewer__pct">{Math.round(transform.scale * 100)}%</span>
              <button type="button" aria-label="放大" onClick={() => onZoomBtn(1.2)}>
                <Plus size={15} />
              </button>
            </div>
            <div className="artifact-viewer__hint">滚轮缩放（指向处放大）· 暗区拖拽平移 · 双击复位 · 内容可直接交互</div>
          </>
        )}
      </div>
    </div>
  );
}
