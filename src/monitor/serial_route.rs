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

const MAX_ATTEMPTS: u8 = 2;
const ADB_RETRY: Duration = Duration::from_secs(2);
#[cfg(test)]
#[path = "serial_route_tests.rs"]
mod tests;
type RawSender = mpsc::Sender<TerminalTransportCommand>;

struct AdbRecovery {
    device: AdbConnection,
    retry_at: Instant,
}

#[derive(Clone, Default)]
pub(crate) struct RouteStatus {
    pub label: String,
    pub adb: Option<AdbConnection>,
    pub ssh: Option<HostLogin>,
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
        Self::with_retry_interval(connector, settings, candidates, ADB_RETRY)
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
            let mut attempts = 0;
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
                        attempts = 0;
                        problem = None;
                        *status.lock().unwrap_or_else(|error| error.into_inner()) = RouteStatus {
                            label,
                            adb: None,
                            ssh: Some(login),
                        };
                    }
                    Job::Adb(device) => {
                        *status.lock().unwrap_or_else(|error| error.into_inner()) = RouteStatus {
                            label: device.label(),
                            adb: Some(device.clone()),
                            ssh: None,
                        };
                        link = Some(Box::new(device));
                        recovery = None;
                        attempts = MAX_ATTEMPTS;
                        problem = None;
                    }
                    Job::Execute {
                        raw,
                        command,
                        serial_command,
                        independent_only,
                        polling,
                        reply,
                    } => {
                        if recovery
                            .as_ref()
                            .is_some_and(|pending| Instant::now() >= pending.retry_at)
                        {
                            let mut pending = recovery.take().unwrap();
                            match connector.reconnect_adb(&pending.device, &cancel) {
                                Ok(selected) => {
                                    *status.lock().unwrap_or_else(|error| error.into_inner()) =
                                        RouteStatus {
                                            label: selected.label(),
                                            adb: selected.adb_connection(),
                                            ssh: selected.ssh_login(),
                                        };
                                    link = Some(selected);
                                    problem = None;
                                }
                                Err(_) => {
                                    // Schedule from the end of the attempt, so a slow
                                    // offline device cannot cause a burst of retries.
                                    pending.retry_at = Instant::now() + retry_interval;
                                    recovery = Some(pending);
                                }
                            }
                        }
                        if link.is_none()
                            && recovery.is_none()
                            && attempts < MAX_ATTEMPTS
                            && settings.channel != MonitorChannel::Serial
                        {
                            attempts += 1;
                            *status.lock().unwrap_or_else(|error| error.into_inner()) =
                                RouteStatus {
                                    label: t!("monitor.route.detecting").to_string(),
                                    adb: None,
                                    ssh: None,
                                };
                            if identity.is_none() {
                                identity = raw_command(
                                    raw.as_ref(),
                                    crate::serial::files::NETWORK_COMMAND,
                                    &cancel,
                                    true,
                                )
                                .ok();
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
                                        };
                                    link = Some(selected);
                                    problem = None;
                                }
                                Ok(None) => {
                                    problem = Some(t!("monitor.route.unavailable").to_string())
                                }
                                Err(error) => problem = Some(error),
                            }
                        }
                        let result = if cancel.load(Ordering::Acquire) {
                            Err(t!("adb.interrupted").to_string())
                        } else if let Some(selected) = &mut link {
                            let result = selected.sample(&command, &cancel);
                            if result.is_err() && selected.sample(":", &cancel).is_err() {
                                recovery = selected.adb_connection().map(|device| AdbRecovery {
                                    device,
                                    retry_at: Instant::now() + retry_interval,
                                });
                                link = None;
                                attempts = MAX_ATTEMPTS;
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
                            *status.lock().unwrap_or_else(|error| error.into_inner()) =
                                RouteStatus {
                                    label: t!("monitor.route.serial").to_string(),
                                    adb: None,
                                    ssh: None,
                                };
                            raw_command(
                                raw.as_ref(),
                                serial_command.as_deref().unwrap_or(&command),
                                &cancel,
                                polling,
                            )
                        } else {
                            let error = problem
                                .clone()
                                .unwrap_or_else(|| t!("monitor.route.not_selected").to_string());
                            *status.lock().unwrap_or_else(|error| error.into_inner()) =
                                RouteStatus {
                                    label: error.clone(),
                                    adb: None,
                                    ssh: None,
                                };
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

    fn adopt(&self, job: Job) {
        if self.jobs.try_send(job).is_err() {
            *self
                .status
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = RouteStatus {
                label: t!("monitor.route.queue_busy").to_string(),
                adb: None,
                ssh: None,
            };
        }
    }

    pub(crate) fn status(&self) -> RouteStatus {
        self.status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
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
