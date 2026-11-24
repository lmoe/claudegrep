use anyhow::Result;

use crate::app::App;
use crate::commands::{embedding, status};
use crate::embed::Embedder;
use crate::view::IndexView;

pub fn run(app: &mut App) -> Result<IndexView> {
    let embedder = Embedder::load(&app.config.model, app.config.threads)?;
    let embedded = embedding::embed(app, &embedder)?;
    Ok(IndexView { stored: app.refresh.stored, parsed: app.refresh.parsed, embedded, status: status::run(app)? })
}
