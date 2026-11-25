mod config;
mod embedding;
mod forget;
mod index;
mod list;
mod read;
mod search;
mod status;

use anyhow::{Result, bail};

use crate::app::{Needs, Setup};
use crate::cli::Command;
use crate::discover::Scope;
use crate::render;
use crate::store::{Filter, SessionRow, Store};

pub fn dispatch(command: Command, setup: Setup) -> Result<String> {
    Ok(match command {
        Command::Config(args) => render::config(&config::run(&setup.store, &args)?),
        Command::List => render::list(&list::run(&setup.open(Needs::Index)?)?),
        Command::Read(options) => render::read(&read::run(&setup.open(Needs::Index)?, &options)?),
        Command::Search { query, options } => render::search(&search::run(&mut setup.open(Needs::Embedding)?, &query.join(" "), options)?),
        Command::Index => render::index(&index::run(&mut setup.open(Needs::Embedding)?)?),
        Command::Status => render::status(&status::run(&setup.open(Needs::Index)?)?),
        Command::Forget { session } => render::forget(&forget::run(&mut setup.open(Needs::Index)?, &session)?),
    })
}

fn current_session() -> Option<String> {
    std::env::var("CLAUDE_CODE_SESSION_ID").ok().filter(|id| !id.is_empty())
}

/// A session reference of at most this many digits is a number from `list`. Longer ones are id prefixes.
const LIST_NUMBER_MAX_DIGITS: usize = 3;

fn resolve_session(store: &Store, filter: &Filter, reference: &str) -> Result<SessionRow> {
    let sessions = store.sessions(filter)?;
    if let Ok(number) = reference.parse::<usize>()
        && number >= 1
        && number <= sessions.len()
        && reference.len() <= LIST_NUMBER_MAX_DIGITS
    {
        return Ok(sessions[number - 1].clone());
    }
    let matches: Vec<&SessionRow> = sessions.iter().filter(|s| s.session_id.starts_with(reference)).collect();
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] if !filter.scope.is_all() => {
            let all = if matches!(filter.scope, Scope::Under { .. }) { "use --all instead of --under" } else { "pass --all" };
            bail!(
                "no session {reference} in {}; see `{} list`, or {all} for other projects",
                render::scope_name(&filter.scope),
                render::invocation(&filter.scope)
            )
        }
        [] => bail!("no session {reference}; see `claudegrep --all list`"),
        many => bail!(
            "{reference} matches {} sessions: {}",
            many.len(),
            many.iter().map(|s| s.session_id.as_str()).collect::<Vec<_>>().join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::Scope;
    use crate::test_support::lines::Transcript;
    use crate::test_support::store::TempStore;

    fn store() -> TempStore {
        let mut store = TempStore::new();
        for (id, day) in [("9999aaaa-0000", "01"), ("1234ffff-0000", "02"), ("1234abcd-0000", "03")] {
            store.add("-p", id, &Transcript::new("/p").at(&format!("2026-01-{day}T10:00:00.000Z")).user("hello"));
        }
        store
    }

    fn resolve(store: &Store, reference: &str) -> Result<String> {
        resolve_session(store, &Filter::default(), reference).map(|row| row.session_id)
    }

    #[test]
    fn numbers_count_from_the_most_recent_session() {
        let store = store();
        assert_eq!(resolve(&store, "1").unwrap(), "1234abcd-0000");
        assert_eq!(resolve(&store, "3").unwrap(), "9999aaaa-0000");
    }

    #[test]
    fn a_unique_prefix_names_a_session() {
        let store = store();
        assert_eq!(resolve(&store, "1234a").unwrap(), "1234abcd-0000");
        assert_eq!(resolve(&store, "9").unwrap(), "9999aaaa-0000", "9 is past the list, so it is a prefix");
        assert_eq!(resolve(&store, "1234ffff-0000").unwrap(), "1234ffff-0000");
    }

    #[test]
    fn four_digits_are_a_prefix_not_a_number() {
        let store = store();
        let error = resolve(&store, "1234").unwrap_err().to_string();
        assert_eq!(error, "1234 matches 2 sessions: 1234abcd-0000, 1234ffff-0000");
    }

    #[test]
    fn zero_and_unknown_references_are_errors() {
        let store = store();
        assert_eq!(resolve(&store, "0").unwrap_err().to_string(), "no session 0; see `claudegrep --all list`");
        assert_eq!(resolve(&store, "zz").unwrap_err().to_string(), "no session zz; see `claudegrep --all list`");
        let directory = Filter { scope: Scope::Directory { root: "/p".into() }, ..Filter::default() };
        assert_eq!(
            resolve_session(&store, &directory, "zz").unwrap_err().to_string(),
            "no session zz in `/p`; see `claudegrep list`, or pass --all for other projects"
        );
        let under = Filter { scope: Scope::Under { root: "/q".into() }, ..Filter::default() };
        assert_eq!(
            resolve_session(&store, &under, "1234a").unwrap_err().to_string(),
            "no session 1234a in `/q` and below; see `claudegrep --under /q list`, or use --all instead of --under for other projects"
        );
    }
}
