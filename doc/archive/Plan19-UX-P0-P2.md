# Plan19 - 交互体验补强(0.0.29,P0–P2)

项目代号:MDGA
文档定位:基于上一轮 UX 分析,优先修掉**新用户第一公里**与**多模态输入人体工学**两条主线的痛点,并补齐聊天基础肌肉记忆与识图透明度。本版只做 P0–P2 共 7 项。更深一轮 UX 分析待本版验收+主创裁定提交后再做。

> ⚠️ 子 agent 实现 → 主控集成验收(cargo build/test + tsc)→ **不提交**,交主创裁定。

---

## 1. 范围(7 项)

| 优先级 | 项 | 端 |
|:---:|---|---|
| P0a | 首屏过时卡片修正 + 未配供应商引导 | 前端 |
| P0b | composer 支持**粘贴截图 / 拖拽图片** | 前端 |
| P1a | 消息级操作(复制 / 重发 / 编辑重试) | 前端 |
| P1b | 视觉分析**可折叠卡片** + 视觉 usage 显示 | 全栈 |
| P1c | 视觉调用**单独记账**(进 token 账本) | 后端 |
| P2a | 供应商「**测试连接**」按钮 | 全栈 |
| P2b | 审批弹窗显示**动作内容预览** | 全栈 |

## 2. 任务分工(文件域不重叠,可并行)

- **子 agent A(后端 Rust)**:`crates/deepseek-client/`、`apps/desktop/src-tauri/src/{commands.rs, main.rs, agent_loop.rs, permissions.rs}`、`crates/storage/`、`crates/token-accounting/`。
- **子 agent B(前端)**:`apps/desktop/src/App.tsx`、`apps/desktop/src/styles.css`。

两者**只按 §3 契约**对接,互不读改对方文件。

## 3. 接口契约(钉死,双方按此实现)

### C-A 测试连接
- 新 Tauri 命令(在 `commands.rs` 实现并在 `main.rs` 的 `generate_handler!` 注册):
  ```
  test_provider_connection(base_url: String, api_key: String, model: String, api_format: String) -> Result<String, String>
  ```
  - 行为:用入参做一次**最小请求**(max_tokens 极小、prompt 形如 "ping"),`api_format=="anthropic"` 走 `/v1/messages`(x-api-key + anthropic-version,纯文本 user 消息),否则走 `/chat/completions`(Bearer)。base_url 用 §既有的 `chat_completions_url` / `anthropic_messages_url` 容错拼接逻辑。
  - 返回:成功 `Ok("连接成功")`;失败 `Err(<classify_api_error 人话化文案>)`。
  - 注:Key 为空时,前端先拦截不调用(已配状态下点测试,前端把已存的 key 传空 → 命令内若 key 空则用 DB 已存 key:**为简化,前端在用户输入了新 key 时传新 key,否则传空串,命令内 key 空则从 DB 读该 role 既有 key**)。命令额外接收 `role: String` 以便回读 DB key。最终签名:
  ```
  test_provider_connection(role, base_url, api_key, model, api_format) -> Result<String,String>
  ```
- 前端(ProviderCard):保存按钮旁加「测试连接」按钮 → 调用命令 → 结果就地显示(成功绿色 / 失败红色 `var(--danger)`),不跳主界面。

### C-B 视觉分析可见 + 记账
- `analyze_image` 改为返回 `(String, Option<mdga_shared::RawUsage>)`:OpenAI 取 `usage`(prompt/completion/total),Anthropic 取 `usage`(input_tokens/output_tokens)→ 归一到 RawUsage。解析缺失则 `None`。(同步更新 vision.rs 现有 6 测)
- `agent_loop.rs` 自动初看完成后:
  1. **emit 事件** `"vision-analysis"`,payload:
     ```json
     { "conversationId": "<id>", "count": <图片数>, "analysis": "<文本>", "usage": <RawUsage|null> }
     ```
  2. **持久化**:把同一信息作为**助手消息 parts_json 的首个 part**:
     ```json
     { "type": "vision", "count": <N>, "analysis": "<文本>", "usage": <RawUsage|null> }
     ```
     使重载会话后卡片仍在(沿用现有 record_tool_event/parts 持久化范式)。
  3. **记账**:视觉 usage 写入 token 账本,**单独条目**(kind/tag 标记为 `vision`,与主模型分开),保证 CSV 导出含视觉开销;不要把视觉 usage 合并进主助手消息的 usage 徽标。
- 前端:
  - `MessageContent` / parts 渲染新增 `type==="vision"` 分支 → **默认折叠的「视觉分析」卡片**(标题如「🔎 视觉分析(N 张图)」,展开见 analysis 文本;若有 usage 显示小徽标 `视觉 · {total} tokens`)。
  - 同时监听 `"vision-analysis"` 事件,在当前回复流中即时插入该卡片(与持久化 part 二选一渲染,避免重复:实时事件用于"发送中"即时显示,重载后用持久化 part)。

### C-C 审批动作预览
- `permissions.rs` 的 `request_tool_approval` emit `"approval-request"` 时**新增 `preview` 字段**:
  - `run_command` → 命令全文(`command` 参数)。
  - `write_file` / `create_file` → 内容前 ~40 行或 ~2KB(截断标注)。
  - `apply_patch` / 编辑类 → diff 文本(若参数已含)。
  - 其它 → 空串。
  - 实现:新增 `fn approval_preview(tool_name, arguments) -> String`。
  ```json
  { "actionId","toolName","target","preview": "<string>" }
  ```
- 前端:`ApprovalRequest` 类型加 `preview?: string`;`ApprovalModal` 在 hint 上方,`preview` 非空时渲染**等宽、可滚动的预览块**(命令/内容/diff),让用户"看清再点允许"。

### C-D 纯前端项(无后端依赖)
- **P0a 首屏**:
  - 删除/重写 `App.tsx:1444-1455` 的三张过时卡片(尤其"只从环境变量读取 API Key,不在应用内保存"是**事实错误**)。改为反映现状:应用内配置供应商、成本透明、权限分级。
  - 挂载时查主模型是否已配(用现有 `get_deepseek_api_key_status` 或 `get_model_provider_config('main')`)→ **未配则在首屏显著位置给 CTA**「先去 设置 → 模型供应商 配置模型」,点击直接打开设置到「模型供应商」分类。
- **P0b 粘贴/拖拽图片**:
  - composer `<textarea>`(及 composer 容器)加 `onPaste` / `onDrop` / `onDragOver`:从剪贴板/拖拽取 image Blob → `FileReader` 读为 base64(去 `data:` 前缀)→ 校验类型(png/jpg/jpeg/gif/webp)+ 大小(≤10MB)→ push 进 `pendingImages`(复用现有缩略图托盘)。
  - **门禁**:仅在已配视觉模型时接受(与现 📎 图片门禁一致);未配时给与现有一致的拒绝提示。
- **P1a 消息级操作**:
  - 消息气泡 hover 显示操作条:**复制**(整条文本到剪贴板)、对**用户消息**提供**重发**与**编辑重试**(把该用户消息文本回填 composer 供修改后再发 / 或直接重发)。复用现有 `handleSend` 路径。

## 4. 验收(本计划范围)
- `cargo build -p mdga-desktop` + `cargo test --workspace` + `tsc --noEmit` 全绿、零新增警告。
- 主创 dev 走查:① 新装/未配供应商首屏有正确引导、无过时文案;② 截图 Ctrl+V / 拖拽进 composer 能识图;③ 消息可复制/重发/编辑重试;④ 识图时出现可折叠视觉分析卡片、账本含视觉开销;⑤ 供应商「测试连接」即时反馈;⑥ 审批弹窗显示命令/内容预览。
- 通过后由主创裁定是否提交、定版本 0.0.29。

## 5. 不在本计划
- 更深一轮 UX 重审(待本版验收+裁定后)。
- 控制行模型下拉联动自定义供应商(P3)、折叠摘要聚合数字、批量审批等。
- 音频模态、订阅制模型。
