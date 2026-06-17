//! R1 端到端冒烟测试：在临时 Cargo crate 里真跑 lsp_* 工具（拉起真实 rust-analyzer）。
//! 需系统已安装 rust-analyzer；缺失则**优雅跳过**（镜像 git_smoke.rs 的 git_available 模式）。

use mdga_lsp::{
    lsp_definition, lsp_diagnostics, lsp_hover, lsp_references, LspDiagnosticsRequest,
    LspPositionRequest,
};
use std::path::PathBuf;
use std::process::Command;

/// rust-analyzer 是否可用（`--version` 成功）。不可用则测试跳过，不算失败。
fn rust_analyzer_available() -> bool {
    Command::new("rust-analyzer")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn unique_tmp() -> PathBuf {
    let mut dir = std::env::temp_dir();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.push(format!("mdga_lsp_smoke_{}_{}", std::process::id(), stamp));
    std::fs::create_dir_all(dir.join("src")).expect("create tmp crate dirs");
    dir
}

/// 搭一个最小可索引的 Cargo crate：Cargo.toml + src/lib.rs（含一个函数与一处调用）。
fn scaffold_crate(dir: &std::path::Path) {
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"smoke_target\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    )
    .expect("write Cargo.toml");
    // add 定义在第 0 行；在 use_it 里第 5 行第 4 列处调用 add。
    let lib = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub fn use_it() -> i32 {\n    add(1, 2)\n}\n";
    std::fs::write(dir.join("src").join("lib.rs"), lib).expect("write lib.rs");
}

#[test]
fn lsp_tools_end_to_end() {
    if !rust_analyzer_available() {
        eprintln!("跳过：系统未安装 rust-analyzer");
        return;
    }
    let dir = unique_tmp();
    scaffold_crate(&dir);

    // hover 在 add 的定义处（第 0 行第 7 列 = "add" 标识符）。rust-analyzer 应给出函数签名。
    let hover = lsp_hover(
        &dir,
        LspPositionRequest {
            path: "src/lib.rs".to_string(),
            line: 0,
            character: 7,
        },
    );
    let hover = hover.expect("lsp_hover 调用应成功");
    eprintln!("hover found={} contents={:?}", hover.found, hover.contents);
    // request_until_ready 会等索引就绪，因此应能拿到 add 的真实签名。
    assert!(hover.found, "应取到 hover 信息");
    assert!(
        hover.contents.contains("fn add"),
        "hover 应含 add 的签名，实际: {}",
        hover.contents
    );

    // definition：在 use_it 里调用点（第 5 行 "add(" 的 a）跳到 add 定义（第 0 行）。
    let def = lsp_definition(
        &dir,
        LspPositionRequest {
            path: "src/lib.rs".to_string(),
            line: 5,
            character: 4,
        },
    )
    .expect("lsp_definition 调用应成功");
    eprintln!("definition count={} {:?}", def.count, def.locations);
    assert_eq!(def.count, 1, "add 应有唯一定义");
    assert_eq!(def.locations[0].path, "src/lib.rs");
    assert_eq!(def.locations[0].line, 0, "add 定义在第 0 行");

    // references：add 的引用（含声明）——声明 + use_it 里的调用，共 2 处。
    let refs = lsp_references(
        &dir,
        LspPositionRequest {
            path: "src/lib.rs".to_string(),
            line: 0,
            character: 7,
        },
    )
    .expect("lsp_references 调用应成功");
    eprintln!("references count={}", refs.count);
    assert!(refs.count >= 2, "add 应至少有声明+调用 2 处引用，实际 {}", refs.count);

    // diagnostics：干净文件应能取到诊断列表（可能为空），调用须成功。
    let diags = lsp_diagnostics(
        &dir,
        LspDiagnosticsRequest {
            path: "src/lib.rs".to_string(),
        },
    )
    .expect("lsp_diagnostics 调用应成功");
    assert_eq!(diags.path, "src/lib.rs");
    eprintln!("diagnostics count={}", diags.count);

    // 路径逃逸防护：越界路径被拒（不依赖 rust-analyzer 行为）。
    assert!(lsp_hover(
        &dir,
        LspPositionRequest {
            path: "../escape.rs".to_string(),
            line: 0,
            character: 0,
        }
    )
    .is_err());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 不依赖 rust-analyzer：未知扩展名应得到清晰错误而非挂死。
#[test]
fn unsupported_extension_errors_fast() {
    let dir = unique_tmp();
    std::fs::write(dir.join("README.md"), "# hi\n").expect("write md");
    let err = lsp_hover(
        &dir,
        LspPositionRequest {
            path: "README.md".to_string(),
            line: 0,
            character: 0,
        },
    );
    assert!(err.is_err(), "未知扩展名应报错");
    let _ = std::fs::remove_dir_all(&dir);
}
