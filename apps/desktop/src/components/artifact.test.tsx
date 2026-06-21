import { describe, it, expect } from "vitest";
import {
  buildSrcdoc,
  buildExportScript,
  clampArtifactHeight,
  ARTIFACT_MIN_HEIGHT,
  ARTIFACT_MAX_HEIGHT,
  ARTIFACT_CSP,
  ARTIFACT_SANDBOX,
  ARTIFACT_EXPORT_MAX_BYTES,
  clampScale,
  zoomAtPoint,
  panBy,
  IDENTITY_TRANSFORM,
  ARTIFACT_MIN_SCALE,
  ARTIFACT_MAX_SCALE,
  checkExportImage,
  generateExportNonce,
  sanitizeFilename,
  isClickNotDrag,
  ARTIFACT_CLICK_SLOP,
  pinchWheelFactor,
  ARTIFACT_PINCH_WHEEL_K,
  ARTIFACT_PINCH_FACTOR_MIN,
  ARTIFACT_PINCH_FACTOR_MAX,
  touchDistance,
  touchMidpoint,
  pinchDistanceFactor,
} from "./artifact";

// 0.0.70 安全 wrapper 冒烟:锁住 buildSrcdoc 的隔离不变量,防有人改回 CDN / 删 CSP / 加 unsafe-eval。
// (注:jsdom 不真执行 sandbox iframe 脚本,故这里只做结构断言;真渲染冒烟靠本机半自动。)
describe("artifact buildSrcdoc 安全 wrapper", () => {
  const out = buildSrcdoc("<p id='ok'>RENDER_OK_SMOKE</p>", { "--color-text": "#000" }, "TESTNONCE");

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

  it("sandbox 仅 allow-scripts:srcdoc 不带 allow-same-origin（沙箱逃逸红线）", () => {
    // 渲染侧 iframe 的 sandbox 属性在组件 JSX 内固定为 allow-scripts；srcdoc 本身不引入同源标记。
    expect(out).not.toContain("allow-same-origin");
  });

  it("结构合法:单一 CSP meta、doctype/html/body 闭合", () => {
    expect(out.startsWith("<!doctype html>")).toBe(true);
    expect(out).toContain("</body></html>");
    expect((out.match(/Content-Security-Policy/g) || []).length).toBe(1);
  });

  it("主题桥(修复 #6):注入只认 __mdgaArtifact:'theme' 的 message 监听、改写 :root CSS 变量,且不削弱安全", () => {
    // 父→子 postMessage theme 桥:iframe 内监听只处理本类型消息,setProperty 写 CSS 变量。
    expect(out).toContain("'theme'");
    expect(out).toContain("setProperty");
    expect(out).toContain("addEventListener('message'");
    // 桥纯 CSS 变量更新,不得引入网络/外联/eval,也不得削弱 CSP/sandbox。
    expect(out).toContain("connect-src 'none'");
    expect(out).not.toContain("unsafe-eval");
    expect(out).not.toContain("allow-same-origin");
  });
});

// 0.0.74 高度全自适应:clamp 上限仅防浏览器被超大自报值撑爆,下限防空卡,正常内容永不触上限。
describe("clampArtifactHeight 高度钳制", () => {
  it("正常内容高原样返回（不触上限,故永不出滚动条）", () => {
    expect(clampArtifactHeight(120)).toBe(120);
    expect(clampArtifactHeight(8000)).toBe(8000); // 远超旧 2000 上限,现照样放行
  });
  it("低于下限抬到下限（防空卡）", () => {
    expect(clampArtifactHeight(0)).toBe(ARTIFACT_MIN_HEIGHT);
    expect(clampArtifactHeight(10)).toBe(ARTIFACT_MIN_HEIGHT);
  });
  it("恶意超大自报值钳到上限（防撑爆浏览器）", () => {
    expect(clampArtifactHeight(1e9)).toBe(ARTIFACT_MAX_HEIGHT);
  });
  it("非有限值（NaN/Infinity）回退到下限", () => {
    expect(clampArtifactHeight(NaN)).toBe(ARTIFACT_MIN_HEIGHT);
    expect(clampArtifactHeight(Infinity)).toBe(ARTIFACT_MIN_HEIGHT);
  });
});

// 0.0.74 第二步：dev CSP 可观测——仅 dev 构建注入 securitypolicyviolation 监听并回传父侧。
describe("buildSrcdoc dev CSP 可观测", () => {
  it("默认（生产）不注入 securitypolicyviolation 监听、不回传 csp-violation", () => {
    const prod = buildSrcdoc("<p>x</p>", { "--color-text": "#000" }, "N");
    expect(prod).not.toContain("securitypolicyviolation");
    expect(prod).not.toContain("csp-violation");
  });
  it("dev=true 注入监听并经 postMessage('csp-violation') 回传", () => {
    const dev = buildSrcdoc("<p>x</p>", { "--color-text": "#000" }, "N", true);
    expect(dev).toContain("securitypolicyviolation");
    expect(dev).toContain("csp-violation");
    // dev 注入不得削弱安全 wrapper：CSP / 探针 / 无 same-origin 依旧。
    expect(dev).toContain("default-src 'none'");
    expect(dev).toContain("get_app_info");
    expect(dev).not.toContain("allow-same-origin");
    expect(dev).not.toContain("unsafe-eval");
  });
});

// 0.0.74 返工：导出改为**展示 iframe 自身**用 foreignObject 导出（弃 html2canvas / 截图 iframe）。
// 锁住：① html2canvas 彻底不在场（依赖移除）；② 导出 IIFE 在产物 code 之前注入、nonce 烤进闭包局部、
// 产物 code 之前看不到该 nonce；③ SVG/HTML 分支与 foreignObject 包装在场；④ 零外链（全 data:）。
describe("buildExportScript / buildSrcdoc 导出注入（foreignObject）", () => {
  const theme = { "--color-text": "#000" };

  it("html2canvas 彻底不在场（依赖移除、不再内联任何 ?raw 库）", () => {
    const out = buildSrcdoc("<div>X</div>", theme, "NONCE_X");
    expect(out).not.toContain("html2canvas");
    expect(out).not.toContain("?raw");
  });

  it("导出 IIFE：在产物 code 之前注入，nonce 烤进闭包、走 foreignObject → image/png → export-image，零外链", () => {
    const code = "<div id='a'>PROD_CODE</div>";
    const out = buildSrcdoc(code, theme, "BAKED_NONCE_42");
    // nonce 以字面量烤进脚本（JSON.stringify 转义）。
    expect(out).toContain('"BAKED_NONCE_42"');
    // 导出脚本注入在用户产物 code **之前**（闭包局部对后跑的产物不可见 → #3 防伪造）。
    expect(out.indexOf('"BAKED_NONCE_42"')).toBeLessThan(out.indexOf("PROD_CODE"));
    // 仅响应父侧 export-request；回传 export-image 带 token；走 foreignObject + image/png；
    expect(out).toContain("export-request");
    expect(out).toContain("export-image");
    expect(out).toContain("token:NONCE");
    expect(out).toContain("foreignObject");
    expect(out).toContain("'image/png'");
    expect(out).toContain("XMLSerializer"); // SVG 分支
    // 全 data:（svg+xml），不触网/不外链。
    expect(out).toContain("data:image/svg+xml");
    expect(out).not.toContain("createObjectURL"); // 导出不走 blob: URL，纯 data:
  });

  it("nonce 经 JSON.stringify 转义烤入：含引号的 nonce 不破坏脚本/不逃逸字符串", () => {
    const out = buildExportScript(JSON.stringify('a"b'));
    // 含转义后的引号，不会出现裸 `"a"b"` 这种破坏字符串字面量的形态。
    expect(out).toContain('var NONCE="a\\"b";');
  });

  it("sandbox/CSP 红线在展示 iframe 上未变（同款常量、不带 same-origin）", () => {
    expect(ARTIFACT_SANDBOX).toBe("allow-scripts");
    expect(ARTIFACT_SANDBOX).not.toContain("allow-same-origin");
    const out = buildSrcdoc("<div>X</div>", theme, "N");
    expect(out).toContain(`content="${ARTIFACT_CSP}"`);
    expect(out).not.toContain("allow-same-origin");
    expect(out).not.toContain("unsafe-eval");
  });
});

// 0.0.74 第二步：放大查看器纯变换逻辑（缩放 clamp / 以光标为中心缩放 / 平移）。
describe("放大查看器变换计算", () => {
  it("clampScale 钳到 [min,max]，非有限回退 1", () => {
    expect(clampScale(1)).toBe(1);
    expect(clampScale(0.01)).toBe(ARTIFACT_MIN_SCALE);
    expect(clampScale(999)).toBe(ARTIFACT_MAX_SCALE);
    expect(clampScale(NaN)).toBe(1);
  });

  it("zoomAtPoint：光标下的点缩放前后落在同一屏幕位置（中心不动）", () => {
    // 起始 identity，放大 2x，光标在 (100,50)。光标处内容点缩放后仍应映射到 (100,50)。
    const t0 = IDENTITY_TRANSFORM;
    const t1 = zoomAtPoint(t0, 2, 100, 50);
    expect(t1.scale).toBeCloseTo(2);
    // 内容点 content = (px - tx)/scale；屏幕点 = tx + scale*content，应 == px。
    const screenX = t1.tx + t1.scale * ((100 - t0.tx) / t0.scale);
    const screenY = t1.ty + t1.scale * ((50 - t0.ty) / t0.scale);
    expect(screenX).toBeCloseTo(100);
    expect(screenY).toBeCloseTo(50);
  });

  it("zoomAtPoint：触到 scale 上限后不再漂移（eff 用真实生效倍率换算）", () => {
    // 已在上限附近，再放大应停在上限、且锚点仍稳定。
    const t0 = { scale: ARTIFACT_MAX_SCALE, tx: 0, ty: 0 };
    const t1 = zoomAtPoint(t0, 4, 200, 200);
    expect(t1.scale).toBe(ARTIFACT_MAX_SCALE);
    // scale 没变 → 平移不应变（eff=1）。
    expect(t1.tx).toBeCloseTo(0);
    expect(t1.ty).toBeCloseTo(0);
  });

  it("panBy：叠加位移、scale 不变", () => {
    const t = panBy({ scale: 2, tx: 10, ty: 20 }, 5, -8);
    expect(t).toEqual({ scale: 2, tx: 15, ty: 12 });
  });
});

// 0.0.74 第三步（修复 #4）：放大态点画布空白处关闭——「点击 vs 拖拽平移」判定阈值。
describe("isClickNotDrag 点击/拖拽判定", () => {
  it("位移在阈值内＝单纯点击（可关查看器）", () => {
    expect(isClickNotDrag(0, 0)).toBe(true);
    expect(isClickNotDrag(ARTIFACT_CLICK_SLOP, 0)).toBe(true);
    expect(isClickNotDrag(0, -ARTIFACT_CLICK_SLOP)).toBe(true);
    expect(isClickNotDrag(3, -2)).toBe(true);
  });
  it("位移超阈值＝拖拽平移（不关）", () => {
    expect(isClickNotDrag(ARTIFACT_CLICK_SLOP + 1, 0)).toBe(false);
    expect(isClickNotDrag(0, ARTIFACT_CLICK_SLOP + 1)).toBe(false);
    expect(isClickNotDrag(20, 20)).toBe(false);
  });
  it("自定义阈值生效", () => {
    expect(isClickNotDrag(8, 8, 10)).toBe(true);
    expect(isClickNotDrag(8, 8, 5)).toBe(false);
  });
});

// 0.0.75 触控板/触屏捏合缩放：ctrl+wheel 平滑因子 + 双指距离→factor + 两触点距离/中点（纯逻辑）。
// 注：gesture/touch 的真实捏合需真机（WebView2/Windows 触控板 + macOS 触控板/触屏），jsdom 测不了端到端，
// 这里只测能抽的纯计算。
describe("pinchWheelFactor ctrl+wheel 平滑因子（Chromium/WebView2 触控板捏合）", () => {
  it("deltaY<0（捏开）→ factor>1 放大；deltaY>0（捏合）→ factor<1 缩小", () => {
    expect(pinchWheelFactor(-10)).toBeGreaterThan(1);
    expect(pinchWheelFactor(10)).toBeLessThan(1);
  });
  it("deltaY=0 → factor=1（不缩放）", () => {
    expect(pinchWheelFactor(0)).toBeCloseTo(1);
  });
  it("等于 exp(-deltaY*k)（在夹取区间内时）", () => {
    // 小 delta 不触发夹取：直接等于 exp 公式。
    expect(pinchWheelFactor(-20, ARTIFACT_PINCH_WHEEL_K)).toBeCloseTo(Math.exp(20 * ARTIFACT_PINCH_WHEEL_K));
    expect(pinchWheelFactor(30, ARTIFACT_PINCH_WHEEL_K)).toBeCloseTo(Math.exp(-30 * ARTIFACT_PINCH_WHEEL_K));
  });
  it("极端 deltaY 被夹在 [MIN,MAX]（防一帧爆缩/爆放）", () => {
    expect(pinchWheelFactor(-100000)).toBe(ARTIFACT_PINCH_FACTOR_MAX);
    expect(pinchWheelFactor(100000)).toBe(ARTIFACT_PINCH_FACTOR_MIN);
  });
  it("非有限 deltaY（NaN / ±Infinity）回退 1（无效输入不缩放）", () => {
    expect(pinchWheelFactor(NaN)).toBe(1);
    expect(pinchWheelFactor(Infinity)).toBe(1);
    expect(pinchWheelFactor(-Infinity)).toBe(1);
  });
});

describe("touchDistance / touchMidpoint / pinchDistanceFactor 双指捏合纯逻辑", () => {
  it("touchDistance：欧氏距离", () => {
    expect(touchDistance({ clientX: 0, clientY: 0 }, { clientX: 3, clientY: 4 })).toBeCloseTo(5);
    expect(touchDistance({ clientX: 10, clientY: 10 }, { clientX: 10, clientY: 10 })).toBeCloseTo(0);
  });
  it("touchMidpoint：两触点中点", () => {
    expect(touchMidpoint({ clientX: 0, clientY: 0 }, { clientX: 10, clientY: 20 })).toEqual({ x: 5, y: 10 });
  });
  it("pinchDistanceFactor：当前距/上次距", () => {
    expect(pinchDistanceFactor(100, 200)).toBeCloseTo(2); // 双指张开一倍 → 放大 2x
    expect(pinchDistanceFactor(200, 100)).toBeCloseTo(0.5); // 收拢一半 → 缩小
  });
  it("pinchDistanceFactor：上次为 0 / 非法距离 回退 1（不缩放）", () => {
    expect(pinchDistanceFactor(0, 100)).toBe(1);
    expect(pinchDistanceFactor(100, 0)).toBe(1);
    expect(pinchDistanceFactor(NaN, 100)).toBe(1);
    expect(pinchDistanceFactor(100, Infinity)).toBe(1);
  });
});

// 0.0.74 返工：导出（export-image）消息校验——nonce(#3) + 类型(image/png dataURL) + 大小(≤上限)。
describe("checkExportImage 导出消息校验", () => {
  const pngUrl = (b64: string) => "data:image/png;base64," + b64;
  const TOK = "GOOD_TOKEN";
  const withTok = (extra: Record<string, unknown>) => ({ token: TOK, ...extra });

  it("合法 token + image/png dataURL 通过", () => {
    const r = checkExportImage(withTok({ dataUrl: pngUrl("AAAA") }), TOK);
    expect(r.ok).toBe(true);
    if (r.ok) expect(r.dataUrl).toContain("image/png");
  });

  it("#3 nonce 校验：token 缺失 / 不匹配 / expectedToken 为空 一律拒（产物无法伪造）", () => {
    // token 缺失
    expect(checkExportImage({ dataUrl: pngUrl("AAAA") }, TOK).ok).toBe(false);
    // token 不匹配（伪造）
    const bad = checkExportImage({ token: "WRONG", dataUrl: pngUrl("AAAA") }, TOK);
    expect(bad.ok).toBe(false);
    if (!bad.ok) expect(bad.reason).toBe("bad token");
    // expectedToken 为空（iframe 未就绪）：即便对方给了 token 也拒
    expect(checkExportImage({ token: "", dataUrl: pngUrl("AAAA") }, "").ok).toBe(false);
    // token 非字符串
    expect(checkExportImage({ token: 123 as unknown as string, dataUrl: pngUrl("AAAA") }, TOK).ok).toBe(false);
    // nonce 校验先于 error 分支：错 token 的 error 回传也判 bad token（而非 render-error）
    const e = checkExportImage({ token: "WRONG", error: "boom" }, TOK);
    expect(e.ok).toBe(false);
    if (!e.ok) expect(e.reason).toBe("bad token");
  });

  it("拒非 png dataURL（如 jpeg / svg / 外部 url）", () => {
    expect(checkExportImage(withTok({ dataUrl: "data:image/jpeg;base64,AAAA" }), TOK).ok).toBe(false);
    expect(checkExportImage(withTok({ dataUrl: "data:image/svg+xml;base64,AAAA" }), TOK).ok).toBe(false);
    expect(checkExportImage(withTok({ dataUrl: "https://evil.tld/x.png" }), TOK).ok).toBe(false);
  });

  it("拒缺 dataUrl / 空 payload", () => {
    expect(checkExportImage(withTok({}), TOK).ok).toBe(false);
    expect(checkExportImage(withTok({ dataUrl: pngUrl("") }), TOK).ok).toBe(false);
  });

  it("拒超大（> 上限）", () => {
    // 默认上限为 12MB。
    expect(ARTIFACT_EXPORT_MAX_BYTES).toBe(12 * 1024 * 1024);
    // 自定义更小上限：base64 长 2000 → 近似 1500 字节 > 100，应被拦。
    const r = checkExportImage(withTok({ dataUrl: pngUrl("A".repeat(2000)) }), TOK, 100);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toBe("too large");
    // 同样 payload 在默认大上限下通过。
    expect(checkExportImage(withTok({ dataUrl: pngUrl("A".repeat(2000)) }), TOK).ok).toBe(true);
  });

  it("iframe 回传 error（且 token 合法）时判失败并带原因", () => {
    const r = checkExportImage(withTok({ error: "foreignObject taint" }), TOK);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toContain("foreignObject taint");
  });
});

// 0.0.74 返工：导出 nonce 生成 + 下载文件名清洗（#9）。
describe("generateExportNonce", () => {
  it("生成非空、每次不同的 nonce", () => {
    const a = generateExportNonce();
    const b = generateExportNonce();
    expect(typeof a).toBe("string");
    expect(a.length).toBeGreaterThan(0);
    expect(a).not.toBe(b);
  });
});

describe("sanitizeFilename 下载文件名清洗（#9）", () => {
  it("普通标题原样保留", () => {
    expect(sanitizeFilename("我的图表 2026")).toBe("我的图表 2026");
  });
  it("非法/路径/控制字符替换为 -（普通空格保留、连续空白折叠为一）", () => {
    expect(sanitizeFilename('a/b\\c:d*e?f"g<h>i|j')).toBe("a-b-c-d-e-f-g-h-i-j");
    expect(sanitizeFilename("x\ty")).toBe("x-y");
    expect(sanitizeFilename("a   b")).toBe("a b");
  });
  it("空 / undefined / 全空白 回退 artifact", () => {
    expect(sanitizeFilename("")).toBe("artifact");
    expect(sanitizeFilename(undefined)).toBe("artifact");
    expect(sanitizeFilename("   ")).toBe("artifact");
    expect(sanitizeFilename("...")).toBe("artifact");
  });
  it("首尾点/空白裁掉", () => {
    expect(sanitizeFilename("  hello  ")).toBe("hello");
    expect(sanitizeFilename(".hidden.")).toBe("hidden");
  });
});
