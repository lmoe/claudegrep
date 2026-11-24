use std::path::PathBuf;

use crate::discover::Scope;
use crate::render::Page;
use crate::search::SessionHits;
use crate::store::{SessionRow, Stats};

pub struct ListView {
    pub scope: Scope,
    pub sessions: Vec<SessionRow>,
    pub current: Option<String>,
    pub moved_root: Option<PathBuf>,
}

pub struct ReadView {
    pub scope: Scope,
    pub session: SessionRow,
    pub header: String,
    pub page: Page,
    pub last_turn: usize,
    pub mode: ReadMode,
}

pub enum ReadMode {
    Outline,
    Content {
        deep: bool,
        /// The first turn after the requested range, when the range ended before the session did.
        later: Option<usize>,
        /// The `--turn` and `--context` that show the turns before a `--turn` range.
        earlier: Option<Earlier>,
    },
}

pub struct Earlier {
    pub turn: usize,
    pub context: usize,
}

pub struct SearchView {
    pub scope: Scope,
    pub query: String,
    pub limit: usize,
    /// Nothing matched because the only session in scope is the one claudegrep runs in, which search skips.
    pub only_current: bool,
    pub groups: Vec<SessionHits>,
    pub moved_root: Option<PathBuf>,
}

pub struct StatusView {
    pub scope: Scope,
    pub scoped: Stats,
    pub whole: Stats,
    pub model: String,
    pub embedder: Option<String>,
    pub projects_root: PathBuf,
    pub db_path: PathBuf,
}

pub struct IndexView {
    pub stored: usize,
    pub parsed: usize,
    pub embedded: usize,
    pub status: StatusView,
}

pub enum ConfigView {
    All(Vec<ConfigEntry>),
    One { name: String, value: String, reembed: bool },
}

pub struct ConfigEntry {
    pub name: &'static str,
    pub value: String,
    pub is_default: bool,
    pub help: &'static str,
}
