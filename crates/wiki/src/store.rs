//! wiki 的持久化层：把区段写成 .mdga/wiki/ 下的 markdown + JSONL，并支持指纹增量与回读。
//!
//! 布局（全部在工作区内的 .mdga/wiki/ 下，派生数据、可随时重建）：
//!   - `index.jsonl`：每行一个区段的结构化 JSON（机器可读，供 query 回读、供模型精确消费）。
//!   - `<sanitized-dir>.md`：每个目录一个 markdown 区段（人/模型可读全文）。
//!   - `.fingerprint`：当前 wiki 内容指纹，用于增量跳过。
//!
//! 安全：所有 .md 文件名都经 sanitize::dir_to_doc_stem → 单层安全名，绝不逃出 wiki 目录。
//! 写入只发生在 .mdga/wiki/ 内；失败一律返回 Err 让上层优雅放弃，绝不触碰用户源码。

use crate::sanitize::dir_to_doc_stem;
use crate::sections::WikiSection;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::Path;

const INDEX_FILE: &str = "index.jsonl";
const FINGERPRINT_FILE: &str = ".fingerprint";

/// 计算一批区段的内容指纹（稳定：依赖区段的稳定序列化，与磁盘无关）。
pub fn fingerprint(sections: &[WikiSection]) -> String {
    let mut hasher = DefaultHasher::new();
    // 版本前缀：未来若改变序列化口径，指纹自动失配触发重建。
    "wiki-v1".hash(&mut hasher);
    sections.len().hash(&mut hasher);
    for s in sections {
        // 用确定性 JSON 串参与哈希（serde 对我们的结构是字段定义序，确定）。
        if let Ok(j) = serde_json::to_string(s) {
            j.hash(&mut hasher);
        }
    }
    format!("{:016x}", hasher.finish())
}

/// 当前磁盘上的指纹是否与给定指纹一致（缺失/读失败都视为不一致 → 需要重写）。
pub fn fingerprint_matches(wiki_dir: &Path, fp: &str) -> bool {
    match fs::read_to_string(wiki_dir.join(FINGERPRINT_FILE)) {
        Ok(existing) => existing.trim() == fp,
        Err(_) => false,
    }
}

/// 给定目录路径，返回其 markdown 区段文件的**工作区相对**路径（供 query 回灌给模型）。
pub fn section_doc_rel(directory: &str) -> String {
    format!("{}/{}.md", crate::WIKI_DIR, dir_to_doc_stem(directory))
}

/// 全量写出 wiki：清空旧 .md/index，再写新区段 + 指纹。任一步失败返回 Err。
pub fn write_all(wiki_dir: &Path, sections: &[WikiSection], fp: &str) -> std::io::Result<()> {
    // 确保 .mdga/wiki/ 存在。
    fs::create_dir_all(wiki_dir)?;

    // 清理旧的派生 .md（避免删掉某目录后其陈旧区段文件残留）。仅删本目录内的 .md 与 index/指纹。
    if let Ok(entries) = fs::read_dir(wiki_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let is_md = path.extension().map(|e| e == "md").unwrap_or(false);
            if is_md {
                let _ = fs::remove_file(&path);
            }
        }
    }

    // 写 index.jsonl（每行一个区段）。先写临时再 rename，避免半截文件。
    let mut index_buf = String::new();
    for s in sections {
        let line = serde_json::to_string(s)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        index_buf.push_str(&line);
        index_buf.push('\n');
    }
    write_atomic(&wiki_dir.join(INDEX_FILE), index_buf.as_bytes())?;

    // 每个区段一个 markdown 文件。
    for s in sections {
        let stem = dir_to_doc_stem(&s.directory);
        let md_path = wiki_dir.join(format!("{stem}.md"));
        // 二次校验：拼出的路径必须仍在 wiki_dir 内（纵深防御，sanitize 已保证单层名）。
        if md_path.parent() != Some(wiki_dir) {
            continue;
        }
        write_atomic(&md_path, render_markdown(s).as_bytes())?;
    }

    // 最后写指纹（成功落盘后才标记一致）。
    write_atomic(&wiki_dir.join(FINGERPRINT_FILE), fp.as_bytes())?;
    Ok(())
}

/// 从 index.jsonl 回读区段（供 query）。文件缺失/任一行解析失败都返回该错误，让上层降级。
pub fn load_sections(wiki_dir: &Path) -> std::io::Result<Vec<WikiSection>> {
    let content = fs::read_to_string(wiki_dir.join(INDEX_FILE))?;
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<WikiSection>(line) {
            Ok(s) => out.push(s),
            // 单行损坏：跳过该行而非整体失败，最大化可用性。
            Err(_) => continue,
        }
    }
    Ok(out)
}

/// 原子写：写到同目录临时文件再 rename，避免并发/中断读到半截内容。
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "目标路径无父目录")
    })?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("wiki")
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
    }
    // Windows 上 rename 覆盖已存在目标会失败，先删旧目标。
    let _ = fs::remove_file(path);
    fs::rename(&tmp, path)
}

/// 把一个区段渲染成 markdown 全文。
fn render_markdown(s: &WikiSection) -> String {
    let mut out = String::new();
    let title = if s.directory == "." {
        "(repository root)"
    } else {
        s.directory.as_str()
    };
    out.push_str(&format!("# {title}\n\n"));
    out.push_str(&format!("**Role:** {}\n\n", s.role));
    out.push_str(&format!("**Files in this directory:** {}\n\n", s.file_count));

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
            // markdown 表格转义：把 | 换成 \| ，去掉换行。
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sections::WikiSymbol;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp_dir() -> std::path::PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let id = N.fetch_add(1, Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("mdga-wiki-store-{}-{}", std::process::id(), id));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn section(dir: &str) -> WikiSection {
        WikiSection {
            directory: dir.to_string(),
            role: "core engine".to_string(),
            file_count: 1,
            key_files: vec![format!("{dir}/a.rs")],
            symbols: vec![WikiSymbol {
                name: "run".to_string(),
                file: format!("{dir}/a.rs"),
                line: 3,
                signature: "pub fn run()".to_string(),
            }],
        }
    }

    #[test]
    fn write_then_load_roundtrips() {
        let d = tmp_dir();
        let secs = vec![section("src/core"), section("src/api")];
        let fp = fingerprint(&secs);
        write_all(&d, &secs, &fp).unwrap();

        assert!(d.join("index.jsonl").is_file());
        assert!(d.join("src__core.md").is_file());
        assert!(d.join("src__api.md").is_file());
        assert!(fingerprint_matches(&d, &fp), "写后指纹应匹配");

        let loaded = load_sections(&d).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded, secs, "回读区段应与写入一致");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn rewrite_removes_stale_md() {
        let d = tmp_dir();
        write_all(&d, &[section("src/old")], &fingerprint(&[section("src/old")])).unwrap();
        assert!(d.join("src__old.md").is_file());
        // 重写为不含 old 的集合 → old 的 .md 应被清掉。
        let next = vec![section("src/new")];
        write_all(&d, &next, &fingerprint(&next)).unwrap();
        assert!(!d.join("src__old.md").exists(), "陈旧 .md 应被清理");
        assert!(d.join("src__new.md").is_file());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn fingerprint_is_stable_and_sensitive() {
        let a = vec![section("src/core")];
        let b = vec![section("src/core")];
        assert_eq!(fingerprint(&a), fingerprint(&b), "同内容指纹应一致");
        let c = vec![section("src/other")];
        assert_ne!(fingerprint(&a), fingerprint(&c), "不同内容指纹应不同");
    }

    #[test]
    fn section_doc_rel_uses_sanitized_name() {
        assert_eq!(section_doc_rel("src/api"), ".mdga/wiki/src__api.md");
        assert_eq!(section_doc_rel("."), ".mdga/wiki/_root.md");
    }
}
