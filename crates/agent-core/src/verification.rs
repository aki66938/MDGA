//! 写后验证命令探测（纯逻辑部分，不含实际执行）。
//!
//! 本轮（Plan28 P3-9）从桌面端迁入 agent-core：
//! - `read_diagnostics_command`（原 hooks.rs）：读 .mdga/diagnostics 首行命令，纯 std::fs。
//! - `detect_verification_command`（原 agent_loop.rs）：识别 Cargo.toml→cargo、package.json→npm，
//!   纯 std::fs + serde_json，逻辑一字不改，仅提升为 `pub`、改引用本 crate 内的 read_diagnostics_command。
//!
//! 实际「执行验证命令并回灌」的部分（依赖 run_command / emit）仍留桌面端工具循环。

/// 读取工作区 .mdga/diagnostics 的诊断命令（首个非空非注释行）；不存在则 None。
/// 例：写入 `cargo check` 或 `npm run typecheck`，Agent 改完代码收尾前自动跑、有错则修。
pub fn read_diagnostics_command(workspace: &str) -> Option<String> {
    let path = std::path::Path::new(workspace).join(".mdga").join("diagnostics");
    let content = std::fs::read_to_string(path).ok()?;
    content
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
}

/// 探测本工作区可用的「写后验证」命令（Plan25 #7）：优先用户显式配置的 .mdga/diagnostics，
/// 否则按工作区可识别的构建/测试约定推断。探测不到则返回 None（跳过验证回路）。
///
/// 推断优先级：Cargo.toml → `cargo check`；package.json 含 scripts.test → `npm test`，
/// 否则含 scripts.build → `npm run build`。其它生态暂不推断（避免误跑昂贵/有副作用命令）。
pub fn detect_verification_command(workspace: &str) -> Option<String> {
    // 显式诊断命令最高优先：用户已声明权威验证手段。
    if let Some(cmd) = read_diagnostics_command(workspace) {
        return Some(cmd);
    }
    let root = std::path::Path::new(workspace);
    if root.join("Cargo.toml").is_file() {
        return Some("cargo check".to_string());
    }
    if let Ok(text) = std::fs::read_to_string(root.join("package.json")) {
        if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&text) {
            let scripts = pkg.get("scripts");
            let has = |name: &str| {
                scripts
                    .and_then(|s| s.get(name))
                    .and_then(|v| v.as_str())
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
            };
            if has("test") {
                return Some("npm test".to_string());
            }
            if has("build") {
                return Some("npm run build".to_string());
            }
        }
    }
    None
}
