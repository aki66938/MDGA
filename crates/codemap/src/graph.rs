//! 引用图 + 个性化 PageRank（对标 Aider repo-map 的排名思路，纯整内存，无外部依赖）。
//!
//! 思路：把「文件」当节点，「A 文件引用了 B 文件里定义的符号」当有向边（权重∝引用次数）。
//! 重要文件 = 被许多重要文件引用的文件。再把每个文件的 rank 沿出边按权重摊回到「被引用的
//! 那个定义」上 → 得到「定义级」重要度，供渲染挑选最值得展示的符号。
//!
//! 个性化：focus 文件与 query 命中的符号会抬高 teleport 概率，让地图围绕当前关注点收敛。

use crate::tags::FileTags;
use std::collections::HashMap;

/// 一个标识符被太多文件定义则视为过于通用（如 new/len/get），不建边以免噪声主导排名。
const MAX_DEFINERS: usize = 10;
const DAMPING: f64 = 0.85;
const MAX_ITERS: usize = 100;
const CONVERGENCE: f64 = 1e-6;

/// PageRank 排名结果。
pub struct GraphRanks {
    /// 每个文件（按传入顺序索引）的 PageRank 分。
    pub file_rank: Vec<f64>,
    /// 定义级重要度：(文件索引, 符号名) → 流入该定义的 rank。
    pub def_score: HashMap<(usize, String), f64>,
}

struct EdgeRec {
    src: usize,
    dst: usize,
    ident: String,
    w: f64,
}

/// 基于各文件标签构图并跑 PageRank。
///
/// - `file_tags`：与文件索引一一对应的标签。
/// - `focus`：被关注的文件索引（个性化 teleport 集中于此）。
/// - `mentioned`：query 解析出的关注符号名（命中的符号边权放大、其定义文件抬高 teleport）。
pub fn rank(
    file_tags: &[FileTags],
    focus: &[usize],
    mentioned: &std::collections::HashSet<String>,
) -> GraphRanks {
    let n = file_tags.len();
    if n == 0 {
        return GraphRanks {
            file_rank: Vec::new(),
            def_score: HashMap::new(),
        };
    }

    // defines[ident] = 定义该符号的文件集合；references[ident][file] = 引用次数。
    let mut defines: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut references: HashMap<&str, HashMap<usize, usize>> = HashMap::new();
    for (i, tags) in file_tags.iter().enumerate() {
        for d in &tags.defs {
            defines.entry(d.name.as_str()).or_default().push(i);
        }
        for r in &tags.refs {
            *references
                .entry(r.as_str())
                .or_default()
                .entry(i)
                .or_insert(0) += 1;
        }
    }

    // 建边：仅对「既被定义又被引用」的符号；跳过过于通用的符号与自引用。
    let mut edges: Vec<EdgeRec> = Vec::new();
    for (ident, refers) in &references {
        let Some(definers) = defines.get(ident) else {
            continue;
        };
        if definers.is_empty() || definers.len() > MAX_DEFINERS {
            continue;
        }
        let mul = if mentioned.contains(*ident) {
            10.0
        } else if ident.starts_with('_') {
            0.1
        } else {
            1.0
        };
        for (&src, &count) in refers {
            let w = (count as f64).sqrt() * mul;
            for &dst in definers {
                if src == dst {
                    continue;
                }
                edges.push(EdgeRec {
                    src,
                    dst,
                    ident: (*ident).to_string(),
                    w,
                });
            }
        }
    }

    // 出边聚合与出权和。
    let mut adjacency: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    let mut out_sum = vec![0.0f64; n];
    {
        let mut agg: HashMap<(usize, usize), f64> = HashMap::new();
        for e in &edges {
            *agg.entry((e.src, e.dst)).or_insert(0.0) += e.w;
        }
        for ((src, dst), w) in agg {
            adjacency[src].push((dst, w));
            out_sum[src] += w;
        }
    }

    // 个性化 teleport 向量。
    let mut p = vec![0.0f64; n];
    for &f in focus {
        if f < n {
            p[f] += 1.0;
        }
    }
    for ident in mentioned {
        if let Some(definers) = defines.get(ident.as_str()) {
            for &d in definers {
                p[d] += 1.0;
            }
        }
    }
    let p_sum: f64 = p.iter().sum();
    if p_sum > 0.0 {
        for v in &mut p {
            *v /= p_sum;
        }
    } else {
        for v in &mut p {
            *v = 1.0 / n as f64;
        }
    }

    // 幂迭代。
    let mut rank = vec![1.0 / n as f64; n];
    for _ in 0..MAX_ITERS {
        let mut next = vec![0.0f64; n];
        let mut dangling = 0.0;
        for i in 0..n {
            if out_sum[i] == 0.0 {
                dangling += rank[i];
                continue;
            }
            let ri = rank[i];
            let denom = out_sum[i];
            for &(j, w) in &adjacency[i] {
                next[j] += DAMPING * ri * (w / denom);
            }
        }
        let redistribute = (1.0 - DAMPING) + DAMPING * dangling;
        for j in 0..n {
            next[j] += redistribute * p[j];
        }
        let delta: f64 = (0..n).map(|i| (next[i] - rank[i]).abs()).sum();
        rank = next;
        if delta < CONVERGENCE {
            break;
        }
    }

    // 把文件 rank 沿出边按权重摊回到被引用的定义上。
    let mut def_score: HashMap<(usize, String), f64> = HashMap::new();
    for e in &edges {
        if out_sum[e.src] == 0.0 {
            continue;
        }
        let contribution = rank[e.src] * e.w / out_sum[e.src];
        *def_score
            .entry((e.dst, e.ident.clone()))
            .or_insert(0.0) += contribution;
    }

    GraphRanks {
        file_rank: rank,
        def_score,
    }
}
