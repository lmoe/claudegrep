use std::fmt::{self, Display, Formatter};

use crate::render::scope_name;
use crate::store::Stats;
use crate::text::plural;
use crate::view::{IndexView, StatusView};

pub const HEADING: &str = "# claudegrep status";

const MIB: f64 = 1_048_576.0;

pub fn status(view: &StatusView) -> String {
    view.to_string()
}

pub fn index(view: &IndexView) -> String {
    view.to_string()
}

impl Display for StatusView {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        writeln!(f, "{HEADING}")?;
        write!(f, "{}", Section { title: &scope_name(&self.scope), stats: &self.scoped })?;
        write!(f, "{}", Section { title: "the whole index", stats: &self.whole })?;
        writeln!(f, "\n## Setup\n")?;
        writeln!(f, "- model: `{}`", self.model)?;
        writeln!(f, "- vectors from: `{}`", self.embedder.as_deref().unwrap_or("(nothing embedded yet)"))?;
        writeln!(f, "- projects: `{}`", self.projects_root.display())?;
        writeln!(f, "- database: `{}`", self.db_path.display())
    }
}

struct Section<'a> {
    title: &'a str,
    stats: &'a Stats,
}

impl Display for Section<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let Self { title, stats } = self;
        writeln!(f, "\n## {title}\n")?;
        writeln!(f, "- sessions: {}", stats.sessions)?;
        writeln!(f, "- chunks: {} ({} distinct)", stats.chunks, stats.unique_chunks)?;
        let mib = stats.vector_bytes as f64 / MIB;
        writeln!(f, "- embedded: {} of {} ({mib:.1} MiB of vectors)", stats.embedded, stats.unique_chunks)
    }
}

impl Display for IndexView {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let Self { stored, parsed, embedded, status } = self;
        write!(
            f,
            "Archived {stored} new or changed transcript {}, parsed {parsed} {}, embedded {embedded} new {}.\n\n{status}",
            plural(*stored, "file", "files"),
            plural(*parsed, "session", "sessions"),
            plural(*embedded, "chunk", "chunks")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::Scope;

    fn view(scope: Scope, embedder: Option<&str>) -> StatusView {
        StatusView {
            scope,
            scoped: Stats { sessions: 2, chunks: 40, unique_chunks: 38, embedded: 30, vector_bytes: 3 * 1_048_576 / 2 },
            whole: Stats { sessions: 9, chunks: 400, unique_chunks: 350, embedded: 350, vector_bytes: 0 },
            model: "lightonai/answerai-colbert-small-v1".to_owned(),
            embedder: embedder.map(str::to_owned),
            projects_root: "/home/u/.claude/projects".into(),
            db_path: "/home/u/.local/share/claudegrep/index.sqlite".into(),
        }
    }

    #[test]
    fn status_reports_the_scope_the_whole_index_and_the_setup() {
        let directory = Scope::Directory { root: "/p/app".into(), project_dir: "-p-app".into() };
        insta::assert_snapshot!(status(&view(directory, Some("colbert:answerai@1"))), @r"
        # claudegrep status

        ## `/p/app`

        - sessions: 2
        - chunks: 40 (38 distinct)
        - embedded: 30 of 38 (1.5 MiB of vectors)

        ## the whole index

        - sessions: 9
        - chunks: 400 (350 distinct)
        - embedded: 350 of 350 (0.0 MiB of vectors)

        ## Setup

        - model: `lightonai/answerai-colbert-small-v1`
        - vectors from: `colbert:answerai@1`
        - projects: `/home/u/.claude/projects`
        - database: `/home/u/.local/share/claudegrep/index.sqlite`
        ");
    }

    #[test]
    fn status_says_when_nothing_is_embedded_yet() {
        let report = status(&view(Scope::All, None));
        assert!(report.contains("\n## all projects\n"), "{report}");
        assert!(report.contains("- vectors from: `(nothing embedded yet)`\n"), "{report}");
    }

    #[test]
    fn index_reports_its_work_before_the_status() {
        let view = IndexView { stored: 3, parsed: 2, embedded: 17, status: view(Scope::All, None) };
        let report = index(&view);
        let (first, rest) = report.split_once("\n\n").unwrap();
        assert_eq!(first, "Archived 3 new or changed transcript files, parsed 2 sessions, embedded 17 new chunks.");
        assert_eq!(rest, status(&view.status));
    }

    #[test]
    fn index_counts_one_of_each_in_the_singular() {
        let view = IndexView { stored: 1, parsed: 1, embedded: 1, status: view(Scope::All, None) };
        let report = index(&view);
        assert_eq!(
            report.split_once("\n\n").unwrap().0,
            "Archived 1 new or changed transcript file, parsed 1 session, embedded 1 new chunk."
        );
    }
}
