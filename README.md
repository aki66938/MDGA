# MDGA

**Make DeepSeek Great Again** — Windows 桌面端 DeepSeek 本地客户端

基于 Tauri 2 + Rust + React 构建，本地优先，无云账号，无数据上传。API Key 由用户在应用「设置 → 模型供应商」中配置，存于本地，不上传云端。

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

---

## 下载安装

前往 [Releases](https://github.com/aki66938/MDGA/releases) 下载最新版本的 Windows 安装包：

- `.msi` — 推荐，Windows Installer 标准格式
- `.exe` — NSIS 安装程序

安装后**无需任何账号**，在应用内配置好模型供应商即可直接使用。

---

## 使用前提

首次启动后，在应用内打开「**设置 → 模型供应商**」配置主模型：

1. 选择供应商预设（DeepSeek / 智谱 GLM / 月之暗面 Kimi / 通义 / 自定义）
2. 填入该供应商的 API Key 与模型 ID（Base URL 留空走官方端点，自托管/代理可在高级设置里覆盖）
3. 点击「保存」即可开始对话

DeepSeek 的 API Key 可在 [DeepSeek 开放平台](https://platform.deepseek.com) 获取。Key 存于本地数据库，不上传云端。

---

## 功能

MDGA 是一个**本地优先的 AI 编码 agent**（对标 Claude Code / Codex）：用你自己的 API Key，在你的工作区里读写代码、跑命令、查资料、自我验证——全程本地，无云账号、无数据上传。

**对话与基础**

- 多供应商流式聊天（DeepSeek / 智谱 GLM / Kimi / 通义 / 任意 OpenAI 兼容），实时输出 + Markdown 渲染
- 每轮 Token 用量与估算费用（含缓存命中）；可设单任务预算上限防失控
- 本地运行，无云同步、无账号；新版本发布时应用内自动提示更新

**Agent 能力**

- 工作区内文件读 / 写 / 编辑 / 移动 / 删除，精确补丁，ripgrep 级搜索
- 命令执行在 **AppContainer / 受限令牌沙箱**中（文件 + 网络隔离、擦除密钥环境）；后台命令、计划模式、子代理、文件检查点回退、运行中插话
- 联网搜索 / 抓取、文档导入、视觉识图、MCP 外部工具、Skills、项目长期记忆
- 四档权限（只读 → 完全访问）+ 细粒度规则；写完自动「编译 / 测试」验证并自纠

**代码智能（LSP + 符号地图）**

- 接入**语言服务器（LSP）**，给 agent 编译器级的「跳转定义 / 找全部引用 / 看类型签名 / 实时诊断」，大代码库里少臆造符号、少静默改坏（支持 Rust / TS·JS / Python / Go / C·C++ / Ruby / PHP / Lua 等）
- **无需你配置**：agent 自动探测系统里已安装的语言服务器并使用；缺失时它可自行用命令按需安装（前提是该语言的工具链已在，如装 gopls 需先有 Go）；都没有也不报错，自动退回文本搜索
- **仓库符号地图**：tree-sitter + PageRank 给出「按引用度排名的关键符号」，会话开局即注入，让 agent 一上来就知道核心代码在哪；无 grammar 的语言走通用启发式回退，**任何语言都有粗粒度地图**

---

## 版本记录

查看 [doc/history.md](doc/history.md) 了解完整版本迭代记录。

---

## 二次开发

本项目基于 [MIT License](LICENSE) 开源，允许自由 fork 和二次开发。

```
git clone https://github.com/aki66938/MDGA.git
cd MDGA
npm install
cd apps/desktop && npx tauri dev
```

**环境依赖**：Node.js 20+、Rust stable、Visual Studio Build Tools（含 C++ 工作负载）

---

## 技术栈

- [Tauri 2](https://tauri.app) — 桌面应用框架
- [Rust](https://www.rust-lang.org) — 后端逻辑与 API 调用
- [React](https://react.dev) — 前端界面
- [DeepSeek API](https://platform.deepseek.com/docs) — 大模型接口
