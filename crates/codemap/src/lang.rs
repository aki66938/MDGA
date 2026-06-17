//! 语言注册表：文件扩展名 → tree-sitter Language + 内嵌 tags 查询。
//!
//! 查询采用本 crate 自定义的捕获命名约定（不依赖各 grammar crate 是否导出 TAGS_QUERY）：
//!   - 定义名节点捕获为 `@def.<kind>`（kind ∈ function/method/struct/enum/trait/type/class/
//!     interface/constant/module…），节点文本即符号名、起始行即定义行。
//!   - 引用标识符节点捕获为 `@ref`（调用名、类型用名、new 构造名等）。
//!
//! 这样 tags.rs 只需看捕获名前缀即可区分定义/引用，无需关心各语言 AST 细节。

use tree_sitter::Language;

/// 一种受支持语言：名字（用于诊断）、tree-sitter Language、tags 查询源。
pub struct LangDef {
    pub name: &'static str,
    pub language: fn() -> Language,
    pub query: &'static str,
}

/// 由文件扩展名（小写，不含点）解析出语言定义；不支持的返回 None。
pub fn lang_for_extension(ext: &str) -> Option<&'static LangDef> {
    let def = match ext {
        "rs" => &RUST,
        "py" | "pyi" => &PYTHON,
        "js" | "jsx" | "mjs" | "cjs" => &JAVASCRIPT,
        "ts" | "mts" | "cts" => &TYPESCRIPT,
        "tsx" => &TSX,
        "go" => &GO,
        "java" => &JAVA,
        // C：头文件 .h 既可能是 C 也可能是 C++，统一交给 C 语法（结构/函数声明节点通用），
        // 仅当确属 C++ 专有语法时才会少抽几个符号，可接受。
        "c" | "h" => &C,
        "cpp" | "cc" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" => &CPP,
        "cs" => &CSHARP,
        "rb" => &RUBY,
        "php" | "phtml" | "php3" | "php4" | "php5" | "php7" | "phps" => &PHP,
        "sh" | "bash" | "zsh" => &BASH,
        "lua" => &LUA,
        "scala" | "sc" => &SCALA,
        _ => return None,
    };
    Some(def)
}

/// 该扩展名是否有专属 tree-sitter grammar（精确解析路径）。
pub fn is_supported_extension(ext: &str) -> bool {
    lang_for_extension(ext).is_some()
}

/// 已知的「非文本/无符号价值」扩展名：二进制、媒体、压缩、字体、锁文件、富文档等。
/// 这些不送启发式回退（既无符号又可能很大/是二进制）。判定大小写不敏感（调用方已转小写）。
const NON_TEXT_EXTENSIONS: &[&str] = &[
    // 图像 / 媒体
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "webp", "tif", "tiff", "svg", "psd", "mp3", "wav",
    "flac", "ogg", "mp4", "mov", "avi", "mkv", "webm", "m4a", "aac",
    // 压缩 / 归档 / 安装包
    "zip", "gz", "tgz", "bz2", "xz", "zst", "7z", "rar", "tar", "jar", "war", "dmg", "iso", "deb",
    "rpm", "msi", "apk",
    // 可执行 / 目标 / 库
    "exe", "dll", "so", "dylib", "o", "obj", "a", "lib", "class", "pyc", "pyo", "wasm", "bin",
    "pdb",
    // 字体
    "ttf", "otf", "woff", "woff2", "eot",
    // 富文档 / 表格
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt",
    // 数据 / 序列化大件
    "db", "sqlite", "parquet", "bin", "dat", "pack", "idx",
    // 锁文件（机器生成、巨大、无符号价值）
    "lock", "sum",
];

/// 文件发现阶段是否应扫描该扩展名：有 grammar 的精确解析、其余文本文件交给启发式回退。
/// 只排除明确的二进制/媒体/锁文件等「无符号价值」类型，其它一律放行——
/// 真正的二进制内容还会在 heuristic::extract 里按 NUL 字节再判空，超大文件由字节上限拦截。
pub fn should_scan_extension(ext: &str) -> bool {
    is_supported_extension(ext) || !NON_TEXT_EXTENSIONS.contains(&ext)
}

static RUST: LangDef = LangDef {
    name: "rust",
    language: || tree_sitter_rust::LANGUAGE.into(),
    query: r#"
(function_item name: (identifier) @def.function)
(function_signature_item name: (identifier) @def.function)
(struct_item name: (type_identifier) @def.struct)
(union_item name: (type_identifier) @def.struct)
(enum_item name: (type_identifier) @def.enum)
(trait_item name: (type_identifier) @def.trait)
(type_item name: (type_identifier) @def.type)
(mod_item name: (identifier) @def.module)
(macro_definition name: (identifier) @def.macro)
(const_item name: (identifier) @def.constant)
(static_item name: (identifier) @def.constant)

(call_expression function: (identifier) @ref)
(call_expression function: (field_expression field: (field_identifier) @ref))
(call_expression function: (scoped_identifier name: (identifier) @ref))
(macro_invocation macro: (identifier) @ref)
(type_identifier) @ref
"#,
};

static PYTHON: LangDef = LangDef {
    name: "python",
    language: || tree_sitter_python::LANGUAGE.into(),
    query: r#"
(function_definition name: (identifier) @def.function)
(class_definition name: (identifier) @def.class)

(call function: (identifier) @ref)
(call function: (attribute attribute: (identifier) @ref))
"#,
};

static JAVASCRIPT: LangDef = LangDef {
    name: "javascript",
    language: || tree_sitter_javascript::LANGUAGE.into(),
    query: JS_TS_COMMON_QUERY,
};

static TYPESCRIPT: LangDef = LangDef {
    name: "typescript",
    language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    query: TS_QUERY,
};

static TSX: LangDef = LangDef {
    name: "tsx",
    language: || tree_sitter_typescript::LANGUAGE_TSX.into(),
    query: TS_QUERY,
};

static GO: LangDef = LangDef {
    name: "go",
    language: || tree_sitter_go::LANGUAGE.into(),
    query: r#"
(function_declaration name: (identifier) @def.function)
(method_declaration name: (field_identifier) @def.method)
(type_declaration (type_spec name: (type_identifier) @def.type))
(const_spec name: (identifier) @def.constant)

(call_expression function: (identifier) @ref)
(call_expression function: (selector_expression field: (field_identifier) @ref))
(type_identifier) @ref
"#,
};

static JAVA: LangDef = LangDef {
    name: "java",
    language: || tree_sitter_java::LANGUAGE.into(),
    query: r#"
(class_declaration name: (identifier) @def.class)
(interface_declaration name: (identifier) @def.interface)
(enum_declaration name: (identifier) @def.enum)
(record_declaration name: (identifier) @def.class)
(method_declaration name: (identifier) @def.method)
(constructor_declaration name: (identifier) @def.method)

(method_invocation name: (identifier) @ref)
(object_creation_expression type: (type_identifier) @ref)
(superclass (type_identifier) @ref)
(type_identifier) @ref
"#,
};

static C: LangDef = LangDef {
    name: "c",
    language: || tree_sitter_c::LANGUAGE.into(),
    query: r#"
(function_declarator declarator: (identifier) @def.function)
(struct_specifier name: (type_identifier) @def.struct body: (_))
(union_specifier name: (type_identifier) @def.struct body: (_))
(enum_specifier name: (type_identifier) @def.enum body: (_))
(type_definition declarator: (type_identifier) @def.type)

(call_expression function: (identifier) @ref)
(type_identifier) @ref
"#,
};

static CPP: LangDef = LangDef {
    name: "cpp",
    language: || tree_sitter_cpp::LANGUAGE.into(),
    query: r#"
(function_declarator declarator: (identifier) @def.function)
(function_declarator declarator: (field_identifier) @def.method)
(function_declarator
  declarator: (qualified_identifier name: (identifier) @def.method))
(struct_specifier name: (type_identifier) @def.struct body: (_))
(union_specifier name: (type_identifier) @def.struct body: (_))
(class_specifier name: (type_identifier) @def.class)
(enum_specifier name: (type_identifier) @def.enum body: (_))
(type_definition declarator: (type_identifier) @def.type)
(namespace_definition name: (namespace_identifier) @def.module)

(call_expression function: (identifier) @ref)
(call_expression function: (field_expression field: (field_identifier) @ref))
(type_identifier) @ref
"#,
};

static CSHARP: LangDef = LangDef {
    name: "csharp",
    language: || tree_sitter_c_sharp::LANGUAGE.into(),
    query: r#"
(class_declaration name: (identifier) @def.class)
(interface_declaration name: (identifier) @def.interface)
(struct_declaration name: (identifier) @def.struct)
(enum_declaration name: (identifier) @def.enum)
(record_declaration name: (identifier) @def.class)
(method_declaration name: (identifier) @def.method)
(constructor_declaration name: (identifier) @def.method)
(namespace_declaration name: (identifier) @def.module)

(invocation_expression
  function: (member_access_expression name: (identifier) @ref))
(invocation_expression function: (identifier) @ref)
(object_creation_expression type: (identifier) @ref)
"#,
};

static RUBY: LangDef = LangDef {
    name: "ruby",
    language: || tree_sitter_ruby::LANGUAGE.into(),
    query: r#"
(method name: (identifier) @def.method)
(singleton_method name: (identifier) @def.method)
(class name: (constant) @def.class)
(module name: (constant) @def.module)

(call method: (identifier) @ref)
"#,
};

static PHP: LangDef = LangDef {
    name: "php",
    language: || tree_sitter_php::LANGUAGE_PHP.into(),
    query: r#"
(function_definition name: (name) @def.function)
(method_declaration name: (name) @def.method)
(class_declaration name: (name) @def.class)
(interface_declaration name: (name) @def.interface)
(trait_declaration name: (name) @def.interface)
(enum_declaration name: (name) @def.enum)
(namespace_definition name: (namespace_name) @def.module)

(function_call_expression function: (name) @ref)
(scoped_call_expression name: (name) @ref)
(member_call_expression name: (name) @ref)
(object_creation_expression (qualified_name (name) @ref))
"#,
};

static BASH: LangDef = LangDef {
    name: "bash",
    language: || tree_sitter_bash::LANGUAGE.into(),
    query: r#"
(function_definition name: (word) @def.function)

(command name: (command_name (word) @ref))
"#,
};

static LUA: LangDef = LangDef {
    name: "lua",
    language: || tree_sitter_lua::LANGUAGE.into(),
    query: r#"
(function_declaration name: (identifier) @def.function)
(function_declaration
  name: (dot_index_expression field: (identifier) @def.function))
(function_declaration
  name: (method_index_expression method: (identifier) @def.method))

(function_call name: (identifier) @ref)
(function_call
  name: (dot_index_expression field: (identifier) @ref))
(function_call
  name: (method_index_expression method: (identifier) @ref))
"#,
};

static SCALA: LangDef = LangDef {
    name: "scala",
    language: || tree_sitter_scala::LANGUAGE.into(),
    query: r#"
(class_definition name: (identifier) @def.class)
(object_definition name: (identifier) @def.class)
(trait_definition name: (identifier) @def.interface)
(enum_definition name: (identifier) @def.enum)
(function_definition name: (identifier) @def.function)
(type_definition name: (type_identifier) @def.type)

(call_expression (identifier) @ref)
(extends_clause (type_identifier) @ref)
(instance_expression (type_identifier) @ref)
(type_identifier) @ref
"#,
};

/// JS 与 TS 共享的定义/引用模式（不含类型相关节点）。
const JS_TS_COMMON_QUERY: &str = r#"
(function_declaration name: (identifier) @def.function)
(method_definition name: (property_identifier) @def.method)
(class_declaration name: (identifier) @def.class)
(variable_declarator name: (identifier) @def.function value: (arrow_function))
(variable_declarator name: (identifier) @def.function value: (function_expression))

(call_expression function: (identifier) @ref)
(call_expression function: (member_expression property: (property_identifier) @ref))
(new_expression constructor: (identifier) @ref)
"#;

/// TS 在 JS 基础上加类型声明与类型引用。
const TS_QUERY: &str = r#"
(function_declaration name: (identifier) @def.function)
(method_definition name: (property_identifier) @def.method)
(class_declaration name: (type_identifier) @def.class)
(abstract_class_declaration name: (type_identifier) @def.class)
(interface_declaration name: (type_identifier) @def.interface)
(type_alias_declaration name: (type_identifier) @def.type)
(enum_declaration name: (identifier) @def.enum)
(variable_declarator name: (identifier) @def.function value: (arrow_function))
(variable_declarator name: (identifier) @def.function value: (function_expression))

(call_expression function: (identifier) @ref)
(call_expression function: (member_expression property: (property_identifier) @ref))
(new_expression constructor: (identifier) @ref)
(type_identifier) @ref
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Query;

    /// 全部已注册语言的 tags 查询都应能针对各自 grammar 成功编译。
    /// 这能挡住「节点名拼错被 compiled_query fail-soft 静默吞掉、结果整门语言抽不到符号」的回归。
    #[test]
    fn all_language_queries_compile() {
        let langs: &[&LangDef] = &[
            &RUST, &PYTHON, &JAVASCRIPT, &TYPESCRIPT, &TSX, &GO, &JAVA, &C, &CPP, &CSHARP, &RUBY,
            &PHP, &BASH, &LUA, &SCALA,
        ];
        for def in langs {
            let language = (def.language)();
            assert!(
                Query::new(&language, def.query).is_ok(),
                "{} 的 tags 查询应能编译（节点名与该 grammar 版本对得上）",
                def.name
            );
        }
    }

    /// 抽样验证新加语言的扩展名映射接上了对应 grammar（而非漏配）。
    #[test]
    fn new_extensions_are_wired() {
        assert_eq!(lang_for_extension("java").unwrap().name, "java");
        assert_eq!(lang_for_extension("c").unwrap().name, "c");
        assert_eq!(lang_for_extension("cpp").unwrap().name, "cpp");
        assert_eq!(lang_for_extension("cs").unwrap().name, "csharp");
        assert_eq!(lang_for_extension("rb").unwrap().name, "ruby");
        assert_eq!(lang_for_extension("php").unwrap().name, "php");
        // 未支持扩展名仍返回 None（交给启发式回退处理）。
        assert!(lang_for_extension("zig").is_none());
    }
}
