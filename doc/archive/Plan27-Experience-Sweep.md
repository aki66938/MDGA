# Plan27（拟并入 0.0.35）- 全面体验复审落地（P0–P2 全做）

项目代号:MDGA
文档定位:落地「全面体验复审」全部项。重点把**模型能力呈现**(推理可见、重生成、ask_vision)与**多供应商纵深**(上下文窗口随供应商)补上,并加可发现性/快捷键/搜索/可访问性/性能/规则等。

> 并行子 agent(后端两路 + 前端一路,文件域不重叠)→ 主控集成验收 → 主创审阅后决定是否并入 0.0.35。

## 1. 范围
| 级别 | 项 | 端 |
|:---:|---|---|
| 🔴#1a | 推理过程可见:流式接 reasoning_content → 折叠「思考过程」卡片 | 全栈 |
| 🔴#2 | 上下文窗口随供应商:provider 增 context_window,压缩阈值据其推导 | 全栈 |
| 🟠#1b | 助手消息「重新生成」 | 全栈 |
| 🟠#1c | `ask_vision(question)` 追问工具(对会话图片二次精读) | 后端 |
| 🟠#3a | 全局快捷键 + Ctrl+K 命令面板 | 前端 |
| 🟠#3b | `/help` 能力披露 | 前端 |
| 🟡#6 | 对话**正文**搜索 | 全栈 |
| 🟡#7 | 菜单键盘可达 + 模态焦点陷阱 | 前端 |
| 🟡#8 | 长会话消息虚拟化 | 前端 |
| 🟡#9 | 从最近被拦动作一键加规则 | 全栈 |

## 2. 分工（并行）
- **Lane A（后端核心）**:`agent_loop.rs`、`commands.rs`、`main.rs`、`storage/lib.rs`、`tools.rs`。做 #1a 发射端、#2 后端、#1c、#1b 后端命令、#6 后端、#9 后端。
- **Lane B（deepseek-client）**:`crates/deepseek-client/**`。做 #1a 流式 reasoning 表面化。
- **Lane D（前端）**:`apps/desktop/src/App.tsx`、`styles.css`。做全部前端项(按 P0→P2 优先级)。

## 3. 跨 lane 契约
- **C1（#1a）**:Lane B 定义 `pub enum StreamChunk<'a> { Content(&'a str), Reasoning(&'a str) }`,把 `chat_stream` / 流式带工具函数的 chunk 回调由 `FnMut(&str)` 改为 `FnMut(StreamChunk)`:delta `content`→`Content`,delta `reasoning_content`→`Reasoning`(content 仍走防泄漏守卫;reasoning 不走守卫)。Lane A 改 `agent_loop.rs:264/288` 与 `chat.rs:82` 回调:`Content`→emit `"chat-chunk"`,`Reasoning`→emit `"chat-reasoning"`(payload 为字符串增量)。Lane D 监听 `"chat-reasoning"`,累积到当前助手消息的一个 `reasoning` part,渲染默认折叠的「思考过程」卡片(流式中实时增长)。
- **C2（#2）**:Lane A:`ModelProvider` 增 `context_window: Option<i64>`(tokens);`upsert/get/list_model_provider`、`save_model_provider` 命令增该字段(camel `contextWindow`);`agent_loop` 软上限 = 主 provider `context_window` × 0.8(取整),无则回退现 `CONTEXT_SOFT_LIMIT_TOKENS`/env。Lane D:ProviderCard 增「上下文窗口(tokens, 可选)」字段,预设可预填(deepseek 1000000 等),保存透传 `contextWindow`。
- **C3（#1c）**:Lane A:`tools.rs` 加 `ask_vision` schema `{ "question": string }`;`agent_loop` 分发 ask_vision——从会话历史(get_messages → 解析 parts_json 的 image part,base64)取图 + 读 vision provider → `analyze_image(question)` → 返回文本结果(无图/未配视觉则返回提示)。前端无需特别处理(作为工具结果卡片展示)。
- **C4（#1b）**:Lane A:命令 `delete_last_assistant_message(conversation_id) -> Result<(), String>`(删该会话最后一条且 role=assistant 的消息;storage 加对应 fn)。Lane D:助手消息(最后一条)加「重新生成」→ 调该命令删旧回复 + 用截至上一条 user 的历史重跑 send_message(不新增 user 消息)。
- **C5（#6）**:Lane A:命令 `search_conversations(query: String) -> Vec<Conversation>`(标题或消息正文 LIKE 命中,按时间倒序)。Lane D:侧栏搜索 query 非空时改调它(防抖),空则恢复本地列表。
- **C6（#9）**:Lane A:命令 `recent_denied_actions() -> Vec<{toolName, target}>`(从 activity events 取近期被拒/权限失败的动作,去重)。Lane D:权限规则设置区列出,每条配「+ 允许 / + 拒绝」按钮(复用 handleAddPermRule 构造规则串)。

## 4. 前端优先级（Lane D，按此序，做不完先报）
P0:#1a 思考卡片、#2 provider 字段 → P1:#1b 重新生成、#3a 快捷键+Ctrl+K 面板、#3b /help、#6 正文搜索 → P2:#7 键盘可达+焦点陷阱、#8 虚拟化、#9 规则 UI。

## 5. 验收
- `cargo build -p mdga-desktop`(0 警告)+ `cargo test --workspace` + `tsc -p tsconfig.json --noEmit` 全绿。
- dev:推理流式折叠卡片;非 DeepSeek 小窗口模型会按真实窗口压缩;重新生成;ask_vision 追问;Ctrl+K 面板/快捷键;/help;正文搜索;菜单键盘可达;长会话流畅;被拦动作一键加规则。
