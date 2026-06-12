import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  DEEPSEEK_MODELS,
  DEFAULT_DEEPSEEK_MODEL_ID,
  getApiKeyStatusLabel,
  getPermissionModeLabel,
  type ApiKeyStatus,
  type DeepSeekModelId,
  type PermissionMode,
} from "@mdga/ui";
import "./styles.css";

// ── 类型定义 ──────────────────────────────────────────────────────────────

/** 消息中的文字块，直接 Markdown 渲染 */
type TextPart = { type: "text"; content: string };

/** 消息中的工具执行卡片，内联展示于叙述文字之间 */
type ToolPart = {
  type: "tool";
  toolName: string;
  target: string;
  status: "running" | "succeeded" | "failed" | "denied";
  error?: string;
};

/** 高风险动作审批请求，由后端在 AskEveryTime / 越界场景下发起 */
type ApprovalRequest = {
  actionId: string;
  toolName: string;
  target: string;
};

type MessagePart = TextPart | ToolPart;

type Message = {
  role: "user" | "assistant";
  /** 所有内容都用 parts 表示，文字与工具卡片交错排列 */
  parts: MessagePart[];
  usage?: UsageSummary;
};

type UsageSummary = {
  promptTokens: number;
  completionTokens: number;
  totalTokens: number;
  cacheHitTokens: number;
  cacheMissTokens: number;
  reasoningTokens: number;
  estimatedCostUsd: number;
  usageSource: string;
  pricingVersion: string;
};

type Conversation = {
  id: string;
  title: string;
  workspacePath?: string | null;
  workspaceName?: string | null;
  mode: "chat_only" | "local_workspace";
  createdAt: number;
  updatedAt: number;
};

type StoredMessage = {
  id: string;
  conversationId: string;
  role: string;
  content: string;
  usageJson: string | null;
  createdAt: number;
};

type ToolEvent = {
  toolName: string;
  status: string;
  inputJson?: string | null;
  outputJson?: string | null;
  errorMessage?: string | null;
};

type DraftWorkspace = {
  name: string;
  path: string;
};

type UpdateState =
  | { status: "idle" }
  | { status: "available"; version: string }
  | { status: "downloading"; progress: number }
  | { status: "error"; message: string };

const PERMISSION_MODES: PermissionMode[] = [
  "restricted",
  "ask_every_time",
  "workspace_auto",
  "full_access",
];

// ── App ───────────────────────────────────────────────────────────────────

export function App() {
  const [apiKeyStatus, setApiKeyStatus] = useState<ApiKeyStatus>({ state: "missing" });
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [activeConvId, setActiveConvId] = useState<string | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [model, setModel] = useState<DeepSeekModelId>(DEFAULT_DEEPSEEK_MODEL_ID);
  const [permissionMode, setPermissionMode] = useState<PermissionMode>("workspace_auto");
  const [draftWorkspace, setDraftWorkspace] = useState<DraftWorkspace | null>(null);
  const [workspaceError, setWorkspaceError] = useState<string | null>(null);
  const [approval, setApproval] = useState<ApprovalRequest | null>(null);
  const [update, setUpdate] = useState<UpdateState>({ status: "idle" });
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const streamingTextRef = useRef(""); // 只累积纯文字内容，用于 chat-done 持久化
  const streamingUsageRef = useRef<UsageSummary | null>(null);

  useEffect(() => {
    invoke<string>("get_deepseek_api_key_status")
      .then((raw) => {
        const state =
          raw === "Configured" ? "configured" :
          raw === "ConnectionFailed" ? "connection_failed" :
          "missing";
        setApiKeyStatus({ state });
      })
      .catch(() => setApiKeyStatus({ state: "missing" }));
  }, []);

  useEffect(() => {
    loadConversations();
  }, []);

  useEffect(() => {
    if (!activeConvId) { setMessages([]); return; }
    invoke<StoredMessage[]>("load_messages", { conversationId: activeConvId })
      .then((stored) => setMessages(stored.map(storedToMessage)))
      .catch(() => setMessages([]));
  }, [activeConvId]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  // 持续监听高风险动作审批请求，弹出确认框
  useEffect(() => {
    const unlisten = listen<ApprovalRequest>("approval-request", (e) => {
      setApproval(e.payload);
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  useEffect(() => {
    const timer = setTimeout(() => {
      invoke<string | null>("check_update")
        .then((v) => { if (v) setUpdate({ status: "available", version: v }); })
        .catch(() => {});
    }, 3000);
    const unlistenProgress = listen<number>("update-progress", (e) => {
      setUpdate({ status: "downloading", progress: e.payload });
    });
    return () => {
      clearTimeout(timer);
      unlistenProgress.then((fn) => fn());
    };
  }, []);

  // ── 工具函数 ────────────────────────────────────────────────────────────

  function storedToMessage(s: StoredMessage): Message {
    const usage = s.usageJson ? JSON.parse(s.usageJson) as UsageSummary : undefined;
    return {
      role: s.role as "user" | "assistant",
      parts: [{ type: "text", content: s.content }],
      usage,
    };
  }

  /** 从工具输入参数提取展示目标（path / from / command）*/
  function extractTarget(inputJson: string | null | undefined): string {
    if (!inputJson) return "";
    try {
      const parsed = JSON.parse(inputJson) as Record<string, unknown>;
      const target = parsed.path ?? parsed.from ?? parsed.command ?? "";
      return typeof target === "string" ? target : "";
    } catch {
      return "";
    }
  }

  async function loadConversations() {
    const list = await invoke<Conversation[]>("get_conversations").catch(() => []);
    setConversations(list);
    return list;
  }

  // ── 会话操作 ────────────────────────────────────────────────────────────

  async function handleNewConversation() {
    if (sending) return;
    setActiveConvId(null);
    setMessages([]);
    setInput("");
    setDraftWorkspace(null);
    setWorkspaceError(null);
  }

  async function handleSelectConversation(id: string) {
    if (id === activeConvId || sending) return;
    setActiveConvId(id);
    setDraftWorkspace(null);
    setWorkspaceError(null);
  }

  async function handleDeleteConversation(e: React.MouseEvent, id: string) {
    e.stopPropagation();
    await invoke("remove_conversation", { conversationId: id }).catch(() => {});
    setConversations((prev) => prev.filter((c) => c.id !== id));
    if (activeConvId === id) {
      setActiveConvId(null);
      setMessages([]);
    }
  }

  async function handleSelectWorkspace() {
    try {
      const selected = await open({ directory: true, multiple: false });
      if (!selected || Array.isArray(selected)) return;
      setDraftWorkspace({ path: selected, name: basenameFromPath(selected) });
      setWorkspaceError(null);
    } catch (err) {
      setWorkspaceError(String(err));
    }
  }

  // ── 发送消息 ────────────────────────────────────────────────────────────

  async function handleSend() {
    const text = input.trim();
    if (!text || sending) return;

    let convId = activeConvId;
    if (!convId) {
      const conv = await invoke<Conversation>("new_conversation_with_workspace", {
        workspacePath: draftWorkspace?.path ?? null,
      }).catch((err) => {
        setWorkspaceError(String(err));
        return null;
      });
      if (!conv) return;
      convId = conv.id;
      setConversations((prev) => [conv, ...prev]);
      setActiveConvId(conv.id);
      setWorkspaceError(null);
    }

    await invoke("persist_message", {
      conversationId: convId,
      role: "user",
      content: text,
      usageJson: null,
    }).catch(() => {});

    const currentConv = conversations.find((c) => c.id === convId);
    if (!currentConv || currentConv.title === "新对话") {
      const title = text.slice(0, 20);
      await invoke("rename_conversation", { conversationId: convId, title }).catch(() => {});
      setConversations((prev) =>
        prev.map((c) => (c.id === convId ? { ...c, title } : c))
      );
    }

    // 构建发给后端的纯文字消息（parts 中只取 text 块）
    const outgoing: Message[] = [...messages, { role: "user", parts: [{ type: "text", content: text }] }];
    setMessages([...outgoing, { role: "assistant", parts: [] }]);
    setInput("");
    setSending(true);
    streamingTextRef.current = "";
    streamingUsageRef.current = null;

    // ── 流式事件监听 ────────────────────────────────────────────────────

    // chat-chunk：追加文字到当前 assistant 消息的最后一个 text part
    const unlistenChunk = await listen<string>("chat-chunk", (e) => {
      streamingTextRef.current += e.payload;
      setMessages((prev) => {
        const updated = [...prev];
        const lastIdx = updated.length - 1;
        const last = updated[lastIdx];
        const parts = [...last.parts];
        const tail = parts[parts.length - 1];
        if (tail?.type === "text") {
          parts[parts.length - 1] = { type: "text", content: tail.content + e.payload };
        } else {
          parts.push({ type: "text", content: e.payload });
        }
        updated[lastIdx] = { ...last, parts };
        return updated;
      });
    });

    // tool-event：running 时插入新卡片，succeeded/failed 时更新最近匹配的 running 卡片
    const unlistenTool = await listen<ToolEvent>("tool-event", (e) => {
      const { toolName, status, inputJson, errorMessage } = e.payload;
      const target = extractTarget(inputJson);
      setMessages((prev) => {
        const updated = [...prev];
        const lastIdx = updated.length - 1;
        const last = updated[lastIdx];
        if (last.role !== "assistant") return prev;
        const parts = [...last.parts];
        if (status === "running") {
          parts.push({ type: "tool", toolName, target, status: "running" });
        } else if (status === "denied") {
          // 被拒绝的工具没有 running 阶段，直接插入一张拒绝卡片
          parts.push({
            type: "tool",
            toolName,
            target,
            status: "denied",
            error: errorMessage ?? undefined,
          });
        } else {
          // 从后往前找同名的最近 running 卡片并更新状态
          for (let i = parts.length - 1; i >= 0; i--) {
            const p = parts[i];
            if (p.type === "tool" && p.toolName === toolName && p.status === "running") {
              parts[i] = {
                ...p,
                status: status as "succeeded" | "failed",
                error: errorMessage ?? undefined,
              };
              break;
            }
          }
        }
        updated[lastIdx] = { ...last, parts };
        return updated;
      });
    });

    const unlistenUsage = await listen<UsageSummary>("chat-usage", (e) => {
      streamingUsageRef.current = e.payload;
      setMessages((prev) => {
        const updated = [...prev];
        const last = updated[updated.length - 1];
        updated[updated.length - 1] = { ...last, usage: e.payload };
        return updated;
      });
    });

    const finalConvId = convId;
    const unlistenDone = await listen("chat-done", async () => {
      setSending(false);
      setApproval(null);
      unlistenChunk();
      unlistenTool();
      unlistenUsage();
      unlistenDone();

      const usageJson = streamingUsageRef.current
        ? JSON.stringify(streamingUsageRef.current)
        : null;
      // 持久化时只存纯文字内容，工具卡片是运行时状态不落库
      await invoke("persist_message", {
        conversationId: finalConvId,
        role: "assistant",
        content: streamingTextRef.current,
        usageJson,
      }).catch(() => {});

      const list = await invoke<Conversation[]>("get_conversations").catch(() => []);
      setConversations(list);
    });

    try {
      await invoke("send_message", {
        conversationId: finalConvId,
        messages: outgoing.map((m) => ({
          role: m.role,
          content: m.parts.filter((p) => p.type === "text").map((p) => (p as TextPart).content).join(""),
        })),
        model,
        permissionMode,
      });
    } catch (err) {
      setMessages((prev) => {
        const updated = [...prev];
        updated[updated.length - 1] = {
          role: "assistant",
          parts: [{ type: "text", content: `错误：${err}` }],
        };
        return updated;
      });
      setSending(false);
      unlistenChunk();
      unlistenTool();
      unlistenUsage();
      unlistenDone();
    }
  }

  async function handleStop() {
    // 若有挂起的审批请求，先拒绝它，避免后端工具循环卡在等待中
    if (approval) {
      await invoke("respond_approval", { actionId: approval.actionId, approved: false }).catch(() => {});
      setApproval(null);
    }
    if (!activeConvId) return;
    await invoke("cancel_agent", { conversationId: activeConvId }).catch(() => {});
  }

  async function respondApproval(approved: boolean) {
    if (!approval) return;
    const actionId = approval.actionId;
    setApproval(null);
    await invoke("respond_approval", { actionId, approved }).catch(() => {});
  }

  function handleKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  }

  async function handleInstallUpdate() {
    setUpdate({ status: "downloading", progress: 0 });
    try {
      await invoke("install_update");
    } catch (err) {
      setUpdate({ status: "error", message: String(err) });
    }
  }

  const hasMessages = messages.length > 0;
  const activeConversation = conversations.find((conv) => conv.id === activeConvId);
  const conversationUsage = aggregateUsage(messages);

  // ── UI ──────────────────────────────────────────────────────────────────

  return (
    <main className="app-shell">
      {/* 侧边栏 */}
      <aside className="sidebar" aria-label="MDGA navigation">
        <button className="new-chat" type="button" onClick={handleNewConversation}>
          + 新对话
        </button>

        {conversations.length > 0 && (
          <nav className="conv-list" aria-label="会话列表">
            <p className="nav-label">历史对话</p>
            {conversations.map((conv) => (
              <div
                key={conv.id}
                className={`conv-item${conv.id === activeConvId ? " conv-item--active" : ""}`}
                onClick={() => handleSelectConversation(conv.id)}
                role="button"
                tabIndex={0}
                onKeyDown={(e) => e.key === "Enter" && handleSelectConversation(conv.id)}
              >
                <span className="conv-item__title">{conv.title}</span>
                <button
                  className="conv-item__delete"
                  type="button"
                  aria-label="删除会话"
                  onClick={(e) => handleDeleteConversation(e, conv.id)}
                >
                  ×
                </button>
              </div>
            ))}
          </nav>
        )}

        {update.status === "available" && (
          <div className="update-banner">
            <p className="update-banner__title">发现新版本</p>
            <p className="update-banner__version">v{update.version}</p>
            <div className="update-banner__actions">
              <button className="update-banner__btn update-banner__btn--primary" type="button" onClick={handleInstallUpdate}>
                立即更新
              </button>
              <button className="update-banner__btn" type="button" onClick={() => setUpdate({ status: "idle" })}>
                稍后
              </button>
            </div>
          </div>
        )}

        {update.status === "downloading" && (
          <div className="update-banner">
            <p className="update-banner__title">正在下载更新…</p>
            <div className="update-banner__progress-bar">
              <div className="update-banner__progress-fill" style={{ width: `${update.progress}%` }} />
            </div>
            <p className="update-banner__version">{update.progress}%</p>
          </div>
        )}

        {update.status === "error" && (
          <div className="update-banner update-banner--error">
            <p className="update-banner__title">更新失败</p>
            <p className="update-banner__version">{update.message}</p>
            <button className="update-banner__btn" type="button" onClick={() => setUpdate({ status: "idle" })}>
              关闭
            </button>
          </div>
        )}
      </aside>

      {/* 工作区 */}
      <section className="workspace" aria-label="MDGA workspace">
        <header className="topbar">
          <div>
            <p className="eyebrow">Make DeepSeek Great Again</p>
            <h1>MDGA</h1>
          </div>
          <div className="status-strip" aria-label="status">
            <span>{getApiKeyStatusLabel(apiKeyStatus)}</span>
            <select
              className="model-select"
              value={permissionMode}
              onChange={(e) => setPermissionMode(e.target.value as PermissionMode)}
              disabled={sending}
              aria-label="权限模式"
            >
              {PERMISSION_MODES.map((mode) => (
                <option key={mode} value={mode}>{getPermissionModeLabel(mode)}</option>
              ))}
            </select>
            {activeConversation?.workspaceName && (
              <span title={activeConversation.workspacePath ?? undefined}>
                {activeConversation.workspaceName}
              </span>
            )}
            <select
              className="model-select"
              value={model}
              onChange={(e) => setModel(e.target.value as DeepSeekModelId)}
              disabled={sending}
              aria-label="模型选择"
              title={DEEPSEEK_MODELS.find((item) => item.id === model)?.description}
            >
              {DEEPSEEK_MODELS.map((item) => (
                <option key={item.id} value={item.id}>{item.label}</option>
              ))}
            </select>
          </div>
        </header>

        {hasMessages ? (
          <section className="message-list" aria-label="Conversation">
            {messages.map((msg, i) => (
              <div key={i} className="message-row">
                <div className={`message message--${msg.role}`}>
                  <MessageContent msg={msg} />
                </div>
                {msg.role === "assistant" && msg.usage && (
                  <UsageBadge usage={msg.usage} />
                )}
              </div>
            ))}
            <div ref={messagesEndRef} />
          </section>
        ) : (
          <section className="hero-panel" aria-label="New conversation">
            <h2>我们应该在 MDGA 中做些什么？</h2>
            <section className="workspace-picker" aria-label="New conversation workspace">
              <button type="button" onClick={handleSelectWorkspace}>
                选择工作区
              </button>
              {draftWorkspace ? (
                <div className="workspace-picker__selected">
                  <strong>{draftWorkspace.name}</strong>
                  <span title={draftWorkspace.path}>{draftWorkspace.path}</span>
                </div>
              ) : (
                <p>未绑定工作区，仅聊天模式</p>
              )}
              {workspaceError && <p className="workspace-picker__error">{workspaceError}</p>}
            </section>
            <section className="mvp-grid" aria-label="MVP status cards">
              <article>
                <h3>DeepSeek 连接</h3>
                <p>只从环境变量读取 API Key，不在应用内保存。</p>
              </article>
              <article>
                <h3>Token 账本</h3>
                <p>记录请求级 usage、缓存命中与估算费用。</p>
              </article>
              <article>
                <h3>权限模式</h3>
                <p>默认受限，高风险动作进入审批与审计。</p>
              </article>
            </section>
          </section>
        )}

        {conversationUsage && (
          <ConversationUsageSummary usage={conversationUsage} />
        )}

        <div className="composer">
          <textarea
            aria-label="Message"
            placeholder="随心输入（Enter 发送，Shift+Enter 换行）"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
          />
          {sending ? (
            <button type="button" className="composer__stop" onClick={handleStop}>
              停止
            </button>
          ) : (
            <button type="button" onClick={handleSend} disabled={!input.trim()}>
              发送
            </button>
          )}
        </div>
      </section>

      {approval && (
        <ApprovalModal
          approval={approval}
          onAllow={() => respondApproval(true)}
          onDeny={() => respondApproval(false)}
        />
      )}
    </main>
  );
}

// ── ApprovalModal ───────────────────────────────────────────────────────────

function ApprovalModal({
  approval,
  onAllow,
  onDeny,
}: {
  approval: ApprovalRequest;
  onAllow: () => void;
  onDeny: () => void;
}) {
  return (
    <div className="approval-overlay" role="dialog" aria-modal="true" aria-label="高风险动作审批">
      <div className="approval-card">
        <p className="approval-card__title">Agent 请求执行高风险动作</p>
        <div className="approval-card__detail">
          <span className="approval-card__tool">{approval.toolName}</span>
          {approval.target && (
            <span className="approval-card__target">{approval.target}</span>
          )}
        </div>
        <p className="approval-card__hint">是否允许本次操作？</p>
        <div className="approval-card__actions">
          <button type="button" className="approval-card__btn approval-card__btn--allow" onClick={onAllow}>
            允许一次
          </button>
          <button type="button" className="approval-card__btn" onClick={onDeny}>
            拒绝
          </button>
        </div>
      </div>
    </div>
  );
}

// ── MessageContent ──────────────────────────────────────────────────────────

function MessageContent({ msg }: { msg: Message }) {
  return (
    <>
      {msg.parts.map((part, i) => {
        if (part.type === "text") {
          return msg.role === "user" ? (
            <p key={i}>{part.content}</p>
          ) : (
            <ReactMarkdown key={i} remarkPlugins={[remarkGfm]}>
              {part.content}
            </ReactMarkdown>
          );
        }
        if (part.type === "tool") {
          return <ToolInlineRow key={i} part={part} />;
        }
        return null;
      })}
    </>
  );
}

// ── ToolInlineRow ───────────────────────────────────────────────────────────

function ToolInlineRow({ part }: { part: ToolPart }) {
  const { toolName, target, status, error } = part;
  const icon =
    status === "running" ? "⚙" :
    status === "succeeded" ? "✓" :
    status === "denied" ? "⊘" : "✗";
  return (
    <div className={`tool-inline tool-inline--${status}`} aria-label={`${toolName} ${status}`}>
      <span className="tool-inline__icon" aria-hidden="true">{icon}</span>
      <span className="tool-inline__name">{toolName}</span>
      {target && <span className="tool-inline__target">{target}</span>}
      {status === "running" && <span className="tool-inline__dots" aria-hidden="true">…</span>}
      {status === "denied" && <span className="tool-inline__error">{error ?? "已拒绝"}</span>}
      {status === "failed" && error && (
        <span className="tool-inline__error">{error}</span>
      )}
    </div>
  );
}

// ── UsageBadge ────────────────────────────────────────────────────────────

function UsageBadge({ usage }: { usage: UsageSummary }) {
  const costStr = formatUsd(usage.estimatedCostUsd);
  const isEstimate = usage.usageSource !== "deepseek_usage";

  return (
    <div className="usage-badge" aria-label="Token usage">
      <span>{usage.totalTokens.toLocaleString()} tokens</span>
      <span className="usage-sep">·</span>
      <span>{usage.promptTokens.toLocaleString()} in</span>
      <span className="usage-sep">/</span>
      <span>{usage.completionTokens.toLocaleString()} out</span>
      {usage.cacheHitTokens > 0 && (
        <>
          <span className="usage-sep">·</span>
          <span className="usage-cache">{usage.cacheHitTokens.toLocaleString()} cached</span>
        </>
      )}
      <span className="usage-sep">·</span>
      <span className="usage-cost">
        {costStr}{isEstimate && " (估算)"}
      </span>
    </div>
  );
}

// ── ConversationUsageSummary ───────────────────────────────────────────────

function ConversationUsageSummary({ usage }: { usage: UsageSummary }) {
  return (
    <div className="conversation-usage" aria-label="Conversation token summary">
      <span className="conversation-usage__label">会话累计</span>
      <span>{usage.totalTokens.toLocaleString()} tokens</span>
      <span className="usage-sep">·</span>
      <span>{usage.promptTokens.toLocaleString()} in</span>
      <span className="usage-sep">/</span>
      <span>{usage.completionTokens.toLocaleString()} out</span>
      {usage.reasoningTokens > 0 && (
        <>
          <span className="usage-sep">·</span>
          <span>{usage.reasoningTokens.toLocaleString()} reasoning</span>
        </>
      )}
      {usage.cacheHitTokens > 0 && (
        <>
          <span className="usage-sep">·</span>
          <span className="usage-cache">{usage.cacheHitTokens.toLocaleString()} cached</span>
        </>
      )}
      <span className="usage-sep">·</span>
      <span className="usage-cost">{formatUsd(usage.estimatedCostUsd)}</span>
    </div>
  );
}

// ── 工具函数 ──────────────────────────────────────────────────────────────

function aggregateUsage(messages: Message[]): UsageSummary | null {
  const usages = messages
    .map((m) => m.usage)
    .filter((u): u is UsageSummary => Boolean(u));
  if (usages.length === 0) return null;
  const pricingVersions = new Set(usages.map((u) => u.pricingVersion));
  const usageSources = new Set(usages.map((u) => u.usageSource));
  return usages.reduce<UsageSummary>(
    (total, u) => ({
      promptTokens: total.promptTokens + u.promptTokens,
      completionTokens: total.completionTokens + u.completionTokens,
      totalTokens: total.totalTokens + u.totalTokens,
      cacheHitTokens: total.cacheHitTokens + u.cacheHitTokens,
      cacheMissTokens: total.cacheMissTokens + u.cacheMissTokens,
      reasoningTokens: total.reasoningTokens + u.reasoningTokens,
      estimatedCostUsd: total.estimatedCostUsd + u.estimatedCostUsd,
      usageSource: usageSources.size === 1 ? u.usageSource : "mixed",
      pricingVersion: pricingVersions.size === 1 ? u.pricingVersion : "mixed",
    }),
    {
      promptTokens: 0, completionTokens: 0, totalTokens: 0,
      cacheHitTokens: 0, cacheMissTokens: 0, reasoningTokens: 0,
      estimatedCostUsd: 0, usageSource: "", pricingVersion: "",
    }
  );
}

function formatUsd(cost: number): string {
  if (cost < 0.0001 && cost > 0) return "<$0.0001";
  return `$${cost.toFixed(6).replace(/\.?0+$/, "")}`;
}

function basenameFromPath(path: string): string {
  const normalized = path.replace(/[\\/]+$/, "");
  const parts = normalized.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}
