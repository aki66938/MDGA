// 模态弹窗与浮层子组件（0.0.37 从 App.tsx 抽出，纯搬移，无逻辑改动）。

import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import {
  SquarePen, Search, GitCompare, Plug, Gauge, Lock,
  MessageCircle, Cpu, HelpCircle, CornerDownLeft, Settings2,
} from "lucide-react";
import type {
  FileCheckpoint, ApprovalRequest, AskUserRequest, Conversation, SettingsSection, PaletteItem,
} from "../types";

/** MDGA 品牌标识：深海声纳波纹（致敬 DeepSeek 的「deep」，非官方鲸鱼 logo） */
export function BrandMark({ size = 24 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" aria-hidden="true">
      <path d="M4 19 C9 13 14 13 16 16 C18 19 23 19 28 13" fill="none"
        stroke="var(--brand)" strokeWidth="2.6" strokeLinecap="round" />
      <circle cx="16" cy="16" r="2.4" fill="var(--brand)" />
    </svg>
  );
}

// ── useFocusTrap（Plan27 #7）────────────────────────────────────────────────

/**
 * 模态焦点陷阱：打开时聚焦容器内首个可聚焦元素，Tab/Shift+Tab 在容器内循环。
 * 返回挂到模态容器的 ref。配合各模态的 Esc 关闭逻辑共同满足可访问性。
 */
export function useFocusTrap<T extends HTMLElement>(active: boolean = true) {
  const ref = useRef<T>(null);
  useEffect(() => {
    if (!active) return;
    const el = ref.current;
    if (!el) return;
    const SELECTOR =
      'a[href], button:not([disabled]), textarea:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])';
    const focusables = () =>
      Array.from(el.querySelectorAll<HTMLElement>(SELECTOR)).filter(
        (n) => n.offsetParent !== null || n === document.activeElement,
      );
    // 打开时聚焦首个可聚焦元素（已有 autoFocus 的元素优先保留其焦点）。
    const prevActive = document.activeElement as HTMLElement | null;
    const first = focusables()[0];
    if (first && !el.contains(prevActive)) first.focus();
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Tab") return;
      const list = focusables();
      if (list.length === 0) return;
      const firstEl = list[0];
      const lastEl = list[list.length - 1];
      const cur = document.activeElement as HTMLElement | null;
      if (e.shiftKey) {
        if (cur === firstEl || !el.contains(cur)) {
          e.preventDefault();
          lastEl.focus();
        }
      } else {
        if (cur === lastEl || !el.contains(cur)) {
          e.preventDefault();
          firstEl.focus();
        }
      }
    };
    el.addEventListener("keydown", onKeyDown);
    return () => {
      el.removeEventListener("keydown", onKeyDown);
      // 关闭时把焦点还给打开前的元素，避免焦点丢失。
      if (prevActive && document.contains(prevActive)) prevActive.focus();
    };
  }, [active]);
  return ref;
}

// ── ChangesModal ────────────────────────────────────────────────────────────

export function ChangesModal({
  checkpoints,
  onRevert,
  onClose,
}: {
  checkpoints: FileCheckpoint[];
  onRevert: (id: string) => void;
  onClose: () => void;
}) {
  const trapRef = useFocusTrap<HTMLDivElement>(true);
  return (
    <div
      className="approval-overlay"
      role="dialog"
      aria-modal="true"
      aria-label="文件变更记录"
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
    >
      <div className="approval-card panel-card" ref={trapRef} onClick={(e) => e.stopPropagation()}>
        <p className="approval-card__title">文件变更记录</p>
        {checkpoints.length === 0 ? (
          <p className="approval-card__hint">本会话还没有文件变更。</p>
        ) : (
          <div className="changes-list">
            {checkpoints.map((c) => (
              <div key={c.id} className={`changes-row${c.reverted ? " changes-row--reverted" : ""}`}>
                <span className="changes-row__tool">{c.toolName}</span>
                <span className="changes-row__path" title={c.relPath}>{c.relPath}</span>
                {c.reverted ? (
                  <span className="changes-row__state">已回退</span>
                ) : c.revertible ? (
                  <button
                    className="changes-row__revert"
                    type="button"
                    title="回退此变更及其后的所有变更"
                    onClick={() => onRevert(c.id)}
                  >
                    回退到此前
                  </button>
                ) : (
                  <span className="changes-row__state">不可回退</span>
                )}
              </div>
            ))}
          </div>
        )}
        <div className="approval-card__actions">
          <button type="button" className="approval-card__btn" onClick={onClose}>
            关闭
          </button>
        </div>
      </div>
    </div>
  );
}

// ── ApprovalModal ───────────────────────────────────────────────────────────

export function ApprovalModal({
  approval,
  onAllow,
  onAlwaysAllow,
  onDeny,
}: {
  approval: ApprovalRequest;
  onAllow: () => void;
  onAlwaysAllow: () => void;
  onDeny: () => void;
}) {
  const trapRef = useFocusTrap<HTMLDivElement>(true);
  return (
    <div className="approval-overlay" role="dialog" aria-modal="true" aria-label="高风险动作审批">
      <div className="approval-card" ref={trapRef}>
        <p className="approval-card__title">Agent 请求执行高风险动作</p>
        <div className="approval-card__detail">
          <span className="approval-card__tool">{approval.toolName}</span>
          {approval.target && (
            <span className="approval-card__target">{approval.target}</span>
          )}
        </div>
        {approval.preview && approval.preview.trim().length > 0 && (
          <pre className="approval-preview" aria-label="动作内容预览">{approval.preview}</pre>
        )}
        <p className="approval-card__hint">是否允许本次操作？「总是允许」会记住同类动作，后续免审批。</p>
        <div className="approval-card__actions">
          <button type="button" className="approval-card__btn approval-card__btn--allow" onClick={onAllow}>
            允许一次
          </button>
          <button type="button" className="approval-card__btn" onClick={onAlwaysAllow}>
            总是允许
          </button>
          <button type="button" className="approval-card__btn" onClick={onDeny}>
            拒绝
          </button>
        </div>
      </div>
    </div>
  );
}

// ── AskUserModal ──────────────────────────────────────────────────────────────

/** Agent 发起的结构化澄清提问：每题渲染可点选项卡片，自动附「其他」自定义输入。 */
export function AskUserModal({
  request,
  onSubmit,
  onCancel,
}: {
  request: AskUserRequest;
  onSubmit: (answer: string) => void;
  onCancel: () => void;
}) {
  const trapRef = useFocusTrap<HTMLDivElement>(true);
  const questions = request.questions;
  // 每题已选 label 集合；"__other__" 代表选中了自定义项。
  const [selected, setSelected] = useState<Record<number, Set<string>>>(() =>
    Object.fromEntries(questions.map((_, i) => [i, new Set<string>()])),
  );
  const [otherText, setOtherText] = useState<Record<number, string>>({});

  function toggle(qIndex: number, label: string, multi: boolean) {
    setSelected((prev) => {
      const next = { ...prev };
      const set = new Set(next[qIndex]);
      if (multi) {
        if (set.has(label)) set.delete(label);
        else set.add(label);
      } else {
        if (set.has(label)) set.clear();
        else {
          set.clear();
          set.add(label);
        }
      }
      next[qIndex] = set;
      return next;
    });
  }

  // 每题至少有一项选择（普通选项，或选了「其他」且填了文本）才能提交。
  const ready = questions.every((_, i) => {
    const set = selected[i] ?? new Set<string>();
    const hasOption = Array.from(set).some((s) => s !== "__other__");
    const hasOther = set.has("__other__") && (otherText[i] ?? "").trim().length > 0;
    return hasOption || hasOther;
  });

  function submit() {
    const answer = questions.map((q, i) => {
      const set = selected[i] ?? new Set<string>();
      const picks = Array.from(set).filter((s) => s !== "__other__");
      if (set.has("__other__") && (otherText[i] ?? "").trim()) {
        picks.push(otherText[i].trim());
      }
      return { question: q.question, header: q.header ?? "", selected: picks };
    });
    onSubmit(JSON.stringify(answer));
  }

  return (
    <div className="approval-overlay" role="dialog" aria-modal="true" aria-label="Agent 提问">
      <div className="approval-card askuser-card" ref={trapRef}>
        <p className="approval-card__title">Agent 需要你确认</p>
        <div className="askuser-questions">
          {questions.map((q, i) => {
            const multi = q.multiSelect === true;
            const set = selected[i] ?? new Set<string>();
            return (
              <div className="askuser-question" key={i}>
                <div className="askuser-question__head">
                  {q.header && <span className="askuser-chip">{q.header}</span>}
                  <span className="askuser-question__text">{q.question}</span>
                </div>
                <div className="askuser-options">
                  {q.options.map((opt, j) => (
                    <button
                      type="button"
                      key={j}
                      className={`askuser-option${set.has(opt.label) ? " askuser-option--on" : ""}`}
                      onClick={() => toggle(i, opt.label, multi)}
                    >
                      <span className="askuser-option__label">{opt.label}</span>
                      {opt.description && (
                        <span className="askuser-option__desc">{opt.description}</span>
                      )}
                    </button>
                  ))}
                  <button
                    type="button"
                    className={`askuser-option${set.has("__other__") ? " askuser-option--on" : ""}`}
                    onClick={() => toggle(i, "__other__", multi)}
                  >
                    <span className="askuser-option__label">其他…</span>
                    <span className="askuser-option__desc">自定义回答</span>
                  </button>
                  {set.has("__other__") && (
                    <input
                      className="askuser-other-input"
                      type="text"
                      placeholder="输入你的回答"
                      value={otherText[i] ?? ""}
                      onChange={(e) =>
                        setOtherText((prev) => ({ ...prev, [i]: e.target.value }))
                      }
                      autoFocus
                    />
                  )}
                </div>
                {multi && <p className="askuser-multi-hint">可多选</p>}
              </div>
            );
          })}
        </div>
        <div className="approval-card__actions">
          <button
            type="button"
            className="approval-card__btn approval-card__btn--allow"
            disabled={!ready}
            onClick={submit}
          >
            提交
          </button>
          <button type="button" className="approval-card__btn" onClick={onCancel}>
            取消
          </button>
        </div>
      </div>
    </div>
  );
}

// ── CommandPalette（Plan27 #3a）────────────────────────────────────────────────

/**
 * 命令面板：居中浮层（参照 approval-overlay），输入框 + 列表。
 * - 空查询：列出快捷动作（新对话、设置各分类、/命令、帮助、变更）。
 * - 非空查询：防抖调 search_conversations 列出匹配会话（正文/标题命中），并保留命中名称的动作。
 * 方向键移动、Enter 执行、Esc 关闭；每次执行后关闭面板。
 */
export function CommandPalette({
  hasActiveConv,
  onClose,
  onNewConversation,
  onSelectConversation,
  onOpenSettings,
  onOpenHelp,
  onOpenChanges,
  onRunSlash,
}: {
  hasActiveConv: boolean;
  onClose: () => void;
  onNewConversation: () => void;
  onSelectConversation: (id: string) => void;
  onOpenSettings: (section: SettingsSection) => void;
  onOpenHelp: () => void;
  onOpenChanges: () => void;
  onRunSlash: (cmd: string) => void;
}) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<Conversation[]>([]);
  const [active, setActive] = useState(0);
  const trapRef = useFocusTrap<HTMLDivElement>(true);
  const listRef = useRef<HTMLDivElement>(null);

  // 防抖正文搜索（Plan27 #3a，复用 #6 后端命令）：query 非空时 ~200ms 后调 search_conversations。
  useEffect(() => {
    const q = query.trim();
    if (!q) { setResults([]); return; }
    const timer = setTimeout(() => {
      invoke<Conversation[]>("search_conversations", { query: q })
        .then((list) => setResults(Array.isArray(list) ? list : []))
        .catch(() => setResults([]));
    }, 200);
    return () => clearTimeout(timer);
  }, [query]);

  function act(fn: () => void) {
    onClose();
    fn();
  }

  // 静态动作项（始终可用，按查询词模糊过滤标签）。
  const staticActions: PaletteItem[] = [
    { id: "new", label: "新建对话", hint: "Ctrl/Cmd+N", icon: <SquarePen size={15} />, run: () => act(onNewConversation) },
    { id: "set-provider", label: "设置 · 模型供应商", icon: <Cpu size={15} />, run: () => act(() => onOpenSettings("provider")) },
    { id: "set-permission", label: "设置 · 权限", icon: <Lock size={15} />, run: () => act(() => onOpenSettings("permission")) },
    { id: "set-rules", label: "设置 · 权限规则", icon: <Lock size={15} />, run: () => act(() => onOpenSettings("rules")) },
    { id: "set-mcp", label: "设置 · MCP 服务器", icon: <Plug size={15} />, run: () => act(() => onOpenSettings("mcp")) },
    { id: "set-account", label: "设置 · 账户", icon: <Settings2 size={15} />, run: () => act(() => onOpenSettings("account")) },
    { id: "set-data", label: "设置 · 数据", icon: <Settings2 size={15} />, run: () => act(() => onOpenSettings("data")) },
    { id: "help", label: "帮助 · 能做什么", hint: "/help", icon: <HelpCircle size={15} />, run: () => act(onOpenHelp) },
    ...(hasActiveConv
      ? [
          { id: "compact", label: "压缩当前会话", hint: "/compact", icon: <Gauge size={15} />, run: () => act(() => onRunSlash("/compact")) } as PaletteItem,
          { id: "changes", label: "文件变更记录", hint: "/rewind", icon: <GitCompare size={15} />, run: () => act(onOpenChanges) } as PaletteItem,
        ]
      : []),
    { id: "clear", label: "开启新会话（清空）", hint: "/clear", icon: <SquarePen size={15} />, run: () => act(() => onRunSlash("/clear")) },
  ];

  const q = query.trim().toLowerCase();
  const filteredActions = q
    ? staticActions.filter((a) => a.label.toLowerCase().includes(q) || (a.hint ?? "").toLowerCase().includes(q))
    : staticActions;

  // 会话结果项（仅查询非空时）。
  const convItems: PaletteItem[] = results.map((c) => ({
    id: `conv-${c.id}`,
    label: c.title,
    hint: "会话",
    icon: <MessageCircle size={15} />,
    run: () => act(() => onSelectConversation(c.id)),
  }));

  const items: PaletteItem[] = [...filteredActions, ...convItems];

  // 查询变化时把高亮复位到首项。
  useEffect(() => { setActive(0); }, [query, results.length]);

  function onKeyDown(e: React.KeyboardEvent) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((i) => Math.min(i + 1, items.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      items[active]?.run();
    }
    // Esc 由全局 Esc 处理器关闭。
  }

  // 高亮项滚动进视口。
  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(`[data-idx="${active}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [active]);

  return (
    <div
      className="approval-overlay command-palette-overlay"
      role="dialog"
      aria-modal="true"
      aria-label="命令面板"
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
    >
      <div className="command-palette" ref={trapRef} onKeyDown={onKeyDown}>
        <div className="command-palette__search">
          <Search size={16} className="command-palette__search-icon" />
          <input
            className="command-palette__input"
            type="text"
            placeholder="搜索会话、跳转设置、运行命令…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            autoFocus
            aria-label="命令面板搜索"
          />
        </div>
        <div className="command-palette__list" ref={listRef} role="listbox" aria-label="命令与结果">
          {items.length === 0 ? (
            <p className="command-palette__empty">无匹配项</p>
          ) : (
            items.map((it, i) => (
              <button
                key={it.id}
                type="button"
                data-idx={i}
                className={`command-palette__item${i === active ? " command-palette__item--active" : ""}`}
                role="option"
                aria-selected={i === active}
                onMouseEnter={() => setActive(i)}
                onClick={it.run}
              >
                <span className="command-palette__item-icon">{it.icon}</span>
                <span className="command-palette__item-label">{it.label}</span>
                {it.hint && <span className="command-palette__item-hint">{it.hint}</span>}
              </button>
            ))
          )}
        </div>
        <div className="command-palette__foot">
          <span><CornerDownLeft size={12} /> 执行</span>
          <span>↑↓ 选择</span>
          <span>Esc 关闭</span>
        </div>
      </div>
    </div>
  );
}

// ── HelpModal（Plan27 #3b）─────────────────────────────────────────────────────

/** 「能做什么」静态披露面板：简述核心能力，帮助用户发现功能。 */
export function HelpModal({ onClose }: { onClose: () => void }) {
  const trapRef = useFocusTrap<HTMLDivElement>(true);
  const SECTIONS: Array<{ title: string; body: string }> = [
    { title: "工作区", body: "为会话绑定本地目录后，Agent 可读写其中文件、执行命令；不绑定则为纯聊天。在输入框下方胶囊里选择或更换。" },
    { title: "@ 引用文件", body: "在输入框键入 @ 触发工作区文件补全，把指定文件引用进对话上下文。" },
    { title: "/ 命令", body: "键入 / 唤出命令菜单：/compact 压缩历史、/clear 新会话、/init 生成项目记忆、/rewind 文件变更、/model 改模型、/help 本面板。工作区 .mdga/commands/*.md 可加自定义命令。" },
    { title: "计划模式", body: "开启后 Agent 先给出分步计划、等你确认再执行，避免一上来就改动。" },
    { title: "技能 / .mdga", body: "工作区 .mdga 目录可放长期记忆（MDGA.md）与自定义斜杠命令，作为项目级约定持续生效。" },
    { title: "MCP", body: "在 设置 → MCP 服务器 接入外部 MCP（stdio 或 HTTP），其工具并入模型工具集，统一经权限审批。" },
    { title: "视觉", body: "在 设置 → 模型供应商 开启「扩展模态」并配置视觉模型后，可粘贴/拖拽/导入图片让 Agent 识图。" },
    { title: "权限模式", body: "受限 / 每次询问 / 工作区自动 / 完全访问 四档，控制 Agent 改文件、跑命令前是否需要审批。可在 设置 → 权限规则 配细粒度规则。" },
    { title: "快捷键", body: "Ctrl/Cmd+N 新对话、Ctrl/Cmd+K 命令面板、Ctrl/Cmd+, 设置、Enter 发送、Shift+Enter 换行、Esc 关闭弹窗。" },
  ];
  return (
    <div
      className="approval-overlay"
      role="dialog"
      aria-modal="true"
      aria-label="帮助：能做什么"
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
    >
      <div className="approval-card panel-card help-modal" ref={trapRef}>
        <p className="approval-card__title"><HelpCircle size={16} style={{ verticalAlign: "-3px", marginRight: 6 }} />MDGA 能做什么</p>
        <div className="help-modal__body">
          {SECTIONS.map((s) => (
            <div className="help-section" key={s.title}>
              <h4 className="help-section__title">{s.title}</h4>
              <p className="help-section__body">{s.body}</p>
            </div>
          ))}
        </div>
        <div className="approval-card__actions">
          <button type="button" className="approval-card__btn" onClick={onClose}>关闭</button>
        </div>
      </div>
    </div>
  );
}
