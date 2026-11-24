use std::path::PathBuf;

use anyhow::{Error, Result, anyhow, bail};
use strum::{EnumIter, EnumString, IntoEnumIterator, IntoStaticStr};

use crate::discover;
use crate::embed::{self, Device};
use crate::store::Store;

/// A setting of `claudegrep config`, stored in the index's `config` table under its name. `claudegrep config` lists them
/// in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, EnumString, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum Setting {
    Model,
    Limit,
    Context,
    Budget,
    Threads,
    Device,
    ProjectsRoot,
}

pub struct Key<T> {
    setting: Setting,
    parse: fn(&str) -> Result<T>,
}

const MODEL: Key<String> = Key {
    setting: Setting::Model,
    parse: |value| {
        if value.trim().is_empty() {
            bail!("the model must not be empty");
        }
        Ok(value.to_owned())
    },
};
pub const LIMIT: Key<usize> = Key { setting: Setting::Limit, parse: |value| positive(value, "the limit must be a positive number") };
const CONTEXT: Key<usize> = Key { setting: Setting::Context, parse: |value| count(value, "the context must be a number") };
const BUDGET: Key<usize> = Key { setting: Setting::Budget, parse: |value| count(value, "the budget must be a number") };
const THREADS: Key<usize> = Key { setting: Setting::Threads, parse: |value| positive(value, "threads must be a positive number") };
const DEVICE: Key<Device> =
    Key { setting: Setting::Device, parse: |value| value.parse().map_err(|_| Error::msg("the device must be auto, gpu or cpu")) };
/// Any path parses, because the default may not exist yet. [`Setting::check`] requires a new one to exist.
const PROJECTS_ROOT: Key<PathBuf> = Key { setting: Setting::ProjectsRoot, parse: |value| Ok(value.into()) };

impl<T> Key<T> {
    pub fn parse(&self, value: &str) -> Result<T> {
        (self.parse)(value)
    }

    pub fn load(&self, store: &Store) -> Result<T> {
        self.parse(&get(store, self.setting)?)
    }
}

impl Setting {
    pub fn named(name: &str) -> Result<Self> {
        name.parse().map_err(|_| {
            let known: Vec<&str> = Self::iter().map(Into::into).collect();
            anyhow!("unknown setting {name}; known: {}", known.join(", "))
        })
    }

    pub const fn help(self) -> &'static str {
        match self {
            Self::Model => {
                "embedding model: a Hugging Face repo with an ONNX ColBERT export, or a local directory; changing it re-embeds everything"
            }
            Self::Limit => "number of search results",
            Self::Context => "turns shown before and after `read --turn`",
            Self::Budget => "page size of `read` in characters, 0 for no limit",
            Self::Threads => "CPU threads for parsing and embedding; claudegrep also runs at the lowest scheduling priority",
            Self::Device => {
                "where documents are embedded: `gpu`, `cpu`, or `auto`, which uses a GPU when the machine has one and stores `gpu` once that worked"
            }
            Self::ProjectsRoot => "Claude Code's projects directory",
        }
    }

    pub fn default(self) -> String {
        match self {
            Self::Model => embed::DEFAULT_MODEL.to_owned(),
            Self::Limit => "8".to_owned(),
            Self::Context => "2".to_owned(),
            // Claude Code hands Claude 30,000 characters of a command's output and saves anything longer to a file, so a
            // page with its header and footer stays under that.
            Self::Budget => "24000".to_owned(),
            Self::Threads => default_threads().to_string(),
            Self::Device => <&str>::from(Device::Auto).to_owned(),
            Self::ProjectsRoot => discover::default_projects_root().to_string_lossy().into_owned(),
        }
    }

    pub fn needs_reembed(self) -> bool {
        self == Self::Model
    }

    pub fn check(self, value: &str) -> Result<()> {
        match self {
            Self::Model => MODEL.parse(value).map(drop),
            Self::Limit => LIMIT.parse(value).map(drop),
            Self::Context => CONTEXT.parse(value).map(drop),
            Self::Budget => BUDGET.parse(value).map(drop),
            Self::Threads => THREADS.parse(value).map(drop),
            Self::Device => DEVICE.parse(value).map(drop),
            Self::ProjectsRoot if !PROJECTS_ROOT.parse(value)?.is_dir() => bail!("{value} is not a directory"),
            Self::ProjectsRoot => Ok(()),
        }
    }
}

fn count(value: &str, error: &'static str) -> Result<usize> {
    value.parse().map_err(|_| Error::msg(error))
}

fn positive(value: &str, error: &'static str) -> Result<usize> {
    match count(value, error)? {
        0 => bail!(error),
        n => Ok(n),
    }
}

fn default_threads() -> usize {
    let cores = std::thread::available_parallelism().map_or(2, std::num::NonZero::get);
    (cores / 2).max(1)
}

pub fn get(store: &Store, setting: Setting) -> Result<String> {
    Ok(store.config_value(setting.into())?.unwrap_or_else(|| setting.default()))
}

pub fn set(store: &Store, setting: Setting, value: Option<&str>) -> Result<()> {
    if let Some(value) = value {
        setting.check(value)?;
    }
    store.set_config_value(setting.into(), value)
}

pub struct Config {
    pub model: String,
    pub limit: usize,
    pub context: usize,
    pub budget: usize,
    pub threads: usize,
    pub device: Device,
    pub projects_root: PathBuf,
}

impl Config {
    pub fn load(store: &Store) -> Result<Self> {
        Ok(Self {
            model: MODEL.load(store)?,
            limit: LIMIT.load(store)?,
            context: CONTEXT.load(store)?,
            budget: BUDGET.load(store)?,
            threads: THREADS.load(store)?,
            device: DEVICE.load(store)?,
            projects_root: PROJECTS_ROOT.load(store)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::test_support::store::TempStore;

    #[test]
    fn every_default_parses() {
        let store = TempStore::new();
        assert_eq!(MODEL.load(&store).unwrap(), embed::DEFAULT_MODEL);
        assert_eq!(LIMIT.load(&store).unwrap(), 8);
        assert_eq!(CONTEXT.load(&store).unwrap(), 2);
        assert_eq!(BUDGET.load(&store).unwrap(), 24000);
        assert!(THREADS.load(&store).unwrap() > 0);
        assert_eq!(DEVICE.load(&store).unwrap(), Device::Auto);
        assert_eq!(PROJECTS_ROOT.load(&store).unwrap(), discover::default_projects_root());
    }

    #[test]
    fn every_setting_but_the_projects_root_accepts_its_default() {
        for setting in Setting::iter().filter(|&setting| setting != Setting::ProjectsRoot) {
            assert!(setting.check(&setting.default()).is_ok(), "{setting:?}");
        }
    }

    #[test]
    fn keys_belong_to_their_setting() {
        let settings =
            [MODEL.setting, LIMIT.setting, CONTEXT.setting, BUDGET.setting, THREADS.setting, DEVICE.setting, PROJECTS_ROOT.setting];
        assert_eq!(Setting::iter().collect::<Vec<_>>(), settings);
    }

    /// Every setting with its name, which the index stores and the command line takes, so these must never change.
    const NAMES: [(Setting, &str); 7] = [
        (Setting::Model, "model"),
        (Setting::Limit, "limit"),
        (Setting::Context, "context"),
        (Setting::Budget, "budget"),
        (Setting::Threads, "threads"),
        (Setting::Device, "device"),
        (Setting::ProjectsRoot, "projects-root"),
    ];

    #[test]
    fn settings_round_trip_through_their_names() {
        for (setting, name) in NAMES {
            assert_eq!(<&str>::from(setting), name);
            assert_eq!(Setting::named(name).unwrap(), setting);
        }
    }

    #[test]
    fn settings_are_listed_in_the_order_of_the_config_command() {
        assert_eq!(Setting::iter().collect::<Vec<_>>(), NAMES.map(|(setting, _)| setting));
    }

    #[test]
    fn only_the_exact_name_is_a_setting() {
        for bad in ["Model", "ProjectsRoot", "projects_root", "projectsroot", " limit", ""] {
            assert!(Setting::named(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn values_parse_to_their_type() {
        assert_eq!(MODEL.parse("some/model").unwrap(), "some/model");
        assert_eq!(LIMIT.parse("5").unwrap(), 5);
        assert_eq!(CONTEXT.parse("0").unwrap(), 0);
        assert_eq!(BUDGET.parse("0").unwrap(), 0);
        assert_eq!(THREADS.parse("3").unwrap(), 3);
        assert_eq!(DEVICE.parse("gpu").unwrap(), Device::Gpu);
        assert_eq!(PROJECTS_ROOT.parse("/no/such/dir").unwrap(), PathBuf::from("/no/such/dir"));
    }

    #[test]
    fn invalid_values_are_rejected_with_a_helpful_message() {
        let cases = [
            (Setting::Model, "", "the model must not be empty"),
            (Setting::Model, "  ", "the model must not be empty"),
            (Setting::Limit, "0", "the limit must be a positive number"),
            (Setting::Limit, "-1", "the limit must be a positive number"),
            (Setting::Limit, "many", "the limit must be a positive number"),
            (Setting::Context, "-1", "the context must be a number"),
            (Setting::Context, "two", "the context must be a number"),
            (Setting::Budget, "1.5", "the budget must be a number"),
            (Setting::Threads, "0", "threads must be a positive number"),
            (Setting::Threads, "", "threads must be a positive number"),
            (Setting::ProjectsRoot, "/no/such/dir", "/no/such/dir is not a directory"),
        ];
        for (setting, value, message) in cases {
            assert_eq!(setting.check(value).unwrap_err().to_string(), message, "{setting:?} = {value:?}");
        }
    }

    #[test]
    fn an_unknown_setting_lists_the_known_ones() {
        let error = Setting::named("colour").unwrap_err();
        assert_eq!(error.to_string(), "unknown setting colour; known: model, limit, context, budget, threads, device, projects-root");
    }

    #[test]
    fn only_the_model_needs_reembedding() {
        let reembed: Vec<_> = Setting::iter().filter(|setting| setting.needs_reembed()).collect();
        assert_eq!(reembed, [Setting::Model]);
    }

    #[test]
    fn unset_restores_the_default() {
        let store = TempStore::new();
        set(&store, Setting::Limit, Some("5")).unwrap();
        assert_eq!(get(&store, Setting::Limit).unwrap(), "5");
        set(&store, Setting::Limit, None).unwrap();
        assert_eq!(get(&store, Setting::Limit).unwrap(), "8");
        assert_eq!(store.config_value("limit").unwrap(), None);
    }

    #[test]
    fn an_invalid_value_is_not_stored() {
        let store = TempStore::new();
        assert!(set(&store, Setting::Threads, Some("0")).is_err());
        assert_eq!(store.config_value("threads").unwrap(), None);
    }

    #[test]
    fn load_uses_the_defaults() {
        let config = Config::load(&TempStore::new()).unwrap();
        assert_eq!(config.model, embed::DEFAULT_MODEL);
        assert_eq!((config.limit, config.context, config.budget), (8, 2, 24000));
        assert!(config.threads > 0);
        assert_eq!(config.projects_root, discover::default_projects_root());
    }

    #[test]
    fn load_uses_the_stored_values() {
        let store = TempStore::new();
        let root = TempDir::new().unwrap();
        let values = [
            (Setting::Model, "some/model"),
            (Setting::Limit, "3"),
            (Setting::Context, "0"),
            (Setting::Budget, "500"),
            (Setting::Threads, "7"),
            (Setting::ProjectsRoot, root.path().to_str().unwrap()),
        ];
        for (setting, value) in values {
            set(&store, setting, Some(value)).unwrap();
        }
        let config = Config::load(&store).unwrap();
        assert_eq!(config.model, "some/model");
        assert_eq!((config.limit, config.context, config.budget, config.threads), (3, 0, 500, 7));
        assert_eq!(config.projects_root, root.path());
    }
}
