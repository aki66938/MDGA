# MDGA Agent 能力路线图(缺口分析)

> 生成于 2026-06-18。依据:MDGA 自身代码盘点(0.0.1→0.0.50)+ 2025–2026 联网调研(Cursor/Aider/Cline/OpenHands 官方文档、Cognition/Devin、MCP 官方博客、arXiv、SWE-bench 相关,30+ 来源)。
> 用途:作为「下一步做什么」的对照表 + 多会话并行开发的**协调/认领看板**(每条路线在「认领状态」里标明谁在做、什么分支)。

> **2026-06-18 收官:R1–R12 全部落地(0.0.51→0.0.57)。** 高优先 R1-R5 + 中优先 R6-R10 + 低优先 R11-R12 + 各特性后续(LSP 用户可配置注册表 + 设置 UI、codemap L 向量阶段、R8 角色 UI、git push/PR、R7 加测、R10 worktree 隔离原语)均已实现并经对抗式审查合并。版本对照见文末各行「认领状态」。后续为打磨增量(R10 自动 fan-out 编排、code_search 可选 embedding 重排、wiki LLM 摘要增强等),非缺口。
>
> **0.0.58 打磨增量已落地**(4 项,全部严格 opt-in、0.0.57 默认行为逐字节不变,经对抗式审查):① R10 自动 fan-out —— `run_parallel_subtasks` 显式开关式并行可写子代理编排(拒污染前置:具名分支+干净树才跑;并发隔离 worktree 写、按序合并、冲突上报不自动解、RAII 清理;默认单子代理路径不变);② code_search 可选 embedding 重排(`useEmbedding` 默认 OFF,失败静默回退本地);③ repo_wiki 可选 LLM 摘要(`enrich` 默认 false,指纹缓存,确定性区段回退);④ git push/pr e2e 测试落地 + LSP 池 Drop 移出锁外。审查修复 2 处 low(merge --abort 失败不再谎报已还原 + code_search 离线 JSON 不再多带 embedding_reranked 键)。gh 2.94.0 已装并真机验证 git_push/git_pr(真建 PR 需用户 `gh auth login`)。后续仍可深化:R10 retained 分支自动 GC、embedding/LLM 增强的设置页 UI、enrich 用量计入账本。

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
| R1 | **LSP 集成**(跳转/引用/类型/即时诊断) | 现零 LSP,导航靠文本搜、编辑靠字节匹配、无符号感知 → 大/强类型库臆造符号、静默回归。补上=编译级真相 | L:新 crate 包 rust-analyzer/tsserver/pyright over JSON-RPC,暴露 find_symbol/find_references/hover/diagnostics 工具,复用现成进程/ConPTY | ✅ **已实现合并(0.0.54)**:新 crate `mdga-lsp` —— spawn 语言服务器(rust-analyzer / tsserver / pyright,**硬编码白名单**)+ Content-Length JSON-RPC,4 个只读工具 `lsp_definition`/`references`/`hover`/`diagnostics`(→ FileRead 自动放行、可并行)。安全:路径工作区限定 + 擦密钥 + 45s 超时 + Drop 强杀。rust-analyzer 端到端实测过;TS/Py 同构接线待装服务器验。**0.0.55 泛用化 + 池化**:精选注册表扩到 8 服务器(+gopls/clangd/ruby-lsp/intelephense/lua,预留用户授权扩展点)、会话池长驻复用(免冷启动)、绝对路径加固(顺修 TS/Py `.cmd` shim 在 Win 上根本起不来的真 bug)、**TS/Python 真机 e2e 通过**。✅ **设置页 UI 已实现合并(0.0.57 波2)**:设置页新增「语言服务器」(8 服务器逐个启停 + 可选二进制路径覆盖)+「角色路由」(action/plan/critique 绑模型、未配回退主模型)。**安全红线不动**:服务器种类/命令/参数仍是编译期常量,`save_lsp_server_config` 拒未知种类、路径覆盖须为已存在文件且只指向已知服务器,untrusted 输入绝不能引入任意命令;新增 Tauri 命令均在 main.rs invoke_handler 注册 |
| R2 | **repo/codemap → 语义检索** | 现只有文件树摘要 + code_overview,无法按语义找代码、无引用排名定位 | 先 M(tree-sitter+PageRank repo map,免基础设施、最划算第一步),后 L(向量索引 chunk/embed/store/sync) | ✅ **M 阶段已验收合并(0.0.52)**:新 crate `mdga-codemap`(tree-sitter 抽取定义/引用 + 个性化 PageRank)→ `repo_map` 只读工具 + 开局自动注入符号地图;纯内存、无联网、有内存上限。**0.0.55 泛用化**:tree-sitter 扩 9 门(Java/C/C++/C#/Ruby/PHP/Bash/Lua/Scala,核心 bump 0.25)+ **通用启发式回退**(无 grammar 的语言也按声明关键字抽粗粒度符号,任何文本文件都进地图)。✅ **L 阶段已实现合并(0.0.57 波2)**:本地语义检索 `code_search`——tree-sitter 切块(无 grammar 回退定长窗口)+ BM25 词法(标识符 camelCase/snake_case 拆词)+ PageRank 文件重要度加权 + 精确符号命中加分,回最相关代码块(路径+行号+片段+排名理由);**完全离线、无 embedding、零新依赖**,预留 `Embedder` 钩子供未来向量重排;与 repo_map 共用 `discover_source_files` 发现口径(手解三方合并已对齐)。8 单测 |
| R3 | **真 TDD 自修复循环** | 现诊断只是"跑配置命令喂回错误"~2 轮,非"跑测试→读失败→打补丁到绿";自修复是 2026 最大分水岭 | M:结构化解析测试结果 + 迭代到绿 + doom-loop 护栏 + 可选按影响选测,长在现有诊断循环上 | ✅ **已修复并合并(0.0.53)**:两段门(build→test)+ 结构化解析 + doom-loop 护栏 + 按影响窄跑;验收发现的 2 个安全阻断已修——① 验证命令改走用户「命令沙箱」开关 + 会话网络模式(不再硬编码裸跑);② `focused_command` 失败名白名单过滤防命令注入。R2 建图锁外化一并修 |
| R4 | **Git 原生工具** | git 现只能 run_command 裸跑字符串,无结构化 commit/diff/branch/PR;git 是可审计/可回滚/可交付底座 | M:git2/gh 包成工具,复用权限/审计层 | ✅ **已验收合并(0.0.51)**——壳调 git CLI:status/diff/log/branch/add/commit,只读→FileRead 自动放行、写→FileWrite;含单测+端到端冒烟。**push+PR 已补(0.0.57 波1)**:`git_push`(分支取自 rev-parse、固定 `push [--set-upstream] <remote> <branch>`、**永不 force**、单测锁不变量)+ `git_pr`(gh CLI、title/body 分离传参、base 校验);gh 绝对路径解析防 cwd 抢占、擦密钥、NetworkAccess 门控 |
| R5 | **跨文件原子编辑** | apply_patch 明确仅单文件;改函数名+全部调用点没法一次原子完成,大重构脆弱 | M:扩成多文件 patch set(all-or-nothing + revert),配合 R1 重命名 | ✅ **已实现合并(0.0.56)**:`apply_multi_patch`——全量校验/全过才写/任一失败一处不写,改动经检查点可整体回退;单文件 apply_patch 不变 |

### 🟡 中优先

| ID | 缺口 | 一句话 | 量级 | 认领状态 |
|----|------|--------|------|---------|
| R6 | stale-read + 序列级 doom-loop 检测 | 现仅"相同调用失败"检测;缺"文件读后被改又基于旧内容编辑"防护 | S:按文件 read mtime/hash 标脏 + 窗口级 loop 检测 | ✅ **已实现合并(0.0.56)**:read_file 记 mtime+size → 写前若被改注入「请重读」警告(只警告);+ 窗口级序列 doom-loop(A B A B…重复即停) |
| R7 | 浏览器/computer-use | 能看截图(只输入),不能开浏览器点流程验自己写的 UI | M:无头 Chrome/Playwright 驱动 navigate/click/fill/screenshot/console,复用视觉管线 | ✅ **已实现合并(0.0.56)**:crates/browser(headless_chrome)6 工具 navigate/screenshot/click/fill/read_text/console;http(s) 限定 + NetworkAccess 门控 + 30s 超时 + Drop 杀 chrome(零泄漏);无 Chrome 优雅跳过 |
| R8 | 按角色多模型路由 | 现只同族容灾;不能"强模型规划/批判 + 便宜模型干活",和成本透明卖点搭 | M:配置把模型绑到 action/think/critique/plan/vision 角色 + 回退链 | ✅ **已实现合并(0.0.56)**:action/plan 绑模型(计划轮用 plan、工具轮用 action)、**未配回退主模型(向后兼容)**;critique 已铺存储/解析/命令、UI 留后 |
| R9 | execution-free reviewer 子代理 | 现只有执行式验证;没测试时缺 rubric/LLM-judge 审 diff 的门 | S:内置 reviewer agent 类型 + diff 评审 rubric,跑在 finalize 前 | ✅ **已实现合并(0.0.57 波1)**:finalize 前 execution-free 评审一轮(crates/agent-core/reviewer.rs:REVIEW_RUBRIC + parse_review 纯解析器,`REVIEW: CLEAN/ISSUES` 协议);agent_loop.rs 累积本轮写工具的行 diff(≤16k)→ verify 门过后、tool_calls 空时跑一次无工具 model 调用评审;判 ISSUES 则注入反馈再跑一轮(REVIEW_MAX_ROUNDS=1 封顶),否则定稿;**全程 fail-open**(调用/解析出错即定稿,行为不退化);无写改动的轮次为真空操作。5 单测 |
| R10 | 并行可写子代理(worktree 隔离) | 写子代理共享同工作区、无隔离,做不了安全 fan-out | L:Windows worktree/写时复制 + 合并冲突处理,交叉 AppContainer 工作 | ✅ **原语已实现合并(0.0.57 波2,保守开关式)**:`IsolatedWorktree` RAII 守卫(父 HEAD 开临时分支+独立 worktree、唯一 nonce 防碰撞)+ `preview_merge`(只读冲突探针)+ `merge_into`(`--no-ff --no-edit`,**永不 force/`-X ours\|theirs`/`-s ours`**,冲突 `merge --abort` 还原+上抛)+ Drop 强清理(成功/失败/panic 都清);分支名净化拒 `-`、擦密钥、git 绝对路径。**默认子代理路径不变**,只交经测原语 + 接入点 `run_isolated_write_subtask`,自动 fan-out 编排留后续(「正确原语不自动开启」)。9+5 测试 |

### 🟢 低优先

| ID | 缺口 | 量级 | 认领状态 |
|----|------|------|---------|
| R11 | 自动生成 repo wiki(可查询代码库知识) | L(依赖 R2 先做) | ✅ **已实现合并(0.0.57 波2)**:`repo_wiki` 工具——基于 codemap 新增 `analyze_repo` 公共 API,**确定性、离线**按目录归纳「关键文件/顶层符号(含行号)/结构推断角色」落 `.mdga/wiki/`(增量幂等、原子写、可重建);`build` 生成 / `query` 词法检索区段并降级到实时分析;路径逐段净化绝不写出缓存目录外。新增 crate `mdga-wiki`,22 单测 |
| R12 | 五级渐进压缩 + 情景/工作记忆显式分离 | M(现两级已够好,属精修) | ✅ **已实现合并(0.0.57 波1)**:压缩加磁盘归档层(`.mdga/archive/<conv>.jsonl` 追加被丢内容、占位留指针可 read_file 找回)+ 中间「凝练成关键事实」级(中龄大结果换一行关键字段);三级渐进降级 condense→stub→summarize,每级先归档再丢;conversation_id 净化防穿越、归档失败 fail-soft 不中断 |

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
