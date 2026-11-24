use std::ops::{Deref, DerefMut};

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
