# Plan22 - 交互细节修复(F1–F5)

项目代号:MDGA
文档定位:修掉"按钮点击无效化/反馈缺失"类交互细节,根治"密钥要存两次"。版本号主创提交时定。

> F1 后端(主控手做,小改)+ F2–F5 前端(子 agent)→ 集成验收 → 主创 dev 走查 + 裁定提交。

## 范围(5 项)
| 级别 | 项 | 端 |
|:---:|---|---|
| 🔴F1 | 供应商保存:密钥留空时**保留已存 key**,不再覆盖为空(根治"粘两次") | 后端 |
| 🟠F2 | 保存成功给明确反馈(卡片内"已保存 ✓") | 前端 |
| 🟠F3 | 设置/变更弹窗:点遮罩空白处 + Esc 关闭;审批/提问弹窗:Esc=拒绝/取消(不加遮罩点击) | 前端 |
| 🟡F4 | 改 key/url/模型/格式时清空残留的"测试连接"结果 | 前端 |
| 🟡F5 | 全局按钮 `:active` 按下反馈(轻微缩放,尊重 prefers-reduced-motion) | 前端 |

## F1 实现(后端,主控)
`commands.rs` 的 `save_model_provider`:当 `api_key.trim().is_empty()` 时,读取该 role 既有 provider 的 key 作为 effective_key(无既有则空),用它调 `upsert_model_provider`,实现"留空=保留旧密钥"。

## F2–F5 实现(前端,子 agent)
- **F2**:`ProviderCard.handleSave` 成功后置本地 `saved` 标志,按钮旁显示"已保存 ✓"(数秒后淡出);失败仍走现有 error 行。
- **F3**:
  - 设置(SettingsModal 2482)、变更(ChangesModal 2049)的 `.approval-overlay` 加 `onClick={(e)=>{ if(e.target===e.currentTarget) onClose() }}`(只在点到遮罩本身时关,点卡片内不关)。
  - App 顶层加一个 keydown(Escape)effect,按当前打开的弹窗分派:设置→关设置;变更→关变更;审批→`respondApproval(false)`;提问→`respondAskUser("")`。审批/提问**不加**遮罩点击关闭。
- **F4**:`ProviderCard` 的 apiKey/baseUrl/modelId/preset/apiFormat 各 onChange 里 `setTestResult(null)`。
- **F5**:`styles.css` 加全局/通用按钮 `:active { transform: scale(.98) }`(或轻微变暗),放在 `@media (prefers-reduced-motion)` 之外、reduced-motion 时禁用 transform。

## 验收
- `cargo build`(0 警告)+ `cargo test --workspace` + `tsc --noEmit` 全绿。
- dev 走查:① 配好 key 保存后,**不重输 key 再保存一次,主页仍可用**(密钥未被清空);② 保存有"已保存 ✓";③ 点设置/变更窗外空白或按 Esc 可关;审批/提问按 Esc=拒绝/取消;④ 改字段后旧"连接成功"消失;⑤ 按钮按下有轻微反馈。
