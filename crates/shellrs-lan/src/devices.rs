use std::collections::BTreeSet;
use std::net::IpAddr;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{AuditAction, AuditEntry, Core};

pub const DEVICE_CHUNK: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Serial,
    Adb,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SerialParameters {
    pub baud: u32,
    pub data_bits: u8,
    pub parity: String,
    pub stop_bits: u8,
    pub flow: String,
}

impl SerialParameters {
    pub fn new(baud: u32, data_bits: u8, parity: String, stop_bits: u8, flow: String) -> Self {
        Self {
            baud,
            data_bits,
            parity,
            stop_bits,
            flow,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SharedDevice {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    pub address: String,
    pub detail: String,
    pub encoding: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<SerialParameters>,
}

impl SharedDevice {
    pub fn new(
        id: String,
        name: String,
        kind: DeviceKind,
        address: String,
        detail: String,
        encoding: String,
    ) -> Self {
        Self {
            id,
            name,
            kind,
            address,
            detail,
            encoding,
            serial: None,
        }
    }

    pub fn with_serial(mut self, serial: SerialParameters) -> Self {
        self.serial = Some(serial);
        self
    }

    pub(super) fn validate(&self) -> Result<()> {
        ensure!(
            !self.id.is_empty()
                && self.id.len() <= 128
                && self.name.len() <= 1024
                && self.address.len() <= 1024
                && self.detail.len() <= 512
                && ["utf-8", "gbk", "gb18030"].contains(&self.encoding.as_str()),
            "invalid_device_catalog"
        );
        ensure!(
            !self
                .id
                .chars()
                .chain(self.name.chars())
                .chain(self.address.chars())
                .any(char::is_control),
            "invalid_device_catalog"
        );
        if let Some(serial) = &self.serial {
            ensure!(
                serial.baud > 0
                    && (5..=8).contains(&serial.data_bits)
                    && [1, 2].contains(&serial.stop_bits)
                    && ["none", "odd", "even"].contains(&serial.parity.as_str())
                    && ["none", "rts-cts", "xon-xoff"].contains(&serial.flow.as_str()),
                "invalid_device_catalog"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeviceSize {
    pub columns: u16,
    pub rows: u16,
}

impl DeviceSize {
    pub fn new(columns: u16, rows: u16) -> Result<Self> {
        ensure!(
            (1..=1000).contains(&columns) && (1..=1000).contains(&rows),
            "invalid_device_size"
        );
        Ok(Self { columns, rows })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceInput {
    Write(Vec<u8>),
    Resize(DeviceSize),
    Break,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceEvent {
    Started,
    Output(Vec<u8>),
    Closed,
}

pub trait DeviceProvider: Send + Sync + 'static {
    fn run_adb(
        &self,
        _device: &str,
        _service: &str,
        _commands: tokio::sync::mpsc::Receiver<DeviceInput>,
        _events: tokio::sync::mpsc::Sender<DeviceEvent>,
        _cancelled: &AtomicBool,
    ) -> Result<()> {
        bail!("adb_service_unavailable")
    }
    fn devices(&self) -> Vec<SharedDevice>;
    fn is_available(&self, device: &str) -> bool {
        self.devices().iter().any(|item| item.id == device)
    }
    fn resource(&self, device: &str) -> Option<String> {
        self.devices()
            .into_iter()
            .find(|item| item.id == device)
            .map(|item| item.id)
    }
    fn run(
        &self,
        device: &str,
        size: DeviceSize,
        commands: tokio::sync::mpsc::Receiver<DeviceInput>,
        events: tokio::sync::mpsc::Sender<DeviceEvent>,
        cancelled: &AtomicBool,
    ) -> Result<()>;
}

pub(super) struct NoDevices;
impl DeviceProvider for NoDevices {
    fn devices(&self) -> Vec<SharedDevice> {
        Vec::new()
    }
    fn run(
        &self,
        _: &str,
        _: DeviceSize,
        _: tokio::sync::mpsc::Receiver<DeviceInput>,
        _: tokio::sync::mpsc::Sender<DeviceEvent>,
        _: &AtomicBool,
    ) -> Result<()> {
        bail!("device_unavailable")
    }
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ActiveDevice {
    pub id: u64,
    pub peer: String,
    pub device: String,
    pub source: IpAddr,
}

pub(super) struct DeviceLease {
    pub snapshot: ActiveDevice,
    pub resource: String,
    pub cancelled: Arc<AtomicBool>,
}

impl Core {
    pub fn set_device_provider(&self, provider: Arc<dyn DeviceProvider>) -> Result<()> {
        ensure!(!self.is_running(), "device_provider_already_started");
        *self.device_provider.write().unwrap() = provider;
        self.hosts_changed();
        Ok(())
    }

    pub fn local_devices(&self) -> Vec<SharedDevice> {
        self.device_provider.read().unwrap().devices()
    }

    pub fn shared_devices(&self) -> BTreeSet<String> {
        self.state
            .lock()
            .unwrap()
            .configuration
            .shared_devices
            .clone()
    }

    pub fn device_access_enabled(&self) -> bool {
        self.state
            .lock()
            .unwrap()
            .configuration
            .device_access_enabled
    }

    pub fn device_permissions(&self, peer: &str) -> BTreeSet<String> {
        self.state
            .lock()
            .unwrap()
            .configuration
            .device_grants
            .get(peer)
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_device_sharing(&self, devices: BTreeSet<String>, enabled: bool) -> Result<()> {
        ensure!(
            devices.len() <= 256 && devices.iter().all(|id| id.len() <= 128),
            "too_many_shared_devices"
        );
        let mut state = self.state.lock().unwrap();
        let mut configuration = state.configuration.clone();
        configuration.shared_devices = devices.clone();
        configuration.device_access_enabled = enabled;
        for granted in configuration.device_grants.values_mut() {
            granted.retain(|device| devices.contains(device));
        }
        self.commit_device_configuration(
            &mut state,
            configuration,
            AuditEntry::new(AuditAction::SharingChanged, "local", "", "requested"),
        )?;
        drop(state);
        self.cancel_unauthorized_devices();
        Ok(())
    }

    pub fn set_device_shared(&self, device: &str, shared: bool) -> Result<()> {
        ensure!(device.len() <= 128, "invalid_device_request");
        if shared {
            ensure!(
                self.local_devices().iter().any(|item| item.id == device),
                "device_unavailable"
            );
        }
        let mut state = self.state.lock().unwrap();
        let mut next = state.configuration.clone();
        if shared {
            next.shared_devices.insert(device.to_owned());
        } else {
            next.shared_devices.remove(device);
            for devices in next.device_grants.values_mut() {
                devices.remove(device);
            }
        }
        ensure!(next.shared_devices.len() <= 256, "too_many_shared_devices");
        self.commit_device_configuration(
            &mut state,
            next,
            AuditEntry::new(AuditAction::SharingChanged, "local", device, "requested"),
        )
    }

    pub fn set_device_access_enabled(&self, enabled: bool) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let mut next = state.configuration.clone();
        next.device_access_enabled = enabled;
        self.commit_device_configuration(
            &mut state,
            next,
            AuditEntry::new(AuditAction::SharingChanged, "local", "", "requested"),
        )
    }

    pub fn set_device_permission(&self, peer: &str, device: &str, allowed: bool) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        ensure!(
            state.configuration.grants.contains_key(peer),
            "device_not_authorized"
        );
        ensure!(
            device.len() <= 128
                && (!allowed || state.configuration.shared_devices.contains(device)),
            "device_not_whitelisted"
        );
        let mut next = state.configuration.clone();
        let devices = next.device_grants.entry(peer.to_owned()).or_default();
        if allowed {
            devices.insert(device.to_owned());
        } else {
            devices.remove(device);
        }
        self.commit_device_configuration(
            &mut state,
            next,
            AuditEntry::new(AuditAction::PermissionChanged, peer, device, "requested"),
        )
    }

    pub fn set_device_permissions(&self, peer: &str, devices: BTreeSet<String>) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        ensure!(
            state.configuration.grants.contains_key(peer),
            "device_not_authorized"
        );
        ensure!(
            devices.len() <= 256 && devices.is_subset(&state.configuration.shared_devices),
            "device_not_whitelisted"
        );
        let mut configuration = state.configuration.clone();
        configuration
            .device_grants
            .insert(peer.to_owned(), devices.clone());
        self.commit_device_configuration(
            &mut state,
            configuration,
            AuditEntry::new(AuditAction::PermissionChanged, peer, "", "requested"),
        )?;
        drop(state);
        self.cancel_unauthorized_devices();
        Ok(())
    }

    pub fn revoke_device_access(&self, peer: &str) -> Result<()> {
        self.set_device_permissions(peer, BTreeSet::new())
    }

    fn commit_device_configuration(
        &self,
        state: &mut crate::State,
        next: crate::Configuration,
        entry: AuditEntry,
    ) -> Result<()> {
        state.configuration.device_access_enabled &= next.device_access_enabled;
        state
            .configuration
            .shared_devices
            .retain(|device| next.shared_devices.contains(device));
        for (peer, devices) in &mut state.configuration.device_grants {
            devices.retain(|device| {
                next.device_grants
                    .get(peer)
                    .is_some_and(|granted| granted.contains(device))
            });
        }
        self.invalidate_disallowed_devices(&state.configuration);
        self.record_device_audit(entry)?;
        self.persist(&next)?;
        state.configuration = next;
        self.invalidate_disallowed_devices(&state.configuration);
        Ok(())
    }

    pub fn audit(&self) -> Vec<AuditEntry> {
        self.device_audit.lock().unwrap().entries()
    }

    pub(super) fn record_device_audit(&self, mut entry: AuditEntry) -> Result<()> {
        if entry.device_name.is_empty() && !entry.device.is_empty() {
            entry.device_name = self
                .local_devices()
                .into_iter()
                .find(|device| device.id == entry.device)
                .map(|device| {
                    device
                        .name
                        .chars()
                        .filter(|character| !character.is_control())
                        .take(256)
                        .collect()
                })
                .unwrap_or_default();
        }
        if let Err(error) = self.device_audit.lock().unwrap().append(entry) {
            *self.device_audit_error.lock().unwrap() = Some(error.to_string());
            self.hosts_changed();
            return Err(error);
        }
        self.hosts_changed();
        Ok(())
    }

    pub fn take_device_audit_error(&self) -> Option<String> {
        self.device_audit_error.lock().unwrap().take()
    }

    pub fn active_devices(&self) -> Vec<ActiveDevice> {
        self.device_leases
            .lock()
            .unwrap()
            .values()
            .map(|lease| lease.snapshot.clone())
            .collect()
    }

    pub fn disconnect_device(&self, id: u64) -> Result<()> {
        let lease = self
            .device_leases
            .lock()
            .unwrap()
            .get(&id)
            .map(|lease| (lease.snapshot.clone(), lease.cancelled.clone()));
        if let Some((snapshot, cancelled)) = lease {
            cancelled.store(true, Ordering::Release);
            self.record_device_audit(AuditEntry::new(
                AuditAction::Revoked,
                "local",
                &snapshot.device,
                "disconnect",
            ))?;
        }
        Ok(())
    }

    pub(super) fn device_authorized(&self, peer: &str, device: &str, source: IpAddr) -> bool {
        if !self.is_active() {
            return false;
        }
        let state = self.state.lock().unwrap();
        let configuration = &state.configuration;
        configuration.device_access_enabled
            && !configuration.blocked_ips.contains(&source)
            && configuration.grants.contains_key(peer)
            && configuration.shared_devices.contains(device)
            && configuration
                .device_grants
                .get(peer)
                .is_some_and(|devices| devices.contains(device))
            && self.device_provider.read().unwrap().is_available(device)
    }

    pub fn cancel_unauthorized_devices(&self) {
        let snapshots: Vec<_> = self
            .device_leases
            .lock()
            .unwrap()
            .values()
            .map(|lease| (lease.snapshot.clone(), lease.cancelled.clone()))
            .collect();
        for (snapshot, cancelled) in snapshots {
            if !self.device_authorized(&snapshot.peer, &snapshot.device, snapshot.source) {
                cancelled.store(true, Ordering::Release);
            }
        }
    }

    pub(super) fn cancel_all_devices(&self) {
        for lease in self.device_leases.lock().unwrap().values() {
            lease.cancelled.store(true, Ordering::Release);
        }
    }

    pub fn invalidate_device_access(&self) {
        self.device_generation.fetch_add(1, Ordering::AcqRel);
        self.cancel_all_devices();
    }

    pub(super) fn invalidate_disallowed_devices(&self, configuration: &crate::Configuration) {
        self.device_generation.fetch_add(1, Ordering::AcqRel);
        for lease in self.device_leases.lock().unwrap().values() {
            let target = &lease.snapshot;
            if !configuration.device_access_enabled
                || configuration.blocked_ips.contains(&target.source)
                || !configuration.grants.contains_key(&target.peer)
                || !configuration.shared_devices.contains(&target.device)
                || !configuration
                    .device_grants
                    .get(&target.peer)
                    .is_some_and(|devices| devices.contains(&target.device))
            {
                lease.cancelled.store(true, Ordering::Release);
            }
        }
    }

    pub(super) fn acquire_device(
        &self,
        peer: &str,
        device: &str,
        source: IpAddr,
        cancelled: Arc<AtomicBool>,
    ) -> Result<u64> {
        let generation = self.device_generation.load(Ordering::Acquire);
        ensure!(
            self.device_authorized(peer, device, source),
            "device_control_not_authorized"
        );
        let resource = self
            .device_provider
            .read()
            .unwrap()
            .resource(device)
            .ok_or_else(|| anyhow::anyhow!("device_unavailable"))?;
        let mut entry = AuditEntry::new(AuditAction::Open, peer, device, "requested");
        entry.source = Some(source);
        self.record_device_audit(entry)?;
        let mut leases = self.device_leases.lock().unwrap();
        ensure!(
            generation == self.device_generation.load(Ordering::Acquire),
            "device_control_not_authorized"
        );
        ensure!(leases.len() < 16, "too_many_device_streams");
        ensure!(
            resource.starts_with("adb:")
                || !leases.values().any(|lease| lease.resource == resource),
            "device_busy"
        );
        let id = self.next_device_stream.fetch_add(1, Ordering::Relaxed);
        leases.insert(
            id,
            DeviceLease {
                snapshot: ActiveDevice {
                    id,
                    peer: peer.to_owned(),
                    device: device.to_owned(),
                    source,
                },
                resource,
                cancelled,
            },
        );
        Ok(id)
    }

    pub(super) fn release_device(&self, id: u64) {
        self.device_leases.lock().unwrap().remove(&id);
        self.hosts_changed();
    }

    pub fn stream_device(
        &self,
        peer: &str,
        device: &str,
        size: DeviceSize,
        commands: std::sync::mpsc::Receiver<DeviceInput>,
        cancelled: &AtomicBool,
        events: &mut dyn FnMut(DeviceEvent) -> std::io::Result<()>,
    ) -> Result<()> {
        let target = self
            .peers()
            .into_iter()
            .find(|target| target.id == peer)
            .ok_or_else(|| anyhow::anyhow!("device is not paired"))?;
        super::transport::stream_device(self, &target, device, size, commands, cancelled, events)
    }

    pub fn stream_adb(
        &self,
        peer: &str,
        device: &str,
        service: &str,
        commands: std::sync::mpsc::Receiver<DeviceInput>,
        cancelled: &AtomicBool,
        events: &mut dyn FnMut(DeviceEvent) -> std::io::Result<()>,
    ) -> Result<()> {
        validate_adb_service(service)?;
        let target = self
            .peers()
            .into_iter()
            .find(|target| target.id == peer)
            .ok_or_else(|| anyhow::anyhow!("device is not paired"))?;
        crate::device_transport::stream_adb(
            self, &target, device, service, commands, cancelled, events,
        )
    }

    pub fn has_peer(&self, peer: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .configuration
            .peers
            .contains_key(peer)
    }
}

pub fn validate_adb_service(service: &str) -> Result<()> {
    ensure!(
        service.len() <= 16384 && !service.contains('\0'),
        "invalid_adb_service"
    );
    ensure!(
        service == "sync:"
            || service.starts_with("shell:")
            || service.split_once(':').is_some_and(|(kind, _)| {
                kind.starts_with("shell,v2,")
                    && kind.split(',').skip(2).all(|option| {
                        matches!(option, "raw" | "pty")
                            || option.strip_prefix("TERM=").is_some_and(|term| {
                                !term.is_empty()
                                    && term.len() <= 128
                                    && term.bytes().all(|byte| {
                                        byte.is_ascii_alphanumeric() || b"-_+.".contains(&byte)
                                    })
                            })
                    })
            })
            || service.starts_with("exec:")
            || service
                .strip_prefix("localabstract:scrcpy_")
                .is_some_and(|id| {
                    id.len() == 8 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
                }),
        "invalid_adb_service"
    );
    Ok(())
}

pub(super) fn input_frame(input: &DeviceInput) -> Result<Vec<u8>> {
    let (kind, payload) = match input {
        DeviceInput::Write(bytes) => {
            ensure!(
                !bytes.is_empty() && bytes.len() <= DEVICE_CHUNK,
                "invalid_device_input"
            );
            (1, bytes.clone())
        }
        DeviceInput::Resize(size) => {
            DeviceSize::new(size.columns, size.rows)?;
            (
                2,
                [size.columns.to_be_bytes(), size.rows.to_be_bytes()].concat(),
            )
        }
        DeviceInput::Break => (3, Vec::new()),
    };
    let mut frame = vec![kind];
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend(payload);
    Ok(frame)
}

pub(super) fn take_input(buffer: &mut Vec<u8>) -> Result<Option<DeviceInput>> {
    if buffer.len() < 5 {
        return Ok(None);
    }
    let count = u32::from_be_bytes(buffer[1..5].try_into().unwrap()) as usize;
    ensure!(count <= DEVICE_CHUNK, "invalid_device_input");
    if buffer.len() < count + 5 {
        return Ok(None);
    }
    let input = match (buffer[0], count) {
        (1, 1..=DEVICE_CHUNK) => DeviceInput::Write(buffer[5..5 + count].to_vec()),
        (2, 4) => DeviceInput::Resize(DeviceSize::new(
            u16::from_be_bytes(buffer[5..7].try_into().unwrap()),
            u16::from_be_bytes(buffer[7..9].try_into().unwrap()),
        )?),
        (3, 0) => DeviceInput::Break,
        _ => bail!("invalid_device_input"),
    };
    buffer.drain(..count + 5);
    Ok(Some(input))
}
