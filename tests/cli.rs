//! Search and index use the real embedding model from the local caches (see `Fixture::run_with_model`), and the first
//! `claudegrep search` on a machine downloads it. Their snapshots assume the same model and ONNX Runtime version.

mod support;

use std::fs;
use std::path::PathBuf;

use insta::assert_snapshot;
use serde_json::json;

use support::fixture::Fixture;
use support::lines::{Transcript, agent_meta, document, image, text, thinking, tool_error, tool_result, tool_use};

const TOKEN: &str = "aaaa1111-0000-4000-8000-000000000001";
const NOTES: &str = "aaaa2222-0000-4000-8000-000000000002";
const CI: &str = "bbbb3333-0000-4000-8000-000000000003";
const MOVED: &str = "cccc4444-0000-4000-8000-000000000004";

struct Project {
    fixture: Fixture,
    app: PathBuf,
    other: PathBuf,
}

impl Project {
    fn new() -> Self {
        let fixture = Fixture::new();
        let app = fixture.dir("app");
        let other = fixture.dir("other");
        fs::create_dir_all(app.join(".git")).unwrap();
        fs::create_dir_all(app.join("src")).unwrap();

        let cwd = app.to_str().unwrap();
        let auth = app.join("src/auth.rs");
        let auth = auth.to_str().unwrap();
        fixture.session(
            &app,
            TOKEN,
            &Transcript::new(cwd)
                .user("How did we fix the token refresh?\nIt fails after an hour.")
                .assistant(
                    "m1",
                    vec![
                        thinking("The refresh token probably expires."),
                        text("Let me look at the auth module."),
                        tool_use("t1", "Read", json!({ "file_path": auth })),
                        tool_use("t2", "Bash", json!({ "command": format!("cd {cwd} && cargo test auth 2>&1 | tail") })),
                    ],
                )
                .user_blocks(vec![tool_result("t1", "fn refresh() {}"), tool_error("t2", "test auth::refresh ... FAILED")])
                .assistant(
                    "m2",
                    vec![
                        tool_use("t3", "Edit", json!({ "file_path": auth, "old_string": "a", "new_string": "b" })),
                        tool_use("t4", "mcp__docs__lookup", json!({ "query": "oauth refresh" })),
                        tool_use("t5", "Bash", json!({ "command": "claudegrep \"token refresh\"" })),
                        text(&explanation()),
                    ],
                )
                .user_blocks(vec![
                    tool_result("t3", "The file has been updated."),
                    tool_result("t4", "Refresh tokens rotate on every use."),
                    tool_result("t5", "# Search: token refresh\n\n*1 match in 1 session, all projects*"),
                ])
                .user("<command-name>/model</command-name>\n<command-args>opus</command-args>")
                .user_blocks(vec![text("Here is the screenshot of the error."), image(), document("spec.pdf")])
                .assistant("m3", vec![text("The screenshot shows the expired token error, which the fix covers.")])
                .ai_title("Fix token refresh"),
        );
        fixture.agent(
            &app,
            TOKEN,
            "explore",
            &Transcript::sidechain(cwd).at("2026-01-01T10:03:30.000Z").user("Find where tokens are refreshed\nLook in src.").assistant(
                "r1",
                vec![text("Tokens are refreshed in src/auth.rs by refresh(), which the HTTP middleware calls on every 401.")],
            ),
            Some(&agent_meta("Find token refresh", Some("Explore"))),
        );
        fixture.session(
            &app,
            NOTES,
            &Transcript::new(cwd)
                .at("2026-01-03T09:00:00.000Z")
                .user("Write the release notes for 0.2")
                .assistant(
                    "n1",
                    vec![text("Here are the notes for 0.2:\n\n- transcripts are archived\n- --since and --until filter by time")],
                )
                .ai_title("Release notes draft")
                .custom_title("Release notes 0.2"),
        );
        fixture.session(
            &other,
            CI,
            &Transcript::new(other.to_str().unwrap())
                .at("2026-01-02T15:00:00.000Z")
                .user("Set up CI for this project")
                .assistant("c1", vec![text("Added a GitHub Actions workflow that runs cargo test on every push.")])
                .ai_title("Set up CI"),
        );
        Self { fixture, app, other }
    }

    fn run(&self, args: &[&str]) -> String {
        self.fixture.run_in(&self.app, args).success()
    }

    fn fail(&self, args: &[&str]) -> String {
        self.fixture.run_in(&self.app, args).failure()
    }

    fn reject(&self, args: &[&str]) -> String {
        self.fixture.run_in(&self.app, args).usage_error()
    }

    fn embed(&self, args: &[&str]) -> String {
        self.fixture.run_with_model(&self.app, args, &[]).success()
    }
}

fn explanation() -> String {
    let cause = "The refresh token expired because the clock skew check compared local time with UTC. ".repeat(7);
    let fix = "The check now converts both sides to UTC before comparing and allows thirty seconds of skew. ".repeat(6);
    format!("## Cause\n\n{}\n\n## Fix\n\n{}\n\n## Follow-up\n\n- add a regression test\n- log the measured skew", cause.trim(), fix.trim())
}

fn continue_from(page: &str) -> Option<String> {
    let start = page.find("Continue: `")?;
    let hint = &page[start..];
    let from = hint.find("--from ")? + "--from ".len();
    Some(hint[from..].split(['`', ' ']).next()?.to_owned())
}

#[test]
fn list_shows_sessions_started_here_most_recent_first() {
    assert_snapshot!(Project::new().run(&["list"]));
}

#[test]
fn list_is_the_default_command() {
    let project = Project::new();
    assert_eq!(project.run(&[]), project.run(&["list"]));
}

#[test]
fn list_all_shows_every_project_with_its_directory() {
    assert_snapshot!(Project::new().run(&["--all", "list"]));
}

#[test]
fn list_marks_the_current_session() {
    let project = Project::new();
    let run = project.fixture.run_with_env(&project.app, &["list"], &[("CLAUDE_CODE_SESSION_ID", TOKEN)]);
    assert_snapshot!(run.success());
}

#[test]
fn list_from_a_subdirectory_falls_back_to_the_git_root() {
    let project = Project::new();
    let run = project.fixture.run_in(&project.app.join("src"), &["list"]);
    assert_eq!(run.success(), project.run(&["list"]));
}

#[test]
fn a_window_does_not_change_which_directory_is_in_scope() {
    let project = Project::new();
    let nothing_in_the_window = ["list", "--since", "2099-01-01"];
    let run = project.fixture.run_in(&project.app.join("src"), &nothing_in_the_window);
    assert_eq!(run.success(), project.run(&nothing_in_the_window));
}

#[test]
fn list_in_a_directory_without_sessions_points_to_all() {
    let project = Project::new();
    let empty = project.fixture.dir("empty");
    assert_snapshot!(project.fixture.run_in(&empty, &["list"]).success());
}

#[test]
fn since_keeps_sessions_active_after_the_date() {
    assert_snapshot!(Project::new().run(&["list", "--since", "2026-01-02"]));
}

#[test]
fn until_keeps_sessions_active_before_the_end_of_the_date() {
    assert_snapshot!(Project::new().run(&["list", "--until", "2026-01-01"]));
}

#[test]
fn since_and_until_together_select_a_window() {
    assert_snapshot!(Project::new().run(&["--all", "list", "--since", "2026-01-02", "--until", "2026-01-02"]));
}

#[test]
fn invalid_since_is_rejected_by_argument_parsing() {
    assert_snapshot!(Project::new().reject(&["list", "--since", "yesterday"]));
}

#[test]
fn invalid_until_is_rejected_by_argument_parsing() {
    assert_snapshot!(Project::new().reject(&["read", "aaaa1111", "--until", "2026-02-30"]));
}

#[test]
fn read_shows_messages_replies_reports_and_tool_summaries() {
    assert_snapshot!(Project::new().run(&["read", "aaaa1111"]));
}

#[test]
fn read_accepts_the_number_from_list() {
    let project = Project::new();
    assert_eq!(project.run(&["read", "2"]), project.run(&["read", "aaaa1111"]));
}

#[test]
fn read_outline_lists_one_line_per_turn() {
    assert_snapshot!(Project::new().run(&["read", "aaaa1111", "--outline"]));
}

#[test]
fn read_outline_is_paged() {
    assert_snapshot!(Project::new().run(&["read", "aaaa1111", "--outline", "--budget", "300"]));
}

#[test]
fn read_deep_adds_thinking_tool_calls_and_results() {
    assert_snapshot!(Project::new().run(&["read", "aaaa1111", "--deep"]));
}

#[test]
fn read_from_starts_mid_turn() {
    assert_snapshot!(Project::new().run(&["read", "aaaa1111", "--from", "1.1"]));
}

#[test]
fn read_turn_shows_context_either_side() {
    assert_snapshot!(Project::new().run(&["read", "aaaa1111", "--turn", "3", "--context", "1"]));
}

#[test]
fn read_pages_follow_each_other_to_the_end() {
    let project = Project::new();
    let first = project.run(&["read", "aaaa1111", "--budget", "600"]);
    assert_snapshot!(first);

    let mut pages = vec![first];
    while let Some(from) = continue_from(pages.last().unwrap()) {
        assert!(pages.len() < 20, "paging does not end");
        pages.push(project.run(&["read", "aaaa1111", "--budget", "600", "--from", &from]));
    }
    assert!(pages.len() > 2, "the budget splits the session into several pages");
    let all = pages.join("\n");
    for turn in 0..=5 {
        assert!(all.contains(&format!("### #{turn} ")), "turn {turn} is on some page");
    }
}

#[test]
fn read_deep_pages_stop_between_tool_calls_and_follow_each_other_to_the_end() {
    let project = Project::new();
    let whole = project.run(&["read", "aaaa1111", "--deep", "--budget", "0"]);
    let first = project.run(&["read", "aaaa1111", "--deep", "--budget", "600"]);
    assert_snapshot!(first);

    let mut pages = vec![first];
    while let Some(from) = continue_from(pages.last().unwrap()) {
        assert!(pages.len() < 40, "paging does not end");
        assert!(from.contains(':'), "a deep page continues at a block: {from}");
        pages.push(project.run(&["read", "aaaa1111", "--deep", "--budget", "600", "--from", &from]));
    }
    assert!(pages.len() > 4, "the budget splits the session into several pages");
    let all = pages.join("\n");
    for line in whole.lines().filter(|line| !line.starts_with("*Turns ")) {
        assert!(all.contains(line), "a page shows: {line}");
    }
    for marker in ["**🔧 Read**", "**🔧 Edit**", "💭 Thinking"] {
        assert_eq!(all.matches(marker).count(), whole.matches(marker).count(), "{marker} is on exactly one page");
    }
}

#[test]
fn read_deep_opens_a_search_position_at_its_message() {
    let page = Project::new().run(&["read", "aaaa1111", "--deep", "--from", "1.1"]);
    assert!(page.contains("(continued from block "), "{page}");
    assert!(page.contains("\n## Fix\n"), "{page}");
    assert!(!page.contains("Let me look at the auth module."), "{page}");
}

#[test]
fn read_from_a_block_needs_deep() {
    assert_snapshot!(Project::new().fail(&["read", "aaaa1111", "--from", "1:2"]));
}

#[test]
fn read_unknown_session_is_an_error() {
    assert_snapshot!(Project::new().fail(&["read", "zzzz"]));
}

#[test]
fn read_unknown_session_with_all_is_an_error() {
    assert_snapshot!(Project::new().fail(&["--all", "read", "zzzz"]));
}

#[test]
fn read_ambiguous_prefix_is_an_error() {
    assert_snapshot!(Project::new().fail(&["read", "aaaa"]));
}

#[test]
fn read_session_of_another_project_needs_all() {
    let project = Project::new();
    assert_snapshot!(project.fail(&["read", "bbbb3333"]));
    assert!(project.run(&["--all", "read", "bbbb3333"]).starts_with("# Set up CI\n"));
}

#[test]
fn read_turn_out_of_range_is_an_error() {
    assert_snapshot!(Project::new().fail(&["read", "aaaa1111", "--turn", "99"]));
}

#[test]
fn status_reports_index_and_setup() {
    assert_snapshot!(Project::new().run(&["status"]));
}

#[test]
fn config_lists_all_settings() {
    assert_snapshot!(Project::new().run(&["config"]));
}

#[test]
fn config_sets_shows_and_unsets_a_value() {
    let project = Project::new();
    assert_eq!(project.run(&["config", "limit"]), "limit = 8\n");
    assert_eq!(project.run(&["config", "limit", "5"]), "limit = 5\n");
    assert_eq!(project.run(&["config", "limit"]), "limit = 5\n");
    assert!(project.run(&["config"]).contains("- `limit` = `5` — "));
    assert_eq!(project.run(&["config", "limit", "--unset"]), "limit = 8\n");
}

#[test]
fn config_model_change_warns_about_reembedding() {
    assert_snapshot!(Project::new().run(&["config", "model", "some/model"]));
}

#[test]
fn config_rejects_invalid_values() {
    assert_snapshot!(Project::new().fail(&["config", "limit", "0"]));
}

#[test]
fn config_rejects_unknown_keys() {
    assert_snapshot!(Project::new().fail(&["config", "colour"]));
}

#[test]
fn a_configured_projects_root_that_is_gone_names_the_ways_out() {
    let fixture = Fixture::new();
    fixture.remove_projects_root();
    assert_snapshot!(fixture.run_in(&fixture.dir("app"), &["list"]).failure());
}

#[test]
fn without_claude_codes_own_projects_directory_a_search_fails_before_it_downloads_anything() {
    let fixture = Fixture::new();
    let cwd = fixture.dir("app");
    fixture.run_in(&cwd, &["config", "projects-root", "--unset"]).success();
    assert_snapshot!(fixture.run_in(&cwd, &["search", "token refresh"]).failure());
    assert!(!fixture.home().join(".cache").exists(), "the search downloaded something");
}

#[test]
fn deleted_sessions_stay_readable_until_forgotten() {
    let project = Project::new();
    project.run(&["list"]);
    project.fixture.delete_session(&project.app, TOKEN);

    assert_snapshot!("archived_list", project.run(&["list"]));
    let read = project.run(&["read", "aaaa1111"]);
    assert_snapshot!("archived_header", read.lines().take(3).collect::<Vec<_>>().join("\n"));
    assert!(read.contains("### #2 🛰 Agent (Explore)"), "the subagent report survives deletion");

    assert_snapshot!("forgotten", project.run(&["forget", "aaaa1111"]));
    assert!(!project.run(&["list"]).contains("Fix token refresh"));
}

#[test]
fn forget_refuses_sessions_claude_code_still_has() {
    assert_snapshot!(Project::new().fail(&["forget", "aaaa2222"]));
}

#[test]
fn other_project_is_untouched_by_scope() {
    let project = Project::new();
    let other = project.fixture.run_in(&project.other, &["list"]).success();
    assert!(other.contains("Set up CI") && !other.contains("Release notes"));
}

#[test]
fn a_session_moved_with_cd_is_listed_where_it_started_and_where_it_moved_to() {
    let fixture = Fixture::new();
    let (start, moved) = (fixture.dir("start"), fixture.dir("moved"));
    // After `/cd`, Claude Code keeps the transcript in the project directory of the new directory, but the first entries
    // still name the old one.
    let transcript = Transcript::new(start.to_str().unwrap())
        .user("Plan the parser rewrite")
        .assistant("m1", vec![text("First split the lexer from the parser.")])
        .cwd(moved.to_str().unwrap())
        .user("Now write the lexer")
        .assistant("m2", vec![text("The lexer is in src/lexer.rs now.")])
        .ai_title("Parser rewrite");
    fixture.session(&moved, MOVED, &transcript);

    let unlisted: Vec<_> =
        [&start, &moved].into_iter().filter(|dir| !fixture.run_in(dir, &["list"]).success().contains("Parser rewrite")).collect();
    assert!(unlisted.is_empty(), "the session is not listed in {unlisted:?}");
}

#[test]
fn a_transcript_moved_after_it_was_indexed_is_one_session() {
    let fixture = Fixture::new();
    let (start, moved) = (fixture.dir("start"), fixture.dir("moved"));
    let (start_cwd, moved_cwd) = (start.to_str().unwrap(), moved.to_str().unwrap());
    let transcript = Transcript::new(start_cwd)
        .user("Plan the parser rewrite")
        .assistant("m1", vec![text("First split the lexer from the parser.")])
        .ai_title("Parser rewrite");
    fixture.session(&start, MOVED, &transcript);
    let report = text("The lexer lives in src/lexer.rs and reads one token at a time from the input.");
    let agent = Transcript::sidechain(start_cwd).at("2026-01-01T10:00:30.000Z").user("Find the lexer").assistant("r1", vec![report]);
    fixture.agent(&start, MOVED, "explore", &agent, Some(&agent_meta("Find the lexer", None)));
    assert_eq!(fixture.run_in(&start, &["--all", "list"]).success().matches("cccc4444").count(), 1);

    fixture.move_session(&start, &moved, MOVED);
    let later = Transcript::new(moved_cwd).at("2026-01-01T11:00:00.000Z").user("Now write the lexer");
    fixture.append(&moved, MOVED, &later);

    let all = fixture.run_in(&start, &["--all", "list"]).success();
    assert_eq!(all.matches("cccc4444").count(), 1, "{all}");
    assert!(!all.contains("*archived*"), "the transcript is still on disk:\n{all}");
    for dir in [&start, &moved] {
        let read = fixture.run_in(dir, &["read", "cccc4444"]).success();
        assert!(read.contains("Now write the lexer") && read.contains("src/lexer.rs"), "the moved transcript with its agent:\n{read}");
    }
}

#[test]
fn a_shell_cd_to_a_directory_with_the_same_encoded_name_adds_no_directory() {
    let fixture = Fixture::new();
    // `a.b`, `a-b` and `a/b` share one project directory.
    for (start, shell, id, title) in [("a.b", "a-b", TOKEN, "Dot session"), ("a-b", "a/b", NOTES, "Dash session")] {
        let (start, shell) = (fixture.dir(start), fixture.dir(shell));
        let transcript = Transcript::new(start.to_str().unwrap())
            .user("Look around")
            .cwd(shell.to_str().unwrap())
            .assistant("m1", vec![text("Looked around.")])
            .ai_title(title);
        fixture.session(&start, id, &transcript);
    }
    let listed = |dir: &str| fixture.run_in(&fixture.dir(dir), &["list"]).success();
    let (dot, dash, slash) = (listed("a.b"), listed("a-b"), listed("a/b"));
    assert!(dot.contains("Dot session") && !dot.contains("Dash session"), "{dot}");
    assert!(dash.contains("Dash session") && !dash.contains("Dot session"), "{dash}");
    assert!(!slash.contains("Dot session") && !slash.contains("Dash session"), "{slash}");
}

#[test]
fn a_shell_cd_into_a_subdirectory_keeps_the_fallback_to_the_git_root() {
    let fixture = Fixture::new();
    let repo = fixture.dir("repo");
    let src = repo.join("src");
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::create_dir_all(&src).unwrap();
    let cwd = repo.to_str().unwrap();
    let drifted = Transcript::new(cwd)
        .user("Run the tests in src")
        .assistant("d1", vec![text("Running them from src.")])
        .cwd(src.to_str().unwrap())
        .assistant("d2", vec![text("They pass.")])
        .ai_title("Tests from src");
    fixture.session(&repo, TOKEN, &drifted);
    let other = Transcript::new(cwd).user("Write the changelog").assistant("c1", vec![text("Done.")]).ai_title("Changelog");
    fixture.session(&repo, NOTES, &other);

    let from_src = fixture.run_in(&src, &["list"]).success();
    assert_eq!(from_src, fixture.run_in(&repo, &["list"]).success());
    assert!(from_src.contains("Tests from src") && from_src.contains("Changelog"), "{from_src}");
}

#[test]
fn help_texts_are_stable() {
    let project = Project::new();
    for (name, args) in [
        ("help_long", &["--help"][..]),
        ("help_short", &["-h"][..]),
        ("help_list", &["list", "--help"][..]),
        ("help_read", &["read", "--help"][..]),
        ("help_search", &["search", "--help"][..]),
        ("help_index", &["index", "--help"][..]),
        ("help_status", &["status", "--help"][..]),
        ("help_forget", &["forget", "--help"][..]),
        ("help_config", &["config", "--help"][..]),
    ] {
        assert_snapshot!(name, project.run(args));
    }
}

#[test]
fn conflicting_read_options_are_rejected() {
    let project = Project::new();
    assert_snapshot!("reject_outline_deep", project.reject(&["read", "aaaa1111", "--outline", "--deep"]));
    assert_snapshot!("reject_from_turn", project.reject(&["read", "aaaa1111", "--from", "1", "--turn", "1"]));
    assert_snapshot!("reject_context_without_turn", project.reject(&["read", "aaaa1111", "--context", "1"]));
    assert_snapshot!("reject_unset_with_value", project.reject(&["config", "limit", "5", "--unset"]));
    assert_snapshot!("reject_zero_limit", project.reject(&["search", "-n", "0", "x"]));
}

#[test]
fn search_lists_matches_by_session_with_read_positions() {
    assert_snapshot!(Project::new().embed(&["search", "why did the token refresh fail"]));
}

#[test]
fn a_bare_query_is_a_search() {
    let project = Project::new();
    assert_eq!(project.embed(&["token", "refresh"]), project.embed(&["search", "token refresh"]));
}

#[test]
fn search_context_shows_neighbouring_chunks() {
    assert_snapshot!(Project::new().embed(&["search", "-n", "2", "-C", "1", "clock skew between local time and UTC"]));
}

#[test]
fn search_before_and_after_are_independent() {
    assert_snapshot!(Project::new().embed(&["search", "-n", "1", "-B", "1", "-A", "0", "regression test for the skew"]));
}

#[test]
fn search_all_covers_other_projects() {
    assert_snapshot!(Project::new().embed(&["--all", "search", "-n", "3", "continuous integration workflow"]));
}

#[test]
fn search_respects_the_time_window() {
    assert_snapshot!(Project::new().embed(&["--all", "search", "-n", "3", "--since", "2026-01-02", "token refresh"]));
}

#[test]
fn search_skips_the_current_session() {
    let project = Project::new();
    let run = project.fixture.run_with_model(&project.app, &["search", "-n", "3", "token refresh"], &[("CLAUDE_CODE_SESSION_ID", TOKEN)]);
    let out = run.success();
    assert!(!out.contains("aaaa1111"), "the current session is skipped:\n{out}");
    assert_snapshot!(out);
}

#[test]
fn search_explains_when_the_only_session_here_is_the_current_one() {
    let project = Project::new();
    let run = project.fixture.run_with_model(&project.other, &["search", "workflow"], &[("CLAUDE_CODE_SESSION_ID", CI)]);
    assert_snapshot!(run.success());
}

#[test]
fn search_in_a_directory_without_sessions_points_to_all() {
    let project = Project::new();
    let empty = project.fixture.dir("empty");
    assert_snapshot!(project.fixture.run_with_model(&empty, &["search", "anything"], &[]).success());
}

#[test]
fn index_embeds_every_chunk_once() {
    let project = Project::new();
    assert_snapshot!(project.embed(&["index"]));
    let again = project.embed(&["index"]);
    assert!(again.starts_with("Archived 0 new or changed transcript files, parsed 0 sessions, embedded 0 new chunks."), "{again}");
}
