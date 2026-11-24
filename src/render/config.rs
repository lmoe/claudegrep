use std::fmt::{self, Display, Formatter};

use crate::view::ConfigView;

pub const HEADING: &str = "# claudegrep config";

pub fn config(view: &ConfigView) -> String {
    view.to_string()
}

impl Display for ConfigView {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::All(entries) => {
                write!(f, "{HEADING}\n\n")?;
                for entry in entries {
                    let origin = if entry.is_default { " (default)" } else { "" };
                    writeln!(f, "- `{}` = `{}`{origin} — {}", entry.name, entry.value, entry.help)?;
                }
                Ok(())
            }
            Self::One { name, value, reembed } => {
                writeln!(f, "{name} = {value}")?;
                if *reembed {
                    f.write_str("The next search or `claudegrep index` embeds every chunk with this model.\n")?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::ConfigEntry;

    #[test]
    fn all_settings_are_listed_with_their_origin() {
        let entries = vec![
            ConfigEntry { name: "limit", value: "8".to_owned(), is_default: true, help: "matches per search" },
            ConfigEntry { name: "budget", value: "4000".to_owned(), is_default: false, help: "characters per page" },
        ];
        insta::assert_snapshot!(config(&ConfigView::All(entries)), @r"
        # claudegrep config

        - `limit` = `8` (default) — matches per search
        - `budget` = `4000` — characters per page
        ");
    }

    #[test]
    fn one_setting_is_shown_with_a_warning_when_it_reembeds() {
        let one = |reembed| ConfigView::One { name: "model".to_owned(), value: "some/model".to_owned(), reembed };
        assert_eq!(config(&one(false)), "model = some/model\n");
        assert_eq!(config(&one(true)), "model = some/model\nThe next search or `claudegrep index` embeds every chunk with this model.\n");
    }
}
