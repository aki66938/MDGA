// 类型定义与常量（0.0.37 从 App.tsx 抽出，纯搬移，无逻辑改动）。
// 仅收纳 App.tsx 内**定义**的 type 与常量；从别处 import 的常量保持原 import 不动。

import type React from "react";
import { type PermissionMode } from "@mdga/ui";

/** 权限模式在底栏胶囊里的精简标签（设置页仍用完整 getPermissionModeLabel）。 */
export const PERMISSION_SHORT: Record<string, string> = {
  restricted: "受限",
  ask_every_time: "询问",
  workspace_auto: "自动",
  full_access: "完全",
};

// ── 类型定义 ──────────────────────────────────────────────────────────────

/** 消息中的文字块，直接 Markdown 渲染 */
export type TextPart = { type: "text"; content: string };

/** 消息中的工具执行卡片，内联展示于叙述文字之间 */
export type ToolPart = {
  type: "tool";
  toolName: string;
  target: string;
  status: "running" | "succeeded" | "failed" | "denied";
  error?: string;
  /** 文件写类工具的行级 diff（unified 格式），点击行可展开查看 */
  diff?: string;
  added?: number;
  removed?: number;
  /** run_command 运行中的实时输出（行累积，截断保留尾部） */
  liveOutput?: string;
  /** 已被回退（Plan21 #3）：handleRevert 成功后给现存 diff 卡片打标，渲染置灰并加「已回退」角标。 */
  reverted?: boolean;
};

/** Agent 自维护的任务清单项（todo_write 工具） */
export type TodoItem = {
  text: string;
  status: "pending" | "in_progress" | "done";
};

/** 文件变更检查点（rewind 用） */
export type FileCheckpoint = {
  id: string;
  conversationId: string;
  seq: number;
  toolName: string;
  relPath: string;
  revertible: boolean;
  reverted: boolean;
  createdAt: number;
};

export type AppInfo = { version: string; dataDir: string };

/** 最近被拦动作（Plan27 #9）：recent_denied_actions 返回，用于一键加规则。 */
export type DeniedAction = { toolName: string; target: string };

/** DeepSeek 账户余额（官方 /user/balance，唯一账户信息来源） */
export type BalanceInfo = {
  currency: string;
  totalBalance: string;
  grantedBalance: string;
  toppedUpBalance: string;
};
export type UserBalance = { isAvailable: boolean; balanceInfos: BalanceInfo[] };
export type BalanceState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "ok"; data: UserBalance }
  | { status: "error"; message: string };

/** MCP server 配置与连接状态 */
export type McpServer = {
  id: string;
  name: string;
  command: string;
  enabled: boolean;
  connected: boolean;
  toolCount: number;
};

/** 高风险动作审批请求，由后端在 AskEveryTime / 越界场景下发起 */
export type ApprovalRequest = {
  actionId: string;
  toolName: string;
  target: string;
  /** 动作内容预览（Plan19 C-C）：命令全文 / 文件内容前若干行 / diff；空串表示无预览。 */
  preview?: string;
};

/** ask_user 结构化提问：Agent 在需求含糊时主动发起，前端弹选择卡片 */
export type AskOption = { label: string; description?: string };
export type AskQuestion = {
  question: string;
  header?: string;
  multiSelect?: boolean;
  options: AskOption[];
};
export type AskUserRequest = { questionId: string; questions: AskQuestion[] };

/** 系统通知卡片：上下文压缩等用户需要感知的运行时事件，内联显示在对话流中 */
export type NoticePart = { type: "notice"; text: string };

/** 全局 toast（Plan20 🔴2）：右下角堆叠的瞬时通知，不依赖消息列表，承载用户主动操作的即时成败。 */
export type Toast = { id: number; kind: "error" | "info"; text: string };

/** 用户消息中附带的图片（Plan18 M18.1）：mediaType + base64，渲染为缩略图；持久化进 partsJson。 */
export type ImagePart = { type: "image"; mediaType: string; base64: string; name?: string };

/**
 * 后端 RawUsage 的线上形状（Plan19 C-B）：serde 默认 snake_case。
 * 视觉分析的 usage 由后端归一为该结构，前端只取 total_tokens 做小徽标展示。
 */
export type RawUsageWire = {
  prompt_tokens?: number;
  completion_tokens?: number;
  total_tokens?: number;
};

/**
 * 视觉分析卡片块（Plan19 C-B）：自动初看完成后，作为助手消息 parts_json 的首个 part 持久化；
 * 发送中亦由 "vision-analysis" 事件即时插入。默认折叠，展开见 analysis 文本。
 */
export type VisionPart = {
  type: "vision";
  count: number;
  analysis: string;
  usage?: RawUsageWire | null;
};

/**
 * 思考过程块（Plan27 #1a）：流式监听 "chat-reasoning" 累积模型 reasoning_content，
 * 作为助手消息 parts 的一员持久化（排在 vision 之后、正文之前）。默认折叠的「🧠 思考过程」卡片。
 */
export type ReasoningPart = { type: "reasoning"; content: string };

export type MessagePart = TextPart | ToolPart | NoticePart | ImagePart | VisionPart | ReasoningPart;

export type Message = {
  role: "user" | "assistant";
  /** 所有内容都用 parts 表示，文字与工具卡片交错排列 */
  parts: MessagePart[];
  usage?: UsageSummary;
};

export type UsageSummary = {
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

export type Conversation = {
  id: string;
  title: string;
  workspacePath?: string | null;
  workspaceName?: string | null;
  mode: "chat_only" | "local_workspace";
  pinned: boolean;
  archived: boolean;
  createdAt: number;
  updatedAt: number;
};

export type StoredMessage = {
  id: string;
  conversationId: string;
  role: string;
  content: string;
  usageJson: string | null;
  partsJson: string | null;
  createdAt: number;
};

export type ToolEvent = {
  toolName: string;
  status: string;
  inputJson?: string | null;
  outputJson?: string | null;
  errorMessage?: string | null;
};

export type DraftWorkspace = {
  name: string;
  path: string;
};

export type UpdateState =
  | { status: "idle" }
  | { status: "checking" }
  | { status: "uptodate" }
  | { status: "available"; version: string }
  | { status: "downloading"; progress: number }
  | { status: "error"; message: string };

export const PERMISSION_MODES: PermissionMode[] = [
  "restricted",
  "ask_every_time",
  "workspace_auto",
  "full_access",
];

// ── 模型供应商预设（Plan17 §6.2）─────────────────────────────────────────────
// preset 携带官方 base_url（与后端 preset_base_url 表保持一致）；custom 必须用户填 base_url。
export type ProviderPreset = {
  id: string;
  label: string;
  /** 官方端点；作为高级行 Base URL 输入框的 placeholder（留空＝用它）。custom 无官方端点。 */
  baseUrl: string | null;
  /** 切换到该预设时给出的合理默认 modelId 占位。 */
  defaultModelId: string;
};

export const PROVIDER_PRESETS: ProviderPreset[] = [
  { id: "deepseek", label: "DeepSeek", baseUrl: "https://api.deepseek.com", defaultModelId: "deepseek-v4-pro" },
  { id: "zhipu", label: "智谱 GLM", baseUrl: "https://open.bigmodel.cn/api/paas/v4", defaultModelId: "glm-4" },
  { id: "moonshot", label: "月之暗面 Kimi", baseUrl: "https://api.moonshot.cn/v1", defaultModelId: "moonshot-v1-8k" },
  { id: "qwen", label: "通义", baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1", defaultModelId: "qwen-plus" },
  { id: "custom", label: "自定义", baseUrl: null, defaultModelId: "" },
];

/** 视觉块预设默认 modelId 占位（识图模型）。 */
export const VISION_PRESET_MODEL: Record<string, string> = {
  deepseek: "deepseek-vl",
  zhipu: "glm-4v",
  moonshot: "moonshot-v1-8k-vision-preview",
  qwen: "qwen-vl-plus",
  custom: "",
};

/** 主 provider 配置回填形状（get_model_provider_config 返回，apiKey 已脱敏为空）。 */
export type ProviderConfig = {
  id: string;
  role: string;
  preset?: string | null;
  label?: string | null;
  baseUrl?: string | null;
  apiKey: string;
  modelId: string;
  /** 视觉 provider 的 API 格式（openai|anthropic）；主模型恒 openai。 */
  apiFormat?: string | null;
  /** 上下文窗口（tokens，可选，Plan27 #2）：后端据此推导压缩软上限；缺省回退默认值。 */
  contextWindow?: number | null;
  enabled: boolean;
  updatedAt?: number | null;
};

/** 各预设的常见上下文窗口（tokens，Plan27 #2）：保存时可预填，留空亦可。 */
export const PRESET_CONTEXT_WINDOW: Record<string, number | null> = {
  deepseek: 1000000,
  zhipu: 128000,
  moonshot: 128000,
  qwen: 131072,
  custom: null,
};

export const IMAGE_EXTENSIONS = ["png", "jpg", "jpeg", "gif", "webp"];

/** 设置弹窗的分类标识；提到顶层以便首屏 CTA 指定初始分类（Plan19 P0a）。
 *  R-uicfg：新增 "lsp"（语言服务器注册表）与 "routing"（角色→模型路由）。 */
export type SettingsSection =
  | "account"
  | "provider"
  | "routing"
  | "lsp"
  | "permission"
  | "rules"
  | "mcp"
  | "data"
  | "about";

// ── LSP 服务器注册表（R-uicfg / 0.0.57）──────────────────────────────────────

/** 一个已知语言服务器的只读描述（get_lsp_known_servers 返回，源自后端硬编码注册表）。 */
export type LspKnownServer = {
  kind: string;
  displayName: string;
  command: string;
  args: string[];
  extensions: string[];
};

/** 单个已知服务器的用户设置（启用 + 可选路径覆盖）。 */
export type LspServerSetting = {
  enabled: boolean;
  pathOverride?: string | null;
};

/** 全部已知服务器的稀疏配置：键为服务器 kind。缺席＝启用且无覆盖（走 PATH）。
 *  对应后端 LspServerConfig 的透明 map 形状（{ servers: ... } 被 serde transparent 摊平为裸 map）。 */
export type LspServerConfig = Record<string, LspServerSetting>;

// ── R8 角色→模型路由（R-uicfg / 0.0.57）─────────────────────────────────────

/** 一个功能角色当前的路由概览（get_role_routing 返回）。 */
export type RoleRouting = {
  role: "action" | "plan" | "critique";
  configured: boolean;
  effectivePreset?: string | null;
  effectiveModel?: string | null;
  /** 'self' 用角色自身配置 | 'main' 回退主模型 | 'none' 主模型也没配。 */
  source: "self" | "main" | "none";
};

/** 三个可路由的功能角色及其中文展示名（路由设置页用）。 */
export const ROUTING_ROLES: Array<{ id: "action" | "plan" | "critique"; label: string; desc: string }> = [
  { id: "action", label: "行动（Action）", desc: "执行工具的常规循环用此模型" },
  { id: "plan", label: "规划（Plan）", desc: "计划模式 / 规划步骤用此模型" },
  { id: "critique", label: "评审（Critique）", desc: "审查 / 批评步骤用此模型（暂为预留角色）" },
];

/** 斜杠命令清单：输入框以 / 开头时弹出 */
export const SLASH_COMMANDS: Array<{ cmd: string; desc: string }> = [
  { cmd: "/compact", desc: "把当前会话历史压缩为摘要，释放上下文" },
  { cmd: "/clear", desc: "开启一个全新会话" },
  { cmd: "/init", desc: "让 Agent 分析项目并生成 MDGA.md 长期记忆" },
  { cmd: "/rewind", desc: "打开文件变更记录，可回退改动" },
  { cmd: "/model", desc: "打开 设置 → 模型供应商，修改主模型" },
  { cmd: "/help", desc: "查看 MDGA 能做什么（工作区、@引用、命令、快捷键等）" },
];

/** /init 发送的固定提示词（对标 CC 的 /init 生成 CLAUDE.md） */
export const INIT_PROMPT =
  "请分析当前工作区项目：阅读关键文件、理解项目目标、技术栈、目录结构与开发约定，然后在工作区根目录创建（或更新）MDGA.md 文件，写入项目长期记忆：项目目标、架构概览、关键约定、常用命令。内容要精炼，控制在 100 行以内。";

// ── CommandPalette / MessageContent 局部类型 ────────────────────────────────

/** 命令面板列表项（Plan27 #3a） */
export type PaletteItem = {
  id: string;
  label: string;
  hint?: string;
  icon: React.ReactNode;
  run: () => void;
};

/** 渲染块：单个非工具 part，或一段连续的工具调用（聚合为可折叠组） */
export type RenderBlock =
  | { kind: "part"; part: TextPart | NoticePart | ImagePart | VisionPart | ReasoningPart; index: number }
  | { kind: "tools"; parts: ToolPart[]; index: number };
