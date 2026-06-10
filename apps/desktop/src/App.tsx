import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  getApiKeyStatusLabel,
  getPermissionModeLabel,
  type ApiKeyStatus,
  type PermissionMode
} from "@mdga/ui";
import "./styles.css";

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

type UpdateState =
  | { status: "idle" }
  | { status: "available"; version: string }
  | { status: "downloading"; progress: number }
  | { status: "error"; message: string };

const permissionMode: PermissionMode = "restricted";

export function App() {
  const [apiKeyStatus, setApiKeyStatus] = useState<ApiKeyStatus>({ state: "missing" });
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [update, setUpdate] = useState<UpdateState>({ status: "idle" });
  const messagesEndRef = useRef<HTMLDivElement>(null);

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

  // 启动后延迟 3 秒检查更新，不阻塞主流程
  useEffect(() => {
    const timer = setTimeout(() => {
      invoke<string | null>("check_update")
        .then((version) => {
          if (version) setUpdate({ status: "available", version });
        })
        .catch(() => {}); // 静默失败，不打扰用户
    }, 3000);

    // 监听下载进度事件
    const unlistenProgress = listen<number>("update-progress", (e) => {
      setUpdate({ status: "downloading", progress: e.payload });
    });

    return () => {
      clearTimeout(timer);
      unlistenProgress.then((fn) => fn());
    };
  }, []);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  async function handleInstallUpdate() {
    setUpdate({ status: "downloading", progress: 0 });
    try {
      await invoke("install_update");
    } catch (err) {
      setUpdate({ status: "error", message: String(err) });
    }
  }

  async function handleSend() {
    const text = input.trim();
    if (!text || sending) return;

    const outgoing: Message[] = [...messages, { role: "user", content: text }];
    setMessages([...outgoing, { role: "assistant", content: "" }]);
    setInput("");
    setSending(true);

    const unlistenChunk = await listen<string>("chat-chunk", (e) => {
      setMessages((prev) => {
        const updated = [...prev];
        const last = updated[updated.length - 1];
        updated[updated.length - 1] = { ...last, content: last.content + e.payload };
        return updated;
      });
    });

    const unlistenUsage = await listen<UsageSummary>("chat-usage", (e) => {
      setMessages((prev) => {
        const updated = [...prev];
        const last = updated[updated.length - 1];
        updated[updated.length - 1] = { ...last, usage: e.payload };
        return updated;
      });
    });

    const unlistenDone = await listen("chat-done", () => {
      setSending(false);
      unlistenChunk();
      unlistenUsage();
      unlistenDone();
    });

    try {
      await invoke("send_message", { messages: outgoing });
    } catch (err) {
      setMessages((prev) => {
        const updated = [...prev];
        updated[updated.length - 1] = {
          role: "assistant",
          content: `错误：${err}`
        };
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

  const hasMessages = messages.length > 0;

  return (
    <main className="app-shell">
      <aside className="sidebar" aria-label="MDGA navigation">
        <button className="new-chat" type="button">新对话</button>
        <nav>
          <p className="nav-label">项目</p>
          <button className="nav-item active" type="button">MDGA</button>
        </nav>

        {/* 更新提示区域，固定在侧边栏底部 */}
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

      <section className="workspace" aria-label="MDGA workspace">
        <header className="topbar">
          <div>
            <p className="eyebrow">Make DeepSeek Great Again</p>
            <h1>MDGA</h1>
          </div>
          <div className="status-strip" aria-label="MVP status">
            <span>{getApiKeyStatusLabel(apiKeyStatus)}</span>
            <span>{getPermissionModeLabel(permissionMode)}</span>
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
          <button type="button" onClick={handleSend} disabled={!input.trim() || sending}>
            {sending ? "…" : "发送"}
          </button>
        </div>
      </section>
    </main>
  );
}

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
