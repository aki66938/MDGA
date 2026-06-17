// 消息渲染相关子组件（0.0.37 从 App.tsx 抽出，纯搬移，无逻辑改动）。

import { useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";
import "highlight.js/styles/github.css";
import {
  CheckSquare, CircleDot, Square, Info, Check, Copy, RefreshCw, Pencil,
  ChevronDown, ChevronRight, Ban, X,
} from "lucide-react";
import type {
  TodoItem, Message, ToolPart, VisionPart, ReasoningPart, RenderBlock, UsageSummary,
} from "../types";
import { formatUsd } from "../utils";

export function TodoPanel({ items }: { items: TodoItem[] }) {
  // Agent 自维护任务清单：常驻输入框上方，实时反映多步任务进度。
  const done = items.filter((t) => t.status === "done").length;
  return (
    <div className="todo-panel" aria-label="任务清单">
      <span className="todo-panel__head">任务 {done}/{items.length}</span>
      <div className="todo-panel__items">
        {items.map((t, i) => (
          <span key={i} className={`todo-item todo-item--${t.status}`}>
            {t.status === "done" ? <CheckSquare size={13} /> : t.status === "in_progress" ? <CircleDot size={13} /> : <Square size={13} />} {t.text}
          </span>
        ))}
      </div>
    </div>
  );
}

// ── MessageContent ──────────────────────────────────────────────────────────

export function MessageContent({ msg }: { msg: Message }) {
  // 连续的工具卡片聚合成一个可折叠组；叙述文字与通知保持原位，时间轴不变。
  const blocks: RenderBlock[] = [];
  msg.parts.forEach((part, i) => {
    if (part.type === "tool") {
      const tail = blocks[blocks.length - 1];
      if (tail && tail.kind === "tools") {
        tail.parts.push(part);
      } else {
        blocks.push({ kind: "tools", parts: [part], index: i });
      }
    } else {
      blocks.push({ kind: "part", part, index: i });
    }
  });

  // ChangeSet 汇总：本条消息内所有带 diff 的文件变更聚合为一行摘要。
  const changedFiles = new Set<string>();
  let totalAdded = 0;
  let totalRemoved = 0;
  msg.parts.forEach((part) => {
    if (part.type === "tool" && (part.added !== undefined || part.removed !== undefined)) {
      changedFiles.add(part.target);
      totalAdded += part.added ?? 0;
      totalRemoved += part.removed ?? 0;
    }
  });

  return (
    <>
      {blocks.map((block) => {
        if (block.kind === "tools") {
          return <ToolGroup key={`t${block.index}`} parts={block.parts} />;
        }
        const { part, index } = block;
        if (part.type === "text") {
          return msg.role === "user" ? (
            <p key={index}>{part.content}</p>
          ) : (
            <ReactMarkdown
              key={index}
              remarkPlugins={[remarkGfm]}
              rehypePlugins={[rehypeHighlight]}
              components={{ pre: CodeBlock }}
            >
              {part.content}
            </ReactMarkdown>
          );
        }
        if (part.type === "image") {
          return (
            <img
              key={index}
              className="msg-image"
              src={`data:${part.mediaType};base64,${part.base64}`}
              alt={part.name ?? "上传的图片"}
              title={part.name}
            />
          );
        }
        if (part.type === "vision") {
          return <VisionCard key={index} part={part} />;
        }
        if (part.type === "reasoning") {
          return <ReasoningCard key={index} part={part} />;
        }
        return (
          <div key={index} className="notice-inline" aria-label="系统通知">
            <Info size={13} /> {part.text}
          </div>
        );
      })}
      {changedFiles.size > 0 && (
        <div className="changeset-summary" aria-label="变更汇总">
          本轮变更 {changedFiles.size} 个文件
          <span className="diff-added"> +{totalAdded}</span>
          <span className="diff-removed"> −{totalRemoved}</span>
        </div>
      )}
    </>
  );
}

// ── MessageActions（Plan19 P1a）─────────────────────────────────────────────

/**
 * 消息气泡 hover 操作条：复制（整条文本到剪贴板）；用户消息额外提供重发与编辑重试。
 * 仅当消息含文字时显示，避免对纯图片/纯工具卡片消息出现空操作条。
 */
export function MessageActions({
  msg,
  disabled,
  isLastAssistant,
  onCopy,
  onResend,
  onEditRetry,
  onRegenerate,
  usage,
  showCost,
}: {
  msg: Message;
  disabled: boolean;
  /** 是否为最后一条助手消息（Plan27 #1b）：仅它显示「重新生成」。 */
  isLastAssistant: boolean;
  onCopy: () => string;
  onResend: () => void;
  onEditRetry: () => void;
  onRegenerate: () => void;
  /** 该轮 token 用量（0.0.45）：随操作条同行展示，仅 assistant 消息有。 */
  usage?: UsageSummary;
  /** 是否显示成本金额（非 DeepSeek 主供应商时以「—」占位）。 */
  showCost: boolean;
}) {
  const [copied, setCopied] = useState(false);
  const hasText = msg.parts.some((p) => p.type === "text" && p.content.trim().length > 0);
  const showUsage = msg.role === "assistant" && !!usage;
  // 有文字才显示复制/重发/编辑；但最后一条助手消息即使无文字（纯工具）也要能「重新生成」；
  // 另:assistant 带 token 用量时也要出条(把每轮 token 行并入本操作条,0.0.45)。
  if (!hasText && !isLastAssistant && !showUsage) return null;

  async function handleCopy() {
    try {
      await navigator.clipboard.writeText(onCopy());
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // 剪贴板不可用时静默失败
    }
  }

  return (
    <div className={`message-actions message-actions--${msg.role}`} aria-label="消息操作">
      {showUsage && usage && <UsageBadge usage={usage} showCost={showCost} />}
      {hasText && (
        <button
          type="button"
          className="message-actions__btn"
          title={copied ? "已复制" : "复制整条消息"}
          aria-label={copied ? "已复制" : "复制整条消息"}
          onClick={handleCopy}
        >
          {copied ? <Check size={14} /> : <Copy size={14} />}
        </button>
      )}
      {/* 重新生成（Plan27 #1b）：仅最后一条助手消息显示，删旧回复后用截至上一条 user 的历史重跑。 */}
      {msg.role === "assistant" && isLastAssistant && (
        <button
          type="button"
          className="message-actions__btn"
          title="重新生成这条回复"
          aria-label="重新生成这条回复"
          onClick={onRegenerate}
          disabled={disabled}
        >
          <RefreshCw size={14} />
        </button>
      )}
      {msg.role === "user" && (
        <>
          <button
            type="button"
            className="message-actions__btn"
            title="重新发送这条消息"
            aria-label="重新发送这条消息"
            onClick={onResend}
            disabled={disabled}
          >
            <RefreshCw size={14} />
          </button>
          <button
            type="button"
            className="message-actions__btn"
            title="把这条消息回填到输入框，修改后再发送"
            aria-label="编辑重试"
            onClick={onEditRetry}
            disabled={disabled}
          >
            <Pencil size={14} />
          </button>
        </>
      )}
    </div>
  );
}

// ── ToolGroup ───────────────────────────────────────────────────────────────

export function ToolGroup({ parts }: { parts: ToolPart[] }) {
  // 连续工具调用的折叠组：执行中实时显示运行行，全部完成后默认折叠为一行摘要，
  // 点击可展开查看每一步（对标 CC/Codex 的工具过程折叠）。
  const [expanded, setExpanded] = useState(false);
  const running = parts.filter((p) => p.status === "running");
  const failed = parts.filter((p) => p.status === "failed").length;
  const denied = parts.filter((p) => p.status === "denied").length;

  // 只有一条时不加折叠壳，直接显示
  if (parts.length === 1) {
    return <ToolInlineRow part={parts[0]} />;
  }

  const visibleRows = expanded ? parts : running;

  return (
    <div className="tool-group">
      <button
        className="tool-group__summary"
        type="button"
        onClick={() => setExpanded((v) => !v)}
      >
        <span className="tool-group__caret">{expanded ? <ChevronDown size={13} /> : <ChevronRight size={13} />}</span>
        已执行 {parts.length} 个工具动作
        {failed > 0 && <span className="tool-group__failed"> · {failed} 失败</span>}
        {denied > 0 && <span className="tool-group__failed"> · {denied} 被拒</span>}
        {running.length > 0 && <span className="tool-group__running"> · 进行中…</span>}
      </button>
      {visibleRows.length > 0 && (
        <div className="tool-group__rows">
          {visibleRows.map((p, i) => (
            <ToolInlineRow key={i} part={p} />
          ))}
        </div>
      )}
    </div>
  );
}

// ── VisionCard（Plan19 C-B 前端）──────────────────────────────────────────────

/** 视觉分析可折叠卡片：默认折叠，标题「🔎 视觉分析（N 张图）」，展开见 analysis；有 usage 显示 token 徽标。 */
export function VisionCard({ part }: { part: VisionPart }) {
  const [expanded, setExpanded] = useState(false);
  const total = part.usage?.total_tokens ?? 0;
  return (
    <div className="vision-card">
      <button
        type="button"
        className="vision-card__summary"
        onClick={() => setExpanded((v) => !v)}
        aria-expanded={expanded}
      >
        <span className="vision-card__caret">{expanded ? <ChevronDown size={13} /> : <ChevronRight size={13} />}</span>
        <span className="vision-card__title">🔎 视觉分析（{part.count} 张图）</span>
        {total > 0 && (
          <span className="vision-card__usage">视觉 · {total.toLocaleString()} tokens</span>
        )}
      </button>
      {expanded && (
        <div className="vision-card__body">
          <ReactMarkdown remarkPlugins={[remarkGfm]} rehypePlugins={[rehypeHighlight]} components={{ pre: CodeBlock }}>
            {part.analysis}
          </ReactMarkdown>
        </div>
      )}
    </div>
  );
}

// ── ReasoningCard（Plan27 #1a）────────────────────────────────────────────────

/**
 * 思考过程可折叠卡片：默认折叠，标题「🧠 思考过程」，展开见 reasoning 文本（纯文本逐行渲染，
 * 不走 Markdown 以保留模型推理原貌）。流式期间随增量实时增长。样式参照 vision-card。
 */
export function ReasoningCard({ part }: { part: ReasoningPart }) {
  const [expanded, setExpanded] = useState(false);
  const content = part.content.trim();
  if (!content) return null;
  return (
    <div className="reasoning-card">
      <button
        type="button"
        className="reasoning-card__summary"
        onClick={() => setExpanded((v) => !v)}
        aria-expanded={expanded}
      >
        <span className="reasoning-card__caret"><ChevronRight size={13} /></span>
        <span className="reasoning-card__bulb" aria-hidden="true">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.8} strokeLinecap="round" strokeLinejoin="round">
            <line x1="12" y1="1.6" x2="12" y2="3" />
            <line x1="5.6" y1="3.6" x2="6.6" y2="4.7" />
            <line x1="18.4" y1="3.6" x2="17.4" y2="4.7" />
            <line x1="2.6" y1="11" x2="4.1" y2="11" />
            <line x1="19.9" y1="11" x2="21.4" y2="11" />
            <path d="M12 4.4a5.4 5.4 0 0 0-3.4 9.6c0.6 0.5 0.9 1.2 0.9 2v0.3h5v-0.3c0-0.8 0.3-1.5 0.9-2A5.4 5.4 0 0 0 12 4.4z" />
            <line x1="9.9" y1="18.6" x2="14.1" y2="18.6" />
            <line x1="10.5" y1="20.6" x2="13.5" y2="20.6" />
          </svg>
        </span>
        <span className="reasoning-card__title">思考过程</span>
      </button>
      {expanded && (
        <pre className="reasoning-card__body">{content}</pre>
      )}
    </div>
  );
}

// ── CodeBlock ───────────────────────────────────────────────────────────────

export function CodeBlock(props: React.HTMLAttributes<HTMLPreElement>) {
  // 代码块外壳：右上角悬浮复制按钮，点击复制整块代码文本。
  const preRef = useRef<HTMLPreElement>(null);
  const [copied, setCopied] = useState(false);

  async function handleCopy() {
    const text = preRef.current?.innerText ?? "";
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // 剪贴板不可用时静默失败
    }
  }

  return (
    <div className="code-block">
      <button className="code-block__copy" type="button" onClick={handleCopy}>
        {copied ? <><Check size={12} /> 已复制</> : "复制"}
      </button>
      <pre ref={preRef} {...props} />
    </div>
  );
}

// ── ToolInlineRow ───────────────────────────────────────────────────────────

export function ToolInlineRow({ part }: { part: ToolPart }) {
  const { toolName, target, status, error, diff, added, removed, liveOutput, reverted } = part;
  const [showDiff, setShowDiff] = useState(false);
  const icon =
    status === "running" ? <span className="star-spin tool-inline__star" aria-hidden="true">✦</span> :
    status === "succeeded" ? <Check size={13} /> :
    status === "denied" ? <Ban size={13} /> : <X size={13} />;
  const hasDiff = typeof diff === "string" && diff.trim().length > 0;
  return (
    <div className="tool-inline-wrap">
      <div
        className={`tool-inline tool-inline--${status}${hasDiff ? " tool-inline--clickable" : ""}${reverted ? " tool-inline--reverted" : ""}`}
        aria-label={`${toolName} ${status}${reverted ? " 已回退" : ""}`}
        onClick={hasDiff ? () => setShowDiff((v) => !v) : undefined}
        role={hasDiff ? "button" : undefined}
      >
        <span className="tool-inline__icon" aria-hidden="true">{icon}</span>
        <span className="tool-inline__name">{toolName}</span>
        {target && <span className="tool-inline__target">{target}</span>}
        {/* Plan21 #3：回退后角标,与置灰样式配合标识该变更已撤销。 */}
        {reverted && <span className="tool-inline__reverted-badge">已回退</span>}
        {(added !== undefined || removed !== undefined) && (
          <span className="tool-inline__stats">
            {added ? <span className="diff-added">+{added}</span> : null}
            {removed ? <span className="diff-removed">−{removed}</span> : null}
          </span>
        )}
        {hasDiff && (
          <span className="tool-inline__expand">
            {showDiff ? <ChevronDown size={13} /> : <ChevronRight size={13} />} diff
          </span>
        )}
        {status === "running" && <span className="tool-inline__dots" aria-hidden="true">…</span>}
        {status === "denied" && <span className="tool-inline__error">{error ?? "已拒绝"}</span>}
        {status === "failed" && error && (
          <span className="tool-inline__error">{error}</span>
        )}
      </div>
      {status === "running" && liveOutput && (
        <pre className="tool-live-output">{liveOutput}</pre>
      )}
      {showDiff && hasDiff && <DiffBlock diff={diff!} />}
    </div>
  );
}

// ── DiffBlock ───────────────────────────────────────────────────────────────

export function DiffBlock({ diff }: { diff: string }) {
  // 按行渲染 unified diff：+ 绿、- 红、@@ 弱化。
  return (
    <pre className="diff-block">
      {diff.split("\n").map((line, i) => {
        const cls = line.startsWith("+")
          ? "diff-line--add"
          : line.startsWith("-")
            ? "diff-line--del"
            : line.startsWith("@@")
              ? "diff-line--hunk"
              : "";
        return (
          <span key={i} className={`diff-line ${cls}`}>
            {line}
            {"\n"}
          </span>
        );
      })}
    </pre>
  );
}

// ── UsageBadge ────────────────────────────────────────────────────────────

// showCost（Plan21 #5）：非 DeepSeek 主供应商时按 DeepSeek 价表算出的金额会误导，
// 此时金额位以「—」占位（token 数照常展示）。
export function UsageBadge({ usage, showCost }: { usage: UsageSummary; showCost: boolean }) {
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
        {showCost ? <>{costStr}{isEstimate && " (估算)"}</> : <span className="usage-cost--na" title="该供应商暂无成本价表，金额不可估算">—</span>}
      </span>
    </div>
  );
}

// ── ConversationUsageSummary ───────────────────────────────────────────────

export function ConversationUsageSummary({ usage, showCost }: { usage: UsageSummary; showCost: boolean }) {
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
      <span className="usage-cost">
        {showCost ? formatUsd(usage.estimatedCostUsd) : <span className="usage-cost--na" title="该供应商暂无成本价表，金额不可估算">—</span>}
      </span>
    </div>
  );
}
