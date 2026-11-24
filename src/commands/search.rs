use anyhow::Result;

use crate::app::App;
use crate::cli::SearchOptions;
use crate::commands::{current_session, embedding};
use crate::embed::Embedder;
use crate::search::{self, Window};
use crate::store::Filter;
use crate::view::SearchView;

pub fn run(app: &mut App, query: &str, options: SearchOptions) -> Result<SearchView> {
    let embedder = Embedder::load(&app.config.model, app.config.threads)?;
    embedding::embed(app, &embedder)?;
    let filter = Filter { exclude: current_session(), ..app.filter.clone() };
    let only_current = app.store.count_sessions(&app.filter)? > 0 && app.store.count_sessions(&filter)? == 0;
    let window = window(options, app.config.limit);
    let groups = search::find(&app.store, &filter, &embedder, query, window)?;
    Ok(SearchView { scope: filter.scope, query: query.to_owned(), limit: window.limit, only_current, groups, moved_root: app.moved_root() })
}

fn window(options: SearchOptions, limit: usize) -> Window {
    Window {
        limit: options.limit.unwrap_or(limit),
        before: options.before.or(options.context).unwrap_or(0),
        after: options.after.or(options.context).unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(limit: Option<usize>, before: Option<usize>, after: Option<usize>, context: Option<usize>) -> SearchOptions {
        SearchOptions { limit, before, after, context }
    }

    #[test]
    fn defaults_to_the_configured_limit_and_no_context() {
        assert_eq!(window(options(None, None, None, None), 8), Window { limit: 8, before: 0, after: 0 });
    }

    #[test]
    fn context_sets_both_sides_and_before_after_override_it() {
        assert_eq!(window(options(Some(3), None, None, Some(2)), 8), Window { limit: 3, before: 2, after: 2 });
        assert_eq!(window(options(None, Some(1), None, Some(2)), 8), Window { limit: 8, before: 1, after: 2 });
        assert_eq!(window(options(None, None, Some(4), None), 8), Window { limit: 8, before: 0, after: 4 });
    }
}
