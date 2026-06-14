# MDGA 开发历程

项目代号：MDGA — Make DeepSeek Great Again
仓库地址：https://github.com/aki66938/MDGA
主创：aki66938

版本号规则：`主版本.里程碑版本.功能版本`

---

| 版本 | 更新描述 | 开发者 |
|------|----------|--------|
| 0.0.1 | 项目初始化，建立 Tauri 2 + Rust workspace + React 桌面应用骨架<br>实现 DeepSeek API 流式聊天（SSE），支持 Enter 发送、Shift+Enter 换行<br>实现 API Key 环境变量检测（DEEPSEEK_API_KEY），状态栏显示配置状态<br>实现 Token 用量统计：展示总 token、输入/输出、缓存命中、估算费用<br>实现 assistant 回复 Markdown 渲染（react-markdown + remark-gfm）<br>assistant 消息无气泡背景融入页面，token 统计行独立显示于内容下方<br>建立 GitHub Actions CI/CD，tag 推送自动构建 Windows 安装包并发布到 Release | Claude Code / Codex |
| 0.0.2 | 实现会话持久化：SQLite 存储，应用重启后历史对话保留<br>实现多会话管理：侧边栏显示历史对话列表，支持新建、切换、删除会话<br>首条消息自动命名会话标题（取前 20 字），无需手动操作<br>流式回复结束后自动保存 assistant 消息及 token 统计到本地数据库 | Claude Code |
| 0.0.3 | 更新 DeepSeek 模型选择：默认使用 `deepseek-v4-flash`，并提供 `deepseek-v4-pro` 可选<br>移除界面中的旧模型别名 `deepseek-chat` / `deepseek-reasoner`，避免继续使用即将废弃的模型 ID<br>按所选模型匹配 token 费用估算价格，提升账本对照准确性 | Codex |
| 0.0.4 | 增加当前会话累计 token 与估算费用展示，历史会话加载后可基于已保存 usage 自动聚合<br>统一单次回复与会话累计费用格式，便于用户观察消耗水平并与账单对照<br>补充前端测试覆盖持久化 usage 聚合展示 | Codex |
| 0.0.5 | 增加当前工作区绑定：用户可输入本地目录路径并保存为 Agent 授权工作区<br>工作区信息写入 SQLite，应用重启后可恢复显示<br>后端保存前校验路径存在且为目录，为后续权限与文件任务边界打基础 | Codex |
| 0.0.6 | 重构工作区绑定为新对话 session 级选择：首屏通过系统目录选择器选择工作区，普通对话正文不再显示路径输入表单<br>conversation 表新增 workspace snapshot 与 mode 字段，发送首条消息时将所选目录绑定到整轮会话<br>接入 Tauri dialog 插件与权限配置，新增前端测试、storage 测试并通过全量 Rust / 前端验证 | Codex |
| 0.0.7 | 打通 workspace 认知闭环：发送消息时前端传入 conversationId，后端从 SQLite 读取该会话 workspace snapshot<br>调用 DeepSeek 前自动注入 system workspace context，使模型能够回答当前会话绑定的工作区路径<br>补充单会话 workspace 查询、system message 注入与前端发送参数测试 | Codex |
| 0.0.8 | 打通首个真实本地工具闭环：DeepSeek 可通过 Tool Calls 请求 `create_file`，MDGA 后端在会话工作区内真实创建文件<br>新增 workspace path guard，拒绝绝对路径和 `..` 越界写入，目标文件存在时拒绝覆盖<br>补充 DeepSeek tool-call 解析、Rust tool-runtime 与桌面后端桥接测试 | Codex |
| 0.0.9 | 实现完整 Agent Tool Runtime：模型可自主调用工具并多轮推理，最多 5 轮工具循环<br>新增文件移动（move_path）、目录删除（delete_dir）、本地命令执行（run_command）工具<br>修复 DSML 双竖线解析 Bug，确保模型内嵌工具调用格式能被正确识别和执行<br>新增权限模式选择器（Restricted / AskEveryTime / WorkspaceAuto / FullAccess），run_command 仅 FullAccess 可用<br>工具执行记录持久化到 SQLite，前端工具事件面板实时展示每步执行结果 | Claude Code |
| 0.0.10 | Agent Loop 控制力强化：工具循环轮数上限从 5 提升到 20，复杂多步任务不再被过早截断<br>多轮过程流式可见：模型调用工具前的叙述内容实时推送，不再黑盒等待<br>新增中断能力：发送中显示「停止」按钮，用户可随时中止 Agent 任务，已执行结果保留 | Claude Code |
| 0.0.11 | 工具执行过程内联渲染：AI 的工具调用直接插入对话流，与叙述文字交错显示，告别底部扎堆面板<br>工具卡片实时展示执行状态：运行中显示动画、完成显示绿色 ✓、失败显示红色 ✗ 与错误原因<br>用户视角下 AI「思考到哪一步、调用了什么工具」一目了然，体验连贯不黑盒 | Claude Code |
| 0.0.12 | 权限审批闭环：修复「每次询问」模式下连读文件都报错的问题，只读操作直接放行，仅写/删/命令才弹审批<br>新增高风险动作审批弹窗：Agent 执行越界或敏感操作前弹框请用户「允许一次/拒绝」<br>低风险命令白名单：cargo check/test、git status、npm test 等常用命令在工作区自动模式下免审批直接执行<br>修复 DSML 工具标记泄漏：任务步数过多撞上限时，不再把内部工具标记当正文显示，改为清晰的上限提示 | Claude Code |
| 0.0.13 | 修复工具执行记录重启后丢失：工具卡片与叙述文字的交错结构现已持久化，重启或切换会话后完整还原<br>历史会话加载时还原内联工具调用记录（✓/✗/⊘ 状态、目标路径），不再只剩纯文字<br>旧版本数据兼容加载，自动升级数据库结构 | Claude Code |
| 0.0.14 | 项目结构认知：会话开局自动注入工作区目录树摘要，模型无需逐层探查即可了解项目骨架<br>工具失败自纠引导：工具执行失败时引导模型读取错误、调整方案后重试，而非反复重复同一次失败调用<br>网络抖动自动重试：DeepSeek API 偶发网络错误 / 服务端错误 / 限流时自动退避重试（最多 4 次），长任务不再因一次网络波动中断 | Claude Code |
| 0.0.15 | 上下文自动压缩（auto-compact）：长任务上下文接近上限时先压缩较早的工具输出，仍超限则把早期对话自动总结为任务进度摘要，对话持续进行不再卡住<br>Agent 工作状态实时可见：思考中（第 N 轮）、执行工具、压缩上下文、输出回复均实时显示并附耗时计秒，不再黑盒等待<br>状态栏新增上下文用量百分比；压缩发生时对话流内显示通知卡片<br>支持工作区根目录 MDGA.md 项目长期记忆文件，跨会话持久注入项目目标与约定，不被压缩冲掉<br>压缩阈值支持环境变量 MDGA_CONTEXT_SOFT_LIMIT 调节，便于验证压缩机制 | Claude Code |
| 0.0.16 | 取消工具调用轮数上限：Agent 任务不再被 20 步硬上限截断，复杂任务可持续推进至自然完成（上下文压缩兜底体积，停止按钮兜底失控）<br>连续工具调用折叠显示：执行中实时展示当前动作，完成后自动折叠为一行摘要，点击可展开查看每一步<br>新增 Plan13 对标路线文档：以 Claude Code / Codex 为最终形态目标的功能补齐总规划 | Claude Code |
| 0.0.17 | 变更管理：文件修改在对话流内显示彩色 diff（+N/−M 行），每轮变更自动快照，「变更」面板可一键回退任意改动<br>任务系统：Agent 自维护任务清单实时显示进度；新增计划模式（先出计划确认后执行）与只读子代理（run_subtask）<br>命令增强：命令输出实时流式显示、支持后台运行；审批新增「总是允许」记忆，同类动作免重复审批<br>输入增强：斜杠命令（/compact、/clear、/init、/rewind、/model）与 @ 文件引用补全；📎 文档导入（TXT/MD/CSV/PDF/DOCX）总结问答<br>MCP 接入：设置页可添加/启停 MCP 服务器，外部工具统一进权限审批与审计；Skills 技能体系（.mdga/skills 渐进披露加载）<br>体验补齐：代码高亮+复制、会话搜索/重命名/置顶/归档、设置页（移至侧边栏底部）、错误提示人话化<br>修复：API 请求无超时导致的「永久思考」隐患；&lt;ToolCall&gt; 工具标记泄漏新变体；纯聊天会话幻觉工具调用（明确提示需绑定工作区，顶栏显示「纯聊天」标识） | Claude Code |
| 0.0.18 | 全新 UI 设计系统（贴近 DeepSeek 品牌）：DeepSeek 蓝主色、亮色 / 深色「深海」双主题、Lucide 线性图标全面替换 emoji、MDGA 自有深海标识<br>布局精修对标 CC/Codex：顶栏精简为工作区 / 上下文 / 变更胶囊；权限模式、模型、计划模式下移至输入框上方控制行（随时可切，下一轮生效）；composer 改为左附件 + 输入 + 圆形发送三段式<br>思考指示由旋转圆圈改为柔和脉冲三点，更贴近「思考中」观感<br>上下文压缩软上限由 96K 提升至 800K（贴合 V4 1M 标称，留 headroom）<br>设置页新增账户余额：调用 DeepSeek 官方接口显示余额状态与各币种总额 / 充值 / 赠送明细，可手动刷新<br>新增 Plan14 UI 设计系统文档 | Claude Code |
| 0.0.19 | 联网能力：新增 web_search（联网搜索）与 web_fetch（抓取网页正文）工具，Agent 可查文档 / 查报错 / 查最新资料<br>流式叙述：Agent 工作时叙述逐字流式输出，不再整块跳出；内置防泄漏守卫杜绝内部工具标记外显<br>并行只读工具：一轮内多个只读操作（读多文件 / 抓多 URL）并发执行，提速明显<br>自动记忆：新增 remember 工具，Agent 可自主把项目约定 / 关键路径沉淀到 MDGA.md，跨会话生效<br>沙箱加固：命令子进程擦除敏感环境变量（API Key / Token / 密码），防止 Agent 命令读取或外泄凭据<br>工具运行动画由快速旋转圆圈改为四角星柔和闪动 | Claude Code |
| 0.0.20 | Agent 基础设施四件地基：① 大文件分页读取（read_file 支持 offset/limit，模型可分段翻页）② 细粒度权限规则（deny 优先 + 按工具/路径 glob，如 deny:read_file:**/.env）③ 运行中插话 Steering（Agent 工作时输入排队，下一轮注入纠偏，不打断任务）④ Hooks 生命周期（.mdga/hooks.json 的 PreToolUse 可拦截工具、PostToolUse 后处理）<br>设置页重做为左栏分类导航 + 右栏内容双栏，加入各项说明；弹窗加宽<br>思考与工具运行图标统一为四角星缓转闪动（DeepSeek 蓝） | Claude Code |
| 0.0.21 | 修复自动更新：开启 updater 清单产物（createUpdaterArtifacts），CI 发版起会生成并发布 latest.json，客户端「检查更新」从此可真正发现新版并自动下载安装<br>检查更新全程反馈：检查中 / 已是最新 / 发现新版本 / 失败原因，不再点了无反应 | Claude Code |
| 0.0.22 | 搜索升级为 ripgrep 同源引擎：自动尊重 .gitignore、跳过隐藏/噪声文件，支持正则，搜大型项目又快又准<br>上下文缓存优化：工作区结构摘要按会话缓存，提升 DeepSeek 缓存命中、降低费用与延迟<br>模型容灾：主模型持续不可用（过载/限流）时自动切换同族备用模型继续<br>托管后台命令：后台命令返回句柄，Agent 可轮询输出、查看状态、随时终止<br>检查更新按钮交互优化：结果直接在按钮上显示 10 秒、期间禁用防误触，尺寸不变 | Claude Code |
| 0.0.23 | 命令沙箱（受限令牌，M8.1）：run_command 在降权的受限令牌 + Job Object 沙箱中执行——剥离管理员特权、进程随会话干净销毁、子进程环境擦除密钥；失败即拒绝执行不降级<br>设置「模型与权限」新增命令沙箱开关，少数需特权命令受阻时可临时关闭<br>（网络与文件路径隔离将由后续 AppContainer 版本 M8.2 提供） | Claude Code |
| 0.0.24 | 成本预算：可设单任务 token 上限，超额自动暂停防止失控烧 token（费用透明卖点强化）<br>数据治理：设置新增「数据」分类，支持导出会话（Markdown）、导出 Token 账本（CSV，可与官方账单对账）、清除所有会话<br>自定义斜杠命令：工作区 .mdga/commands/*.md 自动进斜杠菜单，$ARGUMENTS 传参<br>自定义子代理类型：run_subtask 可指定 agentType 加载 .mdga/agents 角色定义<br>诊断反馈环：配置 .mdga/diagnostics 后，Agent 改完代码收尾前自动跑检查，有错自动修复 | Claude Code |
| 0.0.25 | MCP HTTP 传输：MCP 服务器除 stdio 外支持 http(s):// 地址（Streamable HTTP，自动识别 JSON / SSE 响应），可填静态 Bearer Token 接入大量托管服务<br>MCP OAuth 2.1（PKCE 授权码流，浏览器授权 + token 自动持久化）⚠️ **未经真机验证**：缺少需要 OAuth 的真实 MCP 服务器测试环境，代码按规范实现、PKCE 经 RFC 7636 官方测试向量验证，但端到端流程待后续真机确认<br>修复 Agent 身份混淆：明确告知模型 MDGA 不是 Claude Code、配置目录是 .mdga/（非 .claude/），杜绝模型沿用训练记忆里的 Claude 约定去找/改 .claude/settings.json<br>新增 add_mcp_server 工具：Agent 可用 MDGA 真实机制（写入服务器表 + 即时连接）为自己注册 MCP，而非编辑无效配置文件 | Claude Code |
| 0.0.26 | 结构化提问：需求含糊时 Agent 弹出选项卡片请你确认（支持单选 / 多选 / 自定义「其他」），不再擅自假设方向<br>异步子代理：探索类子任务可后台运行，立即返回任务号、回头取结果，长调查不阻塞主对话，可随时查看进度或终止<br>代码检索增强：内容搜索新增「仅列文件 / 计数 / 上下文行 / 大小写不敏感 / 跨行 / 按文件类型或文件名过滤」等模式；新增按文件名快速查找文件<br>接入 MCP 资源：可列举并读取已连接 MCP 服务器暴露的资源<br>超大工具输出自动落盘为工作区文件、仅回传摘要与路径，避免撑爆上下文（需要时用 read_file 分页查看） | Claude Code |
