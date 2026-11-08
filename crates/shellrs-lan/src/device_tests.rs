use super::*;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;

#[derive(Default)]
struct FakeDevices {
    inputs: Mutex<Vec<DeviceInput>>,
    running: AtomicUsize,
}

impl DeviceProvider for FakeDevices {
    fn devices(&self) -> Vec<SharedDevice> {
        ["serial", "alias", "hidden", "adb"]
            .into_iter()
            .map(|id| {
                SharedDevice::new(
                    id.into(),
                    id.into(),
                    if id == "adb" {
                        DeviceKind::Adb
                    } else {
                        DeviceKind::Serial
                    },
                    id.into(),
                    "115200 8N1".into(),
                    "utf-8".into(),
                )
            })
            .collect()
    }
    fn resource(&self, device: &str) -> Option<String> {
        Some(
            if device == "alias" {
                "serial"
            } else if device == "adb" {
                "adb:adb"
            } else {
                device
            }
            .into(),
        )
    }
    fn run_adb(
        &self,
        device: &str,
        _: &str,
        commands: tokio::sync::mpsc::Receiver<DeviceInput>,
        events: tokio::sync::mpsc::Sender<DeviceEvent>,
        cancelled: &AtomicBool,
    ) -> Result<()> {
        self.run(
            device,
            DeviceSize::new(80, 24)?,
            commands,
            events,
            cancelled,
        )
    }
    fn run(
        &self,
        _: &str,
        _: DeviceSize,
        mut commands: tokio::sync::mpsc::Receiver<DeviceInput>,
        events: tokio::sync::mpsc::Sender<DeviceEvent>,
        cancelled: &AtomicBool,
    ) -> Result<()> {
        self.running.fetch_add(1, Ordering::AcqRel);
        struct Running<'a>(&'a AtomicUsize);
        impl Drop for Running<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::AcqRel);
            }
        }
        let _running = Running(&self.running);
        events.blocking_send(DeviceEvent::Started)?;
        events.blocking_send(DeviceEvent::Output(b"boot\r\n".to_vec()))?;
        while !cancelled.load(Ordering::Acquire) {
            match commands.try_recv() {
                Ok(input) => {
                    self.inputs.lock().unwrap().push(input.clone());
                    if let DeviceInput::Write(bytes) = input {
                        events.blocking_send(DeviceEvent::Output(bytes))?;
                    }
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
            }
        }
        Ok(())
    }
}

struct NoCommands;
impl Executor for NoCommands {
    fn hosts(&self) -> Vec<SharedHost> {
        Vec::new()
    }
    fn execute(
        &self,
        _: &str,
        _: &str,
        _: &AtomicBool,
        _: &mut dyn FnMut(bool, &[u8]) -> io::Result<()>,
    ) -> Result<i32> {
        anyhow::bail!("no SSH commands in device tests")
    }
}

struct Fixture {
    a: Arc<Core>,
    b: Arc<Core>,
    provider: Arc<FakeDevices>,
    target: Device,
    _a_network: Network,
    _b_network: Network,
    _a_directory: tempfile::TempDir,
    b_directory: tempfile::TempDir,
}

fn fixture() -> Fixture {
    fixture_on(std::net::Ipv4Addr::LOCALHOST.into())
}

fn fixture_on(address: IpAddr) -> Fixture {
    let a_directory = tempfile::tempdir().unwrap();
    let b_directory = tempfile::tempdir().unwrap();
    let a = Core::load(a_directory.path(), "A", Arc::new(NoCommands)).unwrap();
    let b = Core::load(b_directory.path(), "B", Arc::new(NoCommands)).unwrap();
    let provider = Arc::new(FakeDevices::default());
    b.set_device_provider(provider.clone()).unwrap();
    a.set_password("test password for A").unwrap();
    b.set_password("test password for B").unwrap();
    b.set_device_sharing(
        BTreeSet::from(["serial".into(), "alias".into(), "adb".into()]),
        true,
    )
    .unwrap();
    let a_network = Network::start(a.clone(), address, 0, 0).unwrap();
    let b_network = Network::start(b.clone(), address, 0, 0).unwrap();
    let target = a.inspect(b_network.tcp_address).unwrap();
    a.pair_password(target.clone(), "test password for B")
        .unwrap();
    Fixture {
        a,
        b,
        provider,
        target,
        _a_network: a_network,
        _b_network: b_network,
        _a_directory: a_directory,
        b_directory,
    }
}

#[test]
fn ipv6_serial_and_adb_streams_keep_binary_data_and_independent_revocation() {
    let fixture = fixture_on(std::net::Ipv6Addr::LOCALHOST.into());
    let serial = open(&fixture, "serial");
    started(&serial);
    let adb = open_service(&fixture, "adb", Some("sync:"));
    started(&adb);
    serial
        .commands
        .send(DeviceInput::Write(vec![0, 255, 13, 10]))
        .unwrap();
    assert_eq!(
        serial.events.recv_timeout(Duration::from_secs(5)).unwrap(),
        DeviceEvent::Output(vec![0, 255, 13, 10])
    );
    fixture
        .b
        .set_device_permission(fixture.a.id(), "serial", false)
        .unwrap();
    assert!(serial.worker.join().unwrap().is_err());
    adb.commands
        .send(DeviceInput::Write(b"still active".to_vec()))
        .unwrap();
    assert_eq!(
        adb.events.recv_timeout(Duration::from_secs(5)).unwrap(),
        DeviceEvent::Output(b"still active".to_vec())
    );
    fixture.b.set_locked(true);
    assert!(adb.worker.join().unwrap().is_err());
    assert!(eventually(|| fixture.b.active_devices().is_empty()));
    assert!(
        fixture
            .b
            .audit()
            .iter()
            .any(|entry| entry.source == Some(std::net::Ipv6Addr::LOCALHOST.into()))
    );
}

fn eventually(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

struct Opened {
    commands: mpsc::SyncSender<DeviceInput>,
    events: mpsc::Receiver<DeviceEvent>,
    cancelled: Arc<AtomicBool>,
    worker: std::thread::JoinHandle<Result<()>>,
}

fn open(fixture: &Fixture, id: &str) -> Opened {
    open_service(fixture, id, None)
}

fn open_service(fixture: &Fixture, id: &str, service: Option<&str>) -> Opened {
    let (commands, receiver) = mpsc::sync_channel(32);
    let (events, output) = mpsc::sync_channel(32);
    let cancelled = Arc::new(AtomicBool::new(false));
    let core = fixture.a.clone();
    let peer = fixture.b.id().to_owned();
    let id = id.to_owned();
    let stop = cancelled.clone();
    let service = service.map(str::to_owned);
    let worker = std::thread::spawn(move || {
        let mut receive = |event| {
            events
                .send(event)
                .map_err(|_| io::ErrorKind::BrokenPipe.into())
        };
        if let Some(service) = service {
            return core.stream_adb(&peer, &id, &service, receiver, &stop, &mut receive);
        }
        core.stream_device(
            &peer,
            &id,
            DeviceSize::new(80, 24).unwrap(),
            receiver,
            &stop,
            &mut receive,
        )
    });
    Opened {
        commands,
        events: output,
        cancelled,
        worker,
    }
}

#[test]
fn adb_services_enforce_device_kind_scope_and_service_allowlist() {
    let fixture = fixture();
    for (device, service) in [
        ("adb", "host:transport:other"),
        ("serial", "sync:"),
        ("hidden", "sync:"),
        ("adb", "tcp:5037"),
    ] {
        let opened = open_service(&fixture, device, Some(service));
        assert!(opened.worker.join().unwrap().is_err());
    }
    assert_eq!(fixture.provider.running.load(Ordering::Acquire), 0);
    fixture
        .b
        .set_device_permission(fixture.a.id(), "adb", false)
        .unwrap();
    let opened = open_service(&fixture, "adb", Some("sync:"));
    assert!(opened.worker.join().unwrap().is_err());
}

#[test]
fn adb_files_install_and_mirror_streams_are_concurrent_and_revocable() {
    let fixture = fixture();
    let streams: Vec<_> = [
        "sync:",
        "exec:cmd package install -S 123",
        "localabstract:scrcpy_1234abcd",
    ]
    .iter()
    .map(|service| {
        let stream = open_service(&fixture, "adb", Some(service));
        started(&stream);
        stream
    })
    .collect();
    assert_eq!(fixture.provider.running.load(Ordering::Acquire), 3);
    for stream in &streams {
        stream
            .commands
            .send(DeviceInput::Write(b"private APK bytes".to_vec()))
            .unwrap();
        assert_eq!(
            stream.events.recv_timeout(Duration::from_secs(3)).unwrap(),
            DeviceEvent::Output(b"private APK bytes".to_vec())
        );
    }
    fixture.b.revoke_device_access(fixture.a.id()).unwrap();
    for stream in streams {
        assert!(stream.worker.join().unwrap().is_err());
    }
    assert!(eventually(|| fixture
        .provider
        .running
        .load(Ordering::Acquire)
        == 0
        && fixture.b.active_devices().is_empty()));
    let audit =
        std::fs::read_to_string(fixture.b_directory.path().join("device-audit.json")).unwrap();
    assert!(!audit.contains("private APK bytes"));
    assert!(
        audit.contains("adb_files")
            && audit.contains("adb_install")
            && audit.contains("adb_mirror")
    );
}

#[test]
fn adb_streams_stop_when_sharing_lock_network_or_blacklist_changes() {
    for change in 0..4 {
        let fixture = fixture();
        let stream = open_service(&fixture, "adb", Some("sync:"));
        started(&stream);
        match change {
            0 => fixture.b.set_device_shared("adb", false).unwrap(),
            1 => fixture.b.set_locked(true),
            2 => fixture.b.set_device_access_enabled(false).unwrap(),
            _ => {
                fixture
                    .b
                    .state
                    .lock()
                    .unwrap()
                    .configuration
                    .blocked_ips
                    .insert(std::net::Ipv4Addr::LOCALHOST.into());
                fixture.b.cancel_unauthorized_devices();
            }
        }
        assert!(stream.worker.join().unwrap().is_err());
        assert!(eventually(|| fixture.b.active_devices().is_empty()));
    }
}

fn started(opened: &Opened) {
    assert_eq!(
        opened.events.recv_timeout(Duration::from_secs(5)).unwrap(),
        DeviceEvent::Started
    );
    assert_eq!(
        opened.events.recv_timeout(Duration::from_secs(5)).unwrap(),
        DeviceEvent::Output(b"boot\r\n".to_vec())
    );
}

#[test]
fn password_pairing_grants_only_the_current_whitelist_not_ssh_execution() {
    let fixture = fixture();
    assert!(!fixture.b.grants()[0].execute);
    assert_eq!(
        fixture.b.device_permissions(fixture.a.id()),
        BTreeSet::from(["serial".into(), "alias".into(), "adb".into()])
    );
    let listed = fixture
        .a
        .request(
            fixture.b.id(),
            RemoteRequest::Devices,
            &AtomicBool::new(false),
            &mut |_, _| Ok(()),
        )
        .unwrap();
    assert_eq!(listed.devices.len(), 3);
    assert!(!listed.devices.iter().any(|device| device.id == "hidden"));
    let hidden = open(&fixture, "hidden");
    assert!(
        hidden
            .worker
            .join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("device_control_not_authorized")
    );
    assert_eq!(fixture.provider.running.load(Ordering::Acquire), 0);
    fixture.b.set_device_shared("hidden", true).unwrap();
    assert!(
        !fixture
            .b
            .device_permissions(fixture.a.id())
            .contains("hidden")
    );
    fixture
        .a
        .pair_password(fixture.target, "test password for B")
        .unwrap();
    assert!(
        fixture
            .b
            .device_permissions(fixture.a.id())
            .contains("hidden")
    );
}

#[test]
fn approval_pairing_stays_device_read_only_until_explicit_device_grant() {
    let fixture = fixture();
    fixture.b.revoke(fixture.a.id()).unwrap();
    fixture.a.request_pair(&fixture.target).unwrap();
    let pending = fixture.b.pending();
    fixture
        .b
        .approve(fixture.a.id(), pending[0].token, BTreeSet::new(), false)
        .unwrap();
    fixture.a.complete_pair(fixture.target.clone()).unwrap();
    assert!(fixture.b.device_permissions(fixture.a.id()).is_empty());
    let listed = fixture
        .a
        .request(
            fixture.b.id(),
            RemoteRequest::Devices,
            &AtomicBool::new(false),
            &mut |_, _| Ok(()),
        )
        .unwrap();
    assert!(listed.devices.is_empty());
    fixture
        .b
        .set_device_permission(fixture.a.id(), "serial", true)
        .unwrap();
    let opened = open(&fixture, "serial");
    started(&opened);
    opened.cancelled.store(true, Ordering::Release);
    assert!(opened.worker.join().unwrap().is_err());
}

#[test]
fn stopping_the_server_and_ip_blocking_cancel_device_streams() {
    let fixture = fixture();
    let opened = open(&fixture, "serial");
    started(&opened);
    let directory = tempfile::tempdir().unwrap();
    let caller = Core::load(directory.path(), "C", Arc::new(NoCommands)).unwrap();
    caller.set_password("test password for C").unwrap();
    let _network = Network::start(caller.clone(), std::net::Ipv4Addr::LOCALHOST, 0, 0).unwrap();
    caller.request_pair(&fixture.target).unwrap();
    let pending = fixture.b.pending();
    fixture
        .b
        .reject(caller.id(), pending[0].token, true)
        .unwrap();
    assert!(opened.worker.join().unwrap().is_err());
    assert!(eventually(|| fixture.b.active_devices().is_empty()));
    fixture
        .b
        .unblock(std::net::Ipv4Addr::LOCALHOST.into())
        .unwrap();
    let opened = open(&fixture, "serial");
    started(&opened);
    drop(fixture._b_network);
    assert!(opened.worker.join().unwrap().is_err());
    assert!(eventually(|| fixture
        .provider
        .running
        .load(Ordering::Acquire)
        == 0));
}

#[test]
fn per_device_revocation_survives_fast_regrant_without_interrupting_other_devices() {
    let fixture = fixture();
    let serial = open(&fixture, "serial");
    started(&serial);
    let adb = open(&fixture, "adb");
    started(&adb);
    fixture
        .b
        .set_device_permission(fixture.a.id(), "serial", false)
        .unwrap();
    fixture
        .b
        .set_device_permission(fixture.a.id(), "serial", true)
        .unwrap();
    assert!(serial.worker.join().unwrap().is_err());
    adb.commands
        .send(DeviceInput::Write(b"still-authorized".to_vec()))
        .unwrap();
    assert_eq!(
        adb.events.recv_timeout(Duration::from_secs(5)).unwrap(),
        DeviceEvent::Output(b"still-authorized".to_vec())
    );
    assert!(eventually(|| fixture.b.active_devices().len() == 1));
    let reconnected = open(&fixture, "serial");
    started(&reconnected);
    reconnected.cancelled.store(true, Ordering::Release);
    adb.cancelled.store(true, Ordering::Release);
    assert!(reconnected.worker.join().unwrap().is_err());
    assert!(adb.worker.join().unwrap().is_err());
    assert!(eventually(|| fixture.b.active_devices().is_empty()));
}

#[test]
fn interactive_bytes_resize_break_busy_and_immediate_revocation_use_encrypted_streams() {
    let fixture = fixture();
    let opened = open(&fixture, "serial");
    started(&opened);
    let bytes = b"\0\xff\r\nuser-secret-not-for-audit".to_vec();
    opened
        .commands
        .send(DeviceInput::Write(bytes.clone()))
        .unwrap();
    assert_eq!(
        opened.events.recv_timeout(Duration::from_secs(5)).unwrap(),
        DeviceEvent::Output(bytes.clone())
    );
    let size = DeviceSize::new(120, 40).unwrap();
    opened.commands.send(DeviceInput::Resize(size)).unwrap();
    opened.commands.send(DeviceInput::Break).unwrap();
    assert!(eventually(|| fixture
        .provider
        .inputs
        .lock()
        .unwrap()
        .contains(&DeviceInput::Resize(size))
        && fixture
            .provider
            .inputs
            .lock()
            .unwrap()
            .contains(&DeviceInput::Break)));
    let alias = open(&fixture, "alias");
    assert!(
        alias
            .worker
            .join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("device_busy")
    );
    fixture.b.revoke_device_access(fixture.a.id()).unwrap();
    assert!(opened.worker.join().unwrap().is_err());
    assert!(eventually(|| fixture
        .provider
        .running
        .load(Ordering::Acquire)
        == 0
        && fixture.b.active_devices().is_empty()));
    let log = fixture.b.audit();
    assert!(log.iter().any(|entry| entry.action == AuditAction::Break));
    assert!(
        log.iter()
            .any(|entry| entry.action == AuditAction::Denied && entry.outcome == "device_busy")
    );
    assert!(log.iter().any(|entry| entry.action == AuditAction::Closed
        && entry.bytes_in == bytes.len() as u64
        && entry.bytes_out >= bytes.len() as u64));
    let stored =
        std::fs::read_to_string(fixture.b_directory.path().join("device-audit.json")).unwrap();
    assert!(!stored.contains("user-secret-not-for-audit"));
    let reloaded = Core::load(fixture.b_directory.path(), "B", Arc::new(NoCommands)).unwrap();
    assert!(reloaded.device_permissions(fixture.a.id()).is_empty());
    assert_eq!(reloaded.audit(), log);
}

#[test]
fn silent_streams_end_on_unsharing_locking_disabling_disconnect_and_controller_cancellation() {
    let fixture = fixture();
    for action in 0..5 {
        fixture.b.set_locked(false);
        fixture
            .b
            .set_device_sharing(BTreeSet::from(["serial".into(), "adb".into()]), true)
            .unwrap();
        fixture
            .b
            .set_device_permissions(fixture.a.id(), fixture.b.shared_devices())
            .unwrap();
        let id = if action == 4 { "adb" } else { "serial" };
        let opened = open(&fixture, id);
        started(&opened);
        match action {
            0 => fixture.b.set_device_shared("serial", false).unwrap(),
            1 => fixture.b.set_locked(true),
            2 => fixture.b.set_device_access_enabled(false).unwrap(),
            3 => fixture
                .b
                .disconnect_device(fixture.b.active_devices()[0].id)
                .unwrap(),
            4 => opened.cancelled.store(true, Ordering::Release),
            _ => unreachable!(),
        }
        assert!(opened.worker.join().unwrap().is_err());
        assert!(eventually(|| fixture
            .provider
            .running
            .load(Ordering::Acquire)
            == 0
            && fixture.b.active_devices().is_empty()));
    }
}

#[test]
fn revoking_still_stops_streams_if_audit_persistence_fails_and_new_grants_fail_closed() {
    let fixture = fixture();
    let opened = open(&fixture, "serial");
    started(&opened);
    let path = fixture.b_directory.path().join("device-audit.json");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(fixture.b.revoke_device_access(fixture.a.id()).is_err());
    assert!(fixture.b.device_permissions(fixture.a.id()).is_empty());
    assert!(opened.worker.join().unwrap().is_err());
    assert!(eventually(|| fixture
        .provider
        .running
        .load(Ordering::Acquire)
        == 0));
    assert!(
        fixture
            .b
            .set_device_permission(fixture.a.id(), "serial", true)
            .is_err()
    );
    assert!(
        !fixture
            .b
            .device_permissions(fixture.a.id())
            .contains("serial")
    );
    assert!(fixture.b.take_device_audit_error().is_some());
}

#[test]
fn device_frames_are_bounded_and_keep_fragmented_binary_input_intact() {
    let inputs = [
        DeviceInput::Write(vec![0, 255, 13, 10]),
        DeviceInput::Resize(DeviceSize::new(1, 1000).unwrap()),
        DeviceInput::Break,
    ];
    for input in inputs {
        let frame = crate::devices::input_frame(&input).unwrap();
        for count in 0..frame.len() {
            assert!(
                crate::devices::take_input(&mut frame[..count].to_vec())
                    .unwrap()
                    .is_none()
            );
        }
        let mut buffer = frame;
        assert_eq!(
            crate::devices::take_input(&mut buffer).unwrap(),
            Some(input)
        );
        assert!(buffer.is_empty());
    }
    assert!(crate::devices::take_input(&mut vec![1, 255, 255, 255, 255]).is_err());
    assert!(crate::devices::input_frame(&DeviceInput::Write(vec![0; DEVICE_CHUNK + 1])).is_err());
    assert!(DeviceSize::new(0, 80).is_err());
    assert!(DeviceSize::new(80, 1001).is_err());
}

#[test]
fn audit_history_is_bounded_persistent_and_sequenced() {
    let directory = tempfile::tempdir().unwrap();
    let core = Core::load(directory.path(), "B", Arc::new(NoCommands)).unwrap();
    for _ in 0..505 {
        core.record_device_audit(AuditEntry::new(
            AuditAction::SharingChanged,
            "local",
            "serial",
            "requested",
        ))
        .unwrap();
    }
    let entries = core.audit();
    assert_eq!(entries.len(), 500);
    assert_eq!(entries.first().unwrap().sequence, 6);
    assert_eq!(entries.last().unwrap().sequence, 505);
    let reloaded = Core::load(directory.path(), "B", Arc::new(NoCommands)).unwrap();
    assert_eq!(reloaded.audit(), entries);
}
