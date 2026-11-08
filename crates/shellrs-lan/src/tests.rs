use super::*;
use anyhow::bail;

struct FakeExecutor {
    cancelled: Arc<AtomicBool>,
    started: Arc<AtomicBool>,
}
impl Executor for FakeExecutor {
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
fn only_private_ipv4_addresses_and_strict_messages_are_accepted() {
    assert!(parse_endpoint("192.168.1.2").is_ok());
    assert!(parse_endpoint("127.0.0.1:12345").is_ok());
    for value in [
        "8.8.8.8:22",
        "0.0.0.0:22",
        "[::1]:22",
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
