import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import { useEffect, useRef, useState } from "react";
import {
  Settings2,
  ListChecks, Square, ArrowUp, GitCompare,
  Check, X, Ban, Info,
  ChevronDown, FolderOpen, Gauge, AtSign, CornerDownRight,
  Plus, MessageCircle, Cpu,
} from "lucide-react";
import {
  DEFAULT_DEEPSEEK_MODEL_ID,
  getApiKeyStatusLabel,
  getPermissionModeLabel,
  type ApiKeyStatus,
  type PermissionMode,
} from "@mdga/ui";
import "./styles.css";
import {
  PERMISSION_SHORT,
  PERMISSION_MODES,
  IMAGE_EXTENSIONS,
  SLASH_COMMANDS,
  INIT_PROMPT,
  type TextPart,
  type TodoItem,
  type FileCheckpoint,
  type AppInfo,
  type UserBalance,
  type BalanceState,
  type McpServer,
  type ApprovalRequest,
  type AskUserRequest,
  type ImagePart,
  type RawUsageWire,
  type ReasoningPart,
  type MessagePart,
  type Message,
  type UsageSummary,
  type Conversation,
  type StoredMessage,
  type ToolEvent,
  type DraftWorkspace,
  type ProviderConfig,
  type SettingsSection,
} from "./types";
import {
  fmtTokens,
  aggregateUsage,
  findToolMarkupIndex,
  humanizeError,
  basenameFromPath,
} from "./utils";
import {
  TodoPanel,
  MessageContent,
  MessageActions,
  UsageBadge,
  ConversationUsageSummary,
} from "./components/messages";
import {
  ChangesModal,
  ApprovalModal,
  AskUserModal,
  CommandPalette,
  HelpModal,
} from "./components/dialogs";
import { SettingsModal } from "./components/settings";
import { Sidebar } from "./components/Sidebar";
import { useTheme, useToasts, useUpdate, useKeyboardShortcuts } from "./hooks";

// 类型与常量已抽至 ./types；本文件仅保留 App 组件与子组件实现。

// ── App ───────────────────────────────────────────────────────────────────

export function App() {
  const [apiKeyStatus, setApiKeyStatus] = useState<ApiKeyStatus>({ state: "missing" });
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [activeConvId, setActiveConvId] = useState<string | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  // 主模型 id（Plan20 🔴1）：单一真相源 = 主 provider 的 model_id（设置→模型供应商配）。
  // 类型放宽为 string 以容纳任意供应商模型名；初始化为默认 DeepSeek 值仅作兜底占位，
  // 挂载/配置后用 get_model_provider_config('main').modelId 覆盖。后端忽略此入参选模型。
  const [model, setModel] = useState<string>(DEFAULT_DEEPSEEK_MODEL_ID);
  // 控制行只读「当前模型」胶囊展示用：主 provider 的 model_id（未配时为空）。
  const [mainModelId, setMainModelId] = useState<string>("");
  // 主 provider 预设（Plan21 #5）：决定余额查询门禁与成本金额展示。
  // 取自 get_model_provider_config('main').preset；未配或缺省视为 deepseek（与后端 preset 默认一致），不误伤默认 DeepSeek 用户。
  const [mainPreset, setMainPreset] = useState<string>("deepseek");
  // 主 provider 是否 DeepSeek：成本金额位与余额查询的统一门禁（Plan21 #5）。
  const isDeepseekMain = mainPreset === "deepseek";
  const [permissionMode, setPermissionMode] = useState<PermissionMode>("workspace_auto");
  const [draftWorkspace, setDraftWorkspace] = useState<DraftWorkspace | null>(null);
  const [workspaceError, setWorkspaceError] = useState<string | null>(null);
  // 工作区胶囊小菜单开合（B2）：footer 内胶囊点击弹出「选择/更换」「纯聊天」两项。
  const [workspaceMenuOpen, setWorkspaceMenuOpen] = useState(false);
  const [approval, setApproval] = useState<ApprovalRequest | null>(null);
  const [askUser, setAskUser] = useState<AskUserRequest | null>(null);
  // 侧边栏：搜索过滤、行内重命名、归档区展开
  const [searchQuery, setSearchQuery] = useState("");
  const [editingConvId, setEditingConvId] = useState<string | null>(null);
  const [editingTitle, setEditingTitle] = useState("");
  const [showArchived, setShowArchived] = useState(false);
  // Agent 实时状态：思考中 / 执行工具 / 压缩上下文 / 输出中，发送期间常驻显示
  const [agentStatus, setAgentStatus] = useState<string | null>(null);
  const [elapsedSec, setElapsedSec] = useState(0);
  // Agent 自维护任务清单（todo_write），常驻输入框上方
  const [todos, setTodos] = useState<TodoItem[]>([]);
  // 计划模式：先出计划等确认，本轮不执行工具
  const [planMode, setPlanMode] = useState(false);
  // C-4 计划闭环：上一轮以计划模式产出了助手回复，等待用户「批准并执行」。
  // chat-done 成功收尾时置 true（仅当该轮为计划轮）；新建/切换会话、下一次普通发送时清除。
  const [awaitingPlanApproval, setAwaitingPlanApproval] = useState(false);
  // 标记「本轮发送是计划轮」：发送时按 planMode 写入，chat-done 据此决定是否进入待批准态。
  // 用 ref 而非 state——chat-done 回调注册一次，闭包内需读到本轮最新值，规避陈旧。
  const planRoundRef = useRef(false);
  // 设置面板 / 变更记录面板
  const [showSettings, setShowSettings] = useState(false);
  // 打开设置面板时初始定位的分类（首屏 CTA 可直接跳到「模型供应商」）。
  const [settingsSection, setSettingsSection] = useState<SettingsSection>("account");
  // 主模型是否已配（Plan19 P0a）：未配则首屏给「去配置」引导。
  const [mainConfigured, setMainConfigured] = useState<boolean | null>(null);
  const [showChanges, setShowChanges] = useState(false);
  const [checkpoints, setCheckpoints] = useState<FileCheckpoint[]>([]);
  const [appInfo, setAppInfo] = useState<AppInfo | null>(null);
  const [mcpServers, setMcpServers] = useState<McpServer[]>([]);
  const [balance, setBalance] = useState<BalanceState>({ status: "idle" });
  const [permRules, setPermRules] = useState<string[]>([]);
  const [commandSandbox, setCommandSandbox] = useState(true);
  const [taskBudget, setTaskBudget] = useState(0);
  // 自定义斜杠命令（工作区 .mdga/commands/*.md）
  const [customCommands, setCustomCommands] = useState<Array<{ name: string; description: string; body: string }>>([]);
  // Steering：运行中已排队但尚未被注入的插话消息
  const [queuedSteering, setQueuedSteering] = useState<string[]>([]);
  const [theme, setTheme] = useTheme();
  // @文件引用补全
  const [workspaceFiles, setWorkspaceFiles] = useState<string[]>([]);
  const [fileMention, setFileMention] = useState<string | null>(null);
  // 斜杠菜单 / @文件菜单的键盘高亮项（Plan27 #7）：方向键移动、Enter 选中、Esc 关。
  const [slashActive, setSlashActive] = useState(0);
  const [mentionActive, setMentionActive] = useState(0);
  // 工作区胶囊菜单容器（Plan27 #7）：打开时聚焦首项 + 方向键漫游 + Esc 关。
  const workspaceMenuRef = useRef<HTMLDivElement>(null);
  // 待发送的附图（Plan18 M18.1）：📎 选图后暂存，发送时随 send_message 上送，发送框上方显示缩略图。
  const [pendingImages, setPendingImages] = useState<ImagePart[]>([]);
  // 拖拽图片悬停高亮（Plan19 P0b）：dragenter/over 置 true，drop/leave 置 false。
  const [dragOver, setDragOver] = useState(false);
  // 上下文用量（上次请求 prompt_tokens / 压缩阈值）。移入底栏指示器（Plan26）。
  const [ctxUsage, setCtxUsage] = useState<{ promptTokens: number; softLimit: number } | null>(null);
  // 上下文指示器弹层开关（Plan26）。
  const [ctxPopoverOpen, setCtxPopoverOpen] = useState(false);
  const { update, setUpdate, handleInstallUpdate } = useUpdate();
  // 全局 toast（Plan20 🔴2）：不依赖消息列表的右下角通知，承载用户主动操作的即时成败。
  const { toasts, pushToast, dismissToast } = useToasts();
  // 粘底滚动（Plan20 🟠4）：贴底时才自动跟随；非贴底显示「跳到最新」并暂停跟随。
  const [isAtBottom, setIsAtBottom] = useState(true);
  // 长会话分段渲染（Plan27 #8）：仅渲染最近 visibleCount 条，顶部「加载更早」逐段放开，
  // 避免数百条消息一次性渲染卡顿。切换会话时复位。
  const MSG_WINDOW = 60; // 初始/每次加载的窗口步长
  const [visibleCount, setVisibleCount] = useState(MSG_WINDOW);
  const messageListRef = useRef<HTMLElement>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const streamingTextRef = useRef(""); // 只累积纯文字内容，用于 chat-done 持久化（供模型上下文）
  const streamingPartsRef = useRef<MessagePart[]>([]); // 跟踪当前 assistant 的完整 parts，用于持久化交错的工具卡片
  const streamingUsageRef = useRef<UsageSummary | null>(null);
  // 命令面板（Plan27 #3a）：Ctrl/Cmd+K 打开的居中浮层。
  const [showCommandPalette, setShowCommandPalette] = useState(false);
  // /help 能力披露面板（Plan27 #3b）：纯静态。
  const [showHelp, setShowHelp] = useState(false);
  // 正文搜索结果（Plan27 #6）：query 非空时改调 search_conversations，结果存此；null=用本地全量列表。
  const [searchResults, setSearchResults] = useState<Conversation[] | null>(null);

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

  // 挂载时查主模型是否已配（Plan19 P0a）：未配则首屏给「去 设置 → 模型供应商」CTA。
  // 同时取主 provider 的 model_id（Plan20 🔴1）作控制行只读胶囊展示与透传 send_message 的值。
  useEffect(() => {
    void refreshMainModel();
  }, []);

  /** 拉取主 provider 配置（Plan20 🔴1）：刷新 mainConfigured 与 mainModelId，并把 model 透传值同步为 model_id。 */
  async function refreshMainModel() {
    const cfg = await invoke<ProviderConfig | null>("get_model_provider_config", { role: "main" })
      .catch(() => null);
    setMainConfigured(!!cfg);
    const id = cfg?.modelId?.trim() ?? "";
    setMainModelId(id);
    if (id) setModel(id);
    // 缓存主 provider 预设（Plan21 #5）：未配/缺省回落 deepseek，供余额门禁与成本金额位判断。
    setMainPreset((cfg?.preset ?? "deepseek") || "deepseek");
  }

  useEffect(() => {
    loadConversations();
  }, []);

  // 输入变化时把斜杠/＠菜单高亮复位到首项（Plan27 #7）。
  useEffect(() => { setSlashActive(0); }, [input]);
  useEffect(() => { setMentionActive(0); }, [fileMention]);

  // 工作区菜单打开时聚焦首个菜单项（Plan27 #7）。
  useEffect(() => {
    if (workspaceMenuOpen) {
      workspaceMenuRef.current?.querySelector<HTMLButtonElement>("button")?.focus();
    }
  }, [workspaceMenuOpen]);

  // 正文搜索（Plan27 #6）：query 非空时防抖（~250ms）调 search_conversations（标题或正文命中）；
  // 空则清结果回退本地全量列表。
  useEffect(() => {
    const q = searchQuery.trim();
    if (!q) { setSearchResults(null); return; }
    const timer = setTimeout(() => {
      invoke<Conversation[]>("search_conversations", { query: q })
        .then((list) => setSearchResults(Array.isArray(list) ? list : []))
        .catch(() => setSearchResults([]));
    }, 250);
    return () => clearTimeout(timer);
  }, [searchQuery]);

  useEffect(() => {
    setIsAtBottom(true); // 切换会话回到贴底跟随（Plan20 🟠4）
    setVisibleCount(MSG_WINDOW); // 切换会话复位分段渲染窗口（Plan27 #8）
    if (!activeConvId) { setMessages([]); return; }
    invoke<StoredMessage[]>("load_messages", { conversationId: activeConvId })
      .then((stored) => setMessages(stored.map(storedToMessage)))
      .catch(() => setMessages([]));
  }, [activeConvId]);

  // 粘底滚动（Plan20 🟠4）：仅当用户当前贴底时才跟随到最新；上滚查看历史时不强拽。
  useEffect(() => {
    if (isAtBottom) {
      messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
    }
  }, [messages, isAtBottom]);

  // 监听消息列表滚动：距底 < 80px 视为贴底，控制是否跟随与「跳到最新」按钮显隐。
  function handleMessageListScroll(e: React.UIEvent<HTMLElement>) {
    const el = e.currentTarget;
    const dist = el.scrollHeight - el.scrollTop - el.clientHeight;
    setIsAtBottom(dist < 80);
  }

  /** 点击「跳到最新」（Plan20 🟠4）：回到底部并恢复跟随。 */
  function jumpToLatest() {
    setIsAtBottom(true);
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }

  // 持续监听高风险动作审批请求，弹出确认框
  useEffect(() => {
    const unlisten = listen<ApprovalRequest>("approval-request", (e) => {
      setApproval(e.payload);
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // 监听 Agent 发起的 ask_user 结构化提问，弹出选择卡片
  useEffect(() => {
    const unlisten = listen<AskUserRequest>("ask-user-request", (e) => {
      setAskUser(e.payload);
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // 发送期间的耗时计时器：每秒刷新，让用户确信 agent 仍在工作而不是被截断
  useEffect(() => {
    if (!sending) {
      setElapsedSec(0);
      setAgentStatus(null);
      return;
    }
    const start = Date.now();
    const timer = setInterval(
      () => setElapsedSec(Math.floor((Date.now() - start) / 1000)),
      1000
    );
    return () => clearInterval(timer);
  }, [sending]);

  // 启动时恢复默认权限模式（localStorage 持久化）。
  // Plan20 🔴1：模型不再从 localStorage 快切恢复，唯一真相源为主 provider 的 model_id。
  useEffect(() => {
    const savedMode = localStorage.getItem("mdga.defaultPermissionMode");
    if (savedMode && PERMISSION_MODES.includes(savedMode as PermissionMode)) {
      setPermissionMode(savedMode as PermissionMode);
    }
  }, []);

  // todo 清单实时更新（todo_write 工具）
  useEffect(() => {
    const unlisten = listen<TodoItem[]>("todo-update", (e) => {
      setTodos(Array.isArray(e.payload) ? e.payload : []);
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // 视觉分析事件（Plan19 C-B）：自动初看完成后即时把「视觉分析」卡片插到当前回复流首位。
  // 与持久化 vision part 二选一：实时事件用于发送中即时显示；重载后用持久化 part（storedToMessage）。
  useEffect(() => {
    const unlisten = listen<{ conversationId: string; count: number; analysis: string; usage?: RawUsageWire | null }>(
      "vision-analysis",
      (e) => {
        const { count, analysis, usage } = e.payload;
        setMessages((prev) => {
          const updated = [...prev];
          const lastIdx = updated.length - 1;
          const last = updated[lastIdx];
          if (!last || last.role !== "assistant") return prev;
          // 防重：本轮已插入过 vision 卡片则跳过。
          if (last.parts.some((p) => p.type === "vision")) return prev;
          // 视觉卡片排在助手回复最前（与后端持久化「parts 首个 part」一致）。
          const parts: MessagePart[] = [{ type: "vision", count, analysis, usage }, ...last.parts];
          updated[lastIdx] = { ...last, parts };
          streamingPartsRef.current = parts;
          return updated;
        });
      },
    );
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // Steering：一条排队的插话被注入后，从待注入列表里移除一条
  useEffect(() => {
    const unlisten = listen<string>("steering-injected", () => {
      setQueuedSteering((prev) => prev.slice(1));
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // 后台命令完成通知：插入通知卡片
  useEffect(() => {
    const unlisten = listen<{ command: string; exitCode?: number | null; error?: string }>(
      "background-command-done",
      (e) => {
        const { command, exitCode, error } = e.payload;
        const text = error
          ? `后台命令失败：${command} — ${error}`
          : `后台命令完成：${command}（退出码 ${exitCode ?? "?"}）`;
        appendNoticeToLastMessage(text);
      }
    );
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // 命令实时输出：附加到最近一个运行中的 run_command 卡片
  useEffect(() => {
    const unlisten = listen<string>("command-output", (e) => {
      setMessages((prev) => {
        const updated = [...prev];
        const lastIdx = updated.length - 1;
        const last = updated[lastIdx];
        if (!last || last.role !== "assistant") return prev;
        const parts = [...last.parts];
        for (let i = parts.length - 1; i >= 0; i--) {
          const p = parts[i];
          if (p.type === "tool" && p.toolName === "run_command" && p.status === "running") {
            const existing = p.liveOutput ?? "";
            // 只保留尾部 4000 字符，防止超长输出拖垮渲染
            const next = (existing + e.payload + "\n").slice(-4000);
            parts[i] = { ...p, liveOutput: next };
            updated[lastIdx] = { ...last, parts };
            streamingPartsRef.current = parts;
            return updated;
          }
        }
        return prev;
      });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // 会话切换时加载工作区文件列表（@引用补全），并清空 todo
  useEffect(() => {
    setTodos([]);
    setWorkspaceFiles([]);
    setCustomCommands([]);
    if (!activeConvId) return;
    invoke<string[]>("list_workspace_files", { conversationId: activeConvId })
      .then(setWorkspaceFiles)
      .catch(() => setWorkspaceFiles([]));
    invoke<Array<{ name: string; description: string; body: string }>>("list_custom_commands", { conversationId: activeConvId })
      .then(setCustomCommands)
      .catch(() => setCustomCommands([]));
  }, [activeConvId]);

  /** 向当前最后一条 assistant 消息追加通知卡片 */
  function appendNoticeToLastMessage(text: string) {
    setMessages((prev) => {
      const updated = [...prev];
      const lastIdx = updated.length - 1;
      const last = updated[lastIdx];
      if (!last || last.role !== "assistant") return prev;
      const parts: MessagePart[] = [...last.parts, { type: "notice", text }];
      updated[lastIdx] = { ...last, parts };
      streamingPartsRef.current = parts;
      return updated;
    });
  }

  // ── 工具函数 ────────────────────────────────────────────────────────────

  function storedToMessage(s: StoredMessage): Message {
    const usage = s.usageJson ? JSON.parse(s.usageJson) as UsageSummary : undefined;
    // 优先用持久化的结构化 parts 还原文字+工具卡片交错；缺失时回退为单个 text part。
    let parts: MessagePart[] = [{ type: "text", content: s.content }];
    if (s.partsJson) {
      try {
        const parsed = JSON.parse(s.partsJson) as MessagePart[];
        if (Array.isArray(parsed) && parsed.length > 0) parts = parsed;
      } catch {
        // 解析失败保留纯文字回退
      }
    }
    return { role: s.role as "user" | "assistant", parts, usage };
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
    resetPerConversationState();
  }

  async function handleSelectConversation(id: string) {
    if (id === activeConvId || sending) return;
    setActiveConvId(id);
    setDraftWorkspace(null);
    setWorkspaceError(null);
    resetPerConversationState();
  }

  /** 跨会话状态重置（Plan20 🟠6）：新建/切换会话时清掉上一会话残留的附图、计划模式与排队插话。
      （todos/workspaceFiles 已在 activeConvId effect 中清，故此处不重复。） */
  function resetPerConversationState() {
    setPendingImages([]);
    setPlanMode(false);
    setQueuedSteering([]);
    setWorkspaceMenuOpen(false);
    setCtxPopoverOpen(false);
    // C-4：跨会话清除待批准态与本轮计划标记，避免上一会话的计划闭环残留到新会话。
    setAwaitingPlanApproval(false);
    planRoundRef.current = false;
  }

  async function handleDeleteConversation(e: React.MouseEvent, id: string) {
    e.stopPropagation();
    // 删除二次确认（Plan20 🔴3）：与「清除所有会话」一致策略，文案带会话标题。
    const conv = conversations.find((c) => c.id === id);
    const title = conv?.title ?? "该会话";
    if (!window.confirm(`确定删除会话「${title}」？此操作不可撤销。`)) return;
    await invoke("remove_conversation", { conversationId: id }).catch(() => {});
    setConversations((prev) => prev.filter((c) => c.id !== id));
    if (activeConvId === id) {
      setActiveConvId(null);
      setMessages([]);
    }
  }

  async function handleTogglePin(e: React.MouseEvent, conv: Conversation) {
    e.stopPropagation();
    await invoke("pin_conversation", {
      conversationId: conv.id,
      pinned: !conv.pinned,
    }).catch(() => {});
    await loadConversations();
  }

  async function handleToggleArchive(e: React.MouseEvent, conv: Conversation) {
    e.stopPropagation();
    await invoke("archive_conversation", {
      conversationId: conv.id,
      archived: !conv.archived,
    }).catch(() => {});
    await loadConversations();
  }

  function startRename(e: React.MouseEvent, conv: Conversation) {
    e.stopPropagation();
    setEditingConvId(conv.id);
    setEditingTitle(conv.title);
  }

  async function commitRename() {
    const id = editingConvId;
    const title = editingTitle.trim();
    setEditingConvId(null);
    if (!id || !title) return;
    await invoke("rename_conversation", { conversationId: id, title }).catch(() => {});
    setConversations((prev) =>
      prev.map((c) => (c.id === id ? { ...c, title } : c))
    );
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

  /**
   * 从 composer 工作区胶囊菜单触发「选择/更换工作区…」（B2）。
   * - 无 activeConvId（新会话草稿）：选目录后写入 draftWorkspace，首发时随 new_conversation_with_workspace 绑定。
   * - 有 activeConvId（已存会话）：调后端 set_conversation_workspace 改绑，用返回的 Conversation 刷新该条。
   */
  async function handlePickWorkspaceFromComposer() {
    setWorkspaceMenuOpen(false);
    if (!activeConvId) {
      await handleSelectWorkspace();
      return;
    }
    try {
      const selected = await open({ directory: true, multiple: false });
      if (!selected || Array.isArray(selected)) return;
      const updated = await invoke<Conversation>("set_conversation_workspace", {
        conversationId: activeConvId,
        path: selected,
      });
      setConversations((prev) => prev.map((c) => (c.id === updated.id ? updated : c)));
    } catch (err) {
      pushToast("error", humanizeError(String(err)));
    }
  }

  /**
   * 从 composer 工作区胶囊菜单触发「纯聊天（不绑定）」（B2）。
   * - 无 activeConvId：清空 draftWorkspace。
   * - 有 activeConvId：调后端 set_conversation_workspace（path=null）解绑。
   */
  async function handleClearWorkspaceFromComposer() {
    setWorkspaceMenuOpen(false);
    if (!activeConvId) {
      setDraftWorkspace(null);
      setWorkspaceError(null);
      return;
    }
    try {
      const updated = await invoke<Conversation>("set_conversation_workspace", {
        conversationId: activeConvId,
        path: null,
      });
      setConversations((prev) => prev.map((c) => (c.id === updated.id ? updated : c)));
    } catch (err) {
      pushToast("error", humanizeError(String(err)));
    }
  }

  /** 工作区胶囊当前展示名（B2）：已存会话取 activeConversation，新会话草稿取 draftWorkspace。 */
  function composerWorkspaceName(): string | null {
    if (activeConvId) return activeConversation?.workspaceName ?? null;
    return draftWorkspace?.name ?? null;
  }

  // ── 发送消息 ────────────────────────────────────────────────────────────

  /** 处理斜杠命令；返回 true 表示已消费输入，不再走正常发送。 */
  async function handleSlashCommand(text: string): Promise<boolean> {
    if (!text.startsWith("/")) return false;
    const [cmd, ...rest] = text.split(/\s+/);
    switch (cmd) {
      case "/clear":
        setInput("");
        await handleNewConversation();
        return true;
      case "/rewind":
        setInput("");
        await openChangesPanel();
        return true;
      case "/help":
        // Plan27 #3b：打开「能做什么」披露面板，纯静态。
        setInput("");
        setShowHelp(true);
        return true;
      case "/model": {
        // Plan20 🔴1：模型唯一真相源 = 主 provider 的 model_id。/model 不再切 DeepSeek 清单，
        // 直接打开 设置 → 模型供应商，由用户在那里改 model_id。
        setInput("");
        await openSettings("provider");
        return true;
      }
      case "/compact": {
        setInput("");
        if (!activeConvId) return true;
        setAgentStatus("正在压缩会话…");
        setSending(true);
        try {
          await invoke("compact_history", { conversationId: activeConvId, model });
          const stored = await invoke<StoredMessage[]>("load_messages", {
            conversationId: activeConvId,
          });
          setMessages(stored.map(storedToMessage));
        } catch (err) {
          // 用户主动 /compact 的即时失败（Plan20 🔴2）→ 全局 toast，不依赖消息列表。
          pushToast("error", humanizeError(String(err)));
        } finally {
          setSending(false);
        }
        return true;
      }
      default: {
        // 自定义斜杠命令（.mdga/commands/<name>.md）：用命令体替换发送，$ARGUMENTS 替换为参数
        const custom = customCommands.find((c) => c.name === cmd);
        if (custom) {
          const args = rest.join(" ");
          const filled = custom.body.replace(/\$ARGUMENTS/g, args);
          setInput("");
          await sendText(filled);
          return true;
        }
        return false;
      }
    }
  }

  async function handleSend() {
    let text = input.trim();
    if (!text || sending) return;
    // /init 替换为固定提示词走正常发送；其余斜杠命令直接消费
    if (text === "/init") {
      text = INIT_PROMPT;
    } else if (await handleSlashCommand(text)) {
      return;
    }
    await sendText(text);
  }

  /**
   * 发送一段文本（供输入框与文件导入复用）。
   * @param executePlan C-4：true 时透传 send_message 的 executePlan，让后端注入「严格按上一条计划执行」语义；
   *                    普通发送省略或传 false。仅「批准并执行计划」按钮会传 true。
   */
  async function sendText(text: string, executePlan: boolean = false) {
    if (!text || sending) return;

    // C-4：记录本轮是否为计划轮（仅 planMode 为真且非执行计划时算计划轮，供 chat-done 决定是否进入待批准）；
    // 任何一次新发送都先清掉上一轮的待批准态（点「批准并执行」的发送也会先清，再由本轮结果重置）。
    planRoundRef.current = planMode && !executePlan;
    setAwaitingPlanApproval(false);

    // 快照本轮附图并清空暂存（Plan18 M18.1）：随消息上送视觉模型，并入用户消息 parts 持久化展示。
    const outImages = pendingImages;
    setPendingImages([]);

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

    // 用户消息 parts：文字 + 附图缩略图。content 仅存文字（供模型上下文），partsJson 含图片引用（供刷新后还原展示）。
    const userParts: MessagePart[] = [
      { type: "text", content: text },
      ...outImages,
    ];
    await invoke("persist_message", {
      conversationId: convId,
      role: "user",
      content: text,
      usageJson: null,
      partsJson: outImages.length > 0 ? JSON.stringify(userParts) : null,
    }).catch(() => {});

    const currentConv = conversations.find((c) => c.id === convId);
    if (!currentConv || currentConv.title === "新对话") {
      const title = text.slice(0, 20);
      await invoke("rename_conversation", { conversationId: convId, title }).catch(() => {});
      setConversations((prev) =>
        prev.map((c) => (c.id === convId ? { ...c, title } : c))
      );
    }

    // 构建发给后端的消息：用户消息 parts 含文字 + 附图缩略图（图片仅前端展示，后端按 images 参数单独识图）。
    const outgoing: Message[] = [...messages, { role: "user", parts: userParts }];
    setInput("");
    await streamAgent(convId, outgoing, outImages, executePlan);
  }

  /**
   * 与后端跑一轮 Agent 流式对话（Plan27 #1b 抽取自 sendText 复用）。
   * 负责：插入空助手占位 → 注册全部流式监听 → invoke("send_message") → chat-done 收尾持久化。
   * @param convId 目标会话；@param outgoing 发给后端的完整历史（含本轮 user，但 rerun 时末条为 user 而非新增）；
   * @param outImages 本轮附图（rerun 传空）；@param executePlan C-4 透传。
   */
  async function streamAgent(
    convId: string,
    outgoing: Message[],
    outImages: ImagePart[],
    executePlan: boolean,
  ) {
    setMessages([...outgoing, { role: "assistant", parts: [] }]);
    setSending(true);
    streamingTextRef.current = "";
    streamingPartsRef.current = [];
    streamingUsageRef.current = null;

    // ── 流式事件监听 ────────────────────────────────────────────────────

    // chat-chunk：追加文字到当前 assistant 消息的最后一个 text part
    const unlistenChunk = await listen<string>("chat-chunk", (e) => {
      setAgentStatus("正在输出回复…");
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
        streamingPartsRef.current = parts;
        return updated;
      });
    });

    // chat-reasoning（Plan27 #1a）：累积模型 reasoning_content 增量到当前助手消息的 reasoning part。
    // reasoning 卡片排在该助手消息最前（在 vision 卡片之后、正文之前）：若已存在则原地累积，否则新插入。
    const unlistenReasoning = await listen<string>("chat-reasoning", (e) => {
      setAgentStatus("正在思考…");
      setMessages((prev) => {
        const updated = [...prev];
        const lastIdx = updated.length - 1;
        const last = updated[lastIdx];
        if (!last || last.role !== "assistant") return prev;
        const parts = [...last.parts];
        const existingIdx = parts.findIndex((p) => p.type === "reasoning");
        if (existingIdx >= 0) {
          const cur = parts[existingIdx] as ReasoningPart;
          parts[existingIdx] = { type: "reasoning", content: cur.content + e.payload };
        } else {
          // 插入位置：所有前置 vision 卡片之后（vision 始终在最前），其余内容之前。
          let insertAt = 0;
          while (insertAt < parts.length && parts[insertAt].type === "vision") insertAt++;
          parts.splice(insertAt, 0, { type: "reasoning", content: e.payload });
        }
        updated[lastIdx] = { ...last, parts };
        streamingPartsRef.current = parts;
        return updated;
      });
    });

    // tool-event：running 时插入新卡片，succeeded/failed 时更新最近匹配的 running 卡片
    const unlistenTool = await listen<ToolEvent>("tool-event", (e) => {
      const { toolName, status, inputJson, outputJson, errorMessage } = e.payload;
      if (status === "running") setAgentStatus(`正在执行 ${toolName}…`);
      const target = extractTarget(inputJson);
      // 解析输出中的 diff 信息（文件写类工具）
      let diff: string | undefined;
      let added: number | undefined;
      let removed: number | undefined;
      if (outputJson) {
        try {
          const out = JSON.parse(outputJson) as Record<string, unknown>;
          if (typeof out.diff === "string") diff = out.diff;
          if (typeof out.added === "number") added = out.added;
          if (typeof out.removed === "number") removed = out.removed;
        } catch {
          // 输出非 JSON 时忽略
        }
      }
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
          // 从后往前找同名的最近 running 卡片并更新状态（附带 diff 与最终输出）
          for (let i = parts.length - 1; i >= 0; i--) {
            const p = parts[i];
            if (p.type === "tool" && p.toolName === toolName && p.status === "running") {
              parts[i] = {
                ...p,
                status: status as "succeeded" | "failed",
                error: errorMessage ?? undefined,
                diff,
                added,
                removed,
              };
              break;
            }
          }
        }
        updated[lastIdx] = { ...last, parts };
        streamingPartsRef.current = parts;
        return updated;
      });
    });

    // agent-status：后端推送的实时状态（思考中第 N 轮 / 压缩上下文中）
    const unlistenStatus = await listen<{ state: string; round?: number }>(
      "agent-status",
      (e) => {
        const { state, round } = e.payload;
        if (state === "thinking") {
          setAgentStatus(round ? `正在思考…（第 ${round} 轮）` : "正在思考…");
        } else if (state === "compacting") {
          setAgentStatus("正在压缩上下文…");
        }
      }
    );

    // context-usage：每轮请求后的真实上下文体积，驱动状态栏百分比
    const unlistenCtx = await listen<{ promptTokens: number; softLimit: number }>(
      "context-usage",
      (e) => setCtxUsage(e.payload)
    );

    // context-compacted：压缩事件，在对话流中插入通知卡片（对标 CC/Codex 的 compact 提示）
    const unlistenCompact = await listen<{ kind: string; count?: number }>(
      "context-compacted",
      (e) => {
        const text =
          e.payload.kind === "summary"
            ? "上下文较长，已自动压缩为任务进度摘要，对话继续"
            : `上下文较长，已压缩 ${e.payload.count ?? 0} 条较早的工具输出`;
        setMessages((prev) => {
          const updated = [...prev];
          const lastIdx = updated.length - 1;
          const last = updated[lastIdx];
          if (last.role !== "assistant") return prev;
          const parts: MessagePart[] = [...last.parts, { type: "notice", text }];
          updated[lastIdx] = { ...last, parts };
          streamingPartsRef.current = parts;
          return updated;
        });
      }
    );

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
      setAskUser(null);
      setQueuedSteering([]);
      unlistenChunk();
      unlistenReasoning();
      unlistenTool();
      unlistenStatus();
      unlistenCtx();
      unlistenCompact();
      unlistenUsage();
      unlistenDone();

      // 兜底清洗：模型可能把工具调用标记直接吐进正文（纯聊天会话尤其常见——没有工具
      // 时模型会幻觉 <ToolCall>/DSML 语法）。
      // Plan20 🟠7：只截断 streamingPartsRef 中【最后一个 text part】的泄漏内容并在其后追加 notice，
      // 保留前面的 tool/vision part，不再用 [text, notice] 整体覆盖（否则含工具调用的回复卡片会丢）。
      const leakIdx = findToolMarkupIndex(streamingTextRef.current);
      if (leakIdx >= 0) {
        const conv = conversations.find((c) => c.id === finalConvId);
        const noticeText = conv?.workspacePath
          ? "已清理模型输出中的内部工具标记"
          : "本会话未绑定工作区，Agent 无法执行本地文件操作。请点击「+ 新对话」并选择工作区后再试。";
        // content（供模型上下文）仍按全文截断到泄漏处。
        streamingTextRef.current = streamingTextRef.current.slice(0, leakIdx).trimEnd();
        // parts：定位最后一个 text part，对其内容单独做泄漏截断，再插入 notice。
        const parts = [...streamingPartsRef.current];
        let lastTextIdx = -1;
        for (let i = parts.length - 1; i >= 0; i--) {
          if (parts[i].type === "text") { lastTextIdx = i; break; }
        }
        if (lastTextIdx >= 0) {
          const text = (parts[lastTextIdx] as TextPart).content;
          const localLeak = findToolMarkupIndex(text);
          const cleaned = (localLeak >= 0 ? text.slice(0, localLeak) : text).trimEnd();
          if (cleaned) {
            parts[lastTextIdx] = { type: "text", content: cleaned };
          } else {
            parts.splice(lastTextIdx, 1); // 整段都是泄漏标记则移除空 text part
          }
        }
        parts.push({ type: "notice", text: noticeText });
        streamingPartsRef.current = parts;
        setMessages((prev) => {
          const updated = [...prev];
          const lastIdx = updated.length - 1;
          if (updated[lastIdx]?.role === "assistant") {
            updated[lastIdx] = { ...updated[lastIdx], parts };
          }
          return updated;
        });
      }

      const usageJson = streamingUsageRef.current
        ? JSON.stringify(streamingUsageRef.current)
        : null;
      // content 存纯文字（供模型上下文）；partsJson 存文字+工具卡片交错结构（供重启后还原展示）
      const finalParts = streamingPartsRef.current;
      const partsJson = finalParts.length > 0 ? JSON.stringify(finalParts) : null;
      await invoke("persist_message", {
        conversationId: finalConvId,
        role: "assistant",
        content: streamingTextRef.current,
        usageJson,
        partsJson,
      }).catch(() => {});

      // C-4 计划闭环：本轮为计划轮且成功产出了助手回复（有非空正文）→ 进入待批准态，
      // 在回复下方显示「批准并执行计划」按钮。下一次发送（含批准执行）会先清掉它。
      if (planRoundRef.current && streamingTextRef.current.trim().length > 0) {
        setAwaitingPlanApproval(true);
      }
      planRoundRef.current = false;

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
        planMode,
        // C-4：批准并执行计划时透传 true，后端注入「严格按上一条计划执行 + 先建 todo 清单」语义；普通发送为 false。
        executePlan,
        // 本轮附图（Plan18 M18.1）：后端据此走「自动初看」识图并注入主 agent。
        images: outImages.map((img) => ({ mediaType: img.mediaType, base64: img.base64 })),
      });
    } catch (err) {
      setMessages((prev) => {
        const updated = [...prev];
        updated[updated.length - 1] = {
          role: "assistant",
          parts: [{ type: "text", content: humanizeError(String(err)) }],
        };
        return updated;
      });
      // 发送失败恢复附图（Plan20 🟠5）：把本轮已快照清空的附图还原回托盘，避免用户重选。
      if (outImages.length > 0) setPendingImages(outImages);
      setSending(false);
      unlistenChunk();
      unlistenReasoning();
      unlistenTool();
      unlistenStatus();
      unlistenCtx();
      unlistenCompact();
      unlistenUsage();
      unlistenDone();
    }
  }

  /**
   * 重新生成（Plan27 #1b）：删该会话最后一条助手消息 → 从 messages 去掉它 →
   * 以截至上一条 user 的历史重跑（不新增 user 消息，复用 streamAgent 流式流程）。
   * 仅对「最后一条助手消息」启用；sending 中禁用。
   */
  async function handleRegenerate() {
    if (sending || !activeConvId) return;
    // 找到最后一条助手消息的下标。
    let lastAssistantIdx = -1;
    for (let i = messages.length - 1; i >= 0; i--) {
      if (messages[i].role === "assistant") { lastAssistantIdx = i; break; }
    }
    if (lastAssistantIdx < 0) return;
    // 截至上一条（即助手消息之前的全部历史，末条应为 user）。
    const history = messages.slice(0, lastAssistantIdx);
    if (history.length === 0 || history[history.length - 1].role !== "user") return;
    // 后端删最后一条助手消息（持久化层）；失败则中止，避免重复回复。
    const ok = await invoke<boolean>("delete_last_assistant_message", { conversationId: activeConvId })
      .catch(() => false);
    if (!ok) {
      pushToast("error", "无法删除上一条回复，重新生成已取消。");
      return;
    }
    // C-4：重新生成不是计划轮；清待批准态。
    planRoundRef.current = false;
    setAwaitingPlanApproval(false);
    await streamAgent(activeConvId, history, [], false);
  }

  /** 把一条消息的全部文字提取为纯文本（用于复制 / 重发 / 编辑回填）。 */
  function messageText(msg: Message): string {
    return msg.parts
      .filter((p): p is TextPart => p.type === "text")
      .map((p) => p.content)
      .join("")
      .trim();
  }

  /** 编辑重试（Plan19 P1a）：把该用户消息文字回填到输入框，供修改后再次发送。 */
  function editRetryMessage(msg: Message) {
    if (sending) return;
    const text = messageText(msg);
    if (!text) return;
    setInput(text);
    updateFileMention(text);
  }

  /** 重发（Plan19 P1a）：直接以该用户消息的文字再发一次（复用 sendText / handleSend 路径）。 */
  async function resendMessage(msg: Message) {
    if (sending) return;
    const text = messageText(msg);
    if (!text) return;
    await sendText(text);
  }

  /**
   * C-4「批准并执行计划」：以 planMode=false + executePlan=true 走发送路径，
   * 让后端据上一条计划注入「严格按计划执行 + 先建 todo 清单」语义。
   * 关掉计划模式（避免又出一份计划），发送固定文本「按上述计划执行」，随后清待批准态（sendText 内已清）。
   */
  async function approveAndExecutePlan() {
    if (sending || !awaitingPlanApproval) return;
    setPlanMode(false);
    await sendText("按上述计划执行", true);
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

  async function respondApproval(approved: boolean, remember = false) {
    if (!approval) return;
    const actionId = approval.actionId;
    setApproval(null);
    await invoke("respond_approval", { actionId, approved, remember }).catch(() => {});
  }

  async function respondAskUser(answer: string) {
    if (!askUser) return;
    const questionId = askUser.questionId;
    setAskUser(null);
    await invoke("respond_ask_user", { questionId, answer }).catch(() => {});
  }

  // F3：Esc 关闭/取消当前弹窗。用 ref 镜像最新状态与处理函数，
  // 监听器只注册一次也能读到最新值，规避闭包陈旧问题。
  const escState = useRef({
    approval, askUser, showChanges, showSettings, showHelp, showCommandPalette,
    respondApproval, respondAskUser, setShowChanges, setShowSettings, setShowHelp, setShowCommandPalette,
  });
  escState.current = {
    approval, askUser, showChanges, showSettings, showHelp, showCommandPalette,
    respondApproval, respondAskUser, setShowChanges, setShowSettings, setShowHelp, setShowCommandPalette,
  };
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key !== "Escape") return;
      const s = escState.current;
      // 优先级：审批 → 提问 → 命令面板 → 帮助 → 变更 → 设置（审批/提问 Esc=拒绝/取消）。
      if (s.approval) { s.respondApproval(false); return; }
      if (s.askUser) { s.respondAskUser(""); return; }
      if (s.showCommandPalette) { s.setShowCommandPalette(false); return; }
      if (s.showHelp) { s.setShowHelp(false); return; }
      if (s.showChanges) { s.setShowChanges(false); return; }
      if (s.showSettings) { s.setShowSettings(false); return; }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  // 全局快捷键（Plan27 #3a）：Ctrl/Cmd+N 新对话、Ctrl/Cmd+K 命令面板、Ctrl/Cmd+, 设置。已抽至 useKeyboardShortcuts。
  useKeyboardShortcuts({
    onNewConversation: handleNewConversation,
    onOpenPalette: () => setShowCommandPalette(true),
    onOpenSettings: () => openSettings(),
  });

  async function openChangesPanel() {
    if (!activeConvId) return;
    const list = await invoke<FileCheckpoint[]>("get_checkpoints", {
      conversationId: activeConvId,
    }).catch(() => [] as FileCheckpoint[]);
    setCheckpoints(list);
    setShowChanges(true);
  }

  async function handleRevert(checkpointId: string) {
    if (!activeConvId) return;
    try {
      const count = await invoke<number>("revert_to_checkpoint", {
        conversationId: activeConvId,
        checkpointId,
      });
      appendNoticeToLastMessage(`已回退 ${count} 处文件变更`); // 过程性通知保留内联（Plan20 🔴2）
      // Plan21 #3：回退成功后,把对话流里现存的 tool 类 diff 卡片标记 reverted（简化:全部 diff 卡片打标），
      // 渲染时置灰并加「已回退」角标,与「已回退 N 处」通知并存。
      setMessages((prev) =>
        prev.map((m) => ({
          ...m,
          parts: m.parts.map((p) =>
            p.type === "tool" && typeof p.diff === "string" && p.diff.trim().length > 0
              ? { ...p, reverted: true }
              : p
          ),
        }))
      );
      await openChangesPanel(); // 刷新列表状态
    } catch (err) {
      pushToast("error", humanizeError(String(err))); // 回退失败属即时操作失败 → toast
    }
  }

  async function refreshMcpServers() {
    const list = await invoke<McpServer[]>("get_mcp_servers").catch(() => [] as McpServer[]);
    setMcpServers(list);
  }

  async function refreshBalance() {
    setBalance({ status: "loading" });
    try {
      const data = await invoke<UserBalance>("get_account_balance");
      setBalance({ status: "ok", data });
    } catch (err) {
      setBalance({ status: "error", message: humanizeError(String(err)) });
    }
  }

  async function refreshPermRules() {
    const list = await invoke<string[]>("get_permission_rules").catch(() => [] as string[]);
    setPermRules(list);
  }

  async function handleAddPermRule(rule: string) {
    await invoke("create_permission_rule", { rule }).catch(() => {});
    await refreshPermRules();
  }

  async function handleDeletePermRule(rule: string) {
    await invoke("delete_permission_rule", { rule }).catch(() => {});
    await refreshPermRules();
  }

  async function handleToggleSandbox(enabled: boolean) {
    setCommandSandbox(enabled);
    await invoke("set_command_sandbox", { enabled }).catch(() => {});
  }

  async function handleSetBudget(budget: number) {
    setTaskBudget(budget);
    await invoke("set_task_budget", { budget }).catch(() => {});
  }

  async function handleExportConversation() {
    if (!activeConvId) return;
    const path = await save({ defaultPath: "conversation.md", filters: [{ name: "Markdown", extensions: ["md"] }] }).catch(() => null);
    if (!path) return;
    await invoke("export_conversation", { conversationId: activeConvId, path }).catch((e) => pushToast("error", humanizeError(String(e))));
  }

  async function handleExportLedger() {
    const path = await save({ defaultPath: "mdga-token-ledger.csv", filters: [{ name: "CSV", extensions: ["csv"] }] }).catch(() => null);
    if (!path) return;
    await invoke("export_token_ledger", { path }).catch((e) => pushToast("error", humanizeError(String(e))));
  }

  async function handleClearData() {
    if (!window.confirm("确定清除所有会话与消息？此操作不可撤销。")) return;
    await invoke("clear_all_conversations").catch(() => {});
    setConversations([]);
    setActiveConvId(null);
    setMessages([]);
  }

  async function openSettings(section: SettingsSection = "account") {
    setSettingsSection(section);
    const info = await invoke<AppInfo>("get_app_info").catch(() => null);
    setAppInfo(info);
    await refreshMcpServers();
    await refreshPermRules();
    setCommandSandbox(await invoke<boolean>("get_command_sandbox").catch(() => true));
    setTaskBudget(await invoke<number>("get_task_budget").catch(() => 0));
    setShowSettings(true);
    // 余额查询门禁（Plan21 #5）：仅 DeepSeek 主供应商才打余额端点，
    // 非 DeepSeek 时后端会返回 Err，且余额区改为提示文案，无需发起请求。
    if (isDeepseekMain) refreshBalance();
  }

  async function handleAddMcpServer(name: string, command: string, authToken?: string) {
    await invoke("create_mcp_server", { name, command, authToken: authToken || null }).catch((err) => {
      pushToast("error", humanizeError(String(err))); // MCP 添加失败属即时操作失败 → toast（Plan20 🔴2）
    });
    await refreshMcpServers();
  }

  async function handleToggleMcpServer(id: string, enabled: boolean) {
    await invoke("toggle_mcp_server", { serverId: id, enabled }).catch(() => {});
    await refreshMcpServers();
  }

  async function handleDeleteMcpServer(id: string) {
    await invoke("delete_mcp_server", { serverId: id }).catch(() => {});
    await refreshMcpServers();
  }

  /** 导入本地文档：抽取文本后自动作为消息发送，进入问答流程。 */
  async function handleImportFile() {
    if (sending) return;
    try {
      // 模态门禁（Plan17 §7）：有 vision provider 才放开图像入口（本阶段仅放开选择，不做识图推理）。
      const visionCfg = await invoke<ProviderConfig | null>("get_model_provider_config", { role: "vision" }).catch(() => null);
      const hasVision = !!visionCfg;
      const textExtensions = ["txt", "md", "csv", "json", "log", "pdf", "docx", "xml", "html", "toml", "yaml", "yml"];
      const filters = hasVision
        ? [{ name: "文档与图像", extensions: [...textExtensions, ...IMAGE_EXTENSIONS] }]
        : [{ name: "文档", extensions: textExtensions }];
      const selected = await open({ multiple: false, filters });
      if (!selected || Array.isArray(selected)) return;
      // 即使对话框可能放过（部分平台 filter 不强制），再按扩展名兜底门禁：无 vision 拒绝图像。
      const ext = selected.split(".").pop()?.toLowerCase() ?? "";
      if (IMAGE_EXTENSIONS.includes(ext)) {
        if (!hasVision) {
          pushToast("error", "当前未配置视觉模型，无法导入图片。需在 设置 → 模型供应商 → 扩展 agent 的模态 里配置视觉模型。");
          return;
        }
        // 有 vision provider（Plan18 M18.1）：读图为 base64 + media_type，暂存到输入框上方缩略图预览，
        // 随下一次发送上送视觉模型识图。
        const img = await invoke<{ name: string; mediaType: string; base64: string }>(
          "read_image_base64",
          { path: selected }
        );
        setPendingImages((prev) => [
          ...prev,
          { type: "image", mediaType: img.mediaType, base64: img.base64, name: img.name },
        ]);
        return;
      }
      const result = await invoke<{ name: string; text: string; truncated: boolean }>(
        "import_file_text",
        { path: selected }
      );
      const note = result.truncated ? "\n\n（文档过长，已截断导入前 10 万字符）" : "";
      const prepared = `请阅读以下导入文档《${result.name}》，先给出简要总结，然后准备回答我关于它的问题：${note}\n\n${result.text}`;
      setInput(prepared.slice(0, 200) + (prepared.length > 200 ? "…" : ""));
      await sendText(prepared);
    } catch (err) {
      pushToast("error", humanizeError(String(err))); // 导入失败属即时操作失败 → toast（Plan20 🔴2）
    }
  }

  /**
   * 粘贴/拖拽图片入托盘（Plan19 P0b）：读 Blob 为 base64（去 data: 前缀）→ 校验类型与大小 →
   * push 进 pendingImages（复用现有缩略图托盘）。门禁与 📎 一致：仅在已配视觉模型时接受。
   */
  async function ingestImageBlobs(files: File[]) {
    const images = files.filter((f) => f.type.startsWith("image/"));
    if (images.length === 0) return;
    // 模态门禁：无视觉 provider 时拒绝，提示与 📎 入口一致。
    const visionCfg = await invoke<ProviderConfig | null>("get_model_provider_config", { role: "vision" }).catch(() => null);
    if (!visionCfg) {
      pushToast("error", "当前未配置视觉模型，无法导入图片。需在 设置 → 模型供应商 → 扩展 agent 的模态 里配置视觉模型。");
      return;
    }
    const MAX_BYTES = 10 * 1024 * 1024; // 10MB 上限
    for (const file of images) {
      // mediaType 取 image/png|jpeg|gif|webp；jpg 归一为 jpeg。
      const subtype = file.type.slice("image/".length).toLowerCase();
      const okType = ["png", "jpeg", "jpg", "gif", "webp"].includes(subtype);
      if (!okType) {
        pushToast("error", `不支持的图片格式：${file.type || "未知"}（仅支持 png/jpg/jpeg/gif/webp）`);
        continue;
      }
      if (file.size > MAX_BYTES) {
        pushToast("error", `图片过大（${(file.size / 1024 / 1024).toFixed(1)}MB），上限 10MB`);
        continue;
      }
      const base64 = await new Promise<string | null>((resolve) => {
        const reader = new FileReader();
        reader.onload = () => {
          const result = typeof reader.result === "string" ? reader.result : "";
          // FileReader 读到的是 data:URL，去掉 data: 前缀只留 base64 主体。
          const comma = result.indexOf(",");
          resolve(comma >= 0 ? result.slice(comma + 1) : null);
        };
        reader.onerror = () => resolve(null);
        reader.readAsDataURL(file);
      });
      if (!base64) {
        pushToast("error", "读取图片失败，请重试");
        continue;
      }
      const mediaType = subtype === "jpg" ? "image/jpeg" : file.type;
      setPendingImages((prev) => [
        ...prev,
        { type: "image", mediaType, base64, name: file.name || undefined },
      ]);
    }
  }

  /** composer 粘贴：截取剪贴板里的 image 项，走统一入托盘逻辑（Plan19 P0b）。 */
  function handleComposerPaste(e: React.ClipboardEvent) {
    const files = Array.from(e.clipboardData?.items ?? [])
      .filter((it) => it.kind === "file" && it.type.startsWith("image/"))
      .map((it) => it.getAsFile())
      .filter((f): f is File => !!f);
    if (files.length === 0) return;
    e.preventDefault(); // 阻止把二进制当文本粘进输入框
    void ingestImageBlobs(files);
  }

  /** composer 拖拽放下：取拖入的 image 文件入托盘（Plan19 P0b）。 */
  function handleComposerDrop(e: React.DragEvent) {
    const files = Array.from(e.dataTransfer?.files ?? []).filter((f) => f.type.startsWith("image/"));
    if (files.length === 0) return;
    e.preventDefault();
    setDragOver(false);
    void ingestImageBlobs(files);
  }

  function handleComposerDragOver(e: React.DragEvent) {
    if (!Array.from(e.dataTransfer?.items ?? []).some((it) => it.kind === "file")) return;
    e.preventDefault();
    setDragOver(true);
  }

  function handlePermissionModeChange(next: PermissionMode) {
    setPermissionMode(next);
    localStorage.setItem("mdga.defaultPermissionMode", next);
  }

  /** 输入框 @文件引用：取光标前最后一个 @ 开头的 token 作为过滤词 */
  function updateFileMention(value: string) {
    const match = /(?:^|\s)@([^\s@]*)$/.exec(value);
    setFileMention(match ? match[1] : null);
  }

  function applyFileMention(path: string) {
    setInput((prev) => prev.replace(/(?:^|\s)@([^\s@]*)$/, (m) =>
      m.startsWith(" ") ? ` @${path} ` : `@${path} `
    ));
    setFileMention(null);
  }

  function handleKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    // 斜杠菜单键盘导航（Plan27 #7）：方向键移动、Enter 选中、Esc 关。
    if (slashMenuOpen && slashMenuItems.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSlashActive((i) => Math.min(i + 1, slashMenuItems.length - 1));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setSlashActive((i) => Math.max(i - 1, 0));
        return;
      }
      if (e.key === "Enter" && !e.shiftKey) {
        const item = slashMenuItems[Math.min(slashActive, slashMenuItems.length - 1)];
        if (item && !item.conflict) {
          e.preventDefault();
          setInput(item.cmd);
          return;
        }
      }
      if (e.key === "Escape") {
        // 退出斜杠菜单：清空 / 前缀（保留已输入内容简单起见直接清空 input）。
        e.preventDefault();
        setInput("");
        return;
      }
    }
    // @文件菜单键盘导航（Plan27 #7）。
    if (mentionMenuOpen && mentionItems.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setMentionActive((i) => Math.min(i + 1, mentionItems.length - 1));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setMentionActive((i) => Math.max(i - 1, 0));
        return;
      }
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        applyFileMention(mentionItems[Math.min(mentionActive, mentionItems.length - 1)]);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setFileMention(null);
        return;
      }
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      if (sending) {
        queueSteeringMessage();
      } else {
        handleSend();
      }
    }
  }

  /** 弹层菜单通用键盘漫游（Plan27 #7）：方向键在菜单项间移动焦点，Esc 关闭并回退焦点。 */
  function handleMenuKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    const container = e.currentTarget;
    const items = Array.from(container.querySelectorAll<HTMLButtonElement>('button:not([disabled])'));
    if (items.length === 0) return;
    const curIdx = items.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === "ArrowDown") {
      e.preventDefault();
      items[Math.min(curIdx + 1, items.length - 1)]?.focus();
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      items[Math.max(curIdx - 1, 0)]?.focus();
    } else if (e.key === "Escape") {
      e.preventDefault();
      setWorkspaceMenuOpen(false);
      setCtxPopoverOpen(false);
    }
  }

  /** Agent 运行中，把输入作为插话排队（下一轮注入），不打断当前任务。 */
  async function queueSteeringMessage() {
    const text = input.trim();
    if (!text || !activeConvId) return;
    setInput("");
    setQueuedSteering((prev) => [...prev, text]);
    await invoke("queue_steering", { conversationId: activeConvId, text }).catch(() => {
      setQueuedSteering((prev) => prev.filter((m) => m !== text));
    });
  }

  const hasMessages = messages.length > 0;
  // 最后一条助手消息下标（Plan27 #1b）：仅它显示「重新生成」按钮。-1 表示无助手消息。
  let lastAssistantIdx = -1;
  for (let i = messages.length - 1; i >= 0; i--) {
    if (messages[i].role === "assistant") { lastAssistantIdx = i; break; }
  }
  const activeConversation = conversations.find((conv) => conv.id === activeConvId);
  const conversationUsage = aggregateUsage(messages);

  // 斜杠菜单可见项（Plan27 #7：提到 render 顶层供键盘导航与 JSX 共用）。
  const slashMenuOpen = input.startsWith("/") && !input.includes(" ") && !sending;
  const slashMenuItems = slashMenuOpen
    ? [
        ...SLASH_COMMANDS.map((c) => ({ cmd: c.cmd, desc: c.desc, conflict: false })),
        ...customCommands.map((c) => ({
          cmd: c.name,
          desc: c.description || "自定义命令",
          conflict: SLASH_COMMANDS.some((b) => b.cmd === c.name),
        })),
      ].filter((c) => c.cmd.startsWith(input))
    : [];
  // 分段渲染窗口（Plan27 #8）：只渲染最近 visibleCount 条；更早的折叠到顶部「加载更早」。
  const windowStart = Math.max(0, messages.length - visibleCount);
  const visibleMessages = messages.slice(windowStart);
  const hiddenCount = windowStart;

  // @文件菜单可见项。
  const mentionMenuOpen = fileMention !== null && workspaceFiles.length > 0;
  const mentionItems = mentionMenuOpen
    ? workspaceFiles
        .filter((f) => f.toLowerCase().includes((fileMention ?? "").toLowerCase()))
        .slice(0, 8)
    : [];
  // 上下文占用百分比（Plan26 底栏指示器）：无数据时为 null。
  const ctxPct = ctxUsage && ctxUsage.softLimit > 0
    ? Math.round((ctxUsage.promptTokens / ctxUsage.softLimit) * 100)
    : null;

  // 侧边栏列表（Plan27 #6）：搜索词非空时用后端正文搜索结果（searchResults，标题或正文命中），
  // 空时回退本地全量列表；归档的拆到独立折叠区（置顶排序由后端 SQL 保证）。
  const query = searchQuery.trim();
  const filteredConversations = query
    ? (searchResults ?? [])
    : conversations;
  const visibleConversations = filteredConversations.filter((c) => !c.archived);
  const archivedConversations = filteredConversations.filter((c) => c.archived);

  // ── UI ──────────────────────────────────────────────────────────────────

  return (
    <main className="app-shell">
      {/* 侧边栏（Task B：抽至 ./components/Sidebar，状态留 App、只传 props） */}
      <Sidebar
        conversations={conversations}
        visibleConversations={visibleConversations}
        archivedConversations={archivedConversations}
        activeConvId={activeConvId}
        searchQuery={searchQuery}
        onSearchChange={setSearchQuery}
        showArchived={showArchived}
        onToggleArchived={() => setShowArchived((v) => !v)}
        editingConvId={editingConvId}
        editingTitle={editingTitle}
        onEditingTitleChange={setEditingTitle}
        onCommitRename={commitRename}
        onCancelRename={() => setEditingConvId(null)}
        onStartRename={startRename}
        onNewConversation={handleNewConversation}
        onSelectConversation={handleSelectConversation}
        onDeleteConversation={handleDeleteConversation}
        onTogglePin={handleTogglePin}
        onToggleArchive={handleToggleArchive}
        theme={theme}
        onToggleTheme={() => setTheme((t) => (t === "dark" ? "light" : "dark"))}
        onOpenSettings={() => openSettings()}
        update={update}
        onInstallUpdate={handleInstallUpdate}
        onDismissUpdate={() => setUpdate({ status: "idle" })}
      />

      {/* 工作区 */}
      <section className="workspace" aria-label="MDGA workspace">
        <header className="topbar topbar--slim">
          <div className="topbar__brand">
            <p className="eyebrow">Make DeepSeek Great Again</p>
            <h1>MDGA</h1>
          </div>
          <div className="status-strip" aria-label="status">
            {/* 工作区身份由 composer 底部胶囊承载、上下文用量由底栏指示器承载（Plan26），顶栏仅保留「变更」。 */}
            {activeConvId && (
              <button
                className="topbar-btn"
                type="button"
                title="文件变更记录（可回退）"
                onClick={openChangesPanel}
              >
                <GitCompare size={14} /> 变更
              </button>
            )}
          </div>
        </header>

        {hasMessages ? (
          <section
            className="message-list"
            aria-label="Conversation"
            ref={messageListRef}
            onScroll={handleMessageListScroll}
          >
            {/* 顶部「加载更早」（Plan27 #8）：仍有更早消息未渲染时显示，点击放开一段窗口。 */}
            {hiddenCount > 0 && (
              <button
                type="button"
                className="load-earlier"
                onClick={() => setVisibleCount((c) => c + MSG_WINDOW)}
              >
                <ChevronDown size={14} style={{ transform: "rotate(180deg)" }} /> 加载更早的 {Math.min(MSG_WINDOW, hiddenCount)} 条消息（还有 {hiddenCount} 条）
              </button>
            )}
            {visibleMessages.map((msg, j) => {
              const i = windowStart + j; // 原始下标，用于 key 与 lastAssistant 判定
              return (
                <div key={i} className="message-row">
                  <div className={`message message--${msg.role}`}>
                    <MessageContent msg={msg} />
                  </div>
                  <MessageActions
                    msg={msg}
                    disabled={sending}
                    isLastAssistant={msg.role === "assistant" && i === lastAssistantIdx}
                    onCopy={() => messageText(msg)}
                    onResend={() => resendMessage(msg)}
                    onEditRetry={() => editRetryMessage(msg)}
                    onRegenerate={handleRegenerate}
                  />
                  {msg.role === "assistant" && msg.usage && (
                    <UsageBadge usage={msg.usage} showCost={isDeepseekMain} />
                  )}
                </div>
              );
            })}
            {sending && (
              <div className="agent-working" aria-label="Agent 工作状态">
                <span className="star-spin" aria-hidden="true">✦</span>
                <span>{agentStatus ?? "正在思考…"}</span>
                <span className="agent-working__elapsed">{elapsedSec}s</span>
              </div>
            )}
            <div ref={messagesEndRef} />
            {/* 跳到最新（Plan20 🟠4）：非贴底时浮现，点击回底并恢复跟随。 */}
            {!isAtBottom && (
              <button
                type="button"
                className="jump-latest"
                onClick={jumpToLatest}
                aria-label="跳到最新"
                title="跳到最新"
              >
                <ChevronDown size={16} /> 跳到最新
              </button>
            )}
          </section>
        ) : (
          <section className="hero-panel" aria-label="New conversation">
            {/* B4：空态以「提问语 + 输入框」为重心；工作区入口下沉到 composer 胶囊。 */}
            <h2>我们应该在 MDGA 中做些什么？</h2>
            <p className="hero-panel__hint">在下方输入框左下角选择工作区，或直接开始纯聊天。</p>
            {workspaceError && <p className="hero-panel__error">{workspaceError}</p>}
            {/* 未配主模型引导（Plan19 P0a）：显著 CTA，点击直达 设置 → 模型供应商。 */}
            {mainConfigured === false && (
              <div className="onboarding-cta" role="status" aria-label="需要配置模型">
                <div className="onboarding-cta__text">
                  <strong>还没配置模型</strong>
                  <span>先去「设置 → 模型供应商」配置主模型（填 API Key 与模型 ID），即可开始对话。</span>
                </div>
                <button
                  type="button"
                  className="onboarding-cta__btn"
                  onClick={() => openSettings("provider")}
                >
                  <Settings2 size={15} /> 去配置模型
                </button>
              </div>
            )}
            {/* B4：能力卡弱化为一行精简提示，不再抢空态视觉焦点。 */}
            <ul className="hero-tips" aria-label="能力概览">
              <li>应用内配置供应商，密钥仅存本地</li>
              <li>请求级成本透明，可导出账本</li>
              <li>权限分级，高风险动作先审批</li>
            </ul>
          </section>
        )}

        {conversationUsage && (
          <ConversationUsageSummary usage={conversationUsage} showCost={isDeepseekMain} />
        )}

        {todos.length > 0 && <TodoPanel items={todos} />}

        {queuedSteering.length > 0 && (
          <div className="steering-queue" aria-label="排队的插话">
            {queuedSteering.map((m, i) => (
              <span key={i} className="steering-chip" title={m}>
                <CornerDownRight size={12} /> {m.length > 30 ? m.slice(0, 30) + "…" : m}
              </span>
            ))}
            <span className="steering-queue__hint">将在下一轮注入</span>
          </div>
        )}

        {/* C-4 计划闭环「批准并执行」：上一轮计划模式成功产出后、且当前未在发送时，
            在 composer 上方显示醒目按钮；点击以 planMode=false + executePlan=true 发送「按上述计划执行」。 */}
        {awaitingPlanApproval && !sending && (
          <div className="plan-approval" role="region" aria-label="批准并执行计划">
            <div className="plan-approval__text">
              <ListChecks size={15} className="plan-approval__icon" />
              <span>计划已就绪。确认后将关闭计划模式并严格按上述计划执行。</span>
            </div>
            <button
              type="button"
              className="plan-approval__btn"
              onClick={approveAndExecutePlan}
            >
              <Check size={15} /> 批准并执行计划
            </button>
          </div>
        )}

        <div className="composer-area">
          {/* 斜杠命令菜单（Plan27 #7：键盘高亮，方向键移动 + Enter 选中 + Esc 关）。 */}
          {slashMenuOpen && slashMenuItems.length > 0 && (
            <div className="slash-menu" role="listbox" aria-label="斜杠命令">
              {/* Plan21 #9：内置命令优先,与内置同名的自定义命令在 handleSlashCommand 里被忽略,
                  菜单条目据此标注「与内置命令冲突,已被忽略」并置灰。 */}
              {slashMenuItems.map((c, i) => (
                <button
                  key={`${c.cmd}-${i}`}
                  className={`slash-menu__item${c.conflict ? " slash-menu__item--conflict" : ""}${i === slashActive ? " slash-menu__item--active" : ""}`}
                  type="button"
                  role="option"
                  aria-selected={i === slashActive}
                  disabled={c.conflict}
                  onMouseEnter={() => setSlashActive(i)}
                  onClick={() => !c.conflict && setInput(c.cmd)}
                >
                  <span className="slash-menu__cmd">{c.cmd}</span>
                  <span className="slash-menu__desc">
                    {c.desc}
                    {c.conflict && <span className="slash-menu__conflict"> · 与内置命令冲突，已被忽略</span>}
                  </span>
                </button>
              ))}
            </div>
          )}

          {/* @文件引用补全（Plan27 #7：键盘高亮）。 */}
          {mentionMenuOpen && mentionItems.length > 0 && (
            <div className="slash-menu" role="listbox" aria-label="文件引用">
              {mentionItems.map((f, i) => (
                <button
                  key={f}
                  className={`slash-menu__item${i === mentionActive ? " slash-menu__item--active" : ""}`}
                  type="button"
                  role="option"
                  aria-selected={i === mentionActive}
                  onMouseEnter={() => setMentionActive(i)}
                  onClick={() => applyFileMention(f)}
                >
                  <AtSign size={14} className="slash-menu__icon" />
                  <span className="slash-menu__cmd">{f}</span>
                </button>
              ))}
            </div>
          )}

          {/* B1：composer 统一容器 —— 上为 textarea、中为待发图托盘、下为底部控制行。
              拖拽高亮/粘贴/拖放等行为绑定在此容器上，保持原逻辑不变。 */}
          <div
            className={`composer composer--unified${dragOver ? " composer--dragover" : ""}`}
            onDrop={handleComposerDrop}
            onDragOver={handleComposerDragOver}
            onDragLeave={() => setDragOver(false)}
          >
            <textarea
              className="composer__input"
              aria-label="Message"
              placeholder={sending ? "Agent 运行中：输入并回车可插话，下一轮生效（不打断当前任务）" : planMode ? "计划模式：先出计划，确认后再执行（Enter 发送）" : "随心输入（Enter 发送，Shift+Enter 换行，/ 命令，@ 引用文件）"}
              value={input}
              onChange={(e) => {
                setInput(e.target.value);
                updateFileMention(e.target.value);
              }}
              onKeyDown={handleKeyDown}
              onPaste={handleComposerPaste}
            />

            {/* 附图预览（Plan18 M18.1）：选中的图片在发送前显示缩略图，可逐个移除。位于 textarea 与 footer 之间。 */}
            {pendingImages.length > 0 && (
              <div className="image-tray" aria-label="待发送图片">
                {pendingImages.map((img, i) => (
                  <span key={i} className="image-tray__item" title={img.name}>
                    <img
                      className="image-tray__thumb"
                      src={`data:${img.mediaType};base64,${img.base64}`}
                      alt={img.name ?? "待发送图片"}
                    />
                    <button
                      type="button"
                      className="image-tray__remove"
                      aria-label="移除图片"
                      onClick={() => setPendingImages((prev) => prev.filter((_, j) => j !== i))}
                    >
                      ×
                    </button>
                  </span>
                ))}
              </div>
            )}

            {/* 底部控制行（Plan26）：左组 [+ 附件][上下文][工作区][权限][计划]，右组 [模型][发送/停止]。 */}
            <div className="composer-footer">
              <div className="composer-footer__left">
                {/* 附件「+」（B1）：原 handleImportFile，图标由 Paperclip 改为 Plus。 */}
                <button
                  type="button"
                  className="composer__attach"
                  title="导入本地文档（txt/md/csv/pdf/docx）或图片（需配置视觉模型）"
                  aria-label="导入文档或图片"
                  onClick={handleImportFile}
                  disabled={sending}
                >
                  <Plus size={18} />
                </button>

                {/* 上下文指示器 + 弹层（Plan26）：从顶栏移入，点击展开「上下文窗口 用量/上限」+ 简化余额。 */}
                <div className="ctx-pill-wrap">
                  <button
                    type="button"
                    className="ctx-pill"
                    aria-haspopup="dialog"
                    aria-expanded={ctxPopoverOpen}
                    title="上下文用量"
                    onClick={() => {
                      const next = !ctxPopoverOpen;
                      setCtxPopoverOpen(next);
                      if (next && isDeepseekMain && balance.status === "idle") refreshBalance();
                    }}
                  >
                    <span className="ctx-pill__top">
                      <span className="ctx-pill__label">
                        <Gauge size={12} className="ctx-pill__icon" />
                        上下文
                      </span>
                      {ctxPct !== null && <span className="ctx-pill__pct">{ctxPct}%</span>}
                    </span>
                    <span className="ctx-pill__bar">
                      <span className="ctx-pill__fill" style={{ width: `${ctxPct ?? 0}%` }} />
                    </span>
                  </button>
                  {ctxPopoverOpen && (
                    <>
                      <div className="workspace-menu__backdrop" onClick={() => setCtxPopoverOpen(false)} />
                      <div className="ctx-popover" role="dialog" aria-label="上下文用量" onKeyDown={handleMenuKeyDown}>
                        <div className="ctx-popover__row">
                          <span className="ctx-popover__label">上下文窗口</span>
                          <span className="ctx-popover__nums">
                            {ctxUsage && ctxUsage.softLimit > 0
                              ? `${fmtTokens(ctxUsage.promptTokens)} / ${fmtTokens(ctxUsage.softLimit)} (${ctxPct}%)`
                              : "暂无数据"}
                          </span>
                        </div>
                        <span className="ctx-popover__bar">
                          <span className="ctx-popover__fill" style={{ width: `${ctxPct ?? 0}%` }} />
                        </span>
                        <div className="ctx-popover__foot">
                          <span>接近上限自动压缩</span>
                          {isDeepseekMain && (
                            <button
                              type="button"
                              className="ctx-popover__link"
                              onClick={() => { setCtxPopoverOpen(false); openSettings("account"); }}
                            >
                              {balance.status === "ok" && balance.data.balanceInfos[0]
                                ? `余额 ${balance.data.balanceInfos[0].currency} ${balance.data.balanceInfos[0].totalBalance}`
                                : "账户余额"} →
                            </button>
                          )}
                        </div>
                      </div>
                    </>
                  )}
                </div>

                {/* 工作区胶囊（B2）：点击弹出小菜单——选择/更换工作区、纯聊天。 */}
                <div className="workspace-pill-wrap">
                  <button
                    type="button"
                    className={`workspace-pill${composerWorkspaceName() ? " workspace-pill--bound" : ""}`}
                    onClick={() => setWorkspaceMenuOpen((v) => !v)}
                    aria-haspopup="menu"
                    aria-expanded={workspaceMenuOpen}
                    title={composerWorkspaceName() ? `工作区：${composerWorkspaceName()}` : "未绑定工作区（纯聊天），点击选择"}
                  >
                    <FolderOpen size={13} className="workspace-pill__icon" />
                    <span className="workspace-pill__name">{composerWorkspaceName() ?? "选择工作区"}</span>
                  </button>
                  {workspaceMenuOpen && (
                    <>
                      {/* 点击遮罩关闭菜单（覆盖全屏，透明） */}
                      <div className="workspace-menu__backdrop" onClick={() => setWorkspaceMenuOpen(false)} />
                      <div className="workspace-menu" role="menu" onKeyDown={handleMenuKeyDown} ref={workspaceMenuRef}>
                        <button type="button" className="workspace-menu__item" role="menuitem" onClick={handlePickWorkspaceFromComposer}>
                          <FolderOpen size={14} /> 选择/更换工作区…
                        </button>
                        <button type="button" className="workspace-menu__item" role="menuitem" onClick={handleClearWorkspaceFromComposer}>
                          <MessageCircle size={14} /> 纯聊天（不绑定）
                        </button>
                      </div>
                    </>
                  )}
                </div>

                {/* 权限模式紧凑胶囊：精简标签（Plan26），onChange 行为不变。 */}
                <select
                  className="control-select control-select--compact"
                  value={permissionMode}
                  onChange={(e) => handlePermissionModeChange(e.target.value as PermissionMode)}
                  aria-label="权限模式"
                  title={sending ? "切换将在当前回复结束后的下一轮生效" : "权限模式"}
                >
                  {PERMISSION_MODES.map((mode) => (
                    <option key={mode} value={mode}>{PERMISSION_SHORT[mode] ?? getPermissionModeLabel(mode)}</option>
                  ))}
                </select>

                {/* 计划模式 toggle（Plan26：移入左组）。 */}
                <button
                  type="button"
                  className={`control-toggle${planMode ? " control-toggle--on" : ""}`}
                  title="计划模式：让 Agent 先给出分步计划，确认后再执行"
                  onClick={() => setPlanMode((v) => !v)}
                >
                  <ListChecks size={14} /> 计划
                </button>
              </div>

              <div className="composer-footer__right">
                {/* 当前模型只读胶囊（Plan26：移入右组、贴近发送；图标 Gauge→Cpu）。点击进 设置 → 模型供应商。 */}
                <button
                  type="button"
                  className="model-pill"
                  onClick={() => openSettings("provider")}
                  aria-label="当前模型，点击修改"
                  title={mainModelId ? `当前模型：${mainModelId}（点击进 设置 → 模型供应商 修改）` : "尚未配置主模型，点击去配置"}
                >
                  <Cpu size={13} className="model-pill__icon" />
                  <span className="model-pill__id">{mainModelId || "未配置模型"}</span>
                </button>
                {sending ? (
                  <button type="button" className="composer__send composer__send--stop" onClick={handleStop} aria-label="停止">
                    <Square size={14} fill="currentColor" />
                  </button>
                ) : (
                  <button type="button" className="composer__send" onClick={handleSend} disabled={!input.trim()} aria-label="发送">
                    <ArrowUp size={18} />
                  </button>
                )}
              </div>
            </div>
          </div>
        </div>
      </section>

      {approval && (
        <ApprovalModal
          approval={approval}
          onAllow={() => respondApproval(true)}
          onAlwaysAllow={() => respondApproval(true, true)}
          onDeny={() => respondApproval(false)}
        />
      )}

      {askUser && (
        <AskUserModal
          request={askUser}
          onSubmit={respondAskUser}
          onCancel={() => respondAskUser("")}
        />
      )}

      {showChanges && (
        <ChangesModal
          checkpoints={checkpoints}
          onRevert={handleRevert}
          onClose={() => setShowChanges(false)}
        />
      )}

      {showSettings && (
        <SettingsModal
          initialSection={settingsSection}
          onMainConfiguredChange={(configured) => {
            setMainConfigured(configured);
            // 配置/更新主 provider 后重新拉取 model_id（Plan20 🔴1），刷新控制行胶囊与透传值。
            void refreshMainModel();
          }}
          appInfo={appInfo}
          apiKeyLabel={getApiKeyStatusLabel(apiKeyStatus)}
          balance={balance}
          onRefreshBalance={refreshBalance}
          permissionMode={permissionMode}
          mcpServers={mcpServers}
          permRules={permRules}
          commandSandbox={commandSandbox}
          onToggleSandbox={handleToggleSandbox}
          taskBudget={taskBudget}
          onSetBudget={handleSetBudget}
          hasActiveConv={!!activeConvId}
          onExportConversation={handleExportConversation}
          onExportLedger={handleExportLedger}
          onClearData={handleClearData}
          onPermissionModeChange={handlePermissionModeChange}
          onAddMcpServer={handleAddMcpServer}
          onToggleMcpServer={handleToggleMcpServer}
          onDeleteMcpServer={handleDeleteMcpServer}
          onRefreshMcp={refreshMcpServers}
          onAddPermRule={handleAddPermRule}
          onDeletePermRule={handleDeletePermRule}
          onUpdateAvailable={(v) => setUpdate({ status: "available", version: v })}
          onClose={() => setShowSettings(false)}
        />
      )}

      {/* 命令面板（Plan27 #3a）：Ctrl/Cmd+K 打开，搜索会话 + 跳转设置 + 触发命令。 */}
      {showCommandPalette && (
        <CommandPalette
          hasActiveConv={!!activeConvId}
          onClose={() => setShowCommandPalette(false)}
          onNewConversation={handleNewConversation}
          onSelectConversation={(id) => handleSelectConversation(id)}
          onOpenSettings={(section) => openSettings(section)}
          onOpenHelp={() => setShowHelp(true)}
          onOpenChanges={openChangesPanel}
          onRunSlash={(cmd) => { void handleSlashCommand(cmd); }}
        />
      )}

      {/* /help 能力披露面板（Plan27 #3b）：纯静态。 */}
      {showHelp && <HelpModal onClose={() => setShowHelp(false)} />}

      {/* 全局 toast（Plan20 🔴2）：右下角堆叠、自动消失、可手动关，不依赖消息列表。 */}
      {toasts.length > 0 && (
        <div className="toast-stack" role="region" aria-label="通知">
          {toasts.map((t) => (
            <div key={t.id} className={`toast toast--${t.kind}`} role={t.kind === "error" ? "alert" : "status"}>
              {t.kind === "error" ? <Ban size={15} className="toast__icon" /> : <Info size={15} className="toast__icon" />}
              <span className="toast__text">{t.text}</span>
              <button
                type="button"
                className="toast__close"
                aria-label="关闭通知"
                onClick={() => dismissToast(t.id)}
              >
                <X size={14} />
              </button>
            </div>
          ))}
        </div>
      )}
    </main>
  );
}
