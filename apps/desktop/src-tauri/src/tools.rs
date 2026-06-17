//! 内建工具：schema 定义、文件/命令工具的派发执行、技能加载、remember / todo_write、
//! 只读工具并行执行入口。
//!
//! 从 main.rs 抽出（Plan16）：纯代码搬移，无行为变更。

use crate::permissions::tool_capability_for_name;
use crate::web::{execute_web_fetch, execute_web_search};
use mdga_sandbox_runtime::SessionSecurityContext;
use mdga_tool_runtime::{
    code_overview, create_file, delete_dir, delete_file, edit_file, git_add, git_branch,
    git_commit, git_diff, git_log, git_status, glob_files, list_dir, make_dir, move_path, read_file,
    run_command, search_text, stat_path, write_file, CodeOverviewRequest, CreateFileRequest,
    DeleteDirRequest, DeleteFileRequest, EditFileRequest, GitAddRequest, GitBranchRequest,
    GitCommitRequest, GitDiffRequest, GitLogRequest, GitStatusRequest, GlobFilesRequest,
    ListDirRequest, MakeDirRequest, MovePathRequest, ReadFileRequest, RunCommandRequest,
    SearchTextRequest, StatPathRequest, WriteFileRequest,
};
use tauri::{AppHandle, Emitter};

/// 扫描工作区 .mdga/skills/*/SKILL.md，返回技能名与描述（首行 frontmatter 或首段）。
pub(crate) fn load_workspace_skills(workspace: &str) -> Vec<(String, String)> {
    let skills_dir = std::path::Path::new(workspace).join(".mdga").join("skills");
    let Ok(entries) = std::fs::read_dir(&skills_dir) else {
        return Vec::new();
    };
    let mut skills = Vec::new();
    for entry in entries.flatten().take(30) {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let skill_md = dir.join("SKILL.md");
        let Ok(content) = std::fs::read_to_string(&skill_md) else {
            continue;
        };
        // 描述：取 frontmatter 的 description: 行，否则取第一行非空文本。
        let description = content
            .lines()
            .find_map(|line| line.trim().strip_prefix("description:").map(|d| d.trim().to_string()))
            .or_else(|| {
                content
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty() && !l.starts_with("---") && !l.starts_with('#'))
                    .map(str::to_string)
            })
            .unwrap_or_default();
        skills.push((name, description));
    }
    skills
}

/// load_skill 工具：按名加载工作区技能的完整 SKILL.md 内容（按需注入，CC 渐进披露同款）。
pub(crate) fn execute_load_skill(workspace: &str, arguments: &str) -> Result<serde_json::Value, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(arguments).map_err(|e| format!("工具参数解析失败: {e}"))?;
    let name = parsed
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("load_skill 缺少 name")?;
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("技能名只允许字母数字、横线与下划线".to_string());
    }
    let path = std::path::Path::new(workspace)
        .join(".mdga")
        .join("skills")
        .join(name)
        .join("SKILL.md");
    let content = std::fs::read_to_string(&path).map_err(|_| format!("技能 {name} 不存在"))?;
    let capped: String = content.chars().take(32_000).collect();
    Ok(serde_json::json!({ "name": name, "skill": capped }))
}

/// remember 工具：把一条值得跨会话记住的事实追加到工作区 MDGA.md 的「自动记忆」区。
///
/// 让 Agent 在工作中自主沉淀经验（项目约定、踩过的坑、关键路径），下次会话自动注入。
/// 去重：同样内容已存在则不重复追加。
pub(crate) fn execute_remember(workspace: &str, arguments: &str) -> Result<serde_json::Value, String> {
    const SECTION: &str = "## 自动记忆（由 Agent 维护）";
    let parsed: serde_json::Value =
        serde_json::from_str(arguments).map_err(|e| format!("工具参数解析失败: {e}"))?;
    let fact = parsed
        .get("fact")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("remember 缺少 fact")?;
    if fact.chars().count() > 500 {
        return Err("单条记忆过长（上限 500 字符），请精炼".to_string());
    }
    let path = std::path::Path::new(workspace).join("MDGA.md");
    let mut content = std::fs::read_to_string(&path).unwrap_or_default();
    let entry = format!("- {fact}");
    if content.contains(&entry) {
        return Ok(serde_json::json!({ "note": "该记忆已存在，未重复添加" }));
    }
    if content.contains(SECTION) {
        // 在 section 标题后插入
        content = content.replacen(SECTION, &format!("{SECTION}\n{entry}"), 1);
    } else {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!("\n{SECTION}\n{entry}\n"));
    }
    std::fs::write(&path, content).map_err(|e| format!("写入 MDGA.md 失败: {e}"))?;
    Ok(serde_json::json!({ "note": "已记入 MDGA.md，下次会话自动生效", "fact": fact }))
}

/// todo_write 工具：更新任务清单并推送给前端实时展示，不触碰文件系统。
pub(crate) fn execute_todo_write(app: &AppHandle, arguments: &str) -> Result<serde_json::Value, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(arguments).map_err(|e| format!("工具参数解析失败: {e}"))?;
    let items = parsed
        .get("items")
        .and_then(|v| v.as_array())
        .ok_or("todo_write 需要 items 数组")?;
    if items.len() > 50 {
        return Err("todo 项过多（上限 50）".to_string());
    }
    let _ = app.emit("todo-update", serde_json::Value::Array(items.clone()));
    Ok(serde_json::json!({
        "count": items.len(),
        "note": "任务清单已更新并实时展示给用户"
    }))
}

/// 可并行执行的只读工具集合（无副作用，并发安全）。
/// code_overview（Plan28 P0-2）只读并统计代码结构，与 search_text 同属只读、可并行。
pub(crate) const PARALLEL_READONLY_TOOLS: &[&str] = &[
    "read_file",
    "list_dir",
    "search_text",
    "glob_files",
    "stat_path",
    "code_overview",
    "repo_map",
    "web_fetch",
    "web_search",
    // R4：git 只读工具，无副作用、可并行。
    "git_status",
    "git_diff",
    "git_log",
];

/// 执行一个只读工具调用（同步文件工具或异步 web 工具），供并行批量执行。
pub(crate) async fn execute_readonly_call(
    security_context: &SessionSecurityContext,
    tool_name: &str,
    arguments: &str,
) -> Result<serde_json::Value, String> {
    match tool_name {
        "web_fetch" => execute_web_fetch(arguments).await,
        "web_search" => execute_web_search(arguments).await,
        _ => execute_builtin_tool_call(security_context, tool_name, arguments),
    }
}

pub(crate) fn all_builtin_tool_schemas() -> Vec<serde_json::Value> {
    vec![
        file_tool_schema(
            "create_file",
            "Create a new text file inside the current MDGA conversation workspace. Use this when the user asks to create a file. The path must be relative to the workspace.",
            &["path", "content"],
        ),
        file_tool_schema(
            "write_file",
            "Write or overwrite a full UTF-8 text file inside the current MDGA conversation workspace. Use this only when the user asks to replace the whole file.",
            &["path", "content"],
        ),
        file_tool_schema(
            "edit_file",
            "Edit an existing UTF-8 text file by replacing oldText with newText. Prefer this for code or text modifications.",
            &["path", "oldText", "newText"],
        ),
        // apply_patch（Plan25 C-2）：对同一文件按顺序做多处精确替换，优于多次 edit_file。
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "apply_patch",
                "description": "Apply multiple precise text replacements to a SINGLE file in order. Each edit's oldText must match exactly once in the (current) file content; if any oldText is empty, missing, or matches more than once, the whole patch fails and nothing is written. Use this for several edits to one file at once — it is better than calling edit_file repeatedly.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path of the existing file inside the workspace." },
                        "edits": {
                            "type": "array",
                            "description": "Ordered list of replacements applied one after another to the file content.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "oldText": { "type": "string", "description": "Exact existing text to replace. Must be uniquely present in the current content at the time this edit runs." },
                                    "newText": { "type": "string", "description": "Replacement text." }
                                },
                                "required": ["oldText", "newText"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["path", "edits"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a UTF-8 text file inside the workspace. Returns up to ~1500 lines by default with total line count and has_more. For large files, page through with offset (0-based start line) and limit (lines).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path inside the workspace." },
                        "offset": { "type": "integer", "description": "0-based start line. Use with has_more to page large files. Default 0." },
                        "limit": { "type": "integer", "description": "Max lines to return (<= 4000). Default ~1500." }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        }),
        file_tool_schema(
            "delete_file",
            "Delete a single file inside the current MDGA conversation workspace. Use this when the user asks to remove a file.",
            &["path"],
        ),
        file_tool_schema(
            "list_dir",
            "List entries in a directory inside the current MDGA conversation workspace. Use this when the user asks what files or folders exist.",
            &["path"],
        ),
        file_tool_schema(
            "make_dir",
            "Create a directory inside the current MDGA conversation workspace.",
            &["path"],
        ),
        file_tool_schema(
            "stat_path",
            "Return whether a relative workspace path exists and whether it is a file or directory.",
            &["path"],
        ),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "search_text",
                "description": "Search file CONTENTS recursively inside a workspace directory (ripgrep-style, gitignore-aware, skips hidden/ignored files). Use this to find where text/code appears. For finding files by NAME/path pattern, use glob_files instead.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative directory to search in (e.g. \".\" for workspace root)." },
                        "query": { "type": "string", "description": "Search pattern (literal substring, or regex when isRegex=true)." },
                        "isRegex": { "type": "boolean", "description": "Interpret query as a regular expression. Default false." },
                        "outputMode": { "type": "string", "enum": ["content", "files_with_matches", "count"], "description": "content = matching lines (default); files_with_matches = just file paths; count = matches per file." },
                        "caseInsensitive": { "type": "boolean", "description": "Case-insensitive match (-i). Default false." },
                        "multiline": { "type": "boolean", "description": "Allow the pattern to span lines (. matches newlines). Default false." },
                        "context": { "type": "integer", "description": "Lines of context before AND after each match (-C). content mode only." },
                        "beforeContext": { "type": "integer", "description": "Lines of context before each match (-B). content mode only." },
                        "afterContext": { "type": "integer", "description": "Lines of context after each match (-A). content mode only." },
                        "fileType": { "type": "string", "description": "Restrict to a file type, e.g. \"rs\", \"ts\", \"py\", \"json\"." },
                        "glob": { "type": "string", "description": "Restrict to files whose name/path matches this glob, e.g. \"*.rs\" or \"src/**\"." },
                        "maxResults": { "type": "integer", "description": "Cap on returned matches/files/counts." }
                    },
                    "required": ["path", "query"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "glob_files",
                "description": "Find files by NAME/path glob pattern inside the workspace (gitignore-aware), returned newest-first. Use this to locate files (e.g. all \"*.rs\", everything under \"src/**\") rather than searching their contents.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Glob: supports * ? and ** (e.g. \"**/*.rs\", \"src/**\", \"*.toml\"). A pattern without \"/\" matches by file name in any directory." },
                        "path": { "type": "string", "description": "Relative directory to start from. Defaults to workspace root." },
                        "maxResults": { "type": "integer", "description": "Cap on returned file paths." }
                    },
                    "required": ["pattern"],
                    "additionalProperties": false
                }
            }
        }),
        // code_overview（Plan28 P0-2，Lane B）：语言无关的「求真」概览，给模型在下结论前拿事实。
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "code_overview",
                "description": "Get GROUNDED structural FACTS about a file, module, or package inside the workspace BEFORE concluding anything about it — especially before claiming it is an 'empty shell', has 'no real code', or has 'few tests'. Language-agnostic: returns lines of code, public/exported symbol counts, test counts, and detected build/dependency files, aggregated by language. For a directory or repo root it also lists detected packages/crates and suggests verify commands (e.g. 'cargo test --workspace', 'npm test', 'pytest', 'go test ./...') as STRINGS only (it does not run them). Use this instead of guessing from dependency manifests, directory listings, or file names — 'few dependencies' or 'small file' does NOT mean 'no code'. Lightweight (regex heuristics, no AST); large directories are capped and truncated.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path inside the workspace. Use \".\" for the workspace root; may be a file or a directory." }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        }),
        // repo_map（R2）：tree-sitter 抽取定义 + PageRank 引用排名的全仓符号地图。
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "repo_map",
                "description": "Get a RANKED repository map of the most important code definitions across the whole workspace, WITHOUT reading files one by one. Parses sources with tree-sitter (Rust, Python, JS, TS/TSX, Go) to extract definitions (functions, methods, types, classes, traits, etc.) and references, then ranks them with a personalized PageRank over the reference graph (the more a symbol is referenced by important files, the higher it ranks). Output lists files in importance order, each with its top definition signature lines and line numbers. Use this EARLY to orient in an unfamiliar or large codebase, to find where the core/most-referenced code lives, and to see who-depends-on-what — it complements glob_files/search_text (which find by name/text) and code_overview (which counts structure in one path). Pass focus_files and/or query to bias the map toward a specific area. Read-only; lightweight; large repos are capped and the result says so.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "focusFiles": { "type": "array", "items": { "type": "string" }, "description": "Optional workspace-relative file paths to focus the map around (PageRank is personalized toward these and their collaborators)." },
                        "query": { "type": "string", "description": "Optional free-text keywords; symbols whose names match are boosted and their defining files surfaced first." },
                        "maxTokens": { "type": "integer", "description": "Optional token budget for the rendered map (default 1500, clamped to 200–20000)." }
                    },
                    "required": [],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "move_path",
                "description": "Move or rename a file or directory inside the current MDGA conversation workspace. Use this for moving/renaming instead of create+delete.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "from": { "type": "string", "description": "Existing relative source path inside the workspace." },
                        "to": { "type": "string", "description": "New relative destination path inside the workspace. Must not already exist." }
                    },
                    "required": ["from", "to"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "delete_dir",
                "description": "Delete a directory inside the current MDGA conversation workspace. Set recursive=true to delete a non-empty directory.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative directory path inside the workspace. Cannot be the workspace root." },
                        "recursive": { "type": "boolean", "description": "Delete recursively including contents. Required true for non-empty directories. Defaults to false." }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "run_command",
                "description": "Run a single PowerShell command in the workspace directory. Low-risk commands (cargo check/test, npm test, git status/diff, dir) run directly under Workspace Auto; others need Full Access or user approval. Set background=true for long-running commands (servers, watchers): it returns immediately and the result is reported later.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "The PowerShell command line to execute." },
                        "timeoutSecs": { "type": "integer", "description": "Optional timeout in seconds, default 120, max 600." },
                        "background": { "type": "boolean", "description": "Run in background and return immediately. Defaults to false." }
                    },
                    "required": ["command"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_shell_output",
                "description": "Poll a background shell's accumulated output and status (running/done/killed/error). Use with the shellId returned by run_command background=true.",
                "parameters": { "type": "object", "properties": { "shellId": { "type": "string" } }, "required": ["shellId"], "additionalProperties": false }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "kill_shell",
                "description": "Terminate a running background shell by shellId.",
                "parameters": { "type": "object", "properties": { "shellId": { "type": "string" } }, "required": ["shellId"], "additionalProperties": false }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "list_shells",
                "description": "List all background shells with their id, command and status.",
                "parameters": { "type": "object", "properties": {}, "additionalProperties": false }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "todo_write",
                "description": "Maintain a visible todo list for the current multi-step task. Call this at the start of a complex task to plan steps, and update statuses as you progress (exactly one item in_progress at a time). The list is shown to the user as live progress.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "items": {
                            "type": "array",
                            "description": "The full todo list, replacing the previous one.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "text": { "type": "string", "description": "Short description of the step." },
                                    "status": { "type": "string", "enum": ["pending", "in_progress", "done"], "description": "Current status of this step." }
                                },
                                "required": ["text", "status"]
                            }
                        }
                    },
                    "required": ["items"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "ask_user",
                "description": "Ask the user 1-4 structured multiple-choice questions when requirements are genuinely ambiguous and guessing would risk doing the wrong work. The UI renders clickable option cards; an 'Other' free-text choice is always added automatically, and questions can allow multiple selections. Prefer this over assuming. Do NOT use it for anything you can determine yourself by reading files or running tools — only for real decisions that are the user's to make.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "questions": {
                            "type": "array",
                            "description": "1 to 4 questions to ask at once.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "question": { "type": "string", "description": "The full, specific question, ending with a question mark." },
                                    "header": { "type": "string", "description": "Very short label (<= 12 chars) shown as a chip, e.g. 'Library', 'Approach'." },
                                    "multiSelect": { "type": "boolean", "description": "Allow selecting multiple options. Defaults to false." },
                                    "options": {
                                        "type": "array",
                                        "description": "2 to 4 mutually-exclusive choices ('Other' is added automatically; do not add it yourself).",
                                        "items": {
                                            "type": "object",
                                            "properties": {
                                                "label": { "type": "string", "description": "Concise option text (1-5 words)." },
                                                "description": { "type": "string", "description": "What this option means or implies (trade-offs)." }
                                            },
                                            "required": ["label"]
                                        }
                                    }
                                },
                                "required": ["question", "options"]
                            }
                        }
                    },
                    "required": ["questions"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "load_skill",
                "description": "Load the full instructions of a workspace skill (from .mdga/skills/<name>/SKILL.md). Call this when the available-skills list (in system context) has a skill matching the current task.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Skill directory name from the available-skills list." }
                    },
                    "required": ["name"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "web_fetch",
                "description": "Fetch a web page or document by URL and return its readable text content. Use this to read documentation, articles, error references, or any known URL. http/https only.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "The http/https URL to fetch." }
                    },
                    "required": ["url"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web and get a list of result titles, URLs and snippets. Use this when you need to find current information, documentation, or solutions you don't already know. Follow up with web_fetch on the most relevant URLs.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "The search query." }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "add_mcp_server",
                "description": "Register and connect an MCP server in MDGA's real mechanism (not by editing config files). Use this when the user asks you to install/add an MCP server for yourself. command is either a stdio launch command (e.g. 'npx -y @modelcontextprotocol/server-memory') or an http(s):// URL. After it connects, its tools become callable as mcp_<server>_<tool>.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Short server name, e.g. memory, github." },
                        "command": { "type": "string", "description": "stdio launch command or http(s):// URL." },
                        "authToken": { "type": "string", "description": "Optional Bearer token for HTTP servers." }
                    },
                    "required": ["name", "command"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "remember",
                "description": "Persist a concise fact worth remembering across sessions (project convention, a gotcha you hit, a key file path). It is appended to the workspace MDGA.md and auto-injected in future sessions. Use sparingly for durable, reusable facts — not transient details.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "fact": { "type": "string", "description": "One concise fact to remember (<= 500 chars)." }
                    },
                    "required": ["fact"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "run_subtask",
                "description": "Delegate a focused subtask to a sub-agent with its own fresh context, returning a concise text report. Use mode='read' (default) for READ-ONLY investigation (e.g. 'find where X is implemented') — the sub-agent can only list/read/search files. Use mode='write' to delegate actual work (editing files, running commands) within the SAME permission and checkpoint protection as the main agent: every write/delete/command goes through user approval gating and is checkpointed. Use this to keep large investigations or self-contained work off the main conversation. Optionally set agentType to use a custom agent role from .mdga/agents/<type>.md. background=true runs asynchronously (returns a taskId; poll with get_task_output, stop with kill_task) and is only supported for mode='read' (a write subtask always runs in the foreground so its actions can be approved).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "description": { "type": "string", "description": "Clear, self-contained description of what to do and what the report should contain." },
                        "mode": { "type": "string", "enum": ["read", "write"], "description": "'read' (default) = read-only exploration; 'write' = may edit files / run commands under the main agent's permission and checkpoint protection." },
                        "agentType": { "type": "string", "description": "Optional custom agent type name (loads .mdga/agents/<type>.md as the sub-agent role)." },
                        "background": { "type": "boolean", "description": "Run asynchronously: return a taskId immediately instead of blocking. Poll with get_task_output, stop with kill_task. Only honored for mode='read'. Default false." }
                    },
                    "required": ["description"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_task_output",
                "description": "Poll a background sub-agent task (started by run_subtask background=true) for its accumulated report and status (running/done/killed/error). Set block=true to wait until it finishes or timeoutSecs elapses.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "taskId": { "type": "string", "description": "The taskId returned by run_subtask background=true." },
                        "block": { "type": "boolean", "description": "Wait for completion (up to timeoutSecs) instead of returning immediately. Default false." },
                        "timeoutSecs": { "type": "integer", "description": "Max seconds to wait when block=true. Default 30, max 120." }
                    },
                    "required": ["taskId"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "kill_task",
                "description": "Stop a running background sub-agent task by taskId.",
                "parameters": { "type": "object", "properties": { "taskId": { "type": "string" } }, "required": ["taskId"], "additionalProperties": false }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "list_tasks",
                "description": "List all background sub-agent tasks with their id, description and status.",
                "parameters": { "type": "object", "properties": {}, "additionalProperties": false }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "list_mcp_resources",
                "description": "List resources exposed by connected MCP servers (resources/list). Optionally filter by server name. Returns each resource's uri, name and mimeType.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "server": { "type": "string", "description": "Optional MCP server name to filter by; omit to list across all connected servers." }
                    },
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "read_mcp_resource",
                "description": "Read a resource from a connected MCP server (resources/read), returning its text content.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "server": { "type": "string", "description": "MCP server name that exposes the resource." },
                        "uri": { "type": "string", "description": "The resource URI to read (from list_mcp_resources)." }
                    },
                    "required": ["server", "uri"],
                    "additionalProperties": false
                }
            }
        }),
        // ask_vision（Plan27 C3 #1c）：对本会话已上传的图片做针对性追问/精读。
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "ask_vision",
                "description": "对本会话已上传图片做针对性追问/精读。当你需要图片中某个具体细节（某处文字、数据、坐标、颜色、报错、布局等）而初次视觉分析没有覆盖时，用一个明确的问题调用本工具，由视觉模型重新精读会话里的图片并回答。仅在本会话确实有用户上传过图片、且你需要图里更细的信息时使用。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "question": { "type": "string", "description": "对会话图片的具体追问，越聚焦越好（例如「截图右下角按钮上的文字是什么」「表格第二列的数值分别是多少」）。" }
                    },
                    "required": ["question"],
                    "additionalProperties": false
                }
            }
        }),
        // R4：git 原生工具——结构化 commit/diff/branch/status，取代 run_command 裸跑 git 字符串。
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "git_status",
                "description": "Get the STRUCTURED git status of the workspace: current branch, upstream, ahead/behind counts, and lists of staged / unstaged / untracked / conflicted files (each with a porcelain status code). Prefer this over running `git status` via run_command — it returns parsed fields, not text to scrape.",
                "parameters": { "type": "object", "properties": {}, "additionalProperties": false }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "git_diff",
                "description": "Show changes as a structured diff: per-file additions/deletions plus the unified patch text. Use mode to choose what is compared. Prefer this over `git diff` via run_command.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "mode": { "type": "string", "enum": ["unstaged", "staged", "all"], "description": "unstaged (default) = working tree vs index; staged = index vs HEAD; all = working tree vs HEAD." },
                        "path": { "type": "string", "description": "Optional workspace-relative file or directory to limit the diff to." }
                    },
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "git_log",
                "description": "Return recent commits as structured records (hash, short hash, author, email, ISO date, subject). Prefer this over `git log` via run_command.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "maxCount": { "type": "integer", "description": "Number of commits to return (default 20, max 200)." },
                        "path": { "type": "string", "description": "Optional workspace-relative path: only commits touching it." }
                    },
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "git_branch",
                "description": "List branches, or create/switch branches. action='list' (default) returns local branches (set includeRemote=true to include remote-tracking) with the current one flagged. action='create' creates AND switches to a new branch (name required). action='switch' switches to an existing branch (name required).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["list", "create", "switch"], "description": "Defaults to list." },
                        "name": { "type": "string", "description": "Branch name (required for create/switch)." },
                        "includeRemote": { "type": "boolean", "description": "list only: include remote-tracking branches. Default false." }
                    },
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "git_add",
                "description": "Stage changes for commit. Provide paths (workspace-relative) to stage specific files, or set all=true to stage everything (`git add -A`). Returns the full set of currently staged files afterwards.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "paths": { "type": "array", "items": { "type": "string" }, "description": "Workspace-relative paths to stage." },
                        "all": { "type": "boolean", "description": "Stage all changes (overrides paths). Default false." }
                    },
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "git_commit",
                "description": "Create a commit from the staged changes with the given message. Set all=true to also stage modified/deleted TRACKED files first (`git commit -a`); untracked files are never included by all. Returns the new commit hash and summary. Stage new files with git_add first.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "message": { "type": "string", "description": "Commit message." },
                        "all": { "type": "boolean", "description": "Stage tracked modifications before committing (-a). Default false." }
                    },
                    "required": ["message"],
                    "additionalProperties": false
                }
            }
        }),
    ]
}

fn file_tool_schema(name: &str, description: &str, required: &[&str]) -> serde_json::Value {
    let mut properties = serde_json::json!({
        "path": {
            "type": "string",
            "description": "Relative path inside the current workspace. Use . for workspace root."
        }
    });
    if required.contains(&"content") {
        properties["content"] = serde_json::json!({
            "type": "string",
            "description": "UTF-8 text content to write. Use an empty string when the user asks for an empty file."
        });
    }
    if required.contains(&"oldText") {
        properties["oldText"] = serde_json::json!({
            "type": "string",
            "description": "Exact existing text to replace. It should be unique unless replaceAll is true."
        });
    }
    if required.contains(&"newText") {
        properties["newText"] = serde_json::json!({
            "type": "string",
            "description": "Replacement text."
        });
        properties["replaceAll"] = serde_json::json!({
            "type": "boolean",
            "description": "Replace every match. Defaults to false."
        });
    }
    if required.contains(&"query") {
        properties["query"] = serde_json::json!({
            "type": "string",
            "description": "Text or regex to search for. Search is gitignore-aware (skips ignored/hidden files)."
        });
        properties["maxResults"] = serde_json::json!({
            "type": "integer",
            "description": "Maximum number of matches to return, up to 50."
        });
        properties["isRegex"] = serde_json::json!({
            "type": "boolean",
            "description": "Treat query as a regular expression. Defaults to false (literal substring)."
        });
    }

    serde_json::json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false
            }
        }
    })
}

pub(crate) fn execute_builtin_tool_call(
    security_context: &SessionSecurityContext,
    tool_name: &str,
    arguments: &str,
) -> Result<serde_json::Value, String> {
    // 权限门控已在工具循环（gate_tool_decision）完成，这里只做工具名校验与真实执行。
    tool_capability_for_name(tool_name)?;
    let workspace_path = security_context.workspace_root.as_str();

    match tool_name {
        "create_file" => execute_create_file_tool_call(workspace_path, arguments),
        "write_file" => {
            let request = serde_json::from_str::<WriteFileRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(write_file(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "edit_file" => {
            let request = serde_json::from_str::<EditFileRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(edit_file(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "apply_patch" => execute_apply_patch(workspace_path, arguments),
        "read_file" => {
            let request = serde_json::from_str::<ReadFileRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(read_file(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "delete_file" => {
            let request = serde_json::from_str::<DeleteFileRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(delete_file(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "list_dir" => {
            let request = serde_json::from_str::<ListDirRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(list_dir(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "make_dir" => {
            let request = serde_json::from_str::<MakeDirRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(make_dir(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "stat_path" => {
            let request = serde_json::from_str::<StatPathRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(stat_path(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "search_text" => {
            let request = serde_json::from_str::<SearchTextRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(search_text(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "glob_files" => {
            let request = serde_json::from_str::<GlobFilesRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(glob_files(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "code_overview" => {
            let request = serde_json::from_str::<CodeOverviewRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(
                code_overview(workspace_path, request).map_err(|err| err.to_string())?,
            )
            .map_err(|err| err.to_string())
        }
        "repo_map" => execute_repo_map(workspace_path, arguments),
        "move_path" => {
            let request = serde_json::from_str::<MovePathRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(move_path(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "delete_dir" => {
            let request = serde_json::from_str::<DeleteDirRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(delete_dir(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "run_command" => {
            let request = serde_json::from_str::<RunCommandRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(run_command(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        // R4：git 原生工具（结构化）。
        "git_status" => {
            let request = serde_json::from_str::<GitStatusRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(git_status(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "git_diff" => {
            let request = serde_json::from_str::<GitDiffRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(git_diff(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "git_log" => {
            let request = serde_json::from_str::<GitLogRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(git_log(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "git_branch" => {
            let request = serde_json::from_str::<GitBranchRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(git_branch(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "git_add" => {
            let request = serde_json::from_str::<GitAddRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(git_add(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        "git_commit" => {
            let request = serde_json::from_str::<GitCommitRequest>(arguments)
                .map_err(|err| format!("工具参数解析失败: {err}"))?;
            serde_json::to_value(git_commit(workspace_path, request).map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())
        }
        other => Err(format!("未知工具: {other}")),
    }
}

/// repo_map 工具（R2）：解析工作区源码，构建 tree-sitter + PageRank 的全仓符号地图。
/// 参数全可选；空参数即「默认预算的全仓概览」。只读、无副作用。
fn execute_repo_map(workspace_path: &str, arguments: &str) -> Result<serde_json::Value, String> {
    #[derive(serde::Deserialize, Default)]
    struct RepoMapArgs {
        #[serde(rename = "focusFiles", default)]
        focus_files: Vec<String>,
        #[serde(default)]
        query: Option<String>,
        #[serde(rename = "maxTokens", default)]
        max_tokens: usize,
    }
    // 空字符串 / "{}" / 缺省都按默认请求处理。
    let trimmed = arguments.trim();
    let args = if trimmed.is_empty() {
        RepoMapArgs::default()
    } else {
        serde_json::from_str::<RepoMapArgs>(trimmed)
            .map_err(|err| format!("工具参数解析失败: {err}"))?
    };
    let request = mdga_codemap::CodemapRequest {
        focus_files: args.focus_files,
        query: args.query,
        max_tokens: args.max_tokens,
    };
    serde_json::to_value(mdga_codemap::build_repo_map(workspace_path, &request))
        .map_err(|err| err.to_string())
}

pub(crate) fn execute_create_file_tool_call(
    workspace_path: &str,
    arguments: &str,
) -> Result<serde_json::Value, String> {
    let request = serde_json::from_str::<CreateFileRequest>(arguments)
        .map_err(|err| format!("工具参数解析失败: {err}"))?;
    let result = create_file(workspace_path, request).map_err(|err| err.to_string())?;
    serde_json::to_value(result).map_err(|err| err.to_string())
}

/// apply_patch 工具（Plan25 C-2）：对同一文件按 edits 顺序做多处「唯一匹配」精确替换。
///
/// 语义（全有或全无）：先把原文整字节读入内存（保留行尾/末尾换行，不经分页规整），
/// 按顺序对每条 edit 校验 oldText 在当前内容中 **恰好命中一次**
/// （空串 / 未命中 / 多处命中 → 整体失败、不写盘，错误标明第几条与原因），
/// 全部成功后一次性写回文件。返回 `{ ok, path, applied }`。
/// 路径安全：读取自做安全拼接（拒绝绝对路径 / `..` / 越界，且目标须为工作区内已存在文件），
/// 写回复用 `write_file` 的越界校验，与其它写工具保持一致。
fn execute_apply_patch(workspace_path: &str, arguments: &str) -> Result<serde_json::Value, String> {
    #[derive(serde::Deserialize)]
    struct PatchEdit {
        #[serde(rename = "oldText", default)]
        old_text: String,
        #[serde(rename = "newText", default)]
        new_text: String,
    }
    #[derive(serde::Deserialize)]
    struct ApplyPatchRequest {
        path: String,
        edits: Vec<PatchEdit>,
    }

    let request = serde_json::from_str::<ApplyPatchRequest>(arguments)
        .map_err(|err| format!("工具参数解析失败: {err}"))?;
    if request.edits.is_empty() {
        return Err("apply_patch 的 edits 不能为空".to_string());
    }

    // 整字节读取原文（与 edit_file 一致，保留原始行尾与末尾换行）。
    let mut content = read_existing_text_for_patch(workspace_path, &request.path)?;

    // 按顺序在内容串上做唯一匹配替换：任一条失败立即整体返回错误（不写盘）。
    for (idx, edit) in request.edits.iter().enumerate() {
        let no = idx + 1; // 面向模型用 1 基序号
        if edit.old_text.is_empty() {
            return Err(format!("第 {no} 条 edit 的 oldText 为空"));
        }
        let count = content.matches(&edit.old_text).count();
        if count == 0 {
            return Err(format!("第 {no} 条 edit 的 oldText 在（当前）文件中未命中"));
        }
        if count > 1 {
            return Err(format!(
                "第 {no} 条 edit 的 oldText 在（当前）文件中匹配到 {count} 处，必须唯一，请补充上下文使其唯一"
            ));
        }
        content = content.replacen(&edit.old_text, &edit.new_text, 1);
    }

    // 全部校验通过，一次性写回（write_file 复用越界校验，与其它写工具一致）。
    write_file(
        workspace_path,
        WriteFileRequest {
            path: request.path.clone(),
            content,
        },
    )
    .map_err(|err| err.to_string())?;

    Ok(serde_json::json!({
        "ok": true,
        "path": request.path,
        "applied": request.edits.len()
    }))
}

/// 为 apply_patch 整字节读取工作区内已存在的 UTF-8 文本文件（保留原始字节，不经 read_file 分页规整）。
///
/// 路径安全规则对齐 tool-runtime 的 resolve_existing_path：拒绝绝对路径与含 `..` 的相对路径，
/// 拼接后须落在工作区内、且目标确为已存在文件；超过 1 MiB 视为过大，建议改用 edit_file。
fn read_existing_text_for_patch(workspace_path: &str, rel: &str) -> Result<String, String> {
    const MAX_PATCH_BYTES: u64 = 1024 * 1024;
    let trimmed = rel.trim();
    if trimmed.is_empty() || trimmed == "." {
        return Err("apply_patch 需要一个具体的文件路径".to_string());
    }
    let candidate = std::path::Path::new(trimmed);
    if candidate.is_absolute() || candidate.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err("路径必须是工作区内的相对路径，且不能包含 ..".to_string());
    }
    let workspace = std::path::Path::new(workspace_path)
        .canonicalize()
        .map_err(|e| format!("工作区路径无效: {e}"))?;
    let target = workspace.join(candidate);
    if !target.exists() {
        return Err(format!("文件不存在: {trimmed}"));
    }
    let target = target
        .canonicalize()
        .map_err(|e| format!("路径解析失败: {e}"))?;
    if !target.starts_with(&workspace) {
        return Err("路径越出工作区范围".to_string());
    }
    if !target.is_file() {
        return Err(format!("不是文件: {trimmed}"));
    }
    let meta = std::fs::metadata(&target).map_err(|e| e.to_string())?;
    if meta.len() > MAX_PATCH_BYTES {
        return Err("文件过大（超过 1 MiB），apply_patch 暂不支持，请改用 edit_file 分段处理".to_string());
    }
    let bytes = std::fs::read(&target).map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|_| "文件不是有效的 UTF-8 文本".to_string())
}
