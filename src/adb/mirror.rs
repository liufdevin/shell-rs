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

pub(super) fn find_scrcpy() -> Option<PathBuf> {
    let executable = if cfg!(windows) {
        "scrcpy.exe"
    } else {
        "scrcpy"
    };
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
        .map(|path| path.join(executable))
        .find(|path| path.is_file())
}

pub(super) struct ScrcpyProcess {
    child: Child,
    group: CommandGroup,
    log: File,
    ended: bool,
}

fn mirror_command(device: &AdbConnection, executable: &Path, adb: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .args([
            "--serial",
            device.serial(),
            "--no-control",
            "--no-audio",
            "--window-title",
        ])
        .arg(format!(
            "ShellLF · {} ({})",
            device.display_name(),
            device.serial()
        ))
        .env("ADB", adb)
        .stdin(Stdio::null());
    if let Some(folder) = executable.parent() {
        command.current_dir(folder);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000);
    }
    command
}

impl ScrcpyProcess {
    pub(super) fn start(device: &AdbConnection, executable: &Path) -> Result<Self> {
        if !executable.is_absolute() || !executable.is_file() {
            bail!(t!("android.scrcpy_missing"));
        }
        let adb = device
            .program()
            .and_then(|path| path.canonicalize().ok())
            .ok_or_else(|| anyhow!(t!("adb.missing")))?;
        let log = tempfile::tempfile()?;
        let mut command = mirror_command(device, executable, &adb);
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
    fn mirroring_selects_one_device_and_reuses_the_existing_adb_without_control_or_audio() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("scrcpy.exe");
        let adb = dir.path().join("platform tools").join("adb.exe");
        let device = AdbConnection::new(adb.clone(), "192.0.2.1:5555".into())
            .unwrap()
            .with_name("Helper A133");
        let command = mirror_command(&device, &executable, &adb);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert_eq!(
            args,
            [
                "--serial",
                "192.0.2.1:5555",
                "--no-control",
                "--no-audio",
                "--window-title",
                "ShellLF · Helper A133 (192.0.2.1:5555)",
            ]
        );
        assert_eq!(command.get_program(), executable.as_os_str());
        assert_eq!(command.get_current_dir(), Some(dir.path()));
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == OsStr::new("ADB") && value == Some(adb.as_os_str()))
        );
    }
}
