use std::path::PathBuf;

use gpui_kit::{App, Task};

use crate::i18n::t;

pub(super) fn choose_firmware(
    initial: Option<PathBuf>,
    cx: &mut App,
) -> Task<Result<Option<PathBuf>, String>> {
    #[cfg(all(windows, not(test)))]
    {
        let title = t!("flashing.choose").to_string();
        let filter = t!("flashing.image_filter").to_string();
        cx.spawn(async move |_| pick_image(&title, &filter, initial.as_deref()))
    }
    #[cfg(any(not(windows), test))]
    {
        let _ = initial;
        let picked = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(t!("flashing.choose")),
        });
        cx.spawn(async move |_| {
            picked
                .await
                .map_err(|error| error.to_string())?
                .map(|paths| paths.and_then(|paths| paths.into_iter().next()))
                .map_err(|error| error.to_string())
        })
    }
}

#[cfg(any(all(windows, not(test)), test))]
fn initial_directory(initial: Option<&std::path::Path>) -> Option<PathBuf> {
    let initial = initial?;
    let dir = if initial.is_dir() {
        initial
    } else {
        initial.parent()?
    };
    (!dir.as_os_str().is_empty()).then(|| dir.to_path_buf())
}

#[cfg(any(all(windows, not(test)), test))]
fn initial_file_name(initial: Option<&std::path::Path>) -> Option<std::ffi::OsString> {
    let initial = initial?;
    if initial.is_dir() {
        return None;
    }
    let name = initial.file_name()?;
    (!name.is_empty()).then(|| name.to_os_string())
}

#[cfg(all(windows, not(test)))]
mod filename_scroll_fix {
    use std::cell::{Cell, RefCell};

    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::Threading::GetCurrentThreadId,
        UI::WindowsAndMessaging::{
            CWPRETSTRUCT, CallNextHookEx, GetClassNameW, GetWindowTextLengthW, GetWindowTextW,
            HHOOK, SendMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_CALLWNDPROCRET,
            WM_SETFOCUS, WM_SIZE,
        },
    };

    const EM_GETSEL: u32 = 0x00B0;
    const EM_SETSEL: u32 = 0x00B1;

    thread_local! {
        static EXPECTED_NAME: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
        static IN_RESET: Cell<bool> = const { Cell::new(false) };
    }

    /// Resets the Win32 file dialog's filename `Edit` control horizontal scroll
    /// offset after Windows populates/resizes it. Without this, Win10/Win11's
    /// Common Item Dialog selects the pre-filled filename while the Edit control
    /// is still at its narrow initial template width, scrolling the leading part
    /// of the filename off-screen to the left even after the dialog expands.
    pub(super) struct Guard {
        hook: HHOOK,
    }

    impl Guard {
        pub(super) fn install(file_name: &[u16]) -> Option<Self> {
            if file_name.is_empty() {
                return None;
            }
            EXPECTED_NAME.with(|slot| {
                let mut slot = slot.borrow_mut();
                slot.clear();
                slot.extend_from_slice(file_name);
            });
            let hook = unsafe {
                SetWindowsHookExW(
                    WH_CALLWNDPROCRET,
                    Some(hook_proc),
                    std::ptr::null_mut(),
                    GetCurrentThreadId(),
                )
            };
            if hook.is_null() {
                EXPECTED_NAME.with(|slot| slot.borrow_mut().clear());
                None
            } else {
                Some(Self { hook })
            }
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe {
                UnhookWindowsHookEx(self.hook);
            }
            EXPECTED_NAME.with(|slot| slot.borrow_mut().clear());
            IN_RESET.with(|flag| flag.set(false));
        }
    }

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 && lparam != 0 {
            let info = unsafe { &*(lparam as *const CWPRETSTRUCT) };
            if matches!(info.message, WM_SIZE | WM_SETFOCUS | EM_SETSEL) {
                maybe_reset_scroll(info.hwnd);
            }
        }
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }

    fn maybe_reset_scroll(hwnd: HWND) {
        if hwnd.is_null() || IN_RESET.with(Cell::get) {
            return;
        }
        let mut class_name = [0u16; 16];
        let class_len =
            unsafe { GetClassNameW(hwnd, class_name.as_mut_ptr(), class_name.len() as i32) };
        const EDIT_CLASS: [u16; 4] = [b'E' as u16, b'd' as u16, b'i' as u16, b't' as u16];
        if class_len != EDIT_CLASS.len() as i32 || class_name[..4] != EDIT_CLASS {
            return;
        }
        let expected_len = EXPECTED_NAME.with(|slot| slot.borrow().len());
        if expected_len == 0 {
            return;
        }
        let text_len = unsafe { GetWindowTextLengthW(hwnd) };
        if text_len <= 0 || text_len as usize != expected_len {
            return;
        }
        let mut actual = vec![0u16; expected_len + 1];
        let copied = unsafe { GetWindowTextW(hwnd, actual.as_mut_ptr(), actual.len() as i32) };
        if copied as usize != expected_len {
            return;
        }
        let matches =
            EXPECTED_NAME.with(|slot| slot.borrow().as_slice() == &actual[..expected_len]);
        if !matches {
            return;
        }
        let mut start: u32 = 0;
        let mut end: u32 = 0;
        unsafe {
            SendMessageW(
                hwnd,
                EM_GETSEL,
                (&mut start as *mut u32) as WPARAM,
                (&mut end as *mut u32) as LPARAM,
            );
        }
        if start != 0 || end as usize != expected_len {
            return;
        }
        IN_RESET.with(|flag| flag.set(true));
        unsafe {
            // Move caret to 0 first to reset the horizontal scroll offset, then
            // re-select the full filename with the active end (caret) at 0 so
            // the start of the filename stays visible.
            SendMessageW(hwnd, EM_SETSEL, 0, 0);
            SendMessageW(hwnd, EM_SETSEL, expected_len, 0);
        }
        IN_RESET.with(|flag| flag.set(false));
    }
}

#[cfg(all(windows, not(test)))]
fn pick_image(
    title: &str,
    filter: &str,
    initial: Option<&std::path::Path>,
) -> Result<Option<PathBuf>, String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
    use windows_sys::Win32::UI::{
        Controls::Dialogs::{
            CommDlgExtendedError, GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST,
            OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
        },
        Input::KeyboardAndMouse::GetActiveWindow,
    };

    let title: Vec<_> = title.encode_utf16().chain([0]).collect();
    let filter: Vec<_> = format!("{filter}\0*.img\0\0").encode_utf16().collect();
    let extension: Vec<_> = "img\0".encode_utf16().collect();
    let initial_dir: Option<Vec<u16>> =
        initial_directory(initial).map(|dir| dir.as_os_str().encode_wide().chain([0]).collect());
    let initial_name: Option<Vec<u16>> =
        initial_file_name(initial).map(|name| name.encode_wide().collect());
    let mut path = vec![0u16; 32768];
    if let Some(name) = &initial_name
        && name.len() < path.len()
    {
        path[..name.len()].copy_from_slice(name);
    }
    let _scroll_guard = initial_name
        .as_deref()
        .and_then(filename_scroll_fix::Guard::install);
    let mut options = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: unsafe { GetActiveWindow() },
        lpstrFilter: filter.as_ptr(),
        nFilterIndex: 1,
        lpstrFile: path.as_mut_ptr(),
        nMaxFile: path.len() as u32,
        lpstrInitialDir: initial_dir
            .as_ref()
            .map_or(std::ptr::null(), |dir| dir.as_ptr()),
        lpstrTitle: title.as_ptr(),
        lpstrDefExt: extension.as_ptr(),
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..unsafe { std::mem::zeroed() }
    };
    if unsafe { GetOpenFileNameW(&mut options) } != 0 {
        let length = path
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(path.len());
        return Ok(Some(PathBuf::from(OsString::from_wide(&path[..length]))));
    }
    let error = unsafe { CommDlgExtendedError() };
    if error == 0 {
        Ok(None)
    } else {
        Err(format!("Windows file dialog error: {error:#x}"))
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::{initial_directory, initial_file_name};

    #[test]
    fn initial_directory_uses_parent_for_file_paths() {
        let path = Path::new(r"\\192.168.0.100\share\a133_android10_c4_uart0.img");
        assert_eq!(
            initial_directory(Some(path)),
            Some(PathBuf::from(r"\\192.168.0.100\share"))
        );
    }

    #[test]
    fn initial_directory_ignores_bare_filename_and_none() {
        assert_eq!(initial_directory(None), None);
        assert_eq!(initial_directory(Some(Path::new("firmware.img"))), None);
    }

    #[test]
    fn initial_directory_keeps_existing_directory() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(
            initial_directory(Some(temp.path())),
            Some(temp.path().to_path_buf())
        );
    }

    #[test]
    fn initial_file_name_extracts_only_file_name() {
        let path = Path::new(r"\\bj34lf\home\bj60yxx\out\t153_linux_bga_demo_uart0.img");
        assert_eq!(
            initial_file_name(Some(path)),
            Some(OsString::from("t153_linux_bga_demo_uart0.img"))
        );
        assert_eq!(initial_file_name(None), None);
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(initial_file_name(Some(temp.path())), None);
    }
}
