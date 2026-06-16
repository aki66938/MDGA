# Plan28 - 中期改进路线（活跃总纲）

项目代号:MDGA
文档定位:0.0.35 起的**中期**改进总纲。融合两个来源并统一排优先级——
- **(我反思)Agent 求真能力**:DeepSeek 自查时把 sandbox-runtime 误判"空壳"(实 285 行)、把测试误判"只 2 个"(实 ~99 内联),根因不是模型读不懂,而是**工程没强制它"接地"**(凭依赖清单/文件名臆断、对可度量事实不执行求真)。用工程把"会读会跑"逼成"下结论前必须读过跑过"。
- **(DeepSeek 指出 + 主创认同)中期工程债**:App.tsx 巨石、agent-core 占位、集成测试薄、自我示范缺、文档归档、价格硬编码、OS 沙箱悬置。

> 已完成的 Plan 已归档 `doc/archive/`;Plan13 留作历史总路线;本 Plan28 为当前活跃总纲,按阶段推进、逐项另立子计划落地。

## 原则:语言无关
所有"求真"能力**不得绑定单一语言/构建系统**。构建/测试/符号/测试标记一律**按检测到的项目类型**处理(Rust `Cargo.toml`、Node `package.json`、Python `pyproject.toml`/`setup.py`、Go `go.mod`、Java `pom.xml`/`build.gradle`…),不得硬编码 Cargo。

---

## Track 1 — Agent 求真能力(语言无关，最高优先)

### 🔴 P0-1 接地纪律（灵魂文件 `agent_prompt.rs`）
行为准则新增三条硬规矩:
1. **断言必有证据**:对某文件/模块/测试覆盖/依赖下结论前,**必须已 read_file 读过该文件 或 run_command 跑过对应命令**;**禁止**凭构建/依赖清单(Cargo.toml / package.json / pyproject.toml / go.mod …)、目录列表、文件名臆断内容("依赖少 / 文件小"≠"没代码")。
2. **度量优先执行**:测试数量 / 构建状态 / 规模等可度量事实,用**项目自身的工具实测**(自动识别 cargo/npm/pytest/go/maven…),不靠数文件名或目录推断。
3. **整库评估先完整性自检**:产出"项目级"判断前,确认每个下了结论的对象都有实际工具证据,无证据的先补查或显式标注"未核实"。

### 🔴 P0-2 `code_overview` 工具（`tools.rs`，语言无关）
输入 crate/目录/文件,返回**结构化事实**,让模型一眼拿到"实质"判断而无需读完/靠旁证:
- **LOC**;
- **公开符号**(按语言:Rust `pub fn/struct/enum/trait`;TS/JS `export`/`function`/`class`;Python `def`/`class`;Go `func`/`type`;Java `class`/`interface`…);
- **测试计数**(按语言:`#[test]`/`#[tokio::test]`;`it(`/`test(`/`describe(`;`def test_`/`class Test`;`func Test…`…);
- **识别到的构建/依赖文件**。
- 实现:复用现有 ripgrep 引擎做正则计数 + 按扩展名/构建文件判语言;**不上 AST**,保持轻量。
- 目标:把"空壳"误判从"靠纪律避免"升级为"工具层面难以发生"。

### 🟠 P1-3 度量默认执行 / `project_stats`
识别项目类型→能跑就跑其**测试 / 构建**求真(测试总数、通过/失败、构建状态)。复用并**泛化** Plan27 #7 验证回路的 `detect_verification_command` 到"度量"用途。可做成 `project_stats` 工具或在分析场景引导执行。

### 🟠 P1-4 报告自核验回路（claim-grounding pass）
agent 产出整库分析/报告前,过一遍"每条结论是否有工具证据",无证据回去补查或标注。把"写完即验"(Plan27 #7)扩展到"**断言即核**"。

---

## Track 2 — 中期工程债

### 🟠 P1-5 App.tsx 拆分（纯重构，不发版）
按 Plan16 拆 main.rs 的同款打法(模块化、**行为零变更、UI 零变更**),把 ~3800 行的 App.tsx 拆成组件:对话流渲染、Composer、侧边栏、设置面板、工具卡片、审批/提问弹窗、变更面板、命令面板/帮助等。收益:组件级测试、**前端从此能并行开发**(解决多轮的单文件瓶颈)、降风险。只重构、代号不变。

### 🟠 P1-6 自我示范 `.mdga/`
给项目自身配 `.mdga/diagnostics`(按本项目=`cargo check --workspace` + `tsc`)让 agent 写完自检;加 `.mdga/commands/`(如 `/release`)。便宜、立竿见影、顺带验证 Plan27 #7 验证回路 + dogfood 可扩展机制。

### 🟡 P2-7 集成测试
storage(SQLite `:memory:` 端到端 CRUD)、mcp-client(JSON-RPC 解析)、tools(路径守卫 / 错误路径)。补齐"集成层"薄弱面(内联单测已 ~99,这里补端到端)。

### 🟡 P2-8 价格透明
`token-accounting` 价格快照加注释/时间戳标注"需手工更新";多供应商下非 DeepSeek 已显示「—」,后续如需再做 per-provider 价表。

### 🧭 中长期(单独立项)
- **P3-9 agent-core 抽取**:先抽象 Tauri IPC / 存储接口,再把工具调度 / 消息构建 / 压缩触发迁入 `agent-core`(现仅 23 行占位),为独立测试 / 将来多端复用。
- **P3-10 AppContainer 沙箱(M8.2)**:文件路径 + 网络隔离,补齐安全纵深(当前仅降权 + Job Object)。
  - **进展(0.0.39,基座完成但暂未启用)**:`crates/tool-runtime/src/appcontainer_win.rs` 已实现并真机验证——
    文件路径隔离(默认拒绝 + 工作区 ACL 授权)、网络默认拒绝 + 能力 SID 放行(放行需宿主防火墙开)、
    powershell 包装 + 擦密钥 + 容器临时目录;另修复 R3(后台命令绕过沙箱的 fail-open)。
  - **拦路问题(showstopper)**:容器内 native console 程序(git/npm/node…)的 stdout/stderr **不回传**——
    根因是 AppContainer 断开「容器进程 → 其子进程」的 I/O 继承(powershell 自身输出回得来,其派生的
    native 孙进程输出丢失)。**ConPTY 限时 spike 已验证仍无解**(非容器能捕获、容器不能;回归测试
    `conpty_appcontainer_loses_native_child_output`)。
  - **决策**:默认仍用受限令牌沙箱;AppContainer 全套实现 + 隔离测试以 `#![allow(dead_code)]` 门控保留。
    待**换稳定版 Windows**(本机 Canary 28000,疑似构建相关)或**真实无控制台进程(Tauri)环境**复测后再评估启用。
    详见 memory `appcontainer-console-output-blocker`。

---

## 已完成（本轮）
- Plan 归档:25 份已完成 Plan 移入 `doc/archive/`,Plan13 留作总路线。

## 排期建议
- **下一版(0.0.36)**:Track 1 的 P0-1 + P0-2(接地纪律 + code_overview)——直接提升 agent 求真,最高性价比。
- 其后:P1-3/P1-4(度量执行 + 报告自核验)、P1-5(App.tsx 拆分,独立重构)、P1-6(.mdga 示范)。
- 再后:P2 与中长期按需。

## 不做（明确排除）
- 项目元信息注入 / 让 agent 低成本完成外部动作的专用工具(对标 CC/Codex 的那类)——主创定**远期、暂不考虑**。
