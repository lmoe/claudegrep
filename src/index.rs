use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rayon::prelude::*;

use crate::archive;
use crate::chunk::{self, Chunk};
use crate::discover::{self, DiskFile, FileStamp};
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
    let stamps = store.archived_files()?;
    let mut moves = Vec::new();
    let moved_copy = |session: &ArchivedSessionKey| archived_copy_moved(store, session);
    for planned in moved_transcripts(&sessions, files, &stamps, moved_copy) {
        if starts_alike(store, &planned)? {
            moves.push(planned);
        }
    }
    if moves.is_empty() {
        return Ok(());
    }
    let tx = store.transaction()?;
    for Move { from, to, project_dir, .. } in moves {
        tx.move_archive(from, to, project_dir)?;
    }
    tx.commit()
}

/// An archived session that moves to `to`, the path of the same transcript, in `project_dir`.
#[derive(Debug, PartialEq, Eq)]
struct Move<'a> {
    from: &'a str,
    to: &'a str,
    project_dir: &'a str,
    /// The transcript at `to`, when it is on disk.
    file: Option<&'a Path>,
}

/// The archived sessions whose transcript is gone from its path, each with the path it moves to. When exactly one
/// transcript on disk has the session's id, it moves there. When none has, as after Claude Code deleted a transcript that
/// an older index holds at two paths, the largest archived copy stays and the others move to it. Of copies of one size,
/// a copy that `/cd` moved (see `moved_copy`) stays, so the session stays in the directory `/cd` went to. A transcript
/// that Claude Code deleted and that has no other copy stays archived where it is.
fn moved_transcripts<'a>(
    sessions: &'a [ArchivedSessionKey],
    files: &'a [DiskFile],
    stamps: &HashMap<String, FileStamp>,
    moved_copy: impl Fn(&ArchivedSessionKey) -> bool,
) -> Vec<Move<'a>> {
    let mut on_disk_by_id: HashMap<&str, Vec<&DiskFile>> = HashMap::new();
    for file in files.iter().filter(|file| file.parent.is_none()) {
        on_disk_by_id.entry(file.session_id.as_str()).or_default().push(file);
    }
    let on_disk: HashSet<&str> = files.iter().map(|file| file.key.as_str()).collect();
    let mut gone_by_id: BTreeMap<&str, Vec<&ArchivedSessionKey>> = BTreeMap::new();
    for session in sessions.iter().filter(|session| !on_disk.contains(session.path.as_str())) {
        gone_by_id.entry(session.session_id.as_str()).or_default().push(session);
    }
    let mut moves = Vec::new();
    for (id, gone) in gone_by_id {
        match on_disk_by_id.get(id).map(Vec::as_slice) {
            Some([file]) => moves.extend(gone.iter().map(|session| Move {
                from: &session.path,
                to: &file.key,
                project_dir: &file.project_dir,
                file: Some(&file.path),
            })),
            None => {
                let size = |session: &ArchivedSessionKey| stamps.get(&session.path).map_or(0, |stamp| stamp.size);
                let Some(kept) = gone.iter().max_by_key(|session| (size(session), moved_copy(session), &session.path)) else {
                    continue;
                };
                moves.extend(gone.iter().filter(|session| session.path != kept.path).map(|session| Move {
                    from: &session.path,
                    to: &kept.path,
                    project_dir: &kept.project_dir,
                    file: None,
                }));
            }
            Some(_) => {}
        }
    }
    moves
}

/// Whether the transcript archived at `session.path` has moved to its project directory with `/cd`.
fn archived_copy_moved(store: &Store, session: &ArchivedSessionKey) -> bool {
    let Ok(archived) = store.load_archive(&session.path) else {
        return false;
    };
    let parsed = archive::decompress(&archived.main).and_then(|text| transcript::parse(&session.session_id, &text));
    parsed.is_some_and(|parsed| discover::transcript_moved(&session.project_dir, &parsed.cwds))
}

/// Whether the transcript archived at `planned.from` starts with the same entry as the one at `planned.to`, as two
/// copies of one transcript do. A transcript with the same session id that starts differently is another one.
fn starts_alike(store: &Store, planned: &Move<'_>) -> Result<bool> {
    let archived_first_line = |path: &str| -> Result<Option<Vec<u8>>> {
        Ok(archive::decompress(&store.load_archive(path)?.main).and_then(|text| archive::first_kept_line(text.as_bytes())))
    };
    let from = archived_first_line(planned.from)?;
    let to = match planned.file {
        Some(path) => fs::read(path).ok().and_then(|bytes| archive::first_kept_line(&bytes)),
        None => archived_first_line(planned.to)?,
    };
    Ok(from.is_some() && from == to)
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
    use crate::store::{Filter, SessionRow, StoredVector};
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
    fn an_index_of_schema_4_keeps_its_vectors_and_its_embedder_on_the_upgrade() {
        let mut indexed = Indexed::new();
        indexed.store.use_embedder("model").unwrap();
        let hashes: Vec<_> = indexed.store.pending().unwrap().into_iter().map(|chunk| chunk.hash).collect();
        let vectors: Vec<_> = hashes.iter().map(|&hash| StoredVector { hash, tokens: 1, data: &[1] }).collect();
        indexed.store.insert_vectors(&vectors).unwrap();
        indexed.execute("DROP TABLE session_dirs; UPDATE meta SET value = '4' WHERE key = 'schema';");

        indexed.store = Store::open(&indexed.dir.path().join("index.sqlite")).unwrap();
        assert_eq!(indexed.refresh(), (0, 1));
        assert_eq!(indexed.store.embedder().unwrap().as_deref(), Some("model"));
        assert_eq!(indexed.store.pending().unwrap(), [], "nothing is embedded again");
        assert_eq!(indexed.store.stats(&Filter::default()).unwrap().embedded, hashes.len());
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
    fn an_old_index_with_two_archived_paths_of_a_deleted_session_keeps_the_larger_one() {
        let mut indexed = Indexed::new();
        let copy = indexed.project.with_file_name("-q");
        fs::create_dir_all(&copy).unwrap();
        let grown = format!(
            "{}\n{}",
            fs::read_to_string(indexed.main_transcript()).unwrap(),
            line("user", "2026-01-01T10:01:00Z", r#""message":{"role":"user","content":"one more question"}"#)
        );
        fs::write(copy.join("s1.jsonl"), grown).unwrap();
        let both = [indexed.project.clone(), copy.clone()];
        refresh(&mut indexed.store, &both).unwrap();
        assert_eq!(indexed.store.sessions(&Filter::default()).unwrap().len(), 2);

        // An index of schema 4 holds both paths, and then Claude Code deleted both transcripts.
        fs::remove_file(indexed.main_transcript()).unwrap();
        fs::remove_dir_all(indexed.project.join("s1")).unwrap();
        fs::remove_file(copy.join("s1.jsonl")).unwrap();
        indexed.execute("DROP TABLE session_dirs; UPDATE meta SET value = '4' WHERE key = 'schema';");
        indexed.store = Store::open(&indexed.dir.path().join("index.sqlite")).unwrap();
        refresh(&mut indexed.store, &both).unwrap();
        let session = only_session(&indexed.store);
        assert_eq!((session.path.as_str(), session.archived), ("-q/s1.jsonl", true));
        assert_eq!(indexed.store.lexical(&Filter::default(), "\"one more question\"", 5).unwrap().len(), 1);
        assert_eq!(archived_paths(&indexed.store), ["-q/s1.jsonl", "-q/s1/subagents/agent-x.jsonl"]);
    }

    /// A transcript that started in `/z` and moved with `/cd` to `/m`, which sorts before `/z`.
    fn started_in_z_and_moved_to_m() -> String {
        let entry =
            |kind: &str, ts: &str, cwd: &str, extra: &str| format!(r#"{{"type":"{kind}","timestamp":"{ts}","cwd":"{cwd}",{extra}}}"#);
        [
            entry("user", "2026-01-01T10:00:00Z", "/z", r#""message":{"role":"user","content":"start in z"}"#),
            entry("assistant", "2026-01-01T10:00:01Z", "/z", r#""message":{"id":"a","content":[{"type":"text","text":"Started."}]}"#),
            entry("user", "2026-01-01T10:01:00Z", "/m", r#""message":{"role":"user","content":"go on in m"}"#),
        ]
        .join("\n")
    }

    #[test]
    fn of_two_archived_copies_of_one_size_the_moved_one_stays() {
        let dir = tempfile::TempDir::new().unwrap();
        let (start, moved) = (dir.path().join("projects").join("-z"), dir.path().join("projects").join("-m"));
        let transcript = started_in_z_and_moved_to_m();
        for project in [&start, &moved] {
            fs::create_dir_all(project).unwrap();
            fs::write(project.join("s1.jsonl"), &transcript).unwrap();
        }
        let mut store = Store::open(&dir.path().join("index.sqlite")).unwrap();
        let both = [start.clone(), moved.clone()];
        refresh(&mut store, &both).unwrap();
        assert_eq!(store.sessions(&Filter::default()).unwrap().len(), 2, "both copies are on disk");

        fs::remove_file(start.join("s1.jsonl")).unwrap();
        fs::remove_file(moved.join("s1.jsonl")).unwrap();
        refresh(&mut store, &both).unwrap();
        assert_eq!(only_session(&store).path, "-m/s1.jsonl");
        assert_eq!(store.sessions(&in_directory("/m")).unwrap().len(), 1, "the session is where /cd took it");
        assert_eq!(store.sessions(&in_directory("/z")).unwrap().len(), 1, "and where it started");
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

    fn moves_by_size(archived: &[(&str, u64)], on_disk: &[&str]) -> Vec<(String, String)> {
        let sessions: Vec<_> = archived.iter().map(|(path, _)| key(path)).collect();
        let stamps = archived.iter().map(|(path, size)| ((*path).to_owned(), discover::FileStamp { size: *size, mtime: 1 })).collect();
        let files: Vec<_> = on_disk.iter().map(|path| disk_file(path, "s")).collect();
        moved_transcripts(&sessions, &files, &stamps, |_| false)
            .into_iter()
            .map(|planned| (planned.from.to_owned(), planned.to.to_owned()))
            .collect()
    }

    fn moves(archived: &[&str], on_disk: &[&str]) -> Vec<(String, String)> {
        moves_by_size(&archived.iter().map(|path| (*path, 1)).collect::<Vec<_>>(), on_disk)
    }

    #[test]
    fn a_session_moved_when_its_path_is_gone_and_one_other_file_has_its_id() {
        assert_eq!(moves(&["-a/s.jsonl"], &["-b/s.jsonl"]), [("-a/s.jsonl".to_owned(), "-b/s.jsonl".to_owned())]);
        assert_eq!(moves(&["-a/s.jsonl", "-b/s.jsonl"], &["-b/s.jsonl"]), [("-a/s.jsonl".to_owned(), "-b/s.jsonl".to_owned())]);
        assert_eq!(moves(&["-a/s.jsonl"], &["-a/s.jsonl", "-b/s.jsonl"]), [], "both files are on disk");
        assert_eq!(moves(&["-a/s.jsonl"], &[]), [], "Claude Code deleted it");
        assert_eq!(moves(&["-a/s.jsonl"], &["-b/s.jsonl", "-c/s.jsonl"]), [], "two files have its id");
        let other = disk_file("-b/t.jsonl", "t");
        assert!(moved_transcripts(&[key("-a/s.jsonl")], std::slice::from_ref(&other), &HashMap::new(), |_| false).is_empty(), "another id");
    }

    #[test]
    fn archived_copies_of_a_session_that_is_on_disk_nowhere_move_to_the_largest() {
        let to_b = |from: &str| (from.to_owned(), "-b/s.jsonl".to_owned());
        assert_eq!(moves_by_size(&[("-a/s.jsonl", 5), ("-b/s.jsonl", 9)], &[]), [to_b("-a/s.jsonl")]);
        assert_eq!(
            moves_by_size(&[("-b/s.jsonl", 9), ("-a/s.jsonl", 5), ("-c/s.jsonl", 7)], &[]),
            [to_b("-a/s.jsonl"), to_b("-c/s.jsonl")]
        );
        assert_eq!(moves_by_size(&[("-a/s.jsonl", 9), ("-b/s.jsonl", 9)], &[]), [to_b("-a/s.jsonl")], "the same size picks one path");
        let sessions = [key("-a/s.jsonl"), key("-b/s.jsonl")];
        let stamps = sessions.iter().map(|session| (session.path.clone(), discover::FileStamp { size: 9, mtime: 1 })).collect();
        let moved_a = |session: &ArchivedSessionKey| session.path == "-a/s.jsonl";
        let moves: Vec<_> =
            moved_transcripts(&sessions, &[], &stamps, moved_a).into_iter().map(|planned| (planned.from, planned.to)).collect();
        assert_eq!(moves, [("-b/s.jsonl", "-a/s.jsonl")], "of one size, the copy that moved stays");
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
