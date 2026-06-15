# Plan14 - UI 设计系统（DeepSeek 风格重设计）

项目代号：MDGA
文档定位：MDGA 整体 UI 视觉重设计的设计系统规范（brand-spec）。布局参考 Claude Code Desktop / Codex，**配色、图标、风格全部重新规划，贴近 DeepSeek 品牌**。本文是后续所有视觉实现的唯一 token 来源。

> 由 web-design-engineer skill 工作流产出，主创于 Checkpoint 1 审定方向：DeepSeek 蓝单主色、亮色+深色双主题、Lucide 图标库。

---

## 1. 品牌定位

- **流派**：Modern Tool / Builder SaaS（Linear / Vercel / Raycast 家族）——开发者 Agent 工具，**重新着色为 DeepSeek 身份**。
- **视觉温度**：安静的工作区 + 一抹自信的品牌蓝。克制、专业、可长时间使用，绝不 cyber-neon / 紫粉渐变。
- **标识**：MDGA 自有 mark——一道「深海声纳波纹」蓝色曲线（致敬 DeepSeek 的「deep / 深海」，**非** DeepSeek 官方鲸鱼 logo 商标）。
- **MDGA ≠ DeepSeek**：MDGA 是为 DeepSeek 打造的第三方客户端，借用 DeepSeek 蓝的精神，但有自己的标识与名字。

---

## 2. 色彩系统

### 2.1 亮色主题（默认，贴合 DeepSeek chat 亮色优先）

| 角色 | 值 | 用途 |
|---|---|---|
| brand | `#4D6BFE` | 主操作、激活态、链接、品牌强调 |
| brand-hover | `#3B57E0` | hover / pressed |
| brand-active | `#2F49C8` | pressed 更深 |
| brand-tint | `#EEF1FF` | 极浅蓝底（hover 区） |
| brand-tint-strong | `#E5EAFF` | 选中会话、用户气泡 |
| canvas | `#F7F8FB` | 应用画布底色（冷调浅灰） |
| surface | `#FFFFFF` | 卡片 / 弹层面 |
| surface-2 | `#FAFBFD` | 侧栏 / 次级面 |
| border | `#E8EAF0` | 发丝边 |
| border-strong | `#D8DCE6` | hover 边 |
| text | `#1A1D24` | 主文本（墨） |
| text-2 | `#5A6172` | 次文本 |
| text-3 | `#8A92A6` | 弱文本 / 占位 |
| success | `#2BA471` | 成功 / diff 增行（冷调绿，和谐蓝） |
| danger | `#E5484D` | 失败 / diff 删行 |
| warning | `#D9941A` | 审批 / 警示 |

### 2.2 深色主题「深海 Deep Sea」（点击切换）

深海导航而非 GitHub-cyber：以深靛蓝为底，呼应「deep」。

| 角色 | 值 |
|---|---|
| brand | `#6B83FF`（提亮，深底上对比足够） |
| brand-hover | `#8497FF` |
| brand-tint | `#1E2747` |
| brand-tint-strong | `#26315A` |
| canvas | `#0E1422`（深海蓝黑，非纯黑） |
| surface | `#161D2E` |
| surface-2 | `#121829` |
| border | `#232C40` |
| border-strong | `#303A52` |
| text | `#E8EBF2` |
| text-2 | `#9AA3B8` |
| text-3 | `#6B7488` |
| success | `#3DBA88` |
| danger | `#F0666B` |
| warning | `#E0A53A` |

### 2.3 实现方式

- 全部 token 用 CSS 自定义属性挂在 `:root`（亮色）与 `:root[data-theme="dark"]`（深色）。
- 组件只引用 `var(--xxx)`，禁止硬编码 hex。
- 主题持久化 localStorage `mdga.theme`，默认 `light`；尊重 `prefers-color-scheme` 作为首次默认。

---

## 3. 字体

- **UI**：中文 `PingFang SC` / `Microsoft YaHei` 领衔；Latin 用 `Geist`（避开 Inter 陈词，工具感）+ 系统兜底。
- **代码**：`JetBrains Mono`（保留）。
- **字号**：正文 14px（密度型工具）；次要 12.5px；标题/品牌 17–20px；代码 12.5px。
- **字重**：仅 400 / 500 / 600 三档。

---

## 4. 形状 / 阴影 / 动效

- **圆角层级**：容器 12 / 卡片·输入 10 / 按钮·chip 8 / 标签 6（非一刀切药丸）。
- **阴影**：极简。卡片靠发丝边 + 底色对比；弹层 `0 8px 28px rgba(20,30,60,.12)`（深色用 `rgba(0,0,0,.4)`）。
- **动效**：120–180ms ease-out（hover / 状态）；220ms `cubic-bezier(.2,.8,.2,1)`（面板 / 弹窗）；`prefers-reduced-motion` 时禁用。

---

## 5. 图标

- **方案**：`lucide-react` 图标库，统一描边 1.5、尺寸 16–18px、继承 currentColor、激活态品牌蓝。
- **全面替换** 现有 emoji（📌🗂⚙📎✦⊜⊘ 等）。映射：
  - 新对话 `SquarePen`、搜索 `Search`、置顶 `Pin`、归档 `Archive`、删除 `Trash2`、设置 `Settings2`、导入 `Paperclip`、计划 `ListChecks`、停止 `Square`、发送 `ArrowUp`、变更 `GitCompare`、上下文 `Gauge`、MCP `Plug`、技能 `Sparkles`→`BookText`、主题切换 `Sun`/`Moon`、工具成功 `Check`、失败 `X`、拒绝 `Ban`、运行中 `Loader`、通知 `Info`、回退 `Undo2`。

---

## 6. 布局（参考 CC/Codex，精修）

- **侧栏（~264px）**：顶部 MDGA 标识 + 「新对话」品牌蓝主按钮；搜索框；会话分区（置顶 / 历史 / 已归档）；底部 footer = 设置 + 主题切换。
- **顶栏**：会话标题（可编辑）；右侧状态收为**胶囊 chip**——工作区 / 上下文用量 / 权限模式 / 模型 / 变更。
- **线程**：居中 ~780px 列；用户消息浅蓝气泡右对齐；助手无气泡流式；工具行/折叠组/diff 用 Lucide 图标克制呈现；ChangeSet 汇总弱化。
- **Composer**：卡片含 textarea + 控制行（📎导入 / 计划开关 / 发送）；斜杠 & @ 菜单浮于上方。
- **弹层**：审批 / 设置 / 变更 → 统一 modal 风格（发丝边 + 弹层阴影）。
- **空状态**：MDGA 标识 + 工作区选择 + 2–3 能力 chip，DeepSeek 风。

---

## 7. 实施顺序

1. CSS token 层重写（亮/深双主题变量）+ 全局基础样式。
2. Lucide 接入，替换全部 emoji，新增主题切换与品牌 mark。
3. 逐组件套用 token：侧栏 / 顶栏 chip / 线程 / 工具卡 / diff / composer / 弹层 / 空状态。
4. tsc + vite build 验证，dev 自查。

非破坏性：纯前端视觉层，不动后端与交互逻辑。
