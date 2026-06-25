// 互动卡片画布的「内联 UI 运行时」(0.0.80)：一套**零第三方依赖、手写 vanilla** 的小组件库 + 极简
// 渲染核 + 声明式 spec 渲染器，作为字符串内联进 render_artifact 的沙箱 iframe（buildSrcdoc 注入在
// 导出 IIFE 之后、用户产物 code 之前）。
//
// 为什么自己手写而不引 Preact/Alpine：
//   · **零供应链轴**——不引任何外部库，库漏洞=0，无需 SCA/vendoring 跟踪；
//   · **无 build 步**——直接是字符串，随 vite 打进二进制；
//   · **CSP 安全**——全程不用 eval / new Function / 动态 import / fetch / storage（ARTIFACT_CSP 无
//     'unsafe-eval'，也禁外链/网络），只触 DOM，跑在 null 源沙箱里和现有桥接脚本同一条 'unsafe-inline' 放行路径。
//
// 设计原则：**每个控件「自带交互」**——原生表单控件(input/range/select/checkbox)本就可交互，
// 自管控件(Switch/Tabs)点击即自更新 DOM，**即便模型不写任何状态逻辑，控件也能动**（这正是弱模型可控产出
// 可交互产品设计稿的关键）。UI.mount/UI.store 提供可选的跨组件响应式（整树重渲，适合计数器/简单状态展示）。
//
// 主题：组件用宿主已注入的 CSS 变量(--brand/--color-text/--color-bg/--border/--on-brand 等)，亮/暗自动跟随。
//
// 安全红线（不可碰）：本运行时只挂 window.UI / window.h，**绝不**读 nonce（导出 IIFE 闭包局部）、绝不读
// 父 DOM/cookie/storage、绝不发网络。注入位置在导出 IIFE 之后、产物 code 之前，不改 CSP/sandbox/探针/桥接。

/* eslint-disable */
// 运行时源码（ES5 风格、单/双引号、**绝不含反引号或 ${} **，以便安全嵌进外层模板字符串）。
export const ARTIFACT_RUNTIME_JS: string = [
  "(function(){",
  "  'use strict';",
  "  if (window.UI) { return; }",
  "  function isEl(x){ return x && x.nodeType === 1; }",
  // h(tag, props, ...children): 建元素。props 支持 class/className、style(对象或字符串)、text、html(已信任的内联)、
  //   on{Event}/onClick 绑事件、其余作 attribute。children 可为元素/字符串/数组/null。
  "  function h(tag, props){",
  "    var node = document.createElement(tag);",
  "    props = props || {};",
  "    for (var k in props){ if(!Object.prototype.hasOwnProperty.call(props,k)) continue; var v = props[k];",
  "      if (v == null || v === false) continue;",
  "      if (k === 'class' || k === 'className'){ node.className = v; }",
  "      else if (k === 'text'){ node.textContent = String(v); }",
  // 不提供 `html` prop（innerHTML 未净化的内联属性）：声明式 spec 路径本就不经它,但移除后连产物 JS 也无此入口,
  // 缩小沙箱内 DOM-XSS 残留面（产物若要内联标记仍可直接写进 body,与运行时无关）。文本一律走 textContent。
  "      else if (k === 'value'){ node.value = v; }",
  "      else if (k === 'checked'){ node.checked = !!v; }",
  "      else if (k === 'style'){ if (typeof v === 'string'){ node.setAttribute('style', v); } else { for (var s in v){ if(Object.prototype.hasOwnProperty.call(v,s)){ try{ node.style[s] = v[s]; }catch(e){} } } } }",
  "      else if (k.indexOf('on') === 0 && typeof v === 'function'){ node.addEventListener(k.slice(2).toLowerCase(), v); }",
  "      else { try{ node.setAttribute(k, v === true ? '' : String(v)); }catch(e){} }",
  "    }",
  "    for (var i = 2; i < arguments.length; i++){ appendChild(node, arguments[i]); }",
  "    return node;",
  "  }",
  "  function appendChild(node, c){",
  "    if (c == null || c === false || c === true) return;",
  "    if (Array.isArray(c)){ for (var i=0;i<c.length;i++) appendChild(node, c[i]); return; }",
  "    if (isEl(c)){ node.appendChild(c); return; }",
  "    node.appendChild(document.createTextNode(String(c)));",
  "  }",
  // ── 响应式（可选）：mount(viewFn) 整树渲染，store.set 后重渲。组件本身自带交互，不依赖此机制。
  "  var _view = null, _root = null;",
  "  function mount(viewFn, root){ _view = viewFn; _root = root || document.body; rerender(); return _root; }",
  "  function rerender(){ if(!_view||!_root) return; try{ var n = _view(); _root.innerHTML=''; appendChild(_root, n); }catch(e){ try{ _root.textContent = 'render error: ' + (e&&e.message||e); }catch(_){} } }",
  "  function store(initial){ var s = {}; if(initial){ for(var k in initial){ if(Object.prototype.hasOwnProperty.call(initial,k)) s[k]=initial[k]; } }",
  "    return { get: function(k){ return s[k]; }, set: function(patch){ if(patch){ for(var k in patch){ if(Object.prototype.hasOwnProperty.call(patch,k)) s[k]=patch[k]; } } rerender(); }, state: s }; }",
  // ── 主题工具：拿宿主 CSS 变量，组件统一引用。
  "  var V = { brand:'var(--brand)', onBrand:'var(--on-brand)', text:'var(--color-text)', bg:'var(--color-bg)', text2:'var(--color-text-secondary)', text3:'var(--color-text-tertiary)', border:'var(--border)', ok:'var(--color-success)', danger:'var(--color-danger)' };",
  // ── 组件（每个都自带交互或基于原生可交互控件）。
  "  function Button(p){ p=p||{}; var v=p.variant||'default';",
  "    var bg = v==='primary'? V.brand : v==='danger'? V.danger : 'transparent';",
  "    var fg = (v==='primary'||v==='danger')? V.onBrand : V.text;",
  "    var bd = v==='primary'||v==='danger'? bg : V.border;",
  "    var st = 'appearance:none;cursor:pointer;font:inherit;font-size:13px;padding:7px 14px;border-radius:8px;border:1px solid '+bd+';background:'+bg+';color:'+fg+';transition:filter .12s,background .12s;';",
  "    var b = h('button', { type:'button', style: st + (v==='ghost'?'border-color:transparent;':''), onClick: p.onClick }, p.label || p.text || 'Button');",
  "    b.addEventListener('mouseenter', function(){ b.style.filter='brightness(.94)'; });",
  "    b.addEventListener('mouseleave', function(){ b.style.filter='none'; });",
  "    return b;",
  "  }",
  "  function Input(p){ p=p||{}; var st='font:inherit;font-size:13px;width:100%;box-sizing:border-box;padding:8px 10px;border:1px solid '+V.border+';border-radius:8px;background:'+V.bg+';color:'+V.text+';';",
  "    var n = h('input', { type: p.type||'text', value: p.value!=null?p.value:'', placeholder: p.placeholder||'', style: st });",
  "    if (p.onInput) n.addEventListener('input', function(e){ p.onInput(e.target.value, e); });",
  "    if (p.onChange) n.addEventListener('change', function(e){ p.onChange(e.target.value, e); });",
  "    n.addEventListener('focus', function(){ n.style.borderColor=V.brand; }); n.addEventListener('blur', function(){ n.style.borderColor=V.border; });",
  "    return n;",
  "  }",
  "  function Textarea(p){ p=p||{}; var st='font:inherit;font-size:13px;width:100%;box-sizing:border-box;padding:8px 10px;border:1px solid '+V.border+';border-radius:8px;background:'+V.bg+';color:'+V.text+';resize:vertical;';",
  "    var n = h('textarea', { rows: p.rows||3, placeholder: p.placeholder||'', style: st }, p.value!=null?String(p.value):'');",
  "    if (p.onInput) n.addEventListener('input', function(e){ p.onInput(e.target.value, e); });",
  "    return n;",
  "  }",
  "  function Select(p){ p=p||{}; var st='font:inherit;font-size:13px;width:100%;box-sizing:border-box;padding:8px 10px;border:1px solid '+V.border+';border-radius:8px;background:'+V.bg+';color:'+V.text+';cursor:pointer;';",
  "    var opts = (p.options||[]).map(function(o){ var ov = (o&&typeof o==='object')? o.value : o; var ol = (o&&typeof o==='object')? o.label : o; return h('option', { value: ov, selected: String(ov)===String(p.value) }, ol); });",
  "    var n = h('select', { style: st }, opts);",
  "    if (p.onChange) n.addEventListener('change', function(e){ p.onChange(e.target.value, e); });",
  "    return n;",
  "  }",
  "  function Checkbox(p){ p=p||{}; var box=h('input',{ type:'checkbox', checked:!!p.checked, style:'width:16px;height:16px;accent-color:var(--brand);cursor:pointer;' });",
  "    if (p.onChange) box.addEventListener('change', function(e){ p.onChange(e.target.checked, e); });",
  "    return h('label',{ style:'display:inline-flex;align-items:center;gap:8px;cursor:pointer;color:'+V.text+';font-size:13px;' }, box, p.label!=null? h('span',{},p.label):null);",
  "  }",
  // Switch：自管翻转（点击即更新自身 DOM），并回调 onChange——不写状态也能动。
  "  function Switch(p){ p=p||{}; var on = !!p.checked;",
  "    var knob = h('span', { style:'position:absolute;top:2px;left:2px;width:16px;height:16px;border-radius:50%;background:#fff;transition:transform .15s;box-shadow:0 1px 2px rgba(0,0,0,.3);' });",
  "    var track = h('span', { style:'position:relative;display:inline-block;width:38px;height:20px;border-radius:999px;transition:background .15s;background:'+(on?V.brand:V.border)+';' }, knob);",
  "    function paint(){ track.style.background = on? V.brand : V.border; knob.style.transform = on? 'translateX(18px)' : 'translateX(0)'; }",
  "    paint();",
  "    var wrap = h('label', { style:'display:inline-flex;align-items:center;gap:8px;cursor:pointer;color:'+V.text+';font-size:13px;', onClick:function(){ on=!on; paint(); if(p.onChange) p.onChange(on); } }, track, p.label!=null? h('span',{},p.label):null);",
  "    return wrap;",
  "  }",
  // Slider：原生 range，本就可拖；带可选值显示。
  "  function Slider(p){ p=p||{}; var min=p.min!=null?p.min:0, max=p.max!=null?p.max:100, val=p.value!=null?p.value:Math.round((min+max)/2);",
  "    var out = h('span', { style:'font-size:12px;color:'+V.text2+';min-width:34px;text-align:right;' }, String(val));",
  "    var range = h('input', { type:'range', min:min, max:max, step:p.step||1, value:val, style:'flex:1;accent-color:var(--brand);cursor:pointer;' });",
  "    range.addEventListener('input', function(e){ out.textContent = e.target.value; if(p.onInput) p.onInput(Number(e.target.value), e); });",
  "    var rowEl = h('div', { style:'display:flex;align-items:center;gap:10px;width:100%;' }, range, out);",
  "    return p.label!=null? Field({ label:p.label, control: rowEl }) : rowEl;",
  "  }",
  // Tabs：点击切换内容，自管激活态——不写状态也能切。
  "  function Tabs(p){ p=p||{}; var tabs=p.tabs||[]; var active = p.active!=null? p.active : 0;",
  "    var bodyEl = h('div', { style:'padding-top:10px;' });",
  "    var btns = [];",
  "    function paint(){ for(var i=0;i<btns.length;i++){ var on=(i===active); btns[i].style.color = on? V.text : V.text2; btns[i].style.borderBottom = '2px solid '+(on? V.brand : 'transparent'); btns[i].style.fontWeight = on? '600':'400'; } bodyEl.innerHTML=''; var c=tabs[active]&&tabs[active].content; appendChild(bodyEl, c!=null? c : ''); }",
  "    var bar = h('div', { style:'display:flex;gap:4px;border-bottom:1px solid '+V.border+';' }, tabs.map(function(t,i){ var b=h('button',{ type:'button', style:'appearance:none;background:none;cursor:pointer;font:inherit;font-size:13px;padding:7px 12px;border:none;color:'+V.text2+';', onClick:function(){ active=i; paint(); if(p.onChange) p.onChange(i); } }, t.label||('Tab '+(i+1))); btns.push(b); return b; }));",
  "    var wrap = h('div', {}, bar, bodyEl); paint(); return wrap;",
  "  }",
  "  function Card(p){ p=p||{}; var inner=[]; if(p.title!=null) inner.push(h('div',{ style:'font-weight:600;font-size:14px;color:'+V.text+';margin-bottom:8px;' }, p.title)); var children = arguments.length>1? Array.prototype.slice.call(arguments,1) : (p.children||[]); inner.push(h('div',{}, children));",
  "    return h('div', { style:'border:1px solid '+V.border+';border-radius:12px;padding:14px;background:'+V.bg+';' }, inner);",
  "  }",
  "  function Field(p){ p=p||{}; var rows=[]; if(p.label!=null) rows.push(h('label',{ style:'font-size:12px;color:'+V.text2+';display:block;margin-bottom:5px;' }, p.label)); rows.push(p.control||(p.children?h('div',{},p.children):null)); if(p.hint!=null) rows.push(h('div',{ style:'font-size:11px;color:'+V.text3+';margin-top:4px;' }, p.hint));",
  "    return h('div', { style:'margin-bottom:12px;' }, rows);",
  "  }",
  "  function Badge(p){ p=p||{}; var tone=p.tone||'default'; var c = tone==='success'? V.ok : tone==='danger'? V.danger : tone==='brand'? V.brand : V.text2;",
  "    return h('span', { style:'display:inline-block;font-size:11px;padding:2px 8px;border-radius:999px;border:1px solid '+c+';color:'+c+';' }, p.text||p.label||'');",
  "  }",
  "  function Heading(p){ p=p||{}; var lv=p.level||2; var sz = lv<=1?'20px':lv===2?'16px':'14px'; return h('div', { style:'font-weight:600;font-size:'+sz+';color:'+V.text+';margin:0 0 6px;' }, p.text||''); }",
  "  function Text(p){ p=p||{}; return h('div', { style:'font-size:13px;line-height:1.5;color:'+(p.muted?V.text2:V.text)+';' }, p.text||''); }",
  "  function Progress(p){ p=p||{}; var max=p.max||100; var pct=Math.max(0,Math.min(100, (Number(p.value)||0)/max*100)); return h('div',{ style:'width:100%;height:8px;border-radius:999px;background:'+V.border+';overflow:hidden;' }, h('div',{ style:'height:100%;width:'+pct+'%;background:'+V.brand+';transition:width .2s;' })); }",
  "  function Divider(){ return h('div', { style:'height:1px;background:'+V.border+';margin:12px 0;' }); }",
  "  function Stack(p){ var args=Array.prototype.slice.call(arguments); var props={}; var kids=args; if(args[0]&&!isEl(args[0])&&!Array.isArray(args[0])&&typeof args[0]==='object'){ props=args[0]; kids=args.slice(1); } var gap=props.gap!=null?props.gap:10; return h('div',{ style:'display:flex;flex-direction:column;gap:'+gap+'px;' }, kids.length?kids:(props.children||[])); }",
  "  function Row(){ var args=Array.prototype.slice.call(arguments); var props={}; var kids=args; if(args[0]&&!isEl(args[0])&&!Array.isArray(args[0])&&typeof args[0]==='object'){ props=args[0]; kids=args.slice(1); } var gap=props.gap!=null?props.gap:10; return h('div',{ style:'display:flex;flex-direction:row;align-items:'+(props.align||'center')+';gap:'+gap+'px;flex-wrap:wrap;' }, kids.length?kids:(props.children||[])); }",
  "  function Col(){ return Stack.apply(null, arguments); }",
  // ── 声明式 spec 渲染：node = {type, ...props, children:[...]} 或字符串。模型可只输出一段 JSON。
  "  var REG = { button:Button, input:Input, textarea:Textarea, select:Select, checkbox:Checkbox, 'switch':Switch, toggle:Switch, slider:Slider, tabs:Tabs, card:Card, field:Field, badge:Badge, heading:Heading, title:Heading, text:Text, progress:Progress, divider:Divider, stack:Stack, col:Stack, row:Row };",
  "  function buildNode(node){",
  "    if (node == null) return null;",
  "    if (typeof node === 'string' || typeof node === 'number') return document.createTextNode(String(node));",
  "    if (Array.isArray(node)) return node.map(buildNode);",
  "    if (isEl(node)) return node;",
  "    var t = String(node.type||'').toLowerCase();",
  "    var kids = node.children ? buildNode(node.children) : null;",
  "    var fn = REG[t];",
  "    if (!fn) { return h('div', {}, kids); }",
  "    if (t==='card'){ return Card(node, kids); }",
  "    if (t==='tabs'){ var tabs=(node.tabs||[]).map(function(tb){ return { label: tb.label, content: tb.content!=null? buildNode(tb.content): (tb.children?buildNode(tb.children):null) }; }); return Tabs({ tabs:tabs, active:node.active, onChange:node.onChange }); }",
  "    if (t==='field'){ var ctl = node.control!=null? buildNode(node.control) : (kids||null); return Field({ label:node.label, hint:node.hint, control: ctl }); }",
  "    if (t==='stack'||t==='col'||t==='row'){ var arr = kids? (Array.isArray(kids)?kids:[kids]) : []; return fn.apply(null, [{ gap:node.gap, align:node.align }].concat(arr)); }",
  "    return fn(node);",
  "  }",
  // mountSpec：把一个 spec(节点树 或 {title, body/children})渲染进 body。
  "  function mountSpec(spec, root){ root = root || document.body;",
  "    try{ if (typeof spec === 'string'){ spec = JSON.parse(spec); } }catch(e){ root.textContent='spec parse error: '+(e&&e.message||e); return; }",
  "    var top = spec && (spec.children!=null||spec.body!=null||spec.type) ? spec : { type:'stack', children: spec };",
  "    var pad = h('div', { style:'padding:14px;color:'+V.text+';' });",
  "    if (top.title!=null) appendChild(pad, Heading({ text: top.title, level:1 }));",
  "    var content = top.body!=null? top.body : (top.type? top : { type:'stack', children: top.children });",
  "    var node = buildNode(content);",
  "    appendChild(pad, node);",
  "    root.innerHTML=''; appendChild(root, pad);",
  "  }",
  "  window.h = h;",
  "  window.UI = { h:h, mount:mount, rerender:rerender, store:store, mountSpec:mountSpec, theme:V,",
  "    Button:Button, Input:Input, Textarea:Textarea, Select:Select, Checkbox:Checkbox, Switch:Switch, Slider:Slider, Tabs:Tabs, Card:Card, Field:Field, Badge:Badge, Heading:Heading, Text:Text, Progress:Progress, Divider:Divider, Stack:Stack, Row:Row, Col:Col };",
  "})();",
].join("\n");

/** 判断一段产物 code 是否是「纯 JSON 声明式 spec」(去空白后以 `{` 开头且能解析成对象)。
 *  是 → buildSrcdoc 用 UI.mountSpec 渲染;否 → 当作 HTML/SVG/JS 原样插入。
 *  HTML/SVG 几乎不会以裸 `{` 开头,故此判定安全、无误伤。 */
export function isDeclarativeSpec(code: string): boolean {
  const t = (code || "").trim();
  if (t.charCodeAt(0) !== 0x7b /* { */) return false;
  try {
    const v = JSON.parse(t);
    return !!v && typeof v === "object" && !Array.isArray(v);
  } catch {
    return false;
  }
}
