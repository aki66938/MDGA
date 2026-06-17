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
        _ => return None,
    };
    Some(def)
}

/// 该扩展名是否受支持（供文件发现阶段快速过滤）。
pub fn is_supported_extension(ext: &str) -> bool {
    lang_for_extension(ext).is_some()
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
