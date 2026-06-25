# Plan29 · MDGA 能力优化待办看板(Capability Optimization Backlog)

> 来源:0.0.79 后做的「Agent 发展里程碑 + MDGA 十维评分」深度评审(7 簇网搜调研 + 4 子 agent 读码评分 + 综合)。
> 总分 ≈ **73.5/100(7.35/10)**。本看板把评分里每维的 gap/improvement 整理成**可认领的优化项**,按 ROI 排序。
> 用途:**供后续多个子 agent 并行认领处理**。每项带维度、现状分、目标分、具体做法(含文件锚点)、粗估工作量/风险、状态。
> 红线沿用全项目约定:安全(沙箱/CSP/路径守卫)不弱化;不动 NovaCode;发版走 v0.0.X tag。

## 十维现状分
| 维度 | 分 | 形态 |
|---|---|---|
| ① 推理/思考深度 | 7 | 能用但浅 |
| ② 工具/接口(MCP/浏览器/结构化) | 8 | 生产候选 |
| ③ Agent 循环/harness | 7.5 | 接近 |
| ④ 编排/多智能体 | 7 | 能用但浅 |
| ⑤ 上下文工程 | 8 | 生产候选 |
| ⑥ 记忆 | 7 | 能用但浅 |
| ⑦ Prompt/spec 工程 | 8 | 生产候选 |
| ⑧ **评估与可观测** | **6** | **唯一明显短板·系统性风险** |
| ⑨ 安全/权限/沙箱 | 8 | 生产候选 |
| ⑩ 自主性/规划/自我纠错 | 7 | 能用但浅 |

---

## P0 — 最高 ROI(先做;⑧ 是其余一切「自我进化」的前置)

### OPT-1 · 评估与可观测框架(维度⑧ 6→8)
- **为何最高**:最低分 + 系统性盲点;外部材料反复指出「评估/可观测是 25–26 企业 agent 失败主因(非模型质量)」;MDGA 当前"跑测试看绿"≠真 eval,缺量化指标与全链路 tracing。
- **做什么**:
  - 接 SWE-bench / τ-bench 风格的**量化 eval 框架**(小型任务集 + 通过率/`pass^k` 一致性指标),挂 CI。
  - 关键路径(压缩 / LSP / widget 渲染 / 每个工具轮)**结构化 tracing**(span + 属性,可考虑 OTel GenAI 语义约定)。
  - **按工具/按轮次成本分解**展示(复用 token_usage_attribution)。
- **锚点**:`apps/desktop/src-tauri/src/{test_report.rs,verification.rs}`、`crates/token-accounting`、CI(vitest+cargo 矩阵)。
- **工作量/风险**:大 / 中。**状态:待认领**

### OPT-2 · 多供应商 pricing + 成本路由(维度⑧+①+⑩)
- **为何**:non-DeepSeek 现在显「—」→ 成本算不全、**成本感知规划做不了**。
- **做什么**:补 openai/gemini/claude/智谱/Kimi 单价库;`send_message` 前算「一轮预算」,`plan_mode` 超预算 warn;为"按成本选档/选模型"打底。
- **锚点**:`crates/token-accounting`、pricing_capture、agent_loop。
- **工作量/风险**:中 / 低。**状态:待认领**

---

## P1 — 高价值(P0 之后)

### OPT-3 · 真规划深度 + 显式反思循环(维度⑩ 7→8)
- 现状只是「批准后严格执行」,缺自主拆解与「成果/遗留」自陈。
- 做:`detect_task_breakdown` 自动切子任务 + plan UI「自动拆解建议」;finalize 后跑独立 system prompt 自评(成果/遗留)记日志,喂下会话。对应 PEV / Ng-Reflection。
- 锚点:agent_loop、plan 模式(ROLE_PLAN)、subagent.rs。**工作量/风险**:中 / 中。**状态:待认领**

### OPT-4 · 多智能体编排升级(维度④ 7→8)
- 缺自动 fan-out、子代理共享上下文、声明式 DAG、plan-vs-实际偏差检测。
- 做:`SubagentOutput{status,result_json,metrics,errors}` 统一 schema + `shared_context`;轻量 JSON DAG(steps + depends_on + join_policy);`ToolDependencyAnalyzer` 提议 fan-out。对应 Sub-Agent-as-Tools / LangGraph 状态图。
- 锚点:`run_subtask`/`run_parallel_subtasks`、subagent.rs、command_run.rs。**工作量/风险**:大 / 中。**状态:待认领**

### OPT-5 · 记忆结构化 + 跨会话长期库(维度⑥ 7→8)
- `MDGA.md` 无 schema/时间轴;无情景消歧;跨会话遗忘;symbol 映射变更后失效。
- 做:`MDGA.md` 加 YAML frontmatter(version/tags/confidence);`.mdga/memory-log.jsonl`(逐条 timestamp + relevance_decay);`.mdga/.memory-embeddings` 全局向量库,init 时 Top-K prepend。对应 MemGPT/Letta 分层记忆、Episodic/Semantic/Procedural。
- 锚点:remember 工具、read_workspace_memory、`crates/wiki`、`.mdga/`。**工作量/风险**:中 / 中。**状态:待认领**

---

## P2 — 打磨/补强(逐维 gap,按维度归并)

- **OPT-6 推理可见性 + 成本预估(①)**:工具轮 reasoning 改 per-turn 数组 + 「展开全程推理」;思考深度滑块 hover 弹「本档估 x–y token」(用 pricing 库)。锚点 `thinking.rs`、ReasoningPart。**待认领**
- **OPT-7 工具接口补强(②)**:新增 `mcp_read_resource(server_id,uri)` 工具(白名单校验);`compute_tool_result_tokens` 加 browser 分支(浏览器工具计费);browser 工具 `url_whitelist`(默认 localhost),收紧 computer-use 网络门控。锚点 `crates/mcp-client`、`crates/browser`、tools.rs。**待认领**
- **OPT-8 循环健壮性(③)**:`RetryPolicy{max_attempts,backoff,retriable_errors}` 指数退避 + `ToolFailure` 分支化恢复;中断后注入「按已有结果重新规划」;探索"插话可打断工具轮"。锚点 agent_loop、`crates/agent-core`。**待认领**
- **OPT-9 上下文/检索(⑤)**:本地 embedding(ollama/llamafile,可选)+ `.mdga/.embeddings` 向量缓存;archive 簇聚/年表索引(替枚举 read);压缩阈值按实测窗口自适应(去硬编码)。锚点 `compaction.rs`、code_search、codemap。**待认领**
- **OPT-10 prompt/spec 自省(⑦)**:`SKILL.md` 加 frontmatter + JSON tools section,load 后 cache 进工具库;`.mdga/hooks.schema.json` 标准化;`run_subtask` 加 `capability_override`(read-only/no-delete/network-off)细粒度权限。锚点 prompt.rs、load_skill、hooks。**待认领**
- **OPT-11 安全审计(⑨)**:DB 新表 `command_audit(conv_id,timestamp,tool,effect,approved_by)`(接 feed_tool_denial);`MDGA_LOW_RISK_PATTERNS` 可配;`apply_multi_patch` 写前 full-hash check-then-write 防 TOCTOU;ConPTY 跨版本真机验。锚点 permissions.rs、`crates/sandbox-runtime`。**待认领**
- **OPT-12 自主性补强(⑩)**:doom-loop 改阈值梯度(3 轮 warn→改思路、6 轮→升子代理),识别"卡在难题"而非仅"打转"。锚点 `loop_guard.rs`。**待认领**

---

> 认领约定:子 agent 取走某 OPT 时把「状态」改为「认领中@<agent>」,完成发版后改「已发布@vX.Y.Z」。P0 两项建议优先、且 OPT-1 先行。
