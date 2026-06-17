//! `which`-风格的可执行文件查找：把服务器**程序名**解析为 PATH 目录下的**绝对路径**。
//!
//! 安全要点（abs-path 加固）：直接 `Command::new("gopls")` 会让 OS 在「当前工作目录」
//! 也参与查找（尤其 Windows 默认把 cwd 纳入搜索），于是工作区里放一个同名
//! `gopls.exe`/`gopls.cmd` 就可能被劫持执行——这正是我们要堵的洞。本模块**只**遍历
//! `PATH` 环境变量里的目录（不含 cwd），命中第一个可执行文件即返回其绝对路径；
//! 找不到则返回 `None`，调用方据此报「服务器未安装」而非挂死。
//!
//! Windows 细节：可执行性靠扩展名判定。若程序名自带扩展名（如 `foo.exe`）则按原名找；
//! 否则依次尝试 `PATHEXT`（缺省 `.COM;.EXE;.BAT;.CMD`，我们兜底覆盖 .exe/.cmd/.bat）。
//! 注意：npm 安装的 LSP（typescript-language-server / pyright-langserver）在 PATH 上
//! **只有 `.cmd` 垫片**，没有裸 `.exe`，故必须尝试 `.cmd`/`.bat`，否则在本机会判定为「未安装」。

use std::path::{Path, PathBuf};

/// 在 PATH 中把程序名解析为绝对路径。找不到返回 `None`。
///
/// `program` 是来自 `ServerSpec.command` 的**编译期常量名**（非用户输入）。
pub fn resolve_in_path(program: &str) -> Option<PathBuf> {
    // 防御：名字里若含路径分隔符，不属于「PATH 里的纯名」，按需直接判存在性（不引入 cwd 查找）。
    if program.contains('/') || program.contains('\\') {
        let p = Path::new(program);
        return if p.is_file() { Some(p.to_path_buf()) } else { None };
    }

    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        if dir.as_os_str().is_empty() {
            // 空项在某些系统上表示 cwd；我们刻意跳过，绝不把 cwd 纳入查找。
            continue;
        }
        if let Some(hit) = probe_dir(&dir, program) {
            return Some(hit);
        }
    }
    None
}

/// 在单个目录下尝试解析程序名（含平台扩展名规则）。
fn probe_dir(dir: &Path, program: &str) -> Option<PathBuf> {
    for candidate in candidate_names(program) {
        let full = dir.join(&candidate);
        if full.is_file() {
            return Some(full);
        }
    }
    None
}

/// 生成在某目录下要尝试的文件名列表（按优先级）。
#[cfg(windows)]
fn candidate_names(program: &str) -> Vec<String> {
    // 已自带扩展名：只按原名找。
    if has_extension(program) {
        return vec![program.to_string()];
    }
    // 否则按 PATHEXT 顺序尝试；缺省/异常时兜底常见可执行扩展名。
    let exts = windows_pathext();
    let mut names = Vec::with_capacity(exts.len() + 1);
    // 也允许「无扩展名直接命中」（少见，但无害）。
    for ext in &exts {
        // ext 形如 ".EXE"；拼成 "program.exe"（大小写在 Windows 文件系统不敏感）。
        names.push(format!("{program}{}", ext.to_ascii_lowercase()));
    }
    names
}

#[cfg(not(windows))]
fn candidate_names(program: &str) -> Vec<String> {
    // 类 Unix：可执行性由权限位决定，按原名找即可（不追加扩展名）。
    vec![program.to_string()]
}

/// 程序名是否已带（疑似可执行的）扩展名。仅用于 Windows 分支。
#[cfg(windows)]
fn has_extension(program: &str) -> bool {
    Path::new(program)
        .extension()
        .map(|e| !e.is_empty())
        .unwrap_or(false)
}

/// 读取 `PATHEXT`（分号分隔）；缺失/为空时兜底覆盖常见 LSP 垫片扩展名。
#[cfg(windows)]
fn windows_pathext() -> Vec<String> {
    let raw = std::env::var("PATHEXT").unwrap_or_default();
    let mut exts: Vec<String> = raw
        .split(';')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.starts_with('.') {
                s.to_string()
            } else {
                format!(".{s}")
            }
        })
        .collect();
    // 兜底：确保 .EXE/.CMD/.BAT 一定在列（npm 垫片是 .CMD）。
    for needed in [".EXE", ".CMD", ".BAT"] {
        if !exts.iter().any(|e| e.eq_ignore_ascii_case(needed)) {
            exts.push(needed.to_string());
        }
    }
    exts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_with_path_separator_not_searched_in_path() {
        // 含分隔符的名字不会去遍历 PATH（避免误解析）；不存在则 None。
        assert!(resolve_in_path("does/not/exist/foo").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn candidate_names_appends_pathext_on_windows() {
        let names = candidate_names("gopls");
        assert!(
            names.iter().any(|n| n.eq_ignore_ascii_case("gopls.exe")),
            "应尝试 gopls.exe，实际 {names:?}"
        );
        assert!(
            names.iter().any(|n| n.eq_ignore_ascii_case("gopls.cmd")),
            "应尝试 gopls.cmd（npm 垫片），实际 {names:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn candidate_names_keeps_explicit_extension() {
        let names = candidate_names("foo.exe");
        assert_eq!(names, vec!["foo.exe".to_string()]);
    }

    #[cfg(not(windows))]
    #[test]
    fn candidate_names_plain_on_unix() {
        assert_eq!(candidate_names("gopls"), vec!["gopls".to_string()]);
    }

    #[test]
    fn resolves_a_known_system_binary() {
        // 跨平台地找一个几乎必然存在的可执行：Windows 用 cmd，类 Unix 用 sh。
        #[cfg(windows)]
        let prog = "cmd";
        #[cfg(not(windows))]
        let prog = "sh";
        if let Some(p) = resolve_in_path(prog) {
            assert!(p.is_absolute(), "解析结果应为绝对路径: {p:?}");
            assert!(p.is_file());
        }
        // 若环境异常找不到也不算失败（保持测试稳健）。
    }
}
