//! `--under`, `--since` and `--until` through the binary. The tests with a model use the real embedding model from the
//! local caches, see `Fixture::run_with_model`.

mod support;

use std::path::PathBuf;

use support::fixture::Fixture;

const APP_OLD: &str = "aaaa0001-0000-4000-8000-000000000001";
const SUB_NEW: &str = "aaaa0002-0000-4000-8000-000000000002";
const APPLE_OLD: &str = "bbbb0003-0000-4000-8000-000000000003";
const APPLE_NEW: &str = "bbbb0004-0000-4000-8000-000000000004";

/// Sessions in `app`, in `app/sub` and twice in `apple`, whose name starts with `app`, all below the returned directory.
fn app_sub_and_apple() -> (Fixture, PathBuf) {
    let fixture = Fixture::new();
    let sessions = [
        ("app", APP_OLD, "App old", "2026-01-01T10:00:00.000Z"),
        ("app/sub", SUB_NEW, "Sub new", "2026-01-05T10:00:00.000Z"),
        ("apple", APPLE_OLD, "Apple old", "2026-01-01T12:00:00.000Z"),
        ("apple", APPLE_NEW, "Apple new", "2026-01-05T12:00:00.000Z"),
    ];
    for (name, id, title, start) in sessions {
        fixture.titled_session(name, id, title, start);
    }
    let work = fixture.dir("app").parent().unwrap().to_path_buf();
    (fixture, work)
}

/// The titles of a list, in its order.
fn titles(list: &str) -> Vec<&str> {
    list.lines().filter_map(|line| line.split_once(". **")?.1.split_once("**")).map(|(title, _)| title).collect()
}

fn section<'a>(status: &'a str, heading: &str) -> &'a str {
    let (_, rest) = status.split_once(&format!("## {heading}\n")).unwrap_or_else(|| panic!("no section {heading}:\n{status}"));
    rest.split("\n## ").next().unwrap()
}

#[test]
fn under_and_since_keep_only_the_sessions_below_the_directory_that_are_in_the_window() {
    let (fixture, work) = app_sub_and_apple();
    let list = fixture.run_in(&work, &["--under", "app", "--since", "2026-01-03", "list"]).success();
    assert_eq!(titles(&list), ["Sub new"], "{list}");
}

#[test]
fn under_and_until_keep_only_the_sessions_below_the_directory_that_started_before_the_end() {
    let (fixture, work) = app_sub_and_apple();
    let list = fixture.run_in(&work, &["--under", "app", "list", "--until", "2026-01-03"]).success();
    assert_eq!(titles(&list), ["App old"], "{list}");
}

#[test]
fn status_under_a_directory_counts_its_sessions_and_names_it_and_below() {
    let (fixture, work) = app_sub_and_apple();
    let status = fixture.run_in(&work, &["--under", "app", "status"]).success();
    assert!(section(&status, "`[ROOT]/work/app` and below").starts_with("\n- sessions: 2\n"), "{status}");
    assert!(section(&status, "the whole index").starts_with("\n- sessions: 4\n"), "{status}");
}

#[test]
fn forget_under_a_directory_removes_an_archived_session_below_it() {
    let (fixture, work) = app_sub_and_apple();
    fixture.run_in(&work, &["--all", "list"]).success();
    fixture.delete_session(&work.join("app/sub"), SUB_NEW);

    let forgotten = fixture.run_in(&work, &["--under", "app", "forget", "aaaa0002"]).success();
    assert_eq!(forgotten, "Forgot Sub new (aaaa0002).\n");
    let list = fixture.run_in(&work, &["--all", "list"]).success();
    assert_eq!(titles(&list), ["Apple new", "Apple old", "App old"], "{list}");
}

#[test]
fn forget_under_a_directory_does_not_find_a_session_outside_it_and_says_to_use_all() {
    let (fixture, work) = app_sub_and_apple();
    fixture.run_in(&work, &["--all", "list"]).success();
    fixture.delete_session(&work.join("apple"), APPLE_NEW);

    let error = fixture.run_in(&work, &["--under", "app", "forget", "bbbb0004"]).failure();
    assert_eq!(
        error,
        "claudegrep: no session bbbb0004 in `[ROOT]/work/app` and below; see `claudegrep --under [ROOT]/work/app list`, \
         or use --all instead of --under for other projects\n"
    );
    let list = fixture.run_in(&work, &["--all", "list"]).success();
    assert!(titles(&list).contains(&"Apple new"), "{list}");
}

#[test]
fn read_by_list_number_under_a_directory_counts_the_sessions_of_the_scope() {
    let (fixture, work) = app_sub_and_apple();
    let list = fixture.run_in(&work, &["--under", "app", "list"]).success();
    assert_eq!(titles(&list), ["Sub new", "App old"], "{list}");

    let read = fixture.run_in(&work, &["--under", "app", "read", "2", "--outline"]).success();
    assert!(read.contains("App old") && !read.contains("Sub new"), "{read}");
    let everywhere = fixture.run_in(&work, &["--all", "read", "2", "--outline"]).success();
    assert!(everywhere.contains("Sub new"), "{everywhere}");
}

#[test]
fn claude_config_dir_moves_the_default_projects_root() {
    let (fixture, work) = app_sub_and_apple();
    fixture.run_in(&work, &["config", "projects-root", "--unset"]).success();
    let config_dir = fixture.project_dir(&work.join("app")).parent().unwrap().parent().unwrap().to_path_buf();

    let error = fixture.run_in(&work, &["--all", "list"]).failure();
    assert!(error.starts_with("claudegrep: [ROOT]/home/.claude/projects does not exist, so there are no Claude Code sessions"), "{error}");
    let list = fixture.run_with_env(&work, &["--all", "list"], &[("CLAUDE_CONFIG_DIR", config_dir.to_str().unwrap())]).success();
    assert_eq!(titles(&list), ["Apple new", "Sub new", "Apple old", "App old"], "{list}");
}

#[test]
fn a_window_whose_since_is_after_its_until_lists_nothing_and_does_not_fail() {
    let (fixture, work) = app_sub_and_apple();
    let list = |window: &[&str]| fixture.run_in(&work, &[&["--all", "list"][..], window].concat()).success();
    assert_eq!(titles(&list(&["--since", "2026-01-03"])), ["Apple new", "Sub new"]);
    assert_eq!(titles(&list(&["--until", "2026-01-02"])), ["Apple old", "App old"]);
    assert_eq!(titles(&list(&["--since", "2026-01-03", "--until", "2026-01-02"])), Vec::<&str>::new());
}

#[test]
fn search_under_a_directory_with_since_keeps_both_limits() {
    let (fixture, work) = app_sub_and_apple();
    let found = fixture.run_with_model(&work, &["--under", "app", "search", "--since", "2026-01-03", "work"], &[]).success();
    assert!(found.contains("Sub new"), "{found}");
    assert!(!found.contains("App old") && !found.contains("Apple"), "{found}");
}

#[test]
fn index_under_a_directory_embeds_every_project_and_reports_the_scoped_status() {
    let (fixture, work) = app_sub_and_apple();
    let report = fixture.run_with_model(&work, &["--under", "app", "index"], &[]).success();
    let first = report.lines().next().unwrap();
    assert_eq!(
        first, "Archived 4 new or changed transcript files, parsed 4 sessions, embedded 4 new chunks.",
        "index embeds the 4 distinct chunks of every project, not the 3 of the scope"
    );
    assert!(section(&report, "`[ROOT]/work/app` and below").starts_with("\n- sessions: 2\n"), "{report}");
    for heading in ["`[ROOT]/work/app` and below", "the whole index"] {
        let embedded = section(&report, heading).lines().find_map(|line| line.strip_prefix("- embedded: ")).unwrap();
        let (done, rest) = embedded.split_once(" of ").unwrap();
        assert!(rest.starts_with(&format!("{done} ")), "{heading} is not fully embedded: {embedded}");
    }
}
