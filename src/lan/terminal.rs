use std::io;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

use anyhow::{Result, ensure};
use shellrs_lan::{Core, DeviceEvent, DeviceInput, DeviceKind, DeviceSize, SharedDevice};

use crate::terminal::{
    SerialTerminalTransportProvider, SharedTerminalTransportFactory, TerminalSize,
    TerminalTransport, TerminalTransportCommand, TerminalTransportEvent, TerminalTransportFactory,
    send_event,
};

#[derive(Clone)]
pub(crate) struct DeviceTerminalFactory {
    core: Arc<Core>,
    peer: String,
    device: SharedDevice,
}

impl DeviceTerminalFactory {
    pub fn new(core: Arc<Core>, peer: String, device: SharedDevice) -> Self {
        Self { core, peer, device }
    }
}

impl SerialTerminalTransportProvider for DeviceTerminalFactory {
    fn factory_for(&self, _: &crate::host::SerialLine) -> SharedTerminalTransportFactory {
        Arc::new(self.clone())
    }
}

impl TerminalTransportFactory for DeviceTerminalFactory {
    fn create(&self) -> Box<dyn TerminalTransport> {
        Box::new(self.clone())
    }
}

impl TerminalTransport for DeviceTerminalFactory {
    fn run(
        self: Box<Self>,
        initial_size: TerminalSize,
        commands: mpsc::Receiver<TerminalTransportCommand>,
        events: async_channel::Sender<TerminalTransportEvent>,
    ) -> Result<()> {
        let size = DeviceSize::new(
            initial_size.columns().clamp(1, 1000) as u16,
            initial_size.rows().clamp(1, 1000) as u16,
        )?;
        let (input, received) = mpsc::sync_channel(32);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let core = self.core.clone();
        let peer = self.peer.clone();
        let device = self.device.id.clone();
        let worker = std::thread::Builder::new()
            .name("shelllf-lan-terminal".into())
            .spawn(move || {
                core.stream_device(
                    &peer,
                    &device,
                    size,
                    received,
                    &worker_cancelled,
                    &mut |event| {
                        let event = match event {
                            DeviceEvent::Started => TerminalTransportEvent::Started,
                            DeviceEvent::Output(bytes) => TerminalTransportEvent::Output(bytes),
                            DeviceEvent::Closed => TerminalTransportEvent::Exited {
                                code: 0,
                                signal: None,
                            },
                        };
                        if send_event(&events, event) {
                            Ok(())
                        } else {
                            Err(io::ErrorKind::BrokenPipe.into())
                        }
                    },
                )
            })?;
        let mut shutdown = false;
        let result = (|| -> Result<()> {
            while !worker.is_finished() {
                match commands.recv_timeout(Duration::from_millis(20)) {
                    Ok(
                        TerminalTransportCommand::Write(bytes)
                        | TerminalTransportCommand::WriteData(bytes),
                    ) => {
                        for chunk in bytes.chunks(shellrs_lan::DEVICE_CHUNK) {
                            ensure!(
                                input.try_send(DeviceInput::Write(chunk.to_vec())).is_ok(),
                                "device_input_backpressure"
                            );
                        }
                    }
                    Ok(TerminalTransportCommand::Resize(size)) => {
                        let size = DeviceSize::new(
                            size.columns().clamp(1, 1000) as u16,
                            size.rows().clamp(1, 1000) as u16,
                        )?;
                        ensure!(
                            input.try_send(DeviceInput::Resize(size)).is_ok(),
                            "device_input_backpressure"
                        );
                    }
                    Ok(TerminalTransportCommand::SendBreak)
                        if self.device.kind == DeviceKind::Serial =>
                    {
                        ensure!(
                            input.try_send(DeviceInput::Break).is_ok(),
                            "device_input_backpressure"
                        );
                    }
                    Ok(TerminalTransportCommand::Shutdown)
                    | Err(mpsc::RecvTimeoutError::Disconnected) => {
                        shutdown = true;
                        break;
                    }
                    Ok(
                        TerminalTransportCommand::Exec(request)
                        | TerminalTransportCommand::Monitor(request)
                        | TerminalTransportCommand::SerialTransfer(request),
                    ) => {
                        let _ = request
                            .reply
                            .send(Err(
                                crate::i18n::t!("lan.devices.no_side_channels").to_string()
                            ));
                    }
                    Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
            Ok(())
        })();
        cancelled.store(true, Ordering::Release);
        drop(input);
        let finished = worker
            .join()
            .map_err(|_| anyhow::anyhow!("device_connection_lost"))?;
        if shutdown {
            return Ok(());
        }
        result
            .and(finished)
            .map_err(|error| anyhow::anyhow!(super::describe_error(&error)))
    }
}
