use std::collections::HashMap;

use super::wire::{AgentMeta, Content, ContentBlock, Entry, Kind, Message, ResultPart};
use super::{AgentTurn, AssistantTurn, Block, Session, SessionMeta, SlashCommand, ToolCall, ToolResult, Turn, UserTurn};
use crate::text::{char_len, shorten};
use crate::timestamp::Timestamp;

fn entries(content: &str) -> impl Iterator<Item = Entry> + '_ {
    content.lines().filter_map(|line| serde_json::from_str(line).ok())
}

impl Entry {
    fn parsed_timestamp(&self) -> Option<Timestamp> {
        self.timestamp.as_deref()?.parse().ok()
    }
}

/// What a subagent reported back: its last assistant message, described by its `.meta.json` or else its prompt.
pub fn agent_report(content: &str, meta: Option<&str>) -> Option<AgentTurn> {
    let entries: Vec<Entry> = entries(content).collect();
    let (timestamp, text) = last_assistant_message(&entries)?;
    if char_len(&text) < MIN_REPORT_CHARS {
        return None;
    }
    let meta = AgentMeta::parse(meta);
    Some(AgentTurn { timestamp, description: meta.description(prompt(&entries)), agent_type: meta.agent_type, text })
}

const MIN_REPORT_CHARS: usize = 40;

/// The timestamp of the last assistant message's last entry, and the message's text. Claude Code writes an entry per
/// content block, so the entries of one API message share its id. An entry without an id is a message of its own.
fn last_assistant_message(entries: &[Entry]) -> Option<(Timestamp, String)> {
    let mut id = None;
    let mut texts: Vec<&str> = Vec::new();
    let mut timestamp = None;
    for entry in entries.iter().filter(|entry| entry.kind == Kind::Assistant) {
        let Some(Message { id: message_id, content: Content::Blocks(blocks), .. }) = &entry.message else {
            continue;
        };
        let new_texts: Vec<&str> = blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.trim()),
                _ => None,
            })
            .filter(|text| !text.is_empty())
            .collect();
        if new_texts.is_empty() {
            continue;
        }
        if message_id.is_none() || message_id.as_deref() != id {
            texts.clear();
            id = message_id.as_deref();
        }
        texts.extend(new_texts);
        timestamp = entry.parsed_timestamp();
    }
    Some((timestamp?, texts.join("\n\n")))
}

fn prompt(entries: &[Entry]) -> Option<&str> {
    entries.iter().filter(|entry| entry.kind == Kind::User).find_map(|entry| match &entry.message.as_ref()?.content {
        Content::Text(text) => Some(text.as_str()),
        _ => None,
    })
}

impl AgentMeta {
    fn parse(meta: Option<&str>) -> Self {
        meta.and_then(|meta| serde_json::from_str(meta).ok()).unwrap_or_default()
    }

    fn description(&self, prompt: Option<&str>) -> String {
        self.description.clone().or_else(|| prompt?.lines().next().map(|line| shorten(line, 80))).unwrap_or_else(|| "subagent".to_owned())
    }
}

/// The session in a main transcript, or `None` when it has no exchange beyond slash commands, or no title at all.
pub fn parse(id: &str, content: &str) -> Option<Session> {
    let mut parser = Parser::default();
    for entry in entries(content) {
        parser.entry(entry);
    }
    parser.finish(id)
}

#[derive(Default)]
struct Parser {
    ai_title: Option<String>,
    custom_title: Option<String>,
    first_timestamp: Option<Timestamp>,
    last_timestamp: Option<Timestamp>,
    cwd: Option<String>,
    turns: Vec<Turn>,
    /// Tool calls by id. Results arrive in later entries and are attached when parsing finishes.
    calls: HashMap<String, BlockRef>,
    results: HashMap<String, ToolResult>,
}

#[derive(Clone, Copy)]
struct BlockRef {
    turn: usize,
    block: usize,
}

impl BlockRef {
    fn call(self, turns: &mut [Turn]) -> Option<&mut ToolCall> {
        let Some(Turn::Assistant(turn)) = turns.get_mut(self.turn) else {
            return None;
        };
        match turn.blocks.get_mut(self.block)? {
            Block::Tool(call) => Some(call),
            Block::Text(_) | Block::Thinking(_) => None,
        }
    }
}

impl Parser {
    fn entry(&mut self, entry: Entry) {
        match entry.kind {
            Kind::AiTitle => {
                if entry.ai_title.is_some() {
                    self.ai_title = entry.ai_title;
                }
                return;
            }
            Kind::CustomTitle => {
                if entry.custom_title.is_some() {
                    self.custom_title = entry.custom_title;
                }
                return;
            }
            Kind::User | Kind::Assistant | Kind::System | Kind::Attachment => {}
            Kind::Other => return,
        }
        let Some(timestamp) = entry.parsed_timestamp() else {
            return;
        };
        self.first_timestamp.get_or_insert(timestamp);
        self.last_timestamp = Some(timestamp);
        if self.cwd.is_none() {
            self.cwd = entry.cwd;
        }
        if entry.is_sidechain == Some(true) {
            return;
        }
        let Some(message) = entry.message else {
            return;
        };
        match entry.kind {
            Kind::User => self.user(message.content, entry.is_meta == Some(true), timestamp),
            Kind::Assistant => self.assistant(message, timestamp),
            _ => {}
        }
    }

    fn user(&mut self, content: Content<ContentBlock>, is_meta: bool, timestamp: Timestamp) {
        let UserContent { texts, images, documents, results } = UserContent::from(content);
        self.results.extend(results);
        if is_meta {
            return;
        }
        let text = texts.join("\n\n");
        let text = text.trim();
        if (text.is_empty() && images == 0 && documents.is_empty()) || is_injected(text) {
            return;
        }
        let command = SlashCommand::parse(text);
        let text = if command.is_some() { String::new() } else { text.to_owned() };
        self.turns.push(Turn::User(UserTurn { timestamp, text, images, documents, command }));
    }

    fn assistant(&mut self, message: Message, timestamp: Timestamp) {
        if !matches!(self.turns.last(), Some(Turn::Assistant(_))) {
            self.turns.push(Turn::Assistant(AssistantTurn { timestamp, model: None, blocks: Vec::new() }));
        }
        let index = self.turns.len() - 1;
        let Some(Turn::Assistant(turn)) = self.turns.last_mut() else {
            unreachable!("an assistant turn was just ensured");
        };
        if turn.model.is_none() {
            turn.model = message.model;
        }

        let Content::Blocks(blocks) = message.content else {
            return;
        };
        for block in blocks {
            match block {
                ContentBlock::Text { text } if !text.trim().is_empty() => turn.blocks.push(Block::Text(text)),
                ContentBlock::Thinking { thinking } if !thinking.trim().is_empty() => turn.blocks.push(Block::Thinking(thinking)),
                ContentBlock::ToolUse { id, name, input } => {
                    if let Some(id) = id {
                        self.calls.insert(id, BlockRef { turn: index, block: turn.blocks.len() });
                    }
                    turn.blocks.push(Block::Tool(ToolCall { name: name.unwrap_or_else(|| "?".to_owned()), input, result: None }));
                }
                _ => {}
            }
        }
    }

    fn finish(mut self, id: &str) -> Option<Session> {
        for (id, at) in self.calls {
            if let Some(result) = self.results.remove(&id)
                && let Some(call) = at.call(&mut self.turns)
            {
                call.result = Some(result);
            }
        }

        let has_exchange = self.turns.iter().any(|turn| match turn {
            Turn::User(user) => user.command.is_none(),
            Turn::Assistant(_) | Turn::Agent(_) => true,
        });
        if !has_exchange {
            return None;
        }
        let title = self.custom_title.or(self.ai_title).or_else(|| fallback_title(&self.turns))?;
        let meta = SessionMeta { title, cwd: self.cwd, first: self.first_timestamp?, last: self.last_timestamp? };
        Some(Session { id: id.to_owned(), meta, turns: self.turns })
    }
}

#[derive(Default)]
struct UserContent {
    texts: Vec<String>,
    images: usize,
    documents: Vec<String>,
    results: Vec<(String, ToolResult)>,
}

impl From<Content<ContentBlock>> for UserContent {
    fn from(content: Content<ContentBlock>) -> Self {
        let mut user = Self::default();
        match content {
            Content::None => {}
            Content::Text(text) => user.texts.push(text),
            Content::Blocks(blocks) => {
                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => user.texts.push(text),
                        ContentBlock::Image => user.images += 1,
                        ContentBlock::Document { title } => user.documents.push(title.unwrap_or_else(|| "document".to_owned())),
                        ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                            user.results.push((tool_use_id, tool_result(content, is_error)));
                        }
                        ContentBlock::Thinking { .. } | ContentBlock::ToolUse { .. } | ContentBlock::Unknown => {}
                    }
                }
            }
        }
        user
    }
}

/// Whether a user message was written by the harness (command output, reminders, notifications), not the user.
fn is_injected(text: &str) -> bool {
    text.contains("<local-command-stdout>") || text.contains("<local-command-caveat>") || INJECTED.iter().any(|tag| text.starts_with(tag))
}

const INJECTED: &[&str] = &["<task-notification>", "<bash-stdout>", "<bash-stderr>", "<system-reminder>"];

impl SlashCommand {
    /// A `/command` as Claude Code records it (`<command-name>`, optionally `<command-args>`), or a `!` shell command
    /// (`<bash-input>`).
    fn parse(text: &str) -> Option<Self> {
        if let Some(name) = between(text, "<command-name>", "</command-name>") {
            let args = between(text, "<command-args>", "</command-args>").unwrap_or_default();
            return Some(Self { name: name.trim().to_owned(), args: args.trim().to_owned() });
        }
        between(text, "<bash-input>", "</bash-input>").map(|input| Self { name: "!".to_owned(), args: input.trim().to_owned() })
    }
}

fn tool_result(content: Content<ResultPart>, is_error: Option<bool>) -> ToolResult {
    let mut images = 0;
    let text = match content {
        Content::Text(text) => text,
        Content::Blocks(parts) => {
            let mut texts = Vec::new();
            for part in parts {
                match part {
                    ResultPart::Text { text } => texts.push(text.unwrap_or_default()),
                    ResultPart::Image => images += 1,
                    ResultPart::ToolReference { tool_name } => {
                        texts.push(format!("[tool reference: {}]", tool_name.as_deref().unwrap_or("?")));
                    }
                    ResultPart::Unknown => {}
                }
            }
            texts.join("\n")
        }
        Content::None => String::new(),
    };
    ToolResult { text, is_error: is_error == Some(true), images }
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

fn fallback_title(turns: &[Turn]) -> Option<String> {
    turns.iter().find_map(|turn| {
        let Turn::User(user) = turn else {
            return None;
        };
        let line = user.text.lines().next()?.trim();
        (!line.is_empty()).then(|| shorten(line, MAX_TITLE_CHARS))
    })
}

const MAX_TITLE_CHARS: usize = 60;

#[cfg(test)]
mod tests {
    use super::*;

    fn command(text: &str) -> Option<String> {
        SlashCommand::parse(text).map(|command| format!("{}|{}", command.name, command.args))
    }

    #[test]
    fn slash_commands_are_parsed() {
        assert_eq!(command("<command-name>/clear</command-name>").as_deref(), Some("/clear|"));
        assert_eq!(
            command("<command-message>model</command-message>\n<command-name>/model</command-name>\n<command-args>opus</command-args>")
                .as_deref(),
            Some("/model|opus")
        );
        assert_eq!(command(" <command-name>  /review </command-name><command-args>  42 </command-args> ").as_deref(), Some("/review|42"));
        assert_eq!(command("<command-name>/x</command-name><command-args>unclosed").as_deref(), Some("/x|"));
        assert_eq!(command("<command-name>/x").as_deref(), None);
        assert_eq!(command("plain text").as_deref(), None);
    }

    #[test]
    fn shell_commands_are_parsed() {
        assert_eq!(command("<bash-input>  git status </bash-input>").as_deref(), Some("!|git status"));
        assert_eq!(command("<bash-input>ls</bash-input><command-name>/c</command-name>").as_deref(), Some("/c|"), "a slash command wins");
        assert_eq!(command("<bash-input>ls").as_deref(), None);
    }

    #[test]
    fn nested_tags_end_at_the_first_close() {
        assert_eq!(command("<command-name>/a<command-name>/b</command-name>").as_deref(), Some("/a<command-name>/b|"));
        assert_eq!(command("<command-name>/a</command-name>/b</command-name>").as_deref(), Some("/a|"));
        assert_eq!(command("</command-name><command-name>/a").as_deref(), None);
    }

    #[test]
    fn injected_text_is_recognised() {
        for tag in INJECTED {
            assert!(is_injected(&format!("{tag}x")), "{tag}");
            assert!(!is_injected(&format!("x {tag}")), "{tag} only counts at the start");
        }
        assert!(is_injected("x <local-command-stdout>out</local-command-stdout>"));
        assert!(is_injected("x <local-command-caveat>Caveat</local-command-caveat>"));
        assert!(!is_injected("a normal prompt"));
    }
}
