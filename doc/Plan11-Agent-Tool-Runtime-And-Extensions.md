# Plan11 - Agent Tool Runtime And Extensions

项目代号：MDGA
文档定位：定义 MDGA 从“模型知道工作区”升级到“Agent 能真实执行本地动作”的工具运行时、MCP 接入与 Skills 体系。本文承接 `Plan05-Agent-Kernel.md`、`Plan06-Security-And-Permissions.md`、`Plan09-Sandbox-Runtime.md` 与 `Spec01-Workspace-Session-Binding.md`。

---

## 1. 背景

当前 `0.0.7` 已经把 conversation workspace 注入 DeepSeek 上下文。模型能够回答当前工作区路径，但仍然不能真实创建文件、编辑文件或执行命令。

用户在对话中要求“在工作区创建一个 txt 文件”时，模型可能生成代码并自行描述“已创建”，但这只是文本生成，不是本地执行。成熟 Agent 产品的核心差异在于：模型不直接执行动作，Host / Runtime 接收模型的工具调用请求，完成权限校验、真实执行、结果回传和审计展示。

Plan11 的目标是补齐这条链路。

---

## 2. 成熟 Agent 产品的做法

### 2.1 Codex

Codex 的公开文档强调 sandbox 与 approvals。sandbox 的意义是让 Agent 在已批准边界内读取文件、编辑文件和运行常规项目命令，减少每次低风险操作都询问用户的审批疲劳；越界或高风险操作再进入审批。

参考：

- [Codex Sandbox](https://developers.openai.com/codex/concepts/sandboxing)
- [Codex Agent approvals & security](https://developers.openai.com/codex/agent-approvals-security)

对 MDGA 的启发：

- workspace 不是提示词装饰，而是工具执行边界。
- 低风险文件操作应可在 workspace 内自动执行。
- 命令执行、删除、越界写入、网络等动作需要审批策略。
- 前端需要展示真实工具事件，而不是只显示模型文本。

### 2.2 Claude Code

Claude Code 把 permissions 和 sandboxing 分开。官方权限文档说明 permissions 控制 Claude Code 可以使用哪些工具，以及可以访问哪些文件或域名；这些规则适用于 Bash、Read、Edit、WebFetch、MCP 等工具。sandboxing 则是 OS-level 防线，主要限制 Bash 及其子进程的文件系统和网络访问。

Anthropic 还公开介绍过 auto mode：默认模式会在运行命令或修改文件前询问用户，auto mode 通过分类器自动允许安全动作、拦截风险动作，以减少审批疲劳。

参考：

- [Claude Code permissions](https://code.claude.com/docs/en/permissions)
- [How we built Claude Code auto mode](https://www.anthropic.com/engineering/claude-code-auto-mode)

对 MDGA 的启发：

- 权限层不应只绑定单个工具；它需要覆盖 Built-in Tools、Shell、MCP。
- Sandbox 与 Permission 是两层，不可互相替代。
- 后续可以有 Restricted / Ask Every Time / Workspace Auto / Full Access 四档模式。
- 自动审批需要保守起步，先从 workspace 内的低风险读写开始。

### 2.3 Cursor

Cursor Agent 的 Terminal 工具文档说明，它通过终端执行命令，并支持 sandboxing、历史保留和原生终端集成；用户可通过 `sandbox.json` 配置网络与文件系统访问。

参考：

- [Cursor Terminal tool](https://cursor.com/docs/agent/tools/terminal)

对 MDGA 的启发：

- 命令执行要保留历史、输出和上下文。
- 命令执行与文件工具不是同一风险等级。
- `run_command` 应晚于文件工具实现，且必须具备 cwd、超时、输出截断、审批和终止能力。

### 2.4 Cline / Roo Code

Cline 文档说明 tools 是模型可调用的可执行函数：模型决定调用哪个工具，Cline 运行它，然后把结果返回给模型。Cline / Roo Code 常见工具包括 `read_file`、`write_to_file`、`execute_command` 等。Cline Marketplace 也强调它能创建/编辑文件、运行命令、使用浏览器，并且每一步都经过用户许可。

参考：

- [Cline Tools Reference](https://docs.cline.bot/tools-reference/all-cline-tools)
- [Cline VS Code Marketplace](https://marketplace.visualstudio.com/items?itemName=saoudrizwan.claude-dev)
- [Roo Code Tool Use Overview](https://roocodeinc.github.io/Roo-Code/advanced-usage/available-tools/tool-use-overview/)

对 MDGA 的启发：

- Tool Call 是 Agent 可信执行的中心协议。
- 工具结果必须回传给模型，模型基于真实结果继续回复。
- UI 应显示工具调用、参数摘要、审批状态和执行结果。

### 2.5 OpenHands

OpenHands 的 Agent Server / Runtime 设计把 agent 逻辑与执行环境拆开。它管理 workspace，处理命令和文件操作，向客户端流式推送事件，也可以把执行环境放在 Docker、远程 sandbox 或其他 runtime 中。

参考：

- [OpenHands SDK Architecture Overview](https://docs.openhands.dev/sdk/arch/overview)
- [OpenHands Agent Server Overview](https://docs.openhands.dev/sdk/guides/agent-server/overview)

对 MDGA 的启发：

- Runtime 应是独立边界，而不是把所有逻辑堆在聊天请求函数里。
- 文件/命令结果要事件化，方便回放、审计和调试。
- MVP 可以先用本地进程级 runtime，后续再扩展 remote / sandbox backend。

### 2.6 Goose

Goose 是开源本地 Agent，支持 Desktop / CLI，并基于扩展机制与 MCP 扩展能力。Goose 文档说明它默认更自主，配合 Developer extension 可以执行命令和修改文件；如需更多控制，可配置 permission mode、tool permissions 和 `.gooseignore`。

参考：

- [Goose extensions](https://goose-docs.ai/docs/getting-started/using-extensions/)
- [Goose extension marketplace](https://goose-docs.ai/extensions/)

对 MDGA 的启发：

- MCP / Extensions 适合作为生态层。
- 但本地文件和命令能力必须先经过 MDGA 的权限策略。
- ignore 机制值得参考，后续可做 `.mdgaignore` 或复用 `.gitignore`。

### 2.7 Aider

Aider 更偏终端代码协作。它要求用户把要编辑的文件加入 chat session，随后可以编辑这些文件；它还使用 repo map 提供代码库结构摘要，并通过 Git commit 让变更可审阅、可撤销。

参考：

- [Aider Usage](https://aider.chat/docs/usage.html)
- [Aider Repository Map](https://aider.chat/docs/repomap.html)
- [Aider Git integration](https://aider.chat/docs/git.html)

对 MDGA 的启发：

- 不应把整个项目无脑塞进上下文。
- 后续需要 repo map / workspace index 帮模型理解项目。
- 文件修改应尽量支持 diff、审阅和回滚。

---

## 3. MCP、Skills 与 Built-in Tools 的区别

### 3.1 Built-in Tools

Built-in Tools 是 MDGA 自己实现和控制的可信执行内核。

职责：

- 在 conversation workspace 内读写文件。
- 执行最小必要的本地动作。
- 做路径规范化和越界判断。
- 记录 Activity Event。
- 把真实执行结果返回给模型。

第一版必须先做 Built-in Tools，因为它们直接承载本地安全边界。

### 3.2 MCP

MCP 是开放协议，允许 AI 应用连接外部工具、资源和服务。官方文档把 MCP server 能力分为 Tools、Resources、Prompts，其中 Tools 是模型主动调用的函数，可以写数据库、调用 API、修改文件或触发外部逻辑；Resources 则是应用控制的只读上下文来源。

参考：

- [MCP Introduction](https://modelcontextprotocol.io/docs/getting-started/intro)
- [MCP Server Concepts](https://modelcontextprotocol.io/docs/learn/server-concepts)
- [MCP Resources](https://modelcontextprotocol.io/specification/2025-06-18/server/resources)

MDGA 中 MCP 的定位：

- 生态接入层。
- 用于 GitHub、Linear、Obsidian、数据库、浏览器、第三方 API。
- 不直接取代 MDGA 的本地文件工具。
- MCP tool 执行也必须进入 MDGA Permission Manager 和 Activity Event。

### 3.3 Skills

Skills 是可复用的工作流说明和资源包。Codex 官方文档说明 skill 是包含 `SKILL.md` 的目录，可带 scripts、references、assets 等。它让 Agent 在需要时按需加载任务说明，减少重复提示。

参考：

- [Codex Skills](https://developers.openai.com/codex/skills)
- [Agent Skills overview](https://agentskills.io/home)

MDGA 中 Skills 的定位：

- 任务方法论和工作流层。
- 例如“写项目文档”“执行 TDD”“同步 Obsidian”“审阅代码”“生成 release notes”。
- Skill 可以指导模型调用工具，但 Skill 不是权限边界，也不是执行器。

---

## 4. MDGA 推荐架构

```text
User
  |
  v
Desktop UI
  |
  v
Agent Kernel
  |
  +-- System Prompt / Workspace Context
  +-- DeepSeek Tool Calls
  +-- Activity Event Stream
  |
  v
Permission Manager
  |
  +-- Restricted
  +-- Ask Every Time
  +-- Workspace Auto
  +-- Full Access
  |
  v
Tool Runtime
  |
  +-- Built-in File Tools
  +-- Built-in Command Tool
  +-- MCP Adapter
  +-- Skill Loader
  |
  v
Sandbox Runtime
  |
  v
Local Workspace / External Services
```

核心原则：

- 模型只提出 tool call，不直接声称本地动作已完成。
- MDGA Host 负责执行、审批、审计和结果回传。
- Built-in Tools 是最小可信内核。
- MCP 是扩展协议。
- Skills 是工作流知识。
- Sandbox 是最终执行边界。

---

## 5. 第一版能力范围

### 5.1 必须实现

第一版只做 workspace 内低风险文件工具：

- `list_dir`
- `read_file`
- `create_file`
- `write_file`

共同约束：

- 工具参数只接受相对路径。
- 所有路径相对 conversation workspace。
- 后端使用 canonicalize / normalize 做路径边界判断。
- 禁止 `..` 逃逸、绝对路径直写、UNC 路径绕过。
- 文件写入成功后返回结构化结果。
- 前端显示真实工具事件。
- assistant 只能基于工具结果说“已创建”或“失败”。

### 5.2 暂缓实现

暂缓：

- `run_command`
- 删除文件 / 批量删除
- 跨 workspace 操作
- MCP Marketplace
- Skills Marketplace
- 自动审批分类器
- Git diff / rollback

理由：

- `run_command` 风险高，需要超时、输出截断、进程终止、环境变量隔离和审批策略。
- MCP 和 Skills 在没有基础 Runtime 前会扩大风险面。
- 自动审批需要足够 Activity Event 数据后再做。

---

## 6. DeepSeek Tool Calls 设计方向

DeepSeek 支持 Function Calling / Tool Calls。MDGA 应使用 tool schema 告诉模型可用工具，然后由后端执行工具调用。

参考：

- [DeepSeek Function Calling](https://api-docs.deepseek.com/guides/function_calling)
- [DeepSeek Tool Calls 中文文档](https://api-docs.deepseek.com/zh-cn/guides/tool_calls)

请求流程：

```text
1. 用户：在当前工作区创建 test.txt
2. MDGA：发送 messages + tools schema 给 DeepSeek
3. DeepSeek：返回 tool_call create_file({ path: "test.txt", content: "" })
4. MDGA：Permission Manager 判断是否允许
5. Tool Runtime：真实创建文件
6. MDGA：把 tool result 发回 DeepSeek
7. DeepSeek：基于真实结果回复用户
8. UI：显示工具事件与最终回复
```

关键点：

- 模型不能跳过工具调用直接声称成功。
- 工具失败时，assistant 必须说明失败原因。
- Tool result 要持久化到 conversation event log。

---

## 7. 数据与事件模型

需要新增 Activity Event：

```text
activity_events
  id
  conversation_id
  event_type
  tool_name nullable
  status pending | approved | denied | running | succeeded | failed
  input_json
  output_json nullable
  error_message nullable
  workspace_path_snapshot nullable
  created_at
  completed_at nullable
```

事件类型：

- `tool_requested`
- `permission_requested`
- `permission_decision`
- `tool_started`
- `tool_succeeded`
- `tool_failed`

用途：

- 前端折叠展示 Agent 工作过程。
- 审计 Full Access / Workspace Auto 行为。
- 后续支持重放、调试、撤销和账本分析。

---

## 8. 开发阶段

### Phase 1 - Built-in File Tools MVP ✅ 首个工具闭环已实现

目标：

- 跑通“创建 test.txt 后本地真实存在”。

任务：

- 新增 `crates/tool-runtime` 文件工具实现。✅ `0.0.8` 已实现 `create_file`。
- 新增 workspace path guard。
- 后端 `send_message` 支持 DeepSeek tool calls。✅ `0.0.8` 已接入非流式 tool-call 决策与结果回传。
- 前端显示基础工具事件。
- 测试覆盖 workspace 内创建、越界拒绝、工具结果回传。✅ 已覆盖 create_file、绝对路径拒绝、`..` 越界拒绝和桌面后端桥接。

验收：

- 用户请求“在当前工作区创建 test.txt”后，本地确实出现文件。✅ 后端工具闭环已具备，待 dev 版真实 API 手测。
- 如果模型请求 `../test.txt`，后端拒绝。✅
- UI 显示真实执行结果。

### Phase 2 - Read / Write / List 完整文件工具

目标：

- 让 Agent 能读项目文件、列目录、写 Markdown。

任务：

- `list_dir` 返回名称、类型、大小、mtime。
- `read_file` 支持大小限制和文本编码错误处理。
- `write_file` 支持新建或覆盖策略。
- UI 折叠展示文件工具事件。

验收：

- Agent 能读取当前工作区 README 或 doc 文件并总结。
- Agent 能创建/更新 workspace 内 Markdown。
- 大文件和二进制文件有明确错误。

### Phase 3 - Permission Manager 接入

目标：

- 把工具动作接到 Restricted / Ask Every Time / Workspace Auto / Full Access。

任务：

- 定义 tool risk level。
- workspace 内低风险读写在 Workspace Auto 下自动通过。
- 覆盖写入、越界、绝对路径进入审批或拒绝。
- 持久化审批事件。

验收：

- Restricted 下写文件需要用户确认或被拒绝。
- Workspace Auto 下 workspace 内创建文件自动执行。
- Full Access 下仍记录审计事件。

### Phase 4 - Command Tool

目标：

- 支持安全的 `run_command`。

任务：

- 命令 cwd 默认 conversation workspace。
- 限制超时、输出大小、环境变量传递。
- 支持长进程终止。
- 高风险命令进入审批。

验收：

- 可以运行 `dir`、`git status`、测试命令。
- 删除、网络、安装依赖等动作进入审批。

### Phase 5 - MCP Adapter

目标：

- 支持外部 MCP server，但仍经过 MDGA 权限层。

任务：

- MCP server 注册与启停。
- 工具列表发现。
- 工具 allowlist / denylist。
- MCP tool call 统一转换为 Activity Event。

验收：

- 可连接一个 filesystem 或 GitHub MCP server。
- MCP 工具调用必须可审计、可禁用。

### Phase 6 - Skills

目标：

- 让 MDGA 拥有可复用工作流说明。

任务：

- 定义 MDGA skill 目录规范。
- 加载 `SKILL.md` metadata。
- 按需注入技能说明。
- Skill 可引用 Built-in Tools 或 MCP Tools，但不直接绕过权限。

验收：

- 可以创建“Obsidian 文档同步”“TDD 开发”“代码审阅”等技能。
- 技能只改变 Agent 工作方式，不改变权限边界。

---

## 9. 与现有 Plan 的关系

- `Plan05-Agent-Kernel.md`：负责 Agent loop、tool call 调度和模型交互。
- `Plan06-Security-And-Permissions.md`：负责工具审批策略和权限模式。
- `Plan09-Sandbox-Runtime.md`：负责 OS / process-level 执行边界。
- `Plan11-Agent-Tool-Runtime-And-Extensions.md`：负责工具体系本身，包括 Built-in Tools、MCP Adapter 和 Skills。

Plan11 是当前 MVP 下一步的核心计划。没有 Plan11，workspace 只能停留在“模型知道路径”，无法变成“Agent 能真实完成本地任务”。

---

## 10. 近期实现建议

`0.0.8` 已完成第一条 `create_file` 工具闭环。接下来不应立即跳到 MCP / Skills / 命令执行，而应继续补齐本地文件操作底座，让 DeepSeek 在 workspace 内具备基础开发协作能力。

### 10.1 下一阶段实施顺序

#### Step 1 - 完整文件工具组

优先级最高。目标是让模型在意识到需要查看、修改、删除、列举文件时，能够自行调用工具，而不是要求用户手动操作。

工具：

- `list_dir`
- `read_file`
- `write_file`
- `delete_file`

约束：

- 全部工具只接受 workspace-relative path。
- 全部工具复用同一套 path guard。
- `write_file` 默认覆盖文本文件，但必须返回 `previousExists`、`bytesWritten`。
- `delete_file` 只能删除文件，暂不允许删除目录。
- `read_file` 需要限制文件大小，第一版建议 256 KiB。
- 二进制或不可 UTF-8 解码文件返回明确错误。

验收：

- 用户要求“读取 README 并总结”，模型调用 `read_file`。
- 用户要求“把 test.txt 改成 123456”，模型调用 `write_file`，本地文件真实变化。
- 用户要求“删除 test.txt”，模型调用 `delete_file`，本地文件真实消失。
- 用户要求“看看当前目录有什么”，模型调用 `list_dir`。

#### Step 2 - 工具事件可视化

目标是把 Agent 工作过程从纯文本中分离出来，形成类似 Codex / Claude Code 的折叠过程面板。

事件：

- `tool_requested`
- `tool_started`
- `tool_succeeded`
- `tool_failed`

前端要求：

- 默认折叠。
- 灰度弱化。
- 显示工具名、目标路径、结果摘要。
- 失败时可展开查看错误。

验收：

- 创建、读取、修改、删除文件时，UI 中能看到真实工具事件。
- assistant 最终回复不再伪造执行过程。

#### Step 3 - Diff / Patch 底座

目标是让代码修改可审阅，而不是只做整文件覆盖。

工具：

- `read_file`
- `write_file`
- `apply_patch`
- `show_diff`

约束：

- `apply_patch` 第一版只支持单文件文本 patch。
- patch 应返回 before/after 摘要。
- 后续进入审阅面板和可撤销机制。

验收：

- 用户要求修改某个源码文件时，Agent 能先读文件，再生成 patch，再应用 patch。
- UI 能展示文件 diff。

#### Step 4 - 命令执行与测试闭环

目标是让 Agent 能自主运行低风险测试和检查命令。

工具：

- `run_command`

约束：

- cwd 固定为 conversation workspace。
- 第一版只允许低风险命令，例如 `npm test`、`npm run build`、`cargo test`、`cargo check`、`git status`、`dir`。
- 超时默认 120 秒。
- 输出截断并保留完整日志摘要。
- 高风险命令进入审批或拒绝。

验收：

- Agent 修改代码后可自行运行测试。
- 测试失败时可读取错误并继续修复。
- 测试通过后才能声明完成。

#### Step 5 - Permission Manager 与 Sandbox 深化

目标是让工具能力从“能执行”升级为“可控地执行”。

能力：

- Restricted / Ask Every Time / Workspace Auto / Full Access。
- 越界路径审批。
- 高风险工具审批。
- Activity Event 持久化。
- `.mdgaignore` 或 `.gitignore` 读取策略。

验收：

- Workspace Auto 下，workspace 内读写自动执行。
- 越界请求触发确认或拒绝。
- Full Access 下仍有明显状态提示和审计日志。

### 10.2 当前下一轮开发目标

下一轮最小可交付版本建议为 `0.0.9`：

> 在 `0.0.8 create_file` 的基础上，补齐 `list_dir`、`read_file`、`write_file`、`delete_file` 四个 Built-in File Tools，并接入 DeepSeek Tool Calls，使用户能让 DeepSeek 自主读取、修改、删除 workspace 内的文本文件。

开发原则：

- 继续 TDD：每个工具先写红测，再实现。
- 不引入 MCP / Skills，先补 Built-in Tools。
- 不实现 `run_command`，避免风险面过早扩大。
- 不做复杂 UI，先保证真实本地行为跑通。
- 完成源码变更并经 dev 验证后，必须更新 history、推送 main、推送三段式 release tag。
