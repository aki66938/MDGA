# Plan15 - 多代理 / 交互 / 工具面补齐（M12）

项目代号：MDGA
文档定位：本文是 Plan13（CC/Codex 对标总路线）之后的**下一阶段执行细化**，承接 Plan13 第 4 节「待做队列」中的「并行子代理执行」并把它与新一轮三方对照（Plan01-14 规划 × Claude Code 2.1.88 实际工具面 × 当前 v0.0.25 实测实现）暴露出的核心缺口合并为一个里程碑 **M12**。后续这批能力以本文为准。

> 三方对照基准：Claude Code 2.1.88 的 `sdk-tools.d.ts` 暴露 20 个工具面 + 多代理/后台/worktree 架构；当前 MDGA 已实现 21 个工具 + checkpoint/diff/plan/steering/hooks/MCP(stdio+HTTP+OAuth)/skills/诊断环/成本预算。**单代理闭环已基本完整**；剩余缺口集中在「多代理编排、异步任务、交互澄清、工具面富参数」四块 CC 的重型地基上。

---

## 1. 三方对照：本里程碑要补的缺口

> 完整对照见会话记录；下表只列入 M12 范围的项。受 DeepSeek 模型能力制约的项（多模态视觉读取 image/PDF/notebook）维持 Plan13 的 deferred，不在本批。

| 编号 | 缺口 | Claude Code 形态 | MDGA 现状 | 范围 |
|---|---|---|---|---|
| ① | 结构化澄清提问 | `AskUserQuestion`：运行中弹 1-4 题、每题 2-4 选项、multiSelect、preview，需求不清时主动问 | 无（`ask_user`/`ask_question` 0 命中），歧义只能猜或停 | **M12.1** |
| ② | 异步任务系统 | 后台**代理** `run_in_background` + `TaskOutput`(block/timeout) 轮询 + `TaskStop` 中止 | 仅后台 **shell**（run_command background + get/kill/list_shells）；`run_subtask` 同步阻塞 | **M12.2** |
| ③ | 并行/可写/隔离子代理 | 子代理可并行、可写、`isolation:"worktree"` 隔离、命名可寻址组队、按代理设权限 | `run_subtask` 单个、只读、同步、无隔离 | **M12.3** |
| ④ | Git Worktree 隔离 | `EnterWorktree`/`ExitWorktree` 创建/销毁 worktree，并行写入底座 | 无（`worktree` 0 命中） | **M12.3** |
| ⑤ | Grep 富参数 + 专用 Glob | `Grep`（output_mode、-A/-B/-C、type、multiline、head_limit/offset）+ 独立 `Glob`（按名快速匹配） | `search_text` 基础版（path+query+isRegex）；无专用 glob 工具 | **M12.4** |
| ⑥ | MCP Resources | `ListMcpResources`/`ReadMcpResource` 列举/读取 server 资源 | 仅接 MCP **工具**调用，未接 MCP **资源** | **M12.4** |
| ⑦ | 大输出持久化 | 超大命令/工具输出落盘 `persistedOutputPath`，回传文件引用不污染上下文 | 截断丢弃（`truncated`/`MAX_CHARS` cap） | **M12.4** |

---

## 2. 里程碑 M12（规划版本 0.0.26 起，归属由主创按 .dev-rules.md 裁决）

> 版本号仅为规划参考。每个子项可独立交付与 dev 真机验证，里程碑内部可拆多个小版本；AI 开发者不得自行决定版本归属。

### M12.1 - `ask_user` 结构化澄清提问（最低成本，优先）✅ 代码已实现（待 dev 验证）

**目标**：Agent 在需求不清时主动弹结构化选择卡片，而非凭空臆测或卡住。对标 `AskUserQuestion`。

**新增内置工具 `ask_user`**
```
questions: [1..4] 每题 {
  question: string,                 // 完整问题，问号结尾
  header:   string,                 // <=12 字符短标签（chip）
  multiSelect: bool,                // 是否多选
  options: [2..4] {
    label: string,                  // 1-5 词选项文案
    description: string,            // 该选项含义/取舍
    preview?: string                // 可选预览（代码片段/对比）
  }
}
```

**运行机制**
- 工具被调用时**暂停工具循环**，复用现有「审批弹窗 IPC 通道」的 pending 机制：把 questions 推给前端，等用户选择。
- 前端在对话流内渲染选择卡片：`header` 作 chip，选项为可点卡片，`description` 副文案，`preview` 聚焦时展开；多选时支持勾选多项。
- **自动附「Other」自定义输入项**（CC 同款），用户可输入选项外的答案。
- 用户提交后，把选择（含自定义文本）组装为 `tool_result` 回注，循环继续。
- 权限：纯交互、无副作用，**自动放行**，不进审批层；但全量进 `activity_events` 审计。

**与既有机制协调**
- `ask_user`（Agent 主动问） vs `steering`（用户主动插话）：前者阻塞等输入，后者非阻塞排队，二者互补不冲突。
- Plan 模式下也可调用 `ask_user` 细化计划参数。

**验收**：Agent 调用 `ask_user` → 前端出结构化卡片（含 Other）→ 用户单选/多选/自定义 → 选择正确回注、循环继续；刷新/切会话后该轮交互结构可还原。

---

### M12.2 - 异步任务系统（后台子代理 + `get_task_output`/`kill_task`）✅ 代码已实现（待 dev 验证）

**目标**：长耗时子代理可「挂后台、回头取结果」，对标 `Agent run_in_background` + `TaskOutput` + `TaskStop`。

**改造 `run_subtask`**：新增 `background: bool`。`background=true` 时立即返回 `taskId`，子代理在独立 tokio task 中跑自己的工具 loop。

**新增工具**
- `get_task_output(taskId, block?: bool, timeoutSecs?: int)`：轮询后台子代理的累计输出与状态（running/done/killed/error）；`block=true` 时最多等 timeout。
- `kill_task(taskId)`：中止后台子代理。
- `list_tasks()`：列出所有后台子代理及状态。

**架构**：复用「托管后台 shell 注册表」模式建**后台任务注册表**（task registry），与 shell registry 同构（句柄 + 输出 buffer + 状态机）。后台子代理的 token usage **计入会话账本与成本预算**，超预算按既有规则收尾。

**UI**：后台任务卡片显示运行中/完成，可点开看报告；完成时对话流通知（复用压缩通知卡片样式）。

**验收**：`run_subtask background=true` 立即返回 taskId；主代理继续工作的同时后台子代理推进；`get_task_output` 能取到增量输出、`kill_task` 能中止；usage 正确计入账本。

> 本子项是 M12.3 并行子代理的前置基建（先有「后台单代理」，再有「并行多代理」）。

---

### M12.3 - Worktree 隔离 + 并行可写子代理（重型，可拆小版本）⏳ 本批未做，留待后续单独攻坚

**目标**：解除 `run_subtask` 的「只读」限制，让多个可写子代理在 git worktree 隔离下并行改代码互不冲突。对标 `EnterWorktree`/`ExitWorktree` + `Agent isolation:"worktree"`。本项是本里程碑最大架构变更，**单独攻坚、必须 dev 真机验证**。

**M12.3a - Worktree 工具（底座）**
- `enter_worktree(name?)`：基于 `git worktree` 在临时分支上建工作区隔离副本（要求当前工作区是 git 仓库）。`name` 仅允许字母/数字/`. _ -`，≤64 字符，缺省随机生成。
- `exit_worktree(action: "keep"|"remove", discard_changes?: bool)`：`keep` 保留 worktree+分支；`remove` 删除二者，若有未提交变更/未合并提交则必须 `discard_changes=true` 才执行，否则拒绝并列出阻塞项。
- **非 git 仓库**：明确报错并提示「需先 git init」；影子目录 fallback 留作后续评估，不在本批（半成品隔离比无隔离更危险，遵 Plan09 原则）。

**M12.3b - 并行可写子代理**
- `run_subtask` 增 `writable: bool` 与隔离开关：可写子代理在各自独立 worktree 中执行，扩展现有「并行只读工具 join_all」基建到「并行子代理」。
- 每个子代理独立 worktree → 无并发写冲突；主代理收集各子代理结果后**人工/规则合并**（合并冲突显式上报，不静默覆盖）。
- 权限：可写子代理**继承会话权限模式**，所有写操作仍走 **checkpoint 快照**（rewind 仍可回退）；越权操作仍弹审批。

**风险标注**：Windows `git worktree` 行为、并发写、合并冲突、worktree 清理。与 Plan13 M8.2 AppContainer 沙箱**关联但不强绑定**（隔离维度不同：worktree 隔文件视图，AppContainer 隔 OS 能力）。

**验收**：git 仓库内 `enter_worktree` 建出隔离副本；2+ 可写子代理并行在各自 worktree 改文件互不干扰；结果合并正确、冲突显式上报；`exit_worktree remove` 对脏 worktree 正确拦截；checkpoint/rewind 在 worktree 场景仍有效。

---

### M12.4 - 工具面补强批（中等收益，可与上面并行推进）✅ 代码已实现（待 dev 验证）

**Grep 富参数化**：`search_text` 升级对标 `Grep`——
- `output_mode`: `content` / `files_with_matches` / `count`
- 上下文行：`-A` / `-B` / `-C`（content 模式）
- `type` 文件类型过滤、`multiline` 跨行、`head_limit`/`offset` 分页、行号
- 保持 gitignore 感知与正则（已有 `ignore` crate 后端）

**新增 `glob_files` 工具**（对标 `Glob`）：按**文件名** glob 快速匹配（复用已有 `glob_match` + `ignore` 遍历），与 `search_text`（内容检索）分工；返回按修改时间排序的路径列表。

**MCP Resources**：`mcp-client` crate 增 `resources/list` + `resources/read` JSON-RPC；新增工具 `list_mcp_resources(server?)` / `read_mcp_resource(server, uri)`，统一进 NetworkAccess 权限层与审计。

**大输出持久化**（对标 `persistedOutputPath`）：`run_command` 及工具输出超阈值时落盘到 `.mdga/tool-results/<id>`，`tool_result` 内回传「摘要 + 文件路径 + 总字节数」，避免污染上下文；`read_file` 可分页读取该落盘文件。替换现有「直接截断丢弃」。

**验收**：`search_text` 各 output_mode/上下文行/分页生效；`glob_files` 按名匹配正确；MCP 资源可列举与读取并进审计；大命令输出落盘且可经 `read_file` 回溯，不再撑爆上下文。

---

## 3. 推进顺序与原则

1. **M12.1**（ask_user）→ **M12.2**（异步后台代理）→ **M12.3**（worktree + 并行可写代理）→ **M12.4**（工具面补强）。
   - M12.4 与 M12.1/M12.2 无强依赖，可穿插并行；M12.3 依赖 M12.2 的后台代理基建。
2. **对标不照抄**：凡 CC 能力与 DeepSeek API/MDGA 架构冲突，做等效实现（如 CC 的命名组队 SendMessage 在 MDGA 先以「主代理编排 + 后台任务注册表」等效，不引入多 leader 团队模型）。
3. **差异化底色不变**：token 账本透明（后台代理 usage 入账）、本地优先、权限可审计（子代理写操作走 checkpoint + 审批）。
4. 每子项独立交付，遵 .dev-rules.md：完成编码 + 单测/tsc 通过后请主创 `npx tauri dev` 验证，主创确认后方可 commit/push/tag；版本归属由主创裁决。

---

## 4. 与既有 Plan 的关系

- **Plan13**：本文承接其第 4 节「待做队列·近期：并行子代理执行」，并新增 ①/⑤/⑥/⑦ 四项三方对照新缺口。Plan13 的 deferred 项（多模态视觉、思考预算、outputStyle/statusLine、M8.2 AppContainer）维持原决策，不在 M12。
- **Plan11**（工具运行时）：M12.4 的 Grep/Glob/MCP Resources/大输出持久化是其工具面的进一步富化。
- **Plan06/09**（安全沙箱）：M12.3 worktree 是「文件视图隔离」，与 Plan09 的「OS 能力隔离」（M8.2 AppContainer）正交互补；子代理写操作沿用既有权限分级 + checkpoint。
