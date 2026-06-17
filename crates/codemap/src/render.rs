//! 把排名后的定义渲染成「token 预算内」的仓库地图字符串。
//!
//! 形如：
//! ```text
//! src/agent_loop.rs:
//!   42: pub async fn run_agent_loop(...) {
//!   88: pub struct AgentState {
//! crates/tool-runtime/src/lib.rs:
//!   997: pub fn code_overview(...
//! ```
//! 文件按 PageRank 分降序（重要度与 focus/query 个性化都体现在这里），文件内定义按引用流入
//! 分挑最重要的若干个、再按行号升序展示；超预算即停并标记 truncated。

use crate::graph::GraphRanks;
use crate::tags::{Def, FileTags};
use std::cmp::Ordering;

/// 单文件最多展示的定义数（防止一个大文件挤占整张地图）。
const MAX_DEFS_PER_FILE: usize = 40;
/// 地图最多包含的文件数。
const MAX_FILES_IN_MAP: usize = 80;

pub struct Rendered {
    pub map: String,
    pub files_included: usize,
    pub truncated: bool,
}

/// 渲染。`rel_paths` 与 `file_tags`、`ranks.file_rank` 同序对应。
pub fn render(
    rel_paths: &[String],
    file_tags: &[FileTags],
    ranks: &GraphRanks,
    max_tokens: usize,
) -> Rendered {
    // 文件按 PageRank 降序（平局用文件索引升序，保证确定性）；跳过无定义的文件。
    let mut file_order: Vec<usize> = (0..file_tags.len())
        .filter(|&i| !file_tags[i].defs.is_empty())
        .collect();
    file_order.sort_by(|&a, &b| {
        frank(ranks, b)
            .partial_cmp(&frank(ranks, a))
            .unwrap_or(Ordering::Equal)
            .then(a.cmp(&b))
    });

    let mut out_files: Vec<(usize, Vec<usize>)> = Vec::new();
    let mut used = 0usize;
    let mut truncated = false;

    for &fi in &file_order {
        if out_files.len() >= MAX_FILES_IN_MAP {
            truncated = true;
            break;
        }
        // 文件内定义按引用流入分降序挑选（平局按行号），再截到单文件上限。
        let mut defs: Vec<usize> = (0..file_tags[fi].defs.len()).collect();
        defs.sort_by(|&x, &y| {
            dscore(ranks, fi, &file_tags[fi].defs[x])
                .partial_cmp(&dscore(ranks, fi, &file_tags[fi].defs[y]))
                .unwrap_or(Ordering::Equal)
                .reverse()
                .then(file_tags[fi].defs[x].line.cmp(&file_tags[fi].defs[y].line))
        });
        if defs.len() > MAX_DEFS_PER_FILE {
            defs.truncate(MAX_DEFS_PER_FILE);
            truncated = true;
        }

        let header_cost = est_tokens(&format!("{}:", rel_paths[fi]));
        let mut chosen: Vec<usize> = Vec::new();
        let mut local = 0usize;
        for di in defs {
            let cost = est_tokens(&def_line(&file_tags[fi].defs[di]));
            let add_header = if chosen.is_empty() { header_cost } else { 0 };
            if used + local + cost + add_header > max_tokens {
                truncated = true;
                break;
            }
            local += cost + add_header;
            chosen.push(di);
        }
        if chosen.is_empty() {
            // 连本文件的「表头 + 一个定义」都放不下：预算耗尽。文件已按重要度排序，到此为止。
            truncated = true;
            break;
        }
        used += local;
        chosen.sort_by_key(|&di| file_tags[fi].defs[di].line);
        out_files.push((fi, chosen));
    }

    let mut lines: Vec<String> = Vec::new();
    for (fi, defs) in &out_files {
        lines.push(format!("{}:", rel_paths[*fi]));
        for &di in defs {
            lines.push(format!("  {}", def_line(&file_tags[*fi].defs[di])));
        }
    }

    Rendered {
        map: lines.join("\n"),
        files_included: out_files.len(),
        truncated,
    }
}

fn frank(ranks: &GraphRanks, i: usize) -> f64 {
    ranks.file_rank.get(i).copied().unwrap_or(0.0)
}

fn dscore(ranks: &GraphRanks, fi: usize, def: &Def) -> f64 {
    ranks
        .def_score
        .get(&(fi, def.name.clone()))
        .copied()
        .unwrap_or(0.0)
}

fn def_line(def: &Def) -> String {
    format!("{}: {}", def.line + 1, def.sig)
}

fn est_tokens(s: &str) -> usize {
    // 粗估：约 4 字符/token，至少 1。
    (s.chars().count() / 4) + 1
}
