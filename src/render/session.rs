use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter, Write};

use crate::chunk::{self, is_heading_line};
use crate::discover::Scope;
use crate::render::clock::Clock;
use crate::render::pager::{Page, Pager};
use crate::render::turn::{CommandLine, blocks, deep_block_of_part, role, role_icon, role_label};
use crate::render::{Detail, Position, TurnRange, Within};
use crate::store::SessionRow;
use crate::text::{char_len, plural, shorten};
use crate::timestamp::Timestamp;
use crate::transcript::{Block, Session, SessionMeta, Turn, UserTurn};

/// The labels of the header's date and session fields, which `tool` recognises in tool results.
pub const DATE: &str = "**Date:** ";
pub const SESSION: &str = " · **Session:** `";

const OUTLINE_USER_CHARS: usize = 100;
const OUTLINE_CHARS: usize = 90;

pub struct Meta<'a> {
    pub id: &'a str,
    session: &'a SessionMeta,
    pub turns: usize,
    archived: bool,
}

impl<'a> Meta<'a> {
    pub const fn of(session: &'a Session, archived: bool) -> Self {
        Self { id: session.id.as_str(), session: &session.meta, turns: session.turns.len(), archived }
    }

    pub const fn span(&self, clock: Clock) -> Span {
        Span { clock, first: self.session.first, last: self.session.last }
    }

    pub const fn turn_range(&self) -> TurnRange {
        TurnRange::all(self.turns)
    }

    pub fn marks(&self, scope: &Scope) -> Marks<'a> {
        Marks { cwd: (!scope.is_one_directory()).then(|| self.session.cwd.as_deref().unwrap_or("?")), archived: self.archived }
    }
}

impl<'a> From<&'a SessionRow> for Meta<'a> {
    fn from(row: &'a SessionRow) -> Self {
        Self { id: row.short_id(), session: &row.meta, turns: row.turns, archived: row.archived }
    }
}

pub struct Span {
    clock: Clock,
    first: Timestamp,
    last: Timestamp,
}

impl Display for Span {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} – {}", self.clock.date_time(self.first), self.clock.date_time(self.last))
    }
}

pub struct Marks<'a> {
    cwd: Option<&'a str>,
    archived: bool,
}

impl Display for Marks<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if let Some(cwd) = self.cwd {
            write!(f, " · `{cwd}`")?;
        }
        if self.archived {
            f.write_str(" · *archived*")?;
        }
        Ok(())
    }
}

const ARCHIVED: &str = " · **Archived:** Claude Code has deleted the transcript, this is claudegrep's copy";

pub fn header(session: &Session, archived: bool) -> String {
    Printer::local(session).header(archived).to_string()
}

pub fn page(session: &Session, from: Position, end_turn: usize, detail: Detail, budget: usize) -> Page {
    Printer::local(session).page(from, end_turn, detail, budget)
}

pub fn outline(session: &Session, from: usize, budget: usize) -> Page {
    Printer::local(session).outline(from, budget)
}

struct Printer<'a> {
    session: &'a Session,
    clock: Clock,
}

impl<'a> Printer<'a> {
    fn new(session: &'a Session, clock: Clock) -> Self {
        Self { session, clock: clock.starting(session.meta.first) }
    }

    fn local(session: &'a Session) -> Self {
        Self::new(session, Clock::LOCAL)
    }

    const fn header(&self, archived: bool) -> Header<'a> {
        Header { meta: Meta::of(self.session, archived), clock: self.clock }
    }

    /// The turns from `from` up to `end_turn`, as far as `budget` allows. Each turn is a heading and its blocks. A
    /// block costs its characters, and the first block shown of a turn also the heading's. Where the page stops inside
    /// a turn, `next` names the block in the unit of `detail`.
    fn page(&self, from: Position, end_turn: usize, detail: Detail, budget: usize) -> Page {
        let session = self.session;
        let parts = chunk_texts(session);
        let mut pager = Pager::new(from.turn, budget);
        for index in from.turn..end_turn.min(session.turns.len()) {
            let turn = &session.turns[index];
            let blocks = blocks(turn, detail, parts.get(&index).map_or(&[], Vec::as_slice), session.meta.cwd.as_deref());
            let start = if index == from.turn { first_block(turn, from.within, detail).min(blocks.len()) } else { 0 };
            let heading = Heading { index, turn, clock: self.clock, detail, continued: start };
            let heading = format!("\n---\n\n{heading}\n");
            for (part, block) in blocks.iter().enumerate().skip(start) {
                let first = part == start;
                let block = format!("\n{block}\n");
                if !pager.fits(char_len(&block) + if first { char_len(&heading) } else { 0 }) {
                    return pager.finish(Some(Position { turn: index, within: detail.within(part) }));
                }
                if first {
                    pager.push(index, &heading);
                }
                pager.push(index, &block);
            }
        }
        pager.finish(None)
    }

    fn outline(&self, from: usize, budget: usize) -> Page {
        let mut pager = Pager::new(from, budget);
        for (index, turn) in self.session.turns.iter().enumerate().skip(from) {
            let line = format!("- {}\n", OutlineLine { index, turn, clock: self.clock });
            if !pager.fits(char_len(&line)) {
                return pager.finish(Some(Position::start(index)));
            }
            pager.push(index, &line);
        }
        pager.finish(None)
    }
}

/// The block of `turn` a page in `detail` starts at for `within`. A part opens a deep page at the block its text
/// starts in. A deep block means nothing in default detail, where the turn starts over.
fn first_block(turn: &Turn, within: Within, detail: Detail) -> usize {
    match (detail, within) {
        (Detail::Default, Within::Part(part)) => part,
        (Detail::Default, Within::Block(_)) => 0,
        (Detail::Deep, Within::Part(part)) => deep_block_of_part(turn, part),
        (Detail::Deep, Within::Block(block)) => block,
    }
}

struct Header<'a> {
    meta: Meta<'a>,
    clock: Clock,
}

impl Display for Header<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let meta = &self.meta;
        write!(f, "# {}\n\n{DATE}{}", meta.session.title, meta.span(self.clock))?;
        if let Some(cwd) = &meta.session.cwd {
            write!(f, " · **Project:** `{cwd}`")?;
        }
        write!(f, "{SESSION}{}` · **Turns:** {}", meta.id, meta.turn_range())?;
        if meta.archived {
            f.write_str(ARCHIVED)?;
        }
        f.write_char('\n')
    }
}

/// A turn's heading on a page, `### #3 🤖 Claude — 10:04`.
struct Heading<'a> {
    index: usize,
    turn: &'a Turn,
    clock: Clock,
    detail: Detail,
    /// The block the page starts at, when it starts inside the turn.
    continued: usize,
}

impl Display for Heading<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let turn = self.turn;
        write!(f, "### #{} {}", self.index, role_label(role(turn)))?;
        let qualifier = match turn {
            Turn::User(_) => None,
            Turn::Assistant(assistant) => assistant.model.as_ref().filter(|_| self.detail == Detail::Deep),
            Turn::Agent(agent) => agent.agent_type.as_ref(),
        };
        if let Some(qualifier) = qualifier {
            write!(f, " ({qualifier})")?;
        }
        write!(f, " — {}", self.clock.time(turn.timestamp()))?;
        if self.continued > 0 {
            let unit = if self.detail == Detail::Deep { "block" } else { "part" };
            write!(f, " (continued from {unit} {})", self.continued)?;
        }
        Ok(())
    }
}

/// A turn in the outline, `#1 🤖 10:01 · 2 messages · 1265 chars · 5 tool calls → …`.
struct OutlineLine<'a> {
    index: usize,
    turn: &'a Turn,
    clock: Clock,
}

impl Display for OutlineLine<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let turn = self.turn;
        write!(f, "#{} {} {} · ", self.index, role_icon(role(turn)), self.clock.time(turn.timestamp()))?;
        match turn {
            Turn::User(UserTurn { command: Some(command), .. }) => write!(f, "`{}`", CommandLine(command)),
            Turn::User(user) => {
                let first = user.text.lines().find(|line| !line.trim().is_empty()).unwrap_or("");
                f.write_str(&shorten(first.trim(), OUTLINE_USER_CHARS))?;
                let size = char_len(&user.text);
                if size > char_len(first) { write!(f, " ({size} {})", plural(size, "char", "chars")) } else { Ok(()) }
            }
            Turn::Assistant(assistant) => {
                let texts: Vec<&str> = assistant.texts().collect();
                let tools = assistant.blocks.iter().filter(|block| matches!(block, Block::Tool(_))).count();
                let chars: usize = texts.iter().map(|text| char_len(text)).sum();
                let messages = plural(texts.len(), "message", "messages");
                let (chars_word, tools_word) = (plural(chars, "char", "chars"), plural(tools, "tool call", "tool calls"));
                write!(f, "{} {messages} · {chars} {chars_word} · {tools} {tools_word}", texts.len())?;
                match texts.last().and_then(|text| gist(text)) {
                    Some(ending) => write!(f, " → {}", shorten(ending, OUTLINE_CHARS)),
                    None => Ok(()),
                }
            }
            Turn::Agent(agent) => {
                let size = char_len(&agent.text);
                write!(f, "{} · {size} {}", shorten(&agent.description, OUTLINE_CHARS), plural(size, "char", "chars"))
            }
        }
    }
}

/// The line that says what a reply is about: its first line that is neither blank nor a heading, or its first
/// heading when it has nothing else.
fn gist(text: &str) -> Option<&str> {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let first = lines.next()?;
    Some(if is_heading_line(first) { lines.find(|line| !is_heading_line(line)).unwrap_or(first) } else { first })
}

fn chunk_texts(session: &Session) -> BTreeMap<usize, Vec<String>> {
    let mut parts: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for chunk in chunk::chunks(session) {
        parts.entry(chunk.turn).or_default().push(chunk.text);
    }
    parts
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;
    use serde_json::json;

    use super::*;
    use crate::test_support::lines::{Transcript, text, thinking, tool_result, tool_use};
    use crate::test_support::session::parse;
    use crate::test_support::timestamp;
    use crate::transcript::{AgentTurn, AssistantTurn};

    fn utc() -> Clock {
        Clock::fixed(FixedOffset::east_opt(0).unwrap())
    }

    fn session() -> Session {
        let cause = "The clock skew check compared local time with UTC. ".repeat(12);
        let fix = "Both sides are converted to UTC now. ".repeat(12);
        let reply = format!("## Cause\n\n{}\n\n## Fix\n\n{}", cause.trim(), fix.trim());
        let transcript = Transcript::new("/p/app")
            .user("Why does the token refresh fail?\nIt fails after an hour.")
            .assistant(
                "m1",
                vec![
                    thinking("Probably the clock."),
                    text("Let me look."),
                    tool_use("t1", "Read", json!({ "file_path": "/p/app/src/auth.rs" })),
                ],
            )
            .user_blocks(vec![tool_result("t1", "fn refresh() {}")])
            .assistant("m2", vec![text(&reply)])
            .at("2026-01-02T08:00:00.000Z")
            .user("<command-name>/model</command-name>\n<command-args>opus</command-args>")
            .ai_title("Fix token refresh");
        parse("aaaa1111-0000-4000-8000-000000000001", &transcript)
    }

    fn printer(session: &Session) -> Printer<'_> {
        Printer::new(session, utc())
    }

    const fn at(turn: usize, part: usize) -> Position {
        Position::part(turn, part)
    }

    fn sizes(session: &Session, part: usize) -> (usize, usize) {
        let whole = printer(session).page(at(1, 0), 2, Detail::Default, 0).text;
        let blocks = blocks(&session.turns[1], Detail::Default, &chunk_texts(session)[&1], session.meta.cwd.as_deref());
        let block = format!("\n{}\n", blocks[part]);
        (char_len(&whole[..whole.find(&block).unwrap()]), char_len(&block))
    }

    #[test]
    fn the_header_names_the_session() {
        let session = session();
        insta::assert_snapshot!(printer(&session).header(false).to_string(), @r"
        # Fix token refresh

        **Date:** 2026-01-01 10:00 – 2026-01-02 08:00 · **Project:** `/p/app` · **Session:** `aaaa1111-0000-4000-8000-000000000001` · **Turns:** 0–2
        ");
    }

    #[test]
    fn the_header_marks_archived_sessions_and_leaves_out_a_missing_project() {
        let mut session = session();
        session.meta.cwd = None;
        let header = printer(&session).header(true).to_string();
        assert!(!header.contains("**Project:**"), "{header}");
        assert!(header.ends_with(" · **Turns:** 0–2 · **Archived:** Claude Code has deleted the transcript, this is claudegrep's copy\n"));
    }

    #[test]
    fn a_default_page_shows_texts_commands_and_activity() {
        let session = session();
        let page = printer(&session).page(at(0, 0), 3, Detail::Default, 0);
        assert_eq!((page.first, page.last, page.next), (0, 2, None));
        insta::assert_snapshot!(page.text);
    }

    #[test]
    fn a_deep_page_shows_thinking_tool_calls_and_the_model() {
        let session = session();
        let page = printer(&session).page(at(0, 0), 3, Detail::Deep, 0);
        assert_eq!((page.first, page.last, page.next), (0, 2, None));
        insta::assert_snapshot!(page.text);
    }

    #[test]
    fn a_page_can_start_inside_a_turn() {
        let session = session();
        let page = printer(&session).page(at(1, 1), 2, Detail::Default, 0);
        assert!(page.text.starts_with("\n---\n\n### #1 🤖 Claude — 10:01 (continued from part 1)\n\n## Fix\n\n"), "{}", page.text);
        assert!(!page.text.contains("Let me look."));
    }

    #[test]
    fn a_deep_page_opens_a_part_at_the_block_of_its_message() {
        let session = session();
        let page = printer(&session).page(at(1, 0), 2, Detail::Deep, 0);
        assert!(page.text.starts_with("\n---\n\n### #1 🤖 Claude (claude-test) — 10:01\n\n<details>"), "{}", page.text);
        let page = printer(&session).page(at(1, 1), 2, Detail::Deep, 0);
        assert!(
            page.text.starts_with("\n---\n\n### #1 🤖 Claude (claude-test) — 10:01 (continued from block 3)\n\n## Cause\n"),
            "{}",
            page.text
        );
    }

    #[test]
    fn a_deep_page_can_start_at_any_block_of_a_turn() {
        let session = session();
        let page = printer(&session).page(Position::block(1, 2), 2, Detail::Deep, 0);
        assert!(
            page.text.starts_with("\n---\n\n### #1 🤖 Claude (claude-test) — 10:01 (continued from block 2)\n\n**🔧 Read**\n"),
            "{}",
            page.text
        );
        assert!(!page.text.contains("Let me look."));
        let whole = printer(&session).page(Position::block(1, 0), 2, Detail::Deep, 0);
        assert!(whole.text.starts_with("\n---\n\n### #1 🤖 Claude (claude-test) — 10:01\n\n<details>"), "{}", whole.text);
    }

    #[test]
    fn a_deep_page_stops_between_the_blocks_of_a_turn() {
        let session = session();
        let page = printer(&session).page(at(0, 0), 3, Detail::Deep, 300);
        assert_eq!((page.first, page.last, page.next), (0, 1, Some(Position::block(1, 2))));
        assert!(page.text.ends_with("\nLet me look.\n"), "{}", page.text);
    }

    #[test]
    fn deep_pages_follow_each_other_without_gaps_or_repeats() {
        let session = session();
        let whole = printer(&session).page(at(0, 0), 3, Detail::Deep, 0).text;
        let (mut from, mut pages) = (at(0, 0), Vec::new());
        loop {
            let page = printer(&session).page(from, 3, Detail::Deep, 300);
            pages.push(page.text);
            match page.next {
                Some(next) => from = next,
                None => break,
            }
        }
        assert!(pages.len() > 3, "{} pages", pages.len());
        for (index, turn) in session.turns.iter().enumerate() {
            for block in blocks(turn, Detail::Deep, &[], None) {
                assert_eq!(pages.iter().filter(|page| page.contains(&block)).count(), 1, "turn {index}: {block}");
            }
        }
        assert_eq!(without_continued_headings(&pages.concat()), whole);
    }

    fn without_continued_headings(pages: &str) -> String {
        let mut out = String::new();
        let mut rest = pages;
        while let Some(at) = rest.find(" (continued from block ") {
            let heading = rest[..at].rfind("\n---\n\n").expect("a continued heading follows a separator");
            out.push_str(&rest[..heading]);
            let line_end = at + rest[at..].find('\n').expect("the heading ends its line");
            rest = &rest[line_end + 1..];
        }
        out.push_str(rest);
        out
    }

    #[test]
    fn a_deep_block_starts_the_turn_over_in_default_detail() {
        let session = session();
        let page = printer(&session).page(Position::block(1, 2), 2, Detail::Default, 0);
        assert!(page.text.starts_with("\n---\n\n### #1 🤖 Claude — 10:01\n\nLet me look."), "{}", page.text);
    }

    #[test]
    fn a_part_past_the_end_of_a_turn_skips_it() {
        let session = session();
        let page = printer(&session).page(at(0, 9), 2, Detail::Default, 0);
        assert!(page.text.starts_with("\n---\n\n### #1 "), "{}", page.text);
        assert_eq!((page.first, page.last), (0, 1));
    }

    #[test]
    fn the_end_turn_is_clamped_to_the_session() {
        let session = session();
        assert_eq!(printer(&session).page(at(0, 0), 99, Detail::Default, 0).last, 2);
        assert_eq!(printer(&session).page(at(0, 0), 1, Detail::Default, 0).last, 0);
        assert_eq!(printer(&session).page(at(5, 0), 99, Detail::Default, 0).text, "");
    }

    #[test]
    fn the_first_block_is_shown_whatever_the_budget() {
        let session = session();
        let page = printer(&session).page(at(1, 0), 3, Detail::Default, 1);
        assert!(page.text.contains("Let me look."));
        assert_eq!((page.first, page.last, page.next), (1, 1, Some(at(1, 1))));
    }

    #[test]
    fn only_the_first_part_shown_pays_for_the_heading() {
        let session = session();
        let (before, length) = sizes(&session, 1);
        // `before` is everything up to part 1: the heading and part 0 with their newlines.
        let page = printer(&session).page(at(1, 0), 2, Detail::Default, before + length);
        assert!(page.text.contains("## Fix"), "part 1 fits without a second heading");
        assert_eq!(page.next, Some(at(1, 2)));
        let page = printer(&session).page(at(1, 0), 2, Detail::Default, before + length - 1);
        assert_eq!(page.next, Some(at(1, 1)), "next is the first part that did not fit");
    }

    #[test]
    fn pages_follow_each_other_without_gaps_or_repeats() {
        let session = session();
        let (mut from, mut pages) = (at(0, 0), Vec::new());
        loop {
            let page = printer(&session).page(from, 3, Detail::Default, 300);
            pages.push(page.text);
            match page.next {
                Some(next) => from = next,
                None => break,
            }
        }
        assert!(pages.len() > 2);
        let parts = chunk_texts(&session);
        for (index, turn) in session.turns.iter().enumerate() {
            for block in blocks(turn, Detail::Default, parts.get(&index).map_or(&[], Vec::as_slice), session.meta.cwd.as_deref()) {
                assert_eq!(pages.iter().filter(|page| page.contains(&block)).count(), 1, "{block}");
            }
        }
    }

    fn repeated(text: &str, count: usize, turns: usize) -> Session {
        let transcript = (0..turns).fold(Transcript::new("/p"), |transcript, _| transcript.user(&text.repeat(count)));
        parse("s1", &transcript)
    }

    fn page_ends(session: &Session, budget: usize) -> Option<Position> {
        printer(session).page(Position::start(0), session.turns.len(), Detail::Default, budget).next
    }

    #[test]
    fn a_page_holds_as_many_characters_in_any_script() {
        let ascii = repeated("x", 100, 6);
        for wide in ["я", "語", "🤖"] {
            let wide = repeated(wide, 100, 6);
            for budget in (50..1000).step_by(25) {
                assert_eq!(page_ends(&wide, budget), page_ends(&ascii, budget), "budget {budget}");
                assert_eq!(printer(&wide).outline(0, budget).next, printer(&ascii).outline(0, budget).next, "budget {budget}");
            }
        }
    }

    #[test]
    fn a_later_turn_costs_its_heading_in_characters() {
        let session = repeated("x", 20, 2);
        let whole = printer(&session).page(at(0, 0), 2, Detail::Default, 0).text;
        assert!(whole.len() > char_len(&whole), "the heading of a turn holds more than ASCII: {whole}");
        assert_eq!(page_ends(&session, char_len(&whole)), None, "exactly what the two turns cost");
        assert_eq!(page_ends(&session, char_len(&whole) - 1), Some(at(1, 0)));
    }

    #[test]
    fn a_page_of_several_blocks_stays_within_the_budget() {
        let session = repeated("я", 40, 12);
        for budget in 1..600 {
            let mut from = Position::start(0);
            loop {
                let page = printer(&session).page(from, session.turns.len(), Detail::Default, budget);
                let blocks = page.text.matches("\n---\n\n").count();
                assert!(blocks == 1 || char_len(&page.text) <= budget, "budget {budget}: {} characters", char_len(&page.text));
                match page.next {
                    Some(next) => from = next,
                    None => break,
                }
            }
        }
    }

    #[test]
    fn the_outline_has_a_line_per_turn() {
        let session = session();
        let page = printer(&session).outline(0, 0);
        assert_eq!((page.first, page.last, page.next), (0, 2, None));
        insta::assert_snapshot!(page.text, @r"
        - #0 👤 10:00 · Why does the token refresh fail? (56 chars)
        - #1 🤖 10:01 · 2 messages · 1086 chars · 1 tool call → The clock skew check compared local time with UTC. The clock skew check compared local ti…
        - #2 👤 2026-01-02 08:00 · `/model opus`
        ");
    }

    #[test]
    fn the_outline_counts_one_char_and_one_tool_call_in_the_singular() {
        let transcript =
            Transcript::new("/p").user("question").assistant("a", vec![text("x"), tool_use("t", "Bash", json!({"command": "ls"}))]);
        let session = parse("s1", &transcript);
        let page = printer(&session).outline(0, 0);
        assert!(page.text.contains("· 1 message · 1 char · 1 tool call → x"), "{}", page.text);
    }

    #[test]
    fn the_outline_counts_no_tool_calls_in_the_plural() {
        let transcript = Transcript::new("/p").user("question").assistant("a", vec![text("xy")]);
        let session = parse("s1", &transcript);
        let page = printer(&session).outline(0, 0);
        assert!(page.text.contains("· 1 message · 2 chars · 0 tool calls → xy"), "{}", page.text);
    }

    #[test]
    fn the_gist_of_a_reply_skips_headings() {
        assert_eq!(gist("\n## Cause\n\n### Detail\n\n  The clock was off.\n## Fix"), Some("The clock was off."));
        assert_eq!(gist("Plain first line\n## Later"), Some("Plain first line"));
        assert_eq!(gist("## Only\n\n# headings"), Some("## Only"));
        assert_eq!(gist("#hashtag start"), Some("#hashtag start"));
        assert_eq!(gist(" \n\n"), None);
    }

    #[test]
    fn the_outline_is_paged() {
        let session = session();
        let first = printer(&session).outline(0, 0).text.lines().next().unwrap().len() + 1;
        let page = printer(&session).outline(0, first);
        assert_eq!((page.first, page.last, page.next), (0, 0, Some(at(1, 0))));
        let page = printer(&session).outline(1, 1);
        assert_eq!((page.first, page.last, page.next), (1, 1, Some(at(2, 0))));
        assert_eq!(printer(&session).outline(2, 1).next, None);
    }

    fn heading(turn: &Turn, detail: Detail) -> String {
        Heading { index: 3, turn, clock: utc().starting(timestamp("2026-01-01T09:00:00.000Z")), detail, continued: 0 }.to_string()
    }

    #[test]
    fn headings_show_the_model_only_in_deep_detail() {
        let turn = Turn::Assistant(AssistantTurn {
            timestamp: timestamp("2026-01-01T10:04:00.000Z"),
            model: Some("opus".to_owned()),
            blocks: Vec::new(),
        });
        assert_eq!(heading(&turn, Detail::Default), "### #3 🤖 Claude — 10:04");
        assert_eq!(heading(&turn, Detail::Deep), "### #3 🤖 Claude (opus) — 10:04");
    }

    #[test]
    fn headings_show_the_agent_type_when_there_is_one() {
        let agent = |agent_type: Option<&str>| {
            Turn::Agent(AgentTurn {
                timestamp: timestamp("2026-01-02T10:04:00.000Z"),
                description: "Find it".to_owned(),
                agent_type: agent_type.map(str::to_owned),
                text: "Found.".to_owned(),
            })
        };
        assert_eq!(heading(&agent(Some("Explore")), Detail::Default), "### #3 🛰 Agent (Explore) — 2026-01-02 10:04");
        assert_eq!(heading(&agent(None), Detail::Default), "### #3 🛰 Agent — 2026-01-02 10:04");
    }

    #[test]
    fn rows_show_the_short_id_and_their_span() {
        let row = row(Some("/p"), false);
        let meta = Meta::from(&row);
        assert_eq!(meta.id, "aaaa1111");
        assert_eq!(meta.turn_range().to_string(), "0–3");
        assert_eq!(meta.span(utc()).to_string(), "2026-03-01 09:00 – 2026-03-02 17:45");
    }

    #[test]
    fn marks_name_the_directory_unless_the_scope_is_one_directory() {
        let directory = Scope::Directory { root: "/p".into() };
        let (here, archived, unknown) = (row(Some("/p"), false), row(Some("/p"), true), row(None, true));
        assert_eq!(Meta::from(&here).marks(&directory).to_string(), "");
        assert_eq!(Meta::from(&here).marks(&Scope::All).to_string(), " · `/p`");
        assert_eq!(Meta::from(&here).marks(&Scope::Under { root: "/".into() }).to_string(), " · `/p`");
        assert_eq!(Meta::from(&archived).marks(&directory).to_string(), " · *archived*");
        assert_eq!(Meta::from(&unknown).marks(&Scope::All).to_string(), " · `?` · *archived*");
    }

    fn row(cwd: Option<&str>, archived: bool) -> SessionRow {
        SessionRow {
            path: "/projects/-p/aaaa1111-0000.jsonl".to_owned(),
            session_id: "aaaa1111-0000".to_owned(),
            meta: SessionMeta {
                title: "Title".to_owned(),
                cwd: cwd.map(str::to_owned),
                first: timestamp("2026-03-01T09:00:00.000Z"),
                last: timestamp("2026-03-02T17:45:00.000Z"),
            },
            turns: 4,
            archived,
        }
    }
}
