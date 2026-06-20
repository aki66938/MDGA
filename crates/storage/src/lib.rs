use rusqlite::{params, Connection, Result as SqlResult};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

// ── 公共类型 ─────────────────────────────────────────────────────────────

/// 会话记录，对应 conversations 表一行。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub workspace_path: Option<String>,
    pub workspace_name: Option<String>,
    pub mode: String,
    /// 置顶：列表排序时优先于普通会话。
    pub pinned: bool,
    /// 归档：从主列表移入「已归档」区，不删除数据。
    pub archived: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 消息记录，对应 messages 表一行。
///
/// usage_json 保存序列化后的 CostSummary JSON，为 None 表示本条消息无 token 统计。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredMessage {
    pub id: String,
    pub conversation_id: String,
    pub role: String,
    pub content: String,
    pub usage_json: Option<String>,
    /// 序列化后的消息 parts（文字块 + 工具卡片交错），用于重启后还原内联工具执行记录。
    /// 为 None 表示旧数据或纯文字消息，前端回退为单个 text part。content 仍保留纯文字供模型上下文。
    pub parts_json: Option<String>,
    pub created_at: i64,
}

/// 工作区记录，对应用户授权给 Agent 的本地目录。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub path: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub active: bool,
}

/// Activity Event 记录，对应一次工具调用的请求、裁决和执行结果，用于审计与前端过程展示。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEventRecord {
    pub id: String,
    pub conversation_id: String,
    pub event_type: String,
    pub tool_name: Option<String>,
    pub status: String,
    pub input_json: Option<String>,
    pub output_json: Option<String>,
    pub error_message: Option<String>,
    pub workspace_path: Option<String>,
    pub created_at: i64,
}

/// 文件变更检查点：每次写类工具执行前的文件快照，支撑回退（rewind）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileCheckpoint {
    pub id: String,
    pub conversation_id: String,
    /// 会话内单调递增序号，回退按序号倒序执行。
    pub seq: i64,
    pub tool_name: String,
    pub rel_path: String,
    /// 变更前的文件全文；None 表示此前文件不存在（回退 = 删除）。
    pub prev_content: Option<String>,
    /// 工具相关附加信息（如 move_path 的 from/to）。
    pub extra_json: Option<String>,
    /// 是否可回退（delete_dir 递归删除等不可回退）。
    pub revertible: bool,
    /// 是否已被回退。
    pub reverted: bool,
    pub created_at: i64,
}

// ── 初始化 ────────────────────────────────────────────────────────────────

/// 打开或创建 SQLite 数据库，执行 schema 迁移。
///
/// 输入数据库文件路径；首次调用自动建表，后续调用幂等；
/// 返回已就绪的 Connection，错误时返回 rusqlite::Error。
pub fn init_db(path: &Path) -> SqlResult<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS conversations (
            id         TEXT PRIMARY KEY,
            title      TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS messages (
            id              TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
            role            TEXT NOT NULL,
            content         TEXT NOT NULL,
            usage_json      TEXT,
            created_at      INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_messages_conv
            ON messages (conversation_id, created_at);

        CREATE TABLE IF NOT EXISTS workspaces (
            id         TEXT PRIMARY KEY,
            name       TEXT NOT NULL,
            path       TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            active     INTEGER NOT NULL DEFAULT 0
        );

        CREATE INDEX IF NOT EXISTS idx_workspaces_active
            ON workspaces (active, updated_at);

        CREATE TABLE IF NOT EXISTS activity_events (
            id              TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL,
            event_type      TEXT NOT NULL,
            tool_name       TEXT,
            status          TEXT NOT NULL,
            input_json      TEXT,
            output_json     TEXT,
            error_message   TEXT,
            workspace_path  TEXT,
            created_at      INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_activity_conv
            ON activity_events (conversation_id, created_at);

        CREATE TABLE IF NOT EXISTS file_checkpoints (
            id              TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL,
            seq             INTEGER NOT NULL,
            tool_name       TEXT NOT NULL,
            rel_path        TEXT NOT NULL,
            prev_content    TEXT,
            extra_json      TEXT,
            revertible      INTEGER NOT NULL DEFAULT 1,
            reverted        INTEGER NOT NULL DEFAULT 0,
            created_at      INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_checkpoints_conv
            ON file_checkpoints (conversation_id, seq);

        CREATE TABLE IF NOT EXISTS permission_rules (
            id         TEXT PRIMARY KEY,
            rule       TEXT NOT NULL UNIQUE,
            created_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS mcp_servers (
            id         TEXT PRIMARY KEY,
            name       TEXT NOT NULL,
            command    TEXT NOT NULL,
            enabled    INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS model_providers (
            id          TEXT PRIMARY KEY,
            role        TEXT NOT NULL,
            preset      TEXT,
            label       TEXT,
            base_url    TEXT,
            api_key     TEXT NOT NULL,
            model_id    TEXT NOT NULL,
            enabled     INTEGER NOT NULL DEFAULT 1,
            updated_at  INTEGER
        );

        CREATE INDEX IF NOT EXISTS idx_model_providers_role
            ON model_providers (role);

        -- 0.0.59「连接库 + 角色引用」：connection = 一份「端点 + 密钥」接入(配一次),
        -- role_assignment = 一条「角色 → 模型」纯引用(无密钥,指向某 connection)。
        -- base_url 可空,''/NULL 表示走 preset 官方端点;api_key 明文存(local-first)。
        CREATE TABLE IF NOT EXISTS connections (
            id          TEXT PRIMARY KEY,
            label       TEXT,
            preset      TEXT,
            base_url    TEXT,
            api_key     TEXT NOT NULL,
            api_format  TEXT NOT NULL DEFAULT 'openai',
            created_at  INTEGER,
            updated_at  INTEGER
        );

        -- 每个角色至多一条引用(role 为主键)。enabled=0 视为「未启用 → 回退 main」。
        CREATE TABLE IF NOT EXISTS role_assignments (
            role            TEXT PRIMARY KEY,
            connection_id   TEXT NOT NULL,
            model_id        TEXT NOT NULL,
            context_window  INTEGER,
            enabled         INTEGER NOT NULL DEFAULT 1,
            updated_at      INTEGER
        );

        CREATE INDEX IF NOT EXISTS idx_role_assignments_conn
            ON role_assignments (connection_id);

        -- 0.0.60「模型层」：在 connection 与 role 之间插入用户自建的「模型」中间层。
        -- 一个 connection（端点 + 密钥）下有多个 model（同一把 DeepSeek key 同时跑 pro 与 flash）；
        -- model_id = 实际 API 模型串（如 'deepseek-chat'）;label 可选展示名;context_window 为模型粒度
        --（从旧 role_assignments 下沉到此）。同 (connection_id, model_id) 唯一，避免重复登记。
        CREATE TABLE IF NOT EXISTS models (
            id              TEXT PRIMARY KEY,
            connection_id   TEXT NOT NULL,
            model_id        TEXT NOT NULL,
            label           TEXT,
            context_window  INTEGER,
            created_at      INTEGER,
            updated_at      INTEGER,
            UNIQUE(connection_id, model_id)
        );

        CREATE INDEX IF NOT EXISTS idx_models_conn
            ON models (connection_id);

        -- 0.0.60 新「真源」角色分配表：role → 一个 curated model（model_ref → models.id）。
        -- 取代 0.0.59 的 role_assignments 成为运行时解析来源；后者保留为惰性 legacy/回滚源。
        CREATE TABLE IF NOT EXISTS role_models (
            role        TEXT PRIMARY KEY,
            model_ref   TEXT NOT NULL,
            enabled     INTEGER NOT NULL DEFAULT 1,
            updated_at  INTEGER
        );

        CREATE TABLE IF NOT EXISTS app_settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS token_ledger (
            id              TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL,
            kind            TEXT NOT NULL,
            usage_json      TEXT NOT NULL,
            created_at      INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_token_ledger_conv
            ON token_ledger (conversation_id, created_at);

        CREATE TABLE IF NOT EXISTS wire_snapshots (
            conversation_id TEXT PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE,
            wire_json       TEXT NOT NULL,
            updated_at      INTEGER NOT NULL
        );

        -- 0.0.72「订阅套餐·月度用量」：按 (连接, 年月) 累计原始 token 用量,给订阅进度条做数据支撑。
        -- 与计价/成本路径完全隔离(不读单价、不进 token_ledger);也与「会话累计」(前端从
        -- message.usageJson 聚合)是两套独立的数,互不重复计。对所有连接都记(订阅日后切换有历史)。
        CREATE TABLE IF NOT EXISTS usage_counters (
            connection_id     TEXT NOT NULL,
            ym                TEXT NOT NULL,         -- \"YYYY-MM\"(UTC)
            prompt_tokens     INTEGER NOT NULL DEFAULT 0,
            completion_tokens INTEGER NOT NULL DEFAULT 0,
            total_tokens      INTEGER NOT NULL DEFAULT 0,
            updated_at        INTEGER,
            PRIMARY KEY (connection_id, ym)
        );
        ",
    )?;
    add_column_if_missing(&conn, "conversations", "workspace_path", "TEXT")?;
    add_column_if_missing(&conn, "conversations", "workspace_name", "TEXT")?;
    add_column_if_missing(&conn, "conversations", "mode", "TEXT NOT NULL DEFAULT 'chat_only'")?;
    add_column_if_missing(&conn, "messages", "parts_json", "TEXT")?;
    add_column_if_missing(&conn, "conversations", "pinned", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(&conn, "conversations", "archived", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(&conn, "mcp_servers", "auth_token", "TEXT")?;
    // Plan18 M18.1：视觉 provider 的 API 格式（'openai' | 'anthropic'），默认 openai。
    // 决定 analyze_image 走哪种端点/鉴权/消息结构（见 Plan18 §4 双格式对照表）。
    add_column_if_missing(&conn, "model_providers", "api_format", "TEXT NOT NULL DEFAULT 'openai'")?;
    // Plan27 C2 #2：供应商上下文窗口（tokens，可空）。主 provider 有值时据其推导压缩软上限。
    add_column_if_missing(&conn, "model_providers", "context_window", "INTEGER")?;
    // 0.0.72 计价：模型粒度单价快照（pricing_json，前端存「价格字段 + _ 前缀元数据」的 JSON 串，
    // 后端原样存取；结算时仅以 serde 解析价格字段，多余元数据被忽略）；连接粒度计费方式
    // billing_mode（'api' | 'subscription' | 'none'，默认 api）与订阅描述 subscription_json（可空）。
    add_column_if_missing(&conn, "models", "pricing_json", "TEXT")?;
    add_column_if_missing(&conn, "connections", "billing_mode", "TEXT NOT NULL DEFAULT 'api'")?;
    add_column_if_missing(&conn, "connections", "subscription_json", "TEXT")?;
    // 0.0.59：把旧的 role-keyed model_providers 迁到「连接库 + 角色引用」。
    // 一次性、幂等、改前备份 sqlite 文件；旧表保留一版作回滚路径。失败软处理，不阻断建库。
    migrate_to_connections_0059(&conn)?;
    // 0.0.60：在 connection 与 role 之间插入「模型层」。必须在 0.0.59 迁移**之后**跑——
    // 直接从 pre-0.0.59 升级时，0.0.59 先填好 role_assignments，本迁移再据其建 models + role_models。
    // 一次性、幂等；不 drop/alter role_assignments / model_providers（惰性回滚源）。
    migrate_to_models_layer_0060(&conn)?;
    Ok(conn)
}

// ── 工具 ──────────────────────────────────────────────────────────────────

/// 返回当前 Unix 时间戳（秒）。
pub fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 返回当前 UTC 年月，格式 "YYYY-MM"（0.0.72 月度用量计数键）。
///
/// 工作区无 `chrono` 依赖且本任务不引新依赖，故从 [`now_ts`]（Unix 秒）手写 civil 换算，
/// 仅取年/月，逻辑见 [`ym_from_unix_secs`]（Howard Hinnant `civil_from_days` 的截断特化）。
pub fn current_ym() -> String {
    ym_from_unix_secs(now_ts())
}

/// 把 Unix 时间戳（秒，UTC）换算为 "YYYY-MM"。
///
/// 取自 Howard Hinnant 的 `civil_from_days` 算法（chrono 实现同源），但只需要年和月，
/// 故省去「日」的回算。要点：把纪元偏移到 0000-03-01（3 月起算，使闰日落在年末便于整除），
/// 以 146097 天（400 年周期）和 153 天 5 月块做整数运算，正确处理闰年与月份边界。
/// 负时间戳（1970 前）也用 floor 除法正确处理（虽运行时不会出现）。
fn ym_from_unix_secs(secs: i64) -> String {
    // 向下取整到「天」：对负秒数（理论值）也用 floor div，确保 0..86399 区间映射到同一天。
    let days = secs.div_euclid(86_400);
    // 自 1970-01-01 偏移到自 0000-03-01：719468 = 从 0000-03-01 到 1970-01-01 的天数。
    let z = days + 719_468;
    // era = 400 年纪元编号；doe = 纪元内天序号 [0, 146096]。
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    // yoe = 纪元内年份 [0, 399]。
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    // year（以 3 月为岁首的内部年）。
    let y = yoe + era * 400;
    // doy = 该（3 月起算）年内的天序号 [0, 365]。
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    // mp = 以 3 月为 0 的「移位月」[0, 11]。
    let mp = (5 * doy + 2) / 153;
    // 还原为常规月份：mp<10 → +3（3..12）；否则 -9（1..2，且年份要 +1）。
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}")
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> SqlResult<()> {
    // SQLite 不支持 ADD COLUMN IF NOT EXISTS，需先检查 schema，避免重复迁移失败。
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for existing in columns {
        if existing? == column {
            return Ok(());
        }
    }

    conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"), [])?;
    Ok(())
}

// ── Conversation CRUD ─────────────────────────────────────────────────────

/// 创建新会话，初始标题为"新对话"。
///
/// 输入数据库连接；插入一条 conversations 记录并返回完整结构体。
pub fn create_conversation(conn: &Connection) -> SqlResult<Conversation> {
    create_conversation_with_workspace(conn, None, None)
}

/// 创建新会话，并可绑定创建时选择的工作区快照。
///
/// 输入数据库连接、可选工作区路径和名称；输出完整 Conversation。路径存在时 mode 为
/// `local_workspace`，否则为 `chat_only`，用于后续权限和 Agent cwd 判定。
pub fn create_conversation_with_workspace(
    conn: &Connection,
    workspace_path: Option<&str>,
    workspace_name: Option<&str>,
) -> SqlResult<Conversation> {
    let id = Uuid::new_v4().to_string();
    let now = now_ts();
    let mode = if workspace_path.is_some() {
        "local_workspace"
    } else {
        "chat_only"
    };
    conn.execute(
        "INSERT INTO conversations
         (id, title, workspace_path, workspace_name, mode, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![id, "新对话", workspace_path, workspace_name, mode, now],
    )?;
    Ok(Conversation {
        id,
        title: "新对话".to_string(),
        workspace_path: workspace_path.map(str::to_string),
        workspace_name: workspace_name.map(str::to_string),
        mode: mode.to_string(),
        pinned: false,
        archived: false,
        created_at: now,
        updated_at: now,
    })
}

/// 查询所有会话：置顶在前，其余按最近更新时间倒序；归档的也返回，由前端分区展示。
pub fn list_conversations(conn: &Connection) -> SqlResult<Vec<Conversation>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, workspace_path, workspace_name, mode, pinned, archived, created_at, updated_at
         FROM conversations
         ORDER BY pinned DESC, updated_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Conversation {
            id: row.get(0)?,
            title: row.get(1)?,
            workspace_path: row.get(2)?,
            workspace_name: row.get(3)?,
            mode: row.get(4)?,
            pinned: row.get::<_, i64>(5)? != 0,
            archived: row.get::<_, i64>(6)? != 0,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    })?;
    rows.collect()
}

/// 按关键词搜索会话（Plan27 C5 #6 正文搜索）：标题 LIKE 命中，或该会话存在 content LIKE 命中的消息。
///
/// 输入查询串（调用方应已 trim；空串语义未定义，由命令层处理）；对标题与消息正文做大小写不敏感的
/// 子串匹配（SQLite LIKE 对 ASCII 大小写不敏感），DISTINCT 去重，按 updated_at 倒序返回。
/// 转义 LIKE 通配符 % _ \，避免用户输入被当作通配符，statement 用 ESCAPE '\\' 声明转义符。
pub fn search_conversations(conn: &Connection, query: &str) -> SqlResult<Vec<Conversation>> {
    // 转义 LIKE 特殊字符，包成 %query% 子串模式。
    let escaped = query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    let pattern = format!("%{escaped}%");
    let mut stmt = conn.prepare(
        "SELECT DISTINCT c.id, c.title, c.workspace_path, c.workspace_name, c.mode,
                c.pinned, c.archived, c.created_at, c.updated_at
         FROM conversations c
         LEFT JOIN messages m ON m.conversation_id = c.id
         WHERE c.title LIKE ?1 ESCAPE '\\'
            OR m.content LIKE ?1 ESCAPE '\\'
         ORDER BY c.updated_at DESC",
    )?;
    let rows = stmt.query_map([&pattern], |row| {
        Ok(Conversation {
            id: row.get(0)?,
            title: row.get(1)?,
            workspace_path: row.get(2)?,
            workspace_name: row.get(3)?,
            mode: row.get(4)?,
            pinned: row.get::<_, i64>(5)? != 0,
            archived: row.get::<_, i64>(6)? != 0,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    })?;
    rows.collect()
}

/// 设置会话置顶状态。
pub fn set_conversation_pinned(conn: &Connection, conv_id: &str, pinned: bool) -> SqlResult<()> {
    conn.execute(
        "UPDATE conversations SET pinned = ?1 WHERE id = ?2",
        params![pinned as i64, conv_id],
    )?;
    Ok(())
}

/// 设置会话归档状态。归档不删除数据，只影响前端分区展示。
pub fn set_conversation_archived(conn: &Connection, conv_id: &str, archived: bool) -> SqlResult<()> {
    conn.execute(
        "UPDATE conversations SET archived = ?1 WHERE id = ?2",
        params![archived as i64, conv_id],
    )?;
    Ok(())
}

/// 按 ID 查询单个会话。
///
/// 输入数据库连接和会话 ID；输出完整 Conversation 或 None，供发送链路读取 session 级工作区快照。
pub fn get_conversation(conn: &Connection, conv_id: &str) -> SqlResult<Option<Conversation>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, workspace_path, workspace_name, mode, pinned, archived, created_at, updated_at
         FROM conversations
         WHERE id = ?1
         LIMIT 1",
    )?;
    let mut rows = stmt.query([conv_id])?;

    if let Some(row) = rows.next()? {
        Ok(Some(Conversation {
            id: row.get(0)?,
            title: row.get(1)?,
            workspace_path: row.get(2)?,
            workspace_name: row.get(3)?,
            mode: row.get(4)?,
            pinned: row.get::<_, i64>(5)? != 0,
            archived: row.get::<_, i64>(6)? != 0,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        }))
    } else {
        Ok(None)
    }
}

/// 更新会话标题，同时刷新 updated_at。
pub fn update_title(conn: &Connection, conv_id: &str, title: &str) -> SqlResult<()> {
    conn.execute(
        "UPDATE conversations SET title = ?1, updated_at = ?2 WHERE id = ?3",
        params![title, now_ts(), conv_id],
    )?;
    Ok(())
}

/// 更新已有会话的工作区绑定（Plan23 A：改已有会话工作区）。
///
/// 输入会话 ID、可选工作区路径和名称；更新 workspace_path/workspace_name/mode/updated_at，
/// mode 规则与 create_conversation_with_workspace 一致（path.is_some() → local_workspace，否则
/// chat_only）。更新后查回并返回该 Conversation（复用 get_conversation 的 SELECT 列顺序）。
pub fn update_conversation_workspace(
    conn: &Connection,
    conv_id: &str,
    path: Option<&str>,
    name: Option<&str>,
) -> SqlResult<Conversation> {
    let mode = if path.is_some() {
        "local_workspace"
    } else {
        "chat_only"
    };
    conn.execute(
        "UPDATE conversations
         SET workspace_path = ?1, workspace_name = ?2, mode = ?3, updated_at = ?4
         WHERE id = ?5",
        params![path, name, mode, now_ts(), conv_id],
    )?;
    // 查回最新行返回，列顺序与 get_conversation 保持一致。
    let mut stmt = conn.prepare(
        "SELECT id, title, workspace_path, workspace_name, mode, pinned, archived, created_at, updated_at
         FROM conversations
         WHERE id = ?1
         LIMIT 1",
    )?;
    let mut rows = stmt.query([conv_id])?;
    let row = rows
        .next()?
        .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    Ok(Conversation {
        id: row.get(0)?,
        title: row.get(1)?,
        workspace_path: row.get(2)?,
        workspace_name: row.get(3)?,
        mode: row.get(4)?,
        pinned: row.get::<_, i64>(5)? != 0,
        archived: row.get::<_, i64>(6)? != 0,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

/// 删除会话及其所有消息（ON DELETE CASCADE）。
pub fn delete_conversation(conn: &Connection, conv_id: &str) -> SqlResult<()> {
    conn.execute("DELETE FROM conversations WHERE id = ?1", params![conv_id])?;
    Ok(())
}

/// 清除全部会话（及级联消息），用于数据治理「清除所有会话」。
pub fn delete_all_conversations(conn: &Connection) -> SqlResult<()> {
    conn.execute("DELETE FROM conversations", [])?;
    Ok(())
}

// ── Message CRUD ──────────────────────────────────────────────────────────

/// 保存一条消息，并刷新所属会话的 updated_at。
///
/// 输入会话 ID、角色（user / assistant）、消息内容和可选的 usage JSON；
/// 写入成功后同步更新会话时间戳，使会话列表排序保持最新。
pub fn save_message(
    conn: &Connection,
    conv_id: &str,
    role: &str,
    content: &str,
    usage_json: Option<&str>,
    parts_json: Option<&str>,
) -> SqlResult<()> {
    let id = Uuid::new_v4().to_string();
    let now = now_ts();
    conn.execute(
        "INSERT INTO messages (id, conversation_id, role, content, usage_json, parts_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, conv_id, role, content, usage_json, parts_json, now],
    )?;
    conn.execute(
        "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
        params![now, conv_id],
    )?;
    Ok(())
}

// ── Wire snapshot (任务续接 P0) ────────────────────────────────────────────

/// 落库某会话当前完整 wire 历史(OpenAI 格式消息数组的 JSON)。每会话一行,UPSERT 覆盖。
/// 供断额/崩溃后从 DB 重建完整 wire(含 tool 角色)续接,不再依赖前端 chat-done 整轮才落库。
pub fn save_wire_snapshot(conn: &Connection, conv_id: &str, wire_json: &str) -> SqlResult<()> {
    let now = now_ts();
    conn.execute(
        "INSERT INTO wire_snapshots (conversation_id, wire_json, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(conversation_id) DO UPDATE SET
             wire_json = excluded.wire_json,
             updated_at = excluded.updated_at",
        params![conv_id, wire_json, now],
    )?;
    Ok(())
}

/// 读回某会话的 wire 快照 JSON(无则 None)。供续接重建上下文(P2 消费)。
pub fn get_wire_snapshot(conn: &Connection, conv_id: &str) -> SqlResult<Option<String>> {
    let mut stmt =
        conn.prepare("SELECT wire_json FROM wire_snapshots WHERE conversation_id = ?1")?;
    let mut rows = stmt.query(params![conv_id])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

/// 删除会话的 wire 快照(0.0.49)。截断历史(rewind / compact)后必须清,否则断额/崩溃续接会
/// 从这份旧快照重放被删掉的历史。无则无操作。
pub fn delete_wire_snapshot(conn: &Connection, conv_id: &str) -> SqlResult<()> {
    conn.execute(
        "DELETE FROM wire_snapshots WHERE conversation_id = ?1",
        params![conv_id],
    )?;
    Ok(())
}

/// 查询会话的所有消息，按时间正序排列。
pub fn get_messages(conn: &Connection, conv_id: &str) -> SqlResult<Vec<StoredMessage>> {
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, role, content, usage_json, parts_json, created_at
         FROM messages
         WHERE conversation_id = ?1
         ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([conv_id], |row| {
        Ok(StoredMessage {
            id: row.get(0)?,
            conversation_id: row.get(1)?,
            role: row.get(2)?,
            content: row.get(3)?,
            usage_json: row.get(4)?,
            parts_json: row.get(5)?,
            created_at: row.get(6)?,
        })
    })?;
    rows.collect()
}

/// 删除会话的全部消息（/compact 手动压缩时由摘要替换原文前调用）。
pub fn delete_messages(conn: &Connection, conv_id: &str) -> SqlResult<()> {
    conn.execute(
        "DELETE FROM messages WHERE conversation_id = ?1",
        params![conv_id],
    )?;
    Ok(())
}

/// 删除该会话「最后一条」消息，且仅当其 role='assistant' 时才删（Plan27 C4 #1b 重新生成）。
///
/// 返回是否真的删除了一条：最后一条不是助手消息（如用户刚发完还没回复）或会话无消息，
/// 都返回 false 且不改动数据。供「重新生成」命令先删旧助手回复、再用截至上一条 user 的历史重跑。
pub fn delete_last_assistant_message(conn: &Connection, conv_id: &str) -> SqlResult<bool> {
    // 取最近一条消息的 id 与 role：created_at 倒序、再以 rowid 倒序兜底同秒插入的稳定性。
    let mut stmt = conn.prepare(
        "SELECT id, role FROM messages
         WHERE conversation_id = ?1
         ORDER BY created_at DESC, rowid DESC
         LIMIT 1",
    )?;
    let mut rows = stmt.query([conv_id])?;
    let Some(row) = rows.next()? else {
        return Ok(false); // 会话无消息
    };
    let id: String = row.get(0)?;
    let role: String = row.get(1)?;
    if role != "assistant" {
        return Ok(false); // 最后一条不是助手消息，不动
    }
    let affected = conn.execute("DELETE FROM messages WHERE id = ?1", params![id])?;
    Ok(affected > 0)
}

/// rewind 截断锚点(0.0.49):取「末尾 n 条」里最早一条的 created_at —— 早于它的消息保留、
/// 同它或晚于它的被删。供按时戳关联回退这些被删轮次产生的文件变更。
/// 排序与删除一致用 (created_at DESC, rowid DESC),取第 n 条(OFFSET n-1)即最早被删条。
/// n=0 或会话消息不足 n 条返回 None。
pub fn cut_timestamp_for_last_n(conn: &Connection, conv_id: &str, n: usize) -> SqlResult<Option<i64>> {
    if n == 0 {
        return Ok(None);
    }
    let mut stmt = conn.prepare(
        "SELECT created_at FROM messages
         WHERE conversation_id = ?1
         ORDER BY created_at DESC, rowid DESC
         LIMIT 1 OFFSET ?2",
    )?;
    let mut rows = stmt.query(params![conv_id, (n - 1) as i64])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

/// 删除会话「末尾 n 条」消息(0.0.49 rewind「回退到此处」截断)。返回实删条数。
/// 用子查询按 (created_at DESC, rowid DESC) 选末尾 n 条 id 再删,防同秒并列误删邻近轮次。
pub fn delete_last_n_messages(conn: &Connection, conv_id: &str, n: usize) -> SqlResult<usize> {
    if n == 0 {
        return Ok(0);
    }
    let affected = conn.execute(
        "DELETE FROM messages WHERE id IN (
             SELECT id FROM messages
             WHERE conversation_id = ?1
             ORDER BY created_at DESC, rowid DESC
             LIMIT ?2
         )",
        params![conv_id, n as i64],
    )?;
    Ok(affected)
}

// ── Token 账本（独立条目）─────────────────────────────────────────────────

/// 账本中一条独立的 token 用量记录（Plan19 C-B：视觉等辅助调用与主助手消息分开记账）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenLedgerEntry {
    pub id: String,
    pub conversation_id: String,
    /// 记账类别标记，如 'vision'；CSV 导出据此区分来源，不与主模型 usage 合并。
    pub kind: String,
    /// 序列化后的 CostSummary JSON（与 messages.usage_json 同形）。
    pub usage_json: String,
    pub created_at: i64,
}

/// 向 token 账本写入一条独立条目（不挂在任何 messages 行上，互不干扰）。
///
/// 输入会话 ID、类别标记（如 'vision'）、CostSummary JSON 字符串；用于把视觉等辅助调用的
/// token 开销单独入账，保证 CSV 导出完整、又不污染助手消息的主 usage 徽标。
pub fn save_token_ledger_entry(
    conn: &Connection,
    conv_id: &str,
    kind: &str,
    usage_json: &str,
) -> SqlResult<()> {
    let id = Uuid::new_v4().to_string();
    let now = now_ts();
    conn.execute(
        "INSERT INTO token_ledger (id, conversation_id, kind, usage_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, conv_id, kind, usage_json, now],
    )?;
    Ok(())
}

/// 查询某会话的全部独立账本条目，按时间正序。
pub fn get_token_ledger_entries(
    conn: &Connection,
    conv_id: &str,
) -> SqlResult<Vec<TokenLedgerEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, kind, usage_json, created_at
         FROM token_ledger
         WHERE conversation_id = ?1
         ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([conv_id], |row| {
        Ok(TokenLedgerEntry {
            id: row.get(0)?,
            conversation_id: row.get(1)?,
            kind: row.get(2)?,
            usage_json: row.get(3)?,
            created_at: row.get(4)?,
        })
    })?;
    rows.collect()
}

// ── 月度用量计数（usage_counters，0.0.72 订阅进度条）────────────────────────────

/// 某连接在某 UTC 自然月内累计的原始 token 用量（serde camelCase）。
///
/// 与计价/成本完全无关：这里只累加 RawUsage 的 prompt/completion/total，给订阅套餐进度条用。
/// 无行（该连接该月尚无记录）时各字段为 0。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonthlyUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// 按 (connection_id, ym) 累加一笔月度 token 用量（0.0.72）。
///
/// UPSERT 累加（**不是覆盖**）：首笔 INSERT；后续同 (connection_id, ym) ON CONFLICT 把三项
/// token 各自 += excluded.*，并刷新 updated_at。SQLite INTEGER 为有符号 i64，token 数远不会溢出。
/// 失败由调用方软处理（不阻断 agent 本轮）。
pub fn bump_usage_counter(
    conn: &Connection,
    connection_id: &str,
    ym: &str,
    prompt: u64,
    completion: u64,
    total: u64,
) -> SqlResult<()> {
    let now = now_ts();
    conn.execute(
        "INSERT INTO usage_counters
             (connection_id, ym, prompt_tokens, completion_tokens, total_tokens, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(connection_id, ym) DO UPDATE SET
             prompt_tokens     = prompt_tokens + excluded.prompt_tokens,
             completion_tokens = completion_tokens + excluded.completion_tokens,
             total_tokens      = total_tokens + excluded.total_tokens,
             updated_at        = excluded.updated_at",
        params![connection_id, ym, prompt as i64, completion as i64, total as i64, now],
    )?;
    Ok(())
}

/// 读某连接某 UTC 月的累计用量（0.0.72）。无行返回全 0（[`MonthlyUsage::default`]）。
pub fn get_monthly_usage(
    conn: &Connection,
    connection_id: &str,
    ym: &str,
) -> SqlResult<MonthlyUsage> {
    let mut stmt = conn.prepare(
        "SELECT prompt_tokens, completion_tokens, total_tokens
         FROM usage_counters
         WHERE connection_id = ?1 AND ym = ?2
         LIMIT 1",
    )?;
    let mut rows = stmt.query(params![connection_id, ym])?;
    match rows.next()? {
        Some(row) => Ok(MonthlyUsage {
            prompt_tokens: row.get::<_, i64>(0)? as u64,
            completion_tokens: row.get::<_, i64>(1)? as u64,
            total_tokens: row.get::<_, i64>(2)? as u64,
        }),
        None => Ok(MonthlyUsage::default()),
    }
}

// ── File Checkpoint CRUD ─────────────────────────────────────────────────

/// 记录一次文件变更检查点（写类工具执行成功后调用，保存变更前快照）。
pub fn record_file_checkpoint(
    conn: &Connection,
    conv_id: &str,
    tool_name: &str,
    rel_path: &str,
    prev_content: Option<&str>,
    extra_json: Option<&str>,
    revertible: bool,
) -> SqlResult<FileCheckpoint> {
    let id = Uuid::new_v4().to_string();
    let now = now_ts();
    let seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM file_checkpoints WHERE conversation_id = ?1",
        params![conv_id],
        |row| row.get(0),
    )?;
    conn.execute(
        "INSERT INTO file_checkpoints
         (id, conversation_id, seq, tool_name, rel_path, prev_content, extra_json, revertible, reverted, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9)",
        params![id, conv_id, seq, tool_name, rel_path, prev_content, extra_json, revertible as i64, now],
    )?;
    Ok(FileCheckpoint {
        id,
        conversation_id: conv_id.to_string(),
        seq,
        tool_name: tool_name.to_string(),
        rel_path: rel_path.to_string(),
        prev_content: prev_content.map(str::to_string),
        extra_json: extra_json.map(str::to_string),
        revertible,
        reverted: false,
        created_at: now,
    })
}

/// 查询会话的全部检查点，按序号正序（含已回退的，供 UI 展示状态）。
pub fn list_file_checkpoints(conn: &Connection, conv_id: &str) -> SqlResult<Vec<FileCheckpoint>> {
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, seq, tool_name, rel_path, prev_content, extra_json, revertible, reverted, created_at
         FROM file_checkpoints
         WHERE conversation_id = ?1
         ORDER BY seq ASC",
    )?;
    let rows = stmt.query_map([conv_id], |row| {
        Ok(FileCheckpoint {
            id: row.get(0)?,
            conversation_id: row.get(1)?,
            seq: row.get(2)?,
            tool_name: row.get(3)?,
            rel_path: row.get(4)?,
            prev_content: row.get(5)?,
            extra_json: row.get(6)?,
            revertible: row.get::<_, i64>(7)? != 0,
            reverted: row.get::<_, i64>(8)? != 0,
            created_at: row.get(9)?,
        })
    })?;
    rows.collect()
}

/// 把一个检查点标记为已回退。
pub fn mark_checkpoint_reverted(conn: &Connection, checkpoint_id: &str) -> SqlResult<()> {
    conn.execute(
        "UPDATE file_checkpoints SET reverted = 1 WHERE id = ?1",
        params![checkpoint_id],
    )?;
    Ok(())
}

// ── MCP Server CRUD ──────────────────────────────────────────────────────

/// MCP server 配置记录。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerRecord {
    pub id: String,
    pub name: String,
    /// 启动命令行（stdio）或 http(s):// URL（HTTP 传输）。
    pub command: String,
    pub enabled: bool,
    /// HTTP 传输的静态 Bearer Token（OAuth 取得后也存于此）；None 表示无。
    pub auth_token: Option<String>,
    pub created_at: i64,
}

/// 新增一个 MCP server 配置（可选静态 token）。
pub fn add_mcp_server(
    conn: &Connection,
    name: &str,
    command: &str,
    auth_token: Option<&str>,
) -> SqlResult<McpServerRecord> {
    let id = Uuid::new_v4().to_string();
    let now = now_ts();
    conn.execute(
        "INSERT INTO mcp_servers (id, name, command, enabled, auth_token, created_at) VALUES (?1, ?2, ?3, 1, ?4, ?5)",
        params![id, name, command, auth_token, now],
    )?;
    Ok(McpServerRecord {
        id,
        name: name.to_string(),
        command: command.to_string(),
        enabled: true,
        auth_token: auth_token.map(str::to_string),
        created_at: now,
    })
}

/// 列出全部 MCP server 配置。
pub fn list_mcp_servers(conn: &Connection) -> SqlResult<Vec<McpServerRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, command, enabled, auth_token, created_at FROM mcp_servers ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(McpServerRecord {
            id: row.get(0)?,
            name: row.get(1)?,
            command: row.get(2)?,
            enabled: row.get::<_, i64>(3)? != 0,
            auth_token: row.get(4)?,
            created_at: row.get(5)?,
        })
    })?;
    rows.collect()
}

/// 更新 MCP server 的 auth_token（OAuth 取得 token 后持久化）。
pub fn set_mcp_server_token(conn: &Connection, id: &str, token: &str) -> SqlResult<()> {
    conn.execute(
        "UPDATE mcp_servers SET auth_token = ?1 WHERE id = ?2",
        params![token, id],
    )?;
    Ok(())
}

/// 设置 MCP server 启用状态。
pub fn set_mcp_server_enabled(conn: &Connection, id: &str, enabled: bool) -> SqlResult<()> {
    conn.execute(
        "UPDATE mcp_servers SET enabled = ?1 WHERE id = ?2",
        params![enabled as i64, id],
    )?;
    Ok(())
}

/// 删除一个 MCP server 配置。
pub fn remove_mcp_server(conn: &Connection, id: &str) -> SqlResult<()> {
    conn.execute("DELETE FROM mcp_servers WHERE id = ?1", params![id])?;
    Ok(())
}

// ── Model Provider CRUD ──────────────────────────────────────────────────

/// 模型供应商配置记录（按 role 角色化：main / vision / audio 预留）。
///
/// 一个 provider = 一个 OpenAI 兼容端点的接入参数。base_url 为空时由调用方按 preset
/// 取内置官方端点；非空时为用户自定义覆盖（自托管/代理）。api_key 明文存储（local-first）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProvider {
    pub id: String,
    /// 'main' | 'vision' | 'audio'(预留)。
    pub role: String,
    /// 'deepseek'|'zhipu'|'moonshot'|'qwen'|'custom'，决定官方默认端点。
    pub preset: Option<String>,
    pub label: Option<String>,
    /// 可空：空=用 preset 官方端点；非空=自定义覆盖。
    pub base_url: Option<String>,
    pub api_key: String,
    pub model_id: String,
    /// 视觉 provider 的 API 格式：'openai' | 'anthropic'（Plan18 §4）。主模型恒为 openai 兼容，
    /// 此字段对其无意义；视觉调用据此分支端点/鉴权/消息结构。默认 'openai'。
    pub api_format: String,
    /// 上下文窗口（tokens，可选）：该模型的最大上下文长度，纯用户自定义。
    /// 0.0.61 起：主模型有值时**直接**作为 agent_loop / compaction 的软上限（不再 ×0.8）；
    /// None / 非正值表示**不做窗口驱动的自动压缩**（交端点自身上限兜底，前端 ctx 指示器也随之隐藏）。
    pub context_window: Option<i64>,
    pub enabled: bool,
    pub updated_at: Option<i64>,
}

/// 以 role 为唯一键 upsert 一个 provider（每个角色仅保留一条配置）。
///
/// 输入角色、预设、展示名、base_url（可空）、api_key、model_id；先删同 role 旧记录再插入新记录，
/// 返回完整结构体。这样设置页对某个角色「保存」即覆盖该角色的全部字段，语义直观。
#[allow(clippy::too_many_arguments)]
pub fn upsert_model_provider(
    conn: &Connection,
    role: &str,
    preset: Option<&str>,
    label: Option<&str>,
    base_url: Option<&str>,
    api_key: &str,
    model_id: &str,
    api_format: &str,
    context_window: Option<i64>,
) -> SqlResult<ModelProvider> {
    let id = Uuid::new_v4().to_string();
    let now = now_ts();
    // 归一化 api_format：仅 'anthropic' | 'openai'，未知值落回 'openai'（主模型也走 openai）。
    let api_format = if api_format == "anthropic" { "anthropic" } else { "openai" };
    conn.execute("DELETE FROM model_providers WHERE role = ?1", params![role])?;
    conn.execute(
        "INSERT INTO model_providers
         (id, role, preset, label, base_url, api_key, model_id, api_format, context_window, enabled, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10)",
        params![id, role, preset, label, base_url, api_key, model_id, api_format, context_window, now],
    )?;
    Ok(ModelProvider {
        id,
        role: role.to_string(),
        preset: preset.map(str::to_string),
        label: label.map(str::to_string),
        base_url: base_url.map(str::to_string),
        api_key: api_key.to_string(),
        model_id: model_id.to_string(),
        api_format: api_format.to_string(),
        context_window,
        enabled: true,
        updated_at: Some(now),
    })
}

/// 按角色读取 provider（main / vision），未配置返回 None。
pub fn get_model_provider(conn: &Connection, role: &str) -> SqlResult<Option<ModelProvider>> {
    let mut stmt = conn.prepare(
        "SELECT id, role, preset, label, base_url, api_key, model_id, api_format, context_window, enabled, updated_at
         FROM model_providers
         WHERE role = ?1
         ORDER BY updated_at DESC
         LIMIT 1",
    )?;
    let mut rows = stmt.query([role])?;
    if let Some(row) = rows.next()? {
        Ok(Some(ModelProvider {
            id: row.get(0)?,
            role: row.get(1)?,
            preset: row.get(2)?,
            label: row.get(3)?,
            base_url: row.get(4)?,
            api_key: row.get(5)?,
            model_id: row.get(6)?,
            api_format: row.get(7)?,
            context_window: row.get(8)?,
            enabled: row.get::<_, i64>(9)? != 0,
            updated_at: row.get(10)?,
        }))
    } else {
        Ok(None)
    }
}

/// 列出全部 provider 配置（含全部角色）。
pub fn list_model_providers(conn: &Connection) -> SqlResult<Vec<ModelProvider>> {
    let mut stmt = conn.prepare(
        "SELECT id, role, preset, label, base_url, api_key, model_id, api_format, context_window, enabled, updated_at
         FROM model_providers
         ORDER BY role ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ModelProvider {
            id: row.get(0)?,
            role: row.get(1)?,
            preset: row.get(2)?,
            label: row.get(3)?,
            base_url: row.get(4)?,
            api_key: row.get(5)?,
            model_id: row.get(6)?,
            api_format: row.get(7)?,
            context_window: row.get(8)?,
            enabled: row.get::<_, i64>(9)? != 0,
            updated_at: row.get(10)?,
        })
    })?;
    rows.collect()
}

/// 删除某角色的 provider（如关闭模态扩展时移除 vision provider）。
pub fn delete_model_provider(conn: &Connection, role: &str) -> SqlResult<()> {
    conn.execute("DELETE FROM model_providers WHERE role = ?1", params![role])?;
    Ok(())
}

// ── 角色多模型路由（R8）─────────────────────────────────────────────────────

/// 主模型角色：所有未配置的功能角色（action/plan/critique/subagent/embed）回退到它。
pub const ROLE_MAIN: &str = "main";
/// 行动角色：执行工具的常规循环用此模型（未配置回退 main）。
pub const ROLE_ACTION: &str = "action";
/// 规划角色：计划模式 / 规划步骤用此模型（未配置回退 main）。
pub const ROLE_PLAN: &str = "plan";
/// 评审角色：审查 / 批评步骤用此模型（未配置回退 main）。
pub const ROLE_CRITIQUE: &str = "critique";
/// 视觉角色：识图（已有，独立链路，不参与本回退逻辑）。
pub const ROLE_VISION: &str = "vision";
/// 子代理角色（0.0.59）：run_subtask / 并行子代理用此模型（未配置回退 action → main）。
pub const ROLE_SUBAGENT: &str = "subagent";
/// Embedding 角色（0.0.59）：code_search 可选 embedding 重排用此 provider（未配置回退 main）。
pub const ROLE_EMBED: &str = "embed";

/// 0.0.59 允许配置的全部角色（迁移与命令层据此校验）。
pub const ALL_ROLES: &[&str] = &[
    ROLE_MAIN,
    ROLE_ACTION,
    ROLE_PLAN,
    ROLE_CRITIQUE,
    ROLE_VISION,
    ROLE_SUBAGENT,
    ROLE_EMBED,
];

/// 按角色解析实际生效的 provider，未配置（或被禁用）时回退到主模型 `main`（0.0.59 改写）。
///
/// 0.0.59：数据源从旧的 role-keyed `model_providers` 改为「连接库 + 角色引用」——
/// 解析 role → `role_assignments` 行，再 JOIN `connections` 合成一个 [`ModelProvider`]。
/// **签名与返回的 ModelProvider 形状保持不变**，故 agent_loop / subagent 等运行时消费方零改动。
///
/// 语义（与从前一致的向后兼容保证）：
/// - role == "main"：直接取 main 的引用，不回退（main 没配 → None，调用方报「请先配置主模型」）。
/// - 其它角色：自身有一条 *启用* 的引用则用之；缺失或 enabled=0 → 回退到 main 的引用。
///
/// 返回值：
/// - `Ok(Some(p))`：解析到可用 provider（角色自身的，或回退后的 main）。
/// - `Ok(None)`：角色未配置且连 main 也没配。
pub fn resolve_role_provider(conn: &Connection, role: &str) -> SqlResult<Option<ModelProvider>> {
    // 主角色：直接取 main，不回退。
    if role == ROLE_MAIN {
        return synthesize_role_provider(conn, ROLE_MAIN);
    }
    // 功能角色：自身已配且启用则用之，否则回退 main。
    if let Some(rm) = get_role_model(conn, role)? {
        if rm.enabled {
            if let Some(p) = synthesize_role_provider(conn, role)? {
                return Ok(Some(p));
            }
        }
    }
    synthesize_role_provider(conn, ROLE_MAIN)
}

/// 子代理模型解析（0.0.59）：依次尝试 subagent → action → main，取第一个**已配置且启用**的。
///
/// 默认（subagent / action 均未单独配置）⇒ 落到 main，与「子代理继承 action→main」的旧默认逐字节一致。
/// 仅当用户**显式**为 subagent（或 action）分配了模型时，子代理才用那个模型。返回 None 仅当连 main 都没配。
pub fn resolve_subagent_provider(conn: &Connection) -> SqlResult<Option<ModelProvider>> {
    for role in [ROLE_SUBAGENT, ROLE_ACTION] {
        if let Some(rm) = get_role_model(conn, role)? {
            if rm.enabled {
                if let Some(p) = synthesize_role_provider(conn, role)? {
                    return Ok(Some(p));
                }
            }
        }
    }
    synthesize_role_provider(conn, ROLE_MAIN)
}

/// 把某角色的 `role_models` 行经 `models` 再到 `connections` 合成一个 [`ModelProvider`]（不做回退，0.0.60）。
///
/// 链路：role → role_models[role] → models[model_ref] → connections[model.connection_id]。
/// 任一环缺失（无该角色分配 / model_ref 悬空 / connection 已不存在）→ 返回 None（视为未配置）。
/// 合成出的 ModelProvider.role 即传入的 role；base_url/api_key/api_format 来自 connection，
/// model_id/context_window 来自 model（0.0.60 起 context_window 在 model 粒度）；`enabled` 取 role_models 行。
/// 返回形状与旧 `get_model_provider` 逐字段对齐，故运行时消费方（agent_loop/embedding）零改动。
fn synthesize_role_provider(conn: &Connection, role: &str) -> SqlResult<Option<ModelProvider>> {
    let Some(rm) = get_role_model(conn, role)? else {
        return Ok(None);
    };
    let Some(m) = get_model(conn, &rm.model_ref)? else {
        // model_ref 悬空（model 被删但分配残留，理论上被 delete_model 拦截）：视为未配置。
        return Ok(None);
    };
    let Some(c) = get_connection(conn, &m.connection_id)? else {
        // connection 已不存在（理论上被 delete_connection 级联清理拦截）：视为未配置。
        return Ok(None);
    };
    Ok(Some(ModelProvider {
        id: m.connection_id,
        role: role.to_string(),
        preset: c.preset,
        label: c.label,
        // base_url 原样透传(写入侧 upsert_connection / 0.0.59 迁移已把空白归一为 None,故连接里不会
        // 存纯空白值);不再读侧二次 filter,避免「纯空白→None」与旧 get_model_provider 原样读的字节差异。
        // 下游 resolve_base_url 仍会 trim+filter,空/空白都回退 preset 官方端点,行为一致。
        base_url: c.base_url,
        api_key: c.api_key,
        model_id: m.model_id,
        api_format: c.api_format,
        context_window: m.context_window,
        enabled: rm.enabled,
        updated_at: rm.updated_at,
    }))
}

/// 某角色本轮实际命中的「连接 + 模型」计价上下文（0.0.72）。
///
/// 与 [`resolve_role_provider`] 走**同一条解析 + 回退链**（role → role_models → models →
/// connections，未配置/禁用回退 main），但只携结算需要的字段，不含 api_key：
/// - `billing_mode` / `subscription_json` 来自命中的 **connection**；
/// - `pricing_json` 来自命中的 **model**（前端存的原始 JSON 串，结算侧解析）；
/// - `preset` 来自命中的 connection，供 `pricing_json` 为空且 mode=api 时按预设库回退取价。
///
/// **不含 api_key**：本结构永不返回密钥，可安全用于结算/展示链路。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PricingContext {
    /// 命中连接的计费方式：'api' | 'subscription' | 'none'。
    pub billing_mode: String,
    /// 命中连接的订阅描述（可空）。
    pub subscription_json: Option<String>,
    /// 命中模型的单价 JSON 串（可空，原始串，结算侧解析）。
    pub pricing_json: Option<String>,
    /// 命中连接的 preset（供按预设库回退取价；可空）。
    pub preset: Option<String>,
    /// 命中模型的实际 API 模型串（供按预设库回退匹配）。
    pub model_id: String,
    /// 0.0.72 月度用量：本轮命中连接的 id（供 agent_loop 按连接累计月度 token 用量）。
    /// 纯增量字段，不参与结算（resolve_billing 不读它）。
    pub connection_id: String,
}

/// 按角色解析本轮实际命中的计价上下文，回退语义与 [`resolve_role_provider`] **逐字对齐**（0.0.72）。
///
/// - role == "main"：直接取 main 的分配，不回退（未配置 → None）。
/// - 其它角色：自身有一条 *启用* 的分配则用之，否则回退 main。
///
/// 返回 None 仅当角色未配置且 main 也未配置（与 resolve_role_provider 同口径）。
pub fn resolve_pricing_context(conn: &Connection, role: &str) -> SqlResult<Option<PricingContext>> {
    if role == ROLE_MAIN {
        return synthesize_pricing_context(conn, ROLE_MAIN);
    }
    if let Some(rm) = get_role_model(conn, role)? {
        if rm.enabled {
            if let Some(ctx) = synthesize_pricing_context(conn, role)? {
                return Ok(Some(ctx));
            }
        }
    }
    synthesize_pricing_context(conn, ROLE_MAIN)
}

/// 把某角色的 role_models → models → connections 链合成一个 [`PricingContext`]（不回退）。
/// 任一环缺失 → None（与 [`synthesize_role_provider`] 同口径）。
fn synthesize_pricing_context(
    conn: &Connection,
    role: &str,
) -> SqlResult<Option<PricingContext>> {
    let Some(rm) = get_role_model(conn, role)? else {
        return Ok(None);
    };
    let Some(m) = get_model(conn, &rm.model_ref)? else {
        return Ok(None);
    };
    let Some(c) = get_connection(conn, &m.connection_id)? else {
        return Ok(None);
    };
    Ok(Some(PricingContext {
        billing_mode: c.billing_mode,
        subscription_json: c.subscription_json,
        pricing_json: m.pricing_json,
        preset: c.preset,
        model_id: m.model_id,
        connection_id: m.connection_id,
    }))
}

// ── 连接库（connections）CRUD（0.0.59）──────────────────────────────────────

/// 一份「端点 + 密钥」接入参数（配置一次，可被多个角色引用）。
///
/// base_url 可空：None/空＝走 preset 官方端点；非空＝自定义覆盖。api_key 明文存（local-first），
/// 命令层读时脱敏为空。api_format 仅 'openai' | 'anthropic'，未知值归一为 'openai'。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConnection {
    pub id: String,
    pub label: Option<String>,
    pub preset: Option<String>,
    pub base_url: Option<String>,
    pub api_key: String,
    pub api_format: String,
    /// 0.0.72 计费方式：'api' | 'subscription' | 'none'，默认 'api'。命令层归一化未知值落回 'api'。
    pub billing_mode: String,
    /// 0.0.72 订阅描述（可空）：订阅套餐的自由 JSON 串，仅存储/展示，结算不解析。
    pub subscription_json: Option<String>,
    pub created_at: Option<i64>,
    pub updated_at: Option<i64>,
}

/// 一条「角色 → 模型」纯引用（无密钥；指向某 connection）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleAssignment {
    pub role: String,
    pub connection_id: String,
    pub model_id: String,
    pub context_window: Option<i64>,
    pub enabled: bool,
    pub updated_at: Option<i64>,
}

/// 一条「用户登记的模型」（0.0.60 模型层）：属于某连接，model_id 为实际 API 模型串。
///
/// 一个连接（端点 + 密钥）下可登记多个模型（同一把 key 同时跑 pro 与 flash）。
/// `label` 可选展示名；`context_window` 为该模型粒度的上下文窗口（从旧 role_assignments 下沉到此）。
/// 同一连接下 (connection_id, model_id) 唯一。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CuratedModel {
    pub id: String,
    pub connection_id: String,
    pub model_id: String,
    pub label: Option<String>,
    pub context_window: Option<i64>,
    /// 0.0.72 单价快照（可空）：前端存「价格字段 + `_` 前缀元数据(_source/_confidence/_needsVerify/
    /// _sourceUrl)」的 JSON 串。后端**原样存取**，不解析校验元数据；仅结算时以
    /// `serde_json::from_str::<ModelPricing>()` 取价格字段（serde 默认忽略多余字段）。
    pub pricing_json: Option<String>,
    pub created_at: Option<i64>,
    pub updated_at: Option<i64>,
}

/// 一条「角色 → curated model」分配（0.0.60 真源）：model_ref 指向 models.id。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleModel {
    pub role: String,
    pub model_ref: String,
    pub enabled: bool,
    pub updated_at: Option<i64>,
}

/// 归一化 api_format：仅 'anthropic' | 'openai'，未知值落回 'openai'。
fn normalize_api_format(api_format: &str) -> &'static str {
    if api_format == "anthropic" {
        "anthropic"
    } else {
        "openai"
    }
}

/// 列出全部连接，按创建时间正序。
pub fn list_connections(conn: &Connection) -> SqlResult<Vec<ProviderConnection>> {
    let mut stmt = conn.prepare(
        "SELECT id, label, preset, base_url, api_key, api_format, billing_mode, subscription_json, created_at, updated_at
         FROM connections
         ORDER BY created_at ASC, id ASC",
    )?;
    let rows = stmt.query_map([], row_to_connection)?;
    rows.collect()
}

/// 按 id 读取单个连接，未找到返回 None。
pub fn get_connection(conn: &Connection, id: &str) -> SqlResult<Option<ProviderConnection>> {
    let mut stmt = conn.prepare(
        "SELECT id, label, preset, base_url, api_key, api_format, billing_mode, subscription_json, created_at, updated_at
         FROM connections
         WHERE id = ?1
         LIMIT 1",
    )?;
    let mut rows = stmt.query([id])?;
    match rows.next()? {
        Some(row) => row_to_connection(row).map(Some),
        None => Ok(None),
    }
}

fn row_to_connection(row: &rusqlite::Row) -> SqlResult<ProviderConnection> {
    Ok(ProviderConnection {
        id: row.get(0)?,
        label: row.get(1)?,
        preset: row.get(2)?,
        base_url: row.get(3)?,
        api_key: row.get(4)?,
        api_format: row.get(5)?,
        billing_mode: row.get(6)?,
        subscription_json: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

/// 归一化计费方式：仅 'api' | 'subscription' | 'none'，未知值落回 'api'。
fn normalize_billing_mode(billing_mode: &str) -> &'static str {
    match billing_mode.trim() {
        "subscription" => "subscription",
        "none" => "none",
        _ => "api",
    }
}

/// 新建或更新一个连接。
///
/// - `id` 为空：创建一条新连接（生成新 uuid）。
/// - `id` 非空：更新已存在的该连接（id 不存在则报错）。
/// - 更新时 `api_key` 为空串：保留原 key（不覆盖为空，避免「不重输 key 再保存」清掉凭据）。
///
/// base_url 空串归一为 NULL（走 preset 官方端点）。api_format 归一为 openai|anthropic。
/// 返回写入后的完整 [`ProviderConnection`]（含其 api_key 明文——命令层负责脱敏，storage 不脱敏）。
#[allow(clippy::too_many_arguments)]
pub fn upsert_connection(
    conn: &Connection,
    id: &str,
    label: Option<&str>,
    preset: Option<&str>,
    base_url: Option<&str>,
    api_key: &str,
    api_format: &str,
) -> SqlResult<ProviderConnection> {
    let now = now_ts();
    let api_format = normalize_api_format(api_format);
    let base_url = base_url.map(str::trim).filter(|s| !s.is_empty());
    if id.trim().is_empty() {
        // 创建：服务端兜底校验——新建连接必须带非空 key（前端表单已挡一道，这里防御性再挡，
        // 避免任何调用方建出无密钥的死连接）。更新路径的空 key=保留旧 key 语义不受影响。
        if api_key.trim().is_empty() {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                Some("新建连接必须填写 API Key".to_string()),
            ));
        }
        let new_id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO connections
             (id, label, preset, base_url, api_key, api_format, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![new_id, label, preset, base_url, api_key, api_format, now],
        )?;
        return get_connection(conn, &new_id)?.ok_or(rusqlite::Error::QueryReturnedNoRows);
    }
    // 更新：先确认存在，再决定是否保留旧 key。
    let existing = get_connection(conn, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    let api_key = if api_key.trim().is_empty() {
        existing.api_key
    } else {
        api_key.to_string()
    };
    conn.execute(
        "UPDATE connections
         SET label = ?2, preset = ?3, base_url = ?4, api_key = ?5, api_format = ?6, updated_at = ?7
         WHERE id = ?1",
        params![id, label, preset, base_url, api_key, api_format, now],
    )?;
    get_connection(conn, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

/// 设置某连接的计费方式（0.0.72）。
///
/// `billing_mode` 归一化为 'api' | 'subscription' | 'none'（未知落回 'api'）；`subscription_json`
/// 为订阅描述自由串（可空，原样存）。连接不存在则报错。不触碰 api_key 等其它字段。
/// 返回写入后的完整 [`ProviderConnection`]（含 api_key 明文——命令层负责脱敏）。
pub fn set_connection_billing(
    conn: &Connection,
    id: &str,
    billing_mode: &str,
    subscription_json: Option<&str>,
) -> SqlResult<ProviderConnection> {
    // 先确认连接存在（不存在则报错，避免静默 no-op）。
    get_connection(conn, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    let mode = normalize_billing_mode(billing_mode);
    let now = now_ts();
    conn.execute(
        "UPDATE connections
         SET billing_mode = ?2, subscription_json = ?3, updated_at = ?4
         WHERE id = ?1",
        params![id, mode, subscription_json, now],
    )?;
    get_connection(conn, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

/// 返回引用了某连接「旗下任一模型」的角色名列表（0.0.60：经 role_models → models → connection 反查）。
///
/// 供「拒删被引用连接」判断与人话化错误。结果按角色名去重排序。
pub fn connection_referenced_by(conn: &Connection, id: &str) -> SqlResult<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT rm.role
         FROM role_models rm
         JOIN models m ON m.id = rm.model_ref
         WHERE m.connection_id = ?1
         ORDER BY rm.role ASC",
    )?;
    let rows = stmt.query_map([id], |row| row.get::<_, String>(0))?;
    rows.collect()
}

/// 删除一个连接（0.0.60）。若该连接旗下任一模型仍被某角色（role_models）引用，拒绝删除并返回 Err；
/// 否则删除连接，并级联删除其（此时已无人引用的）模型行。
pub fn delete_connection(conn: &Connection, id: &str) -> SqlResult<()> {
    let refs = connection_referenced_by(conn, id)?;
    if !refs.is_empty() {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!(
                "该连接下的模型仍被某些角色引用：{}，请先改这些角色的分配",
                refs.join("、")
            )),
        ));
    }
    // 级联删旗下模型（此时均无 role_models 引用），再删连接。
    conn.execute("DELETE FROM models WHERE connection_id = ?1", params![id])?;
    conn.execute("DELETE FROM connections WHERE id = ?1", params![id])?;
    Ok(())
}

/// 强制级联删除一个连接（0.0.62）：删除连接、其旗下全部模型，以及任何指向这些模型的角色分配。
///
/// 与拒绝式 [`delete_connection`] 不同——本函数**不拒绝**被引用的连接，而是把被波及的角色分配
/// 一并清掉（**含 `main`**：直接 SQL DELETE role_models 行，不走 [`delete_role_model`]——后者拒删 main）。
/// 清掉 main 的分配后 main 变为「未配置」，`resolve_role_provider(main)` 返回 None，由 app 既有的
/// 「请先配置主模型」处理接管（这是预期行为，给「唯一连接被 main+其它角色占用」的用户一条删除出路）。
///
/// 返回被本次级联**解除分配**的角色名（去重排序）；前端据此提示「这些角色将被取消分配」。
/// 整个操作在**单事务**内（BEGIN/COMMIT，出错 ROLLBACK），保证无半删状态：要么连接 + 模型 +
/// 角色分配一起消失，要么全不动。
pub fn delete_connection_cascade(conn: &Connection, id: &str) -> SqlResult<Vec<String>> {
    // 先算出受影响角色（即返回值）；与事务内 DELETE 用同一连接条件，读写口径一致。
    let affected = connection_referenced_by(conn, id)?;

    conn.execute_batch("BEGIN")?;
    let result = (|| -> SqlResult<()> {
        // (a→b) 删掉所有指向「本连接旗下模型」的角色分配（含 main，直接 SQL，不经 delete_role_model）。
        conn.execute(
            "DELETE FROM role_models
             WHERE model_ref IN (SELECT id FROM models WHERE connection_id = ?1)",
            params![id],
        )?;
        // (c) 删本连接旗下全部模型。
        conn.execute("DELETE FROM models WHERE connection_id = ?1", params![id])?;
        // (d) 删连接本身。
        conn.execute("DELETE FROM connections WHERE id = ?1", params![id])?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(affected)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

// ── 角色引用（role_assignments）CRUD（0.0.59）───────────────────────────────

/// 列出全部角色引用。
pub fn get_role_assignments(conn: &Connection) -> SqlResult<Vec<RoleAssignment>> {
    let mut stmt = conn.prepare(
        "SELECT role, connection_id, model_id, context_window, enabled, updated_at
         FROM role_assignments
         ORDER BY role ASC",
    )?;
    let rows = stmt.query_map([], row_to_role_assignment)?;
    rows.collect()
}

/// 读取某角色的引用，未配置返回 None。
pub fn get_role_assignment(conn: &Connection, role: &str) -> SqlResult<Option<RoleAssignment>> {
    let mut stmt = conn.prepare(
        "SELECT role, connection_id, model_id, context_window, enabled, updated_at
         FROM role_assignments
         WHERE role = ?1
         LIMIT 1",
    )?;
    let mut rows = stmt.query([role])?;
    match rows.next()? {
        Some(row) => row_to_role_assignment(row).map(Some),
        None => Ok(None),
    }
}

fn row_to_role_assignment(row: &rusqlite::Row) -> SqlResult<RoleAssignment> {
    Ok(RoleAssignment {
        role: row.get(0)?,
        connection_id: row.get(1)?,
        model_id: row.get(2)?,
        context_window: row.get(3)?,
        enabled: row.get::<_, i64>(4)? != 0,
        updated_at: row.get(5)?,
    })
}

/// 新建或覆盖某角色的引用（role 为主键，UPSERT 覆盖）。context_window 非正值归一为 None。
pub fn upsert_role_assignment(
    conn: &Connection,
    role: &str,
    connection_id: &str,
    model_id: &str,
    context_window: Option<i64>,
    enabled: bool,
) -> SqlResult<RoleAssignment> {
    let now = now_ts();
    let context_window = context_window.filter(|&cw| cw > 0);
    conn.execute(
        "INSERT INTO role_assignments
         (role, connection_id, model_id, context_window, enabled, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(role) DO UPDATE SET
             connection_id = excluded.connection_id,
             model_id = excluded.model_id,
             context_window = excluded.context_window,
             enabled = excluded.enabled,
             updated_at = excluded.updated_at",
        params![role, connection_id, model_id, context_window, enabled as i64, now],
    )?;
    get_role_assignment(conn, role)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

/// 删除某角色的引用（使其回退到 main）。拒绝删除 role==main（main 不可清）。
pub fn delete_role_assignment(conn: &Connection, role: &str) -> SqlResult<()> {
    if role == ROLE_MAIN {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some("不能清除主模型（main）".to_string()),
        ));
    }
    conn.execute("DELETE FROM role_assignments WHERE role = ?1", params![role])?;
    Ok(())
}

// ── 模型层（models）CRUD（0.0.60）───────────────────────────────────────────

fn row_to_model(row: &rusqlite::Row) -> SqlResult<CuratedModel> {
    Ok(CuratedModel {
        id: row.get(0)?,
        connection_id: row.get(1)?,
        model_id: row.get(2)?,
        label: row.get(3)?,
        context_window: row.get(4)?,
        pricing_json: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

/// 列出全部登记模型（跨所有连接），按连接 + 创建时间正序。
pub fn list_models(conn: &Connection) -> SqlResult<Vec<CuratedModel>> {
    let mut stmt = conn.prepare(
        "SELECT id, connection_id, model_id, label, context_window, pricing_json, created_at, updated_at
         FROM models
         ORDER BY connection_id ASC, created_at ASC, id ASC",
    )?;
    let rows = stmt.query_map([], row_to_model)?;
    rows.collect()
}

/// 列出某连接下用户登记的全部模型（0.0.60：curated 列表，**非**硬编码预设清单）。
pub fn list_models_for_connection(
    conn: &Connection,
    connection_id: &str,
) -> SqlResult<Vec<CuratedModel>> {
    let mut stmt = conn.prepare(
        "SELECT id, connection_id, model_id, label, context_window, pricing_json, created_at, updated_at
         FROM models
         WHERE connection_id = ?1
         ORDER BY created_at ASC, id ASC",
    )?;
    let rows = stmt.query_map([connection_id], row_to_model)?;
    rows.collect()
}

/// 按 id 读取一个模型，未找到返回 None。
pub fn get_model(conn: &Connection, id: &str) -> SqlResult<Option<CuratedModel>> {
    let mut stmt = conn.prepare(
        "SELECT id, connection_id, model_id, label, context_window, pricing_json, created_at, updated_at
         FROM models
         WHERE id = ?1
         LIMIT 1",
    )?;
    let mut rows = stmt.query([id])?;
    match rows.next()? {
        Some(row) => row_to_model(row).map(Some),
        None => Ok(None),
    }
}

/// 按 (connection_id, model_id) 查一个已存模型的 id（dedup 用）。
fn find_model_id(
    conn: &Connection,
    connection_id: &str,
    model_id: &str,
) -> SqlResult<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM models WHERE connection_id = ?1 AND model_id = ?2 LIMIT 1",
    )?;
    let mut rows = stmt.query(params![connection_id, model_id])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

/// 新建或更新一个模型（0.0.60）。
///
/// - `id` 空：创建。但若同 (connection_id, model_id) 已存在一行，则改为 **更新那一行**
///   （刷新 label / context_window）而非插重复，与 UNIQUE 约束语义一致。
/// - `id` 非空：更新该 id 的行（id 不存在则报错）。
///
/// context_window 非正值归一为 None。label 空白归一为 None。返回写入后的完整 [`CuratedModel`]。
pub fn upsert_model(
    conn: &Connection,
    id: &str,
    connection_id: &str,
    model_id: &str,
    label: Option<&str>,
    context_window: Option<i64>,
) -> SqlResult<CuratedModel> {
    let now = now_ts();
    let model_id = model_id.trim();
    let label = label.map(str::trim).filter(|s| !s.is_empty());
    let context_window = context_window.filter(|&cw| cw > 0);

    // 决定要更新的 id：显式 id 优先；否则按 (connection, model_id) dedup 命中已存行；都没有则新建。
    let target_id = if !id.trim().is_empty() {
        // 确认该 id 存在。
        get_model(conn, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        Some(id.to_string())
    } else {
        find_model_id(conn, connection_id, model_id)?
    };

    match target_id {
        Some(existing_id) => {
            // 更新：connection_id 与 model_id 也一并写（允许改 id 路径下重新指定，但通常不变）。
            conn.execute(
                "UPDATE models
                 SET connection_id = ?2, model_id = ?3, label = ?4, context_window = ?5, updated_at = ?6
                 WHERE id = ?1",
                params![existing_id, connection_id, model_id, label, context_window, now],
            )?;
            get_model(conn, &existing_id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
        }
        None => {
            let new_id = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO models
                 (id, connection_id, model_id, label, context_window, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![new_id, connection_id, model_id, label, context_window, now],
            )?;
            get_model(conn, &new_id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
        }
    }
}

/// 设置某模型的单价快照（0.0.72）。`pricing_json` 为前端构造的 JSON 串（价格字段 + `_` 前缀元数据），
/// 后端**原样存取**，不解析校验；传 `None` 清空（恢复「无单价」）。模型不存在则报错。
pub fn set_model_pricing(
    conn: &Connection,
    id: &str,
    pricing_json: Option<&str>,
) -> SqlResult<CuratedModel> {
    // 先确认模型存在（不存在则报错，避免静默 no-op）。
    get_model(conn, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    let now = now_ts();
    conn.execute(
        "UPDATE models SET pricing_json = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, pricing_json, now],
    )?;
    get_model(conn, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

/// 返回引用了某模型的角色名列表（供「拒删被引用模型」判断与人话化错误）。
pub fn model_referenced_by(conn: &Connection, id: &str) -> SqlResult<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT role FROM role_models WHERE model_ref = ?1 ORDER BY role ASC")?;
    let rows = stmt.query_map([id], |row| row.get::<_, String>(0))?;
    rows.collect()
}

/// 删除一个模型。若仍被任意角色（role_models）引用，拒绝删除并返回 Err（与 delete_connection 同款）。
pub fn delete_model(conn: &Connection, id: &str) -> SqlResult<()> {
    let refs = model_referenced_by(conn, id)?;
    if !refs.is_empty() {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!("该模型仍被某些角色引用：{}", refs.join("、"))),
        ));
    }
    conn.execute("DELETE FROM models WHERE id = ?1", params![id])?;
    Ok(())
}

/// 强制级联删除一个模型（0.0.62）：删除该模型，以及任何指向它的角色分配。
///
/// 与拒绝式 [`delete_model`] 不同——本函数**不拒绝**被引用的模型，而是把指向它的角色分配一并清掉
/// （**含 `main`**：直接 SQL DELETE role_models 行，不走拒删 main 的 [`delete_role_model`]）。清掉 main
/// 后 main 变「未配置」、`resolve_role_provider(main)` 返回 None，交 app 既有「请先配置主模型」处理（预期）。
///
/// 返回被解除分配的角色名（去重排序）。整个操作在**单事务**内（出错 ROLLBACK），无半删状态。
pub fn delete_model_cascade(conn: &Connection, id: &str) -> SqlResult<Vec<String>> {
    // 受影响角色（即返回值）；与事务内 DELETE 用同一 model_ref 条件，读写口径一致。
    let affected = model_referenced_by(conn, id)?;

    conn.execute_batch("BEGIN")?;
    let result = (|| -> SqlResult<()> {
        // 删指向本模型的全部角色分配（含 main，直接 SQL，不经 delete_role_model）。
        conn.execute("DELETE FROM role_models WHERE model_ref = ?1", params![id])?;
        // 删模型本身。
        conn.execute("DELETE FROM models WHERE id = ?1", params![id])?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(affected)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

// ── 角色分配（role_models）CRUD（0.0.60，运行时真源）───────────────────────────

fn row_to_role_model(row: &rusqlite::Row) -> SqlResult<RoleModel> {
    Ok(RoleModel {
        role: row.get(0)?,
        model_ref: row.get(1)?,
        enabled: row.get::<_, i64>(2)? != 0,
        updated_at: row.get(3)?,
    })
}

/// 列出全部角色分配。
pub fn get_role_models(conn: &Connection) -> SqlResult<Vec<RoleModel>> {
    let mut stmt = conn.prepare(
        "SELECT role, model_ref, enabled, updated_at
         FROM role_models
         ORDER BY role ASC",
    )?;
    let rows = stmt.query_map([], row_to_role_model)?;
    rows.collect()
}

/// 读取某角色的分配，未配置返回 None。
pub fn get_role_model(conn: &Connection, role: &str) -> SqlResult<Option<RoleModel>> {
    let mut stmt = conn.prepare(
        "SELECT role, model_ref, enabled, updated_at
         FROM role_models
         WHERE role = ?1
         LIMIT 1",
    )?;
    let mut rows = stmt.query([role])?;
    match rows.next()? {
        Some(row) => row_to_role_model(row).map(Some),
        None => Ok(None),
    }
}

/// 新建或覆盖某角色的分配（role 为主键，UPSERT 覆盖）。校验 model_ref 指向一个真实存在的模型。
pub fn upsert_role_model(
    conn: &Connection,
    role: &str,
    model_ref: &str,
    enabled: bool,
) -> SqlResult<RoleModel> {
    // 校验 model_ref 存在，避免建出悬空分配。
    if get_model(conn, model_ref)?.is_none() {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some("所选模型不存在，请先登记模型".to_string()),
        ));
    }
    let now = now_ts();
    conn.execute(
        "INSERT INTO role_models (role, model_ref, enabled, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(role) DO UPDATE SET
             model_ref = excluded.model_ref,
             enabled = excluded.enabled,
             updated_at = excluded.updated_at",
        params![role, model_ref, enabled as i64, now],
    )?;
    get_role_model(conn, role)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

/// 删除某角色的分配（使其回退到 main）。拒绝删除 role==main（main 不可清）。
pub fn delete_role_model(conn: &Connection, role: &str) -> SqlResult<()> {
    if role == ROLE_MAIN {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some("不能清除主模型（main）".to_string()),
        ));
    }
    conn.execute("DELETE FROM role_models WHERE role = ?1", params![role])?;
    Ok(())
}

// ── 0.0.60 数据迁移：role_assignments → models + role_models ──────────────────

/// 把 0.0.59 的 role_assignments 升级为「模型层」：去重出 models，并把角色改指 role_models。
///
/// **一次性、幂等**——可在每次 `init_db` 调用，但只在「`role_models` 为空 且 `role_assignments`
/// 非空」时才真正搬数据。门保证：已迁库（role_models 非空）跳过；全新库（role_assignments 为空）
/// 也跳过——此时 models / role_models 由 `CREATE TABLE IF NOT EXISTS` 建出但保持为空，用户从新表配。
///
/// dedup：对每条 role_assignments 行，按 (connection_id, model_id) 找/建一条 models 行——
/// 同一 (连接, 模型) 跨多个角色只产生**一条** model（carry context_window 到 model；label = None 不设别名）。
/// 然后为该角色 upsert 一条 role_models 指向该 model。
///
/// 回滚：整个搬运在**单事务**内（出错即 ROLLBACK），且**绝不** drop/alter role_assignments /
/// model_providers——它们原封保留为惰性回滚源（与 0.0.59 同纪律，不另落明文密钥文件备份）。
fn migrate_to_models_layer_0060(conn: &Connection) -> SqlResult<()> {
    // 门 1：已有任意角色分配 ⇒ 已迁过（或用户已用新表配置），不再迁。
    let role_model_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM role_models", [], |r| r.get(0))?;
    if role_model_count > 0 {
        return Ok(());
    }
    // 门 2：旧 role_assignments 无行 ⇒ 全新库 / 无可迁数据，无需迁移。
    let assignments = get_role_assignments(conn)?;
    if assignments.is_empty() {
        return Ok(());
    }

    // 在单事务里做完 dedup + 写入，避免半迁状态。
    conn.execute_batch("BEGIN")?;
    let result = (|| -> SqlResult<()> {
        for a in &assignments {
            // dedup：同 (connection_id, model_id) 复用一条 model；首次创建时 carry context_window，
            // label = None 不设别名（0.0.61：避免 UI 把 model_id 渲染两次）。新模型「上下文窗口」是
            // per-model 属性,而旧库可能给同一 (连接,模型)
            // 的不同角色配了不一致的 context_window;迭代是 role ASC,"action"<"main" 会让 action 先建、
            // main 后复用 → 若不处理就丢掉 main 的窗口。而 main 的窗口**驱动压缩软上限**(0.0.61：agent_loop
            // 直接以 main 的 context_window 为压缩点,不再 ×0.8),丢失会导致迁移后压缩点漂移(过早/过晚压缩)。
            // 故:**main 角色复用已建 model 时,以 main 的窗口为准覆盖**(main 权威),保证 main 窗口不丢。
            let model_ref = match find_model_id(conn, &a.connection_id, &a.model_id)? {
                Some(existing) => {
                    if a.role == ROLE_MAIN && a.context_window.is_some() {
                        conn.execute(
                            "UPDATE models SET context_window = ?2, updated_at = ?3 WHERE id = ?1",
                            params![existing, a.context_window, now_ts()],
                        )?;
                    }
                    existing
                }
                None => {
                    // 0.0.61：迁移出的 model 不设别名（label = None）。此前默认 label = model_id 会让 UI
                    // 把模型 id 渲染两次（别名行与 id 行重复）；None ⇒ UI 仅显示 model_id 一次。
                    let created = upsert_model(
                        conn,
                        "",
                        &a.connection_id,
                        &a.model_id,
                        None,
                        a.context_window,
                    )?;
                    created.id
                }
            };
            upsert_role_model(conn, &a.role, &model_ref, a.enabled)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

// ── 0.0.59 数据迁移：model_providers → connections + role_assignments ────────

/// 把旧的 role-keyed `model_providers` 迁到「连接库 + 角色引用」。
///
/// **一次性、幂等**——本函数可在每次 `init_db` 调用，但只在「`role_assignments` 为空
/// 且 `model_providers` 有行」时才真正搬数据；搬完后 `role_assignments` 非空，下次调用即 no-op。
/// 这个「门」既保证不重复迁移、不重复 dedup，也使全新库（无旧行）与已迁库都安全跳过。
///
/// 回滚路径：整个搬运在**单事务**内完成（出错即 ROLLBACK，无半迁状态），且**绝不** drop / alter
/// `model_providers`——旧表原封不动地保留为「事务化、一致的」回滚快照。故**不再额外落一份明文
/// 密钥文件备份**（旧的 `.pre-0059.bak` 方案在 WAL 下可能陈旧/不一致，且会把全部 API Key 再留一份
/// 明文在磁盘上——既不可靠又是泄漏面；保留旧表已是更稳的回滚源）。
///
/// dedup：对每个旧行，按 (preset, COALESCE(base_url,''), api_key, api_format) 去重找/建 connection——
/// 三元组相同的多个角色共用同一条 connection（同一把 DeepSeek key 被 3 个角色用 ⇒ 1 个连接）。
/// 然后为该角色插入一条 role_assignment 指向该 connection。
fn migrate_to_connections_0059(conn: &Connection) -> SqlResult<()> {
    // 门：已有任意角色引用 ⇒ 已迁过（或用户已用新表配置），不再迁。
    let assignment_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM role_assignments", [], |r| r.get(0))?;
    if assignment_count > 0 {
        return Ok(());
    }
    // 旧表无行 ⇒ 全新库，无需迁移。
    let mut old_rows = list_model_providers(conn)?;
    if old_rows.is_empty() {
        return Ok(());
    }
    // 确定性：按 updated_at 升序处理，使「同 role 的多行」中**最新一行最后写入** → ON CONFLICT(role)
    // 覆盖后最新者胜出,与旧解析器 `WHERE role=? ORDER BY updated_at DESC LIMIT 1`(最新者胜)一致。
    // 正常路径不会出现同 role 多行(upsert 先删后插),这是对历史脏数据的防御性确定化。
    old_rows.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));

    // 在单事务里做完 dedup + 写入，避免半迁状态。
    conn.execute_batch("BEGIN")?;
    let result = (|| -> SqlResult<()> {
        for row in &old_rows {
            let preset = row.preset.as_deref();
            // base_url 归一：NULL / 空 / 纯空白都视为「走 preset 官方端点」(=None)；非空值**原样保留
            // 不 trim**,与旧写入/读取路径逐字节一致(旧 save 仅 filter 空白、不改非空内容)。
            let base_url = row.base_url.as_deref().filter(|s| !s.trim().is_empty());
            let api_format = normalize_api_format(&row.api_format);
            // dedup 找已有 connection（按三元组 + preset + format）。
            let existing = find_connection_for_triple(
                conn,
                preset,
                base_url.unwrap_or(""),
                &row.api_key,
                api_format,
            )?;
            let connection_id = match existing {
                Some(id) => id,
                None => {
                    // label 取旧行 label，缺失回退 preset。
                    let label = row
                        .label
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .or(preset);
                    let created = upsert_connection(
                        conn, "", label, preset, base_url, &row.api_key, api_format,
                    )?;
                    created.id
                }
            };
            // 为该角色建引用（enabled 沿用旧行）。
            upsert_role_assignment(
                conn,
                &row.role,
                &connection_id,
                &row.model_id,
                row.context_window,
                row.enabled,
            )?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// dedup 辅助：按 (preset, base_url, api_key, api_format) 找一条已存 connection 的 id。
/// base_url 用 COALESCE(base_url,'') 归一比较（NULL 与空串等价）；preset 用 IS 比较（含 NULL）。
fn find_connection_for_triple(
    conn: &Connection,
    preset: Option<&str>,
    base_url: &str,
    api_key: &str,
    api_format: &str,
) -> SqlResult<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM connections
         WHERE preset IS ?1
           AND COALESCE(base_url, '') = ?2
           AND api_key = ?3
           AND api_format = ?4
         LIMIT 1",
    )?;
    let mut rows = stmt.query(params![preset, base_url, api_key, api_format])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

// ── App Settings (key-value) ──────────────────────────────────────────────

/// 读取一项应用设置（如 modality_extended 模态扩展开关），不存在返回 None。
pub fn get_setting(conn: &Connection, key: &str) -> SqlResult<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM app_settings WHERE key = ?1 LIMIT 1")?;
    let mut rows = stmt.query([key])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row.get(0)?))
    } else {
        Ok(None)
    }
}

/// 写入一项应用设置（upsert）。
pub fn set_setting(conn: &Connection, key: &str, value: &str) -> SqlResult<()> {
    conn.execute(
        "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

// ── LSP 服务器注册表配置（R-uicfg / 0.0.57）──────────────────────────────────
//
// 持久化形态：直接把整份配置序列化为 JSON，存进 app_settings 的单行（key = LSP_SERVER_CONFIG_KEY）。
// 选 KV-JSON 而非新建表：配置是一份「按已知种类查表的稀疏覆盖」，整存整取最简单，无需新增 schema 迁移。
// 安全：这里只负责存/取**字符串 JSON**；其语义（只能调节已知服务器的启用/路径、命令身份恒为常量）
// 由 mdga-lsp::LspServerConfig 在解析与使用时强制。storage 不感知具体结构，保持与 lsp crate 解耦。

/// LSP 服务器配置在 app_settings 里的键名。
pub const LSP_SERVER_CONFIG_KEY: &str = "lsp_server_config";

/// 读取 LSP 服务器配置原始 JSON（未配置返回 None，调用方据此回退默认「全部启用、走 PATH」）。
pub fn get_lsp_server_config_json(conn: &Connection) -> SqlResult<Option<String>> {
    get_setting(conn, LSP_SERVER_CONFIG_KEY)
}

/// 写入 LSP 服务器配置原始 JSON（upsert）。JSON 的结构合法性由调用方（命令层）先行校验。
pub fn set_lsp_server_config_json(conn: &Connection, json: &str) -> SqlResult<()> {
    set_setting(conn, LSP_SERVER_CONFIG_KEY, json)
}

// ── Permission Rule CRUD ─────────────────────────────────────────────────

/// 保存一条「总是允许」权限规则（如 `cmd:git push`、`tool:write_file`）；重复插入幂等。
pub fn add_permission_rule(conn: &Connection, rule: &str) -> SqlResult<()> {
    conn.execute(
        "INSERT OR IGNORE INTO permission_rules (id, rule, created_at) VALUES (?1, ?2, ?3)",
        params![Uuid::new_v4().to_string(), rule, now_ts()],
    )?;
    Ok(())
}

/// 读取全部权限规则。
pub fn list_permission_rules(conn: &Connection) -> SqlResult<Vec<String>> {
    let mut stmt = conn.prepare("SELECT rule FROM permission_rules ORDER BY created_at ASC")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect()
}

/// 删除一条权限规则。
pub fn remove_permission_rule(conn: &Connection, rule: &str) -> SqlResult<()> {
    conn.execute("DELETE FROM permission_rules WHERE rule = ?1", params![rule])?;
    Ok(())
}

// ── Workspace CRUD ────────────────────────────────────────────────────────

/// 保存当前活动工作区。
///
/// 输入数据库连接和用户授权目录路径；输出新的 Workspace 记录。MVP 阶段只保留一个活动工作区，
/// 写入新工作区前会清除旧记录，后续项目列表能力成熟后再扩展为多工作区。
pub fn save_active_workspace(conn: &Connection, path: &str) -> SqlResult<Workspace> {
    let id = Uuid::new_v4().to_string();
    let now = now_ts();
    let name = workspace_name_from_path(path);

    conn.execute("DELETE FROM workspaces", [])?;
    conn.execute(
        "INSERT INTO workspaces (id, name, path, created_at, updated_at, active)
         VALUES (?1, ?2, ?3, ?4, ?4, 1)",
        params![id, name, path, now],
    )?;

    Ok(Workspace {
        id,
        name,
        path: path.to_string(),
        created_at: now,
        updated_at: now,
        active: true,
    })
}

/// 读取当前活动工作区。
///
/// 输入数据库连接；如果用户已绑定工作区，返回 Workspace，否则返回 None。
pub fn get_active_workspace(conn: &Connection) -> SqlResult<Option<Workspace>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, path, created_at, updated_at, active
         FROM workspaces
         WHERE active = 1
         ORDER BY updated_at DESC
         LIMIT 1",
    )?;
    let mut rows = stmt.query([])?;

    if let Some(row) = rows.next()? {
        Ok(Some(Workspace {
            id: row.get(0)?,
            name: row.get(1)?,
            path: row.get(2)?,
            created_at: row.get(3)?,
            updated_at: row.get(4)?,
            active: row.get::<_, i64>(5)? == 1,
        }))
    } else {
        Ok(None)
    }
}

/// 清除当前活动工作区绑定。
///
/// 输入数据库连接；删除 MVP 阶段的工作区记录，用于用户撤销当前授权目录。
pub fn clear_active_workspace(conn: &Connection) -> SqlResult<()> {
    conn.execute("DELETE FROM workspaces WHERE active = 1", [])?;
    Ok(())
}

fn workspace_name_from_path(path: &str) -> String {
    // 从路径末尾提取目录名，提取失败时退回完整路径，避免 UI 出现空标题。
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(path)
        .to_string()
}

// ── Activity Event ────────────────────────────────────────────────────────

/// 记录一条 Activity Event。
///
/// 输入会话 ID、事件类型、状态及可选的工具名、输入/输出 JSON、错误信息和工作区快照；
/// 写入 activity_events 表用于审计与前端折叠过程展示。本方法不做权限判断。
#[allow(clippy::too_many_arguments)]
pub fn record_activity_event(
    conn: &Connection,
    conversation_id: &str,
    event_type: &str,
    tool_name: Option<&str>,
    status: &str,
    input_json: Option<&str>,
    output_json: Option<&str>,
    error_message: Option<&str>,
    workspace_path: Option<&str>,
) -> SqlResult<ActivityEventRecord> {
    let id = Uuid::new_v4().to_string();
    let now = now_ts();
    conn.execute(
        "INSERT INTO activity_events
         (id, conversation_id, event_type, tool_name, status, input_json,
          output_json, error_message, workspace_path, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            id,
            conversation_id,
            event_type,
            tool_name,
            status,
            input_json,
            output_json,
            error_message,
            workspace_path,
            now
        ],
    )?;
    Ok(ActivityEventRecord {
        id,
        conversation_id: conversation_id.to_string(),
        event_type: event_type.to_string(),
        tool_name: tool_name.map(str::to_string),
        status: status.to_string(),
        input_json: input_json.map(str::to_string),
        output_json: output_json.map(str::to_string),
        error_message: error_message.map(str::to_string),
        workspace_path: workspace_path.map(str::to_string),
        created_at: now,
    })
}

/// 查询会话的所有 Activity Event，按时间正序。
pub fn get_activity_events(
    conn: &Connection,
    conv_id: &str,
) -> SqlResult<Vec<ActivityEventRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, event_type, tool_name, status, input_json,
                output_json, error_message, workspace_path, created_at
         FROM activity_events
         WHERE conversation_id = ?1
         ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([conv_id], |row| {
        Ok(ActivityEventRecord {
            id: row.get(0)?,
            conversation_id: row.get(1)?,
            event_type: row.get(2)?,
            tool_name: row.get(3)?,
            status: row.get(4)?,
            input_json: row.get(5)?,
            output_json: row.get(6)?,
            error_message: row.get(7)?,
            workspace_path: row.get(8)?,
            created_at: row.get(9)?,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_lists_file_checkpoints_with_seq() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db should initialize");
        let conv = create_conversation(&conn).expect("conv");

        let c1 = record_file_checkpoint(&conn, &conv.id, "write_file", "a.txt", Some("old"), None, true)
            .expect("checkpoint 1");
        let c2 = record_file_checkpoint(&conn, &conv.id, "create_file", "b.txt", None, None, true)
            .expect("checkpoint 2");
        assert_eq!(c1.seq, 1);
        assert_eq!(c2.seq, 2);

        mark_checkpoint_reverted(&conn, &c2.id).expect("mark reverted");
        let list = list_file_checkpoints(&conn, &conv.id).expect("list");
        assert_eq!(list.len(), 2);
        assert!(!list[0].reverted);
        assert!(list[1].reverted);
        assert_eq!(list[0].prev_content.as_deref(), Some("old"));
        assert_eq!(list[1].prev_content, None);

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn rewind_truncates_last_n_messages_and_clears_wire() {
        // 0.0.49 edit=rewind 的存储原语:删末尾 n 条 + cut_ts 锚点 + 清 wire 快照。
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db");
        let conv = create_conversation(&conn).expect("conv");
        for i in 0..5 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            save_message(&conn, &conv.id, role, &format!("m{i}"), None, None).expect("save");
        }
        save_wire_snapshot(&conn, &conv.id, "[stale]").expect("wire");

        // cut_ts:末尾 2 条里最早一条的 created_at,应存在。
        assert!(cut_timestamp_for_last_n(&conn, &conv.id, 2).expect("cut").is_some());
        assert!(cut_timestamp_for_last_n(&conn, &conv.id, 0).expect("cut0").is_none());

        // 删末尾 2 条 → 剩 3 条,末条为 m2(同秒插入靠 rowid 定序)。
        assert_eq!(delete_last_n_messages(&conn, &conv.id, 2).expect("del"), 2);
        let remaining = get_messages(&conn, &conv.id).expect("get");
        assert_eq!(remaining.len(), 3);
        assert_eq!(remaining.last().unwrap().content, "m2");

        // n=0 无操作;n 超总数删全部。
        assert_eq!(delete_last_n_messages(&conn, &conv.id, 0).expect("del0"), 0);
        assert_eq!(delete_last_n_messages(&conn, &conv.id, 99).expect("delall"), 3);
        assert!(get_messages(&conn, &conv.id).expect("get").is_empty());

        // 清 wire 快照。
        delete_wire_snapshot(&conn, &conv.id).expect("delwire");
        assert!(get_wire_snapshot(&conn, &conv.id).expect("getwire").is_none());

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn permission_rules_are_idempotent() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db should initialize");

        add_permission_rule(&conn, "cmd:git push").expect("rule 1");
        add_permission_rule(&conn, "cmd:git push").expect("rule dup");
        add_permission_rule(&conn, "tool:write_file").expect("rule 2");

        let rules = list_permission_rules(&conn).expect("list");
        assert_eq!(rules.len(), 2);
        assert!(rules.contains(&"cmd:git push".to_string()));

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn saves_and_replaces_active_workspace() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db should initialize");

        let first = save_active_workspace(&conn, "C:\\workspace\\demo")
            .expect("workspace should save");
        assert_eq!(first.name, "demo");
        assert_eq!(first.path, "C:\\workspace\\demo");

        let second = save_active_workspace(&conn, "C:\\workspace\\other")
            .expect("workspace should replace");
        let loaded = get_active_workspace(&conn).expect("workspace should load");

        assert_eq!(loaded.map(|workspace| workspace.path), Some(second.path));
        assert_ne!(first.id, second.id);

        clear_active_workspace(&conn).expect("workspace should clear");
        assert!(get_active_workspace(&conn).expect("query should succeed").is_none());

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn creates_conversation_with_workspace_snapshot() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db should initialize");

        let conv = create_conversation_with_workspace(
            &conn,
            Some("C:\\workspace\\demo"),
            Some("MDGA"),
        )
        .expect("conversation should save workspace snapshot");
        let stored = list_conversations(&conn).expect("conversation should list");

        assert_eq!(conv.workspace_path.as_deref(), Some("C:\\workspace\\demo"));
        assert_eq!(conv.workspace_name.as_deref(), Some("MDGA"));
        assert_eq!(conv.mode, "local_workspace");
        assert_eq!(stored[0].workspace_path.as_deref(), Some("C:\\workspace\\demo"));

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn updates_conversation_workspace_binding() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db should initialize");

        // 起始为纯聊天会话。
        let conv = create_conversation(&conn).expect("conv");
        assert_eq!(conv.mode, "chat_only");
        assert_eq!(conv.workspace_path, None);

        // 绑定到工作区：mode 切 local_workspace，path/name 写入。
        let bound = update_conversation_workspace(
            &conn,
            &conv.id,
            Some("C:\\workspace\\demo"),
            Some("demo"),
        )
        .expect("bind workspace");
        assert_eq!(bound.id, conv.id);
        assert_eq!(bound.mode, "local_workspace");
        assert_eq!(bound.workspace_path.as_deref(), Some("C:\\workspace\\demo"));
        assert_eq!(bound.workspace_name.as_deref(), Some("demo"));
        assert!(bound.updated_at >= conv.updated_at);
        // 落库一致。
        let stored = get_conversation(&conn, &conv.id).expect("query").expect("exists");
        assert_eq!(stored.workspace_path.as_deref(), Some("C:\\workspace\\demo"));
        assert_eq!(stored.mode, "local_workspace");

        // 解绑为纯聊天：path/name 清空，mode 回 chat_only。
        let unbound = update_conversation_workspace(&conn, &conv.id, None, None)
            .expect("unbind workspace");
        assert_eq!(unbound.mode, "chat_only");
        assert_eq!(unbound.workspace_path, None);
        assert_eq!(unbound.workspace_name, None);

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn upserts_and_reads_model_provider_by_role() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db should initialize");

        // 首次配置主 provider（base_url 留空走官方；主模型恒为 openai 格式；context_window 显式给值）。
        let p1 = upsert_model_provider(
            &conn, "main", Some("deepseek"), Some("DeepSeek"), None, "sk-1", "deepseek-v4-pro", "openai",
            Some(1_000_000),
        )
        .expect("upsert main");
        assert_eq!(p1.role, "main");
        assert_eq!(p1.base_url, None);
        assert_eq!(p1.api_format, "openai");
        assert_eq!(p1.context_window, Some(1_000_000));

        // 再次 upsert 同 role 覆盖（验证唯一性：只保留一条；context_window 改为 None 验证可清空）。
        let p2 = upsert_model_provider(
            &conn, "main", Some("custom"), Some("自托管"), Some("https://proxy.local/v1"),
            "sk-2", "my-model", "openai", None,
        )
        .expect("upsert main again");
        assert_eq!(p2.api_key, "sk-2");

        let loaded = get_model_provider(&conn, "main").expect("query").expect("exists");
        assert_eq!(loaded.api_key, "sk-2");
        assert_eq!(loaded.base_url.as_deref(), Some("https://proxy.local/v1"));
        assert_eq!(loaded.model_id, "my-model");
        assert_eq!(loaded.context_window, None);

        // 仅一条 main，vision 未配。
        assert!(get_model_provider(&conn, "vision").expect("query").is_none());

        // 配 vision provider（anthropic 格式），列表含两角色，api_format 正确存取。
        upsert_model_provider(
            &conn, "vision", Some("custom"), Some("Claude Vision"), Some("https://api.anthropic.com"),
            "sk-v", "claude-3-5-sonnet", "anthropic", Some(200_000),
        )
        .expect("upsert vision");
        let vision = get_model_provider(&conn, "vision").expect("query").expect("exists");
        assert_eq!(vision.api_format, "anthropic");
        assert_eq!(vision.context_window, Some(200_000));
        // 未知 api_format 归一化为 openai。
        upsert_model_provider(
            &conn, "vision", Some("zhipu"), Some("GLM-4V"), None, "sk-v2", "glm-4v", "bogus", None,
        )
        .expect("upsert vision openai");
        assert_eq!(
            get_model_provider(&conn, "vision").expect("query").expect("exists").api_format,
            "openai"
        );
        let all = list_model_providers(&conn).expect("list");
        assert_eq!(all.len(), 2);

        // 删除 vision。
        delete_model_provider(&conn, "vision").expect("delete vision");
        assert!(get_model_provider(&conn, "vision").expect("query").is_none());
        assert_eq!(list_model_providers(&conn).expect("list").len(), 1);

        let _ = std::fs::remove_file(db_path);
    }

    // 注（0.0.59）：旧的 `resolves_role_provider_with_fallback_to_main` 已被
    // `resolve_role_provider_via_assignments_with_fallback` 取代——resolve 现从「连接库 + 角色引用」
    // 解析，不再读旧的 role-keyed model_providers 表。旧 upsert_model_provider/get_model_provider
    // 仍保留（旧表 + 迁移源 + 回滚路径），其往返一致由 `upserts_and_reads_model_provider_by_role`
    // 与集成测试 crud.rs 继续覆盖。

    #[test]
    fn reads_and_writes_app_settings() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db should initialize");

        assert_eq!(get_setting(&conn, "modality_extended").expect("get"), None);
        set_setting(&conn, "modality_extended", "true").expect("set");
        assert_eq!(
            get_setting(&conn, "modality_extended").expect("get").as_deref(),
            Some("true")
        );
        // upsert 覆盖。
        set_setting(&conn, "modality_extended", "false").expect("set again");
        assert_eq!(
            get_setting(&conn, "modality_extended").expect("get").as_deref(),
            Some("false")
        );

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn deletes_only_trailing_assistant_message() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db should initialize");
        let conv = create_conversation(&conn).expect("conv");

        // 空会话：无消息可删。
        assert!(!delete_last_assistant_message(&conn, &conv.id).expect("noop on empty"));

        // 最后一条是 user：不删。
        save_message(&conn, &conv.id, "user", "你好", None, None).expect("user msg");
        assert!(!delete_last_assistant_message(&conn, &conv.id).expect("noop on user tail"));
        assert_eq!(get_messages(&conn, &conv.id).expect("list").len(), 1);

        // 末尾是 assistant：删一条，回 true，user 仍在。
        save_message(&conn, &conv.id, "assistant", "回复", None, None).expect("assistant msg");
        assert!(delete_last_assistant_message(&conn, &conv.id).expect("delete assistant tail"));
        let after = get_messages(&conn, &conv.id).expect("list");
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].role, "user");

        // 再次调用：末尾又是 user，不删。
        assert!(!delete_last_assistant_message(&conn, &conv.id).expect("noop again"));

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn searches_conversations_by_title_or_message_body() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db should initialize");

        // c1 标题命中。
        let c1 = create_conversation(&conn).expect("c1");
        update_title(&conn, &c1.id, "关于 RustLang 的讨论").expect("title");
        // c2 仅消息正文命中。
        let c2 = create_conversation(&conn).expect("c2");
        save_message(&conn, &c2.id, "user", "请帮我看看 RustLang 的所有权", None, None).expect("msg");
        // c3 不命中。
        let c3 = create_conversation(&conn).expect("c3");
        save_message(&conn, &c3.id, "user", "无关内容", None, None).expect("msg3");

        let hits = search_conversations(&conn, "RustLang").expect("search");
        let ids: Vec<&str> = hits.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&c1.id.as_str()));
        assert!(ids.contains(&c2.id.as_str()));
        assert!(!ids.contains(&c3.id.as_str()));
        // 多条消息命中同一会话只出现一次（DISTINCT）。
        save_message(&conn, &c2.id, "assistant", "RustLang 又一次提到", None, None).expect("msg again");
        let hits2 = search_conversations(&conn, "RustLang").expect("search again");
        assert_eq!(hits2.iter().filter(|c| c.id == c2.id).count(), 1);

        // 通配符被转义为字面量：搜 "%" 不应匹配全部。
        let pct = search_conversations(&conn, "%").expect("search pct");
        assert!(pct.is_empty());

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn gets_conversation_by_id_with_workspace_snapshot() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db should initialize");

        let conv = create_conversation_with_workspace(
            &conn,
            Some("C:\\workspace\\demo"),
            Some("MDGA"),
        )
        .expect("conversation should save workspace snapshot");
        let stored = get_conversation(&conn, &conv.id)
            .expect("query should succeed")
            .expect("conversation should exist");

        assert_eq!(stored.id, conv.id);
        assert_eq!(stored.workspace_path.as_deref(), Some("C:\\workspace\\demo"));
        assert_eq!(stored.workspace_name.as_deref(), Some("MDGA"));

        let _ = std::fs::remove_file(db_path);
    }

    // ── 0.0.59：连接库 + 角色引用 + 数据迁移 ──────────────────────────────────

    /// 直接把一行旧式 model_providers 写库（绕过 upsert_model_provider 的 DELETE-by-role，
    /// 以便在同一张旧表里塞多角色含相同三元组的种子数据）。
    #[allow(clippy::too_many_arguments)]
    fn seed_old_provider(
        conn: &Connection,
        role: &str,
        preset: Option<&str>,
        label: Option<&str>,
        base_url: Option<&str>,
        api_key: &str,
        model_id: &str,
        api_format: &str,
        context_window: Option<i64>,
    ) {
        conn.execute(
            "INSERT INTO model_providers
             (id, role, preset, label, base_url, api_key, model_id, api_format, context_window, enabled, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10)",
            params![
                Uuid::new_v4().to_string(), role, preset, label, base_url, api_key,
                model_id, api_format, context_window, now_ts()
            ],
        )
        .expect("seed old provider");
    }

    #[test]
    fn connection_and_assignment_crud_roundtrip() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db");

        // 创建连接（id 空 ⇒ 生成新 uuid）。
        let c = upsert_connection(
            &conn, "", Some("DeepSeek"), Some("deepseek"), None, "sk-1", "openai",
        )
        .expect("create conn");
        assert!(!c.id.is_empty());
        assert_eq!(c.api_key, "sk-1");
        assert_eq!(c.api_format, "openai");
        assert_eq!(c.base_url, None);

        // 服务端兜底：新建连接(id 空)带空/纯空白 key 必须被拒(不再只靠前端表单)。
        assert!(
            upsert_connection(&conn, "", Some("X"), Some("deepseek"), None, "", "openai").is_err(),
            "新建连接空 key 应被拒"
        );
        assert!(
            upsert_connection(&conn, "", Some("X"), Some("deepseek"), None, "   ", "openai").is_err(),
            "新建连接纯空白 key 应被拒"
        );

        // 更新：api_key 留空 ⇒ 保留旧 key；base_url 空串归一为 None。
        let updated = upsert_connection(
            &conn, &c.id, Some("DS 改名"), Some("deepseek"), Some("   "), "", "anthropic",
        )
        .expect("update conn");
        assert_eq!(updated.id, c.id);
        assert_eq!(updated.api_key, "sk-1"); // 留空保留旧 key
        assert_eq!(updated.label.as_deref(), Some("DS 改名"));
        assert_eq!(updated.base_url, None);
        assert_eq!(updated.api_format, "anthropic");

        // 更新已存在连接但带新 key ⇒ 覆盖。
        let updated2 = upsert_connection(
            &conn, &c.id, Some("DS"), Some("deepseek"), Some("https://proxy/v1"), "sk-2", "openai",
        )
        .expect("update key");
        assert_eq!(updated2.api_key, "sk-2");
        assert_eq!(updated2.base_url.as_deref(), Some("https://proxy/v1"));

        // 0.0.60 模型层：在连接下登记一个模型，再把 main 角色指向该模型。
        let m = upsert_model(&conn, "", &c.id, "deepseek-chat", Some("DS Chat"), Some(64_000))
            .expect("add model");
        assert_eq!(m.connection_id, c.id);
        assert_eq!(m.context_window, Some(64_000));
        let a = upsert_role_model(&conn, ROLE_MAIN, &m.id, true).expect("assign main");
        assert_eq!(a.role, "main");
        assert_eq!(a.model_ref, m.id);

        // 引用计数：删被引用模型/连接均应被拒。
        let refs = connection_referenced_by(&conn, &c.id).expect("refs");
        assert_eq!(refs, vec!["main".to_string()]);
        assert!(delete_connection(&conn, &c.id).is_err(), "被引用连接不可删");
        assert!(delete_model(&conn, &m.id).is_err(), "被引用模型不可删");

        // 清 main 分配应被拒（main 不可清）。
        assert!(delete_role_model(&conn, ROLE_MAIN).is_err(), "main 不可清");

        // 改 plan 指向同一模型后再清 plan：连接仍有 main 引用 ⇒ 不能删。
        upsert_role_model(&conn, ROLE_PLAN, &m.id, true).expect("assign plan");
        delete_role_model(&conn, ROLE_PLAN).expect("clear plan");
        assert!(delete_connection(&conn, &c.id).is_err());

        // 建第二个连接 + 模型，把 main 改指它，则 c 已无人引用 ⇒ 可删（级联删其模型）。
        let c2 = upsert_connection(&conn, "", Some("Z"), Some("zhipu"), None, "sk-z", "openai")
            .expect("c2");
        let m2 = upsert_model(&conn, "", &c2.id, "glm-4", None, None).expect("add model 2");
        upsert_role_model(&conn, ROLE_MAIN, &m2.id, true).expect("remap main");
        delete_connection(&conn, &c.id).expect("now deletable");
        assert!(get_connection(&conn, &c.id).expect("get").is_none());
        // 级联：c 旗下模型 m 也被删。
        assert!(get_model(&conn, &m.id).expect("get model").is_none());

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn resolve_role_provider_via_assignments_with_fallback() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db");

        // 一个角色都没配：任何角色都解析不到。
        assert!(resolve_role_provider(&conn, ROLE_ACTION).expect("q").is_none());
        assert!(resolve_role_provider(&conn, ROLE_MAIN).expect("q").is_none());

        // 配 main 连接 + 模型 + 分配：未配置的功能角色全部回退 main。
        let main_conn = upsert_connection(
            &conn, "", Some("DeepSeek"), Some("deepseek"), None, "sk-main", "openai",
        )
        .expect("main conn");
        let main_model = upsert_model(&conn, "", &main_conn.id, "main-model", None, Some(128_000))
            .expect("main model");
        upsert_role_model(&conn, ROLE_MAIN, &main_model.id, true).expect("assign main");

        let action = resolve_role_provider(&conn, ROLE_ACTION).expect("q").expect("falls back");
        // 回退时合成出的 role 即被回退到的 main（与旧约定一致：source=main 时 role 显示 "main"）。
        assert_eq!(action.role, "main");
        assert_eq!(action.model_id, "main-model");
        assert_eq!(action.api_key, "sk-main");
        assert_eq!(action.preset.as_deref(), Some("deepseek"));
        assert_eq!(action.context_window, Some(128_000));
        assert_eq!(
            resolve_role_provider(&conn, ROLE_SUBAGENT).expect("q").expect("sub").model_id,
            "main-model"
        );
        assert_eq!(
            resolve_role_provider(&conn, ROLE_EMBED).expect("q").expect("embed").model_id,
            "main-model"
        );
        assert_eq!(
            resolve_role_provider(&conn, ROLE_MAIN).expect("q").expect("main").model_id,
            "main-model"
        );

        // 为 plan 单独绑一个连接 + 模型：plan 用自己的，action 仍回退 main。
        let plan_conn = upsert_connection(
            &conn, "", Some("Planner"), Some("custom"), Some("https://plan/v1"), "sk-plan", "openai",
        )
        .expect("plan conn");
        let plan_model =
            upsert_model(&conn, "", &plan_conn.id, "plan-model", None, None).expect("plan model");
        upsert_role_model(&conn, ROLE_PLAN, &plan_model.id, true).expect("assign plan");
        let plan = resolve_role_provider(&conn, ROLE_PLAN).expect("q").expect("plan");
        assert_eq!(plan.model_id, "plan-model");
        assert_eq!(plan.api_key, "sk-plan");
        assert_eq!(plan.base_url.as_deref(), Some("https://plan/v1"));
        assert_eq!(
            resolve_role_provider(&conn, ROLE_ACTION).expect("q").expect("action").model_id,
            "main-model"
        );

        // 禁用 plan 分配 ⇒ 回退 main。
        upsert_role_model(&conn, ROLE_PLAN, &plan_model.id, false).expect("disable plan");
        assert_eq!(
            resolve_role_provider(&conn, ROLE_PLAN).expect("q").expect("plan disabled").model_id,
            "main-model"
        );

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn migration_dedupes_connections_and_preserves_every_role() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db");

        // 新库：init_db 内迁移因 model_providers 为空而 no-op；此处手动播种旧式数据。
        // main / action / plan 三角色共用同一把 DeepSeek key（相同 preset+base_url(空)+key+format）
        // ⇒ 迁移后必须 dedup 成 1 个连接；critique 用一个不同的自定义 provider ⇒ 第 2 个连接。
        seed_old_provider(&conn, "main", Some("deepseek"), Some("DeepSeek"), None, "sk-shared", "deepseek-chat", "openai", Some(128_000));
        seed_old_provider(&conn, "action", Some("deepseek"), Some("DeepSeek"), None, "sk-shared", "deepseek-chat", "openai", Some(128_000));
        seed_old_provider(&conn, "plan", Some("deepseek"), Some("DeepSeek"), None, "sk-shared", "deepseek-reasoner", "openai", None);
        seed_old_provider(&conn, "critique", Some("custom"), Some("自托管"), Some("https://crit.local/v1"), "sk-crit", "crit-model", "openai", Some(32_000));

        // 迁移前先把每个角色的旧值快照下来，迁移后逐字段比对 resolve 结果。
        let pre: std::collections::HashMap<String, ModelProvider> = list_model_providers(&conn)
            .expect("list old")
            .into_iter()
            .map(|p| (p.role.clone(), p))
            .collect();

        // 跑链式迁移：pre-0.0.59 → 0.0.59（connections + role_assignments）→ 0.0.60（models + role_models）。
        migrate_to_connections_0059(&conn).expect("migrate 0059");

        // dedup：main/action/plan 三角色 → 1 个连接；critique → 另 1 个 ⇒ 共 2 个连接。
        let conns = list_connections(&conn).expect("list conns");
        assert_eq!(conns.len(), 2, "共享三元组应 dedup 为 1 连接 + 独立 1 连接 = 2");

        // 每个角色都有引用，且指向正确连接（main/action/plan 同一个，critique 不同）。
        let main_a = get_role_assignment(&conn, "main").expect("q").expect("main");
        let action_a = get_role_assignment(&conn, "action").expect("q").expect("action");
        let plan_a = get_role_assignment(&conn, "plan").expect("q").expect("plan");
        let crit_a = get_role_assignment(&conn, "critique").expect("q").expect("crit");
        assert_eq!(main_a.connection_id, action_a.connection_id, "main/action 共连接");
        assert_eq!(main_a.connection_id, plan_a.connection_id, "main/plan 共连接");
        assert_ne!(main_a.connection_id, crit_a.connection_id, "critique 独立连接");

        // 0.0.60 迁移：role_assignments → models（按 (connection, model_id) dedup）+ role_models。
        migrate_to_models_layer_0060(&conn).expect("migrate 0060");

        // models dedup：main/action 共 (deepseek 连接, deepseek-chat) ⇒ 1 个 model；
        // plan 同连接但 model_id 不同(deepseek-reasoner) ⇒ 另 1 个 model；critique 独立连接 ⇒ 第 3 个 model。
        let models = list_models(&conn).expect("list models");
        assert_eq!(models.len(), 3, "main/action 同 model dedup；plan 异 model_id；critique 独立 ⇒ 3");
        // main 与 action 指向同一 model_ref；plan 不同（同连接异 model_id）。
        let main_rm = get_role_model(&conn, "main").expect("q").expect("main");
        let action_rm = get_role_model(&conn, "action").expect("q").expect("action");
        let plan_rm = get_role_model(&conn, "plan").expect("q").expect("plan");
        let crit_rm = get_role_model(&conn, "critique").expect("q").expect("crit");
        assert_eq!(main_rm.model_ref, action_rm.model_ref, "main/action 共 model");
        assert_ne!(main_rm.model_ref, plan_rm.model_ref, "plan 异 model（同连接异 model_id）");
        assert_ne!(main_rm.model_ref, crit_rm.model_ref, "critique 独立 model");
        // context_window 下沉到 model：deepseek-chat=128000, deepseek-reasoner=None, crit-model=32000。
        assert_eq!(get_model(&conn, &main_rm.model_ref).unwrap().unwrap().context_window, Some(128_000));
        assert_eq!(get_model(&conn, &plan_rm.model_ref).unwrap().unwrap().context_window, None);
        assert_eq!(get_model(&conn, &crit_rm.model_ref).unwrap().unwrap().context_window, Some(32_000));

        // resolve_role_provider 对每个角色返回与迁移前逐字段等价（base_url/api_key/model_id/
        // api_format/context_window）——经新的 role_models → models → connections 链路。
        for role in ["main", "action", "plan", "critique"] {
            let before = &pre[role];
            let after = resolve_role_provider(&conn, role).expect("resolve").expect("present");
            assert_eq!(after.base_url, before.base_url, "{role} base_url");
            assert_eq!(after.api_key, before.api_key, "{role} api_key");
            assert_eq!(after.model_id, before.model_id, "{role} model_id");
            assert_eq!(after.api_format, before.api_format, "{role} api_format");
            assert_eq!(after.context_window, before.context_window, "{role} context_window");
        }

        // 未分配的角色（vision/subagent/embed 未在旧表）回退 main。
        assert_eq!(
            resolve_role_provider(&conn, ROLE_SUBAGENT).expect("q").expect("sub").model_id,
            "deepseek-chat"
        );
        assert_eq!(
            resolve_role_provider(&conn, ROLE_EMBED).expect("q").expect("embed").api_key,
            "sk-shared"
        );
        // 子代理回退链（subagent→action→main）经 role_models 仍成立。
        assert_eq!(
            resolve_subagent_provider(&conn).expect("q").expect("sub chain").model_id,
            "deepseek-chat"
        );

        // 幂等：再跑 0.0.60 迁移什么都不应改变（models / role_models 数量不变）。
        migrate_to_models_layer_0060(&conn).expect("migrate 0060 twice");
        assert_eq!(list_models(&conn).expect("models2").len(), 3, "二次迁移不应新增 model");
        assert_eq!(get_role_models(&conn).expect("rm2").len(), 4, "二次迁移不应改分配");
        // 0.0.59 迁移再跑也仍 no-op（role_assignments 已非空）。
        migrate_to_connections_0059(&conn).expect("migrate 0059 twice");
        assert_eq!(list_connections(&conn).expect("conns2").len(), 2, "二次 0.0.59 迁移不应新增连接");

        // main 不可清（防御）。
        assert!(delete_role_model(&conn, ROLE_MAIN).is_err());

        // 旧表保留（回滚路径）：model_providers 与 role_assignments 均原封未动。
        assert_eq!(list_model_providers(&conn).expect("old providers still there").len(), 4);
        assert_eq!(get_role_assignments(&conn).expect("legacy assignments still there").len(), 4);

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn resolve_subagent_falls_back_subagent_then_action_then_main() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db");

        // 连 main 都没配：解析不到。
        assert!(resolve_subagent_provider(&conn).expect("q").is_none());

        // 只配 main：subagent 默认落到 main（与「继承 action→main」逐字节一致）。
        let main_c = upsert_connection(&conn, "", Some("M"), Some("deepseek"), None, "sk-main", "openai")
            .expect("main conn");
        let main_m = upsert_model(&conn, "", &main_c.id, "main-model", None, None).expect("main model");
        upsert_role_model(&conn, ROLE_MAIN, &main_m.id, true).expect("main");
        assert_eq!(
            resolve_subagent_provider(&conn).expect("q").expect("main").model_id,
            "main-model"
        );

        // 配 action：subagent 未配 ⇒ 用 action。
        let act_c = upsert_connection(&conn, "", Some("A"), Some("custom"), Some("https://a/v1"), "sk-act", "openai")
            .expect("act conn");
        let act_m = upsert_model(&conn, "", &act_c.id, "action-model", None, None).expect("act model");
        upsert_role_model(&conn, ROLE_ACTION, &act_m.id, true).expect("action");
        assert_eq!(
            resolve_subagent_provider(&conn).expect("q").expect("action").model_id,
            "action-model"
        );

        // 显式配 subagent：优先用它。
        let sub_c = upsert_connection(&conn, "", Some("S"), Some("custom"), Some("https://s/v1"), "sk-sub", "openai")
            .expect("sub conn");
        let sub_m = upsert_model(&conn, "", &sub_c.id, "sub-model", None, None).expect("sub model");
        upsert_role_model(&conn, ROLE_SUBAGENT, &sub_m.id, true).expect("sub");
        let p = resolve_subagent_provider(&conn).expect("q").expect("sub");
        assert_eq!(p.model_id, "sub-model");
        assert_eq!(p.api_key, "sk-sub");

        // 禁用 subagent ⇒ 回退 action。
        upsert_role_model(&conn, ROLE_SUBAGENT, &sub_m.id, false).expect("disable sub");
        assert_eq!(
            resolve_subagent_provider(&conn).expect("q").expect("action again").model_id,
            "action-model"
        );

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn migration_skips_when_assignments_already_exist() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db");

        // 已经用 0.0.60 模型层配好 main（模拟「已迁过 / 用户已用新表配置」）。
        let c = upsert_connection(&conn, "", Some("Z"), Some("zhipu"), None, "sk-new", "openai")
            .expect("conn");
        let m = upsert_model(&conn, "", &c.id, "glm-4", None, None).expect("model");
        upsert_role_model(&conn, ROLE_MAIN, &m.id, true).expect("assign");

        // 旧 model_providers 与 0.0.59 role_assignments 里也各塞数据——两道迁移都应因各自的门而跳过。
        seed_old_provider(&conn, "main", Some("deepseek"), Some("DS"), None, "sk-old", "deepseek-chat", "openai", None);
        let c_old = upsert_connection(&conn, "", Some("Old"), Some("deepseek"), None, "sk-ra", "openai")
            .expect("old conn");
        upsert_role_assignment(&conn, ROLE_ACTION, &c_old.id, "deepseek-chat", None, true)
            .expect("legacy assignment");

        // 0.0.59 迁移：role_assignments 已非空 ⇒ no-op。0.0.60 迁移：role_models 已非空 ⇒ no-op。
        migrate_to_connections_0059(&conn).expect("0059 no-op");
        migrate_to_models_layer_0060(&conn).expect("0060 no-op");

        // 仍是新表里那条配置（未被旧行污染）：main 经模型层解析到 sk-new / glm-4。
        let main_rm = get_role_model(&conn, "main").expect("q").expect("main");
        assert_eq!(get_model(&conn, &main_rm.model_ref).unwrap().unwrap().model_id, "glm-4");
        assert_eq!(
            resolve_role_provider(&conn, ROLE_MAIN).expect("q").expect("main").api_key,
            "sk-new"
        );
        // 0.0.60 没有从 role_assignments 多造 model/role_model（门拦住了）：仅手配的 1 个 model、1 条分配。
        assert_eq!(list_models(&conn).expect("models").len(), 1, "0.0.60 门生效，未从旧表造 model");
        assert_eq!(get_role_models(&conn).expect("rm").len(), 1, "0.0.60 门生效，未从旧表造分配");

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn upsert_model_dedupes_by_connection_and_model_id() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db");
        let c = upsert_connection(&conn, "", Some("DS"), Some("deepseek"), None, "sk", "openai")
            .expect("conn");

        // 新建（id 空）。
        let m1 = upsert_model(&conn, "", &c.id, "deepseek-chat", Some("Chat"), Some(64_000))
            .expect("create");
        assert!(!m1.id.is_empty());
        assert_eq!(m1.label.as_deref(), Some("Chat"));
        assert_eq!(m1.context_window, Some(64_000));

        // 同 (connection, model_id) 再 upsert（id 空）⇒ 更新同一行而非插重复。
        let m1b = upsert_model(&conn, "", &c.id, "deepseek-chat", Some("Chat v2"), Some(128_000))
            .expect("dedupe update");
        assert_eq!(m1b.id, m1.id, "同连接同 model_id 应复用同一行");
        assert_eq!(m1b.label.as_deref(), Some("Chat v2"));
        assert_eq!(m1b.context_window, Some(128_000));
        assert_eq!(list_models_for_connection(&conn, &c.id).expect("list").len(), 1);

        // 同连接、不同 model_id ⇒ 第二行。
        let m2 = upsert_model(&conn, "", &c.id, "deepseek-reasoner", None, None).expect("second");
        assert_ne!(m2.id, m1.id);
        assert_eq!(list_models_for_connection(&conn, &c.id).expect("list").len(), 2);
        // label 缺省（空白归一为 None）。
        assert_eq!(m2.label, None);

        // 显式 id 更新（仅改 label/context_window）。
        let m2b = upsert_model(&conn, &m2.id, &c.id, "deepseek-reasoner", Some("R1"), Some(32_000))
            .expect("update by id");
        assert_eq!(m2b.id, m2.id);
        assert_eq!(m2b.label.as_deref(), Some("R1"));

        // upsert_role_model 校验 model_ref 存在；指向不存在 ⇒ Err。
        assert!(upsert_role_model(&conn, ROLE_MAIN, "nonexistent-id", true).is_err());
        upsert_role_model(&conn, ROLE_MAIN, &m1.id, true).expect("assign main to m1");
        // 被引用模型不可删；改指走后可删。
        assert!(delete_model(&conn, &m1.id).is_err(), "被引用模型不可删");
        upsert_role_model(&conn, ROLE_MAIN, &m2.id, true).expect("remap main");
        delete_model(&conn, &m1.id).expect("now deletable");
        assert!(get_model(&conn, &m1.id).expect("get").is_none());

        let _ = std::fs::remove_file(db_path);
    }

    /// 直接把一行 0.0.59 role_assignments 写库（绕过 init_db 链式迁移），用于播种 0.0.60 迁移输入。
    fn seed_role_assignment(
        conn: &Connection,
        role: &str,
        connection_id: &str,
        model_id: &str,
        context_window: Option<i64>,
        enabled: bool,
    ) {
        conn.execute(
            "INSERT INTO role_assignments
             (role, connection_id, model_id, context_window, enabled, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(role) DO UPDATE SET
                 connection_id = excluded.connection_id,
                 model_id = excluded.model_id,
                 context_window = excluded.context_window,
                 enabled = excluded.enabled,
                 updated_at = excluded.updated_at",
            params![role, connection_id, model_id, context_window, enabled as i64, now_ts()],
        )
        .expect("seed role_assignment");
    }

    #[test]
    fn migration_0060_dedupes_models_and_preserves_every_role() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db");

        // 播种 0.0.59 态：连接 A（deepseek）下两个角色（main/action）共用 SAME (conn, model_id)；
        // plan 同连接 A 但 DIFFERENT model_id；critique 用独立连接 B。
        let conn_a = upsert_connection(&conn, "", Some("A"), Some("deepseek"), None, "sk-a", "openai")
            .expect("conn A");
        let conn_b = upsert_connection(
            &conn, "", Some("B"), Some("custom"), Some("https://b/v1"), "sk-b", "anthropic",
        )
        .expect("conn B");
        // 注意：role_assignments 里 main/action 同 (connA, deepseek-chat, ctx=128000)。
        seed_role_assignment(&conn, "main", &conn_a.id, "deepseek-chat", Some(128_000), true);
        seed_role_assignment(&conn, "action", &conn_a.id, "deepseek-chat", Some(128_000), true);
        seed_role_assignment(&conn, "plan", &conn_a.id, "deepseek-reasoner", None, true);
        seed_role_assignment(&conn, "critique", &conn_b.id, "claude-3-5-sonnet", Some(200_000), true);

        // 迁移前：对每个角色快照「应得 provider」——直接用 role_assignment + connection 合成，
        // 作为逐字节比对基准（这正是 0.0.59 resolve 的语义）。
        let assignments = get_role_assignments(&conn).expect("assignments");
        let pre: std::collections::HashMap<String, ModelProvider> = assignments
            .iter()
            .map(|a| {
                let c = get_connection(&conn, &a.connection_id).unwrap().unwrap();
                (
                    a.role.clone(),
                    ModelProvider {
                        id: a.connection_id.clone(),
                        role: a.role.clone(),
                        preset: c.preset.clone(),
                        label: c.label.clone(),
                        base_url: c.base_url.clone().filter(|s| !s.trim().is_empty()),
                        api_key: c.api_key.clone(),
                        model_id: a.model_id.clone(),
                        api_format: c.api_format.clone(),
                        context_window: a.context_window,
                        enabled: a.enabled,
                        updated_at: a.updated_at,
                    },
                )
            })
            .collect();

        // 跑 0.0.60 迁移。
        migrate_to_models_layer_0060(&conn).expect("migrate 0060");

        // dedup：main/action 同 (connA, deepseek-chat) ⇒ 1 model；plan 同连接异 model_id ⇒ 第 2 model；
        // critique 独立连接 ⇒ 第 3 model。共 3 个 model 行。
        assert_eq!(list_models(&conn).expect("models").len(), 3);
        let main_rm = get_role_model(&conn, "main").expect("q").expect("main");
        let action_rm = get_role_model(&conn, "action").expect("q").expect("action");
        let plan_rm = get_role_model(&conn, "plan").expect("q").expect("plan");
        assert_eq!(main_rm.model_ref, action_rm.model_ref, "main/action 共 model（同 conn+model_id）");
        assert_ne!(main_rm.model_ref, plan_rm.model_ref, "plan 异 model（同 conn 异 model_id）");
        // 连接 A 下应有 2 个 model（deepseek-chat / deepseek-reasoner），连接 B 下 1 个。
        assert_eq!(list_models_for_connection(&conn, &conn_a.id).expect("A").len(), 2);
        assert_eq!(list_models_for_connection(&conn, &conn_b.id).expect("B").len(), 1);

        // resolve_role_provider 对 EVERY role 逐字节等价（base_url/api_key/model_id/api_format/context_window）。
        for role in ["main", "action", "plan", "critique"] {
            let before = &pre[role];
            let after = resolve_role_provider(&conn, role).expect("resolve").expect("present");
            assert_eq!(after.base_url, before.base_url, "{role} base_url");
            assert_eq!(after.api_key, before.api_key, "{role} api_key");
            assert_eq!(after.model_id, before.model_id, "{role} model_id");
            assert_eq!(after.api_format, before.api_format, "{role} api_format");
            assert_eq!(after.context_window, before.context_window, "{role} context_window");
        }

        // 未分配角色（subagent/embed/vision）回退 main；子代理链 subagent→action→main 成立。
        assert_eq!(
            resolve_role_provider(&conn, ROLE_SUBAGENT).expect("q").expect("sub").model_id,
            "deepseek-chat"
        );
        assert_eq!(
            resolve_role_provider(&conn, ROLE_EMBED).expect("q").expect("embed").api_key,
            "sk-a"
        );
        assert_eq!(
            resolve_subagent_provider(&conn).expect("q").expect("subchain").model_id,
            "deepseek-chat"
        );

        // 幂等：再跑一次 = no-op（门：role_models 已非空）。models/role_models 数量不变。
        migrate_to_models_layer_0060(&conn).expect("migrate twice");
        assert_eq!(list_models(&conn).expect("models2").len(), 3);
        assert_eq!(get_role_models(&conn).expect("rm2").len(), 4);

        // 旧表惰性保留：role_assignments 与 model_providers 均原封未动。
        assert_eq!(get_role_assignments(&conn).expect("ra").len(), 4, "role_assignments 未动");
        assert_eq!(list_model_providers(&conn).expect("mp").len(), 0, "model_providers 未被本迁移写入");

        let _ = std::fs::remove_file(db_path);
    }

    // 回归(审查 HIGH):旧库给同 (连接,模型) 的 main 与 action 配了**不同** context_window 时,
    // dedup 复用一条 model;role ASC 迭代下 action 先建,若不处理 main 复用会丢掉 main 的窗口
    //(main 窗口驱动压缩软上限)。本测试锁:迁移后 main 解析出的 context_window 必须是 main 的值(main 权威)。
    #[test]
    fn migration_0060_main_context_window_wins_on_dedup() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db");
        let c = upsert_connection(&conn, "", Some("A"), Some("deepseek"), None, "sk-a", "openai")
            .expect("conn");
        // main=128k、action=64k,同一 (连接, deepseek-chat)。action 字母序在前、先建 model。
        seed_role_assignment(&conn, "main", &c.id, "deepseek-chat", Some(128_000), true);
        seed_role_assignment(&conn, "action", &c.id, "deepseek-chat", Some(64_000), true);

        migrate_to_models_layer_0060(&conn).expect("migrate 0060");

        // 同 (连接,模型) ⇒ 1 个 model 行;其 context_window 须为 main 的 128k(覆盖 action 先建的 64k)。
        assert_eq!(list_models(&conn).expect("models").len(), 1, "同 conn+model_id 应 dedup 为 1 行");
        let main_p = resolve_role_provider(&conn, ROLE_MAIN).expect("q").expect("main");
        assert_eq!(main_p.context_window, Some(128_000), "main 的 context_window 必须不丢(main 权威)");
        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn migration_0060_skips_on_fresh_db_and_when_already_migrated() {
        // 全新库（role_assignments 为空）：迁移 no-op，models/role_models 保持空。
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db");
        // init_db 已跑过两道迁移（均因空而跳过）。
        assert!(list_models(&conn).expect("models").is_empty());
        assert!(get_role_models(&conn).expect("rm").is_empty());
        // 再显式跑一次仍 no-op。
        migrate_to_models_layer_0060(&conn).expect("noop");
        assert!(list_models(&conn).expect("models2").is_empty());

        // 已迁库（role_models 非空）：即便 role_assignments 也有行，迁移因门跳过，不覆盖。
        let c = upsert_connection(&conn, "", Some("Z"), Some("zhipu"), None, "sk-new", "openai")
            .expect("conn");
        let m = upsert_model(&conn, "", &c.id, "glm-4", None, None).expect("model");
        upsert_role_model(&conn, ROLE_MAIN, &m.id, true).expect("assign");
        seed_role_assignment(&conn, "main", &c.id, "should-be-ignored", None, true);
        migrate_to_models_layer_0060(&conn).expect("skip when migrated");
        // main 仍解析到手配的 glm-4（未被 role_assignments 的 should-be-ignored 覆盖）。
        assert_eq!(
            resolve_role_provider(&conn, ROLE_MAIN).expect("q").expect("main").model_id,
            "glm-4"
        );
        assert_eq!(list_models(&conn).expect("models3").len(), 1);

        let _ = std::fs::remove_file(db_path);
    }

    // ── 0.0.62：级联删除（连接 / 模型）──────────────────────────────────────────

    #[test]
    fn delete_connection_cascade_unassigns_roles_including_main() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db");

        // 一个连接，旗下两个模型：main→modelA、action→modelB（都在本连接）；plan 不设（跟随 main）。
        let c = upsert_connection(&conn, "", Some("DS"), Some("deepseek"), None, "sk-1", "openai")
            .expect("conn");
        let model_a = upsert_model(&conn, "", &c.id, "model-a", None, Some(128_000)).expect("model A");
        let model_b = upsert_model(&conn, "", &c.id, "model-b", None, None).expect("model B");
        upsert_role_model(&conn, ROLE_MAIN, &model_a.id, true).expect("assign main");
        upsert_role_model(&conn, ROLE_ACTION, &model_b.id, true).expect("assign action");

        // 前置 sanity：拒绝式 delete_connection 此时会被拒（被 main+action 引用）。
        assert!(delete_connection(&conn, &c.id).is_err(), "被引用连接拒删（拒绝式）");
        // plan 未配，经回退链解析到 main（modelA）。
        assert_eq!(
            resolve_role_provider(&conn, ROLE_PLAN).expect("q").expect("plan via main").model_id,
            "model-a"
        );

        // 级联删：返回被解除分配的角色（排序）= ["action","main"]（含 main，尽管 delete_role_model 拒删 main）。
        let affected = delete_connection_cascade(&conn, &c.id).expect("cascade");
        assert_eq!(affected, vec!["action".to_string(), "main".to_string()]);

        // 之后：连接没了；旗下模型清空；main/action 分配清掉；main 变未配置 ⇒ resolve(main)=None。
        assert!(get_connection(&conn, &c.id).expect("get conn").is_none(), "连接已删");
        assert!(
            list_models_for_connection(&conn, &c.id).expect("list models").is_empty(),
            "旗下模型已级联删"
        );
        assert!(get_model(&conn, &model_a.id).expect("ma").is_none());
        assert!(get_model(&conn, &model_b.id).expect("mb").is_none());
        assert!(get_role_model(&conn, ROLE_MAIN).expect("rm main").is_none(), "main 分配已清");
        assert!(get_role_model(&conn, ROLE_ACTION).expect("rm action").is_none(), "action 分配已清");
        assert!(
            resolve_role_provider(&conn, ROLE_MAIN).expect("q").is_none(),
            "main 未配置 ⇒ 解析为 None（交 app 既有「请先配置主模型」处理）"
        );
        // action 也不再解析（main 都没了，回退无处可去）。
        assert!(resolve_role_provider(&conn, ROLE_ACTION).expect("q").is_none());

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn delete_model_cascade_unassigns_main() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db");

        let c = upsert_connection(&conn, "", Some("DS"), Some("deepseek"), None, "sk-1", "openai")
            .expect("conn");
        let model_a = upsert_model(&conn, "", &c.id, "model-a", None, Some(64_000)).expect("model A");
        upsert_role_model(&conn, ROLE_MAIN, &model_a.id, true).expect("assign main");

        // 拒绝式 delete_model 会被拒（被 main 引用）。
        assert!(delete_model(&conn, &model_a.id).is_err(), "被引用模型拒删（拒绝式）");

        // 级联删模型：返回 ["main"]（清掉了 main 的分配，尽管 delete_role_model 拒删 main）。
        let affected = delete_model_cascade(&conn, &model_a.id).expect("cascade");
        assert_eq!(affected, vec!["main".to_string()]);

        // 模型没了；main 分配清掉；main 未配置 ⇒ resolve(main)=None。连接本身保留（只删模型）。
        assert!(get_model(&conn, &model_a.id).expect("get model").is_none(), "模型已删");
        assert!(get_role_model(&conn, ROLE_MAIN).expect("rm main").is_none(), "main 分配已清");
        assert!(resolve_role_provider(&conn, ROLE_MAIN).expect("q").is_none(), "main 未配置");
        assert!(get_connection(&conn, &c.id).expect("get conn").is_some(), "连接本身仍在（只删模型）");

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn cascades_are_transactional_no_partial_state() {
        // 结构性 sanity：级联在单事务里完成，正常路径下提交后无残留；且对「不存在的 id」是安全 no-op
        // （受影响角色为空、什么都不删、不留半状态）。出错路径（execute_batch ROLLBACK）由实现保证原子。
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db");

        // 删不存在的连接/模型：返回空、无副作用、不报错（事务正常提交）。
        assert!(delete_connection_cascade(&conn, "no-such-conn").expect("noop conn").is_empty());
        assert!(delete_model_cascade(&conn, "no-such-model").expect("noop model").is_empty());

        // 两个连接，各配一个角色。级联删 c1 不应碰 c2 的模型/分配（事务作用域限定在 c1）。
        let c1 = upsert_connection(&conn, "", Some("A"), Some("deepseek"), None, "sk-a", "openai")
            .expect("c1");
        let c2 = upsert_connection(&conn, "", Some("B"), Some("zhipu"), None, "sk-b", "openai")
            .expect("c2");
        let m1 = upsert_model(&conn, "", &c1.id, "m1", None, None).expect("m1");
        let m2 = upsert_model(&conn, "", &c2.id, "m2", None, None).expect("m2");
        upsert_role_model(&conn, ROLE_MAIN, &m1.id, true).expect("main→m1");
        upsert_role_model(&conn, ROLE_ACTION, &m2.id, true).expect("action→m2");

        let affected = delete_connection_cascade(&conn, &c1.id).expect("cascade c1");
        assert_eq!(affected, vec!["main".to_string()]);
        // c2 / m2 / action 分配完好（事务只动了 c1 相关行，无越界删除、无半状态）。
        assert!(get_connection(&conn, &c2.id).expect("c2").is_some());
        assert!(get_model(&conn, &m2.id).expect("m2").is_some());
        assert_eq!(
            get_role_model(&conn, ROLE_ACTION).expect("rm action").expect("present").model_ref,
            m2.id
        );

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn pricing_and_billing_round_trip() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db");

        // 新建连接：billing_mode 默认 'api'，subscription_json 为 None（建库默认值）。
        let c = upsert_connection(&conn, "", Some("DS"), Some("deepseek"), None, "sk-1", "openai")
            .expect("conn");
        let c0 = get_connection(&conn, &c.id).expect("get").expect("present");
        assert_eq!(c0.billing_mode, "api", "新连接默认 api");
        assert!(c0.subscription_json.is_none());

        // 新建模型：pricing_json 默认 None。
        let m = upsert_model(&conn, "", &c.id, "deepseek-chat", None, Some(128_000)).expect("model");
        assert!(get_model(&conn, &m.id).expect("get").expect("present").pricing_json.is_none());

        // set_model_pricing：原样存取（含 `_` 前缀元数据，后端不解析）。
        let pricing = r#"{"currency":"CNY","unit":"per_1m","input":2.0,"output":3.0,"_source":"manual","_needsVerify":false}"#;
        let m1 = set_model_pricing(&conn, &m.id, Some(pricing)).expect("set pricing");
        assert_eq!(m1.pricing_json.as_deref(), Some(pricing), "pricing_json 原样往返");
        // 重读确认持久化。
        assert_eq!(
            get_model(&conn, &m.id).expect("get").expect("present").pricing_json.as_deref(),
            Some(pricing)
        );
        // 传 None 清空。
        let m2 = set_model_pricing(&conn, &m.id, None).expect("clear pricing");
        assert!(m2.pricing_json.is_none(), "传 None 清空 pricing_json");
        // 重新设置，供后续 resolve 断言。
        set_model_pricing(&conn, &m.id, Some(pricing)).expect("reset pricing");

        // set_connection_billing：归一化未知值落回 'api'；合法值原样存。
        let c1 = set_connection_billing(&conn, &c.id, "subscription", Some(r#"{"plan":"pro"}"#))
            .expect("set billing");
        assert_eq!(c1.billing_mode, "subscription");
        assert_eq!(c1.subscription_json.as_deref(), Some(r#"{"plan":"pro"}"#));
        let c2 = set_connection_billing(&conn, &c.id, "garbage", None).expect("normalize");
        assert_eq!(c2.billing_mode, "api", "未知 billing_mode 落回 api");
        assert!(c2.subscription_json.is_none(), "subscription_json 被清空");
        // 设回 none 供 resolve 断言。
        set_connection_billing(&conn, &c.id, "none", None).expect("set none");

        // 不存在的 id 报错（非静默 no-op）。
        assert!(set_model_pricing(&conn, "no-such-model", Some("{}")).is_err());
        assert!(set_connection_billing(&conn, "no-such-conn", "api", None).is_err());

        // resolve_pricing_context：main 直接命中；功能角色未配置回退 main（与 resolve_role_provider 同口径）。
        upsert_role_model(&conn, ROLE_MAIN, &m.id, true).expect("assign main");
        let ctx = resolve_pricing_context(&conn, ROLE_MAIN).expect("q").expect("main ctx");
        assert_eq!(ctx.billing_mode, "none");
        assert_eq!(ctx.pricing_json.as_deref(), Some(pricing));
        assert_eq!(ctx.preset.as_deref(), Some("deepseek"));
        assert_eq!(ctx.model_id, "deepseek-chat");
        // action 未单独配置 ⇒ 回退 main 的连接/模型计价上下文。
        let action_ctx = resolve_pricing_context(&conn, ROLE_ACTION).expect("q").expect("fallback");
        assert_eq!(action_ctx.billing_mode, "none");
        assert_eq!(action_ctx.pricing_json.as_deref(), Some(pricing));
        assert_eq!(action_ctx.model_id, "deepseek-chat");

        // 未配置任何角色的库：resolve 返回 None。
        let empty_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let empty = init_db(&empty_path).expect("empty db");
        assert!(resolve_pricing_context(&empty, ROLE_MAIN).expect("q").is_none());
        assert!(resolve_pricing_context(&empty, ROLE_ACTION).expect("q").is_none());
        let _ = std::fs::remove_file(empty_path);

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn ym_from_unix_secs_known_timestamps() {
        // 已知 UTC 时刻 → 年月（含闰年 2 月底、年/月边界、纪元起点）。
        // 1970-01-01 00:00:00Z
        assert_eq!(ym_from_unix_secs(0), "1970-01");
        // 1970-01-31 23:59:59Z（1 月末最后一秒，仍是 01）
        assert_eq!(ym_from_unix_secs(2_678_399), "1970-01");
        // 1970-02-01 00:00:00Z（进 2 月）
        assert_eq!(ym_from_unix_secs(2_678_400), "1970-02");
        // 2000-02-29 12:00:00Z（闰年 2 月 29 日，世纪闰年）→ 2000-02
        assert_eq!(ym_from_unix_secs(951_825_600), "2000-02");
        // 2000-03-01 00:00:00Z（闰年 2 月翻 3 月）→ 2000-03
        assert_eq!(ym_from_unix_secs(951_868_800), "2000-03");
        // 2021-02-28 23:59:59Z（平年 2 月末最后一秒）→ 2021-02
        assert_eq!(ym_from_unix_secs(1_614_556_799), "2021-02");
        // 2021-03-01 00:00:00Z → 2021-03
        assert_eq!(ym_from_unix_secs(1_614_556_800), "2021-03");
        // 2023-12-31 23:59:59Z（年末最后一秒，跨年边界）→ 2023-12
        assert_eq!(ym_from_unix_secs(1_704_067_199), "2023-12");
        // 2024-01-01 00:00:00Z（跨年）→ 2024-01
        assert_eq!(ym_from_unix_secs(1_704_067_200), "2024-01");
        // 2024-12-01 00:00:00Z（闰年 12 月起点）→ 2024-12
        assert_eq!(ym_from_unix_secs(1_733_011_200), "2024-12");

        // current_ym 形状自洽：恒为 "YYYY-MM"，月在 01..=12。
        let ym = current_ym();
        assert_eq!(ym.len(), 7);
        assert_eq!(&ym[4..5], "-");
        let month: u32 = ym[5..7].parse().expect("month digits");
        assert!((1..=12).contains(&month), "month {month} 越界");
    }

    #[test]
    fn usage_counters_bump_accumulate_isolate_and_default() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db");

        // 无行 → 全 0。
        let zero = get_monthly_usage(&conn, "conn-a", "2026-06").expect("get empty");
        assert_eq!(zero.prompt_tokens, 0);
        assert_eq!(zero.completion_tokens, 0);
        assert_eq!(zero.total_tokens, 0);

        // bump 两次同 (connection_id, ym) → 累加（不是覆盖）。
        bump_usage_counter(&conn, "conn-a", "2026-06", 100, 40, 140).expect("bump 1");
        bump_usage_counter(&conn, "conn-a", "2026-06", 5, 3, 8).expect("bump 2");
        let acc = get_monthly_usage(&conn, "conn-a", "2026-06").expect("get acc");
        assert_eq!(acc.prompt_tokens, 105);
        assert_eq!(acc.completion_tokens, 43);
        assert_eq!(acc.total_tokens, 148);

        // 按 ym 隔离：另一月不受影响。
        bump_usage_counter(&conn, "conn-a", "2026-07", 1, 1, 2).expect("bump other month");
        let jul = get_monthly_usage(&conn, "conn-a", "2026-07").expect("get jul");
        assert_eq!(jul.total_tokens, 2);
        let jun = get_monthly_usage(&conn, "conn-a", "2026-06").expect("get jun again");
        assert_eq!(jun.total_tokens, 148, "6 月不被 7 月写入污染");

        // 按 connection_id 隔离：另一连接同月互不干扰。
        bump_usage_counter(&conn, "conn-b", "2026-06", 9, 9, 18).expect("bump conn-b");
        let b = get_monthly_usage(&conn, "conn-b", "2026-06").expect("get b");
        assert_eq!(b.total_tokens, 18);
        let a_after = get_monthly_usage(&conn, "conn-a", "2026-06").expect("get a after b");
        assert_eq!(a_after.total_tokens, 148, "conn-a 不被 conn-b 写入污染");

        let _ = std::fs::remove_file(db_path);
    }
}
