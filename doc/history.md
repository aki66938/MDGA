# MDGA 开发历程

项目代号：MDGA — Make DeepSeek Great Again
仓库地址：https://github.com/aki66938/MDGA
主创：aki66938

版本号规则：`主版本.里程碑版本.功能版本`

---

| 版本 | 更新描述 | 开发者 |
|------|----------|--------|
| 0.0.1 | 项目初始化，建立 Tauri 2 + Rust workspace + React 桌面应用骨架<br>实现 DeepSeek API 流式聊天（SSE），支持 Enter 发送、Shift+Enter 换行<br>实现 API Key 环境变量检测（DEEPSEEK_API_KEY），状态栏显示配置状态<br>实现 Token 用量统计：展示总 token、输入/输出、缓存命中、估算费用<br>实现 assistant 回复 Markdown 渲染（react-markdown + remark-gfm）<br>assistant 消息无气泡背景融入页面，token 统计行独立显示于内容下方<br>建立 GitHub Actions CI/CD，tag 推送自动构建 Windows 安装包并发布到 Release | Claude Code / Codex |
