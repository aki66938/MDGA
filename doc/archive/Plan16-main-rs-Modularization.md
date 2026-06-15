# Plan16 - 桌面后端 main.rs 模块化重构

项目代号：MDGA
文档定位：`apps/desktop/src-tauri/src/main.rs` 已增长到 **4589 行**，成为典型「上帝文件」——单文件承载了 tauri 命令、Agent 工具循环、工具 schema/派发/执行、子代理、MCP、权限审批、上下文压缩、checkpoint、后台 shell/task 等十余种职责。本计划把它**按职责拆成多个 Rust 子模块**，降低认知与维护成本。

> **执行时机**：本计划在 **0.0.26 发版 + 复刻到 NovaCode 之后**启动（基于已发版的稳定基线重构），不与功能开发混做。
>
> **第一性约束（结果导向）**：本重构是**纯代码搬移 + 可见性调整，零行为变更、零 UI 变更**。任一阶段都不得改动任何工具逻辑、IPC 事件名、前端契约、命令签名。靠三重保证守住：① 现有 `cargo test --workspace` 全绿；② `cargo check` 每阶段通过；③ 收尾由主创 `npx tauri dev` 真机走查所有功能与 UI 效果。

---

## 1. 目标与非目标

**目标**
- 把 `main.rs` 从 4589 行拆到一个 ≤300 行的薄入口 + 若干每个 ≤800 行的聚焦模块。
- 模块边界清晰、依赖单向，便于后续单测与并行开发。

**非目标（本计划明确不做）**
- 不改任何功能行为、不调整 UI、不优化算法、不改 IPC 事件/命令名。
- 不拆 `App.tsx`（2468 行）与 `tool-runtime/lib.rs`（1920 行）——列为**后续可选**（见 §5），本计划只聚焦 `main.rs`。

---

## 2. 目标模块划分（`apps/desktop/src-tauri/src/`）

| 新模块 | 职责 | 主要迁入内容 |
|---|---|---|
| `main.rs`（薄入口，≤300 行） | 仅 `tauri::Builder`、`setup`、`invoke_handler!` 注册、`mod` 声明 | 现 `fn main` + AppState 初始化 + MCP 启动连接 |
| `state.rs` | 应用状态与共享结构 | `AppState`、`BgShell`、`BgTask`、各 `static SEQ`、`McpBinding` |
| `commands.rs` | 所有 `#[tauri::command]`（非 agent 主流程） | 会话 CRUD、工作区、设置、审批响应、ask_user 响应、checkpoint 查询、导出/治理、更新检查 |
| `agent/loop.rs` | `send_message` 主工具循环 | 多轮循环、steering 注入、状态事件、取消、usage 记账、大输出落盘调用 |
| `agent/tools.rs` | 工具 schema 与派发/执行 | `all_builtin_tool_schemas`、`file_tool_schema`、`execute_builtin_tool_call`、各 `execute_*_tool` |
| `agent/subagent.rs` | 子代理与后台任务 | `execute_run_subtask`、`run_subtask_loop`、`execute_bg_task_tool`、`read_only_tool_schemas` |
| `agent/mcp.rs` | MCP 集成 | `collect_mcp_bindings`、`spawn_mcp_connect`、`execute_mcp_tool`、`execute_mcp_resource_tool` |
| `agent/permissions.rs` | 权限裁决与审批 | `tool_capability_for_name`、`request_tool_approval`、规则匹配、`gate_tool_decision`、`feed_tool_denial` |
| `agent/compaction.rs` | 上下文压缩 | `summary_split_points`、auto-compact、`context_soft_limit_tokens` |
| `agent/checkpoint.rs` | 变更快照/回退 | `capture_checkpoint_before`、`persist_checkpoint`、`revert_to_checkpoint`、diff 生成 |
| `agent/web.rs` | 联网工具 | `execute_web_fetch`、`execute_web_search` |
| `agent/command_run.rs` | 命令执行与后台 shell | `execute_run_command_tool`、`execute_bg_shell_tool`、`command_line_callback` |
| `agent/hooks.rs` | 生命周期钩子 | `run_pre_tool_hooks`、`run_post_tool_hooks`、诊断回路 |

> 模块树建议：`agent/` 作为子模块目录（`mod agent; agent/mod.rs` re-export 内部子模块）。命名以实际代码聚类微调，不强求与上表逐字一致。

---

## 3. 分阶段执行（每阶段独立、可编译、可回退）

> 顺序原则：**先搬「叶子」（无内部依赖的纯函数/结构），后搬「主干」（工具循环）**。每阶段结束必须 `cargo check` 通过；每搬完 2-3 个模块跑一次 `cargo test --workspace`。

1. **阶段 0 · 准备**：建 `mod` 骨架文件 + `main.rs` 顶部 `mod` 声明；把跨模块要用的私有项统一改 `pub(crate)`。基线跑一次全量测试存档。
2. **阶段 1 · state.rs**：搬 `AppState`/`BgShell`/`BgTask`/statics。最高频引用，先稳住。
3. **阶段 2 · 叶子工具模块**：`web.rs`、`hooks.rs`、`command_run.rs`、`checkpoint.rs`、`compaction.rs`（彼此低耦合）。
4. **阶段 3 · permissions.rs + mcp.rs**：权限裁决与 MCP 集成。
5. **阶段 4 · tools.rs**：工具 schema + `execute_builtin_tool_call` + 各 `execute_*_tool`。
6. **阶段 5 · subagent.rs**：子代理 + 后台任务（依赖 tools/permissions）。
7. **阶段 6 · agent/loop.rs**：搬 `send_message` 主循环（依赖前述全部）。
8. **阶段 7 · commands.rs**：搬其余 `#[tauri::command]`；`main.rs` 收敛为薄入口。
9. **阶段 8 · 收尾验证**：`cargo test --workspace` + `cargo check --no-default-features` + `npm run typecheck` 全绿；主创 `npx tauri dev` 真机走查：对话/工具调用/diff/checkpoint/plan/steering/MCP/子代理/ask_user/后台任务/设置页/主题，逐项确认行为与 UI 与重构前**完全一致**。

---

## 4. 风险与对策

| 风险 | 对策 |
|---|---|
| Tauri 命令注册需全部可见 | `#[tauri::command]` 函数移到模块后保持 `pub`，`invoke_handler!` 在 `main.rs` 用 `模块::命令` 路径引用；逐个编译验证 |
| 私有函数跨模块调用 | 统一升级为 `pub(crate)`；不对外暴露超出 crate 的 API |
| `AppState` 字段访问 | 字段保持 `pub(crate)`；各模块经 `app.state::<AppState>()` 取用，模式不变 |
| 漏改一处导致行为偏移 | 每阶段 diff 自审「只有位置变化、无逻辑变化」；全量测试 + dev 走查兜底 |
| 重构期间与新功能冲突 | 重构窗口内冻结 `main.rs` 的功能性改动，集中完成后再继续迭代 |

---

## 5. 后续可选（不在本计划内）

- **App.tsx（2468 行）** 拆组件：`ApprovalModal`/`AskUserModal`/`ChangesModal`/`SettingsModal`/`MessageContent`/`Composer`/`Sidebar` 各自成文件 + 共享类型抽到 `types.ts`。
- **tool-runtime/lib.rs（1920 行）** 按职责分文件：`fs_tools.rs`（文件 CRUD）、`search.rs`（search_text/glob_files）、`command.rs`（run_command）。

二者职责相对内聚、风险更低，待 `main.rs` 重构验证稳定后按需推进。

---

## 5.5 执行进度与剩余计划（2026-06 实测）

**已抽出并入库（7 模块，main.rs 4589 → 3251，−29%）**：`state` / `web` / `command_run` / `hooks` / `compaction` / `mcp` / `permissions`。每个均 `cargo test -p mdga-desktop` 14 passed、零警告；被测私有函数随测试迁入对应模块。

**已确立的抽取范式（后续模块照此执行）**：
1. 新建 `<模块>.rs`，对外被引用的项标 `pub(crate)`，模块内部项保持私有。
2. 把代码整段搬入新文件；main.rs 原位置替换为 `mod <模块>; use <模块>::{...};`。
3. 调用方需要的常量/函数升 `pub(crate)`（如 `record_tool_event`、`COMPACTED_TOOL_STUB`）。
4. 引用了私有函数的测试，连同测试一起迁入模块的 `#[cfg(test)] mod tests`。
5. `cargo test -p mdga-desktop` 必须 **14 passed、0 warnings**；清理失效 `use`。
6. 该模块通过后单独 `refactor` 提交并 push main（**不打 tag、不升版本号**）。

**剩余 6 模块（按依赖顺序，agent_loop 最后）**：
| 模块 | 迁入内容 |
|---|---|
| `checkpoint.rs` | CheckpointCapture、CHECKPOINT_MAX_SNAPSHOT_BYTES、safe_workspace_join、capture_checkpoint_before、compute_line_diff、post_execution_diff、persist_checkpoint、apply_checkpoint_revert |
| `chat.rs` | chat_completion_with_retry、fallback_model_for、stream_round_with_retry、recover_tool_calls_from_content、assistant_message_for_tool_calls、chat_messages_to_wire |
| `tools.rs` | all_builtin_tool_schemas、file_tool_schema、execute_builtin_tool_call、execute_create_file_tool_call、execute_remember、execute_todo_write、execute_load_skill、load_workspace_skills、PARALLEL_READONLY_TOOLS、execute_readonly_call |
| `subagent.rs` | SUBTASK_MAX_ROUNDS、read_only_tool_schemas、execute_run_subtask、run_subtask_loop、execute_bg_task_tool（依赖 chat + tools，故在其后）|
| `commands.rs` | 除 send_message 外的全部 `#[tauri::command]` + 仅命令用到的小工具（parse_command_frontmatter、extract_docx_text、workspace_name_from_path、permission_mode_from_str 等）|
| `agent_loop.rs` | send_message、chat_with_builtin_tools、messages_with_workspace_context、read_workspace_memory（依赖前面所有模块，**最后抽**）|

> 依赖以 cargo 报错为准动态解决：缺啥就把调用方的项升 `pub(crate)`、删失效 `use`。真正全局共享的小工具（如 `record_tool_event`）可暂留 main.rs。目标：main.rs 收敛为薄入口（main() + setup + invoke_handler + mod 声明）。

## 6. 验收标准（必须全部满足）

- `cargo test --workspace`、`cargo check`、`cargo check --no-default-features`、`npm run typecheck` 全绿。
- 主创 `npx tauri dev` 真机走查：**所有既有功能与 UI 效果与重构前一致，无任何可感知差异**。
- `main.rs` ≤300 行；其余模块各 ≤800 行。
- 全程无 git 行为变更类改动混入（commit 标 `refactor`，summary 明确「无行为变更」）。
