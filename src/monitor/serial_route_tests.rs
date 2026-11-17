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
fn all_tools_reuse_one_successful_route_and_never_reprobe_ssh_on_disconnect() {
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
fn discovery_stops_after_two_attempts_and_manual_reset_gets_a_fresh_budget() {
    let provider = Arc::new(Provider::default());
    provider.unavailable.store(true, Ordering::Release);
    let service = SerialServices::new(provider.clone(), MonitorSettings::default(), vec![]);
    for _ in 0..10 {
        assert!(result(service.execute(None, "read".into())).is_err());
    }
    assert_eq!(provider.discoveries.load(Ordering::SeqCst), 2);
    service.cancel();
    assert!(result(service.execute(None, "old-generation".into())).is_err());
    let next = SerialServices::new(provider.clone(), MonitorSettings::default(), vec![]);
    provider.unavailable.store(false, Ordering::Release);
    assert_eq!(
        result(next.execute(None, "new-generation".into())).unwrap(),
        "new-generation"
    );
    assert_eq!(provider.discoveries.load(Ordering::SeqCst), 3);
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
    assert_eq!(provider.discoveries.load(Ordering::SeqCst), 2);
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
