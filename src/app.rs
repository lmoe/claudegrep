use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Local;

use crate::cli::Cli;
use crate::commands;
use crate::config::Config;
use crate::discover::{self, Scope};
use crate::embed::{self, Acceleration};
use crate::index::{self, Refresh};
use crate::store::{Filter, Store};
use crate::timestamp::Timestamp;
use crate::when::Edge;

pub fn run(cli: Cli) -> Result<String> {
    let (command, global) = cli.into_command();
    let now = Local::now();
    let since = global.since.map(|when| when.resolve(Edge::Start, now)).transpose()?;
    let until = global.until.map(|when| when.resolve(Edge::End, now)).transpose()?;
    let db_path = global.db.unwrap_or_else(discover::default_db_path);
    let store = Store::open(&db_path)?;
    commands::dispatch(command, Setup { store, db_path, all: global.all, under: global.under, since, until })
}

pub struct Setup {
    pub store: Store,
    db_path: PathBuf,
    all: bool,
    under: Option<PathBuf>,
    since: Option<Timestamp>,
    until: Option<Timestamp>,
}

/// Whether a command embeds text, so the ONNX Runtime has to be set up before the thread pool starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Needs {
    Index,
    Embedding,
}

pub struct App {
    pub store: Store,
    pub config: Config,
    pub filter: Filter,
    /// Whether documents can be embedded on a GPU. Only the commands that embed find out.
    pub acceleration: Acceleration,
    pub db_path: PathBuf,
    pub refresh: Refresh,
}

impl App {
    pub fn moved_root(&self) -> Option<PathBuf> {
        let root = &self.config.projects_root;
        (*root != discover::default_projects_root()).then(|| root.clone())
    }
}

impl Setup {
    pub fn open(mut self, needs: Needs) -> Result<App> {
        let config = Config::load(&self.store)?;
        let project_dirs = discover::project_dirs(&config.projects_root)?;
        lower_priority();
        let acceleration = if needs == Needs::Embedding { embed::prepare_runtime(config.device)? } else { Acceleration::None };
        rayon::ThreadPoolBuilder::new().num_threads(config.threads).build_global()?;

        let refresh = index::refresh(&mut self.store, &project_dirs)?;
        let scope = choose_scope(&self.store, self.all, self.under.as_deref())?;
        let filter = Filter { scope, since: self.since, until: self.until, exclude: None };
        Ok(App { store: self.store, config, filter, acceleration, db_path: self.db_path, refresh })
    }
}

fn choose_scope(store: &Store, all: bool, under: Option<&Path>) -> Result<Scope> {
    if all {
        return Ok(Scope::All);
    }
    if let Some(dir) = under {
        let dir = if dir.is_absolute() { dir.to_path_buf() } else { current_dir()?.join(dir) };
        return Ok(Scope::under(&dir));
    }
    let candidates = Scope::candidates(&current_dir()?);
    for scope in &candidates {
        if store.count_sessions(&Filter { scope: scope.clone(), ..Filter::default() })? > 0 {
            return Ok(scope.clone());
        }
    }
    Ok(candidates.into_iter().next().expect("the cwd is always a candidate"))
}

fn current_dir() -> Result<PathBuf> {
    std::env::current_dir()
        .context("cannot read the current directory, which may have been deleted; --all and an absolute --under DIR work without it")
}

/// claudegrep indexes in the background of a Claude Code session, so it yields the CPU to everything else.
#[cfg(unix)]
fn lower_priority() {
    // SAFETY: setpriority takes no pointers and only changes this process's scheduling priority. A failure is
    // reported through the return value, which is ignored because running at normal priority is fine.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
    }
}

#[cfg(not(unix))]
const fn lower_priority() {}
