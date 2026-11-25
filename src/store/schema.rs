use anyhow::Result;
use rusqlite::Connection;

use super::{get_meta, set_meta};

const SCHEMA_VERSION: &str = "5";

/// An older `SCHEMA_VERSION` whose vectors and embedder hold in this one. An upgrade from it parses the archive again but
/// embeds nothing again. Any other version starts the vectors over.
const SAME_VECTORS_SCHEMA_VERSION: &str = "4";

/// The vectors and the meta, which names their embedder.
const EMBEDDED: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE vectors (
    hash BLOB PRIMARY KEY,
    tokens INTEGER NOT NULL,
    data BLOB NOT NULL
) WITHOUT ROWID;
";

/// What a refresh parses from the archive.
const PARSED: &str = "
CREATE TABLE sessions (
    path TEXT PRIMARY KEY,
    project_dir TEXT NOT NULL,
    session_id TEXT NOT NULL,
    parsed INTEGER NOT NULL,
    archived INTEGER NOT NULL DEFAULT 0,
    title TEXT,
    cwd TEXT,
    first_ts TEXT,
    last_ts TEXT,
    turns INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX sessions_project ON sessions (project_dir);
CREATE INDEX sessions_id ON sessions (session_id);
CREATE TABLE session_dirs (
    path TEXT NOT NULL REFERENCES sessions (path) ON DELETE CASCADE,
    dir TEXT NOT NULL,
    PRIMARY KEY (path, dir)
) WITHOUT ROWID;
CREATE TABLE chunks (
    path TEXT NOT NULL REFERENCES sessions (path) ON DELETE CASCADE,
    turn INTEGER NOT NULL,
    part INTEGER NOT NULL,
    role TEXT NOT NULL,
    ts TEXT NOT NULL,
    text TEXT NOT NULL,
    hash BLOB NOT NULL,
    key INTEGER NOT NULL,
    PRIMARY KEY (path, turn, part)
) WITHOUT ROWID;
CREATE INDEX chunks_hash ON chunks (hash);
CREATE INDEX chunks_key ON chunks (key);
CREATE VIRTUAL TABLE chunk_fts USING fts5 (text, tokenize = 'trigram');
";

const PERSISTENT: &str = "
CREATE TABLE IF NOT EXISTS config (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS archive (
    path TEXT PRIMARY KEY,
    parent TEXT,
    project_dir TEXT NOT NULL,
    session_id TEXT NOT NULL,
    meta TEXT,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL,
    data BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS archive_parent ON archive (parent);
";

/// Creates what `conn` is missing. An index of another `SCHEMA_VERSION` is rebuilt, except the archive and the settings,
/// and except the vectors of `SAME_VECTORS_SCHEMA_VERSION`.
pub(super) fn ensure(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch(PERSISTENT)?;
    let has_meta: bool = tx.query_row("SELECT count(*) FROM sqlite_master WHERE name = 'meta'", [], |row| row.get(0))?;
    let version = if has_meta { get_meta(&tx, "schema")? } else { None };
    if version.as_deref() != Some(SCHEMA_VERSION) {
        tx.execute_batch(
            "DROP TABLE IF EXISTS chunks; DROP TABLE IF EXISTS chunk_fts; DROP TABLE IF EXISTS session_dirs;
             DROP TABLE IF EXISTS sessions;",
        )?;
        if version.as_deref() != Some(SAME_VECTORS_SCHEMA_VERSION) {
            tx.execute_batch("DROP TABLE IF EXISTS vectors; DROP TABLE IF EXISTS meta;")?;
            tx.execute_batch(EMBEDDED)?;
        }
        tx.execute_batch(PARSED)?;
        set_meta(&tx, "schema", SCHEMA_VERSION)?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::super::tests::{STAMP, archive_row};
    use super::super::{Filter, SessionKey, Store, StoredVector};
    use super::*;
    use crate::chunk::{self, ChunkHash};
    use crate::test_support::lines::Transcript;

    #[test]
    fn a_new_schema_version_rebuilds_the_index_and_keeps_archive_and_config() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("index.sqlite");
        let mut store = Store::open(&path).unwrap();
        store.set_config_value("limit", Some("3")).unwrap();
        let session = crate::test_support::session::parse("s1", &Transcript::new("/p").user("hello there"));
        let key = SessionKey { path: "-p/s1.jsonl", project_dir: "-p", session_id: "s1", archived: false };
        let tx = store.transaction().unwrap();
        tx.archive(&archive_row("-p/s1.jsonl", None, b"main")).unwrap();
        tx.write_session(&key, Some(&session), &chunk::chunks(&session)).unwrap();
        tx.commit().unwrap();
        store.sync_text_index().unwrap();
        store.insert_vectors(&[StoredVector { hash: ChunkHash::of("hello there"), tokens: 1, data: &[1] }]).unwrap();
        store.use_embedder("model").unwrap();
        store.conn.execute("UPDATE meta SET value = 'older' WHERE key = 'schema'", []).unwrap();
        drop(store);

        let store = Store::open(&path).unwrap();
        assert_eq!(store.config_value("limit").unwrap().as_deref(), Some("3"));
        assert_eq!(store.archived_files().unwrap()["-p/s1.jsonl"], STAMP);
        assert_eq!(store.load_archive("-p/s1.jsonl").unwrap().main, b"main");
        assert!(store.parse_state().unwrap().is_empty(), "the next refresh parses the archive again");
        assert_eq!(store.passages("-p/s1.jsonl").unwrap(), []);
        assert_eq!(store.lexical(&Filter::default(), "\"hello\"", 10).unwrap(), []);
        assert_eq!(store.stats(&Filter::default()).unwrap().embedded, 0);
        assert_eq!(store.embedder().unwrap(), None);
        assert_eq!(get_meta(&store.conn, "schema").unwrap().as_deref(), Some(SCHEMA_VERSION));
        drop(store);

        let store = Store::open(&path).unwrap();
        assert_eq!(store.archived_files().unwrap().len(), 1, "opening a current index changes nothing");
    }
}
