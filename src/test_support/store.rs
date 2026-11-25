use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tempfile::TempDir;

use crate::chunk;
use crate::store::{SessionKey, Store};
use crate::test_support::lines::Transcript;
use crate::test_support::session;
use crate::transcript::Session;

pub struct TempStore {
    store: Store,
    _dir: TempDir,
}

impl TempStore {
    pub fn new() -> Self {
        let dir = TempDir::new().expect("a temp dir");
        let store = Store::open(&dir.path().join("index.sqlite")).expect("the store opens");
        Self { store, _dir: dir }
    }

    pub fn add(&mut self, project_dir: &str, id: &str, transcript: &Transcript) -> Session {
        let session = session::parse(id, transcript);
        let chunks = chunk::chunks(&session);
        let path = format!("{project_dir}/{id}.jsonl");
        let key = SessionKey { path: &path, project_dir, session_id: id, archived: false };
        let tx = self.store.transaction().expect("a transaction");
        tx.write_session(&key, Some(&session), &chunks).expect("the session is written");
        tx.commit().expect("the transaction commits");
        self.store.sync_text_index().expect("the text index syncs");
        session
    }
}

impl Deref for TempStore {
    type Target = Store;

    fn deref(&self) -> &Store {
        &self.store
    }
}

impl DerefMut for TempStore {
    fn deref_mut(&mut self) -> &mut Store {
        &mut self.store
    }
}

/// Holds the write lock of the index at `path` for `hold` in another connection, and returns once it has the lock. The
/// index must exist. Returns the time of the lock and the handle: join it to wait for the commit.
pub fn hold_write_lock(path: &Path, hold: Duration) -> (Instant, JoinHandle<()>) {
    let shown = path.display().to_string();
    let path = path.to_owned();
    let (locked, wait_for_lock) = mpsc::channel();
    let holder = thread::spawn(move || {
        let take_lock = || -> rusqlite::Result<Connection> {
            let conn = Connection::open(&path)?;
            conn.execute_batch("BEGIN IMMEDIATE")?;
            conn.execute("INSERT OR REPLACE INTO config (key, value) VALUES ('held', 'yes')", [])?;
            Ok(conn)
        };
        let taken = take_lock().map(|conn| (conn, Instant::now()));
        let conn = match taken {
            Ok((conn, at)) => {
                locked.send(Ok(at)).expect("the test waits for the lock");
                conn
            }
            Err(error) => {
                locked.send(Err(error)).expect("the test waits for the lock");
                return;
            }
        };
        thread::sleep(hold);
        conn.execute_batch("COMMIT").expect("the holder commits");
    });
    let taken =
        wait_for_lock.recv().expect("the holder reports").unwrap_or_else(|error| panic!("cannot hold the write lock of {shown}: {error}"));
    (taken, holder)
}
