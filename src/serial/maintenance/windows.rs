use std::{
    collections::HashMap,
    io::{Read as _, Seek as _, SeekFrom},
    net::TcpListener,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use base64::Engine as _;

use super::{MaintenanceFailure, RemovalState, SerialDevice, SerialRemoval};

const COMMON: &str = include_str!("common.ps1");
const SCAN: &str = include_str!("scan.ps1");
const REMOVE: &str = include_str!("remove.ps1");

fn failure(error: impl std::fmt::Display) -> MaintenanceFailure {
    MaintenanceFailure::Failed(
        error
            .to_string()
            .chars()
            .filter(|character| !character.is_control())
            .take(1200)
            .collect(),
    )
}

fn powershell() -> Result<PathBuf, MaintenanceFailure> {
    use std::os::windows::ffi::OsStringExt as _;
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut buffer = vec![0u16; 32768];
    let count = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if count == 0 || count >= buffer.len() {
        return Err(failure(std::io::Error::last_os_error()));
    }
    let path = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..count]))
        .join("WindowsPowerShell/v1.0/powershell.exe");
    if !path.is_file() {
        return Err(failure("Windows PowerShell is unavailable"));
    }
    Ok(path)
}

fn encoded(script: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    )
}

pub(super) fn scan() -> Result<Vec<SerialDevice>, MaintenanceFailure> {
    use std::os::windows::process::CommandExt as _;
    let mut log = tempfile::tempfile().map_err(failure)?;
    let mut errors = tempfile::tempfile().map_err(failure)?;
    let mut child = Command::new(powershell()?)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encoded(&format!("{COMMON}\n{SCAN}")),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().map_err(failure)?))
        .stderr(Stdio::from(errors.try_clone().map_err(failure)?))
        .creation_flags(0x0800_0000)
        .spawn()
        .map_err(failure)?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(failure)? {
            break status;
        }
        if started.elapsed() > Duration::from_secs(45) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(MaintenanceFailure::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if !status.success() {
        errors.seek(SeekFrom::Start(0)).map_err(failure)?;
        let mut detail = String::new();
        errors
            .take(4096)
            .read_to_string(&mut detail)
            .map_err(failure)?;
        return Err(failure(detail));
    }
    if log.metadata().map_err(failure)?.len() > 512 * 1024 {
        return Err(failure("scan_response_too_large"));
    }
    log.seek(SeekFrom::Start(0)).map_err(failure)?;
    let devices: Vec<SerialDevice> = serde_json::from_reader(log).map_err(failure)?;
    if devices.len() > 256 {
        return Err(failure("scan_response_too_large"));
    }
    Ok(devices)
}

pub(super) fn remove(devices: &[SerialDevice]) -> Result<Vec<SerialRemoval>, MaintenanceFailure> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(failure)?;
    listener.set_nonblocking(true).map_err(failure)?;
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let selection = base64::engine::general_purpose::STANDARD
        .encode(serde_json::to_vec(devices).map_err(failure)?);
    let body = REMOVE
        .replace("__SELECTION__", &selection)
        .replace(
            "__PORT__",
            &listener.local_addr().map_err(failure)?.port().to_string(),
        )
        .replace("__TOKEN__", &token);
    let script = format!("{COMMON}\n{body}");
    let process = elevate(&script)?;
    let started = Instant::now();
    let mut reports = HashMap::new();
    while started.elapsed() < Duration::from_secs(120) && reports.len() < devices.len() {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_millis(100)))
                    .map_err(failure)?;
                let mut input = Vec::new();
                let mut authenticated = false;
                let mut closed = false;
                while started.elapsed() < Duration::from_secs(120)
                    && !closed
                    && reports.len() < devices.len()
                {
                    let mut bytes = [0u8; 4096];
                    match stream.read(&mut bytes) {
                        Ok(0) => closed = true,
                        Ok(count) => input.extend_from_slice(&bytes[..count]),
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                            ) => {}
                        Err(_) => closed = true,
                    }
                    if input.len() > 16384 {
                        break;
                    }
                    while let Some(end) = input.iter().position(|byte| *byte == b'\n') {
                        let line: Vec<_> = input.drain(..=end).collect();
                        let line = String::from_utf8_lossy(&line);
                        if !authenticated {
                            if line.trim_end() != token {
                                closed = true;
                                break;
                            }
                            authenticated = true;
                        } else if let Ok(report) = serde_json::from_str::<SerialRemoval>(&line)
                            && report.detail.len() <= 4096
                            && devices
                                .iter()
                                .any(|device| device.instance_id == report.instance_id)
                        {
                            reports.entry(report.instance_id.clone()).or_insert(report);
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if process.has_exited()? {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(failure(error)),
        }
    }
    Ok(devices.iter().map(|device| {
        reports.remove(&device.instance_id).unwrap_or_else(|| SerialRemoval::new(
            device.instance_id.clone(), RemovalState::Failed,
            "No verified result was received. Rescan before retrying; an in-progress removal may have completed.".into(),
        ))
    }).collect())
}

struct ElevatedProcess(windows_sys::Win32::Foundation::HANDLE);

impl ElevatedProcess {
    fn has_exited(&self) -> Result<bool, MaintenanceFailure> {
        use windows_sys::Win32::{
            Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
            System::Threading::WaitForSingleObject,
        };
        match unsafe { WaitForSingleObject(self.0, 0) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(failure(std::io::Error::last_os_error())),
        }
    }
}

impl Drop for ElevatedProcess {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

fn elevate(script: &str) -> Result<ElevatedProcess, MaintenanceFailure> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::{
        Foundation::ERROR_CANCELLED,
        System::Com::{
            COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
        },
        UI::{
            Shell::{
                SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
                ShellExecuteExW,
            },
            WindowsAndMessaging::SW_HIDE,
        },
    };
    let initialized = unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    if initialized < 0 {
        return Err(failure("COM initialization failed"));
    }
    struct Com;
    impl Drop for Com {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    let _com = Com;
    let executable = powershell()?;
    let parameters = format!(
        "-NoLogo -NoProfile -NonInteractive -EncodedCommand {}",
        encoded(script)
    );
    if parameters.len() > 30000 {
        return Err(failure("The selection is too large. Select fewer devices."));
    }
    let wide = |value: &std::ffi::OsStr| {
        value
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>()
    };
    let verb = wide(std::ffi::OsStr::new("runas"));
    let file = wide(executable.as_os_str());
    let arguments = wide(std::ffi::OsStr::new(&parameters));
    let directory = wide(executable.parent().unwrap().as_os_str());
    let mut information = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: arguments.as_ptr(),
        lpDirectory: directory.as_ptr(),
        nShow: SW_HIDE,
        ..unsafe { std::mem::zeroed() }
    };
    if unsafe { ShellExecuteExW(&mut information) } == 0 {
        let error = std::io::Error::last_os_error();
        return Err(if error.raw_os_error() == Some(ERROR_CANCELLED as i32) {
            MaintenanceFailure::Cancelled
        } else {
            failure(error)
        });
    }
    if information.hProcess.is_null() {
        return Err(failure("No elevated process handle"));
    }
    Ok(ElevatedProcess(information.hProcess))
}
