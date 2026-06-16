---
description: 全量自检：cargo check --workspace + 前端 tsc，汇报结果
---
对本项目做一次全量静态自检,逐项执行并汇报结果(有错先读 error 再修到通过):
1. 后端:`cargo check --workspace`(0 警告 0 错误为准)。
2. 前端类型:在 `apps/desktop` 下 `npx tsc -p tsconfig.json --noEmit`(exit 0 为准)。
$ARGUMENTS
完成后用一句话总结是否全绿;若有问题,列出文件:行与原因。
