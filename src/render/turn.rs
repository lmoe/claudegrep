use std::fmt::{self, Display, Formatter, Write};

use crate::chunk::{self, Role};
use crate::render::Detail;
use crate::render::activity::Activity;
use crate::render::markdown::details;
use crate::render::tool::DeepCall;
use crate::transcript::{AssistantTurn, Block, SlashCommand, Turn, UserTurn};

const NO_VISIBLE_OUTPUT: &str = "*(no visible output)*";

pub const fn role(turn: &Turn) -> Role {
    match turn {
        Turn::User(_) => Role::User,
        Turn::Assistant(_) => Role::Assistant,
        Turn::Agent(_) => Role::Agent,
    }
}

pub const fn role_icon(role: Role) -> &'static str {
    match role {
        Role::User => "👤",
        Role::Assistant => "🤖",
        Role::Agent => "🛰",
    }
}

pub const fn role_label(role: Role) -> &'static str {
    match role {
        Role::User => "👤 User",
        Role::Assistant => "🤖 Claude",
        Role::Agent => "🛰 Agent",
    }
}

/// The blocks `turn` shows on a page, each paged as a whole. In default detail they are its chunk texts (`parts`),
/// then its slash command and attachments or its activity line. In deep detail a reply has one block for each
/// message, thinking block and tool call, and any other turn is one block.
pub fn blocks(turn: &Turn, detail: Detail, parts: &[String], cwd: Option<&str>) -> Vec<String> {
    match detail {
        Detail::Default => default_blocks(turn, parts, cwd),
        Detail::Deep => deep_blocks(turn),
    }
}

fn deep_blocks(turn: &Turn) -> Vec<String> {
    let mut blocks: Vec<String> = match turn {
        Turn::User(UserTurn { command: Some(command), .. }) => vec![format!("> `{}`", CommandLine(command))],
        Turn::User(user) => {
            let attachments = Attachments::of(user).map(|attachments| format!("\n\n{attachments}")).unwrap_or_default();
            vec![format!("{}{attachments}", user.text).trim().to_owned()]
        }
        Turn::Assistant(assistant) => reply_blocks(assistant).map(|(_, block)| block).collect(),
        Turn::Agent(agent) => vec![format!("Task: {}\n\n{}", agent.description, agent.text.trim())],
    };
    blocks.retain(|block| !block.is_empty());
    if blocks.is_empty() {
        blocks.push(NO_VISIBLE_OUTPUT.to_owned());
    }
    blocks
}

/// A reply's blocks in deep detail, and for each message its number among the reply's messages. A blank message
/// shows nothing and still counts, as it does in [`AssistantTurn::texts`].
fn reply_blocks(assistant: &AssistantTurn) -> impl Iterator<Item = (Option<usize>, String)> {
    let mut messages = 0;
    assistant.blocks.iter().filter_map(move |block| {
        let message = matches!(block, Block::Text(_)).then(|| {
            messages += 1;
            messages - 1
        });
        let shown = DeepBlock(block).to_string();
        let shown = shown.trim();
        (!shown.is_empty()).then(|| (message, shown.to_owned()))
    })
}

/// The block of `turn` in deep detail that holds the start of part `part` of its text, so a position from search
/// opens the same place in either detail. Part 0 is the start of the turn, with whatever comes before its first
/// message. A turn of one block, and a part the turn does not have, give the first block too.
pub fn deep_block_of_part(turn: &Turn, part: usize) -> usize {
    let Turn::Assistant(assistant) = turn else {
        return 0;
    };
    if part == 0 {
        return 0;
    }
    let messages: Vec<&str> = assistant.texts().collect();
    let Some(&message) = chunk::part_starts(&messages).get(part) else {
        return 0;
    };
    reply_blocks(assistant).position(|(number, _)| number == Some(message)).unwrap_or(0)
}

fn default_blocks(turn: &Turn, parts: &[String], cwd: Option<&str>) -> Vec<String> {
    let mut blocks = parts.to_vec();
    match turn {
        Turn::User(user) => {
            if let Some(command) = &user.command {
                blocks.push(format!("> `{}`", CommandLine(command)));
            }
            blocks.extend(Attachments::of(user).map(|attachments| attachments.to_string()));
        }
        Turn::Assistant(assistant) => {
            let activity = Activity::of(assistant, cwd);
            if !activity.is_empty() {
                blocks.push(activity.to_string());
            }
        }
        Turn::Agent(_) => {}
    }
    if blocks.is_empty() {
        blocks.push(NO_VISIBLE_OUTPUT.to_owned());
    }
    blocks
}

/// A slash command as the user typed it, `/model opus`. `!` stands for a shell command.
pub struct CommandLine<'a>(pub &'a SlashCommand);

impl Display for CommandLine<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let SlashCommand { name, args } = self.0;
        if !name.starts_with('/') && name != "!" {
            f.write_char('/')?;
        }
        f.write_str(name)?;
        if !args.is_empty() {
            write!(f, " {args}")?;
        }
        Ok(())
    }
}

struct Attachments<'a>(&'a UserTurn);

impl<'a> Attachments<'a> {
    fn of(user: &'a UserTurn) -> Option<Self> {
        (user.images > 0 || !user.documents.is_empty()).then_some(Self(user))
    }
}

impl Display for Attachments<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let images = (0..self.0.images).map(|_| "*[pasted image]*".to_owned());
        let documents = self.0.documents.iter().map(|title| format!("📎 *{title}*"));
        for (index, item) in images.chain(documents).enumerate() {
            let separator = if index == 0 { "" } else { " " };
            write!(f, "{separator}{item}")?;
        }
        Ok(())
    }
}

struct DeepBlock<'a>(&'a Block);

impl Display for DeepBlock<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.0 {
            Block::Text(text) => f.write_str(text.trim()),
            Block::Thinking(text) => details("💭 Thinking", text.trim()).fmt(f),
            Block::Tool(call) => DeepCall(call).fmt(f),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::lines::START;
    use crate::test_support::timestamp;
    use crate::transcript::{AgentTurn, AssistantTurn, ToolCall, ToolResult};

    fn user(text: &str, images: usize, documents: &[&str], command: Option<(&str, &str)>) -> Turn {
        Turn::User(UserTurn {
            timestamp: timestamp(START),
            text: text.to_owned(),
            images,
            documents: documents.iter().map(|&title| title.to_owned()).collect(),
            command: command.map(|(name, args)| SlashCommand { name: name.to_owned(), args: args.to_owned() }),
        })
    }

    fn assistant(blocks: Vec<Block>) -> Turn {
        Turn::Assistant(AssistantTurn { timestamp: timestamp(START), model: Some("opus".to_owned()), blocks })
    }

    fn agent(text: &str) -> Turn {
        Turn::Agent(AgentTurn {
            timestamp: timestamp(START),
            description: "Find the parser".to_owned(),
            agent_type: Some("Explore".to_owned()),
            text: text.to_owned(),
        })
    }

    fn tool(name: &str, input: serde_json::Value, text: &str) -> Block {
        Block::Tool(ToolCall {
            name: name.to_owned(),
            input,
            result: Some(ToolResult { text: text.to_owned(), is_error: false, images: 0 }),
        })
    }

    fn parts(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|&text| text.to_owned()).collect()
    }

    #[test]
    fn role_labels_start_with_the_icon() {
        for role in [Role::User, Role::Assistant, Role::Agent] {
            assert!(role_label(role).starts_with(&format!("{} ", role_icon(role))));
        }
        assert_eq!(role_label(Role::Assistant), "🤖 Claude");
    }

    #[test]
    fn slash_commands_read_as_typed() {
        let line = |name: &str, args: &str| CommandLine(&SlashCommand { name: name.to_owned(), args: args.to_owned() }).to_string();
        assert_eq!(line("/model", "opus"), "/model opus");
        assert_eq!(line("model", "opus"), "/model opus");
        assert_eq!(line("clear", ""), "/clear");
        assert_eq!(line("!", "git status"), "! git status");
    }

    #[test]
    fn user_turns_show_their_text_then_their_attachments() {
        let turn = user("Look at this", 2, &["spec.pdf"], None);
        assert_eq!(
            blocks(&turn, Detail::Default, &parts(&["Look at this"]), None),
            ["Look at this", "*[pasted image]* *[pasted image]* 📎 *spec.pdf*"]
        );
        assert_eq!(blocks(&turn, Detail::Deep, &[], None), ["Look at this\n\n*[pasted image]* *[pasted image]* 📎 *spec.pdf*"]);
    }

    #[test]
    fn slash_command_turns_show_the_command() {
        let turn = user("", 0, &[], Some(("model", "opus")));
        assert_eq!(blocks(&turn, Detail::Default, &[], None), ["> `/model opus`"]);
        assert_eq!(blocks(&turn, Detail::Deep, &[], None), ["> `/model opus`"]);
    }

    #[test]
    fn a_turn_with_nothing_to_show_says_so() {
        assert_eq!(blocks(&assistant(Vec::new()), Detail::Default, &[], None), [NO_VISIBLE_OUTPUT]);
        assert_eq!(blocks(&assistant(Vec::new()), Detail::Deep, &[], None), [NO_VISIBLE_OUTPUT]);
        let inspecting = assistant(vec![tool("Bash", json!({ "command": "ls" }), "a b")]);
        assert_eq!(blocks(&inspecting, Detail::Default, &[], None), [NO_VISIBLE_OUTPUT]);
        assert_eq!(blocks(&user("", 0, &[], None), Detail::Default, &[], None), [NO_VISIBLE_OUTPUT]);
    }

    #[test]
    fn assistant_turns_end_in_their_activity_line() {
        let turn = assistant(vec![Block::Text("Done.".to_owned()), tool("Edit", json!({ "file_path": "/p/a.rs" }), "ok")]);
        assert_eq!(blocks(&turn, Detail::Default, &parts(&["Done."]), Some("/p")), ["Done.", "*🔧 edited `a.rs`*"]);
    }

    #[test]
    fn agent_turns_show_their_task_and_report() {
        let turn = agent("  It is in src/parser.rs.\n");
        assert_eq!(blocks(&turn, Detail::Default, &parts(&["Task: Find the parser", "It is in src/parser.rs."]), None).len(), 2);
        assert_eq!(blocks(&turn, Detail::Deep, &[], None), ["Task: Find the parser\n\nIt is in src/parser.rs."]);
    }

    #[test]
    fn a_deep_turn_with_nothing_to_show_says_so() {
        assert_eq!(blocks(&assistant(vec![Block::Text(" \n".to_owned())]), Detail::Deep, &[], None), [NO_VISIBLE_OUTPUT]);
        assert_eq!(blocks(&user("", 0, &[], None), Detail::Deep, &[], None), [NO_VISIBLE_OUTPUT]);
    }

    #[test]
    fn a_blank_message_is_no_block_in_deep_detail() {
        let turn = assistant(vec![
            Block::Text("\n\n".to_owned()),
            tool("Bash", json!({ "command": "ls" }), "a b"),
            Block::Text("Done.".to_owned()),
        ]);
        let deep = blocks(&turn, Detail::Deep, &[], None);
        assert_eq!(deep.len(), 2);
        assert!(deep[0].starts_with("**🔧 Bash**"), "{}", deep[0]);
        assert_eq!(deep[1], "Done.");
    }

    #[test]
    fn a_part_opens_in_deep_detail_at_the_block_of_its_message() {
        let answer = "answer ".repeat(143);
        let turn = assistant(vec![
            Block::Thinking("Check the test first.".to_owned()),
            Block::Text("Running the tests.".to_owned()),
            tool("Bash", json!({ "command": "cargo test" }), "test result: ok"),
            Block::Text(String::new()),
            tool("Read", json!({ "file_path": "/p/a.rs" }), "fn a() {}"),
            Block::Text(answer.trim().to_owned()),
        ]);
        let deep = blocks(&turn, Detail::Deep, &[], None);
        assert_eq!(deep_block_of_part(&turn, 0), 0, "part 0 is the start of the turn, thinking included");
        assert_eq!(deep_block_of_part(&turn, 1), 4);
        assert_eq!(deep[4], answer.trim());
        assert_eq!(deep_block_of_part(&turn, 2), 0, "the turn has two parts");
        assert_eq!(deep_block_of_part(&agent("Found."), 1), 0);
        assert_eq!(deep_block_of_part(&user("Hello", 0, &[], None), 0), 0);
    }

    #[test]
    fn deep_assistant_turns_show_thinking_text_and_tool_calls_in_order() {
        let turn = assistant(vec![
            Block::Thinking("  Check the test first.  ".to_owned()),
            Block::Text("Running the tests.".to_owned()),
            tool("Bash", json!({ "command": "cargo test" }), "test result: ok"),
            Block::Text("All green.\n".to_owned()),
        ]);
        let deep = blocks(&turn, Detail::Deep, &[], None);
        assert_eq!(deep.len(), 4, "one block for each thinking block, message and tool call");
        insta::assert_snapshot!(deep.join("\n\n"), @r#"
        <details>
        <summary>💭 Thinking</summary>

        Check the test first.

        </details>

        Running the tests.

        **🔧 Bash**

        ```json
        {
          "command": "cargo test"
        }
        ```

        <details>
        <summary>Result</summary>

        ```
        test result: ok
        ```

        </details>

        All green.
        "#);
    }
}
