use anyhow::Result;

use crate::app::App;
use crate::store::Filter;
use crate::view::StatusView;

pub fn run(app: &App) -> Result<StatusView> {
    Ok(StatusView {
        scope: app.filter.scope.clone(),
        scoped: app.store.stats(&app.filter)?,
        whole: app.store.stats(&Filter::default())?,
        model: app.config.model.clone(),
        embedder: app.store.embedder()?,
        projects_root: app.config.projects_root.clone(),
        db_path: app.db_path.clone(),
    })
}
