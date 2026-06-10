# Plan10 - MDGA System Protocols

项目代号：MDGA  
文档定位：本文件定义 MDGA 跨层协议、事件 schema、IPC schema、任务状态和数据对象命名原则。它是后续 Spec 与代码实现的协议总表。

---

## 1. 设计目标

MDGA 的前端、Agent Kernel、Tool Runtime、Permission Manager、Sandbox Runtime、Storage 和 Token Accounting 必须通过稳定协议通信。否则系统会快速退化成难以维护的临时字符串和隐式状态。

目标：

- 统一对象 ID。
- 统一状态枚举。
- 统一 Activity Event。
- 统一 Tool Call / Observation。
- 统一 Permission Request。
- 统一 Token Usage Record。
- 统一 IPC command / event 命名。

---

## 2. ID 命名

建议对象 ID：

- `conversation_id`
- `project_id`
- `workspace_id`
- `task_id`
- `step_id`
- `tool_call_id`
- `observation_id`
- `activity_event_id`
- `artifact_id`
- `change_set_id`
- `permission_request_id`
- `token_usage_id`

原则：

- 所有跨层对象都必须有稳定 ID。
- UI 不用数据库自增 ID 推断业务含义。
- 日志、审计、token 账本和变更集通过 ID 关联。

---

## 3. IPC Command

前端可调用产品动作：

- `create_conversation`
- `rename_conversation`
- `archive_conversation`
- `select_project`
- `select_workspace`
- `send_message`
- `create_task`
- `approve_action`
- `deny_action`
- `cancel_task`
- `get_token_summary`
- `export_token_ledger`

禁止直接暴露：

- `write_file`
- `delete_file`
- `run_command`
- `spawn_process`
- `read_secret`
- `start_sandbox`

IPC 原则：

- command 输入输出必须有 schema。
- command 名称表达产品动作。
- 高权限能力只能由后端内部调用。

---

## 4. Event Stream

前端订阅事件：

- `conversation.updated`
- `message.delta`
- `message.completed`
- `task.created`
- `task.status_changed`
- `activity.created`
- `permission.requested`
- `permission.resolved`
- `token_usage.recorded`
- `change_set.created`
- `error.reported`

事件原则：

- 事件是追加式。
- UI 可以折叠展示，但不能要求后端只返回纯文本。
- 事件可持久化，用于恢复、回放和审计。

---

## 5. Activity Event Schema

字段建议：

- `activity_event_id`
- `conversation_id`
- `task_id`
- `step_id`
- `event_type`
- `summary`
- `detail`
- `visibility`
- `risk_level`
- `status`
- `started_at`
- `completed_at`
- `related_tool_call_id`
- `related_change_set_id`
- `related_token_usage_id`

`event_type` 示例：

- `command.started`
- `command.completed`
- `file.read`
- `file.changed`
- `tool.called`
- `tool.completed`
- `permission.requested`
- `permission.approved`
- `permission.denied`
- `network.requested`
- `token.recorded`

---

## 6. Tool Call / Observation

`ToolCall`：

- `tool_call_id`
- `tool_name`
- `arguments_json`
- `schema_version`
- `permission_capability`
- `created_at`

`Observation`：

- `observation_id`
- `tool_call_id`
- `status`
- `output_summary`
- `output_json`
- `error_code`
- `error_message`
- `created_at`

原则：

- Tool Call arguments 必须 schema 校验。
- Observation 应结构化。
- 长输出进入日志或 artifact，不直接塞满主对话。

---

## 7. Permission Request

字段建议：

- `permission_request_id`
- `task_id`
- `step_id`
- `capability`
- `risk_level`
- `reason`
- `target_path`
- `target_resource`
- `command_preview`
- `decision`
- `decided_by`
- `decided_at`

决策：

- `allow_once`
- `allow_for_workspace`
- `deny`
- `cancel_task`

---

## 8. Token Usage Event

字段与 `Plan04` 对齐：

- `token_usage_id`
- `request_id`
- `api_source`
- `model`
- `conversation_id`
- `task_id`
- `step_id`
- `prompt_tokens`
- `completion_tokens`
- `total_tokens`
- `prompt_cache_hit_tokens`
- `prompt_cache_miss_tokens`
- `reasoning_tokens`
- `usage_source`
- `estimated_cost`
- `pricing_version`

---

## 9. Versioning

所有跨层 schema 需要版本：

- IPC command version。
- Activity Event version。
- Tool schema version。
- Token usage schema version。
- Sandbox policy version。

原则：

- 破坏性变更必须升级版本。
- Storage migration 与 schema version 对齐。
- 前端应能识别未知事件并降级展示。

---

## 10. 验收标准

MVP 验收：

- 核心对象 ID 统一。
- IPC command 不暴露裸系统能力。
- Activity Event 可持久化。
- Permission Request 可持久化。
- Token Usage Record 可关联到 Step。
- UI 可根据事件类型折叠展示。
- 未知事件不会导致前端崩溃。

---

## 11. 当前结论

Plan10 是 MDGA 的协议地基。只要跨层协议稳定，后续 UI 优化、Agent Kernel 增强、沙箱升级、移动端远程控制和插件生态都能在同一套事件与对象模型上演进。
