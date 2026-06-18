//! `code_search`：本地语义代码检索（R2 L 阶段，离线、无网络、无 embedding 依赖）。
//!
//! 给定自然语言/关键词 query，返回最相关的若干代码块（文件 + 行区间 + 片段 + 排名理由）。
//! 与 repo_map 同源:复用 codemap 的「gitignore 感知发现 + tree-sitter 标签 + PageRank 图」,
//! 但产出的不是符号地图,而是**可直接阅读的代码块**——填补「按名字/文本找(glob/search_text)」
//! 与「全仓符号排名(repo_map)」之间的空白:用一句话描述意图即可定位最相关代码。
//!
//! ## 切块 (chunking)
//! - 有 tree-sitter 定义的文件:以每个**定义**为锚,块覆盖从定义行到下一个定义行之前(夹到
//!   MAX_CHUNK_LINES),即「符号/块级」粒度。
//! - 无定义的文件(含启发式回退后仍无符号、或纯文本):退化为**固定大小行窗**滑动切块,
//!   保证每个文本文件都可被检索,不整文件丢弃。
//!
//! ## 排名 (本地混合,无网络)
//! 对每个块算分 = BM25 词法分(对 query 词项,块 token 由标识符按 camelCase/snake_case 拆分)
//!   + PageRank 文件重要度提升(复用 graph::rank 的 file_rank)
//!   + 精确标识符命中加成(块内某 token 与某 query 词项完全相等)。
//! 全程纯整内存计算,确定性(同输入同输出),无任何外部基础设施。
//!
//! ## 可扩展钩子
//! 暴露 `Embedder` trait:未来可插入「provider embedding 向量召回」后端,但**默认路径**永远是
//! 本地词法+图排名,不引入任何重型 ML 依赖、不强制网络/API key。

use crate::tags::{Def, FileTags};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// 单块覆盖的最大源行数(超长定义/无定义文件的行窗都夹到此值)。
const MAX_CHUNK_LINES: usize = 60;
/// 无定义文件做固定行窗切块时的窗口大小。
const WINDOW_LINES: usize = 40;
/// 片段(snippet)返回的最大字符数,超出截断并加省略号,避免单块撑爆结果。
const MAX_SNIPPET_CHARS: usize = 1200;
/// 单文件最多产出的块数(防止超大文件刷屏 / 内存膨胀)。
const MAX_CHUNKS_PER_FILE: usize = 60;
/// 全仓最多保留参与排名的块数(内存上界;超过即按发现顺序截断,结果 note 标注)。
const MAX_TOTAL_CHUNKS: usize = 40_000;
/// 返回结果块数默认值与上限。
const DEFAULT_TOP_K: usize = 8;
const MAX_TOP_K: usize = 50;
/// BM25 参数(标准取值)。
const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;
/// 精确标识符命中(块 token 与 query 词项完全相等)的加成权重。
const EXACT_IDENT_BONUS: f64 = 2.5;
/// PageRank 文件重要度提升的缩放(file_rank 通常是很小的概率,放大后做温和加成)。
const PAGERANK_BOOST_SCALE: f64 = 8.0;

/// `code_search` 请求。
#[derive(Debug, Clone, Default)]
pub struct CodeSearchRequest {
    /// 自由文本查询(自然语言或关键词皆可)。空查询返回空结果 + 说明。
    pub query: String,
    /// 返回的最相关块数(0 表示默认 8,夹到 [1, 50])。
    pub top_k: usize,
}

/// 单个检索结果块。
#[derive(Debug, Clone, Serialize)]
pub struct CodeSearchChunk {
    /// 工作区相对路径(正斜杠)。
    pub path: String,
    /// 块起始行(1 基,闭区间)。
    pub start_line: usize,
    /// 块结束行(1 基,闭区间)。
    pub end_line: usize,
    /// 块对应的符号名(以定义为锚时);无定义的行窗块为 None。
    pub symbol: Option<String>,
    /// 片段文本(已按 MAX_SNIPPET_CHARS 截断)。
    pub snippet: String,
    /// 综合相关度分(越大越相关)。
    pub score: f64,
    /// 排名理由(人读),如「BM25 词法命中 query=auth login;精确标识符命中 login;文件重要度高」。
    pub why: String,
}

/// `code_search` 结果。
#[derive(Debug, Clone, Serialize)]
pub struct CodeSearchResult {
    /// 按相关度降序的结果块(最多 top_k 个)。
    pub chunks: Vec<CodeSearchChunk>,
    /// 扫描到的源文件总数。
    pub total_files: usize,
    /// 切出的代码块总数(参与排名的)。
    pub total_chunks: usize,
    /// 是否因文件/块上限有内容被省略。
    pub truncated: bool,
    /// 给模型的口径说明。
    pub note: String,
}

/// 未来可插拔的「向量召回」后端钩子。默认路径**不使用**它(纯本地词法+图排名)。
///
/// 约定:`embed` 把一段文本映射到稠密向量,`code_search` 的可选向量重排会用余弦相似度。
/// 留此 trait 是为了将来接 provider embedding 而**不破坏现有签名**;实现者需自带网络/模型,
/// 本 crate 默认不提供任何实现,也绝不引入重型 ML 依赖。
pub trait Embedder: Send + Sync {
    /// 把文本编码为定长向量。返回 None 表示该后端对此文本不可用(调用方降级到词法分)。
    fn embed(&self, text: &str) -> Option<Vec<f32>>;
    /// 向量维度(用于一致性校验)。
    fn dim(&self) -> usize;
}

/// 一个切出的代码块(内部表示,排名前)。
struct Chunk {
    file_idx: usize,
    start_line: usize, // 0 基
    end_line: usize,   // 0 基,闭区间
    symbol: Option<String>,
    /// 块文本(已按 MAX_SNIPPET_CHARS 截断,直接用作 snippet)。
    text: String,
    /// 块内 token(标识符拆词后,小写),用于 BM25。含重复以体现词频。
    tokens: Vec<String>,
}

/// 本地语义代码检索。永不硬失败:工作区不存在 / 无源码 / 空 query 都返回空块 + 说明。
pub fn code_search(workspace_root: &str, request: &CodeSearchRequest) -> CodeSearchResult {
    let top_k = normalize_top_k(request.top_k);
    let query = request.query.trim();
    if query.is_empty() {
        return empty_result("query 为空:请提供描述意图的自然语言或关键词");
    }

    let root = PathBuf::from(workspace_root);
    if !root.is_dir() {
        return empty_result("工作区路径不存在或不是目录");
    }

    // 1) 与 repo_map 同源地发现源文件。
    let discovered = crate::discover_source_files(&root);
    if discovered.rel_paths.is_empty() {
        return empty_result("工作区内未发现可扫描的文本源文件");
    }
    let total_files = discovered.rel_paths.len();

    // 2) 抽取标签(复用 mtime 缓存)——既用于切块锚点,也喂给 PageRank。
    let file_tags: Vec<FileTags> = discovered
        .abs_paths
        .iter()
        .map(|p| {
            let arc = crate::tags::tags_for_file(p);
            FileTags {
                defs: arc.defs.clone(),
                refs: arc.refs.clone(),
            }
        })
        .collect();

    // 3) PageRank 文件重要度(query 命中的符号会抬高,作为温和的图先验)。
    let mentioned = query_idents(query);
    let ranks = crate::graph::rank(&file_tags, &[], &mentioned);

    // 4) 逐文件切块。
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut chunks_truncated = false;
    for (fi, abs) in discovered.abs_paths.iter().enumerate() {
        if chunks.len() >= MAX_TOTAL_CHUNKS {
            chunks_truncated = true;
            break;
        }
        let Ok(source) = std::fs::read_to_string(abs) else {
            continue; // 非 UTF-8 / 读失败:跳过(与 tags 口径一致)。
        };
        let lines: Vec<&str> = source.lines().collect();
        if lines.is_empty() {
            continue;
        }
        let before = chunks.len();
        chunk_file(fi, &lines, &file_tags[fi].defs, &mut chunks);
        if chunks.len() - before >= MAX_CHUNKS_PER_FILE {
            chunks_truncated = true;
        }
    }

    let total_chunks = chunks.len();
    if total_chunks == 0 {
        return empty_result("未能从源文件切出任何代码块");
    }

    // 5) 本地混合排名。
    let query_terms = query_terms(query);
    let scored = rank_chunks(&chunks, &query_terms, &ranks, &discovered.rel_paths);

    // 6) 取 top_k,组装结果。
    let mut out: Vec<CodeSearchChunk> = Vec::with_capacity(top_k.min(scored.len()));
    for (idx, score, why) in scored.into_iter().take(top_k) {
        let c = &chunks[idx];
        out.push(CodeSearchChunk {
            path: discovered.rel_paths[c.file_idx].clone(),
            start_line: c.start_line + 1,
            end_line: c.end_line + 1,
            symbol: c.symbol.clone(),
            snippet: c.text.clone(),
            score,
            why,
        });
    }

    let truncated = discovered.truncated || chunks_truncated;
    let note = format!(
        "本地语义检索(离线,无 embedding):tree-sitter 切块 + BM25 词法 + PageRank 文件重要度 + 精确标识符命中。\
         共扫描 {total_files} 个源文件、{total_chunks} 个代码块,返回最相关 {} 个。行号 1 基、闭区间。{}",
        out.len(),
        if truncated {
            "部分文件/块因上限被省略;可缩小工作区或提高聚焦度。"
        } else {
            ""
        },
    );

    CodeSearchResult {
        chunks: out,
        total_files,
        total_chunks,
        truncated,
        note,
    }
}

/// 把一个文件切成块:有定义则以定义为锚,无定义则固定行窗。结果追加到 `out`。
fn chunk_file(file_idx: usize, lines: &[&str], defs: &[Def], out: &mut Vec<Chunk>) {
    let n = lines.len();
    if defs.is_empty() {
        // 无符号锚点:固定行窗滑动切块,确保每个文本文件都可被检索。
        let mut start = 0usize;
        let mut made = 0usize;
        while start < n && made < MAX_CHUNKS_PER_FILE {
            let end = (start + WINDOW_LINES).min(n) - 1;
            push_chunk(file_idx, lines, start, end, None, out);
            made += 1;
            start += WINDOW_LINES;
        }
        return;
    }

    // 以定义行为锚:块覆盖 [def.line, 下一个 def.line) ,夹到 MAX_CHUNK_LINES。
    // 先按行号升序排定义(去重同行),保证块不交叠且确定性。
    let mut anchors: Vec<&Def> = defs.iter().collect();
    anchors.sort_by_key(|d| d.line);

    let mut made = 0usize;
    for (i, def) in anchors.iter().enumerate() {
        if made >= MAX_CHUNKS_PER_FILE {
            break;
        }
        let start = def.line.min(n.saturating_sub(1));
        // 下一个不同起始行的锚点为天然边界。
        let next_line = anchors[i + 1..]
            .iter()
            .map(|d| d.line)
            .find(|&l| l > def.line)
            .unwrap_or(n);
        let mut end = next_line.saturating_sub(1).min(n.saturating_sub(1));
        if end < start {
            end = start;
        }
        if end - start + 1 > MAX_CHUNK_LINES {
            end = start + MAX_CHUNK_LINES - 1;
        }
        push_chunk(file_idx, lines, start, end, Some(def.name.clone()), out);
        made += 1;
    }
}

fn push_chunk(
    file_idx: usize,
    lines: &[&str],
    start: usize,
    end: usize,
    symbol: Option<String>,
    out: &mut Vec<Chunk>,
) {
    let raw = lines[start..=end].join("\n");
    let text = truncate_snippet(&raw);
    // token 来自块原文(截断前的全文),含符号名;BM25 的文档词。
    let mut tokens: Vec<String> = tokenize_code(&raw);
    if let Some(sym) = &symbol {
        // 让符号名本身的拆词额外计一次,温和提升「以该符号为名」的块。
        tokens.extend(split_identifier(sym));
    }
    out.push(Chunk {
        file_idx,
        start_line: start,
        end_line: end,
        symbol,
        text,
        tokens,
    });
}

/// 对全部块做本地混合排名,返回 (块索引, 综合分, 理由),按分降序、确定性平局(索引升序)。
fn rank_chunks(
    chunks: &[Chunk],
    query_terms: &[String],
    ranks: &crate::graph::GraphRanks,
    _rel_paths: &[String],
) -> Vec<(usize, f64, String)> {
    let n_docs = chunks.len();
    // 文档频率(df):多少个块至少含该词一次。
    let mut df: HashMap<&str, usize> = HashMap::new();
    let mut total_len = 0usize;
    for c in chunks {
        let mut seen: HashSet<&str> = HashSet::new();
        for t in &c.tokens {
            if seen.insert(t.as_str()) {
                *df.entry(t.as_str()).or_insert(0) += 1;
            }
        }
        total_len += c.tokens.len();
    }
    let avg_len = if n_docs > 0 {
        total_len as f64 / n_docs as f64
    } else {
        1.0
    };

    let query_set: HashSet<&str> = query_terms.iter().map(|s| s.as_str()).collect();

    let mut scored: Vec<(usize, f64, String)> = Vec::with_capacity(n_docs);
    for (idx, c) in chunks.iter().enumerate() {
        // 块内词频。
        let mut tf: HashMap<&str, usize> = HashMap::new();
        for t in &c.tokens {
            *tf.entry(t.as_str()).or_insert(0) += 1;
        }
        let dl = c.tokens.len().max(1) as f64;

        // BM25 词法分。
        let mut bm25 = 0.0f64;
        let mut matched_terms: Vec<&str> = Vec::new();
        for term in query_terms {
            let f = *tf.get(term.as_str()).unwrap_or(&0);
            if f == 0 {
                continue;
            }
            matched_terms.push(term.as_str());
            let n_q = *df.get(term.as_str()).unwrap_or(&0) as f64;
            // idf(BM25 形式,+0.5 平滑,floor 到一个很小正数避免负/零)。
            let idf = (((n_docs as f64 - n_q + 0.5) / (n_q + 0.5)) + 1.0).ln();
            let f = f as f64;
            let denom = f + BM25_K1 * (1.0 - BM25_B + BM25_B * (dl / avg_len));
            bm25 += idf * (f * (BM25_K1 + 1.0)) / denom;
        }

        // 精确标识符命中加成:块的某 token 与某 query 词项完全相等(已是拆词后小写,故等价于词命中,
        // 但对「符号名恰为 query 词」给额外固定加成,凸显定义块)。
        let mut exact_hits: Vec<&str> = Vec::new();
        if let Some(sym) = &c.symbol {
            for piece in split_identifier(sym) {
                if query_set.contains(piece.as_str()) {
                    exact_hits.push("symbol");
                    break;
                }
            }
        }
        let exact_bonus = if exact_hits.is_empty() {
            0.0
        } else {
            EXACT_IDENT_BONUS
        };

        // PageRank 文件重要度提升(温和:把很小的概率放大后取 ln1p)。
        let frank = ranks.file_rank.get(c.file_idx).copied().unwrap_or(0.0);
        let pr_boost = (frank * PAGERANK_BOOST_SCALE).ln_1p();

        // 无任何词法命中的块:不进结果(纯靠 PageRank 的块对 query 无信息量)。
        if bm25 <= 0.0 && exact_bonus <= 0.0 {
            continue;
        }

        let score = bm25 + exact_bonus + pr_boost;
        let why = build_why(&matched_terms, !exact_hits.is_empty(), frank);
        scored.push((idx, score, why));
    }

    // 降序;平局按块索引升序(确定性)。
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    scored
}

fn build_why(matched: &[&str], symbol_hit: bool, frank: f64) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !matched.is_empty() {
        parts.push(format!("词法命中 {}", matched.join("/")));
    }
    if symbol_hit {
        parts.push("符号名精确命中".to_string());
    }
    if frank > 0.0 {
        parts.push(format!("文件重要度 {:.4}", frank));
    }
    if parts.is_empty() {
        "图先验".to_string()
    } else {
        parts.join(";")
    }
}

// ── 分词 ───────────────────────────────────────────────────────────────────

/// 把代码文本拆成 BM25 用的 token:先按非标识符字符切成原始词,再对每个原始词按
/// camelCase/snake_case 拆成子词,全部小写。原始词与子词都计入(子词提召回,原始词保精确)。
fn tokenize_code(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        if raw.is_empty() {
            continue;
        }
        let lower = raw.to_ascii_lowercase();
        // 原始整词(若是多段标识符)也保留一份,利于「整名」精确匹配。
        for piece in split_identifier(&lower) {
            out.push(piece);
        }
        // 避免单字符噪声词把文档撑长:长度≥2 才保留整词副本(子词已覆盖单段情形)。
        if lower.len() >= 2 && !out.last().is_some_and(|l| *l == lower) {
            out.push(lower);
        }
    }
    out
}

/// 拆一个标识符为子词:在 snake/连字符、camelCase 边界、字母↔数字边界切分,小写。
/// 例:`runAgentLoop` → [run, agent, loop];`MAX_FILES` → [max, files];`utf8Text` → [utf, 8, text]。
fn split_identifier(ident: &str) -> Vec<String> {
    let mut pieces: Vec<String> = Vec::new();
    for part in ident.split(|c: char| c == '_' || c == '-' || c == '.') {
        if part.is_empty() {
            continue;
        }
        let chars: Vec<char> = part.chars().collect();
        let mut cur = String::new();
        let mut prev: Option<char> = None;
        for (i, &ch) in chars.iter().enumerate() {
            if let Some(p) = prev {
                let boundary =
                    // 小写/数字 → 大写:camelCase 边界。
                    (!p.is_uppercase() && ch.is_uppercase())
                    // 字母 ↔ 数字 切换。
                    || (p.is_alphabetic() && ch.is_ascii_digit())
                    || (p.is_ascii_digit() && ch.is_alphabetic())
                    // 连续大写后接「大写+小写」(如 HTMLParser → HTML | Parser):向前看一位。
                    || (p.is_uppercase()
                        && ch.is_uppercase()
                        && chars.get(i + 1).is_some_and(|c| c.is_lowercase()));
                if boundary && !cur.is_empty() {
                    pieces.push(std::mem::take(&mut cur).to_ascii_lowercase());
                }
            }
            cur.push(ch);
            prev = Some(ch);
        }
        if !cur.is_empty() {
            pieces.push(cur.to_ascii_lowercase());
        }
    }
    // 过滤长度<2 的碎片以外的噪声:保留单数字/单字母可能有意义(如 c, x),但去空。
    pieces.retain(|p| !p.is_empty());
    pieces
}

/// 把 query 拆成与文档同口径的检索词项(标识符拆词 + 整词,小写,去重保序)。
fn query_terms(query: &str) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for t in tokenize_code(query) {
        if t.len() >= 2 && seen.insert(t.clone()) {
            out.push(t);
        }
    }
    // 长度1的有意义单词(极少)忽略,避免 idf 噪声。
    out
}

/// query 里「标识符样式」的词(给 PageRank 个性化 mentioned 用,口径同 lib::parse_query:
/// 连续字母数字下划线、长度≥2,但**不**拆 camelCase,以匹配源码里出现的整名符号)。
fn query_idents(query: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    let mut cur = String::new();
    for ch in query.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else if !cur.is_empty() {
            if cur.len() >= 2 {
                set.insert(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
    }
    if cur.len() >= 2 {
        set.insert(cur);
    }
    set
}

// ── 杂项 ───────────────────────────────────────────────────────────────────

fn truncate_snippet(s: &str) -> String {
    if s.chars().count() <= MAX_SNIPPET_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX_SNIPPET_CHARS).collect();
    out.push('…');
    out
}

fn normalize_top_k(requested: usize) -> usize {
    let v = if requested == 0 { DEFAULT_TOP_K } else { requested };
    v.clamp(1, MAX_TOP_K)
}

fn empty_result(note: &str) -> CodeSearchResult {
    CodeSearchResult {
        chunks: Vec::new(),
        total_files: 0,
        total_chunks: 0,
        truncated: false,
        note: note.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_workspace() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let id = N.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "mdga-codesearch-test-{}-{}",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    fn req(q: &str, top_k: usize) -> CodeSearchRequest {
        CodeSearchRequest {
            query: q.to_string(),
            top_k,
        }
    }

    /// 一个小 fixture:不同文件含不同主题的符号,用于验证 query 相关性。
    fn make_fixture() -> PathBuf {
        let dir = temp_workspace();
        write(
            &dir,
            "auth.rs",
            "pub fn validate_login(user: &str, password: &str) -> bool {\n    \
             // checks the user credentials against the auth store\n    \
             user_exists(user) && password_matches(password)\n}\n",
        );
        write(
            &dir,
            "math.rs",
            "pub fn add_numbers(a: i32, b: i32) -> i32 {\n    a + b\n}\n\
             pub fn multiply(a: i32, b: i32) -> i32 {\n    a * b\n}\n",
        );
        write(
            &dir,
            "render.ts",
            "export function renderWidget(node: Node) {\n  \
             // draws the widget tree to the canvas\n  paint(node);\n}\n",
        );
        dir
    }

    // ── 切块 ────────────────────────────────────────────────────────────

    #[test]
    fn split_identifier_handles_camel_snake_and_digits() {
        assert_eq!(split_identifier("runAgentLoop"), vec!["run", "agent", "loop"]);
        assert_eq!(split_identifier("MAX_FILES"), vec!["max", "files"]);
        assert_eq!(split_identifier("HTMLParser"), vec!["html", "parser"]);
        assert_eq!(split_identifier("utf8Text"), vec!["utf", "8", "text"]);
        assert_eq!(split_identifier("simple"), vec!["simple"]);
    }

    #[test]
    fn chunks_anchor_on_definitions() {
        let dir = make_fixture();
        // 直接查 multiply,应命中 math.rs 里 multiply 那个块(以定义为锚)。
        let r = code_search(dir.to_str().unwrap(), &req("multiply", 5));
        assert!(r.total_chunks >= 4, "应至少切出 4 个块,实得 {}", r.total_chunks);
        let top = &r.chunks[0];
        assert_eq!(top.path, "math.rs");
        assert_eq!(top.symbol.as_deref(), Some("multiply"));
        assert!(
            top.snippet.contains("multiply"),
            "片段应含 multiply,实得:\n{}",
            top.snippet
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fixed_window_fallback_for_files_without_defs() {
        let dir = temp_workspace();
        // 纯文本、无任何声明关键字 → 无定义 → 行窗回退。多于一窗以验证滑动。
        let mut body = String::new();
        for i in 0..100 {
            body.push_str(&format!("plain prose line number {i} about networking sockets\n"));
        }
        write(&dir, "notes.txt", &body);
        let r = code_search(dir.to_str().unwrap(), &req("networking sockets", 5));
        assert!(r.total_chunks >= 2, "100 行/40 窗应≥2 块,实得 {}", r.total_chunks);
        assert!(!r.chunks.is_empty(), "应有命中块");
        assert!(r.chunks[0].symbol.is_none(), "行窗块无 symbol");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── 查询相关性 ──────────────────────────────────────────────────────

    #[test]
    fn query_returns_most_relevant_chunk_first() {
        let dir = make_fixture();
        // 「login」语义应把 auth.rs 的 validate_login 排到最前。
        let r = code_search(dir.to_str().unwrap(), &req("validate login password", 5));
        assert!(!r.chunks.is_empty(), "应有命中");
        assert_eq!(
            r.chunks[0].path, "auth.rs",
            "login 查询应让 auth.rs 居首,实得:\n{:#?}",
            r.chunks
        );
        assert_eq!(r.chunks[0].symbol.as_deref(), Some("validate_login"));
        assert!(
            r.chunks[0].why.contains("login") || r.chunks[0].why.contains("符号名"),
            "理由应解释命中,实得 {}",
            r.chunks[0].why
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unrelated_query_ranks_correct_topic() {
        let dir = make_fixture();
        // 「render widget canvas」应让 render.ts 居首,而非 auth/math。
        let r = code_search(dir.to_str().unwrap(), &req("render widget canvas", 5));
        assert!(!r.chunks.is_empty());
        assert_eq!(
            r.chunks[0].path, "render.ts",
            "render 查询应让 render.ts 居首,实得:\n{:#?}",
            r.chunks
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn top_k_is_respected_and_clamped() {
        let dir = make_fixture();
        let r = code_search(dir.to_str().unwrap(), &req("a", 2)); // 单字母被忽略 → 无词法命中
        // query "a" 拆词后长度<2 被过滤,无有效词项 → 无命中(求真:不返回噪声)。
        assert!(r.chunks.is_empty(), "无有效查询词应返回空,实得 {:#?}", r.chunks);

        let r2 = code_search(dir.to_str().unwrap(), &req("add multiply numbers", 2));
        assert!(r2.chunks.len() <= 2, "top_k=2 应至多 2 个,实得 {}", r2.chunks.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── 确定性 ──────────────────────────────────────────────────────────

    #[test]
    fn ranking_is_deterministic() {
        let dir = make_fixture();
        let a = code_search(dir.to_str().unwrap(), &req("add numbers multiply login render", 8));
        let b = code_search(dir.to_str().unwrap(), &req("add numbers multiply login render", 8));
        assert_eq!(a.chunks.len(), b.chunks.len());
        for (x, y) in a.chunks.iter().zip(b.chunks.iter()) {
            assert_eq!(x.path, y.path);
            assert_eq!(x.start_line, y.start_line);
            assert_eq!(x.end_line, y.end_line);
            assert!((x.score - y.score).abs() < 1e-12, "同输入分数应一致");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── 软失败 / 边界 ───────────────────────────────────────────────────

    #[test]
    fn empty_query_is_soft_failure() {
        let dir = make_fixture();
        let r = code_search(dir.to_str().unwrap(), &req("   ", 5));
        assert!(r.chunks.is_empty());
        assert_eq!(r.total_chunks, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_workspace_is_soft_failure() {
        let r = code_search("C:/definitely/not/here/mdga-codesearch", &req("anything", 5));
        assert!(r.chunks.is_empty());
        assert_eq!(r.total_files, 0);
    }

    #[test]
    fn snippet_is_capped() {
        let dir = temp_workspace();
        // 一个超长单行(超过 MAX_SNIPPET_CHARS)的定义。
        let huge: String = "x".repeat(MAX_SNIPPET_CHARS + 500);
        write(&dir, "big.rs", &format!("pub fn giant() {{ let s = \"{huge}\"; }}\n"));
        let r = code_search(dir.to_str().unwrap(), &req("giant", 3));
        assert!(!r.chunks.is_empty());
        assert!(
            r.chunks[0].snippet.chars().count() <= MAX_SNIPPET_CHARS + 1,
            "片段应被截断到上限,实得 {} 字符",
            r.chunks[0].snippet.chars().count()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn excludes_heavy_dirs() {
        let dir = temp_workspace();
        write(&dir, "app.rs", "pub fn real_handler() {}\n");
        let nm = dir.join("node_modules").join("pkg");
        std::fs::create_dir_all(&nm).unwrap();
        std::fs::write(nm.join("dep.js"), "export function real_handler() {}\n").unwrap();
        let r = code_search(dir.to_str().unwrap(), &req("real_handler", 10));
        assert_eq!(r.total_files, 1, "node_modules 应被排除,实得 {}", r.total_files);
        for c in &r.chunks {
            assert_eq!(c.path, "app.rs");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
