//! The JSONL format Claude Code writes, as far as claudegrep reads it. Fields it doesn't use are ignored, and unknown
//! entry, block and part types deserialize to `Other` / `Unknown`, so new kinds of entries never break parsing.
//!
//! Tolerance: a line that isn't valid JSON or whose envelope fields have the wrong type (including a `message` that
//! is not an object) is skipped. Inside a message, every block is [`Lenient`]: one malformed block is dropped and the
//! rest of the line is kept. Fields read with a fallback (a tool call's name, a document's title, …) are lenient on
//! their own, so a wrong type means the fallback.

use std::fmt;
use std::marker::PhantomData;

use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    #[serde(rename = "type")]
    pub kind: Kind,
    pub timestamp: Option<String>,
    pub cwd: Option<String>,
    pub is_sidechain: Option<bool>,
    pub is_meta: Option<bool>,
    pub message: Option<Message>,
    pub ai_title: Option<String>,
    pub custom_title: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    User,
    Assistant,
    System,
    Attachment,
    AiTitle,
    CustomTitle,
    #[serde(other)]
    Other,
}

#[derive(Debug, PartialEq, Deserialize)]
pub struct Message {
    #[serde(default, deserialize_with = "lenient")]
    pub id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub model: Option<String>,
    #[serde(default)]
    pub content: Content<ContentBlock>,
}

/// A message's or a tool result's `content`: a plain string or a list of blocks, from which malformed blocks have
/// been dropped. Any other value, `null` and a missing field are `None`.
#[derive(Debug, Default, PartialEq)]
pub enum Content<T> {
    #[default]
    None,
    Text(String),
    Blocks(Vec<T>),
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
    },
    ToolUse {
        #[serde(default, deserialize_with = "lenient")]
        id: Option<String>,
        #[serde(default, deserialize_with = "lenient")]
        name: Option<String>,
        #[serde(default)]
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: Content<ResultPart>,
        #[serde(default, deserialize_with = "lenient")]
        is_error: Option<bool>,
    },
    Image,
    Document {
        #[serde(default, deserialize_with = "lenient")]
        title: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResultPart {
    Text {
        #[serde(default, deserialize_with = "lenient")]
        text: Option<String>,
    },
    Image,
    ToolReference {
        #[serde(default, deserialize_with = "lenient")]
        tool_name: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

/// The `.meta.json` Claude Code writes next to a subagent transcript.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMeta {
    #[serde(default, deserialize_with = "lenient")]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub agent_type: Option<String>,
}

/// A `T`, or `None` when the value is valid JSON but not a `T`. The value is buffered as a [`Value`] first, so a
/// mismatch never leaves the surrounding list or object half-read.
#[derive(Debug, PartialEq)]
struct Lenient<T>(pub Option<T>);

impl<'de, T: DeserializeOwned> Deserialize<'de> for Lenient<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self(T::deserialize(Value::deserialize(deserializer)?).ok()))
    }
}

fn lenient<'de, D: Deserializer<'de>, T: DeserializeOwned>(deserializer: D) -> Result<Option<T>, D::Error> {
    Ok(Lenient::deserialize(deserializer)?.0)
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for Content<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ContentVisitor(PhantomData))
    }
}

/// Reads [`Content`] in one pass, without buffering the whole list as an untagged enum would.
struct ContentVisitor<T>(PhantomData<T>);

impl<'de, T: DeserializeOwned> Visitor<'de> for ContentVisitor<T> {
    type Value = Content<T>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a string or a list of blocks")
    }

    fn visit_str<E>(self, text: &str) -> Result<Self::Value, E> {
        Ok(Content::Text(text.to_owned()))
    }

    fn visit_string<E>(self, text: String) -> Result<Self::Value, E> {
        Ok(Content::Text(text))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut blocks = Vec::new();
        while let Some(Lenient(block)) = seq.next_element()? {
            blocks.extend(block);
        }
        Ok(Content::Blocks(blocks))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Content::None)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Content::None)
    }

    fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
        Ok(Content::None)
    }

    fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
        Ok(Content::None)
    }

    fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
        Ok(Content::None)
    }

    fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
        Ok(Content::None)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn entry(line: &str) -> Entry {
        serde_json::from_str(line).unwrap()
    }

    fn block(json: &str) -> ContentBlock {
        serde_json::from_str(json).unwrap()
    }

    fn part(json: &str) -> ResultPart {
        serde_json::from_str(json).unwrap()
    }

    fn content(json: &str) -> Content<ContentBlock> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn message_entries_deserialize_with_their_envelope() {
        let user = entry(
            r#"{"parentUuid":null,"isSidechain":false,"promptId":"p1","type":"user","message":{"role":"user","content":"fix the bug"},"uuid":"u1","timestamp":"2026-09-17T13:19:37.016Z","userType":"external","entrypoint":"cli","cwd":"/work/app","sessionId":"s1","version":"2.1.274","gitBranch":"main"}"#,
        );
        assert_eq!(user.kind, Kind::User);
        assert_eq!(user.timestamp.as_deref(), Some("2026-09-17T13:19:37.016Z"));
        assert_eq!((user.cwd.as_deref(), user.is_sidechain, user.is_meta), (Some("/work/app"), Some(false), None));
        assert_eq!(user.message.unwrap().content, Content::Text("fix the bug".to_owned()));

        let assistant = entry(
            r#"{"parentUuid":"u1","isSidechain":true,"message":{"model":"claude-opus-4","id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"Done."}],"stop_reason":null,"usage":{"input_tokens":3}},"requestId":"req_1","type":"assistant","uuid":"u2","timestamp":"2026-09-17T13:19:41.491Z"}"#,
        );
        assert_eq!((assistant.kind, assistant.is_sidechain), (Kind::Assistant, Some(true)));
        let message = assistant.message.unwrap();
        assert_eq!((message.id.as_deref(), message.model.as_deref()), (Some("msg_1"), Some("claude-opus-4")));
        assert_eq!(message.content, Content::Blocks(vec![ContentBlock::Text { text: "Done.".to_owned() }]));

        let meta = entry(r#"{"type":"user","isMeta":true,"timestamp":"t","message":{"role":"user","content":"<local-command-caveat>…"}}"#);
        assert_eq!(meta.is_meta, Some(true));
    }

    #[test]
    fn other_entry_kinds_deserialize() {
        let system =
            entry(r#"{"type":"system","subtype":"compact_boundary","content":"Conversation compacted","timestamp":"t","level":"info"}"#);
        assert_eq!((system.kind, system.timestamp.as_deref(), system.message), (Kind::System, Some("t"), None));
        let attachment = entry(r#"{"type":"attachment","attachment":{"type":"hook_success","stdout":"…"},"timestamp":"t"}"#);
        assert_eq!(attachment.kind, Kind::Attachment);
        let ai = entry(r#"{"type":"ai-title","aiTitle":"Fix the bug","sessionId":"s1"}"#);
        assert_eq!((ai.kind, ai.ai_title.as_deref()), (Kind::AiTitle, Some("Fix the bug")));
        let custom = entry(r#"{"type":"custom-title","customTitle":"Mine","sessionId":"s1"}"#);
        assert_eq!((custom.kind, custom.custom_title.as_deref()), (Kind::CustomTitle, Some("Mine")));
        for kind in ["queue-operation", "permission-mode", "last-prompt", "file-history-snapshot", "summary", "User", ""] {
            assert_eq!(entry(&json!({ "type": kind, "timestamp": "t", "x": [1] }).to_string()).kind, Kind::Other, "{kind}");
        }
    }

    #[test]
    fn broken_envelopes_fail_the_line() {
        for line in [
            r#"{"timestamp":"t"}"#,
            r#"{"type":5}"#,
            r#"{"type":"user","timestamp":5}"#,
            r#"{"type":"user","isSidechain":"no"}"#,
            r#"{"type":"user","message":"not an object"}"#,
            r#"{"type":"user","message":true}"#,
            r#"{"type":"ai-title","aiTitle":7}"#,
            r#"{"type":"user","timestamp":"t""#,
            "[]",
        ] {
            assert!(serde_json::from_str::<Entry>(line).is_err(), "{line}");
        }
        let null = entry(r#"{"type":"user","timestamp":null,"isSidechain":null,"message":null,"aiTitle":null}"#);
        assert_eq!((null.timestamp, null.is_sidechain, null.message, null.ai_title), (None, None, None, None));
        // serde reads a struct from a sequence too, field by field, and an empty one leaves every field at its default.
        let list = entry(r#"{"type":"user","message":[]}"#);
        assert_eq!(list.message, Some(Message { id: None, model: None, content: Content::None }));
    }

    #[test]
    fn every_block_type_deserializes() {
        assert_eq!(block(r#"{"type":"text","text":"Hi","citations":null}"#), ContentBlock::Text { text: "Hi".to_owned() });
        assert_eq!(
            block(r#"{"type":"thinking","thinking":"hmm","signature":"c2ln"}"#),
            ContentBlock::Thinking { thinking: "hmm".to_owned() }
        );
        assert_eq!(
            block(r#"{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"},"caller":{"type":"direct"}}"#),
            ContentBlock::ToolUse { id: Some("toolu_1".to_owned()), name: Some("Bash".to_owned()), input: json!({ "command": "ls" }) }
        );
        assert_eq!(
            block(r#"{"tool_use_id":"toolu_1","type":"tool_result","content":"a.rs","is_error":false}"#),
            ContentBlock::ToolResult {
                tool_use_id: "toolu_1".to_owned(),
                content: Content::Text("a.rs".to_owned()),
                is_error: Some(false)
            }
        );
        assert_eq!(
            block(r#"{"type":"tool_result","tool_use_id":"t","content":[{"type":"text","text":"x"},{"type":"image","source":{}}]}"#),
            ContentBlock::ToolResult {
                tool_use_id: "t".to_owned(),
                content: Content::Blocks(vec![ResultPart::Text { text: Some("x".to_owned()) }, ResultPart::Image]),
                is_error: None,
            }
        );
        assert_eq!(block(r#"{"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBO"}}"#), ContentBlock::Image);
        assert_eq!(block(r#"{"type":"image"}"#), ContentBlock::Image);
        assert_eq!(
            block(r#"{"type":"document","title":"spec.pdf","source":{"type":"text"}}"#),
            ContentBlock::Document { title: Some("spec.pdf".to_owned()) }
        );
    }

    #[test]
    fn unknown_block_and_part_types_are_unknown() {
        for json in [
            r#"{"type":"redacted_thinking","data":"enc"}"#,
            r#"{"type":"server_tool_use","id":"s","name":"web_search","input":{}}"#,
            r#"{"type":"future","nested":{"type":"text","text":1}}"#,
            r#"{"type":"Text","text":"wrong case"}"#,
        ] {
            assert_eq!(block(json), ContentBlock::Unknown, "{json}");
        }
        assert_eq!(part(r#"{"type":"search_result","source":"x"}"#), ResultPart::Unknown);
    }

    #[test]
    fn missing_optional_fields_take_their_defaults() {
        assert_eq!(block(r#"{"type":"tool_use"}"#), ContentBlock::ToolUse { id: None, name: None, input: Value::Null });
        assert_eq!(
            block(r#"{"type":"tool_result","tool_use_id":"t"}"#),
            ContentBlock::ToolResult { tool_use_id: "t".to_owned(), content: Content::None, is_error: None }
        );
        assert_eq!(block(r#"{"type":"document"}"#), ContentBlock::Document { title: None });
        assert_eq!(part(r#"{"type":"text"}"#), ResultPart::Text { text: None });
        assert_eq!(part(r#"{"type":"tool_reference"}"#), ResultPart::ToolReference { tool_name: None });
        let message: Message = serde_json::from_str("{}").unwrap();
        assert_eq!(message, Message { id: None, model: None, content: Content::None });
        let entry = entry(r#"{"type":"user"}"#);
        assert_eq!((entry.timestamp, entry.cwd, entry.message), (None, None, None));
    }

    #[test]
    fn required_fields_of_a_block_fail_it() {
        for json in [
            r#"{"type":"text"}"#,
            r#"{"type":"text","text":5}"#,
            r#"{"type":"thinking","thinking":null}"#,
            r#"{"type":"tool_result","content":"no id"}"#,
            r#"{"text":"no type"}"#,
            r#"{"type":7}"#,
            r#""a string""#,
        ] {
            assert!(serde_json::from_str::<ContentBlock>(json).is_err(), "{json}");
        }
    }

    #[test]
    fn fields_with_a_fallback_are_lenient() {
        assert_eq!(
            block(r#"{"type":"tool_use","id":1,"name":["Bash"],"input":"raw"}"#),
            ContentBlock::ToolUse { id: None, name: None, input: json!("raw") }
        );
        assert_eq!(
            block(r#"{"type":"tool_result","tool_use_id":"t","content":7,"is_error":"yes"}"#),
            ContentBlock::ToolResult { tool_use_id: "t".to_owned(), content: Content::None, is_error: None }
        );
        assert_eq!(block(r#"{"type":"document","title":{"text":"x"}}"#), ContentBlock::Document { title: None });
        assert_eq!(part(r#"{"type":"tool_reference","tool_name":null}"#), ResultPart::ToolReference { tool_name: None });
        let message: Message = serde_json::from_str(r#"{"id":5,"model":false,"content":"kept"}"#).unwrap();
        assert_eq!(message, Message { id: None, model: None, content: Content::Text("kept".to_owned()) });
        let meta: AgentMeta = serde_json::from_str(r#"{"description":5,"agentType":"Explore","model":"x"}"#).unwrap();
        assert_eq!(meta, AgentMeta { description: None, agent_type: Some("Explore".to_owned()) });
    }

    #[test]
    fn content_is_text_blocks_or_none() {
        assert_eq!(content(r#""plain""#), Content::Text("plain".to_owned()));
        assert_eq!(content(r#""esc\"aped\n""#), Content::Text("esc\"aped\n".to_owned()));
        assert_eq!(content("[]"), Content::Blocks(Vec::new()));
        for json in ["null", "5", "-5", "1.5", "true", r#"{"type":"text","text":"x"}"#] {
            assert_eq!(content(json), Content::None, "{json}");
        }
        assert!(serde_json::from_str::<Content<ContentBlock>>("[1,").is_err(), "invalid JSON is still an error");
    }

    #[test]
    fn a_malformed_block_is_dropped_and_the_rest_kept() {
        let blocks = content(
            r#"[{"type":"text","text":1},"stray",{"type":"tool_result","tool_use_id":"t","content":[{"type":"text","text":2},{"type":"image"}]},{"type":"tool_result","content":[{"deep":[{"nested":true}]}]},{"type":"text","text":"ok"}]"#,
        );
        assert_eq!(
            blocks,
            Content::Blocks(vec![
                ContentBlock::ToolResult {
                    tool_use_id: "t".to_owned(),
                    content: Content::Blocks(vec![ResultPart::Text { text: None }, ResultPart::Image]),
                    is_error: None,
                },
                ContentBlock::Text { text: "ok".to_owned() },
            ])
        );
    }

    #[test]
    fn lenient_keeps_valid_values_and_drops_the_rest() {
        let values: Vec<Lenient<u8>> = serde_json::from_str(r#"[1, "two", 300, {"x": [3]}, 4]"#).unwrap();
        assert_eq!(values, [Lenient(Some(1)), Lenient(None), Lenient(None), Lenient(None), Lenient(Some(4))]);
        assert!(serde_json::from_str::<Lenient<u8>>("{").is_err(), "invalid JSON is not swallowed");
    }

    #[test]
    fn agent_meta_reads_description_and_type() {
        let meta: AgentMeta =
            serde_json::from_str(r#"{"agentType":"general-purpose","description":"Find the loader","toolUseId":"t","spawnDepth":1}"#)
                .unwrap();
        assert_eq!(meta, AgentMeta { description: Some("Find the loader".to_owned()), agent_type: Some("general-purpose".to_owned()) });
        assert_eq!(serde_json::from_str::<AgentMeta>("{}").unwrap(), AgentMeta::default());
        assert!(serde_json::from_str::<AgentMeta>(r#""a string""#).is_err());
    }
}
