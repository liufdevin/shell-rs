use super::*;
use anyhow::bail;

struct FakeExecutor {
    cancelled: Arc<AtomicBool>,
    started: Arc<AtomicBool>,
}
impl Executor for FakeExecutor {
    fn open_terminal(
        &self,
        _: &str,
        size: DeviceSize,
        mut commands: tokio::sync::mpsc::Receiver<DeviceInput>,
        events: tokio::sync::mpsc::Sender<DeviceEvent>,
        cancelled: &AtomicBool,
    ) -> Result<()> {
        events.blocking_send(DeviceEvent::Started)?;
        events.blocking_send(DeviceEvent::Output(
            format!("{}x{}", size.columns, size.rows).into_bytes(),
        ))?;
        while !cancelled.load(Ordering::Acquire) {
            match commands.try_recv() {
                Ok(DeviceInput::Write(bytes)) if bytes == b"exit7" => {
                    events.blocking_send(DeviceEvent::Exited {
                        code: 7,
                        signal: None,
                    })?;
                    return Ok(());
                }
                Ok(DeviceInput::Write(bytes)) if bytes == b"flood" => {
                    for _ in 0..140 {
                        events.blocking_send(DeviceEvent::Output(vec![b'x'; DEVICE_CHUNK]))?;
                    }
                }
                Ok(DeviceInput::Write(bytes)) => {
                    events.blocking_send(DeviceEvent::Output(bytes))?
                }
                Ok(DeviceInput::Resize(size)) => {
                    events.blocking_send(DeviceEvent::Output(
                        format!("{}x{}", size.columns, size.rows).into_bytes(),
                    ))?;
                }
                Ok(DeviceInput::Break) => {}
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
            }
        }
        self.cancelled.store(true, Ordering::Release);
        Ok(())
    }

    fn hosts(&self) -> Vec<SharedHost> {
        vec![
            SharedHost::new(
                "shared".into(),
                "shared host".into(),
                "10.0.0.1".into(),
                22,
                "root".into(),
            ),
            SharedHost::new(
                "private".into(),
                "private host".into(),
                "10.0.0.2".into(),
                22,
                "root".into(),
            ),
        ]
    }
    fn execute(
        &self,
        _: &str,
        command: &str,
        cancelled: &AtomicBool,
        output: &mut dyn FnMut(bool, &[u8]) -> io::Result<()>,
    ) -> Result<i32> {
        self.started.store(true, Ordering::Release);
        if command == "wait" {
            while !cancelled.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(10));
            }
            self.cancelled.store(true, Ordering::Release);
            bail!("cancelled");
        }
        if command == "large" {
            for _ in 0..200 {
                output(false, &[b'x'; 8192])?;
            }
        } else {
            output(false, command.as_bytes())?;
            output(true, b"stderr")?;
        }
        Ok(7)
    }
}

struct HostStream {
    input: std::sync::mpsc::Sender<DeviceInput>,
    events: std::sync::mpsc::Receiver<DeviceEvent>,
    worker: std::thread::JoinHandle<Result<()>>,
}

fn open_host(core: &Arc<Core>, peer: &str, host: &str) -> HostStream {
    let (input, commands) = std::sync::mpsc::channel();
    let (events, received) = std::sync::mpsc::channel();
    let core = core.clone();
    let peer = peer.to_owned();
    let host = host.to_owned();
    let worker = std::thread::spawn(move || {
        core.stream_host(
            &peer,
            &host,
            DeviceSize::new(90, 25).unwrap(),
            commands,
            &AtomicBool::new(false),
            &mut |event| {
                events
                    .send(event)
                    .map_err(|_| io::ErrorKind::BrokenPipe.into())
            },
        )
    });
    HostStream {
        input,
        events: received,
        worker,
    }
}

fn host_started(stream: &HostStream) {
    assert_eq!(
        stream.events.recv_timeout(Duration::from_secs(5)).unwrap(),
        DeviceEvent::Started
    );
    assert_eq!(
        stream.events.recv_timeout(Duration::from_secs(5)).unwrap(),
        DeviceEvent::Output(b"90x25".to_vec())
    );
}

#[test]
fn ssh_terminal_streams_support_binary_input_resize_large_output_exit_and_multiple_tabs() {
    let (_a_directory, a, _, _) = fixture("A");
    let (_b_directory, b, _, _) = fixture("B");
    let _a_network = running(&a);
    let b_network = running(&b);
    b.set_sharing(BTreeSet::from(["shared".into()]), true)
        .unwrap();
    let target = a.inspect(b_network.tcp_address).unwrap();
    a.pair_password(target.clone(), "correct horse battery")
        .unwrap();
    assert!(!b.device_access_enabled());
    let stream = open_host(&a, b.id(), "shared");
    host_started(&stream);
    let second = open_host(&a, b.id(), "shared");
    host_started(&second);
    stream
        .input
        .send(DeviceInput::Write(vec![0, 255, 3, 13, 10]))
        .unwrap();
    assert_eq!(
        stream.events.recv_timeout(Duration::from_secs(5)).unwrap(),
        DeviceEvent::Output(vec![0, 255, 3, 13, 10])
    );
    stream
        .input
        .send(DeviceInput::Resize(DeviceSize::new(130, 42).unwrap()))
        .unwrap();
    assert_eq!(
        stream.events.recv_timeout(Duration::from_secs(5)).unwrap(),
        DeviceEvent::Output(b"130x42".to_vec())
    );
    stream
        .input
        .send(DeviceInput::Write(b"flood".to_vec()))
        .unwrap();
    let mut bytes = 0;
    while bytes < 140 * DEVICE_CHUNK {
        let DeviceEvent::Output(output) =
            stream.events.recv_timeout(Duration::from_secs(5)).unwrap()
        else {
            panic!("missing terminal output")
        };
        bytes += output.len();
    }
    assert!(bytes > MAX_OUTPUT);
    stream
        .input
        .send(DeviceInput::Write(b"exit7".to_vec()))
        .unwrap();
    assert_eq!(
        stream.events.recv_timeout(Duration::from_secs(5)).unwrap(),
        DeviceEvent::Exited {
            code: 7,
            signal: None
        }
    );
    stream.worker.join().unwrap().unwrap();
    b.disconnect_device(
        b.active_devices()
            .iter()
            .map(|active| active.id)
            .max()
            .unwrap(),
    )
    .unwrap();
    assert!(second.worker.join().unwrap().is_err());
    assert!(eventually(|| b.active_devices().is_empty()));
    assert!(
        b.audit()
            .iter()
            .any(|entry| entry.outcome == "ssh_terminal")
    );
    assert!(
        b.audit()
            .iter()
            .all(|entry| !entry.outcome.contains("exit7"))
    );
}

#[test]
fn ssh_terminals_enforce_host_and_execution_permissions_and_revoke_silent_streams() {
    let (_a_directory, a, _, _) = fixture("A");
    let (_b_directory, b, _, _) = fixture("B");
    let _a_network = running(&a);
    let b_network = running(&b);
    b.set_sharing(BTreeSet::from(["shared".into()]), false)
        .unwrap();
    let target = a.inspect(b_network.tcp_address).unwrap();
    a.pair_password(target.clone(), "correct horse battery")
        .unwrap();
    assert!(
        open_host(&a, b.id(), "shared")
            .worker
            .join()
            .unwrap()
            .is_err()
    );
    b.set_sharing(BTreeSet::from(["shared".into()]), true)
        .unwrap();
    b.set_execution_permission(a.id(), true).unwrap();
    assert!(
        open_host(&a, b.id(), "private")
            .worker
            .join()
            .unwrap()
            .is_err()
    );
    for action in 0..6 {
        b.set_locked(false);
        b.set_sharing(BTreeSet::from(["shared".into()]), true)
            .unwrap();
        a.pair_password(target.clone(), "correct horse battery")
            .unwrap();
        let stream = open_host(&a, b.id(), "shared");
        host_started(&stream);
        match action {
            0 => {
                b.set_execution_permission(a.id(), false).unwrap();
                b.set_execution_permission(a.id(), true).unwrap();
            }
            1 => b.set_sharing(BTreeSet::new(), true).unwrap(),
            2 => b
                .set_sharing(BTreeSet::from(["shared".into()]), false)
                .unwrap(),
            3 => b.revoke(a.id()).unwrap(),
            4 => b.set_locked(true),
            _ => b.set_password("a changed strong password").unwrap(),
        }
        assert!(stream.worker.join().unwrap().is_err());
        assert!(eventually(|| b.active_devices().is_empty()));
    }
}

fn fixture(
    name: &str,
) -> (
    tempfile::TempDir,
    Arc<Core>,
    Arc<AtomicBool>,
    Arc<AtomicBool>,
) {
    let directory = tempfile::tempdir().unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicBool::new(false));
    let core = Core::load(
        directory.path(),
        name,
        Arc::new(FakeExecutor {
            cancelled: cancelled.clone(),
            started: started.clone(),
        }),
    )
    .unwrap();
    (directory, core, cancelled, started)
}
fn running(core: &Arc<Core>) -> Network {
    core.set_password("correct horse battery").unwrap();
    Network::start(core.clone(), std::net::Ipv4Addr::LOCALHOST, 0, 0).unwrap()
}
fn eventually(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn enabling_requires_a_password_and_verifiers_do_not_store_plaintext() {
    let (directory, core, _, _) = fixture("B");
    assert!(!core.enabled_on_start());
    assert!(core.remember_enabled(true).is_err());
    assert!(!core.enabled_on_start());
    assert!(Network::start(core.clone(), std::net::Ipv4Addr::LOCALHOST, 0, 0).is_err());
    assert!(core.set_password("short").is_err());
    core.set_password("correct horse battery").unwrap();
    assert!(core.has_password());
    let file = std::fs::read_to_string(directory.path().join("access.json")).unwrap();
    assert!(!file.contains("correct horse battery"));
    let (_, other, _, _) = fixture("unused");
    let reloaded = Core::load(directory.path(), "B", other.executor.clone()).unwrap();
    assert_eq!(core.id(), reloaded.id());
    assert!(reloaded.has_password());
}

#[test]
fn six_character_password_pairs_over_encrypted_connections_and_survives_reload() {
    let (_a_directory, a, _, _) = fixture("A");
    let (b_directory, b, _, _) = fixture("B");
    assert!(b.set_password("abcde").is_err());
    a.set_password("local6").unwrap();
    b.set_password("lan6ok").unwrap();
    let _a_network = Network::start(a.clone(), std::net::Ipv4Addr::LOCALHOST, 0, 0).unwrap();
    let b_network = Network::start(b.clone(), std::net::Ipv4Addr::LOCALHOST, 0, 0).unwrap();
    let target = a.inspect(b_network.tcp_address).unwrap();
    assert!(a.pair_password(target.clone(), "wrong6").is_err());
    assert!(b.grants().is_empty());
    a.pair_password(target, "lan6ok").unwrap();
    assert_eq!(b.grants()[0].id, a.id());
    assert!(
        !std::fs::read_to_string(b_directory.path().join("access.json"))
            .unwrap()
            .contains("lan6ok")
    );
    drop(b_network);
    let reloaded = Core::load(b_directory.path(), "B", b.executor.clone()).unwrap();
    assert_eq!(b.id(), reloaded.id());
    let network = Network::start(reloaded.clone(), std::net::Ipv4Addr::LOCALHOST, 0, 0).unwrap();
    a.pair_password(a.inspect(network.tcp_address).unwrap(), "lan6ok")
        .unwrap();
    assert_eq!(reloaded.grants()[0].id, a.id());
}

#[test]
fn password_pairing_discovery_permissions_limits_and_revocation_use_real_encrypted_connections() {
    let (_a_directory, a, _, _) = fixture("A");
    let (_b_directory, b, cancelled, started) = fixture("B");
    let a_network = running(&a);
    let b_network = running(&b);
    b.set_sharing(BTreeSet::from(["shared".into()]), true)
        .unwrap();
    b.set_discoverable(true).unwrap();
    let discovered = crate::discovery::discover_at(&[b_network.discovery_address]).unwrap();
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].id, b.id());
    let device = a.inspect(b_network.tcp_address).unwrap();
    assert!(b.pending().is_empty());
    let mut changed_identity = device.clone();
    changed_identity.id = a.id().to_owned();
    assert!(
        a.pair_password(changed_identity, "correct horse battery")
            .is_err()
    );
    assert!(
        a.pair_password(device.clone(), "incorrect password")
            .is_err()
    );
    assert!(b.pending().is_empty());
    a.pair_password(device.clone(), "correct horse battery")
        .unwrap();
    let response = a
        .request(
            b.id(),
            RemoteRequest::Hosts,
            &AtomicBool::new(false),
            &mut |_, _| Ok(()),
        )
        .unwrap();
    assert_eq!(response.hosts.len(), 1);
    assert_eq!(response.hosts[0].id, "shared");
    assert!(response.can_execute);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let result = a
        .request(
            b.id(),
            RemoteRequest::Execute {
                host: "shared".into(),
                command: "hello".into(),
            },
            &AtomicBool::new(false),
            &mut |error, bytes| {
                if error {
                    stderr.extend_from_slice(bytes)
                } else {
                    stdout.extend_from_slice(bytes)
                }
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(result.exit_code, Some(7));
    assert_eq!(stdout, b"hello");
    assert_eq!(stderr, b"stderr");
    assert!(
        a.request(
            b.id(),
            RemoteRequest::Execute {
                host: "private".into(),
                command: "hello".into()
            },
            &AtomicBool::new(false),
            &mut |_, _| Ok(())
        )
        .is_err()
    );
    assert!(
        a.request(
            b.id(),
            RemoteRequest::Execute {
                host: "shared".into(),
                command: "large".into()
            },
            &AtomicBool::new(false),
            &mut |_, _| Ok(())
        )
        .is_err()
    );
    started.store(false, Ordering::Release);
    let client_cancel = Arc::new(AtomicBool::new(false));
    let check_cancel = client_cancel.clone();
    let client = a.clone();
    let peer = b.id().to_owned();
    let cancelled_worker = std::thread::spawn(move || {
        client.request(
            &peer,
            RemoteRequest::Execute {
                host: "shared".into(),
                command: "wait".into(),
            },
            &check_cancel,
            &mut |_, _| Ok(()),
        )
    });
    assert!(eventually(|| started.load(Ordering::Acquire)));
    client_cancel.store(true, Ordering::Release);
    assert!(cancelled_worker.join().unwrap().is_err());
    assert!(eventually(|| cancelled.load(Ordering::Acquire)));
    started.store(false, Ordering::Release);
    cancelled.store(false, Ordering::Release);
    let client = a.clone();
    let peer = b.id().to_owned();
    let worker = std::thread::spawn(move || {
        client.request(
            &peer,
            RemoteRequest::Execute {
                host: "shared".into(),
                command: "wait".into(),
            },
            &AtomicBool::new(false),
            &mut |_, _| Ok(()),
        )
    });
    assert!(eventually(|| started.load(Ordering::Acquire)));
    b.revoke(a.id()).unwrap();
    assert!(worker.join().unwrap().is_err());
    assert!(eventually(|| cancelled.load(Ordering::Acquire)));
    assert!(
        a.request(
            b.id(),
            RemoteRequest::Hosts,
            &AtomicBool::new(false),
            &mut |_, _| Ok(())
        )
        .is_err()
    );
    drop(a_network);
    drop(b_network);
}

#[test]
fn hidden_devices_still_pair_by_ip_and_discovery_changes_apply_without_restarting() {
    let (_controller_directory, controller, _, _) = fixture("A");
    let (owner_directory, owner, _, _) = fixture("B");
    let _controller_network = running(&controller);
    let owner_network = running(&owner);
    assert!(!owner.is_discoverable());
    assert!(
        crate::discovery::discover_at(&[owner_network.discovery_address])
            .unwrap()
            .is_empty()
    );
    let target = controller.inspect(owner_network.tcp_address).unwrap();
    controller
        .pair_password(target, "correct horse battery")
        .unwrap();
    assert_eq!(controller.peers()[0].id, owner.id());
    assert_eq!(owner.grants()[0].id, controller.id());

    owner.set_discoverable(true).unwrap();
    let discovered = crate::discovery::discover_at(&[owner_network.discovery_address]).unwrap();
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].id, owner.id());
    let reloaded = Core::load(owner_directory.path(), "B", owner.executor.clone()).unwrap();
    assert!(reloaded.is_discoverable());

    owner.set_discoverable(false).unwrap();
    assert!(
        crate::discovery::discover_at(&[owner_network.discovery_address])
            .unwrap()
            .is_empty()
    );
    assert!(owner.is_running());
    let response = controller
        .request(
            owner.id(),
            RemoteRequest::Status,
            &AtomicBool::new(false),
            &mut |_, _| Ok(()),
        )
        .unwrap();
    assert_eq!(response.status, "accepted");
    let reloaded = Core::load(owner_directory.path(), "B", owner.executor.clone()).unwrap();
    assert!(!reloaded.is_discoverable());
}

#[test]
fn configurations_without_a_discovery_setting_remain_hidden() {
    let (directory, core, _, _) = fixture("B");
    core.set_password("correct horse battery").unwrap();
    core.remember_enabled(true).unwrap();
    core.set_discoverable(true).unwrap();
    let path = directory.path().join("access.json");
    let mut configuration: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    configuration
        .as_object_mut()
        .unwrap()
        .remove("discoverable");
    std::fs::write(path, serde_json::to_vec(&configuration).unwrap()).unwrap();
    let reloaded = Core::load(directory.path(), "B", core.executor.clone()).unwrap();
    assert!(!reloaded.is_discoverable());
    assert!(reloaded.has_password());
    assert!(reloaded.enabled_on_start());
}

#[test]
fn request_approval_rejection_ip_blacklist_and_unblocking_are_enforced_without_new_pending_requests()
 {
    let (_a_directory, a, _, _) = fixture("A");
    let (b_directory, b, _, _) = fixture("B");
    let _a_network = running(&a);
    let b_network = running(&b);
    b.set_sharing(BTreeSet::from(["shared".into()]), false)
        .unwrap();
    let device = a.inspect(b_network.tcp_address).unwrap();
    a.request_pair(&device).unwrap();
    assert_eq!(b.pending().len(), 1);
    assert_eq!(b.pending()[0].code, device.pairing_code(a.id()));
    assert!(
        a.complete_pair(device.clone())
            .unwrap_err()
            .to_string()
            .contains("pairing_pending")
    );
    let old_token = b.pending()[0].token;
    b.reject(a.id(), old_token, false).unwrap();
    assert!(
        a.complete_pair(device.clone())
            .unwrap_err()
            .to_string()
            .contains("pairing_rejected")
    );
    a.request_pair(&device).unwrap();
    assert!(
        b.approve(a.id(), old_token, b.shared_hosts(), false)
            .is_err()
    );
    b.reject(a.id(), b.pending()[0].token, true).unwrap();
    assert!(b.pending().is_empty());
    assert_eq!(b.blocked_ips(), [IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)]);
    assert!(
        a.inspect(b_network.tcp_address)
            .unwrap_err()
            .to_string()
            .contains("blocked_by_device")
    );
    assert!(
        a.request_pair(&device)
            .unwrap_err()
            .to_string()
            .contains("blocked_by_device")
    );
    assert!(b.pending().is_empty());
    let reloaded = Core::load(b_directory.path(), "B", b.executor.clone()).unwrap();
    assert_eq!(reloaded.blocked_ips(), b.blocked_ips());
    b.unblock(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
        .unwrap();
    a.request_pair(&device).unwrap();
    assert_eq!(b.pending().len(), 1);
    b.approve(a.id(), b.pending()[0].token, b.shared_hosts(), false)
        .unwrap();
    a.complete_pair(device.clone()).unwrap();
    assert!(
        !a.request(
            b.id(),
            RemoteRequest::Hosts,
            &AtomicBool::new(false),
            &mut |_, _| Ok(())
        )
        .unwrap()
        .can_execute
    );
    assert!(
        a.request(
            b.id(),
            RemoteRequest::Execute {
                host: "shared".into(),
                command: "hello".into()
            },
            &AtomicBool::new(false),
            &mut |_, _| Ok(())
        )
        .is_err()
    );
    b.set_sharing(b.shared_hosts(), true).unwrap();
    b.set_execution_permission(a.id(), true).unwrap();
    assert!(
        a.request(
            b.id(),
            RemoteRequest::Hosts,
            &AtomicBool::new(false),
            &mut |_, _| Ok(())
        )
        .unwrap()
        .can_execute
    );
    b.set_locked(true);
    assert!(
        a.request(
            b.id(),
            RemoteRequest::Hosts,
            &AtomicBool::new(false),
            &mut |_, _| Ok(())
        )
        .is_err()
    );
    b.set_locked(false);
    b.set_password("a different strong password").unwrap();
    assert!(b.grants().is_empty());
    assert!(
        a.request(
            b.id(),
            RemoteRequest::Hosts,
            &AtomicBool::new(false),
            &mut |_, _| Ok(())
        )
        .is_err()
    );
}

#[test]
fn stopping_releases_both_ports_before_restarting_and_cancels_silent_commands() {
    let (_a_directory, a, _, _) = fixture("A");
    let (_b_directory, b, cancelled, started) = fixture("B");
    let _a_network = running(&a);
    let b_network = running(&b);
    let tcp_port = b_network.tcp_address.port();
    let discovery_port = b_network.discovery_address.port();
    b.set_sharing(BTreeSet::from(["shared".into()]), true)
        .unwrap();
    let device = a.inspect(b_network.tcp_address).unwrap();
    a.pair_password(device, "correct horse battery").unwrap();
    let client = a.clone();
    let peer = b.id().to_owned();
    let worker = std::thread::spawn(move || {
        client.request(
            &peer,
            RemoteRequest::Execute {
                host: "shared".into(),
                command: "wait".into(),
            },
            &AtomicBool::new(false),
            &mut |_, _| Ok(()),
        )
    });
    assert!(eventually(|| started.load(Ordering::Acquire)));
    drop(b_network);
    assert!(!b.is_running());
    assert!(worker.join().unwrap().is_err());
    assert!(eventually(|| cancelled.load(Ordering::Acquire)));
    let _restarted = Network::start(
        b.clone(),
        std::net::Ipv4Addr::LOCALHOST,
        tcp_port,
        discovery_port,
    )
    .unwrap();
    assert!(b.is_running());
    assert_eq!(
        a.request(
            b.id(),
            RemoteRequest::Hosts,
            &AtomicBool::new(false),
            &mut |_, _| Ok(()),
        )
        .unwrap()
        .hosts
        .len(),
        1
    );
}

#[test]
fn only_local_addresses_and_strict_messages_are_accepted() {
    assert!(parse_endpoint("192.168.1.2").is_ok());
    assert!(parse_endpoint("127.0.0.1:12345").is_ok());
    for value in [
        "8.8.8.8:22",
        "0.0.0.0:22",
        "[::]:22",
        "192.168.1.1:0",
        "example.com:22",
    ] {
        assert!(parse_endpoint(value).is_err());
    }
    assert!(
        serde_json::from_str::<RemoteRequest>(
            r#"{"type":"execute","host":"shared","command":"ls","shell":true}"#
        )
        .is_err()
    );
}

#[test]
fn ipv6_pairing_discovery_commands_blacklist_and_revocation_use_encrypted_connections() {
    use std::net::Ipv6Addr;
    let (_controller_directory, controller, _, _) = fixture("A");
    let (_owner_directory, owner, cancelled, started) = fixture("B");
    controller.set_password("correct horse battery").unwrap();
    owner.set_password("correct horse battery").unwrap();
    let _controller_network =
        Network::start(controller.clone(), Ipv6Addr::LOCALHOST, 0, 0).unwrap();
    let owner_network = Network::start(owner.clone(), Ipv6Addr::LOCALHOST, 0, 0).unwrap();
    owner
        .set_sharing(["shared".to_owned()].into(), true)
        .unwrap();
    assert!(
        crate::discovery::discover_at(&[owner_network.discovery_address])
            .unwrap()
            .is_empty()
    );
    owner.set_discoverable(true).unwrap();
    let discovered = crate::discovery::discover_at(&[owner_network.discovery_address]).unwrap();
    assert_eq!(discovered.len(), 1);
    assert!(discovered[0].endpoint.is_ipv6());
    let target = controller.inspect(owner_network.tcp_address).unwrap();
    assert!(
        controller
            .pair_password(target.clone(), "incorrect password")
            .is_err()
    );
    assert!(owner.pending().is_empty());
    controller
        .pair_password(target.clone(), "correct horse battery")
        .unwrap();
    let response = controller
        .request(
            owner.id(),
            RemoteRequest::Execute {
                host: "shared".into(),
                command: "hello".into(),
            },
            &AtomicBool::new(false),
            &mut |_, _| Ok(()),
        )
        .unwrap();
    assert_eq!(response.exit_code, Some(7));
    started.store(false, Ordering::Release);
    let controlling = controller.clone();
    let peer = owner.id().to_owned();
    let worker = std::thread::spawn(move || {
        controlling.request(
            &peer,
            RemoteRequest::Execute {
                host: "shared".into(),
                command: "wait".into(),
            },
            &AtomicBool::new(false),
            &mut |_, _| Ok(()),
        )
    });
    assert!(eventually(|| started.load(Ordering::Acquire)));
    owner.revoke(controller.id()).unwrap();
    assert!(worker.join().unwrap().is_err());
    assert!(eventually(|| cancelled.load(Ordering::Acquire)));
    controller.request_pair(&target).unwrap();
    let pending = owner.pending().remove(0);
    assert_eq!(pending.address.ip(), IpAddr::V6(Ipv6Addr::LOCALHOST));
    owner.reject(&pending.id, pending.token, true).unwrap();
    assert!(
        controller
            .request_pair(&target)
            .unwrap_err()
            .to_string()
            .contains("blocked_by_device")
    );
    assert!(owner.pending().is_empty());
    assert!(
        crate::discovery::discover_at(&[owner_network.discovery_address])
            .unwrap()
            .is_empty()
    );
    owner.unblock(Ipv6Addr::LOCALHOST.into()).unwrap();
    controller.request_pair(&target).unwrap();
    let pending = owner.pending().remove(0);
    owner
        .approve(
            &pending.id,
            pending.token,
            ["shared".to_owned()].into(),
            false,
        )
        .unwrap();
    controller.complete_pair(target).unwrap();
    owner.set_discoverable(false).unwrap();
    assert!(
        crate::discovery::discover_at(&[owner_network.discovery_address])
            .unwrap()
            .is_empty()
    );
    let response = controller
        .request(
            owner.id(),
            RemoteRequest::Hosts,
            &AtomicBool::new(false),
            &mut |_, _| Ok(()),
        )
        .unwrap();
    assert_eq!(response.hosts.len(), 1);
    assert!(!response.can_execute);
}

#[test]
fn wildcard_listeners_share_one_identity_across_both_families_and_release_all_ports() {
    let (_controller_directory, controller, _, _) = fixture("A");
    let (_owner_directory, owner, _, _) = fixture("B");
    let _controller_network = running(&controller);
    owner.set_password("correct horse battery").unwrap();
    owner.set_discoverable(true).unwrap();
    let network = Network::start(owner.clone(), std::net::Ipv4Addr::UNSPECIFIED, 0, 0).unwrap();
    assert_eq!(network.tcp_addresses().len(), 2);
    assert_eq!(network.discovery_addresses().len(), 2);
    let targets: Vec<_> = network
        .discovery_addresses()
        .iter()
        .map(|address| {
            let mut address = *address;
            address.set_ip(if address.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            });
            address
        })
        .collect();
    let found = crate::discovery::discover_at(&targets).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, owner.id());
    for endpoint in network.tcp_addresses() {
        let mut endpoint = *endpoint;
        endpoint.set_ip(if endpoint.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
        assert_eq!(controller.inspect(endpoint).unwrap().id, owner.id());
    }
    let tcp = network.tcp_addresses().to_vec();
    let udp = network.discovery_addresses().to_vec();
    drop(network);
    for endpoint in tcp {
        assert!(std::net::TcpListener::bind(endpoint).is_ok());
    }
    for endpoint in udp {
        assert!(crate::transport::udp_socket(endpoint).is_ok());
    }
}

#[test]
#[ignore = "sends IPv6 multicast on active local interfaces to an isolated ephemeral test port"]
fn ipv6_multicast_discovers_an_isolated_node_and_honors_hidden_mode() {
    let (_directory, owner, _, _) = fixture("multicast test");
    owner.set_password("correct horse battery").unwrap();
    let network = Network::start(owner.clone(), std::net::Ipv6Addr::UNSPECIFIED, 0, 0).unwrap();
    let port = network
        .discovery_addresses()
        .iter()
        .find(|address| address.is_ipv6())
        .unwrap()
        .port();
    let targets: Vec<_> = crate::interface::interfaces().iter().filter(|interface| interface.addresses.iter().any(|(address, _)| matches!(address, IpAddr::V6(address) if !address.is_loopback() && !address.is_unspecified() && !address.is_multicast())))
        .map(|interface| SocketAddr::V6(std::net::SocketAddrV6::new("ff12::534c:4c46".parse().unwrap(), port, 0, interface.index))).collect();
    assert!(
        !targets.is_empty(),
        "no active local interface has a usable IPv6 address"
    );
    assert!(crate::discovery::discover_at(&targets).unwrap().is_empty());
    owner.set_discoverable(true).unwrap();
    let found = crate::discovery::discover_at(&targets).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, owner.id());
    assert!(found[0].endpoint.is_ipv6());
    if let SocketAddr::V6(endpoint) = found[0].endpoint
        && endpoint.ip().is_unicast_link_local()
    {
        assert_ne!(endpoint.scope_id(), 0);
    }
    let (_controller_directory, controller, _, _) = fixture("multicast controller");
    controller.set_password("correct horse battery").unwrap();
    let _controller_network =
        Network::start(controller.clone(), std::net::Ipv6Addr::LOCALHOST, 0, 0).unwrap();
    let target = controller.inspect(found[0].endpoint).unwrap();
    controller
        .pair_password(target, "correct horse battery")
        .unwrap();
    owner.set_discoverable(false).unwrap();
    assert!(crate::discovery::discover_at(&targets).unwrap().is_empty());
    assert_eq!(
        controller
            .request(
                owner.id(),
                RemoteRequest::Status,
                &AtomicBool::new(false),
                &mut |_, _| Ok(())
            )
            .unwrap()
            .status,
        "accepted"
    );
}
