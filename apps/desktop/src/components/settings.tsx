// 设置弹窗与模型供应商卡片（0.0.37 从 App.tsx 抽出，纯搬移，无逻辑改动）。

import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronRight, Eye, EyeOff, Plug, Lock } from "lucide-react";
import { getPermissionModeLabel, type PermissionMode } from "@mdga/ui";
import {
  PERMISSION_MODES,
  PROVIDER_PRESETS,
  VISION_PRESET_MODEL,
  PRESET_CONTEXT_WINDOW,
  ROUTING_ROLES,
  type ProviderConfig,
  type AppInfo,
  type BalanceState,
  type McpServer,
  type DeniedAction,
  type SettingsSection,
  type LspKnownServer,
  type LspServerConfig,
  type LspServerSetting,
  type RoleRouting,
} from "../types";
import { humanizeError } from "../utils";
import { useFocusTrap } from "./dialogs";

// ── ProviderCard（Plan17 §6.2）───────────────────────────────────────────────

/**
 * 可复用的模型供应商卡片：主模型 role="main"、视觉 role="vision" 各用一次。
 * 两栏网格表单（供应商|模型 一行，API Key 整行，高级折叠行整行），右上角状态徽标，底部保存按钮右对齐。
 * 挂载时回填 get_model_provider_config；保存调 save_model_provider；不回显明文 key。
 */
export function ProviderCard({
  role,
  title,
  onSaved,
}: {
  role: "main" | "vision";
  title: string;
  onSaved?: () => void;
}) {
  const presetModel = role === "vision" ? VISION_PRESET_MODEL : null;
  const [preset, setPreset] = useState<string>("deepseek");
  const [apiKey, setApiKey] = useState("");
  const [modelId, setModelId] = useState(
    role === "vision" ? VISION_PRESET_MODEL.deepseek : "deepseek-v4-pro",
  );
  const [baseUrl, setBaseUrl] = useState("");
  // 上下文窗口（tokens，Plan27 #2）：可选数字输入。空串=不传（后端回退默认）。预设可预填。
  const [contextWindow, setContextWindow] = useState<string>(
    role === "main" ? String(PRESET_CONTEXT_WINDOW.deepseek ?? "") : "",
  );
  // API 格式（Plan18 §4）：仅视觉 provider 暴露选择（openai|anthropic）；主模型恒 openai。
  const [apiFormat, setApiFormat] = useState<"openai" | "anthropic">("openai");
  const [showKey, setShowKey] = useState(false);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  // 已配置：挂载回填到 true，或保存成功后置 true。controls 状态徽标 + key 占位文案。
  const [configured, setConfigured] = useState(false);
  // 已配置但用户尚未在本次会话改动 key 时，占位显示「已配置 ••••」而非空密码框。
  const [keyTouched, setKeyTouched] = useState(false);
  const [saving, setSaving] = useState(false);
  // F2 保存成功反馈：置 true 后数秒自动复位，按钮区显示「已保存 ✓」。
  const [saved, setSaved] = useState(false);
  // 保存成功提示的自动复位定时器；再次保存或组件卸载时清理，避免闭包泄漏。
  const savedTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimer.current) clearTimeout(savedTimer.current); }, []);
  const [error, setError] = useState<string | null>(null);
  // 测试连接（Plan19 P2a）：就地显示结果，不跳主界面。null=未测、ok=成功（绿）、err=失败（红）。
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ ok: boolean; message: string } | null>(null);
  // C-1 工具调用冒烟（Plan25 #3）：就地显示该供应商工具调用兼容性。
  // null=未测；kind="ok" 绿（可用）、"weak" 橙（模型未返回工具调用，可能不支持/较弱）、"error" 红（报错）。
  const [smokeTesting, setSmokeTesting] = useState(false);
  const [smokeResult, setSmokeResult] = useState<{ kind: "ok" | "weak" | "error"; message: string } | null>(null);

  const presetMeta = PROVIDER_PRESETS.find((p) => p.id === preset) ?? PROVIDER_PRESETS[0];
  const isCustom = preset === "custom";

  // 挂载时回填已存配置（apiKey 脱敏为空：configured=true 但不回显明文）。
  useEffect(() => {
    let alive = true;
    invoke<ProviderConfig | null>("get_model_provider_config", { role })
      .then((cfg) => {
        if (!alive || !cfg) return;
        const p = cfg.preset ?? "deepseek";
        setPreset(p);
        setModelId(cfg.modelId || (presetModel ? presetModel[p] ?? "" : PROVIDER_PRESETS.find((x) => x.id === p)?.defaultModelId ?? ""));
        setBaseUrl(cfg.baseUrl ?? "");
        setApiFormat(cfg.apiFormat === "anthropic" ? "anthropic" : "openai");
        // 上下文窗口回填（Plan27 #2）：有值显示，无值留空（不预填，尊重用户「未设」语义）。
        setContextWindow(cfg.contextWindow != null ? String(cfg.contextWindow) : "");
        setConfigured(true);
        if (cfg.baseUrl || p === "custom") setAdvancedOpen(true);
      })
      .catch(() => {});
    return () => { alive = false; };
  }, [role]);

  function handlePresetChange(next: string) {
    setTestResult(null); // F4：改配置后清旧测试结果，避免误导
    setSmokeResult(null); // C-1：同步清工具调用冒烟结果
    setPreset(next);
    const meta = PROVIDER_PRESETS.find((p) => p.id === next);
    // 切换预设给出合理默认 modelId 占位（视觉块用识图模型表）。
    setModelId(presetModel ? presetModel[next] ?? "" : meta?.defaultModelId ?? "");
    // 上下文窗口预填（Plan27 #2）：仅主模型块按预设常见值预填，custom/无值留空。
    if (role === "main") {
      const cw = PRESET_CONTEXT_WINDOW[next];
      setContextWindow(cw != null ? String(cw) : "");
    }
    // custom 自动展开高级行（base_url 必填）；非 custom 收起并清空自定义端点回到官方。
    if (next === "custom") {
      setAdvancedOpen(true);
    } else {
      setAdvancedOpen(false);
      setBaseUrl("");
    }
  }

  async function handleSave() {
    setError(null);
    if (isCustom && !baseUrl.trim()) {
      setError("自定义供应商必须填写 Base URL");
      setAdvancedOpen(true);
      return;
    }
    if (!modelId.trim()) {
      setError("请填写模型 ID");
      return;
    }
    // 已配置且用户未改动 key 时不允许提交空 key（避免清掉已存 key）。首次配置必须填 key。
    if (!keyTouched && !configured && !apiKey.trim()) {
      setError("请填写 API Key");
      return;
    }
    if (keyTouched && !apiKey.trim()) {
      setError("请填写 API Key");
      return;
    }
    if (!configured && !apiKey.trim()) {
      setError("请填写 API Key");
      return;
    }
    setSaving(true);
    try {
      // 上下文窗口（Plan27 #2）：空串/非正数 → 传 null（后端回退默认）；否则传整数 tokens。
      const cwTrim = contextWindow.trim();
      const cwNum = cwTrim ? Math.floor(Number(cwTrim)) : NaN;
      const contextWindowOut = cwTrim && Number.isFinite(cwNum) && cwNum > 0 ? cwNum : null;
      await invoke("save_model_provider", {
        role,
        preset,
        label: presetMeta.label,
        baseUrl: baseUrl.trim() || null,
        apiKey: apiKey.trim(),
        modelId: modelId.trim(),
        apiFormat: role === "vision" ? apiFormat : "openai",
        contextWindow: contextWindowOut,
      });
      setConfigured(true);
      setKeyTouched(false);
      setApiKey("");
      // F2：保存成功显示「已保存 ✓」，数秒后自动复位（重存时先清旧定时器）。
      if (savedTimer.current) clearTimeout(savedTimer.current);
      setSaved(true);
      savedTimer.current = setTimeout(() => setSaved(false), 2600);
      onSaved?.();
    } catch (err) {
      setError(humanizeError(String(err)));
    } finally {
      setSaving(false);
    }
  }

  /**
   * 测试连接（Plan19 P2a / C-A）：用当前表单做一次最小请求。用户输入了新 key 就传新 key，
   * 否则传空串（命令内 key 空时从 DB 读该 role 既有 key）。结果就地显示，不跳主界面。
   */
  async function handleTest() {
    setTestResult(null);
    if (isCustom && !baseUrl.trim()) {
      setTestResult({ ok: false, message: "自定义供应商必须填写 Base URL" });
      setAdvancedOpen(true);
      return;
    }
    if (!modelId.trim()) {
      setTestResult({ ok: false, message: "请填写模型 ID" });
      return;
    }
    // 未配置且未输入 key：前端先拦截不调用。
    if (!configured && !apiKey.trim()) {
      setTestResult({ ok: false, message: "请先填写 API Key" });
      return;
    }
    setTesting(true);
    try {
      const message = await invoke<string>("test_provider_connection", {
        role,
        baseUrl: baseUrl.trim(),
        apiKey: keyTouched ? apiKey.trim() : "",
        model: modelId.trim(),
        apiFormat: role === "vision" ? apiFormat : "openai",
      });
      setTestResult({ ok: true, message: message || "连接成功" });
    } catch (err) {
      setTestResult({ ok: false, message: humanizeError(String(err)) });
    } finally {
      setTesting(false);
    }
  }

  /**
   * 测试工具调用（C-1 / Plan25 #3）：用当前表单做一次最小工具调用冒烟探测。
   * key 透传规则与 handleTest 一致——改过才传新 key，否则传空串（命令内回退 DB 既有 key）。
   * 返回 true=该供应商工具调用可用（原生或兜底恢复均算）；false=模型未返回工具调用（可能不支持/较弱）。
   */
  async function handleSmokeTest() {
    setSmokeResult(null);
    if (isCustom && !baseUrl.trim()) {
      setSmokeResult({ kind: "error", message: "自定义供应商必须填写 Base URL" });
      setAdvancedOpen(true);
      return;
    }
    if (!modelId.trim()) {
      setSmokeResult({ kind: "error", message: "请填写模型 ID" });
      return;
    }
    // 未配置且未输入 key：前端先拦截不调用。
    if (!configured && !apiKey.trim()) {
      setSmokeResult({ kind: "error", message: "请先填写 API Key" });
      return;
    }
    setSmokeTesting(true);
    try {
      const ok = await invoke<boolean>("smoke_test_tool_call", {
        role,
        baseUrl: baseUrl.trim(),
        apiKey: keyTouched ? apiKey.trim() : "",
        model: modelId.trim(),
        apiFormat: role === "vision" ? apiFormat : "openai",
      });
      setSmokeResult(
        ok
          ? { kind: "ok", message: "工具调用可用" }
          : { kind: "weak", message: "该模型未返回工具调用（可能不支持/较弱）" },
      );
    } catch (err) {
      setSmokeResult({ kind: "error", message: humanizeError(String(err)) });
    } finally {
      setSmokeTesting(false);
    }
  }

  // 密码框：已配置且本次未改动 → 占位「已配置 ••••」（只读感）；用户聚焦输入即视为改动。
  const keyPlaceholder = configured && !keyTouched ? "已配置 ••••（如需更换请重新输入）" : "sk-...";

  return (
    <div className="provider-card">
      <div className="provider-card__head">
        <span className="provider-card__title">{title}</span>
        <span className={`provider-badge${configured ? " provider-badge--on" : ""}`}>
          {configured ? "● 已配置" : "○ 未配置"}
        </span>
      </div>

      <div className="provider-grid">
        <label className="provider-field">
          <span className="provider-field__label">供应商</span>
          <select className="model-select" value={preset} onChange={(e) => handlePresetChange(e.target.value)}>
            {PROVIDER_PRESETS.map((p) => (
              <option key={p.id} value={p.id}>{p.label}</option>
            ))}
          </select>
        </label>
        <label className="provider-field">
          <span className="provider-field__label">模型</span>
          <input
            className="conv-search provider-input"
            type="text"
            value={modelId}
            placeholder={presetMeta.defaultModelId || "model-id"}
            onChange={(e) => { setModelId(e.target.value); setTestResult(null); setSmokeResult(null); }}
          />
        </label>

        <label className="provider-field provider-field--full">
          <span className="provider-field__label">API Key</span>
          <span className="provider-key">
            <input
              className="conv-search provider-input"
              type={showKey ? "text" : "password"}
              value={apiKey}
              placeholder={keyPlaceholder}
              onChange={(e) => { setApiKey(e.target.value); setKeyTouched(true); setTestResult(null); setSmokeResult(null); }}
            />
            <button
              type="button"
              className="provider-key__eye"
              title={showKey ? "隐藏" : "显示"}
              onClick={() => setShowKey((v) => !v)}
            >
              {showKey ? <EyeOff size={15} /> : <Eye size={15} />}
            </button>
          </span>
        </label>

        {role === "vision" && (
          <label className="provider-field provider-field--full">
            <span className="provider-field__label">API 格式</span>
            <select
              className="model-select"
              value={apiFormat}
              onChange={(e) => { setApiFormat(e.target.value === "anthropic" ? "anthropic" : "openai"); setTestResult(null); setSmokeResult(null); }}
            >
              <option value="openai">OpenAI 格式（/chat/completions）</option>
              <option value="anthropic">Anthropic 格式（/v1/messages）</option>
            </select>
          </label>
        )}

        {/* 上下文窗口（Plan27 #2）：可选数字输入，回填 cfg.contextWindow，保存透传（空则传 null）。
            后端据其推导压缩软上限；非 DeepSeek 小窗口模型可在此据实填写以更早压缩。 */}
        <label className="provider-field provider-field--full">
          <span className="provider-field__label">上下文窗口（tokens，可选）</span>
          <input
            className="conv-search provider-input"
            type="number"
            min={0}
            value={contextWindow}
            placeholder={PRESET_CONTEXT_WINDOW[preset] != null ? String(PRESET_CONTEXT_WINDOW[preset]) : "如 128000，留空用默认"}
            onChange={(e) => { setContextWindow(e.target.value); setTestResult(null); setSmokeResult(null); }}
          />
          <span className="provider-field__hint">填写该模型的真实上下文窗口，接近上限时自动压缩。留空则用内置默认。</span>
        </label>

        <div className="provider-field--full">
          <button
            type="button"
            className="provider-advanced-toggle"
            onClick={() => !isCustom && setAdvancedOpen((v) => !v)}
            disabled={isCustom}
            title={isCustom ? "自定义供应商必须填写 Base URL" : undefined}
          >
            {advancedOpen ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
            高级设置（自定义 Base URL）
          </button>
          <div className={`provider-advanced${advancedOpen ? " provider-advanced--open" : ""}`}>
            <div className="provider-advanced__inner">
              <input
                className="conv-search provider-input"
                type="text"
                value={baseUrl}
                placeholder={presetMeta.baseUrl ?? "https://your-endpoint/v1"}
                onChange={(e) => { setBaseUrl(e.target.value); setTestResult(null); setSmokeResult(null); }}
              />
              <p className="settings-desc" style={{ marginTop: 4 }}>
                {isCustom
                  ? "自定义供应商必须填写 Base URL（自托管/代理）。基址或完整端点均可，照 API 文档粘贴即可，不会重复拼接路径。"
                  : `留空即使用 ${presetMeta.label} 官方端点；自托管/代理可填。`}
              </p>
            </div>
          </div>
        </div>
      </div>

      {error && <p className="settings-row__value" style={{ color: "var(--danger)" }}>{error}</p>}
      {testResult && (
        <p className="settings-row__value" style={{ color: testResult.ok ? "var(--success)" : "var(--danger)" }}>
          {testResult.ok ? "✓ " : "✗ "}{testResult.message}
        </p>
      )}
      {/* C-1 工具调用冒烟结果（Plan25 #3）：与测试连接同区展示——可用绿、未返回工具调用橙、报错红。 */}
      {smokeResult && (
        <p
          className="settings-row__value"
          style={{
            color:
              smokeResult.kind === "ok"
                ? "var(--success)"
                : smokeResult.kind === "weak"
                ? "var(--warning)"
                : "var(--danger)",
          }}
        >
          {smokeResult.kind === "ok" ? "✓ " : smokeResult.kind === "weak" ? "⚠ " : "✗ "}
          {smokeResult.message}
        </p>
      )}

      <div className="provider-card__actions">
        {saved && (
          <span className="provider-saved" role="status" style={{ color: "var(--success)" }}>
            已保存 ✓
          </span>
        )}
        <button type="button" className="approval-card__btn" onClick={handleTest} disabled={testing || smokeTesting || saving}>
          {testing ? "测试中…" : "测试连接"}
        </button>
        {/* C-1：测试工具调用（Plan25 #3），结果就地显示在上方测试区。 */}
        <button type="button" className="approval-card__btn" onClick={handleSmokeTest} disabled={testing || smokeTesting || saving}>
          <Plug size={14} /> {smokeTesting ? "探测中…" : "测试工具调用"}
        </button>
        <button type="button" className="approval-card__btn approval-card__btn--allow" onClick={handleSave} disabled={saving}>
          {saving ? "保存中…" : "保存"}
        </button>
      </div>
    </div>
  );
}

// ── LspServerSettings（R-uicfg / 0.0.57）──────────────────────────────────────

/**
 * LSP 语言服务器注册表设置：把后端硬编码的 8 个已知服务器列出，每个可启用/禁用，并可选填
 * 二进制路径覆盖（其位置，非命令本身）。安全：服务器种类与命令身份恒为后端常量，UI 无法新增任意命令；
 * 路径覆盖是用户显式录入的本地路径，留空即回退默认 PATH 解析。
 *
 * 挂载时拉 get_lsp_known_servers + get_lsp_server_config；保存整份配置经 save_lsp_server_config。
 */
function LspServerSettings() {
  const [servers, setServers] = useState<LspKnownServer[]>([]);
  const [config, setConfig] = useState<LspServerConfig>({});
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const savedTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimer.current) clearTimeout(savedTimer.current); }, []);

  useEffect(() => {
    let alive = true;
    Promise.all([
      invoke<LspKnownServer[]>("get_lsp_known_servers"),
      invoke<{ [k: string]: LspServerSetting } | LspServerConfig>("get_lsp_server_config"),
    ])
      .then(([known, cfg]) => {
        if (!alive) return;
        setServers(Array.isArray(known) ? known : []);
        // 后端 LspServerConfig 经 serde(transparent) 序列化为裸 map { kind: {enabled, pathOverride} }。
        setConfig((cfg as LspServerConfig) ?? {});
      })
      .catch((e) => { if (alive) setError(humanizeError(String(e))); })
      .finally(() => { if (alive) setLoading(false); });
    return () => { alive = false; };
  }, []);

  /** 取某 kind 的当前设置（缺省＝启用、无覆盖）。 */
  function settingOf(kind: string): LspServerSetting {
    return config[kind] ?? { enabled: true, pathOverride: null };
  }

  function setEnabled(kind: string, enabled: boolean) {
    setSaved(false);
    setConfig((prev) => ({ ...prev, [kind]: { ...settingOf(kind), enabled } }));
  }

  function setPathOverride(kind: string, pathOverride: string) {
    setSaved(false);
    setConfig((prev) => ({ ...prev, [kind]: { ...settingOf(kind), pathOverride } }));
  }

  async function handleSave() {
    setError(null);
    setSaving(true);
    try {
      // 归一化：空白路径覆盖回传 null（后端视为无覆盖）。仅回传已知 kind。
      const out: LspServerConfig = {};
      for (const s of servers) {
        const cur = settingOf(s.kind);
        const path = (cur.pathOverride ?? "").trim();
        out[s.kind] = { enabled: cur.enabled, pathOverride: path || null };
      }
      await invoke("save_lsp_server_config", { config: out });
      if (savedTimer.current) clearTimeout(savedTimer.current);
      setSaved(true);
      savedTimer.current = setTimeout(() => setSaved(false), 2600);
    } catch (e) {
      setError(humanizeError(String(e)));
    } finally {
      setSaving(false);
    }
  }

  return (
    <>
      <h3 className="settings-content__h">语言服务器（LSP）</h3>
      <p className="settings-desc" style={{ marginTop: 0 }}>
        启用/停用下列<b>已知</b>语言服务器，决定 Agent 的代码智能工具（定义跳转 / 引用 / 悬浮 / 诊断）对哪些语言生效。
        可选为某个服务器指定其<b>二进制路径</b>（当它不在 PATH 中时）。出于安全，服务器种类与命令固定，无法新增任意命令；
        路径仅指明已知二进制的位置，留空即按 PATH 自动查找。
      </p>
      {loading ? (
        <p className="settings-row__value">加载中…</p>
      ) : (
        <>
          {servers.map((s) => {
            const cur = settingOf(s.kind);
            return (
              <div key={s.kind} className="provider-card" style={{ marginBottom: 10 }}>
                <div className="provider-card__head">
                  <span className="provider-card__title">{s.displayName}</span>
                  <label className="toggle" style={{ marginBottom: 0 }}>
                    <input
                      type="checkbox"
                      checked={cur.enabled}
                      onChange={(e) => setEnabled(s.kind, e.target.checked)}
                    />
                    <span>{cur.enabled ? "已启用" : "已停用"}</span>
                  </label>
                </div>
                <p className="settings-desc" style={{ marginTop: 2, marginBottom: 6 }}>
                  命令 <code>{s.command}{s.args.length ? " " + s.args.join(" ") : ""}</code>
                  ｜扩展名 {s.extensions.map((x) => "." + x).join(" ")}
                </p>
                <label className="provider-field provider-field--full">
                  <span className="provider-field__label">二进制路径覆盖（可选）</span>
                  <input
                    className="conv-search provider-input"
                    type="text"
                    value={cur.pathOverride ?? ""}
                    placeholder={`留空＝在 PATH 中自动查找 ${s.command}`}
                    disabled={!cur.enabled}
                    onChange={(e) => setPathOverride(s.kind, e.target.value)}
                  />
                </label>
              </div>
            );
          })}
          {error && <p className="settings-row__value" style={{ color: "var(--danger)" }}>{error}</p>}
          <div className="provider-card__actions">
            {saved && (
              <span className="provider-saved" role="status" style={{ color: "var(--success)" }}>
                已保存 ✓
              </span>
            )}
            <button type="button" className="approval-card__btn approval-card__btn--allow" onClick={handleSave} disabled={saving}>
              {saving ? "保存中…" : "保存"}
            </button>
          </div>
        </>
      )}
    </>
  );
}

// ── RoleRoutingSettings（R8 角色→模型路由，R-uicfg / 0.0.57）──────────────────

/**
 * 角色→模型路由设置：为 action / plan / critique 三个功能角色各绑定一个模型+供应商；不设＝回退主模型。
 * 概览经 get_role_routing；每个角色的表单回填 get_role_provider_config，保存经 save_role_provider，
 * 清除（回退主模型）经 clear_role_provider。api_key 不回显明文（留空＝保留已存 key）。
 */
function RoleRoutingSettings() {
  const [routing, setRouting] = useState<RoleRouting[]>([]);

  const refresh = () => {
    invoke<RoleRouting[]>("get_role_routing")
      .then((r) => setRouting(Array.isArray(r) ? r : []))
      .catch(() => setRouting([]));
  };
  useEffect(() => { refresh(); }, []);

  function routingOf(role: string): RoleRouting | undefined {
    return routing.find((r) => r.role === role);
  }

  return (
    <>
      <h3 className="settings-content__h">角色 → 模型路由</h3>
      <p className="settings-desc" style={{ marginTop: 0 }}>
        为不同<b>功能角色</b>绑定各自的模型与供应商：未设置的角色自动回退到「主模型」（与从前行为一致）。
        例如可让「规划」用更强的推理模型、「行动」用更快更省的模型。
      </p>
      {ROUTING_ROLES.map((r) => {
        const cur = routingOf(r.id);
        return (
          <RoleRoutingCard
            key={r.id}
            role={r.id}
            title={r.label}
            desc={r.desc}
            routing={cur}
            onChanged={refresh}
          />
        );
      })}
    </>
  );
}

/** 单个角色的路由卡片：状态徽标（自身/回退主模型）+ 供应商/模型/Key 表单 + 保存/回退主模型。 */
function RoleRoutingCard({
  role,
  title,
  desc,
  routing,
  onChanged,
}: {
  role: "action" | "plan" | "critique";
  title: string;
  desc: string;
  routing?: RoleRouting;
  onChanged: () => void;
}) {
  const [preset, setPreset] = useState<string>("deepseek");
  const [modelId, setModelId] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [configured, setConfigured] = useState(false);
  const [keyTouched, setKeyTouched] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const savedTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimer.current) clearTimeout(savedTimer.current); }, []);

  const presetMeta = PROVIDER_PRESETS.find((p) => p.id === preset) ?? PROVIDER_PRESETS[0];
  const isCustom = preset === "custom";

  // 挂载回填该角色自身配置（不回退 main；apiKey 脱敏为空）。
  useEffect(() => {
    let alive = true;
    invoke<ProviderConfig | null>("get_role_provider_config", { role })
      .then((cfg) => {
        if (!alive || !cfg) return;
        const p = cfg.preset ?? "deepseek";
        setPreset(p);
        setModelId(cfg.modelId || (PROVIDER_PRESETS.find((x) => x.id === p)?.defaultModelId ?? ""));
        setBaseUrl(cfg.baseUrl ?? "");
        setConfigured(true);
        if (cfg.baseUrl || p === "custom") setAdvancedOpen(true);
      })
      .catch(() => {});
    return () => { alive = false; };
  }, [role]);

  function handlePresetChange(next: string) {
    setPreset(next);
    const meta = PROVIDER_PRESETS.find((p) => p.id === next);
    setModelId(meta?.defaultModelId ?? "");
    if (next === "custom") setAdvancedOpen(true);
    else { setAdvancedOpen(false); setBaseUrl(""); }
  }

  async function handleSave() {
    setError(null);
    if (isCustom && !baseUrl.trim()) {
      setError("自定义供应商必须填写 Base URL");
      setAdvancedOpen(true);
      return;
    }
    if (!modelId.trim()) { setError("请填写模型 ID"); return; }
    // 首配必须填 key；已配且未改动 key 时留空＝保留已存。
    if (!configured && !apiKey.trim()) { setError("请填写 API Key"); return; }
    if (keyTouched && !apiKey.trim()) { setError("请填写 API Key"); return; }
    setSaving(true);
    try {
      await invoke("save_role_provider", {
        role,
        preset,
        label: presetMeta.label,
        baseUrl: baseUrl.trim() || null,
        apiKey: keyTouched ? apiKey.trim() : "",
        modelId: modelId.trim(),
        contextWindow: null,
      });
      setConfigured(true);
      setKeyTouched(false);
      setApiKey("");
      if (savedTimer.current) clearTimeout(savedTimer.current);
      setSaved(true);
      savedTimer.current = setTimeout(() => setSaved(false), 2600);
      onChanged();
    } catch (e) {
      setError(humanizeError(String(e)));
    } finally {
      setSaving(false);
    }
  }

  async function handleClear() {
    setError(null);
    setSaving(true);
    try {
      await invoke("clear_role_provider", { role });
      setConfigured(false);
      setApiKey("");
      setKeyTouched(false);
      onChanged();
    } catch (e) {
      setError(humanizeError(String(e)));
    } finally {
      setSaving(false);
    }
  }

  // 状态文案：自身配置 / 回退主模型 / 主模型未配。
  const source = routing?.source ?? "main";
  const effective = routing?.effectiveModel;
  const badgeText =
    source === "self" ? "● 使用专属模型" : source === "main" ? "○ 回退主模型" : "⚠ 主模型未配置";
  const keyPlaceholder = configured && !keyTouched ? "已配置 ••••（如需更换请重新输入）" : "sk-...";

  return (
    <div className="provider-card" style={{ marginBottom: 10 }}>
      <div className="provider-card__head">
        <span className="provider-card__title">{title}</span>
        <span className={`provider-badge${source === "self" ? " provider-badge--on" : ""}`}>{badgeText}</span>
      </div>
      <p className="settings-desc" style={{ marginTop: 2, marginBottom: 6 }}>
        {desc}
        {source !== "self" && effective ? `　当前实际：${effective}` : ""}
      </p>

      <div className="provider-grid">
        <label className="provider-field">
          <span className="provider-field__label">供应商</span>
          <select className="model-select" value={preset} onChange={(e) => handlePresetChange(e.target.value)}>
            {PROVIDER_PRESETS.map((p) => (
              <option key={p.id} value={p.id}>{p.label}</option>
            ))}
          </select>
        </label>
        <label className="provider-field">
          <span className="provider-field__label">模型</span>
          <input
            className="conv-search provider-input"
            type="text"
            value={modelId}
            placeholder={presetMeta.defaultModelId || "model-id"}
            onChange={(e) => setModelId(e.target.value)}
          />
        </label>

        <label className="provider-field provider-field--full">
          <span className="provider-field__label">API Key</span>
          <span className="provider-key">
            <input
              className="conv-search provider-input"
              type={showKey ? "text" : "password"}
              value={apiKey}
              placeholder={keyPlaceholder}
              onChange={(e) => { setApiKey(e.target.value); setKeyTouched(true); }}
            />
            <button type="button" className="provider-key__eye" title={showKey ? "隐藏" : "显示"} onClick={() => setShowKey((v) => !v)}>
              {showKey ? <EyeOff size={15} /> : <Eye size={15} />}
            </button>
          </span>
        </label>

        <div className="provider-field--full">
          <button
            type="button"
            className="provider-advanced-toggle"
            onClick={() => !isCustom && setAdvancedOpen((v) => !v)}
            disabled={isCustom}
            title={isCustom ? "自定义供应商必须填写 Base URL" : undefined}
          >
            {advancedOpen ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
            高级设置（自定义 Base URL）
          </button>
          <div className={`provider-advanced${advancedOpen ? " provider-advanced--open" : ""}`}>
            <div className="provider-advanced__inner">
              <input
                className="conv-search provider-input"
                type="text"
                value={baseUrl}
                placeholder={presetMeta.baseUrl ?? "https://your-endpoint/v1"}
                onChange={(e) => setBaseUrl(e.target.value)}
              />
              <p className="settings-desc" style={{ marginTop: 4 }}>
                {isCustom
                  ? "自定义供应商必须填写 Base URL（自托管/代理）。"
                  : `留空即使用 ${presetMeta.label} 官方端点；自托管/代理可填。`}
              </p>
            </div>
          </div>
        </div>
      </div>

      {error && <p className="settings-row__value" style={{ color: "var(--danger)" }}>{error}</p>}

      <div className="provider-card__actions">
        {saved && (
          <span className="provider-saved" role="status" style={{ color: "var(--success)" }}>
            已保存 ✓
          </span>
        )}
        {configured && (
          <button type="button" className="approval-card__btn" onClick={handleClear} disabled={saving}>
            回退主模型
          </button>
        )}
        <button type="button" className="approval-card__btn approval-card__btn--allow" onClick={handleSave} disabled={saving}>
          {saving ? "保存中…" : "保存"}
        </button>
      </div>
    </div>
  );
}

// ── SettingsModal ───────────────────────────────────────────────────────────

export function SettingsModal({
  initialSection,
  onMainConfiguredChange,
  appInfo,
  apiKeyLabel,
  balance,
  onRefreshBalance,
  permissionMode,
  mcpServers,
  permRules,
  commandSandbox,
  onToggleSandbox,
  taskBudget,
  onSetBudget,
  hasActiveConv,
  onExportConversation,
  onExportLedger,
  onClearData,
  onPermissionModeChange,
  onAddMcpServer,
  onToggleMcpServer,
  onDeleteMcpServer,
  onRefreshMcp,
  onAddPermRule,
  onDeletePermRule,
  onUpdateAvailable,
  onClose,
}: {
  initialSection: SettingsSection;
  onMainConfiguredChange: (configured: boolean) => void;
  appInfo: AppInfo | null;
  apiKeyLabel: string;
  balance: BalanceState;
  onRefreshBalance: () => void;
  permissionMode: PermissionMode;
  mcpServers: McpServer[];
  permRules: string[];
  commandSandbox: boolean;
  onToggleSandbox: (enabled: boolean) => void;
  taskBudget: number;
  onSetBudget: (budget: number) => void;
  hasActiveConv: boolean;
  onExportConversation: () => void;
  onExportLedger: () => void;
  onClearData: () => void;
  onPermissionModeChange: (m: PermissionMode) => void;
  onAddMcpServer: (name: string, command: string, authToken?: string) => void;
  onToggleMcpServer: (id: string, enabled: boolean) => void;
  onDeleteMcpServer: (id: string) => void;
  onRefreshMcp: () => void;
  onAddPermRule: (rule: string) => void;
  onDeletePermRule: (rule: string) => void;
  onUpdateAvailable: (version: string) => void;
  onClose: () => void;
}) {
  const trapRef = useFocusTrap<HTMLDivElement>(true);
  const [mcpName, setMcpName] = useState("");
  const [mcpCommand, setMcpCommand] = useState("");
  const [mcpToken, setMcpToken] = useState("");
  const [ruleInput, setRuleInput] = useState("");
  // 最近被拦动作（Plan27 #9）：进入「权限规则」分类时拉取，每条可一键加 allow/deny 规则。
  const [deniedActions, setDeniedActions] = useState<DeniedAction[]>([]);
  // 检查更新按钮自管理：idle → checking → result(10s) → idle，期间禁用、尺寸不变。
  const [checkLabel, setCheckLabel] = useState("检查更新");
  const [checkBusy, setCheckBusy] = useState(false);
  const checkTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (checkTimerRef.current) clearTimeout(checkTimerRef.current); }, []);

  async function handleCheckUpdateBtn() {
    if (checkBusy) return;
    setCheckBusy(true);
    setCheckLabel("检查中…");
    let label = "已是最新版本";
    try {
      const v = await invoke<string | null>("check_update");
      if (v) {
        label = `发现新版本 v${v}`;
        onUpdateAvailable(v);
      }
    } catch {
      label = "检查失败，请稍后重试";
    }
    setCheckLabel(label);
    checkTimerRef.current = setTimeout(() => {
      setCheckLabel("检查更新");
      setCheckBusy(false);
    }, 10000);
  }
  const [section, setSection] = useState<SettingsSection>(initialSection);
  const [budgetInput, setBudgetInput] = useState(String(taskBudget));
  // 「扩展 agent 的模态」开关：持久化于 settings(modality_extended)；开 → 露出视觉/音频块。
  const [modalityExtended, setModalityExtended] = useState(false);
  // 主 provider 是否 deepseek 预设：决定账户区 DeepSeek 余额卡片是否显示。
  const [mainIsDeepseek, setMainIsDeepseek] = useState(false);

  useEffect(() => {
    invoke<string | null>("get_app_setting", { key: "modality_extended" })
      .then((v) => setModalityExtended(v === "1"))
      .catch(() => {});
    invoke<ProviderConfig | null>("get_model_provider_config", { role: "main" })
      .then((cfg) => setMainIsDeepseek((cfg?.preset ?? "deepseek") === "deepseek" && !!cfg))
      .catch(() => {});
  }, []);

  // 进入「权限规则」分类时拉取最近被拦动作（Plan27 #9）。
  useEffect(() => {
    if (section !== "rules") return;
    invoke<DeniedAction[]>("recent_denied_actions")
      .then((list) => setDeniedActions(Array.isArray(list) ? list : []))
      .catch(() => setDeniedActions([]));
  }, [section]);

  async function handleToggleModality(next: boolean) {
    setModalityExtended(next);
    await invoke("set_app_setting", { key: "modality_extended", value: next ? "1" : "" }).catch(() => {});
    // 关闭模态：移除视觉 provider，使图像门禁回到「无视觉」态。
    if (!next) {
      await invoke("remove_model_provider", { role: "vision" }).catch(() => {});
    }
  }

  const NAV: Array<{ id: typeof section; label: string }> = [
    { id: "account", label: "账户" },
    { id: "provider", label: "模型供应商" },
    { id: "routing", label: "角色路由" },
    { id: "lsp", label: "语言服务器" },
    { id: "permission", label: "权限" },
    { id: "rules", label: "权限规则" },
    { id: "mcp", label: "MCP 服务器" },
    { id: "data", label: "数据" },
    { id: "about", label: "关于" },
  ];

  return (
    <div
      className="approval-overlay"
      role="dialog"
      aria-modal="true"
      aria-label="设置"
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
    >
      <div className="approval-card panel-card settings-modal" ref={trapRef} onClick={(e) => e.stopPropagation()}>
        <nav className="settings-nav" aria-label="设置分类">
          <p className="settings-nav__title">设置</p>
          {NAV.map((n) => (
            <button
              key={n.id}
              type="button"
              className={`settings-nav__item${section === n.id ? " settings-nav__item--active" : ""}`}
              onClick={() => setSection(n.id)}
            >
              {n.label}
            </button>
          ))}
          <button type="button" className="settings-nav__close" onClick={onClose}>
            关闭
          </button>
        </nav>

        <div className="settings-content">
          {section === "account" && (
            <>
              <h3 className="settings-content__h">账户</h3>
              <div className="settings-row">
                <span className="settings-row__label">DeepSeek API Key</span>
                <span className="settings-row__value">{apiKeyLabel}</span>
              </div>
              <p className="settings-desc">API Key 在「<b>模型供应商</b>」选项卡中配置，存于本地，不上传云端。</p>

              {/* 余额查询门禁（Plan21 #5）：仅 DeepSeek 主供应商支持余额查询；
                  其他供应商后端会返回 Err，此处直接以提示替代，不触发刷新。 */}
              {!mainIsDeepseek && (
                <>
                  <div className="settings-section__head" style={{ marginTop: 16 }}>
                    <span>账户余额</span>
                  </div>
                  <p className="settings-desc">该供应商不提供余额查询（仅 DeepSeek 支持）。</p>
                </>
              )}

              {mainIsDeepseek && (
                <>
                  <div className="settings-section__head" style={{ marginTop: 16 }}>
                    <span>账户余额</span>
                    <button
                      className="changes-row__revert"
                      type="button"
                      onClick={onRefreshBalance}
                      disabled={balance.status === "loading"}
                    >
                      {balance.status === "loading" ? "查询中…" : "刷新"}
                    </button>
                  </div>
                  {balance.status === "error" && (
                    <p className="settings-row__value" style={{ color: "var(--danger)" }}>{balance.message}</p>
                  )}
                  {balance.status === "ok" && (
                    <>
                      <div className="settings-row">
                        <span className="settings-row__label">状态</span>
                        <span className="settings-row__value" style={{ color: balance.data.isAvailable ? "var(--success)" : "var(--danger)" }}>
                          {balance.data.isAvailable ? "余额充足，可正常调用" : "余额不足"}
                        </span>
                      </div>
                      {balance.data.balanceInfos.length === 0 && <p className="settings-row__value">未返回余额明细</p>}
                      {balance.data.balanceInfos.map((b) => (
                        <div key={b.currency} className="balance-card">
                          <div className="balance-card__total">
                            <span className="balance-card__amount">{b.totalBalance}</span>
                            <span className="balance-card__currency">{b.currency}</span>
                          </div>
                          <div className="balance-card__detail">
                            <span>充值 {b.toppedUpBalance}</span>
                            <span>赠送 {b.grantedBalance}</span>
                          </div>
                        </div>
                      ))}
                    </>
                  )}
                </>
              )}
            </>
          )}

          {section === "provider" && (
            <>
              <h3 className="settings-content__h">模型供应商</h3>
              <p className="settings-desc" style={{ marginTop: 0, marginBottom: 8 }}>
                配置主模型供应商（OpenAI 兼容）：选预设、填 API Key 与模型 ID 即可。Base URL 留空走预设官方端点，自托管/代理可在高级设置里覆盖。
              </p>
              <ProviderCard
                role="main"
                title="主模型"
                onSaved={() => {
                  // 保存后刷新「DeepSeek 余额卡片是否显示」与顶栏 key 状态，并消除首屏未配引导。
                  onMainConfiguredChange(true);
                  invoke<ProviderConfig | null>("get_model_provider_config", { role: "main" })
                    .then((cfg) => setMainIsDeepseek((cfg?.preset ?? "deepseek") === "deepseek" && !!cfg))
                    .catch(() => {});
                }}
              />

              <label className="toggle provider-modality-toggle">
                <input
                  type="checkbox"
                  checked={modalityExtended}
                  onChange={(e) => handleToggleModality(e.target.checked)}
                />
                <span><b>扩展 agent 的模态</b>　开启后可接入视觉/音频模型扩展能力</span>
              </label>

              {modalityExtended && (
                <div className="provider-modality">
                  <ProviderCard role="vision" title="视觉（识图）" />
                  <div className="provider-card provider-card--disabled">
                    <div className="provider-card__head">
                      <span className="provider-card__title">
                        <Lock size={13} style={{ verticalAlign: "-2px", marginRight: 4 }} />
                        音频（语音）
                      </span>
                      <span className="provider-badge">🔒 敬请期待</span>
                    </div>
                    <p className="settings-desc" style={{ marginTop: 4 }}>
                      后续接入语音模型，实现语音对话交流（占位禁用）。
                    </p>
                  </div>
                </div>
              )}
            </>
          )}

          {section === "routing" && <RoleRoutingSettings />}

          {section === "lsp" && <LspServerSettings />}

          {section === "permission" && (
            <>
              <h3 className="settings-content__h">权限</h3>
              <div className="settings-row">
                <span className="settings-row__label">默认权限模式</span>
                <select className="model-select" value={permissionMode} onChange={(e) => onPermissionModeChange(e.target.value as PermissionMode)}>
                  {PERMISSION_MODES.map((mode) => (
                    <option key={mode} value={mode}>{getPermissionModeLabel(mode)}</option>
                  ))}
                </select>
              </div>
              <ul className="settings-desc settings-desc--list">
                <li><b>受限</b>：只允许聊天与只读，不能改文件或执行命令。</li>
                <li><b>每次询问</b>：每个写入/命令/越界动作都弹窗确认。</li>
                <li><b>工作区自动</b>：工作区内读写自动执行，低风险命令直接跑，其余审批。</li>
                <li><b>完全访问</b>：放开执行，仍保留审计与回退。</li>
              </ul>

              <div className="settings-row" style={{ marginTop: 16 }}>
                <span className="settings-row__label">命令沙箱</span>
                <label className="toggle">
                  <input type="checkbox" checked={commandSandbox} onChange={(e) => onToggleSandbox(e.target.checked)} />
                  <span>{commandSandbox ? "已开启" : "已关闭"}</span>
                </label>
              </div>
              <p className="settings-desc">
                开启后，run_command 在<b>受限令牌沙箱</b>中执行：剥离管理员特权、进程随会话干净销毁、子进程环境擦除 API Key 等密钥。
                少数需要特权的命令可能受影响，可临时关闭。（网络与文件路径隔离将在后续 AppContainer 版本提供。）
              </p>

              <div className="settings-row" style={{ marginTop: 16 }}>
                <span className="settings-row__label">单轮 token 上限（超出即暂停本轮）</span>
                <span style={{ display: "flex", gap: 6 }}>
                  <input
                    className="conv-search"
                    style={{ width: 120, marginBottom: 0 }}
                    type="number"
                    min={0}
                    value={budgetInput}
                    onChange={(e) => setBudgetInput(e.target.value)}
                    onBlur={() => onSetBudget(Math.max(0, parseInt(budgetInput) || 0))}
                  />
                </span>
              </div>
              {/* Plan21 #4：正名为「单轮上限」，消除「任务级累计」误解；说明含视觉调用开销。 */}
              <p className="settings-desc">本轮(单次发送)累计 token 超过此值时自动暂停本轮，防止失控烧 token。该上限按每轮独立计算，<b>含本轮视觉识图等调用的 token 开销</b>（0 = 不限）。当前 {taskBudget === 0 ? "不限" : taskBudget.toLocaleString()}。</p>
            </>
          )}

          {section === "data" && (
            <>
              <h3 className="settings-content__h">数据</h3>
              <p className="settings-desc">所有数据保存在本地，可随时导出、备份或删除。</p>
              <div style={{ display: "flex", flexDirection: "column", gap: 8, marginTop: 12, maxWidth: 320 }}>
                <button type="button" className="approval-card__btn" onClick={onExportConversation} disabled={!hasActiveConv}>导出当前会话（Markdown）</button>
                <button type="button" className="approval-card__btn" onClick={onExportLedger}>导出 Token 账本（CSV）</button>
                <button type="button" className="approval-card__btn" onClick={onClearData} style={{ color: "var(--danger)" }}>清除所有会话</button>
              </div>
              <p className="settings-desc">Token 账本 CSV 可与 DeepSeek 官方账单对照核对消费。</p>
            </>
          )}

          {section === "rules" && (
            <>
              <h3 className="settings-content__h">权限规则</h3>
              <p className="settings-desc">
                细粒度规则按 <b>deny 优先</b> 生效。格式：<code>[allow:|deny:]&lt;工具&gt;:&lt;路径glob&gt;</code>，
                或 <code>cmd:&lt;命令前缀&gt;</code>、<code>tool:&lt;工具名&gt;</code>。
                例：<code>deny:read_file:**/.env</code>、<code>allow:cmd:git push</code>。审批弹窗的「总是允许」会自动生成 allow 规则。
              </p>
              {permRules.length === 0 && <p className="settings-row__value">暂无规则。</p>}
              {permRules.map((r) => (
                <div key={r} className="changes-row">
                  <span className={`mcp-dot${r.startsWith("deny:") ? "" : " mcp-dot--on"}`} aria-hidden="true">●</span>
                  <span className="changes-row__path" title={r} style={{ fontFamily: "monospace" }}>{r}</span>
                  <button className="changes-row__revert" type="button" onClick={() => onDeletePermRule(r)}>删除</button>
                </div>
              ))}
              <div className="mcp-add">
                <input className="conv-search" placeholder="如 deny:read_file:**/.env 或 allow:cmd:git push" value={ruleInput} onChange={(e) => setRuleInput(e.target.value)} />
                <button className="approval-card__btn" type="button" disabled={!ruleInput.trim()} onClick={() => { onAddPermRule(ruleInput.trim()); setRuleInput(""); }}>添加规则</button>
              </div>

              {/* 从最近被拦动作一键加规则（Plan27 #9）：列出 recent_denied_actions，每条配「+ 允许 / + 拒绝」。 */}
              <div className="settings-section__head" style={{ marginTop: 18 }}>
                <span>最近被拦动作</span>
              </div>
              {deniedActions.length === 0 ? (
                <p className="settings-desc">暂无被拦动作。当 Agent 的某个工具被权限拦下后，会出现在此，可一键为其加规则。</p>
              ) : (
                deniedActions.map((d, i) => {
                  const rule = `tool:${d.toolName}`;
                  return (
                    <div key={`${d.toolName}-${d.target}-${i}`} className="changes-row">
                      <span className="mcp-dot" aria-hidden="true">●</span>
                      <span className="changes-row__tool">{d.toolName}</span>
                      {d.target && <span className="changes-row__path" title={d.target}>{d.target}</span>}
                      <button
                        className="changes-row__revert"
                        type="button"
                        title={`添加规则 allow:${rule}`}
                        onClick={() => onAddPermRule(`allow:${rule}`)}
                      >
                        + 允许
                      </button>
                      <button
                        className="changes-row__revert"
                        type="button"
                        title={`添加规则 deny:${rule}`}
                        onClick={() => onAddPermRule(`deny:${rule}`)}
                      >
                        + 拒绝
                      </button>
                    </div>
                  );
                })
              )}
            </>
          )}

          {section === "mcp" && (
            <>
              <div className="settings-content__h" style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                <span><Plug size={15} style={{ verticalAlign: "-2px" }} /> MCP 服务器</span>
                <button className="changes-row__revert" type="button" onClick={onRefreshMcp}>刷新状态</button>
              </div>
              <p className="settings-desc">接入外部 MCP 服务器，其工具会并入模型工具集，统一经权限审批与审计。<b>stdio</b>：填启动命令（需 Node/npx 等）；<b>HTTP</b>：填 http(s):// 地址，可选填 Token；留空且服务端要求授权时自动走浏览器 OAuth。</p>
              {mcpServers.map((s) => (
                <div key={s.id} className="changes-row">
                  <span className={`mcp-dot${s.connected ? " mcp-dot--on" : ""}`} aria-hidden="true">●</span>
                  <span className="changes-row__tool">{s.name}</span>
                  <span className="changes-row__path" title={s.command}>
                    {s.connected ? `${s.toolCount} 个工具` : s.enabled ? "未连接" : "已停用"}
                  </span>
                  <button className="changes-row__revert" type="button" onClick={() => onToggleMcpServer(s.id, !s.enabled)}>{s.enabled ? "停用" : "启用"}</button>
                  <button className="changes-row__revert" type="button" onClick={() => onDeleteMcpServer(s.id)}>删除</button>
                </div>
              ))}
              <div className="mcp-add">
                <input className="conv-search" placeholder="名称（如 filesystem / github）" value={mcpName} onChange={(e) => setMcpName(e.target.value)} />
                <input className="conv-search" placeholder="启动命令 或 http(s):// 地址" value={mcpCommand} onChange={(e) => setMcpCommand(e.target.value)} />
                <input className="conv-search" placeholder="Bearer Token（仅 HTTP，可选；留空走 OAuth）" value={mcpToken} onChange={(e) => setMcpToken(e.target.value)} />
                <button className="approval-card__btn" type="button" disabled={!mcpName.trim() || !mcpCommand.trim()} onClick={() => { onAddMcpServer(mcpName.trim(), mcpCommand.trim(), mcpToken.trim() || undefined); setMcpName(""); setMcpCommand(""); setMcpToken(""); }}>添加并连接</button>
              </div>
            </>
          )}

          {section === "about" && (
            <>
              <h3 className="settings-content__h">关于</h3>
              <div className="settings-row">
                <span className="settings-row__label">版本</span>
                <span className="settings-row__value">v{appInfo?.version ?? "…"}</span>
              </div>
              <div className="settings-row">
                <span className="settings-row__label">数据目录</span>
                <span className="settings-row__value" title={appInfo?.dataDir}>{appInfo?.dataDir ?? "…"}</span>
              </div>
              <p className="settings-desc">会话、token 账本、权限规则等本地数据保存在数据目录的 SQLite 中。</p>
              <button
                type="button"
                className="approval-card__btn check-update-btn"
                style={{ marginTop: 12 }}
                onClick={handleCheckUpdateBtn}
                disabled={checkBusy}
              >
                {checkLabel}
              </button>
              <p className="settings-desc">发现新版本时，可在左下角横幅一键安装更新。</p>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
