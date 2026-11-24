use rusqlite::ToSql;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use strum::{EnumString, IntoStaticStr};

use crate::text::char_len;
use crate::timestamp::Timestamp;
use crate::transcript::{Session, Turn};

pub const VERSION: u32 = 7;

const MAX_CHARS: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    Agent,
}

impl ToSql for Role {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        <&str>::from(self).to_sql()
    }
}

impl FromSql for Role {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        value.as_str()?.parse().map_err(|error: strum::ParseError| FromSqlError::Other(Box::new(error)))
    }
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub turn: usize,
    pub part: usize,
    pub role: Role,
    pub timestamp: Timestamp,
    pub text: String,
    pub hash: ChunkHash,
}

/// The hash of a chunk's text, which identifies its vector, so that text repeated across sessions is embedded once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkHash([u8; 32]);

impl ChunkHash {
    pub fn of(text: &str) -> Self {
        Self(*blake3::hash(text.as_bytes()).as_bytes())
    }

    pub const fn fts_rowid(&self) -> i64 {
        let bytes = &self.0;
        i64::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]])
    }

    #[cfg(test)]
    pub(crate) const fn filled(byte: u8) -> Self {
        Self([byte; 32])
    }
}

impl ToSql for ChunkHash {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

impl FromSql for ChunkHash {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        <[u8; 32]>::column_result(value).map(Self)
    }
}

pub fn chunks(session: &Session) -> Vec<Chunk> {
    let mut out = Vec::new();
    for (turn_index, turn) in session.turns.iter().enumerate() {
        let task;
        let (role, messages): (Role, Vec<&str>) = match turn {
            Turn::User(user) if user.command.is_none() => (Role::User, vec![user.text.as_str()]),
            Turn::User(_) => continue,
            Turn::Agent(agent) => {
                task = format!("Task: {}", agent.description);
                (Role::Agent, vec![task.as_str(), agent.text.as_str()])
            }
            Turn::Assistant(assistant) => (Role::Assistant, assistant.texts().collect()),
        };
        for (part, text) in pack(&messages, Level::Message).into_iter().enumerate() {
            out.push(Chunk { turn: turn_index, part, role, timestamp: turn.timestamp(), hash: ChunkHash::of(&text), text });
        }
    }
    out
}

/// For each part `messages` are packed into, the index of the message it starts in. Packing goes message by message,
/// so the parts of the first messages are the first parts of all of them, and a part starts in the message that first
/// adds it to the count.
pub fn part_starts(messages: &[&str]) -> Vec<usize> {
    let mut starts = Vec::new();
    for end in 1..=messages.len() {
        let parts = pack(&messages[..end], Level::Message).len();
        starts.resize(parts.max(starts.len()), end - 1);
    }
    starts
}

#[derive(Clone, Copy)]
enum Level {
    Message,
    Section,
    Paragraph,
    Line,
    Word,
}

impl Level {
    const fn joiner(self) -> &'static str {
        match self {
            Self::Message | Self::Section | Self::Paragraph => "\n\n",
            Self::Line => "\n",
            Self::Word => " ",
        }
    }

    const fn finer(self) -> Self {
        match self {
            Self::Message => Self::Section,
            Self::Section => Self::Paragraph,
            Self::Paragraph => Self::Line,
            Self::Line | Self::Word => Self::Word,
        }
    }

    /// `piece` as it is joined. Where the joiner is a blank line, the piece loses its own leading and trailing blank
    /// lines, so joining never makes two blank lines in a row, and a blank piece is dropped. Lines stay as they are,
    /// because a blank line inside a code fence is content.
    fn tidy(self, piece: &str) -> Option<&str> {
        match self {
            Self::Message | Self::Section | Self::Paragraph => Some(strip_blank_lines(piece)).filter(|piece| !piece.is_empty()),
            Self::Line | Self::Word => Some(piece),
        }
    }
}

fn strip_blank_lines(text: &str) -> &str {
    let content = text.len() - text.trim_start().len();
    let start = text[..content].rfind('\n').map_or(0, |i| i + 1);
    let content_end = text.trim_end().len();
    let end = text[content_end..].find('\n').map_or(text.len(), |i| content_end + i);
    &text[start..end.max(start)]
}

fn pack<S: AsRef<str>>(units: &[S], level: Level) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for unit in units {
        for piece in split(unit.as_ref(), level) {
            let Some(piece) = level.tidy(&piece) else {
                continue;
            };
            let fits = char_len(&current) + level.joiner().len() + char_len(piece) <= MAX_CHARS;
            if !current.trim().is_empty() && !fits && !is_heading_only(&current) {
                let headings = current.split_off(trailing_headings(&current));
                chunks.push(std::mem::replace(&mut current, headings));
            }
            if !current.is_empty() {
                current.push_str(level.joiner());
            }
            current.push_str(piece);
        }
    }
    chunks.push(current);
    chunks.into_iter().map(|chunk| chunk.trim().to_owned()).filter(|chunk| !chunk.is_empty()).collect()
}

fn split(unit: &str, level: Level) -> Vec<String> {
    if char_len(unit) <= MAX_CHARS {
        return vec![unit.to_owned()];
    }
    match level {
        Level::Message => pack(&sections(unit), Level::Section),
        Level::Section => pack(&paragraphs(unit), Level::Paragraph),
        Level::Paragraph => pack(&unit.lines().collect::<Vec<_>>(), Level::Line),
        Level::Line | Level::Word => words(unit),
    }
    .into_iter()
    .flat_map(|piece| {
        if char_len(&piece) > MAX_CHARS && !is_heading_only(piece.lines().next().unwrap_or("")) {
            split(&piece, level.finer())
        } else {
            vec![piece]
        }
    })
    .collect()
}

fn sections(text: &str) -> Vec<String> {
    let mut sections = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for (line, in_fence) in fenced(text) {
        if !in_fence && is_heading_line(line) && !current.is_empty() {
            sections.push(current.join("\n"));
            current.clear();
        }
        current.push(line);
    }
    sections.push(current.join("\n"));
    sections
}

fn paragraphs(text: &str) -> Vec<String> {
    let mut paragraphs = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for (line, in_fence) in fenced(text) {
        if line.trim().is_empty() && !in_fence {
            if !current.is_empty() {
                paragraphs.push(current.join("\n"));
                current.clear();
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        paragraphs.push(current.join("\n"));
    }
    paragraphs
}

/// The lines of `text`, each with whether it is inside a code fence. An opening fence line counts as inside, a
/// closing one as outside.
fn fenced(text: &str) -> impl Iterator<Item = (&str, bool)> {
    let mut in_fence = false;
    text.lines().map(move |line| {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        (line, in_fence)
    })
}

fn words(text: &str) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut rest = text.trim();
    while char_len(rest) > MAX_CHARS {
        let limit = rest.char_indices().nth(MAX_CHARS).map_or(rest.len(), |(i, _)| i);
        let at = rest[..limit].rfind(char::is_whitespace).filter(|&i| i > limit / 2).unwrap_or(limit);
        pieces.push(rest[..at].to_owned());
        rest = rest[at..].trim_start();
    }
    pieces.push(rest.to_owned());
    pieces
}

/// Where the heading lines at the end of `text` start, with any blank lines between them, or `text.len()` when its last
/// line is not a heading. A closed chunk hands these on to the next one, so a heading never ends a chunk.
fn trailing_headings(text: &str) -> usize {
    let mut start = text.len();
    let mut rest = text.trim_end();
    loop {
        let line = rest.rfind('\n').map_or(0, |i| i + 1);
        if !is_heading_line(&rest[line..]) {
            return start;
        }
        start = line;
        rest = rest[..line].trim_end();
    }
}

fn is_heading_only(text: &str) -> bool {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty()).peekable();
    lines.peek().is_some() && lines.all(is_heading_line)
}

/// Whether `line` is a heading, `#` to `######` and a space, without indentation. Chunk boundaries depend on this, so a
/// change to it needs a new `VERSION`.
pub fn is_heading_line(line: &str) -> bool {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    (1..=6).contains(&hashes) && line[hashes..].starts_with(' ')
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;

    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::test_support::lines::{Transcript, agent_meta, image, text, thinking, tool_result, tool_use};
    use crate::test_support::session;

    fn chunk(messages: &[&str]) -> Vec<String> {
        pack(messages, Level::Message)
    }

    #[test]
    fn parts_start_in_the_message_their_text_begins_in() {
        let long = format!("## Cause\n\n{}\n\n## Fix\n\n{}", "cause ".repeat(150).trim(), "fix ".repeat(247).trim());
        let messages = ["Checking the logs.", "Found it:", long.as_str(), "", "Done."];
        let parts = chunk(&messages);
        let starts = part_starts(&messages);
        assert_eq!(starts, [0, 2, 4], "{parts:#?}");
        assert_eq!(starts.len(), parts.len());
        for (part, &message) in parts.iter().zip(&starts) {
            let first_line = part.lines().next().unwrap();
            assert!(messages[message].contains(first_line), "{first_line} is not in message {message}");
        }
        assert_eq!(part_starts(&[]), Vec::<usize>::new());
        assert_eq!(part_starts(&["", " "]), Vec::<usize>::new());
    }

    #[test]
    fn small_messages_of_a_turn_are_packed_together() {
        let pieces = chunk(&["Checking the logs.", "Found it:", "The config was wrong."]);
        assert_eq!(pieces, ["Checking the logs.\n\nFound it:\n\nThe config was wrong."]);
    }

    #[test]
    fn a_message_is_not_split_when_it_fits_on_its_own() {
        let answer = "answer ".repeat(143);
        let pieces = chunk(&["Reading the file.", answer.trim()]);
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[1], answer.trim());
    }

    #[test]
    fn long_message_splits_at_headings() {
        let intro = "word ".repeat(120);
        let body = "detail ".repeat(60);
        let message = format!("{intro}\n\n## What the code shows\n\n{body}\n\nThe things that worry me:\n\n- one\n- two");
        let pieces = chunk(&[&message]);
        assert_eq!(pieces.len(), 2);
        assert!(pieces[1].starts_with("## What the code shows"));
        assert!(pieces[1].ends_with("- two"));
    }

    #[test]
    fn heading_never_ends_a_chunk() {
        let message = format!("## Heading\n\n{}\n\n- item", "word ".repeat(250));
        let pieces = chunk(&[&message]);
        assert!(pieces.len() > 1);
        assert!(pieces[0].starts_with("## Heading\n\nword"));
        assert!(pieces.iter().all(|p| !is_heading_only(p.lines().last().unwrap())));
    }

    #[test]
    fn long_text_respects_limit_and_keeps_all_words() {
        let paragraph = "word ".repeat(150);
        let text = format!("{paragraph}\n\n{paragraph}\n\n```\n{}\n```", "x = 1;\n".repeat(200));
        let pieces = chunk(&[&text]);
        assert!(pieces.len() > 2);
        assert!(pieces.iter().all(|p| char_len(p) <= MAX_CHARS));
        let words = |s: &str| s.split_whitespace().count();
        assert_eq!(pieces.iter().map(|p| words(p)).sum::<usize>(), words(&text));
    }

    #[test]
    fn giant_token_is_cut() {
        let pieces = chunk(&[&"a".repeat(2500)]);
        assert_eq!(pieces.iter().map(|p| char_len(p)).collect::<Vec<_>>(), vec![1000, 1000, 500]);
    }

    fn prose(len: usize) -> String {
        let text: String = "word ".repeat(len).chars().take(len - 1).collect();
        format!("{text}x")
    }

    #[test]
    fn exactly_full_message_is_not_split() {
        let message = prose(MAX_CHARS);
        assert_eq!(chunk(&[&message]), [message]);
        let pieces = chunk(&[&prose(MAX_CHARS + 1)]);
        assert_eq!(pieces.len(), 2);
        assert!(pieces.iter().all(|p| char_len(p) <= MAX_CHARS));
    }

    #[test]
    fn limit_counts_characters_not_bytes() {
        for c in ["🤖", "漢", "ä"] {
            assert_eq!(chunk(&[&c.repeat(MAX_CHARS)]).len(), 1);
            let lengths: Vec<_> = chunk(&[&c.repeat(MAX_CHARS + 1)]).iter().map(|p| char_len(p)).collect();
            assert_eq!(lengths, [MAX_CHARS, 1]);
        }
    }

    #[test]
    fn joiner_counts_towards_the_limit() {
        let half = (MAX_CHARS - 2) / 2;
        let (a, b) = ("🤖".repeat(half), "漢".repeat(half));
        assert_eq!(chunk(&[&a, &b]), [format!("{a}\n\n{b}")]);
        let longer = "漢".repeat(half + 1);
        assert_eq!(chunk(&[&a, &longer]), [a, longer]);
    }

    #[test]
    fn empty_and_whitespace_only_messages_are_dropped() {
        assert_eq!(chunk(&[]), Vec::<String>::new());
        assert_eq!(chunk(&["", "  ", "\n\t\n"]), Vec::<String>::new());
    }

    #[test]
    fn strip_blank_lines_keeps_the_other_lines_as_they_are() {
        assert_eq!(strip_blank_lines("\n \n    code\n  more  \n\n"), "    code\n  more  ");
        assert_eq!(strip_blank_lines("text"), "text");
        assert_eq!(strip_blank_lines(" \n\t"), "");
    }

    #[test]
    fn joined_pieces_are_separated_by_one_blank_line() {
        assert_eq!(
            chunk(&["Found it:\n", "\n\nThe config was wrong.", "  \n", "Fixed."]),
            ["Found it:\n\nThe config was wrong.\n\nFixed."]
        );
        let intro = "word ".repeat(150).trim_end().to_owned();
        let message = format!("{intro}\n\n## Cause\n\nThe clock.\n\n\n## Fix\n\n{}", "detail ".repeat(100));
        let pieces = chunk(&[&message]);
        assert_eq!(pieces.len(), 2);
        assert!(pieces[0].ends_with("word\n\n## Cause\n\nThe clock."), "{:?}", pieces[0]);
        assert!(pieces[1].starts_with("## Fix\n\ndetail"));
    }

    #[test]
    fn blank_lines_inside_a_fence_are_kept() {
        let code = format!("```\n{}\n\n\n{}\n```", "let a = 1;\n".repeat(60), "let b = 2;\n".repeat(60));
        let pieces = chunk(&[&code]);
        assert!(pieces.len() > 1);
        assert!(pieces.iter().any(|p| p.contains("let a = 1;\n\n\n\nlet b = 2;")));
    }

    fn filler(word: &str) -> String {
        format!("{word} ").repeat(180)
    }

    fn assert_no_heading_ends_a_chunk(pieces: &[String]) {
        for piece in pieces {
            assert!(!is_heading_line(piece.lines().last().unwrap()), "{piece:?}");
        }
    }

    #[test]
    fn nested_headings_never_end_a_chunk() {
        let message = format!(
            "# Report\n\n## Part one\n\n### Detail\n\n{}\n\n## Part two\n\nIn short:\n\n#### Deep\n\n{}\n\n### Last\n\nFinally:\n\n###### Tiny\n\n{}",
            filler("alpha"),
            filler("beta"),
            filler("gamma")
        );
        let pieces = chunk(&[&message]);
        assert!(pieces.len() > 3);
        assert_no_heading_ends_a_chunk(&pieces);
        assert!(pieces[0].starts_with("# Report\n\n## Part one\n\n### Detail\n\nalpha"));
    }

    #[test]
    fn heading_directly_above_a_subheading_does_not_end_a_chunk() {
        let message = format!("## Part one\n\n{}\n\n## Part two\n\n#### Deep\n\n{}", filler("alpha"), filler("beta"));
        let pieces = chunk(&[&message]);
        assert_no_heading_ends_a_chunk(&pieces);
        assert!(pieces.iter().any(|p| p.starts_with("## Part two\n\n#### Deep\n\nbeta")), "{pieces:?}");
    }

    #[test]
    fn trailing_headings_start_after_the_last_other_line() {
        assert_eq!(trailing_headings("text\n\n## A\n\n### B\n"), "text\n\n".len());
        assert_eq!(trailing_headings("text\n## A"), "text\n".len());
        assert_eq!(trailing_headings("## A\ntext"), "## A\ntext".len());
        assert_eq!(trailing_headings("## A\n\n## B"), 0);
        assert_eq!(trailing_headings(""), 0);
    }

    #[test]
    fn long_run_of_headings_stays_whole() {
        let headings = (0..200).map(|i| format!("## Heading {i}")).collect::<Vec<_>>().join("\n\n");
        let message = format!("{}\n\n{headings}\n\n{}", filler("alpha"), filler("beta"));
        let pieces = chunk(&[&message]);
        assert_no_heading_ends_a_chunk(&pieces);
        let words = |text: &str| text.split_whitespace().map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(pieces.iter().flat_map(|p| words(p)).collect::<Vec<_>>(), words(&message));
    }

    #[test]
    fn seven_hashes_or_no_space_is_not_a_heading() {
        assert!(is_heading_line("# a") && is_heading_line("###### a"));
        assert!(!is_heading_line("####### a") && !is_heading_line("#a") && !is_heading_line(" # a"));
    }

    #[test]
    fn turns_without_text_keep_their_numbers() {
        let reply = format!("## Plan\n\n{}\n\n## Result\n\n{}", "step ".repeat(150), "done ".repeat(150));
        let session = session::parse(
            "s",
            &Transcript::new("/w")
                .user("Why does the build fail?")
                .assistant("m1", vec![tool_use("t1", "Bash", json!({ "command": "make" }))])
                .user_blocks(vec![image()])
                .user("<command-name>/clear</command-name>")
                .assistant("m2", vec![text(&reply)]),
        );
        let chunks = chunks(&session);
        let positions: Vec<_> = chunks.iter().map(|c| (c.turn, c.part, c.role)).collect();
        assert_eq!(positions, [(0, 0, Role::User), (4, 0, Role::Assistant), (4, 1, Role::Assistant)]);
        assert_eq!(chunks[1].timestamp.to_string(), "2026-01-01T10:04:00.000Z");
        for chunk in &chunks {
            assert_eq!(chunk.timestamp, session.turns[chunk.turn].timestamp());
            assert_eq!(chunk.hash, ChunkHash::of(&chunk.text));
        }
    }

    #[test]
    fn slash_commands_produce_no_chunk() {
        let session = session::parse(
            "s",
            &Transcript::new("/w")
                .user("<command-name>/model</command-name>\n<command-args>opus</command-args>")
                .user("<bash-input>ls src</bash-input>")
                .user("What changed?")
                .assistant("m1", vec![text("Two files.")]),
        );
        let chunks: Vec<_> = chunks(&session).into_iter().map(|c| (c.turn, c.text)).collect();
        assert_eq!(chunks, [(2, "What changed?".to_owned()), (3, "Two files.".to_owned())]);
    }

    #[test]
    fn agent_turn_starts_with_its_task() {
        let main = Transcript::new("/w").user("Find the bug").assistant("m1", vec![text("Asking an agent.")]);
        let report = "The bug is in src/auth.rs, where the clock is compared in local time.";
        let agent =
            Transcript::sidechain("/w").at("2026-01-01T10:01:30.000Z").user("Find the bug\nin src").assistant("r1", vec![text(report)]);
        let meta = agent_meta("Locate the bug", Some("Explore"));
        let chunks = chunks(&session::parse_with_agents("s", &main, &[(&agent, Some(&meta))]));
        let agent = &chunks[2];
        assert_eq!((agent.turn, agent.part, agent.role), (2, 0, Role::Agent));
        assert_eq!(agent.text, format!("Task: Locate the bug\n\n{report}"));
        assert_eq!(agent.timestamp.to_string(), "2026-01-01T10:02:30.000Z");
    }

    #[test]
    fn long_agent_report_has_its_task_only_in_the_first_chunk() {
        let main = Transcript::new("/w").user("Find the bug").assistant("m1", vec![text("Asking an agent.")]);
        let agent =
            Transcript::sidechain("/w").at("2026-01-01T10:01:30.000Z").user("Find it").assistant("r1", vec![text(&"finding ".repeat(200))]);
        let chunks: Vec<_> =
            chunks(&session::parse_with_agents("s", &main, &[(&agent, None)])).into_iter().filter(|c| c.role == Role::Agent).collect();
        assert!(chunks.len() > 1);
        assert!(chunks[0].text.starts_with("Task: Find it"));
        assert!(chunks[1..].iter().all(|c| !c.text.contains("Task:")));
    }

    #[test]
    fn fence_lines_are_inside_from_opening_to_before_closing() {
        let text = "a\n```rust\n# not a heading\n```\nb";
        let flags: Vec<_> = fenced(text).map(|(_, in_fence)| in_fence).collect();
        assert_eq!(flags, [false, true, true, false, false]);
    }

    #[test]
    fn indented_fences_count() {
        let flags: Vec<_> = fenced("  ```\nx\n   ```\ny").map(|(_, in_fence)| in_fence).collect();
        assert_eq!(flags, [true, true, false, false]);
    }

    #[test]
    fn heading_inside_a_fence_is_not_a_split_point() {
        let text = "intro\n```sh\n## comment\n```\n## Real heading\nbody";
        assert_eq!(sections(text), ["intro\n```sh\n## comment\n```", "## Real heading\nbody"]);
    }

    #[test]
    fn blank_line_inside_a_fence_does_not_split_the_paragraph() {
        let text = "one\n\n```\nx\n\ny\n```\n\ntwo";
        assert_eq!(paragraphs(text), ["one", "```\nx\n\ny\n```", "two"]);
    }

    #[test]
    fn unclosed_fence_runs_to_the_end() {
        let text = "intro\n```\n## not a heading\n\nstill code";
        assert_eq!(sections(text), [text]);
        assert_eq!(paragraphs(text), ["intro\n```\n## not a heading\n\nstill code"]);
    }

    #[test]
    fn hash_covers_every_byte() {
        assert_eq!(ChunkHash::of("a"), ChunkHash::of("a"));
        assert_ne!(ChunkHash::of("a"), ChunkHash::of("a "));
    }

    #[test]
    fn hash_rowid_is_the_first_eight_bytes_little_endian() {
        let mut bytes = [0xff; 32];
        bytes[..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(ChunkHash(bytes).fts_rowid(), 0x0807_0605_0403_0201);
    }

    #[test]
    fn hashes_order_by_their_bytes() {
        assert!(ChunkHash::filled(1) < ChunkHash::filled(2));
    }

    #[test]
    fn hash_is_stored_as_a_32_byte_blob_and_reads_back() {
        let conn = Connection::open_in_memory().unwrap();
        let hash = ChunkHash::of("a");
        let (kind, length, read): (String, usize, ChunkHash) =
            conn.query_row("SELECT typeof(?1), length(?1), ?1", [hash], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
        assert_eq!((kind.as_str(), length, read), ("blob", 32, hash));
    }

    #[test]
    fn a_blob_of_another_length_is_not_a_hash() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(conn.query_row("SELECT x'0102'", [], |row| row.get::<_, ChunkHash>(0)).is_err());
    }

    /// Every role with its name, which the index stores, so these must never change.
    const ROLES: [(Role, &str); 3] = [(Role::User, "user"), (Role::Assistant, "assistant"), (Role::Agent, "agent")];

    #[test]
    fn roles_round_trip_through_their_names() {
        for (role, name) in ROLES {
            assert_eq!(<&str>::from(role), name);
            assert_eq!(name.parse::<Role>(), Ok(role));
        }
    }

    #[test]
    fn an_unknown_role_is_an_error() {
        for bad in ["", "system", "User", "ASSISTANT", " user", "👤 User"] {
            assert!(bad.parse::<Role>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn roles_are_stored_as_their_names_and_read_back() {
        let conn = Connection::open_in_memory().unwrap();
        for (role, name) in ROLES {
            let (text, read): (String, Role) = conn.query_row("SELECT ?1, ?1", [role], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
            assert_eq!((text.as_str(), read), (name, role));
        }
    }

    #[test]
    fn a_stored_role_that_is_unknown_is_an_error() {
        let conn = Connection::open_in_memory().unwrap();
        for bad in ["SELECT 'system'", "SELECT 5", "SELECT NULL"] {
            assert!(conn.query_row(bad, [], |row| row.get::<_, Role>(0)).is_err(), "{bad}");
        }
    }

    fn golden_session() -> Session {
        let reply = format!(
            "The CI runner uses a different locale.\n\n## What differs\n\n{}\n\n```sh\n# run with the CI locale\nLANG=C cargo test\n\n## not a heading\n```\n\n### Details\n\n- `LANG` is unset on CI\n- the test compares sorted names\n\n## Fix\n\n{}",
            "The test sorts file names, and the order of ä and z depends on the locale. ".repeat(8),
            "Sort with a locale-independent key, so both machines agree. ".repeat(6),
        );
        let main = Transcript::new("/work/app")
            .user("Why does `cargo test` fail on CI but not locally? 🤖")
            .assistant(
                "m1",
                vec![
                    thinking("The CI config is the first place to look."),
                    text("Let me look at the CI config."),
                    tool_use("t1", "Read", json!({ "file_path": "/work/app/.github/workflows/ci.yml" })),
                ],
            )
            .user_blocks(vec![tool_result("t1", "env:\n  LANG: C")])
            .assistant("m2", vec![text("Found it:"), text(&reply)])
            .user("<command-name>/model</command-name>\n<command-args>opus</command-args>")
            .user_blocks(vec![image()])
            .user_blocks(vec![text("And this log, in Japanese?"), image()])
            .assistant("m3", vec![text(&"ログはロケールの違いを示しています。".repeat(70))]);
        let agent = Transcript::sidechain("/work/app")
            .at("2026-01-01T10:02:30.000Z")
            .user("Find where file names are sorted\nLook in src.")
            .assistant("r1", vec![text("Names are sorted in src/list.rs by sort_names(), which uses the default string order.")]);
        let meta = agent_meta("Find the sort", Some("Explore"));
        session::parse_with_agents("golden", &main, &[(&agent, Some(&meta))])
    }

    /// Pins the chunks of `golden_session`. **If this test fails, chunk text or boundaries changed: bump `VERSION`**, so
    /// every session is re-chunked, then update the snapshot and the pinned version and hash together.
    #[test]
    fn golden_chunks() {
        let mut listing = String::new();
        let mut combined = blake3::Hasher::new();
        for c in chunks(&golden_session()) {
            let role: &str = c.role.into();
            writeln!(listing, "--- {}.{} {role} {}\n{}", c.turn, c.part, c.timestamp, c.text).unwrap();
            combined.update(&[u8::try_from(c.turn).unwrap(), u8::try_from(c.part).unwrap()]).update(role.as_bytes()).update(&c.hash.0);
        }
        insta::with_settings!({ snapshot_path => "../tests/snapshots", prepend_module_to_snapshot => false }, {
            insta::assert_snapshot!("chunk__golden_chunks", listing);
        });
        assert_eq!(
            (VERSION, combined.finalize().to_hex().as_str()),
            (7, "8a4b70e66ae5451f4b1f41bf21edeb0357978f5d6685726b31fb0bb2a1d128e8")
        );
    }

    mod properties {
        use proptest::prelude::*;
        use proptest::test_runner::Config;

        use super::*;

        fn word() -> impl Strategy<Value = String> {
            prop::collection::vec(prop::sample::select(&['a', 'b', 'ä', 'ö', '漢', '字', '🤖'][..]), 1..12).prop_map(String::from_iter)
        }

        fn words(count: std::ops::Range<usize>) -> impl Strategy<Value = String> {
            prop::collection::vec(word(), count).prop_map(|words| words.join(" "))
        }

        fn heading() -> impl Strategy<Value = String> {
            (1..=7usize, words(1..5)).prop_map(|(level, title)| format!("{} {title}", "#".repeat(level)))
        }

        fn line(long: bool) -> impl Strategy<Value = String> {
            prop_oneof![
                6 => words(1..40),
                1 => words(150..300),
                2 => heading(),
                u32::from(long) => words(150..300).prop_map(|title| format!("## {title}")),
                u32::from(long) => (prop::collection::vec(heading(), 50..150), any::<bool>())
                    .prop_map(|(headings, spaced)| headings.join(if spaced { "\n\n" } else { "\n" })),
                1 => Just("```".to_owned()),
                3 => Just(String::new()),
                1 => words(1..10).prop_map(|line| format!("    {line}")),
                u32::from(long) => (prop::sample::select(&["x", "漢"][..]), 500..2500usize).prop_map(|(c, n)| c.repeat(n)),
            ]
        }

        fn messages(long: bool) -> impl Strategy<Value = Vec<String>> {
            prop::collection::vec(prop::collection::vec(line(long), 0..60).prop_map(|lines| lines.join("\n")), 0..4)
        }

        fn pieces(messages: &[String]) -> Vec<String> {
            pack(messages, Level::Message)
        }

        proptest! {
            // Failures are not saved to proptest-regressions/: the shrunk input is in the failure message.
            #![proptest_config(Config { failure_persistence: None, ..Config::default() })]

            #[test]
            fn non_whitespace_is_kept_in_order(messages in messages(true)) {
                let visible = |texts: &[String]| texts.concat().chars().filter(|c| !c.is_whitespace()).collect::<String>();
                prop_assert_eq!(visible(&pieces(&messages)), visible(&messages));
            }

            #[test]
            fn short_words_are_kept_in_order(messages in messages(false)) {
                let words = |texts: &[String]| texts.iter().flat_map(|t| t.split_whitespace()).map(str::to_owned).collect::<Vec<_>>();
                prop_assert_eq!(words(&pieces(&messages)), words(&messages));
            }

            /// A heading never ends a chunk, so it is glued to the next piece even when that one fills a chunk on its
            /// own. Everything after the leading headings fits.
            #[test]
            fn chunks_respect_the_limit_after_leading_headings(messages in messages(true)) {
                for piece in pieces(&messages) {
                    let body = piece.lines().skip_while(|line| line.trim().is_empty() || is_heading_line(line)).collect::<Vec<_>>();
                    let body = body.join("\n");
                    prop_assert!(char_len(&body) <= MAX_CHARS, "{} chars: {:?}", char_len(&body), piece);
                }
            }

            #[test]
            fn chunks_without_headings_respect_the_limit(messages in messages(true)) {
                for piece in pieces(&messages).iter().filter(|piece| !piece.lines().any(is_heading_line)) {
                    prop_assert!(char_len(piece) <= MAX_CHARS, "{} chars: {:?}", char_len(piece), piece);
                }
            }

            #[test]
            fn chunks_are_trimmed_and_not_empty(messages in messages(true)) {
                for piece in pieces(&messages) {
                    prop_assert!(!piece.is_empty());
                    prop_assert_eq!(piece.trim(), piece.as_str());
                }
            }

            /// The last chunk of a turn is exempt: when the text itself ends with a heading, nothing follows it.
            #[test]
            fn no_chunk_ends_with_a_heading_unless_it_is_only_headings(messages in messages(true)) {
                let pieces = pieces(&messages);
                for piece in pieces.iter().rev().skip(1) {
                    prop_assert!(is_heading_only(piece) || !is_heading_line(piece.lines().last().unwrap()), "{:?}", piece);
                }
            }

            #[test]
            fn joining_adds_no_blank_lines(messages in messages(true)) {
                let collapse = |text: &str| {
                    let mut text = text.to_owned();
                    while text.contains("\n\n\n") {
                        text = text.replace("\n\n\n", "\n\n");
                    }
                    text
                };
                let messages: Vec<_> = messages.iter().map(|m| collapse(m)).collect();
                for piece in pieces(&messages) {
                    prop_assert!(!piece.contains("\n\n\n"), "{:?}", piece);
                }
            }

            #[test]
            fn chunking_is_deterministic(messages in messages(true)) {
                prop_assert_eq!(pieces(&messages), pieces(&messages.clone()));
            }
        }
    }
}
