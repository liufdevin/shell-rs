#[cfg(windows)]
use crate::i18n::t;

pub(super) fn check() -> Result<(), String> {
    let conflicts = conflict_names()?;
    if !conflicts.is_empty() {
        #[cfg(windows)]
        return Err(t!("flashing.tool.conflict", programs = conflicts.join(", ")).to_string());
    }
    Ok(())
}

pub(super) fn conflict_names() -> Result<Vec<String>, String> {
    #[cfg(windows)]
    {
        let entries = processes_with_pids()
            .map_err(|error| t!("flashing.tool.process_check_failed", error = error).to_string())?;
        let mut names: Vec<_> = entries
            .into_iter()
            .filter(|(_, name)| conflicting(name))
            .map(|(_, name)| name)
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }
    #[cfg(not(windows))]
    {
        Ok(Vec::new())
    }
}

pub(super) fn terminate_conflicts() -> Result<usize, String> {
    #[cfg(windows)]
    {
        use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_TERMINATE, TerminateProcess,
        };

        let entries = processes_with_pids()
            .map_err(|error| t!("flashing.tool.process_check_failed", error = error).to_string())?;
        let targets: Vec<_> = entries
            .into_iter()
            .filter(|(_, name)| conflicting(name))
            .collect();
        if targets.is_empty() {
            return Ok(0);
        }
        let mut terminated = 0usize;
        let mut errors = Vec::new();
        for (pid, name) in targets {
            let raw = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
            if raw.is_null() {
                errors.push(format!(
                    "{name} ({pid}): {}",
                    std::io::Error::last_os_error()
                ));
                continue;
            }
            let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
            if unsafe { TerminateProcess(handle.as_raw_handle(), 1) } != 0 {
                terminated += 1;
            } else {
                errors.push(format!(
                    "{name} ({pid}): {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
        if terminated == 0 && !errors.is_empty() {
            return Err(errors.join("; "));
        }
        Ok(terminated)
    }
    #[cfg(not(windows))]
    {
        Ok(0)
    }
}

#[cfg(any(windows, test))]
fn conflicting(name: &str) -> bool {
    [
        "PhoenixSuit.exe",
        "LiveSuit.exe",
        "PhoenixUSBPro.exe",
        "PhoenixConsole.exe",
    ]
    .iter()
    .any(|candidate| name.eq_ignore_ascii_case(candidate))
        || (name.to_ascii_lowercase().starts_with(".shelllf-console-")
            && name.to_ascii_lowercase().ends_with(".exe"))
}

#[cfg(windows)]
fn processes_with_pids() -> Result<Vec<(u32, String)>, std::io::Error> {
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use windows_sys::Win32::{
        Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE},
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
    };
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut present = unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) };
    let mut result = Vec::new();
    while present != 0 {
        if entry.th32ProcessID != std::process::id() {
            let end = entry
                .szExeFile
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(entry.szExeFile.len());
            result.push((
                entry.th32ProcessID,
                String::from_utf16_lossy(&entry.szExeFile[..end]),
            ));
        }
        present = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) };
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
        return Err(error);
    }
    Ok(result)
}

#[cfg(all(windows, test))]
fn processes() -> Result<Vec<String>, std::io::Error> {
    let mut names: Vec<_> = processes_with_pids()?
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allwinner_flashers_conflict_but_shared_adb_and_unrelated_tools_do_not() {
        for name in [
            "PhoenixSuit.exe",
            "phoenixsuit.EXE",
            "LiveSuit.exe",
            "PhoenixUSBPro.exe",
            "PhoenixConsole.exe",
            ".shelllf-console-private.exe",
        ] {
            assert!(conflicting(name));
        }
        for name in [
            "adb.exe",
            "ShellLF.exe",
            "shelllf-cli.exe",
            "PhoenixSuit.exe.backup",
            "explorer.exe",
        ] {
            assert!(!conflicting(name));
        }
    }

    #[cfg(windows)]
    #[test]
    fn process_snapshot_is_read_only_and_does_not_require_elevation() {
        assert!(!processes().unwrap().is_empty());
    }
}
