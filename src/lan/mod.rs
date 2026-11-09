mod adapter;
mod device_adapter;
mod device_page;
pub mod firewall;
mod page;
mod prompt;
mod ssh_terminal;
mod terminal;

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

use gpui_kit::{AppContext as _, Context, Entity, EventEmitter, Task};
use shellrs_lan::{
    Core, Device, Grant, Network, Pending, RemoteRequest, Response, SharedDevice, SharedHost,
};

use crate::cli::Secret;
use crate::host::HostStore;
use crate::i18n::t;

pub(crate) use error_text as describe_error;
pub use page::LanPage;
pub(crate) use prompt::show_pair_request;
pub(crate) use terminal::DeviceTerminalFactory;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LanCommand {
    SavePassword(Secret),
    Enable,
    Disable,
    SetDiscoverable(bool),
    CheckFirewall,
    ConfigureFirewall,
    RemoveFirewall,
    Discover,
    Inspect(String),
    RequestPair,
    PasswordPair(Secret),
    ShareHost(String, bool),
    AllowExecution(bool),
    Approve(String, u64, bool),
    Reject(String, u64, bool),
    Revoke(String),
    SetExecutionPermission(String, bool),
    Forget(String),
    Unblock(IpAddr),
    RefreshHosts(String),
    SelectHost(String),
    Execute(String),
    Cancel,
    RefreshLocalDevices,
    ShareDevice(String, bool),
    EnableDeviceAccess(bool),
    GrantDevice {
        peer: String,
        device: String,
        allowed: bool,
    },
    RevokeDevices(String),
    DisconnectDevice(u64),
    RefreshDevices(String),
    OpenDevice {
        peer: String,
        device: String,
    },
    OpenHost {
        peer: String,
        host: String,
    },
    ManageAdb {
        peer: String,
        device: String,
    },
    AdbFiles {
        peer: String,
        device: String,
    },
    ManageDevicePermissions(String),
    ApproveDevices(String, u64),
}

pub enum LanEvent {
    Tick,
}

enum Completion {
    Loaded(Arc<Core>, Option<Network>, Option<String>),
    Done,
    Discovered(Vec<Device>),
    Inspected(Device),
    Waiting(Device),
    Paired,
    Hosts(String, Response),
    DeviceList(String, Vec<SharedDevice>),
}

pub struct LanService {
    firewall_backend: Arc<dyn firewall::FirewallBackend>,
    firewall_state: firewall::FirewallState,
    firewall_task: Option<Task<()>>,
    core: Option<Arc<Core>>,
    network: Option<Network>,
    adapter: Option<Arc<adapter::HostExecutor>>,
    device_adapter: Option<Arc<device_adapter::DeviceCatalog>>,
    pub(super) device_peer: Option<String>,
    pub(super) remote_devices: Vec<SharedDevice>,
    pub(super) permission_peer: Option<String>,
    pub(super) devices: Vec<Device>,
    pub(super) candidate: Option<Device>,
    waiting: Option<Device>,
    pub(super) peer: Option<String>,
    pub(super) remote: Vec<SharedHost>,
    pub(super) host: Option<String>,
    pub(super) can_execute: bool,
    pub(super) status: String,
    pub(super) busy: bool,
    pub(super) running: bool,
    pub(super) output: Vec<u8>,
    pub(super) exit_code: Option<i32>,
    pub(super) prompt_seen: BTreeSet<(String, u64)>,
    pub(super) prompt_active: Option<(String, u64)>,
    cancel: Arc<AtomicBool>,
    stream: Option<mpsc::Receiver<(bool, Vec<u8>)>>,
    task: Option<Task<()>>,
    command_task: Option<Task<()>>,
    pair_task: Option<Task<()>>,
    poll: Option<Task<()>>,
    revision: u64,
    ticks: u64,
}

impl EventEmitter<LanEvent> for LanService {}

impl LanService {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut service = Self {
            firewall_backend: Arc::new(firewall::SystemFirewall),
            firewall_state: firewall::FirewallState::Unchecked,
            firewall_task: None,
            core: None,
            network: None,
            adapter: None,
            device_adapter: None,
            device_peer: None,
            remote_devices: Vec::new(),
            permission_peer: None,
            devices: Vec::new(),
            candidate: None,
            waiting: None,
            peer: None,
            remote: Vec::new(),
            host: None,
            can_execute: false,
            status: String::new(),
            busy: false,
            running: false,
            output: Vec::new(),
            exit_code: None,
            prompt_seen: BTreeSet::new(),
            prompt_active: None,
            cancel: Arc::new(AtomicBool::new(false)),
            stream: None,
            task: None,
            command_task: None,
            pair_task: None,
            poll: None,
            revision: 0,
            ticks: 0,
        };
        service.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                if this.update(cx, |this, cx| this.poll(cx)).is_err() {
                    break;
                }
            }
        }));
        service
    }

    pub fn core(&self) -> Option<Arc<Core>> {
        self.core.clone()
    }
    pub fn attach_core(&mut self, core: Arc<Core>, cx: &mut Context<Self>) {
        self.core = Some(core);
        self.status = t!("lan.ready").to_string();
        cx.notify();
    }
    pub fn enabled(&self) -> bool {
        self.core.as_ref().is_some_and(|core| core.is_running())
    }
    pub fn available(&self) -> bool {
        self.core.is_some()
    }
    pub fn has_password(&self) -> bool {
        self.core.as_ref().is_some_and(|core| core.has_password())
    }
    pub fn local_hosts(&self) -> Vec<SharedHost> {
        self.core
            .as_ref()
            .map(|core| core.hosts())
            .unwrap_or_default()
    }
    pub fn selected_hosts(&self) -> BTreeSet<String> {
        self.core
            .as_ref()
            .map(|core| core.shared_hosts())
            .unwrap_or_default()
    }
    pub fn permits_execution(&self) -> bool {
        self.core.as_ref().is_some_and(|core| core.allow_execute())
    }
    pub fn is_remote_execution_allowed(&self) -> bool {
        self.can_execute
    }
    pub fn peers(&self) -> Vec<Device> {
        self.core
            .as_ref()
            .map(|core| core.peers())
            .unwrap_or_default()
    }
    pub fn grants(&self) -> Vec<Grant> {
        self.core
            .as_ref()
            .map(|core| core.grants())
            .unwrap_or_default()
    }
    pub fn blocked(&self) -> Vec<IpAddr> {
        self.core
            .as_ref()
            .map(|core| core.blocked_ips())
            .unwrap_or_default()
    }
    pub fn pending(&self) -> Vec<Pending> {
        self.core
            .as_ref()
            .map(|core| core.pending())
            .unwrap_or_default()
    }
    pub fn set_locked(&self, locked: bool) {
        if let Some(core) = &self.core {
            core.set_locked(locked);
        }
    }
    pub fn set_hosts(&self, store: &HostStore) {
        if let Some(adapter) = &self.device_adapter
            && adapter.update(store)
            && let Some(core) = &self.core
        {
            core.cancel_unauthorized_devices();
            core.hosts_changed();
        }
        if let Some(adapter) = &self.adapter
            && adapter.update(store)
            && let Some(core) = &self.core
        {
            core.cancel_unauthorized_devices();
            core.hosts_changed();
        }
    }

    pub fn attach(
        &mut self,
        directory: PathBuf,
        connector: crate::ssh::SshConnector,
        store: Entity<HostStore>,
        locked: bool,
        cx: &mut Context<Self>,
    ) {
        let adapter = Arc::new(adapter::HostExecutor::new(connector));
        adapter.update(store.read(cx));
        self.adapter = Some(adapter.clone());
        let device_adapter = Arc::new(device_adapter::DeviceCatalog::default());
        device_adapter.update(store.read(cx));
        self.device_adapter = Some(device_adapter.clone());
        let name = crate::app::computer_name().unwrap_or_else(|| "ShellLF".into());
        self.job(cx, move || {
            let core = Core::load(&directory, &name, adapter)?;
            core.set_device_provider(device_adapter)?;
            core.set_locked(locked);
            let mut error = None;
            let network = if core.enabled_on_start() {
                match start_network(core.clone()) {
                    Ok(network) => Some(network),
                    Err(failure) => {
                        error = Some(error_text(&failure));
                        None
                    }
                }
            } else {
                None
            };
            Ok(Completion::Loaded(core, network, error))
        });
    }

    fn job(
        &mut self,
        cx: &mut Context<Self>,
        operation: impl FnOnce() -> anyhow::Result<Completion> + Send + 'static,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.status = t!("lan.working").to_string();
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { operation() }).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(Completion::Loaded(core, network, error)) => {
                        this.core = Some(core);
                        this.network = network;
                        if let Some(error) = error {
                            this.status = error;
                            cx.notify();
                            return;
                        }
                    }
                    Ok(Completion::Discovered(devices)) => this.devices = devices,
                    Ok(Completion::Inspected(device)) => this.candidate = Some(device),
                    Ok(Completion::Waiting(device)) => this.waiting = Some(device),
                    Ok(Completion::Hosts(peer, response)) => {
                        this.peer = Some(peer);
                        this.remote = response.hosts;
                        this.can_execute = response.can_execute;
                        this.host = this.remote.first().map(|host| host.id.clone());
                    }
                    Ok(Completion::DeviceList(peer, devices)) => {
                        this.device_peer = Some(peer);
                        this.remote_devices = devices;
                    }
                    Ok(Completion::Done | Completion::Paired) => {}
                    Err(error) => {
                        this.status = error_text(&error);
                        cx.notify();
                        return;
                    }
                }
                this.status = if this.waiting.is_some() {
                    t!("lan.waiting").to_string()
                } else {
                    t!("lan.ready").to_string()
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn access_job(
        &self,
        cx: &mut Context<Self>,
        operation: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
    ) {
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { operation() }).await;
            let _ = this.update(cx, |this, cx| {
                this.status = match result {
                    Ok(()) => t!("lan.ready").to_string(),
                    Err(error) => error_text(&error),
                };
                cx.notify();
            });
        })
        .detach();
    }

    pub fn dispatch(&mut self, command: LanCommand, cx: &mut Context<Self>) {
        let firewall_operation = match command {
            LanCommand::CheckFirewall => Some(firewall::FirewallOperation::Check),
            LanCommand::ConfigureFirewall => Some(firewall::FirewallOperation::Configure),
            LanCommand::RemoveFirewall => Some(firewall::FirewallOperation::Remove),
            _ => None,
        };
        if let Some(operation) = firewall_operation {
            self.firewall_operation(operation, cx);
            return;
        }
        if let LanCommand::ManageDevicePermissions(peer) = &command {
            if self.grants().iter().any(|grant| grant.id == *peer) {
                self.permission_peer = Some(peer.clone());
                cx.notify();
            }
            return;
        }
        if let LanCommand::SelectHost(id) = &command {
            if self.remote.iter().any(|host| host.id == *id) {
                self.host = Some(id.clone());
                cx.notify();
            }
            return;
        }
        if command == LanCommand::Cancel {
            self.cancel.store(true, Ordering::Release);
            return;
        }
        let Some(core) = self.core.clone() else {
            self.status = t!("lan.unavailable").to_string();
            cx.notify();
            return;
        };
        match &command {
            LanCommand::SetDiscoverable(discoverable) => {
                let discoverable = *discoverable;
                self.job(cx, move || {
                    core.set_discoverable(discoverable)?;
                    Ok(Completion::Done)
                });
                return;
            }
            LanCommand::ApproveDevices(peer, token) => {
                let (peer, token) = (peer.clone(), *token);
                self.access_job(cx, move || {
                    core.approve(&peer, token, core.shared_hosts(), false)?;
                    core.set_device_permissions(&peer, core.shared_devices())
                });
                return;
            }
            LanCommand::EnableDeviceAccess(enabled) => {
                let enabled = *enabled;
                self.access_job(cx, move || core.set_device_access_enabled(enabled));
                return;
            }
            LanCommand::ShareDevice(device, shared) => {
                let device = device.clone();
                let shared = *shared;
                self.access_job(cx, move || core.set_device_shared(&device, shared));
                return;
            }
            LanCommand::GrantDevice {
                peer,
                device,
                allowed,
            } => {
                let (peer, device, allowed) = (peer.clone(), device.clone(), *allowed);
                self.access_job(cx, move || {
                    core.set_device_permission(&peer, &device, allowed)
                });
                return;
            }
            LanCommand::RevokeDevices(peer) => {
                let peer = peer.clone();
                self.access_job(cx, move || core.revoke_device_access(&peer));
                return;
            }
            LanCommand::DisconnectDevice(id) => {
                let id = *id;
                self.access_job(cx, move || core.disconnect_device(id));
                return;
            }
            LanCommand::Approve(id, token, execute) => {
                let id = id.clone();
                let token = *token;
                let execute = *execute;
                self.access_job(cx, move || {
                    core.approve(&id, token, core.shared_hosts(), execute)
                });
                return;
            }
            LanCommand::Reject(id, token, block) => {
                let id = id.clone();
                let token = *token;
                let block = *block;
                self.access_job(cx, move || core.reject(&id, token, block));
                return;
            }
            LanCommand::Revoke(id) => {
                let id = id.clone();
                self.access_job(cx, move || core.revoke(&id));
                return;
            }
            LanCommand::Unblock(address) => {
                let address = *address;
                self.access_job(cx, move || core.unblock(address));
                return;
            }
            LanCommand::SetExecutionPermission(id, execute) => {
                let id = id.clone();
                let execute = *execute;
                self.access_job(cx, move || core.set_execution_permission(&id, execute));
                return;
            }
            _ => {}
        }
        if matches!(command, LanCommand::Disable) {
            self.cancel.store(true, Ordering::Release);
            self.waiting = None;
            let network = self.network.take();
            self.job(cx, move || {
                drop(network);
                core.remember_enabled(false)?;
                Ok(Completion::Done)
            });
            return;
        }
        if matches!(&command, LanCommand::Execute(_)) {
            if let LanCommand::Execute(command) = command {
                self.execute(core, command, cx);
            }
            return;
        }
        match command {
            LanCommand::RefreshLocalDevices => {
                let Some(adapter) = self.device_adapter.clone() else {
                    return;
                };
                self.job(cx, move || {
                    let result = adapter.refresh_adb(&crate::adb::SystemAdbDetector);
                    core.cancel_unauthorized_devices();
                    core.hosts_changed();
                    result?;
                    Ok(Completion::Done)
                });
            }
            LanCommand::RefreshDevices(peer) => self.job(cx, move || {
                let response = core.request(
                    &peer,
                    RemoteRequest::Devices,
                    &AtomicBool::new(false),
                    &mut |_, _| Ok(()),
                )?;
                Ok(Completion::DeviceList(peer, response.devices))
            }),
            LanCommand::Enable => {
                if self.busy {
                    return;
                }
                if !core.has_password() {
                    self.status = t!("lan.password_required").to_string();
                    cx.notify();
                    return;
                }
                self.busy = true;
                self.task = Some(cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_spawn(async move {
                            let network = start_network(core.clone())?;
                            core.remember_enabled(true)?;
                            Ok::<_, anyhow::Error>(network)
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        this.busy = false;
                        match result {
                            Ok(network) => {
                                this.network = Some(network);
                                this.status = t!("lan.ready").to_string();
                            }
                            Err(error) => this.status = error_text(&error),
                        }
                        cx.notify();
                    });
                }));
                cx.notify();
            }
            LanCommand::SavePassword(password) => self.job(cx, move || {
                core.set_password(password.expose())?;
                Ok(Completion::Done)
            }),
            LanCommand::Discover => self.job(cx, move || {
                anyhow::ensure!(core.is_active(), "LAN cooperation unavailable");
                Ok(Completion::Discovered(
                    shellrs_lan::discover()?
                        .into_iter()
                        .filter(|device| {
                            device.id != core.id() && !core.is_blocked(device.endpoint.ip())
                        })
                        .collect(),
                ))
            }),
            LanCommand::Inspect(endpoint) => self.job(cx, move || {
                Ok(Completion::Inspected(
                    core.inspect(shellrs_lan::parse_endpoint(&endpoint)?)?,
                ))
            }),
            LanCommand::RequestPair => {
                let Some(device) = self.candidate.clone() else {
                    return;
                };
                self.job(cx, move || {
                    core.request_pair(&device)?;
                    Ok(Completion::Waiting(device))
                });
            }
            LanCommand::PasswordPair(password) => {
                let Some(device) = self.candidate.clone() else {
                    return;
                };
                self.job(cx, move || {
                    core.pair_password(device, password.expose())?;
                    Ok(Completion::Paired)
                });
            }
            LanCommand::ShareHost(host, shared) => {
                let mut hosts = core.shared_hosts();
                if shared {
                    hosts.insert(host);
                } else {
                    hosts.remove(&host);
                }
                let execute = core.allow_execute();
                self.job(cx, move || {
                    core.set_sharing(hosts, execute)?;
                    Ok(Completion::Done)
                });
            }
            LanCommand::AllowExecution(execute) => self.job(cx, move || {
                core.set_sharing(core.shared_hosts(), execute)?;
                Ok(Completion::Done)
            }),
            LanCommand::Approve(id, token, execute) => self.job(cx, move || {
                core.approve(&id, token, core.shared_hosts(), execute)?;
                Ok(Completion::Done)
            }),
            LanCommand::Reject(id, token, block) => self.job(cx, move || {
                core.reject(&id, token, block)?;
                Ok(Completion::Done)
            }),
            LanCommand::Revoke(id) => self.job(cx, move || {
                core.revoke(&id)?;
                Ok(Completion::Done)
            }),
            LanCommand::Forget(id) => {
                self.cancel.store(true, Ordering::Release);
                self.job(cx, move || {
                    core.forget(&id)?;
                    Ok(Completion::Done)
                });
            }
            LanCommand::Unblock(address) => self.job(cx, move || {
                core.unblock(address)?;
                Ok(Completion::Done)
            }),
            LanCommand::RefreshHosts(peer) => self.job(cx, move || {
                let response = core.request(
                    &peer,
                    RemoteRequest::Hosts,
                    &AtomicBool::new(false),
                    &mut |_, _| Ok(()),
                )?;
                Ok(Completion::Hosts(peer, response))
            }),
            LanCommand::SetExecutionPermission(..)
            | LanCommand::Disable
            | LanCommand::SetDiscoverable(_)
            | LanCommand::CheckFirewall
            | LanCommand::ConfigureFirewall
            | LanCommand::RemoveFirewall
            | LanCommand::SelectHost(_)
            | LanCommand::Execute(_)
            | LanCommand::Cancel => {}
            LanCommand::ShareDevice(..)
            | LanCommand::EnableDeviceAccess(..)
            | LanCommand::GrantDevice { .. }
            | LanCommand::RevokeDevices(_)
            | LanCommand::DisconnectDevice(_)
            | LanCommand::OpenDevice { .. }
            | LanCommand::OpenHost { .. }
            | LanCommand::ManageAdb { .. }
            | LanCommand::AdbFiles { .. }
            | LanCommand::ManageDevicePermissions(_)
            | LanCommand::ApproveDevices(..) => {}
        }
    }

    fn execute(&mut self, core: Arc<Core>, command: String, cx: &mut Context<Self>) {
        if self.running || !self.can_execute || command.trim().is_empty() {
            return;
        }
        let (Some(peer), Some(host)) = (self.peer.clone(), self.host.clone()) else {
            return;
        };
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        self.output.clear();
        self.exit_code = None;
        self.running = true;
        let (sender, receiver) = mpsc::sync_channel(32);
        self.stream = Some(receiver);
        self.command_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    core.request(
                        &peer,
                        RemoteRequest::Execute { host, command },
                        &cancel,
                        &mut |stderr, bytes| {
                            sender
                                .send((stderr, bytes.to_vec()))
                                .map_err(|_| std::io::ErrorKind::BrokenPipe.into())
                        },
                    )
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.drain();
                this.running = false;
                match result {
                    Ok(response) => {
                        this.exit_code = response.exit_code;
                        this.status = t!("lan.finished", code = response.exit_code.unwrap_or(255))
                            .to_string();
                    }
                    Err(error) => this.status = error_text(&error),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn drain(&mut self) -> bool {
        let mut changed = false;
        if let Some(receiver) = &self.stream {
            for (_, bytes) in receiver.try_iter().take(128) {
                let left = shellrs_lan::MAX_OUTPUT.saturating_sub(self.output.len());
                self.output
                    .extend_from_slice(&bytes[..left.min(bytes.len())]);
                changed = true;
            }
        }
        changed
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        self.ticks += 1;
        let mut changed = self.drain();
        if let Some(core) = &self.core {
            if let Some(error) = core.take_device_audit_error() {
                tracing::error!(%error, "Cannot persist LAN device audit log");
                self.status = t!("lan.devices.audit_failed", reason = error).to_string();
                changed = true;
            }
            if self.revision != core.revision() {
                self.revision = core.revision();
                changed = true;
            }
            let pending: BTreeSet<_> = core
                .pending()
                .iter()
                .map(|pending| (pending.id.clone(), pending.token))
                .collect();
            self.prompt_seen.retain(|id| pending.contains(id));
        }
        if self.ticks.is_multiple_of(8)
            && self.pair_task.is_none()
            && let (Some(device), Some(core)) = (self.waiting.clone(), self.core.clone())
        {
            self.pair_task = Some(cx.spawn(async move |this, cx| {
                let result = cx
                    .background_spawn(async move { core.complete_pair(device) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.pair_task = None;
                    match result {
                        Ok(()) => {
                            this.waiting = None;
                            this.status = t!("lan.paired").to_string();
                        }
                        Err(error) if error.to_string().contains("pairing_pending") => {}
                        Err(error) => {
                            this.waiting = None;
                            this.status = error_text(&error);
                        }
                    }
                    cx.notify();
                });
            }));
        }
        cx.emit(LanEvent::Tick);
        if changed {
            cx.notify();
        }
    }

    pub(crate) fn device_target(
        &self,
        peer: &str,
        id: &str,
    ) -> Option<(Arc<Core>, SharedDevice, String)> {
        if self.device_peer.as_deref() != Some(peer) {
            return None;
        }
        let device = self
            .remote_devices
            .iter()
            .find(|device| device.id == id)?
            .clone();
        let core = self.core.clone()?;
        let owner = core
            .peers()
            .into_iter()
            .find(|owner| owner.id == peer)?
            .name;
        Some((core, device, owner))
    }

    pub(crate) fn host_target(
        &self,
        peer: &str,
        host: &str,
    ) -> Option<(Arc<Core>, SharedHost, String)> {
        if self.peer.as_deref() != Some(peer) || !self.can_execute {
            return None;
        }
        let host = self.remote.iter().find(|target| target.id == host)?.clone();
        let core = self.core.clone()?;
        let owner = core
            .peers()
            .into_iter()
            .find(|target| target.id == peer)?
            .name;
        Some((core, host, owner))
    }
}

impl Drop for LanService {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

fn start_network(core: Arc<Core>) -> anyhow::Result<Network> {
    let address = test_bind_address(
        std::env::var_os("SHELLRS_LAN_TEST_ADDRESS").as_deref(),
        std::env::var_os("SHELLRS_DATA_DIR").as_deref(),
    )?;
    Network::start(
        core,
        address,
        shellrs_lan::TCP_PORT,
        shellrs_lan::DISCOVERY_PORT,
    )
}

fn test_bind_address(
    address: Option<&std::ffi::OsStr>,
    directory: Option<&std::ffi::OsStr>,
) -> anyhow::Result<IpAddr> {
    let Some(address) = address else {
        return Ok(Ipv4Addr::UNSPECIFIED.into());
    };
    anyhow::ensure!(
        directory.is_some_and(|directory| !directory.is_empty()),
        "SHELLRS_LAN_TEST_ADDRESS requires an isolated SHELLRS_DATA_DIR"
    );
    let address = address
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("invalid loopback test address"))?
        .parse::<IpAddr>()?;
    anyhow::ensure!(address.is_loopback(), "test address must be loopback");
    Ok(address)
}

pub(super) fn error_text(error: &anyhow::Error) -> String {
    tracing::warn!(error = %format!("{error:#}"), "LAN operation failed");
    let text = error.to_string();
    if text.contains("shared_host_terminal_unavailable") {
        t!("lan.ssh_terminal_unavailable").to_string()
    } else if text.contains("shared_host_shell_refused") {
        t!("lan.ssh_shell_refused").to_string()
    } else if text.contains("shared_host_connection_lost") {
        t!("lan.ssh_connection_lost").to_string()
    } else if text.contains("blocked_by_device") {
        t!("lan.blocked_by_device").to_string()
    } else if text.contains("invalid_network_interface") {
        t!("lan.invalid_interface").to_string()
    } else if text.contains("invalid_device_address")
        || text.contains("use a local network address")
        || text.contains("invalid IP address syntax")
    {
        t!("lan.invalid_address").to_string()
    } else if text.contains("device_control_not_authorized")
        || text.contains("device_control_revoked")
    {
        t!("lan.devices.access_denied").to_string()
    } else if text.contains("device_busy") {
        t!("lan.devices.busy").to_string()
    } else if text.contains("shared_serial_open_failed") {
        t!("lan.devices.serial_open_failed").to_string()
    } else if text.contains("shared_serial_disconnected")
        || text.contains("shared_adb_disconnected")
        || text.contains("device_connection_lost")
    {
        t!("lan.devices.disconnected").to_string()
    } else if text.contains("shared_serial_write_failed")
        || text.contains("device_input_backpressure")
    {
        t!("lan.devices.backpressure").to_string()
    } else if text.contains("device_stream_cancelled") {
        t!("lan.devices.cancelled").to_string()
    } else if text.contains("device_open_timed_out") {
        t!("lan.devices.open_timed_out").to_string()
    } else if text.contains("device_unavailable") {
        t!("lan.devices.unavailable").to_string()
    } else if text.contains("pairing_rejected") {
        t!("lan.rejected").to_string()
    } else if text.contains("pairing_password_required") {
        t!("lan.password_required").to_string()
    } else if text.contains("pairing_password_refused") {
        t!("lan.password_refused").to_string()
    } else if text.contains("pairing_expired") {
        t!("lan.pairing_expired").to_string()
    } else if text.contains("execution_not_authorized") || text.contains("device_not_authorized") {
        t!("lan.access_refused").to_string()
    } else if text.contains("command_cancelled_or_timed_out") {
        t!("lan.cancelled").to_string()
    } else if text.contains("output_limit_reached") {
        t!("lan.output_limit").to_string()
    } else if text.contains("shared_host_key_unknown") {
        t!("lan.host_key_unknown").to_string()
    } else if text.contains("shared_host_key_changed") {
        t!("lan.host_key_changed").to_string()
    } else if text.contains("shared_host_credentials_missing") {
        t!("lan.host_credentials_missing").to_string()
    } else if text.contains("shared_host_connect_failed") {
        t!("lan.host_connect_failed").to_string()
    } else {
        t!("lan.failed", reason = text).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn description_does_not_advertise_network_protocol_versions() {
        crate::i18n::isolate_thread();
        for locale in ["zh-CN", "en"] {
            crate::i18n::set_locale(locale);
            let description = t!("lan.description");
            assert!(!description.contains("IPv4") && !description.contains("IPv6"));
        }
    }

    #[test]
    fn test_binding_is_loopback_only_and_requires_a_separate_data_directory() {
        assert_eq!(
            test_bind_address(None, None).unwrap(),
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        );
        assert!(test_bind_address(Some(OsStr::new("127.0.0.2")), None).is_err());
        for address in [
            "0.0.0.0",
            "192.168.1.2",
            "8.8.8.8",
            "::",
            "fe80::1",
            "invalid",
        ] {
            assert!(
                test_bind_address(Some(OsStr::new(address)), Some(OsStr::new("test-data")))
                    .is_err()
            );
        }
        assert_eq!(
            test_bind_address(Some(OsStr::new("127.0.0.2")), Some(OsStr::new("test-data")))
                .unwrap(),
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2))
        );
        assert_eq!(
            test_bind_address(Some(OsStr::new("::1")), Some(OsStr::new("test-data"))).unwrap(),
            IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
        );
    }
}
