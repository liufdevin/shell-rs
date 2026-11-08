use std::{
    fs,
    io::Write,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
};

use tracing::{Level, subscriber::with_default};
use tracing_subscriber::{fmt::MakeWriter, prelude::*};

use super::{
    LogQueue, Message, QUEUE_RECORDS, RECORD_BYTES, TRUNCATED, beside_executable, start_worker,
    targets,
};

#[test]
fn logs_are_beside_the_executable() {
    let executable = Path::new("portable").join("ShellLF.exe");
    assert_eq!(
        beside_executable(&executable).unwrap(),
        Path::new("portable").join("log")
    );
}

#[test]
fn background_writer_flushes_records_with_levels_and_targets() {
    let directory = tempfile::tempdir().unwrap();
    let guard = start_worker(directory.path().to_path_buf()).unwrap();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(guard.queue.clone())
            .with_filter(targets(None)),
    );
    with_default(subscriber, || {
        tracing::info!(target: "shellrs::test", process_id = 42, "Application started");
        tracing::warn!(target: "shellrs::test", "Connection failed");
        tracing::debug!(target: "shellrs::test", "hidden debug record");
        tracing::debug!(target: "russh", "hidden protocol record");
    });
    guard.flush();
    let path = fs::read_dir(directory.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let text = fs::read_to_string(path).unwrap();
    assert!(text.contains("INFO") && text.contains("WARN"));
    assert!(text.contains("shellrs::test") && text.contains("process_id=42"));
    assert!(text.contains("Application started") && text.contains("Connection failed"));
    assert!(!text.contains("hidden") && !text.contains('\u{1b}'));
}

#[test]
fn normal_exit_drains_records_without_an_explicit_flush() {
    let directory = tempfile::tempdir().unwrap();
    let guard = start_worker(directory.path().to_path_buf()).unwrap();
    for number in 0..50 {
        writeln!(guard.queue.make_writer(), "record {number}").unwrap();
    }
    drop(guard);
    let path = fs::read_dir(directory.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let text = fs::read_to_string(path).unwrap();
    assert_eq!(text.lines().count(), 50);
    assert!(text.contains("record 49"));
}

#[test]
fn queue_is_bounded_and_critical_panic_records_bypass_congestion() {
    let (sender, receiver) = mpsc::channel();
    let queue = LogQueue {
        sender,
        pending: Arc::new(AtomicUsize::new(0)),
        dropped: Arc::new(AtomicUsize::new(0)),
    };
    for _ in 0..QUEUE_RECORDS + 20 {
        writeln!(queue.make_writer(), "record").unwrap();
    }
    assert_eq!(queue.pending.load(Ordering::Relaxed), QUEUE_RECORDS);
    assert_eq!(queue.dropped.load(Ordering::Relaxed), 20);
    queue.record(b"panic".to_vec(), true);
    assert_eq!(receiver.try_iter().count(), QUEUE_RECORDS + 1);
}

#[test]
fn oversized_utf8_records_are_bounded_and_marked() {
    let (sender, receiver) = mpsc::channel();
    let queue = LogQueue {
        sender,
        pending: Arc::new(AtomicUsize::new(0)),
        dropped: Arc::new(AtomicUsize::new(0)),
    };
    write!(queue.make_writer(), "{}", "日志".repeat(RECORD_BYTES)).unwrap();
    let Message::Record { bytes, .. } = receiver.recv().unwrap() else {
        panic!("expected a log record")
    };
    assert!(bytes.len() <= RECORD_BYTES);
    assert!(bytes.ends_with(TRUNCATED));
    assert!(std::str::from_utf8(&bytes).is_ok());
}

#[test]
fn debug_override_does_not_enable_dependency_protocol_tracing() {
    let filter = targets(Some("debug"));
    assert!(filter.would_enable("shellrs::test", &Level::DEBUG));
    assert!(!filter.would_enable("russh", &Level::DEBUG));
    assert!(!filter.would_enable("shellrs::test", &Level::TRACE));
    assert!(targets(Some("invalid")).would_enable("shellrs::test", &Level::INFO));
}

#[test]
fn panic_hook_captures_a_real_panic_and_flushes_in_a_subprocess() {
    const DIRECTORY_ENV: &str = "SHELLLF_LOGGING_TEST_DIRECTORY";
    if let Some(directory) = std::env::var_os(DIRECTORY_ENV) {
        let guard = super::install(start_worker(directory.into()).unwrap(), None).unwrap();
        let result = std::panic::catch_unwind(|| panic!("diagnostic panic test"));
        assert!(result.is_err());
        drop(guard);
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "app::logging::tests::panic_hook_captures_a_real_panic_and_flushes_in_a_subprocess",
            "--nocapture",
        ])
        .env(DIRECTORY_ENV, directory.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let path = fs::read_dir(directory.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let text = fs::read_to_string(path).unwrap();
    assert!(text.contains("ShellLF starting"));
    assert!(text.contains("ERROR shellrs::panic"));
    assert!(text.contains("diagnostic panic test") && text.contains("tests.rs"));
}
