//! Application diagnostics beside the executable, with bounded background writes.

mod files;

use std::{
    backtrace::Backtrace,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use chrono::Local;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::{
    filter::Targets,
    fmt::{MakeWriter, format::Writer, time::FormatTime},
    prelude::*,
};

use files::DailyLog;

const QUEUE_RECORDS: usize = 128;
const RECORD_BYTES: usize = 32 * 1024;
const TRUNCATED: &[u8] = b"\n[log record truncated]\n";
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);

enum Message {
    Record { bytes: Vec<u8>, counted: bool },
    Flush(Sender<()>),
    Shutdown,
}

#[derive(Clone)]
struct LogQueue {
    sender: Sender<Message>,
    pending: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
}

struct EventWriter {
    queue: LogQueue,
    bytes: Vec<u8>,
    truncated: bool,
}

impl Write for EventWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let remaining = (RECORD_BYTES - TRUNCATED.len()).saturating_sub(self.bytes.len());
        self.bytes
            .extend_from_slice(&bytes[..bytes.len().min(remaining)]);
        self.truncated |= bytes.len() > remaining;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for EventWriter {
    fn drop(&mut self) {
        if self.bytes.is_empty() {
            return;
        }
        if self.truncated {
            if let Err(error) = std::str::from_utf8(&self.bytes) {
                self.bytes.truncate(error.valid_up_to());
            }
            self.bytes.extend_from_slice(TRUNCATED);
        }
        self.queue.record(std::mem::take(&mut self.bytes), false);
    }
}

impl<'writer> MakeWriter<'writer> for LogQueue {
    type Writer = EventWriter;

    fn make_writer(&'writer self) -> Self::Writer {
        EventWriter {
            queue: self.clone(),
            bytes: Vec::new(),
            truncated: false,
        }
    }
}

impl LogQueue {
    fn record(&self, mut bytes: Vec<u8>, critical: bool) {
        if bytes.len() > RECORD_BYTES {
            bytes.truncate(RECORD_BYTES - TRUNCATED.len());
            if let Err(error) = std::str::from_utf8(&bytes) {
                bytes.truncate(error.valid_up_to());
            }
            bytes.extend_from_slice(TRUNCATED);
        }
        if !critical
            && self
                .pending
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |pending| {
                    (pending < QUEUE_RECORDS).then_some(pending + 1)
                })
                .is_err()
        {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if self
            .sender
            .send(Message::Record {
                bytes,
                counted: !critical,
            })
            .is_err()
            && !critical
        {
            self.pending.fetch_sub(1, Ordering::Relaxed);
        }
    }

    fn flush(&self) {
        let (sender, receiver) = mpsc::channel();
        if self.sender.send(Message::Flush(sender)).is_ok() {
            let _ = receiver.recv_timeout(Duration::from_secs(3));
        }
    }
}

/// Keeps background logging alive; dropping it drains queued records on normal exit.
pub struct LogGuard {
    queue: LogQueue,
    worker: Option<JoinHandle<()>>,
}

impl LogGuard {
    /// Flush pending diagnostics before an explicit process exit.
    pub fn flush(&self) {
        self.queue.flush();
    }
}

impl Drop for LogGuard {
    fn drop(&mut self) {
        let _ = self.queue.sender.send(Message::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct LocalTime;

impl FormatTime for LocalTime {
    fn format_time(&self, writer: &mut Writer<'_>) -> std::fmt::Result {
        write!(
            writer,
            "{}",
            Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z")
        )
    }
}

/// The `log` folder beside the running executable, independent of its data folder.
pub fn log_dir() -> io::Result<PathBuf> {
    beside_executable(&std::env::current_exe()?)
}

fn beside_executable(executable: &Path) -> io::Result<PathBuf> {
    executable
        .parent()
        .map(|parent| parent.join("log"))
        .ok_or_else(|| io::Error::other("the executable has no parent directory"))
}

fn targets(level: Option<&str>) -> Targets {
    let level = level
        .and_then(|level| level.parse::<LevelFilter>().ok())
        .unwrap_or(LevelFilter::INFO);
    Targets::new().with_default(LevelFilter::WARN).with_targets(
        [
            "shellrs",
            "shellrs_lan",
            "shellrs_ssh",
            "shellrs_hosts",
            "shellrs_sync",
        ]
        .map(|target| (target, level)),
    )
}

/// Install daily file diagnostics and panic capture before starting the application.
/// Failure to create the log directory falls back to stderr without preventing startup.
pub fn init() -> Option<LogGuard> {
    let level = std::env::var("SHELLLF_LOG").ok();
    let started = log_dir().and_then(start_worker);
    let guard = match started {
        Ok(guard) => guard,
        Err(error) => {
            eprintln!("ShellLF: application file logging is unavailable: {error}");
            let _ = tracing_subscriber::fmt()
                .with_ansi(false)
                .with_timer(LocalTime)
                .with_writer(io::stderr)
                .with_max_level(LevelFilter::WARN)
                .try_init();
            return None;
        }
    };
    install(guard, level.as_deref())
}

fn install(guard: LogGuard, level: Option<&str>) -> Option<LogGuard> {
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_timer(LocalTime)
            .with_thread_ids(true)
            .with_writer(guard.queue.clone())
            .with_filter(targets(level)),
    );
    if let Err(error) = subscriber.try_init() {
        eprintln!("ShellLF: cannot install application diagnostics: {error}");
        return None;
    }
    let queue = guard.queue.clone();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let record = format!(
            "{} ERROR shellrs::panic thread={:?} {info}\n{}\n",
            Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z"),
            thread::current().name(),
            Backtrace::force_capture()
        );
        queue.record(record.into_bytes(), true);
        if thread::current().name() != Some("shelllf-logging") {
            queue.flush();
        }
        previous(info);
    }));
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        platform = std::env::consts::OS,
        architecture = std::env::consts::ARCH,
        process_id = std::process::id(),
        "ShellLF starting"
    );
    Some(guard)
}

fn start_worker(directory: PathBuf) -> io::Result<LogGuard> {
    let log = DailyLog::new(directory, Local::now().date_naive(), std::process::id())?;
    let (sender, receiver) = mpsc::channel();
    let queue = LogQueue {
        sender,
        pending: Arc::new(AtomicUsize::new(0)),
        dropped: Arc::new(AtomicUsize::new(0)),
    };
    let pending = queue.pending.clone();
    let dropped = queue.dropped.clone();
    let worker = thread::Builder::new()
        .name("shelllf-logging".into())
        .spawn(move || run_worker(log, receiver, pending, dropped))?;
    Ok(LogGuard {
        queue,
        worker: Some(worker),
    })
}

fn run_worker(
    mut log: DailyLog,
    receiver: Receiver<Message>,
    pending: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
) {
    let mut reported_failure = false;
    loop {
        let message = receiver.recv_timeout(MAINTENANCE_INTERVAL);
        let date = Local::now().date_naive();
        let missed = dropped.swap(0, Ordering::Relaxed);
        if missed > 0 {
            let record = format!(
                "{} WARN shellrs::app::logging: dropped {missed} log records because the queue was full\n",
                Local::now().to_rfc3339()
            );
            let _ = log.write(date, record.as_bytes());
        }
        let result = match message {
            Ok(Message::Record { bytes, counted }) => {
                if counted {
                    pending.fetch_sub(1, Ordering::Relaxed);
                }
                log.write(date, &bytes)
            }
            Ok(Message::Flush(sender)) => {
                let result = log.flush();
                let _ = sender.send(());
                result
            }
            Ok(Message::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = log.flush();
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => log.maintain(date),
        };
        match result {
            Ok(()) => reported_failure = false,
            Err(error) if !reported_failure => {
                eprintln!("ShellLF: cannot write application diagnostics: {error}");
                reported_failure = true;
            }
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests;
