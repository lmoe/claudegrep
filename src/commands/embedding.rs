use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};

use crate::app::App;
use crate::config::{self, Setting};
use crate::embed::pipeline::{self, Progress};
use crate::embed::{Acceleration, Device, Documents, Embedder, Encoder, fall_back};

/// Fewer chunks than this are embedded on the CPU, which has done them before a GPU session is ready.
const GPU_MIN_CHUNKS: usize = 64;

/// Embeds every chunk that has no vector yet: on the GPU when there is one and the work is worth starting it, else on
/// the CPU, as `embedder` does. Work of that size that runs on the CPU although a GPU was to be used says so in a line.
pub fn embed(app: &mut App, embedder: &Embedder) -> Result<usize> {
    let App { store, acceleration, config, .. } = app;
    let documents = Documents::new(embedder);
    let mut bar = None;
    let embedded = pipeline::embed_pending(store, &documents, |progress| match progress {
        Progress::Reembedding => eprintln!("claudegrep: model changed to {}, re-embedding", embedder.id()),
        Progress::Started { total } => {
            if total >= GPU_MIN_CHUNKS {
                match &*acceleration {
                    Acceleration::None => {}
                    Acceleration::Unavailable(reason) => fall_back(reason),
                    Acceleration::Gpu(gpu) => match embedder.on(*gpu) {
                        Ok(on_gpu) => documents.use_gpu(on_gpu),
                        Err(error) => fall_back(format_args!("{error:#}")),
                    },
                }
            }
            bar = Some(progress_bar(total));
        }
        Progress::Embedded(n) => {
            if let Some(bar) = &bar {
                bar.inc(n as u64);
            }
        }
    })?;
    if let Some(bar) = bar {
        bar.finish_and_clear();
    }
    if documents.on_gpu() && config.device == Device::Auto {
        config::set(store, Setting::Device, Some(Device::Gpu.into()))?;
    }
    Ok(embedded)
}

fn progress_bar(total: usize) -> ProgressBar {
    let bar = ProgressBar::new(total as u64).with_style(
        ProgressStyle::with_template("{msg} [{bar:30}] {pos}/{len} chunks, {eta} left").expect("valid template").progress_chars("=> "),
    );
    bar.set_message("claudegrep: embedding");
    bar
}
