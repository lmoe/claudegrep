//! Which programs a shell command runs, for the activity line and for spotting claudegrep's own output. The command is
//! read by tree-sitter's bash grammar, so quoting, substitutions, here-documents, redirections, `case` patterns,
//! function definitions and keywords are its business: each `command` node of its tree is one simple command, with the
//! name and the arguments as fields.

use std::fmt::{self, Display, Formatter};

use tree_sitter::{Node, Parser, Tree};

/// Programs that run the program named in their arguments.
const WRAPPERS: &[Wrapper] = &[
    Wrapper { name: "command", queries: "-v -V", ..Wrapper::PLAIN },
    Wrapper { name: "env", valued: "-u --unset -C --chdir -S --split-string", ..Wrapper::PLAIN },
    Wrapper { name: "exec", valued: "-a", ..Wrapper::PLAIN },
    Wrapper { name: "nice", valued: "-n --adjustment", ..Wrapper::PLAIN },
    Wrapper { name: "nohup", ..Wrapper::PLAIN },
    Wrapper {
        name: "sudo",
        valued: "-u --user -g --group -h --host -p --prompt -C --close-from -D --chdir -R --chroot -r --role -t --type -T \
                 --command-timeout -U --other-user",
        ..Wrapper::PLAIN
    },
    Wrapper { name: "time", valued: "-f --format -o --output", ..Wrapper::PLAIN },
    Wrapper { name: "timeout", valued: "-k --kill-after -s --signal", arguments: 1, ..Wrapper::PLAIN },
    Wrapper {
        name: "xargs",
        valued: "-a --arg-file -d --delimiter -E -I -L --max-lines -n --max-args -P --max-procs -s --max-chars",
        ..Wrapper::PLAIN
    },
];

struct Wrapper {
    name: &'static str,
    /// Its options that take the next word as their value, separated by spaces.
    valued: &'static str,
    /// How many arguments of its own come before the program, such as the duration of `timeout`.
    arguments: usize,
    /// Its options that make it only look the program up, separated by spaces.
    queries: &'static str,
}

impl Wrapper {
    const PLAIN: Self = Self { name: "", valued: "", arguments: 0, queries: "" };

    fn program<'a>(&self, words: &mut impl Iterator<Item = &'a str>) -> Option<&'a str> {
        let listed = |options: &str, word| options.split(' ').any(|option| option == word);
        let mut arguments = self.arguments;
        while let Some(word) = words.next() {
            match word {
                query if listed(self.queries, query) => return None,
                option if option.starts_with('-') && option.len() > 1 => {
                    if listed(self.valued, option) {
                        words.next();
                    }
                }
                assignment if assignment.contains('=') => {}
                _ if arguments > 0 => arguments -= 1,
                program => return Some(program),
            }
        }
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Program<'a> {
    pub name: &'a str,
    subcommand: Option<&'a str>,
}

impl<'a> Program<'a> {
    /// A builtin such as `cd` counts as a program, because the grammar reads it as a command like any other.
    fn of(node: Node<'_>, command: &'a str) -> Option<Self> {
        let text = |node: Node<'_>| &command[node.byte_range()];
        let mut cursor = node.walk();
        let mut words = node.children_by_field_name("argument", &mut cursor).map(text);
        let mut word = text(node.child_by_field_name("name")?);
        while let Some(wrapper) = WRAPPERS.iter().find(|wrapper| wrapper.name == word) {
            word = wrapper.program(&mut words)?;
        }
        let word = word.trim_matches(['"', '\'']);
        Some(Self { name: word.rsplit('/').next().unwrap_or(word), subcommand: words.next().filter(|word| is_subcommand(word)) })
    }
}

impl Display for Program<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.subcommand {
            Some(subcommand) => write!(f, "{} {subcommand}", self.name),
            None => f.write_str(self.name),
        }
    }
}

/// Whether `word` reads as a subcommand: a lowercase letter, then lowercase letters, digits, `-` and `:`, as in `test`,
/// `ls-files` and `lint:fix`. A path, an option, a number, a quoted string and a substitution do not.
fn is_subcommand(word: &str) -> bool {
    word.starts_with(|c: char| c.is_ascii_lowercase())
        && word.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-:".contains(c))
}

/// The programs `command` runs, in the order they appear, those inside substitutions and subshells included.
pub fn programs(command: &str) -> Vec<Program<'_>> {
    let tree = parse(command);
    nodes(&tree).filter(|node| node.kind() == "command").filter_map(|node| Program::of(node, command)).collect()
}

/// The syntax tree of `command`. The grammar reads past what it cannot make sense of, so every text has one.
fn parse(command: &str) -> Tree {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_bash::LANGUAGE.into()).expect("the grammar is built for this tree-sitter");
    parser.parse(command, None).expect("a parser with a language and no time limit returns a tree")
}

/// Every node of `tree`, each before the nodes inside it, in the order of the text.
fn nodes(tree: &Tree) -> impl Iterator<Item = Node<'_>> {
    let mut cursor = tree.walk();
    let mut more = true;
    std::iter::from_fn(move || {
        more.then(|| {
            let node = cursor.node();
            more = cursor.goto_first_child()
                || loop {
                    if cursor.goto_next_sibling() {
                        break true;
                    }
                    if !cursor.goto_parent() {
                        break false;
                    }
                };
            node
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROGRAMS: &[(&str, &[&str])] = &[
        ("cargo test", &["cargo"]),
        ("cd /x && cargo test --release 2>&1 | tail", &["cd", "cargo", "tail"]),
        ("cd x; cargo build && ./target/debug/app | tee log", &["cd", "cargo", "app", "tee"]),
        ("RUST_LOG=1 git status", &["git"]),
        ("export X=1; source env.sh; make", &["source", "make"]),
        ("/usr/bin/python3 -I script.py", &["python3"]),
        ("grep claudegrep Cargo.toml", &["grep"]),
        ("cd /home/dev/claudegrep && cargo test", &["cd", "cargo"]),
        ("cd x && ~/.local/bin/claudegrep list | head", &["cd", "claudegrep", "head"]),
        ("\"$HOME/bin/claudegrep\" list", &["claudegrep"]),
        ("claudegrep 'token refresh' -n 3", &["claudegrep"]),
        ("\\\n  cargo build \\\n  --release", &["cargo"]),
        ("a\\;b claudegrep", &["a\\;b"]),
        ("", &[]),
        ("   ", &[]),
        ("A=1 B=2", &[]),
        ("cd /x", &["cd"]),
        // Quoted text is no command, and a substitution inside it is.
        ("git commit -m \"fix; claudegrep later\"", &["git"]),
        ("echo 'a; claudegrep b' | tee log", &["echo", "tee"]),
        ("python3 -c 'import os; print(os.sep)'", &["python3"]),
        ("echo \"$(claudegrep list)\"", &["echo", "claudegrep"]),
        ("echo `claudegrep status`", &["echo", "claudegrep"]),
        ("x=\"$(git rev-parse HEAD)\" && make", &["git", "make"]),
        ("grep -E \"a|b\" file && cargo $(echo test)", &["grep", "cargo", "echo"]),
        ("(cd x && make) | tee `date`; echo $(a $(b))", &["cd", "make", "tee", "date", "echo", "a", "b"]),
        ("diff <(claudegrep list) <(ls)", &["diff", "claudegrep", "ls"]),
        ("env FOO=1 sudo nice cargo build", &["cargo"]),
        ("time command claudegrep search x", &["claudegrep"]),
        ("sudo -u postgres psql -c 'select 1'", &["psql"]),
        ("sudo -E cargo test", &["cargo"]),
        ("sudo -n -u root -- make all", &["make"]),
        ("sudo --user=x claudegrep list", &["claudegrep"]),
        ("sudo --user x claudegrep list", &["claudegrep"]),
        ("sudo -u x", &[]),
        ("sudo -u x; make", &["make"]),
        ("echo sudo -u x", &["echo"]),
        ("env -i FOO=1 cargo build", &["cargo"]),
        ("env -u HOME -C /x make", &["make"]),
        ("nice -n 19 cargo build", &["cargo"]),
        ("nice -5 make all", &["make"]),
        ("time -p make all", &["make"]),
        ("exec -a name cargo run", &["cargo"]),
        ("timeout 30 cargo test", &["cargo"]),
        ("timeout -k 5 30s cargo test", &["cargo"]),
        ("timeout --signal=KILL 30 make", &["make"]),
        ("timeout -s KILL 30 claudegrep list", &["claudegrep"]),
        ("sudo timeout 5 nice -n 1 claudegrep list", &["claudegrep"]),
        ("find . -name x | xargs -n 1 -P 4 claudegrep read", &["find", "claudegrep"]),
        ("xargs -0 -I {} git add {}", &["git"]),
        ("xargs -n1 git add", &["git"]),
        ("xargs", &[]),
        ("command -v claudegrep", &[]),
        ("command -V claudegrep", &[]),
        ("command -p claudegrep list", &["claudegrep"]),
        ("make 2>&1 | tail", &["make", "tail"]),
        ("2>/dev/null make all", &["make"]),
        ("cargo test &>> out; claudegrep list", &["cargo", "claudegrep"]),
        ("cat <<< 'claudegrep list'", &["cat"]),
        ("make |& claudegrep list", &["make", "claudegrep"]),
        ("cat<in>out", &["cat"]),
        // Keywords, tests, patterns and names are no programs.
        ("for f in a b; do claudegrep read $f; done", &["claudegrep"]),
        ("if ! grep -q x f; then make; fi", &["grep", "make"]),
        ("until make; do sleep 1; done", &["make", "sleep"]),
        ("[ -f x ] && make all", &["make"]),
        ("[[ -f x ]] || make", &["make"]),
        ("{ make; cargo test; } > log", &["make", "cargo"]),
        ("case $x in claudegrep) make ;; b|c) ls ;; esac", &["make", "ls"]),
        ("case $x in\n  a)\n    make\n    ;;\n  b) ls\nesac\ndate", &["make", "ls", "date"]),
        ("claudegrep () {\n  make all\n}", &["make"]),
        ("build() { cargo test; }; build", &["cargo", "build"]),
        ("function build { make; }", &["make"]),
        ("run() { local dir=$1; shift; cargo test \"$@\"; return 1; }", &["shift", "cargo", "return"]),
        ("args=(claudegrep list)\nmake", &["make"]),
        ("files=($(git ls-files) extra); wc -l", &["git", "wc"]),
        ("declare -a names=(a b)", &[]),
        ("# don't run this\nclaudegrep list", &["claudegrep"]),
        ("echo hi # claudegrep list", &["echo"]),
        ("echo a#b $# 'c d'", &["echo"]),
        ("echo $((a << b))\nclaudegrep list", &["echo", "claudegrep"]),
        ("((i++)); make", &["make"]),
        ("for ((i=0; i<3; i++)); do make; done", &["make"]),
        ("cat << EOF\nclaudegrep list\nEOF\nclaudegrep status", &["cat", "claudegrep"]),
        ("cat > f <<'EOF'\n(claudegrep list)\nEOF\nmake all", &["cat", "make"]),
        ("cat <<-EOF | tail\n\tclaudegrep\n\tEOF\nclaudegrep list", &["cat", "tail", "claudegrep"]),
        ("python3 - <<EOF | tail\nimport os; os.system('rm')\nEOF\nmake", &["python3", "tail", "make"]),
        ("git commit -m \"$(cat <<'EOF'\nDon't run claudegrep list; make\nEOF\n)\"", &["git", "cat"]),
        ("echo '<<EOF'\nclaudegrep list", &["echo", "claudegrep"]),
        // What the grammar cannot read.
        ("echo \"unclosed; claudegrep", &["echo"]),
        ("make all )) && cargo test", &["make", "cargo"]),
    ];

    #[test]
    fn programs_are_listed_in_the_order_they_appear() {
        for &(command, names) in PROGRAMS {
            let found: Vec<&str> = programs(command).iter().map(|program| program.name).collect();
            assert_eq!(found, names, "{command:?}");
        }
    }

    fn first(command: &str) -> String {
        programs(command)[0].to_string()
    }

    #[test]
    fn a_first_argument_that_reads_as_a_subcommand_is_shown() {
        assert_eq!(first("cargo test --release"), "cargo test");
        assert_eq!(first("timeout 30 gh pr create"), "gh pr");
        assert_eq!(first("git ls-files src"), "git ls-files");
        assert_eq!(first("pnpm lint:fix"), "pnpm lint:fix");
        assert_eq!(first("apt install -y jq"), "apt install");
        assert_eq!(first("\"$HOME/bin/claudegrep\" list"), "claudegrep list");
    }

    #[test]
    fn any_other_first_argument_is_left_out() {
        assert_eq!(first("make"), "make");
        assert_eq!(first("git -C repo status"), "git");
        assert_eq!(first("python3 script.py"), "python3");
        assert_eq!(first("ls src/render"), "ls");
        assert_eq!(first("sleep 5"), "sleep");
        assert_eq!(first("make CC=gcc all"), "make");
        assert_eq!(first("claudegrep \"token refresh\""), "claudegrep");
        assert_eq!(first("cargo $(echo test)"), "cargo");
        assert_eq!(first("grep TODO notes"), "grep");
    }
}
