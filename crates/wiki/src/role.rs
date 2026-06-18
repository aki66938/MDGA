//! 结构性（**非 LLM**）的目录角色推断。
//!
//! 求真原则：角色只由**可观测的结构信号**得出——目录名约定、文件扩展名分布、符号签名里的
//! 关键字（test/trait/struct/component…）——绝不调用模型、绝不臆测语义。信号不足时如实给
//! 一个中性角色（"source module"），而非编造。这保证 wiki 完全离线、确定、可复现。

use crate::sections::WikiSymbol;
use mdga_codemap::FileAnalysis;
use std::collections::HashMap;

/// 由目录名、该目录文件、汇总符号推断一个简短角色描述。
pub fn infer_role(directory: &str, files: &[&FileAnalysis], symbols: &[WikiSymbol]) -> String {
    let last = last_segment(directory);
    let last_lower = last.to_lowercase();

    // 1) 目录名约定优先（最强的人为意图信号）。
    if let Some(role) = role_from_dir_name(&last_lower) {
        return role.to_string();
    }

    // 2) 扩展名分布：看这目录主要是什么语言/资产。
    let ext_role = role_from_extensions(files);

    // 3) 符号签名关键字：是否大量是测试 / 类型定义 / 组件等。
    if let Some(role) = role_from_symbols(symbols) {
        return combine(role, ext_role.as_deref());
    }

    match ext_role {
        Some(r) => r,
        None => "source module".to_string(),
    }
}

fn last_segment(directory: &str) -> &str {
    if directory == "." || directory.is_empty() {
        return ".";
    }
    directory.rsplit(['/', '\\']).next().unwrap_or(directory)
}

/// 常见目录名约定 → 角色。命中即返回（覆盖绝大多数仓库布局）。
fn role_from_dir_name(name: &str) -> Option<&'static str> {
    let role = match name {
        "." => "repository root",
        "src" | "lib" | "source" => "primary source root",
        "test" | "tests" | "__tests__" | "spec" | "specs" => "test suite",
        "bench" | "benches" | "benchmark" | "benchmarks" => "benchmarks",
        "example" | "examples" | "demo" | "demos" | "sample" | "samples" => "examples",
        "doc" | "docs" | "documentation" => "documentation",
        "api" | "apis" | "routes" | "handlers" | "controllers" | "endpoints" => {
            "API / request handlers"
        }
        "model" | "models" | "entity" | "entities" | "schema" | "schemas" => "data models / schema",
        "component" | "components" | "widgets" | "views" => "UI components",
        "page" | "pages" | "screen" | "screens" => "UI pages / screens",
        "hook" | "hooks" => "reusable hooks",
        "util" | "utils" | "helper" | "helpers" | "common" | "shared" => "shared utilities",
        "config" | "configs" | "configuration" | "settings" => "configuration",
        "core" | "kernel" | "engine" => "core engine",
        "service" | "services" => "service layer",
        "store" | "stores" | "state" | "reducers" => "state management",
        "middleware" | "middlewares" => "middleware",
        "migration" | "migrations" => "database migrations",
        "cmd" | "bin" | "cli" => "executable entrypoints",
        "internal" | "private" => "internal implementation",
        "types" | "typings" | "interfaces" => "type definitions",
        "style" | "styles" | "css" | "theme" | "themes" => "styling / theme",
        "asset" | "assets" | "static" | "public" | "resources" => "static assets",
        "script" | "scripts" | "tools" | "tooling" => "build / tooling scripts",
        "proto" | "protos" | "protobuf" => "protocol definitions",
        "graphql" | "gql" => "GraphQL schema / resolvers",
        _ => return None,
    };
    Some(role)
}

/// 看目录里文件扩展名分布，给一个语言/资产维度的角色。无明显主导则 None。
fn role_from_extensions(files: &[&FileAnalysis]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for f in files {
        let ext = f
            .path
            .rsplit('.')
            .next()
            .filter(|e| !e.contains('/'))
            .unwrap_or("");
        let bucket = match ext.to_lowercase().as_str() {
            "rs" => "Rust",
            "ts" | "tsx" | "mts" | "cts" => "TypeScript",
            "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
            "py" | "pyi" => "Python",
            "go" => "Go",
            "java" => "Java",
            "rb" => "Ruby",
            "php" => "PHP",
            "c" | "h" => "C",
            "cpp" | "cc" | "cxx" | "hpp" | "hh" => "C++",
            "cs" => "C#",
            "lua" => "Lua",
            "scala" | "sc" => "Scala",
            "sh" | "bash" => "shell",
            _ => "other",
        };
        *counts.entry(bucket).or_insert(0) += 1;
    }
    // 取占比最高的桶（平局按名字升序确定）。
    let total = files.len();
    let (lang, n) = counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))?;
    if lang == "other" || n == 0 {
        return None;
    }
    // 主导（>=60%）才下结论，否则视为混合。
    if n * 100 / total >= 60 {
        Some(format!("{lang} source"))
    } else {
        Some("mixed-language source".to_string())
    }
}

/// 看汇总符号的签名关键字，判断偏测试 / 类型定义 / 组件等。
fn role_from_symbols(symbols: &[WikiSymbol]) -> Option<&'static str> {
    if symbols.is_empty() {
        return None;
    }
    let mut test_like = 0usize;
    let mut type_like = 0usize;
    let mut component_like = 0usize;
    for s in symbols {
        let sig = s.signature.to_lowercase();
        let name = s.name.to_lowercase();
        if sig.contains("#[test]")
            || sig.starts_with("test")
            || name.starts_with("test_")
            || name.starts_with("test")
            || sig.contains("describe(")
            || sig.contains("it(")
        {
            test_like += 1;
        }
        if sig.starts_with("struct ")
            || sig.starts_with("pub struct")
            || sig.starts_with("enum ")
            || sig.starts_with("pub enum")
            || sig.starts_with("interface ")
            || sig.starts_with("type ")
            || sig.starts_with("class ")
            || sig.starts_with("pub trait")
            || sig.starts_with("trait ")
        {
            type_like += 1;
        }
        if sig.contains("react.") || sig.contains("jsx") || sig.contains("=> (") {
            component_like += 1;
        }
    }
    let total = symbols.len();
    if test_like * 100 / total >= 50 {
        return Some("test suite");
    }
    if type_like * 100 / total >= 60 {
        return Some("type / data definitions");
    }
    if component_like * 100 / total >= 50 {
        return Some("UI components");
    }
    None
}

/// 把符号信号与扩展名信号组合成一句话（如 "type / data definitions (Rust source)"）。
fn combine(primary: &str, ext: Option<&str>) -> String {
    match ext {
        Some(e) => format!("{primary} ({e})"),
        None => primary.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fa(path: &str) -> FileAnalysis {
        FileAnalysis {
            path: path.to_string(),
            file_rank: 0.1,
            definition_count: 0,
            top_symbols: Vec::new(),
        }
    }

    fn sym(name: &str, sig: &str) -> WikiSymbol {
        WikiSymbol {
            name: name.to_string(),
            file: "x.rs".to_string(),
            line: 1,
            signature: sig.to_string(),
        }
    }

    #[test]
    fn dir_name_conventions_win() {
        let files = [fa("tests/foo.rs")];
        let refs: Vec<&FileAnalysis> = files.iter().collect();
        assert_eq!(infer_role("crate/tests", &refs, &[]), "test suite");
        assert_eq!(infer_role("src/api", &refs, &[]), "API / request handlers");
        assert_eq!(infer_role(".", &refs, &[]), "repository root");
    }

    #[test]
    fn extension_distribution_gives_language_role() {
        let files = [fa("x/a.rs"), fa("x/b.rs"), fa("x/c.rs")];
        let refs: Vec<&FileAnalysis> = files.iter().collect();
        // 目录名 "x" 无约定 → 落到扩展名：Rust 主导。
        assert_eq!(infer_role("x", &refs, &[]), "Rust source");
    }

    #[test]
    fn symbol_keywords_detect_type_definitions() {
        let files = [fa("m/types.rs")];
        let refs: Vec<&FileAnalysis> = files.iter().collect();
        let syms = vec![
            sym("Widget", "pub struct Widget"),
            sym("Color", "pub enum Color"),
            sym("Shape", "pub trait Shape"),
        ];
        let role = infer_role("m", &refs, &syms);
        assert!(
            role.starts_with("type / data definitions"),
            "应判为类型定义目录，实得 {role}"
        );
    }

    #[test]
    fn falls_back_to_neutral_when_no_signal() {
        // 无名约定、混合扩展、无符号 → 中性角色。
        let files = [fa("z/readme.xyz")];
        let refs: Vec<&FileAnalysis> = files.iter().collect();
        assert_eq!(infer_role("z", &refs, &[]), "source module");
    }
}
