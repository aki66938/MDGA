//! R10 端到端测试：在临时 git 仓库里真跑 worktree 隔离原语。需系统 git；缺失则跳过。
//!
//! 覆盖：
//! 1) 创建隔离工作树 + 临时分支，在其中写文件并提交（改动只落隔离目录/分支）。
//! 2) 干净合并回父分支（用一个签出目标分支的辅助工作树执行 merge，不污染主仓库索引）。
//! 3) 冲突合并：冲突被**结构化抛出**（MergeOutcome::Conflict + 路径），父工作树被 abort 还原干净，
//!    绝不静默强解。
//! 4) preview_merge 只读预览：干净/冲突两种都不触碰任何工作树。
//! 5) RAII/Drop 清理：守卫离开作用域后，工作树目录被移除、临时分支被删除——错误路径下也清理。

use mdga_tool_runtime::{IsolatedWorktree, MergeOutcome};
use std::path::{Path, PathBuf};
use std::process::Command;

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 跑一条 raw git（仅供测试搭建/校验仓库），失败即 panic。回传 stdout。
fn raw_git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-c")
        .arg("core.autocrlf=false")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} 失败: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// raw git 但不要求成功（用于校验「分支已不存在」之类的负向断言）。
fn raw_git_status(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-c")
        .arg("core.autocrlf=false")
        .args(args)
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn unique_tmp(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.push(format!(
        "mdga_wt_iso_{}_{}_{}",
        tag,
        std::process::id(),
        stamp
    ));
    std::fs::create_dir_all(&dir).expect("create tmp dir");
    dir
}

/// 初始化一个带一条 base 提交、默认分支 main 的临时仓库；返回 canonical 仓库根。
fn init_repo(tag: &str) -> PathBuf {
    let dir = unique_tmp(tag);
    raw_git(&dir, &["init", "-b", "main"]);
    raw_git(&dir, &["config", "user.email", "iso@test.local"]);
    raw_git(&dir, &["config", "user.name", "Iso Test"]);
    raw_git(&dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("base.txt"), "base\n").expect("write base.txt");
    raw_git(&dir, &["add", "-A"]);
    raw_git(&dir, &["commit", "-m", "base"]);
    dir.canonicalize().expect("canonicalize repo root")
}

#[test]
fn create_write_commit_clean_merge_and_cleanup() {
    if !git_available() {
        eprintln!("跳过：系统未安装 git");
        return;
    }
    let repo = init_repo("clean");

    let (worktree_path, branch) = {
        let wt = IsolatedWorktree::create(&repo, "feature-x").expect("create worktree");
        let wt_path = wt.path().to_path_buf();
        let branch = wt.branch().to_string();

        // 隔离工作树目录应真实存在，且不是父仓库根本身。
        assert!(wt_path.exists(), "隔离工作树目录应存在: {wt_path:?}");
        assert_ne!(wt_path, repo, "隔离工作树不应等于父仓库根");
        assert!(branch.starts_with("mdga/subagent/"), "临时分支名前缀: {branch}");

        // 在隔离工作树里写一个新文件并提交——改动只落隔离目录/分支。
        std::fs::write(wt_path.join("feature.txt"), "isolated work\n").expect("write in worktree");
        let head = wt.commit_all("add feature.txt").expect("commit in worktree");
        assert_eq!(head.len(), 40, "提交哈希应 40 位: {head}");

        // 父仓库工作目录里**不应**出现该文件（隔离生效）。
        assert!(
            !repo.join("feature.txt").exists(),
            "隔离改动不应泄漏进父仓库工作目录"
        );

        // 预览合并：干净（main 无独立提交）。
        let preview = wt.preview_merge("main").expect("preview");
        assert!(preview.is_clean(), "预览应为干净合并: {preview:?}");

        // 真实 merge：父仓库主工作树仍签出在 main 且干净，merge_into 默认在父仓库执行。
        let outcome = wt.merge_into("main").expect("merge");
        assert!(outcome.is_clean(), "应干净合并: {outcome:?}");

        // 合并后父仓库主工作树（在 main 上）应出现 feature.txt。
        assert!(
            repo.join("feature.txt").exists(),
            "干净合并后 main 工作树应含 feature.txt"
        );
        // main 分支日志应包含合并进来的提交主题。
        let log = raw_git(&repo, &["log", "--oneline", "main"]);
        assert!(log.contains("add feature.txt"), "main 应含合并提交: {log}");

        (wt_path, branch)
        // wt 在此离开作用域 → Drop 触发清理。
    };

    // Drop 后：隔离工作树目录被移除、临时分支被删除。
    assert!(
        !worktree_path.exists(),
        "Drop 后隔离工作树目录应被移除: {worktree_path:?}"
    );
    let branch_ref = format!("refs/heads/{branch}");
    assert!(
        !raw_git_status(&repo, &["rev-parse", "--verify", &branch_ref]),
        "Drop 后临时分支应已删除: {branch}"
    );
    // worktree list 不应再登记该路径。
    let list = raw_git(&repo, &["worktree", "list"]);
    assert!(
        !list.contains(&worktree_path.to_string_lossy().replace('\\', "/")),
        "worktree list 不应再含已清理路径: {list}"
    );

    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn conflicting_merge_is_surfaced_not_silently_resolved() {
    if !git_available() {
        eprintln!("跳过：系统未安装 git");
        return;
    }
    let repo = init_repo("conflict");

    // 先在 base 上拉隔离工作树（隔离分支与 main 此刻同基）。
    let wt = IsolatedWorktree::create(&repo, "conflicting").expect("create worktree");
    // 隔离分支把 base.txt 改成 from-subagent 并提交。
    std::fs::write(wt.path().join("base.txt"), "from-subagent\n").expect("sub edit");
    wt.commit_all("subagent edit base.txt").expect("commit");

    // 创建之后，父侧在 main 上独立把同一行改成不同内容并提交——二者真正分叉、必冲突。
    std::fs::write(repo.join("base.txt"), "from-parent\n").expect("parent edit");
    raw_git(&repo, &["commit", "-am", "parent edit base.txt"]);

    // 预览应报冲突，且不触碰任何工作树。
    let preview = wt.preview_merge("main").expect("preview");
    match &preview {
        MergeOutcome::Conflict { paths } => {
            assert!(
                paths.iter().any(|p| p == "base.txt"),
                "预览冲突路径应含 base.txt: {paths:?}"
            );
        }
        other => panic!("预览应为冲突，实际: {other:?}"),
    }
    // 预览是只读的：父工作树/索引不应被它弄脏。
    let pre_status = raw_git(&repo, &["status", "--porcelain"]);
    assert!(
        pre_status.trim().is_empty(),
        "preview_merge 不应弄脏父工作树，实际: {pre_status:?}"
    );

    // 真实合并到父仓库 main，应抛出 Conflict，且 abort 后父工作树干净。
    let outcome = wt.merge_into("main").expect("merge call");
    match &outcome {
        MergeOutcome::Conflict { paths } => {
            assert!(
                paths.iter().any(|p| p == "base.txt"),
                "合并冲突路径应含 base.txt: {paths:?}"
            );
        }
        other => panic!("应抛出冲突（绝不静默强解），实际: {other:?}"),
    }

    // 关键安全断言：冲突后父工作树被 abort 还原干净——无未合并条目、无半合并状态。
    let status = raw_git(&repo, &["status", "--porcelain"]);
    assert!(
        status.trim().is_empty(),
        "冲突 abort 后工作树应干净（无半合并/无冲突标记），实际: {status:?}"
    );
    // base.txt 内容应仍是 main 原内容，未被任一侧静默覆盖。
    let content = std::fs::read_to_string(repo.join("base.txt")).expect("read base.txt");
    assert_eq!(content, "from-parent\n", "冲突不应静默选边覆盖父侧内容");
    // MERGE_HEAD 不应残留（merge --abort 已清）。
    assert!(
        !raw_git_status(&repo, &["rev-parse", "--verify", "MERGE_HEAD"]),
        "abort 后不应残留 MERGE_HEAD"
    );

    drop(wt);
    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn cleanup_runs_on_error_path_via_drop() {
    if !git_available() {
        eprintln!("跳过：系统未安装 git");
        return;
    }
    let repo = init_repo("errpath");

    let leaked_path;
    let leaked_branch;
    {
        let wt = IsolatedWorktree::create(&repo, "panicky").expect("create");
        leaked_path = wt.path().to_path_buf();
        leaked_branch = wt.branch().to_string();
        assert!(leaked_path.exists());

        // 模拟「子代理中途出错」——写了点东西但不提交，随即让守卫在错误路径上被丢弃。
        std::fs::write(leaked_path.join("half.txt"), "partial\n").expect("write");

        // 模拟一个会提前 return 的错误路径：这里直接 drop(wt) 代表函数因错误返回时的栈展开清理。
        // （即便有未提交改动，worktree remove --force 也应强制清掉。）
        drop(wt);
    }

    // 错误路径下也清理干净：工作树目录移除 + 临时分支删除，未提交改动不致泄漏。
    assert!(
        !leaked_path.exists(),
        "错误路径 Drop 后工作树目录仍应被移除: {leaked_path:?}"
    );
    let branch_ref = format!("refs/heads/{leaked_branch}");
    assert!(
        !raw_git_status(&repo, &["rev-parse", "--verify", &branch_ref]),
        "错误路径 Drop 后临时分支应已删除: {leaked_branch}"
    );

    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn explicit_cleanup_is_idempotent_and_drop_is_safe() {
    if !git_available() {
        eprintln!("跳过：系统未安装 git");
        return;
    }
    let repo = init_repo("idem");

    let mut wt = IsolatedWorktree::create(&repo, "idem").expect("create");
    let path = wt.path().to_path_buf();
    // 显式清理一次：成功。
    wt.cleanup().expect("explicit cleanup");
    assert!(!path.exists(), "显式清理后目录应移除");
    // 再次显式清理：幂等成功（不报错）。
    wt.cleanup().expect("idempotent cleanup");
    // 随后 Drop 也不应 panic 或报错（cleaned 标记已置位）。
    drop(wt);

    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn create_rejects_non_git_directory() {
    if !git_available() {
        eprintln!("跳过：系统未安装 git");
        return;
    }
    // 一个普通（非 git）临时目录：create 应失败而非误建工作树。
    let plain = unique_tmp("nonrepo");
    let res = IsolatedWorktree::create(&plain, "x");
    assert!(res.is_err(), "在非 git 目录创建隔离工作树应失败");
    std::fs::remove_dir_all(&plain).ok();
}

// ── P1（0.0.58）：并行编排器的 git 语义端到端测试 ──────────────────────────────────
//
// 编排器（apps/desktop subagent.rs::run_parallel_write_subtasks）依赖 LLM 与 AppHandle，无法在
// 纯 crate 测试里真跑子代理 loop。但它的**全部 git 不变量**——多个隔离工作树同基创建、各写各的、
// 串行合并回父分支、首冲突即停、父工作树保持干净、全部 RAII 清理——都只依赖本 crate 的
// `IsolatedWorktree` 原语。下面这些测试**精确复刻编排器的 git 操作序列**（用直接写文件替代子代理
// 的产出），从而在没有 LLM 的前提下覆盖编排器的关键安全行为。

/// 三个隔离工作树写**不相交**文件 => 串行合并全部干净，父分支拿到三方改动；全部 RAII 清理。
#[test]
fn parallel_disjoint_worktrees_all_merge_clean_and_cleanup() {
    if !git_available() {
        eprintln!("跳过：系统未安装 git");
        return;
    }
    let repo = init_repo("par_disjoint");

    let mut paths = Vec::new();
    let mut branches = Vec::new();
    {
        // 三个工作树都在同一 base HEAD 上创建（模拟并发 fan-out）。
        let mut guards = Vec::new();
        for (i, fname) in ["a.txt", "b.txt", "c.txt"].iter().enumerate() {
            let wt = IsolatedWorktree::create(&repo, &format!("disjoint-{i}"))
                .expect("create worktree");
            paths.push(wt.path().to_path_buf());
            branches.push(wt.branch().to_string());
            // 每个写一个**不同**的新文件并提交（互不相交）。
            std::fs::write(wt.path().join(fname), format!("content {i}\n")).expect("write");
            wt.commit_all(&format!("add {fname}")).expect("commit");
            guards.push(wt);
        }

        // 串行合并回 main（编排器的合并阶段）：三者都应干净。
        for (i, wt) in guards.iter().enumerate() {
            let outcome = wt.merge_into("main").expect("merge call");
            assert!(outcome.is_clean(), "第 {i} 个不相交合并应干净: {outcome:?}");
        }

        // 父仓库 main 工作树应同时含三个文件。
        for fname in ["a.txt", "b.txt", "c.txt"] {
            assert!(
                repo.join(fname).exists(),
                "干净并行合并后 main 应含 {fname}"
            );
        }
        // guards 在此离开作用域 → 三个工作树/分支全部 RAII 清理。
    }

    // 全部清理：目录移除 + 临时分支删除。
    for p in &paths {
        assert!(!p.exists(), "Drop 后隔离工作树目录应移除: {p:?}");
    }
    for b in &branches {
        let branch_ref = format!("refs/heads/{b}");
        assert!(
            !raw_git_status(&repo, &["rev-parse", "--verify", &branch_ref]),
            "Drop 后临时分支应删除: {b}"
        );
    }
    // 父工作树干净、无残留合并状态。
    let status = raw_git(&repo, &["status", "--porcelain"]);
    assert!(status.trim().is_empty(), "并行合并后父工作树应干净: {status:?}");

    std::fs::remove_dir_all(&repo).ok();
}

/// 两个隔离工作树写**同一文件**且内容冲突 => 第一个干净合并，第二个上报 Conflict；父工作树被
/// abort 还原干净（无 MERGE_HEAD、无半合并）；绝不强解/选边。复刻编排器「首冲突即停」语义。
#[test]
fn parallel_conflicting_worktrees_first_merges_second_surfaces_conflict() {
    if !git_available() {
        eprintln!("跳过：系统未安装 git");
        return;
    }
    let repo = init_repo("par_conflict");

    // 两个工作树同基创建，都改同一个已存在文件 base.txt 的同一行为不同内容。
    let wt1 = IsolatedWorktree::create(&repo, "conf-1").expect("create wt1");
    std::fs::write(wt1.path().join("base.txt"), "from-agent-1\n").expect("w1");
    wt1.commit_all("agent1 edit base.txt").expect("commit1");

    let wt2 = IsolatedWorktree::create(&repo, "conf-2").expect("create wt2");
    std::fs::write(wt2.path().join("base.txt"), "from-agent-2\n").expect("w2");
    wt2.commit_all("agent2 edit base.txt").expect("commit2");

    let wt2_branch = wt2.branch().to_string();

    // 串行合并：第一个干净（main 此刻无独立改动）。
    let o1 = wt1.merge_into("main").expect("merge1");
    assert!(o1.is_clean(), "第一个并行分支应干净合并: {o1:?}");
    assert_eq!(
        std::fs::read_to_string(repo.join("base.txt")).unwrap(),
        "from-agent-1\n",
        "第一个合并后 main 应为 agent-1 内容"
    );

    // 第二个改了同一行、与已并入的 agent-1 冲突 => 应上报 Conflict 而非静默选边。
    let o2 = wt2.merge_into("main").expect("merge2 call");
    match &o2 {
        MergeOutcome::Conflict { paths } => {
            assert!(
                paths.iter().any(|p| p == "base.txt"),
                "第二个并行分支冲突路径应含 base.txt: {paths:?}"
            );
        }
        other => panic!("第二个并行分支应上报冲突（编排器据此停止），实际: {other:?}"),
    }

    // 关键安全断言：冲突 abort 后父工作树干净、内容仍是已干净并入的 agent-1（未被 agent-2 静默覆盖）。
    let status = raw_git(&repo, &["status", "--porcelain"]);
    assert!(
        status.trim().is_empty(),
        "冲突 abort 后父工作树应干净（无半合并）: {status:?}"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("base.txt")).unwrap(),
        "from-agent-1\n",
        "冲突不应静默覆盖：base.txt 应仍为已合并的 agent-1 内容"
    );
    assert!(
        !raw_git_status(&repo, &["rev-parse", "--verify", "MERGE_HEAD"]),
        "abort 后不应残留 MERGE_HEAD"
    );

    // 编排器在冲突分支上 mem::forget 守卫以保留分支；这里模拟「保留」：分支此刻仍存在。
    assert!(
        raw_git_status(&repo, &["rev-parse", "--verify", &format!("refs/heads/{wt2_branch}")]),
        "冲突保留阶段，第二个隔离分支应仍存在供人工处理: {wt2_branch}"
    );

    // 清理：wt1 Drop 清理；wt2 这里显式清理（编排器实战里 forget 后由用户/上层处置；测试负责收尾不泄漏）。
    drop(wt1);
    drop(wt2);
    std::fs::remove_dir_all(&repo).ok();
}

/// 冲突/错误路径上，剩余未合并的隔离工作树仍被 RAII 清理（不因提前停止而泄漏）。
#[test]
fn parallel_remaining_worktrees_cleaned_on_conflict_path() {
    if !git_available() {
        eprintln!("跳过：系统未安装 git");
        return;
    }
    let repo = init_repo("par_remain");

    let path_b;
    let branch_b;
    {
        // 两个工作树：第一个会冲突，第二个根本没轮到合并（编排器停止后保留分支但 RAII 仍兜底）。
        let wt_a = IsolatedWorktree::create(&repo, "remain-a").expect("create a");
        std::fs::write(wt_a.path().join("base.txt"), "a-side\n").expect("wa");
        wt_a.commit_all("a edits base").expect("ca");

        let wt_b = IsolatedWorktree::create(&repo, "remain-b").expect("create b");
        path_b = wt_b.path().to_path_buf();
        branch_b = wt_b.branch().to_string();
        std::fs::write(wt_b.path().join("other.txt"), "b-side\n").expect("wb");
        wt_b.commit_all("b adds other").expect("cb");

        // 父侧 main 也独立改 base.txt 制造与 wt_a 的真冲突。
        std::fs::write(repo.join("base.txt"), "parent-side\n").expect("wp");
        raw_git(&repo, &["commit", "-am", "parent edits base"]);

        let oa = wt_a.merge_into("main").expect("merge a");
        assert!(oa.has_conflict(), "wt_a 应冲突: {oa:?}");
        // 父工作树被 abort 还原干净。
        let status = raw_git(&repo, &["status", "--porcelain"]);
        assert!(status.trim().is_empty(), "冲突后父工作树应干净: {status:?}");

        // 编排器此处会停止，不再合并 wt_b——wt_b 守卫仍在作用域内，离开时 RAII 清理。
        drop(wt_a);
        // wt_b 在此块结束时 Drop。
    }

    // 即便从未合并，wt_b 的工作树/分支也被清理掉（不泄漏）。
    assert!(!path_b.exists(), "未合并的剩余工作树也应被 Drop 清理: {path_b:?}");
    assert!(
        !raw_git_status(&repo, &["rev-parse", "--verify", &format!("refs/heads/{branch_b}")]),
        "未合并的剩余临时分支也应被删除: {branch_b}"
    );

    std::fs::remove_dir_all(&repo).ok();
}
