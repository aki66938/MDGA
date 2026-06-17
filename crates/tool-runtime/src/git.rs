//! Git 原生工具（R4）：把 git 包成结构化工具，而非 run_command 裸跑字符串。
//!
//! 实现方式：壳调系统 `git` CLI，使用机器可读格式（`--porcelain=v2`、`--format` + `-z`、
//! `--numstat`）跑命令，再把输出解析成结构化结果。比 libgit2 绑定免去 Windows 上 vendored
//! C 构建依赖；「结构化」由本模块的解析层保证，回传给模型的是结构化对象而非裸字符串。
//!
//! v1 范围：本地 git——git_status / git_diff / git_log / git_branch / git_add / git_commit。
//! 远端（push）与 PR（gh）留作后续增量。
//!
//! 安全：以参数向量调用 `git`（不经 shell，无注入面）；路径参数复用 tool-runtime 的
//! `validate_relative_path` 做工作区内校验；`scrub_secret_env` 擦除子进程密钥；
//! `GIT_TERMINAL_PROMPT=0` 杜绝凭据交互挂死（本地操作也作防御）。

use crate::ToolRuntimeError;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// git 命令超时（本地操作，给大仓库留足余量；网络操作不在 v1）。
const GIT_TIMEOUT_SECS: u64 = 60;

// ── 请求类型 ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusRequest {}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffRequest {
    /// 对比范围：`unstaged`（默认，工作区 vs 暂存区）| `staged`（暂存区 vs HEAD）| `all`（工作区 vs HEAD）。
    #[serde(default)]
    pub mode: Option<String>,
    /// 可选：限定到某个工作区内相对路径（文件或目录）。
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitLogRequest {
    /// 返回的提交数上限（默认 20，上限 200）。
    #[serde(default)]
    pub max_count: Option<usize>,
    /// 可选：仅看影响某相对路径的提交。
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchRequest {
    /// 动作：`list`（默认，列出分支）| `create`（新建并切换）| `switch`（切到已有分支）。
    #[serde(default)]
    pub action: Option<String>,
    /// create / switch 所需的分支名。
    #[serde(default)]
    pub name: Option<String>,
    /// list 时是否包含远端跟踪分支（默认仅本地）。
    #[serde(default)]
    pub include_remote: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitAddRequest {
    /// 要暂存的工作区相对路径列表。
    #[serde(default)]
    pub paths: Vec<String>,
    /// 为 true 时暂存所有改动（`git add -A`），忽略 paths。
    #[serde(default)]
    pub all: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitRequest {
    /// 提交信息（必填，非空）。
    pub message: String,
    /// 为 true 时先暂存已跟踪文件的改动再提交（`git commit -a`），不含未跟踪文件。
    #[serde(default)]
    pub all: bool,
}

// ── 结果类型 ────────────────────────────────────────────────────────────────

/// 单个文件的状态变更条目（status 为 git porcelain 单字符码：M/A/D/R/C/T 等）。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileChange {
    pub status: String,
    pub path: String,
    /// 重命名/复制时的原路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orig_path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusResult {
    /// 当前分支名；detached HEAD 时为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// 上游分支（如 origin/main），无则 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    /// 领先上游的提交数。
    pub ahead: i64,
    /// 落后上游的提交数。
    pub behind: i64,
    /// 已暂存（index）的变更。
    pub staged: Vec<GitFileChange>,
    /// 未暂存（工作区）的变更。
    pub unstaged: Vec<GitFileChange>,
    /// 未跟踪文件。
    pub untracked: Vec<String>,
    /// 冲突（未合并）文件。
    pub conflicts: Vec<String>,
    /// 工作区是否干净（无任何变更）。
    pub clean: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffFile {
    pub path: String,
    /// 新增行数；二进制文件为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additions: Option<u64>,
    /// 删除行数；二进制文件为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deletions: Option<u64>,
    pub binary: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffResult {
    /// 实际生效的对比范围。
    pub mode: String,
    /// 逐文件增删统计。
    pub files: Vec<GitDiffFile>,
    /// 统一 diff 文本（可能因体积上限截断）。
    pub patch: String,
    /// patch 是否被截断。
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommit {
    pub hash: String,
    pub short: String,
    pub author: String,
    pub email: String,
    /// 作者日期（ISO 8601）。
    pub date: String,
    pub subject: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitLogResult {
    pub commits: Vec<GitCommit>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchEntry {
    pub name: String,
    pub current: bool,
    pub remote: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchResult {
    /// 实际执行的动作。
    pub action: String,
    /// 当前所在分支。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// list 动作返回的分支列表（create/switch 时为空）。
    pub branches: Vec<GitBranchEntry>,
    /// 人读说明（create/switch 时填）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitAddResult {
    /// 是否使用了 -A（全部暂存）。
    pub all: bool,
    /// 本次请求暂存的路径（all=true 时为空）。
    pub requested: Vec<String>,
    /// 当前暂存区内的全部文件（add 之后的实际状态）。
    pub staged: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitResult {
    pub commit: String,
    pub short: String,
    pub message: String,
    /// git 提交后的概要输出（人读）。
    pub summary: String,
}

// ── 命令执行底座 ──────────────────────────────────────────────────────────────

struct GitOutput {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    truncated: bool,
}

/// 在工作区目录下以参数向量调用 `git`（不经 shell），抽干管道、带超时。
fn run_git(workspace: &Path, args: &[&str]) -> Result<GitOutput, ToolRuntimeError> {
    let mut builder = Command::new("git");
    builder
        .args(args)
        .current_dir(workspace)
        // 防御：绝不因凭据/编辑器交互而挂死（本地操作也保险）。
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // 与 run_command 一致：从子进程环境擦除密钥，git 子进程不应拿到 API Key 等。
    crate::scrub_secret_env(&mut builder);

    let mut child = builder.spawn().map_err(|e| {
        ToolRuntimeError::CommandFailed(format!(
            "无法启动 git（请确认已安装 git 且在 PATH 中）: {e}"
        ))
    })?;

    // 独立线程抽干 stdout/stderr，避免管道缓冲填满死锁（复用 run_command 的 drain 实现，64K 截断）。
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let out_handle = std::thread::spawn(move || crate::drain_pipe_streaming(stdout_pipe, None));
    let err_handle = std::thread::spawn(move || crate::drain_pipe_streaming(stderr_pipe, None));

    let started = Instant::now();
    let timeout = Duration::from_secs(GIT_TIMEOUT_SECS);
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    timed_out = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(ToolRuntimeError::CommandFailed(e.to_string())),
        }
    }
    let status = child
        .wait()
        .map_err(|e| ToolRuntimeError::CommandFailed(e.to_string()))?;
    let (stdout, truncated) = out_handle.join().unwrap_or((String::new(), false));
    let (stderr, _) = err_handle.join().unwrap_or((String::new(), false));

    if timed_out {
        return Err(ToolRuntimeError::CommandFailed(format!(
            "git 命令超时（>{GIT_TIMEOUT_SECS}s）: git {}",
            args.join(" ")
        )));
    }
    Ok(GitOutput {
        code: status.code(),
        stdout,
        stderr,
        truncated,
    })
}

/// 跑 git 并要求退出码为 0，否则把 stderr（或 stdout）作为清晰错误回传给模型。
fn run_git_checked(workspace: &Path, args: &[&str]) -> Result<GitOutput, ToolRuntimeError> {
    let out = run_git(workspace, args)?;
    if out.code != Some(0) {
        let msg = if !out.stderr.trim().is_empty() {
            out.stderr.trim().to_string()
        } else if !out.stdout.trim().is_empty() {
            out.stdout.trim().to_string()
        } else {
            format!("git 命令失败（退出码 {:?}）", out.code)
        };
        return Err(ToolRuntimeError::CommandFailed(msg));
    }
    Ok(out)
}

/// 校验并规整一个工作区内相对路径，转为 git 可用的正斜杠形式（拒绝绝对路径 / `..` 逃逸）。
fn safe_git_relpath(path: &str) -> Result<String, ToolRuntimeError> {
    let rel = crate::validate_relative_path(path)?;
    if rel.as_os_str().is_empty() {
        return Err(ToolRuntimeError::EmptyPath);
    }
    Ok(crate::normalize_relative_path(&rel))
}

/// 校验分支名：非空、不以 `-` 开头（避免被当作 git 选项）、不含空白；其余交给 git 自身的 ref 校验。
fn valid_branch_name(name: &str) -> Result<String, ToolRuntimeError> {
    let n = name.trim();
    if n.is_empty() {
        return Err(ToolRuntimeError::CommandFailed("分支名不能为空".to_string()));
    }
    if n.starts_with('-') {
        return Err(ToolRuntimeError::CommandFailed(
            "分支名不能以 - 开头".to_string(),
        ));
    }
    if n.chars().any(char::is_whitespace) {
        return Err(ToolRuntimeError::CommandFailed(
            "分支名不能包含空白字符".to_string(),
        ));
    }
    Ok(n.to_string())
}

// ── 工具实现 ────────────────────────────────────────────────────────────────

/// git_status：结构化工作区状态（分支/上游/领先落后 + 暂存/未暂存/未跟踪/冲突）。
pub fn git_status(
    workspace_root: impl AsRef<Path>,
    _request: GitStatusRequest,
) -> Result<GitStatusResult, ToolRuntimeError> {
    let ws = crate::canonical_workspace(workspace_root)?;
    let out = run_git_checked(&ws, &["status", "--porcelain=v2", "--branch"])?;
    let mut res = GitStatusResult::default();

    for line in out.stdout.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            res.branch = if rest == "(detached)" {
                None
            } else {
                Some(rest.to_string())
            };
        } else if let Some(rest) = line.strip_prefix("# branch.upstream ") {
            res.upstream = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            let mut it = rest.split_whitespace();
            if let Some(a) = it.next() {
                res.ahead = a.trim_start_matches('+').parse().unwrap_or(0);
            }
            if let Some(b) = it.next() {
                res.behind = b.trim_start_matches('-').parse().unwrap_or(0);
            }
        } else if let Some(rest) = line.strip_prefix("1 ") {
            // 普通变更：<xy> <sub> <mH> <mI> <mW> <hH> <hI> <path>
            let fields: Vec<&str> = rest.splitn(8, ' ').collect();
            if fields.len() == 8 {
                push_change(fields[0], fields[7].to_string(), None, &mut res);
            }
        } else if let Some(rest) = line.strip_prefix("2 ") {
            // 重命名/复制：... <Xscore> <path>\t<origPath>
            let fields: Vec<&str> = rest.splitn(9, ' ').collect();
            if fields.len() == 9 {
                let (path, orig) = match fields[8].split_once('\t') {
                    Some((p, o)) => (p.to_string(), Some(o.to_string())),
                    None => (fields[8].to_string(), None),
                };
                push_change(fields[0], path, orig, &mut res);
            }
        } else if let Some(rest) = line.strip_prefix("u ") {
            // 未合并（冲突）：<xy> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>
            let fields: Vec<&str> = rest.splitn(10, ' ').collect();
            if fields.len() == 10 {
                res.conflicts.push(fields[9].to_string());
            }
        } else if let Some(rest) = line.strip_prefix("? ") {
            res.untracked.push(rest.to_string());
        }
        // "! " 忽略项不回传。
    }

    res.clean = res.staged.is_empty()
        && res.unstaged.is_empty()
        && res.untracked.is_empty()
        && res.conflicts.is_empty();
    Ok(res)
}

/// 解析 porcelain v2 的 XY 状态码并归入暂存/未暂存（一个文件可同时落两边）。
fn push_change(xy: &str, path: String, orig: Option<String>, res: &mut GitStatusResult) {
    let mut chars = xy.chars();
    let x = chars.next().unwrap_or('.');
    let y = chars.next().unwrap_or('.');
    if x != '.' {
        res.staged.push(GitFileChange {
            status: x.to_string(),
            path: path.clone(),
            orig_path: orig.clone(),
        });
    }
    if y != '.' {
        res.unstaged.push(GitFileChange {
            status: y.to_string(),
            path,
            orig_path: orig,
        });
    }
}

/// git_diff：返回逐文件增删统计 + 统一 diff 文本。
pub fn git_diff(
    workspace_root: impl AsRef<Path>,
    request: GitDiffRequest,
) -> Result<GitDiffResult, ToolRuntimeError> {
    let ws = crate::canonical_workspace(workspace_root)?;
    let mode = match request.mode.as_deref() {
        Some("staged") | Some("cached") => "staged",
        Some("all") | Some("head") | Some("HEAD") => "all",
        _ => "unstaged",
    };
    let base: &[&str] = match mode {
        "staged" => &["diff", "--staged"],
        "all" => &["diff", "HEAD"],
        _ => &["diff"],
    };
    let path = match request.path.as_deref() {
        Some(p) => Some(safe_git_relpath(p)?),
        None => None,
    };

    // numstat：逐文件 新增\t删除\t路径（二进制为 -\t-）。
    let mut numstat_args: Vec<&str> = base.to_vec();
    numstat_args.push("--numstat");
    if let Some(p) = path.as_deref() {
        numstat_args.push("--");
        numstat_args.push(p);
    }
    let numstat = run_git_checked(&ws, &numstat_args)?;
    let mut files = Vec::new();
    for line in numstat.stdout.lines() {
        let mut it = line.splitn(3, '\t');
        let (a, d, p) = match (it.next(), it.next(), it.next()) {
            (Some(a), Some(d), Some(p)) => (a, d, p),
            _ => continue,
        };
        let binary = a == "-" || d == "-";
        files.push(GitDiffFile {
            path: p.to_string(),
            additions: if binary { None } else { a.parse().ok() },
            deletions: if binary { None } else { d.parse().ok() },
            binary,
        });
    }

    // patch 文本。
    let mut patch_args: Vec<&str> = base.to_vec();
    if let Some(p) = path.as_deref() {
        patch_args.push("--");
        patch_args.push(p);
    }
    let patch = run_git_checked(&ws, &patch_args)?;

    Ok(GitDiffResult {
        mode: mode.to_string(),
        files,
        patch: patch.stdout,
        truncated: patch.truncated,
    })
}

/// git_log：结构化提交历史。
pub fn git_log(
    workspace_root: impl AsRef<Path>,
    request: GitLogRequest,
) -> Result<GitLogResult, ToolRuntimeError> {
    let ws = crate::canonical_workspace(workspace_root)?;
    let n = request.max_count.unwrap_or(20).clamp(1, 200);
    let max = format!("--max-count={n}");
    // 字段用单元分隔符 \x1f，记录用 -z 的 NUL 分隔，避免与提交信息中的字符冲突。
    let mut args: Vec<&str> = vec![
        "log",
        "-z",
        &max,
        "--format=%H%x1f%h%x1f%an%x1f%ae%x1f%aI%x1f%s",
    ];
    let path = match request.path.as_deref() {
        Some(p) => Some(safe_git_relpath(p)?),
        None => None,
    };
    if let Some(p) = path.as_deref() {
        args.push("--");
        args.push(p);
    }
    let out = run_git_checked(&ws, &args)?;

    let mut commits = Vec::new();
    for record in out.stdout.split('\0') {
        if record.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = record.split('\u{1f}').collect();
        if f.len() < 6 {
            continue; // 截断造成的残缺记录跳过
        }
        commits.push(GitCommit {
            hash: f[0].trim().to_string(),
            short: f[1].trim().to_string(),
            author: f[2].to_string(),
            email: f[3].to_string(),
            date: f[4].trim().to_string(),
            subject: f[5].to_string(),
        });
    }
    Ok(GitLogResult { commits })
}

/// git_branch：list（默认）/ create（新建并切换）/ switch（切到已有分支）。
pub fn git_branch(
    workspace_root: impl AsRef<Path>,
    request: GitBranchRequest,
) -> Result<GitBranchResult, ToolRuntimeError> {
    let ws = crate::canonical_workspace(workspace_root)?;
    let action = request.action.as_deref().unwrap_or("list");

    match action {
        "list" => {
            // 注意：git branch 的 ref-filter 格式不解释 `%x1f`（会原样输出），与 git log 的
            // pretty 格式不同；改用 tab（%09，git 禁止分支名含 tab）作分隔符。
            let mut args: Vec<&str> = vec![
                "branch",
                "--list",
                "--format=%(HEAD)%09%(refname)%09%(refname:short)",
            ];
            if request.include_remote {
                args.push("-a");
            }
            let out = run_git_checked(&ws, &args)?;
            let mut branches = Vec::new();
            let mut current = None;
            for line in out.stdout.lines() {
                let f: Vec<&str> = line.split('\t').collect();
                if f.len() < 3 {
                    continue;
                }
                let is_current = f[0].trim() == "*";
                let remote = f[1].starts_with("refs/remotes/");
                let name = f[2].to_string();
                if is_current {
                    current = Some(name.clone());
                }
                branches.push(GitBranchEntry {
                    name,
                    current: is_current,
                    remote,
                });
            }
            Ok(GitBranchResult {
                action: "list".to_string(),
                current,
                branches,
                note: None,
            })
        }
        "create" => {
            let name = valid_branch_name(
                request
                    .name
                    .as_deref()
                    .ok_or_else(|| ToolRuntimeError::CommandFailed("create 需要 name".to_string()))?,
            )?;
            run_git_checked(&ws, &["switch", "-c", &name])?;
            Ok(GitBranchResult {
                action: "create".to_string(),
                current: Some(name.clone()),
                branches: Vec::new(),
                note: Some(format!("已创建并切换到分支 {name}")),
            })
        }
        "switch" => {
            let name = valid_branch_name(
                request
                    .name
                    .as_deref()
                    .ok_or_else(|| ToolRuntimeError::CommandFailed("switch 需要 name".to_string()))?,
            )?;
            run_git_checked(&ws, &["switch", &name])?;
            Ok(GitBranchResult {
                action: "switch".to_string(),
                current: Some(name.clone()),
                branches: Vec::new(),
                note: Some(format!("已切换到分支 {name}")),
            })
        }
        other => Err(ToolRuntimeError::CommandFailed(format!(
            "未知 git_branch 动作: {other}（应为 list/create/switch）"
        ))),
    }
}

/// git_add：暂存指定路径或全部改动。
pub fn git_add(
    workspace_root: impl AsRef<Path>,
    request: GitAddRequest,
) -> Result<GitAddResult, ToolRuntimeError> {
    let ws = crate::canonical_workspace(workspace_root)?;

    if request.all {
        run_git_checked(&ws, &["add", "-A"])?;
    } else {
        if request.paths.is_empty() {
            return Err(ToolRuntimeError::CommandFailed(
                "git_add 需要 paths（非空）或 all=true".to_string(),
            ));
        }
        let mut safe_paths = Vec::with_capacity(request.paths.len());
        for p in &request.paths {
            safe_paths.push(safe_git_relpath(p)?);
        }
        let mut args: Vec<&str> = vec!["add", "--"];
        for p in &safe_paths {
            args.push(p);
        }
        run_git_checked(&ws, &args)?;
    }

    // add 之后回读当前暂存区，给模型确定性的结果。
    let staged_out = run_git_checked(&ws, &["diff", "--cached", "--name-only", "-z"])?;
    let staged: Vec<String> = staged_out
        .stdout
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    Ok(GitAddResult {
        all: request.all,
        requested: if request.all {
            Vec::new()
        } else {
            request.paths
        },
        staged,
    })
}

/// git_commit：提交已暂存改动（可选 -a 先暂存已跟踪改动）。
pub fn git_commit(
    workspace_root: impl AsRef<Path>,
    request: GitCommitRequest,
) -> Result<GitCommitResult, ToolRuntimeError> {
    let ws = crate::canonical_workspace(workspace_root)?;
    let message = request.message.trim();
    if message.is_empty() {
        return Err(ToolRuntimeError::CommandFailed(
            "提交信息不能为空".to_string(),
        ));
    }

    let mut args: Vec<&str> = vec!["commit"];
    if request.all {
        args.push("-a");
    }
    args.push("-m");
    args.push(message);
    let commit_out = run_git_checked(&ws, &args)?;

    // 取新提交的完整与短哈希。
    let full = run_git_checked(&ws, &["rev-parse", "HEAD"])?
        .stdout
        .trim()
        .to_string();
    let short = run_git_checked(&ws, &["rev-parse", "--short", "HEAD"])?
        .stdout
        .trim()
        .to_string();

    Ok(GitCommitResult {
        commit: full,
        short,
        message: message.to_string(),
        summary: commit_out.stdout.trim().to_string(),
    })
}

// ── 单元测试（纯解析逻辑，不依赖系统 git） ────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_name_validation() {
        assert!(valid_branch_name("feature/x").is_ok());
        assert_eq!(valid_branch_name("  main  ").unwrap(), "main");
        assert!(valid_branch_name("").is_err());
        assert!(valid_branch_name("   ").is_err());
        assert!(valid_branch_name("-rf").is_err());
        assert!(valid_branch_name("has space").is_err());
    }

    #[test]
    fn safe_relpath_rejects_escape() {
        assert!(safe_git_relpath("../etc/passwd").is_err());
        assert!(safe_git_relpath("/abs").is_err());
        assert!(safe_git_relpath("").is_err());
        assert_eq!(safe_git_relpath("src/main.rs").unwrap(), "src/main.rs");
        // 反斜杠分量在 Windows 上会被规整为正斜杠输出。
        assert!(!safe_git_relpath("src/lib.rs").unwrap().contains('\\'));
    }

    #[test]
    fn status_parse_ordinary_and_branch() {
        let sample = "\
# branch.oid abc123
# branch.head main
# branch.upstream origin/main
# branch.ab +2 -1
1 M. N... 100644 100644 100644 aaa bbb staged_only.rs
1 .M N... 100644 100644 100644 aaa bbb unstaged_only.rs
1 MM N... 100644 100644 100644 aaa bbb both.rs
? new_file.txt
";
        let mut res = GitStatusResult::default();
        for line in sample.lines() {
            if let Some(rest) = line.strip_prefix("# branch.head ") {
                res.branch = Some(rest.to_string());
            } else if let Some(rest) = line.strip_prefix("# branch.upstream ") {
                res.upstream = Some(rest.to_string());
            } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
                let mut it = rest.split_whitespace();
                res.ahead = it.next().unwrap().trim_start_matches('+').parse().unwrap();
                res.behind = it.next().unwrap().trim_start_matches('-').parse().unwrap();
            } else if let Some(rest) = line.strip_prefix("1 ") {
                let fields: Vec<&str> = rest.splitn(8, ' ').collect();
                push_change(fields[0], fields[7].to_string(), None, &mut res);
            } else if let Some(rest) = line.strip_prefix("? ") {
                res.untracked.push(rest.to_string());
            }
        }
        assert_eq!(res.branch.as_deref(), Some("main"));
        assert_eq!(res.upstream.as_deref(), Some("origin/main"));
        assert_eq!(res.ahead, 2);
        assert_eq!(res.behind, 1);
        // staged: staged_only + both = 2；unstaged: unstaged_only + both = 2
        assert_eq!(res.staged.len(), 2);
        assert_eq!(res.unstaged.len(), 2);
        assert_eq!(res.untracked, vec!["new_file.txt".to_string()]);
    }

    #[test]
    fn status_parse_rename() {
        // 2 R. ... <score> <new>\t<old>
        let line = "2 R. N... 100644 100644 100644 aaa bbb R100 new_name.rs\told_name.rs";
        let mut res = GitStatusResult::default();
        let rest = line.strip_prefix("2 ").unwrap();
        let fields: Vec<&str> = rest.splitn(9, ' ').collect();
        assert_eq!(fields.len(), 9);
        let (path, orig) = match fields[8].split_once('\t') {
            Some((p, o)) => (p.to_string(), Some(o.to_string())),
            None => (fields[8].to_string(), None),
        };
        push_change(fields[0], path, orig, &mut res);
        assert_eq!(res.staged.len(), 1);
        assert_eq!(res.staged[0].status, "R");
        assert_eq!(res.staged[0].path, "new_name.rs");
        assert_eq!(res.staged[0].orig_path.as_deref(), Some("old_name.rs"));
    }

    #[test]
    fn log_parse_z_records() {
        let us = '\u{1f}';
        let stdout = format!(
            "h1{us}s1{us}Alice{us}a@x.com{us}2026-01-01T00:00:00+00:00{us}first commit\0h2{us}s2{us}Bob{us}b@x.com{us}2026-01-02T00:00:00+00:00{us}second\0"
        );
        let mut commits = Vec::new();
        for record in stdout.split('\0') {
            if record.trim().is_empty() {
                continue;
            }
            let f: Vec<&str> = record.split('\u{1f}').collect();
            if f.len() < 6 {
                continue;
            }
            commits.push(GitCommit {
                hash: f[0].to_string(),
                short: f[1].to_string(),
                author: f[2].to_string(),
                email: f[3].to_string(),
                date: f[4].to_string(),
                subject: f[5].to_string(),
            });
        }
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].hash, "h1");
        assert_eq!(commits[0].subject, "first commit");
        assert_eq!(commits[1].author, "Bob");
    }
}
