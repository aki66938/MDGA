//! R4 端到端冒烟测试：在临时仓库里真跑 git_* 工具（壳调真实 git CLI + 解析），
//! 覆盖 init→add→commit→status→log→branch→diff 全链路。需系统 git；缺失则跳过。

use mdga_tool_runtime::{
    git_add, git_branch, git_commit, git_diff, git_log, git_status, GitAddRequest,
    GitBranchRequest, GitCommitRequest, GitDiffRequest, GitLogRequest, GitStatusRequest,
};
use std::path::{Path, PathBuf};
use std::process::Command;

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 在指定目录下跑一条 raw git（仅供测试搭建仓库用），失败即 panic。
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

fn unique_tmp() -> PathBuf {
    let mut dir = std::env::temp_dir();
    // 进程 id + 纳秒时间戳拼唯一目录名（测试内允许用时间）。
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.push(format!("mdga_git_smoke_{}_{}", std::process::id(), stamp));
    std::fs::create_dir_all(&dir).expect("create tmp dir");
    dir
}

#[test]
fn git_tools_end_to_end() {
    if !git_available() {
        eprintln!("跳过：系统未安装 git");
        return;
    }
    let dir = unique_tmp();

    // 搭建仓库：固定默认分支为 main，配置本地身份，关闭签名避免交互。
    raw_git(&dir, &["init", "-b", "main"]);
    raw_git(&dir, &["config", "user.email", "smoke@test.local"]);
    raw_git(&dir, &["config", "user.name", "Smoke Test"]);
    raw_git(&dir, &["config", "commit.gpgsign", "false"]);

    std::fs::write(dir.join("a.txt"), "hello\n").expect("write a.txt");

    // 1) status：未跟踪 a.txt、分支 main、不干净。
    let st = git_status(&dir, GitStatusRequest::default()).expect("status");
    assert_eq!(st.branch.as_deref(), Some("main"));
    assert!(!st.clean);
    assert!(st.untracked.iter().any(|p| p == "a.txt"), "应含未跟踪 a.txt: {st:?}");

    // 2) add：暂存 a.txt。
    let added = git_add(
        &dir,
        GitAddRequest {
            paths: vec!["a.txt".to_string()],
            all: false,
        },
    )
    .expect("add");
    assert!(added.staged.iter().any(|p| p == "a.txt"), "应暂存 a.txt: {added:?}");

    // 3) commit：返回 40 位哈希与非空短哈希。
    let c = git_commit(
        &dir,
        GitCommitRequest {
            message: "first commit".to_string(),
            all: false,
        },
    )
    .expect("commit");
    assert_eq!(c.commit.len(), 40, "完整哈希应 40 位: {}", c.commit);
    assert!(!c.short.is_empty());
    assert_eq!(c.message, "first commit");

    // 提交后工作区应干净。
    let st2 = git_status(&dir, GitStatusRequest::default()).expect("status2");
    assert!(st2.clean, "提交后应干净: {st2:?}");

    // 4) log：一条提交，subject 对得上。
    let log = git_log(&dir, GitLogRequest::default()).expect("log");
    assert_eq!(log.commits.len(), 1);
    assert_eq!(log.commits[0].subject, "first commit");
    assert_eq!(log.commits[0].author, "Smoke Test");
    assert_eq!(log.commits[0].hash, c.commit);

    // 5) branch create：新建并切换到 dev。
    let br = git_branch(
        &dir,
        GitBranchRequest {
            action: Some("create".to_string()),
            name: Some("dev".to_string()),
            include_remote: false,
        },
    )
    .expect("branch create");
    assert_eq!(br.current.as_deref(), Some("dev"));
    let st3 = git_status(&dir, GitStatusRequest::default()).expect("status3");
    assert_eq!(st3.branch.as_deref(), Some("dev"));

    // 6) branch list：含 main 与 dev，当前为 dev。
    let list = git_branch(
        &dir,
        GitBranchRequest {
            action: Some("list".to_string()),
            ..Default::default()
        },
    )
    .expect("branch list");
    assert!(list.branches.iter().any(|b| b.name == "main"));
    assert!(list
        .branches
        .iter()
        .any(|b| b.name == "dev" && b.current));

    // 7) diff：改动 a.txt 后未暂存 diff 应含该文件且有新增行。
    std::fs::write(dir.join("a.txt"), "hello\nworld\n").expect("modify a.txt");
    let diff = git_diff(
        &dir,
        GitDiffRequest {
            mode: Some("unstaged".to_string()),
            path: None,
        },
    )
    .expect("diff");
    assert_eq!(diff.mode, "unstaged");
    let f = diff
        .files
        .iter()
        .find(|f| f.path == "a.txt")
        .expect("diff 应含 a.txt");
    assert_eq!(f.additions, Some(1));
    assert!(diff.patch.contains("a.txt"), "patch 文本应提到 a.txt");

    // 8) 路径逃逸防护：越界路径被拒。
    assert!(git_diff(
        &dir,
        GitDiffRequest {
            mode: None,
            path: Some("../escape".to_string()),
        }
    )
    .is_err());

    // 清理（best-effort）。
    let _ = std::fs::remove_dir_all(&dir);
}
