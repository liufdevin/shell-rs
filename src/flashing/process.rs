use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::Duration,
};

use super::EraseMode;
use crate::shared::process::CommandGroup;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// PhoenixConsole's own 30 second no-device timeout is too short for a person
/// to press a FEL key. The page re-launches the wait when it exits, but a
/// stalled write gets this much room before it is called a failure.
const BURN_TIMEOUT_SECONDS: &str = "300";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunEvent {
    Line(String),
    Failed(String),
    Exited { code: Option<i32>, stopped: bool },
}

/// A running PhoenixConsole wait or burn, owned by the flashing tab.
pub struct Run {
    stop: Arc<AtomicBool>,
}

impl Drop for Run {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Run {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Find the PhoenixConsole directory without a setting: a portable build
/// keeps it beside the executable, and a development checkout keeps it in
/// `dist`.
pub fn tool_dir() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("SHELLRS_PHOENIX_DIR") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("PhoenixConsole_Windows"));
        candidates.push(dir.join("dist").join("PhoenixConsole_Windows"));
        if let Some(parent) = dir.parent() {
            candidates.push(parent.join("dist").join("PhoenixConsole_Windows"));
        }
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("dist")
            .join("PhoenixConsole_Windows"),
    );
    candidates.into_iter().find(|dir| {
        dir.is_absolute()
            && dir.join("PhoenixConsole.exe").is_file()
            && dir.join("adb.exe").is_file()
    })
}

fn adb_path(tool_dir: &Path) -> PathBuf {
    tool_dir.join("adb.exe")
}

/// Start PhoenixConsole in the “only FES/device changes” mode. The
/// application owns the ADB-triggered reboot, so `--no-adb` keeps the tool
/// from restarting an already connected Android device by itself.
pub fn start_wait(tool_dir: &Path, firmware: &Path, mode: EraseMode) -> (Run, Receiver<RunEvent>) {
    let stop = Arc::new(AtomicBool::new(false));
    let run = Run { stop: stop.clone() };
    let (sender, receiver) = mpsc::sync_channel(512);
    let tool_dir = tool_dir.to_path_buf();
    let firmware = firmware.to_path_buf();
    std::thread::spawn(move || {
        if stop.load(Ordering::Relaxed) {
            let _ = sender.send(RunEvent::Exited {
                code: None,
                stopped: true,
            });
            return;
        }
        let mut command = Command::new(tool_dir.join("PhoenixConsole.exe"));
        command
            .current_dir(&tool_dir)
            .args([
                "--no-adb",
                "--reboot",
                "-o",
                "1",
                "-t",
                BURN_TIMEOUT_SECONDS,
                "-e",
                mode.flag(),
                "-p",
            ])
            .arg(&firmware)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let (mut child, group) = match CommandGroup::spawn(&mut command) {
            Ok(process) => process,
            Err(error) => {
                let _ = sender.send(RunEvent::Failed(error.to_string()));
                return;
            }
        };
        if let Err(error) = group.terminate_on_close() {
            group.terminate();
            let _ = child.wait();
            let _ = sender.send(RunEvent::Failed(error.to_string()));
            return;
        }
        let stdout = child.stdout.take().map(|stdout| {
            let sender = sender.clone();
            std::thread::spawn(move || read_lines(stdout, sender))
        });
        let stderr = child.stderr.take().map(|stderr| {
            let sender = sender.clone();
            std::thread::spawn(move || read_lines(stderr, sender))
        });
        let outcome = loop {
            if stop.load(Ordering::Relaxed) {
                group.terminate();
                let code = child.wait().ok().and_then(|status| status.code());
                break RunEvent::Exited {
                    code,
                    stopped: true,
                };
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    break RunEvent::Exited {
                        code: status.code(),
                        stopped: false,
                    };
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(error) => {
                    group.terminate();
                    let _ = child.wait();
                    break RunEvent::Failed(error.to_string());
                }
            }
        };
        group.terminate();
        for reader in stdout.into_iter().chain(stderr) {
            let _ = reader.join();
        }
        let _ = sender.send(outcome);
    });
    (run, receiver)
}

fn read_lines(mut stream: impl std::io::Read, sender: mpsc::SyncSender<RunEvent>) {
    let mut buffer = [0u8; 4096];
    let mut pending = Vec::new();
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                for byte in &buffer[..count] {
                    if matches!(*byte, b'\n' | b'\r') {
                        if !pending.is_empty() {
                            let line = decode(&pending);
                            pending.clear();
                            if sender.send(RunEvent::Line(line)).is_err() {
                                return;
                            }
                        }
                    } else if pending.len() < 8192 {
                        pending.push(*byte);
                    }
                }
            }
            Err(_) => break,
        }
    }
    if !pending.is_empty() {
        let _ = sender.send(RunEvent::Line(decode(&pending)));
    }
}

fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => {
            let (text, _, _) = encoding_rs::GBK.decode(bytes);
            text.into_owned()
        }
    }
}

/// The serial numbers ADB currently sees as usable devices.
pub fn adb_devices(tool_dir: &Path) -> Result<Vec<String>, String> {
    let program = adb_path(tool_dir);
    let output = crate::adb::run_usb_adb(program.clone(), &["devices", "-l"])
        .map_err(|error| error.to_string())?;
    let listing = decode(&output);
    let usb_devices = parse_usb_devices(&listing);
    if usb_devices.len() > 1 {
        return Ok(usb_devices);
    }
    if !listing
        .lines()
        .any(|line| line.split_whitespace().nth(1) == Some("device"))
    {
        return Ok(Vec::new());
    }
    match crate::adb::run_usb_adb(program, &["-d", "get-serialno"]) {
        Ok(serial) => {
            let serial = decode(&serial).trim().to_owned();
            Ok(listing
                .lines()
                .filter_map(|line| {
                    let mut fields = line.split_whitespace();
                    let candidate = fields.next()?;
                    (candidate == serial && fields.next() == Some("device")).then(|| serial.clone())
                })
                .collect())
        }
        Err(error) if error.to_string().contains("more than one") => Err(error.to_string()),
        Err(_) => Ok(Vec::new()),
    }
}

fn parse_usb_devices(output: &str) -> Vec<String> {
    let mut devices = Vec::new();
    for line in output.lines() {
        let mut fields = line.split_whitespace();
        let Some(serial) = fields.next() else {
            continue;
        };
        if fields.next() == Some("device")
            && !serial.starts_with('-')
            && fields.any(|field| field.starts_with("usb:"))
        {
            devices.push(serial.to_string());
        }
    }
    devices
}

/// Ask the connected Android system to reboot into efex, which is what a
/// board's FEL key does in hardware.
pub fn reboot_to_efex(tool_dir: &Path, serial: &str) -> Result<String, String> {
    if adb_devices(tool_dir)?.as_slice() != [serial] {
        return Err(String::new());
    }
    crate::adb::run_usb_adb(
        adb_path(tool_dir),
        &["-s", serial, "shell", "reboot", "efex"],
    )
    .map(|output| decode(&output).trim().to_string())
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{decode, parse_usb_devices};

    #[test]
    fn decodes_gbk_output_when_it_is_not_utf8() {
        let (bytes, _, _) = encoding_rs::GBK.encode("正在烧录");
        assert_eq!(decode(&bytes), "正在烧录");
    }

    #[test]
    fn only_authorized_usb_devices_are_reboot_candidates() {
        assert_eq!(
            parse_usb_devices(
                "List of devices attached\nboard device usb:1-2 product:board\nremote:5555 device product:board\nother unauthorized usb:1-3\n"
            ),
            ["board"]
        );
    }
}
