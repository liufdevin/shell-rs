use super::{
    AdbConnection,
    android::{MirrorProcess, clean_output},
    process::CommandGroup,
};
use crate::i18n::t;
use anyhow::{Result, anyhow, bail};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

fn executable_name() -> &'static str {
    if cfg!(windows) {
        "scrcpy.exe"
    } else {
        "scrcpy"
    }
}

pub(super) fn validate_scrcpy(executable: &Path) -> Result<PathBuf> {
    if !executable
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case(executable_name()))
    {
        bail!(t!("android.scrcpy_invalid"));
    }
    if !executable.is_absolute() || !executable.is_file() {
        bail!(t!("android.scrcpy_missing"));
    }
    let executable = dunce::canonicalize(executable)?;
    #[cfg(windows)]
    if !executable
        .parent()
        .is_some_and(|folder| folder.join("scrcpy-server").is_file())
    {
        bail!(t!("android.scrcpy_incomplete"));
    }
    Ok(executable)
}

pub(super) fn find_scrcpy() -> Option<PathBuf> {
    let mut locations = Vec::new();
    if let Some(base) = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    {
        locations.extend([base.join("scrcpy"), base]);
    }
    if let Some(paths) = std::env::var_os("PATH") {
        locations.extend(std::env::split_paths(&paths));
    }
    locations
        .into_iter()
        .filter(|path| path.is_absolute())
        .find_map(|path| validate_scrcpy(&path.join(executable_name())).ok())
}

pub(super) struct ScrcpyProcess {
    child: Child,
    group: CommandGroup,
    log: File,
    ended: bool,
}

fn mirror_command(device: &AdbConnection, executable: &Path, adb: &Path, audio: bool) -> Command {
    let mut command = Command::new(executable);
    command
        .args(["--serial", device.serial(), "--window-title"])
        .arg(format!(
            "ShellLF · {} ({})",
            device.display_name(),
            device.serial()
        ))
        .env("ADB", adb)
        .stdin(Stdio::null());
    if !audio {
        command.arg("--no-audio");
    }
    if let Some(folder) = executable.parent() {
        command.current_dir(folder);
        let server = folder.join("scrcpy-server");
        if server.is_file() {
            command.env("SCRCPY_SERVER_PATH", server);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000);
    }
    command
}

impl ScrcpyProcess {
    pub(super) fn start(device: &AdbConnection, executable: &Path, audio: bool) -> Result<Self> {
        let executable = validate_scrcpy(executable)?;
        let adb = device
            .program()
            .and_then(|path| path.canonicalize().ok())
            .ok_or_else(|| anyhow!(t!("adb.missing")))?;
        let log = tempfile::tempfile()?;
        let mut command = mirror_command(device, &executable, &adb, audio);
        command
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log.try_clone()?));
        let (mut child, group) = CommandGroup::spawn(&mut command)?;
        if let Err(error) = group.terminate_on_close() {
            group.terminate();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.into());
        }
        Ok(Self {
            child,
            group,
            log,
            ended: false,
        })
    }
}
impl MirrorProcess for ScrcpyProcess {
    fn poll(&mut self) -> Result<bool> {
        if self.ended {
            return Ok(false);
        }
        if let Some(status) = self.child.try_wait()? {
            self.ended = true;
            if !status.success() {
                let size = self.log.metadata()?.len();
                self.log.seek(SeekFrom::Start(size.saturating_sub(8192)))?;
                let mut bytes = Vec::new();
                (&self.log).take(8192).read_to_end(&mut bytes)?;
                let output = clean_output(&String::from_utf8_lossy(&bytes));
                let error = if output.trim().is_empty() {
                    status.to_string()
                } else {
                    output
                };
                bail!(t!("android.failed", error = error));
            }
            return Ok(false);
        }
        Ok(true)
    }
    fn stop(&mut self) {
        if !self.ended {
            self.group.terminate();
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.ended = true;
        }
    }
}
impl Drop for ScrcpyProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn mirroring_selects_one_device_and_enables_control_with_audio_off_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("scrcpy.exe");
        let server = dir.path().join("scrcpy-server");
        std::fs::write(&server, b"server").unwrap();
        let adb = dir.path().join("platform tools").join("adb.exe");
        let device = AdbConnection::new(adb.clone(), "192.0.2.1:5555".into())
            .unwrap()
            .with_name("Helper A133");
        let command = mirror_command(&device, &executable, &adb, false);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert_eq!(
            args,
            [
                "--serial",
                "192.0.2.1:5555",
                "--window-title",
                "ShellLF · Helper A133 (192.0.2.1:5555)",
                "--no-audio",
            ]
        );
        assert_eq!(command.get_program(), executable.as_os_str());
        assert_eq!(command.get_current_dir(), Some(dir.path()));
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == OsStr::new("ADB") && value == Some(adb.as_os_str()))
        );
        assert!(command.get_envs().any(|(key, value)| {
            key == OsStr::new("SCRCPY_SERVER_PATH") && value == Some(server.as_os_str())
        }));
    }

    #[test]
    fn audio_option_enables_forwarding_without_disabling_device_control() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join(executable_name());
        let adb = dir.path().join("adb");
        let device = AdbConnection::new(adb.clone(), "board".into()).unwrap();
        let command = mirror_command(&device, &executable, &adb, true);
        assert!(
            !command
                .get_args()
                .any(|arg| arg == "--no-audio" || arg == "--no-control")
        );
        assert!(command.get_args().any(|arg| arg == "board"));
    }

    #[test]
    fn selecting_a_gui_or_script_is_rejected_before_spawning_any_process() {
        let dir = tempfile::tempdir().unwrap();
        let device = AdbConnection::new(dir.path().join("unused-adb"), "board".into()).unwrap();
        for name in [
            "Escrcpy.exe",
            "escrcpy",
            "scrcpy.bat",
            "scrcpy-noconsole.vbs",
            "scrcpy.lnk",
        ] {
            let executable = dir.path().join(name);
            std::fs::write(&executable, b"not a native scrcpy executable").unwrap();
            let error = match ScrcpyProcess::start(&device, &executable, false) {
                Ok(_) => panic!("a GUI or script was started instead of native scrcpy"),
                Err(error) => error,
            };
            assert_eq!(error.to_string(), t!("android.scrcpy_invalid").as_ref());
        }
    }

    #[test]
    fn native_scrcpy_uses_an_absolute_path_from_a_complete_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join(executable_name());
        std::fs::write(&executable, b"native scrcpy").unwrap();
        std::fs::write(dir.path().join("scrcpy-server"), b"server").unwrap();
        assert_eq!(
            validate_scrcpy(&executable).unwrap(),
            dunce::canonicalize(&executable).unwrap()
        );
        assert!(validate_scrcpy(Path::new(executable_name())).is_err());
        assert!(validate_scrcpy(&dir.path().join("missing").join(executable_name())).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_bundle_without_its_server_is_not_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("scrcpy.exe");
        std::fs::write(&executable, b"native scrcpy").unwrap();
        assert_eq!(
            validate_scrcpy(&executable).unwrap_err().to_string(),
            t!("android.scrcpy_incomplete").as_ref()
        );
    }
}
