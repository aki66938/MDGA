import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
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

type Conversation = {
  id: string;
  title: string;
  createdAt: number;
  updatedAt: number;
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

type Message = {
  role: "user" | "assistant";
  content: string;
  usage?: UsageSummary;
};

type StoredMessage = {
  id: string;
  conversationId: string;
  role: string;
  content: string;
  usageJson: string | null;
  createdAt: number;
};

type UpdateState =
  | { status: "idle" }
  | { status: "available"; version: string }
  | { status: "downloading"; progress: number }
  | { status: "error"; message: string };

const permissionMode: PermissionMode = "restricted";

// ── App ───────────────────────────────────────────────────────────────────

export function App() {
  const [apiKeyStatus, setApiKeyStatus] = useState<ApiKeyStatus>({ state: "missing" });
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [activeConvId, setActiveConvId] = useState<string | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [model, setModel] = useState<DeepSeekModelId>(DEFAULT_DEEPSEEK_MODEL_ID);
  const [update, setUpdate] = useState<UpdateState>({ status: "idle" });
  const messagesEndRef = useRef<HTMLDivElement>(null);
  // 流式积累的 assistant 内容，用于 chat-done 时持久化
  const streamingContentRef = useRef("");
  const streamingUsageRef = useRef<UsageSummary | null>(null);

  // 检测 API Key
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

  // 启动时加载会话列表
  useEffect(() => {
    loadConversations();
  }, []);

  // activeConvId 切换时加载对应消息
  useEffect(() => {
    if (!activeConvId) { setMessages([]); return; }
    invoke<StoredMessage[]>("load_messages", { conversationId: activeConvId })
      .then((stored) => setMessages(stored.map(storedToMessage)))
      .catch(() => setMessages([]));
  }, [activeConvId]);

  // 自动滚动到底部
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  // 启动后延迟检查更新
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
    return { role: s.role as "user" | "assistant", content: s.content, usage };
  }

  async function loadConversations() {
    const list = await invoke<Conversation[]>("get_conversations").catch(() => []);
    setConversations(list);
    return list;
  }

  // ── 会话操作 ────────────────────────────────────────────────────────────

  async function handleNewConversation() {
    if (sending) return;
    const conv = await invoke<Conversation>("new_conversation").catch(() => null);
    if (!conv) return;
    setConversations((prev) => [conv, ...prev]);
    setActiveConvId(conv.id);
    setMessages([]);
    setInput("");
  }

  async function handleSelectConversation(id: string) {
    if (id === activeConvId || sending) return;
    setActiveConvId(id);
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

  // ── 发送消息 ────────────────────────────────────────────────────────────

  async function handleSend() {
    const text = input.trim();
    if (!text || sending) return;

    // 没有活跃会话时自动创建
    let convId = activeConvId;
    if (!convId) {
      const conv = await invoke<Conversation>("new_conversation").catch(() => null);
      if (!conv) return;
      convId = conv.id;
      setConversations((prev) => [conv, ...prev]);
      setActiveConvId(conv.id);
    }

    // 持久化用户消息
    await invoke("persist_message", {
      conversationId: convId,
      role: "user",
      content: text,
      usageJson: null,
    }).catch(() => {});

    // 如果还是"新对话"，用首条消息前 20 字作为标题
    const currentConv = conversations.find((c) => c.id === convId);
    if (!currentConv || currentConv.title === "新对话") {
      const title = text.slice(0, 20);
      await invoke("rename_conversation", { conversationId: convId, title }).catch(() => {});
      setConversations((prev) =>
        prev.map((c) => (c.id === convId ? { ...c, title } : c))
      );
    }

    const outgoing: Message[] = [...messages, { role: "user", content: text }];
    setMessages([...outgoing, { role: "assistant", content: "" }]);
    setInput("");
    setSending(true);
    streamingContentRef.current = "";
    streamingUsageRef.current = null;

    const unlistenChunk = await listen<string>("chat-chunk", (e) => {
      streamingContentRef.current += e.payload;
      setMessages((prev) => {
        const updated = [...prev];
        const last = updated[updated.length - 1];
        updated[updated.length - 1] = { ...last, content: last.content + e.payload };
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
      unlistenChunk();
      unlistenUsage();
      unlistenDone();

      // 持久化 assistant 消息
      const usageJson = streamingUsageRef.current
        ? JSON.stringify(streamingUsageRef.current)
        : null;
      await invoke("persist_message", {
        conversationId: finalConvId,
        role: "assistant",
        content: streamingContentRef.current,
        usageJson,
      }).catch(() => {});

      // 刷新会话列表（更新 updatedAt 排序）
      const list = await invoke<Conversation[]>("get_conversations").catch(() => []);
      setConversations(list);
    });

    try {
      await invoke("send_message", { messages: outgoing, model });
    } catch (err) {
      setMessages((prev) => {
        const updated = [...prev];
        updated[updated.length - 1] = { role: "assistant", content: `错误：${err}` };
        return updated;
      });
      setSending(false);
      unlistenChunk();
      unlistenUsage();
      unlistenDone();
    }
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

        {/* 更新提示区域 */}
        {update.status === "available" && (
          <div className="update-banner">
            <p className="update-banner__title">发现新版本</p>
            <p className="update-banner__version">v{update.version}</p>
            <div className="update-banner__actions">
              <button
                className="update-banner__btn update-banner__btn--primary"
                type="button"
                onClick={handleInstallUpdate}
              >
                立即更新
              </button>
              <button
                className="update-banner__btn"
                type="button"
                onClick={() => setUpdate({ status: "idle" })}
              >
                稍后
              </button>
            </div>
          </div>
        )}

        {update.status === "downloading" && (
          <div className="update-banner">
            <p className="update-banner__title">正在下载更新…</p>
            <div className="update-banner__progress-bar">
              <div
                className="update-banner__progress-fill"
                style={{ width: `${update.progress}%` }}
              />
            </div>
            <p className="update-banner__version">{update.progress}%</p>
          </div>
        )}

        {update.status === "error" && (
          <div className="update-banner update-banner--error">
            <p className="update-banner__title">更新失败</p>
            <p className="update-banner__version">{update.message}</p>
            <button
              className="update-banner__btn"
              type="button"
              onClick={() => setUpdate({ status: "idle" })}
            >
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
          <div className="status-strip" aria-label="MVP status">
            <span>{getApiKeyStatusLabel(apiKeyStatus)}</span>
            <span>{getPermissionModeLabel(permissionMode)}</span>
            <select
              className="model-select"
              value={model}
              onChange={(e) => setModel(e.target.value as DeepSeekModelId)}
              disabled={sending}
              aria-label="模型选择"
              title={DEEPSEEK_MODELS.find((item) => item.id === model)?.description}
            >
              {DEEPSEEK_MODELS.map((item) => (
                <option key={item.id} value={item.id}>
                  {item.label}
                </option>
              ))}
            </select>
          </div>
        </header>

        {hasMessages ? (
          <section className="message-list" aria-label="Conversation">
            {messages.map((msg, i) => (
              <div key={i} className="message-row">
                <div className={`message message--${msg.role}`}>
                  {msg.role === "user" ? (
                    <p>{msg.content}</p>
                  ) : (
                    <ReactMarkdown remarkPlugins={[remarkGfm]}>
                      {msg.content}
                    </ReactMarkdown>
                  )}
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

        <div className="composer">
          <textarea
            aria-label="Message"
            placeholder="随心输入（Enter 发送，Shift+Enter 换行）"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
          />
          <button
            type="button"
            onClick={handleSend}
            disabled={!input.trim() || sending}
          >
            {sending ? "…" : "发送"}
          </button>
        </div>
      </section>
    </main>
  );
}

// ── UsageBadge ────────────────────────────────────────────────────────────

function UsageBadge({ usage }: { usage: UsageSummary }) {
  const cost = usage.estimatedCostUsd;
  const costStr =
    cost < 0.0001 && cost > 0
      ? "<$0.0001"
      : `$${cost.toFixed(6).replace(/\.?0+$/, "")}`;
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
        {costStr}
        {isEstimate && " (估算)"}
      </span>
    </div>
  );
}
