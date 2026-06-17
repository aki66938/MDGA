//! 语言服务器解析：按文件扩展名映射到**硬编码精选注册表（curated registry）**里的已知服务器。
//!
//! 安全要点（强约束，R1 不变量）：服务器程序与参数全部来自本文件内**编译期常量**，
//! **绝不**接受来自 config / 工作区 / 模型输入的任意命令——这是把 LSP 工具与「跑任意命令」
//! 隔离开的关键边界。未知扩展名或找不到二进制时返回清晰错误，调用方据此优雅报错而非挂死。
//!
//! 设计（R1 泛用化）：把原先 `match ext` 的硬编码分支重构为一张 **REGISTRY 表**
//! （`&[ServerEntry]`），每条目声明 `extensions / command / args / language_ids`。
//! 解析时遍历表、按扩展名命中即返回。这样新增一门语言只需往表里加一行常量，
//! 不改解析逻辑，也不放松「命令必须是编译期常量」这一安全不变量。
//!
//! 未来扩展点（**仅结构与注释，本次不实现**）：
//! 见 [`resolve_server`] 上方的 “USER-AUTHORIZED EXTENSION POINT” 注释。

use crate::LspError;

/// 一个已解析的语言服务器启动规格（程序名 + 参数 + 语言标识）。
///
/// `command` 是**程序名**（如 `gopls`），不含路径；真正 spawn 前由 `which` 模块解析为
/// PATH 中的绝对路径（见 `crate::which`），以防工作区 cwd 下同名可执行被劫持。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerSpec {
    /// 可执行程序名（在 PATH 中查找；硬编码常量，**非用户输入**）。
    pub command: &'static str,
    /// 启动参数（硬编码常量）。
    pub args: &'static [&'static str],
    /// LSP `languageId`（textDocument.languageId），用于 didOpen。
    pub language_id: &'static str,
}

/// 注册表里的一条「扩展名族 → 服务器」映射（编译期常量）。
///
/// 一个语言可能有多个扩展名（如 C/C++ 头/源），且不同扩展名可能要不同的 `languageId`
/// （如 `.ts`→typescript、`.tsx`→typescriptreact）。因此 `language_ids` 与 `extensions`
/// 一一对应（同序、等长）：`extensions[i]` 命中时用 `language_ids[i]` 作为 LSP languageId。
struct ServerEntry {
    /// 该条目认领的扩展名（小写、不含点）。与 `language_ids` 同序等长。
    extensions: &'static [&'static str],
    /// 与 `extensions` 一一对应的 LSP languageId。
    language_ids: &'static [&'static str],
    /// 可执行程序名（编译期常量，非用户输入）。
    command: &'static str,
    /// 启动参数（编译期常量）。
    args: &'static [&'static str],
}

/// **精选注册表（curated registry）**：扩展名 → 简单 stdio 语言服务器的硬编码映射表。
///
/// 收录原则：仅收「单可执行 + stdio 即用」的服务器（spawn 程序、走 stdin/stdout 即可）。
/// 这些都是 IDE 生态里最常见、安装即在 PATH 暴露同名命令的服务器。
///
/// 刻意**不**收录需要复杂 bootstrap 的服务器（如 Java 的 `jdtls`：需 -data 工作区 +
/// 一堆 -configuration/-jar 参数；C# 的 `omnisharp`/`OmniSharp`：需 -lsp/-s 解决方案路径等）。
/// 这类服务器的启动契约更复杂，强行塞进「单命令 + 固定 args」会脆弱；留待未来按需单独建模。
// TODO(future, complex-bootstrap): jdtls（Java，需 -data/-configuration/-jar）、
//   omnisharp（C#，需 -lsp 与解决方案定位）等需要专门的启动建模，暂不纳入本表。
const REGISTRY: &[ServerEntry] = &[
    // Rust：rust-analyzer（无参数，stdio）。
    ServerEntry {
        extensions: &["rs"],
        language_ids: &["rust"],
        command: "rust-analyzer",
        args: &[],
    },
    // TypeScript / JavaScript 家族：typescript-language-server --stdio。
    ServerEntry {
        extensions: &["ts", "tsx", "js", "jsx", "mjs", "cjs"],
        language_ids: &[
            "typescript",
            "typescriptreact",
            "javascript",
            "javascriptreact",
            "javascript",
            "javascript",
        ],
        command: "typescript-language-server",
        args: &["--stdio"],
    },
    // Python：pyright-langserver --stdio。
    ServerEntry {
        extensions: &["py", "pyi"],
        language_ids: &["python", "python"],
        command: "pyright-langserver",
        args: &["--stdio"],
    },
    // Go：gopls（无参数，默认 stdio）。
    ServerEntry {
        extensions: &["go"],
        language_ids: &["go"],
        command: "gopls",
        args: &[],
    },
    // C / C++：clangd（无参数，stdio）。覆盖常见源/头扩展名。
    ServerEntry {
        extensions: &["c", "h", "cpp", "cc", "cxx", "hpp", "hh"],
        language_ids: &[
            "c",
            "c", // .h 归到 c（clangd 自身按内容/编译库判定 C/C++，languageId 仅作提示）
            "cpp",
            "cpp",
            "cpp",
            "cpp",
            "cpp",
        ],
        command: "clangd",
        args: &[],
    },
    // Ruby：ruby-lsp（无参数，stdio）。
    ServerEntry {
        extensions: &["rb"],
        language_ids: &["ruby"],
        command: "ruby-lsp",
        args: &[],
    },
    // PHP：intelephense --stdio。
    ServerEntry {
        extensions: &["php"],
        language_ids: &["php"],
        command: "intelephense",
        args: &["--stdio"],
    },
    // Lua：lua-language-server（无参数，stdio）。
    ServerEntry {
        extensions: &["lua"],
        language_ids: &["lua"],
        command: "lua-language-server",
        args: &[],
    },
];

/// 从文件路径里取小写、去点的扩展名；无扩展名返回空串。
fn extension_of(path: &str) -> String {
    path.rsplit('.')
        .next()
        .filter(|e| !e.contains('/') && !e.contains('\\') && *e != path)
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// 按文件路径的扩展名解析语言服务器。未知扩展名返回 `Unsupported`。
///
/// 解析只查 [`REGISTRY`]（编译期常量表）。返回的 `command` 永远是表里的常量，
/// 不可能是来自外部输入的任意命令——这是安全边界。
///
/// ───────────────────────────────────────────────────────────────────────────
/// USER-AUTHORIZED EXTENSION POINT（未来项，**本次仅注释、不实现**）
///
/// 目标：允许**用户**（且仅用户）在应用设置里登记额外的语言服务器（如某门小众语言，
/// 或同一语言换一个服务器实现），而**绝不**接受来自工作区文件 / 模型输出的命令。
///
/// 预期形态（实现时再落地，不在本次范围）：
///   1. 一个 `UserServerEntry { extensions, command(绝对路径或名), args, language_id }`
///      的列表，来源是**应用设置**（受信任的本地配置，由人类用户显式录入/确认），
///      不是工作区里的 `.json`/`.toml`，也不是模型生成的 JSON。
///   2. `resolve_server` 增加一个可选的 `user_overrides: &[UserServerEntry]` 入参
///      （或线程外注入的全局只读快照），**先查内置 REGISTRY，未命中再查用户表**
///      （或反之，按产品策略；但内置表始终是安全基线）。
///   3. 用户表里的 `command` 同样要过 `crate::which` 的绝对路径化与存在性校验，
///      并复用同样的 secret-env 擦除与超时/Drop 强杀护栏。
///
/// 安全不变量（任何实现都必须保持）：命令来源只能是 ① 本文件的编译期常量，或
/// ② 应用设置里人类显式录入的条目；**永不**来自工作区扫描或模型输出。
/// 本次提交不引入设置存储 / DB / GUI，仅保留以上结构位与注释。
/// ───────────────────────────────────────────────────────────────────────────
pub fn resolve_server(path: &str) -> Result<ServerSpec, LspError> {
    let ext = extension_of(path);
    if ext.is_empty() {
        return Err(LspError::Unsupported(format!(
            "文件 `{path}` 没有扩展名，无法判断语言服务器"
        )));
    }

    for entry in REGISTRY {
        if let Some(idx) = entry.extensions.iter().position(|e| *e == ext) {
            return Ok(ServerSpec {
                command: entry.command,
                args: entry.args,
                // language_ids 与 extensions 同序等长；命中下标即对应 languageId。
                language_id: entry.language_ids[idx],
            });
        }
    }

    Err(LspError::Unsupported(format!(
        "扩展名 `.{ext}` 暂无受支持的语言服务器（支持: {}）",
        supported_extensions_summary()
    )))
}

/// 生成「当前支持的扩展名」摘要，用于报错文案（按注册表实际内容，自动跟随扩展）。
fn supported_extensions_summary() -> String {
    REGISTRY
        .iter()
        .map(|e| {
            e.extensions
                .iter()
                .map(|x| format!(".{x}"))
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect::<Vec<_>>()
        .join(" / ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_entries_are_consistent() {
        // 不变量：每条目 extensions 与 language_ids 同序等长，且扩展名全局唯一（无重复认领）。
        let mut seen: Vec<&str> = Vec::new();
        for entry in REGISTRY {
            assert_eq!(
                entry.extensions.len(),
                entry.language_ids.len(),
                "entry `{}` extensions/language_ids 长度不一致",
                entry.command
            );
            for ext in entry.extensions {
                assert!(
                    !seen.contains(ext),
                    "扩展名 `.{ext}` 被多条注册表条目重复认领",
                );
                seen.push(ext);
            }
        }
    }

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
    fn resolves_broadened_languages() {
        // R1 泛用化新增的精选条目。
        let go = resolve_server("main.go").unwrap();
        assert_eq!(go.command, "gopls");
        assert_eq!(go.language_id, "go");
        assert!(go.args.is_empty());

        for (path, lang) in [
            ("a.c", "c"),
            ("a.h", "c"),
            ("a.cpp", "cpp"),
            ("a.cc", "cpp"),
            ("a.cxx", "cpp"),
            ("a.hpp", "cpp"),
            ("a.hh", "cpp"),
        ] {
            let s = resolve_server(path).unwrap();
            assert_eq!(s.command, "clangd", "for {path}");
            assert_eq!(s.language_id, lang, "for {path}");
        }

        let rb = resolve_server("app.rb").unwrap();
        assert_eq!(rb.command, "ruby-lsp");
        assert_eq!(rb.language_id, "ruby");

        let php = resolve_server("index.php").unwrap();
        assert_eq!(php.command, "intelephense");
        assert_eq!(php.args, &["--stdio"]);
        assert_eq!(php.language_id, "php");

        let lua = resolve_server("init.lua").unwrap();
        assert_eq!(lua.command, "lua-language-server");
        assert_eq!(lua.language_id, "lua");
    }

    #[test]
    fn case_insensitive_extension() {
        assert_eq!(resolve_server("FOO.RS").unwrap().command, "rust-analyzer");
        assert_eq!(
            resolve_server("Comp.TSX").unwrap().language_id,
            "typescriptreact"
        );
        assert_eq!(resolve_server("Main.GO").unwrap().command, "gopls");
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
        // 防御性：任何能解析的扩展名都只能映射到精选注册表里的命令之一。
        const ALLOWED: &[&str] = &[
            "rust-analyzer",
            "typescript-language-server",
            "pyright-langserver",
            "gopls",
            "clangd",
            "ruby-lsp",
            "intelephense",
            "lua-language-server",
        ];
        for path in [
            "a.rs", "a.ts", "a.tsx", "a.js", "a.py", "a.pyi", "a.go", "a.c", "a.cpp", "a.rb",
            "a.php", "a.lua",
        ] {
            let s = resolve_server(path).unwrap();
            assert!(ALLOWED.contains(&s.command), "命令越权: {}", s.command);
        }
    }
}
