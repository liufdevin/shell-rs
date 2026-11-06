use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use chrono::{Local, NaiveDate};
use gpui_kit::{App, Global, SharedString};
use serde::{Deserialize, Serialize};

use crate::i18n::t;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct TerminalConnectionSettings {
    pub auto_reconnect: bool,
    pub reconnect_delay_secs: u32,
}

impl Default for TerminalConnectionSettings {
    fn default() -> Self {
        Self {
            auto_reconnect: false,
            reconnect_delay_secs: 5,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SerialDataModes {
    pub receive_hex: bool,
    pub send_hex: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SerialPreferences(BTreeMap<String, SerialDataModes>);

impl Global for SerialPreferences {}

impl SerialPreferences {
    pub fn get(&self, host_id: &str) -> SerialDataModes {
        self.0.get(host_id).copied().unwrap_or_default()
    }

    pub fn set(&mut self, host_id: String, modes: SerialDataModes) {
        if modes == SerialDataModes::default() {
            self.0.remove(&host_id);
        } else {
            self.0.insert(host_id, modes);
        }
    }

    pub(super) fn current(host_id: &str, cx: &App) -> SerialDataModes {
        cx.try_global::<Self>()
            .map(|preferences| preferences.get(host_id))
            .unwrap_or_default()
    }
}

pub fn default_log_directory() -> PathBuf {
    crate::app::data_dir().join("logs")
}

#[derive(Clone)]
pub(super) struct DailyLog {
    directory: PathBuf,
    name: String,
}

impl DailyLog {
    pub fn new(directory: PathBuf, name: &str) -> Self {
        let name: String = name
            .chars()
            .take(60)
            .map(|character| {
                if character.is_control() || "<>:\"/\\|?*".contains(character) {
                    '_'
                } else {
                    character
                }
            })
            .collect();
        let name = name.trim_matches([' ', '.']);
        Self {
            directory,
            name: if name.is_empty() {
                "terminal".into()
            } else {
                name.into()
            },
        }
    }

    pub fn path(&self, date: NaiveDate) -> PathBuf {
        self.directory
            .join(format!("{}_{}.log", self.name, date.format("%Y-%m-%d")))
    }
}

#[derive(Default)]
pub(super) struct TerminalOptions {
    pub encoding: crate::host::TerminalEncoding,
    pub remote: bool,
    pub shared_shell: bool,
    pub serial: Option<(String, SerialDataModes)>,
    pub log: Option<DailyLog>,
    pub reconnect: TerminalConnectionSettings,
}

#[derive(Clone)]
enum RecordingTarget {
    File(PathBuf),
    Daily(DailyLog),
}

impl RecordingTarget {
    fn path(&self, date: NaiveDate) -> PathBuf {
        match self {
            Self::File(path) => path.clone(),
            Self::Daily(log) => log.path(date),
        }
    }

    fn open(&self, date: NaiveDate) -> io::Result<(PathBuf, File)> {
        let path = self.path(date);
        if let Self::Daily(log) = self {
            std::fs::create_dir_all(&log.directory)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok((path, file))
    }
}

struct LogChunk {
    date: NaiveDate,
    bytes: Vec<u8>,
}

pub(super) struct Recording {
    sender: mpsc::SyncSender<LogChunk>,
    failure: Arc<Mutex<Option<String>>>,
    writer: Option<thread::JoinHandle<()>>,
    target: RecordingTarget,
    path: Arc<Mutex<PathBuf>>,
}

impl Recording {
    pub fn open(path: &Path, hex: bool) -> io::Result<Self> {
        Self::start(RecordingTarget::File(path.to_path_buf()), hex)
    }

    pub fn daily(log: DailyLog, hex: bool) -> io::Result<Self> {
        Self::start(RecordingTarget::Daily(log), hex)
    }

    fn start(target: RecordingTarget, hex: bool) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<LogChunk>(256);
        let failure = Arc::new(Mutex::new(None));
        let failed = failure.clone();
        let date = Local::now().date_naive();
        let path = Arc::new(Mutex::new(target.path(date)));
        let written_path = path.clone();
        let destination = target.clone();
        let writer = thread::Builder::new()
            .name("shellrs-terminal-log".into())
            .spawn(move || {
                let written = (|| -> io::Result<()> {
                    let (mut current, mut file) = destination.open(date)?;
                    while let Ok(chunk) = receiver.recv() {
                        let next = destination.path(chunk.date);
                        if next != current {
                            file.flush()?;
                            let opened = destination.open(chunk.date)?;
                            current = opened.0;
                            file = opened.1;
                            *written_path
                                .lock()
                                .unwrap_or_else(|error| error.into_inner()) = current.clone();
                        }
                        if hex {
                            file.write_all(format_hex(&chunk.bytes).as_bytes())?;
                            file.write_all(b"\r\n")?;
                        } else {
                            file.write_all(&chunk.bytes)?;
                        }
                    }
                    file.flush()
                })();
                if let Err(error) = written {
                    *failed.lock().unwrap_or_else(|error| error.into_inner()) =
                        Some(error.to_string());
                }
            })?;
        Ok(Self {
            sender,
            failure,
            writer: Some(writer),
            target,
            path,
        })
    }

    /// Stop taking data and wait for the writer to flush and close the
    /// file, so it can be truncated or removed safely afterwards.
    pub fn finish(mut self) {
        drop(self.sender);
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

#[derive(Default)]
pub(super) struct SessionIo {
    pub receive_hex: AtomicBool,
    pub send_hex: AtomicBool,
    recording: Mutex<Option<Recording>>,
    stopped_failures: Mutex<Vec<Arc<Mutex<Option<String>>>>>,
    failure: Mutex<Option<String>>,
}

impl SessionIo {
    pub fn set_recording(&self, recording: Recording) {
        self.stop_recording();
        *self
            .recording
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(recording);
    }

    pub fn stop_recording(&self) {
        if let Some(recording) = self
            .recording
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            self.stopped_failures
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(recording.failure.clone());
        }
    }

    pub fn take_recording(&self) -> Option<Recording> {
        self.recording
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    pub fn recording_path(&self) -> Option<PathBuf> {
        self.recording
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|active| {
                active
                    .path
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone()
            })
    }

    pub fn report_error(&self, error: String) {
        *self
            .failure
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(error);
    }

    pub fn is_recording(&self) -> bool {
        self.recording
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
    }

    pub fn record(&self, bytes: &[u8]) {
        let mut recording = self
            .recording
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(active) = recording.as_ref()
            && let Err(error) = active.sender.try_send(LogChunk {
                date: Local::now().date_naive(),
                bytes: bytes.to_vec(),
            })
        {
            let reason = match error {
                mpsc::TrySendError::Full(_) => t!("terminal.recording.overloaded").to_string(),
                mpsc::TrySendError::Disconnected(_) => active
                    .failure
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                    .unwrap_or_else(|| t!("terminal.recording.writer_stopped").to_string()),
            };
            *self
                .failure
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(reason);
            *recording = None;
        }
    }

    pub fn take_error(&self) -> Option<String> {
        let mut error = self
            .failure
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        let mut recording = self
            .recording
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(active) = recording.as_ref() {
            let failed = active
                .failure
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take();
            if failed.is_some() {
                error = failed;
                *recording = None;
            }
        }
        self.stopped_failures
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|failure| {
                if let Some(failed) = failure
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                {
                    error = Some(failed);
                }
                Arc::strong_count(failure) > 1
            });
        error
    }

    pub fn displays_hex(&self) -> bool {
        self.receive_hex.load(Ordering::Acquire)
    }
}

pub(super) fn format_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut text = String::with_capacity(bytes.len().saturating_mul(3));
    for (ix, byte) in bytes.iter().enumerate() {
        if ix > 0 {
            text.push(' ');
        }
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 15) as usize] as char);
    }
    text
}

pub(super) fn clear_log(
    path: &Path,
    previous: Option<Recording>,
    hex: bool,
) -> io::Result<Recording> {
    let target = previous
        .as_ref()
        .map(|previous| previous.target.clone())
        .unwrap_or_else(|| RecordingTarget::File(path.to_path_buf()));
    if let Some(previous) = previous {
        previous.finish();
    }
    OpenOptions::new().write(true).truncate(true).open(path)?;
    Recording::start(target, hex)
}

pub(super) fn delete_log(path: &Path, previous: Option<Recording>) -> io::Result<()> {
    if let Some(previous) = previous {
        previous.finish();
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(super) fn decode_hex(text: &str) -> Result<Vec<u8>, SharedString> {
    if text.len() > 256 * 1024 {
        return Err(t!("serial.hex.too_large"));
    }
    let mut bytes = Vec::new();
    for word in text.split_ascii_whitespace() {
        let digits = word
            .strip_prefix("0x")
            .or_else(|| word.strip_prefix("0X"))
            .unwrap_or(word);
        if digits.is_empty()
            || !digits.len().is_multiple_of(2)
            || !digits.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(t!("serial.hex.invalid"));
        }
        for pair in digits.as_bytes().as_chunks::<2>().0 {
            let digit = |byte: u8| {
                if byte.is_ascii_digit() {
                    byte - b'0'
                } else {
                    byte.to_ascii_lowercase() - b'a' + 10
                }
            };
            bytes.push((digit(pair[0]) << 4) | digit(pair[1]));
        }
    }
    if bytes.is_empty() {
        return Err(t!("serial.hex.empty"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::{DailyLog, LogChunk, Recording, SessionIo, decode_hex, format_hex};
    use chrono::NaiveDate;
    use std::time::{Duration, Instant};

    #[test]
    fn hex_accepts_compact_spaced_and_prefixed_bytes_without_adding_enter() {
        for text in ["00FF0d0A", "00 FF 0d 0A", "0x00 0Xff\n0d\t0A"] {
            assert_eq!(decode_hex(text).unwrap(), [0, 255, 13, 10]);
        }
        for text in ["", " ", "0", "0x", "GG", "FF 0", "你", "01,02"] {
            assert!(decode_hex(text).is_err(), "{text}");
        }
        assert_eq!(format_hex(&[0, 255, 13, 10]), "00 FF 0D 0A");
    }

    #[test]
    fn daily_logs_rotate_by_received_date_and_append_after_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let log = DailyLog::new(dir.path().join("nested"), "COM3");
        let first = NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();
        let next = first.succ_opt().unwrap();
        let recording = Recording::daily(log.clone(), false).unwrap();
        for (date, bytes) in [
            (first, b"day one\n".to_vec()),
            (next, b"day two\n".to_vec()),
        ] {
            recording.sender.send(LogChunk { date, bytes }).unwrap();
        }
        recording.finish();
        assert_eq!(std::fs::read(log.path(first)).unwrap(), b"day one\n");
        assert_eq!(std::fs::read(log.path(next)).unwrap(), b"day two\n");
        let reopened = Recording::daily(log.clone(), false).unwrap();
        reopened
            .sender
            .send(LogChunk {
                date: next,
                bytes: b"reconnected\n".to_vec(),
            })
            .unwrap();
        reopened.finish();
        assert_eq!(
            std::fs::read(log.path(next)).unwrap(),
            b"day two\nreconnected\n"
        );
        assert!(log.path(first).ends_with("COM3_2026-11-05.log"));
    }

    #[test]
    fn daily_log_names_cannot_escape_the_directory_or_contain_windows_path_characters() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "../../COM3",
            "COM3: test|<>?*\r\n",
            "...",
            "开发板",
            &"🦀".repeat(200),
        ] {
            let log = DailyLog::new(dir.path().to_path_buf(), name);
            let path = log.path(date);
            assert_eq!(path.parent(), Some(dir.path()));
            let filename = path.file_name().unwrap().to_str().unwrap();
            assert!(filename.len() <= 255);
            assert!(
                !filename
                    .chars()
                    .any(|character| character.is_control() || "<>:\"/\\|?*".contains(character))
            );
        }
    }

    #[test]
    fn recording_appends_received_bytes_and_hex_without_truncating_the_file() {
        for hex in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("serial.log");
            std::fs::write(&path, b"before\n").unwrap();
            let io = SessionIo::default();
            io.set_recording(Recording::open(&path, hex).unwrap());
            io.record(&[0x41, 0, 255]);
            io.stop_recording();
            assert!(!io.is_recording());
            let expected = if hex {
                b"before\n41 00 FF\r\n".as_slice()
            } else {
                b"before\nA\0\xff".as_slice()
            };
            let deadline = Instant::now() + Duration::from_secs(2);
            while std::fs::read(&path).unwrap() != expected && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(std::fs::read(&path).unwrap(), expected);
            assert!(io.take_error().is_none());
        }
    }
}
