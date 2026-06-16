---
description: 发版助手：核对 .dev-rules 后提交、打 tag、推送触发 CI 发版
---
准备为 MDGA 发布一个新版本(严格遵循 doc/archive 与 .dev-rules.md 的发版规则)。版本号:$ARGUMENTS
步骤(每步确认无误再下一步,**版本号由主创确定,不要自行决定**):
1. 确认工作树:`git status`;确认 `cargo check --workspace` 与前端 `tsc` 全绿(可用 /check)。
2. 在 `doc/history.md` 顶部数据行新增该版本一行(面向用户、写能感知的变化,不写实现细节)。
3. 提交:信息格式 `<type>(<scope>): <summary>`,正文说明改动,末尾保留 Co-Authored-By。
4. 打 tag `v<版本号>` 并 `git push origin main` + `git push origin v<版本号>`(tag 触发 .github/workflows/release.yml 构建 Windows 安装包)。
5. 报告 commit、tag、推送结果。
