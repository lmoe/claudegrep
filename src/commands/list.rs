use anyhow::Result;

use crate::app::App;
use crate::commands::current_session;
use crate::view::ListView;

pub fn run(app: &App) -> Result<ListView> {
    let sessions = app.store.sessions(&app.filter)?;
    Ok(ListView { scope: app.filter.scope.clone(), sessions, current: current_session(), moved_root: app.moved_root() })
}
