//! This test runs with `harness = false` (see Cargo.toml): dates render in local time, and `main` sets `TZ=UTC` before
//! any thread exists, which the multi-threaded test harness would not allow.

use claudegrep::chunk::Role;
use claudegrep::discover::Scope;
use claudegrep::render;
use claudegrep::search::{Hit, SessionHits};
use claudegrep::store::{Passage, SessionRow};
use claudegrep::transcript::SessionMeta;
use claudegrep::view::{ListView, SearchView};
use insta::assert_snapshot;

fn main() {
    // SAFETY: this is the first statement of the process, so no other thread can read the environment concurrently.
    unsafe { std::env::set_var("TZ", "UTC") };
    let cases: &[(&str, fn())] = &[
        ("matches_with_context_in_a_directory", matches_with_context_in_a_directory),
        ("matches_in_all_projects", matches_in_all_projects),
        ("one_match", one_match),
        ("two_matches_in_one_turn", two_matches_in_one_turn),
        ("the_same_text_in_one_other_place", the_same_text_in_one_other_place),
        ("long_query_is_shortened", long_query_is_shortened),
        ("nothing_found_in_a_directory", nothing_found_in_a_directory),
        ("nothing_found_anywhere", nothing_found_anywhere),
        ("only_the_current_session_is_here", only_the_current_session_is_here),
        ("sessions_in_a_directory", sessions_in_a_directory),
        ("sessions_in_all_projects", sessions_in_all_projects),
        ("no_sessions_in_a_directory", no_sessions_in_a_directory),
        ("no_sessions_anywhere", no_sessions_anywhere),
        ("forgotten_session", forgotten_session),
    ];
    for (name, case) in cases {
        case();
        println!("render::{name} ... ok");
    }
    println!("\n{} render snapshots passed", cases.len());
}

fn directory() -> Scope {
    Scope::Directory { root: "/home/dev/app".into(), project_dir: "-home-dev-app".into() }
}

fn session(id: &str, title: &str, turns: usize, archived: bool) -> SessionRow {
    SessionRow {
        path: format!("/projects/-home-dev-app/{id}.jsonl"),
        session_id: id.to_owned(),
        meta: SessionMeta {
            title: title.to_owned(),
            cwd: Some("/home/dev/app".to_owned()),
            first: "2026-03-01T09:00:00.000Z".parse().unwrap(),
            last: "2026-03-02T17:45:00.000Z".parse().unwrap(),
        },
        turns,
        archived,
    }
}

fn passage(turn: usize, part: usize, role: Role, text: &str) -> Passage {
    Passage { turn, part, role, ts: format!("2026-03-01T09:{turn:02}:00.000Z").parse().unwrap(), text: text.to_owned() }
}

const fn hit(matched: usize, range: std::ops::Range<usize>, elsewhere: usize) -> Hit {
    Hit { matched, range, elsewhere }
}

fn view(scope: Scope, query: &str, groups: Vec<SessionHits>) -> SearchView {
    SearchView { scope, query: query.to_owned(), limit: 8, only_current: false, groups, moved_root: None }
}

fn token_session() -> SessionHits {
    SessionHits {
        session: session("aaaa1111-0000-4000-8000-000000000001", "Fix token refresh", 4, false),
        passages: vec![
            passage(0, 0, Role::User, "Why does the token refresh fail after an hour?"),
            passage(1, 0, Role::Assistant, "The clock skew check compared local time with UTC.\n\nSo tokens looked expired early."),
            passage(1, 1, Role::Assistant, "## Fix\n\n- convert both sides to UTC\n- allow 30 s of skew"),
            passage(2, 0, Role::Agent, "Task: Find token refresh\n\nTokens are refreshed in src/auth.rs."),
            passage(3, 0, Role::User, "Thanks, ship it."),
        ],
        hits: vec![hit(1, 0..3, 0), hit(3, 3..4, 2)],
    }
}

fn notes_session() -> SessionHits {
    SessionHits {
        session: session("aaaa2222-0000-4000-8000-000000000002", "Release notes 0.2", 2, true),
        passages: vec![passage(0, 0, Role::User, "Write the release notes"), passage(1, 0, Role::Assistant, "- token refresh is fixed")],
        hits: vec![hit(1, 1..2, 0)],
    }
}

fn matches_with_context_in_a_directory() {
    assert_snapshot!(render::search(&view(directory(), "token refresh", vec![token_session(), notes_session()])));
}

fn matches_in_all_projects() {
    assert_snapshot!(render::search(&view(Scope::All, "token refresh", vec![token_session(), notes_session()])));
}

fn one_match() {
    assert_snapshot!(render::search(&view(directory(), "release notes", vec![notes_session()])));
}

fn two_matches_in_one_turn() {
    let mut group = token_session();
    group.hits = vec![hit(1, 1..2, 0), hit(2, 2..3, 0)];
    assert_snapshot!(render::search(&view(directory(), "skew", vec![group])));
}

fn the_same_text_in_one_other_place() {
    let mut group = notes_session();
    group.hits[0].elsewhere = 1;
    assert_snapshot!(render::search(&view(directory(), "release notes", vec![group])));
}

fn long_query_is_shortened() {
    let query = "why ".repeat(40);
    assert_snapshot!(render::search(&view(directory(), query.trim(), vec![notes_session()])));
}

fn nothing_found_in_a_directory() {
    assert_snapshot!(render::search(&view(directory(), "nothing", Vec::new())));
}

fn nothing_found_anywhere() {
    assert_snapshot!(render::search(&view(Scope::All, "nothing", Vec::new())));
}

fn only_the_current_session_is_here() {
    let mut view = view(directory(), "nothing", Vec::new());
    view.only_current = true;
    assert_snapshot!(render::search(&view));
}

fn sessions() -> Vec<SessionRow> {
    let mut other = session("bbbb3333-0000-4000-8000-000000000003", "Set up CI", 2, false);
    other.meta.cwd = None;
    other.meta.first = "2026-03-03T08:00:00.000Z".parse().unwrap();
    other.meta.last = "2026-03-03T08:30:00.000Z".parse().unwrap();
    vec![other, session("aaaa2222-0000-4000-8000-000000000002", "Release notes 0.2", 1, true), token_session().session]
}

fn list(scope: Scope, sessions: Vec<SessionRow>, current: Option<&str>) -> ListView {
    ListView { scope, sessions, current: current.map(str::to_owned), moved_root: None }
}

fn sessions_in_a_directory() {
    let current = Some("aaaa1111-0000-4000-8000-000000000001");
    assert_snapshot!(render::list(&list(directory(), sessions(), current)));
}

fn sessions_in_all_projects() {
    assert_snapshot!(render::list(&list(Scope::All, sessions(), None)));
}

fn no_sessions_in_a_directory() {
    assert_snapshot!(render::list(&list(directory(), Vec::new(), None)));
}

fn no_sessions_anywhere() {
    assert_snapshot!(render::list(&list(Scope::All, Vec::new(), None)));
}

fn forgotten_session() {
    assert_eq!(render::forget(&notes_session().session), "Forgot Release notes 0.2 (aaaa2222).\n");
}
