//! OKF（Open Knowledge Format）序列化层：把内部 `WikiSection[]` 投影成**严格合规的
//! Google OKF v0.1 bundle**，并能写入目录树、读取外部 bundle。
//!
//! 这是**合规命脉**：本模块的产物必须严格等同 OKF v0.1 规范，零自主发挥。下面逐条对照规范：
//!
//! - bundle = 一个目录树；**concept = 一个 .md 文件，身份 = 文件路径（去 .md）**。
//!   → 我们用**真目录路径**作 concept 路径（`src/api` → `src/api.md`），身份即路径。
//! - frontmatter：`--- ... ---` YAML；**唯一必填 `type`**（非空短字符串）；可选
//!   `title`/`description`/`resource`/`tags`/`timestamp`。生产者可加任意额外键；
//!   **消费者必须保留未知键、绝不因未知字段/type/版本报错**。
//!   → 写：`emit_frontmatter` 永远输出非空 `type`。读：`parse_frontmatter` 容错，
//!   缺 `type` 给缺省、未知键忽略但不报错。
//! - 根 `index.md`：**唯一**可带 frontmatter 的 index，且只放 `okf_version: "0.1"`；
//!   其余为 concept 链接清单。子目录 `index.md` 纯清单、无 frontmatter。
//!   → `render_root_index` 输出 `okf_version: "0.1"` frontmatter + 全 concept 链接。
//! - `log.md`（可选）：按日期分组的变更史。→ `OkfBundle.log_md`，可空。
//! - concept 间用**普通 markdown 链接**互连；外部引用放 `# Citations`。
//! - 容错：断链不报错、缺字段/缺 index best-effort。
//!
//! 与 MDGA 工作存储的关系：本模块是**纯 OKF 导出/导入**——`write_okf_bundle` 绝不写
//! `index.jsonl`/`.fingerprint` 等 MDGA sidecar；wiki 现有的 `store`/`query`/`fingerprint`
//! 逻辑一字未动，本模块只是新增的投影层。

use crate::sections::WikiSection;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, Component, Path as StdPath};

/// 一个 OKF concept（= bundle 里的一个 .md 文件）。身份 = `rel_path` 去掉 `.md`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OkfConcept {
    /// 在 bundle 内的**真**相对路径（正斜杠，含 `.md`），如 `"src/api.md"`。身份 = 去 `.md`。
    pub rel_path: String,
    /// frontmatter 唯一必填项：concept 类型。非空短字符串。
    pub type_: String,
    /// 可选标题（一句）。
    pub title: Option<String>,
    /// 可选描述（一句）。
    pub description: Option<String>,
    /// 可选标签列表（可空）。
    pub tags: Vec<String>,
    /// 可选 ISO 8601 时间戳。
    pub timestamp: Option<String>,
    /// frontmatter 之后的 markdown 正文。
    pub body: String,
}

/// 一个完整的 OKF bundle（内存表示）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OkfBundle {
    /// 所有 concept。
    pub concepts: Vec<OkfConcept>,
    /// 根 `index.md` 的完整内容（含 `okf_version: "0.1"` frontmatter + concept 清单）。
    pub index_md: String,
    /// 可选 `log.md` 的内容。
    pub log_md: Option<String>,
}

// ============================ section → OKF 投影 ============================

/// 把内部区段投影为严格合规的 OKF v0.1 bundle。`timestamp` 由调用方传入（便于测试确定性）。
///
/// 映射要点：
///   - 每个 `section.directory` → 一个 concept，rel_path = **真目录路径** + `.md`
///     （`"src/api"` → `"src/api.md"`）；根 `"."` → `"root.md"`（描述仓库根）。
///   - frontmatter：`type` 由 role 派生一个稳定且有意义的值（见 [`type_from_role`]，**非空**）；
///     `title` = 目录名（根为 `"(repository root)"`）；`description` = summary 优先否则 role；
///     `tags` 从 role 拆词（可空）；`timestamp` = 传入值。
///   - body：复用 [`render_section_body`]（Role/Key files/Top symbols 表，**无重复 H1**），
///     末尾追加「## 相关」相对链接指向父/子 concept（断链容忍）。
pub fn sections_to_okf(sections: &[WikiSection], timestamp: &str) -> OkfBundle {
    // 先把每个目录映射成**消歧后**的 concept rel_path，建一张「目录 → rel_path」表，
    // 供「## 相关」生成父/子相对链接时查找（断链容忍：查不到就跳过）。
    //
    // 消歧（fix ②）：bundle 的保留文件名 `index.md`/`log.md` 必须预占，绝不能让任一 concept
    // 落到这两个名上（否则 write 会与根 index/log 撞名、read 会把它误归为 index/log 而丢 concept）；
    // 两个不同目录也绝不能映射到同一文件（如真有 `index/` 目录其默认 `index.md` 与保留名撞）。
    // 命中保留名或与已分配 rel_path 撞名 → 追加去歧后缀，保证唯一且不落保留名。
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    // 预占 bundle 保留名（小写比较：写出的就是这两个确切名字）。
    used.insert("index.md".to_string());
    used.insert("log.md".to_string());

    let mut dir_to_path: BTreeMap<String, String> = BTreeMap::new();
    for s in sections {
        let rel_path = disambiguate_rel_path(&concept_rel_path(&s.directory), &mut used);
        dir_to_path.insert(s.directory.clone(), rel_path);
    }

    let mut concepts: Vec<OkfConcept> = Vec::with_capacity(sections.len());
    for s in sections {
        // 用上面已消歧并落进 dir_to_path 的最终 rel_path（保证「## 相关」链接指向正确文件）。
        let rel_path = dir_to_path
            .get(&s.directory)
            .cloned()
            .unwrap_or_else(|| concept_rel_path(&s.directory));
        let title = dir_title(&s.directory);
        // description：summary 优先（取首句/整段去换行），否则退回 role。
        let description = s
            .summary
            .as_ref()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .map(one_line)
            .unwrap_or_else(|| s.role.clone());

        let mut body = render_section_body(s);
        // 追加「## 相关」：相对 markdown 链接指向父/子目录 concept（有就加、断链容忍）。
        let related = render_related(&s.directory, &rel_path, &dir_to_path);
        if !related.is_empty() {
            body.push_str(&related);
        }

        concepts.push(OkfConcept {
            rel_path,
            type_: type_from_role(&s.role, &s.directory),
            title: Some(title),
            description: Some(description),
            tags: tags_from_role(&s.role),
            timestamp: Some(timestamp.to_string()),
            body,
        });
    }

    let index_md = render_root_index(&concepts);

    OkfBundle {
        concepts,
        index_md,
        log_md: None,
    }
}

/// 目录 → concept 的**真**相对路径。`"."`/空 → `"root.md"`；否则 `"<dir>.md"`（正斜杠）。
fn concept_rel_path(directory: &str) -> String {
    if directory.is_empty() || directory == "." {
        return "root.md".to_string();
    }
    // 统一为正斜杠（codemap 已是正斜杠，这里防御性处理 Windows 反斜杠）。
    let norm = directory.replace('\\', "/");
    let norm = norm.trim_matches('/');
    if norm.is_empty() {
        return "root.md".to_string();
    }
    format!("{norm}.md")
}

/// 把候选 rel_path 消歧为**唯一且不落 bundle 保留名（`index.md`/`log.md`）**的最终 rel_path。
///
/// `used` 既是已占名集合（含预占的两个保留名）又承载本次分配（成功后插入返回值）。
/// 撞名策略（保持真目录路径口径、仅在文件名 stem 上加后缀，仍合规：concept 身份 = 路径去 `.md`）：
///
/// 1. 候选未占用 → 直接用。
/// 2. 撞名 → 先试 `<stem>.concept.md`；再撞 → `<stem>-2.md`、`<stem>-3.md` … 直到空位。
///
/// 后缀只动**文件名 stem**、保留目录前缀，故 `index/foo.md` 不受影响，仅根级 `index.md`/`log.md`
/// 以及与之/彼此撞名者被改名。返回的 rel_path 一定不在 `used` 内、一定不是保留名。
fn disambiguate_rel_path(candidate: &str, used: &mut std::collections::HashSet<String>) -> String {
    if !used.contains(candidate) {
        used.insert(candidate.to_string());
        return candidate.to_string();
    }
    // 拆出目录前缀与 stem（去 `.md`）。candidate 形如 `a/b.md` 或 `root.md`。
    let (dir_prefix, stem) = match candidate.rfind('/') {
        Some(idx) => {
            let (d, f) = candidate.split_at(idx + 1); // d 含尾随 '/'
            (d.to_string(), f.trim_end_matches(".md").to_string())
        }
        None => (String::new(), candidate.trim_end_matches(".md").to_string()),
    };
    let make = |suffix: &str| format!("{dir_prefix}{stem}{suffix}.md");
    // 先试 `.concept` 后缀。
    let first = make(".concept");
    if !used.contains(&first) {
        used.insert(first.clone());
        return first;
    }
    // 再退回数字后缀 `-2`、`-3` …（理论上几乎到不了，但保证终止于空位）。
    let mut n = 2usize;
    loop {
        let cand = make(&format!("-{n}"));
        if !used.contains(&cand) {
            used.insert(cand.clone());
            return cand;
        }
        n += 1;
    }
}

/// 目录的人类可读标题。根 → `"(repository root)"`；否则取末段（更短、更像标题）。
fn dir_title(directory: &str) -> String {
    if directory.is_empty() || directory == "." {
        return "(repository root)".to_string();
    }
    directory
        .replace('\\', "/")
        .trim_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(directory)
        .to_string()
}

/// 由角色（与目录）派生一个**稳定、有意义、非空**的 OKF `type`。
///
/// OKF 不约束 `type` 取值（生产者自定，消费者不得因未知 type 报错），但要求非空短字符串。
/// 我们用一组通用、自描述的 type：`module`（普通源码目录）、`directory`（中性/无强信号）、
/// 以及若干语义化值（`test-suite`/`documentation`/`api`/`data-model`/`ui-component`/
/// `configuration`/`tooling`），从 role 文本里识别关键词得出。根目录用 `repository`。
/// **永不返回空**：兜底为 `"directory"`。
fn type_from_role(role: &str, directory: &str) -> String {
    if directory.is_empty() || directory == "." {
        return "repository".to_string();
    }
    let r = role.to_lowercase();
    // 关键词 → 语义 type（顺序：更具体在前）。
    let ty = if r.contains("test") {
        "test-suite"
    } else if r.contains("benchmark") {
        "benchmark"
    } else if r.contains("documentation") || r.contains("doc") {
        "documentation"
    } else if r.contains("example") || r.contains("demo") || r.contains("sample") {
        "example"
    } else if r.contains("api") || r.contains("handler") || r.contains("route") || r.contains("endpoint") {
        "api"
    } else if r.contains("model") || r.contains("schema") || r.contains("entit")
        || r.contains("type") || r.contains("data definition")
    {
        "data-model"
    } else if r.contains("component") || r.contains("ui ") || r.contains("page") || r.contains("screen") {
        "ui-component"
    } else if r.contains("config") || r.contains("setting") {
        "configuration"
    } else if r.contains("tooling") || r.contains("script") || r.contains("build") {
        "tooling"
    } else if r.contains("migration") {
        "migration"
    } else if r.contains("middleware") {
        "middleware"
    } else if r.contains("service") {
        "service"
    } else if r.contains("state") || r.contains("store") || r.contains("reducer") {
        "state"
    } else if r.contains("asset") || r.contains("static") || r.contains("style") || r.contains("theme") {
        "asset"
    } else if r.contains("source") || r.contains("engine") || r.contains("core")
        || r.contains("module") || r.contains("internal")
    {
        "module"
    } else {
        // 有 role 但无识别关键词：仍非空，给中性但有意义的 "directory"。
        "directory"
    };
    ty.to_string()
}

/// 从 role 文本拆出标签：取字母数字词、小写、去停用词与过短词、去重，保序。可返回空向量。
fn tags_from_role(role: &str) -> Vec<String> {
    const STOP: &[&str] = &["the", "and", "for", "with", "of", "to", "a", "an", "in", "on"];
    let mut seen: Vec<String> = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        if cur.len() >= 3 {
            let w = std::mem::take(cur);
            if !STOP.contains(&w.as_str()) && !out.contains(&w) {
                out.push(w);
            }
        } else {
            cur.clear();
        }
    };
    for ch in role.chars() {
        if ch.is_ascii_alphanumeric() {
            for c in ch.to_lowercase() {
                cur.push(c);
            }
        } else {
            flush(&mut cur, &mut seen);
        }
    }
    flush(&mut cur, &mut seen);
    seen
}

/// 取一段文本的「一行」表示：折叠所有空白（含换行）为单空格、去首尾。用于 description。
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 渲染区段正文（**无 H1**，title 已进 frontmatter）。复刻 `store::render_markdown` 的正文部分：
/// Role / (Summary) / Key files / Top symbols 表 + 派生说明脚注。`store.rs` 一字未改。
fn render_section_body(s: &WikiSection) -> String {
    let mut out = String::new();
    out.push_str(&format!("**Role:** {}\n\n", s.role));
    out.push_str(&format!("**Files in this directory:** {}\n\n", s.file_count));

    if let Some(summary) = s
        .summary
        .as_ref()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
    {
        out.push_str("## Summary\n\n");
        out.push_str(summary);
        out.push_str("\n\n");
    }

    if !s.key_files.is_empty() {
        out.push_str("## Key files\n\n");
        for f in &s.key_files {
            out.push_str(&format!("- `{f}`\n"));
        }
        out.push('\n');
    }

    if !s.symbols.is_empty() {
        out.push_str("## Top symbols\n\n");
        out.push_str("| Symbol | File | Line | Signature |\n");
        out.push_str("| --- | --- | --- | --- |\n");
        for sym in &s.symbols {
            let sig = sym.signature.replace('|', "\\|").replace('\n', " ");
            out.push_str(&format!(
                "| `{}` | `{}` | {} | `{}` |\n",
                sym.name, sym.file, sym.line, sig
            ));
        }
        out.push('\n');
    }

    out.push_str(
        "_Derived by mdga-wiki from codemap analysis (tree-sitter + PageRank). \
         Regenerable; do not edit by hand._\n",
    );
    out
}

/// 生成「## 相关」段：相对 markdown 链接指向**已存在**的父目录 / 直接子目录 concept。
/// 断链容忍：只链接 `dir_to_path` 里真实存在的目录；查不到就跳过。无任何相关项时返回空串。
fn render_related(
    directory: &str,
    self_rel: &str,
    dir_to_path: &BTreeMap<String, String>,
) -> String {
    let mut links: Vec<(String, String)> = Vec::new(); // (label, relative-link)

    // 父目录。
    if let Some(parent) = parent_directory(directory) {
        if let Some(target) = dir_to_path.get(&parent) {
            if let Some(rel) = relative_link(self_rel, target) {
                let label = if parent == "." {
                    "(repository root)".to_string()
                } else {
                    parent.clone()
                };
                links.push((format!("parent: {label}"), rel));
            }
        }
    }

    // 直接子目录（按目录字典序）。
    for (dir, target) in dir_to_path {
        if is_direct_child(directory, dir) {
            if let Some(rel) = relative_link(self_rel, target) {
                links.push((format!("child: {dir}"), rel));
            }
        }
    }

    if links.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n## 相关\n\n");
    for (label, rel) in links {
        // 转义 label（`[ ] ( ) \`）与 target（空格/`(`/`)`）保证产出合法 markdown 链接。
        out.push_str(&format!("- [{}]({})\n", md_escape_label(&label), md_escape_dest(&rel)));
    }
    out
}

/// 转义 markdown 链接 **label**：对 `\ [ ] ( )` 前置反斜杠（避免破坏 `[...]` 结构）。
fn md_escape_label(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '\\' | '[' | ']' | '(' | ')') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// 转义 markdown 链接 **target/destination**：空格→`%20`、`(`→`%28`、`)`→`%29`
/// （避免破坏 `(...)` 结构、避免空格截断 URL）。其余字符原样（我们的 target 已是正斜杠相对路径）。
fn md_escape_dest(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            ' ' => out.push_str("%20"),
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            _ => out.push(ch),
        }
    }
    out
}

/// 父目录（正斜杠口径，与 codemap 一致）。根 `"."` 无父 → None；顶层目录父为 `"."`。
fn parent_directory(directory: &str) -> Option<String> {
    if directory.is_empty() || directory == "." {
        return None;
    }
    let norm = directory.replace('\\', "/");
    let norm = norm.trim_matches('/');
    match norm.rfind('/') {
        Some(idx) => Some(norm[..idx].to_string()),
        None => Some(".".to_string()),
    }
}

/// `child` 是否为 `parent` 的**直接**子目录。
fn is_direct_child(parent: &str, child: &str) -> bool {
    match parent_directory(child) {
        Some(p) => {
            let pn = parent.replace('\\', "/");
            let pn = if pn.is_empty() { "." } else { pn.trim_matches('/') };
            let pn = if pn.is_empty() { "." } else { pn };
            p == pn
        }
        None => false,
    }
}

/// 由「当前 concept 的 rel_path」与「目标 concept 的 rel_path」算出相对 markdown 链接。
/// 同目录 → `./name.md`；否则用 `../` 上跳再下行。失败（异常路径）返回 None（断链容忍）。
fn relative_link(from_rel: &str, to_rel: &str) -> Option<String> {
    let from_dir: Vec<&str> = {
        let p = from_rel.trim_end_matches(|c| c != '/');
        p.trim_end_matches('/').split('/').filter(|s| !s.is_empty()).collect()
    };
    let to_parts: Vec<&str> = to_rel.split('/').filter(|s| !s.is_empty()).collect();
    if to_parts.is_empty() {
        return None;
    }
    let (to_dir, to_file) = to_parts.split_at(to_parts.len() - 1);

    // 公共前缀。
    let mut common = 0usize;
    while common < from_dir.len() && common < to_dir.len() && from_dir[common] == to_dir[common] {
        common += 1;
    }
    let ups = from_dir.len() - common;
    let mut rel = String::new();
    if ups == 0 {
        rel.push_str("./");
    } else {
        for _ in 0..ups {
            rel.push_str("../");
        }
    }
    for seg in &to_dir[common..] {
        rel.push_str(seg);
        rel.push('/');
    }
    rel.push_str(to_file[0]);
    Some(rel)
}

/// 渲染根 `index.md`：**唯一**带 frontmatter 的 index，且 frontmatter **只**放 `okf_version: "0.1"`；
/// 正文为全部 concept 的 markdown 链接清单（按 rel_path 字典序，稳定）。
fn render_root_index(concepts: &[OkfConcept]) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str("okf_version: \"0.1\"\n");
    out.push_str("---\n\n");
    out.push_str("# Knowledge bundle index\n\n");

    let mut sorted: Vec<&OkfConcept> = concepts.iter().collect();
    sorted.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    for c in sorted {
        let label = c.title.clone().unwrap_or_else(|| c.rel_path.clone());
        // 根相对链接：以 `/` 开头表示 bundle 根（OKF 允许 `/a/b.md` 形态）。
        // 转义 label 与 target，保证含 `( ) [ ]`/空格的目录名仍产出合法链接。
        out.push_str(&format!(
            "- [{}](/{})\n",
            md_escape_label(&label),
            md_escape_dest(&c.rel_path)
        ));
    }
    out
}

// ============================ frontmatter 读写 ============================

/// 把一个 concept 渲染成完整 .md（frontmatter + body）。`type` 永远输出、非空。
pub fn render_concept_md(c: &OkfConcept) -> String {
    let mut out = String::new();
    out.push_str(&emit_frontmatter(c));
    out.push_str(&c.body);
    out
}

/// 手写 OKF frontmatter（我们完全控制结构，故不引 YAML 依赖）：
/// `--- ... ---` 包裹的 `key: value` 标量 + `tags` 列表。**`type` 永远非空输出**。
/// 字符串值含特殊字符时安全双引号引用（见 [`yaml_scalar`]）。
fn emit_frontmatter(c: &OkfConcept) -> String {
    let mut out = String::from("---\n");
    // type：唯一必填。空则兜底为 "directory"（绝不输出空 type）。
    let ty = if c.type_.trim().is_empty() {
        "directory"
    } else {
        c.type_.trim()
    };
    out.push_str(&format!("type: {}\n", yaml_scalar(ty)));
    if let Some(title) = c.title.as_ref().filter(|t| !t.trim().is_empty()) {
        out.push_str(&format!("title: {}\n", yaml_scalar(title)));
    }
    if let Some(desc) = c.description.as_ref().filter(|t| !t.trim().is_empty()) {
        out.push_str(&format!("description: {}\n", yaml_scalar(desc)));
    }
    if !c.tags.is_empty() {
        out.push_str("tags:\n");
        for t in &c.tags {
            out.push_str(&format!("  - {}\n", yaml_scalar(t)));
        }
    }
    if let Some(ts) = c.timestamp.as_ref().filter(|t| !t.trim().is_empty()) {
        out.push_str(&format!("timestamp: {}\n", yaml_scalar(ts)));
    }
    out.push_str("---\n\n");
    out
}

/// 把一个标量安全地表示为 YAML：纯净的简单字符串原样输出，否则双引号引用并转义。
fn yaml_scalar(s: &str) -> String {
    let needs_quote = s.is_empty()
        || s.chars().next().map(|c| c.is_whitespace()).unwrap_or(false)
        || s.chars().last().map(|c| c.is_whitespace()).unwrap_or(false)
        || s.contains(|c: char| {
            matches!(
                c,
                ':' | '#' | '"' | '\'' | '\n' | '\t' | '{' | '}' | '[' | ']'
                    | ',' | '&' | '*' | '!' | '|' | '>' | '%' | '@' | '`'
            )
        })
        // 以这些字符开头会被 YAML 当作特殊语义，需引用。
        || s.starts_with('-')
        || s.starts_with('?')
        // 看起来像布尔/null/数字的也引用，确保读回来仍是字符串。
        || matches!(s.to_lowercase().as_str(), "true" | "false" | "null" | "yes" | "no" | "~");
    if !needs_quote {
        return s.to_string();
    }
    let mut q = String::with_capacity(s.len() + 2);
    q.push('"');
    for ch in s.chars() {
        match ch {
            '"' => q.push_str("\\\""),
            '\\' => q.push_str("\\\\"),
            '\n' => q.push_str("\\n"),
            '\t' => q.push_str("\\t"),
            _ => q.push(ch),
        }
    }
    q.push('"');
    q
}

/// 解析 OKF frontmatter（宽松手写，**容错**）：在文档开头识别 `--- ... ---` 块，
/// 提取我们关心的标量/列表键；**未知键忽略但不报错、缺 `type` 不报错**。
/// 返回 (frontmatter字段, body)。无 frontmatter → 字段全缺省、body = 全文。
struct ParsedFront {
    type_: Option<String>,
    title: Option<String>,
    description: Option<String>,
    tags: Vec<String>,
    timestamp: Option<String>,
}

fn parse_frontmatter(text: &str) -> (ParsedFront, String) {
    let empty = ParsedFront {
        type_: None,
        title: None,
        description: None,
        tags: Vec::new(),
        timestamp: None,
    };
    // 去除可能的 BOM / 前导空行后，必须以 `---` 行起头才算有 frontmatter。
    let trimmed = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = trimmed.lines();
    let first = match lines.next() {
        Some(l) => l,
        None => return (empty, String::new()),
    };
    if first.trim() != "---" {
        // 无 frontmatter：整篇为 body（容错：best-effort）。
        return (empty, text.to_string());
    }

    let mut front = empty;
    let mut in_tags = false;
    let mut consumed = first.len() + 1; // 已吃掉首行（含换行近似）。
    let mut closed = false;
    // 逐行直到闭合的 `---`。
    for line in trimmed.lines().skip(1) {
        consumed += line.len() + 1;
        if line.trim() == "---" {
            closed = true;
            break;
        }
        // tags 列表项：`  - value`。
        let lt = line.trim_start();
        if in_tags && lt.starts_with('-') {
            let v = lt[1..].trim();
            let v = unquote_scalar(v);
            if !v.is_empty() {
                front.tags.push(v);
            }
            continue;
        }
        // 形如 `key: value`。无冒号 → 容错跳过。
        if let Some(idx) = line.find(':') {
            let key = line[..idx].trim().to_lowercase();
            let val = line[idx + 1..].trim();
            match key.as_str() {
                "tags" => {
                    in_tags = true;
                    // 支持内联 `tags: [a, b]`。
                    if !val.is_empty() {
                        for part in val.trim_matches(|c| c == '[' || c == ']').split(',') {
                            let v = unquote_scalar(part.trim());
                            if !v.is_empty() {
                                front.tags.push(v);
                            }
                        }
                    }
                    continue;
                }
                _ => {
                    in_tags = false;
                }
            }
            let v = unquote_scalar(val);
            match key.as_str() {
                "type" => front.type_ = Some(v),
                "title" => front.title = Some(v),
                "description" => front.description = Some(v),
                "timestamp" => front.timestamp = Some(v),
                // 未知键：保留语义上「忽略但不报错」。
                _ => {}
            }
        } else {
            in_tags = false;
        }
    }

    if !closed {
        // frontmatter 未闭合：容错——当作没有 frontmatter，整篇作 body。
        return (
            ParsedFront {
                type_: None,
                title: None,
                description: None,
                tags: Vec::new(),
                timestamp: None,
            },
            text.to_string(),
        );
    }

    // body = 闭合 `---` 之后的剩余原文。用字符计数法切，避免行尾换行差异。
    let body = remainder_after_front(trimmed, consumed);
    (front, body)
}

/// 取 frontmatter 闭合后的正文。从近似消费长度处起，跳过紧随的一个空行（美观，不影响语义）。
fn remainder_after_front(text: &str, approx_consumed: usize) -> String {
    let idx = approx_consumed.min(text.len());
    let rest = &text[idx..];
    rest.strip_prefix('\n').unwrap_or(rest).to_string()
}

/// 去掉标量两端的单/双引号并做基本反转义；非引用值原样去首尾空白返回。
fn unquote_scalar(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        let inner = &s[1..s.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some(other) => out.push(other),
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        out
    } else if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        s[1..s.len() - 1].replace("''", "'")
    } else {
        s.to_string()
    }
}

// ============================ 写 / 读 bundle ============================

/// 返回 bundle 的全部文件 `(rel_path 正斜杠, 内容)`，供「写目录」(`write_okf_bundle`) 与
/// 「打包成单个 .zip」(desktop 命令层) 共用**同一布局**——保证两条产出路径绝不漂移。
///
/// concept 用其（已消歧的）真路径，经 [`sanitize_rel_path`] 净化（剔除 `..`/空段，防越界）；
/// 万一某 concept 净化后落到保留名 `index.md`/`log.md` 或为空 → 跳过（绝不覆盖根 index/log）。
/// 末尾追加根 `index.md`（+ `log.md` 若有）。顺序：concepts 原序 + index +（log）。
pub fn okf_bundle_files(bundle: &OkfBundle) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::with_capacity(bundle.concepts.len() + 2);
    for c in &bundle.concepts {
        let Some(rel) = sanitize_rel_path(&c.rel_path) else {
            continue; // 异常路径：跳过该 concept（防越界），不整体失败。
        };
        let rel_str = path_to_forward_slash(&rel);
        if rel_str.is_empty() || rel_str == "index.md" || rel_str == "log.md" {
            continue; // 防御：concept 不得占用保留名。
        }
        out.push((rel_str, render_concept_md(c)));
    }
    out.push(("index.md".to_string(), bundle.index_md.clone()));
    if let Some(log) = &bundle.log_md {
        out.push(("log.md".to_string(), log.clone()));
    }
    out
}

/// 把净化后的相对路径转成正斜杠字符串（zip 条目名 / 写目录键共用，跨平台稳定）。
fn path_to_forward_slash(p: &Path) -> String {
    p.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// 写出**纯 OKF** bundle 到 `target_dir`：按各 concept 的 rel_path 建真子目录写 .md +
/// 根 `index.md`（+ `log.md` 若有）。原子写（临时→rename）。布局取自 [`okf_bundle_files`]。
///
/// **绝不写** `index.jsonl`/`.fingerprint` 等 MDGA sidecar。写前清理 `target_dir` 里上次写的
/// concept .md/index.md/log.md（避免陈旧），但**只动 .md/index.md/log.md**、绝不碰用户其它文件。
pub fn write_okf_bundle(bundle: &OkfBundle, target_dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(target_dir)?;

    // 1) 清理：递归删除 target_dir 下**我们会产出的**文件类别（.md 文件 + index.md + log.md）。
    //    只删 .md（任意子目录下的 .md 都是我们的产物口径），绝不删非 .md 用户文件。
    clean_md_recursive(target_dir);

    // 2) 写全部文件（concept .md + 根 index.md + 可选 log.md），与 zip 打包同一布局。
    for (rel, content) in okf_bundle_files(bundle) {
        let dest = target_dir.join(&rel); // rel 正斜杠；std::Path 跨平台按分隔符解析。
        // 纵深防御：rel 已净化无 `..`，仍校验解析后必须仍在 target_dir 内。
        if !is_within(target_dir, &dest) {
            continue;
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        write_atomic(&dest, content.as_bytes())?;
    }

    Ok(())
}

/// 读取外部 OKF bundle（供消费/展示）。遍历所有 .md，解析 frontmatter（**容错**）。
/// rel_path = 相对 `dir` 的正斜杠路径。根 `index.md`/`log.md` 不作为 concept，分别归入
/// `index_md`/`log_md`；其余每个 .md 都是一个 concept（含子目录 index.md，作普通 concept 收纳，
/// 不丢）。解析失败的 concept 退化为 type 缺省 + body = 全文、绝不 panic、绝不丢 concept。
pub fn read_okf_bundle(dir: &Path) -> OkfBundle {
    let mut concepts: Vec<OkfConcept> = Vec::new();
    let mut index_md = String::new();
    let mut log_md: Option<String> = None;

    // 索引闸与 read 闸同口径（fix ④）：以 canonicalize 后的 base 为边界，逐个候选 .md 解析后必须
    // 仍 starts_with(canon_base)，否则（越界 symlink/解析失败）**跳过不收**，绝不泄露根外
    // frontmatter。canonicalize 失败则退回原 dir（best-effort，不崩）。
    let canon_base = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());

    let mut files: Vec<std::path::PathBuf> = Vec::new();
    collect_md_files(dir, &mut files);
    files.sort();

    for path in files {
        let rel = match rel_to(dir, &path) {
            Some(r) => r,
            None => continue,
        };
        // 越界校验：候选解析后须仍在 canon_base 内。解析失败/越界 → 跳过（best-effort，不崩）。
        match path.canonicalize() {
            Ok(canon) if canon.starts_with(&canon_base) => {}
            _ => continue,
        }
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => continue, // 读失败：跳过该文件，best-effort。
        };

        // 根级 index.md / log.md 特殊归类（子目录的 index.md 仍当 concept，不丢）。
        if rel == "index.md" {
            index_md = text;
            continue;
        }
        if rel == "log.md" {
            log_md = Some(text);
            continue;
        }

        let (front, body) = parse_frontmatter(&text);
        // 容错：缺 type → 退化为缺省 "concept"（非空，符合「不因缺字段报错」）。
        let type_ = front
            .type_
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "concept".to_string());
        concepts.push(OkfConcept {
            rel_path: rel,
            type_,
            title: front.title.filter(|t| !t.trim().is_empty()),
            description: front.description.filter(|t| !t.trim().is_empty()),
            tags: front.tags,
            timestamp: front.timestamp.filter(|t| !t.trim().is_empty()),
            body,
        });
    }

    concepts.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    OkfBundle {
        concepts,
        index_md,
        log_md,
    }
}

// ============================ 文件系统辅助 ============================

/// 递归收集 `dir` 下所有 `.md` 文件的绝对路径。容错：读目录失败的子树跳过。
fn collect_md_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_md_files(&path, out);
        } else if path.extension().map(|e| e == "md").unwrap_or(false) {
            out.push(path);
        }
    }
}

/// 递归删除 `dir` 下所有 `.md` 文件（我们产出的全部类别都是 .md）。绝不删非 .md 用户文件、
/// 绝不删目录本身。容错：失败的删除忽略。
fn clean_md_recursive(dir: &Path) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            clean_md_recursive(&path);
        } else if path.extension().map(|e| e == "md").unwrap_or(false) {
            let _ = fs::remove_file(&path);
        }
    }
}

/// 计算 `path` 相对 `base` 的正斜杠相对路径。失败（不在 base 下/异常）→ None。
fn rel_to(base: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(base).ok()?;
    let mut parts: Vec<String> = Vec::new();
    for comp in rel.components() {
        // 只收 Normal 组件；其它（不应出现）安全起见忽略。
        if let Component::Normal(s) = comp {
            parts.push(s.to_string_lossy().to_string());
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// 把 concept 的 rel_path 净化为安全的相对路径分量序列（剔除空段/`.`/`..`，正反斜杠都拆）。
/// 全空 → None。这样恶意/异常 rel_path 绝不可能逃出 target_dir。
fn sanitize_rel_path(rel_path: &str) -> Option<std::path::PathBuf> {
    let mut buf = std::path::PathBuf::new();
    for seg in rel_path.split(['/', '\\']) {
        let seg = seg.trim();
        if seg.is_empty() || seg == "." || seg == ".." {
            continue;
        }
        buf.push(seg);
    }
    if buf.as_os_str().is_empty() {
        None
    } else {
        Some(buf)
    }
}

/// `candidate` 是否在 `base` 目录内（按路径组件判断，不解析符号链接；输入已净化无 `..`）。
fn is_within(base: &Path, candidate: &Path) -> bool {
    let base_comps: Vec<Component> = normalize_components(base);
    let cand_comps: Vec<Component> = normalize_components(candidate);
    if cand_comps.len() < base_comps.len() {
        return false;
    }
    base_comps
        .iter()
        .zip(cand_comps.iter())
        .all(|(a, b)| a == b)
}

/// 取一条路径的「Normal/Root/Prefix」组件序列（剔除 `.`；输入已无 `..`）。
fn normalize_components(p: &StdPath) -> Vec<Component<'_>> {
    p.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect()
}

/// 原子写：写临时文件再 rename（Windows 上先删旧目标）。与 `store::write_atomic` 同口径，
/// 但为「纯 OKF、不依赖 store」而在本模块独立实现（不改 store.rs）。
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "目标路径无父目录")
    })?;
    let tmp = parent.join(format!(
        ".{}.okf.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("okf")
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
    }
    let _ = fs::remove_file(path);
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sections::WikiSymbol;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp_dir() -> std::path::PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let id = N.fetch_add(1, Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("mdga-okf-{}-{}", std::process::id(), id));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn section(dir: &str, role: &str) -> WikiSection {
        WikiSection {
            directory: dir.to_string(),
            role: role.to_string(),
            file_count: 1,
            key_files: vec![format!("{}/a.rs", dir.trim_end_matches('.'))],
            symbols: vec![WikiSymbol {
                name: "run".to_string(),
                file: format!("{}/a.rs", dir.trim_end_matches('.')),
                line: 3,
                signature: "pub fn run() -> Result<(), Error>".to_string(),
            }],
            summary: None,
        }
    }

    const TS: &str = "2026-06-22T00:00:00Z";

    #[test]
    fn each_concept_has_nonempty_type() {
        let secs = vec![
            section("src/api", "API / request handlers"),
            section("src/core", "core engine"),
            section(".", "repository root"),
        ];
        let bundle = sections_to_okf(&secs, TS);
        assert_eq!(bundle.concepts.len(), 3);
        for c in &bundle.concepts {
            assert!(!c.type_.trim().is_empty(), "type 必须非空：{:?}", c);
            // 渲染出的 frontmatter 必含一行非空 type。
            let md = render_concept_md(c);
            assert!(md.starts_with("---\n"), "应以 frontmatter 起头");
            assert!(
                md.lines().any(|l| l.starts_with("type:") && l.trim() != "type:"),
                "frontmatter 必含非空 type，实得：\n{md}"
            );
        }
    }

    #[test]
    fn rel_path_is_real_directory_path() {
        let secs = vec![section("src/api", "API / request handlers")];
        let bundle = sections_to_okf(&secs, TS);
        let c = &bundle.concepts[0];
        assert_eq!(c.rel_path, "src/api.md", "应是真路径而非 sanitize 扁平名");
        assert!(!c.rel_path.contains("__"), "不应出现 src__api 扁平形态");
    }

    #[test]
    fn root_directory_maps_to_root_md() {
        let secs = vec![section(".", "repository root")];
        let bundle = sections_to_okf(&secs, TS);
        let c = &bundle.concepts[0];
        assert_eq!(c.rel_path, "root.md");
        assert_eq!(c.type_, "repository");
        assert_eq!(c.title.as_deref(), Some("(repository root)"));
    }

    #[test]
    fn root_index_has_okf_version_and_links() {
        let secs = vec![
            section("src/api", "API / request handlers"),
            section("src/core", "core engine"),
        ];
        let bundle = sections_to_okf(&secs, TS);
        assert!(
            bundle.index_md.contains("okf_version: \"0.1\""),
            "根 index.md 必含 okf_version: \"0.1\"，实得：\n{}",
            bundle.index_md
        );
        // 必含 concept 链接（指向真路径）。
        assert!(bundle.index_md.contains("(/src/api.md)"), "应含 src/api.md 链接");
        assert!(bundle.index_md.contains("(/src/core.md)"), "应含 src/core.md 链接");
    }

    #[test]
    fn body_has_no_duplicate_h1() {
        let secs = vec![section("src/api", "API / request handlers")];
        let bundle = sections_to_okf(&secs, TS);
        let body = &bundle.concepts[0].body;
        // title 已在 frontmatter，body 不应再有 H1（# 开头行）。
        assert!(
            !body.lines().any(|l| l.starts_with("# ")),
            "body 不应有重复 H1，实得：\n{body}"
        );
        // 但应保留 Role / Key files / Top symbols。
        assert!(body.contains("**Role:**"));
        assert!(body.contains("## Top symbols"));
    }

    #[test]
    fn type_derivation_is_meaningful_and_stable() {
        assert_eq!(type_from_role("test suite", "x/tests"), "test-suite");
        assert_eq!(type_from_role("API / request handlers", "x/api"), "api");
        assert_eq!(type_from_role("documentation", "x/docs"), "documentation");
        assert_eq!(type_from_role("core engine", "x/core"), "module");
        assert_eq!(type_from_role("Rust source", "x/foo"), "module");
        // 无识别关键词但非空 → 中性非空 type。
        assert_eq!(type_from_role("something unusual zzz", "x/foo"), "directory");
        // 稳定：同输入同输出。
        assert_eq!(type_from_role("core engine", "x/core"), type_from_role("core engine", "x/core"));
    }

    #[test]
    fn write_then_read_roundtrips() {
        let d = tmp_dir();
        let secs = vec![
            section("src/api", "API / request handlers"),
            section("src/core", "core engine"),
            section(".", "repository root"),
        ];
        let bundle = sections_to_okf(&secs, TS);
        write_okf_bundle(&bundle, &d).unwrap();

        // 真子目录与真路径文件应存在。
        assert!(d.join("src").join("api.md").is_file(), "应写出 src/api.md");
        assert!(d.join("src").join("core.md").is_file());
        assert!(d.join("root.md").is_file());
        assert!(d.join("index.md").is_file());

        let read = read_okf_bundle(&d);
        // 概念数一致。
        assert_eq!(read.concepts.len(), bundle.concepts.len(), "概念数应一致");
        // 路径与 type 一致（按 rel_path 排序后逐一比对）。
        let mut want: Vec<(String, String)> =
            bundle.concepts.iter().map(|c| (c.rel_path.clone(), c.type_.clone())).collect();
        let mut got: Vec<(String, String)> =
            read.concepts.iter().map(|c| (c.rel_path.clone(), c.type_.clone())).collect();
        want.sort();
        got.sort();
        assert_eq!(got, want, "往返后路径/type 应一致");
        // index.md 往返保留 okf_version。
        assert!(read.index_md.contains("okf_version: \"0.1\""));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn write_emits_no_mdga_sidecar() {
        let d = tmp_dir();
        let bundle = sections_to_okf(&[section("src/api", "API / request handlers")], TS);
        write_okf_bundle(&bundle, &d).unwrap();
        assert!(!d.join("index.jsonl").exists(), "纯 OKF 绝不写 index.jsonl");
        assert!(!d.join(".fingerprint").exists(), "纯 OKF 绝不写 .fingerprint");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn bundle_files_match_disk_layout_and_carry_reserved_names() {
        // okf_bundle_files 应与写目录布局一致：concept 真路径 + 根 index.md（本例无 log.md）。
        let secs = vec![
            section("src/api", "API / request handlers"),
            section("src/core", "core engine"),
            section(".", "repository root"),
        ];
        let bundle = sections_to_okf(&secs, TS);
        let files = okf_bundle_files(&bundle);
        let names: std::collections::HashSet<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains("src/api.md"), "应含 concept 真路径 src/api.md");
        assert!(names.contains("src/core.md"));
        assert!(names.contains("root.md"));
        assert!(names.contains("index.md"), "应含根 index.md");
        // 每个 concept 文件内容 = render_concept_md（frontmatter + body）。
        let api = files.iter().find(|(n, _)| n == "src/api.md").unwrap();
        assert!(api.1.starts_with("---\n") && api.1.contains("**Role:**"));
        // 根 index.md 内容带 okf_version。
        let idx = files.iter().find(|(n, _)| n == "index.md").unwrap();
        assert!(idx.1.contains("okf_version: \"0.1\""));

        // 与磁盘写出的文件集合一致（同一布局，无漂移）。
        let d = tmp_dir();
        write_okf_bundle(&bundle, &d).unwrap();
        for (rel, content) in &files {
            let on_disk = d.join(rel);
            assert!(on_disk.is_file(), "okf_bundle_files 的 {rel} 应同样写到磁盘");
            assert_eq!(&fs::read_to_string(&on_disk).unwrap(), content, "{rel} 内容应一致");
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn read_tolerates_missing_type_unknown_keys_and_broken_links() {
        let d = tmp_dir();
        // 1) 缺 type、含未知键、正文有断链。
        fs::create_dir_all(d.join("a")).unwrap();
        fs::write(
            d.join("a").join("no_type.md"),
            "---\ntitle: No Type Here\nweird_unknown_key: whatever\ncustom: 123\n---\n\nBody with a [broken link](/does/not/exist.md).\n",
        )
        .unwrap();
        // 2) 完全没有 frontmatter 的裸 .md。
        fs::write(d.join("bare.md"), "# Just a heading\n\nplain body, no frontmatter\n").unwrap();
        // 3) 一个正常 concept。
        fs::write(
            d.join("ok.md"),
            "---\ntype: module\ntitle: OK\n---\n\nbody\n",
        )
        .unwrap();

        // 不应 panic、不应丢 concept。
        let read = read_okf_bundle(&d);
        assert_eq!(read.concepts.len(), 3, "三个 .md 都应被收为 concept，实得 {}", read.concepts.len());

        let by_path: std::collections::HashMap<&str, &OkfConcept> =
            read.concepts.iter().map(|c| (c.rel_path.as_str(), c)).collect();

        // 缺 type 的 concept：退化为非空缺省 type，且保留已知字段、未知键被忽略而非报错。
        let no_type = by_path.get("a/no_type.md").expect("应含 a/no_type.md");
        assert!(!no_type.type_.is_empty(), "缺 type 应退化为非空缺省");
        assert_eq!(no_type.title.as_deref(), Some("No Type Here"));
        assert!(no_type.body.contains("broken link"), "断链正文应原样保留、不报错");

        // 裸 .md：type 缺省、body = 全文。
        let bare = by_path.get("bare.md").expect("应含 bare.md");
        assert!(!bare.type_.is_empty());
        assert!(bare.body.contains("plain body"));

        let ok = by_path.get("ok.md").expect("应含 ok.md");
        assert_eq!(ok.type_, "module");

        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn rewrite_cleans_stale_md_only() {
        let d = tmp_dir();
        // 用户在 bundle 目录里放了一个非 .md 文件——绝不能被我们删。
        fs::write(d.join("USER_NOTES.txt"), "keep me").unwrap();

        let first = sections_to_okf(&[section("src/old", "core engine")], TS);
        write_okf_bundle(&first, &d).unwrap();
        assert!(d.join("src").join("old.md").is_file());

        // 重写为不含 old 的集合 → 旧 concept .md 应被清理。
        let next = sections_to_okf(&[section("src/new", "core engine")], TS);
        write_okf_bundle(&next, &d).unwrap();
        assert!(!d.join("src").join("old.md").exists(), "陈旧 .md 应被清理");
        assert!(d.join("src").join("new.md").is_file());
        // 用户非 .md 文件必须原样保留。
        assert!(d.join("USER_NOTES.txt").is_file(), "绝不动用户非 .md 文件");
        assert_eq!(fs::read_to_string(d.join("USER_NOTES.txt")).unwrap(), "keep me");

        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn frontmatter_quotes_special_chars_safely() {
        // description 含冒号 → 必须被引用，读回仍完整。
        let mut secs = vec![section("src/api", "API: handlers, routes")];
        secs[0].summary = Some("Handles HTTP: GET, POST — with #hashes".to_string());
        let bundle = sections_to_okf(&secs, TS);
        let md = render_concept_md(&bundle.concepts[0]);
        // 写出的 description 行应是带引号的安全标量。
        assert!(
            md.lines().any(|l| l.starts_with("description:") && l.contains('"')),
            "含特殊字符的 description 应被引用，实得：\n{md}"
        );
        // 往返解析回来一致。
        let d = tmp_dir();
        write_okf_bundle(&bundle, &d).unwrap();
        let read = read_okf_bundle(&d);
        let c = read
            .concepts
            .iter()
            .find(|c| c.rel_path == "src/api.md")
            .expect("应能读回 src/api.md");
        assert_eq!(
            c.description.as_deref(),
            Some("Handles HTTP: GET, POST — with #hashes"),
            "含特殊字符的 description 应往返无损"
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn related_links_are_relative_and_tolerate_missing() {
        // 父 src 存在、子 src/api/v2 存在 → src/api 应链接到两者。
        let secs = vec![
            section("src", "primary source root"),
            section("src/api", "API / request handlers"),
            section("src/api/v2", "API / request handlers"),
        ];
        let bundle = sections_to_okf(&secs, TS);
        let api = bundle
            .concepts
            .iter()
            .find(|c| c.rel_path == "src/api.md")
            .unwrap();
        assert!(api.body.contains("## 相关"), "应有相关段");
        // 相对链接：父在上一层 → ../src.md；子在下一层 → ./api/v2.md。
        assert!(api.body.contains("(../src.md)"), "父相对链接，实得：\n{}", api.body);
        assert!(api.body.contains("(./api/v2.md)"), "子相对链接，实得：\n{}", api.body);

        // 孤立目录（父/子都不在 bundle）→ 无相关段、不报错。
        let lone = sections_to_okf(&[section("deep/nested/leaf", "core engine")], TS);
        assert!(!lone.concepts[0].body.contains("## 相关"), "孤立 concept 不应有相关段");
    }

    // ── fix ②：保留名/撞名消歧，往返不丢、无两 concept 同 rel_path、不落 index.md/log.md ──
    #[test]
    fn reserved_and_colliding_dir_names_are_disambiguated_and_roundtrip() {
        let d = tmp_dir();
        let secs = vec![
            section("index", "core engine"),  // 默认会撞保留名 index.md
            section("log", "core engine"),    // 默认会撞保留名 log.md
            section("root", "core engine"),   // 真实 root 目录与 `.` 哨兵 root.md 撞
            section(".", "repository root"),   // 哨兵 root.md
            section("src/api", "API handlers"),
        ];
        let bundle = sections_to_okf(&secs, TS);

        // (a) 没有任何 concept 落到保留名 index.md / log.md。
        for c in &bundle.concepts {
            assert_ne!(c.rel_path, "index.md", "concept 绝不可落 index.md：{:?}", c);
            assert_ne!(c.rel_path, "log.md", "concept 绝不可落 log.md：{:?}", c);
        }
        // (b) 所有 rel_path 唯一（无两 concept 映射同一文件）。
        let mut paths: Vec<&str> = bundle.concepts.iter().map(|c| c.rel_path.as_str()).collect();
        paths.sort();
        let n = paths.len();
        paths.dedup();
        assert_eq!(paths.len(), n, "rel_path 必须两两不同（无撞名）");

        // (c) 往返：写出 → 读回，concept 数不丢、无两 concept 同 rel_path、不落 index.md/log.md。
        write_okf_bundle(&bundle, &d).unwrap();
        let read = read_okf_bundle(&d);
        assert_eq!(
            read.concepts.len(),
            bundle.concepts.len(),
            "往返后 concept 数不应丢：want {} got {}",
            bundle.concepts.len(),
            read.concepts.len()
        );
        let mut rpaths: Vec<&str> = read.concepts.iter().map(|c| c.rel_path.as_str()).collect();
        rpaths.sort();
        let rn = rpaths.len();
        rpaths.dedup();
        assert_eq!(rpaths.len(), rn, "往返后 rel_path 仍须两两不同");
        for c in &read.concepts {
            assert_ne!(c.rel_path, "index.md");
            assert_ne!(c.rel_path, "log.md");
        }
        // 根 index.md 仍仅 okf_version（合规：唯一带 frontmatter 的 index）。
        assert!(read.index_md.contains("okf_version: \"0.1\""));

        let _ = fs::remove_dir_all(&d);
    }

    // ── fix ③：链接 label/target 含特殊字符须合法转义 ──
    #[test]
    fn markdown_links_escape_special_chars() {
        // 目录名含 `(group)` / `[id]` / 空格。
        let secs = vec![
            section("src", "primary source root"),
            section("src/(group)", "core engine"),
            section("src/[id] thing", "core engine"),
        ];
        let bundle = sections_to_okf(&secs, TS);

        // 根 index.md 的 target：空格 → %20，括号 → %28/%29（方括号在 dest 合法、保持原样）。
        let idx = &bundle.index_md;
        assert!(idx.contains("%28group%29"), "target `(` `)` 应转义为 %28/%29：\n{idx}");
        assert!(idx.contains("[id]%20thing.md"), "含空格的 target 空格应转义为 %20：\n{idx}");
        // label 中的 `[` `]` `(` `)` 应被反斜杠转义（dir_title 取末段，如 `(group)` / `[id] thing`）。
        // 末段标题 "(group)" → label "\(group\)"。
        assert!(idx.contains("\\(group\\)"), "label 括号应反斜杠转义：\n{idx}");
        assert!(idx.contains("\\[id\\] thing"), "label 方括号应反斜杠转义、空格保留：\n{idx}");

        // 「## 相关」段的子链接 target 同样转义（src → 子 (group)）。
        let src = bundle.concepts.iter().find(|c| c.rel_path == "src.md").unwrap();
        assert!(src.body.contains("## 相关"));
        assert!(src.body.contains("%28group%29"), "相关段子链接 target 应转义：\n{}", src.body);

        // 直观断言：产出里不应出现裸的 `(group)` 作为 target（会破坏 markdown）。
        // target 总以 `(` 起、`)` 止；裸括号会导致链接解析错乱，转义后不再出现 `(/src/(group).md)`。
        assert!(!idx.contains("(/src/(group).md)"), "裸括号 target 不应出现：\n{idx}");
    }

    // ── fix ④：bundle 内指向根外的 symlink .md 不出现在 read 结果（索引闸与 read 闸同口径）──
    #[test]
    fn read_skips_out_of_bundle_symlink_md() {
        // 仅在能建 symlink 的平台测；建不了则 skip-guard（不算失败）。
        let bundle_dir = tmp_dir();
        let outside_dir = tmp_dir();

        // 根外的「秘密」.md（含敏感 frontmatter），不应经 read 泄露。
        let secret = outside_dir.join("secret.md");
        fs::write(&secret, "---\ntype: secret\ntitle: SECRET\n---\n\nsecret frontmatter body\n").unwrap();

        // bundle 内一个正常 concept。
        fs::write(
            bundle_dir.join("ok.md"),
            "---\ntype: module\ntitle: OK\n---\n\nbody\n",
        )
        .unwrap();

        // 在 bundle 内建一个指向根外 secret.md 的 symlink。
        let link = bundle_dir.join("leak.md");
        let made = make_symlink_file(&secret, &link);
        if !made {
            // 无权限/平台不支持 → skip（不让测试在 CI 上假失败）。
            let _ = fs::remove_dir_all(&bundle_dir);
            let _ = fs::remove_dir_all(&outside_dir);
            return;
        }

        let read = read_okf_bundle(&bundle_dir);
        // 越界 symlink 不应被收为 concept；其 frontmatter（type=secret/title=SECRET）不应泄露。
        assert!(
            read.concepts.iter().all(|c| c.type_ != "secret" && c.title.as_deref() != Some("SECRET")),
            "越界 symlink 的根外 frontmatter 不应出现在 read 结果：{:?}",
            read.concepts
        );
        // 正常 concept 仍应在。
        assert!(read.concepts.iter().any(|c| c.rel_path == "ok.md"), "bundle 内正常 concept 应保留");

        let _ = fs::remove_dir_all(&bundle_dir);
        let _ = fs::remove_dir_all(&outside_dir);
    }

    /// 跨平台建文件 symlink；不支持/无权限 → false（调用方据此 skip）。
    fn make_symlink_file(target: &std::path::Path, link: &std::path::Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(target, link).is_ok()
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, link);
            false
        }
    }
}
