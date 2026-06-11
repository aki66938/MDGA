# Spec01 - Workspace Session Binding

项目代号：MDGA
文档定位：定义“新对话时选择工作区、整轮对话绑定工作区、Agent 文件能力受工作区边界约束”的 MVP 设计。本文承接 `Plan03-Desktop-MVP.md`、`Plan06-Security-And-Permissions.md` 和 `Plan09-Sandbox-Runtime.md`。

---

## 1. 背景与参考

用户与 Agent 做本地开发协作时，最重要的上下文不是“当前 UI 正在显示哪个目录”，而是“这一轮会话从一开始被授权在哪个项目目录内工作”。工作区必须是 session 级边界，而不是普通对话正文中的临时表单。

公开资料可参考：

- Codex App：官方文档说明 Codex 桌面端支持跨项目多任务，一个 project 类似在特定目录中启动一个 session；多 app / package 仓库建议拆成不同 project，使 sandbox 只包含该 project 文件。参考：[Codex app features](https://developers.openai.com/codex/app/features)。
- Codex Sandbox：官方文档说明 sandbox 是 Agent 自主行动的技术边界，文件修改和命令执行应受当前 workspace 边界限制；越界交给 approval flow。参考：[Codex Sandbox](https://developers.openai.com/codex/concepts/sandboxing) 与 [Agent approvals & security](https://developers.openai.com/codex/agent-approvals-security)。
- Claude Code Desktop：官方 quickstart 在开始 session 时要求选择 Local 环境并点击 Select folder 选择项目目录；集成终端会在 session 的 working directory 中打开。参考：[Claude Code desktop quickstart](https://code.claude.com/docs/en/desktop-quickstart) 与 [Claude Code desktop application](https://code.claude.com/docs/en/desktop)。
- Claude Code Desktop issue：社区曾反馈新 session 缺少选择 working directory 的入口会导致多项目工作困难，期望 New session 时通过 folder picker 或 project selector 选择目录。参考：[anthropics/claude-code#36175](https://github.com/anthropics/claude-code/issues/36175)。

结论：MDGA 应把工作区选择放在新对话 / 新 session 创建入口，而不是放在普通聊天正文中反复显示。

---

## 2. 产品目标

核心目标：

- 帮助用户明确“本轮对话在哪个项目目录内进行”。
- 帮助 Agent 在工作区内读取项目结构、生成或修改项目相关 Markdown 提示词文档、执行后续本地文件任务。
- 减少误操作和上下文混乱，避免用户在多个项目之间恢复会话时误把 Agent 带到错误目录。
- 为后续 Permission Manager、Tool Runtime、Sandbox Runtime 提供稳定的 session 级工作区边界。

非目标：

- 不在普通对话正文中持续显示工作区绑定表单。
- 不要求 MVP 支持多根 workspace。
- 不在 MVP 阶段实现复杂项目索引、Git worktree、跨项目搜索或云同步。
- 不允许 Agent 在无提示情况下越过当前工作区操作文件。

---

## 3. 核心交互

### 3.1 新对话首屏

新对话首屏需要提供工作区选择入口：

- 主按钮：`选择工作区`
- 次状态：未选择时显示 `未绑定工作区，仅聊天模式`
- 已选择时显示工作区名称，例如 `MDGA`
- 目录路径作为弱化副信息或 tooltip，不进入普通对话正文

用户点击 `选择工作区` 后，应打开系统层目录选择控件，而不是要求用户复制粘贴路径。

MVP 推荐使用 Tauri dialog 插件：

- 前端调用受控产品动作，例如 `select_workspace_for_new_session`
- 后端或 Tauri 插件打开系统目录选择器
- 用户取消选择时不创建或不修改工作区
- 用户选定目录后，后端校验路径存在且为目录

### 3.2 创建 session

用户发送第一条消息或点击“开始对话”时：

- 如果已选择工作区，新 session 绑定该 workspace。
- 如果未选择工作区，新 session 标记为 `projectless` / `chat_only`。
- 绑定后，该 session 的工作区默认不可在正文中修改。

### 3.3 会话中行为

普通对话正文不再显示工作区输入框或绑定表单。

会话中可以保留轻量定位信息，但不能干扰主对话：

- 顶部标题或侧边栏可显示 project basename。
- 侧边栏会话项可后续增加弱化的 workspace basename。
- 需要完整路径时使用 hover、详情面板或会话信息入口。

### 3.4 跳出工作区

如果用户在提示词中明确要求跳出当前工作区处理文件，例如：

- “去桌面另一个目录里找文件”
- “读取 C:\Users\AIT\Downloads 下的资料”
- “把结果复制到当前项目外”

则 Agent 不应静默执行。行为应进入权限判定：

- Restricted：拒绝或要求用户重新选择/授权目录。
- Ask Every Time：明确展示越界路径、动作类型、风险，并请求确认。
- Workspace Auto：工作区内自动执行，越界必须确认。
- Full Access：可放宽，但仍应记录审计日志并显示明显状态。

---

## 4. 数据模型

MVP 需要从“全局单个活动工作区”调整为“session 绑定工作区”。

建议表：

```text
projects
  id
  name
  root_path
  created_at
  updated_at
  last_used_at

workspaces
  id
  project_id
  root_path
  display_name
  created_at
  updated_at

conversations
  id
  title
  workspace_id nullable
  workspace_path_snapshot nullable
  workspace_name_snapshot nullable
  mode chat_only | local_workspace
  created_at
  updated_at
```

说明：

- `workspace_path_snapshot` 用于保证历史会话能显示创建时的目录，即使项目记录后续被改名或删除。
- MVP 可以先不拆 `projects` 表，只在 `conversations` 上保存 workspace snapshot；但后续项目列表和最近项目需要 `projects`。
- 当前已实现的 `workspaces` 单表全局 active 方案只能作为技术验证，不应作为最终产品交互。

---

## 5. 后端边界

Tauri command 不应暴露裸文件系统能力给前端。建议命令按产品动作命名：

- `list_recent_workspaces`
- `select_workspace_for_new_session`
- `create_conversation_with_workspace`
- `get_conversation_workspace`
- `clear_draft_workspace_selection`

目录选择器能力应受控：

- 前端只触发“选择工作区”动作。
- 系统目录选择控件由 Tauri dialog 或后端包装层打开。
- 后端对返回路径做存在性、目录类型、规范化路径校验。
- 保存前记录 workspace basename、absolute path、创建时间。

---

## 6. Agent 与工具边界

会话绑定工作区后：

- Agent 默认 cwd = session.workspace_path。
- 文件读取、写入、创建、删除默认限制在 workspace_path 内。
- 命令执行默认在 workspace_path 内启动。
- Activity Event 必须记录 cwd、目标路径和是否越界。
- Token Accounting 与 Activity Event 应关联 conversation_id，后续可关联 workspace_id。

路径判断需要使用规范化后的绝对路径，不能仅靠字符串前缀判断。Windows 下应注意：

- 大小写不敏感。
- 符号链接 / junction 可能导致路径逃逸。
- `..`、短路径名、UNC 路径需要规范化后再判断。

MVP 可先做基础 `canonicalize` 与 `starts_with` 检查；完整防逃逸逻辑进入 `Plan09-Sandbox-Runtime.md`。

---

## 7. 实现难度评估

该功能不是大型工程模块，但也不是简单 UI 调整。它属于中等复杂度功能，原因是它横跨：

- 前端新对话首屏交互。
- Tauri 系统目录选择插件与 capability 配置。
- SQLite schema 迁移。
- conversation 与 workspace 关系调整。
- 后续 Permission Manager / Tool Runtime 的 cwd 与路径边界。
- 旧的全局 active workspace UI 需要撤下或迁移。

建议分两步开发：

1. 先修正产品形态：新对话首屏使用系统目录选择器，创建 conversation 时绑定 workspace snapshot；普通对话正文不显示工作区表单。
2. 再接权限与 Agent 文件能力：所有文件工具和命令工具默认限制在 conversation workspace 内，越界走审批。

---

## 8. MVP 验收

第一阶段验收：

- 新对话首屏存在 `选择工作区` 按钮。✅ 已在 `0.0.6` 实现。
- 点击后打开系统目录选择器。✅ 已在 `0.0.6` 通过 Tauri dialog 实现。
- 选中目录后，新对话首屏显示 workspace basename。✅ 已在 `0.0.6` 实现。
- 发送第一条消息后，conversation 绑定该 workspace。✅ 已在 `0.0.6` 通过 conversation workspace snapshot 实现。
- 切换历史会话时能恢复该 conversation 的 workspace 信息。
- 普通对话正文不显示工作区路径输入框。✅ 已在 `0.0.6` 实现。
- 未选择工作区也可以开始纯聊天会话。✅ 已在 `0.0.6` 以 `chat_only` mode 支持。

第二阶段验收：

- Agent 文件工具默认只能访问 conversation workspace。
- 明确越界请求会触发权限确认或拒绝。
- Activity Event 记录 workspace、cwd 和路径边界判断结果。

---

## 9. 对当前实现的修正方向

`0.0.5` 曾实现：

- `workspaces` 表。
- 手动输入路径绑定。
- 后端校验路径存在且为目录。
- UI 在普通页面显示当前工作区绑定表单。

`0.0.6` 已完成第一阶段修正：

- 去掉普通对话正文里的路径输入绑定表单。
- 引入系统目录选择器。
- 把工作区从“全局 active workspace”改成“conversation 创建时绑定 workspace snapshot”。
- 会话列表读取 conversation 自身的 workspace snapshot，不再依赖全局 active workspace 展示。

当前实现保留价值：

- storage 与 Tauri command 的路径校验思路已复用到 `new_conversation_with_workspace`。
- `Workspace` DTO 与全局 active workspace API 暂时保留为历史兼容代码，后续接入项目列表时再清理或迁移。
- 测试已改写为“新 session 选择目录后绑定 conversation”。
