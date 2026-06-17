//! 语言服务器解析：按文件扩展名映射到**硬编码白名单**里的已知服务器。
//!
//! 安全要点：服务器程序与参数全部硬编码，**绝不**接受来自 config/模型输入的任意命令——
//! 这是把 LSP 工具与「跑任意命令」隔离开的关键边界。未知扩展名或找不到二进制时返回清晰错误，
//! 调用方据此优雅报错而非挂死。

use crate::LspError;

/// 一个已解析的语言服务器启动规格（程序名 + 参数 + 语言标识）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerSpec {
    /// 可执行程序名（在 PATH 中查找；硬编码，非用户输入）。
    pub command: &'static str,
    /// 启动参数（硬编码）。
    pub args: &'static [&'static str],
    /// LSP `languageId`（textDocument.languageId），用于 didOpen。
    pub language_id: &'static str,
}

/// 按文件路径的扩展名解析语言服务器。未知扩展名返回 `Unsupported`。
///
/// 白名单（且仅此）：
/// - `.rs`                                   → `rust-analyzer`
/// - `.ts/.tsx/.js/.jsx/.mjs/.cjs`           → `typescript-language-server --stdio`
/// - `.py/.pyi`                              → `pyright-langserver --stdio`
pub fn resolve_server(path: &str) -> Result<ServerSpec, LspError> {
    let ext = path
        .rsplit('.')
        .next()
        .filter(|e| !e.contains('/') && !e.contains('\\') && *e != path)
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "rs" => Ok(ServerSpec {
            command: "rust-analyzer",
            args: &[],
            language_id: "rust",
        }),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
            let language_id = match ext.as_str() {
                "ts" => "typescript",
                "tsx" => "typescriptreact",
                "jsx" => "javascriptreact",
                _ => "javascript",
            };
            Ok(ServerSpec {
                command: "typescript-language-server",
                args: &["--stdio"],
                language_id,
            })
        }
        "py" | "pyi" => Ok(ServerSpec {
            command: "pyright-langserver",
            args: &["--stdio"],
            language_id: "python",
        }),
        "" => Err(LspError::Unsupported(format!(
            "文件 `{path}` 没有扩展名，无法判断语言服务器"
        ))),
        other => Err(LspError::Unsupported(format!(
            "扩展名 `.{other}` 暂无受支持的语言服务器（仅支持 .rs / .ts,.tsx,.js,.jsx,.mjs,.cjs / .py,.pyi）"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_rust() {
        let s = resolve_server("src/main.rs").unwrap();
        assert_eq!(s.command, "rust-analyzer");
        assert_eq!(s.language_id, "rust");
        assert!(s.args.is_empty());
    }

    #[test]
    fn resolves_typescript_family() {
        for (path, lang) in [
            ("a/b.ts", "typescript"),
            ("c.tsx", "typescriptreact"),
            ("d.js", "javascript"),
            ("e.jsx", "javascriptreact"),
            ("f.mjs", "javascript"),
            ("g.cjs", "javascript"),
        ] {
            let s = resolve_server(path).unwrap();
            assert_eq!(s.command, "typescript-language-server", "for {path}");
            assert_eq!(s.args, &["--stdio"], "for {path}");
            assert_eq!(s.language_id, lang, "for {path}");
        }
    }

    #[test]
    fn resolves_python() {
        for path in ["x.py", "pkg/mod.pyi"] {
            let s = resolve_server(path).unwrap();
            assert_eq!(s.command, "pyright-langserver");
            assert_eq!(s.args, &["--stdio"]);
            assert_eq!(s.language_id, "python");
        }
    }

    #[test]
    fn case_insensitive_extension() {
        assert_eq!(resolve_server("FOO.RS").unwrap().command, "rust-analyzer");
        assert_eq!(
            resolve_server("Comp.TSX").unwrap().language_id,
            "typescriptreact"
        );
    }

    #[test]
    fn unknown_or_missing_extension_errors() {
        assert!(matches!(
            resolve_server("README.md"),
            Err(LspError::Unsupported(_))
        ));
        assert!(matches!(
            resolve_server("Makefile"),
            Err(LspError::Unsupported(_))
        ));
        // 仅目录、无扩展名。
        assert!(matches!(
            resolve_server("src/bin"),
            Err(LspError::Unsupported(_))
        ));
    }

    #[test]
    fn never_returns_arbitrary_command() {
        // 防御性：任何能解析的扩展名都只能映射到白名单里的三种程序之一。
        const ALLOWED: &[&str] = &[
            "rust-analyzer",
            "typescript-language-server",
            "pyright-langserver",
        ];
        for path in ["a.rs", "a.ts", "a.tsx", "a.js", "a.py", "a.pyi"] {
            let s = resolve_server(path).unwrap();
            assert!(ALLOWED.contains(&s.command), "命令越权: {}", s.command);
        }
    }
}
