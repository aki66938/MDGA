//! mdga-codemap：tree-sitter + 个性化 PageRank 仓库地图（R2，M 阶段）。
//!
//! 用途:给模型在「文件树摘要 + code_overview」之外,提供一张**按引用重要度排名**的
//! 全仓符号地图——既能在不读全文件的情况下定位「核心代码在哪、谁调用谁」,也能围绕
//! focus 文件 / query 关键词做个性化收敛。无外部基础设施(纯解析 + 整内存图)。
//!
//! 流水线:gitignore 感知发现文件 → tree-sitter 抽取定义/引用标签(按 mtime 缓存) →
//! 构引用图跑 PageRank → token 预算内渲染。任一文件/语言失败都降级跳过,不影响整体。

mod graph;
mod heuristic;
mod lang;
mod render;
mod search;
mod tags;

pub use search::{
    code_search, code_search_with_embedder, CodeSearchChunk, CodeSearchRequest, CodeSearchResult,
    Embedder,
};

use ignore::WalkBuilder;
use serde::Serialize;
use std::collections::HashSet;
use std::path::PathBuf;

/// 发现阶段扫描的文件数上限(超过即截断并标记)。
const MAX_FILES: usize = 8000;

/// 无论是否被 gitignore,都硬排除的重目录(依赖/构建产物/VCS 元数据)。
/// 这些目录里没有项目源码价值,却动辄上万文件,放进遍历会拖垮发现阶段。
const HARD_EXCLUDED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    "dist",
    "build",
    ".next",
    ".svelte-kit",
];
const DEFAULT_MAX_TOKENS: usize = 1500;
const MIN_MAX_TOKENS: usize = 200;
const MAX_MAX_TOKENS: usize = 20000;

/// 仓库地图请求。
#[derive(Debug, Clone, Default)]
pub struct CodemapRequest {
    /// 个性化关注的文件(工作区相对路径);非空时排名向这些文件收敛。
    pub focus_files: Vec<String>,
    /// 自由文本关键词;命中的符号边权放大、其定义文件被抬高。
    pub query: Option<String>,
    /// 渲染 token 预算(0 表示用默认值);最终会夹到 [200, 20000]。
    pub max_tokens: usize,
}

/// 仓库地图结果。
#[derive(Debug, Clone, Serialize)]
pub struct CodemapResult {
    /// 渲染好的地图文本。
    pub map: String,
    /// 扫描到的受支持源文件总数。
    pub total_files: usize,
    /// 提取到的定义符号总数。
    pub total_definitions: usize,
    /// 地图实际包含的文件数。
    pub files_in_map: usize,
    /// 是否因预算/上限有内容被省略。
    pub truncated: bool,
    /// 给模型的口径说明。
    pub note: String,
}

/// 发现 + 抽取 + 排名流水线的共享中间产物:既供 `build_repo_map` 渲染地图,也供
/// `analyze_repo` 产出结构化数据,确保二者基于**完全一致**的发现/解析/排名口径(单一真相来源)。
struct PipelineOutput {
    rel_paths: Vec<String>,
    file_tags: Vec<tags::FileTags>,
    ranks: graph::GraphRanks,
    total_definitions: usize,
    discover_truncated: bool,
}

/// 跑「发现 → 抽取标签 → PageRank 排名」共享核心。软失败(工作区不存在或无可扫描源文件)
/// 时返回 `Err(说明文案)`,成功时返回 `Ok(PipelineOutput)`;由上层 `build_repo_map`/`analyze_repo`
/// 各自把 `Err` 落成空地图/空分析。
fn run_pipeline(root: &std::path::Path, request: &CodemapRequest) -> Result<PipelineOutput, String> {
    if !root.is_dir() {
        return Err("工作区路径不存在或不是目录".to_string());
    }

    // 1) gitignore 感知地发现受支持源文件（与 code_search 共用同一套发现/硬排除口径）。
    let Discovered {
        rel_paths,
        abs_paths,
        truncated: discover_truncated,
    } = discover_source_files(root);

    if rel_paths.is_empty() {
        return Err(
            "工作区内未发现可扫描的文本源文件\
             (含 tree-sitter 精确解析的 rust/python/js/ts/go/java/c/c++/c#/ruby/php/bash/lua/scala,\
             及其它文本文件的启发式回退;仅二进制/媒体/锁文件等被排除)"
                .to_string(),
        );
    }

    // 2) 抽取标签(按 mtime 缓存)。
    let file_tags: Vec<tags::FileTags> = abs_paths
        .iter()
        .map(|p| {
            let arc = tags::tags_for_file(p);
            tags::FileTags {
                defs: arc.defs.clone(),
                refs: arc.refs.clone(),
            }
        })
        .collect();
    let total_definitions: usize = file_tags.iter().map(|t| t.defs.len()).sum();

    // 3) 解析 focus / query → 图 PageRank。
    let focus = resolve_focus(&request.focus_files, &rel_paths);
    let mentioned = parse_query(request.query.as_deref());
    let ranks = graph::rank(&file_tags, &focus, &mentioned);

    Ok(PipelineOutput {
        rel_paths,
        file_tags,
        ranks,
        total_definitions,
        discover_truncated,
    })
}

/// 构建仓库地图。永不硬失败:工作区不存在或无源码时返回空 map + 说明。
pub fn build_repo_map(workspace_root: &str, request: &CodemapRequest) -> CodemapResult {
    let root = PathBuf::from(workspace_root);
    let PipelineOutput {
        rel_paths,
        file_tags,
        ranks,
        total_definitions,
        discover_truncated,
    } = match run_pipeline(&root, request) {
        Ok(out) => out,
        Err(note) => return empty_result(&note),
    };

    // 4) 预算内渲染。
    let budget = normalize_tokens(request.max_tokens);
    let rendered = render::render(&rel_paths, &file_tags, &ranks, budget);

    let truncated = rendered.truncated || discover_truncated;
    let note = format!(
        "仓库地图:tree-sitter 抽取定义 + PageRank 引用排名(非语义向量,M 阶段)。\
         共扫描 {} 个源文件、{} 个定义,地图含 {} 个文件。行号为定义所在行。{}{}",
        rel_paths.len(),
        total_definitions,
        rendered.files_included,
        if discover_truncated {
            format!("文件数超过 {MAX_FILES} 上限已截断。")
        } else {
            String::new()
        },
        if rendered.truncated {
            "受 token 预算限制,部分定义已省略;可提高 max_tokens 或用 focus_files/query 聚焦。"
        } else {
            ""
        },
    );

    CodemapResult {
        map: rendered.map,
        total_files: rel_paths.len(),
        total_definitions,
        files_in_map: rendered.files_included,
        truncated,
        note,
    }
}

/// 供「自动注入上下文」用的便捷封装:无 focus/query,较小预算,只返回地图文本。
pub fn repo_map_for_context(workspace_root: &str, max_tokens: usize) -> String {
    let result = build_repo_map(
        workspace_root,
        &CodemapRequest {
            focus_files: Vec::new(),
            query: None,
            max_tokens,
        },
    );
    result.map
}

/// gitignore 感知的源文件发现结果：相对路径与绝对路径一一对应（同序）。
pub(crate) struct Discovered {
    pub rel_paths: Vec<String>,
    pub abs_paths: Vec<PathBuf>,
    /// 是否因 MAX_FILES 上限而截断。
    pub truncated: bool,
}

/// 发现工作区内全部「可扫描的文本源文件」，硬排除依赖/构建/VCS 重目录并遵守 gitignore。
/// repo_map 与 code_search 共用此口径，保证两者「看到的文件集合」完全一致。
pub(crate) fn discover_source_files(root: &std::path::Path) -> Discovered {
    let mut rel_paths: Vec<String> = Vec::new();
    let mut abs_paths: Vec<PathBuf> = Vec::new();
    let mut truncated = false;
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .parents(true)
        // 硬排除重目录:对目录项按名字过滤,filter_entry 会连同其整棵子树一起剪掉,
        // 与 gitignore 无关——即便仓库未忽略 node_modules/target 也照样跳过。
        .filter_entry(|entry| {
            if entry.file_type().is_some_and(|t| t.is_dir()) {
                if let Some(name) = entry.file_name().to_str() {
                    return !HARD_EXCLUDED_DIRS.contains(&name);
                }
            }
            true
        })
        .build();
    for result in walker {
        let Ok(entry) = result else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        if !lang::should_scan_extension(&ext) {
            continue;
        }
        if rel_paths.len() >= MAX_FILES {
            truncated = true;
            break;
        }
        let rel = path.strip_prefix(root).unwrap_or(path);
        rel_paths.push(to_forward_slashes(rel));
        abs_paths.push(path.to_path_buf());
    }
    Discovered {
        rel_paths,
        abs_paths,
        truncated,
    }
}

// ── 结构化分析 API（供 R11 仓库 wiki 等下游消费）─────────────────────────────
//
// `build_repo_map` 把分析结果渲染成「token 预算内的字符串」给模型直读;但下游若要
// **按目录组织、持久化、可查询**,需要的是结构化数据而非要再解析的字符串。`analyze_repo`
// 复用同一条发现/解析/排名流水线,把每个文件的路径、PageRank 分、按重要度排序的顶层符号
// (名/行/签名/定义级流入分)如实导出,让 wiki 层只做「组织 + 持久化 + 检索」、不重做符号抽取。

/// 单个符号(定义)的结构化条目。`score` 为 PageRank 摊回该定义的「被引用重要度」流入分。
#[derive(Debug, Clone, Serialize)]
pub struct SymbolEntry {
    /// 符号名(节点文本)。
    pub name: String,
    /// 定义名所在行(1 基,与渲染地图一致,便于直接 read_file 定位)。
    pub line: usize,
    /// 该定义所在行去空白后的源文本(已含 fn/struct/class 等关键字,种类隐含其中)。
    pub signature: String,
    /// 该定义的引用流入分(越高表示被越多重要文件引用)。
    pub score: f64,
}

/// 单个源文件的结构化分析:路径 + 文件级 PageRank + 按重要度降序的顶层符号。
#[derive(Debug, Clone, Serialize)]
pub struct FileAnalysis {
    /// 工作区相对路径(正斜杠)。
    pub path: String,
    /// 文件级 PageRank 分(越高越「核心/被依赖」)。
    pub file_rank: f64,
    /// 本文件定义总数(可能多于 `top_symbols` 因后者按上限截断)。
    pub definition_count: usize,
    /// 按定义级流入分降序、平局按行号升序挑出的顶层符号(已截到 `max_symbols_per_file`)。
    pub top_symbols: Vec<SymbolEntry>,
}

/// 整仓结构化分析结果。文件按 `file_rank` 降序排列(确定性:平局按路径)。
#[derive(Debug, Clone, Serialize)]
pub struct RepoAnalysis {
    /// 按重要度降序的每文件分析(已剔除无定义的文件)。
    pub files: Vec<FileAnalysis>,
    /// 扫描到的受支持源文件总数。
    pub total_files: usize,
    /// 提取到的定义符号总数。
    pub total_definitions: usize,
    /// 是否因文件数上限发生截断。
    pub truncated: bool,
}

/// 单文件保留的顶层符号数上限,与 render 的口径一致,避免某个大文件挤占整张分析。
const MAX_SYMBOLS_PER_FILE: usize = 40;

/// 结构化分析整个工作区。永不硬失败:工作区不存在或无源码时返回空 `files`。
///
/// 与 `build_repo_map` 共用同一条发现/解析/排名流水线,但导出结构化数据而非渲染字符串。
/// `request.focus_files` / `request.query` 同样会个性化排名(收敛到关注区域)。
pub fn analyze_repo(workspace_root: &str, request: &CodemapRequest) -> RepoAnalysis {
    let root = PathBuf::from(workspace_root);
    let PipelineOutput {
        rel_paths,
        file_tags,
        ranks,
        total_definitions,
        discover_truncated,
    } = match run_pipeline(&root, request) {
        Ok(out) => out,
        Err(_) => {
            return RepoAnalysis {
                files: Vec::new(),
                total_files: 0,
                total_definitions: 0,
                truncated: false,
            }
        }
    };
    let total_files = rel_paths.len();

    // 仅保留有定义的文件(无定义的文件对 wiki 摘要无信息量),按 file_rank 降序、平局按路径升序。
    let mut order: Vec<usize> = (0..file_tags.len())
        .filter(|&i| !file_tags[i].defs.is_empty())
        .collect();
    order.sort_by(|&a, &b| {
        let ra = ranks.file_rank.get(a).copied().unwrap_or(0.0);
        let rb = ranks.file_rank.get(b).copied().unwrap_or(0.0);
        rb.partial_cmp(&ra)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| rel_paths[a].cmp(&rel_paths[b]))
    });

    let mut files: Vec<FileAnalysis> = Vec::with_capacity(order.len());
    for fi in order {
        let tags = &file_tags[fi];
        // 定义按流入分降序、平局按行号升序;再截到单文件上限。
        let mut def_idx: Vec<usize> = (0..tags.defs.len()).collect();
        def_idx.sort_by(|&x, &y| {
            let sx = ranks
                .def_score
                .get(&(fi, tags.defs[x].name.clone()))
                .copied()
                .unwrap_or(0.0);
            let sy = ranks
                .def_score
                .get(&(fi, tags.defs[y].name.clone()))
                .copied()
                .unwrap_or(0.0);
            sy.partial_cmp(&sx)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| tags.defs[x].line.cmp(&tags.defs[y].line))
        });
        def_idx.truncate(MAX_SYMBOLS_PER_FILE);

        let top_symbols: Vec<SymbolEntry> = def_idx
            .into_iter()
            .map(|di| {
                let d = &tags.defs[di];
                SymbolEntry {
                    name: d.name.clone(),
                    line: d.line + 1,
                    signature: d.sig.clone(),
                    score: ranks
                        .def_score
                        .get(&(fi, d.name.clone()))
                        .copied()
                        .unwrap_or(0.0),
                }
            })
            .collect();

        files.push(FileAnalysis {
            path: rel_paths[fi].clone(),
            file_rank: ranks.file_rank.get(fi).copied().unwrap_or(0.0),
            definition_count: tags.defs.len(),
            top_symbols,
        });
    }

    RepoAnalysis {
        files,
        total_files,
        total_definitions,
        truncated: discover_truncated,
    }
}

fn resolve_focus(focus_files: &[String], rel_paths: &[String]) -> Vec<usize> {
    if focus_files.is_empty() {
        return Vec::new();
    }
    let wanted: HashSet<String> = focus_files.iter().map(|f| normalize_rel(f)).collect();
    rel_paths
        .iter()
        .enumerate()
        .filter(|(_, p)| wanted.contains(&normalize_rel(p)))
        .map(|(i, _)| i)
        .collect()
}

/// 把 query 拆成标识符样式的关注词(字母数字/下划线连续段,长度≥2)。
fn parse_query(query: Option<&str>) -> HashSet<String> {
    let mut set = HashSet::new();
    let Some(q) = query else { return set };
    let mut cur = String::new();
    for ch in q.chars() {
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

fn normalize_rel(p: &str) -> String {
    p.replace('\\', "/").trim_start_matches("./").to_string()
}

fn to_forward_slashes(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn normalize_tokens(requested: usize) -> usize {
    let v = if requested == 0 {
        DEFAULT_MAX_TOKENS
    } else {
        requested
    };
    v.clamp(MIN_MAX_TOKENS, MAX_MAX_TOKENS)
}

fn empty_result(note: &str) -> CodemapResult {
    CodemapResult {
        map: String::new(),
        total_files: 0,
        total_definitions: 0,
        files_in_map: 0,
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
            "mdga-codemap-test-{}-{}",
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

    /// hub.rs 定义被 a.rs/b.rs 引用的符号 → PageRank 应把它排到最前;c.ts 验证多语言解析。
    fn make_fixture() -> PathBuf {
        let dir = temp_workspace();
        write(
            &dir,
            "hub.rs",
            "pub fn shared() {}\npub struct Widget { x: i32 }\n",
        );
        write(&dir, "a.rs", "fn alpha() {\n    shared();\n    let _w: Widget;\n}\n");
        write(&dir, "b.rs", "fn beta() {\n    shared();\n}\n");
        write(
            &dir,
            "c.ts",
            "export function helper() {}\nfunction useIt() { helper(); }\n",
        );
        dir
    }

    fn default_req() -> CodemapRequest {
        CodemapRequest::default()
    }

    #[test]
    fn extracts_defs_across_languages_and_ranks_hub_first() {
        let dir = make_fixture();
        let result = build_repo_map(dir.to_str().unwrap(), &default_req());

        assert_eq!(result.total_files, 4, "应扫描到 4 个源文件");
        // shared, Widget, alpha, beta, helper, useIt = 6 个定义。
        assert!(
            result.total_definitions >= 6,
            "定义数应≥6,实得 {}",
            result.total_definitions
        );
        assert!(!result.map.is_empty(), "地图不应为空");
        // 被最多引用的 hub.rs 应排在地图最前。
        let first = result.map.lines().next().unwrap_or_default();
        assert_eq!(first, "hub.rs:", "hub.rs 应排在最前,实得地图:\n{}", result.map);
        assert!(
            result.map.contains("pub fn shared"),
            "应含 shared 的签名行,实得:\n{}",
            result.map
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn focus_files_personalize_ranking() {
        let dir = make_fixture();
        let mut req = default_req();
        req.focus_files = vec!["b.rs".to_string()];
        let result = build_repo_map(dir.to_str().unwrap(), &req);
        let first = result.map.lines().next().unwrap_or_default();
        assert_eq!(
            first, "b.rs:",
            "focus=b.rs 时 b.rs 应被个性化抬到最前,实得地图:\n{}",
            result.map
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn query_terms_bias_ranking() {
        let dir = make_fixture();
        // query 命中 helper(定义于 c.ts)→ 抬高 c.ts。
        let mut req = default_req();
        req.query = Some("helper".to_string());
        let result = build_repo_map(dir.to_str().unwrap(), &req);
        let first = result.map.lines().next().unwrap_or_default();
        assert_eq!(
            first, "c.ts:",
            "query=helper 时 c.ts 应被抬前,实得:\n{}",
            result.map
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn respects_per_file_cap_and_token_budget() {
        let dir = temp_workspace();
        let mut big = String::new();
        for i in 0..200 {
            big.push_str(&format!("pub fn f{i}() {{}}\n"));
        }
        write(&dir, "big.rs", &big);

        let mut req = default_req();
        req.max_tokens = 200;
        let result = build_repo_map(dir.to_str().unwrap(), &req);
        assert!(result.truncated, "200 个函数 + 小预算应触发截断");
        let def_lines = result.map.lines().filter(|l| l.starts_with("  ")).count();
        assert!(
            def_lines <= 40,
            "单文件展示定义数应受 40 上限约束,实得 {}",
            def_lines
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn output_is_deterministic() {
        let dir = make_fixture();
        let a = build_repo_map(dir.to_str().unwrap(), &default_req());
        let b = build_repo_map(dir.to_str().unwrap(), &default_req());
        assert_eq!(a.map, b.map, "同输入两次构建地图应完全一致");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_workspace_is_soft_failure() {
        let result = build_repo_map("C:/definitely/not/here/mdga-x", &default_req());
        assert!(result.map.is_empty());
        assert_eq!(result.total_files, 0);
    }

    /// 重目录(node_modules / target)即便未被 gitignore 也应被硬排除:
    /// 其内源文件不计入 total_files、不出现在地图里;同级真实源码照常收录。
    #[test]
    fn hard_excludes_heavy_dirs_regardless_of_gitignore() {
        let dir = temp_workspace();
        // 真实源码:应被收录。
        write(&dir, "app.rs", "pub fn real_app() {}\n");

        // node_modules 下的源文件:应被排除(注意:此处无 .gitignore,验证与 gitignore 无关)。
        let nm = dir.join("node_modules").join("pkg");
        std::fs::create_dir_all(&nm).unwrap();
        std::fs::write(nm.join("dep.js"), "export function dep() {}\n").unwrap();
        std::fs::write(nm.join("dep.ts"), "export function depTs() {}\n").unwrap();

        // target 下的生成产物源文件:应被排除。
        let tgt = dir.join("target").join("debug");
        std::fs::create_dir_all(&tgt).unwrap();
        std::fs::write(tgt.join("build.rs"), "pub fn generated() {}\n").unwrap();

        let result = build_repo_map(dir.to_str().unwrap(), &default_req());
        assert_eq!(
            result.total_files, 1,
            "只应扫到 app.rs;node_modules/target 须被硬排除,实得 {} 个文件、地图:\n{}",
            result.total_files, result.map
        );
        assert!(
            !result.map.contains("dep") && !result.map.contains("generated"),
            "地图不应包含被排除目录里的符号,实得:\n{}",
            result.map
        );
        assert!(
            result.map.contains("real_app"),
            "地图应包含真实源码 app.rs 的符号,实得:\n{}",
            result.map
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 新增 grammar 端到端：一个 .java 文件经完整流水线应抽出其 class 与 method 定义。
    #[test]
    fn java_grammar_extracts_class_and_method() {
        let dir = temp_workspace();
        write(
            &dir,
            "Greeter.java",
            "public class Greeter {\n    public String greet(String who) {\n        return \"hi \" + who;\n    }\n}\n",
        );
        let result = build_repo_map(dir.to_str().unwrap(), &default_req());
        assert_eq!(result.total_files, 1, "应扫到 1 个 .java 文件");
        assert!(
            result.map.contains("Greeter"),
            "地图应含 Java 类名 Greeter,实得:\n{}",
            result.map
        );
        assert!(
            result.map.contains("greet"),
            "地图应含 Java 方法名 greet,实得:\n{}",
            result.map
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 启发式回退端到端：无 grammar 的扩展名(.kt)文件也应贡献粗粒度符号,
    /// 证明「每个文本文件都进地图」的兜底确实接到了主流水线上。
    #[test]
    fn heuristic_fallback_extracts_symbol_from_unsupported_ext() {
        let dir = temp_workspace();
        // Kotlin 暂无专属 grammar → 走启发式;fun/class 关键字应被识别。
        write(
            &dir,
            "Sample.kt",
            "// a kotlin file with no tree-sitter grammar\nclass Sample {\n    fun doThing() {}\n}\n",
        );
        let result = build_repo_map(dir.to_str().unwrap(), &default_req());
        assert_eq!(result.total_files, 1, "无 grammar 的文本文件也应被发现并计数");
        assert!(
            result.total_definitions >= 1,
            "启发式应至少抽到 1 个定义,实得 {}",
            result.total_definitions
        );
        assert!(
            result.map.contains("Sample") || result.map.contains("doThing"),
            "地图应含启发式抽到的符号(Sample 或 doThing),实得:\n{}",
            result.map
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 结构化分析:与渲染地图共用同一流水线,应导出按 file_rank 降序的每文件结构化符号。
    /// hub.rs 被 a.rs/b.rs 引用 → file_rank 最高 → 排在 files[0];其顶层符号含 shared/Widget。
    #[test]
    fn analyze_repo_exports_structured_ranked_symbols() {
        let dir = make_fixture();
        let analysis = analyze_repo(dir.to_str().unwrap(), &default_req());
        assert_eq!(analysis.total_files, 4, "应扫描到 4 个源文件");
        assert!(analysis.total_definitions >= 6, "定义数应≥6");
        assert!(!analysis.files.is_empty(), "结构化分析不应为空");

        // 被最多引用的 hub.rs 应排在最前。
        assert_eq!(
            analysis.files[0].path, "hub.rs",
            "hub.rs 应按 file_rank 排在最前,实得:{:?}",
            analysis.files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
        // file_rank 应随排序单调不增。
        for w in analysis.files.windows(2) {
            assert!(
                w[0].file_rank >= w[1].file_rank,
                "files 应按 file_rank 降序"
            );
        }
        // 每个符号带 1 基行号(>=1)与非空签名,且行号与签名口径一致。
        let hub = &analysis.files[0];
        assert!(
            hub.top_symbols.iter().any(|s| s.name == "shared"),
            "hub.rs 顶层符号应含 shared,实得:{:?}",
            hub.top_symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
        for s in &hub.top_symbols {
            assert!(s.line >= 1, "行号应为 1 基,实得 {}", s.line);
            assert!(!s.signature.is_empty(), "签名不应为空");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 结构化分析对缺失工作区应软失败:空 files、零计数,绝不 panic。
    #[test]
    fn analyze_repo_missing_workspace_is_soft_failure() {
        let analysis = analyze_repo("C:/definitely/not/here/mdga-wiki-x", &default_req());
        assert!(analysis.files.is_empty());
        assert_eq!(analysis.total_files, 0);
        assert_eq!(analysis.total_definitions, 0);
    }

    /// 二进制/资源类扩展名不应被发现阶段收录(避免把 .png 等当文本送启发式)。
    #[test]
    fn binary_extensions_are_not_scanned() {
        let dir = temp_workspace();
        write(&dir, "real.rs", "pub fn keep_me() {}\n");
        // 一个伪 png:扩展名在排除名单里,即便内容是文本也不该进地图。
        write(&dir, "asset.png", "function shouldNotAppear() {}\n");
        let result = build_repo_map(dir.to_str().unwrap(), &default_req());
        assert_eq!(
            result.total_files, 1,
            ".png 应被发现阶段排除,只剩 real.rs,实得 {} 个文件",
            result.total_files
        );
        assert!(
            !result.map.contains("shouldNotAppear"),
            "排除扩展名的内容不应出现在地图,实得:\n{}",
            result.map
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
