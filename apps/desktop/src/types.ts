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
  /** 0.0.68 降级可观测：run_command 实际生效的沙箱层（"appcontainer"/"restricted"/缺=未沙箱）。 */
  sandboxLayer?: string;
  /** 0.0.68：本应走 AppContainer 但降级到受限令牌沙箱（无文件/网络隔离）——卡片打降级标。 */
  sandboxDegraded?: boolean;
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

/**
 * 本会话「文件累计改动」聚合项（第三栏·变更标签上半段用）。
 * 数据来源是消息流里已有的 diff 卡（ToolPart 的 diff/added/removed），按文件路径聚合，
 * 而非后端 FileCheckpoint（后者不含 diff/±行数）。同一文件多次修改累加 added/removed、收集各次 diff 文本。
 * reverted：该文件涉及的最后一次写入是否被标记为已回退（弱化展示用，非精确撤销态）。
 */
export type FileChange = {
  path: string;
  added: number;
  removed: number;
  /** 该文件历次写入的 unified diff 文本（按出现顺序），展开时逐段复用 DiffBlock 渲染。 */
  diffs: string[];
  reverted: boolean;
};

export type AppInfo = { version: string; dataDir: string };

/**
 * 后台活动视图（第三栏「活动」标签）：list_bg_activity(conversationId) 返回的一行。
 * kind 区分子代理 / 后台命令；status 取 running/done/killed/error；tokens 可选（子代理消耗）。
 * 注意：list 不含开始时间，运行时长由前端记每个 id 首次出现的时间戳近似计算。
 */
export type BgActivityView = {
  id: string;
  kind: "subagent" | "command";
  label: string;
  status: "running" | "done" | "killed" | "error";
  tokens?: number;
};

/**
 * 单个工具在某会话内的活动量视图（第三栏「用量」标签下半段，get_tool_usage 返回，serde camelCase）。
 * 诚实边界：这不是账单——真账单是会话级/角色级（见 UsageSummary）。本视图仅有「调用次数 + 输出 token
 * 体积粗估（字符数/4 累加，非精确分词、非成本）」，作上下文贡献的相对体积近似。
 * toolName 可能带 `sub:` 前缀＝子代理工具，或为 MCP 工具名。后端已按 outputTokens 降序。
 */
export type ToolUsageView = { toolName: string; calls: number; outputTokens: number };

/**
 * 按消费者（角色/子代理）聚合的**真账单**归因视图（第三栏「用量」标签，
 * get_usage_attribution(conversationId) 返回，serde camelCase；已按 totalTokens 降序）。
 * 诚实边界：与 ToolUsageView（按工具活动量·近似非账单）不同——这是真账单：
 * 每个消费者按它**自己模型**的单价结算，故各消费者成本之和可能与「本会话总」（按主模型单价估）略有出入。
 * consumerType 区分：'main'＝主模型循环｜'vision'＝视觉分析｜'subagent'＝子代理；
 * consumerLabel 为细分小字（如 main 的 action/plan、vision 的 初看/追问、subagent 的 前台/并行/后台）。
 * estimatedCost/currency 可空（该消费者模型未填单价/无金额时只显 tokens）。
 */
export type UsageAttributionView = {
  consumerType: "main" | "vision" | "subagent";
  consumerLabel?: string;
  modelId?: string;
  totalTokens: number;
  promptTokens: number;
  completionTokens: number;
  cachedTokens: number;
  estimatedCost?: number | null;
  currency?: PricingCurrency | null;
};

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

/**
 * 思考深度（Thinking）档位与档案视图。get_thinking_profile(connectionPreset, modelId) 返回
 * ThinkingProfileView | null（查不到该模型思考能力即 null，前端整体隐藏控件）。
 * stops 为可选档位（label 仅展示用）；defaultIndex 为模型默认档；
 * adjustable=false 表示思考强制开启、不可关闭/调档（此时 stops 通常仅 1 项）。
 */
export type ThinkingStopView = { label: string };
export type ThinkingProfileView = { stops: ThinkingStopView[]; defaultIndex: number; adjustable: boolean };

/**
 * 互动卡片块（0.0.67 起；0.0.74 改名 render_artifact）：模型通过 render_artifact 工具产出
 * agent 编写的 HTML/SVG/JS，前端在 sandbox="allow-scripts"（绝不带 allow-same-origin）的 iframe
 * 中内联渲染。code 为不可信内容（agent 生成、可能被 prompt 注入），必须完全隔离；随消息 parts 持久化。
 */
export type ArtifactPart = { type: "artifact"; code: string; title?: string; kind?: "svg" | "html" };

export type MessagePart =
  | TextPart
  | ToolPart
  | NoticePart
  | ImagePart
  | VisionPart
  | ReasoningPart
  | ArtifactPart;

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
  /** 0.0.72 计价：该轮成本（金额随 currency；null＝该模型未填单价/无价表）。后端 chat-usage 事件随新字段下发。 */
  estimatedCost?: number | null;
  /** 0.0.72：该轮成本的币种（null＝无金额可言）。 */
  currency?: PricingCurrency | null;
  /** 0.0.72：该轮计费方式（'api'＝按量｜'subscription'＝套餐内｜'none'＝免计费；缺＝旧数据，按 estimatedCostUsd 回退）。 */
  billingMode?: BillingMode;
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
  { id: "siliconflow", label: "硅基流动", baseUrl: "https://api.siliconflow.cn/v1", defaultModelId: "deepseek-ai/DeepSeek-V3" },
  { id: "custom", label: "自定义", baseUrl: null, defaultModelId: "" },
];

// ── 模型连接库 + 用户登记的模型 + 角色分配（0.0.60）──────────────────────────
// 三层：可复用的「连接」(端点+密钥，配一次) → 用户在每个连接下登记的「模型」(connection→model 一对多)
// → 纯「角色分配」(role → 某个已登记模型的 id，无密钥)。
// 0.0.60 在 0.0.59 的「连接 + 引用」之间补回了用户**主动登记模型**的一层，角色从这些登记的模型里选。

/** 一个连接的前端视图（list_connections / save_connection 返回）。绝不含 apiKey 明文；
 *  以 hasKey 表明是否已配密钥。base_url 空＝走 preset 官方端点。 */
export type ConnectionView = {
  id: string;
  label?: string;
  preset?: string;
  baseUrl?: string;
  apiFormat: string;
  hasKey: boolean;
  createdAt?: number;
  updatedAt?: number;
  /** 0.0.72 计价：连接级计费方式。'api'＝按量付费（模型行可填单价）｜'subscription'＝订阅套餐｜'none'＝本地免计费。缺＝按 'api' 处理。 */
  billingMode?: BillingMode;
  /** 0.0.72：billingMode='subscription' 时的套餐元信息 JSON（SubscriptionInfo 序列化）。 */
  subscriptionJson?: string;
};

// ── 计价（Pricing，0.0.72）──────────────────────────────────────────────────

/** 连接级计费方式。后端 set_connection_billing 接受同名字符串。 */
export type BillingMode = "api" | "subscription" | "none";

/** 价格币种。 */
export type PricingCurrency = "CNY" | "USD";

/** 价格单位：每百万 token 或每千 token。预设均为 per_1m。 */
export type PricingUnit = "per_1m" | "per_1k";

/** 上下文分档价（长上下文阶梯定价）：maxContext 为该档上限 token 数。 */
export type PricingTier = {
  maxContext: number;
  input: number;
  output: number;
  cachedInput?: number | null;
};

/**
 * 一个模型的纯价格结构（与后端 serde camelCase 同名）。
 * input/output/... 的数值与 unit 匹配（unit='per_1k' 即以「每千」计），后端按 unit 归一化。
 */
export type ModelPricing = {
  currency: PricingCurrency;
  unit: PricingUnit;
  /** 输入（未命中缓存）单价。 */
  input: number;
  /** 输出单价。 */
  output: number;
  /** 缓存命中输入单价（可空）。 */
  cachedInput?: number | null;
  /** 缓存写入单价（可空）。 */
  cacheWrite?: number | null;
  /** 批量折扣系数（可空，如 0.5＝五折）。 */
  batchDiscount?: number | null;
  /** 上下文分档（长上下文阶梯定价，可空）。 */
  tiers?: PricingTier[];
};

/**
 * 存进 pricing_json 的对象 = ModelPricing + 下划线前缀元数据。
 * 后端原样存取、计算时只读价格字段（忽略下划线字段）；前端用元数据渲染徽标。
 */
export type StoredPricing = ModelPricing & {
  /** 'preset'＝来自内置预设（可改）｜'custom'＝用户改过的自定义价。 */
  _source: "preset" | "custom";
  /** 预设置信度（如 'high'/'medium'/'low'），仅 _source='preset' 时有意义。 */
  _confidence?: string;
  /** 预设需用户核对（价格可能变动），徽标叠「待官网核对」。 */
  _needsVerify?: boolean;
  /** 预设来源链接（官网定价页），编辑器内可点开核对。 */
  _sourceUrl?: string;
};

/** lookup_model_preset 返回视图：命中的预设价 + 展示元信息（null＝无预设）。 */
export type PresetView = {
  pricing: ModelPricing;
  displayName: string;
  confidence: string;
  needsVerify: boolean;
  sourceUrl: string;
};

// ── 官网单价采集（0.0.73）────────────────────────────────────────────────────

/**
 * lookup_effective_pricing 返回视图：当前真正生效的价（采集覆盖优先、编译快照兜底）。
 * 显示侧预设回退 + 加模型自动填都用它（与后端结算口径一致）。
 * source='override'＝采集价（实时官网价，needsVerify 恒 false、confidence 缺）；
 * source='preset'＝编译快照（沿用条目 confidence/needsVerify/sourceUrl）。
 */
export type EffectivePricingView = {
  pricing: ModelPricing;
  /** 'override'＝采集覆盖层｜'preset'＝编译快照。 */
  source: "override" | "preset";
  /** 仅 preset 来源有值（编译条目置信度）；override 来源缺。 */
  confidence?: string;
  /** override 来源恒 false；preset 来源沿用条目 needs_verify。 */
  needsVerify: boolean;
  /** 来源链接（官网定价页）；可空。 */
  sourceUrl?: string;
  /** override 来源为采集时间戳（毫秒前的秒级时间戳，可空）；preset 来源缺。 */
  fetchedAt?: number;
};

/**
 * 单条 diff：一个抽到的模型与「现价」的比对结果（capture_official_pricing 返回的一行）。
 * change='new'＝现价不存在（新模型）｜'changed'＝价格有变｜'unchanged'＝完全一致。
 */
export type PricingDiff = {
  /** 真实 API 模型串（原样，作为 override 主键的一部分）。 */
  modelId: string;
  currency: string;
  change: "new" | "changed" | "unchanged";
  /** 现价（override 或编译快照解析得到）；new 时缺。 */
  oldPricing?: ModelPricing;
  /** 官网抽到的新价。 */
  newPricing: ModelPricing;
};

/**
 * capture_official_pricing 的统一返回（不写库；前端据此渲染 diff 勾选表）。
 * supported=false → 该平台不支持自动采集（显 message）；ok=false → 抓取/抽取/校验失败（显 error）；
 * ok=true → 展开 diff 面板（diffs 逐模型）。
 */
export type CaptureResult = {
  /** 该 preset 是否支持自动采集。 */
  supported: boolean;
  /** 抓取+抽取+校验整体是否成功（supported=false 时无意义，恒 false）。 */
  ok: boolean;
  /** 失败原因（人话）；成功时缺。 */
  error?: string;
  /** supported=false 时的提示文案。 */
  message?: string;
  /** 采集源 url（成功时回填，供 apply 写 source_url）。 */
  sourceUrl?: string;
  /** 抓取/抽取时间戳（秒级，成功时回填）。 */
  fetchedAt?: number;
  /** 页面过大被截断：抽取可能漏采部分模型，前端需显式警示而非以「采集成功」无差别呈现。 */
  truncated: boolean;
  /** 逐模型 diff（changed/new/unchanged 都在；unchanged 由前端默认不勾且置灰）。 */
  diffs: PricingDiff[];
};

/** apply_pricing_overrides 的单条入参：前端把勾选行的 newPricing 序列化回传。 */
export type ApplyItem = {
  /** 真实 API 模型串（原样写入 override 主键）。 */
  modelId: string;
  currency: string;
  /** newPricing 序列化后的 JSON 串（原样存进 override 的 pricing_json）。 */
  pricingJson: string;
  /** 采集源 url（可空；前端从 CaptureResult.sourceUrl 带回）。 */
  sourceUrl?: string;
};

/** subscriptionJson 自由结构：套餐名 + 月费 + 月额度 token（均可空）。 */
export type SubscriptionInfo = {
  planLabel?: string;
  monthlyFee?: number;
  currency?: PricingCurrency;
  monthlyQuotaTokens?: number;
};

/** get_connection_monthly_usage 返回：本月该连接累计用量。 */
export type ConnectionMonthlyUsage = {
  promptTokens: number;
  completionTokens: number;
  totalTokens: number;
};

/** 用户在某连接下登记的一个模型（list_models / list_models_for_connection / add_model / update_model 返回）。
 *  这是「加载模型」库的一行：一个连接可登记多个模型（一对多）。角色分配从这些 id 里选。 */
export type CuratedModelView = {
  /** 模型记录 id（models.id）；角色分配的 modelRef 引用它。 */
  id: string;
  /** 所属连接 id。 */
  connectionId: string;
  /** 所属连接的展示名（label 优先，否则 preset，再否则连接 id）；连接已删时缺。 */
  connectionLabel?: string;
  /** 实际 API 模型串（如 deepseek-chat）。 */
  modelId: string;
  /** 可选展示名/别名。 */
  label?: string;
  /** 可选上下文窗口（tokens）。 */
  contextWindow?: number;
  /** 0.0.72 计价：该模型单价的 pricing_json（StoredPricing 序列化）；缺＝未填单价。 */
  pricingJson?: string;
};

/** 一个角色当前的「分配」概览（get_role_assignments 返回）。无密钥。
 *  modelRef/modelId/... 为该角色**自身**引用的已登记模型（缺＝跟随主模型）；effective 为回退后的实际生效。 */
export type RoleAssignmentView = {
  /** main|action|plan|critique|vision|subagent|embed。 */
  role: string;
  /** 该角色自身引用的已登记模型 id（models.id；None/缺＝跟随主模型）。 */
  modelRef?: string;
  /** 自身引用模型的实际 API 模型串。 */
  modelId?: string;
  /** 自身引用模型的展示名（label）。 */
  modelLabel?: string;
  /** 自身引用模型所属连接的展示名（便于直接渲染）。 */
  connectionLabel?: string;
  /** 自身引用模型的上下文窗口（tokens，可选）。 */
  contextWindow?: number;
  /** 自身引用是否启用（无自身引用则 false）。 */
  enabled: boolean;
  /** 实际生效（经回退主模型后）：连接名 + 模型 + 来源。 */
  effective: {
    connectionLabel?: string;
    modelId?: string;
    /** 'self'＝用了角色自身引用｜'main'＝回退主模型｜'none'＝主模型也没配。 */
    source: "self" | "main" | "none";
  };
};

/** 全部可分配角色及其中文展示名（分配设置页用）。顺序与后端 ALL_ROLES 一致。 */
export const ASSIGNABLE_ROLES: Array<{ id: string; label: string; desc: string }> = [
  { id: "main", label: "主模型（Main）", desc: "默认模型；其它未单独分配的角色都跟随它" },
  { id: "action", label: "行动（Action）", desc: "执行工具的常规循环用此模型" },
  { id: "plan", label: "计划（Plan）", desc: "计划模式 / 规划步骤用此模型" },
  { id: "critique", label: "评审（Critique）", desc: "审查 / 批评步骤用此模型" },
  { id: "vision", label: "视觉（Vision）", desc: "识图模型；未分配＝不开放图像导入" },
  { id: "subagent", label: "子代理（Subagent）", desc: "并行子代理用此模型（未配回退行动→主模型）" },
  { id: "embed", label: "嵌入（Embed）", desc: "code_search 语义检索的 embedding 模型" },
];

export const IMAGE_EXTENSIONS = ["png", "jpg", "jpeg", "gif", "webp"];

/** 设置弹窗的分类标识；提到顶层以便首屏 CTA 指定初始分类（Plan19 P0a）。
 *  0.0.59：用 "connections"（模型连接库）+ "assignments"（角色→连接/模型分配）替换
 *  旧的 "provider"（按角色重填 key 的供应商表单）与 "routing"（角色路由卡片）。 */
export type SettingsSection =
  | "account"
  | "connections"
  | "assignments"
  | "lsp"
  | "permission"
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

/** 斜杠命令清单：输入框以 / 开头时弹出 */
export const SLASH_COMMANDS: Array<{ cmd: string; desc: string }> = [
  { cmd: "/compact", desc: "把当前会话历史压缩为摘要，释放上下文" },
  { cmd: "/clear", desc: "开启一个全新会话" },
  { cmd: "/init", desc: "让 Agent 分析项目并生成 MDGA.md 长期记忆" },
  { cmd: "/rewind", desc: "打开文件变更记录，可回退改动" },
  { cmd: "/model", desc: "打开 设置 → 模型分配，修改主模型" },
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
  | { kind: "part"; part: TextPart | NoticePart | ImagePart | VisionPart | ReasoningPart | ArtifactPart; index: number }
  | { kind: "tools"; parts: ToolPart[]; index: number };
