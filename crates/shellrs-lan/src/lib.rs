mod address;
mod audit;
#[cfg(test)]
mod device_tests;
mod device_transport;
mod devices;
mod discovery;
mod interface;
mod password;
mod storage;
#[cfg(test)]
mod tests;
mod transport;

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use russh::keys::{Algorithm, HashAlg, PrivateKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use address::is_lan;
pub use address::parse_endpoint;
pub use audit::{AuditAction, AuditEntry};
pub use devices::{
    ActiveDevice, DEVICE_CHUNK, DeviceEvent, DeviceInput, DeviceKind, DeviceProvider, DeviceSize,
    SerialParameters, SharedDevice, validate_adb_service,
};
pub use discovery::discover;
pub use transport::{Network, RemoteRequest, Response};

pub const TCP_PORT: u16 = 45838;
pub const DISCOVERY_PORT: u16 = 45837;
pub const MIN_PAIRING_PASSWORD_CHARS: usize = 6;
pub const MAX_COMMAND: usize = 16 * 1024;
pub const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_PEERS: usize = 64;
const PAIR_LIFETIME: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientRequest {
    Peers,
    Hosts {
        peer: String,
    },
    Execute {
        peer: String,
        host: String,
        command: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SharedHost {
    pub id: String,
    pub name: String,
    pub address: String,
    pub port: u16,
    pub user: String,
}

impl SharedHost {
    pub fn new(id: String, name: String, address: String, port: u16, user: String) -> Self {
        Self {
            id,
            name,
            address,
            port,
            user,
        }
    }
}

pub trait Executor: Send + Sync + 'static {
    fn hosts(&self) -> Vec<SharedHost>;
    fn has_host(&self, host: &str) -> bool {
        self.hosts().iter().any(|item| item.id == host)
    }
    fn open_terminal(
        &self,
        _host: &str,
        _size: DeviceSize,
        _commands: tokio::sync::mpsc::Receiver<DeviceInput>,
        _events: tokio::sync::mpsc::Sender<DeviceEvent>,
        _cancelled: &AtomicBool,
    ) -> Result<()> {
        anyhow::bail!("shared_host_terminal_unavailable")
    }
    fn execute(
        &self,
        host: &str,
        command: &str,
        cancelled: &AtomicBool,
        output: &mut dyn FnMut(bool, &[u8]) -> io::Result<()>,
    ) -> Result<i32>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Device {
    pub id: String,
    pub name: String,
    pub endpoint: SocketAddr,
    pub public_key: String,
}

impl Device {
    pub fn pairing_code(&self, local_id: &str) -> String {
        pairing_code(local_id, &self.id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Grant {
    pub id: String,
    pub name: String,
    pub public_key: String,
    pub hosts: BTreeSet<String>,
    pub execute: bool,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Pending {
    pub id: String,
    pub name: String,
    pub address: SocketAddr,
    pub public_key: String,
    pub code: String,
    pub token: u64,
    created: Instant,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Configuration {
    enabled: bool,
    discoverable: bool,
    password: Option<password::Verifier>,
    shared_hosts: BTreeSet<String>,
    allow_execute: bool,
    blocked_ips: BTreeSet<IpAddr>,
    grants: BTreeMap<String, Grant>,
    peers: BTreeMap<String, Device>,
    device_access_enabled: bool,
    shared_devices: BTreeSet<String>,
    device_grants: BTreeMap<String, BTreeSet<String>>,
}

struct State {
    configuration: Configuration,
    pending: BTreeMap<String, Pending>,
    rejected: BTreeMap<String, Instant>,
    failed_passwords: BTreeMap<IpAddr, (u8, Instant)>,
}

pub struct Core {
    directory: PathBuf,
    name: String,
    key: Arc<PrivateKey>,
    id: String,
    state: Mutex<State>,
    revision: AtomicU64,
    running: AtomicBool,
    locked: AtomicBool,
    executor: Arc<dyn Executor>,
    command_slots: Arc<tokio::sync::Semaphore>,
    password_slots: Arc<tokio::sync::Semaphore>,
    device_provider: RwLock<Arc<dyn DeviceProvider>>,
    device_audit: Mutex<audit::AuditLog>,
    device_leases: Mutex<BTreeMap<u64, devices::DeviceLease>>,
    next_device_stream: AtomicU64,
    device_generation: AtomicU64,
    device_audit_error: Mutex<Option<String>>,
}

impl Core {
    pub fn load(directory: &Path, name: &str, executor: Arc<dyn Executor>) -> Result<Arc<Self>> {
        std::fs::create_dir_all(directory)?;
        let metadata = std::fs::symlink_metadata(directory)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "invalid device storage directory"
        );
        storage::secure_directory(directory)?;
        let device_audit = audit::AuditLog::load(directory)?;
        let key_path = directory.join("identity");
        let key = match storage::read_file(&key_path, 16 * 1024) {
            Ok(bytes) => {
                ensure!(bytes.len() <= 16 * 1024, "invalid device identity");
                PrivateKey::from_openssh(bytes)?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
                let encoded = key.to_openssh(russh::keys::ssh_key::LineEnding::LF)?;
                let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
                std::io::Write::write_all(&mut temporary, encoded.as_bytes())?;
                temporary.as_file().sync_all()?;
                temporary.persist_noclobber(&key_path)?;
                key
            }
            Err(error) => return Err(error.into()),
        };
        ensure!(!key.is_encrypted(), "device identity must be usable");
        let mut configuration = match storage::read_file(&directory.join("access.json"), 256 * 1024)
        {
            Ok(bytes) => {
                ensure!(bytes.len() <= 256 * 1024, "access file too large");
                serde_json::from_slice::<Configuration>(&bytes)?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Configuration::default(),
            Err(error) => return Err(error.into()),
        };
        ensure!(
            configuration.grants.len() <= MAX_PEERS && configuration.peers.len() <= MAX_PEERS,
            "too many devices"
        );
        configuration.blocked_ips = configuration
            .blocked_ips
            .into_iter()
            .map(address::normalize_ip)
            .collect();
        for (id, peer) in &mut configuration.peers {
            peer.endpoint = address::normalize_endpoint(peer.endpoint);
            ensure!(
                *id == fingerprint(&russh::keys::PublicKey::from_openssh(&peer.public_key)?),
                "invalid peer identity"
            );
            ensure!(
                address::valid_stored_endpoint(peer.endpoint),
                "invalid peer address"
            );
        }
        for (id, grant) in &configuration.grants {
            ensure!(
                *id == fingerprint(&russh::keys::PublicKey::from_openssh(&grant.public_key)?),
                "invalid grant identity"
            );
        }
        let id = fingerprint(key.public_key());
        Ok(Arc::new(Self {
            directory: directory.to_owned(),
            name: clean_name(name),
            key: Arc::new(key),
            id,
            state: Mutex::new(State {
                configuration,
                pending: BTreeMap::new(),
                rejected: BTreeMap::new(),
                failed_passwords: BTreeMap::new(),
            }),
            revision: AtomicU64::new(0),
            running: AtomicBool::new(false),
            locked: AtomicBool::new(false),
            executor,
            command_slots: Arc::new(tokio::sync::Semaphore::new(4)),
            password_slots: Arc::new(tokio::sync::Semaphore::new(2)),
            device_provider: RwLock::new(Arc::new(devices::NoDevices)),
            device_audit: Mutex::new(device_audit),
            device_leases: Mutex::new(BTreeMap::new()),
            next_device_stream: AtomicU64::new(1),
            device_generation: AtomicU64::new(0),
            device_audit_error: Mutex::new(None),
        }))
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }
    pub fn hosts_changed(&self) {
        self.revision.fetch_add(1, Ordering::Release);
    }
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }
    pub fn enabled_on_start(&self) -> bool {
        self.state.lock().unwrap().configuration.enabled
    }
    pub fn has_password(&self) -> bool {
        self.state.lock().unwrap().configuration.password.is_some()
    }
    pub fn is_discoverable(&self) -> bool {
        self.state.lock().unwrap().configuration.discoverable
    }
    pub fn set_discoverable(&self, discoverable: bool) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let mut configuration = state.configuration.clone();
        configuration.discoverable = discoverable;
        self.persist(&configuration)?;
        state.configuration = configuration;
        Ok(())
    }
    pub fn shared_hosts(&self) -> BTreeSet<String> {
        self.state
            .lock()
            .unwrap()
            .configuration
            .shared_hosts
            .clone()
    }
    pub fn allow_execute(&self) -> bool {
        self.state.lock().unwrap().configuration.allow_execute
    }
    pub fn blocked_ips(&self) -> Vec<IpAddr> {
        self.state
            .lock()
            .unwrap()
            .configuration
            .blocked_ips
            .iter()
            .copied()
            .collect()
    }
    pub fn is_blocked(&self, address: IpAddr) -> bool {
        let address = address::normalize_ip(address);
        self.state
            .lock()
            .unwrap()
            .configuration
            .blocked_ips
            .contains(&address)
    }

    pub fn set_password(&self, password: &str) -> Result<()> {
        let verifier = password::Verifier::new(password)?;
        let mut state = self.state.lock().unwrap();
        let mut configuration = state.configuration.clone();
        configuration.password = Some(verifier);
        configuration.grants.clear();
        configuration.device_grants.clear();
        self.persist(&configuration)?;
        state.configuration = configuration;
        state.pending.clear();
        state.failed_passwords.clear();
        self.invalidate_device_access();
        Ok(())
    }

    pub fn set_sharing(&self, hosts: BTreeSet<String>, execute: bool) -> Result<()> {
        ensure!(hosts.len() <= 256, "too many shared hosts");
        let mut state = self.state.lock().unwrap();
        let mut configuration = state.configuration.clone();
        configuration.shared_hosts = hosts.clone();
        configuration.allow_execute = execute;
        for grant in configuration.grants.values_mut() {
            grant.hosts = hosts.clone();
        }
        self.persist(&configuration)?;
        state.configuration = configuration;
        self.invalidate_disallowed_devices(&state.configuration);
        Ok(())
    }

    pub fn unblock(&self, address: IpAddr) -> Result<()> {
        let address = address::normalize_ip(address);
        let mut state = self.state.lock().unwrap();
        let mut configuration = state.configuration.clone();
        configuration.blocked_ips.remove(&address);
        self.persist(&configuration)?;
        state.configuration = configuration;
        state.failed_passwords.remove(&address);
        Ok(())
    }
    pub fn set_locked(&self, locked: bool) {
        self.locked.store(locked, Ordering::Release);
        if locked {
            self.invalidate_device_access();
        }
    }
    pub fn is_active(&self) -> bool {
        self.is_running() && !self.locked.load(Ordering::Acquire)
    }
    pub fn hosts(&self) -> Vec<SharedHost> {
        self.executor.hosts()
    }
    pub fn peers(&self) -> Vec<Device> {
        self.state
            .lock()
            .unwrap()
            .configuration
            .peers
            .values()
            .cloned()
            .collect()
    }
    pub fn grants(&self) -> Vec<Grant> {
        self.state
            .lock()
            .unwrap()
            .configuration
            .grants
            .values()
            .cloned()
            .collect()
    }
    pub fn pending(&self) -> Vec<Pending> {
        let mut state = self.state.lock().unwrap();
        state
            .pending
            .retain(|_, pending| pending.created.elapsed() < PAIR_LIFETIME);
        state.pending.values().cloned().collect()
    }

    fn persist(&self, configuration: &Configuration) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(configuration)?;
        ensure!(bytes.len() <= 256 * 1024, "access file too large");
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        std::io::Write::write_all(&mut file, &bytes)?;
        file.as_file().sync_all()?;
        file.persist(self.directory.join("access.json"))?;
        self.revision.fetch_add(1, Ordering::Release);
        Ok(())
    }

    pub fn remember_enabled(&self, enabled: bool) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        ensure!(
            !enabled || state.configuration.password.is_some(),
            "pairing_password_required"
        );
        let mut configuration = state.configuration.clone();
        configuration.enabled = enabled;
        self.persist(&configuration)?;
        state.configuration = configuration;
        Ok(())
    }

    pub fn approve(
        &self,
        id: &str,
        token: u64,
        hosts: BTreeSet<String>,
        execute: bool,
    ) -> Result<()> {
        ensure!(self.is_active(), "LAN cooperation is unavailable");
        ensure!(hosts.len() <= 256, "too many shared hosts");
        let available: BTreeSet<_> = self
            .executor
            .hosts()
            .into_iter()
            .map(|host| host.id)
            .collect();
        ensure!(hosts.is_subset(&available), "host is no longer available");
        let mut state = self.state.lock().unwrap();
        let pending = state
            .pending
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("pairing expired"))?;
        ensure!(pending.created.elapsed() < PAIR_LIFETIME, "pairing expired");
        ensure!(pending.token == token, "pairing expired");
        ensure!(
            !state
                .configuration
                .blocked_ips
                .contains(&pending.address.ip()),
            "blocked_by_device"
        );
        ensure!(
            state.configuration.grants.len() < MAX_PEERS,
            "too many devices"
        );
        let grant = Grant {
            id: id.to_owned(),
            name: pending.name.clone(),
            public_key: pending.public_key.clone(),
            hosts,
            execute,
        };
        let mut configuration = state.configuration.clone();
        configuration.grants.insert(id.to_owned(), grant);
        self.persist(&configuration)?;
        state.configuration = configuration;
        state.pending.remove(id);
        Ok(())
    }

    pub fn reject(&self, id: &str, token: u64, block: bool) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let pending = state
            .pending
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("pairing expired"))?;
        let address = pending.address.ip();
        ensure!(pending.token == token, "pairing expired");
        if block {
            let mut configuration = state.configuration.clone();
            ensure!(configuration.blocked_ips.len() < 256, "blacklist is full");
            configuration.blocked_ips.insert(address);
            self.persist(&configuration)?;
            state.configuration = configuration;
            self.invalidate_disallowed_devices(&state.configuration);
            state
                .pending
                .retain(|_, pending| pending.address.ip() != address);
        } else {
            state.pending.remove(id);
        }
        if state.rejected.len() >= 64 {
            state.rejected.clear();
        }
        state.rejected.insert(id.to_owned(), Instant::now());
        self.revision.fetch_add(1, Ordering::Release);
        Ok(())
    }
    pub fn revoke(&self, id: &str) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let mut configuration = state.configuration.clone();
        configuration.grants.remove(id);
        configuration.device_grants.remove(id);
        self.persist(&configuration)?;
        state.configuration = configuration;
        self.invalidate_disallowed_devices(&state.configuration);
        Ok(())
    }
    pub fn set_execution_permission(&self, id: &str, execute: bool) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let mut configuration = state.configuration.clone();
        let grant = configuration
            .grants
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("device_not_authorized"))?;
        grant.execute = execute;
        self.persist(&configuration)?;
        state.configuration = configuration;
        self.invalidate_disallowed_devices(&state.configuration);
        Ok(())
    }

    pub fn forget(&self, id: &str) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let mut configuration = state.configuration.clone();
        configuration.peers.remove(id);
        self.persist(&configuration)?;
        state.configuration = configuration;
        Ok(())
    }

    pub fn inspect(&self, endpoint: SocketAddr) -> Result<Device> {
        transport::inspect(self, endpoint)
    }
    pub fn request_pair(&self, device: &Device) -> Result<()> {
        transport::request_pair(self, device)?;
        let response = self.request_device(
            device,
            RemoteRequest::Status,
            &AtomicBool::new(false),
            &mut |_, _| Ok(()),
        )?;
        ensure!(
            matches!(response.status.as_str(), "accepted" | "pairing_pending"),
            "{}",
            response.status
        );
        Ok(())
    }
    pub fn pair_password(&self, device: Device, password: &str) -> Result<()> {
        transport::pair_password(self, &device, password)?;
        self.complete_pair(device)
    }
    pub fn complete_pair(&self, device: Device) -> Result<()> {
        let status = self.request_device(
            &device,
            RemoteRequest::Status,
            &AtomicBool::new(false),
            &mut |_, _| Ok(()),
        )?;
        ensure!(status.status == "accepted", "{}", status.status);
        let response = self.request_device(
            &device,
            RemoteRequest::Hosts,
            &AtomicBool::new(false),
            &mut |_, _| Ok(()),
        )?;
        let mut device = device;
        device.name = response.name.clone();
        let mut state = self.state.lock().unwrap();
        ensure!(
            state.configuration.peers.len() < MAX_PEERS,
            "too many devices"
        );
        let mut configuration = state.configuration.clone();
        configuration.peers.insert(device.id.clone(), device);
        self.persist(&configuration)?;
        state.configuration = configuration;
        Ok(())
    }
    pub fn request(
        &self,
        peer: &str,
        request: RemoteRequest,
        cancel: &AtomicBool,
        output: &mut dyn FnMut(bool, &[u8]) -> io::Result<()>,
    ) -> Result<Response> {
        let device = self
            .state
            .lock()
            .unwrap()
            .configuration
            .peers
            .get(peer)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("device is not paired"))?;
        self.request_device(&device, request, cancel, output)
    }
    fn request_device(
        &self,
        peer: &Device,
        request: RemoteRequest,
        cancel: &AtomicBool,
        output: &mut dyn FnMut(bool, &[u8]) -> io::Result<()>,
    ) -> Result<Response> {
        ensure!(self.is_active(), "LAN cooperation is unavailable or locked");
        transport::request(self, peer, request, cancel, output)
    }

    fn authorized(&self, id: &str, host: Option<&str>, execute: bool) -> bool {
        if !self.is_active() {
            return false;
        }
        let state = self.state.lock().unwrap();
        state.configuration.grants.get(id).is_some_and(|grant| {
            (!execute || (grant.execute && state.configuration.allow_execute))
                && host.is_none_or(|host| {
                    grant.hosts.contains(host) && state.configuration.shared_hosts.contains(host)
                })
        })
    }

    fn authenticate(
        &self,
        name: &str,
        key: &russh::keys::PublicKey,
        address: SocketAddr,
        request: bool,
    ) -> bool {
        if !self.is_active() || self.is_blocked(address.ip()) {
            return false;
        }
        let id = fingerprint(key);
        if self.authorized(&id, None, false) {
            return true;
        }
        if !request {
            return false;
        }
        let mut state = self.state.lock().unwrap();
        if !self.is_active() || state.configuration.blocked_ips.contains(&address.ip()) {
            return false;
        }
        state
            .pending
            .retain(|_, pending| pending.created.elapsed() < PAIR_LIFETIME);
        if state.pending.len() >= 16 || state.pending.contains_key(&id) {
            return false;
        }
        let Ok(public_key) = key.to_openssh() else {
            return false;
        };
        state.rejected.remove(&id);
        state.pending.insert(
            id.clone(),
            Pending {
                name: clean_name(name),
                address,
                public_key,
                code: pairing_code(&self.id, &id),
                id,
                created: Instant::now(),
                token: rand::random(),
            },
        );
        self.revision.fetch_add(1, Ordering::Release);
        false
    }

    fn rejection(&self, id: &str, address: IpAddr) -> Option<&'static str> {
        let state = self.state.lock().unwrap();
        if state.configuration.blocked_ips.contains(&address) {
            Some("blocked_by_device")
        } else if state
            .rejected
            .get(id)
            .is_some_and(|at| at.elapsed() < PAIR_LIFETIME)
        {
            Some("pairing_rejected")
        } else {
            None
        }
    }

    fn pair_status(&self, id: &str, address: IpAddr) -> String {
        if self.is_blocked(address) {
            return "blocked_by_device".into();
        }
        if self.authorized(id, None, false) {
            return "accepted".into();
        }
        if let Some(reason) = self.rejection(id, address) {
            return reason.into();
        }
        if self
            .state
            .lock()
            .unwrap()
            .pending
            .get(id)
            .is_some_and(|pending| pending.created.elapsed() < PAIR_LIFETIME)
        {
            "pairing_pending".into()
        } else {
            "pairing_expired".into()
        }
    }

    fn authenticate_password(
        &self,
        name: &str,
        key: &russh::keys::PublicKey,
        address: IpAddr,
        password: &str,
    ) -> Result<bool> {
        ensure!(
            self.is_active() && !self.is_blocked(address),
            "blocked_by_device"
        );
        let verifier = {
            let mut state = self.state.lock().unwrap();
            state
                .failed_passwords
                .retain(|_, (_, at)| at.elapsed() < Duration::from_secs(300));
            ensure!(state.failed_passwords.len() < 256, "pairing_rate_limited");
            ensure!(
                !state
                    .failed_passwords
                    .get(&address)
                    .is_some_and(|(count, _)| *count >= 5),
                "pairing_rate_limited"
            );
            state
                .configuration
                .password
                .clone()
                .ok_or_else(|| anyhow::anyhow!("pairing_password_required"))?
        };
        if !verifier.verify(password)? {
            let mut state = self.state.lock().unwrap();
            let failed = state
                .failed_passwords
                .entry(address)
                .or_insert((0, Instant::now()));
            failed.0 = failed.0.saturating_add(1);
            return Ok(false);
        }
        let mut state = self.state.lock().unwrap();
        ensure!(
            !state.configuration.blocked_ips.contains(&address) && self.is_active(),
            "blocked_by_device"
        );
        ensure!(
            state.configuration.password.as_ref() == Some(&verifier),
            "pairing_password_changed"
        );
        let id = fingerprint(key);
        ensure!(
            state.configuration.grants.len() < MAX_PEERS,
            "too many devices"
        );
        let mut configuration = state.configuration.clone();
        configuration.grants.insert(
            id.clone(),
            Grant {
                id: id.clone(),
                name: clean_name(name),
                public_key: key.to_openssh()?,
                hosts: configuration.shared_hosts.clone(),
                execute: configuration.allow_execute,
            },
        );
        let devices = if configuration.device_access_enabled {
            configuration.shared_devices.clone()
        } else {
            BTreeSet::new()
        };
        if !devices.is_empty() {
            self.record_device_audit(AuditEntry::new(
                AuditAction::PermissionChanged,
                &id,
                "",
                "password_pairing_requested",
            ))?;
        }
        configuration.device_grants.insert(id.clone(), devices);
        self.persist(&configuration)?;
        state.configuration = configuration;
        self.invalidate_disallowed_devices(&state.configuration);
        state.failed_passwords.remove(&address);
        state.pending.remove(&id);
        state.rejected.remove(&id);
        Ok(true)
    }
}

fn fingerprint(key: &russh::keys::PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}
fn pairing_code(left: &str, right: &str) -> String {
    let (left, right) = if left < right {
        (left, right)
    } else {
        (right, left)
    };
    let digest = Sha256::digest(format!("shelllf-lan-v1\0{left}\0{right}").as_bytes());
    digest[..6]
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<String>()
}
fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|character| !character.is_control())
        .take(64)
        .collect()
}
