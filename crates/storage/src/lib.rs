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
    /// 上下文窗口（tokens，可选；Plan27 C2 #2）：该供应商模型的最大上下文长度。
    /// 主 provider 有值时，agent_loop / compaction 的软上限按 context_window × 0.8 推导，
    /// 使非 DeepSeek 的小窗口模型也能在真实上限前触发压缩；None 表示沿用默认软上限。
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

/// 主模型角色：所有未配置的功能角色（action/plan/critique）回退到它。
pub const ROLE_MAIN: &str = "main";
/// 行动角色：执行工具的常规循环用此模型（未配置回退 main）。
pub const ROLE_ACTION: &str = "action";
/// 规划角色：计划模式 / 规划步骤用此模型（未配置回退 main）。
pub const ROLE_PLAN: &str = "plan";
/// 评审角色：审查 / 批评步骤用此模型（未配置回退 main）。
pub const ROLE_CRITIQUE: &str = "critique";
/// 视觉角色：识图（已有，独立链路，不参与本回退逻辑）。
pub const ROLE_VISION: &str = "vision";

/// 按角色解析实际生效的 provider，未配置（或被禁用）时回退到主模型 `main`（R8）。
///
/// 语义：给定一个功能角色（如 "action" / "plan" / "critique"），若该角色有一条 *启用* 的 provider，
/// 返回它；否则回退返回 "main" provider。这样在用户没有为任何角色单独配模型时，行为与从前完全一致
/// （全部走 main），即「未配置即等价于关闭」的向后兼容保证。
///
/// 返回值语义：
/// - `Ok(Some(p))`：解析到一个可用 provider（角色自身的，或回退后的 main）。
/// - `Ok(None)`：角色未配置且连 main 也没配（调用方据此报「请先配置主模型」）。
///
/// 注意：role == "main" 时即直接返回 main（不存在二次回退）；被禁用（enabled=false）的角色 provider
/// 视为「未配置」从而触发回退，避免误用一条被关掉的配置。视觉角色有独立链路，调用方一般不经此函数。
pub fn resolve_role_provider(conn: &Connection, role: &str) -> SqlResult<Option<ModelProvider>> {
    // 主角色：直接取 main，不回退。
    if role == ROLE_MAIN {
        return get_model_provider(conn, ROLE_MAIN);
    }
    // 功能角色：自身已配且启用则用之，否则回退 main。
    if let Some(p) = get_model_provider(conn, role)? {
        if p.enabled {
            return Ok(Some(p));
        }
    }
    get_model_provider(conn, ROLE_MAIN)
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

    #[test]
    fn resolves_role_provider_with_fallback_to_main() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db should initialize");

        // 一个角色都没配：任何角色都解析不到（连 main 都没有）。
        assert!(resolve_role_provider(&conn, ROLE_ACTION).expect("query").is_none());
        assert!(resolve_role_provider(&conn, ROLE_PLAN).expect("query").is_none());
        assert!(resolve_role_provider(&conn, ROLE_MAIN).expect("query").is_none());

        // 配主模型后，未配置的功能角色全部回退到 main（向后兼容：等价于关闭多模型路由）。
        upsert_model_provider(
            &conn, ROLE_MAIN, Some("deepseek"), Some("DeepSeek"), None, "sk-main", "main-model",
            "openai", Some(128_000),
        )
        .expect("upsert main");
        let action = resolve_role_provider(&conn, ROLE_ACTION).expect("query").expect("falls back");
        assert_eq!(action.role, "main");
        assert_eq!(action.model_id, "main-model");
        let plan = resolve_role_provider(&conn, ROLE_PLAN).expect("query").expect("falls back");
        assert_eq!(plan.model_id, "main-model");
        let critique =
            resolve_role_provider(&conn, ROLE_CRITIQUE).expect("query").expect("falls back");
        assert_eq!(critique.model_id, "main-model");
        // main 自身解析为 main。
        assert_eq!(
            resolve_role_provider(&conn, ROLE_MAIN).expect("query").expect("main").model_id,
            "main-model"
        );

        // 为 plan 角色单独绑定一个模型：plan 用自己的，action/critique 仍回退 main。
        upsert_model_provider(
            &conn, ROLE_PLAN, Some("custom"), Some("Planner"), Some("https://plan.local/v1"),
            "sk-plan", "plan-model", "openai", None,
        )
        .expect("upsert plan");
        assert_eq!(
            resolve_role_provider(&conn, ROLE_PLAN).expect("query").expect("plan").model_id,
            "plan-model"
        );
        assert_eq!(
            resolve_role_provider(&conn, ROLE_ACTION).expect("query").expect("action").model_id,
            "main-model"
        );
        assert_eq!(
            resolve_role_provider(&conn, ROLE_CRITIQUE).expect("query").expect("critique").model_id,
            "main-model"
        );

        // 被禁用的角色 provider 视为未配置，触发回退到 main（避免误用被关掉的配置）。
        conn.execute(
            "UPDATE model_providers SET enabled = 0 WHERE role = ?1",
            params![ROLE_PLAN],
        )
        .expect("disable plan");
        assert_eq!(
            resolve_role_provider(&conn, ROLE_PLAN).expect("query").expect("plan disabled").model_id,
            "main-model"
        );

        let _ = std::fs::remove_file(db_path);
    }

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
}
