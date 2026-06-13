# Plan13 - CC/Codex Parity Roadmap（对标补齐总路线）

项目代号：MDGA
文档定位：本文件是 0.0.15 之后的**总开发路线**。最终形态目标对标 Claude Code Desktop 与 Codex 两款产品：MDGA = 以 DeepSeek API 为底模的、二者形态的桌面 Agent 工作台。本文基于全部既有 Plan（Plan01-Plan12）、当前程序实际进度（v0.0.15）与 CC/Codex 公开能力的三方比对产出，后续迭代以本文为准；Plan12 剩余条目并入本文里程碑，不再单独推进。

---

## 1. 当前已具备能力盘点（截至 v0.0.15）

### 对话与会话
- DeepSeek 流式聊天、Markdown 渲染、用户/assistant 分离视觉
- 会话持久化（SQLite）、多会话管理、首条消息自动命名、删除会话
- 模型选择、Token 账本（单次 + 会话累计、缓存命中、费用估算）

### Agent 内核
- 12 个 Built-in 工具（文件 CRUD/移动/搜索/目录/stat + run_command）
- 无上限工具循环（自然终止 + 用户中断兜底），DSML 容错解析 + 泄漏清洗
- repo map 开局项目认知、工具失败自纠引导、网络抖动退避重试
- 两级上下文自动压缩（工具输出短桩 → 摘要式 auto-compact），阈值可调
- MDGA.md 工作区长期记忆（对标 CLAUDE.md / AGENTS.md）

### 权限与安全
- 四档权限模式（Restricted / AskEveryTime / WorkspaceAuto / FullAccess）
- 真实审批弹窗（允许一次/拒绝）、低风险命令白名单、workspace path guard
- activity_events 全量审计落库

### UI 可见性
- 工具调用内联渲染（✓/✗/⊘ + 路径）、连续调用折叠组、消息结构持久化还原
- Agent 实时状态行（思考中第 N 轮/执行工具/压缩中/输出中 + 耗时秒）
- 上下文用量百分比、压缩事件通知卡片、停止按钮

### 工程化
- 自动更新（签名）、CI/CD（tag → 安装包）、Cargo.lock 锁定、全量单测

---

## 2. 与 CC/Codex 的差距清单（按领域）

### A. 变更管理（CC/Codex 核心体验，差距最大）
| 缺失 | CC/Codex 形态 |
|---|---|
| diff 展示 | 每次文件编辑在对话流中显示彩色 diff 块（增删行高亮），而非只显示"✓ edit_file 路径" |
| 变更集 ChangeSet | 一轮任务改了哪些文件、各 +N/-M 行，有汇总视图 |
| Checkpoint / 回滚 | CC 自动快照每步文件状态，/rewind 可回退到任意步；Codex 依托 git |
| 编辑审阅 | 修改可被用户逐个接受/拒绝（Codex review 模式） |

### B. 任务系统
| 缺失 | CC/Codex 形态 |
|---|---|
| Todo / Plan 可视化 | CC 的 TodoWrite：Agent 自维护任务清单，UI 实时显示 ☐/■/☑ 进度；用户一眼看到"做到哪一步" |
| Plan 模式 | 先出计划、用户确认后再执行（CC plan mode / Codex 计划工具） |
| Subagent 子任务 | 聚焦子任务交给独立上下文的子 agent，只回传结论，主上下文不膨胀 |

### C. 命令与终端体验
| 缺失 | CC/Codex 形态 |
|---|---|
| 命令输出流式显示 | 命令运行时实时滚动输出，而非结束后一次性返回 |
| 后台任务 | 长命令可后台运行，完成后通知 |
| 权限记忆 | "总是允许此命令/此目录"持久化 allowlist，减少重复审批 |

### D. 会话与输入体验
| 缺失 | CC/Codex 形态 |
|---|---|
| 会话搜索 | 侧边栏搜索历史会话 |
| 手动重命名/置顶/归档 | 会话管理完整动作 |
| 斜杠命令 | /compact 手动压缩、/clear 清上下文、/init 生成 MDGA.md、/model 等 |
| @文件引用 | 输入框 @ 补全工作区文件，直接注入内容 |
| 消息编辑重发 | 修改上一条用户消息并重新生成 |
| 代码高亮 + 复制按钮 | 回复中代码块语法高亮、一键复制 |

### E. 设置与外观
| 缺失 | CC/Codex 形态 |
|---|---|
| 设置页 | 默认模型/权限模式、数据目录展示、版本信息、检查更新入口集中管理 |
| 暗色模式 | 主题切换 |
| 错误人话化 | 分类错误的友好提示与建议动作（当前直接抛原始错误串） |

### F. 扩展生态（既有 Plan11 Phase 5/6）
| 缺失 | CC/Codex 形态 |
|---|---|
| MCP 客户端 | 接入外部 MCP server，工具经统一权限层与审计 |
| Skills / 自定义指令 | 可复用工作流说明按需加载 |
| Hooks | 工具执行前后用户自定义钩子（低优先级） |

### G. 暂缓（既定决策）
- 文件导入/解析/问答：依赖视觉模型，排在 MCP/Skills 之后（主创已定）
- 移动端、云同步、多模型：Plan01 既定边界外

---

## 3. 里程碑排期

> 版本号仅为规划参考，实际归属由主创按 .dev-rules.md 迭代规则裁决。每个里程碑内部可拆多个小版本。

### M1 - 体验补齐（0.0.16 ~ 0.0.18）
最小代价把日常使用的毛刺磨平：
- 代码块语法高亮 + 复制按钮
- 会话搜索、手动重命名、置顶、归档
- 错误人话化（分类错误 → 友好提示 + 建议动作）
- 设置页基础（默认模型/权限、数据目录、版本、检查更新）

### M2 - 变更管理底座（0.0.19 ~ 0.0.21）【对标核心】
- edit_file / write_file 返回结构化 before/after，对话流内渲染 diff 块
- ChangeSet：一轮任务的文件变更汇总（+N/-M 行）
- Checkpoint：每次文件变更前自动快照（工作区影子目录或 git stash 机制），支持回滚单个文件 / 整轮任务
- /rewind 入口（UI 按钮形式先行，斜杠命令在 M4 接入）

### M3 - 任务系统（0.0.22 ~ 0.0.24）
- todo_write 内置工具：Agent 自维护任务清单，UI 常驻显示进度（☐ 进行中 ☑ 完成）
- Plan 模式：复杂任务先产出计划，用户确认后执行
- Subagent（探索版）：read-only 子任务独立上下文执行，回传结论

### M4 - 命令体验与输入增强（0.0.25 ~ 0.0.27）
- run_command 输出流式推送（边跑边显示），长命令后台运行
- 权限记忆：「总是允许」持久化 allowlist（按命令前缀 / 工具 + 路径粒度）
- 斜杠命令框架：/compact、/clear、/init、/model、/rewind
- @文件引用补全

### M8 - OS 沙箱（分阶段）
- **M8.1 受限令牌**（0.0.23）✅ 代码已实现（待 dev 真机验证）：run_command 在 CreateRestrictedToken（DISABLE_MAX_PRIVILEGE + 禁用 Administrators SID，派生自调用方令牌故免特权）+ Job Object（KILL_ON_JOB_CLOSE）中执行；密钥环境擦除；fail-closed；`-EncodedCommand` 规避转义；设置开关（默认开）。**只降权+清理，不隔离网络/文件路径。**
- **M8.2 AppContainer**（待做）：网络默认禁、文件系统仅 ACL 授权工作区——头号隔离目标，重型 FFI，单独攻坚。

### M5 - MCP 接入 ✅ 代码已实现（待 dev 验证）
- 新增 `crates/mcp-client`：最小 stdio JSON-RPC 客户端（initialize 握手 / tools/list / tools/call）✅
- 设置页 MCP 管理：添加（名称+启动命令）、启停、删除、连接状态与工具数 ✅
- MCP 工具以 `mcp_<server>_<tool>` 函数名并入模型工具集；统一按 NetworkAccess 能力进权限层（WorkspaceAuto 审批 / FullAccess 放行 / 可「总是允许」），全量进 activity_events ✅

### M6 - Skills ✅ 代码已实现（待 dev 验证）
- 技能目录规范：工作区 `.mdga/skills/<name>/SKILL.md`（frontmatter description）✅
- 渐进披露：system 注入技能名+描述清单，模型按需调用 `load_skill` 加载完整说明 ✅

### M7 - 文件导入与问答 ✅ 文本类已实现（待 dev 验证）
- 📎 导入按钮：TXT/MD/CSV/JSON/PDF/DOCX 文本抽取（cap 10 万字符），自动发送总结+问答 ✅
- 图片/扫描件：依赖视觉模型，明确提示暂不支持，留待后续评估 ⏳

---

## 4. 与既有 Plan 的关系

- **Plan12**（Agent 能力强化）：0.0.10-0.0.15 已完成全部主体；剩余「diff/patch 审阅底座」并入本文 M2，Plan12 归档。
- **Plan11**（工具运行时）：Phase 1-4 已完成；Phase 5（MCP）= 本文 M5，Phase 6（Skills）= 本文 M6；DSML 排错参考章节继续有效。
- **Plan03**（桌面 MVP）：未完成项（会话搜索/重命名、设置页、错误人话化）并入 M1。
- **Plan06**（安全权限）：权限记忆 allowlist 并入 M4；OS 级沙箱（Plan09）维持远期，不阻塞本路线。
- **Plan01**（总纲）：边界与原则不变，本文是其在「Agent 工作台形态」阶段的执行细化。

---

### M8 - Agent 能力深化（0.0.19）✅ 代码已实现（待 dev 验证）

Plan13 主体补齐后，从「更强 agent」第一性原则补的能力深化：

- **联网能力** ✅：`web_fetch`（抓 URL → 提取正文）+ `web_search`（DuckDuckGo HTML，无需 Key）；NetworkAccess 能力，进权限审批与审计。Agent 可查文档 / 查报错 / 查库 API。
- **流式叙述** ✅：工具循环改用 `chat_stream_with_tools`，叙述 token 边流边显；内置标记防泄漏守卫（检测到 DSML / `<ToolCall>` 即停止外显，留给兜底解析），杜绝标记 token 流到界面。
- **并行只读工具** ✅：一轮内多个「自动放行只读工具」（read/list/search/stat/web_fetch/web_search）并发执行，读多文件 / 抓多 URL 提速。
- **自动记忆** ✅：`remember` 工具把可复用事实追加到工作区 MDGA.md「自动记忆」区，下次会话自动注入。
- **沙箱加固** ✅：`run_command` 子进程擦除敏感环境变量（DEEPSEEK_API_KEY 及含 SECRET/TOKEN/PASSWORD 的变量），落实 Plan06/09「默认不把 Key 传给子进程」。
  - ⏳ 完整 OS 级隔离（Windows restricted token / ACL / 网络隔离）属安全关键 FFI，不与本批其他功能混做，留独立版本专门攻坚（Plan09 明确「沙箱失败不能静默降级」，半成品沙箱比无沙箱更危险）。

> 数据治理（会话导出/删除/备份）与视觉/图片按主创决策不在本批。

---

### M9 - Agent 基础设施·第一梯队（0.0.20）✅ 代码已实现（待 dev 验证）

深挖 Claude Code（公开 npm 包）的底层机制后，补齐最能抬高 agent 能力上限的四件地基：

- **读分页** ✅：`read_file` 支持 `offset`/`limit`（行级），返回 `totalLines`/`hasMore`，大文件可分段读取；字节上限提至 1MB。
- **细粒度权限规则** ✅：规则升级为 `[allow:|deny:]<tool>:<glob>`，支持 deny（优先否决）+ 路径 glob；设置页可增删。
- **Steering（运行中插话）** ✅：Agent 运行时输入回车排队，循环每轮开始注入为 user 消息，不打断即可纠偏。
- **Hooks 生命周期** ✅：`.mdga/hooks.json` 的 PreToolUse（退出码非 0 阻断工具）+ PostToolUse（成功后处理）。

### M10 - 基础设施第二/第三梯队（0.0.22）✅ 代码已实现（待 dev 验证）

- **ripgrep 搜索后端**（第三梯队）✅：`search_text` 改用 `ignore` crate（与 ripgrep 同源），gitignore 感知、跳过隐藏/噪声文件、支持正则（isRegex）。
- **缓存友好上下文**（第二梯队）✅：repo map 按会话缓存，保持 system 前缀字节稳定，提升 DeepSeek prompt 缓存命中、省去每轮重新遍历。
- **fallback 模型抗 overload**（第二梯队）✅：主模型多次重试仍失败时，切同族备用模型（flash↔pro）再试一轮。
- **托管后台 shell**（第二梯队）✅：`run_command background=true` 返回 shellId；新增 `get_shell_output`（轮询输出/状态）、`kill_shell`（终止）、`list_shells`（列出）工具。

> 仍待后续：并行+隔离子代理（worktree 隔离属 Windows 重型，与 M8 OS 沙箱一并攻坚）、思考预算（依赖 DeepSeek 推理模型 API 支持，待确认）。

---

### M11 - 三方对照补缺第一批（0.0.24）✅ 代码已实现（待 dev 验证）

经 plan × CC 源码 × 当前实现三方对照，补齐：

- **成本预算/限额** ✅：单任务 token 上限，超额暂停（Plan01 6.4，强化费用透明卖点）。
- **数据治理** ✅：会话导出 Markdown、Token 账本导出 CSV、清除所有会话（Plan01/Spec05）。
- **自定义斜杠命令** ✅：`.mdga/commands/*.md`，$ARGUMENTS 传参（对标 CC `.claude/commands`）。
- **自定义子代理类型** ✅：`run_subtask` agentType → `.mdga/agents/<type>.md`（对标 CC `.claude/agents`）。
- **诊断反馈环** ✅：`.mdga/diagnostics` 配检查命令，改完代码收尾前自动跑、有错自动修（对标 CC 的 LSP/diagnostics 闭环，轻量版）。

> 待做：MCP SSE/HTTP 传输（0.0.25 单独做）、MCP 完整 OAuth、M8.2 AppContainer、并行子代理执行、视觉/思考预算/outputStyle（主创定缓）。

---

## 5. 执行原则

- 每个里程碑可独立交付与验证，按 .dev-rules.md 的迭代节奏推进（CI 窗口不停工，版本归属主创裁决）。
- 对标不是照抄：凡 CC/Codex 的能力与 DeepSeek API 特性冲突（如视觉、原生 plan tool），以 MDGA 自身架构做等效实现。
- 始终保持差异化底色：token 账本透明、本地优先、权限可审计。
