# Plan06 - MDGA Security And Permissions

项目代号：MDGA  
文档定位：本文件定义 MDGA 的权限模式、安全边界、审批逻辑、审计记录和敏感数据策略。

---

## 1. 设计目标

MDGA 允许 Agent 操作本地文件和命令，因此安全设计必须成为核心架构，而不是后补弹窗。

目标：

- 用户明确选择权限模式。
- 高风险动作可见、可审批、可审计。
- 前端不直接持有系统高权限能力。
- Full Access 可以存在，但必须由用户主动选择。
- API Key 不进入应用存储、日志、诊断包或工具子进程环境。
- 沙箱失败不自动降级为无限制执行。

---

## 2. 权限模式

MVP 保留四档：

- `Restricted`：默认模式，只允许聊天、读取已授权内容和低风险操作。
- `Ask Every Time`：每次高风险动作请求用户确认。
- `Workspace Auto`：授权工作区内自动执行，越界请求确认。
- `Full Access`：技术用户主动开启，允许完整本地执行能力，但保留明显提示和审计。

权限模式是产品策略；沙箱是执行边界。两者不能混为一谈。

---

## 3. 高风险能力

高风险能力包括：

- 读取未授权路径。
- 写入、删除、移动文件。
- 执行命令。
- 启动子进程。
- 访问网络。
- 读取环境变量。
- 调用外部程序。
- 修改 Git 状态。
- 生成可执行脚本。

每个能力需要定义：

- capability id。
- 风险等级。
- 是否可在 Restricted 使用。
- 是否需要审批。
- 是否需要沙箱。
- 是否写入审计。

---

## 4. IPC 安全边界

Tauri 前端只能调用产品动作：

- `create_task`
- `approve_action`
- `cancel_task`
- `select_workspace`
- `get_token_summary`
- `open_review`

前端不能直接调用：

- `write_file`
- `delete_file`
- `run_command`
- `spawn_process`
- `read_secret`
- `start_sandbox`

Tauri 官方 Capability / Permission 系统可用于限制 WebView 可调用命令，但 MDGA 仍需要自己的 Permission Manager，因为产品权限模式、工作区边界、审计和用户审批都属于业务安全逻辑。

参考入口：

- [Tauri Capabilities](https://v2.tauri.app/security/capabilities/)
- [Tauri Permissions](https://v2.tauri.app/security/permissions/)

---

## 5. 审批请求

审批请求至少包含：

- action_id。
- task_id。
- capability。
- target_path 或 target_resource。
- risk_level。
- reason。
- proposed_command。
- permission_mode。
- allow_once / deny / allow_for_workspace。

UI 要求：

- 说明 Agent 想做什么。
- 说明为什么需要。
- 展示影响范围。
- 对文件写入展示目标路径。
- 对命令执行展示命令摘要。
- 对网络访问展示目标域名。

---

## 6. 审计记录

每次高风险动作记录：

- action_id。
- user_decision。
- executed_at。
- permission_mode。
- sandbox_policy_id。
- command_summary。
- target_path。
- result。
- exit_code。
- error。

审计原则：

- 折叠 UI 不影响审计完整性。
- Full Access 不关闭审计。
- 诊断包默认脱敏。
- API Key 永不进入审计。

---

## 7. 敏感数据策略

API Key：

- 只读取 `DEEPSEEK_API_KEY`。
- 不提供应用内输入框。
- 不写 SQLite。
- 不写配置文件。
- 不写 OS Keychain。
- 不传给前端。
- 默认不传给工具子进程。

本地文件：

- 只在授权边界内读取。
- 不自动上传到非 DeepSeek API 目标。
- 诊断包不包含完整隐私文件内容。

---

## 8. 验收标准

MVP 验收：

- 四档权限模式可切换。
- 当前权限模式明显显示。
- 前端无法直接调用高权限命令。
- 高风险动作会进入 Permission Manager。
- Ask Every Time 能阻断并等待用户。
- Workspace Auto 能识别工作区内外。
- Full Access 有醒目标识和审计。
- API Key 不出现在日志、数据库和子进程环境中。

---

## 9. 当前结论

MDGA 的安全策略不是替用户永远锁死能力，而是让用户能清楚选择边界。普通用户默认安全，技术用户可以主动开放能力；无论哪种模式，后端权限网关、审计记录和敏感数据保护都不能被绕过。
