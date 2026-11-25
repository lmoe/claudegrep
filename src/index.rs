use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use rayon::prelude::*;

use crate::archive;
use crate::chunk::{self, Chunk};
use crate::discover::{self, DiskFile};
use crate::store::{ArchiveRow, ArchivedSession, ArchivedSessionKey, ParseState, SessionKey, Store};
use crate::transcript::{self, Session};

const PARSE_BATCH: usize = 64;

#[derive(Debug, Default)]
pub struct Refresh {
    pub stored: usize,
    pub parsed: usize,
}

pub fn refresh(store: &mut Store, project_dirs: &[PathBuf]) -> Result<Refresh> {
    let files = discover::transcripts(project_dirs)?;
    follow_moved_transcripts(store, &files)?;
    let stored = archive_changed(store, &files)?;
    let on_disk: HashSet<&str> = files.iter().filter(|file| file.parent.is_none()).map(|file| file.key.as_str()).collect();
    let sessions = store.archived_sessions()?;
    let changed = stored.iter().map(String::as_str).collect();
    let plan = plan_reparse(&sessions, &changed, &store.parse_state()?, &on_disk);
    for (path, archived) in &plan.archived_flags {
        store.set_archived(path, *archived)?;
    }
    if !plan.reparse.is_empty() {
        reparse(store, &plan.reparse, &on_disk)?;
    }
    Ok(Refresh { stored: stored.len(), parsed: plan.reparse.len() })
}

/// Moves the archive of each session whose transcript is now at another path to that path. `/cd` moves a transcript and
/// the directory of its subagents to the project directory of the new directory. Without this, the old path would stay
/// in the archive as a second session with the same id.
fn follow_moved_transcripts(store: &mut Store, files: &[DiskFile]) -> Result<()> {
    let sessions = store.archived_sessions()?;
    let mut moved = Vec::new();
    for (archived, file) in moved_transcripts(&sessions, files) {
        if starts_alike(store, &archived.path, file)? {
            moved.push((archived, file));
        }
    }
    if moved.is_empty() {
        return Ok(());
    }
    let tx = store.transaction()?;
    for (archived, file) in moved {
        tx.move_archive(&archived.path, &file.key, &file.project_dir)?;
    }
    tx.commit()
}

/// The archived sessions whose transcript is gone from its path while exactly one transcript on disk has their id, each
/// with that transcript. A transcript that Claude Code deleted has no other one with its id, so it stays archived.
fn moved_transcripts<'a>(sessions: &'a [ArchivedSessionKey], files: &'a [DiskFile]) -> Vec<(&'a ArchivedSessionKey, &'a DiskFile)> {
    let mut by_id: HashMap<&str, Vec<&DiskFile>> = HashMap::new();
    for file in files.iter().filter(|file| file.parent.is_none()) {
        by_id.entry(file.session_id.as_str()).or_default().push(file);
    }
    let on_disk: HashSet<&str> = files.iter().map(|file| file.key.as_str()).collect();
    sessions
        .iter()
        .filter(|session| !on_disk.contains(session.path.as_str()))
        .filter_map(|session| match by_id.get(session.session_id.as_str())?.as_slice() {
            [file] => Some((session, *file)),
            _ => None,
        })
        .collect()
}

/// Whether the transcript archived at `path` and `file` start with the same entry, as two copies of one transcript do.
/// A file with the same session id that starts differently is another transcript.
fn starts_alike(store: &Store, path: &str, file: &DiskFile) -> Result<bool> {
    let Some(archived) = archive::decompress(&store.load_archive(path)?.main) else {
        return Ok(false);
    };
    let Ok(on_disk) = fs::read(&file.path) else {
        return Ok(false);
    };
    let first = archive::first_kept_line(archived.as_bytes());
    Ok(first.is_some() && first == archive::first_kept_line(&on_disk))
}

/// Archives the files that are new or replace their archived version and returns, for each one stored, the path of the session it belongs to.
fn archive_changed(store: &mut Store, files: &[DiskFile]) -> Result<Vec<String>> {
    let known = store.archived_files()?;
    let changed: Vec<&DiskFile> =
        files.iter().filter(|file| known.get(&file.key).is_none_or(|&archived| file.stamp.replaces(archived))).collect();
    let rows: Vec<ArchiveRow> = changed.par_iter().filter_map(|file| archive::row(file)).collect();
    let tx = store.transaction()?;
    for row in &rows {
        tx.archive(row)?;
    }
    tx.commit()?;
    Ok(rows.into_iter().map(|row| row.parent.unwrap_or(row.path)).collect())
}

#[derive(Debug, PartialEq, Eq)]
struct ReparsePlan<'a> {
    reparse: Vec<&'a ArchivedSessionKey>,
    /// Sessions that only appeared on or disappeared from disk, with their new archived flag.
    archived_flags: Vec<(&'a str, bool)>,
}

fn plan_reparse<'a>(
    sessions: &'a [ArchivedSessionKey],
    changed: &HashSet<&str>,
    state: &HashMap<String, ParseState>,
    on_disk: &HashSet<&str>,
) -> ReparsePlan<'a> {
    let mut plan = ReparsePlan { reparse: Vec::new(), archived_flags: Vec::new() };
    for key in sessions {
        let path = key.path.as_str();
        let parsed = state.get(path);
        let archived = !on_disk.contains(path);
        if changed.contains(path) || parsed.is_none_or(|parsed| parsed.version != chunk::VERSION) {
            plan.reparse.push(key);
        } else if parsed.is_some_and(|parsed| parsed.archived != archived) {
            plan.archived_flags.push((path, archived));
        }
    }
    plan
}

fn reparse(store: &mut Store, keys: &[&ArchivedSessionKey], on_disk: &HashSet<&str>) -> Result<()> {
    for batch in keys.chunks(PARSE_BATCH) {
        let loaded = batch.iter().map(|key| store.load_archive(&key.path)).collect::<Result<Vec<_>>>()?;
        let parsed: Vec<(Option<Session>, Vec<Chunk>)> = batch
            .par_iter()
            .zip(loaded)
            .map(|(key, archived)| {
                let session = session_from(&key.session_id, &archived);
                let chunks = session.as_ref().map(chunk::chunks).unwrap_or_default();
                (session, chunks)
            })
            .collect();
        let tx = store.transaction()?;
        for (ArchivedSessionKey { path, project_dir, session_id }, (session, chunks)) in batch.iter().copied().zip(&parsed) {
            let key = SessionKey { path, project_dir, session_id, archived: !on_disk.contains(path.as_str()) };
            tx.write_session(&key, session.as_ref(), chunks)?;
        }
        tx.commit()?;
    }
    store.sync_text_index()?;
    store.gc_vectors()?;
    Ok(())
}

pub fn load_session(store: &Store, path: &str, session_id: &str) -> Result<Session> {
    let archived = store.load_archive(path)?;
    session_from(session_id, &archived).context("the session has no messages")
}

fn session_from(session_id: &str, archived: &ArchivedSession) -> Option<Session> {
    let main = archive::decompress(&archived.main)?;
    let session = transcript::parse(session_id, &main)?;
    let reports = archived
        .agents
        .iter()
        .filter_map(|agent| transcript::agent_report(&archive::decompress(&agent.data)?, agent.meta.as_deref()))
        .collect();
    Some(transcript::with_reports(session, reports))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::discover::Scope;
    use crate::store::{Filter, SessionRow};
    use crate::transcript::Turn;

    fn line(kind: &str, ts: &str, extra: &str) -> String {
        format!(r#"{{"type":"{kind}","timestamp":"{ts}","cwd":"/p",{extra}}}"#)
    }

    fn project_in(root: &Path) -> PathBuf {
        let project = root.join("-p");
        let agents = project.join("s1").join("subagents");
        fs::create_dir_all(&agents).unwrap();
        let session = [
            line("user", "2026-01-01T10:00:00Z", r#""message":{"role":"user","content":"find the loader"}"#),
            line("assistant", "2026-01-01T10:00:01Z", r#""message":{"id":"a","content":[{"type":"text","text":"Asking an agent."}]}"#),
        ];
        fs::write(project.join("s1.jsonl"), session.join("\n")).unwrap();
        let report = r#""isSidechain":true,"message":{"id":"r","content":[{"type":"text","text":"The loader lives in src/config.rs and reads TOML files."}]}"#;
        fs::write(agents.join("agent-x.jsonl"), line("assistant", "2026-01-01T10:00:30Z", report)).unwrap();
        project
    }

    fn only_session(store: &Store) -> SessionRow {
        let mut sessions = store.sessions(&Filter::default()).unwrap();
        assert_eq!(sessions.len(), 1);
        sessions.remove(0)
    }

    #[test]
    fn moving_the_projects_root_changes_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let (old, new) = (dir.path().join("old"), dir.path().join("new"));
        let project = project_in(&old);
        let mut store = Store::open(&dir.path().join("index.sqlite")).unwrap();
        let first = refresh(&mut store, std::slice::from_ref(&project)).unwrap();
        assert_eq!((first.stored, first.parsed), (2, 1));

        fs::rename(&old, &new).unwrap();
        let moved = refresh(&mut store, &[new.join("-p")]).unwrap();
        assert_eq!((moved.stored, moved.parsed), (0, 0));
        let session = only_session(&store);
        assert_eq!(session.path, "-p/s1.jsonl");
        assert!(!session.archived);
    }

    #[test]
    fn a_copy_with_new_file_times_replaces_the_session_instead_of_adding_one() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut store = Store::open(&dir.path().join("index.sqlite")).unwrap();
        refresh(&mut store, &[project_in(&dir.path().join("old"))]).unwrap();

        let copy = project_in(&dir.path().join("copy"));
        let transcript = fs::File::options().write(true).open(copy.join("s1.jsonl")).unwrap();
        transcript.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1)).unwrap();
        let copied = refresh(&mut store, &[copy]).unwrap();
        assert!(copied.stored >= 1);
        assert_eq!(copied.parsed, 1);
        assert!(!only_session(&store).archived);
    }

    #[test]
    fn sessions_survive_deletion_with_their_agent_reports() {
        let dir = tempfile::TempDir::new().unwrap();
        let project = dir.path().join("-p");
        let agents = project.join("s1").join("subagents");
        fs::create_dir_all(&agents).unwrap();
        let session = project.join("s1.jsonl");
        fs::write(
            &session,
            [
                line("user", "2026-01-01T10:00:00Z", r#""message":{"role":"user","content":"find the loader"}"#),
                line("assistant", "2026-01-01T10:00:01Z", r#""message":{"id":"a","content":[{"type":"text","text":"Asking an agent."}]}"#),
                line("user", "2026-01-01T11:00:00Z", r#""message":{"role":"user","content":"thanks"}"#),
            ]
            .join("\n"),
        )
        .unwrap();
        fs::write(
            agents.join("agent-x.jsonl"),
            line(
                "assistant",
                "2026-01-01T10:30:00Z",
                r#""isSidechain":true,"message":{"id":"r","content":[{"type":"text","text":"The loader lives in src/config.rs and reads TOML files."}]}"#,
            ),
        )
        .unwrap();
        fs::write(agents.join("agent-x.meta.json"), r#"{"description":"Find loader"}"#).unwrap();

        let mut store = Store::open(&dir.path().join("index.sqlite")).unwrap();
        let first = refresh(&mut store, std::slice::from_ref(&project)).unwrap();
        assert_eq!((first.stored, first.parsed), (2, 1));
        assert_eq!(refresh(&mut store, std::slice::from_ref(&project)).unwrap().parsed, 0);

        fs::remove_file(&session).unwrap();
        refresh(&mut store, std::slice::from_ref(&project)).unwrap();
        let sessions = store.sessions(&Filter::default()).unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].archived);

        let loaded = load_session(&store, &sessions[0].path, "s1").unwrap();
        assert!(matches!(&loaded.turns[2], Turn::Agent(agent) if agent.description == "Find loader"));
        assert_eq!(store.lexical(&Filter::default(), "\"config.rs\"", 5).unwrap().len(), 1);

        let since = Filter { since: "2026-01-02T00:00:00.000Z".parse().ok(), ..Filter::default() };
        assert!(store.sessions(&since).unwrap().is_empty());
        let until = Filter { until: "2026-01-01T10:30:00.000Z".parse().ok(), ..Filter::default() };
        assert_eq!(store.sessions(&until).unwrap().len(), 1);
    }

    struct Indexed {
        dir: tempfile::TempDir,
        project: PathBuf,
        store: Store,
    }

    impl Indexed {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let project = project_in(&dir.path().join("projects"));
            let mut store = Store::open(&dir.path().join("index.sqlite")).unwrap();
            let first = refresh(&mut store, std::slice::from_ref(&project)).unwrap();
            assert_eq!((first.stored, first.parsed), (2, 1));
            Self { dir, project, store }
        }

        fn refresh(&mut self) -> (usize, usize) {
            let refreshed = refresh(&mut self.store, std::slice::from_ref(&self.project)).unwrap();
            (refreshed.stored, refreshed.parsed)
        }

        fn main_transcript(&self) -> PathBuf {
            self.project.join("s1.jsonl")
        }

        fn agent_transcript(&self) -> PathBuf {
            self.project.join("s1").join("subagents").join("agent-x.jsonl")
        }

        fn execute(&self, sql: &str) {
            rusqlite::Connection::open(self.dir.path().join("index.sqlite")).unwrap().execute_batch(sql).unwrap();
        }
    }

    fn touch(path: &Path, seconds: u64) {
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)).unwrap();
    }

    #[test]
    fn an_unchanged_file_is_skipped() {
        let mut indexed = Indexed::new();
        assert_eq!(indexed.refresh(), (0, 0));
    }

    #[test]
    fn a_changed_mtime_archives_the_file_again() {
        let mut indexed = Indexed::new();
        touch(&indexed.main_transcript(), 1);
        assert_eq!(indexed.refresh(), (1, 1));
        assert_eq!(indexed.refresh(), (0, 0));
    }

    #[test]
    fn a_grown_file_replaces_the_session() {
        let mut indexed = Indexed::new();
        let grown = format!(
            "{}\n{}",
            fs::read_to_string(indexed.main_transcript()).unwrap(),
            line("user", "2026-01-01T10:01:00Z", r#""message":{"role":"user","content":"one more question"}"#)
        );
        fs::write(indexed.main_transcript(), grown).unwrap();
        assert_eq!(indexed.refresh(), (1, 1));
        assert_eq!(indexed.store.lexical(&Filter::default(), "\"one more question\"", 5).unwrap().len(), 1);
        assert_eq!(indexed.store.sessions(&Filter::default()).unwrap().len(), 1);
    }

    #[test]
    fn a_shorter_copy_leaves_the_archived_transcript_alone() {
        let mut indexed = Indexed::new();
        let main = indexed.main_transcript();
        let whole = fs::read_to_string(&main).unwrap();
        let older = whole.lines().next().unwrap();
        fs::write(&main, older).unwrap();
        assert_eq!(indexed.refresh(), (0, 0));
        assert_eq!(indexed.refresh(), (0, 0), "it stays ignored");
        let kept = load_session(&indexed.store, "-p/s1.jsonl", "s1").unwrap();
        assert!(kept.turns.iter().any(|turn| matches!(turn, Turn::Assistant(_))), "the longer transcript is still there");
        assert!(!only_session(&indexed.store).archived, "the file is still on disk");

        let grown =
            format!("{whole}\n{}", line("user", "2026-01-01T10:01:00Z", r#""message":{"role":"user","content":"one more question"}"#));
        fs::write(&main, grown).unwrap();
        assert_eq!(indexed.refresh(), (1, 1), "a file that has grown past the archived one replaces it");
        assert_eq!(indexed.store.lexical(&Filter::default(), "\"one more question\"", 5).unwrap().len(), 1);
    }

    #[test]
    fn a_new_subagent_file_parses_its_parent_again() {
        let mut indexed = Indexed::new();
        let report = r#""isSidechain":true,"message":{"id":"n","content":[{"type":"text","text":"A second agent found the other loader in src/app.rs today."}]}"#;
        let agent = indexed.agent_transcript().with_file_name("agent-y.jsonl");
        fs::write(agent, line("assistant", "2026-01-01T10:00:40Z", report)).unwrap();
        assert_eq!(indexed.refresh(), (1, 1));
        assert_eq!(indexed.store.lexical(&Filter::default(), "\"other loader\"", 5).unwrap().len(), 1);
        let loaded = load_session(&indexed.store, "-p/s1.jsonl", "s1").unwrap();
        assert_eq!(loaded.turns.iter().filter(|turn| matches!(turn, Turn::Agent(_))).count(), 2);
    }

    #[test]
    fn a_session_parsed_with_another_chunk_version_is_parsed_again() {
        let mut indexed = Indexed::new();
        indexed.execute("UPDATE sessions SET parsed = parsed - 1");
        assert_eq!(indexed.refresh(), (0, 1), "the archive still holds it, so nothing is stored");
        assert_eq!(indexed.store.parse_state().unwrap()["-p/s1.jsonl"].version, chunk::VERSION);
        assert_eq!(indexed.refresh(), (0, 0));
    }

    #[test]
    fn a_file_that_disappears_is_archived_and_is_not_once_it_is_back() {
        let mut indexed = Indexed::new();
        let main = indexed.main_transcript();
        let kept = fs::read(&main).unwrap();
        let modified = fs::metadata(&main).unwrap().modified().unwrap();

        fs::remove_file(&main).unwrap();
        assert_eq!(indexed.refresh(), (0, 0), "only the flag changes");
        assert!(only_session(&indexed.store).archived);
        assert_eq!(indexed.refresh(), (0, 0));

        fs::write(&main, kept).unwrap();
        fs::File::options().write(true).open(&main).unwrap().set_modified(modified).unwrap();
        assert_eq!(indexed.refresh(), (0, 0), "the same file is back, so only the flag changes");
        assert!(!only_session(&indexed.store).archived);
    }

    #[test]
    fn a_file_that_comes_back_changed_is_parsed_and_not_archived() {
        let mut indexed = Indexed::new();
        let main = indexed.main_transcript();
        let kept = fs::read(&main).unwrap();
        fs::remove_file(&main).unwrap();
        indexed.refresh();
        assert!(only_session(&indexed.store).archived);

        fs::write(&main, kept).unwrap();
        touch(&main, 1);
        assert_eq!(indexed.refresh(), (1, 1));
        assert!(!only_session(&indexed.store).archived);
    }

    #[cfg(unix)]
    fn make_unreadable(path: &Path, unreadable: bool) -> bool {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(if unreadable { 0o000 } else { 0o644 })).unwrap();
        // Root reads anything, so the caller has nothing to test.
        !unreadable || fs::read(path).is_err()
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_is_skipped_and_tried_again_on_the_next_run() {
        let mut indexed = Indexed::new();
        let main = indexed.main_transcript();
        touch(&main, 1);
        if !make_unreadable(&main, true) {
            return;
        }
        assert_eq!(indexed.refresh(), (0, 0), "the archive keeps what it had");
        assert_eq!(only_session(&indexed.store).path, "-p/s1.jsonl");

        make_unreadable(&main, false);
        assert_eq!(indexed.refresh(), (1, 1));
        assert_eq!(indexed.refresh(), (0, 0));
    }

    #[cfg(unix)]
    #[test]
    fn an_agent_of_a_session_that_cannot_be_read_yet_waits_for_it() {
        let dir = tempfile::TempDir::new().unwrap();
        let project = project_in(&dir.path().join("projects"));
        let main = project.join("s1.jsonl");
        if !make_unreadable(&main, true) {
            return;
        }
        let mut store = Store::open(&dir.path().join("index.sqlite")).unwrap();
        let first = refresh(&mut store, std::slice::from_ref(&project)).unwrap();
        assert_eq!((first.stored, first.parsed), (1, 0), "the agent is stored, but there is no session to parse yet");
        assert!(store.parse_state().unwrap().is_empty());

        make_unreadable(&main, false);
        let second = refresh(&mut store, std::slice::from_ref(&project)).unwrap();
        assert_eq!((second.stored, second.parsed), (1, 1));
        let loaded = load_session(&store, "-p/s1.jsonl", "s1").unwrap();
        assert!(loaded.turns.iter().any(|turn| matches!(turn, Turn::Agent(_))), "the agent report is attached");
    }

    fn in_directory(root: &str) -> Filter {
        Filter { scope: Scope::Directory { root: root.into() }, ..Filter::default() }
    }

    #[test]
    fn an_index_of_schema_4_shows_its_sessions_in_their_directory_after_the_upgrade() {
        let mut indexed = Indexed::new();
        indexed.execute("DELETE FROM session_dirs; UPDATE meta SET value = '4' WHERE key = 'schema';");
        indexed.store = Store::open(&indexed.dir.path().join("index.sqlite")).unwrap();
        assert_eq!(indexed.refresh(), (0, 1), "the archive still holds it, so it is only parsed again");
        assert_eq!(indexed.store.sessions(&in_directory("/p")).unwrap().len(), 1);
    }

    #[test]
    fn a_real_transcript_is_in_the_directory_it_started_in_and_not_where_its_shell_went() {
        let dir = tempfile::TempDir::new().unwrap();
        let project = dir.path().join("projects").join("-id-amet-Fugiat-adipiscing-ut");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("fixture.jsonl"), include_str!("transcript/fixtures/session.jsonl")).unwrap();
        let mut store = Store::open(&dir.path().join("index.sqlite")).unwrap();
        refresh(&mut store, std::slice::from_ref(&project)).unwrap();
        let count = |root: &str| store.count_sessions(&in_directory(root)).unwrap();
        assert_eq!(count("/id/amet/Fugiat/adipiscing-ut"), 1);
        assert_eq!(count("/id/amet/Fugiat/adipiscing-ut/aute/laborum/officia"), 0, "a shell cd went there");
    }

    #[test]
    fn a_session_whose_archive_cannot_be_read_is_parsed_without_a_title() {
        let mut indexed = Indexed::new();
        indexed.execute("UPDATE archive SET data = x'00' WHERE parent IS NULL; UPDATE sessions SET parsed = parsed - 1;");
        assert_eq!(indexed.refresh(), (0, 1));
        assert!(indexed.store.sessions(&Filter::default()).unwrap().is_empty());
        assert_eq!(indexed.store.parse_state().unwrap()["-p/s1.jsonl"].version, chunk::VERSION, "it is not retried on every run");
        assert_eq!(indexed.refresh(), (0, 0));
        assert_eq!(load_session(&indexed.store, "-p/s1.jsonl", "s1").unwrap_err().to_string(), "the session has no messages");
    }

    /// Moves `s1` of `project` with its subagents to the project directory `-q` next to it, as `/cd` does.
    fn move_to_q(project: &Path) -> PathBuf {
        let moved = project.with_file_name("-q");
        fs::create_dir_all(&moved).unwrap();
        fs::rename(project.join("s1.jsonl"), moved.join("s1.jsonl")).unwrap();
        fs::rename(project.join("s1"), moved.join("s1")).unwrap();
        moved
    }

    fn archived_paths(store: &Store) -> Vec<String> {
        let mut paths: Vec<String> = store.archived_files().unwrap().into_keys().collect();
        paths.sort();
        paths
    }

    #[test]
    fn a_moved_transcript_takes_its_archive_and_its_agents_along() {
        let mut indexed = Indexed::new();
        let moved = move_to_q(&indexed.project);
        let both = [indexed.project.clone(), moved];
        let refreshed = refresh(&mut indexed.store, &both).unwrap();
        assert_eq!((refreshed.stored, refreshed.parsed), (0, 1), "a move keeps the file times, so only the parse is new");
        assert_eq!(archived_paths(&indexed.store), ["-q/s1.jsonl", "-q/s1/subagents/agent-x.jsonl"]);
        let session = only_session(&indexed.store);
        assert_eq!((session.path.as_str(), session.archived), ("-q/s1.jsonl", false));
        let loaded = load_session(&indexed.store, "-q/s1.jsonl", "s1").unwrap();
        assert!(loaded.turns.iter().any(|turn| matches!(turn, Turn::Agent(_))), "the agent report is attached");
        assert_eq!(indexed.store.lexical(&Filter::default(), "\"config.rs\"", 5).unwrap().len(), 1);
        let again = refresh(&mut indexed.store, &both).unwrap();
        assert_eq!((again.stored, again.parsed), (0, 0));
    }

    #[test]
    fn an_index_that_holds_both_paths_of_a_moved_transcript_keeps_the_larger_copy() {
        let mut indexed = Indexed::new();
        let copy = indexed.project.with_file_name("-q");
        fs::create_dir_all(&copy).unwrap();
        let grown = format!(
            "{}\n{}",
            fs::read_to_string(indexed.main_transcript()).unwrap(),
            line("user", "2026-01-01T10:01:00Z", r#""message":{"role":"user","content":"one more question"}"#)
        );
        fs::write(copy.join("s1.jsonl"), grown).unwrap();
        let both = [indexed.project.clone(), copy];
        refresh(&mut indexed.store, &both).unwrap();
        assert_eq!(indexed.store.sessions(&Filter::default()).unwrap().len(), 2, "a copy is no move, both files are on disk");

        fs::remove_file(indexed.main_transcript()).unwrap();
        refresh(&mut indexed.store, &both).unwrap();
        let session = only_session(&indexed.store);
        assert_eq!((session.path.as_str(), session.archived), ("-q/s1.jsonl", false));
        assert_eq!(indexed.store.lexical(&Filter::default(), "\"one more question\"", 5).unwrap().len(), 1);
        assert_eq!(archived_paths(&indexed.store), ["-q/s1.jsonl", "-q/s1/subagents/agent-x.jsonl"], "the agent went along");
    }

    #[test]
    fn of_two_archived_paths_of_one_session_the_larger_copy_stays_when_the_file_on_disk_is_shorter() {
        let mut indexed = Indexed::new();
        let copy = indexed.project.with_file_name("-q");
        fs::create_dir_all(&copy).unwrap();
        let line = line("user", "2026-01-01T10:01:00Z", r#""message":{"role":"user","content":"one more question"}"#);
        let grown = format!("{}\n{line}", fs::read_to_string(indexed.main_transcript()).unwrap());
        fs::write(copy.join("s1.jsonl"), &grown).unwrap();
        let both = [indexed.project.clone(), copy.clone()];
        refresh(&mut indexed.store, &both).unwrap();

        fs::write(copy.join("s1.jsonl"), grown.lines().next().unwrap()).unwrap();
        fs::remove_file(indexed.main_transcript()).unwrap();
        refresh(&mut indexed.store, &both).unwrap();
        assert_eq!(only_session(&indexed.store).path, "-q/s1.jsonl");
        assert_eq!(indexed.store.lexical(&Filter::default(), "\"one more question\"", 5).unwrap().len(), 1);
    }

    #[test]
    fn a_smaller_archived_copy_at_the_new_path_gives_way_to_the_moved_one() {
        let mut indexed = Indexed::new();
        let copy = indexed.project.with_file_name("-q");
        fs::create_dir_all(&copy).unwrap();
        let whole = fs::read_to_string(indexed.main_transcript()).unwrap();
        fs::write(copy.join("s1.jsonl"), whole.lines().next().unwrap()).unwrap();
        let both = [indexed.project.clone(), copy];
        refresh(&mut indexed.store, &both).unwrap();

        fs::remove_file(indexed.main_transcript()).unwrap();
        refresh(&mut indexed.store, &both).unwrap();
        assert_eq!(only_session(&indexed.store).path, "-q/s1.jsonl");
        let kept = load_session(&indexed.store, "-q/s1.jsonl", "s1").unwrap();
        assert!(kept.turns.iter().any(|turn| matches!(turn, Turn::Assistant(_))), "the larger archived copy stays");
    }

    #[test]
    fn a_moved_transcript_that_is_shorter_than_its_archived_copy_keeps_the_copy() {
        let mut indexed = Indexed::new();
        let moved = move_to_q(&indexed.project);
        let whole = fs::read_to_string(moved.join("s1.jsonl")).unwrap();
        fs::write(moved.join("s1.jsonl"), whole.lines().next().unwrap()).unwrap();
        refresh(&mut indexed.store, &[indexed.project.clone(), moved]).unwrap();
        let session = only_session(&indexed.store);
        assert_eq!(session.path, "-q/s1.jsonl");
        let kept = load_session(&indexed.store, "-q/s1.jsonl", "s1").unwrap();
        assert!(kept.turns.iter().any(|turn| matches!(turn, Turn::Assistant(_))), "the longer transcript is still there");
    }

    #[test]
    fn a_transcript_with_the_same_id_that_starts_differently_is_no_move() {
        let mut indexed = Indexed::new();
        let other = indexed.project.with_file_name("-q");
        fs::create_dir_all(&other).unwrap();
        let start = line("user", "2026-02-01T09:00:00Z", r#""message":{"role":"user","content":"another start"}"#);
        fs::write(other.join("s1.jsonl"), start).unwrap();
        fs::remove_file(indexed.main_transcript()).unwrap();
        refresh(&mut indexed.store, &[indexed.project.clone(), other]).unwrap();
        let mut paths: Vec<_> = indexed.store.sessions(&Filter::default()).unwrap().into_iter().map(|session| session.path).collect();
        paths.sort();
        assert_eq!(paths, ["-p/s1.jsonl", "-q/s1.jsonl"]);
    }

    fn disk_file(key: &str, session_id: &str) -> DiskFile {
        DiskFile {
            path: PathBuf::from(key),
            key: key.to_owned(),
            project_dir: key.split('/').next().unwrap().to_owned(),
            session_id: session_id.to_owned(),
            parent: None,
            meta: None,
            stamp: discover::FileStamp { size: 1, mtime: 1 },
        }
    }

    fn moves(archived: &[&str], on_disk: &[&str]) -> Vec<(String, String)> {
        let sessions: Vec<_> = archived.iter().map(|path| key(path)).collect();
        let files: Vec<_> = on_disk.iter().map(|path| disk_file(path, "s")).collect();
        moved_transcripts(&sessions, &files).into_iter().map(|(session, file)| (session.path.clone(), file.key.clone())).collect()
    }

    #[test]
    fn a_session_moved_when_its_path_is_gone_and_one_other_file_has_its_id() {
        assert_eq!(moves(&["-a/s.jsonl"], &["-b/s.jsonl"]), [("-a/s.jsonl".to_owned(), "-b/s.jsonl".to_owned())]);
        assert_eq!(moves(&["-a/s.jsonl", "-b/s.jsonl"], &["-b/s.jsonl"]), [("-a/s.jsonl".to_owned(), "-b/s.jsonl".to_owned())]);
        assert_eq!(moves(&["-a/s.jsonl"], &["-a/s.jsonl", "-b/s.jsonl"]), [], "both files are on disk");
        assert_eq!(moves(&["-a/s.jsonl"], &[]), [], "Claude Code deleted it");
        assert_eq!(moves(&["-a/s.jsonl"], &["-b/s.jsonl", "-c/s.jsonl"]), [], "two files have its id");
        let other = disk_file("-b/t.jsonl", "t");
        assert!(moved_transcripts(&[key("-a/s.jsonl")], std::slice::from_ref(&other)).is_empty(), "another id");
    }

    fn key(path: &str) -> ArchivedSessionKey {
        ArchivedSessionKey { path: path.to_owned(), project_dir: "-p".to_owned(), session_id: "s".to_owned() }
    }

    fn plan(state: Option<ParseState>, changed: bool, on_disk: bool) -> (usize, Vec<bool>) {
        let sessions = [key("s")];
        let state: HashMap<String, ParseState> = state.into_iter().map(|state| ("s".to_owned(), state)).collect();
        let changed: HashSet<&str> = changed.then_some("s").into_iter().collect();
        let on_disk: HashSet<&str> = on_disk.then_some("s").into_iter().collect();
        let plan = plan_reparse(&sessions, &changed, &state, &on_disk);
        (plan.reparse.len(), plan.archived_flags.iter().map(|(_, archived)| *archived).collect())
    }

    const fn parsed(version: u32, archived: bool) -> ParseState {
        ParseState { version, archived }
    }

    #[test]
    fn the_plan_leaves_a_session_that_is_up_to_date_alone() {
        assert_eq!(plan(Some(parsed(chunk::VERSION, false)), false, true), (0, vec![]));
        assert_eq!(plan(Some(parsed(chunk::VERSION, true)), false, false), (0, vec![]));
    }

    #[test]
    fn the_plan_parses_what_changed_was_never_parsed_or_has_an_old_version() {
        assert_eq!(plan(Some(parsed(chunk::VERSION, false)), true, true), (1, vec![]));
        assert_eq!(plan(None, false, true), (1, vec![]));
        assert_eq!(plan(Some(parsed(chunk::VERSION - 1, false)), false, true), (1, vec![]));
    }

    #[test]
    fn the_plan_only_flips_the_flag_of_a_session_it_does_not_parse() {
        assert_eq!(plan(Some(parsed(chunk::VERSION, false)), false, false), (0, vec![true]));
        assert_eq!(plan(Some(parsed(chunk::VERSION, true)), false, true), (0, vec![false]));
        assert_eq!(plan(Some(parsed(chunk::VERSION, true)), true, true), (1, vec![]), "parsing writes the flag anyway");
        assert_eq!(plan(Some(parsed(chunk::VERSION - 1, false)), false, false), (1, vec![]));
    }

    #[test]
    fn the_plan_keeps_the_order_of_the_sessions() {
        let sessions = [key("b"), key("a"), key("c")];
        let changed = ["c", "b"].into_iter().collect();
        let plan = plan_reparse(&sessions, &changed, &HashMap::new(), &HashSet::new());
        assert_eq!(plan.reparse, [&sessions[0], &sessions[1], &sessions[2]]);
    }
}
