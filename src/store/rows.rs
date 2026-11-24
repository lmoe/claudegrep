use rusqlite::Row;

use crate::chunk::{ChunkHash, Role};
use crate::discover::FileStamp;
use crate::timestamp::Timestamp;
use crate::transcript::SessionMeta;

pub(super) trait FromRow: Sized {
    /// The columns [`from_row`](Self::from_row) reads, in its order. The store's queries call `sessions` `s` and `chunks` `c`.
    const COLUMNS: &'static [&'static str];

    fn from_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<Self>;

    fn columns() -> String {
        Self::COLUMNS.join(", ")
    }
}

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub path: String,
    pub session_id: String,
    pub meta: SessionMeta,
    pub turns: usize,
    pub archived: bool,
}

impl SessionRow {
    pub fn short_id(&self) -> &str {
        self.session_id.get(..8).unwrap_or(&self.session_id)
    }
}

impl FromRow for SessionRow {
    const COLUMNS: &'static [&'static str] =
        &["s.path", "s.session_id", "s.title", "s.cwd", "s.first_ts", "s.last_ts", "s.turns", "s.archived"];

    fn from_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<Self> {
        Ok(Self {
            path: row.get(offset)?,
            session_id: row.get(offset + 1)?,
            meta: SessionMeta {
                title: row.get(offset + 2)?,
                cwd: row.get(offset + 3)?,
                first: row.get(offset + 4)?,
                last: row.get(offset + 5)?,
            },
            turns: row.get(offset + 6)?,
            archived: row.get(offset + 7)?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ChunkRow {
    pub session: SessionRow,
    pub passage: Passage,
}

impl ChunkRow {
    pub(super) fn columns() -> String {
        format!("{}, {}", SessionRow::columns(), Passage::columns())
    }

    pub(super) fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self { session: SessionRow::from_row(row, 0)?, passage: Passage::from_row(row, SessionRow::COLUMNS.len())? })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Passage {
    pub turn: usize,
    pub part: usize,
    pub role: Role,
    pub ts: Timestamp,
    pub text: String,
}

impl FromRow for Passage {
    const COLUMNS: &'static [&'static str] = &["c.turn", "c.part", "c.role", "c.ts", "c.text"];

    fn from_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<Self> {
        Ok(Self {
            turn: row.get(offset)?,
            part: row.get(offset + 1)?,
            role: row.get(offset + 2)?,
            ts: row.get(offset + 3)?,
            text: row.get(offset + 4)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingChunk {
    pub hash: ChunkHash,
    pub text: String,
}

impl FromRow for PendingChunk {
    const COLUMNS: &'static [&'static str] = &["c.hash", "min(c.text)"];

    fn from_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<Self> {
        Ok(Self { hash: row.get(offset)?, text: row.get(offset + 1)? })
    }
}

/// The vector of a chunk's text, as `vector::encode` wrote it. The blob is borrowed, so reading every vector for a search
/// costs no allocation per row.
#[derive(Debug, Clone, Copy)]
pub struct StoredVector<'a> {
    pub hash: ChunkHash,
    pub tokens: usize,
    pub data: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseState {
    /// The `chunk::VERSION` it was parsed with.
    pub version: u32,
    pub archived: bool,
}

impl FromRow for ParseState {
    const COLUMNS: &'static [&'static str] = &["parsed", "archived"];

    fn from_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<Self> {
        Ok(Self { version: row.get(offset)?, archived: row.get(offset + 1)? })
    }
}

impl FromRow for FileStamp {
    const COLUMNS: &'static [&'static str] = &["size", "mtime"];

    fn from_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<Self> {
        Ok(Self { size: row.get(offset)?, mtime: row.get(offset + 1)? })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedSessionKey {
    pub path: String,
    pub project_dir: String,
    pub session_id: String,
}

impl FromRow for ArchivedSessionKey {
    const COLUMNS: &'static [&'static str] = &["path", "project_dir", "session_id"];

    fn from_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<Self> {
        Ok(Self { path: row.get(offset)?, project_dir: row.get(offset + 1)?, session_id: row.get(offset + 2)? })
    }
}

pub struct ArchivedSession {
    pub main: Vec<u8>,
    pub agents: Vec<ArchivedAgent>,
}

pub struct ArchivedAgent {
    pub meta: Option<String>,
    pub data: Vec<u8>,
}

impl FromRow for ArchivedAgent {
    const COLUMNS: &'static [&'static str] = &["meta", "data"];

    fn from_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<Self> {
        Ok(Self { meta: row.get(offset)?, data: row.get(offset + 1)? })
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    #[test]
    fn a_value_is_read_from_its_offset() {
        let conn = Connection::open_in_memory().unwrap();
        let (stamp, state) = conn
            .query_row("SELECT 'skipped', 7, 9, 3, 1", [], |row| Ok((FileStamp::from_row(row, 1)?, ParseState::from_row(row, 3)?)))
            .unwrap();
        assert_eq!(stamp, FileStamp { size: 7, mtime: 9 });
        assert_eq!(state, ParseState { version: 3, archived: true });
    }

    #[test]
    fn a_chunk_row_lists_the_session_before_the_passage() {
        let (session, passage) = (SessionRow::columns(), Passage::columns());
        assert_eq!(ChunkRow::columns(), format!("{session}, {passage}"));
    }

    #[test]
    fn the_short_id_is_eight_characters_or_the_whole_id() {
        let row = |id: &str| SessionRow {
            path: String::new(),
            session_id: id.to_owned(),
            meta: SessionMeta {
                title: String::new(),
                cwd: None,
                first: "2026-01-01T10:00:00.000Z".parse().unwrap(),
                last: "2026-01-01T10:00:00.000Z".parse().unwrap(),
            },
            turns: 0,
            archived: false,
        };
        assert_eq!(row("1234abcd-0000").short_id(), "1234abcd");
        assert_eq!(row("s1").short_id(), "s1");
    }
}
