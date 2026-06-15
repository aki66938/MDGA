# Plan21 - 功能关联/操作逻辑修复(P0–P2)

项目代号:MDGA
文档定位:基于「功能关联/操作逻辑分析」,修掉 P0–P2 的跨功能矛盾,合并为下一个版本(版本号由主创提交时定)。

> 子 agent 并行实现(后端 / 前端,文件域不重叠)→ 主控集成验收(cargo build/test + tsc)→ 交主创 dev 走查 + 裁定提交。

## 1. 范围(6 项)

| 级别 | 项 | 端 |
|:---:|---|---|
| 🔴#2a | 默认模式(WorkspaceAuto)删除由自动放行改为**逐次审批** | 后端 |
| 🔴#2b | **不可回退**的写/删操作(删目录、超大/二进制覆盖或删除)**强制审批**,文案标注不可回退 | 后端 |
| 🔴#5 | 多供应商一致性:非 DeepSeek 主供应商时,**余额查询门禁** + **成本金额标"未知"** | 全栈 |
| 🟠#4 | 任务预算纳入**视觉(及前台子代理)usage**;预算标签厘清为"单轮上限" | 全栈 |
| 🟠#3 | 回退后把对话流里受影响的 **diff 卡片标记"已回退"** | 前端 |
| 🟡#8 / 🟡#9 | #8 只读联网(web_search/web_fetch)在默认模式自动放行;#9 自定义命令与内置/工具**命名冲突提示** | #8 后端 / #9 前端 |

## 2. 分工(可并行,文件域不重叠)

- **子 agent A(后端)**:`crates/sandbox-runtime/src/lib.rs`、`apps/desktop/src-tauri/src/{permissions.rs, agent_loop.rs, tools.rs, commands.rs}`。做 #2a、#2b、#8、#4 后端、#5 后端。
- **子 agent B(前端)**:`apps/desktop/src/App.tsx`、`apps/desktop/src/styles.css`。做 #5 前端、#4 前端、#3、#9。

仅按 §3 契约对接。

## 3. 接口契约 & 实现要点

### #2a 默认模式删除改审批(后端 A)
- `crates/sandbox-runtime/src/lib.rs` 的 `decide_tool_access`:`WorkspaceAuto` 分支把 `FileDelete` 从 `Allow` 改为 `AskUser`(`FileWrite` 保持 `Allow`)。同步更新该文件相关单测(原断言 FileDelete=Allow 的改为 AskUser)。

### #2b 不可回退即强制审批(后端 A)
- 现状:工具循环里写/删类工具执行前调 `checkpoint::capture_checkpoint_before` 得到 `CheckpointCapture{revertible}`。
- 改:在工具**执行前**,若该次为写/删类且 `capture.revertible == false`,**无论权限门控结论如何都先发审批**(复用 `request_tool_approval`),审批 target/preview 标注「⚠ 此操作不可回退」。被拒则按拒绝回灌、不执行。定位:`tools.rs` / `agent_loop.rs` 中调用 capture 与 gate 的工具分发处(自行通读定位,放在 gate 之后、执行之前)。
- 目的:覆盖 #2a 漏掉的「自动放行的写,但目标超大/二进制快照失败 → 不可回退覆盖」场景。

### #8 只读联网默认自动(后端 A)
- `permissions.rs` 的 `gate_tool_decision`:在能力矩阵裁决**之前**,对 `web_search` / `web_fetch` 且 `permission_mode == WorkspaceAuto` 直接返回 `ToolGate::Allow`(deny 规则仍优先,保持在最前)。MCP 工具 / `list_mcp_resources` / `read_mcp_resource` 不在此列,维持 AskUser。不改 `ToolCapability` 枚举。

### #4 预算纳入视觉/子代理(后端 A + 前端 B)
- 后端:`send_message` 的自动初看拿到视觉 usage 后,把它作为**预算累计起点**传入主循环 `chat_with_builtin_tools`(给该函数加一个 `initial_usage: Option<RawUsage>` 入参,循环内 `usage` 以它为初值;前台 `run_subtask` 的 usage 本就并入循环 `usage`,无需额外改)。预算判断(`agent_loop.rs:491-503`)因而覆盖视觉开销。
- 前端:设置→数据(或对应位置)的预算项标签由"任务 token 预算"改为「**单轮 token 上限(超出即暂停本轮)**」并配套说明,消除"任务级"误解。

### #5 多供应商余额/计价一致性(后端 A + 前端 B)
- 后端:`commands.rs` 的 `get_account_balance`,在调用 `get_user_balance` 前判断主 provider 的 `preset`;**非 `deepseek` 直接返回 `Err("当前主供应商不提供余额查询（仅 DeepSeek 支持）")`**,不去打 DeepSeek 端点。
- 前端契约:`get_model_provider_config('main')` 返回对象含 `preset` 字段。
  - 账户/余额区:仅当主 provider `preset === 'deepseek'` 才显示"账户余额"卡与刷新;否则显示「该供应商不提供余额查询」。
  - 成本展示(UsageBadge / 会话累计):当主 provider `preset !== 'deepseek'` 时,金额位显示「—」(token 数照常),不显示按 DeepSeek 价表算出的误导金额。前端挂载/配置后读取并缓存 `mainPreset`。

### #3 回退标记 diff 卡片(前端 B)
- `handleRevert` 成功后,把当前会话消息里**受影响**(同 rel_path,或简化为全部 tool 类 diff 卡片)的卡片标记 `reverted:true`,渲染时置灰/加「已回退」角标。简化可接受:回退后给所有现存 diff 卡片打"已回退"视觉标记 + 现有"已回退 N 处"通知保留。tool part 类型加可选 `reverted?: boolean`。

### #9 自定义命令命名冲突提示(前端 B)
- 加载 `customCommands` 后,与内置 `SLASH_COMMANDS`(及已知 MCP/技能名,若易得)比对;冲突项在斜杠菜单条目上加「与内置命令冲突,已被忽略」标注(因 `handleSlashCommand` 内置优先,自定义同名确被忽略)。

## 4. 验收
- `cargo build -p mdga-desktop`(0 警告)+ `cargo test --workspace` + `tsc --noEmit` 全绿。
- dev 走查:① 默认模式删文件/目录弹审批;② 覆盖超大/二进制文件弹"不可回退"审批;③ 配智谱等非 DeepSeek 主供应商时余额区提示不支持、成本显示「—」、对话正常;④ 设预算后视觉调用计入、超出暂停;标签为"单轮上限";⑤ 回退后 diff 卡片显示"已回退";⑥ 默认模式 web_search/web_fetch 不再每次弹审批;⑦ 同名自定义命令有冲突提示。

## 5. 不在本计划
- delete_dir 软删除到 .mdga/trash(本计划用"强制审批"兜底,软删除留后)。
- per-provider 成本价表(本计划先标"未知")。
- 真正的跨轮"任务级"预算持久累计(本计划先正名为"单轮上限")。
