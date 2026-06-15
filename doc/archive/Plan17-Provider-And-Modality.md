# Plan17 - 模型供应商重定义 + 模态扩展（视觉接入第一步）

项目代号：MDGA
文档定位：本文规划「模型供应商（provider）」抽象的引入，与「扩展 agent 模态」的设置入口。目标是把模型从「写死 DeepSeek + 环境变量取 Key」转为**客户端内可配置的多供应商**，并为视觉（识图）能力铺好 provider 槽位与模态门禁。**本阶段不实现主模型↔视觉模型的协作/推理**（留待后续 Plan）。

> ⚠️ **本计划待主创审阅通过后再开始编码。** 下列「设计决策」处标注了我做的默认假设，审阅时可直接改。

---

## 1. 背景与目标

- 长期方向（见 NovaCode README）：成为**所有国产大模型**（DeepSeek / GLM / Kimi / Qwen…）的通用编码客户端，「模型是可替换的引擎」。
- 现状痛点：① API Key 只从 `DEEPSEEK_API_KEY` 环境变量读取，换模型/多供应商不可行；② 模型客户端写死 DeepSeek；③ 无视觉能力。
- 本计划目标：
  1. 引入 **provider（供应商）** 概念，Key 存客户端、设置页配置，取代环境变量。
  2. 模型客户端泛化为 **OpenAI 兼容**（base_url + key + model 可配），国产大模型多数提供该协议。
  3. 设置页新增「**模型供应商**」选项卡，含**主模型**配置 + 「**扩展 agent 的模态**」开关 → 露出**视觉 / 音频**两块；视觉块可填一个独立的视觉供应商。
  4. **模态门禁**：未配视觉 provider → 行为与现在完全一致（纯对话 + 文本文件导入，禁止图片/图像型 PDF）；配了视觉 provider → 解锁图像模态入口。

---

## 2. 范围

**本计划做（In）**
- provider 数据模型 + SQLite 存储（角色化：main / vision，audio 占位）。
- 模型客户端泛化为 OpenAI 兼容（base_url/key/model 注入）。
- 设置页「模型供应商」选项卡 UI：主模型配置 + 模态扩展开关 + 视觉配置块（音频块占位禁用）。
- 后端改为从 DB 读 provider 配置（替代环境变量）；保留 env 作为一次性迁移回退。
- 模态门禁逻辑（视觉 provider 在与否，决定能否提交图像/图像型 PDF）。
- README / 身份串 / 文档关于「Key 不入应用」的措辞更新。

**本计划不做（Out，后续 Plan）**
- 主 agent 模型与视觉模型之间的**通信/协作/编排**（如何把图交给视觉模型、结果如何回灌主模型）。
- 视觉**推理流水线**（图片编码、多模态消息构造、调用视觉模型并解析）。
- **音频/语音**模型的实际接入（本计划仅占位 UI）。
- 供应商「预设模板」市场、模型列表自动拉取（可作后续增强）。

---

## 3. 关键设计决策（审阅时可改）

| # | 决策点 | 默认方案（建议） | 备选 |
|---|---|---|---|
| D1 | provider 抽象 | ✅ **通用 OpenAI 兼容** + 可选「预设」携带官方 base_url：每个 provider = { preset?, label, base_url(可空), api_key, model_id }。 | — |
| D2 | Key 存储 | ✅ **本地 SQLite 明文**（local-first 常规，app-data 目录内）。 | — |
| D3 | 环境变量迁移 | ✅ **不做迁移**。彻底不碰 `DEEPSEEK_API_KEY`，完全以**设置页首次填写 + DB** 为准；首启未配则提示去设置页配置。 | — |
| D4 | main / vision 关系 | ✅ **两个独立 provider 条目**（按 role 标记），同在「模型供应商」选项卡分区展示。 | — |
| D5 | 视觉门禁粒度 | ✅ 仅「**有无视觉 provider**」二态门禁。 | — |

> **D3 落实**：`get_deepseek_api_key_status`（现读 env）整体改为「读 DB 主 provider 是否已配置」；env 路径删除。首启无主 provider → 顶栏/首屏提示「请在 设置 → 模型供应商 配置主模型」，配好前不可对话。

---

## 4. 数据模型（SQLite）

新增表 `model_providers`（或复用 settings 表，倾向独立表）：

```
model_providers(
  id          TEXT PRIMARY KEY,
  role        TEXT NOT NULL,     -- 'main' | 'vision' | 'audio'(预留)
  preset      TEXT,              -- 'deepseek'|'zhipu'|'moonshot'|'qwen'|'custom'（携带官方 base_url）
  label       TEXT,              -- 展示名，如 "DeepSeek"、"GLM-4V"
  base_url    TEXT,              -- 可空：空=用 preset 的官方端点；填了=自定义覆盖（见 base_url 折叠设计）
  api_key     TEXT NOT NULL,     -- 明文（见 D2）
  model_id    TEXT NOT NULL,     -- 如 deepseek-v4-pro、glm-4v
  enabled     INTEGER NOT NULL DEFAULT 1,
  updated_at  INTEGER
)
```

- **base_url 可空**：空 → 取 `preset` 对应的内置**官方端点**；非空 → 用户自定义覆盖（自托管/代理）。`custom` 预设则 base_url 必填。
- 内置预设官方端点（代码内常量表，OpenAI 兼容 `/chat/completions`）：deepseek→`https://api.deepseek.com`、zhipu(GLM)→`https://open.bigmodel.cn/api/paas/v4`、moonshot(Kimi)→`https://api.moonshot.cn/v1`、qwen(通义)→`https://dashscope.aliyuncs.com/compatible-mode/v1`…（清单可增补）。
- 主模型：role='main'（原 flash/pro 选择迁移为「主 provider 的 model_id」下拉/输入）。
- 视觉模型：role='vision'，仅当用户在模态扩展里配置后存在。
- 「模态扩展开关」状态存 settings（bool `modality_extended`）。

---

## 5. 模型客户端泛化

- `crates/deepseek-client` 现写死 DeepSeek base_url/模型。改造为接收 `{ base_url, api_key, model_id }`（OpenAI 兼容 `/chat/completions`，流式 SSE）。命名可保留 crate 名（避免大改），内部泛化；或新增轻量 `provider` 封装。
- 现有调用点（`chat.rs` 的 `chat_completion_with_retry` / `stream_round_with_retry`、`agent_loop.rs`）改为从「当前 main provider」取 base_url/key/model，而非 env + 写死 host。
- fallback 模型（flash↔pro）逻辑：泛化为「同 provider 下的备用 model_id」或暂时保留 DeepSeek 专属、其他 provider 不 fallback。**审阅决定**。
- 账户余额查询（DeepSeek 专属接口）：非 DeepSeek provider 时隐藏该卡片。

---

## 6. 设置页改版（拆分 + 模型供应商视觉设计）

### 6.1 设置页分类拆分（权限归权限、模型归模型）
现状「**模型与权限**」一个分类里混了：默认模型 / 默认权限模式 / 命令沙箱 / 单任务 token 预算。引入 provider 后**拆成两个独立分类**：

- **模型供应商**（新）：主模型 provider 配置 +「扩展 agent 的模态」开关 + 视觉/音频块。原「默认模型」下拉并入此处（变成主 provider 的 model_id）。
- **权限**（原「模型与权限」去掉模型后剩下的）：默认权限模式 / 命令沙箱 / 单任务 token 预算。

左栏分类顺序建议：账户 → **模型供应商** → **权限** → 权限规则 → MCP 服务器 → 数据 → 关于。

### 6.2 「模型供应商」视觉设计（含 base_url 折叠交互）

沿用现有设计系统（DeepSeek 蓝、卡片式、左分类右内容双栏）。每个 provider 是一张**卡片**，关键是 **base_url 收进「高级」可展开行**，默认折叠、只露最常用的三项（预设、Key、模型）：

```
模型供应商
─────────────────────────────────────────────
 主模型                                    ● 已连接 / ○ 未配置
 ┌───────────────────────────────────────────┐
 │ 供应商   [ DeepSeek            ▾ ]           │  ← 预设下拉(DeepSeek/智谱GLM/月之暗面Kimi/通义/自定义)
 │ API Key  [ •••••••••••••• ]        👁        │  ← 密码框 + 显示切换
 │ 模型     [ deepseek-v4-pro      ▾ ]          │
 │                                             │
 │ ▸ 高级设置（自定义 Base URL）                │  ← 默认折叠；灰字小号；点击展开
 │ ┄┄┄ 展开后 ┄┄┄                              │
 │ ▾ 高级设置                                   │
 │   Base URL [ 留空＝官方默认端点 ]            │  ← placeholder 显示该预设官方地址(灰)
 │   留空即使用 DeepSeek 官方端点；自托管/代理可填 │
 │                                             │
 │            [ 测试连接 ]   [ 保存 ]            │
 └───────────────────────────────────────────┘

 ☐ 扩展 agent 的模态                            ← 一个开关；说明：开启后可接入视觉/音频模型扩展能力
 ┄┄┄ 勾选后淡入展开 ┄┄┄
 ┌─ 视觉（识图） ──────────────────────────────┐
 │ 供应商   [ 智谱 GLM-4V          ▾ ]           │
 │ API Key  [ •••••••••••• ]          👁        │
 │ 模型     [ glm-4v               ▾ ]          │
 │ ▸ 高级设置（自定义 Base URL）                │  ← 同款折叠
 │            [ 测试连接 ]   [ 保存 ]            │
 └───────────────────────────────────────────┘
 ┌─ 音频（语音）  🔒 敬请期待 ──────────────────┐
 │ 后续接入语音模型，实现语音对话交流（占位禁用）│  ← 整块置灰、不可编辑
 └───────────────────────────────────────────┘
```

**布局：两栏（主创定稿）**：provider 卡片内的表单字段用**两栏网格**排布，避免单栏堆叠的空旷感。建议：`供应商 | 模型` 同一行两列；`API Key` 占满整行（key 较长）；`▸ 高级设置（Base URL）` 折叠行占满整行，展开后的 Base URL 输入框也占满整行。卡片底部 `测试连接 / 保存` 按钮右对齐同一行。视觉风格（配色/圆角/徽标/折叠交互）即上方已认可的设计稿，不再改动，仅把单栏改双栏。

**交互/视觉要点（新增设计，本计划一并实现）**：
1. **base_url 折叠行**：默认收起为一行灰色小字 `▸ 高级设置（自定义 Base URL）`；点击展开为输入框，placeholder 显示当前预设的官方地址（灰字示意「留空即用它」）。展开/收起带轻微高度过渡动画，符合现有「柔和」观感。
2. **预设下拉**：选 DeepSeek/GLM/Kimi/通义 时，base_url 自动以官方为默认（折叠态，无需填）；选「自定义」时高级行**自动展开**且 base_url 变必填。
3. **API Key**：密码框 + 👁 显示切换；保存后只回显「已配置 ••••」，不回显明文。
4. **连接状态徽标**：卡片右上角 `● 已连接 / ○ 未配置 / ✗ 连接失败`，由「测试连接」或保存后探测驱动（复用现有 mcp-status 式徽标风格）。
5. **扩展模态开关**：一个 toggle；关 → 视觉/音频块隐藏（行为=现状）；开 → 两块淡入。视觉块可配可测；音频块整块置灰 + 🔒「敬请期待」。
6. 整体配色/圆角/间距复用 Plan14 设计系统，不引入新风格。

---

## 7. 模态门禁（充分不必要）

- 现有 `import_file_text`（📎 导入）当前支持 TXT/MD/CSV/JSON/PDF(文本)/DOCX。
- 门禁规则：
  - **无 vision provider**（或未勾模态扩展）：维持现状——文本类可导入；图片 / 图像型 PDF（扫描件）**拒绝并提示**「需在设置→模型供应商→扩展模态里配置视觉模型」。
  - **有 vision provider**：放开图像 / 图像型 PDF 的提交入口（**本阶段仅放开入口与暂存，不做实际识图推理**——推理是后续 Plan）。
- 即：本计划交付「能不能选图」的门禁 + provider 槽位，**不交付「选了图之后真的识图」**。

---

## 8. 迁移、品牌与文档

- **品牌原则变更**：README 现写「API Key 只从系统环境变量读取，不在应用内保存」。本计划反转该条 → 更新为「API Key 由用户在设置中配置、加密/明文存于本地，不上传云端」。「本地优先 / 无云账号 / 数据不外传」的底色不变。
- main.rs / agent_loop 身份串里若提及 DeepSeek 专属约定，按「供应商可插拔」措辞微调。
- **无环境变量迁移（D3）**：彻底移除 `DEEPSEEK_API_KEY` 读取；首启未配主 provider 即引导去设置页填写，DB 为唯一来源。
- NovaCode 同步：本特性在 MDGA 验证后按单向复刻流程过去；NovaCode 的 README/身份串单独维护。

---

## 9. 安全说明

- Key 入 DB（D2 明文）：local-first 单机常规，存于受 OS 用户隔离的 app-data 目录。**风险**：磁盘明文。**缓解/后续**：可选 OS 凭据库或 at-rest 加密（标注为后续增强，不阻塞本计划）。
- 命令沙箱现擦除 `DEEPSEEK_API_KEY` 等 env：Key 改入 DB 后**不再进子进程环境**，该向量反而更干净；保留对 env 中残留密钥的擦除。

---

## 10. 里程碑拆分（建议）

- **M17.1 provider 底座 + 设置页拆分**：`model_providers` 表 + 存取命令 + 模型客户端泛化（OpenAI 兼容，预设官方端点表）+ 主模型改读 DB（**彻底去 env**）。设置页拆出「模型供应商」「权限」两分类；实现主模型 provider 卡片（预设/Key/模型 + **base_url 折叠高级行** + 测试连接 + 状态徽标）。验证：仅靠设置页配 DeepSeek 即走通现有对话/工具全链路。
- **M17.2 模态扩展开关 + 视觉 provider 槽 + 门禁**：「扩展 agent 的模态」开关 + 视觉块（同款卡片，含 base_url 折叠）+ 音频占位禁用 + 视觉 provider 存取 + 模态门禁（无视觉 provider 拒绝图像/图像型 PDF 并引导；有则放开入口，**不含识图推理**）。
- **（后续 Plan，不在本计划）M18 视觉推理**：图像消息构造、调用视觉模型、主↔视觉协作编排；之后音频。

---

## 11. 验收（本计划范围）

- 设置页可定义主模型 provider（base_url/key/model），不依赖环境变量即可对话与跑工具。
- 「扩展 agent 的模态」开关可展开/收起视觉 + 音频块；视觉块可配置并保存一个视觉 provider；音频块为占位禁用。
- 未配视觉 → 图像/图像型 PDF 提交被拒并给出引导；已配视觉 → 图像入口放开（推理留待后续）。
- DeepSeek 作为一个 provider 配好后，现有功能（对话/工具/diff/checkpoint/MCP/子代理/账本）全部照常。
- `cargo test --workspace` + `tsc` 全绿；主创 dev 真机走查。
