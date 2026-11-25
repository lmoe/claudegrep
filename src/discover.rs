use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};

pub fn default_projects_root() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
        .unwrap_or_default()
        .join("projects")
}

pub fn default_db_path() -> PathBuf {
    dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("claudegrep").join("index.sqlite")
}

fn encode(path: &Path) -> String {
    path.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// `file` the way the index names it: relative to the projects root that holds `project_dir`, with `/` between its parts,
/// as in `-home-dev-app/abc.jsonl`. The encoded project directory keeps sessions of different projects apart, and moving
/// the projects root changes nothing. A file outside the projects root keeps its path.
fn relative_to_root(project_dir: &Path, file: &Path) -> String {
    let root = project_dir.parent().unwrap_or(project_dir);
    let Ok(relative) = file.strip_prefix(root) else {
        return file.to_string_lossy().into_owned();
    };
    relative.components().map(|part| part.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

#[derive(Debug, Clone)]
pub struct DiskFile {
    pub path: PathBuf,
    /// What the index calls the file, see `relative_to_root`.
    pub key: String,
    pub project_dir: String,
    pub session_id: String,
    /// The key of the main transcript, when this is a subagent's.
    pub parent: Option<String>,
    pub meta: Option<PathBuf>,
    pub stamp: FileStamp,
}

/// A file's size and its modification time in nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    pub size: u64,
    pub mtime: i64,
}

impl FileStamp {
    /// Whether a file with this stamp replaces what the archive holds under `archived`. Transcripts only grow, so a smaller
    /// file is an older copy, and the archive may be the only place left that has the rest.
    pub fn replaces(self, archived: Self) -> bool {
        self != archived && self.size >= archived.size
    }
}

impl From<&Metadata> for FileStamp {
    fn from(metadata: &Metadata) -> Self {
        let mtime = metadata.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as i64);
        Self { size: metadata.len(), mtime }
    }
}

pub fn transcripts(project_dirs: &[PathBuf]) -> Result<Vec<DiskFile>> {
    let mut files = Vec::new();
    for project_dir in project_dirs {
        let project = relative_to_root(project_dir, project_dir);
        for (path, stamp) in jsonl_files(project_dir)? {
            let session_id = stem(&path);
            let key = relative_to_root(project_dir, &path);
            let subagents = project_dir.join(&session_id).join("subagents");
            if subagents.is_dir() {
                for (agent, stamp) in jsonl_files(&subagents)? {
                    let meta = agent.with_file_name(format!("{}.meta.json", stem(&agent)));
                    files.push(DiskFile {
                        meta: meta.is_file().then_some(meta),
                        key: relative_to_root(project_dir, &agent),
                        path: agent,
                        project_dir: project.clone(),
                        session_id: session_id.clone(),
                        parent: Some(key.clone()),
                        stamp,
                    });
                }
            }
            files.push(DiskFile { path, key, project_dir: project.clone(), session_id, parent: None, meta: None, stamp });
        }
    }
    Ok(files)
}

fn jsonl_files(dir: &Path) -> Result<Vec<(PathBuf, FileStamp)>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }
        let metadata = entry.metadata()?;
        if !metadata.is_file() {
            continue;
        }
        files.push((path, FileStamp::from(&metadata)));
    }
    Ok(files)
}

fn stem(path: &Path) -> String {
    path.file_stem().unwrap_or_default().to_string_lossy().into_owned()
}

/// The directories a session is in, out of the working directories its entries name (`cwds`, the first one first). It
/// is in the one it started in. After `/cd`, Claude Code keeps the transcript in the project directory of the new
/// directory, so the transcript has moved when the first directory does not give the name of the project directory that
/// holds it. Then the session is also in each later directory that gives that name. A `cd` in a shell command changes
/// only the working directory of later entries, so its directory does not count. The whole path is stored and compared,
/// because `encode` gives `/w/a.b`, `/w/a-b` and `/w/a/b` the same project directory.
pub fn session_dirs<'a>(project_dir: &str, cwds: &'a [String]) -> impl Iterator<Item = &'a str> {
    let moved = transcript_moved(project_dir, cwds);
    cwds.iter()
        .enumerate()
        .filter(move |(index, cwd)| *index == 0 || (moved && encode(Path::new(cwd)) == project_dir))
        .map(|(_, cwd)| cwd.as_str())
}

/// Whether a transcript in `project_dir`, whose entries name the working directories `cwds`, has moved there with
/// `/cd`: its first directory does not give the name of `project_dir`.
pub fn transcript_moved(project_dir: &str, cwds: &[String]) -> bool {
    cwds.first().is_some_and(|first| encode(Path::new(first)) != project_dir)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Scope {
    #[default]
    All,
    Directory {
        root: PathBuf,
    },
}

impl Scope {
    pub fn candidates(cwd: &Path) -> Vec<Self> {
        let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        let git_root = git_root(&cwd).filter(|root| *root != cwd);
        std::iter::once(cwd).chain(git_root).map(|root| Self::Directory { root }).collect()
    }

    pub fn root(&self) -> Option<&Path> {
        match self {
            Self::All => None,
            Self::Directory { root } => Some(root),
        }
    }

    pub const fn is_all(&self) -> bool {
        matches!(self, Self::All)
    }
}

pub fn project_dirs(projects_root: &Path) -> Result<Vec<PathBuf>> {
    if !projects_root.exists() {
        bail!(no_projects_root(projects_root, projects_root == default_projects_root()));
    }
    let mut dirs = Vec::new();
    for entry in fs::read_dir(projects_root).with_context(|| format!("reading {}", projects_root.display()))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn no_projects_root(root: &Path, is_default: bool) -> String {
    let root = root.display();
    if is_default {
        format!(
            "{root} does not exist, so there are no Claude Code sessions to read. If Claude Code keeps its projects somewhere else, \
             name the directory with `claudegrep config projects-root DIR`."
        )
    } else {
        format!(
            "the configured projects-root {root} does not exist. Name another with `claudegrep config projects-root DIR`, or go \
             back to Claude Code's own with `claudegrep config projects-root --unset`."
        )
    }
}

fn git_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().find(|ancestor| ancestor.join(".git").exists()).map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_index_is_named_after_the_program() {
        assert!(default_db_path().ends_with("claudegrep/index.sqlite"));
    }

    #[test]
    fn encodes_like_claude_code() {
        assert_eq!(encode(Path::new("/srv/Code/some-project")), "-srv-Code-some-project");
        assert_eq!(encode(Path::new("/home/dev/.config/app")), "-home-dev--config-app");
    }

    #[test]
    fn a_projects_root_that_does_not_exist_says_what_to_do() {
        let missing = Path::new("/no/such/projects");
        assert_eq!(
            project_dirs(missing).unwrap_err().to_string(),
            "the configured projects-root /no/such/projects does not exist. Name another with `claudegrep config projects-root \
             DIR`, or go back to Claude Code's own with `claudegrep config projects-root --unset`."
        );
        assert_eq!(
            no_projects_root(missing, true),
            "/no/such/projects does not exist, so there are no Claude Code sessions to read. If Claude Code keeps its projects \
             somewhere else, name the directory with `claudegrep config projects-root DIR`."
        );
    }

    #[test]
    fn a_projects_root_that_is_a_file_keeps_the_systems_error() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let error = format!("{:#}", project_dirs(file.path()).unwrap_err());
        assert!(error.starts_with(&format!("reading {}: ", file.path().display())), "{error}");
    }

    #[test]
    fn files_are_named_relative_to_the_projects_root() {
        let project = Path::new("/root/projects/-p");
        assert_eq!(relative_to_root(project, project), "-p");
        assert_eq!(relative_to_root(project, &project.join("s1.jsonl")), "-p/s1.jsonl");
        assert_eq!(relative_to_root(project, &project.join("s1/subagents/a.jsonl")), "-p/s1/subagents/a.jsonl");
    }

    #[test]
    fn a_file_outside_the_projects_root_keeps_its_path() {
        assert_eq!(relative_to_root(Path::new("/root/projects/-p"), Path::new("/elsewhere/s1.jsonl")), "/elsewhere/s1.jsonl");
    }

    #[test]
    fn transcripts_know_their_place_on_disk_and_in_the_index() {
        let dir = tempfile::TempDir::new().unwrap();
        let project = dir.path().join("-p");
        let agents = project.join("s1").join("subagents");
        fs::create_dir_all(&agents).unwrap();
        fs::write(project.join("s1.jsonl"), "main").unwrap();
        fs::write(agents.join("a.jsonl"), "agent").unwrap();
        fs::write(agents.join("a.meta.json"), "{}").unwrap();

        let files = transcripts(std::slice::from_ref(&project)).unwrap();
        let [agent, main] = files.as_slice() else { panic!("{files:?}") };
        assert_eq!((main.path.as_path(), main.key.as_str()), (project.join("s1.jsonl").as_path(), "-p/s1.jsonl"));
        assert_eq!((main.project_dir.as_str(), main.session_id.as_str(), main.parent.as_deref()), ("-p", "s1", None));
        assert_eq!((agent.path.as_path(), agent.key.as_str()), (agents.join("a.jsonl").as_path(), "-p/s1/subagents/a.jsonl"));
        assert_eq!((agent.project_dir.as_str(), agent.session_id.as_str(), agent.parent.as_deref()), ("-p", "s1", Some("-p/s1.jsonl")));
        assert_eq!(agent.meta.as_deref(), Some(agents.join("a.meta.json").as_path()));
    }

    #[test]
    fn only_jsonl_files_are_listed_with_their_stamps() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("s1.jsonl");
        fs::write(&path, "abc").unwrap();
        fs::write(dir.path().join("s1.txt"), "other").unwrap();
        fs::create_dir(dir.path().join("directory.jsonl")).unwrap();

        let files = jsonl_files(dir.path()).unwrap();
        assert_eq!(files.len(), 1);
        let (listed, stamp) = &files[0];
        assert_eq!(listed, &path);
        assert_eq!(stamp.size, 3);
        assert!(stamp.mtime > 0);
    }

    #[test]
    fn stamps_differ_when_the_size_or_the_time_does() {
        let stamp = FileStamp { size: 3, mtime: 100 };
        assert_eq!(stamp, FileStamp { size: 3, mtime: 100 });
        assert_ne!(stamp, FileStamp { size: 4, mtime: 100 });
        assert_ne!(stamp, FileStamp { size: 3, mtime: 101 });
    }

    #[test]
    fn only_a_changed_file_that_is_not_smaller_replaces_the_archived_one() {
        let archived = FileStamp { size: 10, mtime: 100 };
        assert!(!archived.replaces(archived));
        assert!(FileStamp { size: 11, mtime: 100 }.replaces(archived));
        assert!(FileStamp { size: 10, mtime: 101 }.replaces(archived));
        assert!(FileStamp { size: 10, mtime: 99 }.replaces(archived));
        assert!(!FileStamp { size: 9, mtime: 101 }.replaces(archived));
    }

    #[test]
    fn candidates_are_the_directory_then_its_git_root() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path().canonicalize().unwrap().join("repo");
        let sub = repo.join("src");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(&sub).unwrap();

        let candidates = Scope::candidates(&sub);
        let roots: Vec<_> = candidates.iter().map(|scope| scope.root().unwrap().to_path_buf()).collect();
        assert_eq!(roots, [sub, repo]);
    }

    #[test]
    fn candidates_at_the_git_root_are_just_the_directory() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path().canonicalize().unwrap().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        let candidates = Scope::candidates(&repo);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].root(), Some(repo.as_path()));
    }

    #[test]
    fn candidates_resolve_symlinks_and_dot_segments() {
        let dir = tempfile::TempDir::new().unwrap();
        let real = dir.path().canonicalize().unwrap().join("real");
        fs::create_dir_all(&real).unwrap();
        let dotted = real.join("..").join("real");
        assert_eq!(Scope::candidates(&dotted)[0].root(), Some(real.as_path()));
    }

    #[test]
    fn all_has_no_root() {
        assert!(Scope::All.is_all());
        assert_eq!(Scope::All.root(), None);
        let directory = Scope::Directory { root: "/p".into() };
        assert!(!directory.is_all());
    }

    #[test]
    fn a_session_is_in_its_first_directory_and_in_those_its_project_directory_is_named_after() {
        let cwds = ["/w/x", "/w/a-b", "/w/a-b/src", "/w/x/y", "/w/a/b"].map(str::to_owned);
        let dirs: Vec<_> = session_dirs("-w-a-b", &cwds).collect();
        assert_eq!(dirs, ["/w/x", "/w/a-b", "/w/a/b"], "an entry that names /w/a/b is the only way to tell it from /w/a-b");
        assert_eq!(session_dirs("-w-a-b", &[]).count(), 0);
    }

    #[test]
    fn a_session_that_did_not_move_is_only_in_its_first_directory() {
        let dirs = |cwds: &[&str]| {
            session_dirs("-w-a-b", &cwds.iter().map(|cwd| (*cwd).to_owned()).collect::<Vec<_>>()).map(str::to_owned).collect::<Vec<_>>()
        };
        assert_eq!(dirs(&["/w/a.b", "/w/a-b"]), ["/w/a.b"], "a shell cd to a directory with the same encoded name");
        assert_eq!(dirs(&["/w/a-b", "/w/a/b"]), ["/w/a-b"]);
    }
}
