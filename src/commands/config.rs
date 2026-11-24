use anyhow::Result;
use strum::IntoEnumIterator;

use crate::cli::ConfigArgs;
use crate::config::{self, Setting};
use crate::store::Store;
use crate::view::{ConfigEntry, ConfigView};

pub fn run(store: &Store, args: &ConfigArgs) -> Result<ConfigView> {
    let Some(name) = args.key.as_deref() else {
        return Ok(ConfigView::All(Setting::iter().map(|setting| entry(store, setting)).collect::<Result<_>>()?));
    };
    let setting = Setting::named(name)?;
    if args.unset {
        config::set(store, setting, None)?;
    } else if let Some(value) = &args.value {
        config::set(store, setting, Some(value))?;
    }
    let changed = args.unset || args.value.is_some();
    Ok(ConfigView::One {
        name: <&str>::from(setting).to_owned(),
        value: config::get(store, setting)?,
        reembed: setting.needs_reembed() && changed,
    })
}

fn entry(store: &Store, setting: Setting) -> Result<ConfigEntry> {
    let stored = store.config_value(setting.into())?;
    Ok(ConfigEntry {
        name: setting.into(),
        is_default: stored.is_none(),
        value: stored.unwrap_or_else(|| setting.default()),
        help: setting.help(),
    })
}
