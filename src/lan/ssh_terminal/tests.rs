use std::{
    collections::BTreeSet,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use russh::{
    ChannelId, MethodKind, MethodSet,
    keys::{Algorithm, PrivateKey, known_hosts::learn_known_hosts_path},
    server::{self, Server as _},
};
use shellrs_lan::{Core, DeviceEvent, DeviceInput, DeviceSize, Executor, Network};

use crate::lan::adapter::HostExecutor;
use crate::{
    host::{AuthKind, HostDraft, HostStore},
    ssh::{SecretRef, SecretSource, SshConnector},
};

#[derive(Default)]
struct Seen {
    sizes: Vec<(u32, u32)>,
}

struct Listener {
    seen: Arc<Mutex<Seen>>,
    active: Arc<AtomicUsize>,
}

struct Handler {
    seen: Arc<Mutex<Seen>>,
    active: Arc<AtomicUsize>,
}

impl Drop for Handler {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

impl server::Server for Listener {
    type Handler = Handler;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Handler {
        self.active.fetch_add(1, Ordering::AcqRel);
        Handler {
            seen: self.seen.clone(),
            active: self.active.clone(),
        }
    }
}

impl server::Handler for Handler {
    type Error = russh::Error;
    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<server::Auth, Self::Error> {
        Ok(if user == "tester" && password == "password-only-on-B" {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }
    async fn channel_open_session(
        &mut self,
        _: russh::Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        columns: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &[(russh::Pty, u32)],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.seen.lock().unwrap().sizes.push((columns, rows));
        session.channel_success(channel)?;
        Ok(())
    }
    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        session.data(channel, b"real-ssh-ready\r\n".to_vec())?;
        Ok(())
    }
    async fn window_change_request(
        &mut self,
        _: ChannelId,
        columns: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.seen.lock().unwrap().sizes.push((columns, rows));
        Ok(())
    }
    async fn data(
        &mut self,
        channel: ChannelId,
        bytes: &[u8],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if bytes == b"exit\n" {
            session.eof(channel)?;
            session.exit_status_request(channel, 7)?;
            session.close(channel)?;
        } else {
            session.data(channel, bytes.to_vec())?;
        }
        Ok(())
    }
}

struct RunningServer {
    port: u16,
    key: russh::keys::PublicKey,
    seen: Arc<Mutex<Seen>>,
    active: Arc<AtomicUsize>,
    handle: server::RunningServerHandle,
    worker: Option<thread::JoinHandle<()>>,
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        self.handle.shutdown("test complete".into());
        self.worker.take().unwrap().join().unwrap();
    }
}

fn server() -> RunningServer {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let active = Arc::new(AtomicUsize::new(0));
    let mut listener = Listener {
        seen: seen.clone(),
        active: active.clone(),
    };
    let (ready, received) = mpsc::channel();
    let worker = thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let socket = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                    .await
                    .unwrap();
                let port = socket.local_addr().unwrap().port();
                let key = PrivateKey::random(&mut russh::keys::key::safe_rng(), Algorithm::Ed25519)
                    .unwrap();
                let public = key.public_key().clone();
                let configuration = Arc::new(server::Config {
                    keys: vec![key],
                    methods: MethodSet::from(&[MethodKind::Password][..]),
                    auth_rejection_time: Duration::ZERO,
                    auth_rejection_time_initial: Some(Duration::ZERO),
                    ..Default::default()
                });
                let running = listener.run_on_socket(configuration, &socket);
                ready.send((port, public, running.handle())).unwrap();
                running.await.unwrap();
            });
    });
    let (port, key, handle) = received.recv_timeout(Duration::from_secs(5)).unwrap();
    RunningServer {
        port,
        key,
        seen,
        active,
        handle,
        worker: Some(worker),
    }
}

struct Secrets {
    password: Mutex<Option<String>>,
    reads: AtomicUsize,
}

impl SecretSource for Secrets {
    fn secret(&self, _: &SecretRef) -> anyhow::Result<Option<zeroize::Zeroizing<String>>> {
        self.reads.fetch_add(1, Ordering::AcqRel);
        Ok(self
            .password
            .lock()
            .unwrap()
            .clone()
            .map(zeroize::Zeroizing::new))
    }
}

struct Stream {
    input: mpsc::Sender<DeviceInput>,
    events: mpsc::Receiver<DeviceEvent>,
    worker: thread::JoinHandle<anyhow::Result<()>>,
}

fn stream(a: &Arc<Core>, b: &Core, host: &str) -> Stream {
    let (input, commands) = mpsc::channel();
    let (events, received) = mpsc::channel();
    let core = a.clone();
    let peer = b.id().to_owned();
    let host = host.to_owned();
    let worker = thread::spawn(move || {
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
    Stream {
        input,
        events: received,
        worker,
    }
}

fn eventually(mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn real_shared_ssh_checks_b_host_keys_and_credentials_and_closes_the_remote_connection_on_revocation()
 {
    let server = server();
    let directory = tempfile::tempdir().unwrap();
    let known_hosts = directory.path().join("known_hosts");
    let secrets_b = Arc::new(Secrets {
        password: Mutex::new(Some("password-only-on-B".into())),
        reads: AtomicUsize::new(0),
    });
    let secrets_a = Arc::new(Secrets {
        password: Mutex::new(None),
        reads: AtomicUsize::new(0),
    });
    let executor = Arc::new(HostExecutor::new(SshConnector::new(
        &known_hosts,
        secrets_b.clone(),
    )));
    let mut hosts = HostStore::empty();
    hosts.insert_unnotified(HostDraft::new(
        "SSH on B",
        "127.0.0.1",
        server.port,
        "tester",
        AuthKind::Password,
        None,
    ));
    executor.update(&hosts);
    let host = executor.hosts()[0].id.clone();
    let a = Core::load(
        &directory.path().join("a"),
        "A",
        Arc::new(HostExecutor::new(SshConnector::new(
            directory.path().join("unused"),
            secrets_a.clone(),
        ))),
    )
    .unwrap();
    let b = Core::load(&directory.path().join("b"), "B", executor).unwrap();
    a.set_password("pairing password for A").unwrap();
    b.set_password("pairing password for B").unwrap();
    b.set_sharing(BTreeSet::from([host.clone()]), true).unwrap();
    let _a_network = Network::start(a.clone(), std::net::Ipv4Addr::LOCALHOST, 0, 0).unwrap();
    let b_network = Network::start(b.clone(), std::net::Ipv4Addr::LOCALHOST, 0, 0).unwrap();
    a.pair_password(
        a.inspect(b_network.tcp_address).unwrap(),
        "pairing password for B",
    )
    .unwrap();
    let rejected = stream(&a, &b, &host);
    assert!(
        rejected
            .worker
            .join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("shared_host_key_unknown")
    );
    let stale = PrivateKey::random(&mut russh::keys::key::safe_rng(), Algorithm::Ed25519).unwrap();
    learn_known_hosts_path("127.0.0.1", server.port, stale.public_key(), &known_hosts).unwrap();
    let rejected = stream(&a, &b, &host);
    assert!(
        rejected
            .worker
            .join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("shared_host_key_changed")
    );
    std::fs::write(&known_hosts, "").unwrap();
    learn_known_hosts_path("127.0.0.1", server.port, &server.key, &known_hosts).unwrap();
    *secrets_b.password.lock().unwrap() = None;
    let rejected = stream(&a, &b, &host);
    assert!(
        rejected
            .worker
            .join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("shared_host_credentials_missing")
    );
    *secrets_b.password.lock().unwrap() = Some("password-only-on-B".into());
    let terminal = stream(&a, &b, &host);
    assert_eq!(
        terminal
            .events
            .recv_timeout(Duration::from_secs(5))
            .unwrap(),
        DeviceEvent::Started
    );
    assert_eq!(
        terminal
            .events
            .recv_timeout(Duration::from_secs(5))
            .unwrap(),
        DeviceEvent::Output(b"real-ssh-ready\r\n".to_vec())
    );
    terminal
        .input
        .send(DeviceInput::Write(vec![0, 255, 3]))
        .unwrap();
    assert_eq!(
        terminal
            .events
            .recv_timeout(Duration::from_secs(5))
            .unwrap(),
        DeviceEvent::Output(vec![0, 255, 3])
    );
    terminal
        .input
        .send(DeviceInput::Resize(DeviceSize::new(120, 40).unwrap()))
        .unwrap();
    assert!(eventually(|| server
        .seen
        .lock()
        .unwrap()
        .sizes
        .contains(&(120, 40))));
    terminal
        .input
        .send(DeviceInput::Write(b"exit\n".to_vec()))
        .unwrap();
    assert_eq!(
        terminal
            .events
            .recv_timeout(Duration::from_secs(5))
            .unwrap(),
        DeviceEvent::Exited {
            code: 7,
            signal: None
        }
    );
    terminal.worker.join().unwrap().unwrap();
    assert!(secrets_b.reads.load(Ordering::Acquire) > 0);
    assert_eq!(secrets_a.reads.load(Ordering::Acquire), 0);
    assert!(!format!("{:?}", b.hosts()).contains("password-only-on-B"));
    let terminal = stream(&a, &b, &host);
    assert_eq!(
        terminal
            .events
            .recv_timeout(Duration::from_secs(5))
            .unwrap(),
        DeviceEvent::Started
    );
    b.set_execution_permission(a.id(), false).unwrap();
    assert!(terminal.worker.join().unwrap().is_err());
    assert!(eventually(|| server.active.load(Ordering::Acquire) == 0));
    assert!(eventually(|| b.active_devices().is_empty()));
}
