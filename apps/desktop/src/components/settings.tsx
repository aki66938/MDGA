// 设置弹窗：模型连接库（Connections）+ 加载模型（Models）+ 角色分配（Assignments）+ 其它分类（0.0.60）。

import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronRight, Eye, EyeOff, Plug, Lock, Globe, Info, FolderOpen, Upload, Trash2, Pencil, Wrench, Plus, Download, Check, X, RefreshCw } from "lucide-react";
import { open as openDirDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { getPermissionModeLabel, type PermissionMode } from "@mdga/ui";
import {
  PERMISSION_MODES,
  PROVIDER_PRESETS,
  ASSIGNABLE_ROLES,
  type ConnectionView,
  type CuratedModelView,
  type RoleAssignmentView,
  type AppInfo,
  type BalanceState,
  type McpServer,
  type DeniedAction,
  type SettingsSection,
  type LspKnownServer,
  type LspServerConfig,
  type LspServerSetting,
  type BillingMode,
  type ModelPricing,
  type StoredPricing,
  type PricingCurrency,
  type PricingUnit,
  type PricingTier,
  type PresetView,
  type EffectivePricingView,
  type CaptureResult,
  type PricingDiff,
  type SubscriptionInfo,
  type ConnectionMonthlyUsage,
  type OkfSettingsView,
  type OkfBrowseView,
} from "../types";
import {
  humanizeError,
  fmtTokens,
  currencySymbol,
  unitSuffix,
  parseStoredPricing,
  pricingSummary,
  pricingBadges,
  convertPriceForUnit,
  validatePricingNonNeg,
  changeBadge,
  relativeTime,
  buildApplyItems,
  diffKey,
  shouldStoreAutofill,
} from "../utils";
import { useFocusTrap } from "./dialogs";

// ── 模型连接库（Connections，0.0.59）─────────────────────────────────────────

/** preset → API 格式默认值：内置预设均 OpenAI 兼容；custom 由用户选。 */
const PRESET_API_FORMAT: Record<string, "openai" | "anthropic"> = {
  deepseek: "openai",
  zhipu: "openai",
  moonshot: "openai",
  qwen: "openai",
  siliconflow: "openai",
};

/** 取连接的展示名：label 优先，其次 preset 名，再次「未命名连接」。 */
function connectionDisplayName(c: ConnectionView): string {
  if (c.label && c.label.trim()) return c.label.trim();
  const presetLabel = PROVIDER_PRESETS.find((p) => p.id === c.preset)?.label;
  return presetLabel ?? "未命名连接";
}

/**
 * 角色 → 简洁中文名（用于级联删除确认/提示文案）。
 * 源用 ASSIGNABLE_ROLES（main=主模型（Main）…），去掉「（English）」后缀只留中文：主模型 / 行动 / 计划 / 评审 / 视觉 / 子代理 / 嵌入。
 * 后端返回的受影响角色是裸 role id（如 "main"/"action"），不在表里时原样回显。
 */
function roleLabel(role: string): string {
  const meta = ASSIGNABLE_ROLES.find((r) => r.id === role);
  if (!meta) return role;
  return meta.label.replace(/（[^）]*）/g, "").trim() || meta.label;
}

/**
 * 级联删除后的成功提示（连接/模型通用）：returnedRoles＝后端清除分配的角色 id 列表。
 * 无清除＝「已删除。」；有清除＝列出受影响角色中文名；若含 main，附「（主模型现未配置，请重新指定）」。
 */
function cascadeDeleteNotice(returnedRoles: string[]): string {
  if (returnedRoles.length === 0) return "已删除。";
  const names = returnedRoles.map(roleLabel).join("、");
  const mainCleared = returnedRoles.includes("main");
  return `已删除；已清除分配：${names}${mainCleared ? "（主模型现未配置，请重新指定）" : ""}`;
}

// ── 计费方式（Billing，0.0.72）──────────────────────────────────────────────

/** 连接的有效计费方式（缺＝按量付费 'api'）。 */
function effectiveBillingMode(c: ConnectionView): BillingMode {
  return c.billingMode === "subscription" || c.billingMode === "none" ? c.billingMode : "api";
}

/** 三档计费方式的中文标签（分段控件用）。 */
const BILLING_MODES: Array<{ id: BillingMode; label: string }> = [
  { id: "api", label: "按量付费（API）" },
  { id: "subscription", label: "订阅套餐" },
  { id: "none", label: "本地免计费" },
];

/** 解析 subscriptionJson（自由结构）。失败/空返回 {}。 */
function parseSubscription(json: string | null | undefined): SubscriptionInfo {
  if (!json || !json.trim()) return {};
  try {
    const o = JSON.parse(json) as SubscriptionInfo;
    return o && typeof o === "object" ? o : {};
  } catch {
    return {};
  }
}

/** 徽标文案（单价）：preset→「预设·可改」｜needs_verify→「待官网核对」｜custom→「自定义」。 */
function pricingBadgeLabel(b: "preset" | "needs_verify" | "custom"): string {
  if (b === "preset") return "预设·可改";
  if (b === "needs_verify") return "待官网核对";
  return "自定义";
}

/**
 * 把 lookup_effective_pricing 返回的有效价视图转成 StoredPricing（0.0.73）。
 * source='override'（采集价）→ _source:'custom'（实时官网价、可改；不当预设）；
 * source='preset'（编译快照）→ _source:'preset' + confidence/needsVerify/sourceUrl 元数据。
 * 用于「显示侧回退」徽标判定与「加模型自动填」落库。
 */
function effectiveToStored(view: EffectivePricingView): StoredPricing {
  if (view.source === "override") {
    return {
      ...view.pricing,
      _source: "custom",
      // 采集价携带来源链接（官网定价页），保留以便编辑器内可点开核对。
      ...(view.sourceUrl ? { _sourceUrl: view.sourceUrl } : {}),
    };
  }
  return {
    ...view.pricing,
    _source: "preset",
    ...(view.confidence ? { _confidence: view.confidence } : {}),
    ...(view.needsVerify ? { _needsVerify: true } : {}),
    ...(view.sourceUrl ? { _sourceUrl: view.sourceUrl } : {}),
  };
}

/**
 * 模型连接库设置（Connections）：一个连接 = 名称 + 预设 + Base URL + API Key + API 格式。
 * 这是**唯一**录入 API Key 的地方。列表展示每个连接，可新增 / 编辑 / 删除 / 测试连接。
 *
 * 挂载拉 list_connections；新增/编辑经 ConnectionEditor（save_connection）；删除经 delete_connection
 * （若仍被某角色引用，后端拒绝并返回错误，此处原样提示）。绝不回显 apiKey 明文（仅以 hasKey 表态）。
 */
function ConnectionsSettings({ onChanged }: { onChanged?: () => void }) {
  const [connections, setConnections] = useState<ConnectionView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // 级联删除成功后的内联提示（例如「已清除分配：行动、主模型…」）。下一次操作会清掉。
  const [notice, setNotice] = useState<string | null>(null);
  // 编辑器状态：null=未打开；{ conn: undefined }=新增；{ conn }=编辑既有连接。
  const [editing, setEditing] = useState<{ conn?: ConnectionView } | null>(null);
  // 测试连接：针对某连接就地展示结果。key=连接 id。
  const [testState, setTestState] = useState<Record<string, { testing?: boolean; ok?: boolean; message?: string }>>({});

  const refresh = () => {
    setLoading(true);
    invoke<ConnectionView[]>("list_connections")
      .then((list) => setConnections(Array.isArray(list) ? list : []))
      .catch((e) => setError(humanizeError(String(e))))
      .finally(() => setLoading(false));
  };
  useEffect(() => { refresh(); }, []);

  /**
   * 删除连接（0.0.62 级联）：先算出本连接旗下模型被哪些角色引用，按是否有引用给出不同确认文案；
   * 确认后 force=true 级联删（删连接+旗下模型+清这些角色分配，含 main）。返回被清角色 → 内联成功提示。
   * 主模型若被清，提示其「现未配置，请重新指定」。onChanged 让依赖 UI（App 主模型徽标等）刷新。
   */
  async function handleDelete(c: ConnectionView) {
    setError(null);
    setNotice(null);
    const name = connectionDisplayName(c);

    // 算受影响角色：本连接旗下模型 id 集合 ∩ 各角色 modelRef。
    let affectedRoles: string[] = [];
    let modelCount = 0;
    try {
      const [curated, assigns] = await Promise.all([
        invoke<CuratedModelView[]>("list_models_for_connection", { connectionId: c.id }),
        invoke<RoleAssignmentView[]>("get_role_assignments"),
      ]);
      modelCount = curated.length;
      const modelIds = new Set(curated.map((m) => m.id));
      affectedRoles = assigns.filter((a) => a.modelRef && modelIds.has(a.modelRef)).map((a) => a.role);
    } catch (e) {
      setError(humanizeError(String(e)));
      return;
    }

    let message: string;
    if (affectedRoles.length === 0) {
      message = `删除连接「${name}」及其下的模型？此操作不可撤销。`;
    } else {
      const names = affectedRoles.map(roleLabel).join("、");
      const mainAffected = affectedRoles.includes("main");
      message =
        `删除连接「${name}」将一并删除其下 ${modelCount} 个模型，并清除这些角色的模型分配：${names}。\n` +
        `其中：行动/计划等角色会回到「跟随主模型」，` +
        (mainAffected ? `★ 主模型会变为「未配置」（需重新指定一个主模型）。\n` : ``) +
        `确定删除？`;
    }
    if (!window.confirm(message)) return;

    try {
      const cleared = await invoke<string[]>("delete_connection", { id: c.id, force: true });
      refresh();
      onChanged?.();
      setNotice(cascadeDeleteNotice(cleared));
    } catch (e) {
      setError(humanizeError(String(e)));
    }
  }

  /** 测试某连接：弹一个模型 ID（用户登记的模型作提示）后调 test_connection。 */
  async function handleTest(c: ConnectionView) {
    setError(null);
    let known: string[] = [];
    try {
      // 0.0.60：list_models_for_connection 现返回该连接已登记的 curated 模型，取其 modelId 作建议。
      const curated = await invoke<CuratedModelView[]>("list_models_for_connection", { connectionId: c.id });
      known = curated.map((m) => m.modelId);
    } catch {
      known = [];
    }
    const fallback = PROVIDER_PRESETS.find((p) => p.id === c.preset)?.defaultModelId ?? "";
    const suggested = known[0] ?? fallback;
    const model = window.prompt(
      `测试连接「${connectionDisplayName(c)}」，请输入一个用于测试的模型 ID：` +
        (known.length ? `\n（已知：${known.join("、")}）` : ""),
      suggested,
    );
    if (model == null) return; // 取消
    const trimmed = model.trim();
    if (!trimmed) {
      setTestState((s) => ({ ...s, [c.id]: { ok: false, message: "请提供一个待测模型 ID" } }));
      return;
    }
    setTestState((s) => ({ ...s, [c.id]: { testing: true } }));
    try {
      const message = await invoke<string>("test_connection", { connectionId: c.id, model: trimmed });
      setTestState((s) => ({ ...s, [c.id]: { ok: true, message: message || "连接成功" } }));
    } catch (e) {
      setTestState((s) => ({ ...s, [c.id]: { ok: false, message: humanizeError(String(e)) } }));
    }
  }

  /** 测试工具调用：弹模型 ID 后探测该模型在该连接端点能否返回 tool_call（key 由后端按连接取，前端不接触）。 */
  async function handleToolTest(c: ConnectionView) {
    setError(null);
    let known: string[] = [];
    try {
      const curated = await invoke<CuratedModelView[]>("list_models_for_connection", { connectionId: c.id });
      known = curated.map((m) => m.modelId);
    } catch {
      known = [];
    }
    const fallback = PROVIDER_PRESETS.find((p) => p.id === c.preset)?.defaultModelId ?? "";
    const suggested = known[0] ?? fallback;
    const model = window.prompt(
      `测试「${connectionDisplayName(c)}」的工具调用，请输入一个用于测试的模型 ID：` +
        (known.length ? `\n（已知：${known.join("、")}）` : ""),
      suggested,
    );
    if (model == null) return; // 取消
    const trimmed = model.trim();
    if (!trimmed) {
      setTestState((s) => ({ ...s, [c.id]: { ok: false, message: "请提供一个待测模型 ID" } }));
      return;
    }
    setTestState((s) => ({ ...s, [c.id]: { testing: true } }));
    try {
      const ok = await invoke<boolean>("smoke_test_tool_call_for_connection", {
        connectionId: c.id,
        model: trimmed,
      });
      setTestState((s) => ({
        ...s,
        [c.id]: {
          ok,
          message: ok
            ? `模型 ${trimmed} 支持工具调用`
            : `模型 ${trimmed} 未返回工具调用（该模型/端点可能不支持，agent 工具能力会受限）`,
        },
      }));
    } catch (e) {
      setTestState((s) => ({ ...s, [c.id]: { ok: false, message: humanizeError(String(e)) } }));
    }
  }

  if (editing) {
    return (
      <ConnectionEditor
        connection={editing.conn}
        onCancel={() => setEditing(null)}
        onSaved={() => {
          setEditing(null);
          refresh();
          onChanged?.();
        }}
      />
    );
  }

  return (
    <>
      <h3 className="settings-content__h">模型连接</h3>
      <p className="settings-desc" style={{ marginTop: 0, marginBottom: 8 }}>
        连接 = 端点 + 密钥（<b>唯一</b>录入 API Key 处，配一次可复用）。在连接卡下「加载模型」登记要用的模型，再到「模型分配」指派给各角色。
      </p>

      {loading ? (
        <p className="settings-row__value">加载中…</p>
      ) : connections.length === 0 ? (
        <p className="settings-row__value">暂无连接，点下方「新增连接」开始。</p>
      ) : (
        connections.map((c) => {
          const t = testState[c.id];
          const presetLabel = PROVIDER_PRESETS.find((p) => p.id === c.preset)?.label ?? c.preset ?? "自定义";
          return (
            <div key={c.id} className="provider-card" style={{ marginBottom: 10 }}>
              <div className="provider-card__head">
                <span className="provider-card__title">{connectionDisplayName(c)}</span>
                <span className={`provider-badge${c.hasKey ? " provider-badge--on" : ""}`}>
                  {c.hasKey ? "● 已配密钥" : "○ 无密钥"}
                </span>
              </div>
              <p className="settings-desc" style={{ marginTop: 2, marginBottom: 8 }}>
                预设 <b>{presetLabel}</b>
                ｜格式 {c.apiFormat === "anthropic" ? "Anthropic" : "OpenAI"}
                ｜端点 <code>{c.baseUrl?.trim() || "（预设官方端点）"}</code>
              </p>
              {t && (t.message || t.testing) && (
                <p
                  className="settings-row__value"
                  style={{ color: t.testing ? "var(--text-2)" : t.ok ? "var(--success)" : "var(--danger)" }}
                >
                  {t.testing ? "测试中…" : (t.ok ? "✓ " : "✗ ") + (t.message ?? "")}
                </p>
              )}
              <div className="provider-card__actions">
                <button type="button" className="approval-card__btn" onClick={() => handleTest(c)} disabled={!!t?.testing}>
                  <Plug size={14} /> 测试连接
                </button>
                <button type="button" className="approval-card__btn" onClick={() => handleToolTest(c)} disabled={!!t?.testing}>
                  <Wrench size={14} /> 测试工具调用
                </button>
                <button type="button" className="approval-card__btn" onClick={() => setEditing({ conn: c })}>
                  <Pencil size={14} /> 编辑
                </button>
                <button type="button" className="approval-card__btn" style={{ color: "var(--danger)" }} onClick={() => handleDelete(c)}>
                  <Trash2 size={14} /> 删除
                </button>
              </div>

              {/* 0.0.72：连接级「计费方式」（按量/订阅/免计费）+ 订阅套餐元信息 + 订阅月度用量条。
                  改 billingMode 后刷新连接列表，让模型行单价编辑可见性、卡头徽标即时更新。 */}
              <ConnectionBilling connection={c} onChanged={refresh} />

              {/* 0.0.60：在每个连接卡下登记「加载模型」（一个连接可登记多个模型，一对多）。
                  0.0.62：级联删模型可能清掉 main，故把 onChanged 透传给上层刷新（主模型徽标等）。 */}
              <ConnectionModels connection={c} onChanged={onChanged} />
            </div>
          );
        })
      )}

      {notice && <p className="settings-row__value" style={{ color: "var(--success)" }}>{notice}</p>}
      {error && <p className="settings-row__value" style={{ color: "var(--danger)" }}>{error}</p>}

      <div className="provider-card__actions" style={{ justifyContent: "flex-start" }}>
        <button type="button" className="approval-card__btn approval-card__btn--allow" onClick={() => setEditing({ conn: undefined })}>
          新增连接
        </button>
      </div>
    </>
  );
}

/**
 * 连接级「计费方式」（0.0.72）：三档分段控件（按量付费 / 订阅套餐 / 本地免计费）。
 * 改动经 set_connection_billing；选「订阅套餐」额外露出套餐名/月费/月额度 token（写入 subscriptionJson）。
 * subscription 模式再挂一个「本月用量条」（MonthlyUsageBar）。改完调 onChanged 让上层刷新连接列表。
 */
function ConnectionBilling({ connection, onChanged }: { connection: ConnectionView; onChanged: () => void }) {
  const mode = effectiveBillingMode(connection);
  const initialSub = parseSubscription(connection.subscriptionJson);
  const [planLabel, setPlanLabel] = useState(initialSub.planLabel ?? "");
  const [monthlyFee, setMonthlyFee] = useState(initialSub.monthlyFee != null ? String(initialSub.monthlyFee) : "");
  const [subCurrency, setSubCurrency] = useState<PricingCurrency>(initialSub.currency === "USD" ? "USD" : "CNY");
  const [quotaTokens, setQuotaTokens] = useState(
    initialSub.monthlyQuotaTokens != null ? String(initialSub.monthlyQuotaTokens) : "",
  );
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  /** 组装当前订阅字段为 SubscriptionInfo（空字段省略）。 */
  function buildSubscription(): SubscriptionInfo {
    const out: SubscriptionInfo = { currency: subCurrency };
    const pl = planLabel.trim();
    if (pl) out.planLabel = pl;
    const fee = parseFloat(monthlyFee);
    if (monthlyFee.trim() && !Number.isNaN(fee)) out.monthlyFee = fee;
    const q = parseInt(quotaTokens, 10);
    if (quotaTokens.trim() && !Number.isNaN(q) && q > 0) out.monthlyQuotaTokens = q;
    return out;
  }

  /** 切换计费方式：subscription 带当前订阅字段，其它传 null。 */
  async function changeMode(next: BillingMode) {
    if (next === mode && next !== "subscription") return;
    setError(null);
    setSaving(true);
    try {
      const subJson = next === "subscription" ? JSON.stringify(buildSubscription()) : null;
      await invoke("set_connection_billing", {
        connectionId: connection.id,
        billingMode: next,
        subscriptionJson: subJson,
      });
      onChanged();
    } catch (e) {
      setError(humanizeError(String(e)));
    } finally {
      setSaving(false);
    }
  }

  /** 保存订阅元信息（套餐名/月费/月额度），保持 billingMode='subscription'。 */
  async function saveSubscription() {
    setError(null);
    setSaving(true);
    try {
      await invoke("set_connection_billing", {
        connectionId: connection.id,
        billingMode: "subscription",
        subscriptionJson: JSON.stringify(buildSubscription()),
      });
      onChanged();
    } catch (e) {
      setError(humanizeError(String(e)));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="provider-billing">
      <div className="provider-billing__head">
        <span className="provider-billing__title">计费方式</span>
        <span className="provider-billing__seg" role="group" aria-label="计费方式">
          {BILLING_MODES.map((b) => (
            <button
              key={b.id}
              type="button"
              className={`provider-billing__segbtn${mode === b.id ? " provider-billing__segbtn--on" : ""}`}
              disabled={saving}
              aria-pressed={mode === b.id}
              onClick={() => void changeMode(b.id)}
            >
              {b.label}
            </button>
          ))}
        </span>
      </div>

      {mode === "subscription" && (
        <div className="provider-billing__sub">
          <div className="provider-grid">
            <label className="provider-field">
              <span className="provider-field__label">套餐名</span>
              <input
                className="conv-search provider-input"
                type="text"
                value={planLabel}
                placeholder="如 Pro / 月付套餐"
                disabled={saving}
                onChange={(e) => setPlanLabel(e.target.value)}
              />
            </label>
            <label className="provider-field">
              <span className="provider-field__label">月费（可空）</span>
              <span style={{ display: "flex", gap: 6 }}>
                <select
                  value={subCurrency}
                  disabled={saving}
                  style={{ width: 64 }}
                  onChange={(e) => setSubCurrency(e.target.value === "USD" ? "USD" : "CNY")}
                >
                  <option value="CNY">￥</option>
                  <option value="USD">＄</option>
                </select>
                <input
                  className="conv-search provider-input"
                  type="number"
                  min={0}
                  value={monthlyFee}
                  placeholder="如 99"
                  disabled={saving}
                  onChange={(e) => setMonthlyFee(e.target.value)}
                />
              </span>
            </label>
            <label className="provider-field provider-field--full">
              <span className="provider-field__label">月额度 token（可空，用于本月用量进度条）</span>
              <input
                className="conv-search provider-input"
                type="number"
                min={0}
                value={quotaTokens}
                placeholder="如 5000000；留空＝不显进度只显累计"
                disabled={saving}
                onChange={(e) => setQuotaTokens(e.target.value)}
              />
            </label>
          </div>
          <div className="provider-card__actions">
            <button
              type="button"
              className="approval-card__btn approval-card__btn--allow"
              disabled={saving}
              onClick={() => void saveSubscription()}
            >
              {saving ? "保存中…" : "保存套餐信息"}
            </button>
          </div>
          <MonthlyUsageBar connection={connection} quotaTokens={parseSubscription(connection.subscriptionJson).monthlyQuotaTokens} />
        </div>
      )}

      {error && <p className="settings-row__value" style={{ color: "var(--danger)", marginTop: 4 }}>{error}</p>}
    </div>
  );
}

/**
 * 订阅月度用量条（0.0.72 ⑥）：调 get_connection_monthly_usage 取本月该连接累计 token。
 * 有 monthlyQuotaTokens → 进度条「本月 X / Y tokens」；无额度 → 只显「本月 X tokens」。
 * 用 fmtTokens 做 k/M 简写。connection.subscriptionJson 变化时重取（额度来自上层已保存的值）。
 */
function MonthlyUsageBar({
  connection,
  quotaTokens,
}: {
  connection: ConnectionView;
  quotaTokens?: number;
}) {
  const [usage, setUsage] = useState<ConnectionMonthlyUsage | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    setError(null);
    invoke<ConnectionMonthlyUsage>("get_connection_monthly_usage", { connectionId: connection.id })
      .then((u) => { if (alive) setUsage(u ?? null); })
      .catch((e) => { if (alive) setError(humanizeError(String(e))); });
    return () => { alive = false; };
    // 订阅 JSON（含额度）变化时重取，使刚保存的额度即时反映在进度条。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connection.id, connection.subscriptionJson]);

  if (error) {
    return <p className="settings-desc" style={{ marginTop: 6, color: "var(--warning)" }}>本月用量获取失败：{error}</p>;
  }
  if (!usage) {
    return <p className="settings-desc" style={{ marginTop: 6 }}>本月用量加载中…</p>;
  }

  const used = usage.totalTokens ?? 0;
  const hasQuota = quotaTokens != null && quotaTokens > 0;
  const pct = hasQuota ? Math.min(100, Math.round((used / quotaTokens!) * 100)) : 0;
  const over = hasQuota && used > quotaTokens!;

  return (
    <div className="provider-usage">
      <div className="provider-usage__label">
        {hasQuota
          ? `本月 ${fmtTokens(used)} / ${fmtTokens(quotaTokens!)} tokens`
          : `本月 ${fmtTokens(used)} tokens`}
        {over && <span style={{ color: "var(--danger)", marginLeft: 6 }}>已超额度</span>}
      </div>
      {hasQuota && (
        <div className="provider-usage__track" aria-hidden="true">
          <div
            className="provider-usage__fill"
            style={{ width: `${pct}%`, background: over ? "var(--danger)" : "var(--brand)" }}
          />
        </div>
      )}
    </div>
  );
}

/**
 * 「加载模型」子区（0.0.60）：嵌在每个连接卡内，登记**该连接下**用户实际会用到的模型。
 * 这是连接 ↔ 模型「一对多」显式化的地方，也是角色分配可选模型的来源。
 *
 * 列表来自 list_models_for_connection（现返回 curated 模型，**非**旧的预设字符串清单）。
 * 添加：手填 modelId + 可选 label + 可选 contextWindow（高级），经 add_model（按 连接+modelId 去重，重复＝更新）。
 * 「拉取可用模型」：调 fetch_available_models 实时 GET 端点 /models；成功把返回的 id 列为快速添加 chip；
 * 失败（throws）显示「该端点不支持自动拉取，请手动填写模型 ID」并保留手动录入。
 * 删除：调 delete_model；若该模型仍被某角色引用，后端拒绝并返回中文错误，此处原样提示。
 */
function ConnectionModels({ connection, onChanged }: { connection: ConnectionView; onChanged?: () => void }) {
  const [models, setModels] = useState<CuratedModelView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // 级联删除单个模型成功后的内联提示（例如「已清除分配：主模型…」）。
  const [notice, setNotice] = useState<string | null>(null);
  // 添加表单：模型 ID + 可选别名（行内即可，不再用「高级」折叠）。上下文窗口改为登记后在列表行内就地编辑。
  const [modelId, setModelId] = useState("");
  const [label, setLabel] = useState("");
  const [adding, setAdding] = useState(false);
  // 行内编辑「参数 → 上下文窗口」（0.0.61）：editingCtxId=正在编辑的模型 id（null=无）；
  // ctxDraft=输入框草稿（空串＝清空＝由端点默认）；savingCtxId=正在保存的模型 id。
  const [editingCtxId, setEditingCtxId] = useState<string | null>(null);
  const [ctxDraft, setCtxDraft] = useState("");
  const [savingCtxId, setSavingCtxId] = useState<string | null>(null);
  // 「拉取可用模型」状态：fetching=进行中；fetched=端点返回的 id 列表（作快速添加 chip）；notice=失败提示。
  const [fetching, setFetching] = useState(false);
  const [fetched, setFetched] = useState<string[] | null>(null);
  const [fetchNotice, setFetchNotice] = useState<string | null>(null);
  // 单价就地编辑（0.0.72）：editingPriceId=正在改单价的模型 id（null=无）。仅 billingMode='api' 的连接可用。
  const [editingPriceId, setEditingPriceId] = useState<string | null>(null);
  // 官网单价采集（0.0.73）：capturing=抓取中；captureResult=展开 diff 面板的成功结果（null=未展开）；
  // captureNotice=不支持/失败的一句提示（不弹面板）。仅 deepseek/siliconflow 显「官网单价」按钮。
  const [capturing, setCapturing] = useState(false);
  const [captureResult, setCaptureResult] = useState<CaptureResult | null>(null);
  const [captureNotice, setCaptureNotice] = useState<string | null>(null);

  // 该连接的有效计费方式：决定模型行是显单价编辑（api）/「套餐内」（subscription）/「免计费」（none）。
  const billing = effectiveBillingMode(connection);
  // 该 preset 是否支持官网采集（仅两家）：决定是否露出「官网单价」按钮。
  const capturePreset = connection.preset?.trim().toLowerCase();
  const captureSupported = capturePreset === "deepseek" || capturePreset === "siliconflow";

  const refresh = () => {
    setLoading(true);
    invoke<CuratedModelView[]>("list_models_for_connection", { connectionId: connection.id })
      .then((list) => setModels(Array.isArray(list) ? list : []))
      .catch((e) => setError(humanizeError(String(e))))
      .finally(() => setLoading(false));
  };
  useEffect(() => { refresh(); /* eslint-disable-next-line react-hooks/exhaustive-deps */ }, [connection.id]);

  // 计费方式切到非 api（订阅/免计费）时（#12）：「官网单价」按钮随之隐藏，必须同时清掉已展开的
  // diff 面板与采集提示——否则面板会残留且可应用，与隐藏的按钮状态不一致。
  useEffect(() => {
    if (billing !== "api") {
      setCaptureResult(null);
      setCaptureNotice(null);
    }
  }, [billing]);

  /** 已登记的 modelId 集合（小写归一），用于「拉取」结果里标注哪些已添加。 */
  const existingIds = new Set(models.map((m) => m.modelId.trim().toLowerCase()));

  /**
   * 预设价自动填（0.0.73 修正）：新增模型后，按 (连接 preset, modelId, 'CNY') 查 lookup_effective_pricing，
   * **仅当来源是编译预设快照（source==='preset'）才落库**（_source:'preset'，含 confidence/needsVerify/sourceUrl）。
   *
   * 关键：来源是采集覆盖（source==='override'）时**直接 return、不写 pricing_json**——否则会把实时官网价
   * 冻进模型条目，使其不再跟随后续官网刷新、且徽标误显「自定义」。让该模型保持 pricing_json 空，
   * 显示侧（ModelPriceCell 回退查 lookup_effective_pricing）与结算（resolve_billing override>编译兜底）
   * 都会走实时有效价 → 显「官网价」蓝标、后续刷新自动跟随。
   *
   * 静默尽力（查不到/无 preset/出错都不打断添加）。仅 api 才填。
   */
  async function autofillPreset(modelRef: string, modelId: string, alreadyHasPricing: boolean) {
    if (billing !== "api" || alreadyHasPricing) return;
    const preset = connection.preset?.trim();
    if (!preset || preset === "custom") return;
    try {
      const view = await invoke<EffectivePricingView | null>("lookup_effective_pricing", {
        connectionPreset: preset,
        modelId,
        currency: "CNY",
      });
      // 采集覆盖（source==='override'）不落库，保持 pricing_json 空 → 显示/结算走实时有效价回退
      // （显「官网价」、跟随刷新）；仅编译预设快照（source==='preset'）才落库（新增即有可结算价）。
      if (!shouldStoreAutofill(view)) return;
      await invoke<CuratedModelView>("set_model_pricing", {
        modelRef,
        pricingJson: JSON.stringify(effectiveToStored(view)),
      });
    } catch {
      // 静默：无有效价/网络/解析失败都不阻断添加。
    }
  }

  /** 添加一个模型（手填或来自拉取 chip）。可带 label/contextWindow；add_model 按 连接+modelId 去重。 */
  async function addModel(id: string, opts?: { label?: string; contextWindow?: number }) {
    const trimmed = id.trim();
    if (!trimmed) { setError("请填写模型 ID"); return; }
    setError(null);
    setAdding(true);
    try {
      const added = await invoke<CuratedModelView>("add_model", {
        connectionId: connection.id,
        modelId: trimmed,
        label: opts?.label?.trim() || null,
        contextWindow: opts?.contextWindow ?? null,
      });
      // 预设自动填单价（仅按量付费连接；已有单价的不覆盖）。再 refresh 刷新显示。
      await autofillPreset(added.id, added.modelId, !!added.pricingJson);
      refresh();
    } catch (e) {
      setError(humanizeError(String(e)));
    } finally {
      setAdding(false);
    }
  }

  /** 手动添加表单提交：读 modelId + 可选 label（行内），添加后清空表单。上下文窗口在列表行内就地登记。 */
  async function handleAddManual() {
    const id = modelId;
    await addModel(id, { label });
    setModelId("");
    setLabel("");
  }

  /** 进入某行的上下文窗口行内编辑：用当前值填充草稿（未设＝空串）。 */
  function beginEditCtx(m: CuratedModelView) {
    setError(null);
    setEditingCtxId(m.id);
    setCtxDraft(m.contextWindow != null ? String(m.contextWindow) : "");
  }

  /** 退出行内编辑且不保存。 */
  function cancelEditCtx() {
    setEditingCtxId(null);
    setCtxDraft("");
  }

  /** 保存某行的上下文窗口：空串＝清空（contextWindow=null＝由端点默认）；否则须为正整数。经 update_model。 */
  async function saveCtx(m: CuratedModelView) {
    const raw = ctxDraft.trim();
    let next: number | null;
    if (!raw) {
      next = null; // 清空 ⇒ 由端点默认
    } else {
      // 严格只收正整数：拒掉 2e6 / 1.5e3 / 1.5 这类会被 parseInt 静默截断成 2/1 的写法（会把软上限改坏）。
      if (!/^\d+$/.test(raw)) { setError("上下文窗口需为正整数（如 128000），或留空＝由端点默认"); return; }
      const n = parseInt(raw, 10);
      if (n <= 0) { setError("上下文窗口需为正整数，或留空＝由端点默认"); return; }
      next = n;
    }
    setError(null);
    setSavingCtxId(m.id);
    try {
      // 关键：回传当前 label，避免只改 context 时把已有别名冲成 NULL（update_model 整行覆写）。
      await invoke<CuratedModelView>("update_model", {
        id: m.id,
        label: m.label?.trim() || null,
        contextWindow: next,
      });
      setEditingCtxId(null);
      setCtxDraft("");
      refresh();
    } catch (e) {
      setError(humanizeError(String(e)));
    } finally {
      setSavingCtxId(null);
    }
  }

  /**
   * 删除一个已登记模型（0.0.62 级联）：先算出本模型被哪些角色引用，按有无引用给不同确认文案；
   * 确认后 force=true 级联删（删模型 + 清这些角色分配，含 main）。返回被清角色 → 内联成功提示。
   * 主模型若被清，提示其「现未配置，请重新指定」；onChanged 让依赖 UI 刷新。
   */
  async function handleDelete(m: CuratedModelView) {
    setError(null);
    setNotice(null);

    // 算受影响角色：modelRef === 本模型 id 的角色。
    let affectedRoles: string[] = [];
    try {
      const assigns = await invoke<RoleAssignmentView[]>("get_role_assignments");
      affectedRoles = assigns.filter((a) => a.modelRef === m.id).map((a) => a.role);
    } catch (e) {
      setError(humanizeError(String(e)));
      return;
    }

    let message: string;
    if (affectedRoles.length === 0) {
      message = `删除模型「${m.modelId}」？`;
    } else {
      const names = affectedRoles.map(roleLabel).join("、");
      const mainAffected = affectedRoles.includes("main");
      message =
        `删除模型「${m.modelId}」将清除这些角色的分配：${names}` +
        (mainAffected ? `（其中主模型会变未配置，需重新指定）` : ``) +
        `。确定？`;
    }
    if (!window.confirm(message)) return;

    try {
      const cleared = await invoke<string[]>("delete_model", { id: m.id, force: true });
      refresh();
      onChanged?.();
      setNotice(cascadeDeleteNotice(cleared));
    } catch (e) {
      setError(humanizeError(String(e)));
    }
  }

  /** 「拉取可用模型」：实时 GET 端点 /models。成功→把返回 id 列为快速添加 chip；失败→提示手动填写。 */
  async function handleFetch() {
    setError(null);
    setFetchNotice(null);
    setFetched(null);
    setFetching(true);
    try {
      const ids = await invoke<string[]>("fetch_available_models", { connectionId: connection.id });
      setFetched(Array.isArray(ids) ? ids : []);
      if (!ids || ids.length === 0) {
        setFetchNotice("该端点未返回任何模型，请手动填写模型 ID。");
      }
    } catch {
      // 端点无 /models / 网络 / 鉴权 / 解析失败：回退手动录入。
      setFetchNotice("该端点不支持自动拉取，请手动填写模型 ID。");
    } finally {
      setFetching(false);
    }
  }

  /**
   * 官网单价采集（0.0.73）：抓官网定价页 → LLM 抽取 → 与现价 diff（不写库）。
   * supported=false → 一句提示「该平台暂不支持自动采集」（不弹面板）；
   * ok=false → 显 error（如「抓取失败…」，不弹面板）；ok=true → 就地展开 diff 面板。
   */
  async function handleCapturePricing() {
    setCaptureNotice(null);
    setCaptureResult(null);
    setError(null);
    setCapturing(true);
    try {
      const res = await invoke<CaptureResult>("capture_official_pricing", {
        connectionId: connection.id,
      });
      if (!res.supported) {
        setCaptureNotice(res.message ?? "该平台暂不支持自动采集，可手填或恢复预设。");
        return;
      }
      if (!res.ok) {
        setCaptureNotice(res.error ?? "采集失败：以官网为准、保留现价。");
        return;
      }
      setCaptureResult(res);
    } catch (e) {
      setCaptureNotice(humanizeError(String(e)));
    } finally {
      setCapturing(false);
    }
  }

  /** 应用所选采集价（0.0.73）：组装 ApplyItem 调 apply_pricing_overrides → 刷新、关面板。 */
  async function handleApplyCaptured(items: ReturnType<typeof buildApplyItems>) {
    await invoke<number>("apply_pricing_overrides", {
      connectionPreset: connection.preset?.trim() ?? "",
      items,
    });
    setCaptureResult(null);
    refresh();
  }

  /**
   * 连接级「恢复预设」（0.0.73，bug 修复）：把本连接所有模型恢复为内置预设价。
   * 关键：用户「自定义」价在模型 pricing_json（优先级最高那层），只重置采集覆盖层碰不到它，
   * 故必须先清各模型的 pricing_json（撤销手填）→ 再重置采集覆盖（回编译快照）→ 有效价落到内置预设。
   * 破坏性操作（清手填 + 已采集官网价），先确认。
   */
  async function handleResetCaptured() {
    const ok = window.confirm(
      "恢复预设：将本连接所有模型恢复为内置预设价，清除你的手填单价与已采集官网价。继续？",
    );
    if (!ok) return;
    // 撤销各模型手填（pricing_json 置空）——这才是「自定义」价所在。
    for (const m of models) {
      if (m.pricingJson) {
        await invoke("set_model_pricing", { modelRef: m.id, pricingJson: null }).catch(() => {});
      }
    }
    // 重置采集覆盖层 → 有效价跌回编译快照（内置预设）。
    await invoke<number>("reset_pricing_overrides", {
      connectionPreset: connection.preset?.trim() ?? "",
    }).catch(() => {});
    setCaptureResult(null);
    refresh();
  }

  return (
    <div className="provider-models">
      <div className="provider-models__head">
        <span className="provider-models__title">加载模型</span>
        <span className="provider-models__count">{models.length} 个</span>
        {/* 官网单价（0.0.73）：仅 deepseek/siliconflow + 按量付费连接显示。无边框幽灵按钮，品牌色，文字在前图标在后。 */}
        {captureSupported && billing === "api" && (
          <button
            type="button"
            className="provider-models__capture"
            disabled={capturing}
            title="抓取官网现价并对比，勾选后应用"
            onClick={() => void handleCapturePricing()}
          >
            {capturing ? "采集中…" : "官网单价"}
            <RefreshCw size={12} className={capturing ? "provider-models__capture-spin" : undefined} />
          </button>
        )}
      </div>

      {/* 采集一句提示（不支持 / 失败）：不弹面板时显示在标题下。 */}
      {captureNotice && (
        <p className="settings-desc" style={{ margin: "2px 0 6px", color: "var(--warning)" }}>{captureNotice}</p>
      )}

      {/* diff 面板（采集成功后就地展开）。与按钮可见条件对齐：仅 api 计费时渲染（#12）。 */}
      {billing === "api" && captureResult && (
        <PricingCapturePanel
          connectionName={connectionDisplayName(connection)}
          result={captureResult}
          onApply={handleApplyCaptured}
          onReset={handleResetCaptured}
          onCancel={() => setCaptureResult(null)}
          onError={setError}
        />
      )}

      {loading ? (
        <p className="settings-row__value" style={{ margin: "2px 0" }}>加载中…</p>
      ) : models.length === 0 ? (
        <p className="settings-desc" style={{ margin: "2px 0 6px" }}>
          还没有模型，点「拉取可用模型」或在下方手动添加。
        </p>
      ) : (
        <ul className="provider-models__list">
          {models.map((m) => {
            // 仅当 label 是**有意义的别名**时才展示（迁移/旧数据曾把 label 置成 modelId，会重复显示）。
            const hasAlias = !!(m.label && m.label.trim() && m.label.trim() !== m.modelId);
            const editing = editingCtxId === m.id;
            const saving = savingCtxId === m.id;
            return (
              <li key={m.id} className="provider-models__item">
                <div className="provider-models__row">
                  <span className="provider-models__id" title={m.modelId}>
                    <code>{m.modelId}</code>
                    {hasAlias && <span className="provider-models__label">{m.label!.trim()}</span>}
                  </span>
                  <button
                    type="button"
                    className="provider-models__del"
                    title="删除该模型"
                    aria-label={`删除模型 ${m.modelId}`}
                    onClick={() => handleDelete(m)}
                  >
                    <Trash2 size={13} />
                  </button>
                </div>

                {/* 每个模型的「参数」区（0.0.61）：行内就地编辑、可扩展（目前仅上下文窗口，未来如温度等再加一行）。 */}
                <div className="provider-params">
                  <div className="provider-param">
                    <span className="provider-param__name">上下文窗口</span>
                    {editing ? (
                      <span className="provider-param__edit">
                        <input
                          className="provider-param__input"
                          type="number"
                          min={1}
                          autoFocus
                          value={ctxDraft}
                          placeholder="留空＝由端点默认"
                          disabled={saving}
                          onChange={(e) => setCtxDraft(e.target.value)}
                          onKeyDown={(e) => {
                            if (e.key === "Enter") { e.preventDefault(); void saveCtx(m); }
                            else if (e.key === "Escape") { e.preventDefault(); cancelEditCtx(); }
                          }}
                        />
                        <button
                          type="button"
                          className="provider-param__iconbtn provider-param__iconbtn--save"
                          disabled={saving}
                          title={saving ? "保存中…" : "保存"}
                          aria-label={saving ? "保存中" : "保存"}
                          onClick={() => void saveCtx(m)}
                        >
                          <Check size={13} />
                        </button>
                        <button
                          type="button"
                          className="provider-param__iconbtn provider-param__iconbtn--cancel"
                          disabled={saving}
                          title="取消"
                          aria-label="取消"
                          onClick={cancelEditCtx}
                        >
                          <X size={13} />
                        </button>
                      </span>
                    ) : (
                      <button
                        type="button"
                        className="provider-param__value"
                        title="点击编辑上下文窗口"
                        onClick={() => beginEditCtx(m)}
                      >
                        {m.contextWindow != null
                          ? `${m.contextWindow.toLocaleString()} tokens`
                          : "由端点默认"}
                        <Pencil size={11} className="provider-param__editicon" />
                      </button>
                    )}
                  </div>

                  {/* 单价（0.0.72）：仅按量付费连接显示编辑；订阅＝「套餐内」；免计费＝「免计费」。 */}
                  <div className="provider-param">
                    <span className="provider-param__name">单价</span>
                    {billing === "subscription" ? (
                      <span className="provider-param__value" style={{ cursor: "default" }}>套餐内</span>
                    ) : billing === "none" ? (
                      <span className="provider-param__value" style={{ cursor: "default" }}>免计费</span>
                    ) : (
                      <ModelPriceCell
                        model={m}
                        connection={connection}
                        editing={editingPriceId === m.id}
                        onBeginEdit={() => { setError(null); setEditingPriceId(m.id); }}
                        onClose={() => setEditingPriceId(null)}
                        onSaved={() => { setEditingPriceId(null); refresh(); }}
                        onError={setError}
                      />
                    )}
                  </div>
                </div>
              </li>
            );
          })}
        </ul>
      )}

      {/* 拉取可用模型 + 手动添加（模型 ID + 可选别名，行内；上下文窗口登记后在列表行内就地设置）。 */}
      <div className="provider-models__add">
        <div className="provider-models__addrow">
          <input
            className="conv-search provider-input"
            type="text"
            value={modelId}
            placeholder="模型 ID，如 deepseek-chat"
            onChange={(e) => setModelId(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); void handleAddManual(); } }}
          />
          <input
            className="conv-search provider-input provider-models__alias"
            type="text"
            value={label}
            placeholder="别名（可选）"
            onChange={(e) => setLabel(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); void handleAddManual(); } }}
          />
          <button
            type="button"
            className="approval-card__btn approval-card__btn--allow"
            onClick={() => void handleAddManual()}
            disabled={adding || !modelId.trim()}
          >
            <Plus size={14} /> 添加
          </button>
          <button type="button" className="approval-card__btn" onClick={handleFetch} disabled={fetching}>
            <Download size={14} /> {fetching ? "拉取中…" : "拉取可用模型"}
          </button>
        </div>

        {/* 拉取结果：快速添加 chip（已登记的标灰禁用）；失败提示在下方。 */}
        {fetchNotice && (
          <p className="settings-desc" style={{ margin: "6px 0 0", color: "var(--warning)" }}>{fetchNotice}</p>
        )}
        {fetched && fetched.length > 0 && (
          <div className="provider-models__chips">
            <span className="provider-models__chips-hint">点击添加（端点返回 {fetched.length} 个）：</span>
            {fetched.map((id) => {
              const already = existingIds.has(id.trim().toLowerCase());
              return (
                <button
                  key={id}
                  type="button"
                  className={`provider-chip${already ? " provider-chip--on" : ""}`}
                  disabled={already || adding}
                  title={already ? "已登记" : `添加 ${id}`}
                  onClick={() => void addModel(id)}
                >
                  {already ? "✓ " : "+ "}{id}
                </button>
              );
            })}
          </div>
        )}
      </div>

      {notice && <p className="settings-row__value" style={{ color: "var(--success)", marginTop: 4 }}>{notice}</p>}
      {error && <p className="settings-row__value" style={{ color: "var(--danger)", marginTop: 4 }}>{error}</p>}
    </div>
  );
}

/** 一条 ModelPricing 的极简单价摘要（输入/命中/输出，符号随币种）：复用 pricingSummary 口径。
 *  ModelPricing 无 _source 元字段，这里转成 StoredPricing 形（_source 仅占位、不影响 pricingSummary）。 */
function modelPricingSummary(p: ModelPricing | undefined): string {
  if (!p) return "—";
  return pricingSummary({ ...p, _source: "custom" });
}

/**
 * 官网单价采集 diff 面板（0.0.73）：软背景、无边框、圆角容器。
 *
 * 结构：头（连接名 + 极简新鲜度）→ 一句说明（N 项有变化）→ 每行（勾选 + 模型名 + 变化徽标 +
 * 旧→新价对比）→ 底部行（左「恢复预设」幽灵；右「应用所选(n)」主按钮 + 「取消」）。
 * change!='unchanged' 的行默认勾选；unchanged 行禁用置灰、不可勾。
 * 应用：把勾选行组装 ApplyItem → onApply（apply_pricing_overrides）；恢复预设 → onReset。
 */
function PricingCapturePanel({
  connectionName,
  result,
  onApply,
  onReset,
  onCancel,
  onError,
}: {
  connectionName: string;
  result: CaptureResult;
  onApply: (items: ReturnType<typeof buildApplyItems>) => Promise<void>;
  onReset: () => Promise<void>;
  onCancel: () => void;
  onError: (msg: string | null) => void;
}) {
  // 默认勾选：change!='unchanged' 的行。unchanged 行不可勾。
  const [selected, setSelected] = useState<Set<string>>(
    () => new Set(result.diffs.filter((d) => d.change !== "unchanged").map(diffKey)),
  );
  const [applying, setApplying] = useState(false);
  const [resetting, setResetting] = useState(false);

  const changedCount = result.diffs.filter((d) => d.change !== "unchanged").length;
  const selectedCount = selected.size;

  function toggle(d: PricingDiff) {
    if (d.change === "unchanged") return; // unchanged 禁用，不可勾。
    const key = diffKey(d);
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }

  async function handleApply() {
    onError(null);
    setApplying(true);
    try {
      const items = buildApplyItems(result.diffs, selected, result.sourceUrl);
      await onApply(items);
    } catch (e) {
      onError(humanizeError(String(e)));
    } finally {
      setApplying(false);
    }
  }

  async function handleReset() {
    onError(null);
    setResetting(true);
    try {
      await onReset();
    } catch (e) {
      onError(humanizeError(String(e)));
    } finally {
      setResetting(false);
    }
  }

  const busy = applying || resetting;

  return (
    <div className="pricing-capture">
      <div className="pricing-capture__head">
        <span className="pricing-capture__name">{connectionName}</span>
        <span className="pricing-capture__fresh">{relativeTime(result.fetchedAt)}</span>
      </div>
      {/* 截断警示（#8）：页面过大被截断时，抽取可能漏采部分模型——显式警示，不以「采集成功」无差别呈现。 */}
      {result.truncated && (
        <p className="pricing-capture__truncated" role="alert">
          页面过大已截断，可能漏采部分模型，请核对官网或手填。
        </p>
      )}
      <p className="pricing-capture__intro">
        {changedCount} 项有变化，勾选后「应用」才写入；不动你已手填的单价。
      </p>

      <ul className="pricing-capture__list">
        {result.diffs.map((d) => {
          const key = diffKey(d);
          const badge = changeBadge(d);
          const unchanged = d.change === "unchanged";
          const checked = selected.has(key);
          return (
            <li
              key={key}
              className={`pricing-capture__row${unchanged ? " pricing-capture__row--disabled" : ""}`}
            >
              <label className="pricing-capture__check">
                <input
                  type="checkbox"
                  checked={checked}
                  disabled={unchanged || busy}
                  onChange={() => toggle(d)}
                />
              </label>
              <span className="pricing-capture__model" title={d.modelId}>
                <code>{d.modelId}</code>
              </span>
              <span className={`pricing-capture__badge pricing-capture__badge--${badge.kind}`}>
                {badge.label}
              </span>
              <span className="pricing-capture__prices">
                {unchanged ? (
                  <span className="pricing-capture__same">不变 {modelPricingSummary(d.newPricing)}</span>
                ) : d.change === "new" ? (
                  <span className={`pricing-capture__new pricing-capture__new--${badge.kind}`}>
                    {modelPricingSummary(d.newPricing)}
                  </span>
                ) : (
                  <>
                    <span className="pricing-capture__old">{modelPricingSummary(d.oldPricing)}</span>
                    <span className="pricing-capture__arrow" aria-hidden="true">→</span>
                    <span className={`pricing-capture__new pricing-capture__new--${badge.kind}`}>
                      {modelPricingSummary(d.newPricing)}
                    </span>
                  </>
                )}
              </span>
            </li>
          );
        })}
      </ul>

      <div className="pricing-capture__foot">
        <button
          type="button"
          className="approval-card__btn pricing-capture__reset"
          disabled={busy}
          title="删除该连接全部采集覆盖，单价回到内置预设快照"
          onClick={() => void handleReset()}
        >
          {resetting ? "恢复中…" : "恢复预设"}
        </button>
        <span className="pricing-capture__foot-right">
          <button type="button" className="approval-card__btn" disabled={busy} onClick={onCancel}>
            取消
          </button>
          <button
            type="button"
            className="approval-card__btn approval-card__btn--allow"
            disabled={busy || selectedCount === 0}
            onClick={() => void handleApply()}
          >
            {applying ? "应用中…" : `应用所选（${selectedCount}）`}
          </button>
        </span>
      </div>
    </div>
  );
}

/**
 * 模型行单价单元（0.0.72）：未编辑态显「单价摘要 + 徽标 + 改单价」；编辑态展开 ModelPricingEditor。
 * 仅 billingMode='api' 的连接会渲染本组件（见调用处）。
 */
function ModelPriceCell({
  model,
  connection,
  editing,
  onBeginEdit,
  onClose,
  onSaved,
  onError,
}: {
  model: CuratedModelView;
  connection: ConnectionView;
  editing: boolean;
  onBeginEdit: () => void;
  onClose: () => void;
  onSaved: () => void;
  onError: (msg: string | null) => void;
}) {
  const stored = parseStoredPricing(model.pricingJson);
  // 显示侧有效价回退（0.0.73，原 0.0.72 预设回退升级）：pricing_json 为空的模型，按 (连接 preset,
  // modelId, CNY) 查 lookup_effective_pricing（采集覆盖优先、编译快照兜底），与后端结算口径一致。
  // 不写库——点「改单价」保存才落库。fallbackSource 记来源以决定徽标（采集→「官网价」｜预设→「预设·可改」）。
  const billingMode = effectiveBillingMode(connection);
  const presetId = connection.preset?.trim();
  const [presetFallback, setPresetFallback] = useState<StoredPricing | null>(null);
  const [fallbackSource, setFallbackSource] = useState<"override" | "preset" | null>(null);
  useEffect(() => {
    let alive = true;
    setPresetFallback(null);
    setFallbackSource(null);
    if (model.pricingJson || billingMode !== "api" || !presetId || presetId === "custom") return;
    invoke<EffectivePricingView | null>("lookup_effective_pricing", {
      connectionPreset: presetId,
      modelId: model.modelId,
      currency: "CNY",
    })
      .then((view) => {
        if (alive && view && view.pricing) {
          setPresetFallback(effectiveToStored(view));
          setFallbackSource(view.source);
        }
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [model.pricingJson, model.modelId, billingMode, presetId]);

  const pricing = stored ?? presetFallback;

  if (editing) {
    return (
      <ModelPricingEditor
        model={model}
        connection={connection}
        initial={pricing}
        onCancel={onClose}
        onSaved={onSaved}
        onError={onError}
      />
    );
  }

  // 徽标：来自采集覆盖的回退价（fallbackSource==='override'）显「官网价」蓝标；其余沿用 pricingBadges
  // （预设·可改 / 待官网核对 / 自定义）。模型自填的 stored 价不携带 override 来源，故仅在回退态生效。
  const showOfficialBadge = !stored && fallbackSource === "override" && !!pricing;
  const badges = showOfficialBadge ? [] : pricingBadges(pricing);
  return (
    <span className="provider-price__summary">
      <span className="provider-price__nums">{pricingSummary(pricing)}</span>
      {showOfficialBadge && (
        <span
          className="provider-price__badge provider-price__badge--official"
          title="官网采集价：已用最近一次抓取的官网现价"
        >
          官网价
        </span>
      )}
      {badges.map((b) => (
        <span
          key={b}
          className={
            "provider-price__badge" +
            (b === "preset" ? " provider-price__badge--preset" : "") +
            (b === "needs_verify" ? " provider-price__badge--verify" : "") +
            (b === "custom" ? " provider-price__badge--custom" : "")
          }
          title={b === "needs_verify" ? "以官网为准：预设价可能已变动，请核对官网定价" : undefined}
        >
          {pricingBadgeLabel(b)}
        </span>
      ))}
      <button
        type="button"
        className="provider-param__value"
        title="改单价"
        onClick={onBeginEdit}
      >
        改单价
        <Pencil size={11} className="provider-param__editicon" />
      </button>
    </span>
  );
}

/** tiers 的草稿行（字符串草稿，保存时解析）。 */
type TierDraft = { maxContext: string; input: string; output: string; cachedInput: string };

/**
 * 模型单价就地编辑器（0.0.72）：货币/单位 select + 输入(未命中)/缓存命中(可空)/输出三个 input；
 * 进阶折叠区含 tiers（可加/删档）+ 缓存写入 + 批量折扣。
 * 保存：价格字段 + _source:'custom' 组装 → JSON.stringify → set_model_pricing。
 * 恢复预设：重新 lookup_model_preset 覆盖回 _source:'preset'。切货币若有该币种预设则重 lookup 填充。
 * 切单位：换算可见数字（×/÷1000）并更新 unit。
 * 表单控件用裸 <select>/<input>，仅内联 style 覆盖 width，不手写 height/font-size。
 */
function ModelPricingEditor({
  model,
  connection,
  initial,
  onCancel,
  onSaved,
  onError,
}: {
  model: CuratedModelView;
  connection: ConnectionView;
  initial: StoredPricing | null;
  onCancel: () => void;
  onSaved: () => void;
  onError: (msg: string | null) => void;
}) {
  // 草稿：用字符串持有数字输入（允许空 = 未填）。来源/元数据单独持有。
  const [currency, setCurrency] = useState<PricingCurrency>(initial?.currency ?? "CNY");
  const [unit, setUnit] = useState<PricingUnit>(initial?.unit ?? "per_1m");
  const [input, setInput] = useState(initial?.input != null ? String(initial.input) : "");
  const [cachedInput, setCachedInput] = useState(initial?.cachedInput != null ? String(initial.cachedInput) : "");
  const [output, setOutput] = useState(initial?.output != null ? String(initial.output) : "");
  const [cacheWrite, setCacheWrite] = useState(initial?.cacheWrite != null ? String(initial.cacheWrite) : "");
  const [batchDiscount, setBatchDiscount] = useState(initial?.batchDiscount != null ? String(initial.batchDiscount) : "");
  const [tiers, setTiers] = useState<TierDraft[]>(
    (initial?.tiers ?? []).map((t) => ({
      maxContext: String(t.maxContext),
      input: String(t.input),
      output: String(t.output),
      cachedInput: t.cachedInput != null ? String(t.cachedInput) : "",
    })),
  );
  // 来源元数据：保存时若任一价格字段被改过 → 转 custom；恢复预设 → preset（+ 元数据）。
  const [source, setSource] = useState<"preset" | "custom">(initial?._source ?? "custom");
  const [confidence, setConfidence] = useState<string | undefined>(initial?._confidence);
  const [needsVerify, setNeedsVerify] = useState<boolean>(!!initial?._needsVerify);
  const [sourceUrl, setSourceUrl] = useState<string | undefined>(initial?._sourceUrl);
  const [advancedOpen, setAdvancedOpen] = useState(
    !!(initial?.tiers?.length || initial?.cacheWrite != null || initial?.batchDiscount != null),
  );
  const [saving, setSaving] = useState(false);
  const [restoring, setRestoring] = useState(false);

  const preset = connection.preset?.trim();
  const canLookupPreset = !!preset && preset !== "custom";

  /** 把字符串草稿解析为数字（空＝null）。非法（非数字）抛出。 */
  function num(s: string): number | null {
    const t = s.trim();
    if (!t) return null;
    const n = Number(t);
    if (Number.isNaN(n)) throw new Error(`「${t}」不是有效数字`);
    return n;
  }

  /** 用一个 ModelPricing（预设或恢复）填充全部草稿字段。 */
  function fillFromPricing(p: ModelPricing) {
    setCurrency(p.currency === "USD" ? "USD" : "CNY");
    setUnit(p.unit === "per_1k" ? "per_1k" : "per_1m");
    setInput(p.input != null ? String(p.input) : "");
    setOutput(p.output != null ? String(p.output) : "");
    setCachedInput(p.cachedInput != null ? String(p.cachedInput) : "");
    setCacheWrite(p.cacheWrite != null ? String(p.cacheWrite) : "");
    setBatchDiscount(p.batchDiscount != null ? String(p.batchDiscount) : "");
    setTiers(
      (p.tiers ?? []).map((t) => ({
        maxContext: String(t.maxContext),
        input: String(t.input),
        output: String(t.output),
        cachedInput: t.cachedInput != null ? String(t.cachedInput) : "",
      })),
    );
  }

  /** 切货币：若 (preset, modelId, 新币种) 有预设则重 lookup 填充，否则仅改 currency 字段（保留用户值）。 */
  async function handleCurrencyChange(next: PricingCurrency) {
    setCurrency(next);
    if (!canLookupPreset) return;
    try {
      const view = await invoke<PresetView | null>("lookup_model_preset", {
        connectionPreset: preset,
        modelId: model.modelId,
        currency: next,
      });
      if (view && view.pricing) {
        fillFromPricing(view.pricing);
        setSource("preset");
        setConfidence(view.confidence);
        setNeedsVerify(view.needsVerify);
        setSourceUrl(view.sourceUrl);
      } else {
        // 目标币种无预设：保留用户已填数字，但徽标须如实显「自定义」——
        // 否则会显「预设」却配着非该币种的数字（与 handleUnitChange 同样的处理）。
        setSource("custom");
        setConfidence(undefined);
        setNeedsVerify(false);
        setSourceUrl(undefined);
      }
    } catch {
      // 静默：lookup 失败时只切 currency。
    }
  }

  /** 切单位：换算可见数字（per_1m↔per_1k ×/÷1000）并改 unit。用户手改了值→视为 custom。 */
  function handleUnitChange(next: PricingUnit) {
    if (next === unit) return;
    const conv = (s: string): string => {
      const t = s.trim();
      if (!t) return "";
      const n = Number(t);
      if (Number.isNaN(n)) return s;
      const v = convertPriceForUnit(n, unit, next);
      return v != null ? String(v) : "";
    };
    setInput(conv(input));
    setCachedInput(conv(cachedInput));
    setOutput(conv(output));
    setCacheWrite(conv(cacheWrite));
    setTiers((ts) => ts.map((t) => ({ ...t, input: conv(t.input), output: conv(t.output), cachedInput: conv(t.cachedInput) })));
    setUnit(next);
    // 单位换算改了数字基准，标记为 custom（避免误显「预设·可改」却与官网 per_1m 数不一致）。
    setSource("custom");
  }

  /** 恢复预设：重 lookup 覆盖回 preset（+ 元数据）。 */
  async function handleRestorePreset() {
    if (!canLookupPreset) return;
    onError(null);
    setRestoring(true);
    try {
      const view = await invoke<PresetView | null>("lookup_model_preset", {
        connectionPreset: preset,
        modelId: model.modelId,
        currency,
      });
      if (!view || !view.pricing) {
        onError("未找到该模型的预设单价（可能预设库未收录），请手动填写。");
        return;
      }
      fillFromPricing(view.pricing);
      setSource("preset");
      setConfidence(view.confidence);
      setNeedsVerify(view.needsVerify);
      setSourceUrl(view.sourceUrl);
    } catch (e) {
      onError(humanizeError(String(e)));
    } finally {
      setRestoring(false);
    }
  }

  /** 标记用户改了价格字段 → custom（绑在各 input 的 onChange 上）。 */
  function markCustom() {
    if (source !== "custom") setSource("custom");
  }

  function updateTier(i: number, patch: Partial<TierDraft>) {
    markCustom();
    setTiers((ts) => ts.map((t, idx) => (idx === i ? { ...t, ...patch } : t)));
  }
  function addTier() {
    markCustom();
    setTiers((ts) => [...ts, { maxContext: "", input: "", output: "", cachedInput: "" }]);
  }
  function removeTier(i: number) {
    markCustom();
    setTiers((ts) => ts.filter((_, idx) => idx !== i));
  }

  /** 保存：组装 StoredPricing（含 _source 与预设元数据）→ set_model_pricing。 */
  async function handleSave() {
    onError(null);
    let parsed: { input: number | null; output: number | null; cachedInput: number | null; cacheWrite: number | null; batchDiscount: number | null };
    try {
      parsed = {
        input: num(input),
        output: num(output),
        cachedInput: num(cachedInput),
        cacheWrite: num(cacheWrite),
        batchDiscount: num(batchDiscount),
      };
    } catch (e) {
      onError(`单价格式有误：${e instanceof Error ? e.message : String(e)}`);
      return;
    }
    if (parsed.input == null || parsed.output == null) {
      onError("请至少填写「输入（未命中）」与「输出」单价。");
      return;
    }
    // 解析 tiers（每档须 maxContext+input+output；忽略整行全空）。
    const tierOut: PricingTier[] = [];
    try {
      for (const t of tiers) {
        if (!t.maxContext.trim() && !t.input.trim() && !t.output.trim() && !t.cachedInput.trim()) continue;
        const mc = num(t.maxContext);
        const ti = num(t.input);
        const to = num(t.output);
        if (mc == null || ti == null || to == null) {
          onError("分档需填写「上下文上限 / 输入 / 输出」；如不需要请删除该档。");
          return;
        }
        const tc = num(t.cachedInput);
        tierOut.push({ maxContext: mc, input: ti, output: to, ...(tc != null ? { cachedInput: tc } : {}) });
      }
    } catch (e) {
      onError(`分档格式有误：${e instanceof Error ? e.message : String(e)}`);
      return;
    }

    // 非负校验（动钱字段不允许负值；折扣须落在 [0,1]）。任一非法即拒绝保存。
    const nonNegError = validatePricingNonNeg(parsed, tierOut);
    if (nonNegError) {
      onError(nonNegError);
      return;
    }

    const stored: StoredPricing = {
      currency,
      unit,
      input: parsed.input,
      output: parsed.output,
      ...(parsed.cachedInput != null ? { cachedInput: parsed.cachedInput } : {}),
      ...(parsed.cacheWrite != null ? { cacheWrite: parsed.cacheWrite } : {}),
      ...(parsed.batchDiscount != null ? { batchDiscount: parsed.batchDiscount } : {}),
      ...(tierOut.length ? { tiers: tierOut } : {}),
      _source: source,
      ...(source === "preset" && confidence ? { _confidence: confidence } : {}),
      ...(source === "preset" && needsVerify ? { _needsVerify: true } : {}),
      ...(source === "preset" && sourceUrl ? { _sourceUrl: sourceUrl } : {}),
    };

    setSaving(true);
    try {
      await invoke<CuratedModelView>("set_model_pricing", {
        modelRef: model.id,
        pricingJson: JSON.stringify(stored),
      });
      onSaved();
    } catch (e) {
      onError(humanizeError(String(e)));
    } finally {
      setSaving(false);
    }
  }

  /** 清空单价（传 null）。 */
  async function handleClear() {
    onError(null);
    setSaving(true);
    try {
      await invoke<CuratedModelView>("set_model_pricing", { modelRef: model.id, pricingJson: null });
      onSaved();
    } catch (e) {
      onError(humanizeError(String(e)));
    } finally {
      setSaving(false);
    }
  }

  const sym = currencySymbol(currency);
  const unitLabel = unitSuffix(unit);

  return (
    <span className="provider-price__edit">
      <div className="provider-price__grid">
        <label className="provider-price__field">
          <span className="provider-price__flabel">货币</span>
          <select value={currency} disabled={saving} onChange={(e) => void handleCurrencyChange(e.target.value === "USD" ? "USD" : "CNY")}>
            <option value="CNY">￥人民币</option>
            <option value="USD">＄美元</option>
          </select>
        </label>
        <label className="provider-price__field">
          <span className="provider-price__flabel">单位</span>
          <select value={unit} disabled={saving} onChange={(e) => handleUnitChange(e.target.value === "per_1k" ? "per_1k" : "per_1m")}>
            <option value="per_1m">每百万 / 1M</option>
            <option value="per_1k">每千 / 1K</option>
          </select>
        </label>
        <label className="provider-price__field">
          <span className="provider-price__flabel">输入（未命中）{sym}{unitLabel}</span>
          <input type="number" min={0} step="any" style={{ width: 90 }} value={input} disabled={saving}
            onChange={(e) => { setInput(e.target.value); markCustom(); }} />
        </label>
        <label className="provider-price__field">
          <span className="provider-price__flabel">缓存命中（可空）{sym}{unitLabel}</span>
          <input type="number" min={0} step="any" style={{ width: 90 }} value={cachedInput} disabled={saving}
            onChange={(e) => { setCachedInput(e.target.value); markCustom(); }} />
        </label>
        <label className="provider-price__field">
          <span className="provider-price__flabel">输出 {sym}{unitLabel}</span>
          <input type="number" min={0} step="any" style={{ width: 90 }} value={output} disabled={saving}
            onChange={(e) => { setOutput(e.target.value); markCustom(); }} />
        </label>
      </div>

      <button type="button" className="provider-advanced-toggle" onClick={() => setAdvancedOpen((v) => !v)}>
        {advancedOpen ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
        进阶（分档定价 / 缓存写入 / 批量折扣）
      </button>
      {advancedOpen && (
        <div className="provider-price__advanced">
          <div className="provider-price__row">
            <label className="provider-price__field">
              <span className="provider-price__flabel">缓存写入（可空）{sym}{unitLabel}</span>
              <input type="number" min={0} step="any" style={{ width: 90 }} value={cacheWrite} disabled={saving}
                onChange={(e) => { setCacheWrite(e.target.value); markCustom(); }} />
            </label>
            <label className="provider-price__field">
              <span className="provider-price__flabel">批量折扣（可空，如 0.5）</span>
              <input type="number" min={0} step="any" style={{ width: 90 }} value={batchDiscount} disabled={saving}
                onChange={(e) => { setBatchDiscount(e.target.value); markCustom(); }} />
            </label>
          </div>

          <div className="provider-price__tiers">
            <span className="provider-price__flabel">上下文分档（长上下文阶梯定价）</span>
            {tiers.map((t, i) => (
              <div key={i} className="provider-price__tier">
                <input type="number" min={0} step="any" style={{ width: 110 }} placeholder="上限 token" value={t.maxContext} disabled={saving}
                  onChange={(e) => updateTier(i, { maxContext: e.target.value })} />
                <input type="number" min={0} step="any" style={{ width: 72 }} placeholder="输入" value={t.input} disabled={saving}
                  onChange={(e) => updateTier(i, { input: e.target.value })} />
                <input type="number" min={0} step="any" style={{ width: 72 }} placeholder="命中" value={t.cachedInput} disabled={saving}
                  onChange={(e) => updateTier(i, { cachedInput: e.target.value })} />
                <input type="number" min={0} step="any" style={{ width: 72 }} placeholder="输出" value={t.output} disabled={saving}
                  onChange={(e) => updateTier(i, { output: e.target.value })} />
                <button type="button" className="provider-models__del" title="删除该档" aria-label="删除该档" onClick={() => removeTier(i)}>
                  <Trash2 size={13} />
                </button>
              </div>
            ))}
            <button type="button" className="approval-card__btn" style={{ marginTop: 4 }} onClick={addTier} disabled={saving}>
              <Plus size={13} /> 加一档
            </button>
          </div>
        </div>
      )}

      {/* 预设来源信息：恢复预设按钮 + 来源链接 + 置信度。 */}
      {canLookupPreset && (
        <div className="provider-price__preset">
          <button type="button" className="approval-card__btn" onClick={() => void handleRestorePreset()} disabled={saving || restoring}>
            <Download size={13} /> {restoring ? "查询中…" : "恢复预设"}
          </button>
          {source === "preset" && sourceUrl && (
            <a className="provider-price__src" href={sourceUrl} target="_blank" rel="noreferrer" title={sourceUrl}>来源</a>
          )}
          {source === "preset" && confidence && (
            <span className="provider-price__conf">置信度 {confidence}</span>
          )}
        </div>
      )}

      <div className="provider-card__actions" style={{ marginTop: 6 }}>
        <button type="button" className="approval-card__btn" onClick={onCancel} disabled={saving}>取消</button>
        <button type="button" className="approval-card__btn" onClick={() => void handleClear()} disabled={saving} style={{ color: "var(--danger)" }}>清空单价</button>
        <button type="button" className="approval-card__btn approval-card__btn--allow" onClick={() => void handleSave()} disabled={saving}>
          {saving ? "保存中…" : "保存单价"}
        </button>
      </div>
    </span>
  );
}

/**
 * 连接编辑器（新增 / 编辑）：名称 + 预设下拉 + Base URL（可选）+ API Key + API 格式。
 *
 * 编辑既有连接（hasKey）时，Key 框占位「已配置 ••••（如需更换请重新输入）」，留空＝保留已存 key；
 * 新建连接必须填 key（客户端拦截空 key）。保存经 save_connection（id 空＝创建）。绝不回显 key 明文。
 */
function ConnectionEditor({
  connection,
  onCancel,
  onSaved,
}: {
  connection?: ConnectionView;
  onCancel: () => void;
  onSaved: () => void;
}) {
  const isNew = !connection;
  const hasKey = connection?.hasKey ?? false;
  const [label, setLabel] = useState(connection?.label ?? "");
  const [preset, setPreset] = useState(connection?.preset ?? "deepseek");
  const [baseUrl, setBaseUrl] = useState(connection?.baseUrl ?? "");
  const [apiKey, setApiKey] = useState("");
  const [keyTouched, setKeyTouched] = useState(false);
  const [apiFormat, setApiFormat] = useState<"openai" | "anthropic">(
    connection?.apiFormat === "anthropic" ? "anthropic" : "openai",
  );
  const [showKey, setShowKey] = useState(false);
  const [advancedOpen, setAdvancedOpen] = useState(!!(connection?.baseUrl) || (connection?.preset === "custom"));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const presetMeta = PROVIDER_PRESETS.find((p) => p.id === preset) ?? PROVIDER_PRESETS[0];
  const isCustom = preset === "custom";

  function handlePresetChange(next: string) {
    setPreset(next);
    // 内置预设默认 OpenAI 格式；custom 保持当前选择。custom 须填 Base URL，自动展开高级行。
    if (next !== "custom") {
      setApiFormat(PRESET_API_FORMAT[next] ?? "openai");
      setAdvancedOpen(false);
      setBaseUrl("");
    } else {
      setAdvancedOpen(true);
    }
  }

  async function handleSave() {
    setError(null);
    if (isCustom && !baseUrl.trim()) {
      setError("自定义供应商必须填写 Base URL");
      setAdvancedOpen(true);
      return;
    }
    // 首次创建（或既有连接尚无密钥）必须填 key；编辑已配密钥连接时留空＝保留。
    if ((isNew || !hasKey) && !apiKey.trim()) {
      setError("请填写 API Key");
      return;
    }
    if (keyTouched && !apiKey.trim() && (isNew || !hasKey)) {
      setError("请填写 API Key");
      return;
    }
    setSaving(true);
    try {
      await invoke("save_connection", {
        id: connection?.id, // 空/缺＝创建
        label: label.trim() || null,
        preset,
        baseUrl: baseUrl.trim() || null,
        // 留空＝保留已存 key（仅编辑既有连接时有效；新建空 key 已被上面拦截）。
        apiKey: apiKey.trim(),
        apiFormat: isCustom ? apiFormat : (PRESET_API_FORMAT[preset] ?? "openai"),
      });
      onSaved();
    } catch (e) {
      setError(humanizeError(String(e)));
    } finally {
      setSaving(false);
    }
  }

  const keyPlaceholder = hasKey && !keyTouched ? "已配置 ••••（如需更换请重新输入）" : "sk-...";

  return (
    <>
      <h3 className="settings-content__h">{isNew ? "新增连接" : "编辑连接"}</h3>
      <div className="provider-card">
        <div className="provider-grid">
          <label className="provider-field">
            <span className="provider-field__label">名称</span>
            <input
              className="conv-search provider-input"
              type="text"
              value={label}
              placeholder={presetMeta.label}
              onChange={(e) => setLabel(e.target.value)}
            />
          </label>
          <label className="provider-field">
            <span className="provider-field__label">供应商预设</span>
            <select className="model-select" value={preset} onChange={(e) => handlePresetChange(e.target.value)}>
              {PROVIDER_PRESETS.map((p) => (
                <option key={p.id} value={p.id}>{p.label}</option>
              ))}
            </select>
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

          {isCustom && (
            <label className="provider-field provider-field--full">
              <span className="provider-field__label">API 格式</span>
              <select
                className="model-select"
                value={apiFormat}
                onChange={(e) => setApiFormat(e.target.value === "anthropic" ? "anthropic" : "openai")}
              >
                <option value="openai">OpenAI 格式（/chat/completions）</option>
                <option value="anthropic">Anthropic 格式（/v1/messages）</option>
              </select>
            </label>
          )}

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
                    ? "自定义供应商须填 Base URL；基址或完整端点均可，不会重复拼接。"
                    : `留空＝用 ${presetMeta.label} 官方端点；自托管/代理可填。`}
                </p>
              </div>
            </div>
          </div>
        </div>

        {error && <p className="settings-row__value" style={{ color: "var(--danger)" }}>{error}</p>}

        <div className="provider-card__actions">
          <button type="button" className="approval-card__btn" onClick={onCancel} disabled={saving}>
            取消
          </button>
          <button type="button" className="approval-card__btn approval-card__btn--allow" onClick={handleSave} disabled={saving}>
            {saving ? "保存中…" : "保存"}
          </button>
        </div>
      </div>
    </>
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
        启停下列语言服务器，决定代码智能（跳转/引用/悬浮/诊断）对哪些语言生效。可选填其<b>二进制路径</b>（不在 PATH 时），留空即自动查找；种类与命令固定，不可新增任意命令。
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

// ── 角色分配（Assignments，0.0.60）───────────────────────────────────────────

/**
 * 角色 → 模型 分配矩阵：每个角色一行，一个**单一下拉**列出全部已登记模型（list_models），
 * 非主角色额外含「跟随主模型」选项。**本页绝无 API Key 输入**，也不再自由填模型 ID——
 * 只能从「模型连接」里登记过的模型中选。
 *
 * 挂载拉 get_role_assignments + list_models；选模型经 set_role_assignment({ role, modelRef })，
 * 「跟随主模型」经 clear_role_assignment（main 不可清，故主行不提供）。
 * 尚无任何已登记模型时，给出「请先到『模型连接』为某个连接添加模型」的引导。
 */
function AssignmentsSettings() {
  const [assignments, setAssignments] = useState<RoleAssignmentView[]>([]);
  const [models, setModels] = useState<CuratedModelView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = () => {
    Promise.all([
      invoke<RoleAssignmentView[]>("get_role_assignments"),
      invoke<CuratedModelView[]>("list_models"),
    ])
      .then(([a, m]) => {
        setAssignments(Array.isArray(a) ? a : []);
        setModels(Array.isArray(m) ? m : []);
      })
      .catch((e) => setError(humanizeError(String(e))))
      .finally(() => setLoading(false));
  };
  useEffect(() => { refresh(); }, []);

  function metaOf(role: string): { label: string; desc: string } {
    const m = ASSIGNABLE_ROLES.find((r) => r.id === role);
    return m ? { label: m.label, desc: m.desc } : { label: role, desc: "" };
  }

  return (
    <>
      <h3 className="settings-content__h">模型分配</h3>
      <p className="settings-desc" style={{ marginTop: 0, marginBottom: 8 }}>
        给每个<b>角色</b>选一个已登记的模型；未分配的<b>跟随主模型</b>。此处只选模型，不录 Key。
      </p>

      {models.length === 0 && !loading ? (
        <p className="settings-row__value" style={{ color: "var(--warning)" }}>
          还没有模型，请先到「<b>模型连接</b>」给某个连接添加模型。
        </p>
      ) : loading ? (
        <p className="settings-row__value">加载中…</p>
      ) : (
        // 按 ASSIGNABLE_ROLES 顺序渲染（缺失的角色用空占位行，理论上后端会全量返回）。
        ASSIGNABLE_ROLES.map((meta) => {
          const a =
            assignments.find((x) => x.role === meta.id) ??
            ({ role: meta.id, enabled: false, effective: { source: "none" as const } } as RoleAssignmentView);
          return (
            <AssignmentRow
              key={meta.id}
              assignment={a}
              title={metaOf(meta.id).label}
              desc={metaOf(meta.id).desc}
              models={models}
              onChanged={refresh}
              onError={setError}
            />
          );
        })
      )}

      {error && <p className="settings-row__value" style={{ color: "var(--danger)" }}>{error}</p>}
    </>
  );
}

/** 一个已登记模型在下拉里的展示文本：「连接名 / modelId」，有别名时附「（别名）」。 */
function curatedOptionLabel(m: CuratedModelView): string {
  const conn = m.connectionLabel?.trim() || "未知连接";
  const alias = m.label && m.label.trim() ? `（${m.label.trim()}）` : "";
  return `${conn} / ${m.modelId}${alias}`;
}

/**
 * 单角色分配行：一个下拉列出全部已登记模型（值＝模型 id / modelRef），非主角色含「跟随主模型」。
 * 选中即保存——选模型调 set_role_assignment({ role, modelRef })；选「跟随主模型」调 clear_role_assignment。
 * main 行：无「跟随主模型」选项（它就是默认；后端也拒绝清 main）。
 */
function AssignmentRow({
  assignment,
  title,
  desc,
  models,
  onChanged,
  onError,
}: {
  assignment: RoleAssignmentView;
  title: string;
  desc: string;
  models: CuratedModelView[];
  onChanged: () => void;
  onError: (msg: string | null) => void;
}) {
  const role = assignment.role;
  const isMain = role === "main";
  const [saving, setSaving] = useState(false);

  const eff = assignment.effective;
  // 当前下拉值：自身引用的 modelRef（有则选中该模型），否则 ""（＝跟随主模型 / 未选）。
  const selected = assignment.modelRef ?? "";
  // 若自身引用的模型已不在清单里（连接被删等），仍补一行占位以免下拉空选。
  const refMissing = !!selected && !models.some((m) => m.id === selected);

  // 「跟随主模型」生效时，把实际生效连接/模型展示出来（source==="main"）。
  const followingMain = !isMain && eff.source === "main";
  const badgeText = isMain
    ? eff.modelId
      ? "● 已配置"
      : "⚠ 未配置"
    : eff.source === "self"
    ? "● 专属模型"
    : eff.source === "main"
    ? "○ 跟随主模型"
    : "⚠ 主模型未配置";

  /** 下拉变更：空值＝跟随主模型（clear_role_assignment）；否则设为该模型（set_role_assignment）。 */
  async function handleSelect(next: string) {
    onError(null);
    setSaving(true);
    try {
      if (!next) {
        // 仅非主角色会出现空值选项；main 无「跟随主模型」。
        await invoke("clear_role_assignment", { role });
      } else {
        await invoke("set_role_assignment", { role, modelRef: next, enabled: true });
      }
      onChanged();
    } catch (e) {
      onError(humanizeError(String(e)));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="provider-card" style={{ marginBottom: 10 }}>
      <div className="provider-card__head">
        <span className="provider-card__title">{title}</span>
        <span className={`provider-badge${eff.source === "self" || (isMain && eff.modelId) ? " provider-badge--on" : ""}`}>
          {badgeText}
        </span>
      </div>
      <p className="settings-desc" style={{ marginTop: 2, marginBottom: 8 }}>
        {desc}
        {followingMain && (eff.connectionLabel || eff.modelId)
          ? `　跟随主模型 → ${eff.connectionLabel ?? "?"} / ${eff.modelId ?? "?"}`
          : ""}
      </p>

      <div className="provider-grid">
        <label className="provider-field provider-field--full">
          <span className="provider-field__label">模型</span>
          <select
            className="model-select"
            value={selected}
            disabled={saving}
            onChange={(e) => void handleSelect(e.target.value)}
          >
            {isMain && selected === "" && <option value="">（请选择模型）</option>}
            {!isMain && <option value="">跟随主模型</option>}
            {models.map((m) => (
              <option key={m.id} value={m.id}>{curatedOptionLabel(m)}</option>
            ))}
            {/* 自身引用的模型已不在清单（连接被删等）：补一行占位，保留可见、可改选别的。 */}
            {refMissing && (
              <option value={selected}>
                {(assignment.connectionLabel?.trim() || "已失效连接") + " / " + (assignment.modelId ?? selected)}（已失效）
              </option>
            )}
          </select>
        </label>
      </div>
    </div>
  );
}

// ── 知识库 / OKF（2b）────────────────────────────────────────────────────────
// 纯前端控制台：私有/共享可见性 + 共享位置/发布 + 导出 OKF 包 + 消费外部包。
// 全用已就绪命令（camelCase）：get_okf_settings / set_okf_settings / okf_external_add|remove
// / okf_export / okf_publish。极简：解释一律走 ⓘ title hover，不堆说明文案。

/** 小 ⓘ 图标，说明走原生 title 悬停（极简，不占行）。 */
function InfoHint({ text }: { text: string }) {
  return (
    <span className="okf-info" title={text} tabIndex={0} role="img" aria-label={text}>
      <Info size={13} />
    </span>
  );
}

function KnowledgeSettings({ activeConvId }: { activeConvId: string | null }) {
  const [okf, setOkf] = useState<OkfSettingsView | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  function refresh() {
    invoke<OkfSettingsView>("get_okf_settings")
      .then((v) => setOkf(v))
      .catch((e) => setError(humanizeError(String(e))));
  }

  // 通知第三栏知识标签 OKF 设置已变（导入/登记/移除/可见性/发布）→ 它据此重新拉取并展示。
  function notifyOkfChanged() {
    window.dispatchEvent(new CustomEvent("okf-changed"));
  }

  // 登记一个外部包后，浏览它给出**精确**反馈：有内容 / 空包 / 读取失败——避免「登记成功但实际没东西」的困惑。
  async function reportBundleStatus(path: string | undefined) {
    if (!path) {
      setNote("已登记，可在「知识」标签查看。");
      return;
    }
    try {
      const b = await invoke<OkfBrowseView>("okf_browse", { conversationId: activeConvId ?? "", source: path });
      const n = b.concepts?.length ?? 0;
      setNote(n === 0
        ? `已登记，但未发现可浏览的知识（可能是空包或不符合 OKF 结构）。位置：${path}`
        : `已导入 ${n} 条知识，见「知识」标签。位置：${path}`);
    } catch {
      setNote(`已登记，但读取失败（目录可能不存在或不可读）。位置：${path}`);
    }
  }

  useEffect(() => {
    refresh();
  }, []);

  /** 一次性落 visibility / sharedLocation / autoPublish 后刷新本地状态。 */
  async function save(next: { visibility?: "private" | "shared"; sharedLocation?: string; autoPublish?: boolean }) {
    if (!okf) return;
    const visibility = next.visibility ?? okf.visibility;
    const sharedLocation = next.sharedLocation ?? okf.sharedLocation ?? "./knowledge";
    const autoPublish = next.autoPublish ?? okf.autoPublish;
    setBusy(true);
    setError(null);
    setNote(null);
    try {
      await invoke("set_okf_settings", { visibility, sharedLocation, autoPublish });
      refresh();
      notifyOkfChanged();
    } catch (e) {
      setError(humanizeError(String(e)));
    } finally {
      setBusy(false);
    }
  }

  async function pickSharedLocation() {
    try {
      const dir = await openDirDialog({ directory: true, multiple: false });
      if (typeof dir !== "string") return;
      await save({ sharedLocation: dir });
    } catch (e) {
      setError(humanizeError(String(e)));
    }
  }

  async function publishNow() {
    if (!activeConvId || busy) return;
    setBusy(true);
    setError(null);
    setNote(null);
    try {
      const target = await invoke<string>("okf_publish", { conversationId: activeConvId });
      setNote(`已发布到：${target}`);
    } catch (e) {
      setError(humanizeError(String(e)));
    } finally {
      setBusy(false);
    }
  }

  async function exportBundle() {
    if (!activeConvId || busy) return;
    setError(null);
    setNote(null);
    try {
      const target = await saveFileDialog({
        defaultPath: "knowledge-okf.zip",
        filters: [{ name: "OKF 包", extensions: ["zip"] }],
      });
      if (typeof target !== "string") return;
      setBusy(true);
      const out = await invoke<string>("okf_export", { conversationId: activeConvId, targetZip: target });
      setNote(`已导出到：${out}`);
    } catch (e) {
      setError(humanizeError(String(e)));
    } finally {
      setBusy(false);
    }
  }

  /** 导入 .zip 包：后端自动解包成同级真实目录并登记。 */
  async function importZip() {
    setError(null);
    setNote(null);
    try {
      const zip = await openDirDialog({
        directory: false,
        multiple: false,
        filters: [{ name: "OKF 包", extensions: ["zip"] }],
      });
      if (typeof zip !== "string") return;
      setBusy(true);
      const prev = okf?.externalBundles ?? [];
      const list = await invoke<string[]>("okf_external_add", { path: zip });
      setOkf((prevState) => (prevState ? { ...prevState, externalBundles: list } : prevState));
      notifyOkfChanged();
      await reportBundleStatus(list.find((p) => !prev.includes(p)));
    } catch (e) {
      setError(humanizeError(String(e)));
    } finally {
      setBusy(false);
    }
  }

  /** 登记一个已存在的 OKF 目录（无需打包，适合本机已解包/共享盘上的包）。 */
  async function addExternalDir() {
    setError(null);
    setNote(null);
    try {
      const dir = await openDirDialog({ directory: true, multiple: false });
      if (typeof dir !== "string") return;
      setBusy(true);
      const prev = okf?.externalBundles ?? [];
      const list = await invoke<string[]>("okf_external_add", { path: dir });
      setOkf((prevState) => (prevState ? { ...prevState, externalBundles: list } : prevState));
      notifyOkfChanged();
      await reportBundleStatus(list.find((p) => !prev.includes(p)));
    } catch (e) {
      setError(humanizeError(String(e)));
    } finally {
      setBusy(false);
    }
  }

  async function removeExternal(path: string) {
    const name = path.replace(/\\/g, "/").split("/").filter(Boolean).pop() ?? path;
    if (!window.confirm(`移除外部包「${name}」？若是导入的 .zip 包，其解包目录会一并删除（不可撤销）。`)) return;
    setError(null);
    setNote(null);
    try {
      const list = await invoke<string[]>("okf_external_remove", { path });
      setOkf((prev) => (prev ? { ...prev, externalBundles: list } : prev));
      notifyOkfChanged();
    } catch (e) {
      setError(humanizeError(String(e)));
    }
  }

  const shared = okf?.visibility === "shared";

  return (
    <div className="okf-settings">
      <div className="settings-content__h" style={{ display: "flex", alignItems: "center", gap: 6 }}>
        <span>知识库</span>
        <InfoHint text="仓库知识以 Google OKF（开放知识格式）厂商中立·Apache-2.0·纯 markdown 存储，可保持私有或共享给其他 agent。" />
      </div>

      {!okf ? (
        <p className="settings-desc">{error ?? "加载中…"}</p>
      ) : (
        <>
          {/* 私有 | 共享 分段开关 */}
          <div className="provider-billing__head" style={{ marginTop: 12 }}>
            <span className="provider-billing__title">可见性</span>
            <span className="provider-billing__seg" role="group" aria-label="知识可见性">
              <button
                type="button"
                className={`provider-billing__segbtn${!shared ? " provider-billing__segbtn--on" : ""}`}
                disabled={busy}
                aria-pressed={!shared}
                title="私有：知识仅留在本项目 .mdga，其他 agent 拿不到。"
                onClick={() => void save({ visibility: "private" })}
              >
                <Lock size={12} style={{ verticalAlign: "-2px", marginRight: 4 }} />私有
              </button>
              <button
                type="button"
                className={`provider-billing__segbtn${shared ? " provider-billing__segbtn--on" : ""}`}
                disabled={busy}
                aria-pressed={shared}
                title="共享：发布为可见的 OKF 包，其他 agent 可读、开放协作。"
                onClick={() => void save({ visibility: "shared" })}
              >
                <Globe size={12} style={{ verticalAlign: "-2px", marginRight: 4 }} />共享
              </button>
            </span>
          </div>

          {/* 共享子选项（仅共享态显） */}
          {shared && (
            <div className="okf-settings__sub">
              <div className="okf-settings__field">
                <span className="okf-settings__field-label">
                  位置
                  <InfoHint text="项目内目录（如 ./knowledge）随仓库版本化；也可选外部目录，嫁接为全局知识库。" />
                </span>
                <span className="okf-settings__field-control">
                  <span className="okf-settings__path" title={okf.sharedLocation ?? "./knowledge"}>
                    {okf.sharedLocation ?? "./knowledge"}
                  </span>
                  <button className="changes-row__revert" type="button" disabled={busy} onClick={() => void pickSharedLocation()}>
                    <FolderOpen size={13} style={{ verticalAlign: "-2px", marginRight: 4 }} />选目录
                  </button>
                </span>
              </div>

              <div className="okf-settings__field">
                <span className="okf-settings__field-label">发布</span>
                <span className="okf-settings__field-control">
                  <button
                    className="changes-row__revert"
                    type="button"
                    disabled={busy || !activeConvId}
                    title={activeConvId ? "把本项目知识立即写入共享目录。" : "打开一个对话后可发布。"}
                    onClick={() => void publishNow()}
                  >
                    <Upload size={13} style={{ verticalAlign: "-2px", marginRight: 4 }} />立即发布
                  </button>
                </span>
              </div>
            </div>
          )}

          {/* 底部一行：导出 OKF 包 */}
          <div className="settings-section__head" style={{ marginTop: 18 }}>
            <span>导出与外部包</span>
          </div>
          <div className="okf-settings__field">
            <span className="okf-settings__field-label">
              导出 OKF 包
              <InfoHint text="把本项目知识打包为单个 .zip 导出（与可见性无关，随时可导出）。便于跨 agent/项目流转。" />
            </span>
            <button
              className="changes-row__revert"
              type="button"
              disabled={busy || !activeConvId}
              title={activeConvId ? "导出为单个 .zip 包。" : "打开一个对话后可导出。"}
              onClick={() => void exportBundle()}
            >
              <Download size={13} style={{ verticalAlign: "-2px", marginRight: 4 }} />导出 .zip
            </button>
          </div>

          {/* 消费外部包：导入 .zip（自动解包）或登记已有目录 */}
          <div className="okf-settings__field" style={{ alignItems: "flex-start" }}>
            <span className="okf-settings__field-label">
              消费外部包
              <InfoHint text="导入其他 agent 导出的 .zip（自动解包到应用数据区、与工作区无关；移除时一并清理），或登记一个已存在的 OKF 目录（用户自管、移除不删），供 agent 浏览引用其知识。" />
            </span>
            <span className="okf-settings__field-control">
              <button className="changes-row__revert" type="button" disabled={busy} title="导入 .zip 包，自动解包后登记。" onClick={() => void importZip()}>
                <Download size={13} style={{ verticalAlign: "-2px", marginRight: 4 }} />导入 .zip
              </button>
              <button className="changes-row__revert" type="button" disabled={busy} title="登记一个已存在的 OKF 目录。" onClick={() => void addExternalDir()}>
                <Plus size={13} style={{ verticalAlign: "-2px", marginRight: 4 }} />目录
              </button>
            </span>
          </div>
          {okf.externalBundles.length === 0 ? (
            <p className="settings-desc">暂无外部包。</p>
          ) : (
            okf.externalBundles.map((p) => (
              <div key={p} className="changes-row">
                <span className="changes-row__path" title={p} style={{ fontFamily: "monospace" }}>{p}</span>
                <button
                  className="changes-row__revert"
                  type="button"
                  title="移除该外部包"
                  aria-label="移除该外部包"
                  onClick={() => void removeExternal(p)}
                >
                  <X size={13} />
                </button>
              </div>
            ))
          )}

          {error && <p className="settings-desc" style={{ color: "var(--danger)" }}>{error}</p>}
          {note && <p className="settings-desc" style={{ color: "var(--success)" }}>{note}</p>}

          <p className="settings-desc okf-settings__footnote" title="严守 OKF v0.1 开放知识格式 · Apache-2.0 许可。">
            严守 OKF v0.1 · Apache-2.0
          </p>
        </>
      )}
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
  activeConvId,
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
  /** 当前活动会话 id（OKF 导出/发布需要 conversationId；无会话则相关按钮禁用）。 */
  activeConvId: string | null;
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
  // 最近被拦动作（Plan27 #9）：进入「权限」页时拉取，每条可一键加 allow/deny 规则。
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
  // 「扩展 agent 的模态」开关：持久化于 settings(modality_extended)；开 → 露出视觉块占位说明。
  const [modalityExtended, setModalityExtended] = useState(false);
  // 主模型当前是否走 DeepSeek 预设（决定账户区 DeepSeek 余额卡片是否显示）。
  // 0.0.59：从「连接库 + 角色分配」推导——取 main 角色生效连接的 preset。
  const [mainIsDeepseek, setMainIsDeepseek] = useState(false);

  /** 重新推导「主模型是否 DeepSeek」：交叉 get_role_assignments 与 list_connections。 */
  const refreshMainPreset = () => {
    Promise.all([
      invoke<RoleAssignmentView[]>("get_role_assignments"),
      invoke<ConnectionView[]>("list_connections"),
      invoke<CuratedModelView[]>("list_models"),
    ])
      .then(([assigns, conns, models]) => {
        const main = assigns.find((a) => a.role === "main");
        // 0.0.60：main 引用的是已登记模型 id（modelRef）；经 list_models 解析出所属连接，再查 preset。
        // 未配＝按默认 deepseek 不误伤。
        const model = main?.modelRef ? models.find((m) => m.id === main.modelRef) : undefined;
        const conn = model ? conns.find((c) => c.id === model.connectionId) : undefined;
        const hasMain = !!main?.effective.modelId;
        setMainIsDeepseek(hasMain && (conn?.preset ?? "deepseek") === "deepseek");
      })
      .catch(() => {});
  };

  useEffect(() => {
    invoke<string | null>("get_app_setting", { key: "modality_extended" })
      .then((v) => setModalityExtended(v === "1"))
      .catch(() => {});
    refreshMainPreset();
  }, []);

  // 进入「权限」页时拉取最近被拦动作（0.0.66：权限规则已并入「权限」页）。
  useEffect(() => {
    if (section !== "permission") return;
    invoke<DeniedAction[]>("recent_denied_actions")
      .then((list) => setDeniedActions(Array.isArray(list) ? list : []))
      .catch(() => setDeniedActions([]));
  }, [section]);

  async function handleToggleModality(next: boolean) {
    setModalityExtended(next);
    await invoke("set_app_setting", { key: "modality_extended", value: next ? "1" : "" }).catch(() => {});
    // 关闭模态：清除视觉角色分配，使图像门禁回到「无视觉」态。
    if (!next) {
      await invoke("clear_role_assignment", { role: "vision" }).catch(() => {});
    }
  }

  const NAV: Array<{ id: typeof section; label: string }> = [
    { id: "account", label: "账户" },
    { id: "connections", label: "模型连接" },
    { id: "assignments", label: "模型分配" },
    { id: "lsp", label: "语言服务器" },
    { id: "permission", label: "权限" },
    { id: "mcp", label: "MCP 服务器" },
    { id: "data", label: "数据" },
    { id: "knowledge", label: "知识库 / OKF" },
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
              <p className="settings-desc">API Key 在「<b>模型连接</b>」选项卡中配置，存于本地，不上传云端。</p>

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

          {section === "connections" && (
            <ConnectionsSettings
              onChanged={() => {
                // 连接变更后刷新主模型预设推导与首屏未配引导（连接本身不决定是否「已配主模型」，
                // 但删/改可能间接影响，保守刷新一次）。
                refreshMainPreset();
                void invoke<RoleAssignmentView[]>("get_role_assignments")
                  .then((a) => onMainConfiguredChange(!!a.find((x) => x.role === "main")?.effective.modelId))
                  .catch(() => {});
              }}
            />
          )}

          {section === "assignments" && (
            <>
              <AssignmentsSettings />

              {/* 扩展模态开关：vision 已是分配矩阵中的一个角色；此开关额外露出音频占位并在关闭时清空视觉分配。 */}
              <label className="toggle provider-modality-toggle">
                <input
                  type="checkbox"
                  checked={modalityExtended}
                  onChange={(e) => handleToggleModality(e.target.checked)}
                />
                <span><b>扩展 agent 的模态</b>　开启后可给「视觉」角色分配识图模型</span>
              </label>

              {modalityExtended && (
                <div className="provider-modality">
                  <div className="provider-card provider-card--disabled">
                    <div className="provider-card__head">
                      <span className="provider-card__title">
                        <Lock size={13} style={{ verticalAlign: "-2px", marginRight: 4 }} />
                        音频（语音）
                      </span>
                      <span className="provider-badge">🔒 敬请期待</span>
                    </div>
                    <p className="settings-desc" style={{ marginTop: 4 }}>
                      后续接入语音模型（占位）。
                    </p>
                  </div>
                </div>
              )}
            </>
          )}

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
                开启后 run_command 在<b>受限令牌沙箱</b>跑：剥离管理员特权、随会话销毁、子进程擦除密钥。少数需特权的命令可能受影响，可临时关闭。
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
              <p className="settings-desc">单次发送累计 token 超此值即暂停本轮，防失控烧钱；按每轮独立计、<b>含视觉识图开销</b>（0 = 不限）。当前 {taskBudget === 0 ? "不限" : taskBudget.toLocaleString()}。</p>
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

          {section === "knowledge" && <KnowledgeSettings activeConvId={activeConvId} />}

          {/* 权限规则并入「权限」页（0.0.66）：与权限模式 / 命令沙箱 / 单轮上限同页,接在其下。 */}
          {section === "permission" && (
            <>
              <h3 className="settings-content__h" style={{ marginTop: 24 }}>权限规则</h3>
              <p className="settings-desc">
                规则按 <b>deny 优先</b>。格式 <code>[allow:|deny:]工具:路径glob</code> / <code>cmd:命令前缀</code> / <code>tool:工具名</code>；例 <code>deny:read_file:**/.env</code>。审批弹窗「总是允许」会自动加 allow 规则。
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
                <p className="settings-desc">暂无被拦动作。工具被权限拦下后会出现在此，可一键加规则。</p>
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
              <p className="settings-desc">接入外部 MCP 服务器，其工具并入模型工具集、统一经权限审批。<b>stdio</b> 填启动命令；<b>HTTP</b> 填 http(s):// 地址 + 可选 Token（留空且需授权时走浏览器 OAuth）。</p>
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
