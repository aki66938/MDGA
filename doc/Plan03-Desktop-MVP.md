# Plan03 - MDGA Desktop MVP

项目代号：MDGA  
文档定位：本文件定义 MDGA 第一版 Windows 桌面 MVP 的产品范围、开发阶段、验收标准和暂缓事项。它承接 `Plan01-MDGA-Core-Development-Roadmap.md` 与 `Plan02-Technical-Architecture.md`，不替代后续 `Plan04-Plan10` 与 `Spec01-Spec07` 的细节设计。

---

## 1. MVP 目标

MDGA Desktop MVP 的目标是验证一个最小但真实的闭环：

用户可以在 Windows 上安装 MDGA，配置 `DEEPSEEK_API_KEY` 环境变量，打开桌面应用，完成基础聊天、查看本地会话历史、绑定项目目录、看到 token 消耗，并让 Agent 在受控权限下完成一次轻量本地任务。

MVP 不追求功能完整，而追求主链路成立：

- 能启动。
- 能连接 DeepSeek API。
- 能稳定聊天。
- 能保存本地会话。
- 能展示 token 用量。
- 能选择项目与工作区。
- 能在用户授权下执行最小本地工具动作。
- 能把 Agent 工作过程以摘要形式展示出来。
- 能保留必要审计记录。

---

## 2. MVP 用户画像

第一版面向两类用户：

- 普通用户：希望低成本使用 DeepSeek 完成聊天、资料整理、文档总结和轻量自动化。
- 技术用户：愿意配置环境变量，理解本地文件权限，并希望 Agent 能在明确授权下操作项目目录。

MVP 的默认体验应照顾普通用户，但不能压制技术用户的能力边界。权限模式应允许技术用户主动选择更开放的执行能力，例如 Full Access，但所有高权限行为仍需要可见、可审计、可追踪。

---

## 3. MVP 非目标

第一版明确不做：

- 不做 macOS / Linux 发布包。
- 不做移动端。
- 不做云账号系统。
- 不做云同步。
- 不做多模型 Provider。
- 不做 OpenAI、Claude、Gemini、OpenRouter 接入。
- 不在应用内输入、保存或托管 API Key。
- 不做插件市场。
- 不做完整 MCP 治理。
- 不做完整本地知识库。
- 不做企业管理后台。
- 不做完整跨平台沙箱。
- 不做复杂 Git worktree 工作流。
- 不做移动端 Agent。

这些能力可以在后续计划中继续拆分，但不能进入 Windows MVP 的关键路径。

---

## 4. MVP 成功标准

MVP 完成时，应能通过以下验收：

1. 用户安装并启动 Windows 桌面应用。
2. 应用检测 `DEEPSEEK_API_KEY` 环境变量。
3. 环境变量缺失时，应用给出 Windows 配置指引。
4. 环境变量存在时，应用能完成 DeepSeek 连接测试。
5. 用户可以创建一次新对话。
6. 用户可以发送消息并收到 DeepSeek 流式回复。
7. 用户可以看到本次请求和当前会话累计 token 用量。
8. 用户可以在左侧看到本地会话历史。
9. 用户可以自动生成会话标题，并手动重命名。
10. 用户可以添加或选择一个本地项目目录。
11. 用户可以看到当前会话绑定的项目和工作区。
12. 用户可以选择权限模式。
13. Agent 可以在授权工作区内完成一次轻量文件任务，例如创建一个 Markdown 文件。
14. 文件变更会产生结构化 Activity Event。
15. UI 默认折叠显示 Agent 工作过程摘要。
16. 用户可以展开查看命令、文件变更或工具调用详情。
17. token 账本、本地会话、任务事件和文件变更记录写入本地存储。
18. 应用关闭后再次打开，历史会话与 token 记录仍可查看。

---

## 5. 技术主线

MVP 技术主线固定为：

- Tauri 2。
- Windows 桌面优先。
- Rust-first backend。
- WebView 前端。
- DeepSeek API only。
- SQLite 本地存储。
- `DEEPSEEK_API_KEY` 环境变量认证。
- 本地文件系统与命令执行通过 Rust backend 统一调度。

工程原则继承 Plan01 / Plan02：

- 遵循 DRY、SOLID、KISS。
- 坚持高内聚、低耦合。
- 源代码目录必须反映架构边界。
- 不允许无边界的 `utils`、`common`、`core` 杂物目录。
- Rust 公共函数、复杂私有函数和跨模块入口需要中文说明注释，描述用途、输入、输出和关键副作用。
- UI 不直接调用裸系统能力，只调用产品动作。
- 高权限动作必须经过 Permission Manager。

---

## 6. MVP 源码目录建议

第一版建议采用以下目录：

```text
apps/
  desktop/
    src-tauri/
    src/
packages/
  ui/
crates/
  shared/
  deepseek-client/
  token-accounting/
  storage/
  agent-core/
  tool-runtime/
  sandbox-runtime/
docs/
tests/
```

目录职责：

- `apps/desktop`：Tauri 桌面应用入口、窗口、IPC 注册、前端集成。
- `packages/ui`：可复用 UI 组件、布局、状态展示组件。
- `crates/shared`：DTO、错误类型、事件类型、权限枚举和跨 crate 协议类型。
- `crates/deepseek-client`：DeepSeek API、Bearer Auth、流式输出、Tool Calls、usage 解析和错误处理。
- `crates/token-accounting`：usage 标准化、费用估算、账本聚合和导出基础能力。
- `crates/storage`：SQLite schema、migration、repository、本地数据读写。
- `crates/agent-core`：Conversation、Task、Planner、Executor、Context 和任务状态机。
- `crates/tool-runtime`：工具注册、schema 校验、工具执行编排和结构化结果。
- `crates/sandbox-runtime`：Windows-first 本地进程级隔离预研与最小执行边界。

MVP 阶段可以让部分 crate 先保持薄实现，但目录边界不应随意合并。

---

## 7. 桌面界面范围

MVP 需要以下界面：

### 7.1 首次启动 / 设置入口

需要展示：

- `DEEPSEEK_API_KEY` 是否已配置。
- 连接测试按钮。
- Windows 环境变量配置指引。
- 当前应用数据目录。
- 当前版本号。

不展示：

- API Key 输入框。
- API Key 明文。
- 多模型选择器。
- Provider 配置页面。

### 7.2 主对话页

需要展示：

- 当前会话标题。
- 消息列表。
- 输入框。
- 发送按钮。
- 当前 DeepSeek 模型状态。
- 当前权限模式。
- 当前 token 用量摘要。
- 当前项目 / 工作区状态。

第一版不要求复杂富文本编辑器，但必须支持多行输入、流式输出、中断当前回复和错误重试。

#### 7.2.1 消息渲染策略（已实现）

- **用户消息**：纯文本气泡，右对齐，深色背景。输入内容按字面展示，不解析 Markdown，避免用户输入的符号被误渲染。
- **Assistant 消息**：使用 `react-markdown` + `remark-gfm` 渲染为 HTML，支持：标题、粗体、斜体、有序/无序列表、行内代码、代码块、引用块、表格、分隔线。无气泡背景，内容直接融入页面底色，为后续富内容扩展（代码高亮、工具调用展示、文件 diff）预留结构。
- **Token 统计行**：独立于 assistant 消息内容区之外，渲染为页面级弱化文字，不属于任何气泡。
- **Markdown 嵌套说明**：AI 回复的 Markdown 只做一次渲染，代码块内的 Markdown 语法不被解析。如 AI 被要求"输出 Markdown 示例"，示例内容应在代码块内返回，renderer 会正确以字面文本展示。此为工程约束，非模型限制。

### 7.3 左侧导航

需要展示：

- 新对话。
- 搜索。
- 项目列表。
- 当前项目下的最近会话。
- 无项目会话入口。
- 设置入口。

MVP 可以先实现基础列表，不要求完整拖拽、标签、文件夹或多维筛选。

### 7.4 新对话页

需要展示：

- 空状态提问引导。
- 当前项目 / 工作区选择。
- 权限模式入口。
- 可选快捷任务卡片。

快捷任务卡片只做入口，不需要复杂模板市场。

### 7.5 Agent 工作过程展示

MVP 需要最小 Activity Event 展示：

- 默认折叠。
- 灰度弱化。
- 展示摘要，例如“已运行 1 条命令”“已编辑 1 个文件”。
- 可展开查看详情。
- 文件变更展示文件名和增删行数量。
- 命令执行展示命令摘要、耗时和退出码。

完整交互细节放入 `Spec03-Agent-Activity-Visibility.md`，Plan03 只要求 MVP 有结构化事件和最小折叠展示。

---

## 8. 本地数据范围

MVP 至少保存：

- conversation。
- message。
- project。
- workspace。
- task。
- activity_event。
- tool_call。
- token_usage_record。
- permission_grant。
- change_set。

API Key 不保存。

本地数据原则：

- 默认只保存在本机。
- 默认不云同步。
- 诊断信息默认脱敏。
- token usage 原始字段需要保留。
- 用户应能看到数据目录。
- 删除、导出和备份可先进入后续 Spec，不阻塞 MVP 主链路。

---

## 9. DeepSeek 接入范围

MVP 只支持 DeepSeek API。

第一版需要：

- 从 `DEEPSEEK_API_KEY` 读取 API Key。
- Bearer Auth。
- 连接测试。
- 普通聊天。
- 流式输出。
- 流式 usage 返回。
- Tool Calls 基础解析。
- JSON Output 基础支持。
- 错误码分类。
- 限流错误提示。
- usage 原始字段保存。

第一版不做：

- 应用内 API Key 输入。
- 应用内 API Key 保存。
- 多 Provider 抽象。
- 第三方 coding plan 登录。
- 代充值或统一计费。

如果 DeepSeek 后续开放官方 coding plan 或独立编码产品 API，应作为新议题进入 `Plan07-DeepSeek-Client.md`，不影响 MVP。

---

## 10. Token Accounting 范围

Token Accounting 必须进入 MVP 早期。

第一版需要记录：

- request_id。
- api_source，固定为 `deepseek`。
- model。
- conversation_id。
- task_id。
- step_id。
- prompt_tokens。
- completion_tokens。
- total_tokens。
- prompt_cache_hit_tokens。
- prompt_cache_miss_tokens。
- reasoning_tokens。
- usage_source。
- estimated_cost。
- pricing_version。
- created_at。

展示范围：

- 单次请求 token。
- 当前会话累计 token。
- 当前任务累计 token。
- 是否包含缓存命中 / 未命中字段。
- usage 是否来自服务端返回。

复杂账单对照、导出格式和价格版本回放放入 `Plan04-Token-Accounting.md`。

---

## 11. 权限与沙箱 MVP 范围

MVP 权限模式保留四档：

- Restricted。
- Ask Every Time。
- Workspace Auto。
- Full Access。

MVP 最小行为：

- 默认 Restricted。
- 用户可以切换权限模式。
- 当前权限模式在主界面明确显示。
- 文件写入、命令执行、联网等高风险动作进入 Permission Manager。
- Ask Every Time 下每次高风险动作请求确认。
- Workspace Auto 下工作区内允许自动执行，越界请求确认。
- Full Access 允许技术用户主动放宽限制，但仍记录审计日志。

沙箱第一版目标：

- 不依赖 Docker / VM。
- 优先做 Windows 本地进程级隔离预研。
- 在正式沙箱未完善前，不允许悄悄降级为无限制执行。
- 如果某类隔离能力尚未实现，UI 和日志必须明确标注能力状态。

完整沙箱技术路线放入 `Plan09-Sandbox-Runtime.md`。

---

## 12. Agent MVP 范围

MVP 不需要完整高级 Agent，但需要最小任务闭环：

1. 用户输入一个本地任务。
2. Agent 创建 Task。
3. Agent 生成 1-3 个执行步骤。
4. Tool Runtime 校验工具调用。
5. Permission Manager 判断权限。
6. 用户确认高风险动作。
7. 工具在授权范围内执行。
8. 生成 Activity Event。
9. 生成 Change Set。
10. UI 展示折叠摘要。
11. 结果回到主对话。
12. token usage 写入账本。

MVP 推荐首个 Agent 场景：

- 在当前工作区创建一个 Markdown 文件。
- 或读取当前工作区的一个 Markdown 文件并总结。
- 或整理一个小型文件夹并输出清单。

不建议第一版就做复杂代码重构、多文件工程改造或长期自动任务。

---

## 13. 开发阶段

### Phase 1 - 桌面壳与项目骨架

目标：

- 建立 Tauri 2 Windows 桌面应用。
- 建立 Rust workspace。
- 建立前端基础布局。
- 建立 IPC 基础协议。
- 建立日志与错误展示基础。

验收：

- Windows 上可以启动桌面窗口。
- 前端能调用一个 Rust command。
- Rust backend 能发送一个事件给前端。
- 项目目录符合 Plan02 边界。

### Phase 2 - DeepSeek 连接与基础聊天 ✅ 已实现

目标：

- 实现 `deepseek-client`。
- 读取 `DEEPSEEK_API_KEY`。
- 完成连接测试。
- 完成普通聊天和流式输出。
- Assistant 回复以 Markdown 渲染展示（`react-markdown` + `remark-gfm`）。

验收：

- 环境变量缺失时给出明确提示。✅
- 环境变量存在时可以完成连接测试。✅
- 用户可以发送消息并看到流式回复。✅
- DeepSeek 错误能被分类展示。✅（错误分类已在 `deepseek-client` 实现，UI 展示待 Phase 3 错误 UI 完善）
- Assistant 回复 Markdown 正确渲染，代码块、列表、粗体等格式生效。✅

### Phase 3 - 本地存储与会话历史 ✅ 已实现

目标：

- 建立 SQLite schema。
- 保存 conversation / message。
- 实现会话列表。
- 支持自动标题和手动重命名。

验收：

- 应用重启后历史会话仍存在。✅
- 用户可以创建、切换、重命名会话。✅（新建、切换、删除已实现；手动重命名待 UI 补充）
- 会话可以无项目存在。✅

### Phase 4 - Token Accounting MVP ✅ 已实现（持久化展示层）

目标：

- 保存服务端 usage。
- 标准化 token 字段。
- 展示单次和会话累计 token。

验收：

- 完成一次聊天后可以看到 token usage。✅
- 如果返回 cache hit / miss，可以分别展示。✅
- 如果 usage 缺失，明确标注估算或未知。✅
- 当前会话可基于 SQLite 历史 usage 聚合显示累计 token 与费用。✅

> 注：当前 assistant 单次 usage 已随消息写入 SQLite；前端在加载历史消息后基于 `usageJson` 聚合当前会话累计 token 与估算费用。token 统计行已从 assistant 气泡中独立，作为页面级弱化文字显示在回复内容下方。

### Phase 5 - Project / Workspace MVP 🚧 第一阶段已实现（session 级工作区绑定）

目标：

- 添加本地项目目录。
- 绑定会话到项目。
- 定义工作区边界。
- 展示当前项目和工作区状态。

验收：

- 用户可以选择项目目录。
- 新对话首屏可以通过系统目录选择器选择工作区。✅
- 发送首条消息时，conversation 会保存创建时的 workspace snapshot。✅
- 未选择工作区时可以创建 `chat_only` 纯聊天会话。✅
- 普通对话正文不再显示工作区路径输入表单。✅
- 用户可以手动输入本地目录路径并绑定为当前活动工作区。⚠️ `0.0.5` 技术切片，已被 session 级选择取代。
- 应用重启后可以从 SQLite 读取当前活动工作区。⚠️ `0.0.5` 技术切片，后续应迁移为最近项目 / 最近工作区。
- 新对话可以绑定项目。
- 工作区不可访问时给出降级提示。

> 修正：工作区选择应发生在新对话 / 新 session 首屏，通过系统目录选择器完成；普通对话正文不应显示工作区绑定表单。单次选择后，整轮对话默认局限于该工作区内。`0.0.6` 已完成第一阶段：conversation 级 workspace snapshot、Tauri 系统目录选择器、首屏选择 UI 与基础测试。下一步需要将 Agent 文件工具和命令执行默认 cwd 接到该 conversation workspace，并实现越界审批。

### Phase 6 - Permission Manager MVP

目标：

- 实现四档权限模式。
- 实现高风险动作审批流。
- 记录 permission_grant。

验收：

- Restricted 阻止高风险动作。
- Ask Every Time 每次请求确认。
- Workspace Auto 在工作区内自动执行，越界请求确认。
- Full Access 有明显状态提示和审计记录。

### Phase 7 - Tool Runtime 与最小 Agent 任务

目标：

- 实现最小 Tool Registry。
- 支持读取文件、写入文件、列目录三个基础工具。
- 完成一个轻量 Agent 任务闭环。

验收：

- Agent 可以在授权工作区内创建 Markdown 文件。
- 越界写入会被拦截或请求确认。
- 工具调用结果结构化返回。

### Phase 8 - Activity Event 展示

目标：

- 生成结构化 Activity Event。
- UI 默认折叠展示工具动作。
- 支持展开查看详情。

验收：

- 命令、文件变更、工具调用不会淹没主对话。
- 用户可以展开查看必要细节。
- 完整事件写入本地存储。

### Phase 9 - Windows 打包与内部验收

目标：

- 生成 Windows 安装包。
- 完成基础更新策略预研。
- 完成内部验收清单。

验收：

- 可在干净 Windows 环境安装启动。
- 缺少环境变量时指引清晰。
- 卸载后数据保留策略明确。
- 崩溃日志不包含 API Key。

---

## 14. MVP 测试范围

单元测试：

- `deepseek-client` 请求构造与错误解析。
- `token-accounting` usage 标准化。
- `storage` repository。
- `permission-manager` 权限判断。
- `tool-runtime` schema 校验。

集成测试：

- DeepSeek 连接测试。
- 流式输出与 usage 解析。
- 会话保存与恢复。
- 项目 / 工作区绑定。
- 工具调用与权限审批。
- Activity Event 写入与读取。

端到端测试：

- 首次启动。
- 环境变量缺失。
- 环境变量存在。
- 完成一次聊天。
- 查看 token 用量。
- 创建项目。
- 发起一个最小 Agent 文件任务。
- 拒绝一次高风险权限。
- 重启后恢复会话历史。

---

## 15. MVP 风险

### 15.1 Windows 环境变量读取风险

Windows GUI 应用读取环境变量通常可行，但用户修改系统环境变量后，已启动进程不会自动刷新。MVP 需要提供“重新检测”按钮，并提示用户必要时重启应用。

### 15.2 DeepSeek API 变化风险

DeepSeek 模型、usage 字段、价格和 coding 工具集成文档可能变化。MVP 应记录 `pricing_version` 和原始 usage，避免只保存计算后的费用。

### 15.3 沙箱复杂度风险

Windows 本地进程级沙箱工程复杂，MVP 不应承诺一开始就拥有完整隔离强度。未实现的隔离能力必须在 UI 和日志中明确，不允许伪装为已隔离。

### 15.4 UI 复杂度风险

对话、项目、工作区、权限、token 和 Agent 活动都容易进入同一个界面。MVP 应优先保证主对话流清爽，把过程信息折叠到 Activity Event 中。

### 15.5 本地数据膨胀风险

命令输出、工具日志、文件片段和 token 账本可能快速增长。MVP 至少需要为日志保留周期、诊断脱敏和后续清理策略预留字段。

---

## 16. 进入 Plan04 的条件

Plan03 确认后，下一份文档应优先进入 `Plan04-Token-Accounting.md`，因为 token 账本需要在 MVP 早期完成，而不是等完整 Agent Kernel 成熟后再补。

进入 Plan04 前应确认：

- MVP 中 token 统计的展示位置。
- 是否第一版就需要导出 CSV / JSON。
- DeepSeek 当前价格表如何版本化保存。
- 流式 usage 缺失时如何标注。
- 缓存命中 / 未命中字段如何聚合。
- 账单对照以用户手工导出为主，还是预留官方账单导入能力。

---

## 17. 当前结论

MDGA Desktop MVP 的第一目标不是展示完整 Agent，而是打通 Windows 桌面真实使用闭环。

这个闭环由六件事组成：

1. Windows 桌面壳。
2. DeepSeek 环境变量接入。
3. 本地会话与项目工作区。
4. token 账本。
5. 权限模式。
6. 最小 Agent 文件任务。

只要这六件事成立，MDGA 就已经具备从普通聊天工具升级为个人 Agent 工作台的基础。后续的沙箱增强、插件、MCP、移动端、审计、变更回滚和复杂 Agent Kernel，都应在这个 MVP 闭环之上逐步加固，而不是抢在第一版之前把系统拖入过宽范围。
