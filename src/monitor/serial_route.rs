use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

use super::{MonitorConnector, MonitorLink};
use crate::{
    adb::AdbConnection,
    host::{HostLogin, MonitorChannel, MonitorSettings},
    i18n::t,
    terminal::{ExecRequest, ExecResult, TerminalTransportCommand},
};

const RETRY: Duration = Duration::from_secs(2);
const MAX_RETRY: Duration = Duration::from_secs(30);
#[cfg(test)]
#[path = "serial_route_tests.rs"]
mod tests;
type RawSender = mpsc::Sender<TerminalTransportCommand>;

struct AdbRecovery {
    device: AdbConnection,
}

struct RetrySchedule {
    initial: Duration,
    delay: Duration,
    at: Option<Instant>,
}

impl RetrySchedule {
    fn new(initial: Duration) -> Self {
        Self {
            initial,
            delay: initial,
            at: None,
        }
    }

    fn is_due(&self) -> bool {
        self.at.is_none_or(|at| Instant::now() >= at)
    }

    fn failed(&mut self) -> Instant {
        let at = Instant::now() + self.delay;
        self.at = Some(at);
        self.delay = self.delay.saturating_mul(2).min(MAX_RETRY);
        at
    }

    fn reset(&mut self) {
        self.delay = self.initial;
        self.at = None;
    }
}

#[derive(Clone)]
struct PendingRetry {
    at: Instant,
    kind: &'static str,
    detail: String,
}

impl PendingRetry {
    fn message(&self) -> String {
        let seconds = self
            .at
            .saturating_duration_since(Instant::now())
            .as_secs_f64()
            .ceil() as u64;
        let retry = t!("monitor.route.retry_in", seconds = seconds);
        t!(self.kind, retry = retry).to_string()
    }
}

#[derive(Clone, Default)]
pub(crate) struct RouteStatus {
    pub label: String,
    pub adb: Option<AdbConnection>,
    pub ssh: Option<HostLogin>,
    retry: Option<PendingRetry>,
}

impl RouteStatus {
    pub(crate) fn problem(&self) -> Option<(String, String)> {
        self.retry
            .as_ref()
            .map(|retry| (retry.message(), retry.detail.clone()))
    }
}

enum Job {
    Execute {
        raw: Option<RawSender>,
        command: String,
        serial_command: Option<String>,
        independent_only: bool,
        polling: bool,
        reply: mpsc::Sender<ExecResult>,
    },
    Adb(AdbConnection),
    Ssh {
        id: String,
        login: Box<HostLogin>,
    },
    Retry,
}

pub(crate) struct SerialServices {
    jobs: mpsc::SyncSender<Job>,
    status: Arc<Mutex<RouteStatus>>,
    cancel: Arc<AtomicBool>,
}

impl SerialServices {
    pub(crate) fn new(
        connector: Arc<dyn MonitorConnector>,
        settings: MonitorSettings,
        candidates: Vec<(String, HostLogin)>,
    ) -> Arc<Self> {
        Self::with_retry_interval(connector, settings, candidates, RETRY)
    }

    fn with_retry_interval(
        connector: Arc<dyn MonitorConnector>,
        mut settings: MonitorSettings,
        mut candidates: Vec<(String, HostLogin)>,
        retry_interval: Duration,
    ) -> Arc<Self> {
        let (jobs, receiver) = mpsc::sync_channel(32);
        let service = Arc::new(Self {
            jobs,
            status: Arc::new(Mutex::new(RouteStatus::default())),
            cancel: Arc::new(AtomicBool::new(false)),
        });
        let status = service.status.clone();
        let cancel = service.cancel.clone();
        std::thread::spawn(move || {
            let mut link: Option<Box<dyn MonitorLink>> = None;
            let mut identity = None;
            let mut retry = RetrySchedule::new(retry_interval);
            let mut problem = None;
            let mut recovery: Option<AdbRecovery> = None;
            loop {
                if cancel.load(Ordering::Acquire) {
                    break;
                }
                let job = match receiver.recv_timeout(Duration::from_millis(250)) {
                    Ok(job) => job,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                if cancel.load(Ordering::Acquire) {
                    break;
                }
                match job {
                    Job::Ssh { id, login } => {
                        let login = *login;
                        settings.channel = MonitorChannel::Ssh;
                        settings.ssh_host = id.clone();
                        let label = format!("SSH · {}@{}:{}", login.user, login.host, login.port);
                        candidates = vec![(id, login.clone())];
                        link = None;
                        recovery = None;
                        retry.reset();
                        problem = None;
                        *status.lock().unwrap_or_else(|error| error.into_inner()) = RouteStatus {
                            label,
                            adb: None,
                            ssh: Some(login),
                            retry: None,
                        };
                    }
                    Job::Adb(device) => {
                        *status.lock().unwrap_or_else(|error| error.into_inner()) = RouteStatus {
                            label: device.label(),
                            adb: Some(device.clone()),
                            ssh: None,
                            retry: None,
                        };
                        link = Some(Box::new(device));
                        recovery = None;
                        retry.reset();
                        problem = None;
                    }
                    Job::Retry => {
                        retry.reset();
                        if let Some(pending) = &mut status
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .retry
                        {
                            pending.at = Instant::now();
                        }
                    }
                    Job::Execute {
                        raw,
                        command,
                        serial_command,
                        independent_only,
                        polling,
                        reply,
                    } => {
                        if retry.is_due()
                            && let Some(pending) = recovery.take()
                        {
                            match connector.reconnect_adb(&pending.device, &cancel) {
                                Ok(selected) => {
                                    *status.lock().unwrap_or_else(|error| error.into_inner()) =
                                        RouteStatus {
                                            label: selected.label(),
                                            adb: selected.adb_connection(),
                                            ssh: selected.ssh_login(),
                                            retry: None,
                                        };
                                    link = Some(selected);
                                    problem = None;
                                    retry.reset();
                                }
                                Err(error) => {
                                    // Schedule from the end of the attempt, so a slow
                                    // offline device cannot cause a burst of retries.
                                    let at = retry.failed();
                                    status
                                        .lock()
                                        .unwrap_or_else(|error| error.into_inner())
                                        .retry = Some(PendingRetry {
                                        at,
                                        kind: "monitor.route.adb_recovering",
                                        detail: error.clone(),
                                    });
                                    problem = Some(error);
                                    recovery = Some(pending);
                                }
                            }
                        }
                        if link.is_none()
                            && recovery.is_none()
                            && retry.is_due()
                            && settings.channel != MonitorChannel::Serial
                        {
                            *status.lock().unwrap_or_else(|error| error.into_inner()) =
                                RouteStatus {
                                    label: t!("monitor.route.detecting").to_string(),
                                    adb: None,
                                    ssh: None,
                                    retry: None,
                                };
                            if raw.is_some() {
                                let fresh = raw_command(
                                    raw.as_ref(),
                                    crate::serial::files::NETWORK_COMMAND,
                                    &cancel,
                                    true,
                                )
                                .ok();
                                if fresh.is_some() {
                                    identity = fresh;
                                }
                            }
                            match connector.connect(
                                identity.as_deref().unwrap_or(""),
                                &settings,
                                &candidates,
                                &cancel,
                            ) {
                                Ok(Some(selected)) => {
                                    *status.lock().unwrap_or_else(|error| error.into_inner()) =
                                        RouteStatus {
                                            label: selected.label(),
                                            adb: selected.adb_connection(),
                                            ssh: selected.ssh_login(),
                                            retry: None,
                                        };
                                    link = Some(selected);
                                    problem = None;
                                    retry.reset();
                                }
                                Ok(None) => {
                                    problem = Some(
                                        match settings.channel {
                                            MonitorChannel::Ssh => {
                                                t!("monitor.route.ssh_unavailable")
                                            }
                                            MonitorChannel::Adb => {
                                                t!("monitor.route.adb_unmatched")
                                            }
                                            _ => t!("monitor.route.not_selected"),
                                        }
                                        .to_string(),
                                    )
                                }
                                Err(error) => problem = Some(error),
                            }
                            if link.is_none() {
                                let at = retry.failed();
                                status
                                    .lock()
                                    .unwrap_or_else(|error| error.into_inner())
                                    .retry = Some(PendingRetry {
                                    at,
                                    kind: "monitor.route.detection_failed",
                                    detail: problem.clone().unwrap_or_default(),
                                });
                            }
                        }
                        let result = if cancel.load(Ordering::Acquire) {
                            Err(t!("adb.interrupted").to_string())
                        } else if let Some(selected) = &mut link {
                            let result = selected.sample(&command, &cancel);
                            if let Err(error) = &result
                                && selected.sample(":", &cancel).is_err()
                            {
                                recovery = selected
                                    .adb_connection()
                                    .map(|device| AdbRecovery { device });
                                // Reconnect the chosen SSH host, never a different board
                                // found by a fresh automatic scan after a disconnect.
                                if let Some(login) = selected.ssh_login() {
                                    let id = candidates
                                        .iter()
                                        .find(|(_, candidate)| candidate == &login)
                                        .map(|(id, _)| id.clone())
                                        .unwrap_or_else(|| "selected-ssh".to_string());
                                    settings.channel = MonitorChannel::Ssh;
                                    settings.ssh_host = id.clone();
                                    candidates = vec![(id, login)];
                                    // The selected, trusted SSH endpoint stays
                                    // fixed, but its boot id may change. Never
                                    // carry an old serial boot id into recovery
                                    // while the serial console is still busy.
                                    identity = None;
                                }
                                link = None;
                                let at = retry.failed();
                                let message = if recovery.is_some() {
                                    t!("monitor.route.adb_reconnecting")
                                } else {
                                    t!("monitor.route.connection_lost")
                                }
                                .to_string();
                                problem = Some(message.clone());
                                *status.lock().unwrap_or_else(|error| error.into_inner()) =
                                    RouteStatus {
                                        label: message,
                                        adb: None,
                                        ssh: None,
                                        retry: Some(PendingRetry {
                                            at,
                                            kind: if recovery.is_some() {
                                                "monitor.route.adb_recovering"
                                            } else {
                                                "monitor.route.retrying"
                                            },
                                            detail: error.clone(),
                                        }),
                                    };
                            }
                            result
                        } else if recovery.is_some() {
                            // Keep the established ADB choice while it boots.
                            // Do not inject reads into its serial boot console.
                            Err(t!("monitor.route.adb_reconnecting").to_string())
                        } else if !independent_only
                            && (settings.channel == MonitorChannel::Serial
                                || (settings.channel == MonitorChannel::Auto && identity.is_some()))
                        {
                            status
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .label = t!("monitor.route.serial").to_string();
                            let result = raw_command(
                                raw.as_ref(),
                                serial_command.as_deref().unwrap_or(&command),
                                &cancel,
                                polling,
                            );
                            // A usable serial fallback is not an offline error.
                            // Keep the discovery schedule for future reads, but
                            // don't show a countdown on a successfully loaded
                            // view that no longer needs to poll.
                            if result.is_ok()
                                || matches!(&result, Err(error) if error == crate::terminal::MONITOR_PAUSED)
                            {
                                status
                                    .lock()
                                    .unwrap_or_else(|error| error.into_inner())
                                    .retry = None;
                            }
                            result
                        } else {
                            let error = problem
                                .clone()
                                .unwrap_or_else(|| t!("monitor.route.not_selected").to_string());
                            Err(error)
                        };
                        let _ = reply.send(result);
                    }
                }
            }
        });
        service
    }

    pub(crate) fn execute(
        &self,
        raw: Option<RawSender>,
        command: String,
    ) -> mpsc::Receiver<ExecResult> {
        self.send(raw, command, None, false, false)
    }

    pub(crate) fn execute_with_serial(
        &self,
        raw: Option<RawSender>,
        command: String,
        serial_command: Option<String>,
    ) -> mpsc::Receiver<ExecResult> {
        self.send(raw, command, serial_command, false, true)
    }

    fn send(
        &self,
        raw: Option<RawSender>,
        command: String,
        serial_command: Option<String>,
        independent_only: bool,
        polling: bool,
    ) -> mpsc::Receiver<ExecResult> {
        let (reply, receiver) = mpsc::channel();
        if self.cancel.load(Ordering::Acquire) {
            let _ = reply.send(Err(t!("adb.interrupted").to_string()));
        } else if let Err(error) = self.jobs.try_send(Job::Execute {
            raw,
            command,
            serial_command,
            independent_only,
            polling,
            reply,
        }) && let Job::Execute { reply, .. } = match error {
            mpsc::TrySendError::Full(job) | mpsc::TrySendError::Disconnected(job) => job,
        } {
            let _ = reply.send(Err(t!("monitor.route.queue_busy").to_string()));
        }
        receiver
    }

    pub(crate) fn adopt_adb(&self, device: AdbConnection) {
        self.adopt(Job::Adb(device));
    }

    pub(crate) fn adopt_ssh(&self, id: String, login: HostLogin) {
        self.adopt(Job::Ssh {
            id,
            login: Box::new(login),
        });
    }

    /// Reset only the retry delay; preserve the selected device and never
    /// replay the failed command (it may have been a service control).
    pub(crate) fn retry_now(&self) {
        self.adopt(Job::Retry);
    }

    fn adopt(&self, job: Job) {
        if self.jobs.try_send(job).is_err() {
            *self
                .status
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = RouteStatus {
                label: t!("monitor.route.queue_busy").to_string(),
                adb: None,
                ssh: None,
                retry: None,
            };
        }
    }

    pub(crate) fn status(&self) -> RouteStatus {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Some(retry) = &status.retry
            && status.label != t!("monitor.route.serial").as_ref()
        {
            status.label = retry.message();
        }
        status
    }

    pub(crate) fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub(crate) fn monitor_link(self: &Arc<Self>, raw: Option<RawSender>) -> Box<dyn MonitorLink> {
        Box::new(SharedLink {
            service: self.clone(),
            raw,
        })
    }
}

impl Drop for SerialServices {
    fn drop(&mut self) {
        self.cancel();
    }
}

struct SharedLink {
    service: Arc<SerialServices>,
    raw: Option<RawSender>,
}
impl MonitorLink for SharedLink {
    fn ssh_login(&self) -> Option<HostLogin> {
        self.service.status().ssh
    }
    fn label(&self) -> String {
        self.service.status().label
    }
    fn adb_connection(&self) -> Option<AdbConnection> {
        self.service.status().adb
    }
    fn sample(&mut self, command: &str, cancel: &AtomicBool) -> Result<String, String> {
        wait(
            self.service
                .send(self.raw.clone(), command.to_string(), None, true, true),
            cancel,
            Duration::from_secs(60),
        )
    }
}

fn raw_command(
    raw: Option<&RawSender>,
    command: &str,
    cancel: &AtomicBool,
    polling: bool,
) -> ExecResult {
    if cancel.load(Ordering::Acquire) {
        return Err(t!("adb.interrupted").to_string());
    }
    let raw = raw.ok_or_else(|| crate::terminal::MONITOR_PAUSED.to_string())?;
    let (reply, receiver) = mpsc::channel();
    let request = ExecRequest {
        command: command.to_string(),
        reply,
    };
    raw.send(if polling {
        TerminalTransportCommand::Monitor(request)
    } else {
        TerminalTransportCommand::Exec(request)
    })
    .map_err(|_| t!("tools.not_connected").to_string())?;
    wait(
        receiver,
        cancel,
        Duration::from_secs(if polling { 8 } else { 60 }),
    )
}

fn wait(
    receiver: mpsc::Receiver<ExecResult>,
    cancel: &AtomicBool,
    timeout: Duration,
) -> ExecResult {
    let started = std::time::Instant::now();
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(t!("adb.interrupted").to_string());
        }
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(t!("tools.not_connected").to_string());
            }
            Err(mpsc::RecvTimeoutError::Timeout) if started.elapsed() >= timeout => {
                return Err(t!("monitor.route.timed_out").to_string());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}
