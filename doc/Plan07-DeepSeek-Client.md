# Plan07 - MDGA DeepSeek Client

项目代号：MDGA  
文档定位：本文件定义 MDGA 唯一模型接入层 DeepSeek Client 的认证、请求、流式输出、Tool Calls、JSON Output、usage 解析和错误处理方案。

---

## 1. 设计目标

MDGA 只服务 DeepSeek API，不设计多模型 Provider 抽象。DeepSeek Client 应轻量、稳定、可测试，并把 DeepSeek 官方响应转为 MDGA 内部稳定结构。

目标：

- 从 `DEEPSEEK_API_KEY` 读取 API Key。
- 使用 Bearer Auth。
- 支持普通 Chat Completion。
- 支持流式输出。
- 支持 `stream_options.include_usage`。
- 支持 Tool Calls。
- 支持 JSON Output。
- 解析 usage、cache hit / miss、reasoning tokens。
- 分类错误并返回用户可理解信息。

---

## 2. 认证策略

唯一认证来源：

- 环境变量 `DEEPSEEK_API_KEY`。

禁止：

- 应用内输入 API Key。
- 将 API Key 保存到数据库。
- 将 API Key 保存到配置文件。
- 将 API Key 保存到 OS Keychain。
- 在日志、诊断包或前端状态中暴露 API Key。

启动时策略：

- 应用启动读取环境变量状态。
- 设置页提供重新检测。
- 连接测试只返回脱敏状态。
- 缺失时展示 Windows / macOS / Linux 环境变量指引。

---

## 3. 请求能力

MVP 支持：

- `model`
- `messages`
- `stream`
- `stream_options.include_usage`
- `tools`
- `tool_choice`
- `response_format`
- `thinking`
- `reasoning_effort`
- `max_tokens`

请求原则：

- 默认使用 DeepSeek 当前推荐模型。
- 模型名不作为 Provider 抽象，只作为 DeepSeek 模型族参数。
- JSON Output 必须配套系统提示或用户提示要求输出 JSON。
- Tool Calls 返回的 arguments 必须由 Tool Runtime 再次校验，不能信任模型生成 JSON。

---

## 4. 流式输出

流式处理要求：

- 将 delta 内容实时发给 UI。
- 将 reasoning 内容与最终 content 区分。
- 支持用户中断。
- 支持超时。
- 最终 usage chunk 到达后写入 Token Accounting。
- 如果流中断且 usage 缺失，记录为 `missing` 或 `local_estimate`。

DeepSeek 文档说明 `stream_options.include_usage` 会在 `[DONE]` 前返回额外 chunk，其中 usage 是整次请求统计。

---

## 5. Tool Calls

DeepSeek Client 只负责解析模型提出的工具调用：

- tool call id。
- function name。
- arguments string。

不负责：

- 执行工具。
- 判断权限。
- 修改本地文件。
- 自动信任 arguments。

Tool Runtime 必须对 arguments 做 schema 校验，并由 Permission Manager 判断权限。

---

## 6. Usage 解析

需要解析：

- `prompt_tokens`
- `completion_tokens`
- `total_tokens`
- `prompt_cache_hit_tokens`
- `prompt_cache_miss_tokens`
- `completion_tokens_details.reasoning_tokens`

解析原则：

- 保存原始 usage JSON。
- 标准化为 Token Accounting 字段。
- 缺失字段不猜测官方值。
- 本地估算只能作为估算值。

---

## 7. 错误分类

错误至少分为：

- API Key 缺失。
- 认证失败。
- 余额不足。
- 限流。
- 请求参数错误。
- 上下文超限。
- 网络失败。
- 服务端错误。
- 流式中断。
- JSON Output 无效。
- Tool Calls arguments 无效。

UI 文案需要人话化，例如：

- “没有检测到 `DEEPSEEK_API_KEY` 环境变量。”
- “DeepSeek 返回认证失败，请检查环境变量是否正确。”
- “请求被限流，稍后可重试。”

---

## 8. 官方 coding plan 状态

当前只接入 DeepSeek API。

DeepSeek 官方文档提供 Claude Code、OpenCode、OpenClaw、Deep Code 等编码工具集成说明，但这些是第三方工具接入 DeepSeek 模型的方式，不是 MDGA 的登录、订阅或计费入口。

如果 DeepSeek 后续开放官方 coding plan 或独立编码产品 API，应在本文件中新增章节重新评估，但 MVP 不为其设计实现分支。

参考入口：

- [DeepSeek API Authentication](https://api-docs.deepseek.com/api/deepseek-api)
- [DeepSeek Create Chat Completion](https://api-docs.deepseek.com/api/create-chat-completion)
- [DeepSeek Integrate with AI Tools](https://api-docs.deepseek.com/guides/coding_agents)
- [DeepSeek Integrate with Deep Code](https://api-docs.deepseek.com/quick_start/agent_integrations/deepcode)

---

## 9. 验收标准

MVP 验收：

- 未设置 API Key 时连接测试失败且提示清楚。
- 设置 API Key 后连接测试成功。
- 普通聊天可用。
- 流式输出可用。
- 最终 usage 可写入 token 账本。
- Tool Calls 可解析。
- JSON Output 可请求。
- 典型错误可分类。
- 日志不包含 API Key。

---

## 10. 当前结论

DeepSeek Client 应保持单一、轻量和专用。MDGA 的价值不是做模型路由器，而是把 DeepSeek API 包装成本地优先、权限透明、成本可见的个人 Agent 桌面产品。
