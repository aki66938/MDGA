# Plan23 - Composer 统一化与工作区入口整合(UI 布局)

项目代号:MDGA
文档定位:把输入区(composer)重构为单一圆角容器、控件收进底部行;工作区入口从"首屏独立大按钮 + 顶栏只读胶囊"整合为 **composer 底部可点胶囊**,空态与会话中都随手可达。版本号主创提交时定。

> 后端确认:`set_workspace_path` 设的是**全局 active workspace**,`send_message` 实际用的是**会话自身 `workspace_path`**(创建时绑定),**无改已有会话工作区的命令** → 本计划补一个后端命令。

## 1. 范围
| 级别 | 项 | 端 |
|:---:|---|---|
| 🔴A | 新增"改已有会话工作区"命令 + repo_map 缓存失效 | 后端 |
| 🔴B1 | composer 统一为单一圆角容器,控件收进底部行(附件→「+」、发送内嵌、权限/模型/计划同级) | 前端 |
| 🔴B2 | 工作区胶囊进 composer 底部行(空态=选草稿,会话中=改绑),去首屏独立大按钮 | 前端 |
| 🟠B3 | 顶栏品牌瘦身,移除与胶囊重复的顶栏工作区 chip(上下文%/变更保留) | 前端 |
| 🟠B4 | 首屏聚焦输入,能力卡弱化为小提示 | 前端 |

## 2. 分工(可并行,文件域不重叠)
- **子 agent A(后端)**:`crates/storage/src/lib.rs`、`apps/desktop/src-tauri/src/{commands.rs, main.rs}`。
- **子 agent B(前端)**:`apps/desktop/src/App.tsx`、`apps/desktop/src/styles.css`。

## 3. 接口契约

### 后端命令(A)
- storage 新增 `update_conversation_workspace(conn, conv_id, path: Option<&str>, name: Option<&str>) -> SqlResult<Conversation>`:更新 `workspace_path`/`workspace_name`/`mode`(`path.is_some()` → `"local_workspace"` 否则 `"chat_only"`,与 `create_conversation_with_workspace` 一致)/`updated_at`,返回更新后的 Conversation。
- commands 新增并在 main.rs 注册:
  ```
  set_conversation_workspace(state, conversation_id: String, path: Option<String>) -> Result<Conversation, String>
  ```
  - `path` 为 Some 且非空:校验是已存在目录(参照 `set_workspace_path` 的 `is_dir` 校验),name 取 basename;为 None/空:解绑为纯聊天(path/name=None)。
  - 调 `update_conversation_workspace` 写库。
  - **失效 repo_map 缓存**:`state.repo_maps.lock().remove(&conversation_id)`(换了工作区,下轮重新生成结构摘要)。
  - 返回更新后的 Conversation。
- name 的 basename 取法:复用现有逻辑或简单取路径最后一段(与前端 `basenameFromPath` 一致即可)。

### 前端对接(B)
- 新命令:`invoke("set_conversation_workspace", { conversationId, path })`(path: string | null)→ 返回更新后的 Conversation,用它刷新 `conversations`(替换该条)。

## 4. 前端实现要点(B)

### B1 composer 统一化
- 把现有 `.composer-controls`(权限 select + 计划 toggle + model-pill,约 1860-1894)与 `.composer`(📎 + textarea + send,约 1925-1953)**合并为一个圆角容器**:
  - 上:textarea(保留现有 placeholder/onKeyDown/onChange/粘贴拖拽 onPaste/onDrop/onDragOver/onDragLeave 全部行为)。
  - 下:底部控制行 `.composer-footer`:左组 [+ 附件][工作区胶囊][权限][模型胶囊];右组 [计划 toggle][发送/停止]。
  - 附件由 📎(`composer__attach`→handleImportFile)改为「+」次级按钮,行为不变。
  - 发送/停止移入 footer 右侧,保留 `handleSend`/`handleStop`、disabled 逻辑、sending 切「停止」。
  - 权限:`<select>` 改成紧凑胶囊样式(可仍是 select,样式贴合),onChange 行为不变。
  - 计划 toggle、模型胶囊(点击 `openSettings('provider')`)行为不变。
  - 待发图缩略图托盘(`image-tray`)位置放在 textarea 与 footer 之间或 textarea 上方,保持可逐个移除。
  - 斜杠菜单 / @ 补全的浮层定位相对该容器,保证仍在输入框上方弹出。
- 视觉:统一 footer 各控件高度/圆角/间距/hover,消除"零件拼接"感;`:active` 反馈沿用 Plan22。

### B2 工作区胶囊
- footer 里新增工作区胶囊:已绑定显示 `📁 {workspaceName}`,未绑定显示 `📁 选择工作区`(纯聊天)。
- 点击弹出小菜单(或直接动作):**「选择/更换工作区…」**(`open({directory:true})` 取目录)与 **「纯聊天（不绑定）」**。
  - 当前**无 activeConvId**(新会话草稿):选目录→`setDraftWorkspace`;纯聊天→`setDraftWorkspace(null)`。沿用现有首发 `new_conversation_with_workspace(draftWorkspace?.path)` 流程。
  - 当前**有 activeConvId**(已存会话):选目录→`invoke("set_conversation_workspace",{conversationId, path})`;纯聊天→ path=null。成功后用返回的 Conversation 刷新 `conversations` 与顶栏显示。
- 移除首屏 `.workspace-picker` 的独立大按钮(hero 仅留提问语 + 可选一句"在下方输入框选择工作区"提示);`draftWorkspace` 的展示改由胶囊承载。

### B3 顶栏瘦身
- 顶栏品牌 `eyebrow + h1`(约 1659-1661)缩小(或只保留侧栏顶部标识,顶栏不再放大标题)。
- 顶栏的工作区 chip(约 1664-1672)移除(已由 composer 胶囊承载,避免重复);上下文% chip 与「变更」按钮保留。

### B4 首屏聚焦
- hero 的 3 张能力卡(`mvp-grid`)弱化为更小的一行提示或直接精简,空态以"提问语 + 输入框"为视觉重心;未配主模型 CTA(`onboarding-cta`)保留。

## 5. 验收
- `cargo build -p mdga-desktop`(0 警告)+ `cargo test --workspace` + `tsc --noEmit` 全绿。
- dev 走查:① 新会话从 composer 胶囊选工作区→首发即绑定;② 已有会话从胶囊改绑工作区/切纯聊天→生效(下轮用新工作区,repo_map 重生);③ composer 为一个整体、附件「+」、发送内嵌、不再突兀;④ 顶栏更清爽、无重复工作区 chip;⑤ 斜杠/@ 浮层、拖拽粘贴、缩略图、停止/插话等原有行为不回归。
