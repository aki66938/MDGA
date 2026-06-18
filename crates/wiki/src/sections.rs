//! 把 codemap 的「每文件结构化分析」聚合成「每目录 wiki 区段」。
//!
//! 一个区段 = 一个目录的结构化摘要：关键文件（按 file_rank）、顶层符号（跨该目录文件汇总后
//! 按重要度挑选）、以及结构性推断的目录角色。完全确定性、无 LLM。

use crate::role;
use mdga_codemap::FileAnalysis;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 区段里展示的关键文件数上限。
const MAX_KEY_FILES: usize = 8;
/// 区段里展示的顶层符号数上限。
const MAX_SECTION_SYMBOLS: usize = 24;

/// wiki 里的一个符号条目（区段级，跨该目录文件汇总）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WikiSymbol {
    /// 符号名。
    pub name: String,
    /// 定义所在文件（工作区相对路径，正斜杠）。
    pub file: String,
    /// 定义所在行（1 基）。
    pub line: usize,
    /// 签名行（去空白源文本）。
    pub signature: String,
}

/// 一个目录的 wiki 区段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WikiSection {
    /// 目录（工作区相对路径，正斜杠；仓库根为 "."）。
    pub directory: String,
    /// 结构性推断的目录角色（非 LLM）。
    pub role: String,
    /// 该目录纳入分析的源文件数。
    pub file_count: usize,
    /// 关键文件（按 file_rank 降序，已截断）。
    pub key_files: Vec<String>,
    /// 顶层符号（按重要度降序，已截断）。
    pub symbols: Vec<WikiSymbol>,
    /// 可选的 LLM 散文摘要（P3：opt-in enrich）。**纯附加**：
    /// `None` 时本字段不参与序列化（`skip_serializing_if`），区段的 JSON/markdown 与
    /// 0.0.57 逐字节一致；只有用户显式 enrich 才会被填充。它**不参与内容指纹**，
    /// 故指纹仍只表征确定性结构事实（见 `store::fingerprint`），enrich 不影响增量幂等。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// 取一个工作区相对路径的父目录（正斜杠；根目录文件返回 "."）。
fn parent_dir(path: &str) -> String {
    match path.rfind('/') {
        Some(idx) => path[..idx].to_string(),
        None => ".".to_string(),
    }
}

/// 把每文件分析按目录聚合成区段。结果按目录路径升序（确定性、稳定指纹）。
pub fn group_into_sections(files: &[FileAnalysis]) -> Vec<WikiSection> {
    // 目录 → (该目录文件按 file_rank 降序的列表)。用 BTreeMap 保证遍历顺序确定。
    let mut by_dir: BTreeMap<String, Vec<&FileAnalysis>> = BTreeMap::new();
    for f in files {
        by_dir.entry(parent_dir(&f.path)).or_default().push(f);
    }

    let mut sections: Vec<WikiSection> = Vec::with_capacity(by_dir.len());
    for (dir, mut dir_files) in by_dir {
        // 文件按 file_rank 降序、平局按路径升序。
        dir_files.sort_by(|a, b| {
            b.file_rank
                .partial_cmp(&a.file_rank)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.path.cmp(&b.path))
        });

        let file_count = dir_files.len();
        let key_files: Vec<String> = dir_files
            .iter()
            .take(MAX_KEY_FILES)
            .map(|f| f.path.clone())
            .collect();

        // 跨该目录所有文件汇总符号，按定义级 score 降序挑选最重要的若干个。
        let mut symbols: Vec<(f64, WikiSymbol)> = Vec::new();
        for f in &dir_files {
            for s in &f.top_symbols {
                symbols.push((
                    s.score,
                    WikiSymbol {
                        name: s.name.clone(),
                        file: f.path.clone(),
                        line: s.line,
                        signature: s.signature.clone(),
                    },
                ));
            }
        }
        symbols.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                // 平局：按文件、再按行号，保证确定性。
                .then_with(|| a.1.file.cmp(&b.1.file))
                .then_with(|| a.1.line.cmp(&b.1.line))
        });
        let symbols: Vec<WikiSymbol> = symbols
            .into_iter()
            .map(|(_, s)| s)
            .take(MAX_SECTION_SYMBOLS)
            .collect();

        let role = role::infer_role(&dir, &dir_files, &symbols);

        sections.push(WikiSection {
            directory: dir,
            role,
            file_count,
            key_files,
            symbols,
            // 确定性结构区段不带摘要；enrich 是后续的纯附加步骤（见 lib::build_wiki_enriched）。
            summary: None,
        });
    }

    // 已由 BTreeMap 保证按目录升序;显式再排一次以防未来改动。
    sections.sort_by(|a, b| a.directory.cmp(&b.directory));
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdga_codemap::SymbolEntry;

    fn fa(path: &str, rank: f64, syms: &[(&str, usize, f64)]) -> FileAnalysis {
        FileAnalysis {
            path: path.to_string(),
            file_rank: rank,
            definition_count: syms.len(),
            top_symbols: syms
                .iter()
                .map(|(n, l, sc)| SymbolEntry {
                    name: n.to_string(),
                    line: *l,
                    signature: format!("pub fn {n}()"),
                    score: *sc,
                })
                .collect(),
        }
    }

    #[test]
    fn groups_files_by_directory() {
        let files = vec![
            fa("src/core/a.rs", 0.5, &[("alpha", 1, 0.9)]),
            fa("src/core/b.rs", 0.3, &[("beta", 2, 0.4)]),
            fa("src/api/h.rs", 0.2, &[("handle", 3, 0.1)]),
        ];
        let sections = group_into_sections(&files);
        assert_eq!(sections.len(), 2, "应聚成 src/core 与 src/api 两个区段");
        // 升序：src/api 在前。
        assert_eq!(sections[0].directory, "src/api");
        assert_eq!(sections[1].directory, "src/core");
        let core = &sections[1];
        assert_eq!(core.file_count, 2);
        // a.rs（rank 高）应排 key_files 首位。
        assert_eq!(core.key_files[0], "src/core/a.rs");
        // 符号按 score 降序：alpha(0.9) 在 beta(0.4) 前。
        assert_eq!(core.symbols[0].name, "alpha");
    }

    #[test]
    fn root_files_use_dot_directory() {
        let files = vec![fa("main.rs", 0.5, &[("main", 1, 0.9)])];
        let sections = group_into_sections(&files);
        assert_eq!(sections[0].directory, ".");
    }
}
