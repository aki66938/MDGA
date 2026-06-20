//! Tauri 命令层：除 send_message（Agent 主流程）外的全部 `#[tauri::command]`，
//! 涵盖会话 CRUD、工作区管理、设置、审批/插话响应、checkpoint 查询、导出/治理、
//! MCP 管理、文档导入与自动更新，以及仅命令用到的小工具。
//!
//! 从 main.rs 抽出（Plan16）：纯代码搬移与可见性调整，无行为变更。

use crate::chat::chat_completion_with_retry;
use crate::checkpoint::apply_checkpoint_revert;
use crate::mcp::spawn_mcp_connect;
use crate::state::AppState;
use mdga_deepseek_client::{
    detect_api_key_status, fetch_models, get_user_balance, probe_tool_call, resolve_base_url,
    test_connection as test_connection_impl, UserBalance,
};
use mdga_shared::{ApiKeyStatus, PermissionMode};
use mdga_storage::{
    add_mcp_server, add_permission_rule, clear_active_workspace, create_conversation,
    create_conversation_with_workspace, delete_conversation, delete_messages, get_active_workspace,
    get_activity_events, get_conversation, get_messages,
    get_token_ledger_entries, list_conversations,
    list_file_checkpoints,
    list_mcp_servers, list_permission_rules, mark_checkpoint_reverted, remove_mcp_server,
    remove_permission_rule, save_active_workspace, save_message, set_conversation_archived,
    set_conversation_pinned, set_mcp_server_enabled, update_conversation_workspace, update_title,
    ActivityEventRecord, Conversation, FileCheckpoint, StoredMessage, Workspace,
};
use mdga_storage::{
    current_ym, delete_role_model, get_connection, get_lsp_server_config_json, get_model,
    get_monthly_usage, get_role_model, get_setting,
    list_models_for_connection as storage_list_models_for_connection, resolve_role_provider,
    set_connection_billing as storage_set_connection_billing, set_lsp_server_config_json,
    set_model_pricing as storage_set_model_pricing, set_setting, upsert_connection, upsert_model,
    upsert_role_model, CuratedModel, MonthlyUsage, ProviderConnection, ALL_ROLES, ROLE_MAIN,
};
use mdga_token_accounting::{lookup_preset, ModelPricing};
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_updater::UpdaterExt;

// ── DeepSeek ──────────────────────────────────────────────────────────────

#[tauri::command]
pub(crate) fn get_deepseek_api_key_status(state: State<AppState>) -> ApiKeyStatus {
    // Plan17 D3：纯以 DB 主 provider 是否已配置为准，不再读环境变量。
    // 0.0.59：经 resolve_role_provider 从「连接库 + 角色引用」解析 main（合成出含 api_key 的 provider）。
    let key = state
        .db
        .lock()
        .ok()
        .and_then(|db| resolve_role_provider(&db, ROLE_MAIN).ok().flatten().map(|p| p.api_key));
    detect_api_key_status(key.as_deref())
}

// ── 连接库 + 角色引用（0.0.59）───────────────────────────────────────────────
//
// 把旧的「每角色一份完整 provider（含 key）」拆成两层：
//   · connection = 一份「端点 + 密钥」接入（配一次，可被多个角色引用）；
//   · role_assignment = 一条「角色 → 模型」纯引用（无 key，指向某 connection）。
// 命令层：连接读出一律脱敏 api_key（回 ""）+ 一个 `hasKey: bool`；存时空 key=保留旧 key；
// key 只流向真正的 provider HTTP Bearer，绝不回显、绝不记日志。

/// 连接的前端视图：脱敏（无 api_key 明文），附 `hasKey` 表明是否已配密钥。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectionView {
    pub id: String,
    pub label: Option<String>,
    pub preset: Option<String>,
    /// 自定义端点；None/空＝走 preset 官方端点。
    pub base_url: Option<String>,
    pub api_format: String,
    /// 是否已配置密钥（明文绝不回传，仅以此布尔表明「已配」）。
    pub has_key: bool,
    /// 0.0.72 计费方式：'api' | 'subscription' | 'none'。
    pub billing_mode: String,
    /// 0.0.72 订阅描述（可空，自由 JSON 串）。
    pub subscription_json: Option<String>,
    pub created_at: Option<i64>,
    pub updated_at: Option<i64>,
}

impl From<ProviderConnection> for ConnectionView {
    fn from(c: ProviderConnection) -> Self {
        ConnectionView {
            id: c.id,
            label: c.label,
            preset: c.preset,
            base_url: c.base_url,
            api_format: c.api_format,
            has_key: !c.api_key.trim().is_empty(),
            billing_mode: c.billing_mode,
            subscription_json: c.subscription_json,
            created_at: c.created_at,
            updated_at: c.updated_at,
        }
    }
}

/// 列出全部连接（脱敏，绝不回 api_key 明文）。
#[tauri::command]
pub(crate) fn list_connections(state: State<AppState>) -> Result<Vec<ConnectionView>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let conns = mdga_storage::list_connections(&db).map_err(|e| e.to_string())?;
    Ok(conns.into_iter().map(ConnectionView::from).collect())
}

/// 新建或更新一个连接。
///
/// `id` 空＝创建；非空＝更新该连接。`api_key` 空＝保留该连接已存密钥（更新场景；首配空 key 由前端拦截）。
/// base_url 空串归一为「走 preset 官方端点」。返回脱敏后的连接视图。
/// 若被更新的连接是某些角色（含主链路/embedding）引用的 main 连接，刷新 embedding 快照。
#[allow(non_snake_case)]
#[tauri::command]
pub(crate) fn save_connection(
    state: State<AppState>,
    id: Option<String>,
    label: Option<String>,
    preset: Option<String>,
    baseUrl: Option<String>,
    apiKey: Option<String>,
    apiFormat: Option<String>,
) -> Result<ConnectionView, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let id = id.as_deref().unwrap_or("");
    let api_key = apiKey.as_deref().unwrap_or("");
    let api_format = apiFormat.as_deref().unwrap_or("openai");
    let saved = upsert_connection(
        &db,
        id,
        label.as_deref(),
        preset.as_deref(),
        baseUrl.as_deref(),
        api_key,
        api_format,
    )
    .map_err(|e| e.to_string())?;
    // 连接端点/凭据可能改变了 main（embedding 复用 main provider）；保守刷新一次快照（无副作用）。
    crate::embedding::refresh_embedding_config(&db);
    Ok(ConnectionView::from(saved))
}

/// 删除一个连接（0.0.62 支持 `force` 级联）。
///
/// - `force == false`：保持旧的**拒绝式**语义——若该连接旗下任一模型仍被某角色引用，返回
///   「该连接下的模型仍被某些角色引用：…」错误（前端据此弹确认框）；未被引用则直接删除，返回 `Ok([])`。
/// - `force == true`：**级联删除**——连同被波及的角色分配（含 `main`）一并清掉，返回被解除分配的
///   角色名列表（去重排序）。清掉 main 后 main 变未配置，交 app 既有「请先配置主模型」处理。
///
/// 两条路径都不触碰任何 api_key（既不读也不回显）。删除后刷新 embedding 快照（embed/main 解析可能变）。
#[tauri::command]
pub(crate) fn delete_connection(
    state: State<AppState>,
    id: String,
    force: bool,
) -> Result<Vec<String>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let affected = if force {
        mdga_storage::delete_connection_cascade(&db, &id).map_err(|e| e.to_string())?
    } else {
        mdga_storage::delete_connection(&db, &id).map_err(|e| e.to_string())?;
        Vec::new()
    };
    crate::embedding::refresh_embedding_config(&db);
    Ok(affected)
}

/// 对某连接做一次「测试连接」：复用既有 test_connection 逻辑，针对**已存连接**与一个待测模型。
///
/// 入参 connectionId 指向某连接（base_url/api_key/api_format 取该连接），model 为待测模型 ID
/// （连接本身不含模型；测试需指定一个）。成功返回「连接成功」，失败返回人话化错误。
#[allow(non_snake_case)]
#[tauri::command]
pub(crate) async fn test_connection(
    state: State<'_, AppState>,
    connectionId: String,
    model: String,
) -> Result<String, String> {
    let conn = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        get_connection(&db, &connectionId)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "连接不存在".to_string())?
    };
    let resolved_base = resolve_base_url(conn.base_url.as_deref(), conn.preset.as_deref())
        .ok_or_else(|| "无法解析端点：请填写 Base URL 或选择内置预设".to_string())?;
    // 测试用模型：入参优先；为空则报错（连接不含模型，无法兜底）。
    let model = model.trim();
    if model.is_empty() {
        return Err("请提供一个待测模型 ID".to_string());
    }
    test_connection_impl(&resolved_base, &conn.api_key, model, &conn.api_format)
        .await
        .map(|_| "连接成功".to_string())
        .map_err(|e| e.to_string())
}

// ── 模型层（curated models）命令（0.0.60）─────────────────────────────────────
//
// 0.0.60 在「连接」与「角色」之间插入用户自建的「模型」层：一个连接（端点 + 密钥）下可登记多个
// 模型（同一把 key 同时跑 pro 与 flash）。这些命令均不回显 api_key；fetch_available_models 仅把 key
// 作为 Bearer 头打一次 GET /models，绝不回传也不记录。

/// 一个 curated model 的前端视图：附其所属连接的展示名，便于 UI 直接渲染。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CuratedModelView {
    pub id: String,
    pub connection_id: String,
    /// 所属连接的展示名（label 优先，否则 preset，再否则连接 id）。连接已删时为 None。
    pub connection_label: Option<String>,
    pub model_id: String,
    pub label: Option<String>,
    pub context_window: Option<i64>,
    /// 0.0.72 单价快照（可空，原始 JSON 串：价格字段 + `_` 前缀元数据，前端解析渲染）。
    pub pricing_json: Option<String>,
}

/// 把一个 [`CuratedModel`] 包成前端视图，连接展示名按 `db` 现状解析。
fn model_to_view(db: &rusqlite::Connection, m: CuratedModel) -> CuratedModelView {
    let connection_label = get_connection(db, &m.connection_id)
        .ok()
        .flatten()
        .map(|c| c.label.or(c.preset).unwrap_or_else(|| c.id));
    CuratedModelView {
        id: m.id,
        connection_id: m.connection_id,
        connection_label,
        model_id: m.model_id,
        label: m.label,
        context_window: m.context_window,
        pricing_json: m.pricing_json,
    }
}

/// 列出全部已登记模型（跨所有连接），每条附其连接展示名。
#[tauri::command]
pub(crate) fn list_models(state: State<AppState>) -> Result<Vec<CuratedModelView>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let models = mdga_storage::list_models(&db).map_err(|e| e.to_string())?;
    Ok(models.into_iter().map(|m| model_to_view(&db, m)).collect())
}

/// 列出某连接下用户登记的全部模型（0.0.60：curated 列表，**非**硬编码预设清单）。
#[allow(non_snake_case)]
#[tauri::command]
pub(crate) fn list_models_for_connection(
    state: State<AppState>,
    connectionId: String,
) -> Result<Vec<CuratedModelView>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let models =
        storage_list_models_for_connection(&db, &connectionId).map_err(|e| e.to_string())?;
    Ok(models.into_iter().map(|m| model_to_view(&db, m)).collect())
}

/// 在某连接下登记一个模型（modelId 为实际 API 模型串）。
///
/// 同 (connectionId, modelId) 已存在则更新其 label/contextWindow（dedup，不插重复）。
/// connectionId 必须指向一个真实连接。返回写入后的模型视图。
#[allow(non_snake_case)]
#[tauri::command]
pub(crate) fn add_model(
    state: State<AppState>,
    connectionId: String,
    modelId: String,
    label: Option<String>,
    contextWindow: Option<i64>,
) -> Result<CuratedModelView, String> {
    let model_id = modelId.trim();
    if model_id.is_empty() {
        return Err("请填写模型 ID".to_string());
    }
    let db = state.db.lock().map_err(|e| e.to_string())?;
    if get_connection(&db, &connectionId)
        .map_err(|e| e.to_string())?
        .is_none()
    {
        return Err("所选连接不存在，请先创建连接".to_string());
    }
    let saved = upsert_model(
        &db,
        "",
        &connectionId,
        model_id,
        label.as_deref(),
        contextWindow.filter(|&cw| cw > 0),
    )
    .map_err(|e| e.to_string())?;
    Ok(model_to_view(&db, saved))
}

/// 更新一个已登记模型的 label / contextWindow（id 必须指向已存在的模型）。
#[allow(non_snake_case)]
#[tauri::command]
pub(crate) fn update_model(
    state: State<AppState>,
    id: String,
    label: Option<String>,
    contextWindow: Option<i64>,
) -> Result<CuratedModelView, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    // 读出现有模型以保持 connection_id / model_id 不变（本命令只改 label/contextWindow）。
    let existing = get_model(&db, &id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "模型不存在".to_string())?;
    // label 缺省（None）= 保留已有别名，不冲成 NULL——防止「只改 context 的调用方漏带 label」时丢别名
    //（前端 saveCtx 已回传 label，这里再兜一层；改别名走 add_model 同 modelId 覆写路径）。
    let label = label.as_deref().or(existing.label.as_deref());
    let saved = upsert_model(
        &db,
        &id,
        &existing.connection_id,
        &existing.model_id,
        label,
        contextWindow.filter(|&cw| cw > 0),
    )
    .map_err(|e| e.to_string())?;
    // 改 main/embed 角色所用模型的 context_window 可能影响压缩软上限；保守刷新 embedding 快照。
    crate::embedding::refresh_embedding_config(&db);
    Ok(model_to_view(&db, saved))
}

/// 删除一个模型（0.0.62 支持 `force` 级联）。
///
/// - `force == false`：保持旧的**拒绝式**语义——若该模型仍被任意角色（role_models）引用，返回
///   「该模型仍被某些角色引用：…」错误（前端据此弹确认框）；未被引用则直接删除，返回 `Ok([])`。
/// - `force == true`：**级联删除**——连同指向它的角色分配（含 `main`）一并清掉，返回被解除分配的
///   角色名列表（去重排序）。清掉 main 后 main 变未配置，交 app 既有「请先配置主模型」处理。
///
/// 不触碰任何 api_key。删除后刷新 embedding 快照（embed/main 解析可能变，镜像 update_model 的做法）。
#[tauri::command]
pub(crate) fn delete_model(
    state: State<AppState>,
    id: String,
    force: bool,
) -> Result<Vec<String>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let affected = if force {
        mdga_storage::delete_model_cascade(&db, &id).map_err(|e| e.to_string())?
    } else {
        mdga_storage::delete_model(&db, &id).map_err(|e| e.to_string())?;
        Vec::new()
    };
    crate::embedding::refresh_embedding_config(&db);
    Ok(affected)
}

// ── 计价（0.0.72）─────────────────────────────────────────────────────────────
//
// 三条命令给前端「计价」UI 用：lookup_model_preset（添加模型/拉取/恢复预设时自动填单价）、
// set_model_pricing（保存/清空模型单价）、set_connection_billing（保存连接计费方式）。
// 这些命令都不触碰 api_key。pricing_json 前端存「价格字段 + `_` 前缀元数据」的 JSON 串，
// 后端原样存取，不解析校验；仅结算时（agent_loop）以 serde 解析价格字段。

/// 一条预设单价的前端友好视图（0.0.72）：pricing 为结构化单价，余为展示/核对元数据。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PresetView {
    pub pricing: ModelPricing,
    pub display_name: String,
    /// 置信度："high" | "medium" | "low"。
    pub confidence: String,
    /// true → 前端显「待官网核对」黄标。
    pub needs_verify: bool,
    pub source_url: String,
}

/// 在内置预设库中查一条模型单价（0.0.72）：供前端「添加模型 / 拉取 / 恢复预设」时自动填单价。
///
/// 匹配规则见内核 `lookup_preset`（preset 小写相等；model_id 规范化后大小写不敏感匹配；
/// currency 大小写不敏感）。未命中返回 None，前端据此回退手动填价。不触碰任何凭据。
#[allow(non_snake_case)]
#[tauri::command]
pub(crate) fn lookup_model_preset(
    connectionPreset: String,
    modelId: String,
    currency: String,
) -> Option<PresetView> {
    lookup_preset(&connectionPreset, &modelId, &currency).map(|e| PresetView {
        pricing: e.pricing.clone(),
        display_name: e.display_name.to_string(),
        confidence: e.confidence.to_string(),
        needs_verify: e.needs_verify,
        source_url: e.source_url.to_string(),
    })
}

/// 保存或清空某模型的单价快照（0.0.72）。
///
/// `modelRef` = models.id；`pricingJson` 为前端构造的 JSON 串（价格字段 + `_` 前缀元数据），
/// 后端**原样存**，不解析校验；传 None 清空（恢复「无单价」）。返回写入后的模型视图。
/// 改 main/embed 角色所用模型的单价不影响窗口，但镜像 update_model 保守刷新 embedding 快照。
#[allow(non_snake_case)]
#[tauri::command]
pub(crate) fn set_model_pricing(
    state: State<AppState>,
    modelRef: String,
    pricingJson: Option<String>,
) -> Result<CuratedModelView, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let saved = storage_set_model_pricing(&db, &modelRef, pricingJson.as_deref())
        .map_err(|e| e.to_string())?;
    crate::embedding::refresh_embedding_config(&db);
    Ok(model_to_view(&db, saved))
}

/// 保存某连接的计费方式（0.0.72）。
///
/// `billingMode` 归一化为 'api' | 'subscription' | 'none'（未知落回 'api'）；`subscriptionJson`
/// 为订阅描述自由串（可空，原样存）。返回脱敏后的连接视图（绝不回传 api_key）。
#[allow(non_snake_case)]
#[tauri::command]
pub(crate) fn set_connection_billing(
    state: State<AppState>,
    connectionId: String,
    billingMode: String,
    subscriptionJson: Option<String>,
) -> Result<ConnectionView, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let saved = storage_set_connection_billing(
        &db,
        &connectionId,
        &billingMode,
        subscriptionJson.as_deref(),
    )
    .map_err(|e| e.to_string())?;
    Ok(ConnectionView::from(saved))
}

/// 读某连接「当前 UTC 月」累计的原始 token 用量（0.0.72 订阅进度条）。
///
/// 月份键取 [`current_ym`]（"YYYY-MM"，UTC）。该计数与计价/成本完全无关，也独立于会话累计；
/// 无记录时返回全 0 的 [`MonthlyUsage`]。
#[allow(non_snake_case)]
#[tauri::command]
pub(crate) fn get_connection_monthly_usage(
    state: State<AppState>,
    connectionId: String,
) -> Result<MonthlyUsage, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    get_monthly_usage(&db, &connectionId, &current_ym()).map_err(|e| e.to_string())
}

/// 拉取某连接端点真实可用的模型 id 列表（0.0.60）：GET {base}/models（连接的 key 作 Bearer）。
///
/// 服务端按 connectionId 取出端点/密钥（前端从不接触明文 key）；OpenAI 兼容解析
/// `{"data":[{"id":...}]}`（也接受裸数组）。10–12s 超时。任何失败（端点无 /models、网络、
/// 鉴权、解析）返回 Err，UI 据此回退到手动输入。**key 绝不回传也不记录**，仅作 Bearer 头。
#[allow(non_snake_case)]
#[tauri::command]
pub(crate) async fn fetch_available_models(
    state: State<'_, AppState>,
    connectionId: String,
) -> Result<Vec<String>, String> {
    let connection = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        get_connection(&db, &connectionId)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "连接不存在".to_string())?
    };
    let base = resolve_base_url(connection.base_url.as_deref(), connection.preset.as_deref())
        .ok_or_else(|| "无法解析端点（自定义 preset 需填 base_url）".to_string())?;
    fetch_models(&base, &connection.api_key)
        .await
        .map_err(|e| e.to_string())
}

/// 对某 role 的供应商做一次「工具调用冒烟探测」（Plan25 C-1，#3）：发一个极小请求并提供一个
/// trivial 函数工具，判断该模型在当前端点能否返回 tool_call（原生 tool_calls 或正文被兜底
/// 恢复出 tool_call 均算成功）。返回 true=支持工具调用，false=不支持。
///
/// 字段回退逻辑：任一入参为空则回退到该 role 经 resolve_role_provider 解析出的已存 provider，
/// base_url 留空走 preset 官方端点；便于「已配状态下不重输 Key 直接测试」。
#[tauri::command]
pub(crate) async fn smoke_test_tool_call(
    state: State<'_, AppState>,
    role: String,
    base_url: String,
    api_key: String,
    model: String,
    api_format: String,
) -> Result<bool, String> {
    // 读已存 provider 作为各字段的回退来源（0.0.59：经连接库+引用解析该角色）。
    let stored = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        resolve_role_provider(&db, &role).ok().flatten()
    };
    // base_url：优先用入参；为空则用已存 base_url/preset 解析官方端点。
    let resolved_base = {
        let explicit = base_url.trim();
        if !explicit.is_empty() {
            explicit.trim_end_matches('/').to_string()
        } else if let Some(p) = &stored {
            resolve_base_url(p.base_url.as_deref(), p.preset.as_deref()).unwrap_or_default()
        } else {
            String::new()
        }
    };
    // api_key / model / api_format：入参为空则回退已存值。
    let key = if api_key.trim().is_empty() {
        stored.as_ref().map(|p| p.api_key.clone()).unwrap_or_default()
    } else {
        api_key
    };
    let model = if model.trim().is_empty() {
        stored.as_ref().map(|p| p.model_id.clone()).unwrap_or_default()
    } else {
        model
    };
    let api_format = if api_format.trim().is_empty() {
        stored
            .as_ref()
            .map(|p| p.api_format.clone())
            .unwrap_or_else(|| "openai".to_string())
    } else {
        api_format
    };

    probe_tool_call(&resolved_base, &key, &model, &api_format)
        .await
        .map_err(|e| e.to_string())
}

/// 0.0.59：对「连接 + 模型」做工具调用冒烟探测（连接库版，供「模型连接」设置页用）。
///
/// 服务端按 connectionId 取出端点/密钥/格式（前端从不接触明文 key），用给定 model 探测该模型在
/// 该连接端点上能否返回 tool_call。这把重构前「测试工具调用」按钮的能力以「不暴露 key」的方式补回。
#[tauri::command]
pub(crate) async fn smoke_test_tool_call_for_connection(
    state: State<'_, AppState>,
    connection_id: String,
    model: String,
) -> Result<bool, String> {
    let connection = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        mdga_storage::get_connection(&db, &connection_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "连接不存在".to_string())?
    };
    let model = model.trim();
    if model.is_empty() {
        return Err("请提供要测试的模型 id".to_string());
    }
    let base = resolve_base_url(connection.base_url.as_deref(), connection.preset.as_deref())
        .ok_or_else(|| "无法解析端点（自定义 preset 需填 base_url）".to_string())?;
    probe_tool_call(&base, &connection.api_key, model, &connection.api_format)
        .await
        .map_err(|e| e.to_string())
}

/// 读取一个应用设置（如 modality_extended 开关）。
#[tauri::command]
pub(crate) fn get_app_setting(
    state: State<AppState>,
    key: String,
) -> Result<Option<String>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    get_setting(&db, &key).map_err(|e| e.to_string())
}

/// 写入一个应用设置。
#[tauri::command]
pub(crate) fn set_app_setting(
    state: State<AppState>,
    key: String,
    value: String,
) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    set_setting(&db, &key, &value).map_err(|e| e.to_string())?;
    // P2 / 0.0.58:embedding 开关或模型名变更时,刷新 code_search 的 embedding 配置快照。
    if key == crate::embedding::EMBEDDING_ENABLED_KEY
        || key == crate::embedding::EMBEDDING_MODEL_KEY
    {
        crate::embedding::refresh_embedding_config(&db);
    }
    Ok(())
}

// ── LSP 服务器注册表设置（R-uicfg / 0.0.57）──────────────────────────────────

/// 列出**全部已知**语言服务器（硬编码精选注册表的只读快照），供设置页渲染开关与路径覆盖框。
///
/// 安全：该列表完全来自 mdga-lsp 的编译期常量；前端只能据此勾选启用/填写路径，无法新增任意命令。
#[tauri::command]
pub(crate) fn get_lsp_known_servers() -> Vec<mdga_lsp::KnownServer> {
    mdga_lsp::known_servers()
}

/// 读取当前 LSP 服务器配置（按 kind 的启用/路径覆盖稀疏映射）。未配置返回空配置（＝全部启用、走 PATH）。
#[tauri::command]
pub(crate) fn get_lsp_server_config(
    state: State<AppState>,
) -> Result<mdga_lsp::LspServerConfig, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let raw = get_lsp_server_config_json(&db).map_err(|e| e.to_string())?;
    match raw {
        Some(json) => serde_json::from_str(&json).map_err(|e| e.to_string()),
        None => Ok(mdga_lsp::LspServerConfig::default()),
    }
}

/// 保存 LSP 服务器配置：校验后持久化，并刷新运行时缓存，使后续 lsp_* 工具立即生效。
///
/// 安全校验（强约束）：
///   1. 配置里的每个键必须是**已知种类**（mdga_lsp::is_known_kind）——拒绝注入未知服务器条目；
///   2. path_override 是人类显式录入的本地路径，这里不强制其立即存在（用户可能先填后装），
///      但在真正用它启动时由 mdga-lsp 校验其为现存文件。命令身份恒为注册表常量，UI 无法改写。
#[tauri::command]
pub(crate) fn save_lsp_server_config(
    state: State<AppState>,
    config: mdga_lsp::LspServerConfig,
) -> Result<(), String> {
    // 拒绝未知种类键：UI 只应回传 get_lsp_known_servers 列出的 kind。
    for kind in config.servers.keys() {
        if !mdga_lsp::is_known_kind(kind) {
            return Err(format!("未知的语言服务器种类: {kind}"));
        }
    }
    let json = serde_json::to_string(&config).map_err(|e| e.to_string())?;
    {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        set_lsp_server_config_json(&db, &json).map_err(|e| e.to_string())?;
    }
    // 刷新进程级运行时缓存，使工具调用立刻按新配置解析（无需重启）。
    crate::tools::set_lsp_server_config(config);
    Ok(())
}

// ── 角色分配（role → curated model）设置（0.0.60）─────────────────────────────
//
// 0.0.60：角色不再直接绑「连接 + 自由输入模型串」，而是引用一个**已登记的模型**（role_models →
// models → connections）。视图同时给出 modelRef（角色自身引用的模型 id）与实际生效（回退 main）。

/// 一个角色当前的分配状态视图（无密钥）：自身引用的模型 + 实际生效（回退 main 后）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RoleAssignmentView {
    /// 角色：main|action|plan|critique|vision|subagent|embed。
    pub role: String,
    /// 该角色**自身**引用的 curated model id（models.id）。无自身分配则 None ⇒ 回退 main。
    pub model_ref: Option<String>,
    /// 自身引用模型的 model_id（实际 API 模型串）。无自身分配则 None。
    pub model_id: Option<String>,
    /// 自身引用模型的展示名（label）。无自身分配则 None。
    pub model_label: Option<String>,
    /// 自身引用模型所属连接的展示名。无自身分配或连接已删则 None。
    pub connection_label: Option<String>,
    /// 自身引用模型的上下文窗口（tokens，可空；0.0.60 起在模型粒度）。
    pub context_window: Option<i64>,
    /// 自身分配是否启用（无自身分配则 false）。
    pub enabled: bool,
    /// 实际生效（经回退）：用于 UI 展示「跟随 main」。
    pub effective: EffectiveRef,
}

/// 实际生效的分配（resolve 后）：模型串 + 连接展示名 + 来源（self=用自身；main=回退主模型）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EffectiveRef {
    /// 实际生效模型 ID（None＝连 main 都没配）。
    pub model_id: Option<String>,
    /// 实际生效连接的展示名（None＝连 main 都没配）。
    pub connection_label: Option<String>,
    /// 来源：'self'＝用了角色自身分配；'main'＝回退到主模型；'none'＝主模型也没配。
    pub source: String,
}

/// 读取全部角色（main|action|plan|critique|vision|subagent|embed）的分配概览。
///
/// 每个角色返回：自身引用（modelRef/modelId/modelLabel/connectionLabel/contextWindow/enabled，
/// None＝跟随 main）+ 实际生效（effective：经 resolve_role_provider 回退后的模型/连接名 + source）。
/// 绝不回显任何 api_key。
#[tauri::command]
pub(crate) fn get_role_assignments(
    state: State<AppState>,
) -> Result<Vec<RoleAssignmentView>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(ALL_ROLES.len());
    for &role in ALL_ROLES {
        // 自身分配（不回退）：role_models → models → connection 展示名。
        let own = get_role_model(&db, role).map_err(|e| e.to_string())?;
        let (own_ref, own_model_id, own_label, own_conn_label, own_ctx, own_enabled) = match &own {
            Some(rm) => {
                let model = get_model(&db, &rm.model_ref).ok().flatten();
                let conn_label = model.as_ref().and_then(|m| {
                    get_connection(&db, &m.connection_id)
                        .ok()
                        .flatten()
                        .and_then(|c| c.label.or(c.preset))
                });
                (
                    Some(rm.model_ref.clone()),
                    model.as_ref().map(|m| m.model_id.clone()),
                    model.as_ref().and_then(|m| m.label.clone()),
                    conn_label,
                    model.as_ref().and_then(|m| m.context_window),
                    rm.enabled,
                )
            }
            None => (None, None, None, None, None, false),
        };
        // 实际生效（回退 main）。resolve 合成出的 ModelProvider 带 preset/label/model_id。
        let effective_provider = resolve_role_provider(&db, role).map_err(|e| e.to_string())?;
        let source = if own_enabled && own.is_some() {
            "self"
        } else if effective_provider.is_some() {
            "main"
        } else {
            "none"
        };
        let effective = EffectiveRef {
            model_id: effective_provider.as_ref().map(|p| p.model_id.clone()),
            connection_label: effective_provider
                .as_ref()
                .and_then(|p| p.label.clone().or_else(|| p.preset.clone())),
            source: source.to_string(),
        };
        out.push(RoleAssignmentView {
            role: role.to_string(),
            model_ref: own_ref,
            model_id: own_model_id,
            model_label: own_label,
            connection_label: own_conn_label,
            context_window: own_ctx,
            enabled: own_enabled,
            effective,
        });
    }
    Ok(out)
}

/// 设置某角色的分配：把 role 指向一个已登记的 curated model（0.0.60，modelRef → models.id）。
///
/// role 必须是允许的角色之一；modelRef 必须指向一个真实存在的已登记模型。enabled 缺省 true。
/// 覆盖该角色已有分配。改 main/embed 可能影响 embedding（复用 main provider）；保守刷新一次快照。
#[allow(non_snake_case)]
#[tauri::command]
pub(crate) fn set_role_assignment(
    state: State<AppState>,
    role: String,
    modelRef: String,
    enabled: Option<bool>,
) -> Result<(), String> {
    if !ALL_ROLES.contains(&role.as_str()) {
        return Err(format!("不支持的角色: {role}"));
    }
    if modelRef.trim().is_empty() {
        return Err("请选择一个模型".to_string());
    }
    let db = state.db.lock().map_err(|e| e.to_string())?;
    // upsert_role_model 内部已校验 model_ref 存在；这里直接调用并把人话化错误透传。
    upsert_role_model(&db, &role, modelRef.trim(), enabled.unwrap_or(true))
        .map_err(|e| e.to_string())?;
    // main 分配变更可能改 embedding 端点/凭据/模型；保守刷新（其它角色无副作用）。
    if role == ROLE_MAIN || role == mdga_storage::ROLE_EMBED {
        crate::embedding::refresh_embedding_config(&db);
    }
    Ok(())
}

/// 清除某角色的分配，使其回退到主模型。拒绝清除 role==main（main 不可清）。
#[tauri::command]
pub(crate) fn clear_role_assignment(state: State<AppState>, role: String) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    delete_role_model(&db, &role).map_err(|e| e.to_string())?;
    if role == mdga_storage::ROLE_EMBED {
        crate::embedding::refresh_embedding_config(&db);
    }
    Ok(())
}

// ── 会话管理 ──────────────────────────────────────────────────────────────

/// 创建新会话，初始标题为"新对话"。
#[tauri::command]
pub(crate) fn new_conversation(state: State<AppState>) -> Result<Conversation, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    create_conversation(&db).map_err(|e| e.to_string())
}

/// 创建新会话，并将会话绑定到创建时选择的工作区快照。
///
/// 输入可选工作区路径；路径存在时校验目录并写入 conversation snapshot，未传路径时创建纯聊天会话。
#[tauri::command]
pub(crate) fn new_conversation_with_workspace(
    state: State<AppState>,
    workspace_path: Option<String>,
) -> Result<Conversation, String> {
    let workspace = match workspace_path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(path) => {
            let path_buf = std::path::PathBuf::from(path);
            if !path_buf.is_dir() {
                return Err("工作区路径不存在或不是目录".to_string());
            }
            let name = workspace_name_from_path(path);
            Some((path.to_string(), name))
        }
        None => None,
    };

    let db = state.db.lock().map_err(|e| e.to_string())?;
    create_conversation_with_workspace(
        &db,
        workspace.as_ref().map(|(path, _)| path.as_str()),
        workspace.as_ref().map(|(_, name)| name.as_str()),
    )
    .map_err(|e| e.to_string())
}

/// 改绑已有会话的工作区（Plan23 A）：选目录则绑定到该工作区，None/空白则解绑为纯聊天。
///
/// 输入会话 ID 与可选路径；path 为 Some 且 trim 非空时校验为已存在目录（与 set_workspace_path
/// 一致），name 取路径 basename；否则 path/name 均传 None。写库后失效该会话的 repo_map 缓存，
/// 使换工作区后下一轮重新生成结构摘要。返回更新后的 Conversation。
#[tauri::command]
pub(crate) fn set_conversation_workspace(
    state: State<AppState>,
    conversation_id: String,
    path: Option<String>,
) -> Result<Conversation, String> {
    let workspace = match path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(path) => {
            let path_buf = std::path::PathBuf::from(path);
            if !path_buf.is_dir() {
                return Err("工作区路径不存在或不是目录".to_string());
            }
            let name = workspace_name_from_path(path);
            Some((path.to_string(), name))
        }
        None => None,
    };

    // 仅在写库时持有 db 锁，避免与 repo_maps 锁同时持有。
    let conversation = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        update_conversation_workspace(
            &db,
            &conversation_id,
            workspace.as_ref().map(|(path, _)| path.as_str()),
            workspace.as_ref().map(|(_, name)| name.as_str()),
        )
        .map_err(|e| e.to_string())?
    };

    // 失效该会话的 repo_map 缓存：换工作区后下一轮重新生成结构摘要。
    if let Ok(mut maps) = state.repo_maps.lock() {
        maps.remove(&conversation_id);
    }

    Ok(conversation)
}

/// 返回所有会话列表，按最近更新时间倒序。
#[tauri::command]
pub(crate) fn get_conversations(state: State<AppState>) -> Result<Vec<Conversation>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    list_conversations(&db).map_err(|e| e.to_string())
}

/// 返回指定会话的所有消息，按时间正序。
#[tauri::command]
pub(crate) fn load_messages(
    state: State<AppState>,
    conversation_id: String,
) -> Result<Vec<StoredMessage>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    get_messages(&db, &conversation_id).map_err(|e| e.to_string())
}

/// 持久化一条消息到数据库。
///
/// 输入会话 ID、角色、内容和可选的 usage JSON 字符串；
/// 同步更新会话的 updated_at 字段，维持列表排序。
#[tauri::command]
pub(crate) fn persist_message(
    state: State<AppState>,
    conversation_id: String,
    role: String,
    content: String,
    usage_json: Option<String>,
    parts_json: Option<String>,
) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    save_message(
        &db,
        &conversation_id,
        &role,
        &content,
        usage_json.as_deref(),
        parts_json.as_deref(),
    )
    .map_err(|e| e.to_string())
}

/// 续接(P2 入口):读回某会话落库的 wire 快照,补孤儿 tool_result 后返回合法 wire JSON;无则 None。
/// 供前端在断额/崩溃后用完整 wire(含 tool 角色)重建上下文续接,而非只回喂纯文本。
#[tauri::command]
pub(crate) fn load_wire(
    state: State<AppState>,
    conversation_id: String,
) -> Result<Option<String>, String> {
    let raw = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        mdga_storage::get_wire_snapshot(&db, &conversation_id).map_err(|e| e.to_string())?
    };
    match raw {
        None => Ok(None),
        Some(json) => {
            let mut wire: Vec<serde_json::Value> =
                serde_json::from_str(&json).map_err(|e| e.to_string())?;
            crate::agent_loop::finalize_wire(&mut wire);
            serde_json::to_string(&wire)
                .map(Some)
                .map_err(|e| e.to_string())
        }
    }
}

/// 更新会话标题。
///
/// 输入会话 ID 和新标题；用于首条消息发送后自动设置有意义的标题。
#[tauri::command]
pub(crate) fn rename_conversation(
    state: State<AppState>,
    conversation_id: String,
    title: String,
) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    update_title(&db, &conversation_id, &title).map_err(|e| e.to_string())
}

/// 删除会话及其全部消息。
#[tauri::command]
pub(crate) fn remove_conversation(
    state: State<AppState>,
    conversation_id: String,
) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    delete_conversation(&db, &conversation_id).map_err(|e| e.to_string())
}

/// 删除会话最后一条助手消息（Plan27 C4 #1b 重新生成的后端）。
///
/// 仅当会话最后一条消息 role='assistant' 时才删，返回是否删除。前端「重新生成」先调本命令删旧回复，
/// 再用截至上一条 user 的历史重跑 send_message（不新增 user 消息）。
///
/// 0.0.69 真续接:本命令**同时清 wire 快照**——重新生成是「重跑上一轮」,若留着旧快照,续接读回会把
/// 已删的旧助手回复(及其 tool 历史)当权威重放、还会重复追加该 user。清掉后 send_message 会回退到
/// 按前端截断后的历史重建,得到正确的「redo」语义(与 rewind/compact 清快照同理)。
#[tauri::command]
pub(crate) fn delete_last_assistant_message(
    state: State<AppState>,
    conversation_id: String,
) -> Result<bool, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let deleted = mdga_storage::delete_last_assistant_message(&db, &conversation_id)
        .map_err(|e| e.to_string())?;
    let _ = mdga_storage::delete_wire_snapshot(&db, &conversation_id);
    Ok(deleted)
}

/// 编辑已发消息 = 回退到此处（0.0.49，CC「rewind in here」+ 连文件回退）：删该会话「末尾 n 条」
/// 消息；并把这些被删轮次期间产生的文件变更一并回退（按 cut_ts 时戳关联 file_checkpoints、seq 倒序
/// 撤销）；最后清 wire 快照，防断额/崩溃续接重放被删历史。返回 { deleted, filesReverted }。
#[tauri::command]
pub(crate) fn rewind_to_message(
    state: State<AppState>,
    conversation_id: String,
    n: usize,
) -> Result<serde_json::Value, String> {
    // 1) 截断锚点 cut_ts：末尾 n 条里最早一条的 created_at。早于它的文件变更属保留轮次，不回退。
    let cut_ts = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        mdga_storage::cut_timestamp_for_last_n(&db, &conversation_id, n).map_err(|e| e.to_string())?
    };
    let Some(cut_ts) = cut_ts else {
        return Ok(serde_json::json!({ "deleted": 0, "filesReverted": 0 }));
    };

    // 2) 文件回退：created_at >= cut_ts 且未回退、可回退的检查点，按 seq 倒序撤销（后发生的先回退）。
    //    无绑定工作区则跳过文件回退、只截断对话。
    let (workspace, targets) = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let workspace = get_conversation(&db, &conversation_id)
            .map_err(|e| e.to_string())?
            .and_then(|c| c.workspace_path);
        let mut targets: Vec<FileCheckpoint> = if workspace.is_some() {
            list_file_checkpoints(&db, &conversation_id)
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|c| c.created_at >= cut_ts && !c.reverted && c.revertible)
                .collect()
        } else {
            Vec::new()
        };
        targets.sort_by(|a, b| b.seq.cmp(&a.seq));
        (workspace, targets)
    };
    let mut files_reverted = 0usize;
    if let Some(workspace) = workspace {
        for checkpoint in &targets {
            if apply_checkpoint_revert(&workspace, checkpoint).is_ok() {
                let db = state.db.lock().map_err(|e| e.to_string())?;
                let _ = mark_checkpoint_reverted(&db, &checkpoint.id);
                files_reverted += 1;
            }
        }
    }

    // 3) 删末尾 n 条消息 + 清 wire 快照。
    let deleted = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let deleted = mdga_storage::delete_last_n_messages(&db, &conversation_id, n)
            .map_err(|e| e.to_string())?;
        mdga_storage::delete_wire_snapshot(&db, &conversation_id).map_err(|e| e.to_string())?;
        deleted
    };

    Ok(serde_json::json!({ "deleted": deleted, "filesReverted": files_reverted }))
}

/// 按关键词搜索会话（Plan27 C5 #6 正文搜索）：标题或消息正文 LIKE 命中，按 updated_at 倒序。
///
/// query 经 trim 后为空则直接返回空列表（前端空查询应回退本地列表，不应走此命令）。
#[tauri::command]
pub(crate) fn search_conversations(
    state: State<AppState>,
    query: String,
) -> Result<Vec<Conversation>, String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let db = state.db.lock().map_err(|e| e.to_string())?;
    mdga_storage::search_conversations(&db, trimmed).map_err(|e| e.to_string())
}

/// 设置会话置顶状态；置顶会话在列表中排在最前。
#[tauri::command]
pub(crate) fn pin_conversation(
    state: State<AppState>,
    conversation_id: String,
    pinned: bool,
) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    set_conversation_pinned(&db, &conversation_id, pinned).map_err(|e| e.to_string())
}

/// 查询 DeepSeek 账户余额，供设置页展示。Plan17 D3：从 DB 主 provider 取 Key（仅 deepseek 预设有意义），不再读环境变量。
#[tauri::command]
pub(crate) async fn get_account_balance(state: State<'_, AppState>) -> Result<UserBalance, String> {
    let api_key = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let provider = resolve_role_provider(&db, ROLE_MAIN)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "未配置主模型：请在 设置 → 模型供应商 配置".to_string())?;
        // Plan21 #5：余额查询仅 DeepSeek 端点支持。非 deepseek 主供应商直接门禁返回，
        // 不去打 DeepSeek 端点（用别家 Key 查只会拿到误导/失败结果）。
        if provider.preset.as_deref() != Some("deepseek") {
            return Err("当前主供应商不提供余额查询（仅 DeepSeek 支持）".to_string());
        }
        if provider.api_key.trim().is_empty() {
            return Err("未配置主模型：请在 设置 → 模型供应商 配置".to_string());
        }
        provider.api_key
    };
    get_user_balance(&api_key).await.map_err(|e| e.to_string())
}

/// 返回应用信息（版本号、数据目录路径），供设置页展示。
#[tauri::command]
pub(crate) fn get_app_info(app: AppHandle) -> Result<serde_json::Value, String> {
    let version = app.package_info().version.to_string();
    let data_dir = app
        .path()
        .app_data_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    Ok(serde_json::json!({ "version": version, "dataDir": data_dir }))
}

/// 设置会话归档状态；归档会话移入侧边栏「已归档」区，数据不删除。
#[tauri::command]
pub(crate) fn archive_conversation(
    state: State<AppState>,
    conversation_id: String,
    archived: bool,
) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    set_conversation_archived(&db, &conversation_id, archived).map_err(|e| e.to_string())
}

/// 返回指定会话的全部文件变更检查点（含已回退的），供「变更记录」面板展示。
#[tauri::command]
pub(crate) fn get_checkpoints(
    state: State<AppState>,
    conversation_id: String,
) -> Result<Vec<FileCheckpoint>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    list_file_checkpoints(&db, &conversation_id).map_err(|e| e.to_string())
}

/// 回退到指定检查点之前：把该检查点及其后的所有可回退变更按倒序撤销（CC 的 rewind）。
/// 返回成功回退的条数；不可回退的变更跳过。
#[tauri::command]
pub(crate) fn revert_to_checkpoint(
    state: State<AppState>,
    conversation_id: String,
    checkpoint_id: String,
) -> Result<usize, String> {
    let (workspace, targets) = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let conversation = get_conversation(&db, &conversation_id)
            .map_err(|e| e.to_string())?
            .ok_or("会话不存在")?;
        let workspace = conversation
            .workspace_path
            .ok_or("该会话未绑定工作区")?;
        let all = list_file_checkpoints(&db, &conversation_id).map_err(|e| e.to_string())?;
        let target_seq = all
            .iter()
            .find(|c| c.id == checkpoint_id)
            .map(|c| c.seq)
            .ok_or("检查点不存在")?;
        let mut targets: Vec<FileCheckpoint> = all
            .into_iter()
            .filter(|c| c.seq >= target_seq && !c.reverted && c.revertible)
            .collect();
        // 按序号倒序撤销，后发生的变更先回退。
        targets.sort_by(|a, b| b.seq.cmp(&a.seq));
        (workspace, targets)
    };

    let mut reverted = 0;
    for checkpoint in &targets {
        if apply_checkpoint_revert(&workspace, checkpoint).is_ok() {
            let db = state.db.lock().map_err(|e| e.to_string())?;
            let _ = mark_checkpoint_reverted(&db, &checkpoint.id);
            reverted += 1;
        }
    }
    Ok(reverted)
}

/// 手动压缩会话历史（/compact）：把全部消息摘要成一条任务备忘录替换原文。
#[tauri::command]
pub(crate) async fn compact_history(
    app: AppHandle,
    state: State<'_, AppState>,
    conversation_id: String,
    model: String,
) -> Result<(), String> {
    let (base_url, api_key, messages) = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        // 主模型 provider（Plan17 D3）：一律从 DB 取，无主 provider 即报错引导去设置页。
        // 0.0.59：经 resolve_role_provider 从「连接库 + 角色引用」解析 main。
        let (base_url, api_key) = match resolve_role_provider(&db, ROLE_MAIN) {
            Ok(Some(p)) => {
                let bu = resolve_base_url(p.base_url.as_deref(), p.preset.as_deref())
                    .ok_or_else(|| "未配置主模型：请在 设置 → 模型供应商 配置".to_string())?;
                (bu, p.api_key)
            }
            _ => return Err("未配置主模型：请在 设置 → 模型供应商 配置".to_string()),
        };
        let messages = get_messages(&db, &conversation_id).map_err(|e| e.to_string())?;
        (base_url, api_key, messages)
    };
    if messages.len() < 4 {
        return Err("当前会话消息较少，无需压缩".to_string());
    }

    let transcript: String = messages
        .iter()
        .map(|m| {
            let body: String = m.content.chars().take(800).collect();
            format!("[{}] {}", m.role, body)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = format!(
        "你是对话压缩器。请把下面这段用户与 AI 助手的完整对话压缩成简明的中文备忘录，\
保留：1）用户的总体目标与关键需求；2）已完成的事项与结论；3）重要决策与原因；\
4）涉及的文件清单；5）当前进度与待办。只输出备忘录本身。\n\n{transcript}"
    );
    let result = chat_completion_with_retry(
        &base_url,
        &api_key,
        vec![serde_json::json!({ "role": "user", "content": prompt })],
        &model,
        None,
        &app,
    )
    .await?;
    let summary = result.content.unwrap_or_default();
    if summary.trim().is_empty() {
        return Err("压缩失败：模型未返回摘要".to_string());
    }

    let db = state.db.lock().map_err(|e| e.to_string())?;
    delete_messages(&db, &conversation_id).map_err(|e| e.to_string())?;
    // 0.0.49：压缩同样截断了历史，清 wire 快照防断额/崩溃续接重放被删原文。
    mdga_storage::delete_wire_snapshot(&db, &conversation_id).map_err(|e| e.to_string())?;
    save_message(
        &db,
        &conversation_id,
        "assistant",
        &format!("📋 对话已手动压缩（/compact），以下为此前内容的摘要：\n\n{summary}"),
        None,
        None,
    )
    .map_err(|e| e.to_string())
}

/// 列出工作区 .mdga/commands/*.md 自定义斜杠命令（name + description + body）。
/// 命令体中的 $ARGUMENTS 由前端替换为用户在 /name 后输入的参数。
#[tauri::command]
pub(crate) fn list_custom_commands(
    state: State<AppState>,
    conversation_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    let workspace = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        get_conversation(&db, &conversation_id)
            .map_err(|e| e.to_string())?
            .and_then(|c| c.workspace_path)
    };
    let Some(workspace) = workspace else {
        return Ok(Vec::new());
    };
    let dir = std::path::Path::new(&workspace).join(".mdga").join("commands");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut cmds = Vec::new();
    for entry in entries.flatten().take(50) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        let Ok(raw) = std::fs::read_to_string(&path) else { continue };
        // 解析 frontmatter description（可选），其余为命令体。
        let (description, body) = parse_command_frontmatter(&raw);
        cmds.push(serde_json::json!({
            "name": format!("/{stem}"),
            "description": description,
            "body": body,
        }));
    }
    Ok(cmds)
}

/// 从命令 markdown 解析 frontmatter 的 description，返回 (description, body)。
fn parse_command_frontmatter(raw: &str) -> (String, String) {
    let trimmed = raw.trim_start();
    if let Some(rest) = trimmed.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            let front = &rest[..end];
            let body = rest[end + 4..].trim_start().to_string();
            let desc = front
                .lines()
                .find_map(|l| l.trim().strip_prefix("description:").map(|d| d.trim().to_string()))
                .unwrap_or_default();
            return (desc, body);
        }
    }
    // 无 frontmatter：首行非空作描述，全文作命令体
    let desc = raw.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string();
    (desc, raw.trim().to_string())
}

/// 平铺列出当前会话工作区内的文件相对路径，供输入框 @文件引用补全。
#[tauri::command]
pub(crate) fn list_workspace_files(
    state: State<AppState>,
    conversation_id: String,
) -> Result<Vec<String>, String> {
    let workspace = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        get_conversation(&db, &conversation_id)
            .map_err(|e| e.to_string())?
            .and_then(|c| c.workspace_path)
            .ok_or("该会话未绑定工作区")?
    };
    Ok(mdga_tool_runtime::workspace_file_list(&workspace, 500))
}

/// 读取命令沙箱开关状态。
#[tauri::command]
pub(crate) fn get_command_sandbox(state: State<AppState>) -> bool {
    state.command_sandbox.load(Ordering::SeqCst)
}

/// 设置命令沙箱开关。开启时前台命令在受限令牌沙箱中执行（降权 + 进程清理 + 密钥擦除）。
#[tauri::command]
pub(crate) fn set_command_sandbox(state: State<AppState>, enabled: bool) {
    state.command_sandbox.store(enabled, Ordering::SeqCst);
}

/// 把探测到的沙箱层级 + 是否降级翻译成给用户的人话（0.0.68 降级可观测）。
/// **务必区分**「受限令牌(仍剥管理员特权 + Job 进程树 + 擦密钥)」与「完全裸跑」,不要把降级说成无保护。
fn sandbox_layer_note(layer: Option<&str>, degraded: bool) -> String {
    match (layer, degraded) {
        (Some("appcontainer"), false) => {
            "完整隔离：AppContainer——文件路径隔离(仅显式授权的工作区可读写)+ 网络默认拒绝。".to_string()
        }
        (Some("restricted"), _) | (_, true) => {
            "受限沙箱(已降级)：仍剥管理员特权 + Job 进程树清理 + 擦密钥,但**无文件路径 / 网络隔离**\
             (常见于被 dev 监视或文件数过大的工作区)。".to_string()
        }
        (Some(other), _) => format!("沙箱层级：{other}。"),
        (None, false) => "未沙箱：命令直接裸跑,无任何隔离。".to_string(),
    }
}

/// 探测当前工作区下命令**实际生效**的沙箱层级（0.0.68 降级可观测）：跑一条无害命令,回报
/// AppContainer / 受限(降级) / 关闭 / 启动失败,供用户或 UI 确认「我现在到底有没有真隔离」,
/// 而不是静默降级。只读、无副作用（仅 echo 一行)。
#[tauri::command]
pub(crate) fn probe_command_sandbox(
    state: State<AppState>,
    workspace: String,
) -> Result<serde_json::Value, String> {
    let enabled = state.command_sandbox.load(Ordering::SeqCst);
    if !enabled {
        return Ok(serde_json::json!({
            "enabled": false, "layer": "none", "degraded": false,
            "note": "命令沙箱已关闭：命令直接裸跑,无文件 / 网络隔离。"
        }));
    }
    let ws = workspace.trim();
    if ws.is_empty() {
        return Err("当前会话无工作区,无法探测沙箱层级".to_string());
    }
    let policy = mdga_tool_runtime::CommandSandbox::for_session(true, false);
    match mdga_tool_runtime::run_command_streaming(
        ws,
        mdga_tool_runtime::RunCommandRequest {
            command: "echo mdga-sandbox-probe".to_string(),
            timeout_secs: Some(30),
            background: false,
        },
        None,
        None,
        policy,
    ) {
        Ok(r) => Ok(serde_json::json!({
            "enabled": true,
            "layer": r.sandbox_layer,
            "degraded": r.sandbox_degraded,
            "note": sandbox_layer_note(r.sandbox_layer.as_deref(), r.sandbox_degraded),
        })),
        Err(e) => Ok(serde_json::json!({
            "enabled": true, "layer": serde_json::Value::Null, "degraded": true,
            "error": e.to_string(),
            "note": "沙箱启动失败：已 fail-closed(命令会被拒绝执行,不会裸跑)。"
        })),
    }
}

/// 导出单个会话为 Markdown 文件（数据治理：用户可导出/备份）。
#[tauri::command]
pub(crate) fn export_conversation(
    state: State<AppState>,
    conversation_id: String,
    path: String,
) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let conv = get_conversation(&db, &conversation_id)
        .map_err(|e| e.to_string())?
        .ok_or("会话不存在")?;
    let messages = get_messages(&db, &conversation_id).map_err(|e| e.to_string())?;
    let mut md = format!("# {}\n\n", conv.title);
    if let Some(ws) = conv.workspace_path.as_deref() {
        md.push_str(&format!("> 工作区：{ws}\n\n"));
    }
    for m in messages {
        let who = if m.role == "user" { "用户" } else { "助手" };
        md.push_str(&format!("## {who}\n\n{}\n\n", m.content));
    }
    std::fs::write(&path, md).map_err(|e| format!("写入失败: {e}"))
}

/// 导出全部会话的 token 账本为 CSV（数据治理 + 对账）。
#[tauri::command]
pub(crate) fn export_token_ledger(state: State<AppState>, path: String) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let convs = list_conversations(&db).map_err(|e| e.to_string())?;
    let mut csv = String::from(
        "conversation_id,title,role,total_tokens,prompt_tokens,completion_tokens,estimated_cost_usd,created_at\n",
    );
    for conv in convs {
        let messages = get_messages(&db, &conv.id).map_err(|e| e.to_string())?;
        for m in messages {
            let Some(usage_json) = m.usage_json.as_deref() else { continue };
            let v: serde_json::Value = serde_json::from_str(usage_json).unwrap_or_default();
            let g = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
            let cost = v.get("estimatedCostUsd").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let title = conv.title.replace([',', '\n', '"'], " ");
            csv.push_str(&format!(
                "{},{},{},{},{},{},{:.6},{}\n",
                conv.id, title, m.role, g("totalTokens"), g("promptTokens"),
                g("completionTokens"), cost, m.created_at
            ));
        }
        // 独立账本条目（如视觉调用 kind="vision"，Plan19 C-B）：usage_json 为 RawUsage（snake_case），
        // role 列写 kind 以便与主模型消息区分；视觉供应商定价不一,成本列暂记 0。
        let title = conv.title.replace([',', '\n', '"'], " ");
        for entry in get_token_ledger_entries(&db, &conv.id).unwrap_or_default() {
            let v: serde_json::Value = serde_json::from_str(&entry.usage_json).unwrap_or_default();
            let g = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
            csv.push_str(&format!(
                "{},{},{},{},{},{},{:.6},{}\n",
                conv.id, title, entry.kind, g("total_tokens"), g("prompt_tokens"),
                g("completion_tokens"), 0.0, entry.created_at
            ));
        }
    }
    std::fs::write(&path, csv).map_err(|e| format!("写入失败: {e}"))
}

/// 清除全部会话与消息（数据治理：用户主动删除本地数据）。
#[tauri::command]
pub(crate) fn clear_all_conversations(state: State<AppState>) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    mdga_storage::delete_all_conversations(&db).map_err(|e| e.to_string())
}

/// 读取单次任务 token 预算（0 = 不限）。
#[tauri::command]
pub(crate) fn get_task_budget(state: State<AppState>) -> u64 {
    state.task_token_budget.load(Ordering::SeqCst)
}

/// 设置单次任务 token 预算；超出后工具循环暂停并提示。
#[tauri::command]
pub(crate) fn set_task_budget(state: State<AppState>, budget: u64) {
    state.task_token_budget.store(budget, Ordering::SeqCst);
}

/// 列出全部权限规则（allow / deny），供设置页管理。
#[tauri::command]
pub(crate) fn get_permission_rules(state: State<AppState>) -> Result<Vec<String>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    list_permission_rules(&db).map_err(|e| e.to_string())
}

/// 新增一条权限规则（如 `deny:read_file:**/.env`、`allow:cmd:git push`）。
#[tauri::command]
pub(crate) fn create_permission_rule(state: State<AppState>, rule: String) -> Result<(), String> {
    let rule = rule.trim();
    if rule.is_empty() {
        return Err("规则不能为空".to_string());
    }
    let db = state.db.lock().map_err(|e| e.to_string())?;
    add_permission_rule(&db, rule).map_err(|e| e.to_string())
}

/// 删除一条权限规则。
#[tauri::command]
pub(crate) fn delete_permission_rule(state: State<AppState>, rule: String) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    remove_permission_rule(&db, &rule).map_err(|e| e.to_string())
}

/// 列出 MCP server 配置及连接状态（connected/disconnected + 工具数）。
#[tauri::command]
pub(crate) fn get_mcp_servers(state: State<AppState>) -> Result<Vec<serde_json::Value>, String> {
    let records = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        list_mcp_servers(&db).map_err(|e| e.to_string())?
    };
    let connected = state.mcp.lock().map_err(|e| e.to_string())?;
    Ok(records
        .into_iter()
        .map(|r| {
            let client = connected.get(&r.id);
            serde_json::json!({
                "id": r.id,
                "name": r.name,
                "command": r.command,
                "enabled": r.enabled,
                "connected": client.is_some(),
                "toolCount": client.map(|c| c.tools.len()).unwrap_or(0),
            })
        })
        .collect())
}

/// 新增 MCP server 配置并立即后台尝试连接。
#[tauri::command]
#[allow(non_snake_case)]
pub(crate) fn create_mcp_server(
    app: AppHandle,
    state: State<AppState>,
    name: String,
    command: String,
    authToken: Option<String>,
) -> Result<(), String> {
    let name = name.trim();
    let command = command.trim();
    if name.is_empty() || command.is_empty() {
        return Err("名称与启动命令/URL 不能为空".to_string());
    }
    let token = authToken.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let record = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        add_mcp_server(&db, name, command, token).map_err(|e| e.to_string())?
    };
    spawn_mcp_connect(&app, record);
    Ok(())
}

/// 启用 / 停用一个 MCP server：停用立即断开，启用立即后台重连。
#[tauri::command]
pub(crate) fn toggle_mcp_server(
    app: AppHandle,
    state: State<AppState>,
    server_id: String,
    enabled: bool,
) -> Result<(), String> {
    let record = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        set_mcp_server_enabled(&db, &server_id, enabled).map_err(|e| e.to_string())?;
        list_mcp_servers(&db)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|r| r.id == server_id)
    };
    if enabled {
        if let Some(record) = record {
            spawn_mcp_connect(&app, record);
        }
    } else if let Ok(mut map) = state.mcp.lock() {
        map.remove(&server_id); // Drop 时杀子进程
    }
    Ok(())
}

/// 删除一个 MCP server 配置并断开连接。
#[tauri::command]
pub(crate) fn delete_mcp_server(state: State<AppState>, server_id: String) -> Result<(), String> {
    {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        remove_mcp_server(&db, &server_id).map_err(|e| e.to_string())?;
    }
    if let Ok(mut map) = state.mcp.lock() {
        map.remove(&server_id);
    }
    Ok(())
}

/// 导入本地文档并抽取纯文本（TXT/MD/CSV/JSON/PDF/DOCX），供发送给模型问答。
#[tauri::command]
pub(crate) fn import_file_text(path: String) -> Result<serde_json::Value, String> {
    const MAX_IMPORT_CHARS: usize = 100_000;
    let file_path = std::path::Path::new(&path);
    if !file_path.is_file() {
        return Err("文件不存在".to_string());
    }
    let name = file_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = file_path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let text = match ext.as_str() {
        "txt" | "md" | "markdown" | "csv" | "json" | "log" | "xml" | "html" | "toml"
        | "yaml" | "yml" => std::fs::read_to_string(file_path)
            .map_err(|e| format!("读取文件失败: {e}"))?,
        "pdf" => pdf_extract::extract_text(file_path)
            .map_err(|e| format!("PDF 解析失败: {e}"))?,
        "docx" => extract_docx_text(file_path)?,
        other => {
            return Err(format!(
                "暂不支持 .{other} 格式（支持 txt/md/csv/json/pdf/docx 等文本类文档；图片需视觉模型，后续版本支持）"
            ));
        }
    };

    let truncated = text.chars().count() > MAX_IMPORT_CHARS;
    let capped: String = text.chars().take(MAX_IMPORT_CHARS).collect();
    Ok(serde_json::json!({ "name": name, "text": capped, "truncated": truncated }))
}

/// 读取本地图片并返回 base64 + media_type（Plan18 M18.1：composer 附图）。
///
/// 输入图片路径；校验为支持的图像扩展名（png/jpg/jpeg/gif/webp），读字节并 base64 编码，
/// 返回 { name, mediaType, base64 }。限制 ~10MB，避免超大图撑爆请求体与 SQLite。
#[tauri::command]
pub(crate) fn read_image_base64(path: String) -> Result<serde_json::Value, String> {
    use base64::Engine as _;
    const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
    let file_path = std::path::Path::new(&path);
    if !file_path.is_file() {
        return Err("图片不存在".to_string());
    }
    let name = file_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = file_path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let media_type = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        other => return Err(format!("不支持的图片格式 .{other}（支持 png/jpg/jpeg/gif/webp）")),
    };
    let meta = std::fs::metadata(file_path).map_err(|e| format!("读取图片失败: {e}"))?;
    if meta.len() > MAX_IMAGE_BYTES {
        return Err("图片过大（上限 10MB），请压缩后再上传".to_string());
    }
    let bytes = std::fs::read(file_path).map_err(|e| format!("读取图片失败: {e}"))?;
    let base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(serde_json::json!({ "name": name, "mediaType": media_type, "base64": base64 }))
}

/// 从 .docx（zip 内 word/document.xml）抽取纯文本：拼接所有 <w:t> 文本节点，段落换行。
fn extract_docx_text(path: &std::path::Path) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("打开文件失败: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("DOCX 解析失败: {e}"))?;
    let mut doc = archive
        .by_name("word/document.xml")
        .map_err(|_| "DOCX 缺少 document.xml".to_string())?;
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut doc, &mut xml).map_err(|e| format!("读取失败: {e}"))?;

    // 轻量抽取：<w:p> 段落分行，<w:t> 文本节点取内容；不引入完整 XML 解析依赖。
    let mut out = String::new();
    let paragraphs = xml.split("</w:p>");
    for paragraph in paragraphs {
        let mut cursor = 0;
        let bytes = paragraph;
        while let Some(start) = bytes[cursor..].find("<w:t") {
            let tag_start = cursor + start;
            let Some(open_end) = bytes[tag_start..].find('>') else { break };
            let text_start = tag_start + open_end + 1;
            let Some(close) = bytes[text_start..].find("</w:t>") else { break };
            out.push_str(&bytes[text_start..text_start + close]);
            cursor = text_start + close + 6;
        }
        if !out.ends_with('\n') && !out.is_empty() {
            out.push('\n');
        }
    }
    Ok(out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'"))
}

/// 返回指定会话的所有工具 Activity Event，按时间正序，供前端展示历史过程面板。
#[tauri::command]
pub(crate) fn get_conversation_events(
    state: State<AppState>,
    conversation_id: String,
) -> Result<Vec<ActivityEventRecord>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    get_activity_events(&db, &conversation_id).map_err(|e| e.to_string())
}

/// 一条最近被拦截的动作（Plan27 C6 #9）：工具名 + 其目标，供「一键加规则」UI 列出。
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeniedAction {
    pub tool_name: String,
    /// 动作目标：路径 / 源路径 / 命令行（按工具类型从入参提取，提取不到为空串）。
    pub target: String,
}

/// 从工具入参 JSON 提取「目标」：优先 path，其次 from，再次 command（与前端 extractTarget 同款）。
/// 解析失败或都不存在时返回空串。
fn extract_denied_target(input_json: Option<&str>) -> String {
    let Some(raw) = input_json else {
        return String::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return String::new();
    };
    for key in ["path", "from", "command"] {
        if let Some(s) = value.get(key).and_then(|v| v.as_str()) {
            if !s.trim().is_empty() {
                return s.to_string();
            }
        }
    }
    String::new()
}

/// 列出最近被拦截/权限失败的工具动作（Plan27 C6 #9）。
///
/// 跨全部会话扫描 activity events，取 status 为 denied（被拒/权限失败）的工具事件，按时间倒序，
/// 提取 toolName 与 target（path/from/command）并按 (tool, target) 去重，取最近若干条。
/// 供设置页「权限规则」区列出，每条配「+ 允许 / + 拒绝」按钮，复用前端 handleAddPermRule 构造规则串。
#[tauri::command]
pub(crate) fn recent_denied_actions(state: State<AppState>) -> Result<Vec<DeniedAction>, String> {
    const MAX_DENIED: usize = 20;
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let convs = list_conversations(&db).map_err(|e| e.to_string())?;
    // 汇总全部会话的被拒事件（每会话内 get_activity_events 为时间正序）。
    let mut events: Vec<ActivityEventRecord> = Vec::new();
    for conv in &convs {
        let conv_events = get_activity_events(&db, &conv.id).unwrap_or_default();
        events.extend(
            conv_events
                .into_iter()
                .filter(|e| e.status == "denied" && e.tool_name.is_some()),
        );
    }
    // 按时间倒序：最近的被拒动作排在前。
    events.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut out: Vec<DeniedAction> = Vec::new();
    for event in events {
        let Some(tool_name) = event.tool_name else { continue };
        let target = extract_denied_target(event.input_json.as_deref());
        if !seen.insert((tool_name.clone(), target.clone())) {
            continue; // (工具, 目标) 去重
        }
        out.push(DeniedAction { tool_name, target });
        if out.len() >= MAX_DENIED {
            break;
        }
    }
    Ok(out)
}

/// 在 Agent 运行中排队一条插话消息（steering）。下一轮循环开始时作为 user 消息注入，
/// 让用户无需打断即可纠偏 / 追加要求。返回当前队列长度。
#[tauri::command]
pub(crate) fn queue_steering(
    state: State<AppState>,
    conversation_id: String,
    text: String,
) -> Result<usize, String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("插话内容不能为空".to_string());
    }
    let mut steering = state.steering.lock().map_err(|e| e.to_string())?;
    let queue = steering.entry(conversation_id).or_default();
    queue.push(text);
    Ok(queue.len())
}

/// 请求中断指定会话正在运行的 Agent 工具循环。
///
/// 置位该会话的取消标志；循环在下一个检查点安全收尾，已执行的工具结果保留。
/// 若该会话当前没有运行中的 Agent，则为无操作。
#[tauri::command]
pub(crate) fn cancel_agent(state: State<AppState>, conversation_id: String) -> Result<(), String> {
    // 对话总开关:① 置主任务取消 flag(同步子任务共享主 cancel,随之自动停;流式请求经 select! 立即中断)。
    {
        let cancels = state.cancels.lock().map_err(|e| e.to_string())?;
        if let Some(token) = cancels.get(&conversation_id) {
            token.store(true, Ordering::SeqCst);
        }
    }
    // ② 级联停掉该会话所有后台子任务(run_subtask background=true,独立 cancel,每轮检查后停)。
    if let Ok(tasks) = state.bg_tasks.lock() {
        for task in tasks.values() {
            if task.conversation_id == conversation_id {
                task.cancel.store(true, Ordering::SeqCst);
            }
        }
    }
    Ok(())
}

/// 前端对一次高风险动作审批请求作出回应（允许 / 拒绝 / 总是允许）。
///
/// 通过 action_id 找到对应的 oneshot 通道并发送结果，唤醒正在等待的工具循环；
/// remember=true 且批准时，把该动作的规则写入 permission_rules 表，后续同类动作免审批。
#[tauri::command]
pub(crate) fn respond_approval(
    state: State<AppState>,
    action_id: String,
    approved: bool,
    remember: Option<bool>,
) -> Result<(), String> {
    let entry = {
        let mut approvals = state.approvals.lock().map_err(|e| e.to_string())?;
        approvals.remove(&action_id)
    };
    if let Some((sender, rule)) = entry {
        if approved && remember.unwrap_or(false) && !rule.is_empty() {
            if let Ok(db) = state.db.lock() {
                let _ = add_permission_rule(&db, &rule);
            }
        }
        let _ = sender.send(approved);
    }
    Ok(())
}

/// 前端对一次 ask_user 结构化提问作出回应，把答案 JSON（已含用户选择 / 自定义文本）
/// 通过 question_id 对应的 oneshot 通道送回正在等待的工具循环。
#[tauri::command]
pub(crate) fn respond_ask_user(
    state: State<AppState>,
    question_id: String,
    answer: String,
) -> Result<(), String> {
    let sender = {
        let mut map = state.ask_questions.lock().map_err(|e| e.to_string())?;
        map.remove(&question_id)
    };
    if let Some(sender) = sender {
        let _ = sender.send(answer);
    }
    Ok(())
}

// ── 工作区管理 ─────────────────────────────────────────────────────────────

/// 返回当前用户授权的活动工作区。
///
/// 输入应用状态；输出当前 Workspace 或 None，用于前端展示 Agent 可操作目录边界。
#[tauri::command]
pub(crate) fn get_workspace(state: State<AppState>) -> Result<Option<Workspace>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    get_active_workspace(&db).map_err(|e| e.to_string())
}

/// 保存当前用户授权的工作区路径。
///
/// 输入本地目录路径；后端校验路径存在且为目录，写入 SQLite 后返回 Workspace。
#[tauri::command]
pub(crate) fn set_workspace_path(state: State<AppState>, path: String) -> Result<Workspace, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("工作区路径不能为空".to_string());
    }

    let path_buf = std::path::PathBuf::from(trimmed);
    if !path_buf.is_dir() {
        return Err("工作区路径不存在或不是目录".to_string());
    }

    let db = state.db.lock().map_err(|e| e.to_string())?;
    save_active_workspace(&db, trimmed).map_err(|e| e.to_string())
}

/// 清除当前工作区授权。
///
/// 输入应用状态；删除当前活动工作区记录，后续 Agent 文件能力应视为未授权。
#[tauri::command]
pub(crate) fn clear_workspace(state: State<AppState>) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    clear_active_workspace(&db).map_err(|e| e.to_string())
}

// ── 自动更新 ──────────────────────────────────────────────────────────────

/// 检查 GitHub Releases 是否有新版本。
///
/// 返回新版本号字符串，无更新返回 None，出错返回 Err。
#[tauri::command]
pub(crate) async fn check_update(app: AppHandle) -> Result<Option<String>, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    match updater.check().await {
        Ok(Some(update)) => Ok(Some(update.version.clone())),
        Ok(None) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// 下载并安装更新，完成后自动重启应用。
///
/// 必须在用户明确确认后调用；下载进度通过 "update-progress" 事件推送。
#[tauri::command]
pub(crate) async fn install_update(app: AppHandle) -> Result<(), String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "没有可用更新".to_string())?;

    let app_clone = app.clone();
    update
        .download_and_install(
            |downloaded, total| {
                // downloaded: usize, total: Option<u64>，统一转 u64 再计算百分比
                let pct = total.map(|t| downloaded as u64 * 100 / t).unwrap_or(0);
                let _ = app_clone.emit("update-progress", pct);
            },
            || {
                let _ = app.emit("update-ready", ());
            },
        )
        .await
        .map_err(|e| e.to_string())?;

    app.restart();
}

/// 在用户的真实浏览器打开一个外链（0.0.67：供 widget 沙箱的 openLink 桥用）。
///
/// 仅放行 http(s)——拒绝 file: / javascript: / data: 等任何其它 scheme,避免沙箱 widget 借此越权。
/// 前端只在向用户弹确认后才调用本命令；本命令只负责「http(s) 守卫 + 交系统打开」。
#[tauri::command]
pub(crate) fn open_external_url(app: AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let u = url.trim();
    // 拒绝含控制字符（\n / \r / \t / NUL 等）的 URL：防止换行拼接的解析歧义 / 日志注入,
    // 也让前端确认框展示的就是将要打开的整串(无隐藏行)。
    if u.is_empty() || u.chars().any(|c| c.is_control()) {
        return Err("链接为空或含非法控制字符".to_string());
    }
    if !(u.starts_with("http://") || u.starts_with("https://")) {
        return Err("仅允许打开 http(s) 链接".to_string());
    }
    app.opener()
        .open_url(u.to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}

// ── 命令层共享小工具 ───────────────────────────────────────────────────────

/// 从工作区路径取末段目录名作为工作区显示名；取不到时回退原路径。
fn workspace_name_from_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(path)
        .to_string()
}

/// 将前端权限模式字符串映射为后端枚举，未知值回退到最安全的 Restricted。
pub(crate) fn permission_mode_from_str(value: &str) -> PermissionMode {
    match value {
        "ask_every_time" => PermissionMode::AskEveryTime,
        "workspace_auto" => PermissionMode::WorkspaceAuto,
        "full_access" => PermissionMode::FullAccess,
        _ => PermissionMode::Restricted,
    }
}
