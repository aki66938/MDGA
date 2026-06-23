
## 自动记忆（由 Agent 维护）
- ## 关键架构事实
- 后端权限模型: PermissionMode = restricted / ask_every_time / workspace_auto / full_access
- DeepSeek 模型: deepseek-v4-flash(默认) 和 deepseek-v4-pro
- MCP 服务器通过 add_mcp_server 工具注册（非编辑配置文件）
- 配置目录 .mdga/，项目记忆 MDGA.md，技能 .mdga/skills/，钩子 .mdga/hooks.json
- Wiki 知识库 .mdga/wiki/ 已构建（23区段/82文件/2434定义），可 repo_wiki query 查询
- doc/ 下有 27 个架构规划文档，doc/archive/ 含早期计划
