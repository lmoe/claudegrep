//! This file depends only on `serde_json` and `chrono`, so the integration tests in `tests/` include it with
//! `#[path]` instead of keeping a copy.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::{Value, json};

pub const START: &str = "2026-01-01T10:00:00.000Z";

/// A transcript under construction. Every message entry takes the current clock time, then advances it by a minute.
#[must_use]
pub struct Transcript {
    cwd: String,
    sidechain: bool,
    clock: DateTime<Utc>,
    lines: Vec<String>,
}

impl Transcript {
    pub fn new(cwd: &str) -> Self {
        Self { cwd: cwd.to_owned(), sidechain: false, clock: parse(START), lines: Vec::new() }
    }

    pub fn sidechain(cwd: &str) -> Self {
        Self { sidechain: true, ..Self::new(cwd) }
    }

    pub fn at(mut self, timestamp: &str) -> Self {
        self.clock = parse(timestamp);
        self
    }

    /// Entries after this one have `cwd` as their working directory, as after `/cd` or a `cd` in a shell command.
    pub fn cwd(mut self, cwd: &str) -> Self {
        self.cwd = cwd.to_owned();
        self
    }

    pub fn user(self, text: &str) -> Self {
        self.entry("user", json!({ "message": { "role": "user", "content": text } }))
    }

    pub fn user_blocks(self, blocks: Vec<Value>) -> Self {
        self.entry("user", json!({ "message": { "role": "user", "content": Value::Array(blocks) } }))
    }

    pub fn meta(self, text: &str) -> Self {
        self.entry("user", json!({ "isMeta": true, "message": { "role": "user", "content": text } }))
    }

    /// An assistant message. Consecutive entries with the same `id` are parts of one API message.
    pub fn assistant(self, id: &str, blocks: Vec<Value>) -> Self {
        self.assistant_with_model(id, "claude-test", blocks)
    }

    pub fn assistant_with_model(self, id: &str, model: &str, blocks: Vec<Value>) -> Self {
        self.entry("assistant", json!({ "message": { "id": id, "role": "assistant", "model": model, "content": Value::Array(blocks) } }))
    }

    pub fn entry(mut self, kind: &str, fields: Value) -> Self {
        let mut entry = json!({
            "type": kind,
            "timestamp": self.clock.to_rfc3339_opts(SecondsFormat::Millis, true),
            "cwd": self.cwd,
            "isSidechain": self.sidechain,
        });
        if let (Some(entry), Value::Object(fields)) = (entry.as_object_mut(), fields) {
            entry.extend(fields);
        }
        self.clock += Duration::minutes(1);
        self.raw(&entry.to_string())
    }

    pub fn ai_title(self, title: &str) -> Self {
        self.raw(&json!({ "type": "ai-title", "aiTitle": title }).to_string())
    }

    pub fn custom_title(self, title: &str) -> Self {
        self.raw(&json!({ "type": "custom-title", "customTitle": title }).to_string())
    }

    pub fn raw(mut self, line: &str) -> Self {
        self.lines.push(line.to_owned());
        self
    }

    pub fn build(&self) -> String {
        let mut out = self.lines.join("\n");
        out.push('\n');
        out
    }
}

pub fn text(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

pub fn thinking(text: &str) -> Value {
    json!({ "type": "thinking", "thinking": text, "signature": "c2lnbmF0dXJl" })
}

pub fn tool_use(id: &str, name: &str, input: Value) -> Value {
    let mut block = json!({ "type": "tool_use", "id": id, "name": name });
    block["input"] = input;
    block
}

pub fn tool_result(id: &str, content: &str) -> Value {
    json!({ "type": "tool_result", "tool_use_id": id, "content": content })
}

pub fn tool_result_parts(id: &str, parts: Vec<Value>) -> Value {
    json!({ "type": "tool_result", "tool_use_id": id, "content": Value::Array(parts) })
}

pub fn tool_reference(name: &str) -> Value {
    json!({ "type": "tool_reference", "tool_name": name })
}

pub fn tool_error(id: &str, content: &str) -> Value {
    json!({ "type": "tool_result", "tool_use_id": id, "content": content, "is_error": true })
}

pub fn image() -> Value {
    json!({ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo=" } })
}

pub fn document(title: &str) -> Value {
    json!({ "type": "document", "title": title, "source": { "type": "text", "data": "…" } })
}

pub fn agent_meta(description: &str, agent_type: Option<&str>) -> String {
    json!({ "description": description, "agentType": agent_type }).to_string()
}

fn parse(timestamp: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(timestamp).expect("an RFC 3339 timestamp").with_timezone(&Utc)
}
