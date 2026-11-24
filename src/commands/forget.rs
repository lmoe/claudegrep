use anyhow::{Result, bail};

use crate::app::App;
use crate::commands::resolve_session;
use crate::store::SessionRow;

pub fn run(app: &mut App, reference: &str) -> Result<SessionRow> {
    let row = resolve_session(&app.store, &app.filter, reference)?;
    if !row.archived {
        bail!("{} still exists in Claude Code and would be re-indexed; delete it there first", row.short_id());
    }
    app.store.forget(&row.path)?;
    Ok(row)
}
