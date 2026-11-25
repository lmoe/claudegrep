use serde_json::{Value, json};

use super::*;
use crate::test_support::lines::{
    START, Transcript, agent_meta, document, image, text, thinking, tool_error, tool_reference, tool_result, tool_result_parts, tool_use,
};
use crate::test_support::timestamp;

const CWD: &str = "/work/app";

fn session(lines: &[&str]) -> Option<Session> {
    parse("s1", &lines.join("\n"))
}

#[test]
fn merges_assistant_blocks_and_matches_tool_results() {
    let parsed = session(&[
        r#"{"type":"user","timestamp":"2026-01-01T10:00:00Z","cwd":"/p","message":{"role":"user","content":"fix the bug"}}"#,
        r#"{"type":"assistant","timestamp":"2026-01-01T10:00:01Z","message":{"role":"assistant","model":"m","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#,
        r#"{"type":"user","timestamp":"2026-01-01T10:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"a.rs"}]}}"#,
        r#"{"type":"assistant","timestamp":"2026-01-01T10:00:03Z","message":{"role":"assistant","content":[{"type":"text","text":"Fixed."}]}}"#,
        r#"{"type":"ai-title","aiTitle":"Bug fix"}"#,
        r#"{"type":"some-future-type","foo":1}"#,
        r#"{"type":"user","timestamp":"#,
    ])
    .unwrap();
    assert_eq!(parsed.meta.title, "Bug fix");
    assert_eq!(parsed.meta.cwd.as_deref(), Some("/p"));
    assert_eq!(parsed.turns.len(), 2);
    let Turn::Assistant(turn) = &parsed.turns[1] else { panic!() };
    assert_eq!(turn.model.as_deref(), Some("m"));
    let Block::Tool(call) = &turn.blocks[0] else { panic!() };
    assert_eq!(call.result.as_ref().unwrap().text, "a.rs");
    assert!(matches!(&turn.blocks[1], Block::Text(t) if t == "Fixed."));
}

#[test]
fn custom_title_wins_and_commands_are_recognised() {
    let parsed = session(&[
        r#"{"type":"user","timestamp":"2026-01-01T10:00:00Z","message":{"role":"user","content":"<command-name>/model</command-name><command-args>opus</command-args>"}}"#,
        r#"{"type":"user","timestamp":"2026-01-01T10:00:01Z","message":{"role":"user","content":"hello there"}}"#,
        r#"{"type":"ai-title","aiTitle":"Greeting"}"#,
        r#"{"type":"custom-title","customTitle":"My title"}"#,
    ])
    .unwrap();
    assert_eq!(parsed.meta.title, "My title");
    let Turn::User(command) = &parsed.turns[0] else { panic!() };
    let command = command.command.as_ref().unwrap();
    assert_eq!((command.name.as_str(), command.args.as_str()), ("/model", "opus"));
}

#[test]
fn drops_harness_injected_messages() {
    let parsed = session(&[
        r#"{"type":"user","timestamp":"2026-01-01T10:00:00Z","message":{"role":"user","content":"real prompt"}}"#,
        r#"{"type":"user","timestamp":"2026-01-01T10:00:01Z","message":{"role":"user","content":"<task-notification>\n<task-id>x</task-id>\n</task-notification>"}}"#,
        r#"{"type":"user","timestamp":"2026-01-01T10:00:02Z","message":{"role":"user","content":"<bash-input>ls</bash-input>"}}"#,
        r#"{"type":"user","timestamp":"2026-01-01T10:00:03Z","message":{"role":"user","content":"<bash-stdout>a.rs</bash-stdout><bash-stderr></bash-stderr>"}}"#,
    ])
    .unwrap();
    assert_eq!(parsed.turns.len(), 2);
    let Turn::User(shell) = &parsed.turns[1] else { panic!() };
    let shell = shell.command.as_ref().unwrap();
    assert_eq!((shell.name.as_str(), shell.args.as_str()), ("!", "ls"));
}

#[test]
fn agent_report_is_the_last_assistant_message() {
    let content = [
        r#"{"type":"user","isSidechain":true,"timestamp":"2026-01-01T10:00:00Z","message":{"role":"user","content":"Find the config loader\nmore"}}"#,
        r#"{"type":"assistant","isSidechain":true,"timestamp":"2026-01-01T10:00:01Z","message":{"id":"m1","content":[{"type":"text","text":"Looking around."}]}}"#,
        r#"{"type":"assistant","isSidechain":true,"timestamp":"2026-01-01T10:00:05Z","message":{"id":"m2","content":[{"type":"text","text":"The loader lives in src/config.rs and reads TOML."}]}}"#,
        r#"{"type":"assistant","isSidechain":true,"timestamp":"2026-01-01T10:00:06Z","message":{"id":"m2","content":[{"type":"text","text":"It falls back to defaults."}]}}"#,
    ]
    .join("\n");
    let report = agent_report(&content, Some(r#"{"description":"Find loader","agentType":"Explore"}"#)).unwrap();
    assert_eq!(report.description, "Find loader");
    assert_eq!(report.agent_type.as_deref(), Some("Explore"));
    assert_eq!(report.text, "The loader lives in src/config.rs and reads TOML.\n\nIt falls back to defaults.");
    assert_eq!(report.timestamp.to_string(), "2026-01-01T10:00:06.000Z");
    assert_eq!(agent_report(&content, None).unwrap().description, "Find the config loader");
}

#[test]
fn reports_are_placed_chronologically() {
    let parsed = session(&[
        r#"{"type":"user","timestamp":"2026-01-01T10:00:00Z","message":{"role":"user","content":"first"}}"#,
        r#"{"type":"user","timestamp":"2026-01-01T11:00:00Z","message":{"role":"user","content":"second"}}"#,
    ])
    .unwrap();
    let report = |at| AgentTurn { timestamp: timestamp(at), description: "d".to_owned(), agent_type: None, text: "t".to_owned() };
    let merged = with_reports(parsed.clone(), vec![report("2026-01-01T10:30:00Z")]);
    assert!(matches!(merged.turns[1], Turn::Agent(_)));
    // 08:30 in UTC, before both user turns, though its text sorts between them.
    let merged = with_reports(parsed, vec![report("2026-01-01T10:30:00+02:00")]);
    assert!(matches!(merged.turns[0], Turn::Agent(_)));
}

#[test]
fn an_entry_whose_timestamp_does_not_parse_counts_as_having_none() {
    let parsed = session(&[
        r#"{"type":"user","timestamp":"yesterday","message":{"role":"user","content":"not a time"}}"#,
        r#"{"type":"user","timestamp":"","message":{"role":"user","content":"empty"}}"#,
        r#"{"type":"user","timestamp":"2026-01-01T10:00:00Z","message":{"role":"user","content":"real"}}"#,
        r#"{"type":"user","timestamp":"tomorrow","message":{"role":"user","content":"not a time either"}}"#,
    ])
    .unwrap();
    assert_eq!(parsed.turns.len(), 1);
    assert_eq!(parsed.meta.first.to_string(), "2026-01-01T10:00:00.000Z");
    assert_eq!(parsed.meta.last, parsed.meta.first);
}

#[test]
fn skips_sessions_without_exchange() {
    assert!(session(&[
        r#"{"type":"user","timestamp":"2026-01-01T10:00:00Z","message":{"role":"user","content":"<command-name>/model</command-name>"}}"#,
    ])
    .is_none());
}

#[test]
fn falls_back_to_first_prompt_line() {
    let parsed =
        session(&[r#"{"type":"user","timestamp":"2026-01-01T10:00:00Z","message":{"role":"user","content":"first line\nsecond"}}"#])
            .unwrap();
    assert_eq!(parsed.meta.title, "first line");
}

fn parsed(transcript: &Transcript) -> Session {
    crate::test_support::session::parse("s1", transcript)
}

fn turns(transcript: &Transcript) -> Vec<Turn> {
    parsed(transcript).turns
}

fn user(turn: &Turn) -> &UserTurn {
    let Turn::User(user) = turn else { panic!("expected a user turn, got {turn:?}") };
    user
}

fn assistant(turn: &Turn) -> &AssistantTurn {
    let Turn::Assistant(assistant) = turn else { panic!("expected an assistant turn, got {turn:?}") };
    assistant
}

fn calls(turn: &Turn) -> Vec<&ToolCall> {
    assistant(turn)
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Tool(call) => Some(call),
            _ => None,
        })
        .collect()
}

fn prompt(prompt: &str) -> UserTurn {
    user(&turns(&Transcript::new(CWD).user(prompt).assistant("a", vec![text("ok")]).ai_title("Title"))[0]).clone()
}

#[test]
fn unknown_entry_and_block_types_are_ignored() {
    let transcript = Transcript::new(CWD)
        .at("2026-01-01T09:00:00.000Z")
        .entry("some-future-type", json!({ "foo": 1 }))
        .at(START)
        .user_blocks(vec![json!({ "type": "future_block", "x": 1 }), text("hello")])
        .assistant(
            "a",
            vec![
                json!({ "type": "server_tool_use", "id": "s", "name": "web_search" }),
                json!({ "type": "redacted_thinking", "data": "x" }),
                text("Hi."),
            ],
        );
    let parsed = parsed(&transcript);
    assert_eq!(parsed.meta.first.to_string(), START, "an unknown entry's timestamp does not count");
    assert_eq!(parsed.turns.len(), 2);
    assert_eq!(user(&parsed.turns[0]).text, "hello");
    assert!(matches!(assistant(&parsed.turns[1]).blocks.as_slice(), [Block::Text(t)] if t == "Hi."));
}

#[test]
fn a_malformed_block_keeps_the_rest_of_its_line() {
    let transcript = Transcript::new(CWD)
        .user_blocks(vec![
            json!({ "type": "text", "text": 5 }),
            json!("not a block"),
            json!({ "text": "no type" }),
            text("kept"),
            json!({ "type": "tool_result", "content": "no id" }),
            json!({ "type": "tool_result", "tool_use_id": "t1", "content": 7, "is_error": "yes" }),
        ])
        .assistant(
            "a",
            vec![
                json!({ "type": "thinking" }),
                json!({ "type": "text", "text": null }),
                json!(42),
                text("Still here."),
                json!({ "type": "tool_use", "id": "t1", "name": 5, "input": {} }),
                json!({ "type": "tool_use", "name": "Read" }),
            ],
        );
    let turns = turns(&transcript);
    assert_eq!(user(&turns[0]).text, "kept");
    let blocks = &assistant(&turns[1]).blocks;
    assert_eq!(blocks.len(), 3);
    assert!(matches!(&blocks[0], Block::Text(t) if t == "Still here."));
    let calls = calls(&turns[1]);
    assert_eq!(calls[0].name, "?");
    let result = calls[0].result.as_ref().unwrap();
    assert_eq!((result.text.as_str(), result.is_error), ("", false));
    assert_eq!((calls[1].name.as_str(), &calls[1].input, calls[1].result.is_none()), ("Read", &Value::Null, true));
}

#[test]
fn broken_lines_are_skipped_not_the_session() {
    let transcript = Transcript::new(CWD)
        .raw(r#"{"type":"user","cwd":"/elsewhere","message":{"role":"user","content":"no timestamp"}}"#)
        .raw(r#"{"type":"user","timestamp":"2026-01-01T09:00:00.000Z","message":{"role":"user","con"#)
        .raw("not json at all")
        .raw("")
        .raw(r#"{"type":"user","timestamp":"2026-01-01T09:00:00.000Z","isSidechain":"no","message":{"content":"bad flag"}}"#)
        .raw(r#"{"type":5,"timestamp":"2026-01-01T09:00:00.000Z","message":{"content":"bad type"}}"#)
        .user("first")
        .assistant("a", vec![text("Answer.")]);
    let parsed = parsed(&transcript);
    assert_eq!(parsed.turns.len(), 2);
    assert_eq!(user(&parsed.turns[0]).text, "first");
    assert_eq!(parsed.meta.cwd.as_deref(), Some(CWD));
    assert_eq!(parsed.meta.first.to_string(), START);
}

#[test]
fn a_message_that_is_not_an_object_skips_its_line() {
    let transcript = Transcript::new(CWD)
        .at("2026-01-01T09:00:00.000Z")
        .entry("assistant", json!({ "message": "not an object" }))
        .at(START)
        .user("prompt")
        .entry("assistant", json!({ "message": 5 }))
        .user("again")
        .assistant("a", vec![text("Reply.")]);
    let parsed = parsed(&transcript);
    assert_eq!(parsed.meta.first.to_string(), START);
    assert_eq!(parsed.turns.len(), 3, "no empty assistant turns");
}

#[test]
fn sidechain_entries_are_left_out_but_count_for_the_time_span() {
    let side = |transcript: Transcript, content: Value| {
        transcript.entry("user", json!({ "isSidechain": true, "cwd": "/side", "message": { "role": "user", "content": content } }))
    };
    let transcript = side(Transcript::new(CWD).at("2026-01-01T09:00:00.000Z"), json!("side prompt"))
        .at(START)
        .user("main prompt")
        .assistant("a", vec![text("Main reply.")]);
    let transcript = side(transcript.at("2026-01-01T12:00:00.000Z"), json!([text("side text")]));
    let parsed = parsed(&transcript);
    assert_eq!(parsed.turns.len(), 2);
    assert_eq!(user(&parsed.turns[0]).text, "main prompt");
    assert_eq!(parsed.meta.first.to_string(), "2026-01-01T09:00:00.000Z");
    assert_eq!(parsed.meta.last.to_string(), "2026-01-01T12:00:00.000Z");
    assert_eq!(parsed.meta.cwd.as_deref(), Some("/side"), "the first cwd wins, even from a sidechain entry");
}

#[test]
fn every_working_directory_is_kept_once_in_the_order_it_first_appears() {
    let transcript = Transcript::new("/a")
        .raw(r#"{"type":"user","cwd":"/no-timestamp","message":{"role":"user","content":"skipped"}}"#)
        .user("first")
        .cwd("/b")
        .assistant("a", vec![text("Moved.")])
        .cwd("/a")
        .user("back")
        .cwd("/c")
        .entry("system", json!({ "subtype": "local_command" }));
    let parsed = parsed(&transcript);
    assert_eq!(parsed.cwds, ["/a", "/b", "/c"]);
    assert_eq!(parsed.meta.cwd.as_deref(), Some("/a"));
}

#[test]
fn system_and_attachment_entries_count_for_the_time_span_only() {
    let transcript = Transcript::new(CWD)
        .user("prompt")
        .assistant("a", vec![text("Reply.")])
        .entry("system", json!({ "content": "compacted", "message": { "role": "user", "content": "not a turn" } }))
        .entry("attachment", json!({ "attachment": { "type": "file" } }));
    let parsed = parsed(&transcript);
    assert_eq!(parsed.turns.len(), 2);
    assert_eq!(parsed.meta.last.to_string(), "2026-01-01T10:03:00.000Z");
}

#[test]
fn meta_messages_record_tool_results_but_are_no_turn() {
    let transcript = Transcript::new(CWD)
        .user("run it")
        .assistant("a", vec![tool_use("t1", "Bash", json!({ "command": "ls" }))])
        .entry("user", json!({ "isMeta": true, "message": { "role": "user", "content": [tool_result("t1", "a.rs"), text("injected")] } }))
        .meta("plain meta text");
    let turns = turns(&transcript);
    assert_eq!(turns.len(), 2);
    assert_eq!(calls(&turns[1])[0].result.as_ref().unwrap().text, "a.rs");
}

#[test]
fn tool_results_are_matched_to_calls_by_id() {
    let transcript = Transcript::new(CWD)
        .user("go")
        .user_blocks(vec![tool_result("early", "before the call")])
        .assistant(
            "a",
            vec![
                tool_use("early", "Read", json!({})),
                tool_use("dup", "Bash", json!({ "n": 1 })),
                tool_use("dup", "Bash", json!({ "n": 2 })),
                tool_use("none", "Grep", json!({})),
            ],
        )
        .user_blocks(vec![tool_result("dup", "first"), tool_result("stray", "matches nothing"), tool_result("dup", "second")]);
    let turns = turns(&transcript);
    assert_eq!(turns.len(), 2, "messages with only tool results are no turns");
    let results: Vec<Option<&str>> = calls(&turns[1]).iter().map(|call| call.result.as_ref().map(|r| r.text.as_str())).collect();
    assert_eq!(results, [Some("before the call"), None, Some("second"), None], "a duplicate id goes to the last call");
}

#[test]
fn tool_result_parts_are_flattened() {
    let transcript = Transcript::new(CWD)
        .user("q")
        .assistant("a", vec![tool_use("t1", "ToolSearch", json!({})), tool_use("t2", "Read", json!({}))])
        .user_blocks(vec![
            tool_result_parts(
                "t1",
                vec![
                    text("found"),
                    tool_reference("WebFetch"),
                    image(),
                    json!({ "type": "text" }),
                    json!({ "type": "tool_reference" }),
                    json!({ "type": "future_part" }),
                    image(),
                ],
            ),
            tool_error("t2", "no such file"),
        ]);
    let turns = turns(&transcript);
    let calls = calls(&turns[1]);
    let first = calls[0].result.as_ref().unwrap();
    assert_eq!(first.text, "found\n[tool reference: WebFetch]\n\n[tool reference: ?]");
    assert_eq!((first.images, first.is_error), (2, false));
    let second = calls[1].result.as_ref().unwrap();
    assert_eq!((second.text.as_str(), second.images, second.is_error), ("no such file", 0, true));
}

#[test]
fn consecutive_assistant_entries_form_one_turn() {
    let transcript = Transcript::new(CWD)
        .user("q")
        .entry("assistant", json!({ "message": { "id": "a", "role": "assistant", "content": [text("one")] } }))
        .assistant_with_model("b", "m1", vec![text("  "), tool_use("t1", "Read", json!({}))])
        .user_blocks(vec![tool_result("t1", "contents")])
        .assistant_with_model("c", "m2", vec![thinking("hmm"), thinking(" \n"), text("two")])
        .user("next")
        .assistant_with_model("d", "m3", vec![text("three")]);
    let turns = turns(&transcript);
    assert_eq!(turns.len(), 4);
    let first = assistant(&turns[1]);
    assert_eq!(first.model.as_deref(), Some("m1"), "the first model wins");
    assert_eq!(first.timestamp.to_string(), "2026-01-01T10:01:00.000Z");
    let blocks: Vec<&str> = first
        .blocks
        .iter()
        .map(|block| match block {
            Block::Text(text) => text.as_str(),
            Block::Thinking(_) => "thinking",
            Block::Tool(call) => call.name.as_str(),
        })
        .collect();
    assert_eq!(blocks, ["one", "Read", "thinking", "two"]);
    assert_eq!(assistant(&turns[3]).model.as_deref(), Some("m3"));
}

#[test]
fn harness_injected_messages_are_no_turns() {
    for injected in [
        "<task-notification>\n<task-id>x</task-id>\n</task-notification>",
        "<bash-stdout>a.rs</bash-stdout><bash-stderr></bash-stderr>",
        "<bash-stderr>oops</bash-stderr>",
        "  <system-reminder>leading space</system-reminder>",
        "before <local-command-stdout>out</local-command-stdout>",
        "x <local-command-caveat>Caveat: …</local-command-caveat> y",
    ] {
        let transcript = Transcript::new(CWD).user("real").user(injected).assistant("a", vec![text("ok")]);
        assert_eq!(turns(&transcript).len(), 2, "{injected:?}");
    }
    assert_eq!(prompt("see <system-reminder> in the middle").text, "see <system-reminder> in the middle");
}

#[test]
fn slash_command_turns_keep_no_text() {
    let turn = prompt(" <command-name>/model</command-name><command-args>opus</command-args> and text");
    let command = turn.command.unwrap();
    assert_eq!((command.name.as_str(), command.args.as_str(), turn.text.as_str()), ("/model", "opus", ""));
    let shell = prompt("<bash-input>ls</bash-input>").command.unwrap();
    assert_eq!((shell.name.as_str(), shell.args.as_str()), ("!", "ls"));
    let unclosed = prompt("<command-name>/x");
    assert_eq!((unclosed.command.is_none(), unclosed.text.as_str()), (true, "<command-name>/x"));
}

#[test]
fn user_blocks_give_text_images_and_documents() {
    let transcript = Transcript::new(CWD)
        .user_blocks(vec![
            text("look"),
            image(),
            image(),
            document("spec.pdf"),
            json!({ "type": "document", "source": {} }),
            text("  and this  "),
        ])
        .user_blocks(vec![image()])
        .assistant("a", vec![text("Seen.")]);
    let turns = turns(&transcript);
    let first = user(&turns[0]);
    assert_eq!(first.text, "look\n\n  and this");
    assert_eq!(first.images, 2);
    assert_eq!(first.documents, ["spec.pdf", "document"]);
    let second = user(&turns[1]);
    assert_eq!((second.text.as_str(), second.images), ("", 1), "an image alone is a turn");
}

const REPORT: &str = "The loader lives in src/config.rs and reads TOML.";

fn agent() -> Transcript {
    Transcript::sidechain(CWD)
}

#[test]
fn agent_report_needs_a_long_enough_reply() {
    assert!(agent_report(&agent().user("Find the loader").build(), None).is_none());
    let short = "x".repeat(39);
    assert!(agent_report(&agent().user("Find it").assistant("m", vec![text(&short)]).build(), None).is_none());
    let exact = "x".repeat(40);
    assert_eq!(agent_report(&agent().user("Find it").assistant("m", vec![text(&exact)]).build(), None).unwrap().text, exact);
    let untimed = json!({ "type": "assistant", "message": { "id": "m", "content": [text(REPORT)] } }).to_string();
    let untimed = agent().user("Find it").assistant("m", vec![text(REPORT)]).raw(&untimed);
    assert!(agent_report(&untimed.build(), None).is_none(), "the last entry of the report has no timestamp");
    let garbled = json!({ "type": "assistant", "timestamp": "yesterday", "message": { "id": "m", "content": [text(REPORT)] } }).to_string();
    let garbled = agent().user("Find it").assistant("m", vec![text(REPORT)]).raw(&garbled);
    assert!(agent_report(&garbled.build(), None).is_none(), "the last entry's timestamp does not parse");
}

#[test]
fn agent_report_is_the_last_message_by_id() {
    let transcript = agent()
        .user("Find the loader")
        .assistant("m1", vec![text("Looking around, this text is long enough to count.")])
        .assistant("m2", vec![text(&format!("  {REPORT}  ")), tool_use("t1", "Read", json!({}))])
        .user_blocks(vec![tool_result("t1", "fn load() {}")])
        .assistant("m2", vec![tool_use("t2", "Read", json!({}))])
        .assistant("m2", vec![text("It falls back to defaults.")])
        .assistant("m3", vec![thinking("no text here")]);
    let report = agent_report(&transcript.build(), None).unwrap();
    assert_eq!(report.text, format!("{REPORT}\n\nIt falls back to defaults."));
    assert_eq!(report.timestamp.to_string(), "2026-01-01T10:05:00.000Z");

    let without_ids = agent()
        .user("Find the loader")
        .entry("assistant", json!({ "message": { "content": [text(REPORT)] } }))
        .entry("assistant", json!({ "message": { "content": [text("Entries without an id are separate messages.")] } }));
    assert_eq!(agent_report(&without_ids.build(), None).unwrap().text, "Entries without an id are separate messages.");
}

#[test]
fn agent_description_comes_from_meta_or_the_prompt() {
    let transcript = agent().user("Find the config loader\nin the repo").assistant("m", vec![text(REPORT)]).build();
    let report = |meta: Option<&str>| agent_report(&transcript, meta).unwrap();
    let explore = report(Some(&agent_meta("Find loader", Some("Explore"))));
    assert_eq!((explore.description.as_str(), explore.agent_type.as_deref()), ("Find loader", Some("Explore")));
    assert_eq!(report(None).description, "Find the config loader");
    assert_eq!(report(Some("{not json")).description, "Find the config loader");
    assert_eq!(report(Some("[1]")).description, "Find the config loader");
    let plan = report(Some(r#"{"agentType":"Plan"}"#));
    assert_eq!((plan.description.as_str(), plan.agent_type.as_deref()), ("Find the config loader", Some("Plan")));
    let wrong = report(Some(r#"{"description":5,"agentType":"Explore"}"#));
    assert_eq!((wrong.description.as_str(), wrong.agent_type.as_deref()), ("Find the config loader", Some("Explore")));

    let long = "word ".repeat(30);
    let report = agent_report(&agent().user(&long).assistant("m", vec![text(REPORT)]).build(), None).unwrap();
    assert_eq!(report.description, format!("{}…", &long[..79]));

    let blocks = agent().user_blocks(vec![text("Blocks are no prompt")]).assistant("m", vec![text(REPORT)]).build();
    assert_eq!(agent_report(&blocks, None).unwrap().description, "subagent");
    let later = agent().user_blocks(vec![text("Blocks")]).user("The prompt").assistant("m", vec![text(REPORT)]).build();
    assert_eq!(agent_report(&later, None).unwrap().description, "The prompt");
}

fn titled_base() -> Transcript {
    Transcript::new(CWD)
        .user("<command-name>/model</command-name>")
        .user_blocks(vec![image()])
        .user("first prompt line\nmore")
        .assistant("a", vec![text("Reply.")])
}

fn title_of(transcript: &Transcript) -> String {
    parsed(transcript).meta.title
}

#[test]
fn titles_prefer_custom_then_ai_then_the_first_prompt() {
    let base = titled_base;
    let title = |transcript: Transcript| title_of(&transcript);
    assert_eq!(title(base()), "first prompt line");
    assert_eq!(title(base().ai_title("First").ai_title("Second")), "Second");
    assert_eq!(title(base().custom_title("Custom").ai_title("Later AI")), "Custom");
    assert_eq!(title(base().custom_title("One").custom_title("Two")), "Two");
    assert_eq!(title(base().ai_title("Kept").raw(r#"{"type":"ai-title"}"#).raw(r#"{"type":"ai-title","aiTitle":null}"#)), "Kept");
}

#[test]
fn a_custom_title_entry_without_a_title_keeps_the_earlier_one() {
    let without = |line: &str| title_of(&titled_base().custom_title("Kept").raw(line));
    assert_eq!(without(r#"{"type":"custom-title"}"#), "Kept");
    assert_eq!(without(r#"{"type":"custom-title","customTitle":null}"#), "Kept");
}

#[test]
fn an_assistant_entry_whose_content_is_a_string_does_not_stop_the_parser() {
    let string = || json!({ "message": { "id": "a", "model": "m", "content": "a plain string" } });
    let only = Transcript::new(CWD).user("question").entry("assistant", string());
    let _ = parse("s1", &only.build());

    let turns = turns(&Transcript::new(CWD).user("question").entry("assistant", string()).assistant("b", vec![text("A real block.")]));
    let Some(Turn::Assistant(reply)) = turns.last() else { panic!("no assistant turn in {turns:?}") };
    assert!(reply.blocks.iter().any(|block| matches!(block, Block::Text(t) if t == "A real block.")), "{:?}", reply.blocks);
}

#[test]
fn an_agent_report_survives_an_assistant_entry_whose_content_is_not_a_list() {
    let transcript = agent()
        .user("Find the loader")
        .assistant("m1", vec![text(REPORT)])
        .entry("assistant", json!({ "message": { "id": "m2", "content": "a string, not blocks" } }));
    assert_eq!(agent_report(&transcript.build(), None).unwrap().text, REPORT);
}

#[test]
fn long_first_prompts_are_cut_for_the_title() {
    let title = |prompt: &str| parsed(&Transcript::new(CWD).user(prompt).assistant("a", vec![text("ok")])).meta.title;
    assert_eq!(title(&"x".repeat(61)), format!("{}…", "x".repeat(59)));
    assert_eq!(title(&"y".repeat(60)), "y".repeat(60));
    assert_eq!(title(&"ä".repeat(70)), format!("{}…", "ä".repeat(59)), "characters, not bytes");
}

#[test]
fn sessions_without_any_title_are_skipped() {
    let transcript = Transcript::new(CWD).user_blocks(vec![image()]).assistant("a", vec![text("A picture.")]);
    assert!(parse("s1", &transcript.build()).is_none());
    assert!(parse("s1", &transcript.ai_title("Picture").build()).is_some());
}

/// An anonymised prefix of a real session and one of its subagents: words replaced, ids renumbered, long strings cut.
#[test]
fn a_real_transcript_parses_unchanged() {
    let main = parse("fixture", include_str!("fixtures/session.jsonl")).unwrap();
    let report = agent_report(include_str!("fixtures/agent.jsonl"), Some(include_str!("fixtures/agent.meta.json")));
    insta::assert_debug_snapshot!(with_reports(main, report.into_iter().collect()));
}
