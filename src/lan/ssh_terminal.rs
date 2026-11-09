use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use russh::ChannelMsg;
use shellrs_lan::{DEVICE_CHUNK, DeviceEvent, DeviceInput, DeviceSize};

use crate::ssh::{
    ConnectionPrompt, ConnectionPromptKind, MissingCredential, PtySize, SshConnectionConfig,
    SshConnector, SshPrompts, open_shell, resize_pty, write_shell,
};

pub(super) fn run(
    connector: &SshConnector,
    configuration: &SshConnectionConfig,
    size: DeviceSize,
    mut commands: tokio::sync::mpsc::Receiver<DeviceInput>,
    events: tokio::sync::mpsc::Sender<DeviceEvent>,
    cancelled: &AtomicBool,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let seen = Arc::new(AtomicU8::new(0));
    let observed = seen.clone();
    let (_shutdown, shutdown) = tokio::sync::watch::channel(false);
    let prompts = Arc::new(
        SshPrompts::new(
            Arc::new(move |prompt: ConnectionPrompt| {
                let kind = match prompt.kind() {
                    ConnectionPromptKind::UnknownHost(_) => 1,
                    ConnectionPromptKind::HostKeyChanged(_) => 2,
                    ConnectionPromptKind::Authentication(_) => 4,
                };
                observed.fetch_or(kind, Ordering::Release);
                false
            }),
            shutdown,
        )
        .non_interactive(),
    );
    runtime.block_on(async {
        let login = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(20), connector.connect(configuration, prompts)) => {
                result.map_err(|_| anyhow!("shared_host_connect_failed"))?
                    .map_err(|error| login_error(error, seen.load(Ordering::Acquire)))?
            }
            _ = wait_cancelled(cancelled) => bail!("device_stream_cancelled"),
        };
        let (handle, _) = login;
        let result = async {
            let mut channel = tokio::time::timeout(Duration::from_secs(10), open_shell(&handle, "xterm-256color", pty_size(size)))
                .await.map_err(|_| anyhow!("shared_host_connect_failed"))?
                .map_err(|_| anyhow!("shared_host_shell_refused"))?;
            events.send(DeviceEvent::Started).await?;
            let mut exit = None;
            let mut signal = None;
            loop {
                tokio::select! {
                    command = commands.recv() => match command {
                        Some(DeviceInput::Write(bytes)) => write_shell(&channel, bytes).await?,
                        Some(DeviceInput::Resize(size)) => resize_pty(&channel, pty_size(size)).await?,
                        Some(DeviceInput::Break) => {},
                        None => bail!("device_stream_cancelled"),
                    },
                    event = channel.wait() => match event {
                        Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                            for bytes in data.chunks(DEVICE_CHUNK) {
                                events.send(DeviceEvent::Output(bytes.to_vec())).await?;
                            }
                        }
                        Some(ChannelMsg::ExitStatus { exit_status }) => exit = Some(exit_status),
                        Some(ChannelMsg::ExitSignal { signal_name, .. }) => signal = Some(format!("{signal_name:?}")),
                        Some(ChannelMsg::Failure) => bail!("shared_host_shell_refused"),
                        Some(ChannelMsg::Close) | None => {
                            if exit.is_none() && signal.is_none() {
                                bail!("shared_host_connection_lost");
                            }
                            events.send(DeviceEvent::Exited { code: exit.unwrap_or(255), signal }).await?;
                            break;
                        }
                        _ => {},
                    }
                }
            }
            Ok(())
        };
        let result = tokio::select! {
            result = result => result,
            _ = wait_cancelled(cancelled) => Err(anyhow!("device_stream_cancelled")),
        };
        let _ = tokio::time::timeout(Duration::from_secs(1), handle.disconnect(russh::Disconnect::ByApplication, "shared terminal closed", "en")).await;
        result
    })
}

fn pty_size(size: DeviceSize) -> PtySize {
    PtySize {
        columns: u32::from(size.columns),
        rows: u32::from(size.rows),
        width: 0,
        height: 0,
    }
}

fn login_error(error: anyhow::Error, seen: u8) -> anyhow::Error {
    anyhow!(if seen & 2 != 0 {
        "shared_host_key_changed"
    } else if seen & 1 != 0 {
        "shared_host_key_unknown"
    } else if seen & 4 != 0 || error.chain().any(|cause| cause.is::<MissingCredential>()) {
        "shared_host_credentials_missing"
    } else {
        "shared_host_connect_failed"
    })
}

async fn wait_cancelled(cancelled: &AtomicBool) {
    while !cancelled.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(test)]
mod tests;
