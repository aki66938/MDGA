//! P4 端到端测试：把此前临时验证过的 git_push / git_pr 正式落库为提交内测试。
//!
//! 两块互不依赖：
//!   1) `git_push` 全离线跑通——临时工作仓 + `git init --bare` 本地裸远端，做一次真实推送，
//!      断言 Ok、结果带 branch，且裸远端上的分支 SHA == 工作仓 HEAD。纯本地、零网络、确定性，
//!      作为**常规（非 ignored）**测试常驻。
//!   2) `git_pr` 触达 gh——仅在 PATH 中存在 gh 时运行（缺失则优雅跳过，镜像 lsp/browser 测试的
//!      「工具缺失即跳过、绝不算失败」语义）。在一个**没有 GitHub 远端**的临时仓里调用 git_pr，
//!      断言返回的是 gh 自身吐出的错误（未认证/无远端，属预期），而**不是**库内「未安装 gh」那条
//!      缺二进制提示——以此证明 `resolve_gh_path` 找到了 gh 且 `run_gh` 确实把它拉起来了。
//!      绝不创建真实 PR、绝不触达真实 GitHub 远端。
//!
//! 需系统 git；缺失则整体跳过。默认分支名一律用 `-b main` 显式钉死，避免依赖 init.defaultBranch。

use mdga_tool_runtime::{git_pr, git_push, GitPrRequest, GitPushRequest};
use std::path::{Path, PathBuf};
use std::process::Command;

/// 系统是否有可用的 git（缺失则整体跳过，不算失败）。
fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// which 式：在 PATH 目录中解析程序名为绝对路径（Windows 追加 .exe/.cmd/.bat）。
///
/// 用于 git_pr 的可用性闸门：与库内 `resolve_gh_path` 同口径（直接 `Command::new("gh")` 在
/// Windows 上找不到 *.cmd 垫片，且 std 不查 PATHEXT），所以这里自己按 PATH 解析判定 gh 是否在场。
fn gh_available() -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    let candidates: Vec<String> = if cfg!(windows) {
        vec![
            "gh.exe".to_string(),
            "gh.cmd".to_string(),
            "gh.bat".to_string(),
            "gh".to_string(),
        ]
    } else {
        vec!["gh".to_string()]
    };
    for dir in std::env::split_paths(&path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for name in &candidates {
            if dir.join(name).is_file() {
                return true;
            }
        }
    }
    false
}

/// 在指定目录下跑一条 raw git（仅供测试搭建仓库/读裸远端用），失败即 panic。
fn raw_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
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
}

/// 跑一条 raw git 并返回去空白后的 stdout（用于读 SHA），失败即 panic。
fn raw_git_stdout(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// 造一个唯一临时目录（进程 id + 纳秒时间戳），避免并行测试互踩。
fn unique_tmp(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.push(format!(
        "mdga_push_pr_{}_{}_{}",
        tag,
        std::process::id(),
        stamp
    ));
    std::fs::create_dir_all(&dir).expect("create tmp dir");
    dir
}

/// 在 `dir` 初始化一个工作仓：钉死默认分支 main、配本地身份、关签名（避免交互挂死）。
fn init_work_repo(dir: &Path) {
    raw_git(dir, &["init", "-b", "main"]);
    raw_git(dir, &["config", "user.email", "push-pr@test.local"]);
    raw_git(dir, &["config", "user.name", "Push PR Test"]);
    raw_git(dir, &["config", "commit.gpgsign", "false"]);
}

/// 把一个本地裸远端的路径规整成 git 远端 URL 能吃的形式（Windows 反斜杠 → 正斜杠）。
fn remote_url(bare: &Path) -> String {
    bare.to_string_lossy().replace('\\', "/")
}

/// (1) git_push 全离线：工作仓 → 本地裸远端，断言推送成功且裸端分支 SHA == 工作仓 HEAD。
#[test]
fn git_push_to_local_bare_remote_offline() {
    if !git_available() {
        eprintln!("跳过：系统未安装 git");
        return;
    }

    // 裸远端：git init --bare（同样钉死 main，免得默认分支名干扰断言）。
    let bare = unique_tmp("bare");
    raw_git(&bare, &["init", "--bare", "-b", "main"]);

    // 工作仓：init + 一次种子提交。
    let work = unique_tmp("work");
    init_work_repo(&work);
    std::fs::write(work.join("a.txt"), "hello\n").expect("write a.txt");
    raw_git(&work, &["add", "a.txt"]);
    raw_git(&work, &["commit", "-m", "seed commit"]);

    // 把裸远端配成 origin（git_push 默认推 origin）。
    raw_git(&work, &["remote", "add", "origin", &remote_url(&bare)]);

    // 工作仓当前 HEAD（推送后应与裸端 main 完全一致）。
    let work_head = raw_git_stdout(&work, &["rev-parse", "HEAD"]);
    assert_eq!(work_head.len(), 40, "HEAD 应为 40 位完整哈希: {work_head}");

    // —— 被测点：库内 git_push（remote=None→origin，set_upstream=true 首推建跟踪）——
    let res = git_push(
        &work,
        GitPushRequest {
            remote: None,
            set_upstream: true,
        },
    )
    .expect("git_push 离线推送本地裸远端应成功");

    assert_eq!(res.remote, "origin", "默认远端应为 origin");
    assert_eq!(
        res.branch.as_deref(),
        Some("main"),
        "推送的分支应为 main，实际: {:?}",
        res.branch
    );
    assert!(res.set_upstream, "本次带了 --set-upstream");

    // 裸远端上 main 的 SHA 必须等于工作仓 HEAD——证明推送真落地。
    let bare_sha = raw_git_stdout(&bare, &["rev-parse", "refs/heads/main"]);
    assert_eq!(
        bare_sha, work_head,
        "裸远端 main 的 SHA 应等于工作仓 HEAD：bare={bare_sha} head={work_head}"
    );

    // 工作仓的上游应已被设为 origin/main（--set-upstream 生效）。
    let upstream = raw_git_stdout(
        &work,
        &["rev-parse", "--abbrev-ref", "main@{upstream}"],
    );
    assert_eq!(upstream, "origin/main", "上游应为 origin/main，实际: {upstream}");

    // 清理（best-effort）。
    let _ = std::fs::remove_dir_all(&work);
    let _ = std::fs::remove_dir_all(&bare);
}

/// (2) git_pr 触达 gh：gh 缺失则跳过；在场则断言返回的错误来自 gh 本身、而非「未安装 gh」缺二进制提示。
#[test]
fn git_pr_reaches_gh_when_present() {
    if !git_available() {
        eprintln!("跳过：系统未安装 git");
        return;
    }
    if !gh_available() {
        eprintln!("跳过：PATH 中未找到 gh CLI");
        return;
    }

    // 一个没有任何 GitHub 远端的临时仓：gh pr create 必然失败（无远端/未认证），但这恰能证明
    // 「gh 被找到并真的跑起来了」——失败信息应来自 gh，而非库的缺二进制提示。
    let work = unique_tmp("pr");
    init_work_repo(&work);
    std::fs::write(work.join("a.txt"), "hi\n").expect("write a.txt");
    raw_git(&work, &["add", "a.txt"]);
    raw_git(&work, &["commit", "-m", "seed for pr"]);

    let res = git_pr(
        &work,
        GitPrRequest {
            title: "test pr (should not be created)".to_string(),
            body: String::new(),
            base: None,
        },
    );

    // 预期失败（无远端/未认证），但关键在于错误**不是**库内「未安装 gh」那条缺二进制提示——
    // 出现非缺二进制错误，即证明 resolve_gh_path 找到了 gh、run_gh 把它拉起来并拿回了 gh 的输出。
    assert!(
        res.is_err(),
        "无 GitHub 远端时 git_pr 应失败（gh 报错），实际: {res:?}"
    );
    let msg = format!("{}", res.unwrap_err());
    assert!(
        !msg.contains("未安装 gh"),
        "错误应来自 gh 本身、而非库的缺二进制提示，实际: {msg}"
    );
    // 该消息里也不应出现库在「无法启动 gh」时的兜底前缀（同属未真正执行到 gh 的情形）。
    assert!(
        !msg.contains("无法启动 gh"),
        "不应是 spawn 失败，应已真正执行到 gh，实际: {msg}"
    );

    let _ = std::fs::remove_dir_all(&work);
}
