use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, UdpSocket};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::{Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use russh::{
    ChannelId, ChannelMsg,
    keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate},
    server,
};
use serde::{Deserialize, Serialize};

use crate::{
    Core, Device, DeviceInput, DeviceSize, MAX_COMMAND, MAX_OUTPUT, SharedDevice, SharedHost,
    clean_name, fingerprint, is_lan,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RemoteRequest {
    Status,
    WakeInfo,
    Hosts,
    Execute {
        host: String,
        command: String,
    },
    Devices,
    OpenDevice {
        device: String,
        columns: u16,
        rows: u16,
    },
    OpenHost {
        host: String,
        columns: u16,
        rows: u16,
    },
    OpenAdb {
        device: String,
        service: String,
    },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Response {
    pub name: String,
    pub status: String,
    pub hosts: Vec<SharedHost>,
    pub can_execute: bool,
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<SharedDevice>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wake_interfaces: Vec<crate::WakeInterface>,
}

pub struct Network {
    core: Arc<Core>,
    stop: Arc<AtomicBool>,
    workers: Vec<std::thread::JoinHandle<()>>,
    pub tcp_address: SocketAddr,
    pub discovery_address: SocketAddr,
    tcp_addresses: Vec<SocketAddr>,
    discovery_addresses: Vec<SocketAddr>,
}

pub(super) fn family_unavailable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Unsupported | io::ErrorKind::AddrNotAvailable
    ) || matches!(error.raw_os_error(), Some(47 | 97 | 10047 | 10049))
}

pub(super) fn udp_socket(address: SocketAddr) -> io::Result<UdpSocket> {
    let domain = if address.is_ipv6() {
        socket2::Domain::IPV6
    } else {
        socket2::Domain::IPV4
    };
    let socket = socket2::Socket::new(domain, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))?;
    if address.is_ipv6() {
        socket.set_only_v6(true)?;
        socket.set_multicast_hops_v6(1)?;
    }
    socket.bind(&address.into())?;
    Ok(socket.into())
}

fn tcp_listener(address: SocketAddr) -> io::Result<TcpListener> {
    let domain = if address.is_ipv6() {
        socket2::Domain::IPV6
    } else {
        socket2::Domain::IPV4
    };
    let socket = socket2::Socket::new(domain, socket2::Type::STREAM, Some(socket2::Protocol::TCP))?;
    if address.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.bind(&address.into())?;
    socket.listen(64)?;
    socket.set_nonblocking(true)?;
    Ok(socket.into())
}

impl Network {
    pub fn start(
        core: Arc<Core>,
        address: impl Into<IpAddr>,
        tcp_port: u16,
        discovery_port: u16,
    ) -> Result<Self> {
        ensure!(core.has_password(), "pairing_password_required");
        ensure!(!core.is_running(), "LAN cooperation already running");
        let address = address.into();
        let mut addresses = vec![address];
        if address.is_unspecified() {
            addresses.push(if address.is_ipv4() {
                Ipv6Addr::UNSPECIFIED.into()
            } else {
                Ipv4Addr::UNSPECIFIED.into()
            });
        }
        let mut listeners = Vec::new();
        let mut sockets = Vec::new();
        let mut tcp_addresses = Vec::new();
        let mut discovery_addresses = Vec::new();
        let mut ports = (tcp_port, discovery_port);
        for address in addresses {
            let binding = (|| -> io::Result<_> {
                let tcp = tcp_listener(SocketAddr::new(address, ports.0))?;
                let udp = udp_socket(SocketAddr::new(address, ports.1))?;
                Ok((tcp, udp))
            })();
            let (tcp, udp) = match binding {
                Ok(bound) => bound,
                Err(error) if address.is_unspecified() && family_unavailable(&error) => continue,
                Err(error) => return Err(error.into()),
            };
            ports = (tcp.local_addr()?.port(), udp.local_addr()?.port());
            tcp_addresses.push(tcp.local_addr()?);
            discovery_addresses.push(udp.local_addr()?);
            listeners.push(tcp);
            sockets.push(udp);
        }
        ensure!(!listeners.is_empty(), "local_network_unavailable");
        let tcp_address = tcp_addresses[0];
        let discovery_address = discovery_addresses[0];
        let stop = Arc::new(AtomicBool::new(false));
        let mut network = Self {
            core: core.clone(),
            stop: stop.clone(),
            workers: Vec::new(),
            tcp_address,
            discovery_address,
            tcp_addresses,
            discovery_addresses,
        };
        core.running.store(true, Ordering::Release);
        if core.is_active() {
            core.sleep.enable();
        }
        let discovery_core = core.clone();
        let discovery_stop = stop.clone();
        network.workers.push(
            std::thread::Builder::new()
                .name("shelllf-lan-discovery".into())
                .spawn(move || {
                    crate::discovery::respond(
                        discovery_core,
                        sockets,
                        tcp_address.port(),
                        discovery_stop,
                    );
                })?,
        );
        let server_core = core.clone();
        let server_stop = stop.clone();
        network.workers.push(std::thread::Builder::new().name("shelllf-lan-server".into()).spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() else { server_core.running.store(false, Ordering::Release); return };
            runtime.block_on(async {
                let listeners: Result<Vec<_>, _> = listeners.into_iter().map(tokio::net::TcpListener::from_std).collect();
                let Ok(listeners) = listeners else { return };
                let configuration = Arc::new(server::Config {
                    keys: vec![(*server_core.key).clone()],
                    auth_rejection_time: Duration::from_millis(700),
                    auth_rejection_time_initial: Some(Duration::from_millis(700)),
                    max_auth_attempts: 4,
                    inactivity_timeout: Some(Duration::from_secs(150)),
                    keepalive_interval: Some(Duration::from_secs(30)),
                    keepalive_max: 2,
                    ..Default::default()
                });
                let connections = Arc::new(tokio::sync::Semaphore::new(16));
                while !server_stop.load(Ordering::Acquire) {
                    let accepted = tokio::select! {
                        result = std::future::poll_fn(|task_context| {
                            for listener in &listeners {
                                if let std::task::Poll::Ready(result) = listener.poll_accept(task_context) { return std::task::Poll::Ready(result); }
                            }
                            std::task::Poll::Pending
                        }) => result,
                        _ = tokio::time::sleep(Duration::from_millis(100)) => continue
                    };
                    let Ok((stream, address)) = accepted else { continue };
                    if stream.set_nodelay(true).is_err() { continue; }
                    let address = crate::address::normalize_endpoint(address);
                    if !is_lan(address.ip()) { continue; }
                    let Ok(permit) = connections.clone().try_acquire_owned() else { continue };
                    let handler = PeerHandler { core: server_core.clone(), address, key: None, peer: None, channel: None,
                        used: false, cancel: Arc::new(AtomicBool::new(false)), device_input: None, input_buffer: Vec::new(), _permit: permit };
                    let configuration = configuration.clone();
                    tokio::spawn(async move {
                        if let Ok(Ok(session)) = tokio::time::timeout(Duration::from_secs(5), server::run_stream(configuration, stream, handler)).await {
                            let _ = session.await;
                        }
                    });
                }
            });
            server_core.running.store(false, Ordering::Release);
        })?);
        Ok(network)
    }

    pub fn tcp_addresses(&self) -> &[SocketAddr] {
        &self.tcp_addresses
    }
    pub fn discovery_addresses(&self) -> &[SocketAddr] {
        &self.discovery_addresses
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        self.core.running.store(false, Ordering::Release);
        self.core.sleep.reset();
        self.core.invalidate_device_access();
        self.stop.store(true, Ordering::Release);
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

struct PeerHandler {
    core: Arc<Core>,
    address: SocketAddr,
    key: Option<russh::keys::PublicKey>,
    peer: Option<String>,
    channel: Option<ChannelId>,
    used: bool,
    cancel: Arc<AtomicBool>,
    device_input: Option<tokio::sync::mpsc::Sender<DeviceInput>>,
    input_buffer: Vec<u8>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

fn claims(user: &str) -> Option<(&str, String)> {
    let text = user.strip_prefix("shelllf-lan-v1:")?;
    if text.len() > 512 {
        return None;
    }
    let (mode, name) = text.split_once(':')?;
    if !["inspect", "request", "trusted", "password"].contains(&mode) {
        return None;
    }
    let name = String::from_utf8(URL_SAFE_NO_PAD.decode(name).ok()?).ok()?;
    Some((mode, clean_name(&name)))
}

impl server::Handler for PeerHandler {
    type Error = russh::Error;

    async fn authentication_banner(&mut self) -> Result<Option<String>, Self::Error> {
        Ok(self
            .core
            .is_blocked(self.address.ip())
            .then(|| "shelllf:blocked_by_device".into()))
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        key: &russh::keys::PublicKey,
    ) -> Result<server::Auth, Self::Error> {
        let Some((mode, name)) = claims(user) else {
            return Ok(server::Auth::reject());
        };
        if !self.core.is_active() || self.core.is_blocked(self.address.ip()) {
            return Ok(server::Auth::reject());
        }
        self.key = Some(key.clone());
        self.peer = Some(fingerprint(key));
        if mode == "password" || mode == "inspect" {
            return Ok(server::Auth::reject());
        }
        self.core
            .authenticate(&name, key, self.address, mode == "request");
        Ok(server::Auth::Accept)
    }

    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<server::Auth, Self::Error> {
        let Some(("password", name)) = claims(user) else {
            return Ok(server::Auth::reject());
        };
        let Some(key) = self.key.clone() else {
            return Ok(server::Auth::reject());
        };
        let Ok(permit) = self.core.password_slots.clone().try_acquire_owned() else {
            return Ok(server::Auth::reject());
        };
        let core = self.core.clone();
        let address = self.address.ip();
        let password = zeroize::Zeroizing::new(password.to_owned());
        let accepted = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            core.authenticate_password(&name, &key, address, &password)
                .unwrap_or(false)
        })
        .await
        .unwrap_or(false);
        Ok(if accepted {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }

    async fn channel_open_session(
        &mut self,
        channel: russh::Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if self.channel.is_none() && self.peer.is_some() {
            self.channel = Some(channel.id());
            reply.accept().await;
        }
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if self.used || data.len() > MAX_COMMAND * 6 + 1024 {
            session.channel_failure(channel)?;
            return Ok(());
        }
        self.used = true;
        let Ok(request) = serde_json::from_slice::<RemoteRequest>(data) else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        let Some(peer) = self.peer.clone() else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        let opening = match &request {
            RemoteRequest::OpenDevice {
                device,
                columns,
                rows,
            } => Some((device.clone(), *columns, *rows, None, false)),
            RemoteRequest::OpenHost {
                host,
                columns,
                rows,
            } => Some((host.clone(), *columns, *rows, None, true)),
            RemoteRequest::OpenAdb { device, service } => {
                Some((device.clone(), 80, 24, Some(service.clone()), false))
            }
            _ => None,
        };
        if let Some((device, columns, rows, service, host)) = opening {
            let size = DeviceSize::new(columns, rows).map_err(|error| {
                russh::Error::IO(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    error.to_string(),
                ))
            })?;
            let (sender, receiver) = tokio::sync::mpsc::channel(32);
            self.device_input = Some(sender);
            session.channel_success(channel)?;
            crate::device_transport::start(
                self.core.clone(),
                crate::device_transport::OpenRequest {
                    peer,
                    device,
                    source: self.address.ip(),
                    size,
                    service,
                    host,
                },
                receiver,
                self.cancel.clone(),
                channel,
                session.handle(),
            );
            return Ok(());
        }
        session.channel_success(channel)?;
        let core = self.core.clone();
        let address = self.address.ip();
        let cancel = self.cancel.clone();
        let handle = session.handle();
        tokio::spawn(async move {
            let result = serve(
                core,
                peer,
                address,
                channel,
                &handle,
                request,
                cancel.clone(),
            )
            .await;
            cancel.store(true, Ordering::Release);
            if let Err(error) = result {
                let reason = error.to_string().chars().take(2048).collect::<String>();
                let _ = tokio::time::timeout(
                    Duration::from_secs(1),
                    handle.extended_data(channel, 2, reason.into_bytes()),
                )
                .await;
                let _ = handle.exit_status_request(channel, 255).await;
            }
            let _ = handle.eof(channel).await;
            let _ = handle.close(channel).await;
        });
        Ok(())
    }

    async fn channel_close(
        &mut self,
        _: ChannelId,
        _: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.cancel.store(true, Ordering::Release);
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        let Some(sender) = &self.device_input else {
            if !data.is_empty() {
                self.cancel.store(true, Ordering::Release);
                session.close(channel)?;
            }
            return Ok(());
        };
        if self.cancel.load(Ordering::Acquire)
            || self.input_buffer.len().saturating_add(data.len()) > 128 * 1024
        {
            self.cancel.store(true, Ordering::Release);
            session.close(channel)?;
            return Ok(());
        }
        self.input_buffer.extend_from_slice(data);
        loop {
            match crate::devices::take_input(&mut self.input_buffer) {
                Ok(Some(input)) => {
                    if !matches!(
                        tokio::time::timeout(Duration::from_millis(200), sender.send(input)).await,
                        Ok(Ok(()))
                    ) {
                        self.cancel.store(true, Ordering::Release);
                        session.close(channel)?;
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    self.cancel.store(true, Ordering::Release);
                    session.close(channel)?;
                    break;
                }
            }
        }
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        _: ChannelId,
        _: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if self.device_input.is_some() {
            self.cancel.store(true, Ordering::Release);
        }
        Ok(())
    }
    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)?;
        Ok(())
    }
}

impl Drop for PeerHandler {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

enum Output {
    Bytes(bool, Vec<u8>),
    End(Result<i32>),
}

async fn serve(
    core: Arc<Core>,
    peer: String,
    address: std::net::IpAddr,
    channel: ChannelId,
    handle: &server::Handle,
    request: RemoteRequest,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    ensure!(!core.is_blocked(address), "blocked_by_device");
    ensure!(core.is_active(), "LAN cooperation unavailable or locked");
    match request {
        RemoteRequest::WakeInfo => {
            ensure!(core.authorized(&peer, None, false), "device_not_authorized");
            let response = Response {
                wake_interfaces: crate::wake::local_interfaces(),
                ..Default::default()
            };
            handle
                .data(channel, serde_json::to_vec(&response)?)
                .await
                .map_err(|_| anyhow::anyhow!("disconnected"))?;
            handle
                .exit_status_request(channel, 0)
                .await
                .map_err(|_| anyhow::anyhow!("disconnected"))?;
        }
        RemoteRequest::Devices => {
            ensure!(core.authorized(&peer, None, false), "device_not_authorized");
            let _usage = core.keep_awake()?;
            let devices = core
                .local_devices()
                .into_iter()
                .filter(|device| core.device_authorized(&peer, &device.id, address))
                .take(256)
                .collect();
            let response = Response {
                name: core.name.clone(),
                status: "accepted".into(),
                devices,
                ..Default::default()
            };
            let bytes = serde_json::to_vec(&response)?;
            ensure!(bytes.len() <= MAX_OUTPUT, "device list too large");
            handle
                .data(channel, bytes)
                .await
                .map_err(|_| anyhow::anyhow!("disconnected"))?;
            handle
                .exit_status_request(channel, 0)
                .await
                .map_err(|_| anyhow::anyhow!("disconnected"))?;
        }
        RemoteRequest::OpenDevice { .. }
        | RemoteRequest::OpenAdb { .. }
        | RemoteRequest::OpenHost { .. } => {
            bail!("invalid_device_request")
        }
        RemoteRequest::Status => {
            let response = Response {
                name: core.name.clone(),
                status: core.pair_status(&peer, address),
                ..Default::default()
            };
            handle
                .data(channel, serde_json::to_vec(&response)?)
                .await
                .map_err(|_| anyhow::anyhow!("disconnected"))?;
            handle
                .exit_status_request(channel, 0)
                .await
                .map_err(|_| anyhow::anyhow!("disconnected"))?;
        }
        RemoteRequest::Hosts => {
            ensure!(core.authorized(&peer, None, false), "device_not_authorized");
            let _usage = core.keep_awake()?;
            let hosts = core
                .executor
                .hosts()
                .into_iter()
                .filter(|host| core.authorized(&peer, Some(&host.id), false))
                .take(256)
                .collect();
            let response = Response {
                name: core.name.clone(),
                status: "accepted".into(),
                hosts,
                can_execute: core.authorized(&peer, None, true),
                ..Default::default()
            };
            let bytes = serde_json::to_vec(&response)?;
            ensure!(bytes.len() <= MAX_OUTPUT, "host list too large");
            handle
                .data(channel, bytes)
                .await
                .map_err(|_| anyhow::anyhow!("disconnected"))?;
            handle
                .exit_status_request(channel, 0)
                .await
                .map_err(|_| anyhow::anyhow!("disconnected"))?;
        }
        RemoteRequest::Execute { host, command } => {
            ensure!(
                !command.trim().is_empty() && command.len() <= MAX_COMMAND && host.len() <= 128,
                "invalid command"
            );
            ensure!(
                core.authorized(&peer, Some(&host), true),
                "execution_not_authorized"
            );
            let _usage = core.keep_awake()?;
            let permit = core
                .command_slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| anyhow::anyhow!("too_many_commands"))?;
            let (sender, mut receiver) = tokio::sync::mpsc::channel(32);
            let worker_core = core.clone();
            let worker_cancel = cancel.clone();
            let worker_host = host.clone();
            std::thread::Builder::new()
                .name("shelllf-lan-command".into())
                .spawn(move || {
                    let _permit = permit;
                    let result = worker_core.executor.execute(
                        &worker_host,
                        &command,
                        &worker_cancel,
                        &mut |stderr, bytes| {
                            if worker_cancel.load(Ordering::Acquire) {
                                return Err(io::ErrorKind::Interrupted.into());
                            }
                            for chunk in bytes.chunks(8192) {
                                sender
                                    .blocking_send(Output::Bytes(stderr, chunk.to_vec()))
                                    .map_err(|_| io::ErrorKind::BrokenPipe)?;
                            }
                            Ok(())
                        },
                    );
                    let _ = sender.blocking_send(Output::End(result));
                })?;
            let deadline = Instant::now() + Duration::from_secs(120);
            let mut output_bytes = 0usize;
            loop {
                ensure!(
                    Instant::now() < deadline && !cancel.load(Ordering::Acquire),
                    "command_cancelled_or_timed_out"
                );
                ensure!(!core.is_blocked(address), "blocked_by_device");
                ensure!(
                    core.authorized(&peer, Some(&host), true),
                    "execution_not_authorized"
                );
                let received = tokio::select! { value = receiver.recv() => value, _ = tokio::time::sleep(Duration::from_millis(100)) => continue };
                match received {
                    Some(Output::Bytes(stderr, bytes)) => {
                        output_bytes = output_bytes.saturating_add(bytes.len());
                        ensure!(output_bytes <= MAX_OUTPUT, "output_limit_reached");
                        let send = async {
                            if stderr {
                                handle.extended_data(channel, 1, bytes).await
                            } else {
                                handle.data(channel, bytes).await
                            }
                        };
                        tokio::time::timeout(Duration::from_secs(2), send)
                            .await?
                            .map_err(|_| anyhow::anyhow!("disconnected"))?;
                    }
                    Some(Output::End(result)) => {
                        handle
                            .exit_status_request(channel, result? as u32)
                            .await
                            .map_err(|_| anyhow::anyhow!("disconnected"))?;
                        break;
                    }
                    None => bail!("command ended without a result"),
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone, Default)]
pub(super) struct Seen {
    key: Option<russh::keys::PublicKey>,
    blocked: bool,
}
pub(super) struct ClientHandler {
    expected: Option<String>,
    seen: Arc<Mutex<Seen>>,
}
impl russh::client::Handler for ClientHandler {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = key.public_key();
        let id = fingerprint(&key);
        self.seen.lock().unwrap().key = Some(key);
        Ok(self
            .expected
            .as_ref()
            .is_none_or(|expected| expected == &id))
    }
    async fn auth_banner(
        &mut self,
        banner: &str,
        _: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        if banner == "shelllf:blocked_by_device" {
            self.seen.lock().unwrap().blocked = true;
        }
        Ok(())
    }
}

pub(super) fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

pub(super) async fn connect(
    core: &Core,
    endpoint: SocketAddr,
    expected: Option<String>,
    mode: &str,
    password: Option<&str>,
    cancelled: &AtomicBool,
    wake: bool,
) -> Result<(russh::client::Handle<ClientHandler>, Seen, bool)> {
    let endpoint = crate::address::normalize_endpoint(endpoint);
    ensure!(
        core.is_active()
            && crate::address::valid_stored_endpoint(endpoint)
            && is_lan(endpoint.ip()),
        "LAN cooperation unavailable"
    );
    let seen = Arc::new(Mutex::new(Seen::default()));
    let configuration = Arc::new(russh::client::Config {
        nodelay: true,
        inactivity_timeout: Some(Duration::from_secs(150)),
        keepalive_interval: Some(Duration::from_secs(30)),
        ..Default::default()
    });
    let stream = crate::wake::connect(
        core,
        endpoint,
        expected
            .as_deref()
            .filter(|peer| wake && mode == "trusted" && core.has_peer(peer)),
        cancelled,
    )
    .await?;
    stream.set_nodelay(true)?;
    let mut session = tokio::time::timeout(
        Duration::from_secs(5),
        russh::client::connect_stream(
            configuration,
            stream,
            ClientHandler {
                expected,
                seen: seen.clone(),
            },
        ),
    )
    .await??;
    let user = format!(
        "shelllf-lan-v1:{mode}:{}",
        URL_SAFE_NO_PAD.encode(core.name.as_bytes())
    );
    let authenticated: Result<bool> = async {
        let mut accepted = tokio::time::timeout(
            Duration::from_secs(5),
            session.authenticate_publickey(
                user.clone(),
                PrivateKeyWithHashAlg::new(core.key.clone(), None),
            ),
        )
        .await??
        .success();
        if seen.lock().unwrap().blocked {
            bail!("blocked_by_device");
        }
        if let Some(password) = password {
            accepted = tokio::time::timeout(
                Duration::from_secs(5),
                session.authenticate_password(user, password),
            )
            .await??
            .success();
        }
        Ok(accepted)
    }
    .await;
    let accepted = match authenticated {
        Ok(accepted) => accepted,
        Err(error) => {
            let _ = tokio::time::timeout(
                Duration::from_secs(1),
                session.disconnect(
                    russh::Disconnect::ByApplication,
                    "authentication ended",
                    "en",
                ),
            )
            .await;
            return Err(error);
        }
    };
    let found = seen.lock().unwrap().clone();
    Ok((session, found, accepted))
}

pub(super) fn inspect(core: &Core, endpoint: SocketAddr) -> Result<Device> {
    runtime()?.block_on(async {
        let (session, seen, _) = connect(
            core,
            endpoint,
            None,
            "inspect",
            None,
            &AtomicBool::new(false),
            false,
        )
        .await?;
        let key = seen
            .key
            .ok_or_else(|| anyhow::anyhow!("device_identity_unavailable"))?;
        let _ = session
            .disconnect(
                russh::Disconnect::ByApplication,
                "inspection complete",
                "en",
            )
            .await;
        Ok(Device {
            id: fingerprint(&key),
            name: endpoint.to_string(),
            endpoint,
            public_key: key.to_openssh()?,
        })
    })
}

pub(super) fn request_pair(core: &Core, device: &Device) -> Result<()> {
    runtime()?.block_on(async {
        let (session, _, accepted) = connect(
            core,
            device.endpoint,
            Some(device.id.clone()),
            "request",
            None,
            &AtomicBool::new(false),
            false,
        )
        .await?;
        let _ = session
            .disconnect(russh::Disconnect::ByApplication, "request sent", "en")
            .await;
        ensure!(accepted, "pairing_request_refused");
        Ok(())
    })
}

pub(super) fn pair_password(core: &Core, device: &Device, password: &str) -> Result<()> {
    runtime()?.block_on(async {
        let (session, _, accepted) = connect(
            core,
            device.endpoint,
            Some(device.id.clone()),
            "password",
            Some(password),
            &AtomicBool::new(false),
            false,
        )
        .await?;
        let _ = session
            .disconnect(russh::Disconnect::ByApplication, "paired", "en")
            .await;
        ensure!(accepted, "pairing_password_refused");
        Ok(())
    })
}

pub(super) fn request(
    core: &Core,
    device: &Device,
    request: RemoteRequest,
    cancel: &AtomicBool,
    output: &mut dyn FnMut(bool, &[u8]) -> io::Result<()>,
) -> Result<Response> {
    runtime()?.block_on(async {
        let (session, _, accepted) = connect(core, device.endpoint, Some(device.id.clone()), "trusted", None, cancel, matches!(request, RemoteRequest::Hosts | RemoteRequest::Devices | RemoteRequest::Execute { .. })).await?;
        let result: Result<Response> = async {
        ensure!(!cancel.load(Ordering::Acquire) && core.is_active(), "command_cancelled_or_timed_out");
        ensure!(accepted, "device_not_authorized");
        let execute = matches!(&request, RemoteRequest::Execute { .. });
        let mut channel = tokio::time::timeout(Duration::from_secs(5), session.channel_open_session()).await??;
        channel.exec(true, serde_json::to_vec(&request)?).await?;
        channel.eof().await?;
        let deadline = Instant::now() + Duration::from_secs(125);
        let mut response = Vec::new();
        let mut exit = None;
        let mut error = None;
        let mut received = 0usize;
        loop {
            ensure!(Instant::now() < deadline && !cancel.load(Ordering::Acquire) && core.is_active(), "command_cancelled_or_timed_out");
            let message = tokio::select! { message = channel.wait() => message, _ = tokio::time::sleep(Duration::from_millis(50)) => continue };
            match message {
                Some(ChannelMsg::Data { data }) if !execute => { response.extend_from_slice(&data); ensure!(response.len() <= MAX_OUTPUT, "response too large"); }
                Some(ChannelMsg::Data { data }) => { received += data.len(); ensure!(received <= MAX_OUTPUT, "output_limit_reached"); output(false, &data)?; }
                Some(ChannelMsg::ExtendedData { data, ext: 1 }) => { received += data.len(); ensure!(received <= MAX_OUTPUT, "output_limit_reached"); output(true, &data)?; }
                Some(ChannelMsg::ExtendedData { data, ext: 2 }) => { ensure!(data.len() <= 4096, "invalid error"); error = Some(String::from_utf8_lossy(&data).into_owned()); }
                Some(ChannelMsg::ExitStatus { exit_status }) => exit = Some(exit_status as i32),
                Some(ChannelMsg::Failure) => bail!("request_refused"),
                Some(ChannelMsg::Close) | None => break,
                _ => {}
            }
        }
        if let Some(error) = error { bail!("{error}"); }
        let code = exit.ok_or_else(|| anyhow::anyhow!("connection_ended_without_result"))?;
        if execute { Ok(Response { exit_code: Some(code), ..Default::default() }) }
        else {
            let response: Response = serde_json::from_slice(&response)?;
            crate::wake::validate(&response.wake_interfaces)?;
            ensure!(response.devices.len() <= 256, "invalid_device_catalog");
            let mut ids = std::collections::BTreeSet::new();
            for device in &response.devices {
                device.validate()?;
                ensure!(ids.insert(&device.id), "invalid_device_catalog");
            }
            Ok(response)
        }
        }.await;
        let _ = tokio::time::timeout(Duration::from_secs(1), session.disconnect(russh::Disconnect::ByApplication, "complete", "en")).await;
        result
    })
}

pub(super) use crate::device_transport::stream_device;
