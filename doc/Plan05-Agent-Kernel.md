# Plan05 - MDGA Agent Kernel

项目代号：MDGA  
文档定位：本文件定义 MDGA Agent Kernel 的核心对象、任务状态机、Planner / Executor 边界、上下文组装与最小可运行闭环。

---

## 1. 设计目标

Agent Kernel 负责把用户自然语言目标转化为可执行任务。它不直接操作系统资源，不直接写 UI，不直接绕过权限层，而是通过 Tool Runtime、Permission Manager、Sandbox Runtime 和 Storage 完成任务执行。

核心目标：

- 管理 Conversation、Task、Step 的生命周期。
- 组织上下文。
- 调用 DeepSeek Client 生成计划或下一步动作。
- 调用 Tool Runtime 执行工具。
- 处理暂停、恢复、取消、失败和重试。
- 生成 Activity Event。
- 把 token usage、工具结果和文件变更关联回任务。

---

## 2. 核心对象

- `Conversation`：用户与 Agent 的持续对话。
- `Task`：一次明确目标。
- `Step`：Task 内的一步计划或动作。
- `Plan`：由 Planner 生成的步骤集合。
- `ToolCall`：模型请求工具调用或系统内部工具调用。
- `Observation`：工具执行结果。
- `ActivityEvent`：用户可见、可折叠、可审计的过程事件。
- `Artifact`：生成文件或结果。
- `ChangeSet`：文件变更集合。

对象关系：

- Conversation 包含多个 Task。
- Task 包含多个 Step。
- Step 可以产生 ToolCall、Observation、ActivityEvent、TokenUsageRecord 和 ChangeSet。
- Artifact 可以属于 Task，也可以绑定 Conversation。

---

## 3. 任务状态机

`TaskStatus` 建议：

- `created`
- `planning`
- `awaiting_approval`
- `running`
- `paused`
- `completed`
- `failed`
- `cancelled`

状态原则：

- 只有 Agent Kernel 可以推进 Task 状态。
- 权限审批中必须进入 `awaiting_approval`。
- 用户取消必须尽快停止后续步骤。
- 失败需要保存失败原因和可重试状态。
- 完成状态必须关联最终结果或 artifact。

---

## 4. Planner 边界

Planner 负责：

- 理解用户目标。
- 拆解 1-N 个步骤。
- 识别需要的工具能力。
- 判断是否需要用户补充信息。
- 估算高风险动作。

Planner 不负责：

- 实际执行工具。
- 决定权限是否允许。
- 读写本地文件。
- 修改 UI。

MVP Planner 可以很简单：先生成 1-3 步计划，并在每一步执行后根据 Observation 决定是否继续。

---

## 5. Executor 边界

Executor 负责：

- 取出下一步 Step。
- 生成工具调用请求。
- 调用 Tool Runtime。
- 接收 Observation。
- 记录 Activity Event。
- 推进任务状态。

Executor 不负责：

- 绕过 Permission Manager。
- 直接执行命令。
- 直接访问数据库底层细节。
- 自行吞掉失败。

---

## 6. Context Manager

Context Manager 负责为每次模型请求组装上下文：

- 当前用户消息。
- 会话摘要。
- 任务目标。
- 当前计划。
- 最近 Step 和 Observation。
- 当前 Project / Workspace。
- 权限模式。
- token 预算。
- 必要文件片段。

原则：

- 不把完整工具日志无脑塞入上下文。
- 不把敏感环境变量、API Key、完整隐私文件放入上下文。
- reasoning 内容不应原样拼入下一轮上下文，除非后续 DeepSeek 文档允许且设计明确。
- 需要记录每次上下文组装的来源，方便诊断成本和失败原因。

---

## 7. 最小 Agent 闭环

MVP 场景：

1. 用户要求在工作区创建一个 Markdown 文件。
2. Agent Kernel 创建 Task。
3. Planner 生成步骤。
4. Executor 调用 `write_file` 工具。
5. Tool Runtime 校验参数。
6. Permission Manager 判断是否允许。
7. Sandbox Runtime 或文件工具执行写入。
8. Change Manager 记录变更。
9. Activity Event 进入 UI。
10. DeepSeek Client 生成最终回复。
11. Token Accounting 记录 usage。
12. Task 进入 `completed`。

---

## 8. 错误处理

错误类型：

- 模型错误。
- 上下文超限。
- 工具参数错误。
- 权限拒绝。
- 沙箱失败。
- 文件系统错误。
- 用户取消。
- 任务超时。

处理原则：

- 用户拒绝权限不是系统失败，应转为可解释结果。
- 沙箱失败不能自动降级为无限制执行。
- 工具错误必须结构化返回。
- 可重试错误需要保留 retry 入口。
- 不可重试错误需要说明原因。

---

## 9. 验收标准

MVP 验收：

- 可以创建 Task。
- Task 状态机可持久化。
- Planner 能生成最小计划。
- Executor 能执行至少一个工具。
- 权限审批能中断并恢复任务。
- 工具 Observation 能回到模型上下文。
- Activity Event 能完整记录。
- 任务完成后能关联 token usage 和 ChangeSet。

---

## 10. 当前结论

Agent Kernel 是 MDGA 从聊天工具走向个人工作台的核心，但 MVP 不需要一开始就追求复杂自主规划。第一版应把任务状态机、上下文、工具调用、权限审批和事件记录打通，再逐步增强 Planner 智能。
