//! R10：可写子代理的 git-worktree 隔离原语（保守、强测试、显式 opt-in）。
//!
//! 背景：今天的可写子代理与主链路共用同一个工作区，没有任何隔离，于是「并行 fan-out
//! 多个写子代理」无法安全做到——它们会互相踩对方的文件。本模块提供一个基于
//! `git worktree` 的隔离原语：为某个写子代理在当前 HEAD 上拉出一条**临时分支** + 一个
//! **独立工作树目录**，让该子代理的所有文件改动与提交都只发生在这个隔离目录里；完成后
//! 把它那条分支**合并**回父分支（**冲突一律向上抛出，绝不静默强解 / 绝不 `-X ours|theirs`
//! / 绝不 force**）；无论成功、失败还是 panic，都通过 RAII(Drop) 守卫**清理**掉工作树与
//! 临时分支，不留泄漏。
//!
//! 安全要点（与 git.rs / tool-runtime 既有约束一致）：
//! - 以参数向量调用 `git`（不经 shell，无注入面）；先把 `git` 解析成 PATH 里的**绝对路径**
//!   再派生（绕开 Windows「cwd 优先」的可执行查找语义，防工作区里塞同名 git.exe 抢占）。
//! - 所有 git 调用都带 `-c core.autocrlf=false`（仓库 Cargo.toml 等含 CRLF，开 autocrlf 会
//!   制造幽灵冲突）+ `GIT_TERMINAL_PROMPT=0`（杜绝凭据交互挂死）+ `scrub_secret_env`（不把
//!   API Key 等密钥传给 git 子进程）。
//! - **永不 force**：本模块构造的 merge 参数里不出现、也不接受任何 force / `-X ours|theirs`
//!   / 强制重置语义；冲突只做「中止合并保持父工作树干净 + 把冲突路径结构化抛出」。
//! - Windows 友好：路径全程用绝对路径；工作树目录建在系统临时目录下、名字带唯一随机量，
//!   避免与并行子代理互踩；`worktree remove --force` + `branch -D` + `worktree prune` 兜底清理。
//!
//! 这是一个**原语**，默认不接管现有子代理路径（见 subagent.rs 的集成缝）。调用方显式
//! 用它把一个写子代理跑在隔离工作树里，再决定何时合并回来。

use crate::ToolRuntimeError;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// worktree/merge 等本地 git 操作超时（大仓库建工作树可能拷贝不少文件，给足余量）。
const WORKTREE_GIT_TIMEOUT_SECS: u64 = 180;

/// 进程内单调计数器，拼进临时分支名/目录名，确保同一进程内并行创建也唯一。
static WORKTREE_SEQ: AtomicU64 = AtomicU64::new(0);

// ── 结构化结果类型 ────────────────────────────────────────────────────────────

/// 合并预览/合并的结果：要么干净合并，要么带冲突路径**抛回**给调用方（绝不静默强解）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MergeOutcome {
    /// 干净合并：无冲突。`fast_forward` 表示这是一次快进（父分支无独立提交）。
    Clean { fast_forward: bool },
    /// 冲突：列出冲突文件路径。**调用方必须人工处理**，本模块不会自动解决。
    Conflict { paths: Vec<String> },
}

impl MergeOutcome {
    /// 是否为干净合并（无冲突）。
    pub fn is_clean(&self) -> bool {
        matches!(self, MergeOutcome::Clean { .. })
    }

    /// 是否存在冲突。
    pub fn has_conflict(&self) -> bool {
        matches!(self, MergeOutcome::Conflict { .. })
    }
}

// ── git 可执行解析（复用 git.rs 的防 cwd 抢占语义；本模块自带一份避免跨模块可见性耦合） ──

/// 在 PATH 里按 which 风格解析出 `git` 的绝对路径（绕开 Windows cwd 优先查找）。
fn resolve_git_path() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        // 空分量在 Windows 上历史等同当前目录——正是要规避的攻击面，跳过。
        if dir.as_os_str().is_empty() {
            continue;
        }
        for candidate in git_candidate_names() {
            let full = dir.join(&candidate);
            if is_executable_file(&full) {
                return Some(full);
            }
        }
    }
    None
}

/// 为 `git` 生成待尝试的文件名列表（含平台相关可执行扩展名）。
fn git_candidate_names() -> Vec<String> {
    #[cfg(windows)]
    {
        let mut names = vec![
            "git".to_string(),
            "git.exe".to_string(),
            "git.cmd".to_string(),
            "git.bat".to_string(),
        ];
        if let Some(pathext) = std::env::var_os("PATHEXT") {
            if let Some(s) = pathext.to_str() {
                for ext in s.split(';') {
                    let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
                    if ext.is_empty() {
                        continue;
                    }
                    let cand = format!("git.{ext}");
                    if !names.iter().any(|n| n.eq_ignore_ascii_case(&cand)) {
                        names.push(cand);
                    }
                }
            }
        }
        names
    }
    #[cfg(not(windows))]
    {
        vec!["git".to_string()]
    }
}

/// 判断路径是否为一个可执行的常规文件。
fn is_executable_file(path: &Path) -> bool {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

// ── git 执行底座 ──────────────────────────────────────────────────────────────

struct GitRun {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// 在 `cwd` 下以参数向量调用 `git`（不经 shell），抽干管道、带超时。
///
/// 固定前缀注入 `-c core.autocrlf=false`：本仓库含 CRLF 文件，开 autocrlf 会让 worktree/merge
/// 把整文件看成全行改动而炸出幽灵冲突——隔离合并尤其敏感，这里强制关掉。
fn run_git_in(cwd: &Path, args: &[&str]) -> Result<GitRun, ToolRuntimeError> {
    let git_path = resolve_git_path().ok_or_else(|| {
        ToolRuntimeError::CommandFailed(
            "git 未安装或不在 PATH 中（worktree 隔离需要系统 git）".to_string(),
        )
    })?;
    let mut builder = Command::new(&git_path);
    builder
        .arg("-c")
        .arg("core.autocrlf=false")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::scrub_secret_env(&mut builder);

    let mut child = builder.spawn().map_err(|e| {
        ToolRuntimeError::CommandFailed(format!("无法启动 git: {e}"))
    })?;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let out_handle = std::thread::spawn(move || crate::drain_pipe_streaming(stdout_pipe, None));
    let err_handle = std::thread::spawn(move || crate::drain_pipe_streaming(stderr_pipe, None));

    let started = Instant::now();
    let timeout = Duration::from_secs(WORKTREE_GIT_TIMEOUT_SECS);
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
    let (stdout, _) = out_handle.join().unwrap_or((String::new(), false));
    let (stderr, _) = err_handle.join().unwrap_or((String::new(), false));

    if timed_out {
        return Err(ToolRuntimeError::CommandFailed(format!(
            "git 命令超时（>{WORKTREE_GIT_TIMEOUT_SECS}s）: git {}",
            args.join(" ")
        )));
    }
    Ok(GitRun {
        code: status.code(),
        stdout,
        stderr,
    })
}

/// 跑 git 并要求退出码为 0，否则把 stderr（或 stdout）作为清晰错误回传。
fn run_git_checked(cwd: &Path, args: &[&str]) -> Result<GitRun, ToolRuntimeError> {
    let out = run_git_in(cwd, args)?;
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

// ── 隔离工作树守卫 ────────────────────────────────────────────────────────────

/// 一个隔离的 git 工作树 + 临时分支，供单个写子代理独占。
///
/// 由 [`IsolatedWorktree::create`] 在父仓库当前 HEAD 上创建。子代理把它的文件改动与提交
/// 都做在 [`path()`](Self::path) 指向的目录里；完成后用 [`merge_into`](Self::merge_into)
/// 把 [`branch()`](Self::branch) 合并回父分支。
///
/// **RAII**：`Drop` 时会 `git worktree remove --force` + `git branch -D` + `worktree prune`，
/// 无论是正常结束、错误返回还是 panic 展开，都不会留下泄漏的工作树/分支。清理是 best-effort，
/// Drop 中不 panic。需要拿到清理过程中的错误时，显式调用 [`cleanup`](Self::cleanup)。
pub struct IsolatedWorktree {
    /// 父仓库根（worktree/branch 都挂在它名下；merge 默认也在这里执行）。
    parent_repo: PathBuf,
    /// 隔离工作树目录的绝对路径（子代理在此读写）。
    worktree_path: PathBuf,
    /// 临时分支名（隔离分支，子代理的提交落在它上面）。
    branch: String,
    /// 是否已显式清理过——避免 Drop 再清一次。
    cleaned: bool,
}

impl IsolatedWorktree {
    /// 在 `parent_repo`（一个 git 仓库工作树根）当前 HEAD 上创建隔离工作树 + 临时分支。
    ///
    /// - `label`：人读标签（如子代理用途），会被规整成分支名/目录名的一部分；只保留
    ///   `[A-Za-z0-9_-]`，其余替换为 `-`，并截断，避免非法 ref 名或超长路径。
    /// - 工作树目录建在系统临时目录下（绝对路径），名字带 label + 唯一随机量，避免并行互踩。
    /// - 临时分支从当前 HEAD（`HEAD`）拉出，`git worktree add -b <branch> <dir> HEAD`。
    ///
    /// 失败（非 git 仓库 / detached 无 HEAD / 目录已存在等）时回传清晰错误，不残留半成品。
    pub fn create(
        parent_repo: impl AsRef<Path>,
        label: &str,
    ) -> Result<Self, ToolRuntimeError> {
        let parent_repo = parent_repo
            .as_ref()
            .canonicalize()
            .map_err(|e| ToolRuntimeError::WorkspaceUnavailable(e.to_string()))?;

        // 必须是 git 仓库的工作树，且有可解析的 HEAD（detached/空仓库会失败，回传清晰错误）。
        run_git_checked(&parent_repo, &["rev-parse", "--is-inside-work-tree"])?;
        run_git_checked(&parent_repo, &["rev-parse", "--verify", "HEAD"]).map_err(|_| {
            ToolRuntimeError::CommandFailed(
                "父仓库没有可用的 HEAD（空仓库或无提交），无法创建隔离工作树".to_string(),
            )
        })?;

        let slug = sanitize_label(label);
        let nonce = unique_nonce();
        let branch = format!("mdga/subagent/{slug}-{nonce}");
        let dir_name = format!("mdga-worktree-{slug}-{nonce}");
        let worktree_path = std::env::temp_dir().join(dir_name);

        // 目标目录不应已存在（唯一随机量基本杜绝，但仍防御）。
        if worktree_path.exists() {
            return Err(ToolRuntimeError::CommandFailed(format!(
                "隔离工作树目录已存在，拒绝覆盖: {}",
                worktree_path.display()
            )));
        }

        let wt_str = path_arg(&worktree_path)?;
        // worktree add -b <branch> <dir> HEAD：在当前 HEAD 上拉新分支并签出到独立目录。
        run_git_checked(
            &parent_repo,
            &["worktree", "add", "-b", &branch, &wt_str, "HEAD"],
        )?;

        // worktree 目录建好后再 canonicalize 一次，拿到稳定的绝对路径（解开符号链接/8.3 短名）。
        let worktree_path = worktree_path.canonicalize().unwrap_or(worktree_path);

        Ok(Self {
            parent_repo,
            worktree_path,
            branch,
            cleaned: false,
        })
    }

    /// 隔离工作树目录（子代理在此读写文件并提交）。
    pub fn path(&self) -> &Path {
        &self.worktree_path
    }

    /// 临时分支名（子代理的提交落在它上面）。
    pub fn branch(&self) -> &str {
        &self.branch
    }

    /// 父仓库根路径。
    pub fn parent_repo(&self) -> &Path {
        &self.parent_repo
    }

    /// 在隔离工作树里暂存全部改动并提交一条。便于把子代理的文件改动固化到隔离分支上。
    ///
    /// 提交身份：若仓库/全局未配置 user.name/email，git commit 会失败；这里不偷偷注入身份，
    /// 把失败如实抛回（调用方应在父仓库配好身份，worktree 会继承）。空改动（无可提交内容）
    /// 同样抛回 git 的「nothing to commit」错误，避免静默成功误导调用方。
    pub fn commit_all(&self, message: &str) -> Result<String, ToolRuntimeError> {
        let msg = message.trim();
        if msg.is_empty() {
            return Err(ToolRuntimeError::CommandFailed(
                "提交信息不能为空".to_string(),
            ));
        }
        run_git_checked(&self.worktree_path, &["add", "-A"])?;
        run_git_checked(&self.worktree_path, &["commit", "-m", msg])?;
        let head = run_git_checked(&self.worktree_path, &["rev-parse", "HEAD"])?;
        Ok(head.stdout.trim().to_string())
    }

    /// **只读预览**：在不改动任何工作树的前提下，判断把本隔离分支合并进 `target_branch`
    /// 是否会冲突，以及冲突文件有哪些。
    ///
    /// 用 `git merge-tree --write-tree <target_branch> <our_branch>`：干净合并退出 0 且仅输出
    /// 合并后的 tree OID；冲突则退出非 0 并在后续行列出冲突路径。本调用**不触碰**父工作树，
    /// 适合在真正 merge 前先探明冲突，或在不想落地时仅做可合并性检查。
    pub fn preview_merge(&self, target_branch: &str) -> Result<MergeOutcome, ToolRuntimeError> {
        let target = valid_ref_name(target_branch)?;
        let out = run_git_in(
            &self.parent_repo,
            &["merge-tree", "--write-tree", &target, &self.branch],
        )?;
        match out.code {
            Some(0) => Ok(MergeOutcome::Clean {
                fast_forward: false,
            }),
            Some(1) => Ok(MergeOutcome::Conflict {
                paths: parse_merge_tree_conflict_paths(&out.stdout),
            }),
            other => Err(ToolRuntimeError::CommandFailed(format!(
                "git merge-tree 失败（退出码 {other:?}）: {}",
                out.stderr.trim()
            ))),
        }
    }

    /// 把本隔离分支真正合并回父仓库里**当前已签出**的分支（即在父仓库工作树里执行 merge）。
    ///
    /// 调用前提：父仓库当前应签出目标分支且工作树干净（否则 git 会拒绝或留下半完成状态，这里
    /// 一律按 git 的错误抛回，不强行清场）。语义：
    /// - 用 `git merge --no-ff --no-edit <our_branch>`，**绝不** force、**绝不** `-X ours|theirs`、
    ///   **绝不** 自动选边——一切冲突都如实抛出。
    /// - 冲突时调用 `git merge --abort` 把父工作树**还原干净**，并回传 [`MergeOutcome::Conflict`]
    ///   及冲突路径列表，由调用方人工处理（绝不静默落一个半合并状态）。
    /// - 干净合并回传 [`MergeOutcome::Clean`]。
    pub fn merge_into(&self, target_branch: &str) -> Result<MergeOutcome, ToolRuntimeError> {
        self.merge_into_at(&self.parent_repo, target_branch)
    }

    /// 与 [`merge_into`](Self::merge_into) 同，但允许指定执行 merge 的工作树目录（须已签出
    /// `target_branch` 且干净）。抽出此形态便于测试用一个干净的辅助工作树做合并，不污染主仓库。
    pub fn merge_into_at(
        &self,
        merge_cwd: impl AsRef<Path>,
        target_branch: &str,
    ) -> Result<MergeOutcome, ToolRuntimeError> {
        let target = valid_ref_name(target_branch)?;
        let merge_cwd = merge_cwd.as_ref();

        // 防御：确认 merge_cwd 当前确实签出在 target 分支上，避免误并到别的分支。
        let cur = run_git_checked(merge_cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?
            .stdout
            .trim()
            .to_string();
        if cur != target {
            return Err(ToolRuntimeError::CommandFailed(format!(
                "merge 目标工作树当前在分支 {cur}，与请求的 {target} 不符；请先切到目标分支再合并"
            )));
        }

        // 真合并：--no-ff 保留一个合并提交（也使「干净 vs 快进」可区分）；--no-edit 不弹编辑器。
        // 关键安全不变量：参数里没有任何 force / -X ours|theirs / 自动选边。
        let merge_args = build_merge_args(&self.branch);
        let out = run_git_in(merge_cwd, &merge_args)?;
        if out.code == Some(0) {
            return Ok(MergeOutcome::Clean {
                fast_forward: false,
            });
        }

        // 非 0：可能是冲突，也可能是其它失败（如工作树不干净）。读 status 判断是否处于合并冲突。
        let conflicts = list_unmerged_paths(merge_cwd)?;
        if !conflicts.is_empty() {
            // 冲突：中止合并，把父工作树还原干净，绝不留半合并状态、绝不自动选边。
            // 中止本身若失败（index.lock / IO / 半状态），父工作树可能仍残留冲突——绝不谎报
            // 「已还原干净」，如实抛错让上层提示人工清理。
            let abort = run_git_in(merge_cwd, &["merge", "--abort"])?;
            if abort.code != Some(0) {
                return Err(ToolRuntimeError::CommandFailed(format!(
                    "合并冲突后 `git merge --abort` 失败（退出码 {:?}）：父工作树可能仍残留冲突/半合并状态，\
                     请手动执行 `git merge --abort` 清理。冲突文件: {}",
                    abort.code,
                    conflicts.join(", ")
                )));
            }
            return Ok(MergeOutcome::Conflict { paths: conflicts });
        }

        // 没有未合并路径却失败：是别的错误（例如目标工作树本就不干净 / 锁等）。如实抛回。
        // 防御性地尝试中止可能的半开合并状态。
        let _ = run_git_in(merge_cwd, &["merge", "--abort"]);
        let msg = if !out.stderr.trim().is_empty() {
            out.stderr.trim().to_string()
        } else if !out.stdout.trim().is_empty() {
            out.stdout.trim().to_string()
        } else {
            format!("git merge 失败（退出码 {:?}）", out.code)
        };
        Err(ToolRuntimeError::CommandFailed(msg))
    }

    /// 显式清理：移除工作树 + 删除临时分支 + prune。可拿到清理过程中的错误（Drop 版静默吞错）。
    ///
    /// 幂等：已清理过则直接成功返回。清理顺序：先 `worktree remove --force`（强制移除即便有
    /// 未提交改动），再 `branch -D`（强删未合并分支也允许——隔离分支本就是临时的），最后
    /// `worktree prune` 收尾任何悬挂登记。
    pub fn cleanup(&mut self) -> Result<(), ToolRuntimeError> {
        if self.cleaned {
            return Ok(());
        }
        self.cleaned = true;
        let wt_str = path_arg(&self.worktree_path)?;

        // worktree remove --force：即便工作树里有未提交改动也强制移除（隔离工作树是一次性的）。
        let remove = run_git_in(
            &self.parent_repo,
            &["worktree", "remove", "--force", &wt_str],
        );
        // 不论 remove 成败，都尝试删分支 + prune，尽量不留泄漏。
        let del_branch = run_git_in(&self.parent_repo, &["branch", "-D", &self.branch]);
        let _ = run_git_in(&self.parent_repo, &["worktree", "prune"]);

        // 目录若仍残留（极少数 remove 失败场景），best-effort 直接删目录兜底。
        if self.worktree_path.exists() {
            let _ = std::fs::remove_dir_all(&self.worktree_path);
        }

        // 把首个出现的硬错误抛回（remove 优先）；分支删除失败不致命（prune 通常会带走它）。
        remove?;
        del_branch?;
        Ok(())
    }
}

impl Drop for IsolatedWorktree {
    fn drop(&mut self) {
        // RAII 兜底清理：无论正常结束 / 错误 / panic 展开都会走到这里。Drop 中绝不 panic，
        // 错误一律吞掉（显式清理请用 cleanup() 拿错误）。
        if !self.cleaned {
            let _ = self.cleanup();
        }
    }
}

// ── 纯函数辅助（可单测，不依赖系统 git） ─────────────────────────────────────────

/// 把人读 label 规整成可安全用于 ref 名/目录名的 slug：仅保留 `[A-Za-z0-9_-]`，其余转 `-`，
/// 折叠连续 `-`、去掉首尾 `-`，截断到 40 字符；空结果回退为 `agent`，避免非法/超长名。
fn sanitize_label(label: &str) -> String {
    let mut out = String::with_capacity(label.len().min(40));
    let mut last_dash = false;
    for ch in label.chars() {
        let keep = ch.is_ascii_alphanumeric() || ch == '_' || ch == '-';
        if keep {
            out.push(ch);
            last_dash = ch == '-';
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= 40 {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "agent".to_string()
    } else {
        trimmed
    }
}

/// 生成进程内唯一的随机量（纳秒时间戳 + 单调计数器 + pid），拼进分支名/目录名防并行互踩。
fn unique_nonce() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = WORKTREE_SEQ.fetch_add(1, Ordering::SeqCst);
    format!("{}-{}-{}", std::process::id(), nanos, seq)
}

/// 校验一个 ref/分支名作为 merge 目标：非空、不以 `-` 开头（防被当 git 选项注入）、无空白。
fn valid_ref_name(name: &str) -> Result<String, ToolRuntimeError> {
    let n = name.trim();
    if n.is_empty() {
        return Err(ToolRuntimeError::CommandFailed(
            "目标分支名不能为空".to_string(),
        ));
    }
    if n.starts_with('-') {
        return Err(ToolRuntimeError::CommandFailed(
            "目标分支名不能以 - 开头".to_string(),
        ));
    }
    if n.chars().any(char::is_whitespace) {
        return Err(ToolRuntimeError::CommandFailed(
            "目标分支名不能包含空白字符".to_string(),
        ));
    }
    Ok(n.to_string())
}

/// 构造 `git merge` 参数向量。抽出便于单测「绝不 force / 绝不选边」这一安全不变量。
///
/// 固定形态 `merge --no-ff --no-edit <branch>`：永不加入 `--force` / `-X ours` / `-X theirs`
/// / `-s ours` 等任何静默选边或强制语义；冲突交由上层人工处理。
fn build_merge_args(branch: &str) -> Vec<&str> {
    vec!["merge", "--no-ff", "--no-edit", branch]
}

/// 从 `git merge-tree --write-tree` 冲突输出里提取冲突文件路径。
///
/// 该命令冲突时 stdout 形如：首行是（部分）tree OID，随后是 `<mode> <oid> <stage>\t<path>`
/// 的冲突条目行；再后面可能跟「冲突信息块」。我们解析 `\t` 后的路径并去重（同一文件多 stage
/// 会出现多行），稳健起见也兼容只取行内最后一个 tab 段。解析不到则回退给个占位说明。
fn parse_merge_tree_conflict_paths(stdout: &str) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for line in stdout.lines() {
        // 冲突条目行包含一个 tab：制表符前是 `<mode> <oid> <stage>`，其后是路径。
        if let Some((meta, path)) = line.rsplit_once('\t') {
            // meta 段应形如 "100644 <oid> <1|2|3>"——用它过滤掉非条目行（如纯 OID 首行）。
            let looks_like_entry = {
                let mut it = meta.split_whitespace();
                let mode = it.next();
                let _oid = it.next();
                let stage = it.next();
                matches!(mode, Some(m) if m.chars().all(|c| c.is_ascii_digit()) && m.len() >= 6)
                    && matches!(stage, Some(s) if matches!(s, "1" | "2" | "3"))
            };
            if looks_like_entry {
                let p = path.trim().to_string();
                if !p.is_empty() && !paths.contains(&p) {
                    paths.push(p);
                }
            }
        }
    }
    if paths.is_empty() {
        // 解析不到具体路径也别丢失「有冲突」这一事实——给个占位，调用方据此走人工合并。
        paths.push("(冲突文件未能解析，请人工运行 git merge 查看)".to_string());
    }
    paths
}

/// 读取某工作树当前的未合并（冲突）文件路径列表（`git diff --name-only --diff-filter=U -z`）。
fn list_unmerged_paths(cwd: &Path) -> Result<Vec<String>, ToolRuntimeError> {
    let out = run_git_in(
        cwd,
        &["diff", "--name-only", "--diff-filter=U", "-z"],
    )?;
    // 该命令在合并冲突中即便整体退出码非 0 也会列出冲突项；这里直接解析其 stdout。
    Ok(out
        .stdout
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

/// 把路径转成 git 命令行参数用的字符串。要求路径为有效 UTF-8（git 参数走 &str），否则报错。
fn path_arg(path: &Path) -> Result<String, ToolRuntimeError> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| ToolRuntimeError::CommandFailed("路径不是有效 UTF-8".to_string()))
}

// ── 单元测试（纯逻辑，不依赖系统 git） ────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_label_keeps_safe_chars_and_caps_length() {
        assert_eq!(sanitize_label("refactor auth"), "refactor-auth");
        assert_eq!(sanitize_label("a/b\\c:d"), "a-b-c-d");
        assert_eq!(sanitize_label("---weird---"), "weird");
        assert_eq!(sanitize_label(""), "agent");
        assert_eq!(sanitize_label("@@@"), "agent");
        // 连续非法字符折叠成单个 -。
        assert_eq!(sanitize_label("x   y"), "x-y");
        // 截断到 <=40 字符。
        let long = "a".repeat(100);
        assert!(sanitize_label(&long).len() <= 40);
        // 结果只含安全字符。
        let s = sanitize_label("Hello, World! 你好 #42_test-case");
        assert!(s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
    }

    #[test]
    fn unique_nonce_is_unique_within_process() {
        let a = unique_nonce();
        let b = unique_nonce();
        assert_ne!(a, b, "同进程内连续两次 nonce 必须不同: {a} == {b}");
    }

    #[test]
    fn valid_ref_name_rejects_injection_and_empty() {
        assert_eq!(valid_ref_name("main").unwrap(), "main");
        assert_eq!(valid_ref_name("  feature/x  ").unwrap(), "feature/x");
        assert!(valid_ref_name("").is_err());
        assert!(valid_ref_name("   ").is_err());
        // 不能以 - 开头：杜绝被当 git 选项注入。
        assert!(valid_ref_name("-X").is_err());
        assert!(valid_ref_name("--force").is_err());
        assert!(valid_ref_name("has space").is_err());
    }

    #[test]
    fn merge_args_never_force_or_pick_a_side() {
        let args = build_merge_args("mdga/subagent/x-1");
        assert_eq!(args, vec!["merge", "--no-ff", "--no-edit", "mdga/subagent/x-1"]);
        // 安全不变量：无论如何都不得出现 force / 选边策略。
        for a in &args {
            let low = a.to_ascii_lowercase();
            assert!(!low.contains("force"), "merge 参数不得含 force: {a}");
            assert_ne!(low, "-x", "merge 参数不得用 -X 策略选项");
            assert!(!low.starts_with("-s"), "merge 参数不得指定 -s 策略（防 -s ours 静默选边）: {a}");
            assert!(!low.contains("ours"), "merge 参数不得含 ours: {a}");
            assert!(!low.contains("theirs"), "merge 参数不得含 theirs: {a}");
        }
    }

    #[test]
    fn parse_conflict_paths_extracts_unique_paths() {
        // 模拟 merge-tree --write-tree 冲突 stdout：首行 tree OID，随后三 stage 条目同指 f.txt。
        let stdout = "\
1623dade593b473db90e42bfea994e8f3306803e
100644 8681f8b8f32615a16703053bc1eaffb3e5e720a5 1\tf.txt
100644 a8a8dfb56749a4a942dd220ed1d8af1fc7c98724 2\tf.txt
100644 e4cf7fd1b9064dcd7a505e78614ada3478f37425 3\tf.txt

Auto-merging f.txt
CONFLICT (content): Merge conflict in f.txt
";
        let paths = parse_merge_tree_conflict_paths(stdout);
        assert_eq!(paths, vec!["f.txt".to_string()], "同一文件多 stage 应去重为一条");
    }

    #[test]
    fn parse_conflict_paths_multiple_files() {
        let stdout = "\
abc123tree
100644 oid1 1\tsrc/a.rs
100644 oid2 2\tsrc/a.rs
100644 oid3 1\tsrc/b.rs
100644 oid4 3\tsrc/b.rs
";
        let paths = parse_merge_tree_conflict_paths(stdout);
        assert_eq!(paths, vec!["src/a.rs".to_string(), "src/b.rs".to_string()]);
    }

    #[test]
    fn parse_conflict_paths_falls_back_when_unparseable() {
        // 没有可识别的条目行时也不能丢「有冲突」这一事实。
        let paths = parse_merge_tree_conflict_paths("just some text\nwithout entries\n");
        assert_eq!(paths.len(), 1);
        assert!(paths[0].contains("冲突"));
    }

    #[test]
    fn git_candidate_names_first_is_bare() {
        let names = git_candidate_names();
        assert_eq!(names.first().map(String::as_str), Some("git"));
        #[cfg(windows)]
        {
            for want in ["git.exe", "git.cmd", "git.bat"] {
                assert!(names.iter().any(|n| n == want), "应含 {want}: {names:?}");
            }
        }
    }

    #[test]
    fn merge_outcome_predicates() {
        let clean = MergeOutcome::Clean { fast_forward: false };
        assert!(clean.is_clean());
        assert!(!clean.has_conflict());
        let conflict = MergeOutcome::Conflict {
            paths: vec!["x".to_string()],
        };
        assert!(conflict.has_conflict());
        assert!(!conflict.is_clean());
    }
}
