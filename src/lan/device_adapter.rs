use std::collections::VecDeque;
use std::io::{self, Read as _, Write as _};
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use anyhow::{Result, bail, ensure};
use sha2::{Digest as _, Sha256};
use shellrs_lan::{
    DeviceEvent, DeviceInput, DeviceKind, DeviceProvider, DeviceSize, SerialParameters,
    SharedDevice,
};

use crate::adb::{AdbConnection, AdbDetection, AdbDetector};
use crate::host::{HostConnection, HostStore, SerialLine};

#[derive(Clone)]
enum Endpoint {
    Serial(SerialLine),
    Adb(AdbConnection),
}

#[derive(Default)]
pub(super) struct DeviceCatalog {
    serial: RwLock<Vec<(SharedDevice, Endpoint)>>,
    adb: RwLock<Vec<(SharedDevice, Endpoint)>>,
}

fn device_id(kind: &str, identity: &str) -> String {
    let hash = Sha256::digest(identity.as_bytes());
    format!(
        "{kind}:{}",
        hash.iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

impl DeviceCatalog {
    pub fn update(&self, store: &HostStore) -> bool {
        let serial: Vec<_> = store
            .hosts()
            .iter()
            .filter_map(|host| {
                let HostConnection::Serial(line) = store.connection_of(host) else {
                    return None;
                };
                let identity = format!(
                    "{}:{:?}:{}",
                    host.public_id,
                    line,
                    host.terminal_encoding.key()
                );
                Some((
                    SharedDevice::new(
                        device_id("serial", &identity),
                        host.name.to_string(),
                        DeviceKind::Serial,
                        line.device.to_string(),
                        line.settings.summary(),
                        host.terminal_encoding.key().into(),
                    )
                    .with_serial(
                        SerialParameters::new(
                            line.settings.baud,
                            line.settings.data_bits.value(),
                            line.settings.parity.as_str().into(),
                            line.settings.stop_bits.value(),
                            line.settings.flow.as_str().into(),
                        )
                        .with_signals(line.settings.dtr, line.settings.rts),
                    ),
                    Endpoint::Serial(line),
                ))
            })
            .take(256)
            .collect();
        let mut stored = self.serial.write().unwrap();
        let changed = !stored
            .iter()
            .map(|(device, _)| device)
            .eq(serial.iter().map(|(device, _)| device));
        *stored = serial;
        changed
    }

    pub fn refresh_adb(&self, detector: &dyn AdbDetector) -> Result<bool> {
        let detected = match detector.detect(None) {
            AdbDetection::Matched(connection) => vec![connection],
            AdbDetection::Choose(devices) => devices,
            AdbDetection::Unavailable(reason) => {
                let changed = !self.adb.read().unwrap().is_empty();
                self.adb.write().unwrap().clear();
                if reason.is_empty() {
                    return Ok(changed);
                }
                bail!("{reason}");
            }
        };
        let devices: Vec<_> = detected
            .into_iter()
            .take(128)
            .map(|connection| {
                (
                    SharedDevice::new(
                        device_id("adb", &connection.lan_identity()),
                        connection.display_name().into(),
                        DeviceKind::Adb,
                        connection.serial().into(),
                        "ADB shell".into(),
                        "utf-8".into(),
                    ),
                    Endpoint::Adb(connection),
                )
            })
            .collect();
        let mut stored = self.adb.write().unwrap();
        let changed = !stored
            .iter()
            .map(|(device, _)| device)
            .eq(devices.iter().map(|(device, _)| device));
        *stored = devices;
        Ok(changed)
    }

    fn endpoint(&self, id: &str) -> Option<Endpoint> {
        self.serial
            .read()
            .unwrap()
            .iter()
            .chain(self.adb.read().unwrap().iter())
            .find(|(device, _)| device.id == id)
            .map(|(_, endpoint)| endpoint.clone())
    }
}

impl DeviceProvider for DeviceCatalog {
    fn run_adb(
        &self,
        device: &str,
        service: &str,
        commands: tokio::sync::mpsc::Receiver<DeviceInput>,
        events: tokio::sync::mpsc::Sender<DeviceEvent>,
        cancelled: &AtomicBool,
    ) -> Result<()> {
        let Some(Endpoint::Adb(connection)) = self.endpoint(device) else {
            bail!("adb_service_unavailable");
        };
        let stream = connection.open_lan_service(service, cancelled)?;
        relay_adb(stream, commands, events, cancelled)
    }
    fn devices(&self) -> Vec<SharedDevice> {
        self.serial
            .read()
            .unwrap()
            .iter()
            .chain(self.adb.read().unwrap().iter())
            .map(|(device, _)| device.clone())
            .collect()
    }

    fn resource(&self, id: &str) -> Option<String> {
        self.endpoint(id).map(|endpoint| match endpoint {
            Endpoint::Serial(line) => format!(
                "serial:{}",
                if cfg!(windows) {
                    line.device
                        .trim_start_matches("\\\\.\\")
                        .to_ascii_uppercase()
                } else {
                    line.device.to_string()
                }
            ),
            Endpoint::Adb(connection) => format!("adb:{}", connection.serial()),
        })
    }

    fn is_available(&self, id: &str) -> bool {
        self.serial
            .read()
            .unwrap()
            .iter()
            .chain(self.adb.read().unwrap().iter())
            .any(|(device, _)| device.id == id)
    }

    fn run(
        &self,
        device: &str,
        size: DeviceSize,
        commands: tokio::sync::mpsc::Receiver<DeviceInput>,
        events: tokio::sync::mpsc::Sender<DeviceEvent>,
        cancelled: &AtomicBool,
    ) -> Result<()> {
        ensure!(
            !cancelled.load(Ordering::Acquire),
            "device_stream_cancelled"
        );
        match self
            .endpoint(device)
            .ok_or_else(|| anyhow::anyhow!("device_unavailable"))?
        {
            Endpoint::Serial(line) => serial(line, commands, events, cancelled),
            Endpoint::Adb(connection) => adb(connection, size, commands, events, cancelled),
        }
    }
}

fn relay_adb(
    mut stream: std::net::TcpStream,
    mut commands: tokio::sync::mpsc::Receiver<DeviceInput>,
    events: tokio::sync::mpsc::Sender<DeviceEvent>,
    cancelled: &AtomicBool,
) -> Result<()> {
    events.blocking_send(DeviceEvent::Started)?;
    let mut buffer = [0u8; shellrs_lan::DEVICE_CHUNK];
    let mut pending = Vec::new();
    let mut offset = 0;
    while !cancelled.load(Ordering::Acquire) {
        let mut progress = false;
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => {
                events.blocking_send(DeviceEvent::Output(buffer[..count].to_vec()))?;
                progress = true;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }
        if pending.is_empty() {
            match commands.try_recv() {
                Ok(DeviceInput::Write(bytes)) => {
                    pending = bytes;
                    offset = 0;
                }
                Ok(_) => bail!("invalid_adb_input"),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return Ok(()),
            }
        }
        if !pending.is_empty() {
            match stream.write(&pending[offset..]) {
                Ok(0) => bail!("adb_disconnected"),
                Ok(count) => {
                    offset += count;
                    progress = true;
                    if offset == pending.len() {
                        pending.clear();
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
        }
        if !progress {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    Ok(())
}

fn serial(
    line: SerialLine,
    mut commands: tokio::sync::mpsc::Receiver<DeviceInput>,
    events: tokio::sync::mpsc::Sender<DeviceEvent>,
    cancelled: &AtomicBool,
) -> Result<()> {
    let mut port = crate::serial::open_shared_port(&line)
        .map_err(|_| anyhow::anyhow!("shared_serial_open_failed"))?;
    port.set_read_timeout(Duration::from_millis(10))?;
    port.set_write_timeout(Duration::from_millis(20))?;
    ensure!(
        !cancelled.load(Ordering::Acquire),
        "device_stream_cancelled"
    );
    events.blocking_send(DeviceEvent::Started)?;
    let mut buffer = [0u8; shellrs_lan::DEVICE_CHUNK];
    let mut pending = VecDeque::new();
    let mut offset = 0;
    while !cancelled.load(Ordering::Acquire) {
        match port.read(&mut buffer) {
            Ok(0) => bail!("shared_serial_disconnected"),
            Ok(count) => events.blocking_send(DeviceEvent::Output(buffer[..count].to_vec()))?,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => bail!("shared_serial_disconnected"),
        }
        for _ in 0..8 {
            if cancelled.load(Ordering::Acquire) {
                break;
            }
            if pending.len() >= 32 {
                break;
            }
            match commands.try_recv() {
                Ok(command) => pending.push_back(command),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return Ok(()),
            }
        }
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        match pending.front() {
            Some(DeviceInput::Write(bytes)) => {
                ensure!(
                    bytes.len() <= shellrs_lan::DEVICE_CHUNK,
                    "invalid_device_input"
                );
                match port.write(&bytes[offset..]) {
                    Ok(0) => bail!("shared_serial_write_failed"),
                    Ok(count) => {
                        offset += count;
                        if offset == bytes.len() {
                            pending.pop_front();
                            offset = 0;
                        }
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::TimedOut
                                | io::ErrorKind::WouldBlock
                                | io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => bail!("shared_serial_write_failed"),
                }
            }
            Some(DeviceInput::Break) => {
                port.set_break(true)?;
                for _ in 0..15 {
                    if cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                port.set_break(false)?;
                pending.pop_front();
            }
            Some(DeviceInput::Resize(_)) => {
                pending.pop_front();
            }
            None => {}
        }
    }
    Ok(())
}

fn adb(
    connection: AdbConnection,
    size: DeviceSize,
    mut commands: tokio::sync::mpsc::Receiver<DeviceInput>,
    events: tokio::sync::mpsc::Sender<DeviceEvent>,
    cancelled: &AtomicBool,
) -> Result<()> {
    let command = connection.lan_terminal_command(cancelled)?;
    ensure!(
        !cancelled.load(Ordering::Acquire),
        "device_stream_cancelled"
    );
    let pair = portable_pty::native_pty_system().openpty(pty_size(size))?;
    let mut child = AdbChild(pair.slave.spawn_command(command)?);
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    events.blocking_send(DeviceEvent::Started)?;
    let output = events.clone();
    let stopped = Arc::new(AtomicBool::new(false));
    let reader_stopped = stopped.clone();
    let reading = std::thread::Builder::new()
        .name("shelllf-lan-adb-output".into())
        .spawn(move || {
            let mut bytes = [0u8; shellrs_lan::DEVICE_CHUNK];
            while !reader_stopped.load(Ordering::Acquire) {
                match reader.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(count) => {
                        if output
                            .blocking_send(DeviceEvent::Output(bytes[..count].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        })?;
    let result = std::thread::scope(|scope| -> Result<()> {
        let master = pair.master;
        let stopped_input = stopped.clone();
        let writing = scope.spawn(move || -> Result<()> {
            while !cancelled.load(Ordering::Acquire) && !stopped_input.load(Ordering::Acquire) {
                match commands.try_recv() {
                    Ok(DeviceInput::Write(bytes)) => {
                        ensure!(
                            bytes.len() <= shellrs_lan::DEVICE_CHUNK,
                            "invalid_device_input"
                        );
                        writer.write_all(&bytes)?;
                        writer.flush()?;
                    }
                    Ok(DeviceInput::Resize(size)) => master.resize(pty_size(size))?,
                    Ok(DeviceInput::Break) => {}
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                }
            }
            Ok(())
        });
        let outcome = loop {
            if cancelled.load(Ordering::Acquire) || writing.is_finished() {
                break Ok(());
            }
            match child.0.try_wait() {
                Ok(Some(status)) => {
                    break if status.success() {
                        Ok(())
                    } else {
                        Err(anyhow::anyhow!("shared_adb_disconnected"))
                    };
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(error) => break Err(error.into()),
            }
        };
        stopped.store(true, Ordering::Release);
        let _ = child.0.kill();
        let _ = child.0.wait();
        let written = writing
            .join()
            .map_err(|_| anyhow::anyhow!("shared_adb_disconnected"))?;
        outcome.and(written)
    });
    stopped.store(true, Ordering::Release);
    let _ = child.0.kill();
    let _ = child.0.wait();
    drop(events);
    let _ = reading.join();
    result
}

struct AdbChild(Box<dyn portable_pty::Child + Send + Sync>);

impl Drop for AdbChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn pty_size(size: DeviceSize) -> portable_pty::PtySize {
    portable_pty::PtySize {
        rows: size.rows,
        cols: size.columns,
        pixel_width: 0,
        pixel_height: 0,
    }
}
