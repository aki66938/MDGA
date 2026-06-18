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
