#[cfg(any(windows, test))]
use std::collections::BTreeMap;
use std::sync::Arc;

use gpui_kit::{Context, SharedString, Task};

use crate::i18n::t;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DriverReport {
    Installed(Vec<String>),
    OtherDriver(Vec<String>),
    Missing,
}

type Detector = Arc<dyn Fn() -> Result<DriverReport, String> + Send + Sync>;

pub struct DriverCheck {
    detector: Detector,
    result: Option<Result<DriverReport, String>>,
    task: Option<Task<()>>,
}

impl Default for DriverCheck {
    fn default() -> Self {
        Self {
            detector: Arc::new(detect),
            result: None,
            task: None,
        }
    }
}

impl DriverCheck {
    pub fn set_detector(&mut self, detector: Detector, cx: &mut Context<Self>) {
        self.task = None;
        self.detector = detector;
        self.result = None;
        cx.notify();
    }

    pub fn is_checking(&self) -> bool {
        self.task.is_some()
    }

    pub fn message(&self) -> SharedString {
        if self.is_checking() {
            return t!("flashing.driver.checking");
        }
        match &self.result {
            None => t!("flashing.driver.description"),
            Some(Ok(DriverReport::Installed(_))) => t!("flashing.driver.installed"),
            Some(Ok(DriverReport::OtherDriver(_) | DriverReport::Missing)) => {
                t!("flashing.driver.missing")
            }
            Some(Err(_)) => t!("flashing.driver.failed"),
        }
    }

    pub fn check(&mut self, cx: &mut Context<Self>) {
        if self.is_checking() {
            return;
        }
        let detector = self.detector.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        if let Err(error) = std::thread::Builder::new()
            .name("flashing-driver-check".into())
            .spawn(move || {
                let _ = sender.send(detector());
            })
        {
            self.result = Some(Err(error.to_string()));
            cx.notify();
            return;
        }
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = loop {
                match receiver.try_recv() {
                    Ok(result) => break result,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        break Err(t!("flashing.driver.worker_stopped").to_string());
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(50))
                            .await;
                    }
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.result = Some(result);
                this.task = None;
                cx.notify();
            });
        }));
        cx.notify();
    }
}

#[cfg(not(windows))]
fn detect() -> Result<DriverReport, String> {
    Err(t!("flashing.driver.windows_only").to_string())
}

#[cfg(windows)]
fn detect() -> Result<DriverReport, String> {
    use windows_sys::Win32::System::SystemInformation::{
        GetNativeSystemInfo, PROCESSOR_ARCHITECTURE_AMD64, PROCESSOR_ARCHITECTURE_ARM64,
        PROCESSOR_ARCHITECTURE_INTEL, SYSTEM_INFO,
    };
    let mut system = SYSTEM_INFO::default();
    unsafe { GetNativeSystemInfo(&mut system) };
    let architecture = match unsafe { system.Anonymous.Anonymous.wProcessorArchitecture } {
        PROCESSOR_ARCHITECTURE_AMD64 => "x86_64",
        PROCESSOR_ARCHITECTURE_ARM64 => "aarch64",
        PROCESSOR_ARCHITECTURE_INTEL => "x86",
        _ => return Err("Unsupported Windows processor architecture".into()),
    };
    let mut installed = Vec::new();
    let mut other = Vec::new();
    let mut unreadable = None;
    for path in installed_inf_paths()? {
        let bytes = match std::fs::read(&path) {
            Ok(bytes) if bytes.len() <= 4 * 1024 * 1024 => bytes,
            Ok(_) => {
                unreadable = Some(format!("INF exceeds size limit: {}", path.display()));
                continue;
            }
            Err(error) => {
                unreadable = Some(format!("{}: {error}", path.display()));
                continue;
            }
        };
        let package = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        match inf_driver(&bytes, architecture) {
            Some(true) => installed.push(package),
            Some(false) => other.push(package),
            None => {}
        }
    }
    installed.sort();
    other.sort();
    if !installed.is_empty() {
        Ok(DriverReport::Installed(installed))
    } else if let Some(error) = unreadable {
        Err(error)
    } else if !other.is_empty() {
        Ok(DriverReport::OtherDriver(other))
    } else {
        Ok(DriverReport::Missing)
    }
}

#[cfg(windows)]
fn installed_inf_paths() -> Result<Vec<std::path::PathBuf>, String> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt as _};
    use windows_sys::Win32::{
        Devices::DeviceAndDriverInstallation::{INF_STYLE_WIN4, SetupGetInfFileListW},
        Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError},
        System::SystemInformation::GetWindowsDirectoryW,
    };
    let mut directory = vec![0; 32768];
    let length = unsafe { GetWindowsDirectoryW(directory.as_mut_ptr(), directory.len() as u32) };
    if length == 0 || length as usize >= directory.len() {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let directory =
        std::path::PathBuf::from(OsString::from_wide(&directory[..length as usize])).join("INF");
    let mut required = 0;
    unsafe {
        SetupGetInfFileListW(
            std::ptr::null(),
            INF_STYLE_WIN4,
            std::ptr::null_mut(),
            0,
            &mut required,
        );
    }
    for _ in 0..3 {
        if required == 0 || required > 16 * 1024 * 1024 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let mut buffer = vec![0; required as usize];
        if unsafe {
            SetupGetInfFileListW(
                std::ptr::null(),
                INF_STYLE_WIN4,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                &mut required,
            )
        } != 0
        {
            return Ok(buffer
                .split(|value| *value == 0)
                .filter(|name| !name.is_empty())
                .map(|name| directory.join(OsString::from_wide(name)))
                .collect());
        }
        if unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    Err("The installed driver list changed during detection".into())
}

#[cfg(any(windows, test))]
fn inf_driver(bytes: &[u8], architecture: &str) -> Option<bool> {
    let text = if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        let big_endian = bytes[0] == 0xfe;
        String::from_utf16_lossy(
            &bytes[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| {
                    if big_endian {
                        u16::from_be_bytes([pair[0], pair[1]])
                    } else {
                        u16::from_le_bytes([pair[0], pair[1]])
                    }
                })
                .collect::<Vec<_>>(),
        )
    } else {
        String::from_utf8_lossy(bytes)
            .trim_start_matches('\u{feff}')
            .to_string()
    };
    let mut sections: BTreeMap<String, Vec<(String, Vec<String>)>> = BTreeMap::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            section = name.to_owned();
        } else if let Some((key, value)) = line.split_once('=') {
            sections.entry(section.clone()).or_default().push((
                key.trim().to_string(),
                value
                    .split(',')
                    .map(|field| field.trim().trim_matches('"').to_string())
                    .collect(),
            ));
        }
    }
    let architecture = match architecture {
        "x86_64" => "ntamd64",
        "aarch64" => "ntarm64",
        "x86" => "ntx86",
        _ => return None,
    };
    let mut matched = false;
    for (_, manufacturer) in sections.get("manufacturer")? {
        let base = manufacturer.first()?;
        let specific: Vec<_> = manufacturer
            .iter()
            .skip(1)
            .filter(|suffix| {
                suffix.as_str() == architecture || suffix.starts_with(&format!("{architecture}."))
            })
            .collect();
        let models: Vec<_> = if !specific.is_empty() {
            specific
                .into_iter()
                .map(|suffix| format!("{base}.{suffix}"))
                .collect()
        } else if manufacturer.len() == 1 {
            vec![base.clone()]
        } else if manufacturer.iter().skip(1).any(|suffix| suffix == "nt") {
            vec![format!("{base}.nt")]
        } else {
            continue;
        };
        for model in models
            .iter()
            .filter_map(|name| sections.get(name))
            .flatten()
        {
            if !model.1.iter().skip(1).any(|id| {
                id == r"usb\vid_1f3a&pid_efe8"
                    || id
                        .strip_prefix(r"usb\vid_1f3a&pid_efe8&rev_")
                        .is_some_and(|revision| {
                            revision.len() == 4
                                && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
                        })
            }) {
                continue;
            }
            matched = true;
            let install = &model.1[0];
            let services = [
                format!("{install}.{architecture}.services"),
                format!("{install}.nt.services"),
                format!("{install}.services"),
            ]
            .into_iter()
            .find_map(|name| sections.get(&name));
            for (_, fields) in services
                .into_iter()
                .flatten()
                .filter(|(key, _)| key == "addservice")
            {
                if fields.first().is_none_or(|name| name != "usbudisc") {
                    continue;
                }
                if let Some(service) = fields.get(2).and_then(|name| sections.get(name))
                    && service.iter().any(|(key, fields)| {
                        key == "servicebinary"
                            && fields.first().is_some_and(|binary| {
                                binary.rsplit(['\\', '/']).next().is_some_and(|name| {
                                    name.starts_with("usbdrv") && name.ends_with(".sys")
                                })
                            })
                    })
                {
                    return Some(true);
                }
            }
        }
    }
    matched.then_some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INF: &str = "[Version]\nSignature=\"$Windows NT$\"\n[Manufacturer]\n%vendor%=Vendor,NTx86,NTamd64\n[Vendor.NTamd64]\n%device%=Install, USB\\VID_1F3A&PID_EFE8\n[Vendor.NTx86]\n%device%=Install32, USB\\VID_1F3A&PID_EFE8\n[Install.Services]\nAddService=usbUDisc,2,Service\n[Service]\nServiceBinary=%12%\\USBDrv_AMD64.sys\n";

    #[test]
    fn result_messages_only_show_detection_status_without_package_or_error_details() {
        crate::i18n::isolate_thread();
        let mut check = DriverCheck::default();
        for (locale, installed, missing, failed) in [
            (
                "zh-CN",
                "已检测到全志 USBDrv 烧录驱动包",
                "未检测到全志 USBDrv 烧录驱动包",
                "驱动检测失败，请重新检测",
            ),
            (
                "en",
                "Allwinner USBDrv driver package detected",
                "Allwinner USBDrv driver package not detected",
                "Driver check failed. Try again.",
            ),
        ] {
            crate::i18n::set_locale(locale);
            for (result, expected) in [
                (
                    Ok(DriverReport::Installed(vec![
                        "oem42.inf".into(),
                        "oem60.inf".into(),
                    ])),
                    installed,
                ),
                (
                    Ok(DriverReport::OtherDriver(vec!["oem43.inf".into()])),
                    missing,
                ),
                (Ok(DriverReport::Missing), missing),
                (
                    Err("permission denied: private diagnostic details".into()),
                    failed,
                ),
            ] {
                check.result = Some(result);
                assert_eq!(check.message().as_ref(), expected);
            }
        }
    }

    #[test]
    fn detects_the_phoenix_driver_without_a_connected_board() {
        assert_eq!(inf_driver(INF.as_bytes(), "x86_64"), Some(true));
        let utf16: Vec<_> = [0xff, 0xfe]
            .into_iter()
            .chain(INF.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(inf_driver(&utf16, "x86_64"), Some(true));
        let utf16: Vec<_> = [0xfe, 0xff]
            .into_iter()
            .chain(INF.encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        assert_eq!(inf_driver(&utf16, "x86_64"), Some(true));
    }

    #[test]
    fn rejects_adb_strings_comments_partial_ids_and_other_architectures() {
        for text in [
            INF.replace("PID_EFE8", "PID_1007"),
            INF.replace("PID_EFE8", "PID_EFE80"),
            INF.replace("[Vendor.NTamd64]", "[Strings]"),
            INF.replace("%device%=Install,", "; %device%=Install,"),
        ] {
            assert_eq!(inf_driver(text.as_bytes(), "x86_64"), None);
        }
        assert_eq!(inf_driver(INF.as_bytes(), "aarch64"), None);
        assert_eq!(inf_driver(INF.as_bytes(), "x86"), Some(false));
    }

    #[test]
    fn distinguishes_a_generic_fel_driver_from_the_phoenix_usb_driver() {
        assert_eq!(
            inf_driver(INF.replace("usbUDisc", "WinUSB").as_bytes(), "x86_64"),
            Some(false)
        );
        assert_eq!(
            inf_driver(
                INF.replace("USBDrv_AMD64.sys", "missing.sys").as_bytes(),
                "x86_64"
            ),
            Some(false)
        );
    }

    #[cfg(windows)]
    #[test]
    fn system_inventory_only_lists_installed_windows_inf_files() {
        let paths = installed_inf_paths().expect("Windows installed driver inventory");
        assert!(!paths.is_empty());
        assert!(paths.iter().all(|path| {
            path.is_absolute()
                && path
                    .parent()
                    .and_then(|parent| parent.file_name())
                    .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("INF"))
                && path.extension().is_some_and(|extension| {
                    extension.to_string_lossy().eq_ignore_ascii_case("inf")
                })
        }));
    }
}
