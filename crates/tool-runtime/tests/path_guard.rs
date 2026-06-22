//! tool-runtime 路径守卫端到端集成测试（Plan28 P2-7）。
//!
//! 重点覆盖各文件操作（create/write/edit/delete/move/delete_dir/make_dir/read/list/stat）
//! 的**越界拒绝**与错误路径：绝对路径、`..` 穿越、空路径、目标已存在拒覆盖、删根拒绝、
//! 非空目录拒删等。除断言返回 Err 外，还断言**文件系统未被改动**（不创建越界文件、
//! 不误删/误覆盖既有文件），均在独立临时工作区内进行，测完自动清理。
//!
//! 与 src/lib.rs 内联单测互补：内联单测主要覆盖 create_file 的少量拒绝路径与正常路径，
//! 此处系统性覆盖全部写类工具的越界/冲突错误分支与「不动文件系统」不变量。

use mdga_tool_runtime::*;

/// 临时工作区守卫：构造唯一目录，Drop 时递归清理。
struct TempWorkspace {
    path: std::path::PathBuf,
}

impl TempWorkspace {
    fn new() -> Self {
        // 用纳秒 + 进程内计数构造唯一名，避免并行用例碰撞；不引入额外 dev-dep。
        use std::sync::atomic::{AtomicU64, Ordering};
        static CTR: AtomicU64 = AtomicU64::new(0);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("时间应可用")
            .as_nanos();
        let n = CTR.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("mdga-tr-guard-{nonce}-{n}"));
        std::fs::create_dir_all(&path).expect("工作区应可创建");
        TempWorkspace { path }
    }

    fn root(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ── create_file 越界 / 冲突 ───────────────────────────────────────────────

/// 绝对路径被拒，且不在工作区内留下文件。
#[test]
fn create_file_rejects_absolute_path() {
    let ws = TempWorkspace::new();
    let abs = ws.root().join("abs.txt");

    let err = create_file(
        ws.root(),
        CreateFileRequest { path: abs.to_string_lossy().to_string(), content: "x".into() },
    )
    .expect_err("绝对路径应被拒");
    assert!(matches!(err, ToolRuntimeError::PathOutsideWorkspace));
    // 文件系统未被改动：目标文件不存在。
    assert!(!abs.exists());
}

/// `..` 穿越被拒，且父目录里不会被写出逃逸文件。
#[test]
fn create_file_rejects_parent_traversal() {
    let ws = TempWorkspace::new();

    let err = create_file(
        ws.root(),
        CreateFileRequest { path: "../escape.txt".into(), content: "x".into() },
    )
    .expect_err(".. 穿越应被拒");
    assert!(matches!(err, ToolRuntimeError::PathOutsideWorkspace));
    // 逃逸目标（工作区父目录下）不应被创建。
    assert!(!ws.root().parent().unwrap().join("escape.txt").exists());
}

/// 嵌套 `..` 穿越（合法前缀后再逃逸）同样被拒。
#[test]
fn create_file_rejects_nested_parent_traversal() {
    let ws = TempWorkspace::new();

    let err = create_file(
        ws.root(),
        CreateFileRequest { path: "sub/../../escape.txt".into(), content: "x".into() },
    )
    .expect_err("嵌套 .. 穿越应被拒");
    assert!(matches!(err, ToolRuntimeError::PathOutsideWorkspace));
}

/// 空路径被拒。
#[test]
fn create_file_rejects_empty_path() {
    let ws = TempWorkspace::new();
    let err = create_file(
        ws.root(),
        CreateFileRequest { path: "   ".into(), content: "x".into() },
    )
    .expect_err("空路径应被拒");
    assert!(matches!(err, ToolRuntimeError::EmptyPath));
}

/// 目标已存在时拒绝覆盖，且原文件内容保持不变。
#[test]
fn create_file_rejects_overwrite_existing() {
    let ws = TempWorkspace::new();
    // 预置一个文件。
    create_file(ws.root(), CreateFileRequest { path: "a.txt".into(), content: "原始".into() })
        .expect("预置文件");

    let err = create_file(
        ws.root(),
        CreateFileRequest { path: "a.txt".into(), content: "新内容".into() },
    )
    .expect_err("已存在应拒覆盖");
    assert!(matches!(err, ToolRuntimeError::FileAlreadyExists));
    // 文件系统未被改动：内容仍是原始。
    assert_eq!(std::fs::read_to_string(ws.root().join("a.txt")).unwrap(), "原始");
}

// ── write_file 越界 ───────────────────────────────────────────────────────

/// write_file 绝对路径被拒，外部文件不被覆盖。
#[test]
fn write_file_rejects_absolute_path() {
    let ws = TempWorkspace::new();
    let abs = ws.root().join("w.txt");
    let err = write_file(
        ws.root(),
        WriteFileRequest { path: abs.to_string_lossy().to_string(), content: "x".into() },
    )
    .expect_err("绝对路径应被拒");
    assert!(matches!(err, ToolRuntimeError::PathOutsideWorkspace));
    assert!(!abs.exists());
}

/// write_file `..` 穿越被拒。
#[test]
fn write_file_rejects_parent_traversal() {
    let ws = TempWorkspace::new();
    let err = write_file(
        ws.root(),
        WriteFileRequest { path: "../w.txt".into(), content: "x".into() },
    )
    .expect_err(".. 穿越应被拒");
    assert!(matches!(err, ToolRuntimeError::PathOutsideWorkspace));
    assert!(!ws.root().parent().unwrap().join("w.txt").exists());
}

// ── edit_file 越界 / 错误分支 ─────────────────────────────────────────────

/// edit_file 对工作区外路径报「不存在」（越界前先因目标不存在被拒），不改动任何文件。
#[test]
fn edit_file_rejects_traversal_path() {
    let ws = TempWorkspace::new();
    let err = edit_file(
        ws.root(),
        EditFileRequest {
            path: "../outside.txt".into(),
            old_text: "a".into(),
            new_text: "b".into(),
            replace_all: false,
        },
    )
    .expect_err("越界/不存在应被拒");
    // 越界路径在 resolve_existing_path 中先被 validate 拒为 PathOutsideWorkspace。
    assert!(matches!(err, ToolRuntimeError::PathOutsideWorkspace));
}

/// edit_file old_text 为空被拒，文件不被改动。
#[test]
fn edit_file_rejects_empty_old_text() {
    let ws = TempWorkspace::new();
    create_file(ws.root(), CreateFileRequest { path: "a.txt".into(), content: "abc".into() })
        .expect("预置");
    let err = edit_file(
        ws.root(),
        EditFileRequest {
            path: "a.txt".into(),
            old_text: String::new(),
            new_text: "x".into(),
            replace_all: false,
        },
    )
    .expect_err("空 old_text 应被拒");
    assert!(matches!(err, ToolRuntimeError::EmptyOldText));
    assert_eq!(std::fs::read_to_string(ws.root().join("a.txt")).unwrap(), "abc");
}

/// edit_file old_text 出现多次且未启用 replace_all 时拒绝，文件保持不变。
#[test]
fn edit_file_rejects_non_unique_without_replace_all() {
    let ws = TempWorkspace::new();
    create_file(ws.root(), CreateFileRequest { path: "a.txt".into(), content: "x x x".into() })
        .expect("预置");
    let err = edit_file(
        ws.root(),
        EditFileRequest {
            path: "a.txt".into(),
            old_text: "x".into(),
            new_text: "y".into(),
            replace_all: false,
        },
    )
    .expect_err("多次匹配且未 replace_all 应被拒");
    assert!(matches!(err, ToolRuntimeError::PatternNotUnique));
    // 文件未被改动。
    assert_eq!(std::fs::read_to_string(ws.root().join("a.txt")).unwrap(), "x x x");
}

/// edit_file 找不到 old_text 时报 PatternNotFound，文件不变。
#[test]
fn edit_file_pattern_not_found() {
    let ws = TempWorkspace::new();
    create_file(ws.root(), CreateFileRequest { path: "a.txt".into(), content: "abc".into() })
        .expect("预置");
    let err = edit_file(
        ws.root(),
        EditFileRequest {
            path: "a.txt".into(),
            old_text: "zzz".into(),
            new_text: "y".into(),
            replace_all: false,
        },
    )
    .expect_err("未命中应被拒");
    assert!(matches!(err, ToolRuntimeError::PatternNotFound));
    assert_eq!(std::fs::read_to_string(ws.root().join("a.txt")).unwrap(), "abc");
}

// ── delete_file 越界 / 错误分支 ───────────────────────────────────────────

/// delete_file 越界路径被拒。
#[test]
fn delete_file_rejects_traversal_path() {
    let ws = TempWorkspace::new();
    let err = delete_file(ws.root(), DeleteFileRequest { path: "../x.txt".into() })
        .expect_err("越界应被拒");
    assert!(matches!(err, ToolRuntimeError::PathOutsideWorkspace));
}

/// delete_file 目标不存在报 PathNotFound。
#[test]
fn delete_file_missing_target() {
    let ws = TempWorkspace::new();
    let err = delete_file(ws.root(), DeleteFileRequest { path: "nope.txt".into() })
        .expect_err("不存在应被拒");
    assert!(matches!(err, ToolRuntimeError::PathNotFound));
}

/// delete_file 拒绝删除目录（只删文件），目录仍在。
#[test]
fn delete_file_rejects_directory() {
    let ws = TempWorkspace::new();
    std::fs::create_dir_all(ws.root().join("sub")).expect("建目录");
    let err = delete_file(ws.root(), DeleteFileRequest { path: "sub".into() })
        .expect_err("删目录应被拒");
    assert!(matches!(err, ToolRuntimeError::NotAFile));
    assert!(ws.root().join("sub").is_dir());
}

// ── move_path 越界 / 冲突 ─────────────────────────────────────────────────

/// move_path 目标越界（绝对路径）被拒，源文件保持原位。
#[test]
fn move_path_rejects_absolute_destination() {
    let ws = TempWorkspace::new();
    create_file(ws.root(), CreateFileRequest { path: "src.txt".into(), content: "data".into() })
        .expect("预置源");
    let abs_to = ws.root().join("dst.txt");
    let err = move_path(
        ws.root(),
        MovePathRequest {
            from: "src.txt".into(),
            to: abs_to.to_string_lossy().to_string(),
        },
    )
    .expect_err("绝对目标应被拒");
    assert!(matches!(err, ToolRuntimeError::PathOutsideWorkspace));
    // 源仍在、内容不变。
    assert_eq!(std::fs::read_to_string(ws.root().join("src.txt")).unwrap(), "data");
}

/// move_path 目标已存在拒覆盖，源与目标都不被改动。
#[test]
fn move_path_rejects_existing_destination() {
    let ws = TempWorkspace::new();
    create_file(ws.root(), CreateFileRequest { path: "src.txt".into(), content: "源".into() })
        .expect("预置源");
    create_file(ws.root(), CreateFileRequest { path: "dst.txt".into(), content: "目标".into() })
        .expect("预置目标");
    let err = move_path(
        ws.root(),
        MovePathRequest { from: "src.txt".into(), to: "dst.txt".into() },
    )
    .expect_err("目标已存在应拒覆盖");
    assert!(matches!(err, ToolRuntimeError::FileAlreadyExists));
    // 两边都没被动。
    assert_eq!(std::fs::read_to_string(ws.root().join("src.txt")).unwrap(), "源");
    assert_eq!(std::fs::read_to_string(ws.root().join("dst.txt")).unwrap(), "目标");
}

/// move_path 源不存在报 PathNotFound。
#[test]
fn move_path_missing_source() {
    let ws = TempWorkspace::new();
    let err = move_path(
        ws.root(),
        MovePathRequest { from: "nope.txt".into(), to: "dst.txt".into() },
    )
    .expect_err("源不存在应被拒");
    assert!(matches!(err, ToolRuntimeError::PathNotFound));
    assert!(!ws.root().join("dst.txt").exists());
}

// ── delete_dir 越界 / 安全护栏 ────────────────────────────────────────────

/// delete_dir 拒绝删除工作区根目录，根目录仍在。
#[test]
fn delete_dir_rejects_workspace_root() {
    let ws = TempWorkspace::new();
    let err = delete_dir(ws.root(), DeleteDirRequest { path: ".".into(), recursive: true })
        .expect_err("删根应被拒");
    assert!(matches!(err, ToolRuntimeError::CannotDeleteWorkspaceRoot));
    assert!(ws.root().is_dir());
}

/// delete_dir 非空目录未给 recursive 时拒删，目录及其内容保持不变。
#[test]
fn delete_dir_rejects_non_empty_without_recursive() {
    let ws = TempWorkspace::new();
    std::fs::create_dir_all(ws.root().join("sub")).expect("建目录");
    create_file(ws.root(), CreateFileRequest { path: "sub/f.txt".into(), content: "keep".into() })
        .expect("放文件");
    let err = delete_dir(ws.root(), DeleteDirRequest { path: "sub".into(), recursive: false })
        .expect_err("非空且非递归应被拒");
    assert!(matches!(err, ToolRuntimeError::DirectoryNotEmpty));
    // 目录与文件都还在。
    assert!(ws.root().join("sub").is_dir());
    assert_eq!(std::fs::read_to_string(ws.root().join("sub/f.txt")).unwrap(), "keep");
}

/// delete_dir 越界路径被拒。
#[test]
fn delete_dir_rejects_traversal() {
    let ws = TempWorkspace::new();
    let err = delete_dir(ws.root(), DeleteDirRequest { path: "../sub".into(), recursive: true })
        .expect_err("越界应被拒");
    assert!(matches!(err, ToolRuntimeError::PathOutsideWorkspace));
}

// ── make_dir / read_file / list_dir / stat_path 越界 ──────────────────────

/// make_dir 越界（绝对路径）被拒，不在工作区外创建目录。
#[test]
fn make_dir_rejects_absolute_path() {
    let ws = TempWorkspace::new();
    let abs = ws.root().join("d");
    let err = make_dir(
        ws.root(),
        MakeDirRequest { path: abs.to_string_lossy().to_string() },
    )
    .expect_err("绝对路径应被拒");
    assert!(matches!(err, ToolRuntimeError::PathOutsideWorkspace));
    assert!(!abs.exists());
}

/// read_file 越界路径被拒。
#[test]
fn read_file_rejects_traversal() {
    let ws = TempWorkspace::new();
    let err = read_file(
        ws.root(),
        ReadFileRequest { path: "../secret.txt".into(), offset: 0, limit: 0 },
    )
    .expect_err("越界应被拒");
    assert!(matches!(err, ToolRuntimeError::PathOutsideWorkspace));
}

/// list_dir 越界路径被拒。
#[test]
fn list_dir_rejects_traversal() {
    let ws = TempWorkspace::new();
    let err = list_dir(ws.root(), ListDirRequest { path: "..".into() })
        .expect_err("越界应被拒");
    assert!(matches!(err, ToolRuntimeError::PathOutsideWorkspace));
}

/// stat_path 越界路径被拒。
#[test]
fn stat_path_rejects_traversal() {
    let ws = TempWorkspace::new();
    let err = stat_path(ws.root(), StatPathRequest { path: "../x".into() })
        .expect_err("越界应被拒");
    assert!(matches!(err, ToolRuntimeError::PathOutsideWorkspace));
}

// ── 正常路径冒烟：确认守卫不误伤合法操作（端到端写读一致） ────────────────

// ── 只读文件树面板后端：list_workspace_children / read_workspace_text 守卫 ──

/// list_workspace_children 越界（`..` 穿越）被拒（复用 resolve_existing_path 守卫）。
#[test]
fn list_workspace_children_rejects_traversal() {
    let ws = TempWorkspace::new();
    let err = list_workspace_children(ws.root(), "..").expect_err("越界应被拒");
    assert!(matches!(err, ToolRuntimeError::PathOutsideWorkspace));
}

/// list_workspace_children 绝对路径被拒。
#[test]
fn list_workspace_children_rejects_absolute() {
    let ws = TempWorkspace::new();
    let abs = ws.root().join("sub").to_string_lossy().to_string();
    let err = list_workspace_children(ws.root(), &abs).expect_err("绝对路径应被拒");
    assert!(matches!(err, ToolRuntimeError::PathOutsideWorkspace));
}

/// list_workspace_children 空串 / "." 都列出工作区根的直接子项（惰性一层）。
#[test]
fn list_workspace_children_lists_root() {
    let ws = TempWorkspace::new();
    create_file(ws.root(), CreateFileRequest { path: "a.txt".into(), content: "x".into() })
        .expect("文件");
    std::fs::create_dir_all(ws.root().join("dir")).expect("目录");
    // 子目录里的文件不应出现在根列表（仅一层）。
    create_file(ws.root(), CreateFileRequest { path: "dir/nested.txt".into(), content: "y".into() })
        .expect("嵌套文件");

    for key in ["", "."] {
        let mut got = list_workspace_children(ws.root(), key).expect("应列出根");
        got.sort();
        assert_eq!(
            got,
            vec![("a.txt".to_string(), false), ("dir".to_string(), true)],
            "key={key:?} 应只列出根的直接子项"
        );
    }
}

/// read_workspace_text 越界路径被拒。
#[test]
fn read_workspace_text_rejects_traversal() {
    let ws = TempWorkspace::new();
    let err = read_workspace_text(ws.root(), "../secret.txt", 512 * 1024)
        .expect_err("越界应被拒");
    assert!(matches!(err, ToolRuntimeError::PathOutsideWorkspace));
}

/// read_workspace_text 超过字节上限返回 FileTooLarge。
#[test]
fn read_workspace_text_enforces_byte_cap() {
    let ws = TempWorkspace::new();
    create_file(ws.root(), CreateFileRequest { path: "big.txt".into(), content: "abcdef".into() })
        .expect("文件");
    let err = read_workspace_text(ws.root(), "big.txt", 4).expect_err("超限应被拒");
    assert!(matches!(err, ToolRuntimeError::FileTooLarge(4)));
}

/// read_workspace_text 读取合法文件返回完整内容。
#[test]
fn read_workspace_text_reads_full_content() {
    let ws = TempWorkspace::new();
    let body = "第一行\n第二行\n第三行";
    create_file(ws.root(), CreateFileRequest { path: "doc.txt".into(), content: body.into() })
        .expect("文件");
    let got = read_workspace_text(ws.root(), "doc.txt", 512 * 1024).expect("应读出");
    assert_eq!(got, body);
}

/// 合法相对路径 create → read 往返一致，且嵌套父目录被按需创建。
#[test]
fn legal_create_then_read_roundtrip() {
    let ws = TempWorkspace::new();
    let created = create_file(
        ws.root(),
        CreateFileRequest { path: "nested/dir/file.txt".into(), content: "你好 MDGA".into() },
    )
    .expect("合法创建应成功");
    assert_eq!(created.relative_path, "nested/dir/file.txt");
    assert!(ws.root().join("nested/dir/file.txt").is_file());

    let read = read_file(
        ws.root(),
        ReadFileRequest { path: "nested/dir/file.txt".into(), offset: 0, limit: 0 },
    )
    .expect("合法读取应成功");
    assert_eq!(read.content, "你好 MDGA");
    assert_eq!(read.total_lines, 1);
}
