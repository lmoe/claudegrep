use std::fmt;
use std::path::Path;
use std::str::FromStr;

use crate::discover::Scope;

mod activity;
mod clock;
mod config;
mod list;
mod markdown;
mod pager;
mod read;
mod search;
mod session;
mod shell;
mod status;
mod tool;
mod turn;

pub use config::config;
pub use list::{forget, list};
pub use pager::Page;
pub use read::read;
pub use search::search;
pub use session::{header, outline, page};
pub use status::{index, status};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Default,
    Deep,
}

impl Detail {
    pub const fn within(self, index: usize) -> Within {
        match self {
            Self::Default => Within::Part(index),
            Self::Deep => Within::Block(index),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub turn: usize,
    pub within: Within,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Within {
    /// A part of the turn's text, as search counts them, written `12.3`.
    Part(usize),
    /// A block of the turn in deep detail (a message, a thinking block or a tool call), written `12:7`.
    Block(usize),
}

impl Position {
    pub const fn part(turn: usize, part: usize) -> Self {
        Self { turn, within: Within::Part(part) }
    }

    pub const fn block(turn: usize, block: usize) -> Self {
        Self { turn, within: Within::Block(block) }
    }

    pub const fn start(turn: usize) -> Self {
        Self::part(turn, 0)
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.within {
            Within::Part(part) => write!(f, "{}.{part}", self.turn),
            Within::Block(block) => write!(f, "{}:{block}", self.turn),
        }
    }
}

impl FromStr for Position {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (turn, within) = match (value.split_once('.'), value.split_once(':')) {
            (Some((turn, part)), None) => (turn, part.parse().map(Within::Part)),
            (None, Some((turn, block))) => (turn, block.parse().map(Within::Block)),
            (None, None) => (value, Ok(Within::Part(0))),
            (Some(_), Some(_)) => ("", Ok(Within::Part(0))),
        };
        match (turn.parse(), within) {
            (Ok(turn), Ok(within)) => Ok(Self { turn, within }),
            _ => Err(format!("expected TURN, TURN.PART or TURN:BLOCK, got {value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnRange {
    first: usize,
    last: usize,
}

impl TurnRange {
    pub const fn all(count: usize) -> Self {
        Self::upto(count.saturating_sub(1))
    }

    pub const fn upto(last: usize) -> Self {
        Self { first: 0, last }
    }
}

impl fmt::Display for TurnRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}–{}", self.first, self.last)
    }
}

pub fn scope_name(scope: &Scope) -> String {
    scope.root().map_or_else(|| "all projects".to_owned(), |root| format!("`{}`", root.display()))
}

pub const fn invocation(scope: &Scope) -> &'static str {
    if scope.is_all() { "claudegrep --all" } else { "claudegrep" }
}

const RESET_ROOT: &str = "`claudegrep config projects-root --unset` goes back to Claude Code's own";

/// Names the projects root when the settings moved it (`moved_root`), because that explains an empty scope where
/// Claude Code does have sessions.
pub fn nothing_here(scope: &Scope, command: &str, moved_root: Option<&Path>) -> String {
    match (scope.root(), moved_root) {
        (Some(dir), None) => format!("*(no sessions in `{}`; `claudegrep --all {command}` covers every project)*\n", dir.display()),
        (Some(dir), Some(root)) => format!(
            "*(no sessions in `{}` under `{}`, the configured projects-root; `claudegrep --all {command}` covers every \
             project there, and {RESET_ROOT})*\n",
            dir.display(),
            root.display()
        ),
        (None, None) => "*(none)*\n".to_owned(),
        (None, Some(root)) => format!("*(none under `{}`, the configured projects-root; {RESET_ROOT})*\n", root.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_scope_names_a_moved_projects_root_and_how_to_undo_it() {
        let directory = Scope::Directory { root: "/p/app".into() };
        let root = Some(Path::new("/test/projects"));
        assert_eq!(
            nothing_here(&directory, "search", root),
            "*(no sessions in `/p/app` under `/test/projects`, the configured projects-root; `claudegrep --all search` \
             covers every project there, and `claudegrep config projects-root --unset` goes back to Claude Code's own)*\n"
        );
        assert_eq!(
            nothing_here(&Scope::All, "list", root),
            "*(none under `/test/projects`, the configured projects-root; `claudegrep config projects-root --unset` goes back to \
             Claude Code's own)*\n"
        );
    }

    #[test]
    fn scope_names_and_invocations() {
        let directory = Scope::Directory { root: "/p/app".into() };
        assert_eq!(scope_name(&directory), "`/p/app`");
        assert_eq!(scope_name(&Scope::All), "all projects");
        assert_eq!(invocation(&directory), "claudegrep");
        assert_eq!(invocation(&Scope::All), "claudegrep --all");
        assert_eq!(nothing_here(&directory, "list", None), "*(no sessions in `/p/app`; `claudegrep --all list` covers every project)*\n");
        assert_eq!(nothing_here(&Scope::All, "search", None), "*(none)*\n");
    }

    #[test]
    fn positions_parse_and_print() {
        assert_eq!("41.3".parse::<Position>(), Ok(Position::part(41, 3)));
        assert_eq!("7".parse::<Position>(), Ok(Position::start(7)));
        assert_eq!("41:12".parse::<Position>(), Ok(Position::block(41, 12)));
        assert_eq!(Position::part(2, 1).to_string(), "2.1");
        assert_eq!(Position::block(2, 1).to_string(), "2:1");
    }

    #[test]
    fn a_position_that_is_neither_parts_nor_blocks_is_an_error() {
        for value in ["x.1", "1.x", "1:x", ":1", "1.2:3", "1:2.3", "", "1."] {
            assert_eq!(value.parse::<Position>(), Err(format!("expected TURN, TURN.PART or TURN:BLOCK, got {value}")), "{value}");
        }
    }

    #[test]
    fn each_detail_names_places_in_its_own_unit() {
        assert_eq!(Detail::Default.within(3), Within::Part(3));
        assert_eq!(Detail::Deep.within(3), Within::Block(3));
    }

    #[test]
    fn turn_ranges_print_with_an_en_dash() {
        assert_eq!(TurnRange::all(6).to_string(), "0–5");
        assert_eq!(TurnRange::all(0).to_string(), "0–0");
        assert_eq!(TurnRange { first: 2, last: 4 }.to_string(), "2–4");
    }
}
