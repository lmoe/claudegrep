use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use super::lines::Transcript;

pub const ROOT: &str = "[ROOT]";

/// The binary runs isolated from the machine it runs on: no `CLAUDE_CODE_SESSION_ID` (these tests usually run inside
/// Claude Code), `TZ=UTC` because output uses local time, and `HOME`/XDG pointing into the temp dir.
pub struct Fixture {
    root: PathBuf,
    projects: PathBuf,
    _dir: TempDir,
}

impl Fixture {
    pub fn new() -> Self {
        let dir = TempDir::new().expect("a temp dir");
        let root = dir.path().canonicalize().expect("the temp dir canonicalizes");
        let projects = root.join("projects");
        fs::create_dir_all(&projects).expect("the projects root is created");
        let fixture = Self { root, projects, _dir: dir };
        fixture.run_in(&fixture.root, &["config", "projects-root", fixture.projects.to_str().expect("a UTF-8 path")]).success();
        fixture.run_in(&fixture.root, &["config", "threads", "1"]).success();
        fixture.run_in(&fixture.root, &["config", "device", "cpu"]).success();
        fixture
    }

    pub fn dir(&self, name: &str) -> PathBuf {
        let dir = self.root.join("work").join(name);
        fs::create_dir_all(&dir).expect("the work dir is created");
        dir
    }

    pub fn project_dir(&self, cwd: &Path) -> PathBuf {
        self.projects.join(encode(cwd))
    }

    pub fn session(&self, cwd: &Path, id: &str, transcript: &Transcript) {
        let dir = self.project_dir(cwd);
        fs::create_dir_all(&dir).expect("the project dir is created");
        fs::write(dir.join(format!("{id}.jsonl")), transcript.build()).expect("the transcript is written");
    }

    pub fn agent(&self, cwd: &Path, id: &str, agent: &str, transcript: &Transcript, meta: Option<&str>) {
        let dir = self.project_dir(cwd).join(id).join("subagents");
        fs::create_dir_all(&dir).expect("the subagents dir is created");
        fs::write(dir.join(format!("agent-{agent}.jsonl")), transcript.build()).expect("the agent transcript is written");
        if let Some(meta) = meta {
            fs::write(dir.join(format!("agent-{agent}.meta.json")), meta).expect("the agent meta is written");
        }
    }

    pub fn remove_projects_root(&self) {
        fs::remove_dir_all(&self.projects).expect("the projects root is removed");
    }

    pub fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    pub fn delete_session(&self, cwd: &Path, id: &str) {
        fs::remove_file(self.project_dir(cwd).join(format!("{id}.jsonl"))).expect("the transcript is deleted");
    }

    /// Moves a session's transcript and its directory of subagents to the project directory of `to`, as `/cd` does.
    pub fn move_session(&self, from: &Path, to: &Path, id: &str) {
        let (old, new) = (self.project_dir(from), self.project_dir(to));
        fs::create_dir_all(&new).expect("the project dir is created");
        let file = format!("{id}.jsonl");
        fs::rename(old.join(&file), new.join(&file)).expect("the transcript is moved");
        if old.join(id).is_dir() {
            fs::rename(old.join(id), new.join(id)).expect("the session's directory is moved");
        }
    }

    pub fn append(&self, cwd: &Path, id: &str, transcript: &Transcript) {
        let path = self.project_dir(cwd).join(format!("{id}.jsonl"));
        let mut content = fs::read_to_string(&path).expect("the transcript is read");
        content.push_str(&transcript.build());
        fs::write(path, content).expect("the transcript is written");
    }

    pub fn run_in(&self, cwd: &Path, args: &[&str]) -> Run {
        self.run_with_env(cwd, args, &[])
    }

    pub fn run_with_env(&self, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Run {
        let home = self.home();
        let mut command = self.command(cwd, args);
        command.env("HOME", &home).env("XDG_CACHE_HOME", home.join(".cache"));
        self.finish(command, env)
    }

    /// The model and ONNX Runtime come from the real caches (`~/.cache/huggingface`, `~/.cache/claudegrep`), which a
    /// first search downloads, and claudegrep finds them through `HOME` and `XDG_CACHE_HOME`, so those keep their real
    /// values here. The database and the projects root stay inside the fixture.
    pub fn run_with_model(&self, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Run {
        let command = self.command(cwd, args);
        self.finish(command, env)
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_claudegrep"));
        command
            .args(args)
            .current_dir(cwd)
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("ORT_DYLIB_PATH")
            .env("TZ", "UTC")
            .env("XDG_DATA_HOME", self.root.join("home/.local/share"))
            .env("CLAUDEGREP_DB", self.root.join("db.sqlite"));
        command
    }

    fn finish(&self, mut command: Command, env: &[(&str, &str)]) -> Run {
        for (key, value) in env {
            command.env(key, value);
        }
        let output = command.output().expect("claudegrep runs");
        let redact = |bytes: &[u8]| String::from_utf8_lossy(bytes).replace(self.root.to_str().expect("a UTF-8 path"), ROOT);
        Run { stdout: redact(&output.stdout), stderr: redact(&output.stderr), code: output.status.code() }
    }
}

pub struct Run {
    pub stdout: String,
    pub stderr: String,
    pub code: Option<i32>,
}

impl Run {
    #[track_caller]
    pub fn success(self) -> String {
        assert_eq!(self.code, Some(0), "claudegrep failed:\n{self}");
        self.stdout
    }

    #[track_caller]
    pub fn usage_error(self) -> String {
        assert_eq!(self.code, Some(2), "claudegrep did not reject its arguments:\n{self}");
        assert!(self.stdout.is_empty(), "a rejected run printed to stdout:\n{self}");
        self.stderr
    }

    #[track_caller]
    pub fn failure(self) -> String {
        assert_eq!(self.code, Some(1), "claudegrep did not fail:\n{self}");
        assert!(self.stdout.is_empty(), "a failed run printed to stdout:\n{self}");
        self.stderr
    }
}

impl fmt::Display for Run {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "exit code: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}", self.code, self.stdout, self.stderr)
    }
}

fn encode(path: &Path) -> String {
    path.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}
