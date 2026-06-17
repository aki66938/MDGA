# MDGA Agent 能力路线图(缺口分析)

> 生成于 2026-06-18。依据:MDGA 自身代码盘点(0.0.1→0.0.50)+ 2025–2026 联网调研(Cursor/Aider/Cline/OpenHands 官方文档、Cognition/Devin、MCP 官方博客、arXiv、SWE-bench 相关,30+ 来源)。
> 用途:作为「下一步做什么」的对照表 + 多会话并行开发的**协调/认领看板**(每条路线在「认领状态」里标明谁在做、什么分支)。

## 一句话结论

MDGA 的「骨架」已相当完整(对标 Claude Code):agent loop、两级压缩、计划模式、读/写/异步子代理、hooks、MCP、skills、检查点 + rewind、四档权限、AppContainer/受限令牌沙箱、成本预算、同族容灾、多供应商、apply_patch、write-then-verify 诊断、崩溃续接地基、视觉模态。**真正的缺口集中在「代码智能」与「验证深度」**,而非基础设施。研究共识:*2026 年 agent 的强弱在围绕模型的「上下文工程 + 验证脚手架」,而非模型本身*。

## 强 agent 的 12 根支柱

1. 分层规划 + plan-mode 审批门 · 2. 长程自治执行循环 · 3. 主动上下文/记忆管理 · 4. 人工维护的持久项目记忆(LLM 自动生成版反而掉分) · 5. **代码理解:定位 + repo/codemap + 语义检索** · 6. **LSP 类型感知编辑** · 7. 工具注册表 + MCP + **跨文件原子编辑** · 8. **自我验证 + 批判循环(执行式 + 非执行式)** · 9. 子代理编排 + 并行 + 隔离 · 10. 错误恢复与循环控制(doom-loop/stale-read) · 11. 人在环 steering + 纵深防御沙箱 · 12. 会话持久化 + git 原生回滚 + 按角色多模型路由 ·(+)多模态 IO + 浏览器/computer-use。

硬数据:LSP 找全部调用点 ~50ms vs 文本搜 ~45s;语义+grep 比纯 grep 代码库问答 +12.5%(千文件库增益最大);上下文工程 SWE-bench Verified 89% vs ~71%;MCP 2025.12 捐 Linux 基金会成事实标准。

## MDGA 已经很强(不缺)

支柱 1/2/3/4/9/10/11/12 大体覆盖。详见盘点:无上限 loop + 两级自动压缩、plan 模式、读/写/异步子代理 + 自定义 agent 类型、hooks、MCP(stdio/HTTP/OAuth + resources)、skills、文件检查点 + rewind-to-message、四档权限 + deny-first、AppContainer/受限令牌沙箱、成本预算、同族容灾、多供应商、apply_patch(单文件原子)、code_overview + write-then-verify、wire 续接地基、视觉。

## 缺口清单 + 认领看板

> 「认领状态」列:开发前在此填 `分支名 / 负责会话 / 状态`,避免多会话撞车。

### 🔴 高优先(代码智能 + 验证)

| ID | 缺口 | 为什么关键 | 量级 | 认领状态 |
|----|------|-----------|------|---------|
| R1 | **LSP 集成**(跳转/引用/类型/即时诊断) | 现零 LSP,导航靠文本搜、编辑靠字节匹配、无符号感知 → 大/强类型库臆造符号、静默回归。补上=编译级真相 | L:新 crate 包 rust-analyzer/tsserver/pyright over JSON-RPC,暴露 find_symbol/find_references/hover/diagnostics 工具,复用现成进程/ConPTY | `feat/r1-lsp` · 进行中 |
| R2 | **repo/codemap → 语义检索** | 现只有文件树摘要 + code_overview,无法按语义找代码、无引用排名定位 | 先 M(tree-sitter+PageRank repo map,免基础设施、最划算第一步),后 L(向量索引 chunk/embed/store/sync) | `feat/r2-codemap` · 进行中 |
| R3 | **真 TDD 自修复循环** | 现诊断只是"跑配置命令喂回错误"~2 轮,非"跑测试→读失败→打补丁到绿";自修复是 2026 最大分水岭 | M:结构化解析测试结果 + 迭代到绿 + doom-loop 护栏 + 可选按影响选测,长在现有诊断循环上 | `feat/r3-tdd` · 进行中 |
| R4 | **Git 原生工具** | git 现只能 run_command 裸跑字符串,无结构化 commit/diff/branch/PR;git 是可审计/可回滚/可交付底座 | M:git2/gh 包成工具,复用权限/审计层 | ✅ **已验收合并(0.0.51)**——壳调 git CLI:status/diff/log/branch/add/commit,只读→FileRead 自动放行、写→FileWrite;含单测+端到端冒烟。push+PR(gh)留后续增量(另起分支) |
| R5 | **跨文件原子编辑** | apply_patch 明确仅单文件;改函数名+全部调用点没法一次原子完成,大重构脆弱 | M:扩成多文件 patch set(all-or-nothing + revert),配合 R1 重命名 | 未认领 |

### 🟡 中优先

| ID | 缺口 | 一句话 | 量级 | 认领状态 |
|----|------|--------|------|---------|
| R6 | stale-read + 序列级 doom-loop 检测 | 现仅"相同调用失败"检测;缺"文件读后被改又基于旧内容编辑"防护 | S:按文件 read mtime/hash 标脏 + 窗口级 loop 检测 | 未认领 |
| R7 | 浏览器/computer-use | 能看截图(只输入),不能开浏览器点流程验自己写的 UI | M:无头 Chrome/Playwright 驱动 navigate/click/fill/screenshot/console,复用视觉管线 | 未认领 |
| R8 | 按角色多模型路由 | 现只同族容灾;不能"强模型规划/批判 + 便宜模型干活",和成本透明卖点搭 | M:配置把模型绑到 action/think/critique/plan/vision 角色 + 回退链 | 未认领 |
| R9 | execution-free reviewer 子代理 | 现只有执行式验证;没测试时缺 rubric/LLM-judge 审 diff 的门 | S:内置 reviewer agent 类型 + diff 评审 rubric,跑在 finalize 前 | 未认领 |
| R10 | 并行可写子代理(worktree 隔离) | 写子代理共享同工作区、无隔离,做不了安全 fan-out | L:Windows worktree/写时复制 + 合并冲突处理,交叉 AppContainer 工作 | 未认领 |

### 🟢 低优先

| ID | 缺口 | 量级 |
|----|------|------|
| R11 | 自动生成 repo wiki(可查询代码库知识) | L(依赖 R2 先做) |
| R12 | 五级渐进压缩 + 情景/工作记忆显式分离 | M(现两级已够好,属精修) |

## 推荐顺序

R1(LSP)→ R2(repo/codemap 先轻量后语义)→ R3(真 TDD 自修复)→ R4(git 原生)→ R5(跨文件原子)→ R9+R8(reviewer + 角色路由)→ R6(stale-read 护栏)→ R7(浏览器)。

## 多会话并行开发指南

**目标**:多个 Claude Code 会话同时推进不同路线。**前提**:必须 worktree 隔离,否则共享同一工作树会在文件/git/构建锁/`tauri dev` 端口上互相踩脚。

1. 每条路线一个**分支 + git worktree**(独立目录):
   `git worktree add ../MDGA-r1-lsp -b feat/r1-lsp`,`git worktree add ../MDGA-r3-tdd -b feat/r3-tdd` …
2. 在**每个 worktree 目录各开一个 Claude Code 会话**。本文件随 git 进每个 worktree,即跨会话共享真相;开发前在「认领状态」填上分支名占坑。
3. 注意事项:
   - 一次只跑一个 `tauri dev`(端口 1420 冲突);要并行预览需改各 worktree 的 vite 端口。
   - 每个 worktree 各自 `target/`(更费磁盘但无构建锁竞争);别用共享 `CARGO_TARGET_DIR`(会重新引入锁)。
   - **合并冲突高发点**:R1/R4/R5 都要往 `apps/desktop/src-tauri/src/tools.rs`(工具 schema)+ `main.rs`(命令注册)加东西 → 这两个文件会冲突。策略:要么这几条串行做、要么合并时手工合并注册段。
   - 我的 `.claude` 项目记忆按目录键控,worktree(不同目录)不会自动召回本目录记忆——所以**以本文件为准**。
   - 各路线以 PR/merge 回主干,回归测试在各 worktree 跑。
