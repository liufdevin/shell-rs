use std::io::Write as _;
use std::process::{Command, Stdio};

fn cli(directory: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_shelllf-cli"));
    command
        .env("SHELLRS_DATA_DIR", directory)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY");
    command
}

#[test]
fn peers_runs_without_a_desktop_and_reuses_its_identity() {
    let directory = tempfile::tempdir().unwrap();
    let first = cli(directory.path())
        .args(["lan", "debug", "peers"])
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(String::from_utf8(first.stdout).unwrap().trim(), "[]");
    let key_path = directory.path().join("lan-cli/identity");
    let identity = std::fs::read(&key_path).unwrap();
    let second = cli(directory.path())
        .args(["lan", "debug", "peers"])
        .output()
        .unwrap();
    assert!(second.status.success());
    assert!(identity == std::fs::read(&key_path).unwrap());
}

#[test]
fn oversized_stdin_is_rejected_before_pairing_or_identity_creation() {
    let directory = tempfile::tempdir().unwrap();
    let mut child = cli(directory.path())
        .args(["lan", "debug", "exec", "peer", "tab", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input
        .write_all(format!("{}\n\nignored", "x".repeat(512)).as_bytes())
        .unwrap();
    drop(input);
    let result = child.wait_with_output().unwrap();
    assert_eq!(result.status.code(), Some(255));
    assert!(String::from_utf8_lossy(&result.stderr).contains("invalid_serial_debug_command"));
    assert!(!directory.path().join("lan-cli").exists());
}
