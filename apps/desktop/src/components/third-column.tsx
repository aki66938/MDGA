// 常驻第三栏（停靠式右栏）骨架：折叠/展开两档 + 标签框架（活动 / 变更）。
//
// 本期范围（骨架）：布局轨位、折叠交互、标签切换、通知点、变更标签常驻化（复用 ChangesView）。
// 「活动」标签仅占位——真实活动逻辑（list_bg_activity / 监听 background-task-done 等）由下一个子 agent 接入。
// 不碰后端、不碰 artifact/思考深度相关代码。
//
// 宽度两档：折叠 ~44px 细栏 / 展开 ~340px（见 styles.css 的 .third-col 与 .app-shell 第三轨）。
// TODO(拖宽)：本期固定两档宽度；后续可加拖拽手柄在展开态自由调宽（CSS 变量 --third-col-w 已预留思路）。

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
import { save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import {
  ListChecks, GitCompare, Gauge, ChevronLeft, ChevronRight, Maximize2,
  Bot, Terminal, Square, Check, CircleDot, ChevronDown, ChevronRight as ChevronRightSm, CheckSquare,
  FolderTree, Folder, FolderOpen, File as FileIcon, ArrowLeft, LayoutDashboard, X,
  BookOpen, Lock, Globe, FolderOutput, FileText, RefreshCw, Pencil, Undo2,
} from "lucide-react";
import type {
  FileCheckpoint, FileChange, BgActivityView, TodoItem, UsageSummary, ToolUsageView,
  UsageAttributionView, ArtifactPart, OkfSettingsView, OkfBrowseView, OkfConceptView,
  OkfConceptSourceView,
} from "../types";
import { fmtTokens, formatMoney } from "../utils";
import { ChangesView } from "./dialogs";
import { ConversationUsageSummary, DiffBlock, Markdown } from "./messages";
import { ArtifactCard } from "./artifact";

export type ThirdColTab = "activity" | "changes" | "usage" | "files" | "artifact" | "knowledge";

// 运行时长近似显示：list 不含开始时间，故以「该 id 首次出现」到「now（完成项定格在完成时刻）」计算。
function fmtDuration(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  const rem = s % 60;
  if (m < 60) return `${m}m${rem ? ` ${rem}s` : ""}`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}

const ACTIVE_STATUSES = new Set(["done", "killed", "error"]);

// ── 活动标签：顶部 todo/计划进度 + 后台活动列表 ────────────────────────────────
// 数据源：list_bg_activity(activeConvId) 定时轮询(~2s) + 后台事件即时刷新一次；折叠/非活动标签不渲染（停轮询）。
// 时长：useRef Map 记每个 id 首次出现时间戳；完成项定格其「最后一次仍 running」附近的时刻（近似）。
function ActivityPanel({
  activeConvId,
  todos,
}: {
  activeConvId: string | null;
  todos: TodoItem[];
}) {
  const [items, setItems] = useState<BgActivityView[]>([]);
  // 展开输出：当前展开的项 key（`${kind}:${id}`）→ 输出文本；null 表示未展开。
  const [openOutput, setOpenOutput] = useState<Record<string, string | null>>({});
  // 记每个 id 首次出现时间戳（id→startMs）；完成项定格其完成时刻（id→endMs）。
  const startRef = useRef<Map<string, number>>(new Map());
  const endRef = useRef<Map<string, number>>(new Map());
  // 让运行时长每秒走字：定时 bump 触发重渲染（不依赖轮询）。
  const [, setTick] = useState(0);

  // 拉一次活动列表，并维护 start/end 时间戳簿。
  async function refresh() {
    if (!activeConvId) {
      setItems([]);
      return;
    }
    const list = await invoke<BgActivityView[]>("list_bg_activity", {
      conversationId: activeConvId,
    }).catch(() => [] as BgActivityView[]);
    const now = Date.now();
    for (const it of list) {
      if (!startRef.current.has(it.id)) startRef.current.set(it.id, now);
      if (ACTIVE_STATUSES.has(it.status)) {
        // 首次见到完成态时定格完成时刻（后续轮询不再更新）。
        if (!endRef.current.has(it.id)) endRef.current.set(it.id, now);
      }
    }
    setItems(list);
  }

  // 切会话即清时间簿与展开态，避免跨会话串味。
  useEffect(() => {
    startRef.current.clear();
    endRef.current.clear();
    setOpenOutput({});
    setItems([]);
    void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeConvId]);

  // ~2s 轮询（仅本面板挂载时，即第三栏展开且在活动标签）。
  useEffect(() => {
    const id = window.setInterval(() => void refresh(), 2000);
    return () => window.clearInterval(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeConvId]);

  // 后台事件即时刷新一次（与轮询互补，缩短感知延迟）。
  useEffect(() => {
    const names = [
      "background-task-done",
      "background-command-done",
      "command-output",
      "tool-event",
    ];
    const unlistens = names.map((n) => listen(n, () => void refresh()));
    return () => {
      for (const u of unlistens) u.then((fn) => fn());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeConvId]);

  // 有运行中项时每秒走字（运行时长）。
  useEffect(() => {
    if (!items.some((it) => it.status === "running")) return;
    const id = window.setInterval(() => setTick((t) => t + 1), 1000);
    return () => window.clearInterval(id);
  }, [items]);

  async function handleKill(it: BgActivityView) {
    await invoke("kill_bg_activity", { id: it.id, kind: it.kind }).catch(() => {});
    void refresh();
  }

  async function toggleOutput(it: BgActivityView) {
    const key = `${it.kind}:${it.id}`;
    if (key in openOutput) {
      // 已展开 → 收起。
      setOpenOutput((prev) => {
        const next = { ...prev };
        delete next[key];
        return next;
      });
      return;
    }
    setOpenOutput((prev) => ({ ...prev, [key]: null })); // null = 加载中
    const out = await invoke<string | null>("get_bg_activity_output", {
      id: it.id,
      kind: it.kind,
    }).catch(() => null);
    setOpenOutput((prev) => ({ ...prev, [key]: out ?? "" }));
  }

  function durationFor(it: BgActivityView): string {
    const start = startRef.current.get(it.id);
    if (start == null) return "";
    const end = ACTIVE_STATUSES.has(it.status) ? endRef.current.get(it.id) ?? Date.now() : Date.now();
    return fmtDuration(end - start);
  }

  // 运行中置顶、完成项置底（弱化）。
  const running = items.filter((it) => it.status === "running");
  const finished = items.filter((it) => it.status !== "running");
  const ordered = [...running, ...finished];

  const todoDone = todos.filter((t) => t.status === "done").length;

  return (
    <div className="third-col__activity">
      {/* 1) todo/计划进度小区块（无 todo 则整块隐藏，不硬造）。 */}
      {todos.length > 0 && (
        <div className="activity-todo">
          <div className="activity-todo__head">任务清单 {todoDone}/{todos.length}</div>
          <div className="activity-todo__items">
            {todos.slice(0, 5).map((t, i) => (
              <div key={i} className={`activity-todo__item activity-todo__item--${t.status}`}>
                {t.status === "done" ? <CheckSquare size={13} />
                  : t.status === "in_progress" ? <CircleDot size={13} />
                  : <Square size={13} />}
                <span>{t.text}</span>
              </div>
            ))}
            {todos.length > 5 && (
              <div className="activity-todo__more">还有 {todos.length - 5} 项…</div>
            )}
          </div>
        </div>
      )}

      {/* 2) 后台活动列表。 */}
      <div className="activity-list">
        {ordered.length === 0 ? (
          <div className="activity-empty">暂无后台活动</div>
        ) : (
          ordered.map((it) => {
            const key = `${it.kind}:${it.id}`;
            const outOpen = key in openOutput;
            const out = openOutput[key];
            const KindIcon = it.kind === "subagent" ? Bot : Terminal;
            return (
              <div
                key={key}
                className={`activity-item activity-item--${it.status}${it.status !== "running" ? " activity-item--finished" : ""}`}
              >
                <div className="activity-item__row">
                  <span className={`activity-item__status activity-item__status--${it.status}`} aria-hidden="true">
                    {it.status === "running" ? <span className="activity-item__dot" />
                      : it.status === "done" ? <Check size={13} />
                      : <span className="activity-item__x">×</span>}
                  </span>
                  <KindIcon size={13} className="activity-item__kind" aria-hidden="true" />
                  <button
                    className="activity-item__label"
                    type="button"
                    onClick={() => void toggleOutput(it)}
                    title="展开输出"
                  >
                    <span className="activity-item__text">{it.label}</span>
                    <ChevronDown
                      size={12}
                      className={`activity-item__caret${outOpen ? " is-open" : ""}`}
                      aria-hidden="true"
                    />
                  </button>
                  <span className="activity-item__dur">{durationFor(it)}</span>
                  {it.status === "running" && (
                    <button
                      className="icon-btn activity-item__kill"
                      type="button"
                      title="停止"
                      aria-label="停止后台活动"
                      onClick={() => void handleKill(it)}
                    >
                      <Square size={13} />
                    </button>
                  )}
                </div>
                {outOpen && (
                  <pre className="activity-item__output">
                    {out === null ? "加载中…" : out.trim().length > 0 ? out : "（暂无输出）"}
                  </pre>
                )}
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}

// ── 第三栏·变更标签上半段：本会话文件累计改动 ──────────────────────────────────
// 数据源：App 从消息流里已有的 diff 卡聚合出的 fileChanges（ToolPart 的 diff/added/removed 按文件路径聚合），
// 与下半段「检查点时间线」（ChangesView，回退用）分工：上=看改了什么，下=回退到哪。
// 行内 diff 展开复用 messages.tsx 的 DiffBlock（unified diff 高亮），不复制一份高亮逻辑。
// 列表为空（本会话无文件改动）则整段不渲染，只留时间线。
function FileChangesSection({ fileChanges }: { fileChanges: FileChange[] }) {
  // 当前展开行内 diff 的文件路径（key=path）。
  const [openPaths, setOpenPaths] = useState<Record<string, boolean>>({});
  if (fileChanges.length === 0) return null;

  const totalAdded = fileChanges.reduce((s, f) => s + f.added, 0);
  const totalRemoved = fileChanges.reduce((s, f) => s + f.removed, 0);

  return (
    <div className="file-changes">
      <div className="file-changes__summary" aria-label="文件累计改动摘要">
        <span className="file-changes__count">{fileChanges.length} 个文件</span>
        <span className="file-changes__stats">
          <span className="diff-added">+{totalAdded}</span>
          {" "}
          <span className="diff-removed">−{totalRemoved}</span>
        </span>
      </div>
      <div className="file-changes__list">
        {fileChanges.map((f) => {
          const isOpen = !!openPaths[f.path];
          const canExpand = f.diffs.length > 0;
          return (
            <div
              key={f.path}
              className={`file-change-row${f.reverted ? " file-change-row--reverted" : ""}`}
            >
              <button
                className="file-change-row__head"
                type="button"
                aria-expanded={isOpen}
                disabled={!canExpand}
                title={canExpand ? "展开行内 diff" : f.path}
                onClick={
                  canExpand
                    ? () => setOpenPaths((prev) => ({ ...prev, [f.path]: !prev[f.path] }))
                    : undefined
                }
              >
                <span className="file-change-row__caret" aria-hidden="true">
                  {canExpand ? (isOpen ? <ChevronDown size={12} /> : <ChevronRightSm size={12} />) : null}
                </span>
                <span className="file-change-row__path" title={f.path}>{f.path}</span>
                {f.reverted && <span className="file-change-row__reverted">已回退</span>}
                <span className="file-change-row__stats">
                  {f.added ? <span className="diff-added">+{f.added}</span> : null}
                  {f.added && f.removed ? " " : null}
                  {f.removed ? <span className="diff-removed">−{f.removed}</span> : null}
                </span>
              </button>
              {isOpen && canExpand && (
                <div className="file-change-row__diffs">
                  {f.diffs.map((d, i) => (
                    <DiffBlock key={i} diff={d} />
                  ))}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

// consumerType → 友好名（小字 consumerLabel 由调用处单独渲染）。
const CONSUMER_TYPE_LABEL: Record<UsageAttributionView["consumerType"], string> = {
  main: "主模型",
  vision: "视觉",
  subagent: "子代理",
};

// ── 第三栏·用量标签：上下文环 + 本会话真账单 + 按角色/子代理真账单 + 按工具活动量（近似）──────
// 四段数据源全部复用或走只读命令，纯前端、不碰后端账单：
//   1) 上下文环：复用 App 的 ctxUsage（promptTokens / softLimit）。softLimit 为 null 只显 token、不显百分比。
//   2) 本会话用量：复用 <ConversationUsageSummary>（aggregateUsage 求和 + aggregateCost 真账单按币种）。
//   3) 按角色/子代理（真账单）：拉 get_usage_attribution(activeConvId)，每个消费者按它**自己模型**单价结算；
//      与第 2 段「本会话总」（按主模型单价估）口径不同，子代理/视觉用了别的单价模型时两者略有出入属正常。
//   4) 按工具活动：拉 get_tool_usage(activeConvId)，条形列表显「调用次数 / 输出体积」——**上下文贡献近似、非账单**。
// 刷新：本面板挂载时（即第三栏展开且在用量标签）拉一次；监听 tool-event 节流(~800ms)重拉工具段，
//   监听 chat-usage / chat-done（新 usage 才变）重拉归因段；折叠/非本标签不挂载即不拉。卸载清定时器与监听。
function UsagePanel({
  activeConvId,
  ctxUsage,
  conversationUsage,
  turnUsages,
}: {
  activeConvId: string | null;
  ctxUsage: { promptTokens: number; softLimit: number | null } | null;
  conversationUsage: UsageSummary | null;
  turnUsages: UsageSummary[];
}) {
  const [toolUsage, setToolUsage] = useState<ToolUsageView[]>([]);
  // 按角色/子代理真账单归因（get_usage_attribution；新 usage 产生才变）。
  const [attribution, setAttribution] = useState<UsageAttributionView[]>([]);
  // tool-event 节流：避免一连串工具事件高频拉取；尾沿落地一次（工具结束才变值）。
  const throttleRef = useRef<number | null>(null);

  async function refreshTools() {
    if (!activeConvId) {
      setToolUsage([]);
      return;
    }
    const list = await invoke<ToolUsageView[]>("get_tool_usage", {
      conversationId: activeConvId,
    }).catch(() => [] as ToolUsageView[]);
    setToolUsage(list);
  }

  // 真账单归因：后端已按 totalTokens 降序聚合，前端只展示，不再排序/估算。
  async function refreshAttribution() {
    if (!activeConvId) {
      setAttribution([]);
      return;
    }
    const list = await invoke<UsageAttributionView[]>("get_usage_attribution", {
      conversationId: activeConvId,
    }).catch(() => [] as UsageAttributionView[]);
    setAttribution(list);
  }

  // 挂载 / 切会话即各拉一次（折叠或非本标签时本组件不挂载，自然不拉）。
  useEffect(() => {
    void refreshTools();
    void refreshAttribution();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeConvId]);

  // 监听 tool-event 节流重拉工具段（尾沿 ~800ms）；卸载清定时器与监听。
  useEffect(() => {
    const schedule = () => {
      if (throttleRef.current != null) return;
      throttleRef.current = window.setTimeout(() => {
        throttleRef.current = null;
        void refreshTools();
      }, 800);
    };
    const un = listen("tool-event", schedule);
    return () => {
      if (throttleRef.current != null) {
        window.clearTimeout(throttleRef.current);
        throttleRef.current = null;
      }
      un.then((fn) => fn());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeConvId]);

  // 新 usage 产生（chat-usage / chat-done）才重拉归因段——账单只随真实计费轮变化，无需节流高频拉。
  useEffect(() => {
    const names = ["chat-usage", "chat-done"];
    const unlistens = names.map((n) => listen(n, () => void refreshAttribution()));
    return () => {
      for (const u of unlistens) u.then((fn) => fn());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeConvId]);

  // 上下文环：百分比仅在 softLimit 非空时算（别硬造）。
  const ctxPct =
    ctxUsage && ctxUsage.softLimit != null && ctxUsage.softLimit > 0
      ? Math.min(100, Math.round((ctxUsage.promptTokens / ctxUsage.softLimit) * 100))
      : null;

  // 按工具活动量条形归一：以最大 outputTokens 为满格（全 0 时退而用 calls 归一）。
  const maxOut = toolUsage.reduce((m, t) => Math.max(m, t.outputTokens), 0);
  const maxCalls = toolUsage.reduce((m, t) => Math.max(m, t.calls), 0);

  // 归因段条形归一：以最大 totalTokens 为满格（后端已降序，故首行即满格）。
  const maxAttrTokens = attribution.reduce((m, a) => Math.max(m, a.totalTokens), 0);

  return (
    <div className="third-col__usage">
      {/* 1) 上下文环（复用 ctxUsage）。 */}
      <div className="usage-section">
        <div className="usage-section__title">上下文</div>
        {ctxUsage ? (
          <div className="usage-ring-row">
            <div
              className="usage-ring"
              style={{
                background:
                  ctxPct != null
                    ? `conic-gradient(var(--brand) ${ctxPct * 3.6}deg, var(--border) 0deg)`
                    : `conic-gradient(var(--brand) 0deg, var(--border) 0deg)`,
              }}
              role="img"
              aria-label={
                ctxPct != null
                  ? `上下文占用 ${ctxPct}%`
                  : `上下文 ${fmtTokens(ctxUsage.promptTokens)} tokens`
              }
            >
              <div className="usage-ring__hole">
                <span className="usage-ring__main">
                  {ctxPct != null ? `${ctxPct}%` : fmtTokens(ctxUsage.promptTokens)}
                </span>
                <span className="usage-ring__sub">上下文</span>
              </div>
            </div>
            <div className="usage-ring-meta">
              <div className="usage-ring-meta__nums">
                {ctxUsage.promptTokens.toLocaleString()}
                {ctxUsage.softLimit != null && (
                  <>
                    {" / "}
                    {ctxUsage.softLimit.toLocaleString()}
                  </>
                )}
                {" tokens"}
              </div>
              <div className="usage-ring-meta__hint">
                {ctxUsage.softLimit != null
                  ? "上次请求占主模型上下文窗口的比例"
                  : "上次请求 prompt token 数（主模型未填上下文窗口，不显百分比）"}
              </div>
            </div>
          </div>
        ) : (
          <div className="usage-empty">暂无上下文数据（发送一轮后显示）</div>
        )}
      </div>

      {/* 2) 本会话用量（真账单，复用 ConversationUsageSummary）。 */}
      <div className="usage-section">
        <div className="usage-section__title">本会话用量</div>
        {conversationUsage ? (
          <ConversationUsageSummary aggregated={conversationUsage} usages={turnUsages} />
        ) : (
          <div className="usage-empty">本会话暂无用量记录</div>
        )}
      </div>

      {/* 3) 按角色/子代理（真账单）：每个消费者按它自己模型单价结算。空列表整段隐藏。 */}
      {attribution.length > 0 && (
        <div className="usage-section">
          <div className="usage-section__title">
            按角色 / 子代理（真账单）
            <span className="usage-section__note">（各按自身模型单价）</span>
          </div>
          <div className="usage-tools">
            {attribution.map((a, i) => {
              const typeLabel = CONSUMER_TYPE_LABEL[a.consumerType] ?? a.consumerType;
              const pct = maxAttrTokens > 0 ? (a.totalTokens / maxAttrTokens) * 100 : 0;
              // 成本：有金额（estimatedCost>0 且 currency 非空）才显金额，否则显「—」。
              const hasCost = a.estimatedCost != null && a.estimatedCost > 0 && a.currency != null;
              return (
                <div className="usage-tool" key={`${a.consumerType}:${a.consumerLabel ?? ""}:${i}`}>
                  <div className="usage-tool__head">
                    <span className="usage-tool__name" title={a.modelId ?? typeLabel}>
                      {typeLabel}
                      {a.consumerLabel && <span className="usage-tool__sub">{a.consumerLabel}</span>}
                    </span>
                    <span className="usage-tool__nums">
                      {fmtTokens(a.totalTokens)} ·{" "}
                      {hasCost ? formatMoney(a.estimatedCost as number, a.currency ?? undefined) : "—"}
                    </span>
                  </div>
                  <div className="usage-tool__bar" aria-hidden="true">
                    <span className="usage-tool__fill" style={{ width: `${pct}%` }} />
                  </div>
                </div>
              );
            })}
          </div>
          <div className="usage-section__note">
            真账单按各消费者自身模型单价结算，与上方「本会话用量」（按主模型单价估）可能略有出入。
          </div>
        </div>
      )}

      {/* 4) 按工具活动量（近似、非账单）。 */}
      <div className="usage-section">
        <div className="usage-section__title">
          按工具：调用次数 / 输出体积
          <span className="usage-section__note">（上下文贡献近似，非账单）</span>
        </div>
        {toolUsage.length === 0 ? (
          <div className="usage-empty">本会话暂无工具调用</div>
        ) : (
          <div className="usage-tools">
            {toolUsage.map((t) => {
              const isSub = t.toolName.startsWith("sub:");
              const name = isSub ? t.toolName.slice(4) : t.toolName;
              const pct =
                maxOut > 0
                  ? (t.outputTokens / maxOut) * 100
                  : maxCalls > 0
                  ? (t.calls / maxCalls) * 100
                  : 0;
              return (
                <div className="usage-tool" key={t.toolName}>
                  <div className="usage-tool__head">
                    <span className="usage-tool__name" title={t.toolName}>
                      {name}
                      {isSub && <span className="usage-tool__sub">子代理</span>}
                    </span>
                    <span className="usage-tool__nums">
                      {t.calls} 次 · {fmtTokens(t.outputTokens)}
                    </span>
                  </div>
                  <div className="usage-tool__bar" aria-hidden="true">
                    <span className="usage-tool__fill" style={{ width: `${pct}%` }} />
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

// ── 第三栏·文件标签：只读工作区文件树 + 单窗格文件查看 ───────────────────────────
// 数据源（只读、复用已就绪后端命令，不碰后端）：
//   list_workspace_dir(conv, relPath) -> {name,isDir}[]（""=根；惰性一层；目录在前名称升序；已过滤 .git/node_modules）
//   read_workspace_file(conv, relPath) -> string（只读、≤512KB，超限/二进制 Err）
// 状态：树态（默认，可展开/收起文件夹，惰性拉子层并缓存）/ 查看态（单窗格内打开一个文件，返回回树）。
// agent 改过高亮：fileChanges 里的路径集合，命中的树节点加品牌色点。
// 加载策略：仅本标签挂载时（第三栏展开且 tab==="files"）才拉；切会话时由 key 重挂载，缓存与查看态自然清。
type DirEntry = { name: string; isDir: boolean };

function FilesPanel({
  activeConvId,
  fileChanges,
}: {
  activeConvId: string | null;
  fileChanges: FileChange[];
}) {
  // 已拉取的目录层缓存：relPath（""=根）→ 该层条目；未拉取的键不存在。
  const [dirCache, setDirCache] = useState<Record<string, DirEntry[]>>({});
  // 正在加载中的目录 relPath 集合（占位文案 + 防重复并发拉取）。
  const [loadingDirs, setLoadingDirs] = useState<Record<string, boolean>>({});
  // 拉取出错的目录 relPath → 错误文案。
  const [dirErrors, setDirErrors] = useState<Record<string, string>>({});
  // 已展开的文件夹 relPath 集合。
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  // 查看态：选中文件 relPath（null=树态）+ 内容 + 加载/错误。
  const [viewPath, setViewPath] = useState<string | null>(null);
  const [viewContent, setViewContent] = useState<string | null>(null);
  const [viewLoading, setViewLoading] = useState(false);
  const [viewError, setViewError] = useState<string | null>(null);

  // agent 改过的文件路径集合（用 fileChanges 的 path，与 list_workspace_dir 返回的工作区相对路径口径一致）。
  // 统一分隔符为 "/" 以容忍后端可能的反斜杠路径。
  const changedSet = (() => {
    const s = new Set<string>();
    for (const f of fileChanges) {
      if (f.path && f.path !== "(未知文件)") s.add(f.path.replace(/\\/g, "/"));
    }
    return s;
  })();

  // 拉取某一层目录（已缓存 / 加载中则跳过）。relPath ""=根。
  async function loadDir(relPath: string) {
    if (dirCache[relPath] || loadingDirs[relPath]) return;
    setLoadingDirs((p) => ({ ...p, [relPath]: true }));
    setDirErrors((p) => { const n = { ...p }; delete n[relPath]; return n; });
    try {
      const list = await invoke<DirEntry[]>("list_workspace_dir", {
        conversationId: activeConvId,
        relPath,
      });
      setDirCache((p) => ({ ...p, [relPath]: Array.isArray(list) ? list : [] }));
    } catch (err) {
      setDirErrors((p) => ({ ...p, [relPath]: humanizeFsError(String(err)) }));
    } finally {
      setLoadingDirs((p) => { const n = { ...p }; delete n[relPath]; return n; });
    }
  }

  // 挂载（即切到本标签 / 切会话重挂载）时拉根层一次。
  useEffect(() => {
    void loadDir("");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  function toggleDir(relPath: string) {
    const willOpen = !expanded[relPath];
    setExpanded((p) => ({ ...p, [relPath]: willOpen }));
    if (willOpen) void loadDir(relPath); // 惰性：首次展开才拉子层
  }

  async function openFile(relPath: string) {
    setViewPath(relPath);
    setViewContent(null);
    setViewError(null);
    setViewLoading(true);
    try {
      const text = await invoke<string>("read_workspace_file", {
        conversationId: activeConvId,
        relPath,
      });
      setViewContent(text);
    } catch {
      // 后端对过大/二进制/越界统一返回 Err；给友好提示，不暴露原始错误细节。
      setViewError("无法预览（文件过大、二进制或不可读）");
    } finally {
      setViewLoading(false);
    }
  }

  function backToTree() {
    setViewPath(null);
    setViewContent(null);
    setViewError(null);
  }

  // 查看态：单窗格内显示返回按钮 + 路径 + 只读内容。
  if (viewPath !== null) {
    return (
      <div className="files-panel files-panel--view">
        <div className="files-view__head">
          <button
            className="icon-btn files-view__back"
            type="button"
            title="返回文件树"
            aria-label="返回文件树"
            onClick={backToTree}
          >
            <ArrowLeft size={15} />
          </button>
          <span className="files-view__path" title={viewPath}>{viewPath}</span>
        </div>
        {viewLoading ? (
          <div className="files-empty">加载中…</div>
        ) : viewError ? (
          <div className="files-empty">{viewError}</div>
        ) : (
          <pre className="files-view__content">{viewContent ?? ""}</pre>
        )}
      </div>
    );
  }

  // 树态：递归渲染目录层。根层 relPath="".
  const rootEntries = dirCache[""];
  const rootLoading = loadingDirs[""];
  const rootError = dirErrors[""];

  return (
    <div className="files-panel">
      {rootError ? (
        <div className="files-empty">{rootError}</div>
      ) : rootLoading && !rootEntries ? (
        <div className="files-empty">加载中…</div>
      ) : rootEntries && rootEntries.length === 0 ? (
        <div className="files-empty">工作区为空</div>
      ) : rootEntries ? (
        <div className="files-tree" role="tree">
          {rootEntries.map((e) => (
            <FileNode
              key={e.name}
              entry={e}
              relPath={e.name}
              depth={0}
              dirCache={dirCache}
              loadingDirs={loadingDirs}
              dirErrors={dirErrors}
              expanded={expanded}
              changedSet={changedSet}
              onToggleDir={toggleDir}
              onOpenFile={openFile}
            />
          ))}
        </div>
      ) : (
        <div className="files-empty">加载中…</div>
      )}
    </div>
  );
}

// 单个树节点（目录可展开/收起惰性拉子层；文件可点开查看）。缩进按 depth 表层级。
function FileNode({
  entry,
  relPath,
  depth,
  dirCache,
  loadingDirs,
  dirErrors,
  expanded,
  changedSet,
  onToggleDir,
  onOpenFile,
}: {
  entry: DirEntry;
  relPath: string;
  depth: number;
  dirCache: Record<string, DirEntry[]>;
  loadingDirs: Record<string, boolean>;
  dirErrors: Record<string, string>;
  expanded: Record<string, boolean>;
  changedSet: Set<string>;
  onToggleDir: (relPath: string) => void;
  onOpenFile: (relPath: string) => void;
}) {
  const isOpen = !!expanded[relPath];
  const changed = changedSet.has(relPath.replace(/\\/g, "/"));
  const indent = { paddingLeft: `${6 + depth * 14}px` };

  if (!entry.isDir) {
    return (
      <button
        type="button"
        className={`file-node file-node--file${changed ? " file-node--changed" : ""}`}
        style={indent}
        title={relPath}
        onClick={() => onOpenFile(relPath)}
      >
        <span className="file-node__caret" aria-hidden="true" />
        <FileIcon size={14} className="file-node__icon" aria-hidden="true" />
        <span className="file-node__name">{entry.name}</span>
        {changed && <span className="file-node__dot" aria-hidden="true" />}
      </button>
    );
  }

  const children = dirCache[relPath];
  const loading = loadingDirs[relPath];
  const err = dirErrors[relPath];
  return (
    <>
      <button
        type="button"
        className={`file-node file-node--dir${changed ? " file-node--changed" : ""}`}
        style={indent}
        aria-expanded={isOpen}
        title={relPath}
        onClick={() => onToggleDir(relPath)}
      >
        <span className="file-node__caret" aria-hidden="true">
          {isOpen ? <ChevronDown size={12} /> : <ChevronRightSm size={12} />}
        </span>
        {isOpen ? <FolderOpen size={14} className="file-node__icon" aria-hidden="true" />
          : <Folder size={14} className="file-node__icon" aria-hidden="true" />}
        <span className="file-node__name">{entry.name}</span>
        {changed && <span className="file-node__dot" aria-hidden="true" />}
      </button>
      {isOpen && (
        err ? (
          <div className="files-empty files-empty--nested" style={{ paddingLeft: `${6 + (depth + 1) * 14}px` }}>{err}</div>
        ) : loading && !children ? (
          <div className="files-empty files-empty--nested" style={{ paddingLeft: `${6 + (depth + 1) * 14}px` }}>加载中…</div>
        ) : children && children.length === 0 ? (
          <div className="files-empty files-empty--nested" style={{ paddingLeft: `${6 + (depth + 1) * 14}px` }}>空目录</div>
        ) : children ? (
          children.map((c) => (
            <FileNode
              key={c.name}
              entry={c}
              relPath={`${relPath}/${c.name}`}
              depth={depth + 1}
              dirCache={dirCache}
              loadingDirs={loadingDirs}
              dirErrors={dirErrors}
              expanded={expanded}
              changedSet={changedSet}
              onToggleDir={onToggleDir}
              onOpenFile={onOpenFile}
            />
          ))
        ) : null
      )}
    </>
  );
}

// 文件系统错误友好化（目录拉取失败：无工作区 / 越界 / 不存在等统一兜底）。
function humanizeFsError(_raw: string): string {
  return "无法读取目录";
}

// ── 第三栏·知识标签：显化 OKF（开放知识格式）内容 ─────────────────────────────────
// 数据源（只读，复用已就绪后端命令，不碰后端）：
//   get_okf_settings() -> { visibility, sharedLocation, autoPublish, externalBundles }
//   okf_browse(conv, source) -> { source, concepts:[{relPath,type,title,description,tags}], hasContent, note }
//     source="own"＝本项目；或外部 bundle 绝对路径。返回不含正文 body。
//   okf_read_concept(conv, source, relPath) -> string（完整 .md：frontmatter + 正文）
//   okf_export(conv, targetDir) -> string（写到所选目录）
// 模式：浏览态（模式头 + 本项目 concept 树 + 外部包列表）/ 详情态（单窗格看一个 concept，返回回浏览）。
// key=activeConvId：切会话重挂载，状态自然清。仅本标签挂载时（第三栏展开且 tab==="knowledge"）才拉。

/** concept 树节点：目录（可折叠）或叶子 concept。 */
type OkfTreeNode =
  | { kind: "dir"; name: string; path: string; children: OkfTreeNode[] }
  | { kind: "concept"; name: string; concept: OkfConceptView };

// 把扁平 concept 列表（按 relPath）构造成目录树。relPath 用 "/" 分隔，末段为文件名。
function buildOkfTree(concepts: OkfConceptView[]): OkfTreeNode[] {
  type DirAcc = { dirs: Map<string, DirAcc>; files: OkfConceptView[] };
  const root: DirAcc = { dirs: new Map(), files: [] };
  for (const c of concepts) {
    const parts = c.relPath.replace(/\\/g, "/").split("/").filter(Boolean);
    if (parts.length === 0) continue;
    let cur = root;
    for (let i = 0; i < parts.length - 1; i++) {
      const seg = parts[i];
      let next = cur.dirs.get(seg);
      if (!next) { next = { dirs: new Map(), files: [] }; cur.dirs.set(seg, next); }
      cur = next;
    }
    cur.files.push(c);
  }
  const toNodes = (acc: DirAcc, prefix: string): OkfTreeNode[] => {
    const dirNodes: OkfTreeNode[] = [...acc.dirs.entries()]
      .sort((a, b) => a[0].localeCompare(b[0]))
      .map(([name, sub]) => ({
        kind: "dir" as const,
        name,
        path: prefix ? `${prefix}/${name}` : name,
        children: toNodes(sub, prefix ? `${prefix}/${name}` : name),
      }));
    const fileNodes: OkfTreeNode[] = acc.files
      .slice()
      .sort((a, b) => (a.title ?? a.relPath).localeCompare(b.title ?? b.relPath))
      .map((c) => ({
        kind: "concept" as const,
        name: c.relPath.replace(/\\/g, "/").split("/").pop() ?? c.relPath,
        concept: c,
      }));
    return [...dirNodes, ...fileNodes];
  };
  return toNodes(root, "");
}

// concept 树渲染（目录可展开/收起；concept 可点开看详情）。纯前端展开态，无懒加载（列表一次拉全）。
function OkfTree({
  nodes,
  depth,
  expanded,
  onToggleDir,
  onOpen,
}: {
  nodes: OkfTreeNode[];
  depth: number;
  expanded: Record<string, boolean>;
  onToggleDir: (path: string) => void;
  onOpen: (c: OkfConceptView) => void;
}) {
  return (
    <>
      {nodes.map((node) => {
        const indent = { paddingLeft: `${6 + depth * 14}px` };
        if (node.kind === "dir") {
          const isOpen = !!expanded[node.path];
          return (
            <div key={`d:${node.path}`}>
              <button
                type="button"
                className="file-node file-node--dir"
                style={indent}
                aria-expanded={isOpen}
                title={node.path}
                onClick={() => onToggleDir(node.path)}
              >
                <span className="file-node__caret" aria-hidden="true">
                  {isOpen ? <ChevronDown size={12} /> : <ChevronRightSm size={12} />}
                </span>
                {isOpen ? <FolderOpen size={14} className="file-node__icon" aria-hidden="true" />
                  : <Folder size={14} className="file-node__icon" aria-hidden="true" />}
                <span className="file-node__name">{node.name}</span>
              </button>
              {isOpen && (
                <OkfTree
                  nodes={node.children}
                  depth={depth + 1}
                  expanded={expanded}
                  onToggleDir={onToggleDir}
                  onOpen={onOpen}
                />
              )}
            </div>
          );
        }
        const c = node.concept;
        return (
          <button
            key={`c:${c.relPath}`}
            type="button"
            className="file-node file-node--file"
            style={indent}
            title={c.description || c.title || c.relPath}
            onClick={() => onOpen(c)}
          >
            <span className="file-node__caret" aria-hidden="true" />
            <FileText size={14} className="file-node__icon" aria-hidden="true" />
            <span className="file-node__name">{c.title || node.name}</span>
            {c.type && <span className="okf-type" title={`类型：${c.type}`}>{c.type}</span>}
          </button>
        );
      })}
    </>
  );
}

function KnowledgePanel({
  activeConvId,
  pushToast,
  onEditDirtyChange,
}: {
  activeConvId: string | null;
  pushToast?: (kind: "error" | "info", text: string) => void;
  /** 上报「编辑态有未保存改动」给父栏，用于切标签/折叠前确认拦截。 */
  onEditDirtyChange?: (dirty: boolean) => void;
}) {
  const [settings, setSettings] = useState<OkfSettingsView | null>(null);
  // 各 source（"own" 或 bundle 路径）的浏览结果缓存。
  const [browse, setBrowse] = useState<Record<string, OkfBrowseView>>({});
  // 已登记但读取失败的外部包（path → 错误）。失败的包不静默丢弃，仍以错误行展示。
  const [browseErrors, setBrowseErrors] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  // 目录展开态（key=`${source}::${dirPath}`）。
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  // 详情态：选中的 concept（含其 source）；null=浏览态。
  const [view, setView] = useState<{ source: string; concept: OkfConceptView } | null>(null);
  const [viewBody, setViewBody] = useState<string | null>(null);
  const [viewLoading, setViewLoading] = useState(false);
  const [viewError, setViewError] = useState<string | null>(null);
  // 导出进行中（防重复点）。
  const [exporting, setExporting] = useState(false);
  // 覆盖式编辑（own 源专用）。editOriginal=进入编辑时的原文,用于脏判定。
  const [editing, setEditing] = useState(false);
  const [editBody, setEditBody] = useState("");
  const [editOriginal, setEditOriginal] = useState("");
  const [editBusy, setEditBusy] = useState(false);
  const [editError, setEditError] = useState<string | null>(null);

  // 是否有未保存改动；变化时上报父栏(用于切标签/折叠前拦截);卸载时清零。
  const editDirty = editing && editBody !== editOriginal;
  useEffect(() => {
    onEditDirtyChange?.(editDirty);
    return () => onEditDirtyChange?.(false);
  }, [editDirty, onEditDirtyChange]);

  /** 有未保存改动时确认放弃;返回 true 表示可继续(无改动或用户确认)。 */
  function confirmDiscardEdit(): boolean {
    return !editDirty || window.confirm("有未保存的知识编辑，确定放弃吗？");
  }

  async function refresh() {
    setLoading(true);
    setLoadError(null);
    try {
      const s = await invoke<OkfSettingsView>("get_okf_settings");
      const next: Record<string, OkfBrowseView> = {};
      const errs: Record<string, string> = {};
      // 本项目 OKF 需绑定工作区的会话；无会话/无 wiki 则跳过 own（外部包不依赖会话，仍可浏览）。
      if (activeConvId) {
        try {
          next.own = await invoke<OkfBrowseView>("okf_browse", {
            conversationId: activeConvId,
            source: "own",
          });
        } catch { /* own 不可读（无工作区/无 wiki）→ 跳过，外部包仍展示 */ }
      }
      // 外部包逐个拉。失败的**不静默丢弃**——记下错误，渲染时仍以「无法读取」行展示
      // （否则一个已登记但目录被删/移动的包会让面板假装「暂无知识」）。外部源不依赖会话，
      // 故无会话时也能浏览（conversationId 传空串，后端外部分支不读它）。
      for (const path of s.externalBundles ?? []) {
        try {
          next[path] = await invoke<OkfBrowseView>("okf_browse", {
            conversationId: activeConvId ?? "",
            source: path,
          });
        } catch (e) {
          errs[path] = String(e);
        }
      }
      setSettings(s);
      setBrowse(next);
      setBrowseErrors(errs);
    } catch {
      setLoadError("无法加载知识");
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeConvId]);

  // 设置里改 OKF（导入/登记/移除外部包、改可见性/发布）后，第三栏知识标签同步刷新。
  // 两组件各持独立 OKF 状态，靠窗口级 CustomEvent 解耦联动（无需提升状态/穿 props）。
  useEffect(() => {
    const onChanged = () => { void refresh(); };
    window.addEventListener("okf-changed", onChanged);
    return () => window.removeEventListener("okf-changed", onChanged);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeConvId]);

  function toggleDir(source: string, dirPath: string) {
    const key = `${source}::${dirPath}`;
    setExpanded((p) => ({ ...p, [key]: !p[key] }));
  }

  async function openConcept(source: string, c: OkfConceptView) {
    setView({ source, concept: c });
    setViewBody(null);
    setViewError(null);
    setEditing(false);
    setEditError(null);
    setViewLoading(true);
    try {
      const body = await invoke<string>("okf_read_concept", {
        // 外部包读取不依赖会话；无会话时传空串（不能传 null，命令签名要 String）。
        // own 概念只在有会话时才出现在树里，故此处 ?? "" 不影响 own。
        conversationId: activeConvId ?? "",
        source,
        relPath: c.relPath,
      });
      setViewBody(body);
    } catch {
      setViewError("无法读取该知识");
    } finally {
      setViewLoading(false);
    }
  }

  function backToBrowse() {
    if (!confirmDiscardEdit()) return;
    setView(null);
    setViewBody(null);
    setViewError(null);
    setEditing(false);
    setEditError(null);
  }

  // ── 覆盖式编辑（own 源）：进入编辑预填正文 / 保存覆盖 / 还原自动 / 取消 ──
  async function startEdit() {
    if (!view || view.source !== "own" || editBusy) return;
    setEditBusy(true);
    setEditError(null);
    try {
      const src = await invoke<OkfConceptSourceView>("okf_get_concept_source", {
        conversationId: activeConvId,
        relPath: view.concept.relPath,
      });
      setEditBody(src.body);
      setEditOriginal(src.body);
      setEditing(true);
    } catch {
      setEditError("无法载入可编辑内容");
    } finally {
      setEditBusy(false);
    }
  }

  async function saveEdit() {
    if (!view || view.source !== "own" || editBusy) return;
    setEditBusy(true);
    setEditError(null);
    try {
      await invoke("okf_set_overlay", {
        conversationId: activeConvId,
        relPath: view.concept.relPath,
        body: editBody,
      });
      setEditing(false);
      // 重新拉详情正文（已合并覆盖）并标记 overridden；刷新树上的「已编辑」标记。
      await openConcept(view.source, { ...view.concept, overridden: true });
      void refresh();
    } catch {
      setEditError("保存失败");
    } finally {
      setEditBusy(false);
    }
  }

  async function revertEdit() {
    if (!view || view.source !== "own" || editBusy) return;
    setEditBusy(true);
    setEditError(null);
    try {
      await invoke("okf_clear_overlay", {
        conversationId: activeConvId,
        relPath: view.concept.relPath,
      });
      setEditing(false);
      await openConcept(view.source, { ...view.concept, overridden: false });
      void refresh();
    } catch {
      setEditError("还原失败");
    } finally {
      setEditBusy(false);
    }
  }

  function cancelEdit() {
    if (!confirmDiscardEdit()) return;
    setEditing(false);
    setEditError(null);
  }

  /** 移除一个已登记的外部包（用于清掉读不到的死链）。移除后广播 okf-changed 并本地刷新。 */
  async function removeBundle(path: string) {
    const name = path.replace(/\\/g, "/").split("/").filter(Boolean).pop() ?? path;
    if (!window.confirm(`移除外部包「${name}」？若是导入的 .zip 包，其解包目录会一并删除（不可撤销）。`)) return;
    try {
      await invoke<string[]>("okf_external_remove", { path });
      window.dispatchEvent(new CustomEvent("okf-changed"));
      await refresh();
    } catch {
      pushToast?.("error", "移除失败");
    }
  }

  async function handleExport() {
    if (!activeConvId || exporting) return;
    setExporting(true);
    try {
      const target = await saveFileDialog({
        defaultPath: "knowledge-okf.zip",
        filters: [{ name: "OKF 包", extensions: ["zip"] }],
      });
      if (typeof target !== "string") return; // 取消选择
      const note = await invoke<string>("okf_export", {
        conversationId: activeConvId,
        targetZip: target,
      });
      pushToast?.("info", note && note.trim().length > 0 ? `已导出 .zip：${note}` : "已导出 .zip");
    } catch {
      pushToast?.("error", "导出失败");
    } finally {
      setExporting(false);
    }
  }

  // 详情态：单窗格（返回 + 标题 + type/tags chip + markdown 正文 / 编辑器）。
  if (view) {
    const c = view.concept;
    const isOwn = view.source === "own";
    return (
      <div className="files-panel files-panel--view okf-detail">
        <div className="files-view__head">
          <button
            className="icon-btn files-view__back"
            type="button"
            title="返回知识列表"
            aria-label="返回知识列表"
            onClick={backToBrowse}
          >
            <ArrowLeft size={15} />
          </button>
          <span className="files-view__path" title={c.relPath}>{c.title || c.relPath}</span>
          {isOwn && !editing && (
            <button
              className="icon-btn"
              type="button"
              title="编辑此知识"
              aria-label="编辑此知识"
              disabled={editBusy}
              onClick={() => void startEdit()}
            >
              <Pencil size={14} />
            </button>
          )}
        </div>
        <div className="okf-chips">
          {c.type && <span className="okf-chip" title="类型">{c.type}</span>}
          {c.overridden && (
            <span className="okf-chip okf-chip--edited" title="已被你覆盖式编辑">已编辑</span>
          )}
          {c.tags?.map((t) => (
            <span key={t} className="okf-chip okf-chip--tag" title="标签">{t}</span>
          ))}
        </div>
        {editing ? (
          <div className="okf-edit">
            <textarea
              className="okf-edit__area"
              value={editBody}
              spellCheck={false}
              disabled={editBusy}
              onChange={(e) => setEditBody(e.target.value)}
              aria-label="编辑知识正文"
            />
            {editError && <div className="okf-edit__err">{editError}</div>}
            <div className="okf-edit__actions">
              <button className="changes-row__revert" type="button" disabled={editBusy} onClick={() => void saveEdit()}>
                <Check size={13} style={{ verticalAlign: "-2px", marginRight: 4 }} />保存
              </button>
              <button className="changes-row__revert" type="button" disabled={editBusy} onClick={cancelEdit}>
                <X size={13} style={{ verticalAlign: "-2px", marginRight: 4 }} />取消
              </button>
              {c.overridden && (
                <button
                  className="changes-row__revert"
                  type="button"
                  disabled={editBusy}
                  title="丢弃你的编辑，恢复自动生成的内容"
                  onClick={() => void revertEdit()}
                >
                  <Undo2 size={13} style={{ verticalAlign: "-2px", marginRight: 4 }} />还原自动
                </button>
              )}
            </div>
            <p className="okf-edit__hint">编辑的是正文；类型等元信息仍自动管理。保存后随导出/发布带出，wiki 重新生成不会冲掉。注意：agent 的 repo_wiki 仍读自动生成版，你的编辑只进「知识」标签与导出/发布。</p>
          </div>
        ) : viewLoading ? (
          <div className="files-empty">加载中…</div>
        ) : viewError ? (
          <div className="files-empty">{viewError}</div>
        ) : (
          <div className="okf-body">
            <Markdown>{viewBody ?? ""}</Markdown>
          </div>
        )}
      </div>
    );
  }

  // 浏览态。
  const own = browse.own;
  const ownTree = own ? buildOkfTree(own.concepts) : [];
  // 渲染**全部已登记**外部包（不再按是否读到过滤）——读不到的也要出一行，否则会假装「暂无知识」。
  const registeredBundles = settings?.externalBundles ?? [];
  const ownHasContent = !!own?.hasContent && own.concepts.length > 0;
  const isShared = settings?.visibility === "shared";
  const sharedPath = settings?.sharedLocation ?? "";

  return (
    <div className="okf-panel">
      {/* 模式头：私有/共享 pill（title 悬停说明）+ 刷新 + 导出（共享态另以 title 显共享目录路径）。 */}
      <div className="okf-head">
        {settings && (
          isShared ? (
            <span
              className="okf-pill okf-pill--shared"
              title={`共享：已发布到可见 OKF 目录，其他 agent 可读${sharedPath ? `（${sharedPath}）` : ""}`}
            >
              <Globe size={12} /> 共享
            </span>
          ) : (
            <span
              className="okf-pill okf-pill--private"
              title="私有：仅 .mdga，其他 agent 拿不到"
            >
              <Lock size={12} /> 私有
            </span>
          )
        )}
        <span className="okf-head__spacer" />
        <button
          className="icon-btn"
          type="button"
          title="刷新"
          aria-label="刷新知识"
          onClick={() => void refresh()}
        >
          <RefreshCw size={15} />
        </button>
        <button
          className="icon-btn"
          type="button"
          title="导出到目录"
          aria-label="导出知识到目录"
          disabled={exporting || !activeConvId}
          onClick={() => void handleExport()}
        >
          <FolderOutput size={15} />
        </button>
      </div>

      {loadError ? (
        <div className="files-empty">{loadError}</div>
      ) : loading && !own ? (
        <div className="files-empty">加载中…</div>
      ) : !ownHasContent && registeredBundles.length === 0 ? (
        // 空态：仅当本项目无内容**且**没有任何已登记外部包时才显（登记了的包即便读不到也出错误行）。
        <div className="okf-empty" title="agent 整理 wiki 后在此以 OKF 浏览">
          <BookOpen size={20} />
          <span>暂无知识</span>
        </div>
      ) : (
        <>
          {/* 本项目 concept 树。 */}
          {ownHasContent && (
            <div className="files-tree" role="tree">
              <OkfTree
                nodes={ownTree}
                depth={0}
                expanded={Object.fromEntries(
                  Object.entries(expanded)
                    .filter(([k]) => k.startsWith("own::"))
                    .map(([k, v]) => [k.slice("own::".length), v]),
                )}
                onToggleDir={(dirPath) => toggleDir("own", dirPath)}
                onOpen={(c) => void openConcept("own", c)}
              />
            </div>
          )}

          {/* 导入的外部包：渲染**全部已登记**包。读不到的（目录被删/移动等）出「无法读取」行 + 移除，
              绝不静默丢弃（否则会误显「暂无知识」）。只读。 */}
          {registeredBundles.map((path) => {
            const b = browse[path];
            const name = path.replace(/\\/g, "/").split("/").filter(Boolean).pop() ?? path;
            // 读取失败：出错误行（含移除死链）。
            if (!b) {
              return (
                <div className="okf-bundle okf-bundle--err" key={path}>
                  <div className="file-node file-node--dir okf-bundle__head" title={path}>
                    <span className="file-node__caret" aria-hidden="true" />
                    <Folder size={14} className="file-node__icon" aria-hidden="true" />
                    <span className="file-node__name">{name}</span>
                    <span className="okf-bundle__err" title={browseErrors[path] ?? "读取失败"}>无法读取</span>
                    <button
                      className="icon-btn okf-bundle__rm"
                      type="button"
                      title="移除该外部包（目录可能已删除或移动）"
                      aria-label="移除该外部包"
                      onClick={() => void removeBundle(path)}
                    >
                      <X size={13} />
                    </button>
                  </div>
                </div>
              );
            }
            const groupKey = `bundle::${path}`;
            const groupOpen = !!expanded[groupKey];
            const tree = buildOkfTree(b.concepts);
            return (
              <div className="okf-bundle" key={path}>
                <button
                  type="button"
                  className="file-node file-node--dir okf-bundle__head"
                  aria-expanded={groupOpen}
                  title={path}
                  onClick={() => setExpanded((p) => ({ ...p, [groupKey]: !p[groupKey] }))}
                >
                  <span className="file-node__caret" aria-hidden="true">
                    {groupOpen ? <ChevronDown size={12} /> : <ChevronRightSm size={12} />}
                  </span>
                  <Folder size={14} className="file-node__icon" aria-hidden="true" />
                  <span className="file-node__name">{name}</span>
                  <span className="okf-bundle__count" title="知识条数">{b.concepts.length}</span>
                </button>
                {groupOpen && (
                  b.concepts.length === 0 ? (
                    <div className="files-empty files-empty--nested">（空）</div>
                  ) : (
                    <div className="files-tree" role="tree">
                      <OkfTree
                        nodes={tree}
                        depth={1}
                        expanded={Object.fromEntries(
                          Object.entries(expanded)
                            .filter(([k]) => k.startsWith(`${path}::`))
                            .map(([k, v]) => [k.slice(`${path}::`.length), v]),
                        )}
                        onToggleDir={(dirPath) => toggleDir(path, dirPath)}
                        onOpen={(c) => void openConcept(path, c)}
                      />
                    </div>
                  )
                )}
              </div>
            );
          })}
        </>
      )}
    </div>
  );
}


export function ThirdColumn({
  open,
  tab,
  width,
  onResize,
  onToggleOpen,
  onSelectTab,
  // 变更标签数据 + 行为（复用 App 现有 checkpoints / handleRevert / setShowChanges）
  checkpoints,
  fileChanges,
  onRevert,
  onOpenFullChanges,
  // 通知点：有后台活动 或 有未回退检查点时，折叠态细栏顶部亮品牌色小圆点。
  hasActivityDot,
  hasChangesDot,
  // 文件标签可见性：当前会话是否绑定工作区（纯聊天会话隐藏文件标签）。
  hasWorkspace,
  // 活动标签数据：当前会话 + Agent todo 清单（计划进度小区块复用）。
  activeConvId,
  todos,
  // 用量标签数据（全部复用 App 现有聚合，不碰后端账单）：上下文占用 + 会话累计 + 各轮原始用量。
  ctxUsage,
  conversationUsage,
  turnUsages,
  // 「产物」坞（0.0.75）：停靠的互动卡片（null＝无停靠产物，「产物」标签隐藏）；解除停靠回调；
  // 透传给复用的 ArtifactCard 的 toast（同中栏，安全模型一致）。
  dockedArtifact,
  onUndockArtifact,
  pushToast,
}: {
  open: boolean;
  tab: ThirdColTab;
  // 展开态宽度（px，已 clamp）+ 拖拽改宽回调（折叠态不用）。
  width: number;
  onResize: (next: number) => void;
  onToggleOpen: (next: boolean) => void;
  onSelectTab: (tab: ThirdColTab) => void;
  checkpoints: FileCheckpoint[];
  fileChanges: FileChange[];
  onRevert: (id: string) => void;
  onOpenFullChanges: () => void;
  hasActivityDot: boolean;
  hasChangesDot: boolean;
  hasWorkspace: boolean;
  activeConvId: string | null;
  todos: TodoItem[];
  ctxUsage: { promptTokens: number; softLimit: number | null } | null;
  conversationUsage: UsageSummary | null;
  turnUsages: UsageSummary[];
  dockedArtifact: ArtifactPart | null;
  onUndockArtifact: () => void;
  pushToast?: (kind: "error" | "info", text: string) => void;
}) {
  // 拖拽改宽：pointerdown 起捕获指针，move 时按「左移变宽」换算（左边缘手柄，向左拖 = 加宽）。
  // 与思考深度滑轨同款：setPointerCapture + 拖动期间监听本元素 pointermove，抬起释放。手感不依赖全局监听。
  const resizeStartRef = useRef<{ x: number; w: number } | null>(null);
  function onResizerDown(e: React.PointerEvent<HTMLDivElement>) {
    e.preventDefault();
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
    resizeStartRef.current = { x: e.clientX, w: width };
  }
  function onResizerMove(e: React.PointerEvent<HTMLDivElement>) {
    const start = resizeStartRef.current;
    if (!start) return;
    // 第三栏在右侧，手柄在其左边缘：指针向左移（clientX 变小）应加宽 → delta 取反。
    onResize(start.w + (start.x - e.clientX));
  }
  function onResizerUp(e: React.PointerEvent<HTMLDivElement>) {
    resizeStartRef.current = null;
    try { (e.target as HTMLElement).releasePointerCapture(e.pointerId); } catch { /* 已释放则忽略 */ }
  }

  // 知识标签编辑态脏拦截：KnowledgePanel 切标签即卸载会丢未保存编辑,故在父栏切标签/折叠前确认。
  // 折叠态下 KnowledgePanel 未挂载,卸载时已上报 dirty=false,故折叠态的 rail 按钮天然无需拦截。
  const editDirtyRef = useRef(false);
  function guardLeaveEdit(): boolean {
    return !editDirtyRef.current || window.confirm("知识编辑有未保存内容，离开将丢弃。确定吗？");
  }
  function selectTab(t: ThirdColTab) {
    if (!guardLeaveEdit()) return;
    onSelectTab(t);
  }
  // 稳定身份,避免每次渲染都让 KnowledgePanel 的上报 effect 重跑。
  const handleEditDirty = useCallback((d: boolean) => { editDirtyRef.current = d; }, []);

  // 折叠态：竖排图标条。点图标 = 展开并切到该标签。
  if (!open) {
    const anyDot = hasActivityDot || hasChangesDot;
    return (
      <aside className="third-col third-col--collapsed" aria-label="活动与变更（已折叠）">
        {/* 顶部通知点：有后台活动或有未回退检查点时亮（折叠态必显）。 */}
        {anyDot && <span className="third-col__dot third-col__dot--rail" aria-hidden="true" />}
        <button
          className="third-col__rail-btn icon-btn"
          type="button"
          title="展开"
          aria-label="展开第三栏"
          onClick={() => onToggleOpen(true)}
        >
          <ChevronLeft size={16} />
        </button>
        <button
          className={`third-col__rail-btn icon-btn${tab === "activity" ? " is-active" : ""}`}
          type="button"
          title="活动"
          aria-label="活动"
          onClick={() => { onSelectTab("activity"); onToggleOpen(true); }}
        >
          <ListChecks size={16} />
          {hasActivityDot && <span className="third-col__dot third-col__dot--badge" aria-hidden="true" />}
        </button>
        <button
          className={`third-col__rail-btn icon-btn${tab === "changes" ? " is-active" : ""}`}
          type="button"
          title="变更"
          aria-label="变更"
          onClick={() => { onSelectTab("changes"); onToggleOpen(true); }}
        >
          <GitCompare size={16} />
          {hasChangesDot && <span className="third-col__dot third-col__dot--badge" aria-hidden="true" />}
        </button>
        {/* 用量：折叠 rail 直达；用量无通知点（不需要 dot）。 */}
        <button
          className={`third-col__rail-btn icon-btn${tab === "usage" ? " is-active" : ""}`}
          type="button"
          title="用量"
          aria-label="用量"
          onClick={() => { onSelectTab("usage"); onToggleOpen(true); }}
        >
          <Gauge size={16} />
        </button>
        {/* 文件：仅当前会话绑定工作区时显（纯聊天会话隐藏）。无通知点。 */}
        {hasWorkspace && (
          <button
            className={`third-col__rail-btn icon-btn${tab === "files" ? " is-active" : ""}`}
            type="button"
            title="文件"
            aria-label="文件"
            onClick={() => { onSelectTab("files"); onToggleOpen(true); }}
          >
            <FolderTree size={16} />
          </button>
        )}
        {/* 知识：显化 OKF 内容（私有/共享）。无通知点。 */}
        <button
          className={`third-col__rail-btn icon-btn${tab === "knowledge" ? " is-active" : ""}`}
          type="button"
          title="知识"
          aria-label="知识"
          onClick={() => { onSelectTab("knowledge"); onToggleOpen(true); }}
        >
          <BookOpen size={16} />
        </button>
        {/* 产物：仅当有停靠的互动卡片时显（0.0.75）。无通知点。 */}
        {dockedArtifact && (
          <button
            className={`third-col__rail-btn icon-btn${tab === "artifact" ? " is-active" : ""}`}
            type="button"
            title="产物"
            aria-label="产物"
            onClick={() => { onSelectTab("artifact"); onToggleOpen(true); }}
          >
            <LayoutDashboard size={16} />
          </button>
        )}
      </aside>
    );
  }

  // 展开态：左边缘拖拽手柄 + 顶部标签条 + 内容区。宽度由 --third-col-w 驱动（clamp 在 App/onResize）。
  return (
    <aside
      className="third-col third-col--expanded"
      aria-label="活动与变更"
      style={{ ["--third-col-w" as string]: `${width}px` }}
    >
      {/* 左边缘拖拽手柄：pointer 捕获改宽，clamp 在 App/onResize（最小 300，最大动态=窗口宽−侧栏−中栏最小）。折叠态不渲染此节点。 */}
      <div
        className="third-col__resizer"
        role="separator"
        aria-orientation="vertical"
        aria-label="拖拽调整第三栏宽度"
        title="拖拽调整宽度"
        onPointerDown={onResizerDown}
        onPointerMove={onResizerMove}
        onPointerUp={onResizerUp}
        onPointerCancel={onResizerUp}
      />
      <div className="third-col__tabs">
        <div className="third-col__tabs-scroll" role="tablist" aria-label="第三栏标签">
        <button
          className={`third-col__tab${tab === "activity" ? " is-active" : ""}`}
          type="button"
          role="tab"
          aria-selected={tab === "activity"}
          title="活动"
          onClick={() => selectTab("activity")}
        >
          <ListChecks size={14} />
          <span>活动</span>
          {hasActivityDot && <span className="third-col__dot third-col__dot--badge" aria-hidden="true" />}
        </button>
        <button
          className={`third-col__tab${tab === "changes" ? " is-active" : ""}`}
          type="button"
          role="tab"
          aria-selected={tab === "changes"}
          title="变更"
          onClick={() => selectTab("changes")}
        >
          <GitCompare size={14} />
          <span>变更</span>
          {hasChangesDot && <span className="third-col__dot third-col__dot--badge" aria-hidden="true" />}
        </button>
        <button
          className={`third-col__tab${tab === "usage" ? " is-active" : ""}`}
          type="button"
          role="tab"
          aria-selected={tab === "usage"}
          title="用量"
          onClick={() => selectTab("usage")}
        >
          <Gauge size={14} />
          <span>用量</span>
        </button>
        {/* 文件标签：仅有工作区时显。 */}
        {hasWorkspace && (
          <button
            className={`third-col__tab${tab === "files" ? " is-active" : ""}`}
            type="button"
            role="tab"
            aria-selected={tab === "files"}
            title="文件"
            onClick={() => selectTab("files")}
          >
            <FolderTree size={14} />
            <span>文件</span>
          </button>
        )}
        {/* 知识标签：显化 OKF 内容（私有/共享）。 */}
        <button
          className={`third-col__tab${tab === "knowledge" ? " is-active" : ""}`}
          type="button"
          role="tab"
          aria-selected={tab === "knowledge"}
          title="知识"
          onClick={() => selectTab("knowledge")}
        >
          <BookOpen size={14} />
          <span>知识</span>
        </button>
        {/* 产物标签：仅有停靠的互动卡片时显（0.0.75）。 */}
        {dockedArtifact && (
          <button
            className={`third-col__tab${tab === "artifact" ? " is-active" : ""}`}
            type="button"
            role="tab"
            aria-selected={tab === "artifact"}
            title="产物"
            onClick={() => selectTab("artifact")}
          >
            <LayoutDashboard size={14} />
            <span>产物</span>
          </button>
        )}
        </div>
        <button
          className="third-col__collapse icon-btn"
          type="button"
          title="折叠"
          aria-label="折叠第三栏"
          onClick={() => { if (!guardLeaveEdit()) return; onToggleOpen(false); }}
        >
          <ChevronRight size={16} />
        </button>
      </div>

      <div className="third-col__body">
        {tab === "activity" ? (
          // 活动面板：todo/计划进度 + 后台活动列表（自带 ~2s 轮询 + 事件即时刷新，仅本标签挂载时运行）。
          // key=activeConvId：切会话即重挂载，旧会话在途响应/时间簿随实例销毁，杜绝 stale 覆盖。
          <ActivityPanel key={activeConvId ?? "none"} activeConvId={activeConvId} todos={todos} />
        ) : tab === "usage" ? (
          // 用量面板：上下文环（复用 ctxUsage）+ 本会话真账单（复用 ConversationUsageSummary）
          // + 按工具活动量（get_tool_usage；明确标注近似、非账单）。仅本标签挂载时拉数据。
          // key=activeConvId：同上，切会话重挂载防 stale 响应覆盖。
          <UsagePanel
            key={activeConvId ?? "none"}
            activeConvId={activeConvId}
            ctxUsage={ctxUsage}
            conversationUsage={conversationUsage}
            turnUsages={turnUsages}
          />
        ) : tab === "files" ? (
          // 文件面板：只读工作区文件树（树态/查看态单窗格）。key=activeConvId：切会话即重挂载，
          // 缓存与查看态自然清。仅本标签挂载时拉数据（折叠/非本标签不主动拉）。
          <FilesPanel key={activeConvId ?? "none"} activeConvId={activeConvId} fileChanges={fileChanges} />
        ) : tab === "knowledge" ? (
          // 知识面板：显化 OKF（私有/共享）——本项目 concept 树 + 外部包 + 详情（markdown 复用 messages 的 Markdown）。
          // key=activeConvId：切会话即重挂载，缓存/详情/展开态自然清。仅本标签挂载时拉数据。
          <KnowledgePanel
            key={activeConvId ?? "none"}
            activeConvId={activeConvId}
            pushToast={pushToast}
            onEditDirtyChange={handleEditDirty}
          />
        ) : tab === "artifact" && dockedArtifact ? (
          // 产物坞（0.0.75）：复用**同一** ArtifactCard 渲染停靠的互动卡片（同安全模型：sandbox/CSP/探针/
          // nonce 全在 artifact.tsx，未改一字）。顶部一行标题 + 取消停靠；坞内不传 onDock（已在坞里，不再显停靠按钮）。
          // 放大/复制/下载仍走 ArtifactCard 自带的。
          <div className="third-col__artifact">
            <div className="third-col__panel-head">
              <span className="third-col__panel-title" title={dockedArtifact.title ?? "互动卡片"}>
                {dockedArtifact.title ?? "互动卡片"}
              </span>
              <button
                className="icon-btn"
                type="button"
                title="取消停靠"
                aria-label="取消停靠"
                onClick={onUndockArtifact}
              >
                <X size={15} />
              </button>
            </div>
            <ArtifactCard part={dockedArtifact} pushToast={pushToast} />
          </div>
        ) : (
          <div className="third-col__changes">
            <div className="third-col__panel-head">
              <span className="third-col__panel-title">文件变更</span>
              <button
                className="icon-btn"
                type="button"
                title="全屏审查"
                aria-label="全屏审查变更"
                onClick={onOpenFullChanges}
              >
                <Maximize2 size={15} />
              </button>
            </div>
            {/* 上半段：本会话文件累计改动（从消息流 diff 卡聚合；空则不渲染）。看「改了什么」。 */}
            <FileChangesSection fileChanges={fileChanges} />
            {/* 下半段：检查点时间线（回退用），与 ChangesModal 共用 ChangesView，不复制两份。看「回退到哪」。 */}
            <ChangesView checkpoints={checkpoints} onRevert={onRevert} />
          </div>
        )}
      </div>
    </aside>
  );
}
