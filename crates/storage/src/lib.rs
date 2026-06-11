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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_replaces_active_workspace() {
        let db_path = std::env::temp_dir().join(format!("mdga-storage-{}.db", Uuid::new_v4()));
        let conn = init_db(&db_path).expect("db should initialize");

        let first = save_active_workspace(&conn, "C:\\Users\\AIT\\Desktop\\MDGA")
            .expect("workspace should save");
        assert_eq!(first.name, "MDGA");
        assert_eq!(first.path, "C:\\Users\\AIT\\Desktop\\MDGA");

        let second = save_active_workspace(&conn, "C:\\Users\\AIT\\Desktop\\Other")
            .expect("workspace should replace");
        let loaded = get_active_workspace(&conn).expect("workspace should load");

        assert_eq!(loaded.map(|workspace| workspace.path), Some(second.path));
        assert_ne!(first.id, second.id);

        clear_active_workspace(&conn).expect("workspace should clear");
        assert!(get_active_workspace(&conn).expect("query should succeed").is_none());

        let _ = std::fs::remove_file(db_path);
    }
}
