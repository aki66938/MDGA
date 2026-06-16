//! 消息构建：把后端可信的工作区上下文（身份锚定 / 工具纪律 / 行为准则 / repo map /
//! 项目长期记忆 / 技能列表）注入模型上下文，使 DeepSeek 能回答当前工作区问题。
//!
//! 本轮（Plan28 P3-9）从桌面端 `agent_loop.rs` 整体迁入 agent-core，逻辑一字不改：
//! 仅把对桌面端 `crate::agent_prompt::X` 的引用改为本 crate `crate::prompt::X`，
//! 由 `pub(crate)`/私有提升为 `pub`，并连同 3 个现有单测一并迁过来作为行为不变的兜底。

use crate::prompt::{CODE_OF_CONDUCT, IDENTITY_ANCHOR, TOOL_DISCIPLINE};
use mdga_deepseek_client::ChatMessage;

/// 把后端可信的 session 工作区快照注入模型上下文，使 DeepSeek 能回答当前工作区问题。
pub fn messages_with_workspace_context(
    messages: Vec<ChatMessage>,
    workspace_path: Option<&str>,
    workspace_name: Option<&str>,
    repo_map: Option<&str>,
    workspace_memory: Option<&str>,
    skills: &[(String, String)],
) -> Vec<ChatMessage> {
    // 把后端可信的 session 工作区快照注入模型上下文，使 DeepSeek 能回答当前工作区问题。
    let Some(path) = workspace_path.filter(|path| !path.trim().is_empty()) else {
        // 纯聊天会话（未绑定工作区）：明确告知模型没有任何工具，防止它凭训练记忆
        // 幻觉输出 <ToolCall>/DSML 等工具调用标记（0.0.17 dev 实测出现过）。
        let mut injected = Vec::with_capacity(messages.len() + 1);
        injected.push(ChatMessage {
            role: "system".to_string(),
            content: "当前会话未绑定工作区，你没有任何本地文件、目录或命令工具可用。\
如果用户要求读写文件、列目录、修改代码或执行命令，请直接告知：需要点击「+ 新对话」并选择工作区后才能执行本地操作。\
绝对不要输出任何工具调用标记（如 <ToolCall>、DSML 标记等），也不要假装已经执行了本地操作。"
                .to_string(),
        });
        injected.extend(messages);
        return injected;
    };
    let name = workspace_name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("未命名工作区");
    let mut injected = Vec::with_capacity(messages.len() + 4);
    // 不可变核心原则（Plan25 #1，动静分离）：身份锚定 / 工具纪律 / 行为准则全部引用 prompt 常量，
    // 单点维护、字节稳定以提升 prompt 缓存命中。动态的工作区路径 / 记忆 / 技能仍在下方内联拼接。
    // 身份锚定：明确 MDGA 不是 Claude Code，配置在 .mdga/，防止模型沿用 .claude 等训练记忆里的约定。
    injected.push(ChatMessage {
        role: "system".to_string(),
        content: IDENTITY_ANCHOR.to_string(),
    });
    injected.push(ChatMessage {
        role: "system".to_string(),
        content: format!(
            "你正在 MDGA 桌面端中运行。本轮会话绑定的工作区名称是 {name}，工作区路径是 {path}。\
除非用户明确授权越界，否则你应假定所有本地文件任务都发生在该工作区内。\
当用户询问你当前所在的工作区或工作目录时，应直接回答这个路径；不要声称自己没有工作区。\
当用户要求列目录、读取文件、创建文件、修改文件或删除文件时，必须分别调用 list_dir、read_file、\
create_file、write_file 或 delete_file 工具完成真实本地操作；不要只给出代码示例，\
不要建议用户手动操作，也不要在没有工具结果时声称文件已处理。"
        ),
    });
    injected.push(ChatMessage {
        role: "system".to_string(),
        content: TOOL_DISCIPLINE.to_string(),
    });
    // 行为准则（Plan25 #1 新增）：不可变工作风格——简洁、改前先读、优先 edit/apply_patch、
    // 能查清不提问、写完必验证、不可逆操作谨慎、达成即停。
    injected.push(ChatMessage {
        role: "system".to_string(),
        content: CODE_OF_CONDUCT.to_string(),
    });
    // repo map：开局注入工作区结构摘要，让模型无需逐层 list_dir 就了解项目骨架。
    if let Some(map) = repo_map.filter(|map| !map.trim().is_empty()) {
        injected.push(ChatMessage {
            role: "system".to_string(),
            content: format!(
                "当前工作区结构摘要（已忽略 .git/node_modules/target 等噪声目录，可能有省略）：\n{map}\n\
需要查看更深层目录或文件内容时，再调用 list_dir / read_file。"
            ),
        });
    }
    // 项目长期记忆：工作区根目录 MDGA.md（对标 CLAUDE.md / AGENTS.md），每次请求注入，
    // 永不被上下文压缩冲掉，承载项目目标、规范与架构约定。
    if let Some(memory) = workspace_memory.filter(|m| !m.trim().is_empty()) {
        injected.push(ChatMessage {
            role: "system".to_string(),
            content: format!(
                "项目长期记忆（来自工作区根目录的 MDGA.md，跨会话持久有效，优先遵循其中的目标与约定）：\n{memory}"
            ),
        });
    }
    // 技能列表（渐进披露）：只注入名称与描述，完整说明由模型按需调用 load_skill 加载。
    if !skills.is_empty() {
        let list = skills
            .iter()
            .map(|(name, desc)| format!("- {name}：{desc}"))
            .collect::<Vec<_>>()
            .join("\n");
        injected.push(ChatMessage {
            role: "system".to_string(),
            content: format!(
                "当前工作区可用技能（来自 .mdga/skills/）。当任务与某项技能匹配时，先调用 load_skill 加载其完整说明再执行：\n{list}"
            ),
        });
    }
    injected.extend(messages);
    injected
}

/// 读取工作区根目录的 MDGA.md 作为项目长期记忆；不存在或为空时返回 None。
/// 上限 16K 字符，防止超大记忆文件挤占上下文。
pub fn read_workspace_memory(workspace_root: &str) -> Option<String> {
    let path = std::path::Path::new(workspace_root).join("MDGA.md");
    let content = std::fs::read_to_string(path).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(16_000).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepends_workspace_context_to_deepseek_messages() {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "你是否清楚现在所在的工作区路径是什么".to_string(),
        }];

        let injected = messages_with_workspace_context(
            messages,
            Some("C:\\workspace\\demo"),
            Some("MDGA"),
            None,
            None,
            &[],
        );

        // injected[0] 是 MDGA 身份锚定消息。
        assert_eq!(injected[0].role, "system");
        assert!(injected[0].content.contains("MDGA"));
        assert!(injected[0].content.contains(".mdga"));
        assert_eq!(injected[1].role, "system");
        assert!(injected[1].content.contains("C:\\workspace\\demo"));
        assert!(injected[1].content.contains("MDGA"));
        assert!(injected[1].content.contains("除非用户明确授权越界"));
        assert!(injected[1].content.contains("必须分别调用"));
        assert!(injected[1].content.contains("read_file"));
        assert!(injected[1].content.contains("write_file"));
        assert!(injected[1].content.contains("delete_file"));
        assert!(injected[1].content.contains("list_dir"));
        assert_eq!(injected[2].role, "system");
        assert!(injected[2].content.contains("edit_file"));
        assert!(injected[2].content.contains("search_text"));
        // injected[3] 是行为准则（Plan25 #1 新增）。
        assert_eq!(injected[3].role, "system");
        assert!(injected[3].content.contains("行为准则"));
        assert!(injected[3].content.contains("改动前先读"));
        assert_eq!(injected[4].role, "user");
    }


    #[test]
    fn injects_repo_map_when_provided() {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "项目结构是什么".to_string(),
        }];

        let injected = messages_with_workspace_context(
            messages,
            Some("C:\\workspace\\demo"),
            Some("MDGA"),
            Some("src/\n  main.rs\nCargo.toml"),
            None,
            &[],
        );

        // sys(身份) + sys(workspace) + sys(tools) + sys(行为准则) + sys(repo map) + user
        assert_eq!(injected.len(), 6);
        assert_eq!(injected[4].role, "system");
        assert!(injected[4].content.contains("工作区结构摘要"));
        assert!(injected[4].content.contains("main.rs"));
        assert_eq!(injected[5].role, "user");
    }

    #[test]
    fn injects_workspace_memory_when_provided() {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "继续开发".to_string(),
        }];

        let injected = messages_with_workspace_context(
            messages,
            Some("C:\\workspace\\demo"),
            Some("MDGA"),
            None,
            Some("项目目标：做一个计算器。代码规范：KISS。"),
            &[],
        );

        // sys(身份) + sys(workspace) + sys(tools) + sys(行为准则) + sys(memory) + user
        assert_eq!(injected.len(), 6);
        assert_eq!(injected[4].role, "system");
        assert!(injected[4].content.contains("项目长期记忆"));
        assert!(injected[4].content.contains("做一个计算器"));
    }
}
