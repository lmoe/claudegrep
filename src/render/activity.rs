use std::fmt::{self, Display, Formatter};

use serde_json::Value;

use crate::render::markdown::CodeList;
use crate::render::shell;
use crate::transcript::{AssistantTurn, Block, ToolCall};

const BUILTINS: &[&str] =
    &[".", ":", "break", "cd", "continue", "exit", "popd", "pushd", "read", "return", "set", "shift", "source", "test", "wait"];

/// Commands that only look at things.
const INSPECTION: &[&str] = &[
    "cat", "grep", "rg", "sed", "awk", "head", "tail", "ls", "find", "wc", "sort", "uniq", "echo", "printf", "cut", "tr", "diff", "stat",
    "file", "which", "pwd", "true", "sleep", "tree",
];

const PATH_KEYS: &[&str] = &["file_path", "notebook_path", "path"];

/// Displays as one line, `*🔧 edited `src/a.rs` · ran `cargo test ×2`*`.
pub struct Activity<'a> {
    cwd: Option<&'a str>,
    wrote: Vec<String>,
    edited: Vec<String>,
    read: Vec<String>,
    ran: Vec<Count>,
    used: Vec<Count>,
}

impl<'a> Activity<'a> {
    pub const fn new(cwd: Option<&'a str>) -> Self {
        Self { cwd, wrote: Vec::new(), edited: Vec::new(), read: Vec::new(), ran: Vec::new(), used: Vec::new() }
    }

    pub fn of(turn: &AssistantTurn, cwd: Option<&'a str>) -> Self {
        let mut activity = Self::new(cwd);
        for block in &turn.blocks {
            if let Block::Tool(call) = block {
                activity.record(call);
            }
        }
        activity
    }

    fn record(&mut self, call: &ToolCall) {
        let path = || PATH_KEYS.iter().find_map(|key| call.input.get(key).and_then(Value::as_str)).map(|path| relative(path, self.cwd));
        match call.name.as_str() {
            "Write" => add_unique(&mut self.wrote, path()),
            "Edit" | "MultiEdit" | "NotebookEdit" => add_unique(&mut self.edited, path()),
            "Read" => add_unique(&mut self.read, path()),
            "Bash" => {
                let command = call.input.get("command").and_then(Value::as_str).unwrap_or_default();
                let mentioned = |name: &str| !BUILTINS.contains(&name) && !INSPECTION.contains(&name);
                if let Some(program) = shell::programs(command).into_iter().find(|program| mentioned(program.name)) {
                    Count::add(&mut self.ran, program.to_string());
                }
            }
            name => Count::add(&mut self.used, tool_name(name).to_owned()),
        }
    }

    pub const fn is_empty(&self) -> bool {
        self.wrote.is_empty() && self.edited.is_empty() && self.read.is_empty() && self.ran.is_empty() && self.used.is_empty()
    }
}

impl Display for Activity<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let parts: [(&str, &dyn Display, bool); 5] = [
            ("wrote", &CodeList(&self.wrote), self.wrote.is_empty()),
            ("edited", &CodeList(&self.edited), self.edited.is_empty()),
            ("read", &CodeList(&self.read), self.read.is_empty()),
            ("ran", &CodeList(&self.ran), self.ran.is_empty()),
            ("used", &CodeList(&self.used), self.used.is_empty()),
        ];
        f.write_str("*🔧 ")?;
        for (index, (verb, items, _)) in parts.iter().filter(|(_, _, empty)| !empty).enumerate() {
            let separator = if index == 0 { "" } else { " · " };
            write!(f, "{separator}{verb} {items}")?;
        }
        f.write_str("*")
    }
}

fn add_unique(items: &mut Vec<String>, item: Option<String>) {
    if let Some(item) = item
        && !items.contains(&item)
    {
        items.push(item);
    }
}

struct Count {
    name: String,
    times: usize,
}

impl Count {
    fn add(counts: &mut Vec<Self>, name: String) {
        match counts.iter_mut().find(|count| count.name == name) {
            Some(count) => count.times += 1,
            None => counts.push(Self { name, times: 1 }),
        }
    }
}

impl Display for Count {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if self.times > 1 { write!(f, "{} ×{}", self.name, self.times) } else { f.write_str(&self.name) }
    }
}

fn relative(path: &str, cwd: Option<&str>) -> String {
    if let Some(rest) = cwd.and_then(|cwd| path.strip_prefix(cwd)).and_then(|rest| rest.strip_prefix('/')) {
        return rest.to_owned();
    }
    let mut tail: Vec<&str> = path.rsplit('/').filter(|part| !part.is_empty()).take(2).collect();
    tail.reverse();
    if tail.len() < 2 {
        return path.to_owned();
    }
    format!("…/{}", tail.join("/"))
}

fn tool_name(name: &str) -> &str {
    name.rsplit("__").next().unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::lines::START;
    use crate::test_support::timestamp;

    fn call(name: &str, input: Value) -> ToolCall {
        ToolCall { name: name.to_owned(), input, result: None }
    }

    fn bash(command: &str) -> ToolCall {
        call("Bash", json!({ "command": command }))
    }

    fn line(calls: &[ToolCall]) -> String {
        let turn = AssistantTurn { timestamp: timestamp(START), model: None, blocks: calls.iter().cloned().map(Block::Tool).collect() };
        let activity = Activity::of(&turn, Some("/p"));
        if activity.is_empty() { String::new() } else { activity.to_string() }
    }

    #[test]
    fn groups_in_a_fixed_order_whatever_the_call_order() {
        let calls = [
            call("mcp__srv__lookup", json!({})),
            bash("cargo test"),
            call("Read", json!({"file_path": "/p/c.rs"})),
            call("Edit", json!({"file_path": "/p/b.rs"})),
            call("Write", json!({"file_path": "/p/a.rs"})),
        ];
        assert_eq!(line(&calls), "*🔧 wrote `a.rs` · edited `b.rs` · read `c.rs` · ran `cargo test` · used `lookup`*");
    }

    #[test]
    fn repeated_files_are_listed_once_and_repeated_commands_counted() {
        let calls = [
            call("Edit", json!({"file_path": "/p/src/a.rs"})),
            call("MultiEdit", json!({"file_path": "/p/src/a.rs"})),
            call("NotebookEdit", json!({"notebook_path": "/p/n.ipynb"})),
            bash("cargo test"),
            bash("cargo test -q"),
            bash("git status"),
            call("mcp__srv__lookup", json!({})),
            call("mcp__other__lookup", json!({})),
        ];
        assert_eq!(line(&calls), "*🔧 edited `src/a.rs`, `n.ipynb` · ran `cargo test ×2`, `git status` · used `lookup ×2`*");
    }

    #[test]
    fn a_command_is_named_after_the_first_program_that_does_something() {
        assert_eq!(line(&[bash("cd /x && cargo test --release 2>&1 | tail")]), "*🔧 ran `cargo test`*");
        assert_eq!(line(&[bash("timeout 30 git -C repo status")]), "*🔧 ran `git`*");
        assert_eq!(line(&[bash("x=\"$(git rev-parse HEAD)\" && make")]), "*🔧 ran `git rev-parse`*");
        assert_eq!(line(&[bash("echo building; cargo build | tail -3")]), "*🔧 ran `cargo build`*");
        assert_eq!(line(&[bash("cat > msg.md <<'EOF'\nDone.\nEOF\ngh issue comment 7 -F msg.md")]), "*🔧 ran `gh issue`*");
        assert_eq!(line(&[bash("for f in a b; do [ -f $f ] || continue; grep -q x $f && rm $f; done")]), "*🔧 ran `rm`*");
    }

    #[test]
    fn builtins_and_inspection_commands_are_left_out() {
        assert_eq!(line(&[bash("cat a"), bash("grep -r x ."), bash("ls | wc -l"), bash("echo $(pwd)")]), "");
        assert_eq!(line(&[bash("cd /x"), bash("set -e; source env.sh; exit 1"), bash("A=1 B=2"), bash("# nothing"), bash("")]), "");
        assert_eq!(line(&[bash("cd x && ls"), bash("make")]), "*🔧 ran `make`*");
    }

    #[test]
    fn calls_without_a_path_or_command_add_nothing() {
        assert_eq!(line(&[call("Read", json!({})), call("Bash", json!({"description": "x"}))]), "");
    }

    #[test]
    fn long_lists_end_in_and_n_more() {
        let reads: Vec<ToolCall> = (1..=8).map(|n| call("Read", json!({"path": format!("/p/{n}")}))).collect();
        assert_eq!(line(&reads), "*🔧 read `1`, `2`, `3`, `4`, `5`, `6` and 2 more*");
    }

    #[test]
    fn mcp_tools_lose_their_server_prefix() {
        assert_eq!(tool_name("mcp__claude_ai_Gmail__search"), "search");
        assert_eq!(tool_name("WebFetch"), "WebFetch");
    }

    #[test]
    fn paths_are_relative_to_the_working_directory() {
        assert_eq!(relative("/p/src/a.rs", Some("/p")), "src/a.rs");
        assert_eq!(relative("/pp/a.rs", Some("/p")), "…/pp/a.rs");
        assert_eq!(relative("/tmp/x/scratch/sites.txt", Some("/p")), "…/scratch/sites.txt");
        assert_eq!(relative("/tmp/x/scratch/sites.txt", None), "…/scratch/sites.txt");
        assert_eq!(relative("/etc", Some("/p")), "/etc");
        assert_eq!(relative("notes.md", Some("/p")), "notes.md");
    }
}
