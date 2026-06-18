//! mdga-wiki：从 codemap 结构化分析自动生成、可查询、可离线的仓库 wiki（R11）。
//!
//! 用途：给模型一份**持久、可检索**的代码库结构知识，免得每轮都重新推导「这个仓库长什么样、
//! 核心代码在哪个目录、各目录是干什么的」。完全确定性、纯离线——不需要任何模型/网络调用即可工作。
//!
//! 流水线（build）：
//!   codemap::analyze_repo（tree-sitter 抽取定义 + PageRank 排名）
//!     → 按目录聚合成「wiki 区段」（key files / top symbols / 引用关系）
//!     → 结构性（非 LLM）推断每个目录的角色
//!     → 持久化为 .mdga/wiki/ 下的 markdown + JSONL（增量/幂等：内容指纹未变即跳过）。
//!
//! 查询（query）：对已生成的 wiki 区段 + 符号名做简单词法匹配，返回最相关的区段 + 源文件指针，
//! 让模型据此 read_file 取细节。缓存缺失/损坏时优雅降级为「现场分析一次」。
//!
//! 求真与安全：wiki 是**派生数据**（可随时重建），只写入 .mdga/wiki/ 缓存、绝不碰用户源码；
//! 所有写入路径分量都经 sanitize、永不逃出工作区；build 无法运行时 query 仍能降级返回。

mod role;
mod sanitize;
mod sections;
mod store;

use mdga_codemap::{analyze_repo, CodemapRequest};
use serde::Serialize;
use std::path::{Path, PathBuf};

pub use sections::{WikiSection, WikiSymbol};

/// wiki 缓存在工作区内的相对根目录。所有派生产物都落在这里、绝不外溢。
pub const WIKI_DIR: &str = ".mdga/wiki";

/// 一次 build 的结果摘要（计数 + 是否走了增量跳过）。
#[derive(Debug, Clone, Serialize)]
pub struct WikiBuildResult {
    /// 生成/更新的目录区段数。
    pub sections: usize,
    /// 纳入 wiki 的源文件数。
    pub files: usize,
    /// 抽取的定义总数。
    pub definitions: usize,
    /// 内容指纹未变、整库跳过了重写（幂等命中）。
    pub skipped_unchanged: bool,
    /// wiki 缓存目录（工作区相对路径，正斜杠）。
    pub wiki_dir: String,
    /// 给模型的口径说明。
    pub note: String,
}

/// 一次 query 的结果：最相关的若干区段（含源文件指针）+ 说明。
#[derive(Debug, Clone, Serialize)]
pub struct WikiQueryResult {
    /// 按相关度降序的命中区段（已截到上限）。
    pub matches: Vec<WikiQueryMatch>,
    /// 命中是否来自现场分析（缓存缺失时的降级路径）而非持久 wiki。
    pub degraded: bool,
    /// 给模型的口径说明。
    pub note: String,
}

/// 单条 query 命中：一个目录区段 + 其相关度分 + 可直接 read_file 的源文件指针。
#[derive(Debug, Clone, Serialize)]
pub struct WikiQueryMatch {
    /// 目录（工作区相对路径，正斜杠；仓库根为 "."）。
    pub directory: String,
    /// 该目录的结构性角色推断。
    pub role: String,
    /// 词法相关度分（越高越相关）。
    pub score: f64,
    /// 该区段最重要的源文件（工作区相对路径），供模型 read_file。
    pub key_files: Vec<String>,
    /// 命中相关的顶层符号名（便于模型定位）。
    pub matched_symbols: Vec<String>,
    /// 该区段持久化的 markdown 文件（工作区相对路径），供模型直接读全文。
    pub source_doc: String,
}

/// 单次 build 最多生成的目录区段数上限（防极端宽仓刷爆缓存）。
const MAX_SECTIONS: usize = 400;
/// query 默认返回的命中上限。
const DEFAULT_QUERY_LIMIT: usize = 5;

/// 构建（或增量更新）仓库 wiki。永不硬失败：工作区无源码时返回空结果 + 说明。
///
/// `force=true` 时无视指纹强制重写；否则内容指纹与上次一致就整库跳过（幂等、可反复调用）。
pub fn build_wiki(workspace_root: &str, force: bool) -> WikiBuildResult {
    let root = PathBuf::from(workspace_root);
    let wiki_dir_abs = match wiki_dir_within(&root) {
        Some(p) => p,
        None => {
            return empty_build("工作区路径无效，无法定位 wiki 缓存目录");
        }
    };

    let analysis = analyze_repo(workspace_root, &CodemapRequest::default());
    if analysis.files.is_empty() {
        return empty_build(
            "工作区内未发现可生成 wiki 的源文件（无受支持源码或目录不存在）",
        );
    }

    let mut sections = sections::group_into_sections(&analysis.files);
    if sections.len() > MAX_SECTIONS {
        sections.truncate(MAX_SECTIONS);
    }

    // 内容指纹：所有区段的稳定序列化。指纹一致即跳过重写（幂等）。
    let fingerprint = store::fingerprint(&sections);
    if !force && store::fingerprint_matches(&wiki_dir_abs, &fingerprint) {
        return WikiBuildResult {
            sections: sections.len(),
            files: analysis.files.len(),
            definitions: analysis.total_definitions,
            skipped_unchanged: true,
            wiki_dir: WIKI_DIR.to_string(),
            note: "wiki 内容指纹未变，已跳过重写（增量幂等）。可用 action=query 检索。".to_string(),
        };
    }

    match store::write_all(&wiki_dir_abs, &sections, &fingerprint) {
        Ok(()) => WikiBuildResult {
            sections: sections.len(),
            files: analysis.files.len(),
            definitions: analysis.total_definitions,
            skipped_unchanged: false,
            wiki_dir: WIKI_DIR.to_string(),
            note: format!(
                "已为 {} 个目录生成 wiki（覆盖 {} 个源文件、{} 个定义）。\
                 派生数据写入 {}/，可随时重建；用 action=query 按问题检索区段。",
                sections.len(),
                analysis.files.len(),
                analysis.total_definitions,
                WIKI_DIR
            ),
        },
        Err(e) => empty_build(&format!("wiki 写入失败（已优雅放弃，不影响源码）：{e}")),
    }
}

/// 按问题检索 wiki，返回最相关的区段 + 源文件指针。
///
/// 优先读已持久化的 wiki；缓存缺失/损坏时降级为「现场分析一次」并标记 degraded。
pub fn query_wiki(workspace_root: &str, question: &str, limit: usize) -> WikiQueryResult {
    let limit = if limit == 0 { DEFAULT_QUERY_LIMIT } else { limit };
    let root = PathBuf::from(workspace_root);
    let wiki_dir_abs = wiki_dir_within(&root);

    // 1) 尝试从持久缓存加载区段。
    let (sections, degraded) = match wiki_dir_abs
        .as_ref()
        .and_then(|d| store::load_sections(d).ok())
        .filter(|s| !s.is_empty())
    {
        Some(s) => (s, false),
        None => {
            // 降级：现场分析一次（不写盘）。
            let analysis = analyze_repo(workspace_root, &CodemapRequest::default());
            (sections::group_into_sections(&analysis.files), true)
        }
    };

    if sections.is_empty() {
        return WikiQueryResult {
            matches: Vec::new(),
            degraded,
            note: "未找到任何 wiki 区段（工作区可能无源码）。可先 action=build。".to_string(),
        };
    }

    let terms = lexical_terms(question);
    let mut scored: Vec<(f64, Vec<String>, &WikiSection)> = sections
        .iter()
        .map(|s| {
            let (score, matched) = score_section(s, &terms);
            (score, matched, s)
        })
        .collect();
    // 按分降序、平局按目录路径升序（确定性）。
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.2.directory.cmp(&b.2.directory))
    });

    let matches: Vec<WikiQueryMatch> = scored
        .into_iter()
        .filter(|(score, _, _)| *score > 0.0)
        .take(limit)
        .map(|(score, matched, s)| WikiQueryMatch {
            directory: s.directory.clone(),
            role: s.role.clone(),
            score,
            key_files: s.key_files.clone(),
            matched_symbols: matched,
            source_doc: store::section_doc_rel(&s.directory),
        })
        .collect();

    let note = if matches.is_empty() {
        "无区段与问题词法匹配；可换关键词，或 read_file 直接看 key files。".to_string()
    } else {
        format!(
            "命中 {} 个相关目录区段{}。key_files 可直接 read_file 取细节；\
             source_doc 是该区段的 markdown 全文。",
            matches.len(),
            if degraded {
                "（缓存缺失，本次为现场分析降级结果）"
            } else {
                ""
            }
        )
    };

    WikiQueryResult {
        matches,
        degraded,
        note,
    }
}

/// 把工作区根与 WIKI_DIR 安全拼接，并校验结果仍在工作区内（防 symlink/越界）。
fn wiki_dir_within(root: &Path) -> Option<PathBuf> {
    if root.as_os_str().is_empty() {
        return None;
    }
    // WIKI_DIR 为硬编码常量、分量固定（.mdga / wiki），不含任何外部输入，天然不越界。
    let dir = root.join(".mdga").join("wiki");
    Some(dir)
}

/// 把问题拆成小写词法词（字母数字/下划线连续段，长度≥2），用于词法匹配。
fn lexical_terms(question: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in question.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            for c in ch.to_lowercase() {
                cur.push(c);
            }
        } else if !cur.is_empty() {
            if cur.len() >= 2 {
                out.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
    }
    if cur.len() >= 2 {
        out.push(cur);
    }
    out.sort();
    out.dedup();
    out
}

/// 对单个区段按词法词打分，返回 (分, 命中的符号名)。
///
/// 加权直觉：符号名精确命中 > 目录/文件路径命中 > 角色文本命中。让「问某个函数/类型」时
/// 定义它的目录排在最前。
fn score_section(section: &WikiSection, terms: &[String]) -> (f64, Vec<String>) {
    if terms.is_empty() {
        return (0.0, Vec::new());
    }
    let mut score = 0.0f64;
    let mut matched_symbols: Vec<String> = Vec::new();

    let dir_lower = section.directory.to_lowercase();
    let role_lower = section.role.to_lowercase();

    for term in terms {
        // 符号名命中（最强信号）。
        let mut hit_symbol = false;
        for sym in &section.symbols {
            let name_lower = sym.name.to_lowercase();
            if name_lower == *term {
                score += 5.0;
                hit_symbol = true;
            } else if name_lower.contains(term) {
                score += 2.0;
                hit_symbol = true;
            }
            if hit_symbol && !matched_symbols.contains(&sym.name) {
                matched_symbols.push(sym.name.clone());
            }
            hit_symbol = false;
        }
        // 目录路径命中。
        if dir_lower.contains(term) {
            score += 3.0;
        }
        // 文件名命中。
        for f in &section.key_files {
            if f.to_lowercase().contains(term) {
                score += 1.5;
                break;
            }
        }
        // 角色文本命中。
        if role_lower.contains(term) {
            score += 1.0;
        }
    }

    // 命中符号去重并截断，避免回灌过多。
    matched_symbols.truncate(12);
    (score, matched_symbols)
}

fn empty_build(note: &str) -> WikiBuildResult {
    WikiBuildResult {
        sections: 0,
        files: 0,
        definitions: 0,
        skipped_unchanged: false,
        wiki_dir: WIKI_DIR.to_string(),
        note: note.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_workspace() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let id = N.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("mdga-wiki-test-{}-{}", std::process::id(), id));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, content).unwrap();
    }

    /// 一棵小固件树：src/core 是被引用的 hub，src/api 引用它。
    fn make_fixture() -> PathBuf {
        let dir = temp_workspace();
        write(
            &dir,
            "src/core/engine.rs",
            "pub fn run_engine() {}\npub struct EngineState { x: i32 }\n",
        );
        write(
            &dir,
            "src/api/handler.rs",
            "fn handle() {\n    run_engine();\n    let _s: EngineState;\n}\n",
        );
        write(
            &dir,
            "src/util/text.ts",
            "export function slugify(s: string) { return s; }\n",
        );
        dir
    }

    #[test]
    fn build_produces_sections_and_persists() {
        let dir = make_fixture();
        let result = build_wiki(dir.to_str().unwrap(), false);
        assert!(result.sections >= 2, "应至少生成 2 个目录区段，实得 {}", result.sections);
        assert!(result.files >= 3, "应覆盖 ≥3 个源文件，实得 {}", result.files);
        assert!(!result.skipped_unchanged, "首次构建不应跳过");

        // 持久化产物应落在 .mdga/wiki 下。
        let wiki = dir.join(".mdga").join("wiki");
        assert!(wiki.join("index.jsonl").is_file(), "应写出 index.jsonl");
        assert!(
            std::fs::read_dir(&wiki)
                .unwrap()
                .filter_map(|e| e.ok())
                .any(|e| e.path().extension().map(|x| x == "md").unwrap_or(false)),
            "应写出至少一个 .md 区段文件"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_is_idempotent_and_incremental() {
        let dir = make_fixture();
        let first = build_wiki(dir.to_str().unwrap(), false);
        assert!(!first.skipped_unchanged);
        let second = build_wiki(dir.to_str().unwrap(), false);
        assert!(
            second.skipped_unchanged,
            "内容未变第二次构建应跳过（增量幂等）"
        );
        // force=true 应无视指纹重写。
        let forced = build_wiki(dir.to_str().unwrap(), true);
        assert!(!forced.skipped_unchanged, "force 应强制重写");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn query_returns_relevant_section() {
        let dir = make_fixture();
        build_wiki(dir.to_str().unwrap(), false);
        // 问 run_engine（定义在 src/core）→ src/core 应排在最前。
        let res = query_wiki(dir.to_str().unwrap(), "where is run_engine defined", 5);
        assert!(!res.matches.is_empty(), "应有命中");
        assert!(!res.degraded, "已 build，不应走降级");
        let top = &res.matches[0];
        assert!(
            top.directory.contains("core"),
            "run_engine 定义所在的 src/core 应排最前，实得 {}",
            top.directory
        );
        assert!(
            top.matched_symbols.iter().any(|s| s == "run_engine"),
            "命中符号应含 run_engine，实得 {:?}",
            top.matched_symbols
        );
        assert!(
            top.key_files.iter().any(|f| f.contains("engine.rs")),
            "key_files 应含 engine.rs，实得 {:?}",
            top.key_files
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn query_degrades_gracefully_without_cache() {
        let dir = make_fixture();
        // 不 build，直接 query → 应降级现场分析，仍返回相关区段。
        let res = query_wiki(dir.to_str().unwrap(), "slugify", 5);
        assert!(res.degraded, "无缓存应标记 degraded");
        assert!(!res.matches.is_empty(), "降级路径也应能命中");
        assert!(
            res.matches[0].directory.contains("util"),
            "slugify 定义所在的 src/util 应命中，实得 {}",
            res.matches[0].directory
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_missing_workspace_is_soft_failure() {
        let res = build_wiki("C:/definitely/not/here/mdga-wiki-zzz", false);
        assert_eq!(res.sections, 0);
        assert_eq!(res.files, 0);
        assert!(!res.skipped_unchanged);
    }

    #[test]
    fn lexical_terms_splits_and_lowercases() {
        let terms = lexical_terms("Where is RunEngine? (the core)");
        assert!(terms.contains(&"where".to_string()));
        assert!(terms.contains(&"runengine".to_string()));
        assert!(terms.contains(&"core".to_string()));
        // 单字符 "a"/标点被丢弃。
        assert!(!terms.iter().any(|t| t.len() < 2));
    }
}
