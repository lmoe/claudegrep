use std::fmt::{self, Display, Formatter};

use crate::discover::Scope;
use crate::render::clock::Clock;
use crate::render::markdown::Quote;
use crate::render::session::Meta;
use crate::render::turn::role_label;
use crate::render::{invocation, nothing_here, scope_name};
use crate::search::{Hit, SessionHits};
use crate::text::{plural, shorten};
use crate::view::SearchView;

pub const HEADING: &str = "# Search: ";

const QUERY_CHARS: usize = 120;

const ONLY_CURRENT: &str = "*(the only session started here is the current one, which search skips; \
                            `claudegrep --all search` covers every project)*\n";

pub fn search(view: &SearchView) -> String {
    view.to_string()
}

impl Display for SearchView {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let scope = &self.scope;
        let total: usize = self.groups.iter().map(|group| group.hits.len()).sum();
        let sessions = self.groups.len();
        write!(f, "{HEADING}{}\n\n", shorten(&self.query, QUERY_CHARS))?;
        writeln!(
            f,
            "*{total} {} in {sessions} {}, {}*",
            plural(total, "match", "matches"),
            plural(sessions, "session", "sessions"),
            scope_name(scope)
        )?;
        for (rank, group) in self.groups.iter().enumerate() {
            write!(f, "{}", Group { rank: rank + 1, group, scope })?;
        }
        if total > 0 {
            write!(f, "{}", ReadingMore { command: invocation(scope), limit: self.limit })
        } else if self.only_current {
            write!(f, "\n{ONLY_CURRENT}")
        } else {
            write!(f, "\n{}", nothing_here(scope, "search", self.moved_root.as_deref()))
        }
    }
}

struct Group<'a> {
    rank: usize,
    group: &'a SessionHits,
    scope: &'a Scope,
}

impl Display for Group<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let SessionHits { session, passages, hits } = self.group;
        let meta = Meta::from(session);
        let mut turns: Vec<usize> = hits.iter().map(|hit| passages[hit.matched].turn).collect();
        turns.dedup();
        write!(f, "\n## {}. {}\n\n", self.rank, session.meta.title)?;
        write!(f, "`{}` · {} · turns {} · ", meta.id, meta.span(Clock::LOCAL), meta.turn_range())?;
        let matches = hits.len();
        write!(f, "{matches} {} in {}", plural(matches, "match", "matches"), plural(turns.len(), "turn", "turns"))?;
        for (index, turn) in turns.iter().enumerate() {
            write!(f, "{}{turn}", if index == 0 { " " } else { ", " })?;
        }
        writeln!(f, "{}", meta.marks(self.scope))?;
        let command = invocation(self.scope);
        for hit in hits {
            for index in hit.range.clone() {
                write!(f, "{}", Passage { group: self.group, index, hit, command })?;
            }
        }
        Ok(())
    }
}

struct Passage<'a> {
    group: &'a SessionHits,
    index: usize,
    hit: &'a Hit,
    command: &'a str,
}

impl Display for Passage<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let passage = &self.group.passages[self.index];
        let at = Clock::LOCAL.date_time(passage.ts);
        write!(f, "\n**#{} {} · {at}**", passage.turn, role_label(passage.role))?;
        if self.index == self.hit.matched {
            if self.hit.range.len() > 1 {
                f.write_str(" · match")?;
            }
            write!(f, " · `{} read {} --from {}.{}`", self.command, self.group.session.short_id(), passage.turn, passage.part)?;
            if self.hit.elsewhere > 0 {
                let elsewhere = self.hit.elsewhere;
                write!(f, " · also in {elsewhere} other {}", plural(elsewhere, "place", "places"))?;
            }
        }
        write!(f, "\n\n{}\n", Quote(&passage.text))
    }
}

struct ReadingMore<'a> {
    command: &'a str,
    limit: usize,
}

impl Display for ReadingMore<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let Self { command, limit } = self;
        write!(
            f,
            "\n---\n\n**Reading more**\n\n\
             - `-B N`, `-A N`, `-C N` on this search: the N chunks before, after or around each match\n\
             - `-n N` on this search: more matches, so more sessions (now {limit})\n\
             - `--since 2w`, `--until 2026-10-01`: only sessions active in that window\n\
             - `{command} read <id> --from TURN.PART`: read on from a match, paged; each page \
             ends with the `--from` that continues it\n\
             - `{command} read <id> --outline`: one line per turn, to pick where to read\n\
             - `--deep` on read: also tool calls, tool results and thinking\n"
        )
    }
}
