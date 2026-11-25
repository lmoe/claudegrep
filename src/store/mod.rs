use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Params, Row, Statement, TransactionBehavior, params};

use crate::chunk::ChunkHash;
use crate::discover::{FileStamp, Scope};
use crate::timestamp::Timestamp;

mod rows;
mod schema;
mod tx;

use rows::FromRow;
pub use rows::{ArchivedSession, ArchivedSessionKey, ChunkRow, ParseState, Passage, PendingChunk, SessionRow, StoredVector};
pub use tx::{ArchiveRow, SessionKey, StoreTx};

/// Whether the session `s` passes a [`Filter`], whose values [`Filter::bind`] sets. A directory is below the root when
/// it starts with `:below`, the root and a slash; `LIKE` would also match `_` and `%` in it. A window leaves out only
/// sessions it knows to be outside it, so one without timestamps stays in.
const PASSES_FILTER: &str = "(:root IS NULL OR EXISTS (SELECT 1 FROM session_dirs d WHERE d.path = s.path
        AND (d.dir = :root OR substr(d.dir, 1, length(:below)) = :below)))
    AND s.session_id IS NOT :exclude
    AND (julianday(s.last_ts) < julianday(:since)) IS NOT TRUE
    AND (julianday(s.first_ts) > julianday(:until)) IS NOT TRUE";

pub struct Store {
    conn: Connection,
}

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub scope: Scope,
    /// Leaves out sessions that ended before this.
    pub since: Option<Timestamp>,
    /// Leaves out sessions that started after this.
    pub until: Option<Timestamp>,
    pub exclude: Option<String>,
}

impl Filter {
    fn bind(&self, stmt: &mut Statement<'_>) -> rusqlite::Result<()> {
        stmt.raw_bind_parameter(":root", self.scope.root().map(Path::to_string_lossy))?;
        stmt.raw_bind_parameter(":below", self.below())?;
        stmt.raw_bind_parameter(":exclude", self.exclude.as_deref())?;
        stmt.raw_bind_parameter(":since", self.since)?;
        stmt.raw_bind_parameter(":until", self.until)
    }

    /// The start of the name of each directory below the root of an `Under` scope.
    fn below(&self) -> Option<String> {
        match &self.scope {
            Scope::Under { root } => Some(format!("{}/", root.to_string_lossy().trim_end_matches('/'))),
            Scope::All | Scope::Directory { .. } => None,
        }
    }
}

#[derive(Debug)]
pub struct Stats {
    pub sessions: usize,
    pub chunks: usize,
    pub unique_chunks: usize,
    pub embedded: usize,
    pub vector_bytes: u64,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(60))?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")?;
        schema::ensure(&mut conn)?;
        Ok(Self { conn })
    }

    pub fn use_embedder(&mut self, embedder: &str) -> Result<bool> {
        if get_meta(&self.conn, "embedder")?.as_deref() == Some(embedder) {
            return Ok(false);
        }
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous = get_meta(&tx, "embedder")?;
        if previous.as_deref() == Some(embedder) {
            return Ok(false);
        }
        let dropped = tx.execute("DELETE FROM vectors", [])? > 0;
        set_meta(&tx, "embedder", embedder)?;
        tx.commit()?;
        Ok(dropped)
    }

    pub fn embedder(&self) -> Result<Option<String>> {
        get_meta(&self.conn, "embedder")
    }

    pub fn config_value(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM config WHERE key = ?1", [key], |row| row.get(0)).optional()?)
    }

    pub fn set_config_value(&self, key: &str, value: Option<&str>) -> Result<()> {
        match value {
            Some(value) => self.conn.execute("INSERT OR REPLACE INTO config (key, value) VALUES (?1, ?2)", [key, value])?,
            None => self.conn.execute("DELETE FROM config WHERE key = ?1", [key])?,
        };
        Ok(())
    }

    /// Reads a `T` from every row of a query for its columns that goes on with `rest`.
    fn select<T: FromRow>(&self, rest: &str, params: impl Params) -> Result<Vec<T>> {
        let mut stmt = self.conn.prepare(&format!("SELECT {} {rest}", T::columns()))?;
        let rows = stmt.query_map(params, |row| T::from_row(row, 0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn by_path<T: FromRow>(&self, table: &str) -> Result<HashMap<String, T>> {
        let mut stmt = self.conn.prepare(&format!("SELECT path, {} FROM {table}", T::columns()))?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, T::from_row(row, 1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn archived_files(&self) -> Result<HashMap<String, FileStamp>> {
        self.by_path("archive")
    }

    pub fn archived_sessions(&self) -> Result<Vec<ArchivedSessionKey>> {
        self.select("FROM archive WHERE parent IS NULL", [])
    }

    pub fn parse_state(&self) -> Result<HashMap<String, ParseState>> {
        self.by_path("sessions")
    }

    pub fn load_archive(&self, path: &str) -> Result<ArchivedSession> {
        let main = self.conn.query_row("SELECT data FROM archive WHERE path = ?1", [path], |row| row.get(0))?;
        let agents = self.select("FROM archive WHERE parent = ?1 ORDER BY path", [path])?;
        Ok(ArchivedSession { main, agents })
    }

    pub fn set_archived(&self, path: &str, archived: bool) -> Result<()> {
        self.conn.execute("UPDATE sessions SET archived = ?2 WHERE path = ?1", params![path, archived])?;
        Ok(())
    }

    pub fn forget(&mut self, path: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM archive WHERE path = ?1 OR parent = ?1", [path])?;
        tx.execute("DELETE FROM sessions WHERE path = ?1", [path])?;
        tx.commit()?;
        self.sync_text_index()?;
        self.gc_vectors()?;
        Ok(())
    }

    /// Takes the write lock at once. A transaction that reads first and writes later fails with "database is locked"
    /// at once, without waiting, when another process writes.
    pub fn transaction(&mut self) -> Result<StoreTx<'_>> {
        Ok(StoreTx(self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?))
    }

    pub fn sync_text_index(&self) -> Result<()> {
        self.conn.execute_batch(
            "DELETE FROM chunk_fts WHERE rowid NOT IN (SELECT key FROM chunks);
             INSERT INTO chunk_fts (rowid, text)
                 SELECT key, min(text) FROM chunks
                 WHERE key NOT IN (SELECT rowid FROM chunk_fts)
                 GROUP BY key;",
        )?;
        Ok(())
    }

    /// Prepares `sql`, which holds [`PASSES_FILTER`], with the values of `filter`. Read its rows with `raw_query`.
    fn filtered(&self, filter: &Filter, sql: &str) -> Result<Statement<'_>> {
        let mut stmt = self.conn.prepare(sql)?;
        filter.bind(&mut stmt)?;
        Ok(stmt)
    }

    fn filtered_row<T>(&self, filter: &Filter, sql: &str, read: impl FnOnce(&Row<'_>) -> rusqlite::Result<T>) -> Result<T> {
        let mut stmt = self.filtered(filter, sql)?;
        let mut rows = stmt.raw_query();
        let row = rows.next()?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        Ok(read(row)?)
    }

    pub fn lexical(&self, filter: &Filter, fts_query: &str, limit: usize) -> Result<Vec<ChunkHash>> {
        let mut stmt = self.filtered(
            filter,
            &format!(
                "WITH hits AS MATERIALIZED (
                     SELECT rowid, bm25(chunk_fts) AS rank FROM chunk_fts WHERE chunk_fts MATCH :query
                 )
                 SELECT c.hash FROM hits h
                 JOIN chunks c ON c.key = h.rowid
                 JOIN sessions s ON s.path = c.path
                 WHERE {PASSES_FILTER}
                 GROUP BY c.hash ORDER BY min(h.rank) LIMIT :limit"
            ),
        )?;
        stmt.raw_bind_parameter(":query", fts_query)?;
        stmt.raw_bind_parameter(":limit", limit)?;
        Ok(stmt.raw_query().mapped(|row| row.get(0)).collect::<rusqlite::Result<_>>()?)
    }

    pub fn gc_vectors(&self) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM vectors WHERE hash NOT IN (SELECT hash FROM chunks)", [])?)
    }

    pub fn pending(&self) -> Result<Vec<PendingChunk>> {
        self.select("FROM chunks c WHERE NOT EXISTS (SELECT 1 FROM vectors v WHERE v.hash = c.hash) GROUP BY c.hash", [])
    }

    pub fn insert_vectors(&mut self, vectors: &[StoredVector<'_>]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut insert = tx.prepare("INSERT OR REPLACE INTO vectors (hash, tokens, data) VALUES (?1, ?2, ?3)")?;
            for vector in vectors {
                insert.execute(params![vector.hash, vector.tokens, vector.data])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn for_each_vector(&self, filter: &Filter, mut f: impl FnMut(StoredVector<'_>)) -> Result<()> {
        let mut stmt = self.filtered(
            filter,
            &format!(
                "SELECT v.hash, v.tokens, v.data FROM vectors v
                 WHERE EXISTS (SELECT 1 FROM chunks c
                               JOIN sessions s ON s.path = c.path
                               WHERE c.hash = v.hash AND {PASSES_FILTER})"
            ),
        )?;
        let mut rows = stmt.raw_query();
        while let Some(row) = rows.next()? {
            f(StoredVector { hash: row.get(0)?, tokens: row.get(1)?, data: row.get_ref(2)?.as_blob()? });
        }
        Ok(())
    }

    pub fn occurrences(&self, filter: &Filter, hash: &ChunkHash) -> Result<Vec<ChunkRow>> {
        let mut stmt = self.filtered(
            filter,
            &format!(
                "SELECT {} FROM chunks c
                 JOIN sessions s ON s.path = c.path
                 WHERE c.hash = :hash AND {PASSES_FILTER} ORDER BY c.ts DESC",
                ChunkRow::columns()
            ),
        )?;
        stmt.raw_bind_parameter(":hash", hash)?;
        Ok(stmt.raw_query().mapped(ChunkRow::from_row).collect::<rusqlite::Result<_>>()?)
    }

    pub fn passages(&self, path: &str) -> Result<Vec<Passage>> {
        self.select("FROM chunks c WHERE c.path = ?1 ORDER BY c.turn, c.part", [path])
    }

    pub fn sessions(&self, filter: &Filter) -> Result<Vec<SessionRow>> {
        let mut stmt = self.filtered(
            filter,
            &format!(
                "SELECT {} FROM sessions s
                 WHERE s.title IS NOT NULL AND {PASSES_FILTER} ORDER BY s.last_ts DESC",
                SessionRow::columns()
            ),
        )?;
        Ok(stmt.raw_query().mapped(|row| SessionRow::from_row(row, 0)).collect::<rusqlite::Result<_>>()?)
    }

    pub fn count_sessions(&self, filter: &Filter) -> Result<usize> {
        let sql = format!("SELECT count(*) FROM sessions s WHERE s.title IS NOT NULL AND {PASSES_FILTER}");
        self.filtered_row(filter, &sql, |row| row.get(0))
    }

    pub fn stats(&self, filter: &Filter) -> Result<Stats> {
        let (chunks, unique_chunks) = self.filtered_row(
            filter,
            &format!(
                "SELECT count(*), count(DISTINCT c.hash) FROM chunks c
                 JOIN sessions s ON s.path = c.path WHERE {PASSES_FILTER}"
            ),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let (embedded, vector_bytes) = self.filtered_row(
            filter,
            &format!(
                "SELECT count(*), coalesce(sum(length(data)), 0) FROM vectors v
                 WHERE EXISTS (SELECT 1 FROM chunks c JOIN sessions s ON s.path = c.path
                               WHERE c.hash = v.hash AND {PASSES_FILTER})"
            ),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(Stats { sessions: self.count_sessions(filter)?, chunks, unique_chunks, embedded, vector_bytes })
    }
}

fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| row.get(0)).optional()?)
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)", [key, value])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{self, Role};
    use crate::test_support::lines::Transcript;
    use std::time::Instant;

    use crate::test_support::store::{TempStore, hold_write_lock};

    pub(super) const STAMP: FileStamp = FileStamp { size: 7, mtime: 1_767_261_600_000_000_000 };

    pub(super) fn archive_row(path: &str, parent: Option<&str>, data: &[u8]) -> ArchiveRow {
        ArchiveRow {
            path: path.to_owned(),
            parent: parent.map(str::to_owned),
            project_dir: "-p".to_owned(),
            session_id: "s1".to_owned(),
            meta: parent.map(|_| format!("meta of {path}")),
            stamp: STAMP,
            data: data.to_vec(),
        }
    }

    fn archive_all(store: &mut TempStore, rows: &[ArchiveRow]) {
        let tx = store.transaction().unwrap();
        for row in rows {
            tx.archive(row).unwrap();
        }
        tx.commit().unwrap();
    }

    #[test]
    fn archived_files_return_the_stamps_they_were_stored_with() {
        let mut store = TempStore::new();
        archive_all(&mut store, &[archive_row("-p/s1.jsonl", None, b"")]);
        assert_eq!(store.archived_files().unwrap().get("-p/s1.jsonl"), Some(&STAMP));
    }

    #[test]
    fn an_archived_session_is_its_main_transcript_with_its_agents_in_path_order() {
        let mut store = TempStore::new();
        archive_all(
            &mut store,
            &[
                archive_row("-p/s1/b.jsonl", Some("-p/s1.jsonl"), b"agent b"),
                archive_row("-p/s1.jsonl", None, b"main"),
                archive_row("-p/s1/a.jsonl", Some("-p/s1.jsonl"), b"agent a"),
            ],
        );
        let key = ArchivedSessionKey { path: "-p/s1.jsonl".to_owned(), project_dir: "-p".to_owned(), session_id: "s1".to_owned() };
        assert_eq!(store.archived_sessions().unwrap(), [key]);

        let archived = store.load_archive("-p/s1.jsonl").unwrap();
        assert_eq!(archived.main, b"main");
        let agents: Vec<_> = archived.agents.iter().map(|agent| (agent.meta.as_deref().unwrap(), agent.data.as_slice())).collect();
        assert_eq!(agents, [("meta of -p/s1/a.jsonl", b"agent a".as_slice()), ("meta of -p/s1/b.jsonl", b"agent b".as_slice())]);
    }

    #[test]
    fn parse_state_follows_the_session() {
        let mut store = TempStore::new();
        store.add("-p", "s1", &Transcript::new("/p").user("hello there"));
        let state = |store: &TempStore| store.parse_state().unwrap()["-p/s1.jsonl"];
        assert_eq!(state(&store), ParseState { version: chunk::VERSION, archived: false });
        store.set_archived("-p/s1.jsonl", true).unwrap();
        assert_eq!(state(&store), ParseState { version: chunk::VERSION, archived: true });
    }

    #[test]
    fn a_chunk_is_pending_until_its_vector_is_stored() {
        let mut store = TempStore::new();
        store.add("-p", "s1", &Transcript::new("/p").user("hello there"));
        let hash = ChunkHash::of("hello there");
        assert_eq!(store.pending().unwrap(), [PendingChunk { hash, text: "hello there".to_owned() }]);

        store.insert_vectors(&[StoredVector { hash, tokens: 2, data: &[1, 2, 3] }]).unwrap();
        assert_eq!(store.pending().unwrap(), []);
        let mut read = Vec::new();
        store.for_each_vector(&Filter::default(), |vector| read.push((vector.hash, vector.tokens, vector.data.to_vec()))).unwrap();
        assert_eq!(read, [(hash, 2, vec![1, 2, 3])]);
    }

    fn sorted<'a>(paths: impl Iterator<Item = &'a String>) -> Vec<&'a str> {
        let mut paths: Vec<&str> = paths.map(String::as_str).collect();
        paths.sort_unstable();
        paths
    }

    #[test]
    fn an_occurrence_reads_its_session_and_its_passage() {
        let mut store = TempStore::new();
        let session = store.add("-p", "s1", &Transcript::new("/p").user("first").user("hello there").ai_title("Greeting"));
        let [occurrence] = store.occurrences(&Filter::default(), &ChunkHash::of("hello there")).unwrap().try_into().unwrap();
        assert_eq!((occurrence.session.path.as_str(), occurrence.session.session_id.as_str()), ("-p/s1.jsonl", "s1"));
        assert_eq!(occurrence.session.meta, session.meta);
        assert_eq!((occurrence.session.turns, occurrence.session.archived), (2, false));
        let expected =
            Passage { turn: 1, part: 0, role: Role::User, ts: "2026-01-01T10:01:00.000Z".parse().unwrap(), text: "hello there".to_owned() };
        assert_eq!(occurrence.passage, expected);
        assert_eq!(store.passages("-p/s1.jsonl").unwrap()[1], occurrence.passage);
    }

    #[test]
    fn the_same_embedder_keeps_the_vectors_and_a_new_one_drops_them() {
        let mut store = three_sessions();
        assert!(store.use_embedder("model-a").unwrap(), "vectors of an unknown embedder are dropped");
        assert_eq!(store.embedder().unwrap().as_deref(), Some("model-a"));

        let vector = StoredVector { hash: ChunkHash::of("words of early"), tokens: 1, data: &[1, 2] };
        store.insert_vectors(&[vector]).unwrap();
        assert!(!store.use_embedder("model-a").unwrap());
        assert_eq!(store.stats(&Filter::default()).unwrap().embedded, 1);

        assert!(store.use_embedder("model-b").unwrap());
        assert_eq!(store.embedder().unwrap().as_deref(), Some("model-b"));
        assert_eq!(store.stats(&Filter::default()).unwrap().embedded, 0);
        assert!(!store.use_embedder("model-c").unwrap(), "there was nothing left to drop");
    }

    #[test]
    fn a_transaction_that_writes_first_waits_for_another_writer() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("index.sqlite");
        let mut store = Store::open(&path).unwrap();
        let hold = Duration::from_millis(400);
        let (locked_at, holder) = hold_write_lock(&path, hold);

        store.insert_vectors(&[StoredVector { hash: ChunkHash::of("words"), tokens: 1, data: &[1] }]).unwrap();
        assert!(locked_at.elapsed() >= hold, "the write did not wait for the commit of the holder");
        holder.join().unwrap();
        assert_eq!(store.config_value("held").unwrap().as_deref(), Some("yes"));
        assert_eq!(store.gc_vectors().unwrap(), 1, "the vector is stored");
    }

    fn archived_store_with_held_lock(hold: Duration) -> (Store, Instant, std::thread::JoinHandle<()>, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("index.sqlite");
        let mut store = Store::open(&path).unwrap();
        let tx = store.transaction().unwrap();
        tx.archive(&archive_row("-p/s1.jsonl", None, b"one")).unwrap();
        tx.commit().unwrap();
        store.use_embedder("model-a").unwrap();
        let (locked_at, holder) = hold_write_lock(&path, hold);
        (store, locked_at, holder, dir)
    }

    #[test]
    fn moving_an_archive_waits_for_another_writer() {
        let hold = Duration::from_millis(400);
        let (mut store, locked_at, holder, _dir) = archived_store_with_held_lock(hold);
        let tx = store.transaction().unwrap();
        tx.move_archive("-p/s1.jsonl", "-q/s1.jsonl", "-q").unwrap();
        tx.commit().unwrap();
        assert!(locked_at.elapsed() >= hold, "the move did not wait for the commit of the holder");
        holder.join().unwrap();
        assert!(store.archived_files().unwrap().contains_key("-q/s1.jsonl"));
    }

    #[test]
    fn changing_the_embedder_waits_for_another_writer() {
        let hold = Duration::from_millis(400);
        let (mut store, locked_at, holder, _dir) = archived_store_with_held_lock(hold);
        store.use_embedder("model-b").unwrap();
        assert!(locked_at.elapsed() >= hold, "the change did not wait for the commit of the holder");
        holder.join().unwrap();
        assert_eq!(store.embedder().unwrap().as_deref(), Some("model-b"));
    }

    #[test]
    fn keeping_the_embedder_does_not_wait_for_another_writer() {
        let hold = Duration::from_millis(400);
        let (mut store, locked_at, holder, _dir) = archived_store_with_held_lock(hold);
        assert!(!store.use_embedder("model-a").unwrap());
        assert!(locked_at.elapsed() < hold, "a read waited for the holder");
        holder.join().unwrap();
    }

    fn two_archived_sessions() -> TempStore {
        let mut store = TempStore::new();
        let rows = [
            archive_row("-p/s1.jsonl", None, b"one"),
            archive_row("-p/s1/subagents/a.jsonl", Some("-p/s1.jsonl"), b"agent"),
            ArchiveRow { session_id: "s2".to_owned(), ..archive_row("-p/s2.jsonl", None, b"two") },
        ];
        archive_all(&mut store, &rows);
        store.add("-p", "s1", &Transcript::new("/p").user("shared words").user("only in one"));
        store.add("-p", "s2", &Transcript::new("/p").user("shared words").user("only in two"));
        let vectors =
            ["shared words", "only in one", "only in two"].map(|text| StoredVector { hash: ChunkHash::of(text), tokens: 1, data: &[1] });
        store.insert_vectors(&vectors).unwrap();
        store
    }

    fn vector_hashes(store: &Store) -> Vec<ChunkHash> {
        let mut hashes = Vec::new();
        store.for_each_vector(&Filter::default(), |vector| hashes.push(vector.hash)).unwrap();
        let unreferenced: usize =
            store.conn.query_row("SELECT count(*) FROM vectors WHERE hash NOT IN (SELECT hash FROM chunks)", [], |row| row.get(0)).unwrap();
        assert_eq!(unreferenced, 0, "every vector left belongs to a chunk");
        hashes.sort();
        hashes
    }

    fn fts_rows(store: &Store) -> usize {
        store.conn.query_row("SELECT count(*) FROM chunk_fts", [], |row| row.get(0)).unwrap()
    }

    #[test]
    fn forgetting_removes_everything_of_the_session_and_nothing_of_the_others() {
        let mut store = two_archived_sessions();
        assert_eq!(fts_rows(&store), 3);

        store.forget("-p/s1.jsonl").unwrap();
        assert_eq!(sorted(store.archived_files().unwrap().keys()), ["-p/s2.jsonl"], "the agent went with its session");
        assert_eq!(ids(&store, &Filter::default()), ["s2"]);
        assert_eq!(store.passages("-p/s1.jsonl").unwrap(), []);
        assert_eq!(store.lexical(&Filter::default(), "\"only in one\"", 10).unwrap(), []);
        assert_eq!(store.lexical(&Filter::default(), "\"only in two\"", 10).unwrap(), [ChunkHash::of("only in two")]);
        assert_eq!(fts_rows(&store), 2);
    }

    #[test]
    fn forgetting_keeps_a_vector_until_no_chunk_uses_it() {
        let mut store = two_archived_sessions();
        let mut expected = [ChunkHash::of("shared words"), ChunkHash::of("only in two")];
        expected.sort();
        store.forget("-p/s1.jsonl").unwrap();
        assert_eq!(vector_hashes(&store), expected, "the other session still uses the shared text");
        assert_eq!(store.lexical(&Filter::default(), "\"shared words\"", 10).unwrap(), [ChunkHash::of("shared words")]);

        store.forget("-p/s2.jsonl").unwrap();
        assert_eq!(vector_hashes(&store), []);
        assert_eq!(fts_rows(&store), 0);
    }

    #[test]
    fn a_session_row_holds_the_meta_of_its_transcript() {
        let mut store = TempStore::new();
        let session = store.add("-p", "s1", &Transcript::new("/p").user("hello there").ai_title("Greeting"));
        let rows = store.sessions(&Filter::default()).unwrap();
        assert_eq!(rows[0].meta, session.meta);
        assert_eq!(rows[0].turns, session.turns.len());
    }

    #[test]
    fn scope_separates_projects_sharing_an_encoded_name() {
        let mut store = TempStore::new();
        store.add("-p-a-b", "s1", &Transcript::new("/p/a-b").user("dash project"));
        store.add("-p-a-b", "s2", &Transcript::new("/p/a.b").user("dot project"));
        store.add("-p-a-b-sub", "s3", &Transcript::new("/p/a-b/sub").user("subdirectory"));

        let filter = Filter { scope: Scope::Directory { root: "/p/a-b".into() }, ..Filter::default() };
        assert_eq!(ids(&store, &filter), ["s1"]);
        assert_eq!(store.lexical(&filter, "\"project\"", 10).unwrap(), [ChunkHash::of("dash project")]);
        assert_eq!(store.stats(&filter).unwrap().chunks, 1);
        assert!(store.occurrences(&filter, &ChunkHash::of("dot project")).unwrap().is_empty());
        assert_eq!(store.pending().unwrap().len(), 3);
        assert_eq!(store.stats(&Filter::default()).unwrap().sessions, 3);
    }

    #[test]
    fn a_session_moved_with_cd_is_in_the_directory_it_started_in_and_in_the_one_it_moved_to() {
        let mut store = TempStore::new();
        // Started in /w/x, moved with `/cd` to /w/a-b, which holds the transcript, then a shell `cd` went to /w/a-b/src.
        let transcript = Transcript::new("/w/x").user("start").cwd("/w/a-b").user("moved").cwd("/w/a-b/src").user("shell");
        store.add("-w-a-b", "s1", &transcript);
        let directory = |root: &str| Filter { scope: Scope::Directory { root: root.into() }, ..Filter::default() };
        assert_eq!(ids(&store, &directory("/w/x")), ["s1"]);
        assert_eq!(ids(&store, &directory("/w/a-b")), ["s1"]);
        assert_eq!(ids(&store, &directory("/w/a/b")), Vec::<String>::new(), "no entry names it, though it has the same encoded name");
        assert_eq!(ids(&store, &directory("/w/a-b/src")), Vec::<String>::new(), "a shell cd does not move the session");
        assert_eq!(store.lexical(&directory("/w/x"), "\"moved\"", 10).unwrap(), [ChunkHash::of("moved")]);
    }

    fn sorted_ids(store: &Store, scope: Scope) -> Vec<String> {
        let mut ids = ids(store, &Filter { scope, ..Filter::default() });
        ids.sort();
        ids
    }

    #[test]
    fn under_a_directory_are_its_sessions_and_those_below_it_but_not_a_neighbour_that_shares_its_prefix() {
        let mut store = TempStore::new();
        for (project, cwd, id) in [
            ("-w-a", "/w/a", "self"),
            ("-w-a-b", "/w/a/b", "below"),
            ("-w-ab", "/w/ab", "neighbour"),
            ("-w-ab-c", "/w/ab/c", "below-neighbour"),
            ("-w-a--c", "/w/a_/c", "below-underscore"),
            ("-w", "/w", "above"),
        ] {
            store.add(project, id, &Transcript::new(cwd).user(&format!("words of {id}")));
        }
        store.add("-w-a-d", "moved", &Transcript::new("/x").user("start").cwd("/w/a/d").user("moved"));
        store.add("-y", "shell", &Transcript::new("/y").user("start").cwd("/w/a/e").user("shell"));

        let under = |root: &str| Scope::Under { root: root.into() };
        assert_eq!(sorted_ids(&store, under("/w/a")), ["below", "moved", "self"]);
        assert_eq!(sorted_ids(&store, under("/w/a_")), ["below-underscore"], "_ is no wildcard");
        assert_eq!(sorted_ids(&store, under("/w/a/b")), ["below"]);
        assert_eq!(sorted_ids(&store, under("/x")), ["moved"], "it started there");
        assert_eq!(sorted_ids(&store, under("/w")).len(), 7);
        assert_eq!(sorted_ids(&store, under("/")).len(), 8);
        assert_eq!(store.lexical(&Filter { scope: under("/w/a"), ..Filter::default() }, "\"words of\"", 10).unwrap().len(), 2);

        for root in ["/", "/w", "/w/a", "/w/a/b", "/w/ab", "/w/a_", "/w/a/d", "/w/a/e", "/x", "/y"] {
            let here = sorted_ids(&store, Scope::Directory { root: root.into() });
            let below = sorted_ids(&store, under(root));
            assert!(here.iter().all(|id| below.contains(id)), "{root}: {here:?} is not in {below:?}");
        }
    }

    fn ids(store: &Store, filter: &Filter) -> Vec<String> {
        store.sessions(filter).unwrap().into_iter().map(|session| session.session_id).collect()
    }

    fn at(day: u32) -> Option<Timestamp> {
        format!("2026-01-{day:02}T10:00:00.000Z").parse().ok()
    }

    fn three_sessions() -> TempStore {
        let mut store = TempStore::new();
        for (project, cwd, id, first) in [("-p", "/p", "early", 1), ("-p", "/p", "late", 10), ("-o", "/o", "other", 5)] {
            let transcript = Transcript::new(cwd).at(&at(first).unwrap().to_string()).user(&format!("words of {id}"));
            let transcript = transcript.at(&at(first + 1).unwrap().to_string()).user("words of them all");
            store.add(project, id, &transcript);
        }
        let hashes = ["early", "late", "other"].map(|id| ChunkHash::of(&format!("words of {id}")));
        let vectors = hashes.map(|hash| StoredVector { hash, tokens: 1, data: &[1, 2] });
        store.insert_vectors(&vectors).unwrap();
        store
    }

    type Window = (Option<Timestamp>, Option<Timestamp>);

    #[test]
    fn a_filter_combines_scope_window_and_exclusion() {
        let store = three_sessions();
        let all = || Scope::All;
        let p = || Scope::Directory { root: "/p".into() };
        let (no, excluded) = (None, Some("early"));
        let (any, since, until, both) = ((None, None), (at(3), None), (None, at(8)), (at(3), at(8)));
        let cases: [(Scope, Window, Option<&str>, &[&str]); 16] = [
            (all(), any, no, &["late", "other", "early"]),
            (all(), any, excluded, &["late", "other"]),
            (all(), since, no, &["late", "other"]),
            (all(), since, excluded, &["late", "other"]),
            (all(), until, no, &["other", "early"]),
            (all(), until, excluded, &["other"]),
            (all(), both, no, &["other"]),
            (all(), both, excluded, &["other"]),
            (p(), any, no, &["late", "early"]),
            (p(), any, excluded, &["late"]),
            (p(), since, no, &["late"]),
            (p(), since, excluded, &["late"]),
            (p(), until, no, &["early"]),
            (p(), until, excluded, &[]),
            (p(), both, no, &[]),
            (p(), both, excluded, &[]),
        ];
        for (scope, (since, until), exclude, expected) in cases {
            let filter = Filter { scope, since, until, exclude: exclude.map(str::to_owned) };
            assert_eq!(ids(&store, &filter), expected, "{filter:?}");
            assert_eq!(store.count_sessions(&filter).unwrap(), expected.len(), "{filter:?}");
            assert_eq!(store.stats(&filter).unwrap().sessions, expected.len(), "{filter:?}");
            let own_chunks = expected.len();
            let shared = usize::from(!expected.is_empty());
            assert_eq!(store.stats(&filter).unwrap().unique_chunks, own_chunks + shared, "{filter:?}");
            assert_eq!(store.stats(&filter).unwrap().embedded, own_chunks, "{filter:?}");
            let mut vectors = 0;
            store.for_each_vector(&filter, |_| vectors += 1).unwrap();
            assert_eq!(vectors, own_chunks, "{filter:?}");
            assert_eq!(store.lexical(&filter, "\"words\"", 10).unwrap().len(), own_chunks + shared, "{filter:?}");
            assert_eq!(store.occurrences(&filter, &ChunkHash::of("words of them all")).unwrap().len(), own_chunks, "{filter:?}");
        }
    }

    #[test]
    fn a_session_is_in_a_window_it_overlaps() {
        let store = three_sessions();
        let filter = |since, until| Filter { since, until, scope: Scope::Directory { root: "/o".into() }, ..Filter::default() };
        // `other` starts on day 5, and its second and last message is on day 6.
        let (first, last) = (at(5).unwrap(), at(6).unwrap());
        let [before, after] =
            [first.utc() - chrono::Duration::milliseconds(1), last.utc() + chrono::Duration::milliseconds(1)].map(Timestamp::from);
        let middle = Timestamp::from(first.utc() + chrono::Duration::hours(1));

        assert_eq!(ids(&store, &filter(Some(middle), None)), ["other"], "it ends after since, though it starts before");
        assert_eq!(ids(&store, &filter(None, Some(middle))), ["other"], "it starts before until, though it ends after");
        assert_eq!(ids(&store, &filter(Some(middle), Some(middle))), ["other"], "the window lies inside it");
        assert_eq!(ids(&store, &filter(Some(last), None)), ["other"], "ending at since is enough");
        assert_eq!(ids(&store, &filter(None, Some(first))), ["other"], "starting at until is enough");
        assert_eq!(ids(&store, &filter(Some(after), None)), Vec::<String>::new(), "it ended before since");
        assert_eq!(ids(&store, &filter(None, Some(before))), Vec::<String>::new(), "it starts after until");
    }

    #[test]
    fn a_session_without_timestamps_is_in_any_window_but_is_not_listed() {
        let mut store = TempStore::new();
        let key = SessionKey { path: "-p/s1.jsonl", project_dir: "-p", session_id: "s1", archived: false };
        let tx = store.transaction().unwrap();
        tx.write_session(&key, None, &[]).unwrap();
        tx.commit().unwrap();
        let filter = Filter { since: at(1), until: at(1), ..Filter::default() };
        let count = |sql: &str| store.filtered_row(&filter, sql, |row| row.get::<_, usize>(0)).unwrap();
        assert_eq!(count(&format!("SELECT count(*) FROM sessions s WHERE {PASSES_FILTER}")), 1);
        assert_eq!(store.count_sessions(&filter).unwrap(), 0);
    }

    #[test]
    fn a_query_changes_nothing_for_the_next_one() {
        let store = three_sessions();
        let narrow = Filter { exclude: Some("late".to_owned()), since: at(3), ..Filter::default() };
        assert_eq!(ids(&store, &narrow), ["other"]);
        assert_eq!(ids(&store, &Filter::default()), ["late", "other", "early"]);
    }
}
