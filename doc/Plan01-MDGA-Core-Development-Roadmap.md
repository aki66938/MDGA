# Plan01 - MDGA Core Development Roadmap

项目代号：MDGA  
Slogan：Make DeepSeek Great Again  
当前阶段：概念确认与主路线规划  
文档定位：本文件是 MDGA 项目的核心开发路线文档，后续产品设计、技术选型、MVP 开发、测试验收与移动端适配都应以此为主线展开。

---

## 1. 项目愿景

MDGA 的目标不是再做一个给技术爱好者玩的 Agent 工具，而是把 Agent 变成普通人可以安装、配置、理解、信任并持续使用的个人 AI 工作台。

DeepSeek 的关键价值在于低成本、高可用和逐渐逼近顶尖模型的能力。它让个人用户可以以远低于 GPT、Claude 等顶级模型的成本，获得足够强的 AI 能力。MDGA 要做的事情，是把这种模型能力产品化，降低普通用户接触 Agent 的门槛。

项目初期以桌面端为核心，优先完成稳定的 Windows 桌面体验；随后扩展 macOS、Linux；当桌面端 Agent 内核与任务系统成熟后，再将移动端作为远程控制、轻量对话和任务审批入口进行适配。

---

## 2. 产品定位

MDGA 是一个基于 DeepSeek API 的本地优先个人 Agent 桌面应用。

第一阶段的用户不应被要求理解模型、上下文、MCP、工具调用、Agent loop 等复杂概念。用户只需要完成三件事：

1. 安装 MDGA。
2. 设置自己的 `DEEPSEEK_API_KEY` 环境变量。
3. 用自然语言让 MDGA 帮自己完成聊天、文档、文件、资料整理和轻量自动化任务。

产品定位应始终围绕以下原则：

- 低成本：默认使用 DeepSeek 高性价比模型，明确显示任务成本。
- 易上手：普通用户不需要命令行、不需要复杂配置、不需要阅读长教程。
- 本地优先：会话记录、文件索引、长期记忆默认保存在本地；API Key 只从环境变量读取，不进入应用存储。
- 权限透明：任何高风险动作都必须可见、可审计；是否逐次确认由用户选择的权限模式决定。
- 可扩展：后续支持插件、MCP、技能系统，但不支持多模型；MDGA 为 DeepSeek API 而生，不能牺牲基础体验。

---

## 3. 项目边界

### 3.1 初期必须做

- DeepSeek API Key 环境变量读取与连接测试。
- DeepSeek 模型调用、流式输出、错误处理、费用估算。
- 桌面聊天界面。
- 会话历史与本地数据管理。
- 本地对话、项目和工作区管理。
- 文件导入、解析、总结与问答。
- Agent 任务规划、工具调用、执行日志与用户确认。
- 基础权限系统。
- Windows 桌面安装包与自动更新基础能力。

### 3.2 初期暂缓

- 云端账号系统。
- 官方代充值或统一计费。
- 多人协作。
- 插件市场。
- 完整移动端 Agent 执行环境。
- 全自动控制用户电脑。
- 大规模商业化分发。
- 云端同步。

### 3.3 长期可探索

- MCP 工具生态。
- Agent 模板市场。
- 本地长期记忆。
- 本地知识库。
- 任务自动化。
- 移动端远程审批和任务控制。
- 企业版或团队版。
- 数据导出、迁移和跨设备同步。

---

## 4. 推荐技术路线

### 4.1 桌面框架

主路线：

- Tauri 2 + Rust-first core + React/Vue/Svelte 前端

推荐理由：

- 包体小。
- 启动速度和资源占用更适合作为长期常驻的个人 AI 工作台。
- 安全边界相对清晰。
- Rust core 适合处理 Agent Kernel、本地文件、权限、加密存储、沙箱控制和系统接口。
- Tauri 2 保留未来移动端轻量聊天与远程控制入口的实现可能，但不决定 PC 端 Agent 能力上限。

PC 端设计原则：

- 性能和速度优先，Windows / macOS / Linux 均以重度 Rust core 为核心。
- 不因移动端操作系统限制而削弱 PC 端 Rust core 能力；MDGA 的系统原生级 Agent 能力只在 PC 端实现。
- Web 前端主要承担界面、交互和状态展示，不直接承载高权限业务逻辑。
- Agent Kernel、Tool Runtime、Permission Manager、Sandbox Manager、Storage Layer 应设计为可独立测试的 Rust crate。
- UI 与内核通过明确的 command / event / IPC 边界通信。

工程开发原则：

- 遵循 DRY，避免重复实现同一业务规则、权限判断、token 计算或数据转换逻辑。
- 遵循 SOLID，让核心模块具备清晰职责、可替换依赖和可测试边界。
- 遵循 KISS，优先选择简单、可验证、可维护的实现，避免为了未来可能性过早复杂化。
- 坚持高内聚、低耦合：模块内部围绕单一职责组织，模块之间通过明确接口、事件或协议通信。
- 源代码目录必须服务于架构边界，避免出现无边界的 `utils`、`common`、`core` 大杂烩。
- 每个 crate / package / app 目录都应能回答：负责什么、不负责什么、对外暴露什么、依赖什么。

第一版平台策略：

- MVP 只做 Windows，优先打磨 Windows 安装、PowerShell / 文件系统 / 权限 / 沙箱体验。
- 项目采用开源路线，代码结构、权限模型、沙箱策略和 token 账本应尽量透明，便于用户审计和社区贡献。
- 插件生态计划引入，但放在核心原型完成后；MVP 先保留 Extension Layer 和权限声明设计，不急于开放插件市场。

Rust crate 边界初步决策：

- 不建议第一版做成单一巨大 Rust crate。它启动最快，但 Agent、工具、存储、沙箱、模型接入会很快耦合，后续测试、跨 PC 平台适配和安全审计都会困难。
- 也不建议过早拆成大量细碎 crate。边界太多会增加接口维护成本，让原型阶段变慢。
- 推荐采用中等粒度拆分：`agent-core`、`deepseek-client`、`tool-runtime`、`sandbox-runtime`、`token-accounting`、`storage`、`shared`。
- `shared` 只放稳定的数据结构、错误类型、事件类型和协议类型，避免变成杂物箱。
- `agent-core` 负责任务状态机、Planner / Executor、上下文调度和任务恢复，不直接读写系统资源。
- `tool-runtime` 负责工具 schema、参数校验、工具执行编排和结果结构化，不直接绕过权限层。
- `sandbox-runtime` 负责 PC 平台相关隔离能力，允许有 Windows / macOS / Linux 的平台实现。
- `token-accounting` 作为独立模块提前实现，不依赖完整 Agent Kernel。
- `storage` 负责 SQLite、文件索引、配置和账本持久化，业务层通过 repository / service 接口访问。
- 后续可增加 `change-manager` 或在 `tool-runtime` 中先保留变更管理边界，用于承载 diff、snapshot、revert、stage / commit 等能力。

Tauri IPC 与权限模式初步决策：

- 前端不能直接调用高权限 Rust 函数，例如任意读写文件、执行命令、启动沙箱进程或访问密钥。
- Tauri command 应按能力暴露，而不是按底层函数暴露。前端调用的是 `create_task`、`approve_action`、`export_artifact` 这类产品动作，而不是 `write_file`、`run_command` 这类裸能力。
- 所有高风险 IPC 请求必须进入 Permission Manager，由后端根据用户选择的权限模式做判断、审批状态检查、审计记录和沙箱策略生成。
- API Key 等敏感凭据只允许后端读取和使用，前端最多接收脱敏状态，例如“已配置 / 未配置 / 测试失败”。
- 文件选择、导入、导出应尽量通过系统对话框和用户显式授权路径完成，后端只在授权路径内操作。
- 前端展示任务事件和执行日志应走只读事件流，避免前端持有可执行能力。
- Tauri IPC schema 应版本化，便于移动端和未来插件生态复用同一套受控协议。

权限模式应对齐 Codex 类产品的使用逻辑，由用户选择边界，而不是由产品替所有用户固定边界：

- Restricted：默认强受限模式，只允许聊天、读取已授权内容和低风险本地操作。
- Ask Every Time：每次高风险文件写入、命令执行、联网或越界访问都请求用户确认。
- Workspace Auto：在用户授权的工作区和沙箱边界内自动执行，越界时请求确认。
- Full Access：面向有能力的技术用户，允许 Agent 在本机以更高权限执行任务；仍应保留明显警告、模式标识、操作日志、token 账本和可选回滚提示。

Full Access 不等于让前端绕过后端直接访问系统，而是 Permission Manager 在用户显式选择该模式后放宽执行策略。这样既能给工程师用户完整能力，也能保留审计、诊断、撤销和后续安全策略演进的基础。

移动端路线：

- 移动端不规划系统原生级 Agent，不承担本地命令执行、系统文件控制、移动 OS 权限突破或类似 PC 端的沙箱运行时。
- 移动端未来只作为聊天平台和远程控制入口：基础 App 内对话、任务查看、审批推送、通知和跨平台远程控制。
- 移动端可采用 Tauri 2 mobile 或其他原生方案，但技术选型服务于轻量聊天和远程控制，不反向约束 PC 端 Rust core 的能力边界。
- 移动端 Agent 设计只围绕跨平台远程控制桌面 Agent 展开，而不是在手机上实现系统原生级 Agent。

备选路线：

- Electron + React

适用条件：

- 希望更快完成可演示原型。
- 团队更熟悉 Node.js / Electron 生态。
- 只用于极早期交互验证或临时演示，不作为长期产品路线。

建议决策：

- MDGA 的长期桌面主线应确定为 Tauri 2 + Rust-first core。
- Electron 仅作为快速验证 UI 思路的备选，不建议进入正式 MVP 架构。

### 4.2 Agent 内核

Agent 内核应与 UI 解耦，至少包含以下模块：

- DeepSeek Client：封装 DeepSeek API、认证、流式输出、Tool Calls、JSON Output、usage 解析和错误处理。
- Conversation Engine：负责消息、上下文、流式输出和摘要压缩。
- Tool Registry：管理可用工具、参数 schema、权限等级。
- Planner：将用户目标拆解为可执行步骤。
- Executor：执行工具调用、记录状态、处理失败和重试。
- Permission Manager：处理读文件、写文件、联网、执行命令等授权。
- Memory Manager：管理用户偏好、会话摘要、长期记忆。
- Token Accounting：记录每次模型请求的输入、输出、缓存命中、缓存未命中、推理 token、估算费用和实际返回 usage。
- Cost Tracker：基于 Token Accounting 估算任务预算、累计成本、账单对照和异常提醒。
- Workspace Manager：为复杂任务提供独立工作区、产物和日志。
- Sandbox Manager：为工具执行、命令执行和文件写入提供本地进程级隔离。
- Change Manager：管理文件变更、diff、snapshot、revert、stage / commit 和后续 review 工作流。

### 4.3 数据存储

本地数据建议分层保存：

- SQLite：会话、任务、文件索引、token 账本、成本记录、工具日志。
- 本地文件夹：导入文件、生成产物、缓存内容。
- 环境变量：DeepSeek API Key 唯一接入来源，应用不在数据库或 OS Keychain 中保存 API Key。
- 向量索引：后续知识库和长期记忆使用，可先延后。

数据治理原则：

- 用户数据默认本地保存，默认不上传云端。
- 用户必须能导出、备份、删除本地会话、项目、任务、token 账本和产物索引。
- 诊断包必须默认脱敏，不能包含 API Key、完整隐私文件内容或未经用户确认的敏感路径。
- 日志需要有保留周期和清理策略，避免长期堆积命令输出、文件片段和调试信息。
- 如果未来引入云端同步，必须作为独立能力重新设计加密、同步范围、冲突处理和退出机制。

### 4.3.1 本地对话、项目与工作区模型

MDGA 需要在 Plan01 中先定义“对话记录本地存储”的框架，而不是等到 UI 设计阶段才补。原因是对话、项目、工作区、任务、产物、权限和 token 账本之间存在数据关系，会影响后续 Agent Runtime、Storage、Workspace Manager、搜索和审计设计。

Plan01 只定义对象模型和原则，不展开侧边栏、空状态、新对话页、自动命名算法等具体 UI 方案。

核心对象建议：

- Project：用户添加的项目目录或逻辑项目，通常对应一个本地文件夹、仓库或长期工作上下文。
- Conversation / Thread：一次持续对话，可以是纯聊天，也可以绑定到某个 Project 和 Workspace。
- Workspace：Agent 实际读写和执行任务的工作区，可以是本地项目目录、临时任务目录或未来 Git worktree。
- Task：Conversation 内的一次明确目标或 Agent 执行过程。
- Artifact：任务生成或修改的文件、报告、文档、图片、表格等产物。
- Activity Event：命令执行、文件编辑、工具调用、审批、错误、token usage 等过程事件。
- Change Set：一次或多次文件变更的集合，用于 diff 展示、审阅、回滚、stage / commit 和后续 PR 工作流。

第一版需要具备：

- 本地保存所有会话记录。
- 自动生成会话标题，并允许用户重命名。
- 支持按项目组织会话。
- 支持新对话选择或继承当前项目 / 工作区。
- 支持会话搜索。
- 支持会话归档、删除和置顶。
- 支持记录会话对应的项目目录、权限模式、模型、token 账本和任务产物。
- 支持无项目的普通聊天会话。
- 支持工作区丢失、移动或不可访问时的降级提示。

设计原则：

- 对话记录默认本地存储，除非用户明确启用同步。
- Conversation 与 Workspace 需要解耦：会话历史可以保留，即使对应工作区被删除或移动。
- Project 是用户理解和管理长期工作的入口，不应被实现细节绑死为 Git 仓库。
- Workspace 是 Agent 执行边界，与权限、沙箱和文件变更记录强相关。
- 新对话页应让用户快速开始聊天，也应能明确当前绑定的项目或工作区。
- 自动命名应服务于查找和回顾，不应覆盖用户手动标题。
- 后续可支持类似 Codex worktree 的隔离工作区，但第一版不要求完整实现。

Codex 参考：

- Codex app 把 project 视为“在特定目录中启动会话”的对象，并建议不同代码库或包拆成不同项目，以便沙箱只覆盖对应文件。
- Codex app thread 支持 Local、Worktree、Cloud 等模式；其中 Local / Worktree 都在本机运行。
- Codex worktree 文档说明 thread 可以绑定到 worktree，worktree 删除后 thread 仍保留历史，并可通过 snapshot 恢复。
- 这些设计说明：MDGA 应把 Conversation、Project、Workspace、Task 和 Artifact 作为明确对象建模，而不是只存一张简单聊天记录表。

参考入口：[Codex app features](https://developers.openai.com/codex/app/features)、[Codex app worktrees](https://developers.openai.com/codex/app/worktrees)、[Codex app review](https://developers.openai.com/codex/app/review)。

### 4.4 本地进程级沙箱

MDGA 不应优先采用 Docker 或微型虚拟机作为默认沙箱。它们隔离强，但启动慢、资源占用高、环境桥接复杂，容易破坏普通用户对本地应用的自然使用体验。

更适合 MDGA 默认路线的是本地 OS-level / process-level sandbox：在用户真实工作区、真实开发环境和真实文件系统上运行，但让 Agent 触发的命令、脚本和工具进程继承受限权限。

调研结论：

- Codex 本地 CLI / IDE / 桌面端采用 OS 级沙箱与 approval policy 分层：沙箱定义技术边界，审批策略决定何时询问用户。
- Codex 在 macOS / Linux / WSL2 上使用平台原生隔离能力；Windows 版为了支持 PowerShell 和真实 Windows 工作流，实现了 native Windows sandbox。
- Codex Windows sandbox 的公开设计包含 restricted token、专用 sandbox 用户、ACL、Windows Firewall、独立 setup binary 和 command runner。
- Claude Code 的 sandboxed Bash / sandbox runtime 也走本地 OS 级隔离路线，macOS 使用 Seatbelt，Linux / WSL2 使用 bubblewrap，并强调文件系统隔离和网络隔离必须同时存在。

MDGA 建议采用分层沙箱模型：

- Permission Layer：决定 Agent 是否允许调用某个工具，例如读取文件、写入文件、联网、执行命令。
- Sandbox Layer：即使工具被允许，也限制对应进程实际能读写哪些路径、访问哪些网络域名。
- Approval Layer：当 Agent 需要越过当前边界时，请求用户确认并记录原因。
- Audit Layer：记录每次工具调用、命令、文件变更、网络访问和用户授权。

不同平台初步方向：

- Windows：优先调研 restricted token、专用低权限本地用户、ACL、Windows Firewall 的组合方案；避免默认依赖 WSL2、Docker 或 Windows Sandbox。
- macOS：调研 Seatbelt profile，对命令进程施加文件系统和网络边界。
- Linux / WSL2：调研 bubblewrap，作为轻量进程级隔离基础。

Docker / VM 的定位：

- 不作为普通用户默认执行环境。
- 可作为企业版、高风险任务、不可信项目、完整复现环境或强隔离模式的高级选项。

### 4.5 完整 Agent 系统分层

MDGA 不应只被设计成“聊天界面 + 工具调用”。完整的个人 Agent 产品需要把任务运行、上下文、工具、安全、审计、评测和远程控制都放进同一套系统框架中。这样后续拆分 Plan02-Plan10 时，每份文档都能回到同一个总纲。

建议系统分层如下：

- UI Layer：桌面端、移动端、任务面板、审批流、Agent 工作过程展示、设置页、权限审计页和成本统计页。
- Agent Runtime Layer：任务状态机、Planner、Executor、步骤执行、失败重试、暂停、恢复、取消和任务产物关联。
- Context Layer：当前任务上下文、会话摘要、文件片段检索、工具执行历史、用户偏好、权限边界和成本预算组装。
- Tool Runtime Layer：工具注册、参数 schema、参数校验、权限预检、沙箱策略生成、执行、超时、取消和结构化结果返回。
- Permission & Sandbox Layer：工具权限判断、本地进程级隔离、文件系统边界、网络边界、命令边界、用户审批和越权处理。
- Data Layer：SQLite、本地文件产物、导入文件、缓存、配置、密钥、索引和后续向量存储。
- Observability Layer：任务时间线、工具调用日志、命令执行记录、文件变更记录、网络访问记录、授权记录、成本记录、错误原因、用户可见摘要和脱敏诊断包。
- Token Accounting Layer：记录每次模型请求的原始 usage、输入 token、输出 token、缓存命中 token、缓存未命中 token、推理 token、单次费用、任务费用和账单对照数据。
- Evaluation Layer：固定任务集、模型版本对比、工具调用成功率、任务完成率、用户确认次数、成本、耗时和失败分类。
- Extension Layer：Skill、MCP、插件治理、权限声明、签名、启用禁用、更新授权和第三方工具审计。
- Sync / Remote Layer：移动端任务查看、远程发起任务、审批推送、通知同步、桌面在线状态和敏感数据边界。

其中最需要优先明确的不是所有功能，而是各层之间的协议：

- Agent Runtime 与 UI 之间需要统一任务状态、步骤状态、审批请求和产物状态。
- Agent Runtime 与 UI 之间需要统一 Agent 工作事件的展示级别，例如默认折叠、摘要显示、用户展开、完整日志和审计导出。
- Agent Runtime 与 Context Layer 之间需要统一每一步可见上下文的组装规则。
- Agent Runtime 与 Tool Runtime 之间需要统一 tool call、observation、错误和重试协议。
- Tool Runtime 与 Permission & Sandbox Layer 之间需要统一权限声明、沙箱策略和越权结果。
- Data Layer 与 Observability Layer 之间需要统一事件记录、审计日志和诊断导出结构。
- DeepSeek Client 与 Token Accounting Layer 之间需要统一 usage 原始字段、标准化字段和费用计算规则。

第一阶段不需要把所有层都做到完整，但不能让实现绕过这些边界。MVP 至少应形成 Agent Runtime、Tool Runtime、Permission & Sandbox、Data、Observability、Token Accounting 六个基础骨架，否则后续移动端、技能系统、MCP、插件治理和费用透明都会缺少稳定承载面。

---

## 5. 阶段性目标

## Phase 0 - 项目基线与可行性验证

目标：确认 MDGA 的产品方向、技术路线和 DeepSeek API 能力边界。

关键任务：

- 完成 DeepSeek API 调研。
- 验证聊天、流式输出、长上下文、JSON Output、Tool Calls。
- 验证 API Key 环境变量读取、缺失提示和连接测试方案。
- 验证 DeepSeek 在 Agent loop 中的稳定性。
- 验证 Rust-first core + Tauri 2 的最小桌面壳。
- 完成 Windows 本地进程级沙箱可行性调研。
- 明确完整 Agent 系统分层和各层协议边界。
- 制定第一版产品范围。
- 确认项目名称、包名、目录结构和开源许可证候选。

验收标准：

- 可以通过命令行或最小原型稳定调用 DeepSeek。
- 可以完成一次带工具调用的简单任务。
- 可以估算一次任务的大致 token 成本。
- 明确 Rust crate 边界、Tauri IPC 边界和第一版沙箱边界。
- 明确 Agent Runtime、Tool Runtime、Permission & Sandbox、Data、Observability、Token Accounting 的 MVP 骨架。
- 明确 MVP 不做什么。

---

## Phase 1 - 桌面 MVP

目标：让普通用户能够安装、配置 API Key，并完成基础 AI 使用。

核心功能：

- Windows 桌面应用。
- 首次启动向导。
- API Key 环境变量检测、连接测试和配置指引。
- DeepSeek 模型选择。
- Rust core 驱动的基础会话、配置和本地存储。
- 基础聊天。
- 流式输出。
- 会话历史。
- 本地会话搜索。
- 会话自动命名与重命名。
- 项目目录绑定。
- 新对话项目 / 工作区选择。
- Markdown 渲染。
- 文件导入。
- TXT / Markdown / PDF / DOCX 的基础解析。
- 文件总结和问答。
- 本次请求费用估算。
- 本次请求 token 用量展示。
- 会话级 token 用量累计。
- 常见错误提示，例如 Key 无效、余额不足、网络失败、限流。

验收标准：

- 新用户 5 分钟内可以完成安装和第一次对话。
- Windows 用户可以通过系统环境变量配置 API Key，并在应用中完成连接测试。
- 至少支持 3 类本地文件的总结与问答。
- 用户可以看到每次请求的输入 token、输出 token、总 token 和估算费用。
- 应用崩溃不会丢失已有会话。
- 用户可以在项目维度查看、搜索和继续历史会话。

---

## Phase 2 - Agent Kernel v0

目标：MDGA 从聊天工具进化为可控 Agent。

核心功能：

- 任务模式。
- 任务规划。
- 工具调用。
- 执行日志。
- Agent 工作过程事件流。
- 用户确认。
- 权限分级。
- 本地进程级沙箱 v0。
- 失败重试。
- 任务中断与恢复。
- 任务级 token 账本。

第一批工具：

- 读取本地文件。
- 写入本地文件。
- 创建 Markdown / TXT / DOCX 初版产物。
- 网页抓取与总结。
- CSV / Excel 基础分析。
- 本地文件夹整理。
- 在指定工作区内执行低风险命令。

权限等级建议：

- Level 0：纯聊天，无工具。
- Level 1：只读工具，例如读取文件、读取网页。
- Level 2：可写工具，例如生成文件、修改指定目录内容。
- Level 3：高风险工具，例如执行命令、批量改写文件、调用外部应用。

验收标准：

- 用户可以看到 Agent 为什么要执行某一步。
- 用户可以看到 Agent 运行命令、编辑文件、读取文件、生成产物等关键动作的摘要。
- 高风险动作必须按当前权限模式确认、自动放行或拒绝。
- 工具和命令默认只能在授权工作区内读写。
- 沙箱外写入、联网和命令执行必须触发确认或被拒绝。
- 用户可以看到任务中每一步模型调用的 token 用量和累计费用。
- 任务失败时可以看到失败原因。
- 同一个任务可以暂停、继续或取消。

---

## Phase 3 - Public Beta

目标：让真实用户开始持续使用，并建立反馈闭环。

核心功能：

- 自动更新。
- 崩溃日志。
- 可选匿名 telemetry。
- 问题反馈入口。
- 新手任务模板。
- 权限审计页。
- 成本统计页。
- Agent 工作过程展示优化。
- 本地数据备份与恢复。
- 本地数据导出与删除。
- 诊断包脱敏导出。
- 安装包签名。
- 自动更新安全校验。

推荐任务模板：

- 总结一份 PDF。
- 整理一个文件夹。
- 写一份周报。
- 分析一张 CSV 表格。
- 把会议记录整理成待办。
- 生成一篇小红书 / 公众号 / 邮件草稿。

验收标准：

- 20-50 名真实用户可以完成至少一个完整任务。
- 收集到明确的失败案例和高频需求。
- 用户不需要开发者陪同即可完成基础使用。
- 安装包不会被 Windows 安全机制大规模误拦截，自动更新链路可验证来源。

---

## Phase 4 - 技能系统与生态化

目标：让 MDGA 拥有可复用、可扩展、可沉淀的 Agent 能力。

核心功能：

- Skill 文件格式。
- 技能安装、启用、禁用。
- 技能权限声明。
- 技能运行日志。
- 官方技能模板。
- MCP 客户端能力。
- MCP 工具权限管理。
- 本地知识库。
- 长期记忆。

注意事项：

- 插件市场不能过早开放。
- 所有第三方工具必须有权限说明。
- 高风险工具必须有沙箱或隔离策略。
- 用户必须能一键禁用某个工具或技能。

验收标准：

- 用户可以通过安装技能扩展 MDGA。
- 技能不会绕过权限系统。
- MCP 工具可以被配置、审计和禁用。

---

## Phase 5 - 移动端适配

目标：移动端成为基础聊天平台和桌面 Agent 的远程控制入口，而不是完整替代桌面端，也不实现系统原生级移动端 Agent。

第一版移动端定位：

- 基础 App 内对话。
- 查看桌面端任务状态。
- 向桌面端发送新任务。
- 审批高风险操作。
- 接收任务完成通知。
- 浏览会话和产物。
- 进行轻量聊天。

不建议第一版移动端承担：

- 本地大文件处理。
- 完整 Agent 执行环境。
- 本地命令执行。
- 系统级文件控制。
- 移动 OS 权限绕过或深度自动化。
- 复杂插件系统。

验收标准：

- 手机可以远程发起任务。
- 手机可以审批桌面端敏感操作。
- 桌面端与移动端状态一致。
- 移动端不需要获得系统原生级权限即可完成第一版目标。

---

## 6. 核心技术问题

### 6.1 DeepSeek API 接入

需要解决：

- API Key 环境变量读取。
- 流式输出。
- 模型选择。
- Tool Calls。
- JSON Output。
- 错误码处理。
- 限流处理。
- 费用估算。
- 请求重试。
- 上下文压缩。

设计原则：

- MDGA 只支持 DeepSeek API，不设计多模型 Provider 抽象。
- 不支持 OpenAI、Claude、Gemini、OpenRouter 或其他第三方聚合 Provider。
- DeepSeek API 认证采用 Bearer Auth，API Key 只从环境变量读取。
- DeepSeek 模型能力差异通过配置声明，而不是散落在业务代码里。
- 当前只接入 DeepSeek API；若未来 DeepSeek 官方开放独立 coding plan 或一等编码产品入口，再单独评估，不在当前架构中预留通用 provider 体系。
- DeepSeek 官方当前公开文档重点是 API 与第三方/开源编码工具集成，例如 Claude Code、OpenCode、OpenClaw 和 Deep Code；Plan01 不把这些集成视为 MDGA 的登录或订阅入口。

API Key 环境变量策略：

- 唯一推荐变量名：`DEEPSEEK_API_KEY`。
- Windows：用户在系统或用户环境变量中添加 `DEEPSEEK_API_KEY`。
- macOS：用户可在 `~/.zshrc` 中 `export DEEPSEEK_API_KEY=...`；如果未来桌面应用无法继承 shell 环境变量，需要提供只读检测和配置指引，但不在应用内保存 Key。
- Linux：用户可在 `~/.bashrc` 或对应 shell 配置中 `export DEEPSEEK_API_KEY=...`。
- 应用启动时只读取环境变量，不写入、不保存、不回显 API Key。
- 如果环境变量缺失，应用只展示配置指引和连接测试失败原因。
- 工具子进程默认不应继承 `DEEPSEEK_API_KEY`，除非用户明确允许；避免 Full Access 或命令执行场景下通过环境变量泄露 Key。

### 6.2 API Key 安全

需要解决：

- API Key 不进入 MDGA 数据库、配置文件或 OS Keychain。
- API Key 只从环境变量读取。
- 日志不能泄露 Key。
- 导出诊断信息时必须脱敏。
- 用户通过修改或删除系统环境变量来更换或移除 Key。

### 6.2.1 本地数据治理

需要解决：

- 会话、任务、token 账本、工具日志、文件索引和产物索引的导出。
- 本地数据备份与恢复。
- 用户主动删除数据。
- 诊断包脱敏导出。
- 日志保留周期和清理策略。
- 云端同步暂缓时的本地迁移方案。

设计原则：

- 用户应能理解 MDGA 在本地保存了什么。
- 用户应能删除或导出自己的数据。
- 故障诊断不能以牺牲隐私为代价。
- 本地优先不等于没有数据治理，尤其是 Agent 会接触文件内容、命令输出和路径信息。

### 6.3 权限与安全

需要解决：

- 文件读写边界。
- 网络访问边界。
- 命令执行边界。
- 本地进程级沙箱边界。
- 工具权限提示。
- 危险操作确认。
- 操作日志。
- 回滚能力。

安全策略：

- 默认只读或最小权限。
- 任何扩大权限都必须由用户明确授权或选择更高权限模式。
- 高风险动作需要预览。
- 权限判断不能只依赖 Agent 自觉或命令字符串分析，必须有 OS 级执行边界兜底。
- 文件系统隔离和网络隔离要同时设计；只限制写文件但放开网络，仍可能造成敏感信息外泄。

权限模式建议：

- Restricted：默认强受限模式，适合普通用户和未知任务。
- Ask Every Time：每次高风险动作都由用户确认，适合谨慎使用。
- Workspace Auto：在授权工作区和沙箱边界内自动执行，越界请求确认，适合日常工程任务。
- Full Access：面向明确理解风险的技术用户，允许 Agent 获得完整本地执行能力，适合复杂重构、工程自动化和高信任任务。

Full Access 的产品原则：

- 允许存在，不把高级用户锁死在低权限体验里。
- 必须由用户主动选择，不能默认开启。
- 必须有明显状态提示，避免用户忘记当前处于高权限模式。
- 必须保留审计日志、文件变更记录、命令记录、网络访问记录和 token 账本。
- 尽量提供 diff、快照、版本控制或可回滚工作区，帮助用户承担高权限模式下的结果管理。

本地沙箱实现方向：

- 工具权限层负责判断 Agent 是否可以发起动作。
- 沙箱执行层负责启动受限子进程，并让脚本、解释器、构建工具和子进程继承同一限制。
- Windows 优先调研 restricted token、专用 sandbox 用户、ACL 和 Windows Firewall。
- macOS 优先调研 Seatbelt。
- Linux / WSL2 优先调研 bubblewrap。
- Docker / VM 只作为高隔离模式，不作为默认体验。

默认模式下的第一版沙箱边界建议：

- 默认可读范围：用户明确选择的工作区、导入文件和应用自己的数据目录。
- 默认可写范围：任务工作区、用户确认的导出目录和应用缓存目录。
- 默认网络策略：Agent 命令进程默认禁网；模型 API、更新检查、用户确认过的网页抓取走受控网络通道。
- 默认命令策略：禁止任意命令；只开放白名单命令或经用户确认的一次性命令。
- Full Access 模式下可放宽上述边界，但必须由用户主动开启，并保留明显状态提示和完整审计记录。

### 6.4 成本控制

需要解决：

- token 估算。
- 实际 token 统计。
- 服务端 usage 原始字段记录。
- cache hit / cache miss token 记录。
- reasoning token 记录。
- 单任务预算。
- 每日 / 每月预算提醒。
- 上下文裁剪。
- 摘要压缩。
- 缓存复用。
- 文件分块。

Token Accounting 设计原则：

- MDGA 不应只显示粗略费用估算，而应保存每次模型请求的实际 token 用量。
- 对支持 usage 返回的模型，优先使用服务端返回的实际 usage，而不是本地估算值。
- 对 DeepSeek API，应记录 `prompt_tokens`、`completion_tokens`、`total_tokens`、`prompt_cache_hit_tokens`、`prompt_cache_miss_tokens`，以及 thinking 模式下可能返回的 `reasoning_tokens`。
- 对流式输出，应启用可返回最终 usage 的流式选项；如果 DeepSeek 响应缺失 usage，则本地记录为“估算值”，并在 UI 中明确标注。
- 不需要关心服务端缓存如何实现，也不尝试推断无法确认的缓存细节；如果官方 usage 返回 cache hit / miss，则直接记录并用于费用计算。
- 如果 DeepSeek 不返回 cache hit / miss，则只记录实际输入 token、输出 token、总 token 和估算费用。
- 所有 token 账本应能按请求、消息、会话、任务、日期、模型和 api_source 聚合。
- 用户应能导出 token 账本，用于和 DeepSeek 官方账单对照。

Token 账本字段建议：

- request_id。
- api_source，固定为 deepseek。
- model。
- mode，例如 chat、agent、tool-planning、summary、embedding。
- conversation_id。
- task_id。
- step_id。
- prompt_tokens。
- completion_tokens。
- total_tokens。
- prompt_cache_hit_tokens。
- prompt_cache_miss_tokens。
- reasoning_tokens。
- estimated_cost。
- pricing_version。
- usage_source，例如 deepseek_usage、local_estimate、mixed。
- created_at。

这套 token 统计系统应成为 MDGA 与 Claude Desktop、Codex 等编码型 Agent 产品不同的核心透明能力之一。用户不只是看到“本次任务大概花了多少钱”，而是能看到每一步真实消耗了多少 token、哪些输入可能命中了缓存、最终费用如何计算，并能拿它和官方账单做对照。

### 6.5 普通用户体验

需要解决：

- 用户不知道什么是 API Key。
- 用户不知道模型差异。
- 用户不知道 Agent 能做什么。
- 用户害怕软件乱动电脑。
- 用户不理解失败原因。

产品策略：

- 首次启动向导必须极简。
- 所有错误提示必须人话化。
- 所有高风险动作必须可见，并在需要确认时解释原因。
- 默认提供任务模板，而不是空白输入框。

### 6.5.1 Windows 发布与更新体验

Windows-first 不只是开发平台选择，也会影响用户信任和安装转化。

需要解决：

- Windows 安装包格式。
- 代码签名证书。
- SmartScreen 或安全软件误拦截风险。
- 自动更新包签名与完整性校验。
- 崩溃日志与诊断信息开关。
- 卸载、数据保留和数据删除选项。

设计原则：

- 用户不应因为安装警告而误以为 MDGA 是恶意软件。
- 自动更新必须可验证来源，不能引入供应链风险。
- 崩溃日志和诊断信息默认尊重隐私，敏感内容必须脱敏或由用户确认。

### 6.6 Agent 工作过程可见性

Codex 类产品的一个重要体验是：Agent 的命令执行、文件编辑、检查动作和结果并不全部打断主对话，而是以弱化、折叠、可展开的方式出现在任务流中。用户默认看到的是“发生了什么”的摘要，必要时可以展开查看命令、文件、diff、终端输出或审计记录。

MDGA 也需要类似的可见性设计，但优先级应放在 MVP 闭环完成之后。Plan01 只确认涉及逻辑，不展开具体实现方案。

需要解决：

- Agent 运行命令、读取文件、编辑文件、创建文件、联网、调用工具、生成产物时，后端应产生结构化事件。
- 每类事件都需要区分用户可见摘要、可展开详情、完整审计日志和诊断日志。
- 不是所有底层输出都应该进入主对话流；长命令输出、重复日志、低价值中间过程应默认折叠或隐藏。
- 文件编辑需要输出文件名、变更摘要、增删行数量、是否可查看 diff、是否可回滚。
- 命令执行需要输出命令摘要、运行状态、耗时、退出码、关键错误片段和完整日志入口。
- 工具调用需要输出工具名、目的、权限等级、输入摘要、输出摘要和失败原因。
- 高风险动作仍需按权限模式请求确认，不能被折叠逻辑隐藏。
- Full Access 模式下也必须保留操作痕迹，折叠只影响展示，不影响审计。

展示原则：

- 默认折叠：工作过程不应淹没用户与 Agent 的主对话。
- 灰度弱化：工具动作和系统动作应弱于自然语言回答，避免视觉噪音。
- 自动摘要：多条同类动作可以聚合为“已运行 N 条命令”“已编辑 N 个文件”。
- 可展开：用户需要时可以展开单条动作查看详情。
- 可跳转：文件编辑应能跳转到 diff 或文件位置。
- 可审计：完整事件流应可在任务日志、诊断包或审计页中查看。
- 可降噪：前端应有规则决定哪些事件进入对话流，哪些只进入日志面板。

这部分会影响前端展示内容、隐藏逻辑、事件聚合方式，也会反向要求后端接口不要只返回纯文本，而是返回结构化 Agent activity event。具体组件、交互和事件 schema 应在后续规格文档中展开。

后续可参考的开源实现与调研入口：

- OpenHands：重点参考其 Action / Observation / EventLog 思路。OpenHands SDK 论文描述了 event-sourcing 状态管理：ActionEvent 表示工具调用，ObservationEvent 表示工具结果，ConversationState 维护追加式 EventLog。适合后续研究 MDGA 的 Agent activity event、任务回放和审计日志设计。参考：[OpenHands SDK event-sourcing](https://arxiv.org/html/2511.03690v1)、[OpenHands GitHub](https://github.com/OpenHands/OpenHands)。
- OpenHands UI issues：社区已经讨论过“observation block 应显示触发它的命令”，以及“可视化 agent loop、action-observation pair、LLM metrics 和 token usage”。这与 MDGA 的工作过程可见性、token 账本和审计视图高度相关。参考：[Display command in observation block](https://github.com/OpenHands/OpenHands/issues/11853)、[Visualize Agent Loop](https://github.com/OpenHands/OpenHands/issues/8916)。
- Aider：虽然是 CLI 产品，但它明确区分哪些命令输出进入聊天上下文。例如 `/git` 运行 git 命令但输出不进入 chat，`/run` 可选择把 shell 输出加入 chat，`/diff` 专门展示上次消息以来的 diff。适合参考“哪些输出进入主对话，哪些只进入日志或用户主动查看”。参考：[Aider in-chat commands](https://aider.chat/docs/usage/commands.html)、[Aider Git integration](https://aider.chat/)。
- Cline：开源 VS Code Agent，支持创建文件、运行命令、浏览网页和 human-in-the-loop approval。其 changelog 提到 collapsible MCP response panels，用来保持主回复聚焦，同时允许查看详细 MCP 输出。适合参考“工具响应折叠面板”的前端体验方向。参考：[Cline GitHub](https://github.com/cline/cline)、[Cline changelog](https://github.com/cline/cline/blob/main/CHANGELOG.md)。
- Roo Code：社区 issue 中提出“单个可折叠面板显示 X 个文件已变更，展开后每个文件一行并复用 diff 组件”的设计，适合参考文件变更聚合和多文件 diff 展示。参考：[Roo Code file changes panel issue](https://github.com/RooCodeInc/Roo-Code/issues/11493)、[Roo Code tools docs](https://roocodeinc.github.io/Roo-Code/basic-usage/how-tools-work/)。

---

## 7. 测试路线

### 7.1 单元测试

覆盖：

- DeepSeek Client。
- Tool schema 校验。
- 权限判断。
- Agent activity event 生成规则。
- 成本估算。
- 上下文压缩。
- 本地存储。
- 对话、项目、工作区关系模型。
- 数据导出、删除和诊断脱敏规则。
- 文件变更 diff / snapshot / revert 规则。

### 7.2 集成测试

覆盖：

- DeepSeek API 调用。
- 流式输出。
- 文件解析。
- 工具调用链路。
- Agent activity event 到 UI 展示层的数据链路。
- 任务暂停与恢复。
- 错误重试。
- 数据备份、恢复和导出链路。
- 变更集到 review / diff 展示层的数据链路。

### 7.3 端到端测试

覆盖：

- 首次启动。
- 配置 API Key。
- 完成一次聊天。
- 创建、命名、重命名、搜索和恢复一个本地会话。
- 导入文件并总结。
- 执行一次 Agent 任务。
- 展开和折叠 Agent 工作过程。
- 拒绝高风险权限。
- 恢复历史会话。
- 导出并删除本地数据。

### 7.4 Agent Eval

建立固定评测任务集：

- 总结长文档。
- 整理文件夹。
- 生成报告。
- 分析表格。
- 网页资料汇总。
- 多步骤写作。

每个任务记录：

- 是否完成。
- 调用工具次数。
- 用户确认次数。
- token 成本。
- 总耗时。
- 失败原因。
- 输出质量评分。

---

## 8. 风险清单

### 8.1 品牌风险

MDGA 的 slogan 带有强烈表达色彩，适合内部代号或社区传播，但正式发布时需要评估地区、平台、商店审核和公众接受度。

DeepChat 名称已有其他项目使用，因此项目已重命名为 MDGA。后续仍需检查：

- GitHub 项目名。
- 域名。
- 商标。
- 应用商店名称。
- SEO 冲突。

### 8.2 模型能力风险

DeepSeek 性价比高，但在复杂 Agent 任务、工具调用稳定性、长链路规划方面可能仍弱于顶尖模型。

缓解方式：

- 强约束工具 schema。
- 小步执行。
- 每一步记录可见日志。
- 失败可重试。
- 关键步骤要求用户确认。
- 不通过多模型 Provider 规避 DeepSeek 能力风险；只在 DeepSeek 模型族内部选择更适合的模型和参数。

### 8.3 安全风险

Agent 工具一旦拥有本地文件和系统权限，就可能造成误删、误写、隐私泄露或恶意工具滥用。

缓解方式：

- 默认只读。
- 分级授权。
- 本地进程级沙箱。
- 文件系统与网络双重隔离。
- 敏感操作预览。
- 操作日志。
- 可撤销工作区。
- 插件签名和来源审查。

特别注意：

- 只做“权限弹窗”不足以防止恶意命令、脚本或工具绕过约束。
- 只做文件系统隔离但放开网络，可能导致隐私文件被读取后外传。
- 只做网络隔离但放开文件系统，仍可能造成误删、误写和本地持久化风险。
- Windows 原生沙箱复杂度高，不能低估 restricted token、ACL、Firewall、专用用户和安装权限之间的工程成本。

### 8.4 产品复杂度风险

项目容易膨胀为“什么都想做”的超级工具。

缓解方式：

- MVP 只做聊天、文件、任务、权限、成本。
- 每个阶段都有明确不做事项。
- 所有新功能必须回答：它是否让普通用户更容易使用 Agent？

---

## 9. 推荐近期工作顺序

### Step 1 - 新工作区初始化

建立正式代码仓库：

- `apps/desktop`
- `crates/agent-core`
- `crates/deepseek-client`
- `crates/tool-runtime`
- `crates/sandbox-runtime`
- `crates/token-accounting`
- `crates/storage`
- `crates/shared`
- `packages/ui`
- `docs`
- `tests`

### Step 2 - 技术选型确认

结论：

- 桌面端主线采用 Tauri 2 + Rust-first core。
- PC 端覆盖 Windows / macOS / Linux，均以重度 Rust core 为核心。
- 移动端不做系统原生级 Agent，只做基础聊天和桌面 Agent 远程控制；Tauri 2 mobile 只是可选实现路线，不反向约束 PC 端架构。
- Electron 不进入正式 MVP，仅保留为极早期 UI 演示备选。
- 第一版只做 Windows。
- 项目采用开源路线。
- 计划引入插件生态，但优先级放到核心原型完成后。
- Rust crate 采用中等粒度拆分，避免单体 core 和过度拆分两个极端。
- Tauri IPC 采用后端权限网关和用户可选权限模式；技术用户可以主动选择 Full Access，但高权限能力仍不直接暴露给前端。

后续需要细化：

- Windows-first 对安装、更新、签名、沙箱和系统权限的具体影响。
- 开源许可证、贡献规则和安全披露流程。
- 插件生态开放前的权限声明、签名、审计和禁用机制。
- 各 Rust crate 的 public API、错误类型和测试边界。
- Tauri IPC 的 command schema、事件 schema、版本策略、权限模式和权限审计格式。
- Windows 代码签名、SmartScreen、自动更新签名校验和崩溃日志策略。

### Step 3 - DeepSeek Client 原型

建立最小 DeepSeek Client：

- 从 `DEEPSEEK_API_KEY` 环境变量读取 Key。
- 连接测试。
- 普通聊天。
- 流式输出。
- Tool Calls。
- JSON Output。
- 错误处理。
- usage 原始字段解析。
- cache hit / cache miss token 解析。
- 流式输出最终 usage 解析。

### Step 4 - Token Accounting MVP

建立最小 token 统计与费用对照模块：

- 保存每次模型请求的 api_source、model、request_id、conversation_id、task_id 和 step_id。
- 保存服务端返回的原始 usage。
- 标准化记录 prompt tokens、completion tokens、total tokens、cache hit tokens、cache miss tokens 和 reasoning tokens。
- 对不返回 cache hit / miss 的 DeepSeek 响应，标记 usage_source 为 local_estimate 或 mixed。
- 记录 pricing_version 和 estimated_cost，便于后续按价格版本回放计算。
- 支持按请求、会话、任务、日期、模型和 api_source 聚合。
- 在桌面 MVP 中展示单次请求和当前会话累计 token 用量。
- 支持导出 token 账本，便于用户与官方账单对照。

验收标准：

- 完成一次 DeepSeek 聊天后，可以看到服务端返回的实际 token usage。
- 如果返回 cache hit / miss，可以分别展示缓存命中和未命中的输入 token。
- 如果 usage 字段缺失，界面明确标注为估算值。
- token 统计模块不依赖完整 Agent Kernel，也不阻塞后续框架演进。

### Step 5 - 本地沙箱技术预研

建立最小 Sandbox Runtime 调研原型：

- Windows：验证 restricted token、ACL、专用 sandbox 用户和 Windows Firewall 的可行性。
- macOS：验证 Seatbelt profile 对文件读写和网络访问的限制能力。
- Linux / WSL2：验证 bubblewrap 对工作区写入、只读路径和网络访问的限制能力。
- 设计统一的 Sandbox Policy schema。
- 设计工具权限、沙箱执行、用户审批和审计日志之间的调用链路。

验收标准：

- 可以在授权工作区内创建文件。
- 无法写入工作区外路径。
- 默认无法发起未经授权的网络访问。
- 子进程继承同一限制。
- 沙箱失败时不会自动降级为无限制执行。

### Step 6 - 桌面壳与首次启动

完成：

- 桌面窗口。
- 设置页。
- API Key 环境变量检测。
- 连接测试。
- 基础聊天。
- 单次请求 token 用量展示。

### Step 7 - Agent Kernel v0

完成：

- Tool Registry。
- Permission Manager。
- Sandbox Manager。
- Change Manager。
- Planner。
- Executor。
- Task Log。
- Context Manager。
- Observability Event Log。
- Token Accounting。

---

## 10. 后续文档规划

建议后续文档按以下方式拆分：

- `Plan02-Technical-Architecture.md`：技术架构细化。
- `Plan03-Desktop-MVP.md`：桌面 MVP 执行计划。
- `Plan04-Token-Accounting.md`：token 统计、费用计算与账单对照设计。
- `Plan05-Agent-Kernel.md`：Agent 内核设计。
- `Plan06-Security-And-Permissions.md`：权限与安全设计。
- `Plan07-DeepSeek-Client.md`：DeepSeek API 接入、环境变量认证、流式输出、Tool Calls、usage 和错误处理设计。
- `Plan08-Mobile-Adaptation.md`：移动端适配路线。
- `Plan09-Sandbox-Runtime.md`：本地进程级沙箱技术设计。
- `Plan10-System-Protocols.md`：完整 Agent 系统分层、跨层协议与事件 schema 补充设计。
- `Plan11-Agent-Tool-Runtime-And-Extensions.md`：Built-in Tools、MCP Adapter、Skills 与真实本地执行链路设计。
- `Spec01-Desktop-Onboarding.md`：首次启动体验规格。
- `Spec02-Task-And-Permission-Flow.md`：任务与授权流程规格。
- `Spec03-Agent-Activity-Visibility.md`：Agent 工作过程展示、折叠和审计规格。
- `Spec04-Conversation-Project-Workspace.md`：本地对话、项目、工作区和历史记录规格。
- `Spec05-Data-Governance.md`：本地数据导出、删除、备份、恢复和诊断脱敏规格。
- `Spec06-Windows-Install-And-Update.md`：Windows 安装、签名、自动更新和崩溃诊断规格。
- `Spec07-Change-Review-And-Revert.md`：文件变更、diff、snapshot、revert 和审阅规格。

---

## 11. 当前结论

MDGA 最应该先成为一个低成本、本地优先、权限透明的个人 AI Agent 桌面端。

技术主线应明确为 PC-first Rust core + Tauri 2：Windows / macOS / Linux 均以重度 Rust core 承载本地系统能力，移动端只作为基础聊天和桌面 Agent 的远程控制入口，不实现系统原生级移动端 Agent。

安全主线应明确为权限系统 + 本地进程级沙箱 + 用户可选权限模式 + 审计日志。默认体验不依赖 Docker 或微型虚拟机，而是优先采用贴近 Codex、Claude Code 等产品实践的 OS 级进程隔离方案；同时允许技术用户主动选择 Full Access，让 Agent 在明确授权下获得完整本地执行能力。

费用透明也应成为 MDGA 的核心差异化能力。系统应保存每次模型请求的实际 token usage、缓存命中与未命中 token、输出 token、推理 token、估算费用和账单对照数据，让用户能够理解自己的 API Key 花费去了哪里。

短期不要追求万能，不要急着做插件市场，不要让普通用户理解技术概念。第一阶段只需要证明一件事：

普通用户可以安装 MDGA，设置自己的 `DEEPSEEK_API_KEY` 环境变量，然后让它可靠、低成本、可控地完成真实任务。

只要这个闭环成立，后续的技能系统、MCP、移动端、社区生态和商业化才有坚实基础。
