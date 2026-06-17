//! R1 端到端冒烟测试：在临时工程里真跑 lsp_* 工具（拉起真实语言服务器）。
//! 覆盖 Rust（rust-analyzer）、TypeScript（typescript-language-server）、Python（pyright-langserver）。
//! 任一服务器缺失则对应测试**优雅跳过**（镜像 git_smoke.rs 的 available 模式），绝不算失败。
//!
//! 可用性判定与库内一致：在 **PATH 目录**里 which 式解析二进制（含 Windows .exe/.cmd/.bat）。
//! 注意：直接 `Command::new("typescript-language-server")` 在 Windows 上找不到（npm 只装 .cmd 垫片，
//! 且 std 不查 PATHEXT），所以这里必须自己按 PATH+PATHEXT 解析——这也正是库 `which` 模块要解决的问题。

use mdga_lsp::{
    lsp_definition, lsp_diagnostics, lsp_hover, lsp_references, pool_pooled_count,
    pool_reap_idle_secs, LspDiagnosticsRequest, LspPositionRequest,
};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// 串行化「真拉起语言服务器」的 e2e 测试。
///
/// 原因：cargo test 默认并行，但这些 e2e 各自冷启动一个重量级服务器（rust-analyzer / node 工具链），
/// 并共享同一个**进程级会话池**。并发跑会(1)挤占 CPU 让索引超出重试窗口、(2)让池计数/回收断言互相打架。
/// 它们测的是功能正确性而非并发，故用一个进程内互斥锁让它们一个接一个跑——无需全局 `--test-threads=1`。
fn e2e_serial() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    // 锁中毒（前一个 e2e panic）不影响后续：取 inner 继续串行。
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// which 式：在 PATH 目录中解析程序名为绝对路径（Windows 追加 .exe/.cmd/.bat）。镜像库内 `which`。
fn which_in_path(program: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    let candidates: Vec<String> = if cfg!(windows) {
        if Path::new(program).extension().is_some() {
            vec![program.to_string()]
        } else {
            vec![
                format!("{program}.exe"),
                format!("{program}.cmd"),
                format!("{program}.bat"),
            ]
        }
    } else {
        vec![program.to_string()]
    };
    for dir in std::env::split_paths(&path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for name in &candidates {
            let full = dir.join(name);
            if full.is_file() {
                return Some(full);
            }
        }
    }
    None
}

/// 某服务器二进制是否在 PATH 中可解析（与库的「未安装则 ServerUnavailable」判据一致）。
/// 用「能否解析到」而非「--version 成功」作为闸门：pyright 的 --version 退出码非零，不可靠。
fn server_available(program: &str) -> bool {
    which_in_path(program).is_some()
}

/// rust-analyzer 是否可用。不可用则测试跳过，不算失败。
fn rust_analyzer_available() -> bool {
    server_available("rust-analyzer")
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
    let _serial = e2e_serial();
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

// ── TypeScript e2e（typescript-language-server）────────────────────────────────

/// TypeScript 端到端：scaffold 一个含 `add` 函数 + 调用的 .ts，断言 hover 给出签名、definition 解析到定义。
/// typescript-language-server 缺失则优雅跳过。
#[test]
fn lsp_typescript_end_to_end() {
    if !server_available("typescript-language-server") {
        eprintln!("跳过：PATH 中未找到 typescript-language-server");
        return;
    }
    let _serial = e2e_serial();
    let dir = unique_tmp();
    // add 定义在第 0 行；在第 4 行第 2 列处调用 add。tsconfig 帮助服务器把目录当成项目。
    let ts = "export function add(a: number, b: number): number {\n    return a + b;\n}\n\nadd(1, 2);\n";
    std::fs::write(dir.join("app.ts"), ts).expect("write app.ts");
    std::fs::write(
        dir.join("tsconfig.json"),
        "{\n  \"compilerOptions\": { \"strict\": true, \"target\": \"ES2020\", \"module\": \"ESNext\" },\n  \"include\": [\"*.ts\"]\n}\n",
    )
    .expect("write tsconfig.json");

    // hover 在 add 定义处（第 0 行，"add" 标识符在第 16 列：'export function ' 长 16）。
    let hover = lsp_hover(
        &dir,
        LspPositionRequest {
            path: "app.ts".to_string(),
            line: 0,
            character: 16,
        },
    )
    .expect("lsp_hover(ts) 应成功");
    eprintln!("ts hover found={} contents={:?}", hover.found, hover.contents);
    assert!(hover.found, "ts 应取到 hover 信息");
    // tsserver 的 hover 含签名，如 "function add(a: number, b: number): number"。
    assert!(
        hover.contents.contains("add") && hover.contents.contains("number"),
        "ts hover 应含 add 的签名，实际: {}",
        hover.contents
    );

    // definition：在第 4 行调用点（"add(1,2)" 的 a，第 0 列）跳到第 0 行的定义。
    let def = lsp_definition(
        &dir,
        LspPositionRequest {
            path: "app.ts".to_string(),
            line: 4,
            character: 0,
        },
    )
    .expect("lsp_definition(ts) 应成功");
    eprintln!("ts definition count={} {:?}", def.count, def.locations);
    assert!(def.count >= 1, "add 应能解析到定义，实际 {}", def.count);
    assert_eq!(def.locations[0].path, "app.ts");
    assert_eq!(def.locations[0].line, 0, "add 定义在第 0 行");

    let _ = std::fs::remove_dir_all(&dir);
}

// ── Python e2e（pyright-langserver）────────────────────────────────────────────

/// Python 端到端：scaffold 一个含 `add` 函数 + 调用的 .py，断言 hover 给出签名、definition 解析到定义。
/// pyright-langserver 缺失则优雅跳过。
#[test]
fn lsp_python_end_to_end() {
    if !server_available("pyright-langserver") {
        eprintln!("跳过：PATH 中未找到 pyright-langserver");
        return;
    }
    let _serial = e2e_serial();
    let dir = unique_tmp();
    // add 定义在第 0 行；第 4 行调用 add。
    let py = "def add(a: int, b: int) -> int:\n    return a + b\n\n\nadd(1, 2)\n";
    std::fs::write(dir.join("app.py"), py).expect("write app.py");

    // hover 在 add 定义处（第 0 行，"add" 在第 4 列："def " 长 4）。
    let hover = lsp_hover(
        &dir,
        LspPositionRequest {
            path: "app.py".to_string(),
            line: 0,
            character: 4,
        },
    )
    .expect("lsp_hover(py) 应成功");
    eprintln!("py hover found={} contents={:?}", hover.found, hover.contents);
    assert!(hover.found, "py 应取到 hover 信息");
    // pyright 的 hover 含签名，如 "(function) def add(a: int, b: int) -> int"。
    assert!(
        hover.contents.contains("add") && hover.contents.contains("int"),
        "py hover 应含 add 的签名，实际: {}",
        hover.contents
    );

    // definition：第 4 行调用点（"add(1, 2)" 的 a，第 0 列）跳到第 0 行定义。
    let def = lsp_definition(
        &dir,
        LspPositionRequest {
            path: "app.py".to_string(),
            line: 4,
            character: 0,
        },
    )
    .expect("lsp_definition(py) 应成功");
    eprintln!("py definition count={} {:?}", def.count, def.locations);
    assert!(def.count >= 1, "add 应能解析到定义，实际 {}", def.count);
    assert_eq!(def.locations[0].path, "app.py");
    assert_eq!(def.locations[0].line, 0, "add 定义在第 0 行");

    let _ = std::fs::remove_dir_all(&dir);
}

// ── 池化复用（证明会话被复用 + 空闲可回收）────────────────────────────────────

/// 证明进程级池子复用长寿命会话：
///   1) 第一次 lsp_hover 成功后，会话被归还入池 → pooled 数 >= 1（证明留存可复用）；
///   2) 第二次同工作区调用复用该会话，且应不慢于首次（冷启动只发生一次，软性 eprintln 记录）；
///   3) `pool_reap_idle_secs(0)` 能回收空闲会话（证明 reaper 路径可杀掉空闲服务器、不泄漏）。
/// 用 Python（pyright 冷启动快、断言稳定）；缺失则跳过。
#[test]
fn lsp_session_pool_reuses_and_reaps() {
    if !server_available("pyright-langserver") {
        eprintln!("跳过：PATH 中未找到 pyright-langserver");
        return;
    }
    let _serial = e2e_serial();
    let dir = unique_tmp();
    let py = "def mul(a: int, b: int) -> int:\n    return a * b\n\n\nmul(2, 3)\n";
    std::fs::write(dir.join("calc.py"), py).expect("write calc.py");

    // 先清掉本进程里可能残留的空闲会话，让计数从干净起点观察。
    let _ = pool_reap_idle_secs(0);

    let req = || LspPositionRequest {
        path: "calc.py".to_string(),
        line: 0,
        character: 4,
    };

    let t0 = std::time::Instant::now();
    let h1 = lsp_hover(&dir, req()).expect("首次 hover 应成功");
    let first = t0.elapsed();
    assert!(h1.found, "首次 hover 应取到信息");

    // 归还后池中应至少有 1 个空闲会话（证明会话被留存而非关闭）。
    let after_first = pool_pooled_count();
    eprintln!("pooled after first call = {after_first} (first call {first:?})");
    assert!(
        after_first >= 1,
        "首次调用后应有空闲会话留在池中以备复用，实际 {after_first}"
    );

    let t1 = std::time::Instant::now();
    let h2 = lsp_hover(&dir, req()).expect("第二次 hover 应成功（复用会话）");
    let second = t1.elapsed();
    assert!(h2.found, "复用调用 hover 仍应取到信息");
    assert_eq!(h1.contents, h2.contents, "复用会话结果应一致");
    eprintln!("second call {second:?} (reused session)");
    // 软性：复用应不慢于首次太多（冷启动只一次）；给宽松上界，仅作回归提示不强约束环境抖动。
    assert!(
        second <= first + std::time::Duration::from_secs(20),
        "复用调用不应明显更慢：first={first:?} second={second:?}"
    );

    // reaper 路径：以 0 秒 TTL 立即回收全部空闲会话，应回收到 >=1 个（至少含本测试自己的）。
    // 注：cargo test 默认并行，其它 LSP 测试也共享同一进程级池子，故只断言「回收数 >= 1」
    // 与「回收后计数不增」这种对并发稳健的性质，不强求精确归零。
    let before_reap = pool_pooled_count();
    let reaped = pool_reap_idle_secs(0);
    let after_reap = pool_pooled_count();
    eprintln!("reaped {reaped} idle session(s); pooled {before_reap} -> {after_reap}");
    assert!(reaped >= 1, "应能回收至少 1 个空闲会话（证明可杀掉空闲服务器、不泄漏）");
    assert!(
        after_reap <= before_reap.saturating_sub(reaped) + 4,
        "回收后空闲计数不应异常增多：before={before_reap} reaped={reaped} after={after_reap}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
