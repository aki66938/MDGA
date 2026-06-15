# Plan25（并入 0.0.34）- Agent 核心底座加固（P0–P2 全做）

项目代号:MDGA
文档定位:基于「Agent 核心底座评审」,一次性补齐 P0–P2 六项,提升长线任务可靠性与可维护性。并入 0.0.34,主创测试后提交。

> 4 个文件域不重叠的子 agent 并行实现 → 主控集成验收(cargo build/test + tsc) → 主创测试 → 提交。

## 1. 范围（6 项）
| 级别 | 项 | 端 |
|:---:|---|---|
| 🔴#7 | 默认验证回路:写类操作后自动跑构建/测试/diagnostics,失败回灌自纠(有限轮) | 后端 |
| 🔴#4 | 计划闭环:计划模式产出后「批准并执行」→注入 todo 清单 + 严格按计划执行;非 code 增「只读」语义 | 全栈 |
| 🔴#5 | 长任务:当前 todo 每轮回灌 + 持久化 `.mdga/tasks/current.json` + 卡死/重复失败检测 | 后端 |
| 🟠#1 | 灵魂文件:抽出可维护的核心系统提示工件(身份+**行为准则**+安全),动静分离 | 后端 |
| 🟠#3 | 工具调用可靠性:兜底解析泛化 + 多供应商工具调用冒烟探测 | 全栈 |
| 🟠#6/#2 | 可写工作子代理(同权限/检查点)+ `apply_patch` 多处编辑工具 | 后端 |

## 2. 分工（4 lane 并行，文件域不重叠）
- **Lane A（核心协调）**:`agent_loop.rs`、`commands.rs`、`main.rs`、`permissions.rs`、`checkpoint.rs`、新增灵魂模块文件。负责 #1、#4 后端、#5、#7、apply_patch 的能力/检查点接线、smoke 命令包装。
- **Lane B（deepseek-client）**:`crates/deepseek-client/**`。负责 #3 的兜底泛化 + `probe_tool_call` 探测器。
- **Lane C（工具与子代理）**:`apps/desktop/src-tauri/src/tools.rs`、`subagent.rs`。负责 #2 `apply_patch` 工具实现、#6 可写子代理。
- **Lane D（前端）**:`apps/desktop/src/App.tsx`、`styles.css`。负责 #4「批准并执行」按钮、#3 供应商工具调用兼容状态、（#5 可选「任务已保存」提示）。

## 3. 跨 lane 契约（钉死）
- **C-1（#3）**:Lane B 暴露 `pub async fn probe_tool_call(base_url:&str, api_key:&str, model:&str, api_format:&str) -> Result<bool, DeepSeekError>`(发一个极小请求、提供一个 trivial 函数工具,判断模型是否返回 tool_call——原生或兜底恢复均算成功)。Lane A 在 commands.rs 包装为命令 `smoke_test_tool_call(role, base_url, api_key, model, api_format) -> Result<bool, String>`(字段为空回退 DB,逻辑同 `test_provider_connection`),main.rs 注册。Lane D `invoke("smoke_test_tool_call", {role, baseUrl, apiKey, model, apiFormat})`。
- **C-2（#2 apply_patch）**:Lane C 在 `tools.rs` 的 `all_builtin_tool_schemas` 加 `apply_patch` schema、在 `execute_builtin_tool_call` 加分发;工具名 `apply_patch`,参数 `{ "path": string, "edits": [ {"oldText":string,"newText":string}, ... ] }`,按顺序对每个 edit 做**唯一匹配替换**(复用 edit_file 的唯一匹配语义,任一 oldText 不唯一/未命中则整体失败并报明哪条)。Lane A 在 `permissions.rs::tool_capability_for_name` 把 `apply_patch`→`FileWrite`,在 `checkpoint.rs::capture_checkpoint_before`/`post_execution_diff` 按 `apply_patch` 取 `path` 快照原文(同 write_file:可回退)。
- **C-3（#6 可写子代理）**:Lane C 改 `execute_run_subtask` 签名,新增末位入参 `permission: mdga_shared::PermissionMode` 与 `permission_rules: Vec<String>`;`run_subtask` 参数加 `mode: "read"|"write"`(缺省 read)。write 模式:子代理工具集含写/编辑/命令工具,**每次写/命令调用复用主链路的 `permissions::gate_tool_decision` + `request_tool_approval` 门控 与 `checkpoint::capture_checkpoint_before`/`persist_checkpoint`**(都是 pub(crate),可直接调),前台执行、并入 usage;read 模式维持现状(只读+断网)。Lane A 在 `agent_loop.rs:751` 的调用点补传 `permission` 与 `permission_rules.clone()`。
- **C-4（#4 执行计划）**:Lane A 给 `send_message` 加入参 `execute_plan: Option<bool>`;为 true 时在消息装配里注入一条 system:「请严格按你上一条给出的分步计划执行,开工前先用 todo_write 建立清单并随进度更新状态」。Lane D 在「计划模式的助手回复」后显示「批准并执行」按钮,点击以 `planMode=false` + `executePlan=true` 调 `send_message`(消息体可为「按计划执行」)。

## 4. 各 lane 实现要点

### Lane A
- **#1 灵魂文件**:新增 `apps/desktop/src-tauri/src/agent_prompt.rs`(或 const 模块),集中:① 身份锚定(现 :323-328);② 工具纪律(现 :341-344);③ **新增行为准则**段(简洁不寒暄、改前先读、优先 edit_file/apply_patch、能查清不提问、写完必验证、不可逆操作谨慎、达成即停不画蛇添足)。`messages_with_workspace_context` 改为引用该模块常量,**动静分离**(不可变原则 vs 动态的工作区/记忆/技能)。保持纯聊天分支与现有注入顺序。
- **#5**:在 `chat_with_builtin_tools` 循环里:① 维护「最近一次 todo_write 的清单」,每轮在 wire 末尾(user 之前)注入一条轻量 system 提醒「当前任务清单:…,请聚焦未完成项」;② 每次 todo_write 成功后把清单写 `<workspace>/.mdga/tasks/current.json`(失败忽略);③ 卡死检测:记录连续「无成功工具/无新 assistant 文本」轮数与「同一工具+同参连续失败」次数,达阈值(如 3)emit 通知并 `return`(暂停),提示用户介入。
- **#7 验证回路**:工具循环自然结束后(返回前),若本轮发生过写类工具且能探测到验证手段(`.mdga/diagnostics` 或工作区可识别的 cargo/npm/pytest 等),自动执行;失败则把输出作为新一轮 user 回灌让模型自纠,**最多 N=2 轮**;通过或放弃后结束。复用现有命令执行与沙箱路径。
- 接线 C-1/C-2/C-3/C-4 的 A 侧。

### Lane B（deepseek-client）
- **#3 兜底泛化**:在 `parse_dsml_tool_calls` 之外(或之内)增加对常见泄漏格式的宽松解析:` ```json {"name":..,"arguments":..}``` `、`<tool_call>{json}</tool_call>`、`<function=NAME>{json}</function>` 等;保持现有 DSML/`<ToolCall>` 路径与原生 tool_calls 优先级不变。补单测覆盖新增格式。
- `probe_tool_call`(契约 C-1):构造最小请求,提供一个 trivial 函数工具(如 `{name:"ping", parameters:{}}`),`max_tokens` 小;成功条件 = 响应里出现原生 tool_calls 或正文可被兜底恢复出 tool_call。openai/anthropic 两格式分别处理(anthropic 用 `/v1/messages` + tools)。

### Lane C（tools.rs / subagent.rs）
- 见契约 C-2、C-3。apply_patch 复用 tool-runtime 的 edit 唯一匹配能力(可多次调用 `mdga_tool_runtime::edit_file` 或在 tools 层按序替换)。写子代理务必每个写/命令走门控+检查点,绝不绕过权限。

### Lane D（前端）
- 见契约 C-1(供应商设置里加「测试工具调用」状态:成功绿/失败红/未测)、C-4(计划模式回复后的「批准并执行」按钮)。可选:#5 任务持久化无需前端,若易做可加一行「任务已保存到 .mdga/tasks」提示。

## 5. 验收
- `cargo build -p mdga-desktop`(0 警告)+ `cargo test --workspace` + `tsc --noEmit` 全绿。
- 主控集成验收后,主创 dev 测试:① 改代码后自动跑 build/test 并自纠;② 计划模式→批准并执行→按 todo 跟踪;③ 长任务 todo 每轮可见、`.mdga/tasks/current.json` 落盘、卡死能暂停;④ 设置里供应商「测试工具调用」可用;⑤ apply_patch 多处编辑、可回退;⑥ 写子代理在权限/检查点保护下委派写活。
- 通过后并入 0.0.34 提交。

## 6. 不在本计划
- 写子代理的并行多 worker / 后台写(本计划仅前台、同权限单写子代理)。
- agent 能力遥测(工具成功率/任务完成率度量)——评审里点到,留后。
