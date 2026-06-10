# Plan02 - MDGA Technical Architecture

项目代号：MDGA  
文档定位：本文件是 MDGA 的技术架构总设计，承接 `Plan01-MDGA-Core-Development-Roadmap.md`。它定义系统分层、模块边界、运行模型、代码目录原则和关键数据流，不替代后续 Plan03-Plan10 与 Spec01-Spec07 的细节设计。

---

## 1. 架构目标

MDGA 的技术架构应服务于以下目标：

- PC-first：Windows MVP 优先，后续扩展 macOS / Linux，三端均以重度 Rust core 为核心。
- 本地优先：会话、项目、任务、token 账本、文件索引和产物默认保存在本地；API Key 只从环境变量读取，不进入应用数据库。
- 性能优先：桌面端启动、会话、文件处理、任务执行和本地存储应尽量轻量快速。
- 权限透明：用户可选择权限模式，高风险动作必须可见、可审计。
- 费用透明：记录每次模型请求的实际 usage、token 消耗、缓存命中情况和估算费用。
- 可恢复：任务、工具调用、文件变更、日志和产物应具备可追踪、可恢复、可诊断基础。
- 可扩展：后续支持技能、MCP、插件和移动端远程控制，但永不支持多模型 Provider，MDGA 只服务 DeepSeek API。

---

## 2. 工程原则

MDGA 的源代码组织与模块设计必须遵循以下原则：

- DRY：同一业务规则只应有一个权威实现，例如权限判断、token 费用计算、工具 schema 校验和数据脱敏。
- SOLID：核心模块应职责清晰、依赖可替换、接口可测试，避免 UI、存储、模型调用和系统权限互相缠绕。
- KISS：优先简单、可验证、可维护的设计，不为了未来插件生态或移动端提前制造复杂框架。
- 高内聚：每个 crate / package 内部围绕一个明确领域组织。
- 低耦合：模块之间通过接口、事件、DTO、repository 或 service 边界通信。
- 目录即架构：源码目录要能反映系统边界，避免无节制的 `utils`、`common`、`core` 杂物目录。
- 方法注释：每个公开函数、复杂私有函数、跨模块调用入口都必须有中文说明，描述本方法用途、主要输入、输出结果和关键副作用。Rust 中优先使用函数签名上方的 `///` 文档注释；如果方法内部存在关键流程，也应在核心代码起始处用简短中文注释说明意图。

Rust 方法注释示例：

```rust
/// 创建新的任务记录。
///
/// 输入用户请求、会话 ID 和当前权限模式，输出持久化后的任务对象。
/// 本方法只创建任务元数据，不执行模型调用或工具调用。
pub async fn create_task(input: CreateTaskInput) -> Result<Task> {
    // 校验会话、权限模式和任务输入，确保进入运行时前的数据是可信的。
    validate_task_input(&input)?;

    let task = Task::from_input(input);
    task_repository.insert(&task).await?;
    Ok(task)
}
```

每个模块都应能回答：

- 它负责什么？
- 它不负责什么？
- 它对外暴露什么接口？
- 它依赖哪些模块？
- 它如何被单元测试或集成测试验证？

---

## 3. 总体分层

MDGA 建议采用以下系统分层：

- UI Layer：桌面端界面、移动端轻量界面、任务面板、审批流、设置、权限审计、成本统计和 Agent 工作过程展示。
- IPC Layer：Tauri command / event / stream 边界，只暴露产品动作，不暴露裸系统能力。
- Agent Runtime Layer：Conversation、Task、Planner、Executor、任务状态机、暂停、恢复、取消和失败重试。
- Context Layer：上下文组装、会话摘要、文件片段、工具历史、用户偏好、权限边界和成本预算。
- DeepSeek Client Layer：唯一模型接入层，负责 DeepSeek API、Bearer Auth、流式输出、Tool Calls、JSON Output、usage 解析和错误处理。
- Tool Runtime Layer：工具注册、schema、参数校验、执行编排、结果结构化和工具错误。
- Permission & Sandbox Layer：权限模式、审批、沙箱策略、本地进程级隔离、网络边界和越权处理。
- Storage Layer：SQLite、文件缓存、产物目录、配置、索引、密钥引用和迁移。
- Token Accounting Layer：usage 原始字段、标准化 token、费用估算、账单对照和导出。
- Observability Layer：Activity Event、任务时间线、命令记录、文件变更、网络访问、错误、诊断包。
- Change Manager Layer：diff、snapshot、revert、stage / commit 和后续 review 工作流。
- Extension Layer：Skill、MCP、插件治理、权限声明、签名、启用禁用和审计。
- Sync / Remote Layer：移动端聊天、任务查看、审批推送、通知同步和桌面在线状态。

第一版不要求所有层完整实现，但不能绕过这些边界。

---

## 4. 进程与运行模型

第一版运行模型：

- Tauri desktop app 承载 Windows 桌面窗口。
- WebView 前端负责 UI、输入、展示、折叠、审阅和设置。
- Rust backend 承载核心业务逻辑、模型调用、存储、权限判断和任务调度。
- 工具调用和命令执行由 Rust backend 统一调度。
- 高风险工具通过 Permission Manager 判断是否允许执行。
- 需要执行命令或脚本时，由 Sandbox Runtime 启动受限子进程。
- 所有重要动作生成 Activity Event，并写入 Observability / Storage。

长期运行模型：

- Windows / macOS / Linux 均保留重度 Rust core。
- 移动端不实现系统原生级 Agent，只通过 Sync / Remote Layer 与桌面端通信。
- 插件、MCP 和 Skill 必须进入同一套权限、沙箱、审计和 token 统计体系。

---

## 5. Rust Crate 边界

建议第一阶段采用中等粒度 crate 拆分。

### apps/desktop

职责：

- Tauri 桌面应用入口。
- WebView 集成。
- Tauri command / event 注册。
- 桌面窗口、系统托盘、菜单、设置入口。

不负责：

- 不直接实现 Agent 业务逻辑。
- 不直接读写敏感数据。
- 不直接执行命令。

### crates/shared

职责：

- 稳定 DTO。
- 错误类型。
- ID 类型。
- 事件类型。
- 权限模式枚举。
- 跨 crate 协议类型。

约束：

- 只放稳定共享类型，不放业务实现。
- 避免演变成杂物箱。

### crates/deepseek-client

职责：

- DeepSeek API Client。
- 从 `DEEPSEEK_API_KEY` 环境变量读取 API Key。
- Bearer Auth 请求构造。
- 流式输出。
- Tool Calls / JSON Output。
- usage 原始字段解析。
- 错误码和限流处理。
- DeepSeek 官方编码工具 / coding plan 状态调研记录。

不负责：

- 不计算最终账单。
- 不决定任务状态。
- 不处理 UI 展示。
- 不支持 OpenAI、Claude、Gemini、OpenRouter 或其他 Provider。
- 不在数据库、配置文件或 OS Keychain 中保存 API Key。

### crates/token-accounting

职责：

- 标准化 usage。
- 记录 prompt / completion / total tokens。
- 记录 cache hit / cache miss tokens。
- 记录 reasoning tokens。
- 估算费用。
- 保存 pricing_version。
- 支持账单对照和导出。

优先级：

- 提前于完整 Agent Kernel 实现。
- 必须在桌面 MVP 早期可测试。

### crates/storage

职责：

- SQLite schema。
- repository / migration。
- 会话、项目、任务、事件、token 账本和配置持久化。
- 本地文件缓存和产物索引。

不负责：

- 不包含业务决策。
- 不直接调用模型。

### crates/agent-core

职责：

- Conversation Engine。
- Task 状态机。
- Planner / Executor。
- Context Manager。
- Task Resume / Cancel / Retry。

不负责：

- 不直接绕过 Tool Runtime 访问系统。
- 不直接写 UI。
- 不直接持久化底层数据库细节。

### crates/tool-runtime

职责：

- Tool Registry。
- Tool schema。
- 参数校验。
- 工具执行编排。
- 工具结果结构化。
- 工具错误分类。

不负责：

- 不自行决定越权执行。
- 不直接绕过 Permission Manager 或 Sandbox Runtime。

### crates/sandbox-runtime

职责：

- Windows 本地进程级沙箱预研与实现。
- 后续 macOS Seatbelt、Linux / WSL2 bubblewrap 适配。
- 文件系统边界。
- 网络边界。
- 子进程继承限制。

不负责：

- 不决定产品权限策略。
- 不做 UI 审批。

### Change Manager 边界

第一阶段可以先作为 `tool-runtime` 或 `agent-core` 内的清晰子模块存在；当 diff、snapshot、revert、stage / commit 和 review 工作流复杂后，再独立为 `crates/change-manager`。

---

## 6. Tauri IPC 边界

Tauri IPC 的核心原则是：前端调用产品动作，不调用裸系统能力。

允许前端调用的动作示例：

- `create_conversation`
- `rename_conversation`
- `select_project`
- `create_task`
- `approve_action`
- `cancel_task`
- `export_artifact`
- `open_review`
- `get_token_summary`

不应直接暴露给前端的能力：

- `write_file`
- `delete_file`
- `run_command`
- `read_secret`
- `spawn_process`
- `open_network_socket`
- `start_sandbox`

所有高风险动作必须经过：

1. IPC schema 校验。
2. Permission Manager。
3. 当前权限模式判断。
4. 必要时生成审批请求。
5. Sandbox policy 生成。
6. Tool Runtime / Sandbox Runtime 执行。
7. Activity Event 和审计记录写入。

Full Access 只放宽后端执行策略，不允许 WebView 前端绕过后端权限网关。

---

## 6.1 DeepSeek API Key 与接入策略

MDGA 只为 DeepSeek API 服务，不做通用 Provider 平台。

接入原则：

- 唯一 API Key 来源是环境变量 `DEEPSEEK_API_KEY`。
- 应用不提供内置 API Key 输入框。
- 应用不把 API Key 写入 SQLite、配置文件、日志、诊断包或 OS Keychain。
- 应用启动和连接测试时读取当前进程环境变量。
- 前端只接收脱敏状态，例如 `configured`、`missing`、`connection_failed`。
- 工具子进程默认不继承 `DEEPSEEK_API_KEY`，避免命令执行和 Full Access 场景泄露 Key。

平台配置指引：

- Windows：用户在系统或用户环境变量中添加 `DEEPSEEK_API_KEY`。
- macOS：用户在 `~/.zshrc` 中写入 `export DEEPSEEK_API_KEY=...`。需要注意 GUI 应用不一定继承 shell 环境变量，后续 Windows MVP 之后再单独验证 macOS 桌面启动链路。
- Linux：用户在 `~/.bashrc` 或当前 shell 对应配置文件中写入 `export DEEPSEEK_API_KEY=...`。

DeepSeek coding plan 状态：

- 当前架构只接入 DeepSeek API。
- 当前调研到的 DeepSeek 官方资料主要是 API 认证、Chat / Tool Calls / JSON Output / Context Caching，以及接入 Claude Code、OpenCode、OpenClaw、Deep Code 等编码工具的集成文档。
- 未将第三方编码工具集成视为 MDGA 的登录、订阅或计费入口。
- 如果 DeepSeek 未来开放官方 coding plan 或独立编码产品 API，需要作为新议题重新评估；在没有官方入口前，Plan02 不为其设计实现分支。

参考入口：

- [DeepSeek API Authentication](https://api-docs.deepseek.com/api/deepseek-api)
- [DeepSeek Integrate with AI Tools](https://api-docs.deepseek.com/guides/coding_agents)
- [DeepSeek Integrate with Deep Code](https://api-docs.deepseek.com/quick_start/agent_integrations/deepcode)

---

## 7. 核心数据对象

Plan02 先定义对象关系，具体字段在后续 Spec 中展开。

- Project：用户添加的本地项目目录或逻辑项目。
- Conversation / Thread：一次持续对话，可绑定 Project / Workspace。
- Workspace：Agent 实际读写和执行任务的边界。
- Task：Conversation 中的一次明确目标。
- Step：Task 内的一步执行。
- Tool Call：一次工具调用请求。
- Observation：工具调用或命令执行返回结果。
- Activity Event：用户可见、可折叠、可审计的过程事件。
- Artifact：任务生成或修改的产物。
- Change Set：一次或多次文件变更集合。
- Token Usage Record：一次模型请求的 token 账本记录。
- Permission Grant：一次权限授权或权限模式状态。

关键关系：

- 一个 Project 可包含多个 Conversation。
- 一个 Conversation 可包含多个 Task。
- 一个 Task 可包含多个 Step。
- 一个 Step 可产生 Tool Call、Observation、Activity Event、Token Usage Record 和 Change Set。
- Conversation 历史应与 Workspace 解耦，即使 Workspace 丢失也保留历史。

---

## 8. 核心数据流

### 8.1 普通聊天流

1. 用户在 UI 输入消息。
2. UI 通过 IPC 创建 message。
3. Conversation Engine 组装上下文。
4. DeepSeek Client 调用 DeepSeek API。
5. 流式内容返回 UI。
6. usage 进入 Token Accounting。
7. 会话、消息、token 记录写入 Storage。

### 8.2 Agent 任务流

1. 用户创建任务。
2. Agent Runtime 创建 Task。
3. Context Manager 组装任务上下文。
4. Planner 生成步骤。
5. Executor 逐步执行。
6. Tool Runtime 校验工具调用。
7. Permission Manager 判断权限。
8. Sandbox Runtime 执行命令或文件操作。
9. Observation 返回 Agent Runtime。
10. Activity Event 写入 Observability。
11. Token Accounting 记录模型用量。
12. Change Manager 记录文件变更。
13. UI 展示摘要、折叠详情、审批请求或最终产物。

### 8.3 文件变更流

1. Tool Runtime 请求写入或修改文件。
2. Permission Manager 判断是否允许。
3. Sandbox Runtime 在允许边界内执行。
4. Change Manager 生成 Change Set。
5. Observability 记录文件变更事件。
6. UI 展示文件名、增删行、diff 入口和可回滚状态。

### 8.4 Token 账本流

1. DeepSeek Client 返回 usage 原始字段。
2. Token Accounting 标准化字段。
3. Cost Tracker 根据 pricing_version 计算费用。
4. Storage 保存请求级 token 记录。
5. UI 展示请求级、会话级、任务级聚合。
6. 用户可导出账本与官方账单对照。

---

## 9. 权限、沙箱与审计

权限模式：

- Restricted。
- Ask Every Time。
- Workspace Auto。
- Full Access。

架构原则：

- 权限模式由用户选择。
- Permission Manager 决定动作是否允许、需要确认或拒绝。
- Sandbox Runtime 提供 OS 级执行边界。
- Full Access 允许高级用户获得完整本地执行能力，但仍保留审计和状态提示。
- 折叠展示不影响审计完整性。

每次高风险动作至少应记录：

- action_id。
- conversation_id。
- task_id。
- step_id。
- permission_mode。
- requested_capability。
- target_path 或 target_resource。
- decision。
- user_approval。
- executed_at。
- result。

---

## 10. 本地数据与治理

本地数据包括：

- 会话。
- 消息。
- 项目。
- 工作区引用。
- 任务。
- Activity Event。
- token 账本。
- 工具日志。
- 文件索引。
- 产物索引。
- 设置。
- 权限授权记录。

治理原则：

- 默认本地保存。
- 默认不云同步。
- 可导出。
- 可删除。
- 可备份与恢复。
- 诊断包默认脱敏。
- 日志有保留周期。

---

## 11. MVP 架构边界

MVP 必须形成的骨架：

- Tauri desktop shell。
- Rust backend。
- DeepSeek Client。
- 本地 SQLite storage。
- Conversation / Project / Workspace 基础模型。
- Token Accounting。
- Permission Manager。
- Tool Registry / Tool Runtime 最小实现。
- Activity Event 最小实现。
- Windows-first 安装与更新基础。

MVP 可暂缓：

- 完整插件市场。
- 完整 MCP 治理。
- 完整移动端。
- 完整 worktree 实现。
- 完整跨平台沙箱。
- 云端同步。
- 企业管理能力。

---

## 12. 后续文档映射

- `Plan03-Desktop-MVP.md`：把本架构落到第一版桌面应用范围、页面和任务闭环。
- `Plan04-Token-Accounting.md`：细化 token 账本、费用计算、账单对照和导出。
- `Plan05-Agent-Kernel.md`：细化 Agent Runtime、Planner、Executor、Context 和任务状态机。
- `Plan06-Security-And-Permissions.md`：细化权限模式、审批、审计、数据安全和高风险动作策略。
- `Plan07-DeepSeek-Client.md`：细化 DeepSeek API 接入、环境变量认证、流式输出、Tool Calls、usage 和错误处理。
- `Plan08-Mobile-Adaptation.md`：细化移动端聊天和桌面 Agent 远程控制。
- `Plan09-Sandbox-Runtime.md`：细化 Windows 本地进程级沙箱与跨平台扩展路线。
- `Plan10-System-Protocols.md`：细化跨层协议、事件 schema、IPC schema 和系统状态协议。
- `Plan10-System-Protocols.md`：细化跨层协议、事件 schema、IPC schema 和系统状态协议。

---

## 13. 当前结论

MDGA 的技术架构应采用 PC-first Rust core + Tauri 2。前端负责体验和展示，Rust backend 负责模型、任务、权限、工具、存储、token 账本、沙箱和审计。

架构设计必须遵循 DRY、SOLID、KISS，以及高内聚、低耦合原则。目录结构要清晰表达职责边界，避免无边界的巨型 core 或杂物模块。

第一版不追求所有能力完整，但必须把核心边界立住：Conversation / Project / Workspace、Agent Runtime、Tool Runtime、Permission & Sandbox、Storage、Token Accounting、Observability。这些边界一旦打稳，后续技能、MCP、插件、移动端远程控制和跨平台能力才有可靠承载面。
