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

/// 一个区段的**确定性结构事实**，喂给可选的 LLM 摘要器（[`SectionSummarizer`]）。
///
/// 求真与安全：这里只包含 `analyze_repo` 已暴露的公开结构信息（目录、角色、关键文件名、
/// 顶层符号名+签名行）——**不含任何文件正文**。摘要器据此生成散文摘要，绝不会因 enrich
/// 而泄露超出确定性 wiki 已公开的内容。
#[derive(Debug, Clone)]
pub struct SummaryFacts {
    /// 目录（工作区相对路径）。
    pub directory: String,
    /// 结构性推断的角色。
    pub role: String,
    /// 该目录源文件数。
    pub file_count: usize,
    /// 关键文件（工作区相对路径）。
    pub key_files: Vec<String>,
    /// 顶层符号的 (名, 签名行) 列表。
    pub symbols: Vec<(String, String)>,
}

impl SummaryFacts {
    fn from_section(s: &WikiSection) -> Self {
        SummaryFacts {
            directory: s.directory.clone(),
            role: s.role.clone(),
            file_count: s.file_count,
            key_files: s.key_files.clone(),
            symbols: s
                .symbols
                .iter()
                .map(|sym| (sym.name.clone(), sym.signature.clone()))
                .collect(),
        }
    }
}

/// 可选的「把结构事实变成一句话散文摘要」回调。**从桌面层注入**，crates/wiki 对其零硬依赖：
/// wiki 核心保持纯离线/确定性，是否真去调模型完全由实现决定。
///
/// 契约：
///   - 返回 `Some(prose)` 即把摘要附加到该区段（额外的 `summary` 字段 / markdown `## Summary` 段）。
///   - 返回 `None` 表示「本段不加摘要」（含 LLM 失败 / 超时 / 被限流的兜底）——
///     此时区段退回纯确定性形态，**绝不**因摘要失败而破坏 build。
///   - 实现方负责一切边界（超时、prompt 体积上限、绝不外泄密钥、只发送 `facts` 里的公开结构）。
pub trait SectionSummarizer {
    /// 为一个区段生成简短散文摘要；失败/不适用时返回 `None`（调用方据此优雅退回确定性区段）。
    fn summarize(&self, facts: &SummaryFacts) -> Option<String>;
}

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
///
/// **完全离线/确定性**：这是 0.0.57 的默认入口，逐字节行为不变（内部等价于
/// `build_wiki_enriched(.., None)`——不注入任何摘要器，绝无网络/模型调用）。
pub fn build_wiki(workspace_root: &str, force: bool) -> WikiBuildResult {
    build_wiki_enriched(workspace_root, force, None)
}

/// 与 [`build_wiki`] 相同，但可注入一个可选的 [`SectionSummarizer`] 给每个区段附加 LLM 散文摘要。
///
/// 行为约定（P3，严格 opt-in）：
///   - `summarizer == None`：**与 0.0.57 的 [`build_wiki`] 逐字节一致**——纯离线、确定性、
///     无网络、无摘要字段。
///   - `summarizer == Some(_)`：在确定性区段构建完成**之后**，对每个区段做一次有界的摘要调用，
///     把结果存进区段的 `summary`（额外字段 + markdown `## Summary` 段）。
///     **逐段缓存**：按区段的结构指纹（[`store::section_fingerprint`]，不含摘要）键控——
///     结构未变且上次已有摘要即直接复用，**跳过重复付费调用**；摘要器返回 `None`（失败/超时）
///     则该段退回纯确定性形态，绝不破坏 build。
///   - 摘要**不参与内容指纹**：故 enrich 不会改变全局 `force=false` 的跳过判定口径；
///     但为了让首次 enrich 能真正写入摘要、且复用缓存，见下方的 enrich 专属写盘判定。
pub fn build_wiki_enriched(
    workspace_root: &str,
    force: bool,
    summarizer: Option<&dyn SectionSummarizer>,
) -> WikiBuildResult {
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

    // 内容指纹：所有区段的稳定结构序列化（不含摘要）。指纹一致即结构未变。
    let fingerprint = store::fingerprint(&sections);
    let structure_unchanged = store::fingerprint_matches(&wiki_dir_abs, &fingerprint);

    // 摘要复用缓存：把上次持久化区段按「结构指纹 → 已有摘要」建索引，使未变区段免于重复付费调用。
    // 仅在要 enrich 时才回读旧 wiki（默认路径零额外 I/O，保持 0.0.57 口径）。
    let mut enriched = 0usize;
    let mut reused = 0usize;
    if let Some(summarizer) = summarizer {
        let cache = store::load_summary_cache(&wiki_dir_abs);
        for s in &mut sections {
            let key = store::section_fingerprint(s);
            if let Some(cached) = cache.get(&key) {
                // 结构未变且上次已有摘要 → 直接复用，跳过付费 LLM 调用。
                s.summary = Some(cached.clone());
                reused += 1;
                continue;
            }
            // 缓存未命中 → 调一次注入的摘要器。失败/不适用返回 None：该段退回确定性形态。
            let facts = SummaryFacts::from_section(s);
            if let Some(prose) = summarizer.summarize(&facts) {
                let prose = prose.trim();
                if !prose.is_empty() {
                    s.summary = Some(prose.to_string());
                    enriched += 1;
                }
            }
        }
    }

    // 写盘判定：
    //   - 未注入摘要器（默认）：沿用 0.0.57 口径——结构未变且非 force 即整库跳过。
    //   - 注入了摘要器：结构未变时，仅当本次确实新增了摘要（enriched>0）才需重写盘以落库摘要；
    //     若全部命中缓存（reused 覆盖、enriched==0），磁盘已是最新，跳过重写（不重复付费、不空转写盘）。
    let any_new_summary = enriched > 0;
    let should_skip = !force
        && structure_unchanged
        && (summarizer.is_none() || !any_new_summary);
    if should_skip {
        let note = if summarizer.is_some() {
            format!(
                "wiki 结构指纹未变，摘要已全部命中缓存（复用 {reused} 段），跳过重写（增量幂等、零额外 LLM 调用）。可用 action=query 检索。"
            )
        } else {
            "wiki 内容指纹未变，已跳过重写（增量幂等）。可用 action=query 检索。".to_string()
        };
        return WikiBuildResult {
            sections: sections.len(),
            files: analysis.files.len(),
            definitions: analysis.total_definitions,
            skipped_unchanged: true,
            wiki_dir: WIKI_DIR.to_string(),
            note,
        };
    }

    match store::write_all(&wiki_dir_abs, &sections, &fingerprint) {
        Ok(()) => {
            let enrich_note = if summarizer.is_some() {
                format!("；LLM 摘要：新增 {enriched} 段、复用缓存 {reused} 段")
            } else {
                String::new()
            };
            WikiBuildResult {
                sections: sections.len(),
                files: analysis.files.len(),
                definitions: analysis.total_definitions,
                skipped_unchanged: false,
                wiki_dir: WIKI_DIR.to_string(),
                note: format!(
                    "已为 {} 个目录生成 wiki（覆盖 {} 个源文件、{} 个定义）{}。\
                     派生数据写入 {}/，可随时重建；用 action=query 按问题检索区段。",
                    sections.len(),
                    analysis.files.len(),
                    analysis.total_definitions,
                    enrich_note,
                    WIKI_DIR
                ),
            }
        }
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

    // ===== P3：opt-in LLM enrich =====

    use std::sync::atomic::AtomicUsize as Counter;

    /// 计数型假摘要器：记录被调次数，返回确定性的占位散文。用于断言「缓存命中即跳过付费调用」。
    struct CountingSummarizer {
        calls: Counter,
    }
    impl CountingSummarizer {
        fn new() -> Self {
            Self { calls: Counter::new(0) }
        }
        fn count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }
    impl SectionSummarizer for CountingSummarizer {
        fn summarize(&self, facts: &SummaryFacts) -> Option<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            // 散文里嵌目录与角色，便于断言摘要确实进了产物。
            Some(format!(
                "This directory ({}) acts as {} with {} key file(s).",
                facts.directory,
                facts.role,
                facts.key_files.len()
            ))
        }
    }

    /// 永远失败的假摘要器：模拟 LLM 失败/超时，断言区段优雅退回确定性形态、build 不破。
    struct FailingSummarizer;
    impl SectionSummarizer for FailingSummarizer {
        fn summarize(&self, _facts: &SummaryFacts) -> Option<String> {
            None
        }
    }

    /// 读取某目录区段持久化的 markdown 全文。
    fn read_doc(dir: &Path, directory: &str) -> String {
        let stem = crate::sanitize::dir_to_doc_stem(directory);
        std::fs::read_to_string(dir.join(".mdga").join("wiki").join(format!("{stem}.md")))
            .unwrap_or_default()
    }

    /// enrich=false（默认）：摘要器**绝不**被调用，产物与确定性 build 逐字节一致（无 summary）。
    #[test]
    fn enrich_false_is_byte_identical_to_deterministic() {
        let dir_plain = make_fixture();
        build_wiki(dir_plain.to_str().unwrap(), false);
        let plain_index = std::fs::read(
            dir_plain.join(".mdga").join("wiki").join("index.jsonl"),
        )
        .unwrap();

        // 用「显式传 None 摘要器」的 enrich 入口构建另一棵相同固件树。
        let dir_none = make_fixture();
        build_wiki_enriched(dir_none.to_str().unwrap(), false, None);
        let none_index = std::fs::read(
            dir_none.join(".mdga").join("wiki").join("index.jsonl"),
        )
        .unwrap();

        assert_eq!(
            plain_index, none_index,
            "build_wiki 与 build_wiki_enriched(None) 的 index.jsonl 应逐字节一致"
        );
        // index 里不应出现 summary 字段（skip_serializing_if 生效）。
        let s = String::from_utf8(none_index).unwrap();
        assert!(!s.contains("summary"), "默认路径不应序列化 summary 字段");
        // 区段回读后 summary 必为 None。
        let secs = store::load_sections(
            &dir_none.join(".mdga").join("wiki"),
        )
        .unwrap();
        assert!(secs.iter().all(|s| s.summary.is_none()));

        let _ = std::fs::remove_dir_all(&dir_plain);
        let _ = std::fs::remove_dir_all(&dir_none);
    }

    /// enrich=true：摘要被附加进区段 + markdown；第二次（结构未变）build 命中缓存、**不再**调用摘要器。
    #[test]
    fn enrich_adds_summary_and_caches() {
        let dir = make_fixture();
        let summarizer = CountingSummarizer::new();

        let first = build_wiki_enriched(dir.to_str().unwrap(), false, Some(&summarizer));
        assert!(!first.skipped_unchanged, "首次 enrich 应写盘");
        let first_calls = summarizer.count();
        assert!(first_calls >= 2, "应对每个区段各调一次摘要器，实得 {first_calls}");

        // 区段回读：每段都带非空 summary。
        let secs = store::load_sections(&dir.join(".mdga").join("wiki")).unwrap();
        assert!(
            secs.iter().all(|s| s.summary.as_deref().map(|t| !t.is_empty()).unwrap_or(false)),
            "enrich 后每段都应带非空 summary"
        );
        // markdown 应含 `## Summary` 段与摘要文本。
        let core_doc = read_doc(&dir, "src/core");
        assert!(core_doc.contains("## Summary"), "markdown 应含 Summary 段");
        assert!(core_doc.contains("acts as"), "markdown 应含摘要散文");

        // 第二次 enrich：结构未变 → 全部命中缓存 → 摘要器调用次数不增、且整库跳过重写。
        let second = build_wiki_enriched(dir.to_str().unwrap(), false, Some(&summarizer));
        assert_eq!(
            summarizer.count(),
            first_calls,
            "未变结构的二次 enrich 不应再调用摘要器（缓存命中）"
        );
        assert!(
            second.skipped_unchanged,
            "全部命中缓存的二次 enrich 应跳过重写（零额外付费调用）"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// enrich 缓存在「先确定性 build、再 enrich」的升级路径下也成立：摘要进盘，再 enrich 命中缓存。
    #[test]
    fn enrich_after_plain_build_then_caches() {
        let dir = make_fixture();
        // 先做一次纯确定性 build（无摘要）。
        let plain = build_wiki(dir.to_str().unwrap(), false);
        assert!(!plain.skipped_unchanged);

        let summarizer = CountingSummarizer::new();
        // 首次 enrich：结构未变但磁盘还没摘要 → 应调用摘要器并写盘（不能因「结构未变」而跳过）。
        let enr = build_wiki_enriched(dir.to_str().unwrap(), false, Some(&summarizer));
        assert!(!enr.skipped_unchanged, "首次 enrich 即便结构未变也应写入摘要");
        let calls = summarizer.count();
        assert!(calls >= 2, "首次 enrich 应真正调用摘要器，实得 {calls}");

        // 二次 enrich：命中缓存、跳过、不再调用。
        let again = build_wiki_enriched(dir.to_str().unwrap(), false, Some(&summarizer));
        assert_eq!(summarizer.count(), calls, "二次 enrich 应全命中缓存");
        assert!(again.skipped_unchanged, "二次 enrich 应跳过重写");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// LLM 失败（摘要器恒返回 None）：区段优雅退回确定性形态，build 成功、无 summary。
    #[test]
    fn enrich_llm_failure_falls_back_to_deterministic() {
        let dir = make_fixture();
        let result = build_wiki_enriched(dir.to_str().unwrap(), false, Some(&FailingSummarizer));
        assert!(!result.skipped_unchanged, "build 应成功完成");
        assert!(result.sections >= 2, "区段照常生成");

        let secs = store::load_sections(&dir.join(".mdga").join("wiki")).unwrap();
        assert!(
            secs.iter().all(|s| s.summary.is_none()),
            "摘要器全失败时不应有任何 summary，退回确定性形态"
        );
        // 失败的 enrich 产物应与纯确定性产物的 index 一致（无 summary 键）。
        let none_index = std::fs::read(dir.join(".mdga").join("wiki").join("index.jsonl")).unwrap();
        assert!(
            !String::from_utf8(none_index).unwrap().contains("summary"),
            "失败回退后 index 不应含 summary 字段"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 全局指纹**不受摘要影响**：enrich 与否，同一结构的指纹一致（确保 enrich 不污染增量口径）。
    #[test]
    fn fingerprint_ignores_summary() {
        let dir = make_fixture();
        let analysis = analyze_repo(dir.to_str().unwrap(), &CodemapRequest::default());
        let plain = sections::group_into_sections(&analysis.files);
        let fp_plain = store::fingerprint(&plain);

        let mut enriched = plain.clone();
        for s in &mut enriched {
            s.summary = Some("some prose summary".to_string());
        }
        let fp_enriched = store::fingerprint(&enriched);
        assert_eq!(fp_plain, fp_enriched, "摘要不应改变全局内容指纹");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
