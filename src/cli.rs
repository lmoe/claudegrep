use std::path::PathBuf;

use clap::error::ErrorKind;
use clap::{Args, CommandFactory, Parser, Subcommand};

use crate::render::Position;
use crate::when::When;

const ABOUT: &str = "Search, list and read past Claude Code sessions";

const LONG_ABOUT: &str = "\
Search, list and read past Claude Code sessions, to bring context from earlier conversations \
into the current one.

SCOPE
  By default only the sessions of exactly the current directory are listed, read and searched. \
A session is in the directory it started in and in the directory its last /cd went to. \
--under DIR covers DIR and every directory below it, and --all covers every project. \
Every project is indexed either way, \
and claudegrep keeps its own compressed copy of every transcript, so sessions stay readable \
after Claude Code deletes old transcripts (cleanupPeriodDays, 30 days by default).

WORKFLOW
  claudegrep \"how did we fix the token refresh\"   find sessions and the places that match
  claudegrep read <id> --outline                  one line per turn: the session's table of contents
  claudegrep read <id> --from 12.0                read from turn 12; output is paged, the end names the next --from
  claudegrep read <id> --from 12.0 --deep         the same with tool calls, tool results and thinking

POSITIONS
  TURN.PART, e.g. 12.3. A turn is one user message, one Claude reply (everything Claude did \
until the next user message) or one subagent report. Long turns are split into parts. Search \
results print the exact position of each match, ready for `read --from`. A --deep page that stops \
inside a reply names the place as TURN:BLOCK, e.g. 12:7. A block is one message, thinking block or \
tool call of the reply.

PAGES
  read prints one page of at most --budget characters (setting: budget) and ends with the command \
that prints the next one. --budget N changes the size for one call, and --budget 0 prints \
everything at once.

TIME
  --since and --until take a date (2026-10-14) or an age (30m, 6h, 2d, 1w, 3mo) and keep the \
sessions that were active in that window. A session's time comes from its transcript entries.

NOTES
  Inside Claude Code, search skips the calling session (CLAUDE_CODE_SESSION_ID). Settings live \
in the database: claudegrep config.";

#[derive(Parser)]
#[command(name = "claudegrep", version, about = ABOUT, long_about = LONG_ABOUT)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[arg(help = "What to search for (same as `claudegrep search <query>`)")]
    query: Vec<String>,

    #[command(flatten)]
    search: SearchOptions,

    #[arg(long, global = true, help = "Use every project's sessions, not only those of the current directory")]
    all: bool,

    #[arg(long, global = true, value_name = "DIR", help = "Use the sessions of DIR and of every directory below it")]
    under: Option<PathBuf>,

    #[arg(
        long,
        global = true,
        value_name = "WHEN",
        help = "Only sessions active since WHEN: a date (2026-10-14) or an age (30m, 6h, 2d, 1w, 3mo)"
    )]
    since: Option<When>,

    #[arg(long, global = true, value_name = "WHEN", help = "Only sessions active until WHEN: a date (whole day included) or an age")]
    until: Option<When>,

    #[arg(
        long,
        global = true,
        env = "CLAUDEGREP_DB",
        value_name = "FILE",
        help = "Database with the index, the transcript archive and the settings"
    )]
    db: Option<PathBuf>,
}

#[derive(Args, Clone, Copy)]
pub struct SearchOptions {
    #[arg(
        short = 'n',
        long,
        value_parser = positive,
        help = "Number of matching chunks to show [config: limit]"
    )]
    pub limit: Option<usize>,
    #[arg(short = 'B', long, value_name = "N", help = "Also show the N chunks before each match, from the same session")]
    pub before: Option<usize>,
    #[arg(short = 'A', long, value_name = "N", help = "Also show the N chunks after each match, from the same session")]
    pub after: Option<usize>,
    #[arg(short = 'C', long, value_name = "N", help = "Also show N chunks before and after each match")]
    pub context: Option<usize>,
}

#[derive(Args)]
pub struct ReadOptions {
    #[arg(help = "Session id (a unique prefix is enough) or its number from `list`")]
    pub session: String,
    #[arg(long, conflicts_with = "deep", help = "One line per turn instead of the content: the session's table of contents")]
    pub outline: bool,
    #[arg(long, help = "Also show tool calls, tool results and thinking")]
    pub deep: bool,
    #[arg(
        long,
        value_name = "POSITION",
        conflicts_with = "turn",
        help = "Start reading here: TURN.PART from search, or the position printed at the end of a page"
    )]
    pub from: Option<Position>,
    #[arg(long, value_name = "N", help = "Show turn N with --context turns either side")]
    pub turn: Option<usize>,
    #[arg(long, value_name = "K", requires = "turn", help = "Turns either side of --turn [config: context]")]
    pub context: Option<usize>,
    #[arg(long, value_name = "CHARS", help = "Page size in characters, 0 for no limit [config: budget]")]
    pub budget: Option<usize>,
}

#[derive(Subcommand)]
pub enum Command {
    #[command(
        about = "List sessions, most recent first",
        long_about = "List sessions, most recent first, with their id, time span and number of \
                      turns. Archived sessions (deleted by Claude Code, kept by claudegrep) are marked."
    )]
    List,
    #[command(
        about = "Read a session as markdown, paged",
        long_about = "Read a session as markdown. The default shows user messages, Claude's \
                      replies, subagent reports and one line per reply summarising its tool use \
                      (files edited, commands run). --deep adds tool calls, results and thinking. \
                      --outline prints one line per turn instead. Output is paged by --budget; the \
                      last line names the --from that continues where the page stopped."
    )]
    Read(ReadOptions),
    #[command(
        about = "Search (the default when a query is given)",
        long_about = "Search messages, replies and subagent reports. Ranking combines semantic \
                      similarity with exact word matches. Results are grouped by session, best \
                      session first, and each match names its position for `read --from`."
    )]
    Search {
        #[arg(required = true, help = "What to search for")]
        query: Vec<String>,
        #[command(flatten)]
        options: SearchOptions,
    },
    #[command(about = "Index every project now (search does this on its own)")]
    Index,
    #[command(about = "Show what is indexed and archived")]
    Status,
    #[command(
        about = "Remove an archived session from claudegrep",
        long_about = "Remove a session that Claude Code has already deleted from claudegrep's \
                      archive and index. Sessions whose transcript still exists are re-indexed on \
                      the next run, so delete them in Claude Code first."
    )]
    Forget {
        #[arg(help = "Session id (a unique prefix is enough) or its number from `list`")]
        session: String,
    },
    #[command(
        about = "Show or change settings",
        long_about = "Show or change settings. Without arguments all settings are listed; with a \
                      key the value is shown; with a key and a value it is changed; --unset \
                      restores the default."
    )]
    Config(ConfigArgs),
}

#[derive(Args)]
pub struct ConfigArgs {
    #[arg(help = "Setting to show or change")]
    pub key: Option<String>,
    #[arg(help = "New value")]
    pub value: Option<String>,
    #[arg(long, requires = "key", conflicts_with = "value", help = "Reset the setting to its default")]
    pub unset: bool,
}

pub struct Global {
    pub all: bool,
    pub under: Option<PathBuf>,
    pub since: Option<When>,
    pub until: Option<When>,
    pub db: Option<PathBuf>,
}

impl Cli {
    /// Parses the arguments like `Parser::parse`. It also rejects `--all` with `--under`: clap's `conflicts_with` misses
    /// the two when one stands before the subcommand and the other after it.
    pub fn parse_arguments() -> Self {
        let cli = Self::parse();
        if cli.all && cli.under.is_some() {
            Self::command().error(ErrorKind::ArgumentConflict, "the argument '--all' cannot be used with '--under <DIR>'").exit();
        }
        cli
    }

    pub fn into_command(self) -> (Command, Global) {
        let command = match self.command {
            Some(command) => command,
            None if self.query.is_empty() => Command::List,
            None => Command::Search { query: self.query, options: self.search },
        };
        (command, Global { all: self.all, under: self.under, since: self.since, until: self.until, db: self.db })
    }
}

fn positive(value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err("expected a positive number".to_owned()),
    }
}
