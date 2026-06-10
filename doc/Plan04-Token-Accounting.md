# Plan04 - MDGA Token Accounting

项目代号：MDGA  
文档定位：本文件定义 MDGA 的 token 统计、费用估算、价格版本化与账单对照方案。它是 MVP 早期功能，不依赖完整 Agent Kernel。

---

## 1. 设计目标

MDGA 的 token 账本不是附属统计页，而是产品差异化能力。目标是让用户能理解每一次 DeepSeek API 调用消耗了多少 token、哪些输入命中了缓存、输出与 reasoning token 占比多少，以及估算费用如何得出。

核心目标：

- 记录每次请求的服务端原始 usage。
- 标准化 prompt、completion、total、cache hit、cache miss、reasoning tokens。
- 按请求、会话、任务、日期和模型聚合。
- 保存价格版本，支持未来价格变化后的历史费用回放。
- 支持用户导出账本，用于和 DeepSeek 官方账单人工对照。
- 在 usage 缺失时明确标注估算或未知，不伪装成官方返回。

---

## 2. 官方事实边界

根据 DeepSeek 官方文档：

- DeepSeek 按 token 计费，官方建议以模型返回的 usage 作为实际 token 数依据。
- Chat Completion usage 包含 `prompt_tokens`、`completion_tokens`、`total_tokens`。
- usage 中包含 `prompt_cache_hit_tokens` 与 `prompt_cache_miss_tokens`，用于反映上下文缓存命中状态。
- `completion_tokens_details.reasoning_tokens` 用于记录 reasoning token。
- 流式输出可通过 `stream_options.include_usage` 在 `[DONE]` 前获得整次请求 usage。
- 价格可能调整，必须以价格版本记录计算依据。

参考入口：

- [DeepSeek Models & Pricing](https://api-docs.deepseek.com/quick_start/pricing)
- [DeepSeek Token & Token Usage](https://api-docs.deepseek.com/quick_start/token_usage)
- [DeepSeek Create Chat Completion](https://api-docs.deepseek.com/api/create-chat-completion)
- [DeepSeek Context Caching](https://api-docs.deepseek.com/guides/kv_cache)

---

## 3. MVP 记录字段

`token_usage_record` 至少包含：

- `id`
- `request_id`
- `api_source`，固定为 `deepseek`
- `model`
- `conversation_id`
- `task_id`
- `step_id`
- `mode`，例如 `chat`、`agent_planning`、`agent_execution`、`summary`
- `prompt_tokens`
- `completion_tokens`
- `total_tokens`
- `prompt_cache_hit_tokens`
- `prompt_cache_miss_tokens`
- `reasoning_tokens`
- `raw_usage_json`
- `usage_source`，例如 `deepseek_usage`、`local_estimate`、`missing`
- `estimated_cost`
- `currency`
- `pricing_version`
- `created_at`

字段原则：

- 服务端返回什么，原始 JSON 就保存什么。
- 标准化字段用于 UI 和聚合。
- 价格字段只保存本次计算依据，不覆盖历史记录。
- 缺失字段保留为空或 0，但必须通过 `usage_source` 区分。

---

## 4. 费用计算模型

费用计算按价格版本进行：

```text
input_cache_hit_cost = prompt_cache_hit_tokens / 1_000_000 * cache_hit_input_price
input_cache_miss_cost = prompt_cache_miss_tokens / 1_000_000 * cache_miss_input_price
output_cost = completion_tokens / 1_000_000 * output_price
estimated_cost = input_cache_hit_cost + input_cache_miss_cost + output_cost
```

如果 API 只返回 `prompt_tokens`，但没有 cache hit / miss：

- `prompt_tokens` 进入普通 input 估算。
- `usage_source` 标记为 `mixed` 或 `deepseek_usage_without_cache_breakdown`。
- UI 明确说明无法拆分缓存命中费用。

如果没有 usage：

- 可以使用本地 tokenizer 或估算规则记录 `local_estimate`。
- UI 必须标注“估算值”。
- 估算值不应参与严肃账单对照，只能用于成本感知。

---

## 5. 价格版本化

建立 `pricing_snapshot` 表：

- `pricing_version`
- `api_source`
- `model`
- `currency`
- `input_cache_hit_price_per_1m`
- `input_cache_miss_price_per_1m`
- `output_price_per_1m`
- `source_url`
- `source_checked_at`
- `effective_note`
- `created_at`

原则：

- MVP 可以手工内置一份价格快照。
- 每次价格更新新增版本，不改旧版本。
- token 记录引用 `pricing_version`。
- UI 显示费用时标注价格版本。
- 后续可以增加“重新按最新价格回放计算”，但默认保留历史计算结果。

---

## 6. UI 展示范围

MVP 展示：

- 当前请求 token。
- 当前会话累计 token。
- 当前任务累计 token。
- 输入 token、输出 token、reasoning token。
- cache hit / miss token。
- 本次估算费用。
- usage 来源。

后续展示：

- 日 / 周 / 月成本趋势。
- 按项目聚合。
- 按任务聚合。
- 按模型聚合。
- 费用异常提醒。
- 导出 CSV / JSON。
- 与官方账单的差异说明。

展示原则：

- 默认展示简洁数字，不让普通用户陷入字段细节。
- 详情页保留完整拆分。
- 不把估算值显示成官方账单。

---

## 7. 账单对照

MVP 只做用户导出对照，不做自动拉取官方账单。

导出内容：

- 请求时间。
- request_id。
- model。
- conversation_id。
- task_id。
- token 字段。
- 费用字段。
- pricing_version。
- usage_source。

对照原则：

- MDGA 账本用于解释本地使用行为。
- DeepSeek 官方账单是扣费事实来源。
- 如果两者不一致，优先检查价格版本、时区、赠送余额、舍入规则、缓存字段和失败请求。

---

## 8. 验收标准

MVP 验收：

- 完成一次普通聊天后生成 token 记录。
- 流式输出启用最终 usage 获取。
- cache hit / miss 字段可保存和展示。
- reasoning token 可保存和展示。
- 会话累计 token 正确聚合。
- pricing_version 写入每条记录。
- usage 缺失时 UI 明确标注。
- 可导出 CSV 或 JSON 至本地文件。

---

## 9. 当前结论

Token Accounting 应在桌面 MVP 早期实现。它不依赖完整 Agent Kernel，却能让用户从第一天起理解成本水平，也能帮助后续评估上下文压缩、缓存复用、任务拆分和模型参数策略是否有效。
