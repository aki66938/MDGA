//! 无 tree-sitter grammar 语言的「通用启发式」符号提取（行/词法级，不依赖任何语法）。
//!
//! 目的：让**每个文本源文件**都能贡献一批粗粒度符号，而非整门语言一片空白。即便某语言没有
//! 维护良好的 tree-sitter crate（或属冷门 DSL），也能从声明关键字 + 紧随的标识符里抽出
//! 函数/类/类型等定义名，喂给 PageRank 与渲染。
//!
//! 求真原则（与 tags.rs 一致）：宁可少抽、绝不误导更不能 panic。所以：
//!   - 二进制/非 UTF-8 文件直接判空（含 NUL 字节即视为二进制）。
//!   - 只在「关键字位于行首（容许前导空白与少量修饰符）」时采信，避免把注释/字符串里的词当定义。
//!   - 每文件符号数封顶，长行截断，杜绝单个生成物文件撑爆地图。
//!
//! 注意：启发式只产出**定义**、不产出引用——跨文件引用图依赖精确的标识符语义，
//! 行级猜测的引用噪声远大于价值，故 refs 恒为空，这些文件只作为「被引用方/独立符号」入图。

use crate::tags::{Def, FileTags, MAX_SIG_LEN};

/// 启发式单文件最多提取的定义数（防止超大生成文件刷屏）。
const MAX_HEURISTIC_DEFS: usize = 200;
/// 扫描的最大行数（与字节上限互补：极长的单行/超多行文件到此为止）。
const MAX_SCAN_LINES: usize = 50_000;
/// 判定二进制时检视的前导字节数。
const BINARY_SNIFF_BYTES: usize = 8192;

/// 被视作「声明起手」的关键字。命中其一且其后紧跟合法标识符即记为一个定义。
/// 覆盖主流命令式/面向对象/函数式语言的常见声明词，对未知 DSL 也大概率有效。
/// （种类信息隐含在被保留的整行 sig 里，与 tree-sitter 路径口径一致，故不单列 kind。）
const DECL_KEYWORDS: &[&str] = &[
    "function",
    "func",
    "fn",
    "def",
    "defn",
    "defun",
    "sub",
    "proc",
    "method",
    "class",
    "struct",
    "interface",
    "trait",
    "enum",
    "union",
    "type",
    "typedef",
    "module",
    "namespace",
    "package",
    "impl",
    "constructor",
];

/// 行首允许出现、不影响「下一个 token 是关键字」判定的修饰符（跳过它们继续看）。
const MODIFIERS: &[&str] = &[
    "pub",
    "public",
    "private",
    "protected",
    "internal",
    "static",
    "final",
    "abstract",
    "virtual",
    "override",
    "async",
    "export",
    "default",
    "extern",
    "inline",
    "local",
    "global",
    "readonly",
    "sealed",
    "partial",
    "unsafe",
    "open",
    "data",
    "const",
    "mut",
    "var",
    "let",
];

/// 对一段源文本做启发式定义提取。`source` 已是 UTF-8（调用方负责读取）。
pub fn extract(source: &str) -> FileTags {
    if looks_binary(source.as_bytes()) {
        return FileTags::default();
    }

    let mut defs: Vec<Def> = Vec::new();
    for (row, raw_line) in source.lines().enumerate() {
        if row >= MAX_SCAN_LINES || defs.len() >= MAX_HEURISTIC_DEFS {
            break;
        }
        if let Some(def) = scan_line(raw_line, row) {
            defs.push(def);
        }
    }

    FileTags {
        defs,
        refs: Vec::new(),
    }
}

/// 单行扫描：跳过前导空白与已知修饰符，若遇到声明关键字且其后是合法标识符，记一个定义。
fn scan_line(line: &str, row: usize) -> Option<Def> {
    let trimmed = line.trim_start();
    // 明显的整行注释直接跳过（覆盖 // # ; -- 与 /* * 起手），避免把注释里的词当声明。
    if is_comment_start(trimmed) {
        return None;
    }

    // 把行首切成「词」序列（以非标识符字符为界），逐个跳过修饰符，找第一个声明关键字。
    let mut rest = trimmed;
    loop {
        let (word, after) = next_word(rest);
        if word.is_empty() {
            return None;
        }
        if DECL_KEYWORDS.contains(&word) {
            // 关键字命中：其后第一个标识符即定义名。
            let (name, _) = next_word(after.trim_start());
            if is_valid_ident(name) {
                return Some(Def {
                    name: name.to_string(),
                    line: row,
                    sig: truncate_sig(trimmed),
                });
            }
            return None;
        }
        // 不是关键字：仅当它是已知修饰符时才继续往后看，否则放弃本行。
        if MODIFIERS.contains(&word) {
            rest = after.trim_start();
            continue;
        }
        return None;
    }
}

/// 取出开头的一个「标识符词」（连续的字母/数字/下划线/`$`），返回 (词, 其后剩余)。
/// 若开头不是标识符字符，则词为空、剩余为原串（去掉首字符以保证调用方推进）。
fn next_word(s: &str) -> (&str, &str) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && is_ident_byte(bytes[i]) {
        i += 1;
    }
    if i == 0 {
        // 开头是分隔符：返回空词，并吃掉一个字符让外层不至于死循环。
        let mut chars = s.char_indices();
        let step = chars.nth(1).map(|(idx, _)| idx).unwrap_or(s.len());
        return ("", &s[step..]);
    }
    (&s[..i], &s[i..])
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// 合法定义名：非空、首字符非数字、仅由标识符字符组成。
fn is_valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$' => {}
        _ => return false,
    }
    s.bytes().all(is_ident_byte)
}

fn is_comment_start(s: &str) -> bool {
    s.starts_with("//")
        || s.starts_with('#')
        || s.starts_with(';')
        || s.starts_with("--")
        || s.starts_with("/*")
        || s.starts_with('*')
}

/// 含 NUL 字节即视为二进制（在前 BINARY_SNIFF_BYTES 内嗅探）。
fn looks_binary(bytes: &[u8]) -> bool {
    let n = bytes.len().min(BINARY_SNIFF_BYTES);
    bytes[..n].contains(&0)
}

fn truncate_sig(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= MAX_SIG_LEN {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX_SIG_LEN).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_common_declarations() {
        let src = "\
class Foo:
    def bar(self):
        pass

function baz() {}
struct Point { x: i32 }
";
        let tags = extract(src);
        let names: Vec<&str> = tags.defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"Foo"), "应抽到 class Foo，实得 {names:?}");
        assert!(names.contains(&"bar"), "应抽到 def bar，实得 {names:?}");
        assert!(names.contains(&"baz"), "应抽到 function baz，实得 {names:?}");
        assert!(names.contains(&"Point"), "应抽到 struct Point，实得 {names:?}");
        assert!(tags.refs.is_empty(), "启发式不产出引用");
    }

    #[test]
    fn skips_comments_and_modifiers() {
        // 注释行不应被当成定义；修饰符在前的声明应被识别。
        let src = "\
// def fake_in_comment
# class also_fake
public static function realOne() {}
";
        let tags = extract(src);
        let names: Vec<&str> = tags.defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["realOne"], "只应抽到 realOne，实得 {names:?}");
    }

    #[test]
    fn binary_input_yields_nothing() {
        let src = "def real()\u{0}\u{1}\u{2}binary junk";
        let tags = extract(src);
        assert!(tags.defs.is_empty(), "含 NUL 的二进制内容应判空");
    }

    #[test]
    fn caps_symbols_per_file() {
        let mut src = String::new();
        for i in 0..(MAX_HEURISTIC_DEFS + 50) {
            src.push_str(&format!("def f{i}()\n"));
        }
        let tags = extract(&src);
        assert!(
            tags.defs.len() <= MAX_HEURISTIC_DEFS,
            "单文件定义数应封顶 {MAX_HEURISTIC_DEFS}，实得 {}",
            tags.defs.len()
        );
    }
}
