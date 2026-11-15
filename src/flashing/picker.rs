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
    let mut path = vec![0u16; 32768];
    if let Some(initial) = initial {
        let initial: Vec<_> = initial.as_os_str().encode_wide().collect();
        if initial.len() < path.len() {
            path[..initial.len()].copy_from_slice(&initial);
        }
    }
    let mut options = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: unsafe { GetActiveWindow() },
        lpstrFilter: filter.as_ptr(),
        nFilterIndex: 1,
        lpstrFile: path.as_mut_ptr(),
        nMaxFile: path.len() as u32,
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
