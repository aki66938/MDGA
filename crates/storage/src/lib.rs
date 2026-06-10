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
        ",
    )?;
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

// ── Conversation CRUD ─────────────────────────────────────────────────────

/// 创建新会话，初始标题为"新对话"。
///
/// 输入数据库连接；插入一条 conversations 记录并返回完整结构体。
pub fn create_conversation(conn: &Connection) -> SqlResult<Conversation> {
    let id = Uuid::new_v4().to_string();
    let now = now_ts();
    conn.execute(
        "INSERT INTO conversations (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
        params![id, "新对话", now],
    )?;
    Ok(Conversation {
        id,
        title: "新对话".to_string(),
        created_at: now,
        updated_at: now,
    })
}

/// 查询所有会话，按最近更新时间倒序排列。
pub fn list_conversations(conn: &Connection) -> SqlResult<Vec<Conversation>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, created_at, updated_at
         FROM conversations
         ORDER BY updated_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Conversation {
            id: row.get(0)?,
            title: row.get(1)?,
            created_at: row.get(2)?,
            updated_at: row.get(3)?,
        })
    })?;
    rows.collect()
}

/// 更新会话标题，同时刷新 updated_at。
pub fn update_title(conn: &Connection, conv_id: &str, title: &str) -> SqlResult<()> {
    conn.execute(
        "UPDATE conversations SET title = ?1, updated_at = ?2 WHERE id = ?3",
        params![title, now_ts(), conv_id],
    )?;
    Ok(())
}

/// 删除会话及其所有消息（ON DELETE CASCADE）。
pub fn delete_conversation(conn: &Connection, conv_id: &str) -> SqlResult<()> {
    conn.execute("DELETE FROM conversations WHERE id = ?1", params![conv_id])?;
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
) -> SqlResult<()> {
    let id = Uuid::new_v4().to_string();
    let now = now_ts();
    conn.execute(
        "INSERT INTO messages (id, conversation_id, role, content, usage_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, conv_id, role, content, usage_json, now],
    )?;
    conn.execute(
        "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
        params![now, conv_id],
    )?;
    Ok(())
}

/// 查询会话的所有消息，按时间正序排列。
pub fn get_messages(conn: &Connection, conv_id: &str) -> SqlResult<Vec<StoredMessage>> {
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, role, content, usage_json, created_at
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
            created_at: row.get(5)?,
        })
    })?;
    rows.collect()
}
