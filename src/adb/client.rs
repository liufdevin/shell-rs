use crate::i18n::t;
use crate::sftp::{DirectoryEntry, DirectoryListing, EntryKind, FileMetadata};
use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use std::{
    ffi::{OsStr, OsString},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const LIMIT: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct PtyRequired;

impl std::fmt::Display for PtyRequired {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("device only supports allocating a pty")
    }
}

impl std::error::Error for PtyRequired {}

fn requires_pty(message: &str) -> bool {
    let message = message.trim().to_ascii_lowercase();
    message
        .strip_prefix("error: ")
        .unwrap_or(&message)
        .trim_end_matches('.')
        == "device only supports allocating a pty"
}

pub trait AdbDetector: Send + Sync + 'static {
    fn detect(&self, boot_id: Option<&str>) -> AdbDetection;
}

pub enum AdbDetection {
    Matched(AdbConnection),
    Choose(Vec<AdbConnection>),
    Unavailable(String),
}

pub struct NoAdbDetector;
impl AdbDetector for NoAdbDetector {
    fn detect(&self, _: Option<&str>) -> AdbDetection {
        AdbDetection::Unavailable(String::new())
    }
}

pub struct SystemAdbDetector;
impl AdbDetector for SystemAdbDetector {
    fn detect(&self, boot_id: Option<&str>) -> AdbDetection {
        let detect = || -> Result<AdbDetection> {
            let runner: Arc<dyn Runner> = Arc::new(SystemRunner(
                find_adb().ok_or_else(|| anyhow!(t!("adb.missing")))?,
            ));
            discover(runner, boot_id)
        };
        detect().unwrap_or_else(|error| AdbDetection::Unavailable(error.to_string()))
    }
}

pub(super) trait Runner: Send + Sync {
    fn program(&self) -> Option<PathBuf> {
        None
    }
    fn run(
        &self,
        args: &[OsString],
        timeout: Duration,
        cancel: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<u8>>;
}

#[derive(Clone)]
pub struct AdbConnection {
    runner: Arc<dyn Runner>,
    serial: String,
    name: Option<String>,
    boot_id: Option<String>,
    transport_id: Option<u64>,
    pty_required: Arc<AtomicBool>,
}

impl AdbConnection {
    pub(super) fn program(&self) -> Option<PathBuf> {
        self.runner.program()
    }
    pub fn new(program: PathBuf, serial: String) -> Result<Self> {
        if serial.is_empty()
            || serial.starts_with('-')
            || serial
                .chars()
                .any(|value| value.is_whitespace() || value.is_control())
        {
            bail!(t!("adb.invalid_response"));
        }
        Ok(Self {
            runner: Arc::new(SystemRunner(program)),
            serial,
            name: None,
            boot_id: None,
            transport_id: None,
            pty_required: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn with_name(mut self, name: impl AsRef<str>) -> Self {
        self.name = clean_device_name(name.as_ref());
        self
    }

    pub fn serial(&self) -> &str {
        &self.serial
    }

    pub fn boot_id(&self) -> Option<&str> {
        self.boot_id.as_deref()
    }

    pub fn monitor_command(&self, command: &str, cancel: &AtomicBool) -> Result<String> {
        if cancel.load(Ordering::Acquire) {
            bail!(t!("adb.interrupted"));
        }
        let mut cancelled = || cancel.load(Ordering::Acquire);
        self.verify(&mut cancelled)?;
        Ok(String::from_utf8(self.shell(command, &mut cancelled)?)?)
    }

    pub fn collect_bugreport(&self, destination: &Path, cancel: &AtomicBool) -> Result<()> {
        let mut cancelled = || cancel.load(Ordering::Acquire);
        self.verify(&mut cancelled)?;
        self.run(
            &[OsStr::new("bugreport"), destination.as_os_str()],
            Duration::from_secs(300),
            &mut cancelled,
        )?;
        if cancelled() {
            bail!(t!("adb.interrupted"));
        }
        self.verify(&mut cancelled)?;
        if !destination.is_file() {
            bail!(t!("adb.invalid_response"));
        }
        Ok(())
    }

    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.serial)
    }

    pub(super) fn run(
        &self,
        args: &[&OsStr],
        timeout: Duration,
        cancel: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<u8>> {
        let mut command = match self.transport_id {
            Some(id) => vec![OsString::from("-t"), OsString::from(id.to_string())],
            None => vec![OsString::from("-s"), OsString::from(&self.serial)],
        };
        command.extend(args.iter().map(|value| value.to_os_string()));
        self.runner.run(&command, timeout, cancel)
    }

    pub(super) fn shell(&self, script: &str, cancel: &mut dyn FnMut() -> bool) -> Result<Vec<u8>> {
        let marker = format!("__shelllf_adb_{}__", uuid::Uuid::new_v4().simple());
        let wrapped =
            format!("( {script}\n) 2>&1; result=$?; printf '\\000{marker}%s\\n' \"$result\"");
        let output = self.shell_output(&wrapped, Duration::from_secs(15), cancel)?;
        let marker = format!("\0{marker}");
        let offset = output
            .windows(marker.len())
            .rposition(|value| value == marker.as_bytes())
            .ok_or_else(|| anyhow!(t!("adb.invalid_response")))?;
        let status = std::str::from_utf8(&output[offset + marker.len()..])?.trim();
        if status != "0" {
            let message: String = String::from_utf8_lossy(&output[..offset])
                .chars()
                .filter(|value| !value.is_control() || *value == '\n')
                .take(2000)
                .collect();
            if message.trim().is_empty() {
                bail!(t!("adb.command_failed", status = status));
            }
            bail!("{}", message.trim());
        }
        Ok(output[..offset].to_vec())
    }

    fn shell_output(
        &self,
        script: &str,
        timeout: Duration,
        cancel: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<u8>> {
        if cancel() {
            bail!(t!("adb.interrupted"));
        }
        if !self.pty_required.load(Ordering::Acquire) {
            match self.run(
                &[OsStr::new("shell"), OsStr::new("-T"), OsStr::new(script)],
                timeout,
                cancel,
            ) {
                Err(error) if error.is::<PtyRequired>() => {
                    self.pty_required.store(true, Ordering::Release)
                }
                result => return result,
            }
        }
        if cancel() {
            bail!(t!("adb.interrupted"));
        }
        let marker = format!("__shelllf_pty_{}__", uuid::Uuid::new_v4().simple());
        let wrapped = format!(
            "if ! command -v base64 >/dev/null 2>&1; then printf '{marker}N\\n'; else printf '{marker}B\\n'; {{ {script}\n}} 2>&1 | base64; printf '\\n{marker}E\\n'; fi"
        );
        let output = self.run(
            &[OsStr::new("shell"), OsStr::new("-tt"), OsStr::new(&wrapped)],
            timeout,
            cancel,
        )?;
        let missing = format!("{marker}N");
        if output
            .windows(missing.len())
            .any(|bytes| bytes == missing.as_bytes())
        {
            bail!(t!("adb.pty_base64_missing"));
        }
        let begin = format!("{marker}B");
        let end = format!("{marker}E");
        let start = output
            .windows(begin.len())
            .position(|bytes| bytes == begin.as_bytes())
            .map(|offset| offset + begin.len())
            .ok_or_else(|| anyhow!(t!("adb.invalid_response")))?;
        let finish = output[start..]
            .windows(end.len())
            .position(|bytes| bytes == end.as_bytes())
            .map(|offset| start + offset)
            .ok_or_else(|| anyhow!(t!("adb.invalid_response")))?;
        let encoded: Vec<_> = output[start..finish]
            .iter()
            .copied()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect();
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| anyhow!(t!("adb.invalid_response")))
    }

    pub(super) fn verify(&self, cancel: &mut dyn FnMut() -> bool) -> Result<()> {
        if let Some(expected) = &self.boot_id {
            if self.identity(cancel)?.as_ref() != Some(expected) {
                bail!(t!("adb.device_changed"));
            }
        } else {
            let state = self.run(&[OsStr::new("get-state")], Duration::from_secs(5), cancel)?;
            if state != b"device\n" && state != b"device\r\n" {
                bail!(t!("adb.unavailable"));
            }
        }
        Ok(())
    }

    fn identity(&self, cancel: &mut dyn FnMut() -> bool) -> Result<Option<String>> {
        let output = self.shell_output(
            "cat /proc/sys/kernel/random/boot_id",
            Duration::from_secs(2),
            cancel,
        )?;
        Ok(std::str::from_utf8(&output)
            .ok()
            .and_then(canonical_boot_id))
    }

    #[cfg(test)]
    pub(super) fn test_connection(runner: Arc<dyn Runner>) -> Self {
        Self {
            runner,
            serial: "board".into(),
            name: None,
            boot_id: Some("00000000-0000-4000-8000-000000000001".into()),
            transport_id: None,
            pty_required: Arc::new(AtomicBool::new(false)),
        }
    }

    #[cfg(test)]
    pub(crate) fn monitor_test_connection(serial: &str, boot: Option<&str>) -> Self {
        let mut device = Self::new(PathBuf::from("unused-adb"), serial.to_owned()).unwrap();
        device.boot_id = boot.map(str::to_owned);
        device
    }

    pub(super) fn list(
        &self,
        path: &str,
        cancel: &mut dyn FnMut() -> bool,
    ) -> Result<DirectoryListing> {
        let path = if path.starts_with('-') {
            format!("./{path}")
        } else {
            path.to_owned()
        };
        let script = format!(
            "cd {} || exit 1; printf 'SHELLLF_ADB_LIST\\000%s\\000' \"$PWD\"; count=0; for entry in ./* ./.[!.]* ./..?*; do if [ ! -e \"$entry\" ] && [ ! -L \"$entry\" ]; then continue; fi; count=$((count+1)); [ \"$count\" -le 10000 ] || exit 1; info=$(stat -c '%f %s %Y' -- \"$entry\") || exit 1; target=-; if [ -L \"$entry\" ]; then if [ -d \"$entry\" ]; then target=d; elif [ -f \"$entry\" ]; then target=f; fi; fi; printf '%s %s\\000%s\\000' \"$info\" \"$target\" \"${{entry#./}}\"; done; printf 'END\\000'",
            quote(&path)?
        );
        parse_listing(&self.shell(&script, cancel)?)
    }

    pub(super) fn metadata(
        &self,
        path: &str,
        cancel: &mut dyn FnMut() -> bool,
    ) -> Result<Option<FileMetadata>> {
        let path = quote(path)?;
        let output = self.shell(
            &format!("if [ -e {path} ] || [ -L {path} ]; then stat -c '%f %s %Y' -- {path}; fi"),
            cancel,
        )?;
        if output.is_empty() {
            return Ok(None);
        }
        Ok(Some(parse_metadata(std::str::from_utf8(&output)?.trim())?))
    }
}

impl PartialEq for AdbConnection {
    fn eq(&self, other: &Self) -> bool {
        self.serial == other.serial
            && self.boot_id == other.boot_id
            && self.transport_id == other.transport_id
            && Arc::ptr_eq(&self.runner, &other.runner)
    }
}
impl Eq for AdbConnection {}

pub(super) fn quote(value: &str) -> Result<String> {
    if value.is_empty() || value.contains('\0') {
        bail!(t!("adb.invalid_path"));
    }
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}

pub fn canonical_boot_id(value: &str) -> Option<String> {
    uuid::Uuid::parse_str(value.trim())
        .ok()
        .filter(|value| !value.is_nil())
        .map(|value| value.to_string())
}

fn discover(runner: Arc<dyn Runner>, boot_id: Option<&str>) -> Result<AdbDetection> {
    let expected = boot_id.and_then(canonical_boot_id);
    let output = runner.run(
        &["devices".into(), "-l".into()],
        Duration::from_secs(10),
        &mut || false,
    )?;
    let mut devices = Vec::new();
    for line in std::str::from_utf8(&output)?.lines() {
        let mut words = line.split_whitespace();
        let Some(serial) = words.next() else { continue };
        if words.next() != Some("device")
            || devices.len() >= 16
            || serial.starts_with('-')
            || serial.chars().any(char::is_control)
        {
            continue;
        }
        let mut transport_id = None;
        let mut model = None;
        let mut product = None;
        let mut device_name = None;
        for attribute in words {
            let Some((key, value)) = attribute.split_once(':') else {
                continue;
            };
            match key {
                "transport_id" => {
                    transport_id = value.parse::<u64>().ok().filter(|value| *value > 0);
                }
                "model" => model = model.or_else(|| clean_device_name(value)),
                "product" => product = product.or_else(|| clean_device_name(value)),
                "device" => device_name = device_name.or_else(|| clean_device_name(value)),
                _ => {}
            }
        }
        let mut device = AdbConnection {
            runner: runner.clone(),
            serial: serial.to_owned(),
            name: model.or(product).or(device_name),
            boot_id: None,
            transport_id,
            pty_required: Arc::new(AtomicBool::new(false)),
        };
        device.boot_id = device.identity(&mut || false).ok().flatten();
        devices.push(device);
    }
    let matched: Vec<_> = devices
        .iter()
        .enumerate()
        .filter(|(_, device)| expected.is_some() && device.boot_id == expected)
        .map(|(index, _)| index)
        .collect();
    if matched.len() == 1 {
        return Ok(AdbDetection::Matched(devices.remove(matched[0])));
    }
    if devices.is_empty() {
        Ok(AdbDetection::Unavailable(t!("adb.unavailable").to_string()))
    } else {
        Ok(AdbDetection::Choose(devices))
    }
}

fn clean_device_name(raw: &str) -> Option<String> {
    let name = raw.trim().trim_matches('"').replace('_', " ");
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    (!name.is_empty()
        && name.len() <= 100
        && !name.eq_ignore_ascii_case("unknown")
        && !name.chars().any(char::is_control))
    .then_some(name)
}

fn parse_metadata(raw: &str) -> Result<FileMetadata> {
    let mut values = raw.split_whitespace();
    let mode = u32::from_str_radix(
        values
            .next()
            .ok_or_else(|| anyhow!(t!("adb.invalid_response")))?,
        16,
    )?;
    let size = values
        .next()
        .ok_or_else(|| anyhow!(t!("adb.invalid_response")))?
        .parse()?;
    let modified = values.next().and_then(|value| value.parse().ok());
    let kind = match mode & 0o170000 {
        0o040000 => EntryKind::Directory,
        0o100000 => EntryKind::File,
        0o120000 => EntryKind::Symlink,
        _ => EntryKind::Other,
    };
    Ok(FileMetadata::new(kind, size, modified, Some(mode & 0o7777)))
}

fn parse_listing(output: &[u8]) -> Result<DirectoryListing> {
    let mut fields = output.split(|byte| *byte == 0);
    if fields.next() != Some(b"SHELLLF_ADB_LIST".as_slice()) {
        bail!(t!("adb.invalid_response"));
    }
    let path = std::str::from_utf8(
        fields
            .next()
            .ok_or_else(|| anyhow!(t!("adb.invalid_response")))?,
    )?;
    if !path.starts_with('/') {
        bail!(t!("adb.invalid_path"));
    }
    let mut entries = Vec::new();
    loop {
        let field = fields
            .next()
            .ok_or_else(|| anyhow!(t!("adb.invalid_response")))?;
        if field == b"END" {
            break;
        }
        let metadata = parse_metadata(std::str::from_utf8(field)?)?;
        let name = std::str::from_utf8(
            fields
                .next()
                .ok_or_else(|| anyhow!(t!("adb.invalid_response")))?,
        )?;
        if name.is_empty()
            || name.contains('/')
            || matches!(name, "." | "..")
            || entries.len() >= 10000
        {
            bail!(t!("adb.invalid_path"));
        }
        let target_kind = match std::str::from_utf8(field)?.split_whitespace().nth(3) {
            Some("d") => Some(EntryKind::Directory),
            Some("f") => Some(EntryKind::File),
            _ => None,
        };
        entries.push(DirectoryEntry::new(name, metadata).with_target_kind(target_kind));
    }
    if fields.any(|field| !field.is_empty()) {
        bail!(t!("adb.invalid_response"));
    }
    Ok(DirectoryListing::new(path, entries))
}

fn find_adb() -> Option<PathBuf> {
    let executable = if cfg!(windows) { "adb.exe" } else { "adb" };
    let mut directories = Vec::new();
    if let Some(path) = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    {
        directories.push(path.join("platform-tools"));
        directories.push(path.join("scrcpy"));
        directories.push(path);
    }
    for variable in ["ANDROID_SDK_ROOT", "ANDROID_HOME"] {
        if let Some(path) = std::env::var_os(variable) {
            directories.push(PathBuf::from(path).join("platform-tools"));
        }
    }
    if let Some(path) = std::env::var_os("LOCALAPPDATA") {
        directories.push(PathBuf::from(path).join("Android/Sdk/platform-tools"));
    }
    if let Some(home) = dirs::home_dir() {
        directories.push(home.join("Android/Sdk/platform-tools"));
        directories.push(home.join("Library/Android/sdk/platform-tools"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        directories.extend(std::env::split_paths(&path));
    }
    directories
        .into_iter()
        .filter(|path| path.is_absolute())
        .map(|path| path.join(executable))
        .find(|path| path.is_file())
}

struct SystemRunner(PathBuf);
impl Runner for SystemRunner {
    fn program(&self) -> Option<PathBuf> {
        Some(self.0.clone())
    }
    fn run(
        &self,
        args: &[OsString],
        timeout: Duration,
        cancel: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<u8>> {
        let mut command = Command::new(&self.0);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(0x0800_0000);
        }
        let (mut child, group) =
            super::process::CommandGroup::spawn(&mut command).context(t!("adb.start_failed"))?;
        let overflow = Arc::new(AtomicBool::new(false));
        let stdout = read_pipe(child.stdout.take().unwrap(), overflow.clone());
        let stderr = read_pipe(child.stderr.take().unwrap(), overflow.clone());
        let start = Instant::now();
        let mut interrupted = false;
        let status = loop {
            if cancel() || start.elapsed() >= timeout || overflow.load(Ordering::Relaxed) {
                interrupted = true;
                group.terminate();
                let _ = child.kill();
                break child.wait();
            }
            match child.try_wait() {
                Ok(Some(status)) if stdout.is_finished() && stderr.is_finished() => {
                    break Ok(status);
                }
                Ok(Some(_)) | Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(error) => {
                    group.terminate();
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(error);
                }
            }
        };
        let output = stdout
            .join()
            .map_err(|_| anyhow!(t!("adb.invalid_response")))??;
        let error = stderr
            .join()
            .map_err(|_| anyhow!(t!("adb.invalid_response")))??;
        if interrupted || overflow.load(Ordering::Relaxed) {
            bail!(t!("adb.interrupted"));
        }
        let status = status?;
        if !status.success() {
            let message = String::from_utf8_lossy(&error);
            if requires_pty(&message) {
                return Err(PtyRequired.into());
            }
            if message.trim().is_empty() {
                bail!(t!("adb.command_failed", status = status));
            }
            bail!("{}", message.trim());
        }
        Ok(output)
    }
}

fn read_pipe(
    pipe: impl Read + Send + 'static,
    overflow: Arc<AtomicBool>,
) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        let mut output = Vec::new();
        pipe.take((LIMIT + 1) as u64).read_to_end(&mut output)?;
        if output.len() > LIMIT {
            overflow.store(true, Ordering::Relaxed);
        }
        Ok(output)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adb::test_support::{BOARD_BOOT, FakeAdb, OTHER_BOOT};

    struct BugreportAdb {
        inner: FakeAdb,
        changed: bool,
    }

    impl Runner for BugreportAdb {
        fn run(
            &self,
            args: &[OsString],
            timeout: Duration,
            cancel: &mut dyn FnMut() -> bool,
        ) -> Result<Vec<u8>> {
            if args.get(2).is_some_and(|argument| argument == "bugreport") {
                assert_eq!(timeout, Duration::from_secs(300));
                assert_eq!(args[0], "-s");
                assert_eq!(args[1], "board");
                std::fs::write(PathBuf::from(&args[3]), "bugreport archive")?;
                if self.changed {
                    self.inner
                        .identities
                        .lock()
                        .unwrap()
                        .insert("board".into(), OTHER_BOOT.into());
                }
                return Ok(Vec::new());
            }
            self.inner.run(args, timeout, cancel)
        }
    }

    #[test]
    fn bugreport_is_bound_to_the_device_and_rechecks_identity_after_collection() {
        for changed in [false, true] {
            let runner = Arc::new(BugreportAdb {
                inner: FakeAdb::default(),
                changed,
            });
            let device = AdbConnection::test_connection(runner);
            let directory = tempfile::tempdir().unwrap();
            let output = directory.path().join("bugreport.zip");
            let result = device.collect_bugreport(&output, &AtomicBool::new(false));
            assert_eq!(result.is_ok(), !changed);
        }
    }

    #[test]
    fn cancelled_bugreport_never_starts_collection() {
        let runner = Arc::new(BugreportAdb {
            inner: FakeAdb::default(),
            changed: false,
        });
        let device = AdbConnection::test_connection(runner);
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("bugreport.zip");
        assert!(
            device
                .collect_bugreport(&output, &AtomicBool::new(true))
                .is_err()
        );
        assert!(!output.exists());
    }

    #[test]
    #[ignore = "reads directories from an explicitly selected connected ADB device"]
    fn connected_device_directory_listing_is_readable() {
        let program = std::env::var_os("SHELLRS_TEST_ADB").expect("SHELLRS_TEST_ADB is required");
        let serial =
            std::env::var("SHELLRS_TEST_ADB_SERIAL").expect("SHELLRS_TEST_ADB_SERIAL is required");
        let runner = Arc::new(SystemRunner(PathBuf::from(program)));
        let AdbDetection::Choose(devices) = discover(runner, None).unwrap() else {
            panic!("expected connected ADB devices");
        };
        let device = devices
            .into_iter()
            .find(|device| device.serial() == serial)
            .expect("selected device is connected");
        let paths = std::env::var("SHELLRS_TEST_ADB_PATHS")
            .unwrap_or_else(|_| "/;/sdcard;/storage/emulated/0;/system".into());
        for path in paths.split(';') {
            device.verify(&mut || false).unwrap();
            let listing = device
                .list(path, &mut || false)
                .unwrap_or_else(|error| panic!("{path}: {error:#}"));
            assert_eq!(listing.path(), path);
            assert!(!listing.entries().is_empty());
            assert!(matches!(
                device
                    .metadata(path, &mut || false)
                    .unwrap()
                    .unwrap()
                    .kind(),
                EntryKind::Directory | EntryKind::Symlink
            ));
            println!("{}: {} entries", listing.path(), listing.entries().len());
        }
    }

    #[test]
    fn adb_matches_the_current_board_without_assuming_android() {
        let fake = FakeAdb {
            devices: vec![
                ("phone".into(), "device".into()),
                ("board".into(), "device".into()),
            ],
            ..Default::default()
        };
        fake.identities
            .lock()
            .unwrap()
            .insert("phone".into(), OTHER_BOOT.into());
        let fake = Arc::new(fake);
        let AdbDetection::Matched(device) = discover(fake.clone(), Some(BOARD_BOOT)).unwrap()
        else {
            panic!("board must match")
        };
        assert_eq!(device.serial(), "board");
        assert!(
            !fake
                .requests
                .lock()
                .unwrap()
                .iter()
                .flatten()
                .any(|arg| arg.contains("getprop"))
        );
    }

    #[test]
    fn one_unrelated_device_is_never_silently_used() {
        let fake = Arc::new(FakeAdb::default());
        assert!(matches!(
            discover(fake.clone(), Some(OTHER_BOOT)).unwrap(),
            AdbDetection::Choose(_)
        ));
        assert!(matches!(
            discover(fake, None).unwrap(),
            AdbDetection::Choose(_)
        ));
    }

    #[test]
    fn device_names_come_from_the_model_and_fall_back_to_the_serial() {
        let fake = FakeAdb {
            devices: vec![
                ("board".into(), "device".into()),
                ("phone".into(), "device".into()),
                ("bare".into(), "device".into()),
            ],
            ..Default::default()
        };
        fake.models
            .lock()
            .unwrap()
            .insert("board".into(), "Helper_A133".into());
        fake.models
            .lock()
            .unwrap()
            .insert("phone".into(), "Pixel_7_Pro".into());
        let AdbDetection::Choose(devices) = discover(Arc::new(fake), None).unwrap() else {
            panic!("three devices require a choice")
        };
        assert_eq!(devices[0].display_name(), "Helper A133");
        assert_eq!(devices[1].display_name(), "Pixel 7 Pro");
        assert_eq!(devices[2].display_name(), "bare");
        assert_eq!(
            AdbConnection::new(PathBuf::from("adb"), "serial".into())
                .unwrap()
                .with_name("unknown")
                .display_name(),
            "serial"
        );
    }

    #[test]
    fn offline_and_unauthorized_devices_are_not_file_targets() {
        let fake = Arc::new(FakeAdb {
            devices: vec![
                ("board".into(), "unauthorized".into()),
                ("phone".into(), "offline".into()),
            ],
            ..Default::default()
        });
        assert!(matches!(
            discover(fake.clone(), Some(BOARD_BOOT)).unwrap(),
            AdbDetection::Unavailable(_)
        ));
        assert_eq!(fake.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn duplicated_boot_ids_require_explicit_device_selection() {
        let fake = FakeAdb {
            devices: vec![
                ("board".into(), "device".into()),
                ("clone".into(), "device".into()),
            ],
            ..Default::default()
        };
        fake.identities
            .lock()
            .unwrap()
            .insert("clone".into(), BOARD_BOOT.into());
        assert!(matches!(
            discover(Arc::new(fake), Some(BOARD_BOOT)).unwrap(),
            AdbDetection::Choose(_)
        ));
    }

    #[test]
    fn discovered_devices_pin_the_adb_transport_id_when_available() {
        let fake = FakeAdb {
            devices: vec![("board".into(), "device transport_id:7".into())],
            ..Default::default()
        };
        fake.identities
            .lock()
            .unwrap()
            .insert("7".into(), BOARD_BOOT.into());
        let fake = Arc::new(fake);
        let AdbDetection::Matched(device) = discover(fake.clone(), Some(BOARD_BOOT)).unwrap()
        else {
            panic!("matching board required")
        };
        assert_eq!(device.serial(), "board");
        assert_eq!(device.transport_id, Some(7));
        assert!(
            fake.requests
                .lock()
                .unwrap()
                .iter()
                .skip(1)
                .all(|args| args[0] == "-t" && args[1] == "7")
        );
    }

    #[test]
    fn a_restarted_or_replaced_device_is_rejected() {
        let fake = Arc::new(FakeAdb::default());
        let connection = fake.connection();
        assert!(connection.verify(&mut || false).is_ok());
        fake.identities
            .lock()
            .unwrap()
            .insert("board".into(), OTHER_BOOT.into());
        assert!(connection.verify(&mut || false).is_err());
    }

    #[test]
    fn pty_only_boards_are_discovered_and_directory_bytes_survive_terminal_newlines() {
        use super::super::test_support::PtyOnlyAdb;
        let fake = Arc::new(PtyOnlyAdb::default());
        fake.inner.file("/设备\r\nname.txt", b"data");
        let AdbDetection::Matched(device) = discover(fake.clone(), Some(BOARD_BOOT)).unwrap()
        else {
            panic!("PTY board identity should match");
        };
        assert_eq!(device.boot_id(), Some(BOARD_BOOT));
        device.clone().verify(&mut || false).unwrap();
        let listing = device.list("/", &mut || false).unwrap();
        assert_eq!(listing.entries()[0].name(), "设备\r\nname.txt");
        assert_eq!(fake.rejected.load(Ordering::SeqCst), 1);
        assert_eq!(fake.allocated.load(Ordering::SeqCst), 3);
        fake.inner
            .identities
            .lock()
            .unwrap()
            .insert("board".into(), OTHER_BOOT.into());
        assert!(device.verify(&mut || false).is_err());
    }

    #[test]
    fn pty_compatibility_reports_missing_tools_and_never_replays_truncated_commands() {
        use super::super::test_support::PtyOnlyAdb;
        let missing = Arc::new(PtyOnlyAdb {
            missing_base64: true,
            ..Default::default()
        });
        assert!(
            missing
                .connection()
                .shell("cd /", &mut || false)
                .unwrap_err()
                .to_string()
                .contains("base64")
        );
        assert!(missing.inner.requests.lock().unwrap().is_empty());
        let corrupt = Arc::new(PtyOnlyAdb {
            truncated: true,
            ..Default::default()
        });
        assert!(corrupt.connection().shell("cd /", &mut || false).is_err());
        assert_eq!(corrupt.allocated.load(Ordering::SeqCst), 1);
        assert_eq!(corrupt.inner.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn unrelated_adb_failures_do_not_trigger_a_pty_retry() {
        use std::sync::atomic::AtomicUsize;
        struct Failed(AtomicUsize);
        impl Runner for Failed {
            fn run(
                &self,
                _: &[OsString],
                _: Duration,
                _: &mut dyn FnMut() -> bool,
            ) -> Result<Vec<u8>> {
                self.0.fetch_add(1, Ordering::SeqCst);
                bail!("permission denied");
            }
        }
        let runner = Arc::new(Failed(AtomicUsize::new(0)));
        assert!(
            AdbConnection::test_connection(runner.clone())
                .shell("mkdir /test", &mut || false)
                .is_err()
        );
        assert_eq!(runner.0.load(Ordering::SeqCst), 1);
        assert!(requires_pty(
            "error: device only supports allocating a pty\r\n"
        ));
        assert!(!requires_pty("error: device offline"));
        assert!(!requires_pty(
            "permission denied: device only supports allocating a pty"
        ));
    }

    #[test]
    fn listing_preserves_literal_names_and_rejects_truncated_records() {
        let listing = parse_listing(
            b"SHELLLF_ADB_LIST\0/tmp\x0041ed 0 1\0folder\x0081a4 4 2\0a'b $HOME.txt\0END\0",
        )
        .unwrap();
        assert_eq!(listing.entries()[0].metadata().kind(), EntryKind::Directory);
        assert_eq!(listing.entries()[1].name(), "a'b $HOME.txt");
        assert!(parse_listing(b"SHELLLF_ADB_LIST\0/tmp\x0081a4 4 2\0file").is_err());
        assert!(parse_listing(b"SHELLLF_ADB_LIST\0/tmp\x0081a4 4 2\0../bad\0END\0").is_err());
        assert!(quote("bad\0path").is_err());
        assert_eq!(quote("a'b;$HOME").unwrap(), "'a'\\''b;$HOME'");
        let listing = parse_listing(b"SHELLLF_ADB_LIST\0/\0a1ff 0 1 d\0sdcard\0END\0").unwrap();
        assert_eq!(listing.entries()[0].metadata().kind(), EntryKind::Symlink);
        assert_eq!(
            listing.entries()[0].target_kind(),
            Some(EntryKind::Directory)
        );
    }

    #[test]
    fn shell_status_framing_does_not_depend_on_android_line_endings() {
        struct AndroidReplies {
            payload: Vec<u8>,
            status: u32,
        }
        impl Runner for AndroidReplies {
            fn run(
                &self,
                args: &[OsString],
                _: Duration,
                _: &mut dyn FnMut() -> bool,
            ) -> Result<Vec<u8>> {
                let script = args.last().unwrap().to_str().unwrap();
                let marker = script
                    .rsplit_once("printf '\\000")
                    .expect("binary status separator")
                    .1
                    .split("%s")
                    .next()
                    .unwrap();
                let mut output = self.payload.clone();
                output.extend_from_slice(format!("\0{marker}{}\r\n", self.status).as_bytes());
                Ok(output)
            }
        }
        let payload = b"SHELLLF_ADB_LIST\0/sdcard\0\x38\x31a4 4 2 -\0\xe6\x96\x87\xe4\xbb\xb6\r\nname.txt\0END\0";
        let device = AdbConnection::test_connection(Arc::new(AndroidReplies {
            payload: payload.to_vec(),
            status: 0,
        }));
        let output = device.shell("read-only command", &mut || false).unwrap();
        assert_eq!(output, payload);
        let listing = parse_listing(&output).unwrap();
        assert_eq!(listing.entries()[0].name(), "文件\r\nname.txt");
        let device = AdbConnection::test_connection(Arc::new(AndroidReplies {
            payload: b"Permission denied\r\n".to_vec(),
            status: 1,
        }));
        assert!(
            device
                .shell("read-only command", &mut || false)
                .unwrap_err()
                .to_string()
                .contains("Permission denied")
        );
    }

    #[test]
    fn subprocess_timeout_does_not_leave_a_background_command_running() {
        #[cfg(windows)]
        let (program, hello, endless, expected) = (
            PathBuf::from(std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into())),
            vec!["/D".into(), "/C".into(), "echo hello".into()],
            vec![
                "/D".into(),
                "/C".into(),
                "for /L %G in (0,0,1) do @rem".into(),
            ],
            b"hello\r\n".as_slice(),
        );
        #[cfg(unix)]
        let (program, hello, endless, expected) = (
            PathBuf::from("sh"),
            vec!["-c".into(), "printf hello".into()],
            vec!["-c".into(), "while :; do :; done".into()],
            b"hello".as_slice(),
        );
        let runner = SystemRunner(program);
        assert_eq!(
            runner
                .run(&hello, Duration::from_secs(3), &mut || false)
                .unwrap(),
            expected
        );
        let started = Instant::now();
        assert!(
            runner
                .run(&endless, Duration::from_millis(50), &mut || false)
                .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
