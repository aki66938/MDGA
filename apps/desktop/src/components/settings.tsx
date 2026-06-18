// 设置弹窗：模型连接库（Connections）+ 加载模型（Models）+ 角色分配（Assignments）+ 其它分类（0.0.60）。

import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronRight, Eye, EyeOff, Plug, Lock, Trash2, Pencil, Wrench, Plus, Download } from "lucide-react";
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
} from "../types";
import { humanizeError } from "../utils";
import { useFocusTrap } from "./dialogs";

// ── 模型连接库（Connections，0.0.59）─────────────────────────────────────────

/** preset → API 格式默认值：内置预设均 OpenAI 兼容；custom 由用户选。 */
const PRESET_API_FORMAT: Record<string, "openai" | "anthropic"> = {
  deepseek: "openai",
  zhipu: "openai",
  moonshot: "openai",
  qwen: "openai",
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
        在这里集中配置可复用的<b>模型连接</b>（端点 + 密钥，配一次即可）。每个连接 = 名称 + 供应商预设 +
        Base URL + API Key + API 格式。这是<b>唯一</b>录入 API Key 的地方。配好后在每个连接卡下的「<b>加载模型</b>」里
        登记你会用到的模型（一个连接可登记多个），再到「模型分配」把各角色指到其中一个模型。
      </p>

      {loading ? (
        <p className="settings-row__value">加载中…</p>
      ) : connections.length === 0 ? (
        <p className="settings-row__value">暂无连接。点击下方「新增连接」配置你的第一个模型供应商。</p>
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

  const refresh = () => {
    setLoading(true);
    invoke<CuratedModelView[]>("list_models_for_connection", { connectionId: connection.id })
      .then((list) => setModels(Array.isArray(list) ? list : []))
      .catch((e) => setError(humanizeError(String(e))))
      .finally(() => setLoading(false));
  };
  useEffect(() => { refresh(); /* eslint-disable-next-line react-hooks/exhaustive-deps */ }, [connection.id]);

  /** 已登记的 modelId 集合（小写归一），用于「拉取」结果里标注哪些已添加。 */
  const existingIds = new Set(models.map((m) => m.modelId.trim().toLowerCase()));

  /** 添加一个模型（手填或来自拉取 chip）。可带 label/contextWindow；add_model 按 连接+modelId 去重。 */
  async function addModel(id: string, opts?: { label?: string; contextWindow?: number }) {
    const trimmed = id.trim();
    if (!trimmed) { setError("请填写模型 ID"); return; }
    setError(null);
    setAdding(true);
    try {
      await invoke<CuratedModelView>("add_model", {
        connectionId: connection.id,
        modelId: trimmed,
        label: opts?.label?.trim() || null,
        contextWindow: opts?.contextWindow ?? null,
      });
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

  return (
    <div className="provider-models">
      <div className="provider-models__head">
        <span className="provider-models__title">加载模型</span>
        <span className="provider-models__count">{models.length} 个</span>
      </div>

      {loading ? (
        <p className="settings-row__value" style={{ margin: "2px 0" }}>加载中…</p>
      ) : models.length === 0 ? (
        <p className="settings-desc" style={{ margin: "2px 0 6px" }}>
          还没有登记模型。点「拉取可用模型」自动获取，或在下方手动添加。
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
                          className="conv-search provider-input provider-param__input"
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
                          className="approval-card__btn approval-card__btn--allow provider-param__btn"
                          disabled={saving}
                          onClick={() => void saveCtx(m)}
                        >
                          {saving ? "保存中…" : "保存"}
                        </button>
                        <button
                          type="button"
                          className="approval-card__btn provider-param__btn"
                          disabled={saving}
                          onClick={cancelEditCtx}
                        >
                          取消
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
                    ? "自定义供应商必须填写 Base URL（自托管/代理）。基址或完整端点均可，照 API 文档粘贴即可，不会重复拼接路径。"
                    : `留空即使用 ${presetMeta.label} 官方端点；自托管/代理可填。`}
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
        把每个<b>角色</b>指到一个<b>已登记的模型</b>（在「模型连接」的「加载模型」里登记）。未单独分配的角色自动<b>跟随主模型</b>。
        此处只选模型，<b>不输入任何 API Key</b>（密钥在「模型连接」配）。
      </p>

      {models.length === 0 && !loading ? (
        <p className="settings-row__value" style={{ color: "var(--warning)" }}>
          还没有任何已登记的模型。请先到「<b>模型连接</b>」为某个连接添加模型，再回来分配角色。
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
                <span><b>扩展 agent 的模态</b>　开启后可为「视觉」角色分配识图模型（上方矩阵）</span>
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
                      后续接入语音模型，实现语音对话交流（占位禁用）。
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
