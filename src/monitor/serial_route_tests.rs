use super::*;
use std::sync::atomic::AtomicUsize;

#[derive(Default)]
struct Provider {
    discoveries: AtomicUsize,
    unavailable: AtomicBool,
    disconnected: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<String>>>,
    channels: Mutex<Vec<MonitorChannel>>,
}

struct Link {
    disconnected: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<String>>>,
}
impl MonitorLink for Link {
    fn label(&self) -> String {
        "ADB · matched-board".into()
    }
    fn sample(&mut self, command: &str, _: &AtomicBool) -> ExecResult {
        self.requests.lock().unwrap().push(command.to_string());
        if self.disconnected.load(Ordering::Acquire) {
            Err("disconnected".into())
        } else {
            Ok(command.to_string())
        }
    }
}
impl MonitorConnector for Provider {
    fn connect(
        &self,
        _: &str,
        settings: &MonitorSettings,
        _: &[(String, HostLogin)],
        _: &AtomicBool,
    ) -> Result<Option<Box<dyn MonitorLink>>, String> {
        self.discoveries.fetch_add(1, Ordering::SeqCst);
        self.channels.lock().unwrap().push(settings.channel);
        if self.unavailable.load(Ordering::Acquire) {
            return Err("SSH and ADB unavailable".into());
        }
        Ok(Some(Box::new(Link {
            disconnected: self.disconnected.clone(),
            requests: self.requests.clone(),
        })))
    }
}

fn result(receiver: mpsc::Receiver<ExecResult>) -> ExecResult {
    receiver.recv_timeout(Duration::from_secs(3)).unwrap()
}

#[test]
fn all_tools_reuse_one_route_and_disconnect_retries_are_throttled() {
    let provider = Arc::new(Provider::default());
    let service = SerialServices::new(provider.clone(), MonitorSettings::default(), vec![]);
    for command in ["files", "services", "processes", "network", "monitor"] {
        assert_eq!(
            result(service.execute(None, command.into())).unwrap(),
            command
        );
    }
    assert_eq!(provider.discoveries.load(Ordering::SeqCst), 1);
    provider.disconnected.store(true, Ordering::Release);
    assert!(result(service.execute(None, "read".into())).is_err());
    for _ in 0..5 {
        assert!(result(service.execute(None, "poll".into())).is_err());
    }
    assert_eq!(provider.discoveries.load(Ordering::SeqCst), 1);
    assert_eq!(
        provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|command| *command == "read")
            .count(),
        1
    );
}

#[test]
fn discovery_keeps_retrying_past_two_failures_and_recovers_without_reopening() {
    let provider = Arc::new(Provider::default());
    provider.unavailable.store(true, Ordering::Release);
    let service = SerialServices::with_retry_interval(
        provider.clone(),
        MonitorSettings::default(),
        vec![],
        Duration::ZERO,
    );
    for _ in 0..10 {
        assert!(result(service.execute(None, "read".into())).is_err());
    }
    assert_eq!(provider.discoveries.load(Ordering::SeqCst), 10);
    assert!(service.status().problem().is_some());
    provider.unavailable.store(false, Ordering::Release);
    assert_eq!(
        result(service.execute(None, "recovered".into())).unwrap(),
        "recovered"
    );
    assert_eq!(provider.discoveries.load(Ordering::SeqCst), 11);
    assert!(service.status().problem().is_none());
    service.cancel();
    assert!(result(service.execute(None, "old-generation".into())).is_err());
    let next = SerialServices::new(provider.clone(), MonitorSettings::default(), vec![]);
    provider.unavailable.store(false, Ordering::Release);
    assert_eq!(
        result(next.execute(None, "new-generation".into())).unwrap(),
        "new-generation"
    );
    assert_eq!(provider.discoveries.load(Ordering::SeqCst), 12);
}

#[test]
fn serial_identity_and_negative_discovery_are_shared_across_tools() {
    let provider = Arc::new(Provider::default());
    provider.unavailable.store(true, Ordering::Release);
    let (sender, receiver) = mpsc::channel();
    let identities = Arc::new(AtomicUsize::new(0));
    let count = identities.clone();
    let serial = std::thread::spawn(move || {
        while let Ok(command) = receiver.recv() {
            let request = match command {
                TerminalTransportCommand::Monitor(request)
                | TerminalTransportCommand::Exec(request) => request,
                _ => panic!("unexpected serial command"),
            };
            if request.command == crate::serial::files::NETWORK_COMMAND {
                count.fetch_add(1, Ordering::SeqCst);
                request
                    .reply
                    .send(Ok(
                        "@@boot_id\n00000000-0000-4000-8000-000000000001\n".into()
                    ))
                    .unwrap();
            } else {
                request.reply.send(Ok(request.command)).unwrap();
            }
        }
    });
    let service = SerialServices::new(provider.clone(), MonitorSettings::default(), vec![]);
    for command in ["services", "processes", "network", "monitor", "services"] {
        assert_eq!(
            result(service.execute(Some(sender.clone()), command.into())).unwrap(),
            command
        );
    }
    assert_eq!(provider.discoveries.load(Ordering::SeqCst), 1);
    assert_eq!(identities.load(Ordering::SeqCst), 1);
    drop(service);
    drop(sender);
    serial.join().unwrap();
}

#[test]
fn an_explicit_adb_from_files_skips_all_discovery_and_ssh_choices_are_honored() {
    let provider = Arc::new(Provider::default());
    let service = SerialServices::new(provider.clone(), MonitorSettings::default(), vec![]);
    service.adopt_adb(AdbConnection::monitor_test_connection("board-a", None));
    assert!(result(service.execute(None, "sample".into())).is_err());
    assert_eq!(provider.discoveries.load(Ordering::SeqCst), 0);
    service.adopt_ssh(
        "chosen-host".into(),
        HostLogin::new(
            "192.0.2.4",
            2222,
            "root",
            crate::host::LoginMethod::NoPassword,
        ),
    );
    assert_eq!(
        result(service.execute(None, "services".into())).unwrap(),
        "services"
    );
    assert_eq!(*provider.channels.lock().unwrap(), [MonitorChannel::Ssh]);
}

#[derive(Default)]
struct RecoveringProvider {
    inner: Provider,
    recoveries: AtomicUsize,
    selected_serials: Mutex<Vec<String>>,
}

struct RecoveringLink(Link);

impl MonitorLink for RecoveringLink {
    fn label(&self) -> String {
        self.0.label()
    }
    fn adb_connection(&self) -> Option<AdbConnection> {
        Some(AdbConnection::monitor_test_connection(
            "selected-board",
            None,
        ))
    }
    fn sample(&mut self, command: &str, cancel: &AtomicBool) -> ExecResult {
        self.0.sample(command, cancel)
    }
}

impl RecoveringProvider {
    fn link(&self) -> Box<dyn MonitorLink> {
        Box::new(RecoveringLink(Link {
            disconnected: self.inner.disconnected.clone(),
            requests: self.inner.requests.clone(),
        }))
    }
}

impl MonitorConnector for RecoveringProvider {
    fn connect(
        &self,
        _: &str,
        _: &MonitorSettings,
        _: &[(String, HostLogin)],
        _: &AtomicBool,
    ) -> Result<Option<Box<dyn MonitorLink>>, String> {
        self.inner.discoveries.fetch_add(1, Ordering::SeqCst);
        Ok(Some(self.link()))
    }
    fn reconnect_adb(
        &self,
        device: &AdbConnection,
        cancel: &AtomicBool,
    ) -> Result<Box<dyn MonitorLink>, String> {
        self.recoveries.fetch_add(1, Ordering::SeqCst);
        self.selected_serials
            .lock()
            .unwrap()
            .push(device.serial().into());
        if self.inner.disconnected.load(Ordering::Acquire) || cancel.load(Ordering::Acquire) {
            Err("offline".into())
        } else {
            Ok(self.link())
        }
    }
}

#[test]
fn selected_adb_recovers_after_repeated_offline_reads_without_general_discovery_or_replay() {
    let provider = Arc::new(RecoveringProvider::default());
    let service = SerialServices::with_retry_interval(
        provider.clone(),
        MonitorSettings::default(),
        vec![],
        Duration::ZERO,
    );
    assert!(result(service.execute(None, "initial".into())).is_ok());
    provider.inner.disconnected.store(true, Ordering::Release);
    assert!(result(service.execute(None, "control-once".into())).is_err());
    for _ in 0..4 {
        assert!(result(service.execute(None, "poll".into())).is_err());
    }
    assert!(service.status().adb.is_none());
    provider.inner.disconnected.store(false, Ordering::Release);
    for command in ["monitor", "services", "processes", "network"] {
        assert_eq!(
            result(service.execute(None, command.into())).unwrap(),
            command
        );
    }
    assert_eq!(provider.inner.discoveries.load(Ordering::SeqCst), 1);
    assert_eq!(provider.recoveries.load(Ordering::SeqCst), 5);
    assert!(
        provider
            .selected_serials
            .lock()
            .unwrap()
            .iter()
            .all(|serial| serial == "selected-board")
    );
    assert_eq!(service.status().adb.unwrap().serial(), "selected-board");
    assert_eq!(
        provider
            .inner
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|command| *command == "control-once")
            .count(),
        1
    );
    service.cancel();
    assert!(result(service.execute(None, "after-close".into())).is_err());
    assert_eq!(provider.recoveries.load(Ordering::SeqCst), 5);
}

#[test]
fn adb_recovery_is_throttled_and_manual_ssh_selection_cancels_it() {
    let provider = Arc::new(RecoveringProvider::default());
    let service = SerialServices::with_retry_interval(
        provider.clone(),
        MonitorSettings::default(),
        vec![],
        Duration::from_secs(3600),
    );
    assert!(result(service.execute(None, "initial".into())).is_ok());
    provider.inner.disconnected.store(true, Ordering::Release);
    assert!(result(service.execute(None, "lost".into())).is_err());
    for _ in 0..10 {
        assert!(result(service.execute(None, "poll".into())).is_err());
    }
    assert_eq!(provider.recoveries.load(Ordering::SeqCst), 0);
    service.adopt_ssh(
        "manual-host".into(),
        HostLogin::new(
            "192.0.2.4",
            22,
            "root",
            crate::host::LoginMethod::NoPassword,
        ),
    );
    provider.inner.disconnected.store(false, Ordering::Release);
    assert_eq!(
        result(service.execute(None, "manual".into())).unwrap(),
        "manual"
    );
    assert_eq!(provider.inner.discoveries.load(Ordering::SeqCst), 2);
    assert_eq!(provider.recoveries.load(Ordering::SeqCst), 0);
}

#[test]
fn retry_delays_back_off_to_thirty_seconds_and_reset_after_recovery() {
    let mut retry = RetrySchedule::new(RETRY);
    for seconds in [2, 4, 8, 16, 30, 30, 30] {
        let before = Instant::now();
        let at = retry.failed();
        assert!(at >= before + Duration::from_secs(seconds));
        assert!(at <= Instant::now() + Duration::from_secs(seconds));
        assert!(!retry.is_due());
    }
    retry.reset();
    assert!(retry.is_due());
    let at = retry.failed();
    assert!(at <= Instant::now() + RETRY);
}

#[test]
fn retry_now_bypasses_discovery_backoff_without_recreating_the_service() {
    let provider = Arc::new(Provider::default());
    provider.unavailable.store(true, Ordering::Release);
    let service = SerialServices::new(provider.clone(), MonitorSettings::default(), vec![]);
    for _ in 0..5 {
        assert!(result(service.execute(None, "read".into())).is_err());
    }
    assert_eq!(provider.discoveries.load(Ordering::SeqCst), 1);
    let (message, detail) = service.status().problem().unwrap();
    assert!(message.contains('2'));
    assert_eq!(detail, "SSH and ADB unavailable");
    provider.unavailable.store(false, Ordering::Release);
    service.retry_now();
    assert_eq!(
        result(service.execute(None, "read".into())).unwrap(),
        "read"
    );
    assert_eq!(provider.discoveries.load(Ordering::SeqCst), 2);
    assert!(service.status().problem().is_none());
}

#[test]
fn retry_now_preserves_selected_adb_and_never_replays_the_failed_control() {
    let provider = Arc::new(RecoveringProvider::default());
    let service = SerialServices::new(provider.clone(), MonitorSettings::default(), vec![]);
    assert!(result(service.execute(None, "initial".into())).is_ok());
    provider.inner.disconnected.store(true, Ordering::Release);
    assert!(result(service.execute(None, "restart-service-once".into())).is_err());
    assert!(service.status().problem().unwrap().0.contains("ADB"));
    provider.inner.disconnected.store(false, Ordering::Release);
    service.retry_now();
    assert_eq!(
        result(service.execute(None, "read".into())).unwrap(),
        "read"
    );
    assert_eq!(provider.inner.discoveries.load(Ordering::SeqCst), 1);
    assert_eq!(
        *provider.selected_serials.lock().unwrap(),
        ["selected-board"]
    );
    assert_eq!(
        provider
            .inner
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|command| *command == "restart-service-once")
            .count(),
        1
    );
}

#[test]
fn disconnected_ssh_retries_only_the_selected_host_and_recovers() {
    struct SshLink {
        login: HostLogin,
        disconnected: Arc<AtomicBool>,
    }
    impl MonitorLink for SshLink {
        fn label(&self) -> String {
            "SSH · chosen".into()
        }
        fn ssh_login(&self) -> Option<HostLogin> {
            Some(self.login.clone())
        }
        fn sample(&mut self, command: &str, _: &AtomicBool) -> ExecResult {
            if self.disconnected.load(Ordering::Acquire) {
                Err("SSH connection closed".into())
            } else {
                Ok(command.into())
            }
        }
    }
    struct SshProvider {
        calls: AtomicUsize,
        login: HostLogin,
        disconnected: Arc<AtomicBool>,
    }
    impl MonitorConnector for SshProvider {
        fn connect(
            &self,
            identity: &str,
            settings: &MonitorSettings,
            candidates: &[(String, HostLogin)],
            _: &AtomicBool,
        ) -> Result<Option<Box<dyn MonitorLink>>, String> {
            if self.calls.fetch_add(1, Ordering::SeqCst) > 0 {
                assert!(
                    identity.is_empty(),
                    "do not reuse a pre-reboot serial identity"
                );
                assert_eq!(settings.channel, MonitorChannel::Ssh);
                assert_eq!(settings.ssh_host, "chosen");
                assert_eq!(candidates, &[("chosen".into(), self.login.clone())]);
            }
            Ok(Some(Box::new(SshLink {
                login: self.login.clone(),
                disconnected: self.disconnected.clone(),
            })))
        }
    }
    let login = HostLogin::new(
        "192.0.2.4",
        22,
        "root",
        crate::host::LoginMethod::NoPassword,
    );
    let provider = Arc::new(SshProvider {
        calls: AtomicUsize::new(0),
        disconnected: Arc::new(AtomicBool::new(false)),
        login: login.clone(),
    });
    let service = SerialServices::new(
        provider.clone(),
        MonitorSettings::default(),
        vec![
            ("chosen".into(), login.clone()),
            (
                "other".into(),
                HostLogin::new(
                    "192.0.2.5",
                    22,
                    "root",
                    crate::host::LoginMethod::NoPassword,
                ),
            ),
        ],
    );
    let (sender, receiver) = mpsc::channel();
    let serial = std::thread::spawn(move || {
        if let Ok(TerminalTransportCommand::Monitor(request)) = receiver.recv() {
            request
                .reply
                .send(Ok(
                    "@@boot_id\n00000000-0000-4000-8000-000000000001\n".into()
                ))
                .unwrap();
        }
    });
    assert!(result(service.execute(Some(sender), "initial".into())).is_ok());
    serial.join().unwrap();
    provider.disconnected.store(true, Ordering::Release);
    assert!(result(service.execute(None, "failed".into())).is_err());
    assert_eq!(
        service.status().problem().unwrap().1,
        "SSH connection closed"
    );
    provider.disconnected.store(false, Ordering::Release);
    service.retry_now();
    assert_eq!(
        result(service.execute(None, "recovered".into())).unwrap(),
        "recovered"
    );
    assert_eq!(service.status().ssh, Some(login));
    assert!(service.status().problem().is_none());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
}
