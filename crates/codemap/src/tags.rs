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

/// 单个源行渲染上限（避免超长行撑爆地图）。供启发式回退共用同一口径。
pub(crate) const MAX_SIG_LEN: usize = 160;
/// 解析的单文件字节上限（超过视为非源码/生成物，跳过）。
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// 进程内文件标签缓存的条目上限。超过即整表清空（粗粒度淘汰），
/// 避免长驻进程在反复扫不同仓库 / 大量文件后无界膨胀。
/// 条目均经 mtime+len 校验，清空仅丢失复用、不影响正确性。
const MAX_CACHE_ENTRIES: usize = 4000;

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
    {
        let mut guard = cache.lock().unwrap();
        // 插入前若已达上限，整表清空再插入新条目（粗粒度但 O(1) 摊销、实现简单）。
        // 缓存仅作复用加速，丢弃后下次会按 mtime+len 重新解析，语义不变。
        if guard.len() >= MAX_CACHE_ENTRIES {
            guard.clear();
        }
        guard.insert(
            abs_path.to_path_buf(),
            CacheEntry {
                mtime,
                len,
                tags: Arc::clone(&tags),
            },
        );
    }
    tags
}

fn parse_file(abs_path: &Path) -> FileTags {
    let ext = abs_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let Some(def) = lang::lang_for_extension(&ext) else {
        // 无 tree-sitter grammar 的扩展名：走通用启发式回退，让每个文本文件都贡献粗粒度符号。
        // 非 UTF-8 / 二进制由 heuristic::extract 内部判空，读取失败则返回空标签。
        return match std::fs::read_to_string(abs_path) {
            Ok(source) => crate::heuristic::extract(&source),
            Err(_) => FileTags::default(),
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 缓存达上限后再插入会整表清空,验证其不会无界膨胀:
    /// 写 MAX_CACHE_ENTRIES+2 个真实小文件并依次解析,缓存条目数应始终 ≤ 上限。
    /// 注意 tags_cache 是进程全局的——本测试单独控制条目数,故先快照、用专属临时目录,
    /// 并在断言后清理,避免与其他测试相互污染。
    #[test]
    fn tags_cache_is_bounded() {
        static N: AtomicUsize = AtomicUsize::new(0);
        let id = N.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "mdga-codemap-cache-test-{}-{}",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 先清空缓存,确保从已知状态出发(其他测试可能已往全局缓存塞过条目)。
        tags_cache().lock().unwrap().clear();

        let n = MAX_CACHE_ENTRIES + 2;
        for i in 0..n {
            let p = dir.join(format!("f{i}.rs"));
            std::fs::write(&p, format!("pub fn f{i}() {{}}\n")).unwrap();
            let _ = tags_for_file(&p);
            let size = tags_cache().lock().unwrap().len();
            assert!(
                size <= MAX_CACHE_ENTRIES,
                "缓存条目数应始终 ≤ {MAX_CACHE_ENTRIES},第 {i} 次插入后实得 {size}"
            );
        }
        // 越过上限后必然发生过至少一次清空,最终条目数应远小于已解析的文件总数。
        let final_size = tags_cache().lock().unwrap().len();
        assert!(
            final_size < n,
            "越过上限后应已淘汰过条目,实得 {final_size} 不应等于全部 {n} 个文件"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
