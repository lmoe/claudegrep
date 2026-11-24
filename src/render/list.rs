use std::fmt::{self, Display, Formatter, Write};

use crate::render::clock::Clock;
use crate::render::session::Meta;
use crate::render::{nothing_here, scope_name};
use crate::store::SessionRow;
use crate::text::plural;
use crate::view::ListView;

pub const HEADING: &str = "# Sessions in ";

pub fn list(view: &ListView) -> String {
    view.to_string()
}

impl Display for ListView {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{HEADING}{}\n\n", scope_name(&self.scope))?;
        for (number, session) in self.sessions.iter().enumerate() {
            let meta = Meta::from(session);
            let span = meta.span(Clock::LOCAL);
            write!(
                f,
                "{}. **{}** — {span} · `{}` · {} {}{}",
                number + 1,
                session.meta.title,
                meta.id,
                meta.turns,
                plural(meta.turns, "turn", "turns"),
                meta.marks(&self.scope)
            )?;
            if self.current.as_deref() == Some(session.session_id.as_str()) {
                f.write_str(" · *current session*")?;
            }
            f.write_char('\n')?;
        }
        if self.sessions.is_empty() {
            f.write_str(&nothing_here(&self.scope, "list", self.moved_root.as_deref()))?;
        }
        Ok(())
    }
}

pub fn forget(session: &SessionRow) -> String {
    format!("Forgot {} ({}).\n", session.meta.title, session.short_id())
}
