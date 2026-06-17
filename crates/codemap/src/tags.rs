//! 用 tree-sitter 从单个源文件提取「定义 + 引用」标签，按 mtime 做进程内缓存。
//!
//! 求真原则：解析失败 / 查询编译失败 / 非 UTF-8 / 过大文件一律降级为「空标签」而非 panic，
//! 让某种语言或某个文件的问题不至于拖垮整张仓库地图。

use crate::lang;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor};

/// 单条定义标签。
#[derive(Debug, Clone)]
pub struct Def {
    /// 符号名（节点文本）。
    pub name: String,
    /// 定义名所在行（0 基）。
    pub line: usize,
    /// 该行去空白后的源文本（签名行），渲染地图时展示，已截断到上限。已含 fn/struct/class
    /// 等关键字，故种类信息隐含其中、无需单列。
    pub sig: String,
}

/// 一个文件解析出的全部标签。
#[derive(Debug, Default)]
pub struct FileTags {
    pub defs: Vec<Def>,
    /// 引用到的标识符名（按出现次数重复，便于上层统计权重）。
    pub refs: Vec<String>,
}

/// 单个源行渲染上限（避免超长行撑爆地图）。
const MAX_SIG_LEN: usize = 160;
/// 解析的单文件字节上限（超过视为非源码/生成物，跳过）。
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// 读取并解析文件，返回其标签（命中 mtime 缓存则直接复用）。
/// 任何失败都返回空标签（Arc 复用，避免重复分配）。
pub fn tags_for_file(abs_path: &Path) -> Arc<FileTags> {
    let meta = match std::fs::metadata(abs_path) {
        Ok(m) if m.is_file() => m,
        _ => return empty_tags(),
    };
    if meta.len() > MAX_FILE_BYTES {
        return empty_tags();
    }
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let len = meta.len();

    let cache = tags_cache();
    if let Some(entry) = cache.lock().unwrap().get(abs_path) {
        if entry.mtime == mtime && entry.len == len {
            return Arc::clone(&entry.tags);
        }
    }

    let tags = Arc::new(parse_file(abs_path));
    cache.lock().unwrap().insert(
        abs_path.to_path_buf(),
        CacheEntry {
            mtime,
            len,
            tags: Arc::clone(&tags),
        },
    );
    tags
}

fn parse_file(abs_path: &Path) -> FileTags {
    let ext = abs_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let Some(def) = lang::lang_for_extension(&ext) else {
        return FileTags::default();
    };
    let Some(compiled) = compiled_query(def) else {
        return FileTags::default();
    };
    let Ok(source) = std::fs::read_to_string(abs_path) else {
        return FileTags::default();
    };

    let mut parser = Parser::new();
    if parser.set_language(&compiled.language).is_err() {
        return FileTags::default();
    }
    let Some(tree) = parser.parse(&source, None) else {
        return FileTags::default();
    };

    let lines: Vec<&str> = source.lines().collect();
    let bytes = source.as_bytes();
    let capture_names = compiled.query.capture_names();

    let mut defs: Vec<Def> = Vec::new();
    let mut def_bytes: HashSet<usize> = HashSet::new();
    // 引用先暂存 (name, start_byte)，待收集完全部定义后再剔除「定义自身位置」的引用。
    let mut raw_refs: Vec<(String, usize)> = Vec::new();

    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(&compiled.query, tree.root_node(), bytes);
    while let Some(m) = it.next() {
        for cap in m.captures {
            let cname = capture_names[cap.index as usize];
            let node = cap.node;
            let Ok(text) = node.utf8_text(bytes) else {
                continue;
            };
            if cname.starts_with("def.") {
                let row = node.start_position().row;
                let sig = lines
                    .get(row)
                    .map(|l| truncate_sig(l.trim()))
                    .unwrap_or_default();
                def_bytes.insert(node.start_byte());
                defs.push(Def {
                    name: text.to_string(),
                    line: row,
                    sig,
                });
            } else if cname == "ref" {
                raw_refs.push((text.to_string(), node.start_byte()));
            }
        }
    }

    let refs = raw_refs
        .into_iter()
        .filter(|(_, sb)| !def_bytes.contains(sb))
        .map(|(name, _)| name)
        .collect();

    FileTags { defs, refs }
}

fn truncate_sig(s: &str) -> String {
    if s.chars().count() <= MAX_SIG_LEN {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX_SIG_LEN).collect();
    out.push('…');
    out
}

// ── 编译后查询缓存（按语言名）────────────────────────────────────────────────

struct Compiled {
    language: Language,
    query: Query,
}

/// 按语言名缓存编译后查询；None 表示该语言查询与 grammar 不兼容、已降级跳过。
type QueryCacheMap = HashMap<&'static str, Option<Arc<Compiled>>>;

fn compiled_query(def: &'static lang::LangDef) -> Option<Arc<Compiled>> {
    static CACHE: OnceLock<Mutex<QueryCacheMap>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap();
    if let Some(slot) = guard.get(def.name) {
        return slot.clone();
    }
    let language = (def.language)();
    let compiled = match Query::new(&language, def.query) {
        Ok(query) => Some(Arc::new(Compiled { language, query })),
        Err(_) => None, // 查询与该 grammar 版本不兼容：降级跳过该语言。
    };
    guard.insert(def.name, compiled.clone());
    compiled
}

// ── 文件标签缓存（按 mtime+len）──────────────────────────────────────────────

struct CacheEntry {
    mtime: SystemTime,
    len: u64,
    tags: Arc<FileTags>,
}

fn tags_cache() -> &'static Mutex<HashMap<PathBuf, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn empty_tags() -> Arc<FileTags> {
    static EMPTY: OnceLock<Arc<FileTags>> = OnceLock::new();
    Arc::clone(EMPTY.get_or_init(|| Arc::new(FileTags::default())))
}
