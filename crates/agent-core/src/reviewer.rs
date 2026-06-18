//! R9 finalize 前「execution-free 评审」：在 agent 即将收尾（不再调用工具）且本轮发生过写类
//! 改动、且验证回路（若有）已通过时，跑**一次**纯模型评审——把本轮累计的变更 diff 连同评审
//! 准则交给主模型，让它只读 diff 找出**确凿的阻断性问题**（正确性回归 / 明显 bug /
//! 密钥泄漏 / 残留调试或 TODO / 不完整编辑），有则回灌让模型修复一轮，无则放行收尾。
//!
//! 本模块是**纯逻辑**（零 IO、零 Tauri、零正则）：只提供
//! - [`REVIEW_RUBRIC`]：评审提示词常量（system 角色），约束模型只看 diff、只报确凿问题，
//!   并用固定协议（`REVIEW: CLEAN` / `REVIEW: ISSUES`）回复，便于机械解析；
//! - [`parse_review`]：把模型回复解析成 [`ReviewVerdict`]（Clean / Issues(正文)），
//!   解析「干净 vs 有问题」的判定全在此函数、可独立单测。
//!
//! 实际的模型调用（chat_completion_with_retry）与回灌（push user 消息）仍由桌面端
//! agent_loop 编排；本模块只负责「准则」与「判定」两件可移植的纯事。

/// 评审准则提示词（R9）：作为 system 消息发给主模型。刻意语言/生态无关，只让模型基于给定的
/// 变更 diff 做**只读**评审，且用固定首行协议作答，便于 [`parse_review`] 机械解析、绝不误判。
///
/// 设计要点：
/// - 明确「只看 diff、不臆测 diff 外的代码」，避免模型对未展示上下文瞎报；
/// - 只报**确凿的阻断性**问题（宁缺毋滥），把风格 / 主观偏好排除在外，避免无谓返工；
/// - 固定协议：干净则首行恰为 `REVIEW: CLEAN`；有问题则首行 `REVIEW: ISSUES`，随后逐条列出。
pub const REVIEW_RUBRIC: &str = "你是一名严格但克制的代码评审者。下面给出的是本轮 agent 对工作区所做改动的**变更 diff**。\
请仅基于这些 diff 做一次**只读评审**（不要调用任何工具、不要假设 diff 之外的代码、不要臆测未展示的上下文），\
只挑出**确凿的、阻断性的**问题，覆盖以下方面：\
（1）正确性回归：改动引入的明显逻辑错误、把原本可用的行为改坏、边界/空值/错误处理被破坏；\
（2）明显 bug：类型不匹配、用错变量、缺失分支、会 panic/崩溃或编译不过的写法；\
（3）安全与密钥泄漏：硬编码的密钥/令牌/口令/私钥、注入风险、危险命令、把机密写进日志或源码；\
（4）残留调试痕迹：被遗忘的调试打印、临时 hack、注释掉的代码块、`TODO`/`FIXME`/`XXX` 标记被一并提交；\
（5）不完整编辑：半截改动、引用了未定义/未导入的符号、明显未收尾（如 `unimplemented!`、空函数体、被截断的逻辑）。\
\n\n严格遵守：宁缺毋滥——只报你**有把握**的阻断性问题；纯风格、命名偏好、可改可不改的「优化」一律不报。\
若 diff 看起来正确、完整、安全，**不要**硬找问题。\
\n\n回复协议（务必严格遵守，首行用于机器解析）：\
\n- 若**没有**任何阻断性问题：第一行只输出 `REVIEW: CLEAN`，之后不要再写任何内容。\
\n- 若**存在**阻断性问题：第一行只输出 `REVIEW: ISSUES`，从第二行起用简短中文逐条列出每个问题\
（每条指明涉及的文件/符号与具体毛病，必要时给最小修复建议）。只列确凿问题，不要凑数。";

/// 评审结论：要么干净（放行收尾），要么有确凿问题（携带逐条正文，回灌让模型修复一轮）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewVerdict {
    /// 评审通过，无阻断性问题，可直接收尾。
    Clean,
    /// 评审发现确凿的阻断性问题；`String` 为去掉协议首行后的逐条问题正文。
    Issues(String),
}

impl ReviewVerdict {
    /// 是否为「有阻断性问题」结论（便于调用方一行判断是否需要回灌再修一轮）。
    pub fn has_issues(&self) -> bool {
        matches!(self, ReviewVerdict::Issues(_))
    }
}

/// 把模型的评审回复解析成 [`ReviewVerdict`]——「干净 vs 有问题」的判定全在此函数。
///
/// 判定规则（**fail-open / 偏向放行**，绝不无故拦截收尾）：
/// - 找到首个非空行作为「协议行」（容忍模型在前面输出空行）。
/// - 该行（去首尾空白、大小写不敏感）以 `REVIEW: ISSUES` 开头 → [`ReviewVerdict::Issues`]，
///   正文取协议行**之后**的剩余内容（去除首尾空白）；若之后无正文，则回退为整段去掉协议行的内容；
///   仍为空时也判 Issues，正文给一句兜底说明（极少见：模型只回了 ISSUES 头）。
/// - 该行以 `REVIEW: CLEAN` 开头 → [`ReviewVerdict::Clean`]。
/// - 两种协议头都没命中（模型没按协议作答）→ 一律判 [`ReviewVerdict::Clean`]：宁可漏报也不
///   把不可解析的回复当阻断，避免误拦正常收尾（与「评审调用出错就 fail-open 收尾」口径一致）。
/// - 完全空回复 → [`ReviewVerdict::Clean`]。
pub fn parse_review(reply: &str) -> ReviewVerdict {
    // 找到首个非空行（容忍前导空行 / 模型寒暄换行）及其在原文中的字节起点。
    let Some((line_start, marker_line)) = first_nonempty_line(reply) else {
        return ReviewVerdict::Clean; // 空回复：放行。
    };
    let normalized = marker_line.trim();
    let upper = normalized.to_ascii_uppercase();

    if upper.starts_with("REVIEW: ISSUES") || upper.starts_with("REVIEW:ISSUES") {
        // 协议行之后的剩余正文 = 逐条问题清单。
        let after_marker_offset = line_start + marker_line.len();
        let body = reply
            .get(after_marker_offset..)
            .unwrap_or("")
            .trim()
            .to_string();
        if !body.is_empty() {
            return ReviewVerdict::Issues(body);
        }
        // 协议行后没正文：退而取整段去掉协议行后的剩余（极少见的排版）。
        let fallback = strip_marker_line(reply, line_start, marker_line);
        if !fallback.is_empty() {
            return ReviewVerdict::Issues(fallback);
        }
        // 连兜底都空：模型只回了个 ISSUES 头。仍判有问题，给一句通用提示。
        return ReviewVerdict::Issues(
            "评审标记为存在问题但未给出具体条目，请重新检查本轮改动的正确性、完整性与安全性后再收尾。"
                .to_string(),
        );
    }

    // CLEAN 或任何未按协议作答的回复：放行（fail-open）。
    ReviewVerdict::Clean
}

/// 取字符串中首个「非空白」行，返回（该行在原文中的字节起点, 该行内容含原始空白）。
fn first_nonempty_line(text: &str) -> Option<(usize, &str)> {
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        // line 含结尾 '\n'；判定与取内容都用去掉换行后的切片。
        let content = line.strip_suffix('\n').unwrap_or(line);
        let content = content.strip_suffix('\r').unwrap_or(content);
        if !content.trim().is_empty() {
            return Some((offset, content));
        }
        offset += line.len();
    }
    None
}

/// 去掉协议首行后返回剩余正文（去首尾空白）。用于「协议行后无正文但前后另有内容」的兜底。
fn strip_marker_line(text: &str, line_start: usize, marker_line: &str) -> String {
    let before = text.get(..line_start).unwrap_or("");
    let after_offset = line_start + marker_line.len();
    let after = text.get(after_offset..).unwrap_or("");
    format!("{before}{after}").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_verdict() {
        assert_eq!(parse_review("REVIEW: CLEAN"), ReviewVerdict::Clean);
        // 大小写不敏感 + 前导空行 + 行尾多余空白都应判干净。
        assert_eq!(parse_review("\n\n  review: clean  \n"), ReviewVerdict::Clean);
    }

    #[test]
    fn parses_issues_with_body() {
        let reply = "REVIEW: ISSUES\n1) src/a.rs 用了未导入的 Foo\n2) 残留 println! 调试输出";
        match parse_review(reply) {
            ReviewVerdict::Issues(body) => {
                assert!(body.contains("未导入的 Foo"));
                assert!(body.contains("残留 println!"));
                // 协议首行不应混进正文。
                assert!(!body.contains("REVIEW"));
            }
            other => panic!("应解析为 Issues，实际 {other:?}"),
        }
        assert!(parse_review(reply).has_issues());
    }

    #[test]
    fn issues_header_only_still_blocks() {
        // 只回了 ISSUES 头、没列条目：仍判有问题（给兜底正文），绝不误放行。
        match parse_review("REVIEW: ISSUES") {
            ReviewVerdict::Issues(body) => assert!(!body.is_empty()),
            other => panic!("仅 ISSUES 头也应判 Issues，实际 {other:?}"),
        }
    }

    #[test]
    fn unparseable_reply_fails_open_to_clean() {
        // 模型没按协议作答（闲聊 / 噪声）→ fail-open 放行，不拦截收尾。
        assert_eq!(parse_review("看起来还行，没什么大问题。"), ReviewVerdict::Clean);
        assert_eq!(parse_review(""), ReviewVerdict::Clean);
        assert_eq!(parse_review("   \n  \t \n"), ReviewVerdict::Clean);
    }

    #[test]
    fn issues_marker_must_be_on_first_nonempty_line() {
        // 「ISSUES」字样若不在协议首行（出现在正文里），不应误判为有问题。
        let reply = "REVIEW: CLEAN\n（备注：本来担心有 ISSUES，但复查后没有）";
        assert_eq!(parse_review(reply), ReviewVerdict::Clean);
    }
}
