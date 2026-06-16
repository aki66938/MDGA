//! storage crate 的端到端集成测试（Plan28 P2-7）。
//!
//! 用真实 SQLite 临时库覆盖各主要持久化路径的往返一致性：会话 CRUD（含
//! 置顶/归档/改名/删除）、消息保存与读取、model_provider upsert/get（含 context_window）、
//! token_ledger、文件检查点、active workspace 等。每个用例用独立临时文件库，测完自动清理。
//!
//! 这些用例与 src/lib.rs 内联单测互补：内联单测偏单点行为，此处偏「多表协同的端到端流程」
//! 与字段往返一致，确保写入后读出字段逐一相等、更新生效、删除生效。

use mdga_storage::*;
use rusqlite::Connection;
use std::path::PathBuf;
use uuid::Uuid;

/// 返回一个全局唯一的临时数据库文件路径，并附带测试结束时自动删除的守卫。
///
/// 用真实文件（而非内存库）以贴近生产 init_db 的 WAL/外键 PRAGMA 路径；
/// TempDb 在 Drop 时连同 WAL/SHM 旁文件一并清理，保证不留临时残留。
struct TempDb {
    path: PathBuf,
}

impl TempDb {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("mdga-storage-it-{}.db", Uuid::new_v4()));
        TempDb { path }
    }

    /// 建库并返回连接（首次调用建表，幂等）。
    fn open(&self) -> Connection {
        init_db(&self.path).expect("init_db 应建库成功")
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        // SQLite WAL 模式会留下 -wal / -shm 旁文件，一并清理。
        let _ = std::fs::remove_file(&self.path);
        let mut wal = self.path.clone().into_os_string();
        wal.push("-wal");
        let _ = std::fs::remove_file(&wal);
        let mut shm = self.path.clone().into_os_string();
        shm.push("-shm");
        let _ = std::fs::remove_file(&shm);
    }
}

/// init_db 幂等：对同一文件重复调用不报错，已写入的数据仍在。
#[test]
fn init_db_is_idempotent() {
    let db = TempDb::new();
    let conn = db.open();
    let conv = create_conversation(&conn).expect("建会话");
    drop(conn);

    // 第二次 init_db 走 add_column_if_missing 全路径，应幂等且保留旧数据。
    let conn2 = db.open();
    let list = list_conversations(&conn2).expect("列会话");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, conv.id);
}

/// 会话完整生命周期：create → get → list → rename → pin → archive → delete。
#[test]
fn conversation_full_lifecycle() {
    let db = TempDb::new();
    let conn = db.open();

    // create：默认纯聊天，标题「新对话」，未置顶未归档。
    let conv = create_conversation(&conn).expect("建会话");
    assert_eq!(conv.title, "新对话");
    assert_eq!(conv.mode, "chat_only");
    assert!(!conv.pinned);
    assert!(!conv.archived);

    // get：按 ID 读回，字段与 create 返回一致（往返一致）。
    let got = get_conversation(&conn, &conv.id).expect("查会话").expect("应存在");
    assert_eq!(got.id, conv.id);
    assert_eq!(got.title, conv.title);
    assert_eq!(got.mode, conv.mode);
    assert_eq!(got.created_at, conv.created_at);

    // list：含这一条。
    assert_eq!(list_conversations(&conn).expect("列会话").len(), 1);

    // rename：标题更新生效，updated_at 不回退。
    update_title(&conn, &conv.id, "重命名后的标题").expect("改名");
    let renamed = get_conversation(&conn, &conv.id).expect("查").expect("存在");
    assert_eq!(renamed.title, "重命名后的标题");
    assert!(renamed.updated_at >= conv.updated_at);

    // pin：置顶生效。
    set_conversation_pinned(&conn, &conv.id, true).expect("置顶");
    assert!(get_conversation(&conn, &conv.id).expect("查").expect("存在").pinned);
    set_conversation_pinned(&conn, &conv.id, false).expect("取消置顶");
    assert!(!get_conversation(&conn, &conv.id).expect("查").expect("存在").pinned);

    // archive：归档生效（不删除数据，仍可 get/list 到）。
    set_conversation_archived(&conn, &conv.id, true).expect("归档");
    let archived = get_conversation(&conn, &conv.id).expect("查").expect("存在");
    assert!(archived.archived);
    assert_eq!(list_conversations(&conn).expect("列").len(), 1);

    // delete：删除生效，get 返回 None，list 为空。
    delete_conversation(&conn, &conv.id).expect("删会话");
    assert!(get_conversation(&conn, &conv.id).expect("查").is_none());
    assert!(list_conversations(&conn).expect("列").is_empty());
}

/// 置顶会话排在普通会话之前（list_conversations 的 ORDER BY pinned DESC）。
#[test]
fn pinned_conversations_sort_first() {
    let db = TempDb::new();
    let conn = db.open();

    let normal = create_conversation(&conn).expect("普通会话");
    let pinned = create_conversation(&conn).expect("置顶会话");
    set_conversation_pinned(&conn, &pinned.id, true).expect("置顶");

    let list = list_conversations(&conn).expect("列会话");
    assert_eq!(list.len(), 2);
    // 置顶在前。
    assert_eq!(list[0].id, pinned.id);
    assert_eq!(list[1].id, normal.id);
}

/// 消息保存与读取：往返一致（含 usage_json / parts_json），保存消息会刷新会话 updated_at。
#[test]
fn message_save_and_read_roundtrip() {
    let db = TempDb::new();
    let conn = db.open();
    let conv = create_conversation(&conn).expect("建会话");

    // 纯文字 user 消息（无 usage / parts）。
    save_message(&conn, &conv.id, "user", "你好，世界", None, None).expect("存 user 消息");
    // 带 usage_json 与 parts_json 的 assistant 消息。
    save_message(
        &conn,
        &conv.id,
        "assistant",
        "这是回复正文",
        Some(r#"{"totalTokens":42}"#),
        Some(r#"[{"type":"text","text":"这是回复正文"}]"#),
    )
    .expect("存 assistant 消息");

    let msgs = get_messages(&conn, &conv.id).expect("读消息");
    assert_eq!(msgs.len(), 2);

    // 时间正序：user 在前。
    assert_eq!(msgs[0].role, "user");
    assert_eq!(msgs[0].content, "你好，世界");
    assert_eq!(msgs[0].usage_json, None);
    assert_eq!(msgs[0].parts_json, None);
    assert_eq!(msgs[0].conversation_id, conv.id);

    // assistant 字段逐一往返一致。
    assert_eq!(msgs[1].role, "assistant");
    assert_eq!(msgs[1].content, "这是回复正文");
    assert_eq!(msgs[1].usage_json.as_deref(), Some(r#"{"totalTokens":42}"#));
    assert_eq!(
        msgs[1].parts_json.as_deref(),
        Some(r#"[{"type":"text","text":"这是回复正文"}]"#)
    );

    // 保存消息刷新了会话 updated_at（用于列表「最近」排序）。
    let after = get_conversation(&conn, &conv.id).expect("查").expect("存在");
    assert!(after.updated_at >= conv.updated_at);

    // delete_messages：清空该会话消息，会话本身仍在。
    delete_messages(&conn, &conv.id).expect("删消息");
    assert!(get_messages(&conn, &conv.id).expect("读").is_empty());
    assert!(get_conversation(&conn, &conv.id).expect("查").is_some());
}

/// 删除会话级联删除其消息（ON DELETE CASCADE + foreign_keys=ON）。
#[test]
fn deleting_conversation_cascades_messages() {
    let db = TempDb::new();
    let conn = db.open();
    let conv = create_conversation(&conn).expect("建会话");
    save_message(&conn, &conv.id, "user", "一条消息", None, None).expect("存消息");
    assert_eq!(get_messages(&conn, &conv.id).expect("读").len(), 1);

    delete_conversation(&conn, &conv.id).expect("删会话");
    // 级联：消息也应随之消失。
    assert!(get_messages(&conn, &conv.id).expect("读").is_empty());
}

/// model_provider upsert/get 端到端，重点覆盖 context_window 与 api_format 的往返与覆盖语义。
#[test]
fn model_provider_upsert_get_with_context_window() {
    let db = TempDb::new();
    let conn = db.open();

    // 首次写入 main：context_window 显式给值，base_url 留空走官方。
    let p1 = upsert_model_provider(
        &conn,
        "main",
        Some("deepseek"),
        Some("DeepSeek"),
        None,
        "sk-key-1",
        "deepseek-chat",
        "openai",
        Some(128_000),
    )
    .expect("upsert main");
    assert_eq!(p1.role, "main");
    assert_eq!(p1.context_window, Some(128_000));
    assert!(p1.enabled);
    assert!(p1.updated_at.is_some());

    // get 读回，字段逐一往返一致。
    let got = get_model_provider(&conn, "main").expect("查").expect("存在");
    assert_eq!(got.api_key, "sk-key-1");
    assert_eq!(got.model_id, "deepseek-chat");
    assert_eq!(got.preset.as_deref(), Some("deepseek"));
    assert_eq!(got.base_url, None);
    assert_eq!(got.api_format, "openai");
    assert_eq!(got.context_window, Some(128_000));

    // 同 role 再次 upsert：覆盖为唯一一条，context_window 可清空为 None。
    upsert_model_provider(
        &conn,
        "main",
        Some("custom"),
        Some("自托管"),
        Some("https://proxy.local/v1"),
        "sk-key-2",
        "my-model",
        "openai",
        None,
    )
    .expect("覆盖 main");
    let updated = get_model_provider(&conn, "main").expect("查").expect("存在");
    assert_eq!(updated.api_key, "sk-key-2");
    assert_eq!(updated.base_url.as_deref(), Some("https://proxy.local/v1"));
    assert_eq!(updated.context_window, None);
    // 仍只有一条 main。
    assert_eq!(
        list_model_providers(&conn)
            .expect("列")
            .iter()
            .filter(|p| p.role == "main")
            .count(),
        1
    );

    // vision provider（anthropic 格式 + 200k 窗口），独立于 main 存在。
    upsert_model_provider(
        &conn,
        "vision",
        Some("custom"),
        Some("Claude Vision"),
        Some("https://api.anthropic.com"),
        "sk-v",
        "claude-3-5-sonnet",
        "anthropic",
        Some(200_000),
    )
    .expect("upsert vision");
    let vision = get_model_provider(&conn, "vision").expect("查").expect("存在");
    assert_eq!(vision.api_format, "anthropic");
    assert_eq!(vision.context_window, Some(200_000));
    assert_eq!(list_model_providers(&conn).expect("列").len(), 2);

    // 删除 vision，仅留 main。
    delete_model_provider(&conn, "vision").expect("删 vision");
    assert!(get_model_provider(&conn, "vision").expect("查").is_none());
    assert_eq!(list_model_providers(&conn).expect("列").len(), 1);
}

/// token_ledger 独立账本：写入后按会话读出，往返一致、按时间正序，且按会话隔离。
#[test]
fn token_ledger_save_and_read() {
    let db = TempDb::new();
    let conn = db.open();
    let conv = create_conversation(&conn).expect("建会话");
    let other = create_conversation(&conn).expect("另一会话");

    save_token_ledger_entry(&conn, &conv.id, "vision", r#"{"totalTokens":10}"#).expect("入账 1");
    save_token_ledger_entry(&conn, &conv.id, "audio", r#"{"totalTokens":20}"#).expect("入账 2");
    // 另一会话单独入账，验证按会话隔离。
    save_token_ledger_entry(&conn, &other.id, "vision", r#"{"totalTokens":99}"#).expect("入账 other");

    let entries = get_token_ledger_entries(&conn, &conv.id).expect("读账本");
    assert_eq!(entries.len(), 2);
    // 时间正序 + 字段往返一致。
    assert_eq!(entries[0].kind, "vision");
    assert_eq!(entries[0].usage_json, r#"{"totalTokens":10}"#);
    assert_eq!(entries[0].conversation_id, conv.id);
    assert_eq!(entries[1].kind, "audio");
    assert_eq!(entries[1].usage_json, r#"{"totalTokens":20}"#);

    // 隔离：other 会话只看到自己的一条。
    let other_entries = get_token_ledger_entries(&conn, &other.id).expect("读 other 账本");
    assert_eq!(other_entries.len(), 1);
    assert_eq!(other_entries[0].usage_json, r#"{"totalTokens":99}"#);
}

/// 文件检查点：序号单调递增、prev_content 往返（含 None）、标记回退生效。
#[test]
fn file_checkpoints_record_seq_and_revert() {
    let db = TempDb::new();
    let conn = db.open();
    let conv = create_conversation(&conn).expect("建会话");

    let c1 = record_file_checkpoint(
        &conn,
        &conv.id,
        "write_file",
        "src/a.txt",
        Some("旧内容"),
        Some(r#"{"note":"x"}"#),
        true,
    )
    .expect("检查点 1");
    let c2 = record_file_checkpoint(&conn, &conv.id, "create_file", "src/b.txt", None, None, true)
        .expect("检查点 2");
    // 会话内单调递增。
    assert_eq!(c1.seq, 1);
    assert_eq!(c2.seq, 2);

    // 标记 c2 已回退。
    mark_checkpoint_reverted(&conn, &c2.id).expect("标记回退");

    let list = list_file_checkpoints(&conn, &conv.id).expect("列检查点");
    assert_eq!(list.len(), 2);
    // 序号正序 + 字段往返一致。
    assert_eq!(list[0].id, c1.id);
    assert_eq!(list[0].rel_path, "src/a.txt");
    assert_eq!(list[0].prev_content.as_deref(), Some("旧内容"));
    assert_eq!(list[0].extra_json.as_deref(), Some(r#"{"note":"x"}"#));
    assert!(!list[0].reverted);
    // c2 的 prev_content 为 None（变更前文件不存在），回退标记生效。
    assert_eq!(list[1].prev_content, None);
    assert!(list[1].reverted);
}

/// active workspace：保存 → 读取 → 替换（只保留一条） → 清除。
#[test]
fn active_workspace_save_replace_clear() {
    let db = TempDb::new();
    let conn = db.open();

    // 初始无活动工作区。
    assert!(get_active_workspace(&conn).expect("查").is_none());

    // 保存：name 从路径末段提取。
    let first = save_active_workspace(&conn, "C:\\code\\projA").expect("保存工作区");
    assert_eq!(first.name, "projA");
    assert_eq!(first.path, "C:\\code\\projA");
    assert!(first.active);
    let loaded = get_active_workspace(&conn).expect("查").expect("存在");
    assert_eq!(loaded.path, first.path);

    // 替换：写新工作区会清掉旧记录，只保留一条；id 应变。
    let second = save_active_workspace(&conn, "C:\\code\\projB").expect("替换工作区");
    assert_ne!(first.id, second.id);
    let after = get_active_workspace(&conn).expect("查").expect("存在");
    assert_eq!(after.path, "C:\\code\\projB");
    assert_eq!(after.name, "projB");

    // 清除：解绑后读不到。
    clear_active_workspace(&conn).expect("清除工作区");
    assert!(get_active_workspace(&conn).expect("查").is_none());
}

/// 创建带工作区快照的会话：mode 切 local_workspace，路径/名称落库一致。
#[test]
fn create_conversation_with_workspace_snapshot() {
    let db = TempDb::new();
    let conn = db.open();

    let conv = create_conversation_with_workspace(
        &conn,
        Some("C:\\code\\projA"),
        Some("projA"),
    )
    .expect("建带工作区会话");
    assert_eq!(conv.mode, "local_workspace");

    let stored = get_conversation(&conn, &conv.id).expect("查").expect("存在");
    assert_eq!(stored.workspace_path.as_deref(), Some("C:\\code\\projA"));
    assert_eq!(stored.workspace_name.as_deref(), Some("projA"));
    assert_eq!(stored.mode, "local_workspace");

    // 改绑工作区后再解绑：mode 回 chat_only，路径清空。
    let unbound = update_conversation_workspace(&conn, &conv.id, None, None).expect("解绑");
    assert_eq!(unbound.mode, "chat_only");
    assert_eq!(unbound.workspace_path, None);
}

/// activity_events：记录后按会话读出，字段往返一致、按时间正序。
#[test]
fn activity_events_record_and_read() {
    let db = TempDb::new();
    let conn = db.open();
    let conv = create_conversation(&conn).expect("建会话");

    let ev = record_activity_event(
        &conn,
        &conv.id,
        "tool_call",
        Some("write_file"),
        "success",
        Some(r#"{"path":"a.txt"}"#),
        Some(r#"{"bytesWritten":3}"#),
        None,
        Some("C:\\code\\projA"),
    )
    .expect("记录事件");

    let events = get_activity_events(&conn, &conv.id).expect("读事件");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, ev.id);
    assert_eq!(events[0].event_type, "tool_call");
    assert_eq!(events[0].tool_name.as_deref(), Some("write_file"));
    assert_eq!(events[0].status, "success");
    assert_eq!(events[0].input_json.as_deref(), Some(r#"{"path":"a.txt"}"#));
    assert_eq!(events[0].output_json.as_deref(), Some(r#"{"bytesWritten":3}"#));
    assert_eq!(events[0].error_message, None);
    assert_eq!(events[0].workspace_path.as_deref(), Some("C:\\code\\projA"));
}

/// app_settings 与 permission_rules 的 key-value / 幂等写入端到端。
#[test]
fn settings_and_permission_rules() {
    let db = TempDb::new();
    let conn = db.open();

    // app_settings：缺省 None → set → upsert 覆盖。
    assert_eq!(get_setting(&conn, "theme").expect("读"), None);
    set_setting(&conn, "theme", "dark").expect("写");
    assert_eq!(get_setting(&conn, "theme").expect("读").as_deref(), Some("dark"));
    set_setting(&conn, "theme", "light").expect("覆盖");
    assert_eq!(get_setting(&conn, "theme").expect("读").as_deref(), Some("light"));

    // permission_rules：重复插入幂等，删除生效。
    add_permission_rule(&conn, "tool:write_file").expect("规则 1");
    add_permission_rule(&conn, "tool:write_file").expect("重复规则");
    add_permission_rule(&conn, "cmd:git push").expect("规则 2");
    let rules = list_permission_rules(&conn).expect("列规则");
    assert_eq!(rules.len(), 2);
    remove_permission_rule(&conn, "tool:write_file").expect("删规则");
    let after = list_permission_rules(&conn).expect("列规则");
    assert_eq!(after, vec!["cmd:git push".to_string()]);
}

/// mcp_servers：新增 → 列出 → 设 token → 启停 → 删除，字段往返一致。
#[test]
fn mcp_servers_crud() {
    let db = TempDb::new();
    let conn = db.open();

    let srv = add_mcp_server(&conn, "github", "npx -y @modelcontextprotocol/server-github", None)
        .expect("加 server");
    assert_eq!(srv.name, "github");
    assert!(srv.enabled);
    assert_eq!(srv.auth_token, None);

    // 设 token 后读回。
    set_mcp_server_token(&conn, &srv.id, "tok-123").expect("设 token");
    // 停用后读回。
    set_mcp_server_enabled(&conn, &srv.id, false).expect("停用");

    let list = list_mcp_servers(&conn).expect("列 server");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, srv.id);
    assert_eq!(list[0].auth_token.as_deref(), Some("tok-123"));
    assert!(!list[0].enabled);

    // 删除生效。
    remove_mcp_server(&conn, &srv.id).expect("删 server");
    assert!(list_mcp_servers(&conn).expect("列").is_empty());
}
