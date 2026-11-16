use std::{
    ffi::OsString,
    io::Read as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

use super::EraseMode;
use super::firmware::{FirmwareCapabilities, read_capabilities};
use super::model::{FirmwarePartition, parse_partitions};
use crate::shared::process::CommandGroup;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// PhoenixConsole's own 30 second no-device timeout is too short for a person
/// to press a FEL key. The page re-launches the wait when it exits, but a
/// stalled write gets this much room before it is called a failure.
const BURN_TIMEOUT_SECONDS: &str = "300";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunEvent {
    DevicesPresent {
        at: Instant,
        devices: std::collections::BTreeSet<String>,
    },
    DeviceChanged {
        at: Instant,
        instance: String,
        present: bool,
    },
    DetectionUnavailable {
        at: Instant,
        error: String,
    },
    Line {
        at: Instant,
        text: String,
    },
    Failed {
        at: Instant,
        error: String,
    },
    Exited {
        at: Instant,
        code: Option<i32>,
        stopped: bool,
    },
}

pub(super) enum AdbReboot {
    NoDevice,
    MultipleDevices,
    Rebooted { serial: String, output: String },
}

struct ConsoleLease {
    #[cfg(windows)]
    _handle: std::os::windows::io::OwnedHandle,
}

impl ConsoleLease {
    fn acquire() -> Result<Self, String> {
        #[cfg(windows)]
        {
            use std::os::windows::io::{FromRawHandle as _, OwnedHandle};
            use windows_sys::Win32::{
                Foundation::{ERROR_ALREADY_EXISTS, GetLastError},
                System::Threading::CreateMutexW,
            };
            let name: Vec<u16> = "Local\\ShellLF.PhoenixConsole\0".encode_utf16().collect();
            let raw = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
            if raw.is_null() {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
            let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
            if exists {
                return Err("Another ShellLF firmware tool is already running".into());
            }
            Ok(Self { _handle: handle })
        }
        #[cfg(not(windows))]
        {
            Ok(Self {})
        }
    }
}

fn burn_arguments(
    firmware: &Path,
    mode: EraseMode,
    partitions: Option<&[u32]>,
) -> Result<Vec<OsString>, String> {
    let mut arguments: Vec<OsString> = [
        "--no-adb",
        "--reboot",
        "--loop",
        "-o",
        "8",
        "-t",
        BURN_TIMEOUT_SECONDS,
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    if let Some(partitions) = partitions {
        if partitions.is_empty() {
            return Err("No firmware partition selected".into());
        }
        arguments.push("--parts".into());
        arguments.push(
            partitions
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",")
                .into(),
        );
    } else {
        arguments.extend([
            OsString::from("-e"),
            OsString::from(mode.flag()),
            OsString::from("--parts"),
            OsString::from("all"),
        ]);
    }
    arguments.extend([OsString::from("-p"), firmware.as_os_str().to_owned()]);
    Ok(arguments)
}

pub(super) fn inspect_firmware(
    tool_dir: &Path,
    firmware: &Path,
) -> (Run, Receiver<Result<FirmwareInfo, String>>) {
    let stop = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let run = Run {
        stop: stop.clone(),
        finished: finished.clone(),
    };
    let tool_dir = tool_dir.to_path_buf();
    let firmware = firmware.to_path_buf();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _completion = RunCompletion(finished);
        let result = read_partitions(&tool_dir, &firmware, &stop).map(|partitions| FirmwareInfo {
            partitions,
            capabilities: read_capabilities(&firmware),
        });
        let _ = sender.send(result);
    });
    (run, receiver)
}

pub(super) struct FirmwareInfo {
    pub partitions: Vec<FirmwarePartition>,
    pub capabilities: FirmwareCapabilities,
}

fn read_partitions(
    tool_dir: &Path,
    firmware: &Path,
    stop: &AtomicBool,
) -> Result<Vec<FirmwarePartition>, String> {
    if stop.load(Ordering::Relaxed) {
        return Err("Firmware partition inspection was cancelled".into());
    }
    let _lease = ConsoleLease::acquire()?;
    super::tool_conflict::check()?;
    let program = super::compatibility::prepare_console(tool_dir)?;
    let mut command = Command::new(program.as_os_str());
    command
        .current_dir(tool_dir)
        .args(["--no-adb", "--list-parts", "-p"])
        .arg(firmware)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let (mut child, group) =
        CommandGroup::spawn(&mut command).map_err(|error| error.to_string())?;
    if let Err(error) = group.terminate_on_close() {
        group.terminate();
        let _ = child.wait();
        return Err(error.to_string());
    }
    let stdout = child.stdout.take().map(|stream| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stream.take(1024 * 1024).read_to_end(&mut bytes);
            bytes
        })
    });
    let stderr = child.stderr.take().map(|stream| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stream.take(1024 * 1024).read_to_end(&mut bytes);
            bytes
        })
    });
    let started = Instant::now();
    let outcome = loop {
        if stop.load(Ordering::Relaxed) || started.elapsed() >= Duration::from_secs(120) {
            group.terminate();
            let _ = child.wait();
            break Err("Firmware partition inspection was cancelled or timed out".to_string());
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status.success()),
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => {
                group.terminate();
                let _ = child.wait();
                break Err(error.to_string());
            }
        }
    };
    group.terminate();
    let mut bytes = Vec::new();
    for reader in stdout.into_iter().chain(stderr) {
        if let Ok(output) = reader.join() {
            bytes.extend(output);
            bytes.push(b'\n');
        }
    }
    let output = decode(&bytes);
    if !outcome? {
        return Err(output.trim().to_string());
    }
    parse_partitions(&output)
}

/// A running PhoenixConsole wait or burn, owned by the flashing tab.
pub struct Run {
    stop: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
}

struct RunCompletion(Arc<AtomicBool>);

impl Drop for RunCompletion {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
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

    pub(super) fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(super) fn test_run() -> (Self, Arc<AtomicBool>) {
        let stop = Arc::new(AtomicBool::new(false));
        (
            Self {
                stop: stop.clone(),
                finished: Arc::new(AtomicBool::new(false)),
            },
            stop,
        )
    }
}

/// Find the PhoenixConsole directory without a setting: a portable build
/// keeps it beside the executable, and a development checkout keeps it in
/// `dist`.
pub fn tool_dir(preferred: Option<&Path>) -> Option<PathBuf> {
    if let Some(preferred) = preferred {
        return is_tool_dir(preferred).then(|| preferred.to_path_buf());
    }
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
    candidates.into_iter().find(|dir| is_tool_dir(dir))
}

pub(super) fn is_tool_dir(dir: &Path) -> bool {
    dir.is_absolute() && dir.join("PhoenixConsole.exe").is_file()
}

/// Start PhoenixConsole in the “only FES/device changes” mode. The
/// application owns the ADB-triggered reboot, so `--no-adb` keeps the tool
/// from restarting an already connected Android device by itself.
pub fn start_wait(
    tool_dir: &Path,
    firmware: &Path,
    mode: EraseMode,
    partitions: Option<Vec<u32>>,
) -> (Run, Receiver<RunEvent>) {
    let stop = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let run = Run {
        stop: stop.clone(),
        finished: finished.clone(),
    };
    let (sender, receiver) = mpsc::sync_channel(512);
    let tool_dir = tool_dir.to_path_buf();
    let firmware = firmware.to_path_buf();
    std::thread::spawn(move || {
        let _completion = RunCompletion(finished);
        if stop.load(Ordering::Relaxed) {
            let _ = sender.send(RunEvent::Exited {
                at: Instant::now(),
                code: None,
                stopped: true,
            });
            return;
        }
        let _lease = match ConsoleLease::acquire() {
            Ok(lease) => lease,
            Err(error) => {
                let _ = sender.send(RunEvent::Failed {
                    at: Instant::now(),
                    error,
                });
                return;
            }
        };
        let arguments = match burn_arguments(&firmware, mode, partitions.as_deref()) {
            Ok(arguments) => arguments,
            Err(error) => {
                let _ = sender.send(RunEvent::Failed {
                    at: Instant::now(),
                    error,
                });
                return;
            }
        };
        let result = super::compatibility::prepare_console(&tool_dir).and_then(|program| {
            super::tool_conflict::check()?;
            #[cfg(windows)]
            let _monitor = match super::device_monitor::DeviceMonitor::start(sender.clone()) {
                Ok(monitor) => Some(monitor),
                Err(error) => {
                    let _ = sender.send(RunEvent::DetectionUnavailable {
                        at: Instant::now(),
                        error,
                    });
                    None
                }
            };
            super::console::run_console(
                super::console::command(&program, &tool_dir, &arguments),
                &stop,
                sender.clone(),
            )
        });
        let outcome = match result {
            Ok((code, stopped)) => RunEvent::Exited {
                at: Instant::now(),
                code,
                stopped,
            },
            Err(error) => RunEvent::Failed {
                at: Instant::now(),
                error,
            },
        };
        drop(_lease);
        let _ = sender.send(outcome);
    });
    (run, receiver)
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
fn adb_devices(program: &Path, stop: &AtomicBool) -> Result<Vec<String>, String> {
    let output = crate::adb::run_usb_adb(program.to_path_buf(), &["devices", "-l"], &mut || {
        stop.load(Ordering::Relaxed)
    })
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
    match crate::adb::run_usb_adb(program.to_path_buf(), &["-d", "get-serialno"], &mut || {
        stop.load(Ordering::Relaxed)
    }) {
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

/// Check ADB only when the user asks for it. Polling ADB while PhoenixConsole
/// owns the FES device makes device detection slower and can interfere with
/// the flashing tool.
pub(super) fn start_adb_reboot() -> (Run, Receiver<Result<AdbReboot, String>>) {
    let stop = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let run = Run {
        stop: stop.clone(),
        finished: finished.clone(),
    };
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _completion = RunCompletion(finished);
        let _ = sender.send(reboot_to_efex(&stop));
    });
    (run, receiver)
}

fn reboot_to_efex(stop: &AtomicBool) -> Result<AdbReboot, String> {
    let program =
        crate::adb::find_adb().ok_or_else(|| crate::i18n::t!("adb.missing").to_string())?;
    let devices = adb_devices(&program, stop)?;
    let serial = match devices.as_slice() {
        [] => return Ok(AdbReboot::NoDevice),
        [serial] => serial.clone(),
        _ => return Ok(AdbReboot::MultipleDevices),
    };
    crate::adb::run_usb_adb(
        program,
        &["-s", &serial, "shell", "reboot", "efex"],
        &mut || stop.load(Ordering::Relaxed),
    )
    .map(|output| AdbReboot::Rebooted {
        serial,
        output: decode(&output).trim().to_string(),
    })
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{burn_arguments, decode, is_tool_dir, parse_usb_devices, tool_dir};
    use crate::flashing::EraseMode;

    #[test]
    fn listeners_loop_without_adb_and_partition_burns_never_add_full_erase_flags() {
        let firmware = std::path::Path::new("firmware with spaces.img");
        for mode in EraseMode::ALL {
            let args = burn_arguments(firmware, mode, None).unwrap();
            let args = args
                .iter()
                .map(|arg| arg.to_str().unwrap())
                .collect::<Vec<_>>();
            assert!(args.contains(&"--loop"));
            assert!(args.contains(&"--no-adb"));
            assert!(args.windows(2).any(|pair| pair == ["-o", "8"]));
            assert!(args.windows(2).any(|pair| pair == ["-e", mode.flag()]));
            assert_eq!(args.last(), Some(&"firmware with spaces.img"));
            assert!(args.windows(2).any(|pair| pair == ["--parts", "all"]));
        }
        let args = burn_arguments(firmware, EraseMode::EraseAll, Some(&[0, 3])).unwrap();
        let args = args
            .iter()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        assert!(args.windows(2).any(|pair| pair == ["--parts", "0,3"]));
        assert!(!args.contains(&"-e"));
        assert!(!args.contains(&"--erase-all"));
        assert!(burn_arguments(firmware, EraseMode::EraseAll, Some(&[])).is_err());
    }

    #[test]
    fn an_explicit_tool_directory_only_requires_phoenix_and_never_falls_back() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path();
        assert!(!is_tool_dir(path));
        assert!(tool_dir(Some(path)).is_none());
        std::fs::write(path.join("PhoenixConsole.exe"), []).unwrap();
        assert!(is_tool_dir(path));
        assert_eq!(tool_dir(Some(path)), Some(path.to_path_buf()));
        std::fs::remove_file(path.join("PhoenixConsole.exe")).unwrap();
        assert!(tool_dir(Some(path)).is_none());
    }

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
