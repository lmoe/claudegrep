use serde_json::Value;

use crate::timestamp::Timestamp;

mod parser;
mod wire;

pub use parser::{agent_report, parse};

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub meta: SessionMeta,
    /// Every working directory its entries name, in the order they first appear, so the first is `meta.cwd`.
    pub cwds: Vec<String>,
    pub turns: Vec<Turn>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMeta {
    pub title: String,
    pub cwd: Option<String>,
    pub first: Timestamp,
    pub last: Timestamp,
}

#[derive(Debug, Clone)]
pub enum Turn {
    User(UserTurn),
    Assistant(AssistantTurn),
    Agent(AgentTurn),
}

impl Turn {
    pub const fn timestamp(&self) -> Timestamp {
        match self {
            Self::User(turn) => turn.timestamp,
            Self::Assistant(turn) => turn.timestamp,
            Self::Agent(turn) => turn.timestamp,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentTurn {
    pub timestamp: Timestamp,
    pub description: String,
    pub agent_type: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct UserTurn {
    pub timestamp: Timestamp,
    pub text: String,
    pub images: usize,
    pub documents: Vec<String>,
    pub command: Option<SlashCommand>,
}

#[derive(Debug, Clone)]
pub struct SlashCommand {
    pub name: String,
    pub args: String,
}

#[derive(Debug, Clone)]
pub struct AssistantTurn {
    pub timestamp: Timestamp,
    pub model: Option<String>,
    pub blocks: Vec<Block>,
}

impl AssistantTurn {
    pub fn texts(&self) -> impl Iterator<Item = &str> {
        self.blocks.iter().filter_map(|block| match block {
            Block::Text(text) => Some(text.as_str()),
            _ => None,
        })
    }
}

#[derive(Debug, Clone)]
pub enum Block {
    Text(String),
    Thinking(String),
    Tool(ToolCall),
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub name: String,
    pub input: Value,
    pub result: Option<ToolResult>,
}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
    pub images: usize,
}

pub fn with_reports(mut session: Session, reports: Vec<AgentTurn>) -> Session {
    session.turns.extend(reports.into_iter().map(Turn::Agent));
    session.turns.sort_by_key(Turn::timestamp);
    session
}

#[cfg(test)]
mod tests;
