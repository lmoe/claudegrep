use anyhow::Result;
use rusqlite::{OptionalExtension, Transaction, params};

use crate::chunk::{self, Chunk};
use crate::discover::{self, FileStamp};
use crate::transcript::Session;

pub struct ArchiveRow {
    pub path: String,
    pub parent: Option<String>,
    pub project_dir: String,
    pub session_id: String,
    pub meta: Option<String>,
    pub stamp: FileStamp,
    pub data: Vec<u8>,
}

pub struct SessionKey<'a> {
    pub path: &'a str,
    /// The project directory that holds the transcript, see `discover::session_dirs`.
    pub project_dir: &'a str,
    pub session_id: &'a str,
    pub archived: bool,
}

pub struct StoreTx<'a>(pub(super) Transaction<'a>);

impl StoreTx<'_> {
    pub fn archive(&self, row: &ArchiveRow) -> Result<()> {
        self.0
            .prepare_cached(
                "INSERT OR REPLACE INTO archive (path, parent, project_dir, session_id, meta, size, mtime, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?
            .execute(params![row.path, row.parent, row.project_dir, row.session_id, row.meta, row.stamp.size, row.stamp.mtime, row.data])?;
        Ok(())
    }

    /// Moves the archived transcript at `from` and its subagents to `to` in `project_dir`, and drops the sessions parsed
    /// at both paths, so that `to` is parsed again. Where the archive already holds a file at a new path, the larger of
    /// the two stays: transcripts only grow, so the smaller one is an older copy.
    pub fn move_archive(&self, from: &str, to: &str, project_dir: &str) -> Result<()> {
        let rows: Vec<(String, u64)> = self
            .0
            .prepare("SELECT path, size FROM archive WHERE path = ?1 OR parent = ?1")?
            .query_map([from], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        // A subagent's path is the transcript's without `.jsonl`, then `/subagents/…`.
        let (from_stem, to_stem) = (from.strip_suffix(".jsonl").unwrap_or(from), to.strip_suffix(".jsonl").unwrap_or(to));
        for (path, size) in rows {
            let Some(rest) = path.strip_prefix(from_stem) else {
                continue;
            };
            let target = format!("{to_stem}{rest}");
            let kept: Option<u64> = self.0.query_row("SELECT size FROM archive WHERE path = ?1", [&target], |row| row.get(0)).optional()?;
            if kept.is_some_and(|kept| kept >= size) {
                self.0.execute("DELETE FROM archive WHERE path = ?1", [&path])?;
            } else {
                let parent = (path != from).then_some(to);
                self.0.execute("DELETE FROM archive WHERE path = ?1", [&target])?;
                self.0.execute(
                    "UPDATE archive SET path = ?2, parent = ?3, project_dir = ?4 WHERE path = ?1",
                    params![path, target, parent, project_dir],
                )?;
            }
        }
        self.0.execute("DELETE FROM sessions WHERE path IN (?1, ?2)", [from, to])?;
        Ok(())
    }

    /// Replaces the session at `key.path` and its chunks. Without a `session` it is recorded as parsed and empty.
    pub fn write_session(&self, key: &SessionKey<'_>, session: Option<&Session>, chunks: &[Chunk]) -> Result<()> {
        self.0.execute("DELETE FROM sessions WHERE path = ?1", [key.path])?;
        self.0.execute(
            "INSERT INTO sessions (path, session_id, parsed, archived, title, cwd, first_ts, last_ts, turns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                key.path,
                key.session_id,
                chunk::VERSION,
                key.archived,
                session.map(|s| &s.meta.title),
                session.and_then(|s| s.meta.cwd.as_ref()),
                session.map(|s| s.meta.first),
                session.map(|s| s.meta.last),
                session.map_or(0, |s| s.turns.len()),
            ],
        )?;
        if let Some(session) = session {
            let mut insert = self.0.prepare_cached("INSERT INTO session_dirs (path, dir) VALUES (?1, ?2)")?;
            for dir in discover::session_dirs(key.project_dir, &session.cwds) {
                insert.execute(params![key.path, dir])?;
            }
        }
        let mut insert = self
            .0
            .prepare_cached("INSERT INTO chunks (path, turn, part, role, ts, text, hash, key) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)")?;
        for chunk in chunks {
            insert.execute(params![
                key.path,
                chunk.turn,
                chunk.part,
                chunk.role,
                chunk.timestamp,
                chunk.text,
                chunk.hash,
                chunk.hash.fts_rowid()
            ])?;
        }
        Ok(())
    }

    pub fn commit(self) -> Result<()> {
        Ok(self.0.commit()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::Scope;
    use crate::store::Filter;
    use crate::test_support::lines::Transcript;
    use crate::test_support::session;
    use crate::test_support::store::TempStore;

    const STAMP: FileStamp = FileStamp { size: 7, mtime: 1 };

    fn row() -> ArchiveRow {
        ArchiveRow {
            path: "-p/s1.jsonl".to_owned(),
            parent: None,
            project_dir: "-p".to_owned(),
            session_id: "s1".to_owned(),
            meta: None,
            stamp: STAMP,
            data: b"old".to_vec(),
        }
    }

    const KEY: SessionKey<'static> = SessionKey { path: "-p/s1.jsonl", project_dir: "-p", session_id: "s1", archived: false };

    fn write(store: &mut TempStore, text: &str) {
        let session = session::parse("s1", &Transcript::new("/p").user(text));
        let tx = store.transaction().unwrap();
        tx.write_session(&KEY, Some(&session), &chunk::chunks(&session)).unwrap();
        tx.commit().unwrap();
    }

    fn texts(store: &TempStore) -> Vec<String> {
        store.passages(KEY.path).unwrap().into_iter().map(|passage| passage.text).collect()
    }

    #[test]
    fn nothing_is_written_until_the_commit() {
        let mut store = TempStore::new();
        let tx = store.transaction().unwrap();
        tx.archive(&row()).unwrap();
        drop(tx);
        assert!(store.archived_files().unwrap().is_empty());

        let tx = store.transaction().unwrap();
        tx.archive(&row()).unwrap();
        tx.commit().unwrap();
        assert_eq!(store.archived_files().unwrap()[KEY.path], STAMP);
    }

    #[test]
    fn archiving_a_file_again_replaces_it() {
        let mut store = TempStore::new();
        let newer = FileStamp { size: 9, mtime: 2 };
        let tx = store.transaction().unwrap();
        tx.archive(&row()).unwrap();
        tx.archive(&ArchiveRow { stamp: newer, data: b"new".to_vec(), ..row() }).unwrap();
        tx.commit().unwrap();
        assert_eq!(store.archived_files().unwrap()[KEY.path], newer);
        assert_eq!(store.load_archive(KEY.path).unwrap().main, b"new");
    }

    #[test]
    fn writing_a_session_again_replaces_its_chunks() {
        let mut store = TempStore::new();
        write(&mut store, "first text");
        write(&mut store, "second text");
        assert_eq!(texts(&store), ["second text"]);
        assert_eq!(store.stats(&Filter::default()).unwrap().sessions, 1);
    }

    #[test]
    fn writing_a_session_again_replaces_its_directories() {
        let mut store = TempStore::new();
        store.add("-p", "s1", &Transcript::new("/old").user("first text"));
        store.add("-p", "s1", &Transcript::new("/new").user("second text"));
        let directory = |root: &str| Filter { scope: Scope::Directory { root: root.into() }, ..Filter::default() };
        assert_eq!(store.count_sessions(&directory("/old")).unwrap(), 0);
        assert_eq!(store.count_sessions(&directory("/new")).unwrap(), 1);
    }

    #[test]
    fn a_session_with_no_messages_is_parsed_but_not_listed() {
        let mut store = TempStore::new();
        let tx = store.transaction().unwrap();
        tx.write_session(&KEY, None, &[]).unwrap();
        tx.commit().unwrap();
        assert_eq!(store.parse_state().unwrap()[KEY.path].version, chunk::VERSION);
        assert!(store.sessions(&Filter::default()).unwrap().is_empty());
    }
}
