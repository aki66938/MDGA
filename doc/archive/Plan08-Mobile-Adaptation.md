# Plan08 - MDGA Mobile Adaptation

项目代号：MDGA  
文档定位：本文件定义 MDGA 移动端适配路线。移动端不是系统原生级 Agent，只作为聊天、任务查看、审批和桌面 Agent 远程控制入口。

---

## 1. 设计目标

移动端目标是延伸桌面 Agent，而不是复制桌面 Agent。

移动端应支持：

- 基础聊天。
- 查看桌面端会话。
- 查看任务状态。
- 接收审批请求。
- 远程批准或拒绝动作。
- 查看 token 消耗摘要。
- 接收任务完成通知。

移动端不支持：

- 本地命令执行。
- 手机系统文件控制。
- 移动 OS 权限突破。
- 后台长期自动化。
- 移动端系统原生级 Agent。

---

## 2. 技术路线

可选路线：

- Tauri 2 mobile。
- 原生 Swift / Kotlin。
- React Native / Flutter。

当前倾向：

- 优先保留 Tauri 2 mobile 作为可选路线，因为它与桌面端技术栈接近。
- 但移动端技术选型不能反向约束 PC 端 Rust core。
- 如果 Tauri mobile 在通知、后台、系统集成或商店审核上遇到限制，可以切换原生方案。

参考入口：

- [Tauri Mobile Development](https://v2.tauri.app/develop/)
- [Tauri 2 Overview](https://v2.tauri.app/)

---

## 3. 架构定位

移动端只连接 Sync / Remote Layer。

桌面端负责：

- Agent Kernel。
- Tool Runtime。
- Permission Manager。
- Sandbox Runtime。
- 本地文件操作。
- 命令执行。
- token 账本主记录。

移动端负责：

- 用户输入。
- 状态查看。
- 审批交互。
- 通知。
- 远程控制请求。

移动端不直接访问用户 PC 文件系统，也不直接调用桌面端高权限工具。

---

## 4. MVP 后适配范围

第一阶段移动端能力：

- 登录或绑定桌面端。
- 查看会话列表。
- 发起普通聊天。
- 查看任务列表。
- 查看当前任务状态。
- 接收审批推送。
- 批准或拒绝动作。
- 查看单次任务 token 摘要。

暂缓：

- 移动端本地知识库。
- 移动端插件。
- 移动端文件系统 Agent。
- 移动端后台自动任务。
- 多设备复杂同步冲突。

---

## 5. 远程控制协议

移动端发出的请求应是产品动作：

- `send_message`
- `create_remote_task`
- `approve_action`
- `deny_action`
- `pause_task`
- `cancel_task`
- `get_task_status`

禁止移动端直接发送：

- `run_command`
- `write_file`
- `delete_file`
- `spawn_process`

所有远程请求必须在桌面端 Permission Manager 再次判断。

---

## 6. 安全原则

- 移动端绑定桌面端需要用户确认。
- 审批动作需要明确显示风险。
- 移动端丢失时应能撤销绑定。
- 敏感文件内容默认不推送到移动端。
- API Key 不同步到移动端。
- Full Access 不能因为移动端远程操作而静默扩大。

---

## 7. 验收标准

后续移动端 MVP 验收：

- 手机可以查看桌面端在线状态。
- 手机可以发起普通聊天。
- 手机可以查看任务状态。
- 桌面端高风险动作可以推送到手机审批。
- 手机审批结果能回到桌面端任务流。
- 移动端不保存 DeepSeek API Key。
- 移动端无法直接执行系统命令。

---

## 8. 当前结论

移动端是 MDGA 的远程入口，不是第二个 Agent Runtime。这个边界必须长期保持，否则项目会被移动 OS 权限、应用商店规则和商业入口问题拖入完全不同的战场。
