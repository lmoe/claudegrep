//! What an operator sees when the index, a setting or the output is unusable.

mod support;

use std::fs;
use std::io::Read;

use rusqlite::Connection;

use support::fixture::Fixture;
use support::lines::{Transcript, text};

const SESSION: &str = "aaaa0001-0000-4000-8000-000000000001";

#[test]
fn a_database_path_inside_a_file_fails_and_names_what_it_cannot_create() {
    let fixture = Fixture::new();
    let work = fixture.dir("work");
    let file = work.join("not-a-directory");
    fs::write(&file, "text").unwrap();
    let db = file.join("db.sqlite");

    let error = fixture.run_in(&work, &["--db", db.to_str().unwrap(), "list"]).failure();
    assert!(error.starts_with("claudegrep: creating [ROOT]/work/work/not-a-directory: "), "{error}");
}

#[test]
fn a_database_file_that_is_not_sqlite_fails_the_command() {
    let fixture = Fixture::new();
    let work = fixture.dir("work");
    let db = work.join("db.sqlite");
    fs::write(&db, "this is a text file, and it is longer than a database header would need\n".repeat(20)).unwrap();

    let error = fixture.run_in(&work, &["--db", db.to_str().unwrap(), "list"]).failure();
    assert!(error.contains("file is not a database"), "{error}");
}

#[test]
fn config_rejects_an_unknown_device_and_stores_nothing() {
    let fixture = Fixture::new();
    let work = fixture.dir("work");

    let error = fixture.run_in(&work, &["config", "device", "tpu"]).failure();
    assert_eq!(error, "claudegrep: the device must be auto, gpu or cpu\n");
    assert_eq!(fixture.run_in(&work, &["config", "device"]).success(), "device = cpu\n", "the fixture sets cpu");
}

#[test]
fn a_stored_setting_that_does_not_parse_fails_other_commands_but_config_lists_and_unsets_it() {
    let fixture = Fixture::new();
    let work = fixture.dir("work");
    Connection::open(fixture.db_path()).unwrap().execute("INSERT OR REPLACE INTO config (key, value) VALUES ('limit', 'abc')", []).unwrap();

    let error = fixture.run_in(&work, &["list"]).failure();
    assert!(error.contains("the limit must be a positive number"), "{error}");
    assert!(fixture.run_in(&work, &["config"]).success().contains("- `limit` = `abc` "));
    assert_eq!(fixture.run_in(&work, &["config", "limit", "--unset"]).success(), "limit = 8\n");
    fixture.run_in(&work, &["list"]).success();
}

#[test]
fn a_closed_stdout_is_not_an_error() {
    let fixture = Fixture::new();
    let work = fixture.dir("work");
    let transcript =
        Transcript::new(work.to_str().unwrap()).user("Write a long answer").assistant("m1", vec![text(&"word ".repeat(100_000))]);
    fixture.session(&work, SESSION, &transcript);
    let args = ["read", "aaaa0001", "--budget", "0"];
    let full = fixture.run_in(&work, &args).success();
    assert!(full.len() > 400_000, "the output must not fit in a pipe: {} bytes", full.len());

    let mut child = fixture.spawn_in(&work, &args);
    drop(child.stdout.take());
    let mut stderr = String::new();
    child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
    let status = child.wait().unwrap();
    assert_eq!((status.code(), stderr.as_str()), (Some(0), ""));
}
