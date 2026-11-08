use std::fs;

use chrono::{Days, NaiveDate};

use super::{DailyLog, FILE_COUNT, cleanup, log_date};

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 8).unwrap()
}

#[test]
fn creates_the_directory_and_appends_to_the_same_process_log() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("log");
    let mut log = DailyLog::new(directory.clone(), today(), 42).unwrap();
    log.write(today(), b"first\n").unwrap();
    drop(log);
    let mut log = DailyLog::new(directory.clone(), today(), 42).unwrap();
    log.write(today(), b"second\n").unwrap();
    drop(log);
    assert_eq!(
        fs::read_to_string(directory.join("shelllf-2026-11-08-42.log")).unwrap(),
        "first\nsecond\n"
    );
}

#[test]
fn retains_today_and_the_previous_six_days_only() {
    let directory = tempfile::tempdir().unwrap();
    for age in 0..12 {
        let date = today() - Days::new(age);
        for suffix in ["42.log", "42.1.log", "43.4.log"] {
            fs::write(
                directory.path().join(format!("shelllf-{date}-{suffix}")),
                "log",
            )
            .unwrap();
        }
    }
    cleanup(directory.path(), today());
    for age in 0..12 {
        let date = today() - Days::new(age);
        for suffix in ["42.log", "42.1.log", "43.4.log"] {
            assert_eq!(
                directory
                    .path()
                    .join(format!("shelllf-{date}-{suffix}"))
                    .exists(),
                age < 7
            );
        }
    }
}

#[test]
fn cleanup_preserves_unrelated_files_and_directories() {
    let directory = tempfile::tempdir().unwrap();
    for name in [
        "notes.log",
        "shelllf-2020-01-01.log",
        "shelllf-2020-01-01-password.log",
        "shelllf-2020-01-01-42.0.log",
        "shelllf-2020-01-01-42.1.log.bak",
        "shelllf-2020-99-99-42.log",
        "shelllf-2020-01-01-42.txt",
        "shelllf-2020-01-01-0.log",
    ] {
        fs::write(directory.path().join(name), "keep").unwrap();
    }
    let folder = directory.path().join("shelllf-2020-01-01-42.log");
    fs::create_dir(&folder).unwrap();
    cleanup(directory.path(), today());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 9);
    assert!(folder.is_dir());
    assert_eq!(log_date("shelllf-2026-11-08-42.1.log"), Some(today()));
    assert_eq!(log_date("shelllf-2026-11-08-42.1.2.log"), None);
}

#[test]
fn maintenance_rotates_days_and_cleans_up_even_without_a_new_record() {
    let directory = tempfile::tempdir().unwrap();
    let mut log = DailyLog::new(directory.path().to_path_buf(), today(), 42).unwrap();
    log.write(today(), b"old\n").unwrap();
    let next = today() + Days::new(7);
    log.maintain(next).unwrap();
    assert!(!directory.path().join("shelllf-2026-11-08-42.log").exists());
    log.write(next, b"new\n").unwrap();
    assert_eq!(
        fs::read_to_string(directory.path().join("shelllf-2026-11-15-42.log")).unwrap(),
        "new\n"
    );
}

#[test]
fn rotation_bounds_disk_usage_and_keeps_the_most_recent_records() {
    let directory = tempfile::tempdir().unwrap();
    let mut log = DailyLog::new(directory.path().to_path_buf(), today(), 42).unwrap();
    log.limit = 3;
    for number in 0..9 {
        log.write(today(), format!("{number}\n").as_bytes())
            .unwrap();
    }
    drop(log);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), FILE_COUNT);
    assert_eq!(
        fs::read_to_string(directory.path().join("shelllf-2026-11-08-42.log")).unwrap(),
        "8\n"
    );
    assert_eq!(
        fs::read_to_string(directory.path().join("shelllf-2026-11-08-42.4.log")).unwrap(),
        "4\n"
    );
}

#[test]
fn concurrent_processes_have_separate_files() {
    let directory = tempfile::tempdir().unwrap();
    let mut first = DailyLog::new(directory.path().to_path_buf(), today(), 42).unwrap();
    let mut second = DailyLog::new(directory.path().to_path_buf(), today(), 43).unwrap();
    first.write(today(), b"first").unwrap();
    second.write(today(), b"second").unwrap();
    assert_eq!(fs::read_to_string(first.path(0)).unwrap(), "first");
    assert_eq!(fs::read_to_string(second.path(0)).unwrap(), "second");
}

#[test]
fn a_file_in_place_of_the_directory_returns_an_error() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("log");
    fs::write(&path, "keep").unwrap();
    assert!(DailyLog::new(path.clone(), today(), 42).is_err());
    assert_eq!(fs::read_to_string(path).unwrap(), "keep");
}
