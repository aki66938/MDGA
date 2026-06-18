//! R7：浏览器 / computer-use 工具——让 Agent 能验证它自己构建的 Web UI。
//!
//! 用一个无头 Chrome 驱动（[`headless_chrome`]，走 Chrome DevTools Protocol）暴露一组
//! 「读多写少」的浏览器工具：navigate / screenshot / click / fill / read_text / console。
//! 视觉模型据此「看到」页面（截图）并核对它生成的前端是否真的渲染正确。
//!
//! 设计要点：
//! - **会话级单浏览器**：所有工具共享一个进程内的 `Browser` + 单个 `Tab`，靠
//!   [`BrowserSession`] 这个全局惰性单例持有。navigate 切换当前页；其余工具作用于当前页。
//! - **干净关闭，绝不泄漏 chrome**：`headless_chrome::Browser` 在 Drop 时会 kill 子进程；
//!   我们把它放进一个进程级 `OnceLock<Mutex<Option<...>>>`，进程退出时随之释放；并提供
//!   [`shutdown`] 主动回收。每次操作都有超时（导航/查找元素），杜绝挂死。
//! - **安全**：只允许 `http`/`https` 且模型显式给出的 URL（拒绝 file://、about:、data: 等）；
//!   典型场景是 Agent 测自己的 localhost 应用。不做任意命令执行；上层把这些工具按
//!   NetworkAccess 能力门控。
//! - **运行期需要 Chrome/Chromium**：找不到二进制时返回清晰错误（而非 panic）；测试据此跳过。

use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use thiserror::Error;

/// 单次浏览器操作的超时（导航 / 等待加载 / 查找元素），杜绝挂死。
const OP_TIMEOUT: Duration = Duration::from_secs(30);
/// read_text / console 回传文本体积上限（字符），避免塞爆上下文。
const MAX_TEXT_CHARS: usize = 20_000;
/// console / network 日志各自保留的尾部条数。
const MAX_LOG_ENTRIES: usize = 100;

#[derive(Debug, Error)]
pub enum BrowserError {
    /// 找不到可用的 Chrome/Chromium 二进制（运行期前置缺失）——测试据此跳过。
    #[error("未找到 Chrome/Chromium 浏览器：{0}（请安装 Chrome 或 Chromium 后重试）")]
    ChromeNotFound(String),
    /// 启动浏览器失败（非「找不到二进制」的其它启动错误）。
    #[error("启动无头浏览器失败：{0}")]
    LaunchFailed(String),
    /// URL 非法 / 协议不被允许。
    #[error("非法 URL：{0}")]
    InvalidUrl(String),
    /// 当前没有已打开的页面（需先 browser_navigate）。
    #[error("当前没有已打开的页面，请先调用 browser_navigate 打开一个 URL")]
    NoActivePage,
    /// CDP / 页面操作出错（导航、查找元素、点击、求值等）。
    #[error("浏览器操作失败：{0}")]
    Operation(String),
}

// ── 请求类型 ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserNavigateRequest {
    /// 要打开的 http/https URL（典型为 Agent 自己的本地应用，如 http://localhost:5173）。
    pub url: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserScreenshotRequest {
    /// 为 true 时截整页（含滚动区域），否则只截当前视口。默认 false。
    #[serde(default)]
    pub full_page: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserClickRequest {
    /// CSS 选择器，定位要点击的元素。
    pub selector: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserFillRequest {
    /// CSS 选择器，定位要填写的输入控件。
    pub selector: String,
    /// 要键入的文本。
    pub text: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserReadTextRequest {}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserConsoleRequest {}

// ── 结果类型 ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserNavigateResult {
    pub url: String,
    pub title: String,
    /// 主文档 HTTP 状态码（拿不到时为 None）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserScreenshotResult {
    /// PNG 截图的 base64（无 data: 前缀），供视觉模型查看。
    pub screenshot_base64: String,
    /// base64 字符数（便于上层判断体积）。
    pub bytes_base64_len: usize,
    /// 截图时所在页面的 URL。
    pub url: String,
    /// 是否整页截图。
    pub full_page: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserActionResult {
    /// 实际作用的选择器。
    pub selector: String,
    /// 人读说明。
    pub note: String,
    /// 操作后页面 URL（点击可能触发跳转）。
    pub url: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserReadTextResult {
    /// 页面可见文本（document.body.innerText，可能被截断）。
    pub text: String,
    /// 文本是否被截断。
    pub truncated: bool,
    pub url: String,
    pub title: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserConsoleEntry {
    /// 级别：log / warn / error / info / debug 等。
    pub level: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserConsoleResult {
    /// 最近的 console 消息（尾部，最多 MAX_LOG_ENTRIES 条）。
    pub console: Vec<BrowserConsoleEntry>,
    /// 最近的网络失败（加载失败的请求 URL + 原因，尾部）。
    pub network_failures: Vec<String>,
    pub url: String,
}

// ── 会话单例 ────────────────────────────────────────────────────────────────

/// 进程级浏览器会话：持有一个 `Browser` 与当前 `Tab`，被所有工具共享。
struct BrowserSession {
    /// 仅作 RAII 守卫持有——`Browser` Drop 时 kill chrome 子进程，杜绝泄漏；不直接读取。
    #[allow(dead_code)]
    browser: headless_chrome::Browser,
    tab: std::sync::Arc<headless_chrome::Tab>,
}

/// 全局惰性单例。`None` 表示尚未启动 / 已 shutdown。
fn session_cell() -> &'static Mutex<Option<BrowserSession>> {
    static CELL: OnceLock<Mutex<Option<BrowserSession>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

/// 启动一个新的无头浏览器会话（带超时与干净关闭语义）。
fn launch_session() -> Result<BrowserSession, BrowserError> {
    use headless_chrome::{Browser, LaunchOptionsBuilder};

    let options = LaunchOptionsBuilder::default()
        .headless(true)
        // 给 CDP 连接握手留足时间。
        .idle_browser_timeout(Duration::from_secs(60))
        // localhost 自签名 / http 场景常见，忽略证书错误以便测自己的应用。
        .args(vec![std::ffi::OsStr::new("--ignore-certificate-errors")])
        .build()
        .map_err(|e| BrowserError::LaunchFailed(e.to_string()))?;

    let browser = Browser::new(options).map_err(|e| classify_launch_error(&e.to_string()))?;
    let tab = browser
        .new_tab()
        .map_err(|e| BrowserError::LaunchFailed(e.to_string()))?;
    Ok(BrowserSession { browser, tab })
}

/// 把启动错误分类：含「找不到/未安装 chrome」语义的归为 ChromeNotFound（测试据此跳过），
/// 其余归为 LaunchFailed。
fn classify_launch_error(msg: &str) -> BrowserError {
    let low = msg.to_lowercase();
    let looks_missing = low.contains("could not auto detect")
        || low.contains("cannot find")
        || low.contains("not find")
        || low.contains("no chrome")
        || low.contains("fetcher")
        || (low.contains("chrome") && (low.contains("not found") || low.contains("missing")));
    if looks_missing {
        BrowserError::ChromeNotFound(msg.to_string())
    } else {
        BrowserError::LaunchFailed(msg.to_string())
    }
}

/// 探测当前环境是否存在可用的 Chrome/Chromium（不真正建立 CDP 会话）。
///
/// 给测试与上层做「优雅跳过」用：返回 false 时不应尝试任何浏览器操作。
pub fn chrome_available() -> bool {
    // headless_chrome 复用 which/常见安装路径来定位 chrome；这里借它的发现逻辑判断。
    headless_chrome::browser::default_executable().is_ok()
}

/// 取得（必要时启动）会话，并对其执行闭包；闭包返回的结果原样透传。
///
/// 会话被全局 Mutex 串行化——浏览器操作本就不宜并发同一个 tab，串行更稳。
fn with_session<T>(
    f: impl FnOnce(&BrowserSession) -> Result<T, BrowserError>,
) -> Result<T, BrowserError> {
    let mut guard = session_cell().lock().expect("browser session mutex poisoned");
    if guard.is_none() {
        *guard = Some(launch_session()?);
    }
    // 若已有会话但底层进程已死，重启一次。
    let needs_restart = match guard.as_ref() {
        Some(s) => s.tab.get_target_info().is_err(),
        None => true,
    };
    if needs_restart {
        *guard = Some(launch_session()?);
    }
    let session = guard.as_ref().expect("session present after init");
    f(session)
}

/// 主动关闭会话并回收 chrome 子进程（Drop 即 kill，这里显式清空单例）。
pub fn shutdown() {
    if let Ok(mut guard) = session_cell().lock() {
        // 取出后离开作用域即 Drop -> kill chrome。
        let _ = guard.take();
    }
}

// ── URL 安全校验 ──────────────────────────────────────────────────────────────

/// 仅允许模型显式给出的 http/https URL；拒绝 file://、about:、data:、javascript: 等。
fn validate_url(url: &str) -> Result<String, BrowserError> {
    let u = url.trim();
    if u.is_empty() {
        return Err(BrowserError::InvalidUrl("URL 不能为空".to_string()));
    }
    let lower = u.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        // 简单结构校验：scheme 之后须有 host。
        let after = &u[u.find("//").map(|i| i + 2).unwrap_or(u.len())..];
        if after.is_empty() || after.starts_with('/') {
            return Err(BrowserError::InvalidUrl(format!("缺少主机名：{url}")));
        }
        Ok(u.to_string())
    } else if lower.contains("://") {
        Err(BrowserError::InvalidUrl(format!(
            "只允许 http/https，拒绝：{url}"
        )))
    } else if has_non_http_scheme(u) {
        // 单冒号危险 scheme（about: / data: / javascript: / file: / blob: 等）须拒绝，
        // 不能误当成「host:port」补 http://。
        Err(BrowserError::InvalidUrl(format!(
            "只允许 http/https，拒绝：{url}"
        )))
    } else {
        // 无 scheme：默认补 http://（便于模型直接给 localhost:5173）。
        let candidate = format!("http://{u}");
        validate_url(&candidate)
    }
}

/// 判断字符串是否带一个「非 http/https 的 URL scheme」前缀（单冒号形式，如 `about:`、
/// `javascript:`、`data:`、`file:`）。区分 `host:port`：冒号后紧跟数字视为端口，不算 scheme。
fn has_non_http_scheme(s: &str) -> bool {
    let Some(colon) = s.find(':') else {
        return false;
    };
    let scheme = &s[..colon];
    // 合法 scheme：以字母起头，仅含字母/数字/`+`/`-`/`.`，且非空。
    let looks_scheme = !scheme.is_empty()
        && scheme.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false)
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.');
    if !looks_scheme {
        return false;
    }
    // 冒号后紧跟数字 → 当作 host:port（如 localhost:5173），不是 scheme。
    let after = &s[colon + 1..];
    if after.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
        return false;
    }
    // 走到这里：是个非 http/https 的单冒号 scheme（http/https 已在上面的分支处理）。
    true
}

// ── 工具实现 ────────────────────────────────────────────────────────────────

/// 注入「在每个新文档上劫持 console.* 写入 window.__mdgaConsole」的脚本。
///
/// 用 AddScriptToEvaluateOnNewDocument，使其在导航后的每个页面早于业务脚本执行，
/// 从而捕获页面自身打印的 log/warn/error。失败仅记日志、不阻断导航（console 工具会兜底返回空）。
fn inject_console_hook(tab: &headless_chrome::Tab) {
    use headless_chrome::protocol::cdp::Page::AddScriptToEvaluateOnNewDocument;
    let source = r#"
        (function(){
          if (window.__mdgaConsoleHooked) return;
          window.__mdgaConsoleHooked = true;
          window.__mdgaConsole = window.__mdgaConsole || [];
          var levels = ['log','info','warn','error','debug'];
          levels.forEach(function(level){
            var orig = console[level];
            console[level] = function(){
              try {
                var parts = Array.prototype.slice.call(arguments).map(function(a){
                  try { return typeof a === 'string' ? a : JSON.stringify(a); }
                  catch (e) { return String(a); }
                });
                window.__mdgaConsole.push({ level: level, text: parts.join(' ') });
                if (window.__mdgaConsole.length > 500) window.__mdgaConsole.shift();
              } catch (e) {}
              if (orig) return orig.apply(console, arguments);
            };
          });
          window.addEventListener('error', function(e){
            try { window.__mdgaConsole.push({ level: 'error', text: String(e.message || e.error || e) }); } catch (x) {}
          });
        })();
    "#;
    let _ = tab.call_method(AddScriptToEvaluateOnNewDocument {
        source: source.to_string(),
        world_name: None,
        include_command_line_api: None,
        run_immediately: Some(true),
    });
}

/// browser_navigate：打开/加载 URL，返回最终 URL、标题与主文档状态码。
pub fn browser_navigate(
    request: BrowserNavigateRequest,
) -> Result<BrowserNavigateResult, BrowserError> {
    let url = validate_url(&request.url)?;
    with_session(|s| {
        let tab = &s.tab;
        tab.set_default_timeout(OP_TIMEOUT);
        inject_console_hook(tab);
        tab.navigate_to(&url)
            .map_err(|e| BrowserError::Operation(format!("导航失败：{e}")))?;
        tab.wait_until_navigated()
            .map_err(|e| BrowserError::Operation(format!("等待页面加载失败：{e}")))?;
        let final_url = tab.get_url();
        let title = tab.get_title().unwrap_or_default();
        Ok(BrowserNavigateResult {
            url: final_url,
            title,
            // headless_chrome 不直接暴露主文档状态码；保留字段，置 None（导航成功即视为可达）。
            status: None,
        })
    })
}

/// browser_screenshot：对当前页面截 PNG，返回 base64，供视觉模型查看。
pub fn browser_screenshot(
    request: BrowserScreenshotRequest,
) -> Result<BrowserScreenshotResult, BrowserError> {
    use base64::Engine as _;
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption;
    with_session(|s| {
        let tab = &s.tab;
        tab.set_default_timeout(OP_TIMEOUT);
        let data = tab
            .capture_screenshot(
                CaptureScreenshotFormatOption::Png,
                None,
                None,
                request.full_page,
            )
            .map_err(|e| BrowserError::Operation(format!("截图失败：{e}")))?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
        let len = b64.len();
        Ok(BrowserScreenshotResult {
            screenshot_base64: b64,
            bytes_base64_len: len,
            url: tab.get_url(),
            full_page: request.full_page,
        })
    })
}

/// browser_click：按 CSS 选择器点击元素。
pub fn browser_click(request: BrowserClickRequest) -> Result<BrowserActionResult, BrowserError> {
    let selector = request.selector.trim().to_string();
    if selector.is_empty() {
        return Err(BrowserError::Operation("selector 不能为空".to_string()));
    }
    with_session(|s| {
        let tab = &s.tab;
        tab.set_default_timeout(OP_TIMEOUT);
        let el = tab
            .find_element(&selector)
            .map_err(|e| BrowserError::Operation(format!("未找到元素 {selector}：{e}")))?;
        el.click()
            .map_err(|e| BrowserError::Operation(format!("点击 {selector} 失败：{e}")))?;
        Ok(BrowserActionResult {
            selector: selector.clone(),
            note: format!("已点击 {selector}"),
            url: tab.get_url(),
        })
    })
}

/// browser_fill：聚焦输入控件并键入文本（先清空已有值）。
pub fn browser_fill(request: BrowserFillRequest) -> Result<BrowserActionResult, BrowserError> {
    let selector = request.selector.trim().to_string();
    if selector.is_empty() {
        return Err(BrowserError::Operation("selector 不能为空".to_string()));
    }
    with_session(|s| {
        let tab = &s.tab;
        tab.set_default_timeout(OP_TIMEOUT);
        let el = tab
            .find_element(&selector)
            .map_err(|e| BrowserError::Operation(format!("未找到元素 {selector}：{e}")))?;
        // 先聚焦并清空已有内容（全选+删除），再键入，避免追加到旧值后面。
        el.click()
            .map_err(|e| BrowserError::Operation(format!("聚焦 {selector} 失败：{e}")))?;
        // 清空：用 JS 把 value 置空（对 input/textarea 通用），再键入。
        let _ = el.call_js_fn(
            "function() { if ('value' in this) { this.value = ''; } }",
            vec![],
            false,
        );
        el.type_into(&request.text)
            .map_err(|e| BrowserError::Operation(format!("填写 {selector} 失败：{e}")))?;
        Ok(BrowserActionResult {
            selector: selector.clone(),
            note: format!("已向 {selector} 填入文本（{} 字符）", request.text.chars().count()),
            url: tab.get_url(),
        })
    })
}

/// browser_read_text：返回当前页面可见文本（document.body.innerText），超长截断。
pub fn browser_read_text(
    _request: BrowserReadTextRequest,
) -> Result<BrowserReadTextResult, BrowserError> {
    with_session(|s| {
        let tab = &s.tab;
        tab.set_default_timeout(OP_TIMEOUT);
        let result = tab
            .evaluate(
                "document.body ? document.body.innerText : ''",
                false,
            )
            .map_err(|e| BrowserError::Operation(format!("读取页面文本失败：{e}")))?;
        let raw = result
            .value
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let (text, truncated) = truncate_chars(&raw, MAX_TEXT_CHARS);
        Ok(BrowserReadTextResult {
            text,
            truncated,
            url: tab.get_url(),
            title: tab.get_title().unwrap_or_default(),
        })
    })
}

/// browser_console：拉取 console + 网络失败日志尾部。
///
/// 实现：在页面侧维护一个 `window.__mdgaConsole` 缓冲（首次注入劫持 console.*），
/// 每次调用从中读出尾部条目。网络失败用 Performance Resource Timing 近似（无法取到的从略）。
pub fn browser_console(
    _request: BrowserConsoleRequest,
) -> Result<BrowserConsoleResult, BrowserError> {
    with_session(|s| {
        let tab = &s.tab;
        tab.set_default_timeout(OP_TIMEOUT);
        // 读出页面侧缓冲（若未注入则为 []）。注入在 navigate 时通过 add_script_to_evaluate 完成；
        // 这里兜底再尝试取一次。
        let js = r#"(function(){
            try {
                var c = (window.__mdgaConsole || []).slice(-100);
                var failed = [];
                try {
                    (performance.getEntriesByType('resource') || []).forEach(function(e){
                        if (e.responseStatus && e.responseStatus >= 400) {
                            failed.push(e.responseStatus + ' ' + e.name);
                        }
                    });
                } catch (e) {}
                return JSON.stringify({ console: c, failed: failed.slice(-100) });
            } catch (e) { return JSON.stringify({ console: [], failed: [] }); }
        })()"#;
        let result = tab
            .evaluate(js, false)
            .map_err(|e| BrowserError::Operation(format!("读取 console 日志失败：{e}")))?;
        let raw = result
            .value
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "{}".to_string());
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        let console = parsed
            .get("console")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| {
                        let level = e.get("level").and_then(|x| x.as_str()).unwrap_or("log");
                        let text = e.get("text").and_then(|x| x.as_str())?;
                        Some(BrowserConsoleEntry {
                            level: level.to_string(),
                            text: text.to_string(),
                        })
                    })
                    .take(MAX_LOG_ENTRIES)
                    .collect()
            })
            .unwrap_or_default();
        let network_failures = parsed
            .get("failed")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e.as_str().map(str::to_string))
                    .take(MAX_LOG_ENTRIES)
                    .collect()
            })
            .unwrap_or_default();
        Ok(BrowserConsoleResult {
            console,
            network_failures,
            url: tab.get_url(),
        })
    })
}

/// 按字符截断（保留 UTF-8 边界），返回 (截断后文本, 是否截断)。
fn truncate_chars(s: &str, max: usize) -> (String, bool) {
    if s.chars().count() <= max {
        (s.to_string(), false)
    } else {
        (s.chars().take(max).collect(), true)
    }
}

// ── 单元测试 ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_url_accepts_http_and_https() {
        assert_eq!(
            validate_url("http://localhost:5173").unwrap(),
            "http://localhost:5173"
        );
        assert_eq!(
            validate_url("https://example.com/a").unwrap(),
            "https://example.com/a"
        );
    }

    #[test]
    fn validate_url_adds_scheme_for_bare_host() {
        assert_eq!(
            validate_url("localhost:5173").unwrap(),
            "http://localhost:5173"
        );
        assert_eq!(validate_url("127.0.0.1:8080/x").unwrap(), "http://127.0.0.1:8080/x");
    }

    #[test]
    fn validate_url_rejects_dangerous_schemes() {
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("about:blank").is_err());
        assert!(validate_url("data:text/html,<h1>x</h1>").is_err());
        assert!(validate_url("javascript:alert(1)").is_err());
        assert!(validate_url("").is_err());
        assert!(validate_url("http://").is_err());
    }

    #[test]
    fn truncate_chars_caps_length() {
        let (t, trunc) = truncate_chars("hello", 10);
        assert_eq!(t, "hello");
        assert!(!trunc);
        let (t, trunc) = truncate_chars("hello world", 5);
        assert_eq!(t, "hello");
        assert!(trunc);
    }

    /// 起一个进程内长驻 HTTP 服务器（守护线程，循环应答，绝不 join），返回一行 HTML。
    /// 设计要点：Chrome 导航期会发多个请求（主页 + favicon 等），循环 accept 确保不被
    /// 「只应答一次」拖死；线程不 join、随进程退出，避免测试与服务器互等而挂死。
    fn serve_forever(html: &str) -> String {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().unwrap().port();
        let body = html.to_string();
        std::thread::spawn(move || loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buf = [0u8; 2048];
                    let _ = stream.read(&mut buf);
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.flush();
                }
                Err(_) => break,
            }
        });
        format!("http://127.0.0.1:{port}/")
    }

    /// 端到端冒烟：仅当本机存在 Chrome/Chromium 时运行（镜像 git_smoke 的可用性门）。
    /// 起进程内 HTTP 服务器 → navigate → read_text → screenshot → console，验证整条驱动链路。
    /// 无 Chrome 时优雅跳过。整个驱动调用放到独立线程并配看门狗超时，杜绝因导航异常而挂死测试。
    #[test]
    fn browser_smoke_navigate_read_screenshot() {
        if !chrome_available() {
            eprintln!("跳过：本机未找到 Chrome/Chromium，浏览器冒烟测试 skip");
            return;
        }
        let marker = "MDGA_R7_BROWSER_OK";
        let html = format!(
            "<!doctype html><html><head><title>R7 Smoke</title></head><body><h1>{marker}</h1><script>console.log('hello from page');</script></body></html>"
        );
        let url = serve_forever(&html);

        // 在独立线程里跑整条驱动链路，主线程用带超时的 recv 当看门狗：
        // 即便某步 CDP 调用异常挂起，测试也会超时失败（并 shutdown 回收 chrome），不会永久卡住。
        let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
        std::thread::spawn(move || {
            let run = || -> Result<(), String> {
                let nav = browser_navigate(BrowserNavigateRequest { url: url.clone() })
                    .map_err(|e| format!("navigate: {e}"))?;
                if nav.title != "R7 Smoke" {
                    return Err(format!("标题应为 'R7 Smoke'，实得 {:?}", nav.title));
                }
                let text = browser_read_text(BrowserReadTextRequest {})
                    .map_err(|e| format!("read_text: {e}"))?;
                if !text.text.contains(marker) {
                    return Err(format!("页面文本应含 {marker}，实得 {:?}", text.text));
                }
                let shot = browser_screenshot(BrowserScreenshotRequest { full_page: false })
                    .map_err(|e| format!("screenshot: {e}"))?;
                if shot.screenshot_base64.is_empty() {
                    return Err("截图 base64 不应为空".to_string());
                }
                // console 调用本身必须成功（捕获到具体条目与否因 Chrome 版本注入时机而异，不强求）。
                let _console = browser_console(BrowserConsoleRequest {})
                    .map_err(|e| format!("console: {e}"))?;
                Ok(())
            };
            let _ = tx.send(run());
        });

        // 看门狗：90s 仍无结果即判定挂死、测试失败。无论如何最后都 shutdown 回收 chrome。
        let outcome = rx.recv_timeout(Duration::from_secs(90));
        shutdown();
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(e)) => panic!("浏览器端到端冒烟失败：{e}"),
            Err(_) => panic!("浏览器端到端冒烟超时（>90s），疑似挂死"),
        }
    }
}
