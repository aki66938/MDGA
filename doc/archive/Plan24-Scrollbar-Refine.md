# Plan24（并入 0.0.33）- 滚动条:对话栏的右边界 + 精细化

项目代号:MDGA
文档定位:把对话区滚动条从「居中 760 窄列右缘」归位到**对话栏(栅格第 2 轨)右边界**,并做细/淡/融入主题的精细化。纯前端 CSS。

## 核心原则
滚动条 = 第二栏(对话栏)的右边界,由**栅格列结构**决定,不依赖任何「窗口最右」的硬编码。今天它与窗口右缘重合;未来停靠式第三栏在 `.app-shell` 追加第 3 轨后,对话栏收窄、滚动条自动落到第 2/3 栏分界,无需返工。

## 实现(`apps/desktop/src/styles.css`)
### C1 归位(列相对、可扩展)
- 新增 `--col-gutter: 48px`(对话栏列内边距),`.workspace` padding 改用它。
- `.message-list` 改为「满列滚动容器 + 内容居中」:`align-self: stretch; align-items: center; margin: 0 calc(-1 * var(--col-gutter)); padding: 24px var(--col-gutter)`。负外边距抵消**父列自身**内边距,撑到 `.workspace` border-box 边缘(被其 `overflow:hidden` 裁切)→ 滚动条落在对话栏右边界。无窗口宽度硬编码;加第三栏后随列收窄自动归位。
- `.message-row`、`.agent-working` 限宽 `min(760px,100%)`,由列的 `align-items:center` 居中,保持消息/思考指示 760 居中、左右对齐不变。
- `.app-shell` 栅格保持 `260px minmax(0,1fr)`,注释标明第 3 轨追加位;本次不引入三栏。

### C2 精细化(细、淡、融入主题)
- 亮/暗各一套 `--scrollbar-thumb` / `--scrollbar-thumb-hover`(淡中性色)。
- 全局 `::-webkit-scrollbar`(WebView2=Chromium):宽 10px、轨道透明、thumb 圆角 + `border:3px transparent` + `background-clip:padding-box`(可见宽≈4px)、hover 微深、`scrollbar-corner` 透明。统一收编侧栏/设置/代码块所有滚动条。

## 不做
- 第三栏的视觉/结构(暂不规划);只在栅格与变量上留扩展位。
- `scrollbar-gutter: stable`(默认不加)。

## 验收
- 纯 CSS;dev 走查:滚动条在对话栏右边界、细、配色融入、亮/暗协调;消息仍 760 居中;侧栏/设置/代码块滚动条同步精细化。
