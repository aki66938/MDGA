# Plan20 - 交互逻辑修复(🔴致命 + 🟠较重)

项目代号:MDGA
文档定位:基于「交互逻辑深度重审」,修掉全部 🔴致命 与 🟠较重 项,合并为一个版本(0.0.29 已发版,本批落下一版,版本号由主创在提交时定)。🟡次要打磨本计划不做。

> 子 agent 并行实现(后端 Rust / 前端 App.tsx,文件域不重叠)→ 主控集成验收(cargo build/test + tsc)→ 交主创 dev 走查 + 裁定提交。

## 1. 范围(7 项)

| 级别 | 项 | 端 |
|:---:|---|---|
| 🔴1 | 主模型「配了不生效」:实际模型用 provider.model_id,而非控制行写死的 DeepSeek 值 | 全栈 |
| 🔴2 | 首屏反馈静默蒸发:引入独立全局 toast,错误类提示不再依赖「最后一条是助手消息」 | 前端 |
| 🔴3 | 单条会话删除加二次确认 | 前端 |
| 🟠4 | Agent 工作时可向上翻看:粘底自动滚动 + 上滚暂停 + 「跳到最新」 | 前端 |
| 🟠5 | 发送失败时恢复附图(不丢) | 前端 |
| 🟠6 | 跨会话状态残留:新建/切换会话重置 pendingImages/planMode/queuedSteering | 前端 |
| 🟠7 | 泄漏清洗保留工具/视觉卡片(只截尾部 text part) | 前端 |

## 2. 分工(可并行,文件域不重叠)

- **子 agent A(后端 Rust)**:`apps/desktop/src-tauri/src/agent_loop.rs`(+ 如需 `crates/token-accounting/`)。只做 🔴1 后端。
- **子 agent B(前端)**:`apps/desktop/src/App.tsx`、`apps/desktop/src/styles.css`。做 🔴1 前端 + 🔴2、🔴3、🟠4–7。

## 3. 🔴1 设计定调(单一真相源)

**问题**:`send_message` 用控制行传来的 `model`(写死 DeepSeek 清单)发给 DB provider 的 base_url/key,provider 的 `model_id` 对主模型未被使用 → 非 DeepSeek 供应商必失败。

**定调**:**主模型的权威来源 = 主 provider 的 `model_id`(设置→模型供应商里配)**。控制行不再是第二真相源。
- **后端(A)**:`send_message` 读主 provider 时一并取 `model_id`,**用它**做主链路 chat 调用(`chat_stream`/`chat_with_builtin_tools`)与成本计价(`deepseek_pricing_for_model(&model_id)`),不再用入参 `model` 决定模型(入参可保留但不决定模型,或仅作兜底)。`plan_mode`/`compact` 等同链路一并改为 model_id。
- **前端(B)**:控制行的模型 `<select>`(约 1540–1550 行,基于 `DEEPSEEK_MODELS`)**改为只读「当前模型」胶囊**——显示主 provider 的 `model_id`(挂载 / 配置后用 `get_model_provider_config('main').modelId` 取),点击直接 `openSettings('provider')`。`/model` 斜杠命令改为打开设置→模型供应商。`model` state 可保留并初始化为该 model_id 一路透传(后端已不靠它选模型),`DeepSeekModelId` 类型按需放宽为 string。
- **取舍提示**:DeepSeek 用户失去控制行一键切 flash/pro,改为去设置改 model_id。此取舍在验收时交主创定夺(若要保留快切,后续单做"provider 多模型"特性,不在本批)。

## 4. 前端各项要点(子 agent B)

- **🔴2 全局 toast**:新增不依赖消息列表的全局通知组件(右下角堆叠、自动消失、可手动关、支持 error/info 两类)。把当前所有**错误类** `appendNoticeToLastMessage` 调用(图片门禁拒绝 `:1205/:1243`、读图失败、导入失败 `:1229`、MCP 添加失败、导出失败、回退失败等)改走 toast;**过程性通知**(上下文压缩、后台命令完成、已回退 N 处)保留内联在对话流(那些场景末态确是助手消息)。判断标准:用户主动操作的即时成败 → toast;Agent 过程事件 → 内联。
- **🔴3 删除确认**:`handleDeleteConversation` 加 `window.confirm`(文案如「确定删除会话『{标题}』？此操作不可撤销。」)与「清除所有会话」一致策略。
- **🟠4 粘底滚动**:维护 isAtBottom(监听 message-list 滚动,距底 < ~80px 视为贴底);仅贴底时 `scrollIntoView`;非贴底时不强拽,显示「↓ 跳到最新」按钮,点击回到底部并恢复跟随。去掉对每次 messages 变更无条件平滑滚动。
- **🟠5 失败恢复附图**:`sendText` catch 分支把本轮 `outImages` 还原回 `pendingImages`(去重/直接 setPendingImages(outImages))。
- **🟠6 跨会话重置**:`handleNewConversation` 与 `handleSelectConversation` 统一重置 `pendingImages`、`planMode`、`queuedSteering`(todos/workspaceFiles 已在 activeConvId effect 清,保持)。
- **🟠7 泄漏清洗保留卡片**:`chat-done` 的清洗逻辑(约 `:951-972`)改为**只截断 `streamingPartsRef` 中最后一个 text part 的泄漏内容并在其后追加 notice**,保留前面的 tool/vision part,不再用 `[text, notice]` 整体覆盖。

## 5. 验收
- `cargo build -p mdga-desktop` + `cargo test --workspace` + `tsc --noEmit` 全绿、零新增警告。
- dev 走查:① 配非 DeepSeek 主供应商(如智谱 GLM)能正常对话;控制行显示其 model_id,点击进设置;② 首屏拖图未配视觉 → 右下角 toast 提示(不再无声);③ 删会话弹确认;④ Agent 输出时能向上翻看、出现「跳到最新」;⑤ 发送失败后附图还在;⑥ 切/建会话不残留附图与计划模式;⑦ 含工具调用的回复即便末尾有泄漏,工具/视觉卡片仍在。

## 6. 不在本计划(🟡 次要,留后)
打开设置必拉余额、编辑重试/重发语义、停止时 ask_user 残留、/compact 停止按钮、两套已配探测收敛、老用户未配引导。
