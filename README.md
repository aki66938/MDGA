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

- DeepSeek API 流式聊天，实时输出
- 自动检测 API Key 配置状态
- 每次回复展示 Token 用量与估算费用（含缓存命中）
- Assistant 回复 Markdown 渲染（代码块、列表、表格等）
- 本地运行，无云同步，无账号体系
- 新版本发布时应用内自动提示更新

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
