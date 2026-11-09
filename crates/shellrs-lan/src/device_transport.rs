use std::io;
use std::net::IpAddr;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};
use std::time::Duration;

use anyhow::{Result, bail, ensure};
use russh::{ChannelId, ChannelMsg, server};

use crate::{
    AuditAction, AuditEntry, Core, Device, DeviceEvent, DeviceInput, DeviceSize, RemoteRequest,
};

pub(super) struct OpenRequest {
    pub peer: String,
    pub device: String,
    pub source: IpAddr,
    pub size: DeviceSize,
    pub service: Option<String>,
    pub host: bool,
}

struct LeaseGuard {
    core: Arc<Core>,
    id: u64,
    request: OpenRequest,
    input: Arc<AtomicU64>,
    output: Arc<AtomicU64>,
    name: String,
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        let mut entry = AuditEntry::new(
            AuditAction::Closed,
            &self.request.peer,
            &self.request.device,
            "ended",
        );
        entry.source = Some(self.request.source);
        entry.device_name = self.name.clone();
        entry.bytes_in = self.input.load(Ordering::Relaxed);
        entry.bytes_out = self.output.load(Ordering::Relaxed);
        let _ = self.core.record_device_audit(entry);
        self.core.release_device(self.id);
    }
}

pub(super) fn start(
    core: Arc<Core>,
    request: OpenRequest,
    commands: tokio::sync::mpsc::Receiver<DeviceInput>,
    cancelled: Arc<AtomicBool>,
    channel: ChannelId,
    handle: server::Handle,
) {
    tokio::spawn(async move {
        let result = serve(core, request, commands, cancelled.clone(), channel, &handle).await;
        cancelled.store(true, Ordering::Release);
        if let Err(error) = result {
            let message: String = error.to_string().chars().take(2048).collect();
            let _ = tokio::time::timeout(
                Duration::from_secs(1),
                handle.extended_data(channel, 2, message.into_bytes()),
            )
            .await;
            let _ = handle.exit_status_request(channel, 255).await;
        } else {
            let _ = handle.exit_status_request(channel, 0).await;
        }
        let _ = handle.eof(channel).await;
        let _ = handle.close(channel).await;
    });
}

async fn serve(
    core: Arc<Core>,
    request: OpenRequest,
    mut commands: tokio::sync::mpsc::Receiver<DeviceInput>,
    cancelled: Arc<AtomicBool>,
    channel: ChannelId,
    handle: &server::Handle,
) -> Result<()> {
    ensure!(
        !request.device.is_empty() && request.device.len() <= 128,
        "invalid_device_request"
    );
    ensure!(
        core.authorized(&request.peer, None, false),
        "device_not_authorized"
    );
    if let Some(service) = &request.service {
        crate::validate_adb_service(service)?;
        ensure!(
            core.local_devices()
                .iter()
                .any(|device| device.id == request.device && device.kind == crate::DeviceKind::Adb),
            "adb_service_unavailable"
        );
    }
    let id = match core.acquire_stream(
        &request.peer,
        &request.device,
        request.source,
        cancelled.clone(),
        request.host,
    ) {
        Ok(id) => id,
        Err(error) => {
            let mut entry = AuditEntry::new(
                AuditAction::Denied,
                &request.peer,
                &request.device,
                &error.to_string(),
            );
            entry.source = Some(request.source);
            core.record_device_audit(entry)?;
            return Err(error);
        }
    };
    let peer = request.peer.clone();
    let device = request.device.clone();
    let source = request.source;
    let size = request.size;
    let service = request.service.clone();
    let host = request.host;
    let input = Arc::new(AtomicU64::new(0));
    let output = Arc::new(AtomicU64::new(0));
    let guard = LeaseGuard {
        core: core.clone(),
        id,
        request,
        input: input.clone(),
        output: output.clone(),
        name: if host {
            core.hosts()
                .into_iter()
                .find(|item| item.id == device)
                .map(|item| item.name)
                .unwrap_or_default()
        } else {
            core.local_devices()
                .into_iter()
                .find(|item| item.id == device)
                .map(|item| item.name)
                .unwrap_or_default()
        },
    };
    if let Some(service) = &service {
        let operation = if service == "sync:" {
            "adb_files"
        } else if service.starts_with("localabstract:") {
            "adb_mirror"
        } else if (service.contains("package") || service.contains("pm "))
            && service.contains("install")
        {
            "adb_install"
        } else {
            "adb_shell"
        };
        let mut entry = AuditEntry::new(AuditAction::Open, &peer, &device, operation);
        entry.source = Some(source);
        core.record_device_audit(entry)?;
    }
    let provider = core.device_provider.read().unwrap().clone();
    let executor = core.executor.clone();
    let (provider_commands, receiver) = tokio::sync::mpsc::channel(32);
    let (events, mut received) = tokio::sync::mpsc::channel(32);
    let (finished, completion) = tokio::sync::oneshot::channel();
    let target = device.clone();
    let worker_cancel = cancelled.clone();
    std::thread::Builder::new()
        .name("shelllf-lan-device".into())
        .spawn(move || {
            let _guard = guard;
            let result = if host {
                executor.open_terminal(&target, size, receiver, events, &worker_cancel)
            } else if let Some(service) = service {
                provider.run_adb(&target, &service, receiver, events, &worker_cancel)
            } else {
                provider.run(&target, size, receiver, events, &worker_cancel)
            };
            let _ = finished.send(result);
        })?;
    let mut started = false;
    let mut opening =
        Some(tokio::time::Instant::now() + Duration::from_secs(if host { 40 } else { 15 }));
    loop {
        ensure!(
            core.stream_authorized(&peer, &device, source, host)
                && !cancelled.load(Ordering::Acquire),
            "device_control_revoked"
        );
        ensure!(
            opening.is_none_or(|deadline| tokio::time::Instant::now() < deadline),
            "device_open_timed_out"
        );
        tokio::select! {
            event = received.recv() => match event {
                Some(DeviceEvent::Started) => {
                    ensure!(!started, "invalid_device_event");
                    started = true;
                    opening = None;
                    tokio::time::timeout(Duration::from_secs(2), handle.extended_data(channel, 3, b"started".to_vec())).await?.map_err(|_| anyhow::anyhow!("disconnected"))?;
                }
                Some(DeviceEvent::Output(bytes)) => {
                    ensure!(started && bytes.len() <= crate::DEVICE_CHUNK, "invalid_device_event");
                    output.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                    tokio::time::timeout(Duration::from_secs(2), handle.data(channel, bytes)).await?.map_err(|_| anyhow::anyhow!("disconnected"))?;
                }
                Some(DeviceEvent::Closed) => break,
                Some(DeviceEvent::Exited { code, signal }) => {
                    ensure!(host && started && signal.as_ref().is_none_or(|signal| signal.len() <= 128), "invalid_device_event");
                    let bytes = serde_json::to_vec(&(code, signal))?;
                    tokio::time::timeout(Duration::from_secs(2), handle.extended_data(channel, 4, bytes)).await?.map_err(|_| anyhow::anyhow!("disconnected"))?;
                    break;
                }
                None => { completion.await.map_err(|_| anyhow::anyhow!("device_worker_stopped"))??; break; }
            },
            command = commands.recv(), if started => match command {
                Some(command) => {
                    ensure!(core.stream_authorized(&peer, &device, source, host) && !cancelled.load(Ordering::Acquire), "device_control_revoked");
                    match &command {
                        DeviceInput::Write(bytes) => { input.fetch_add(bytes.len() as u64, Ordering::Relaxed); }
                        DeviceInput::Break => {
                            let mut entry = AuditEntry::new(AuditAction::Break, &peer, &device, "requested");
                            entry.source = Some(source);
                            core.record_device_audit(entry)?;
                        }
                        DeviceInput::Resize(_) => {}
                    }
                    tokio::time::timeout(Duration::from_millis(200), provider_commands.send(command)).await?.map_err(|_| anyhow::anyhow!("device_worker_stopped"))?;
                }
                None => break,
            },
            _ = tokio::time::sleep(Duration::from_millis(50)) => {},
        }
    }
    ensure!(
        core.stream_authorized(&peer, &device, source, host) && !cancelled.load(Ordering::Acquire),
        "device_control_revoked"
    );
    Ok(())
}

pub(super) fn stream_host(
    core: &Core,
    device: &Device,
    host: &str,
    size: DeviceSize,
    commands: mpsc::Receiver<DeviceInput>,
    cancelled: &AtomicBool,
    events: &mut dyn FnMut(DeviceEvent) -> io::Result<()>,
) -> Result<()> {
    stream_request(
        core,
        device,
        RemoteRequest::OpenHost {
            host: host.to_owned(),
            columns: size.columns,
            rows: size.rows,
        },
        commands,
        cancelled,
        events,
    )
}

pub(super) fn stream_device(
    core: &Core,
    device: &Device,
    target: &str,
    size: DeviceSize,
    commands: mpsc::Receiver<DeviceInput>,
    cancelled: &AtomicBool,
    events: &mut dyn FnMut(DeviceEvent) -> io::Result<()>,
) -> Result<()> {
    stream_request(
        core,
        device,
        RemoteRequest::OpenDevice {
            device: target.to_owned(),
            columns: size.columns,
            rows: size.rows,
        },
        commands,
        cancelled,
        events,
    )
}

pub(super) fn stream_adb(
    core: &Core,
    device: &Device,
    target: &str,
    service: &str,
    commands: mpsc::Receiver<DeviceInput>,
    cancelled: &AtomicBool,
    events: &mut dyn FnMut(DeviceEvent) -> io::Result<()>,
) -> Result<()> {
    stream_request(
        core,
        device,
        RemoteRequest::OpenAdb {
            device: target.to_owned(),
            service: service.to_owned(),
        },
        commands,
        cancelled,
        events,
    )
}

fn stream_request(
    core: &Core,
    device: &Device,
    request: RemoteRequest,
    commands: mpsc::Receiver<DeviceInput>,
    cancelled: &AtomicBool,
    events: &mut dyn FnMut(DeviceEvent) -> io::Result<()>,
) -> Result<()> {
    let (target, size) = match &request {
        RemoteRequest::OpenDevice {
            device,
            columns,
            rows,
        } => (device, DeviceSize::new(*columns, *rows)?),
        RemoteRequest::OpenHost {
            host,
            columns,
            rows,
        } => (host, DeviceSize::new(*columns, *rows)?),
        RemoteRequest::OpenAdb { device, service } => {
            crate::validate_adb_service(service)?;
            (device, DeviceSize::new(80, 24)?)
        }
        _ => bail!("invalid_device_request"),
    };
    DeviceSize::new(size.columns, size.rows)?;
    ensure!(
        target.len() <= 128 && !target.is_empty(),
        "invalid_device_request"
    );
    crate::transport::runtime()?.block_on(async {
        let (session, _, accepted) = crate::transport::connect(core, device.endpoint, Some(device.id.clone()), "trusted", None).await?;
        let result: Result<()> = async {
            ensure!(accepted, "device_not_authorized");
            ensure!(!cancelled.load(Ordering::Acquire) && core.is_active(), "device_stream_cancelled");
            let mut channel = tokio::time::timeout(Duration::from_secs(5), session.channel_open_session()).await??;
            channel.exec(true, serde_json::to_vec(&request)?).await?;
            let mut started = false;
            let host = matches!(request, RemoteRequest::OpenHost { .. });
            let mut opening = Some(tokio::time::Instant::now() + Duration::from_secs(if host { 45 } else { 20 }));
            let mut exit = None;
            let mut shell_exit = None;
            let mut failure = None;
            loop {
                ensure!(!cancelled.load(Ordering::Acquire) && core.is_active() && core.has_peer(&device.id), "device_stream_cancelled");
                ensure!(opening.is_none_or(|deadline| tokio::time::Instant::now() < deadline), "device_open_timed_out");
                if started {
                    for _ in 0..16 {
                        match commands.try_recv() {
                            Ok(command) => { tokio::time::timeout(Duration::from_secs(1), channel.data(crate::devices::input_frame(&command)?.as_slice())).await??; }
                            Err(mpsc::TryRecvError::Empty) => break,
                            Err(mpsc::TryRecvError::Disconnected) => bail!("device_stream_cancelled"),
                        }
                    }
                }
                let message = tokio::select! { message = channel.wait() => message, _ = tokio::time::sleep(Duration::from_millis(20)) => continue };
                match message {
                    Some(ChannelMsg::ExtendedData { ext: 3, data }) => {
                        ensure!(!started && data.as_ref() == b"started", "invalid_device_event");
                        started = true;
                        opening = None;
                        events(DeviceEvent::Started)?;
                    }
                    Some(ChannelMsg::Data { data }) => {
                        ensure!(started && data.len() <= 128 * 1024, "invalid_device_event");
                        for bytes in data.chunks(crate::DEVICE_CHUNK) { events(DeviceEvent::Output(bytes.to_vec()))?; }
                    }
                    Some(ChannelMsg::ExtendedData { ext: 2, data }) => {
                        ensure!(data.len() <= 4096, "invalid_device_event");
                        failure = Some(String::from_utf8_lossy(&data).into_owned());
                    }
                    Some(ChannelMsg::ExtendedData { ext: 4, data }) => {
                        ensure!(host && started && shell_exit.is_none() && data.len() <= 1024, "invalid_device_event");
                        let (code, signal): (u32, Option<String>) = serde_json::from_slice(&data)?;
                        ensure!(signal.as_ref().is_none_or(|signal| signal.len() <= 128), "invalid_device_event");
                        shell_exit = Some(DeviceEvent::Exited { code, signal });
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => exit = Some(exit_status),
                    Some(ChannelMsg::Failure) => bail!("device_request_refused"),
                    Some(ChannelMsg::Close) | None => break,
                    _ => {},
                }
            }
            if let Some(failure) = failure { bail!("{failure}"); }
            ensure!(exit == Some(0), "device_connection_lost");
            events(shell_exit.unwrap_or(DeviceEvent::Closed))?;
            Ok(())
        }.await;
        let _ = tokio::time::timeout(Duration::from_secs(1), session.disconnect(russh::Disconnect::ByApplication, "device stream ended", "en")).await;
        result
    })
}
