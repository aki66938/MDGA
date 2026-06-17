//! R3 真 TDD 自修复循环（feat/r3-tdd）：在「写后验证」之上把「只看 exit_code 的二元判定 + 原样回灌」
//! 升级为「两段门(编译→测试)+ 结构化解析测试结果 + 失败归因回灌 + doom-loop 停滞护栏 + 按影响选测」。
//!
//! 本模块是**纯逻辑**(std + serde_json,零正则、零外部依赖),全部可独立单测;实际执行(run_command/emit)
//! 仍由桌面端 agent_loop 编排。设计原则:解析尽力而为,解析不出时优雅回退到原始输出尾部,绝不丢信息。
//!
//! 公开面:
//! - [`detect_verification_plan`]:探测**有序**验证计划(先编译门后测试门),支持 .mdga/diagnostics 的
//!   `build:` / `test:` 两段语法(向后兼容裸行)。
//! - [`parse_report`]:按框架把命令原始输出解析成 [`TestReport`](逐失败名 + file:line + 摘要)。
//! - [`format_verify_feedback`]:把 [`TestReport`] 渲染成回灌给模型的结构化反馈。
//! - [`report_signature`]:为 doom-loop 护栏算「本轮失败签名」(失败集合不变=停滞)。
//! - [`focused_command`]:由失败用例名缩出「只重跑失败用例」的命令,加速修复循环。

use serde::{Deserialize, Serialize};
use std::path::Path;

/// 验证门类别:编译门(快速过编译/类型)在前,测试门(跑用例到绿)在后。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum VerifyKind {
    /// 编译/类型门:cargo check、tsc、go build 等,失败=代码都没编过,先修这个。
    Build,
    /// 测试门:cargo test、npm test、pytest、go test 等,跑用例到绿。
    Test,
}

/// 验证命令所属框架,决定用哪个解析器。Generic 表示无法识别 → 回退原始尾部。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Framework {
    CargoCheck,
    CargoTest,
    NodeBuild,
    NodeTest,
    Pytest,
    GoTest,
    Generic,
}

/// 验证计划里的一步:一条命令 + 它是编译门还是测试门 + 用哪套解析器。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VerifyStep {
    pub command: String,
    pub kind: VerifyKind,
    pub framework: Framework,
}

/// 一个工作区的有序验证计划:steps 按「先编译门后测试门」排列,逐步执行、首个失败的门即回灌。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VerifyPlan {
    pub steps: Vec<VerifyStep>,
}

/// 单条失败:用例/错误名 + 源码位置(尽力而为)+ 简短消息。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Failure {
    pub name: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub message: String,
}

/// 一次验证命令的结构化结果。passed/failed/ignored 为尽力而为的摘要计数(解析不出为 0);
/// failures 为逐条失败明细;raw_tail 保留原始输出尾部,供解析不全时兜底回灌。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TestReport {
    pub framework: Framework,
    pub kind: VerifyKind,
    pub passed: usize,
    pub failed: usize,
    pub ignored: usize,
    pub failures: Vec<Failure>,
    pub raw_tail: String,
}

impl TestReport {
    fn empty(framework: Framework, kind: VerifyKind, raw_tail: String) -> Self {
        TestReport {
            framework,
            kind,
            passed: 0,
            failed: 0,
            ignored: 0,
            failures: Vec::new(),
            raw_tail,
        }
    }
}

/// 原始输出尾部上限(字符):与旧逻辑的 6000 字截断对齐,保证回灌不超长。
const RAW_TAIL_CHARS: usize = 6000;
/// 回灌里最多逐条列出的失败数,超出折叠为「其余 N 条省略」,避免淹没。
const MAX_LISTED_FAILURES: usize = 12;
/// 按影响选测时最多窄跑的失败用例数,避免命令过长。
const MAX_FOCUSED: usize = 12;

/// 取字符串尾部 n 个字符(按 char 计,避免切断多字节)。
fn tail_chars(s: &str, n: usize) -> String {
    let total = s.chars().count();
    if total <= n {
        return s.to_string();
    }
    s.chars().skip(total - n).collect()
}

// ── 验证计划探测 ──────────────────────────────────────────────────────────────

/// 探测工作区的有序验证计划:优先 .mdga/diagnostics(支持 `build:`/`test:` 两段语法,
/// 向后兼容裸命令行);否则按生态清单推断「编译门 + 测试门」。探测不到任何步骤则 None。
pub fn detect_verification_plan(workspace: &str) -> Option<VerifyPlan> {
    if let Some(plan) = read_diagnostics_plan(workspace) {
        if !plan.steps.is_empty() {
            return Some(plan);
        }
    }
    let steps = infer_steps_from_manifests(Path::new(workspace));
    if steps.is_empty() {
        None
    } else {
        Some(VerifyPlan { steps })
    }
}

/// 读取 .mdga/diagnostics 并解析成计划。支持两种写法:
/// 1) 多行 `build: <cmd>` / `test: <cmd>`(各自一步,大小写不敏感前缀);
/// 2) 单条裸命令(向后兼容旧行为):命令含 test 视为测试门,否则编译门。
fn read_diagnostics_plan(workspace: &str) -> Option<VerifyPlan> {
    let path = Path::new(workspace).join(".mdga").join("diagnostics");
    let content = std::fs::read_to_string(path).ok()?;
    let mut build: Option<String> = None;
    let mut test: Option<String> = None;
    let mut bare: Option<String> = None;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("build:") {
            let cmd = line[line.len() - rest.len()..].trim().to_string();
            if !cmd.is_empty() && build.is_none() {
                build = Some(cmd);
            }
        } else if let Some(rest) = lower.strip_prefix("test:") {
            let cmd = line[line.len() - rest.len()..].trim().to_string();
            if !cmd.is_empty() && test.is_none() {
                test = Some(cmd);
            }
        } else if bare.is_none() {
            bare = Some(line.to_string());
        }
    }
    let mut steps = Vec::new();
    if let Some(cmd) = build {
        steps.push(step_for_command(cmd, Some(VerifyKind::Build)));
    }
    if let Some(cmd) = test {
        steps.push(step_for_command(cmd, Some(VerifyKind::Test)));
    }
    // 没有显式两段、但有裸命令 → 单步,沿用旧语义(按命令猜门类)。
    if steps.is_empty() {
        if let Some(cmd) = bare {
            steps.push(step_for_command(cmd, None));
        }
    }
    if steps.is_empty() {
        None
    } else {
        Some(VerifyPlan { steps })
    }
}

/// 由命令字符串猜测框架与门类。kind 为 None 时按命令是否含 test 自动判门。
fn step_for_command(command: String, kind: Option<VerifyKind>) -> VerifyStep {
    let framework = framework_from_command(&command);
    let kind = kind.unwrap_or_else(|| {
        if matches!(
            framework,
            Framework::CargoTest | Framework::NodeTest | Framework::Pytest | Framework::GoTest
        ) {
            VerifyKind::Test
        } else {
            VerifyKind::Build
        }
    });
    VerifyStep {
        command,
        kind,
        framework,
    }
}

/// 从命令字符串识别框架(用于 .mdga/diagnostics 自定义命令的解析器选择)。
fn framework_from_command(command: &str) -> Framework {
    let c = command.to_ascii_lowercase();
    if c.contains("cargo test") || c.contains("cargo nextest") {
        Framework::CargoTest
    } else if c.contains("cargo check") || c.contains("cargo build") || c.contains("cargo clippy") {
        Framework::CargoCheck
    } else if c.contains("pytest") || c.contains("py.test") {
        Framework::Pytest
    } else if c.contains("go test") {
        Framework::GoTest
    } else if c.contains("jest") || c.contains("vitest") {
        Framework::NodeTest
    } else if c.contains("test") {
        // npm test / yarn test / pnpm test 等
        Framework::NodeTest
    } else if c.contains("tsc") || c.contains("build") || c.contains("check") {
        Framework::NodeBuild
    } else {
        Framework::Generic
    }
}

/// 无显式诊断时,按工作区清单推断「编译门 + 测试门」。两段策略(R3 设计决定):
/// Rust 先 `cargo check` 再 `cargo test`;Node 先 `npm run build`(若有)再 `npm test`(若有);
/// Python 仅 `pytest`(无编译门);Go 先 `go build ./...` 再 `go test ./...`。
fn infer_steps_from_manifests(root: &Path) -> Vec<VerifyStep> {
    let mut steps = Vec::new();
    if root.join("Cargo.toml").is_file() {
        steps.push(VerifyStep {
            command: "cargo check".to_string(),
            kind: VerifyKind::Build,
            framework: Framework::CargoCheck,
        });
        steps.push(VerifyStep {
            command: "cargo test".to_string(),
            kind: VerifyKind::Test,
            framework: Framework::CargoTest,
        });
        return steps;
    }
    if let Ok(text) = std::fs::read_to_string(root.join("package.json")) {
        if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&text) {
            let scripts = pkg.get("scripts");
            let has = |name: &str| {
                scripts
                    .and_then(|s| s.get(name))
                    .and_then(|v| v.as_str())
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
            };
            if has("build") {
                steps.push(VerifyStep {
                    command: "npm run build".to_string(),
                    kind: VerifyKind::Build,
                    framework: Framework::NodeBuild,
                });
            }
            if has("test") {
                steps.push(VerifyStep {
                    command: "npm test".to_string(),
                    kind: VerifyKind::Test,
                    framework: Framework::NodeTest,
                });
            }
        }
        if !steps.is_empty() {
            return steps;
        }
    }
    if root.join("go.mod").is_file() {
        steps.push(VerifyStep {
            command: "go build ./...".to_string(),
            kind: VerifyKind::Build,
            framework: Framework::Generic,
        });
        steps.push(VerifyStep {
            command: "go test ./...".to_string(),
            kind: VerifyKind::Test,
            framework: Framework::GoTest,
        });
        return steps;
    }
    let py = ["pyproject.toml", "setup.py", "setup.cfg", "pytest.ini", "tox.ini", "conftest.py"]
        .iter()
        .any(|f| root.join(f).is_file());
    if py {
        steps.push(VerifyStep {
            command: "pytest".to_string(),
            kind: VerifyKind::Test,
            framework: Framework::Pytest,
        });
    }
    steps
}

// ── 通用小工具 ────────────────────────────────────────────────────────────────

/// 在一行里寻找首个「源码位置」:形如 `path.ext:line[:col]` 或 tsc 的 `path.ext(line,col)`。
/// 返回 (path, line?)。覆盖 .rs/.ts/.tsx/.js/.jsx/.py/.go 等常见后缀,供各框架解析复用。
fn scan_location(line: &str) -> Option<(String, Option<u32>)> {
    const EXTS: &[&str] = &[
        ".rs", ".tsx", ".ts", ".jsx", ".mjs", ".cjs", ".js", ".py", ".go",
    ];
    let mut best: Option<(usize, String, Option<u32>)> = None;
    for ext in EXTS {
        let mut from = 0;
        while let Some(rel) = line[from..].find(ext) {
            let ext_end = from + rel + ext.len();
            // 后缀后必须紧跟 ':' 或 '(' 才算位置(排除把普通词里的 .js 当路径)。
            let rest = &line[ext_end..];
            let next = rest.chars().next();
            if next != Some(':') && next != Some('(') {
                from = ext_end;
                continue;
            }
            // 向左找路径起点:遇到分隔符停。
            let prefix = &line[..ext_end];
            let path_start = prefix
                .rfind(|c: char| {
                    c == ' ' || c == '(' || c == '\'' || c == '"' || c == ',' || c == '`'
                        || c == '[' || c == '<' || c == '\t'
                })
                .map(|i| i + 1)
                .unwrap_or(0);
            let path = line[path_start..ext_end].to_string();
            let after = if next == Some(':') { &rest[1..] } else { &rest[1..] };
            let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            let line_no = digits.parse::<u32>().ok();
            if !path.is_empty() && best.as_ref().map(|b| path_start < b.0).unwrap_or(true) {
                best = Some((path_start, path, line_no));
            }
            from = ext_end;
        }
    }
    best.map(|(_, p, l)| (p, l))
}

/// 抽取 text 中紧挨在某个词(如 "passed"/"failed"/"ignored")之前的数字。
/// 词比较**大小写敏感**:摘要计数恒为小写(`2 failed`/`7 passed`),而逐条标记是大写
/// (`FAILED tests/..` / cargo `... FAILED`)——大小写区分才能避免把标记行误当计数。
/// 去除词的尾随标点(逗号/分号),用于 cargo/jest/pytest/vitest 摘要计数。
fn num_before_word(text: &str, word: &str) -> Option<usize> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    for i in 1..tokens.len() {
        let tok = tokens[i].trim_matches(|c: char| !c.is_ascii_alphabetic());
        if tok == word {
            if let Ok(n) = tokens[i - 1].trim_matches(|c: char| !c.is_ascii_digit()).parse::<usize>()
            {
                return Some(n);
            }
        }
    }
    None
}

// ── 解析分发 ──────────────────────────────────────────────────────────────────

/// 按框架解析命令原始输出为结构化报告。解析不出明细时 failures 为空,仅保留 raw_tail 兜底。
pub fn parse_report(
    framework: Framework,
    kind: VerifyKind,
    stdout: &str,
    stderr: &str,
) -> TestReport {
    let combined = if stderr.is_empty() {
        stdout.to_string()
    } else if stdout.is_empty() {
        stderr.to_string()
    } else {
        format!("{stdout}\n{stderr}")
    };
    let raw_tail = tail_chars(&combined, RAW_TAIL_CHARS);
    match framework {
        Framework::CargoTest => parse_cargo_test(&combined, kind, raw_tail),
        Framework::CargoCheck => parse_cargo_check(&combined, kind, raw_tail),
        Framework::NodeTest => parse_node_test(&combined, kind, raw_tail),
        Framework::Pytest => parse_pytest(&combined, kind, raw_tail),
        Framework::GoTest => parse_go_test(&combined, kind, raw_tail),
        Framework::NodeBuild | Framework::Generic => {
            TestReport::empty(framework, kind, raw_tail)
        }
    }
}

/// 解析 cargo test(libtest)输出:`test <name> ... FAILED` 收集失败名,`---- <name> stdout ----`
/// 区块抽取 panic 位置与消息,`test result:` 行取通过/失败/忽略计数。
fn parse_cargo_test(text: &str, kind: VerifyKind, raw_tail: String) -> TestReport {
    let lines: Vec<&str> = text.lines().collect();
    let mut failed_names: Vec<String> = Vec::new();
    for line in &lines {
        let t = line.trim();
        if let Some(name) = t.strip_suffix("... FAILED").map(str::trim) {
            if let Some(n) = name.strip_prefix("test ") {
                let n = n.trim();
                if !n.is_empty() && !failed_names.iter().any(|x| x == n) {
                    failed_names.push(n.to_string());
                }
            }
        }
    }
    // 明细区块:name → (file,line,message)。
    let mut details: std::collections::HashMap<String, (Option<String>, Option<u32>, String)> =
        std::collections::HashMap::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        if let Some(inner) = t.strip_prefix("---- ").and_then(|s| s.strip_suffix(" ----")) {
            let name = inner
                .trim_end_matches(" stdout")
                .trim_end_matches(" stderr")
                .trim()
                .to_string();
            let mut file = None;
            let mut line_no = None;
            let mut message = String::new();
            let mut j = i + 1;
            while j < lines.len() {
                let bt = lines[j].trim();
                if bt.starts_with("---- ") || bt.starts_with("test result:") || bt == "failures:" {
                    break;
                }
                if let Some(pos) = bt.find("panicked at") {
                    let rest = &bt[pos..];
                    if let Some((f, l)) = scan_location(rest) {
                        file = Some(f);
                        line_no = l;
                    }
                    // 旧格式: panicked at '<msg>', src/..  → 取引号内;新格式消息在下一非空行。
                    if let Some(q1) = rest.find('\'') {
                        if let Some(q2) = rest[q1 + 1..].rfind('\'') {
                            message = rest[q1 + 1..q1 + 1 + q2].to_string();
                        }
                    }
                    if message.is_empty() {
                        // 新格式:取后续首个非空、非 note 行作消息。
                        let mut k = j + 1;
                        while k < lines.len() {
                            let nb = lines[k].trim();
                            if !nb.is_empty() && !nb.starts_with("note:") {
                                message = nb.to_string();
                                break;
                            }
                            k += 1;
                        }
                    }
                }
                j += 1;
            }
            details.insert(name, (file, line_no, message));
            i = j;
            continue;
        }
        i += 1;
    }
    let failures = failed_names
        .into_iter()
        .map(|name| {
            let (file, line, message) = details.get(&name).cloned().unwrap_or_default();
            Failure {
                name,
                file,
                line,
                message,
            }
        })
        .collect::<Vec<_>>();
    let passed = num_before_word(text, "passed").unwrap_or(0);
    let mut failed = num_before_word(text, "failed").unwrap_or(0);
    let ignored = num_before_word(text, "ignored").unwrap_or(0);
    if failed == 0 && !failures.is_empty() {
        failed = failures.len();
    }
    TestReport {
        framework: Framework::CargoTest,
        kind,
        passed,
        failed,
        ignored,
        failures,
        raw_tail,
    }
}

/// 解析 cargo check/clippy 编译错误:`error[E0308]: ...` / `error: ...` 配下一行 `--> file:line:col`。
fn parse_cargo_check(text: &str, kind: VerifyKind, raw_tail: String) -> TestReport {
    let lines: Vec<&str> = text.lines().collect();
    let mut failures: Vec<Failure> = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if t.starts_with("error") && t.contains(':') {
            // 排除收尾汇总行:`error: aborting due to N previous errors`。
            if t.contains("aborting due to") || t.contains("could not compile") {
                continue;
            }
            let (name, message) = split_error_head(t);
            // 向下数行找 `-->` 定位。
            let mut file = None;
            let mut line_no = None;
            for look in lines.iter().skip(idx + 1).take(4) {
                let lt = look.trim_start();
                if lt.starts_with("-->") {
                    if let Some((f, l)) = scan_location(lt) {
                        file = Some(f);
                        line_no = l;
                    }
                    break;
                }
            }
            failures.push(Failure {
                name,
                file,
                line: line_no,
                message,
            });
        }
    }
    let failed = failures.len();
    TestReport {
        framework: Framework::CargoCheck,
        kind,
        passed: 0,
        failed,
        ignored: 0,
        failures,
        raw_tail,
    }
}

/// 从 `error[E0308]: mismatched types` 拆出 (name, message)。无错误码时 name="error"。
fn split_error_head(line: &str) -> (String, String) {
    // line 形如 `error[E0308]: mismatched types` 或 `error: cannot find value `x``。
    if let Some(rest) = line.strip_prefix("error[") {
        if let Some(end) = rest.find(']') {
            let code = format!("E:{}", &rest[..end]);
            let msg = rest[end + 1..].trim_start_matches(':').trim().to_string();
            return (code, msg);
        }
    }
    let msg = line
        .trim_start_matches("error")
        .trim_start_matches(':')
        .trim()
        .to_string();
    ("error".to_string(), msg)
}

/// 解析 jest/vitest 输出:`✕`/`×`/`●`/`FAIL` 行收集失败名,堆栈或同行 `path:line` 取位置,
/// `Tests: N failed, M passed` / `Tests  N failed | M passed` 取计数。
fn parse_node_test(text: &str, kind: VerifyKind, raw_tail: String) -> TestReport {
    let mut failures: Vec<Failure> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        let mark = ['✕', '×', '✗', '❌'].iter().find_map(|m| {
            t.strip_prefix(*m).or_else(|| {
                // jest 详细列表:`● Suite › name`
                None
            })
        });
        let name = if let Some(rest) = mark {
            Some(rest.trim())
        } else if let Some(rest) = t.strip_prefix("● ") {
            Some(rest.trim())
        } else {
            None
        };
        if let Some(raw_name) = name {
            // 去掉尾部耗时标注,如 "name 12ms" / "name (12 ms)"。
            let clean = strip_duration_suffix(raw_name);
            if clean.is_empty() {
                continue;
            }
            let (file, line_no) = scan_location(t).map(|(f, l)| (Some(f), l)).unwrap_or((None, None));
            if !failures.iter().any(|f| f.name == clean) {
                failures.push(Failure {
                    name: clean.to_string(),
                    file,
                    line: line_no,
                    message: String::new(),
                });
            }
        }
    }
    let passed = num_before_word(text, "passed").unwrap_or(0);
    let mut failed = num_before_word(text, "failed").unwrap_or(0);
    if failed == 0 && !failures.is_empty() {
        failed = failures.len();
    }
    TestReport {
        framework: Framework::NodeTest,
        kind,
        passed,
        failed,
        ignored: 0,
        failures,
        raw_tail,
    }
}

/// 去掉测试名尾部的耗时标注:`foo 12ms`、`foo (3 ms)`、`foo 1.2s`。
fn strip_duration_suffix(name: &str) -> &str {
    let trimmed = name.trim();
    // 去掉结尾括号耗时 "(12 ms)"
    let mut s = trimmed;
    if s.ends_with(')') {
        if let Some(open) = s.rfind('(') {
            let inner = &s[open + 1..s.len() - 1];
            if inner.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                s = s[..open].trim_end();
            }
        }
    }
    s
}

/// 解析 pytest 输出:短摘要 `FAILED path::test - Msg` 收集失败,`N failed, M passed` 取计数。
fn parse_pytest(text: &str, kind: VerifyKind, raw_tail: String) -> TestReport {
    let mut failures: Vec<Failure> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("FAILED ") {
            let (nodeid, message) = match rest.split_once(" - ") {
                Some((a, b)) => (a.trim(), b.trim().to_string()),
                None => (rest.trim(), String::new()),
            };
            let file = nodeid.split("::").next().map(|s| s.to_string());
            if !failures.iter().any(|f| f.name == nodeid) {
                failures.push(Failure {
                    name: nodeid.to_string(),
                    file,
                    line: None,
                    message,
                });
            }
        }
    }
    let passed = num_before_word(text, "passed").unwrap_or(0);
    let mut failed = num_before_word(text, "failed").unwrap_or(0);
    if failed == 0 && !failures.is_empty() {
        failed = failures.len();
    }
    TestReport {
        framework: Framework::Pytest,
        kind,
        passed,
        failed,
        ignored: 0,
        failures,
        raw_tail,
    }
}

/// 解析 go test 输出:`--- FAIL: TestName (..s)` 收集失败,后续缩进 `file_test.go:line: msg` 取位置/消息。
fn parse_go_test(text: &str, kind: VerifyKind, raw_tail: String) -> TestReport {
    let lines: Vec<&str> = text.lines().collect();
    let mut failures: Vec<Failure> = Vec::new();
    let mut passed = 0usize;
    for (idx, line) in lines.iter().enumerate() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("--- PASS:") {
            let _ = rest;
            passed += 1;
        }
        if let Some(rest) = t.strip_prefix("--- FAIL:") {
            let name = rest
                .trim()
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();
            let mut file = None;
            let mut line_no = None;
            let mut message = String::new();
            for look in lines.iter().skip(idx + 1).take(3) {
                if let Some((f, l)) = scan_location(look) {
                    file = Some(f);
                    line_no = l;
                    if let Some(colon) = look.find(": ") {
                        message = look[colon + 2..].trim().to_string();
                    }
                    break;
                }
            }
            if !name.is_empty() && !failures.iter().any(|f| f.name == name) {
                failures.push(Failure {
                    name,
                    file,
                    line: line_no,
                    message,
                });
            }
        }
    }
    let failed = failures.len();
    TestReport {
        framework: Framework::GoTest,
        kind,
        passed,
        failed,
        ignored: 0,
        failures,
        raw_tail,
    }
}

// ── 回灌反馈 + 护栏 + 选测 ────────────────────────────────────────────────────

/// 把报告渲染成回灌给模型的结构化反馈。有逐条失败时给「N/总 + 逐条 name@file:line — msg」,
/// 否则回退原始输出尾部(保留旧行为,绝不丢信息)。
pub fn format_verify_feedback(command: &str, report: &TestReport) -> String {
    let gate = match report.kind {
        VerifyKind::Build => "编译门",
        VerifyKind::Test => "测试门",
    };
    if report.failures.is_empty() {
        return format!(
            "验证命令 `{command}`({gate})报告了问题,请定位并修复后再结束:\n{}",
            report.raw_tail
        );
    }
    let total = report.passed + report.failed + report.ignored;
    let total_str = if total > 0 {
        format!("/{total}")
    } else {
        String::new()
    };
    let mut out = format!(
        "验证命令 `{command}`({gate})失败:{}{} 项未通过,请逐条定位修复后再结束。\n失败明细:\n",
        report.failed.max(report.failures.len()),
        total_str
    );
    for f in report.failures.iter().take(MAX_LISTED_FAILURES) {
        let loc = match (&f.file, f.line) {
            (Some(file), Some(line)) => format!(" @ {file}:{line}"),
            (Some(file), None) => format!(" @ {file}"),
            _ => String::new(),
        };
        let msg = if f.message.is_empty() {
            String::new()
        } else {
            format!(" — {}", f.message)
        };
        out.push_str(&format!("- {}{}{}\n", f.name, loc, msg));
    }
    if report.failures.len() > MAX_LISTED_FAILURES {
        out.push_str(&format!(
            "（其余 {} 条失败已省略）\n",
            report.failures.len() - MAX_LISTED_FAILURES
        ));
    }
    out
}

/// 为 doom-loop 护栏计算「本轮失败签名」:有逐条失败 → 失败名有序集合;否则用原始尾部的
/// 归一化指纹。两轮签名相同=毫无进展(停滞),用于及时升级用户而非空转烧轮。
pub fn report_signature(report: &TestReport, exit_code: Option<i32>, timed_out: bool) -> String {
    if timed_out {
        return "TIMEOUT".to_string();
    }
    if !report.failures.is_empty() {
        let mut names: Vec<&str> = report.failures.iter().map(|f| f.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        return format!("F:{}", names.join("|"));
    }
    // 无结构化失败:用尾部前若干非空行的归一化拼接(去行号/空白抖动)。
    let fp: String = report
        .raw_tail
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(8)
        .map(|l| l.chars().filter(|c| !c.is_ascii_digit()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    format!("R:{}:{}", exit_code.unwrap_or(-1), fp)
}

/// 按影响选测:由失败用例名缩出「只重跑这些失败」的命令,加速修复循环。
/// 仅对支持精确过滤的框架生效;Node/Generic/编译门返回 None(走全量)。
pub fn focused_command(step: &VerifyStep, failing: &[String]) -> Option<String> {
    if failing.is_empty() || step.kind == VerifyKind::Build {
        return None;
    }
    let names: Vec<&String> = failing.iter().take(MAX_FOCUSED).collect();
    match step.framework {
        Framework::CargoTest => {
            // `cargo test -- <filter1> <filter2> ...`:`--` 后的多个过滤词被 libtest 取 OR。
            let filters = names
                .iter()
                .map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            Some(format!("cargo test -- {filters}"))
        }
        Framework::Pytest => {
            // pytest 接受多个 node id 直接定位。
            let ids = names
                .iter()
                .map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            Some(format!("pytest {ids}"))
        }
        Framework::GoTest => {
            let pattern = names
                .iter()
                .map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join("|");
            Some(format!("go test -run '^({pattern})$' ./..."))
        }
        // Node 的 -t 是正则,失败名含特殊字符易误伤,保守走全量。
        Framework::NodeTest | Framework::NodeBuild | Framework::CargoCheck | Framework::Generic => {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_location_handles_common_shapes() {
        assert_eq!(
            scan_location("  --> src/parser.rs:142:9"),
            Some(("src/parser.rs".to_string(), Some(142)))
        );
        assert_eq!(
            scan_location("thread 'x' panicked at src/lib.rs:42:9:"),
            Some(("src/lib.rs".to_string(), Some(42)))
        );
        assert_eq!(
            scan_location("    at Object.<anonymous> (src/foo.js:10:5)"),
            Some(("src/foo.js".to_string(), Some(10)))
        );
        assert_eq!(
            scan_location("foo.ts(12,5): error TS2322: x"),
            Some(("foo.ts".to_string(), Some(12)))
        );
        assert_eq!(scan_location("no location here"), None);
    }

    #[test]
    fn parse_cargo_test_new_panic_format() {
        let out = "\
running 3 tests
test tests::ok_one ... ok
test tests::math::adds ... FAILED
test tests::greets ... FAILED

failures:

---- tests::math::adds stdout ----
thread 'tests::math::adds' panicked at src/math.rs:42:9:
assertion `left == right` failed
  left: 4
  right: 5

---- tests::greets stdout ----
thread 'tests::greets' panicked at src/lib.rs:7:5:
hello mismatch

failures:
    tests::math::adds
    tests::greets

test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
";
        let r = parse_report(Framework::CargoTest, VerifyKind::Test, out, "");
        assert_eq!(r.passed, 1);
        assert_eq!(r.failed, 2);
        assert_eq!(r.failures.len(), 2);
        let adds = r.failures.iter().find(|f| f.name == "tests::math::adds").unwrap();
        assert_eq!(adds.file.as_deref(), Some("src/math.rs"));
        assert_eq!(adds.line, Some(42));
        assert_eq!(adds.message, "assertion `left == right` failed");
    }

    #[test]
    fn parse_cargo_test_old_panic_format() {
        let out = "\
test tests::boom ... FAILED

---- tests::boom stdout ----
thread 'main' panicked at 'assertion failed: x == y', src/old.rs:9:1

test result: FAILED. 0 passed; 1 failed; 0 ignored
";
        let r = parse_report(Framework::CargoTest, VerifyKind::Test, out, "");
        assert_eq!(r.failed, 1);
        let f = &r.failures[0];
        assert_eq!(f.file.as_deref(), Some("src/old.rs"));
        assert_eq!(f.line, Some(9));
        assert_eq!(f.message, "assertion failed: x == y");
    }

    #[test]
    fn parse_cargo_check_errors() {
        let err = "\
error[E0308]: mismatched types
  --> src/main.rs:10:18
   |
10 |     let x: u32 = \"s\";
   |                  ^^^ expected `u32`, found `&str`

error: cannot find value `foo` in this scope
  --> src/lib.rs:3:5

error: aborting due to 2 previous errors
";
        let r = parse_report(Framework::CargoCheck, VerifyKind::Build, "", err);
        assert_eq!(r.failed, 2);
        assert_eq!(r.failures[0].name, "E:E0308");
        assert_eq!(r.failures[0].file.as_deref(), Some("src/main.rs"));
        assert_eq!(r.failures[0].line, Some(10));
        assert_eq!(r.failures[1].name, "error");
        assert_eq!(r.failures[1].file.as_deref(), Some("src/lib.rs"));
    }

    #[test]
    fn parse_pytest_short_summary() {
        let out = "\
=================== short test summary info ====================
FAILED tests/test_math.py::test_add - assert 4 == 5
FAILED tests/test_str.py::test_upper - AssertionError
==================== 2 failed, 7 passed in 0.12s ====================
";
        let r = parse_report(Framework::Pytest, VerifyKind::Test, out, "");
        assert_eq!(r.failed, 2);
        assert_eq!(r.passed, 7);
        assert_eq!(r.failures[0].name, "tests/test_math.py::test_add");
        assert_eq!(r.failures[0].file.as_deref(), Some("tests/test_math.py"));
        assert_eq!(r.failures[0].message, "assert 4 == 5");
    }

    #[test]
    fn parse_node_test_jest() {
        let out = "\
  ● math › adds two numbers
    expect(received).toBe(expected)
      at Object.<anonymous> (src/math.test.js:10:20)

Tests:       2 failed, 5 passed, 7 total
";
        let r = parse_report(Framework::NodeTest, VerifyKind::Test, out, "");
        assert_eq!(r.failed, 2);
        assert_eq!(r.passed, 5);
        assert_eq!(r.failures[0].name, "math › adds two numbers");
    }

    #[test]
    fn parse_go_test_fail() {
        let out = "\
=== RUN   TestAdd
--- FAIL: TestAdd (0.00s)
    math_test.go:12: got 4 want 5
=== RUN   TestOk
--- PASS: TestOk (0.00s)
FAIL
";
        let r = parse_report(Framework::GoTest, VerifyKind::Test, out, "");
        assert_eq!(r.failed, 1);
        assert_eq!(r.passed, 1);
        assert_eq!(r.failures[0].name, "TestAdd");
        assert_eq!(r.failures[0].file.as_deref(), Some("math_test.go"));
        assert_eq!(r.failures[0].line, Some(12));
        assert_eq!(r.failures[0].message, "got 4 want 5");
    }

    #[test]
    fn signature_stable_for_same_failures() {
        let r1 = parse_report(
            Framework::CargoTest,
            VerifyKind::Test,
            "test a::b ... FAILED\ntest c::d ... FAILED\ntest result: FAILED. 0 passed; 2 failed; 0 ignored",
            "",
        );
        // 失败名顺序不同也应同签名(集合化)。
        let r2 = parse_report(
            Framework::CargoTest,
            VerifyKind::Test,
            "test c::d ... FAILED\ntest a::b ... FAILED\ntest result: FAILED. 0 passed; 2 failed; 0 ignored",
            "",
        );
        assert_eq!(
            report_signature(&r1, Some(101), false),
            report_signature(&r2, Some(101), false)
        );
    }

    #[test]
    fn signature_changes_when_failures_change() {
        let r1 = parse_report(Framework::CargoTest, VerifyKind::Test, "test a::b ... FAILED", "");
        let r2 = parse_report(Framework::CargoTest, VerifyKind::Test, "test a::b ... FAILED\ntest e::f ... FAILED", "");
        assert_ne!(
            report_signature(&r1, Some(101), false),
            report_signature(&r2, Some(101), false)
        );
    }

    #[test]
    fn focused_command_cargo_and_pytest() {
        let cargo = VerifyStep {
            command: "cargo test".into(),
            kind: VerifyKind::Test,
            framework: Framework::CargoTest,
        };
        assert_eq!(
            focused_command(&cargo, &["tests::a".into(), "tests::b".into()]),
            Some("cargo test -- tests::a tests::b".into())
        );
        let py = VerifyStep {
            command: "pytest".into(),
            kind: VerifyKind::Test,
            framework: Framework::Pytest,
        };
        assert_eq!(
            focused_command(&py, &["t/x.py::test_a".into()]),
            Some("pytest t/x.py::test_a".into())
        );
        // 编译门不窄跑。
        let build = VerifyStep {
            command: "cargo check".into(),
            kind: VerifyKind::Build,
            framework: Framework::CargoCheck,
        };
        assert_eq!(focused_command(&build, &["x".into()]), None);
    }

    #[test]
    fn diagnostics_plan_two_stage_syntax() {
        let dir = std::env::temp_dir().join(format!("mdga_r3_diag_{}", std::process::id()));
        let mdga = dir.join(".mdga");
        std::fs::create_dir_all(&mdga).unwrap();
        std::fs::write(
            mdga.join("diagnostics"),
            "# comment\nbuild: cargo check\ntest: cargo test --workspace\n",
        )
        .unwrap();
        let plan = detect_verification_plan(dir.to_str().unwrap()).unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].kind, VerifyKind::Build);
        assert_eq!(plan.steps[0].framework, Framework::CargoCheck);
        assert_eq!(plan.steps[1].kind, VerifyKind::Test);
        assert_eq!(plan.steps[1].framework, Framework::CargoTest);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diagnostics_plan_bare_line_backcompat() {
        let dir = std::env::temp_dir().join(format!("mdga_r3_bare_{}", std::process::id()));
        let mdga = dir.join(".mdga");
        std::fs::create_dir_all(&mdga).unwrap();
        std::fs::write(mdga.join("diagnostics"), "npm test\n").unwrap();
        let plan = detect_verification_plan(dir.to_str().unwrap()).unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].kind, VerifyKind::Test);
        assert_eq!(plan.steps[0].command, "npm test");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn format_feedback_lists_failures() {
        let r = parse_report(
            Framework::CargoTest,
            VerifyKind::Test,
            "test a::b ... FAILED\n---- a::b stdout ----\nthread 'a::b' panicked at src/x.rs:5:1:\nboom\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored",
            "",
        );
        let fb = format_verify_feedback("cargo test", &r);
        assert!(fb.contains("测试门"));
        assert!(fb.contains("a::b @ src/x.rs:5 — boom"));
    }

    #[test]
    fn format_feedback_falls_back_to_raw_tail() {
        let r = TestReport::empty(Framework::Generic, VerifyKind::Build, "weird linker error".into());
        let fb = format_verify_feedback("make", &r);
        assert!(fb.contains("weird linker error"));
        assert!(fb.contains("编译门"));
    }
}
