use super::{AdbConnection, client::quote};
use crate::i18n::t;
use anyhow::{Result, anyhow, bail};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AndroidCapabilities {
    sdk: u32,
    install: bool,
    launch: bool,
    mirror: bool,
}
impl AndroidCapabilities {
    pub fn new(sdk: u32, install: bool, launch: bool, mirror: bool) -> Self {
        Self {
            sdk,
            install,
            launch,
            mirror: mirror && sdk >= 21,
        }
    }
    pub fn sdk(&self) -> u32 {
        self.sdk
    }
    pub fn is_installable(&self) -> bool {
        self.install
    }
    pub fn is_launchable(&self) -> bool {
        self.launch
    }
    pub fn is_mirrorable(&self) -> bool {
        self.mirror
    }
    pub fn is_supported(&self) -> bool {
        self.install || self.launch || self.mirror
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndroidApp {
    package: String,
}
impl AndroidApp {
    pub fn new(package: &str) -> Result<Self> {
        validate_package(package)?;
        Ok(Self {
            package: package.into(),
        })
    }
    pub fn package(&self) -> &str {
        &self.package
    }
    pub(super) fn package_ref(&self) -> &String {
        &self.package
    }
}

pub trait MirrorProcess: Send {
    fn poll(&mut self) -> Result<bool>;
    fn stop(&mut self);
}

pub trait AndroidTools: Send + Sync + 'static {
    fn probe(&self, device: &AdbConnection, cancel: &AtomicBool) -> Result<AndroidCapabilities>;
    fn apps(&self, device: &AdbConnection, cancel: &AtomicBool) -> Result<Vec<AndroidApp>>;
    fn install(&self, device: &AdbConnection, apk: &Path, cancel: &AtomicBool) -> Result<()>;
    fn launch(&self, device: &AdbConnection, package: &str, cancel: &AtomicBool) -> Result<()>;
    fn mirror(
        &self,
        device: &AdbConnection,
        program: &Path,
        audio: bool,
        cancel: &AtomicBool,
    ) -> Result<Box<dyn MirrorProcess>>;
    fn scrcpy(&self) -> Option<PathBuf> {
        super::mirror::find_scrcpy()
    }
}

pub struct SystemAndroidTools;

fn verify(device: &AdbConnection, cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        bail!(t!("adb.interrupted"));
    }
    device.verify(&mut || cancel.load(Ordering::Acquire))
}

impl AndroidTools for SystemAndroidTools {
    fn probe(&self, device: &AdbConnection, cancel: &AtomicBool) -> Result<AndroidCapabilities> {
        verify(device, cancel)?;
        let output = device.shell(
            "sdk=$(getprop ro.build.version.sdk 2>/dev/null); printf 'sdk=%s\\n' \"$sdk\"; if command -v pm >/dev/null 2>&1 && pm path android >/dev/null 2>&1; then printf 'pm=1\\n'; fi; if command -v am >/dev/null 2>&1; then printf 'am=1\\n'; fi; if command -v app_process >/dev/null 2>&1; then printf 'mirror=1\\n'; fi; :",
            &mut || cancel.load(Ordering::Acquire))?;
        Ok(parse_capabilities(&String::from_utf8(output)?))
    }

    fn apps(&self, device: &AdbConnection, cancel: &AtomicBool) -> Result<Vec<AndroidApp>> {
        verify(device, cancel)?;
        let output = device.shell("pm list packages --user current", &mut || {
            cancel.load(Ordering::Acquire)
        })?;
        parse_apps(&String::from_utf8(output)?)
    }

    fn install(&self, device: &AdbConnection, apk: &Path, cancel: &AtomicBool) -> Result<()> {
        if apk
            .extension()
            .is_none_or(|value| !value.eq_ignore_ascii_case("apk"))
            || !apk.is_file()
        {
            bail!(t!("android.invalid_apk"));
        }
        let apk = apk.canonicalize()?;
        verify(device, cancel)?;
        let output = device.run(
            &[OsStr::new("install"), OsStr::new("-r"), apk.as_os_str()],
            Duration::from_secs(300),
            &mut || cancel.load(Ordering::Acquire),
        )?;
        check_install(&String::from_utf8(output)?)
    }
    fn launch(&self, device: &AdbConnection, package: &str, cancel: &AtomicBool) -> Result<()> {
        validate_package(package)?;
        verify(device, cancel)?;
        let quoted = quote(package)?;
        let output = device.shell(&format!(
            "component=$(cmd package resolve-activity --brief --user current -a android.intent.action.MAIN -c android.intent.category.LAUNCHER -p {quoted} 2>/dev/null); case \"$component\" in */*) printf '%s\\n' \"$component\";; *) pm resolve-activity --brief --user current -a android.intent.action.MAIN -c android.intent.category.LAUNCHER -p {quoted};; esac"),
            &mut || cancel.load(Ordering::Acquire))?;
        let output = String::from_utf8(output)?;
        let component = output
            .lines()
            .rev()
            .find_map(|line| {
                let line = line.trim();
                let (app, activity) = line.split_once('/')?;
                (app == package && valid_activity(activity)).then_some(line)
            })
            .ok_or_else(|| anyhow!(t!("android.no_launcher")))?;
        let output = device.shell(&format!("am start -W --user current -a android.intent.action.MAIN -c android.intent.category.LAUNCHER -n {}", quote(component)?),
            &mut || cancel.load(Ordering::Acquire))?;
        check_launch(&String::from_utf8(output)?)
    }

    fn mirror(
        &self,
        device: &AdbConnection,
        program: &Path,
        audio: bool,
        cancel: &AtomicBool,
    ) -> Result<Box<dyn MirrorProcess>> {
        verify(device, cancel)?;
        Ok(Box::new(super::mirror::ScrcpyProcess::start(
            device, program, audio,
        )?))
    }
}

fn parse_apps(output: &str) -> Result<Vec<AndroidApp>> {
    let mut apps = Vec::new();
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Some(package) = line.strip_prefix("package:") else {
            bail!(t!("android.failed", error = clean_output(output)));
        };
        apps.push(AndroidApp::new(package)?);
        if apps.len() > 10000 {
            bail!(t!("adb.limit"));
        }
    }
    apps.sort_by(|left, right| left.package.cmp(&right.package));
    apps.dedup_by(|left, right| left.package == right.package);
    Ok(apps)
}

fn validate_package(package: &str) -> Result<()> {
    if package.len() > 255
        || package.is_empty()
        || !package.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .next()
                    .is_some_and(|value| value.is_ascii_alphabetic() || value == '_')
                && part
                    .chars()
                    .all(|value| value.is_ascii_alphanumeric() || value == '_')
        })
    {
        bail!(t!("android.invalid_package"));
    }
    Ok(())
}
fn valid_activity(activity: &str) -> bool {
    !activity.is_empty()
        && activity.len() <= 512
        && activity
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '.' | '_' | '$'))
}
fn parse_capabilities(output: &str) -> AndroidCapabilities {
    let sdk = output
        .lines()
        .find_map(|line| line.strip_prefix("sdk=")?.parse::<u32>().ok())
        .filter(|sdk| *sdk > 0 && *sdk < 1000)
        .unwrap_or(0);
    if sdk == 0 || !output.lines().any(|line| line == "pm=1") {
        return AndroidCapabilities::default();
    }
    AndroidCapabilities::new(
        sdk,
        true,
        output.lines().any(|line| line == "am=1"),
        output.lines().any(|line| line == "mirror=1"),
    )
}
fn check_install(output: &str) -> Result<()> {
    if output
        .lines()
        .any(|line| line.contains("Failure") || line.starts_with("Error"))
        || !output.lines().any(|line| line.trim() == "Success")
    {
        bail!(t!("android.failed", error = clean_output(output)));
    }
    Ok(())
}
fn check_launch(output: &str) -> Result<()> {
    if output.lines().any(|line| {
        line.trim_start().starts_with("Error")
            || line.contains("Exception")
            || line.contains("Permission Denial")
    }) || !output.lines().any(|line| line.trim() == "Status: ok")
    {
        bail!(t!("android.failed", error = clean_output(output)));
    }
    Ok(())
}
pub(super) fn clean_output(output: &str) -> String {
    output
        .chars()
        .filter(|value| !value.is_control() || matches!(value, '\n' | '\t'))
        .take(2500)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adb::{client::Runner, test_support::BOARD_BOOT};
    use std::{
        collections::VecDeque,
        ffi::OsString,
        sync::{Arc, Mutex},
    };

    struct Replies {
        output: Mutex<VecDeque<String>>,
        requests: Mutex<Vec<Vec<OsString>>>,
    }

    impl Runner for Replies {
        fn run(
            &self,
            args: &[OsString],
            _: Duration,
            _: &mut dyn FnMut() -> bool,
        ) -> Result<Vec<u8>> {
            self.requests.lock().unwrap().push(args.to_vec());
            let script = args.last().unwrap().to_str().unwrap();
            if script == "cat /proc/sys/kernel/random/boot_id" {
                return Ok(format!("{BOARD_BOOT}\n").into_bytes());
            }
            let output = self
                .output
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected ADB request");
            if args[2] == "install" {
                return Ok(output.into_bytes());
            }
            let marker = script
                .rsplit_once("printf '\\000")
                .unwrap()
                .1
                .split("%s")
                .next()
                .unwrap();
            Ok(format!("{output}\0{marker}0\n").into_bytes())
        }
    }

    fn device(outputs: &[&str]) -> (AdbConnection, Arc<Replies>) {
        let runner = Arc::new(Replies {
            output: Mutex::new(outputs.iter().map(|output| output.to_string()).collect()),
            requests: Mutex::default(),
        });
        (AdbConnection::test_connection(runner.clone()), runner)
    }

    #[test]
    fn ordinary_linux_and_incomplete_android_do_not_show_actions() {
        for output in ["", "sdk=\npm=1\nam=1", "sdk=35\nam=1", "sdk=bad\npm=1"] {
            assert!(!parse_capabilities(output).is_supported());
        }
        let old = parse_capabilities("sdk=19\npm=1\nam=1\nmirror=1");
        assert!(old.is_installable() && old.is_launchable());
        assert!(!old.is_mirrorable());
        let modern = parse_capabilities("sdk=35\npm=1\nam=1\nmirror=1");
        assert!(modern.is_mirrorable() && modern.is_installable() && modern.is_launchable());
    }
    #[test]
    fn package_names_are_data_not_shell_commands() {
        for name in [
            "",
            "-p",
            "a;reboot",
            "com.example/Activity",
            "com..example",
            "com.$(id)",
            "1.invalid",
        ] {
            assert!(validate_package(name).is_err());
        }
        assert!(validate_package("com.example_app.demo").is_ok());
        assert!(valid_activity(".MainActivity$Inner"));
        assert!(!valid_activity("Main;id"));
    }
    #[test]
    fn android_errors_are_not_reported_as_success() {
        assert!(check_install("Performing Streamed Install\nSuccess\n").is_ok());
        assert!(check_install("Failure [INSTALL_FAILED_VERSION_DOWNGRADE]").is_err());
        assert!(check_install("").is_err());
        assert!(check_launch("Starting: Intent\nStatus: ok\nComplete").is_ok());
        assert!(check_launch("Error type 3\nError: Activity class does not exist").is_err());
        assert!(check_launch("Status: timeout").is_err());
    }

    #[test]
    fn probe_and_application_listing_use_the_existing_verified_adb_shell() {
        let (device, runner) = device(&[
            "sdk=35\npm=1\nam=1\nmirror=1",
            "package:com.example.z\npackage:com.example.a\npackage:com.example.z\n",
        ]);
        let cancel = AtomicBool::new(false);
        assert!(
            SystemAndroidTools
                .probe(&device, &cancel)
                .unwrap()
                .is_mirrorable()
        );
        let apps = SystemAndroidTools.apps(&device, &cancel).unwrap();
        assert_eq!(
            apps.iter().map(AndroidApp::package).collect::<Vec<_>>(),
            ["com.example.a", "com.example.z"]
        );
        assert_eq!(runner.requests.lock().unwrap().len(), 4);
    }

    #[test]
    fn apk_install_uses_an_absolute_path_as_one_argument_and_checks_the_result() {
        let dir = tempfile::tempdir().unwrap();
        let apk = dir.path().join("test app.APK");
        std::fs::write(&apk, b"test").unwrap();
        let (device, runner) = device(&["Success\n", "Failure [INSTALL_FAILED_INVALID_APK]"]);
        let cancel = AtomicBool::new(false);
        SystemAndroidTools.install(&device, &apk, &cancel).unwrap();
        assert!(SystemAndroidTools.install(&device, &apk, &cancel).is_err());
        let requests = runner.requests.lock().unwrap();
        assert_eq!(
            requests[1],
            vec![
                "-s".into(),
                "board".into(),
                "install".into(),
                "-r".into(),
                apk.canonicalize().unwrap().into_os_string(),
            ]
        );
        drop(requests);
        assert!(
            SystemAndroidTools
                .install(&device, &dir.path().join("missing.apk"), &cancel)
                .is_err()
        );
        assert_eq!(runner.requests.lock().unwrap().len(), 4);
    }

    #[test]
    fn launch_resolves_and_checks_the_component_before_starting() {
        let (device, runner) = device(&[
            "priority=0\ncom.example.app/.MainActivity\n",
            "Starting: Intent\nStatus: ok\n",
            "com.other.app/.MainActivity\n",
        ]);
        let cancel = AtomicBool::new(false);
        SystemAndroidTools
            .launch(&device, "com.example.app", &cancel)
            .unwrap();
        assert!(
            SystemAndroidTools
                .launch(&device, "com.example.app", &cancel)
                .is_err()
        );
        assert!(
            SystemAndroidTools
                .launch(&device, "com.example.app;reboot", &cancel)
                .is_err()
        );
        let requests = runner.requests.lock().unwrap();
        assert_eq!(requests.len(), 5);
        let command = requests[2].last().unwrap().to_str().unwrap();
        assert!(command.contains("am start -W"));
        assert!(command.contains("com.example.app/.MainActivity"));
    }

    #[test]
    fn cancelled_actions_do_not_send_adb_commands() {
        let (device, runner) = device(&[]);
        let cancel = AtomicBool::new(true);
        assert!(SystemAndroidTools.probe(&device, &cancel).is_err());
        assert!(
            SystemAndroidTools
                .launch(&device, "com.example.app", &cancel)
                .is_err()
        );
        assert!(runner.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn package_manager_errors_are_not_empty_successful_lists() {
        for output in [
            "Error: unknown option --user",
            "java.lang.SecurityException: Permission Denial",
            "package:com.example.app\nError: user is unavailable",
            "package:invalid;reboot",
        ] {
            let (device, _) = device(&[output]);
            assert!(
                SystemAndroidTools
                    .apps(&device, &AtomicBool::new(false))
                    .is_err()
            );
        }
        assert!(parse_apps("\r\n").unwrap().is_empty());
    }
}
