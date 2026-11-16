#[cfg(windows)]
use crate::i18n::t;

pub(super) fn check() -> Result<(), String> {
    #[cfg(windows)]
    {
        let processes = processes()
            .map_err(|error| t!("flashing.tool.process_check_failed", error = error).to_string())?;
        let conflicts: Vec<_> = processes
            .into_iter()
            .filter(|name| conflicting(name))
            .collect();
        if !conflicts.is_empty() {
            return Err(t!("flashing.tool.conflict", programs = conflicts.join(", ")).to_string());
        }
    }
    Ok(())
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
fn processes() -> Result<Vec<String>, std::io::Error> {
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
    let mut names = Vec::new();
    while present != 0 {
        if entry.th32ProcessID != std::process::id() {
            let end = entry
                .szExeFile
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(entry.szExeFile.len());
            names.push(String::from_utf16_lossy(&entry.szExeFile[..end]));
        }
        present = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) };
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
        return Err(error);
    }
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
