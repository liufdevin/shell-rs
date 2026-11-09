use anyhow::{Result, bail, ensure};
use shellrs_lan::{Core, DeviceEvent, DeviceInput};
use std::{
    collections::HashMap,
    io::{self, Read as _, Write as _},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const FEATURES: &str = "shell_v2,cmd";

struct State {
    core: Arc<Core>,
    peer: String,
    device: String,
    serial: String,
    stopped: AtomicBool,
    connections: AtomicUsize,
    forwards: Mutex<HashMap<u16, Arc<AtomicBool>>>,
}

impl State {
    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
            || !self.core.is_active()
            || !self.core.has_peer(&self.peer)
    }

    fn verify_device(&self) -> Result<()> {
        let response = self.core.request(
            &self.peer,
            shellrs_lan::RemoteRequest::Devices,
            &self.stopped,
            &mut |_, _| Ok(()),
        )?;
        if !response
            .devices
            .iter()
            .any(|device| device.id == self.device)
        {
            self.stopped.store(true, Ordering::Release);
            bail!("device_control_revoked");
        }
        Ok(())
    }
}

pub(crate) struct AdbProxy {
    state: Arc<State>,
    address: SocketAddr,
    _worker: JoinHandle<()>,
}

impl AdbProxy {
    pub(crate) fn start(
        core: Arc<Core>,
        peer: String,
        device: String,
        serial: String,
    ) -> Result<Arc<Self>> {
        ensure!(
            !serial.is_empty() && serial.len() <= 1024 && !serial.contains(['\0', '\n', '\r']),
            "invalid_adb_serial"
        );
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let state = Arc::new(State {
            core,
            peer,
            device,
            serial,
            stopped: AtomicBool::new(false),
            connections: AtomicUsize::new(0),
            forwards: Mutex::new(HashMap::new()),
        });
        let worker_state = state.clone();
        let worker = thread::Builder::new()
            .name("shelllf-lan-adb-proxy".into())
            .spawn(move || {
                let mut workers = Vec::new();
                while !worker_state.is_stopped() {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if worker_state.connections.fetch_add(1, Ordering::AcqRel) >= 12 {
                                worker_state.connections.fetch_sub(1, Ordering::AcqRel);
                                continue;
                            }
                            let state = worker_state.clone();
                            workers.push(thread::spawn(move || {
                                let _permit = ConnectionPermit(state.clone());
                                if let Err(error) = serve(stream, state.clone())
                                    && !state.is_stopped()
                                {
                                    tracing::warn!(%error, "LAN ADB proxy request failed");
                                }
                            }));
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10))
                        }
                        Err(_) => break,
                    }
                    workers.retain(|worker| !worker.is_finished());
                }
                worker_state.stopped.store(true, Ordering::Release);
                for worker in workers {
                    let _ = worker.join();
                }
            })?;
        Ok(Arc::new(Self {
            state,
            address,
            _worker: worker,
        }))
    }

    pub(crate) fn socket(&self) -> String {
        format!("tcp:{}", self.address)
    }
    pub(crate) fn is_stopped(&self) -> bool {
        self.state.is_stopped()
    }
}

impl Drop for AdbProxy {
    fn drop(&mut self) {
        self.state.stopped.store(true, Ordering::Release);
    }
}

struct ConnectionPermit(Arc<State>);
impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::AcqRel);
    }
}

fn read_exact(stream: &mut TcpStream, bytes: &mut [u8], state: &State) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut offset = 0;
    while offset < bytes.len() {
        ensure!(
            !state.is_stopped() && Instant::now() < deadline,
            "adb_proxy_stopped"
        );
        match stream.read(&mut bytes[offset..]) {
            Ok(0) => bail!("adb_proxy_disconnected"),
            Ok(count) => offset += count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn request(stream: &mut TcpStream, state: &State) -> Result<String> {
    let mut header = [0; 4];
    read_exact(stream, &mut header, state)?;
    let length = usize::from_str_radix(std::str::from_utf8(&header)?, 16)?;
    ensure!(length > 0 && length <= 16384, "invalid_adb_request");
    let mut bytes = vec![0; length];
    read_exact(stream, &mut bytes, state)?;
    Ok(String::from_utf8(bytes)?)
}

fn reply(stream: &mut TcpStream, value: &str) -> Result<()> {
    stream.write_all(format!("OKAY{:04x}{value}", value.len()).as_bytes())?;
    Ok(())
}

fn serve(mut stream: TcpStream, state: Arc<State>) -> Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_millis(100)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut selected = false;
    for _ in 0..8 {
        let message = request(&mut stream, &state)?;
        if selected && !message.starts_with("host:") {
            shellrs_lan::validate_adb_service(&message)?;
            return relay(stream, state, &message, true);
        }
        if message == "host:version" {
            return reply(&mut stream, "0029");
        }
        if message == "host:features" || message == "host:host-features" {
            return reply(&mut stream, FEATURES);
        }
        if message == "host:devices" || message == "host:devices-l" {
            state.verify_device()?;
            let details = if message == "host:devices-l" {
                " product:ShellLF model:LAN_device device:LAN transport_id:1"
            } else {
                ""
            };
            return reply(&mut stream, &format!("{}\tdevice{details}\n", state.serial));
        }
        if message == format!("host:transport:{}", state.serial)
            || message == format!("host:tport:serial:{}", state.serial)
        {
            stream.write_all(b"OKAY")?;
            if message.starts_with("host:tport:") {
                stream.write_all(&1u64.to_le_bytes())?;
            }
            selected = true;
            continue;
        }
        let prefix = format!("host-serial:{}:", state.serial);
        if let Some(command) = message.strip_prefix(&prefix).or_else(|| {
            if selected {
                message.strip_prefix("host:")
            } else {
                None
            }
        }) {
            match command {
                "features" => return reply(&mut stream, FEATURES),
                "get-state" => {
                    state.verify_device()?;
                    return reply(&mut stream, "device");
                }
                "get-serialno" => return reply(&mut stream, &state.serial),
                "get-devpath" => return reply(&mut stream, "lan"),
                _ => {}
            }
            if let Some(specification) = command.strip_prefix("forward:") {
                let port = forward(&state, specification)?;
                stream.write_all(b"OKAYOKAY")?;
                if specification.starts_with("tcp:0;")
                    || specification.starts_with("norebind:tcp:0;")
                {
                    stream.write_all(format!("{:04x}{port}", port.to_string().len()).as_bytes())?;
                }
                return Ok(());
            }
            if let Some(port) = command
                .strip_prefix("killforward:tcp:")
                .and_then(|port| port.parse::<u16>().ok())
            {
                if let Some(cancel) = state.forwards.lock().unwrap().remove(&port) {
                    cancel.store(true, Ordering::Release);
                }
                stream.write_all(b"OKAYOKAY")?;
                return Ok(());
            }
        }
        let reason = "ShellLF: only the authorized LAN device is available";
        stream.write_all(format!("FAIL{:04x}{reason}", reason.len()).as_bytes())?;
        bail!("invalid_adb_request");
    }
    bail!("invalid_adb_request")
}

fn forward(state: &Arc<State>, specification: &str) -> Result<u16> {
    let specification = specification
        .strip_prefix("norebind:")
        .unwrap_or(specification);
    let (local, remote) = specification
        .split_once(';')
        .ok_or_else(|| anyhow::anyhow!("invalid_adb_forward"))?;
    let port: u16 = local
        .strip_prefix("tcp:")
        .ok_or_else(|| anyhow::anyhow!("invalid_adb_forward"))?
        .parse()?;
    ensure!(
        remote.starts_with("localabstract:scrcpy_"),
        "invalid_adb_forward"
    );
    shellrs_lan::validate_adb_service(remote)?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut forwards = state.forwards.lock().unwrap();
    ensure!(
        forwards.len() < 4 && !forwards.contains_key(&port),
        "too_many_adb_forwards"
    );
    forwards.insert(port, cancel.clone());
    drop(forwards);
    let target = remote.to_owned();
    let state = state.clone();
    thread::Builder::new()
        .name("shelllf-lan-scrcpy-forward".into())
        .spawn(move || {
            let mut workers = Vec::new();
            while !state.is_stopped() && !cancel.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if state.connections.fetch_add(1, Ordering::AcqRel) >= 12 {
                            state.connections.fetch_sub(1, Ordering::AcqRel);
                            continue;
                        }
                        let (state, target) = (state.clone(), target.clone());
                        workers.push(thread::spawn(move || {
                            let _permit = ConnectionPermit(state.clone());
                            if let Err(error) = relay(stream, state.clone(), &target, false)
                                && !state.is_stopped()
                            {
                                tracing::warn!(%error, "LAN scrcpy socket relay failed");
                            }
                        }));
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
                workers.retain(|worker| !worker.is_finished());
            }
            cancel.store(true, Ordering::Release);
            for worker in workers {
                let _ = worker.join();
            }
        })?;
    Ok(port)
}

fn relay(mut stream: TcpStream, state: Arc<State>, service: &str, acknowledge: bool) -> Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_millis(50)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    let mut reader = stream.try_clone()?;
    let shutdown = stream.try_clone()?;
    let cancelled = AtomicBool::new(false);
    let (sender, receiver) = mpsc::sync_channel(16);
    thread::scope(|scope| {
        let read = scope.spawn(|| {
            let mut bytes = [0; shellrs_lan::DEVICE_CHUNK];
            while !cancelled.load(Ordering::Acquire) && !state.is_stopped() {
                match reader.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(count) => {
                        let mut input = DeviceInput::Write(bytes[..count].to_vec());
                        loop {
                            match sender.try_send(input) {
                                Ok(()) => break,
                                Err(mpsc::TrySendError::Full(returned))
                                    if !cancelled.load(Ordering::Acquire)
                                        && !state.is_stopped() =>
                                {
                                    input = returned;
                                    thread::sleep(Duration::from_millis(2));
                                }
                                Err(_) => {
                                    cancelled.store(true, Ordering::Release);
                                    return;
                                }
                            }
                        }
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                        ) => {}
                    Err(_) => break,
                }
            }
            cancelled.store(true, Ordering::Release);
        });
        let mut acknowledged = false;
        let result = state.core.stream_adb(
            &state.peer,
            &state.device,
            service,
            receiver,
            &cancelled,
            &mut |event| match event {
                DeviceEvent::Started if acknowledge => {
                    acknowledged = true;
                    stream.write_all(b"OKAY")
                }
                DeviceEvent::Output(bytes) => stream.write_all(&bytes),
                _ => Ok(()),
            },
        );
        if acknowledge
            && !acknowledged
            && let Err(error) = &result
        {
            let message = crate::lan::describe_error(error);
            let _ = stream.write_all(format!("FAIL{:04x}{message}", message.len()).as_bytes());
        }
        if result
            .as_ref()
            .is_err_and(|error| error.to_string().contains("device_control_"))
        {
            state.stopped.store(true, Ordering::Release);
        }
        cancelled.store(true, Ordering::Release);
        let _ = shutdown.shutdown(Shutdown::Both);
        let _ = read.join();
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use shellrs_lan::{
        DeviceKind, DeviceProvider, DeviceSize, Executor, Network, SharedDevice, SharedHost,
    };

    #[derive(Default)]
    struct Backend {
        services: Mutex<Vec<String>>,
        file_protocol: AtomicBool,
        file: Mutex<Vec<u8>>,
        apk: Mutex<Vec<u8>>,
        install_fails: AtomicBool,
        mirror_protocol: AtomicBool,
    }
    impl Executor for Backend {
        fn hosts(&self) -> Vec<SharedHost> {
            Vec::new()
        }
        fn execute(
            &self,
            _: &str,
            _: &str,
            _: &AtomicBool,
            _: &mut dyn FnMut(bool, &[u8]) -> io::Result<()>,
        ) -> Result<i32> {
            bail!("not supported")
        }
    }
    impl DeviceProvider for Backend {
        fn devices(&self) -> Vec<SharedDevice> {
            ["board", "private-board"]
                .into_iter()
                .map(|id| {
                    SharedDevice::new(
                        id.into(),
                        id.into(),
                        DeviceKind::Adb,
                        id.into(),
                        "ADB".into(),
                        "utf-8".into(),
                    )
                })
                .collect()
        }
        fn resource(&self, device: &str) -> Option<String> {
            Some(format!("adb:{device}"))
        }
        fn run(
            &self,
            _: &str,
            _: DeviceSize,
            _: tokio::sync::mpsc::Receiver<DeviceInput>,
            _: tokio::sync::mpsc::Sender<DeviceEvent>,
            _: &AtomicBool,
        ) -> Result<()> {
            bail!("not supported")
        }
        fn run_adb(
            &self,
            device: &str,
            service: &str,
            mut commands: tokio::sync::mpsc::Receiver<DeviceInput>,
            events: tokio::sync::mpsc::Sender<DeviceEvent>,
            cancel: &AtomicBool,
        ) -> Result<()> {
            ensure!(device == "board", "wrong device");
            self.services.lock().unwrap().push(service.into());
            events.blocking_send(DeviceEvent::Started)?;
            if self.file_protocol.load(Ordering::Acquire) && service == "sync:" {
                return self.sync(commands, events, cancel);
            }
            if service.starts_with("exec:") && service.contains("install") {
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut apk = Vec::new();
                while !cancel.load(Ordering::Acquire) && Instant::now() < deadline {
                    match commands.try_recv() {
                        Ok(DeviceInput::Write(bytes)) => {
                            apk.extend(bytes);
                            if apk.len() >= b"isolated apk payload".len() {
                                *self.apk.lock().unwrap() = apk;
                                let response = if self.install_fails.load(Ordering::Acquire) {
                                    "Failure [INSTALL_FAILED_TEST]\n"
                                } else {
                                    "Success\n"
                                };
                                events.blocking_send(DeviceEvent::Output(
                                    response.as_bytes().to_vec(),
                                ))?;
                                return Ok(());
                            }
                        }
                        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                        _ => thread::sleep(Duration::from_millis(2)),
                    }
                }
                bail!("fake APK upload stopped");
            }
            if service.starts_with("shell,v2,") && service.ends_with(":echo shelllf-lan-check") {
                let output = b"shelllf-lan-check\n";
                let mut frame = vec![1];
                frame.extend_from_slice(&(output.len() as u32).to_le_bytes());
                frame.extend_from_slice(output);
                frame.extend_from_slice(&[3, 1, 0, 0, 0, 0]);
                events.blocking_send(DeviceEvent::Output(frame))?;
                return Ok(());
            }
            if service.starts_with("localabstract:") {
                if self.mirror_protocol.load(Ordering::Acquire) {
                    let mut header = vec![0];
                    let mut name = [0; 64];
                    name[..12].copy_from_slice(b"ShellLF test");
                    header.extend_from_slice(&name);
                    header.extend_from_slice(b"h264");
                    header.extend_from_slice(&64u32.to_be_bytes());
                    header.extend_from_slice(&64u32.to_be_bytes());
                    events.blocking_send(DeviceEvent::Output(header))?;
                } else {
                    events.blocking_send(DeviceEvent::Output(b"video-header".to_vec()))?;
                }
            }
            while !cancel.load(Ordering::Acquire) {
                match commands.try_recv() {
                    Ok(DeviceInput::Write(bytes)) => {
                        events.blocking_send(DeviceEvent::Output(bytes))?;
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                    _ => thread::sleep(Duration::from_millis(2)),
                }
            }
            Ok(())
        }
    }

    impl Backend {
        fn sync(
            &self,
            mut commands: tokio::sync::mpsc::Receiver<DeviceInput>,
            events: tokio::sync::mpsc::Sender<DeviceEvent>,
            cancel: &AtomicBool,
        ) -> Result<()> {
            let mut buffer = Vec::new();
            let mut file = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(15);
            while !cancel.load(Ordering::Acquire) && Instant::now() < deadline {
                match commands.try_recv() {
                    Ok(DeviceInput::Write(bytes)) => buffer.extend(bytes),
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return Ok(()),
                    _ => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                }
                while buffer.len() >= 8 {
                    let kind: [u8; 4] = buffer[..4].try_into().unwrap();
                    let length = u32::from_le_bytes(buffer[4..8].try_into().unwrap()) as usize;
                    let payload = if &kind == b"DONE" { 0 } else { length };
                    ensure!(payload <= 65536, "oversized fake sync frame");
                    if buffer.len() < payload + 8 {
                        break;
                    }
                    let value = buffer[8..8 + payload].to_vec();
                    buffer.drain(..8 + payload);
                    let response = match &kind {
                        b"STAT" => {
                            let stored = self.file.lock().unwrap();
                            let mut response = b"STAT".to_vec();
                            response.extend_from_slice(
                                &(if stored.is_empty() { 0u32 } else { 0o100644u32 }).to_le_bytes(),
                            );
                            response.extend_from_slice(&(stored.len() as u32).to_le_bytes());
                            response.extend_from_slice(&0u32.to_le_bytes());
                            response
                        }
                        b"SEND" => {
                            ensure!(
                                value.starts_with(b"/sdcard/")
                                    || self.mirror_protocol.load(Ordering::Acquire)
                                        && value.starts_with(b"/data/local/tmp/scrcpy-server.jar,"),
                                "wrong fake path"
                            );
                            file.clear();
                            Vec::new()
                        }
                        b"DATA" => {
                            file.extend(value);
                            Vec::new()
                        }
                        b"DONE" => {
                            *self.file.lock().unwrap() = std::mem::take(&mut file);
                            b"OKAY\0\0\0\0".to_vec()
                        }
                        b"RECV" => {
                            let stored = self.file.lock().unwrap();
                            let mut response = Vec::new();
                            for chunk in stored.chunks(65536) {
                                response.extend_from_slice(b"DATA");
                                response.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
                                response.extend_from_slice(chunk);
                            }
                            response.extend_from_slice(b"DONE\0\0\0\0");
                            response
                        }
                        b"QUIT" => return Ok(()),
                        _ => bail!("unsupported fake sync frame: {kind:?}"),
                    };
                    for chunk in response.chunks(shellrs_lan::DEVICE_CHUNK) {
                        events.blocking_send(DeviceEvent::Output(chunk.to_vec()))?;
                    }
                }
            }
            Ok(())
        }
    }

    struct Fixture {
        proxy: Arc<AdbProxy>,
        controller: Arc<Core>,
        owner: Arc<Core>,
        backend: Arc<Backend>,
        _networks: (Network, Network),
        _directories: (tempfile::TempDir, tempfile::TempDir),
    }
    fn fixture() -> Fixture {
        let directories = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let backend = Arc::new(Backend::default());
        let controller = Core::load(directories.0.path(), "A", backend.clone()).unwrap();
        let owner = Core::load(directories.1.path(), "B", backend.clone()).unwrap();
        controller.set_password("test A password").unwrap();
        owner.set_password("test B password").unwrap();
        owner.set_device_provider(backend.clone()).unwrap();
        owner
            .set_device_sharing(["board".to_owned()].into(), true)
            .unwrap();
        let networks = (
            Network::start(controller.clone(), std::net::Ipv4Addr::LOCALHOST, 0, 0).unwrap(),
            Network::start(owner.clone(), std::net::Ipv4Addr::LOCALHOST, 0, 0).unwrap(),
        );
        controller
            .pair_password(
                controller.inspect(networks.1.tcp_address).unwrap(),
                "test B password",
            )
            .unwrap();
        let proxy = AdbProxy::start(
            controller.clone(),
            owner.id().into(),
            "board".into(),
            "board".into(),
        )
        .unwrap();
        Fixture {
            proxy,
            controller,
            owner,
            backend,
            _networks: networks,
            _directories: directories,
        }
    }
    fn socket(fixture: &Fixture) -> TcpStream {
        let stream = TcpStream::connect(fixture.proxy.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
    }
    fn send(stream: &mut TcpStream, message: &str) {
        stream
            .write_all(format!("{:04x}{message}", message.len()).as_bytes())
            .unwrap();
    }
    fn status(stream: &mut TcpStream) -> [u8; 4] {
        let mut status = [0; 4];
        stream.read_exact(&mut status).unwrap();
        status
    }

    #[test]
    fn only_device_services_and_named_scrcpy_sockets_are_accepted() {
        for service in [
            "sync:",
            "shell:ls /sdcard",
            "shell,v2,raw:cmd package install",
            "shell,v2,TERM=xterm-256color,raw:ls",
            "exec:cat /sdcard/a",
            "localabstract:scrcpy_1234abcd",
        ] {
            assert!(
                shellrs_lan::validate_adb_service(service).is_ok(),
                "{service}"
            );
        }
        for service in [
            "host:kill",
            "host:transport:other",
            "host:connect:192.168.1.1",
            "tcp:5037",
            "localfilesystem:/tmp/test",
            "localabstract:other",
            "localabstract:scrcpy_bad",
            "sync:\0",
        ] {
            assert!(
                shellrs_lan::validate_adb_service(service).is_err(),
                "{service}"
            );
        }
    }

    #[test]
    fn smart_socket_rejects_other_devices_host_files_and_arbitrary_forwards() {
        let fixture = fixture();
        for message in [
            "host:transport:private-board",
            "host:tport:serial:private-board",
            "host:kill",
            "host:connect:10.0.0.1",
            "host-serial:private-board:get-state",
        ] {
            let mut stream = socket(&fixture);
            send(&mut stream, message);
            assert_eq!(status(&mut stream), *b"FAIL");
        }
        for service in [
            "tcp:5037",
            "localfilesystem:/tmp/file",
            "host:transport:private-board",
        ] {
            let mut stream = socket(&fixture);
            send(&mut stream, "host:transport:board");
            assert_eq!(status(&mut stream), *b"OKAY");
            send(&mut stream, service);
            if service.starts_with("host:") {
                assert_eq!(status(&mut stream), *b"FAIL");
            } else {
                let mut bytes = [0; 4];
                assert!(stream.read(&mut bytes).is_ok_and(|count| count == 0));
            }
        }
        assert!(fixture.backend.services.lock().unwrap().is_empty());
    }

    #[test]
    fn encrypted_sync_stream_keeps_binary_chunks_and_stops_on_revocation() {
        let fixture = fixture();
        let mut stream = socket(&fixture);
        send(&mut stream, "host:tport:serial:board");
        assert_eq!(status(&mut stream), *b"OKAY");
        let mut id = [0; 8];
        stream.read_exact(&mut id).unwrap();
        assert_eq!(u64::from_le_bytes(id), 1);
        send(&mut stream, "sync:");
        assert_eq!(status(&mut stream), *b"OKAY");
        let bytes: Vec<_> = (0..100000).map(|index| (index % 256) as u8).collect();
        stream.write_all(&bytes).unwrap();
        let mut echoed = vec![0; bytes.len()];
        stream.read_exact(&mut echoed).unwrap();
        assert_eq!(echoed, bytes);
        fixture
            .owner
            .revoke_device_access(fixture.controller.id())
            .unwrap();
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !fixture.proxy.is_stopped() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(fixture.proxy.is_stopped());
    }

    #[test]
    fn large_binary_streams_exceed_the_channel_event_queue_without_stalling() {
        let fixture = fixture();
        let mut stream = socket(&fixture);
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        send(&mut stream, "host:transport:board");
        assert_eq!(status(&mut stream), *b"OKAY");
        send(&mut stream, "sync:");
        assert_eq!(status(&mut stream), *b"OKAY");
        let payload: Vec<_> = (0..3 * 1024 * 1024)
            .map(|offset| (offset % 251) as u8)
            .collect();
        let mut writer = stream.try_clone().unwrap();
        thread::scope(|scope| {
            let sending = scope.spawn(|| writer.write_all(&payload));
            let mut received = vec![0; payload.len()];
            stream.read_exact(&mut received).unwrap();
            sending.join().unwrap().unwrap();
            assert_eq!(received, payload);
        });
    }

    #[test]
    fn scrcpy_forward_is_local_and_existing_connections_survive_forward_removal() {
        let fixture = fixture();
        let mut control = socket(&fixture);
        send(
            &mut control,
            "host-serial:board:forward:tcp:0;localabstract:scrcpy_1234abcd",
        );
        assert_eq!(status(&mut control), *b"OKAY");
        assert_eq!(status(&mut control), *b"OKAY");
        let length =
            usize::from_str_radix(std::str::from_utf8(&status(&mut control)).unwrap(), 16).unwrap();
        let mut port = vec![0; length];
        control.read_exact(&mut port).unwrap();
        let port: u16 = std::str::from_utf8(&port).unwrap().parse().unwrap();
        let mut video = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
        video
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut header = [0; 12];
        video.read_exact(&mut header).unwrap();
        assert_eq!(&header, b"video-header");
        let mut control = socket(&fixture);
        send(
            &mut control,
            &format!("host-serial:board:killforward:tcp:{port}"),
        );
        assert_eq!(status(&mut control), *b"OKAY");
        assert_eq!(status(&mut control), *b"OKAY");
        video.write_all(b"control-input").unwrap();
        let mut echoed = [0; 13];
        video.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"control-input");
        drop(video);
        assert_eq!(
            *fixture.backend.services.lock().unwrap(),
            ["localabstract:scrcpy_1234abcd"]
        );
    }

    #[test]
    fn closing_a_proxy_releases_its_listener_and_active_device_stream() {
        let fixture = fixture();
        let proxy = AdbProxy::start(
            fixture.controller.clone(),
            fixture.owner.id().into(),
            "board".into(),
            "board".into(),
        )
        .unwrap();
        let address = proxy.address;
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        send(&mut stream, "host:transport:board");
        assert_eq!(status(&mut stream), *b"OKAY");
        send(&mut stream, "sync:");
        assert_eq!(status(&mut stream), *b"OKAY");
        drop(proxy);
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !fixture.owner.active_devices().is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(fixture.owner.active_devices().is_empty());
        assert!(TcpListener::bind(address).is_ok());
    }

    #[test]
    #[ignore = "requires bundled native scrcpy and ADB; isolated fake LAN devices only"]
    fn native_scrcpy_reaches_the_remote_video_socket() {
        let fixture = fixture();
        fixture.backend.file_protocol.store(true, Ordering::Release);
        fixture
            .backend
            .mirror_protocol
            .store(true, Ordering::Release);
        let connection = super::super::AdbConnection::remote(
            "board".into(),
            "LAN test".into(),
            fixture.proxy.clone(),
        )
        .unwrap();
        let executable =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("dist/scrcpy/scrcpy.exe");
        let temporary = tempfile::tempdir().unwrap();
        let log = std::fs::File::create(temporary.path().join("scrcpy.log")).unwrap();
        let mut command = super::super::mirror::mirror_command(
            &connection,
            &executable,
            &connection.program().unwrap(),
            false,
        );
        command
            .args([
                "--no-control",
                "--no-video-playback",
                "--time-limit=1",
                "--record-format=mkv",
            ])
            .arg(format!(
                "--record={}",
                temporary.path().join("test.mkv").display()
            ))
            .stdout(std::process::Stdio::from(log.try_clone().unwrap()))
            .stderr(std::process::Stdio::from(log));
        let (mut process, group) =
            super::super::process::CommandGroup::spawn(&mut command).unwrap();
        group.terminate_on_close().unwrap();
        let deadline = Instant::now() + Duration::from_secs(12);
        let reached = loop {
            if fixture
                .backend
                .services
                .lock()
                .unwrap()
                .iter()
                .any(|service| service.starts_with("localabstract:scrcpy_"))
            {
                break true;
            }
            if process.try_wait().unwrap().is_some() || Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(20));
        };
        group.terminate();
        let _ = process.kill();
        let _ = process.wait();
        let output = std::fs::read_to_string(temporary.path().join("scrcpy.log")).unwrap();
        assert!(
            reached,
            "scrcpy did not reach video socket: {output}; services: {:?}",
            fixture.backend.services.lock().unwrap()
        );
    }

    #[test]
    #[ignore = "requires an installed ADB client; only connects to isolated fake LAN devices"]
    fn installed_adb_accepts_fixed_port_forwarding_used_by_scrcpy() {
        let fixture = fixture();
        let connection = super::super::AdbConnection::remote(
            "board".into(),
            "LAN test".into(),
            fixture.proxy.clone(),
        )
        .unwrap();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let local = format!("tcp:{port}");
        let output = connection
            .run(
                &[
                    "forward".as_ref(),
                    local.as_ref(),
                    "localabstract:scrcpy_1234abcd".as_ref(),
                ],
                Duration::from_secs(5),
                &mut || false,
            )
            .unwrap();
        assert!(output.is_empty());
        let mut video = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
        video
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = [0; 12];
        video.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"video-header");
    }

    #[test]
    #[ignore = "requires an installed ADB client; only connects to isolated fake LAN devices"]
    fn installed_adb_client_uses_the_isolated_server_for_state_shell_and_forwarding() {
        let fixture = fixture();
        let connection = super::super::AdbConnection::remote(
            "board".into(),
            "LAN test".into(),
            fixture.proxy.clone(),
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let command = super::super::mirror::mirror_command(
            &connection,
            std::path::Path::new("scrcpy.exe"),
            &connection.program().unwrap(),
            false,
        );
        assert!(
            command
                .get_args()
                .any(|argument| argument == "--force-adb-forward")
        );
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == "ADB_SERVER_SOCKET"
                    && value == Some(std::ffi::OsStr::new(&fixture.proxy.socket())))
        );
        connection.verify(&mut || false).unwrap();
        let devices = connection
            .run(
                &[std::ffi::OsStr::new("devices"), std::ffi::OsStr::new("-l")],
                Duration::from_secs(10),
                &mut || false,
            )
            .unwrap();
        let devices = String::from_utf8(devices).unwrap();
        assert!(devices.contains("board"));
        assert!(!devices.contains("private-board"));
        let output = connection
            .run(
                &[
                    std::ffi::OsStr::new("shell"),
                    std::ffi::OsStr::new("-T"),
                    std::ffi::OsStr::new("echo shelllf-lan-check"),
                ],
                Duration::from_secs(10),
                &mut || false,
            )
            .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap().replace("\r\n", "\n"),
            "shelllf-lan-check\n"
        );
        let output = connection
            .run(
                &[
                    std::ffi::OsStr::new("forward"),
                    std::ffi::OsStr::new("tcp:0"),
                    std::ffi::OsStr::new("localabstract:scrcpy_1234abcd"),
                ],
                Duration::from_secs(10),
                &mut || cancel.load(Ordering::Acquire),
            )
            .unwrap();
        let port: u16 = std::str::from_utf8(&output)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let mut video = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
        video
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut header = [0; 12];
        video.read_exact(&mut header).unwrap();
        assert_eq!(&header, b"video-header");
        connection
            .run(
                &[
                    std::ffi::OsStr::new("forward"),
                    std::ffi::OsStr::new("--remove"),
                    std::ffi::OsStr::new(&format!("tcp:{port}")),
                ],
                Duration::from_secs(10),
                &mut || false,
            )
            .unwrap();
        drop(video);
    }

    #[test]
    #[ignore = "requires an installed ADB client; no physical device or real APK installation"]
    fn installed_adb_streams_apk_and_transfers_binary_files_through_encrypted_lan() {
        use super::super::{AndroidTools as _, SystemAndroidTools};
        use std::ffi::OsStr;
        let fixture = fixture();
        fixture.backend.file_protocol.store(true, Ordering::Release);
        let connection = super::super::AdbConnection::remote(
            "board".into(),
            "LAN test".into(),
            fixture.proxy.clone(),
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let apk = directory.path().join("test app.apk");
        std::fs::write(&apk, b"isolated apk payload").unwrap();
        SystemAndroidTools
            .install(&connection, &apk, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(
            *fixture.backend.apk.lock().unwrap(),
            b"isolated apk payload"
        );
        fixture.backend.install_fails.store(true, Ordering::Release);
        assert!(
            SystemAndroidTools
                .install(&connection, &apk, &AtomicBool::new(false))
                .is_err()
        );
        let bytes: Vec<_> = (0..200000).map(|index| (index % 256) as u8).collect();
        let source = directory.path().join("binary source.bin");
        let target = directory.path().join("download.bin");
        std::fs::write(&source, &bytes).unwrap();
        connection
            .run(
                &[
                    OsStr::new("push"),
                    source.as_os_str(),
                    OsStr::new("/sdcard/test.bin"),
                ],
                Duration::from_secs(15),
                &mut || false,
            )
            .unwrap();
        assert_eq!(*fixture.backend.file.lock().unwrap(), bytes);
        connection
            .run(
                &[
                    OsStr::new("pull"),
                    OsStr::new("/sdcard/test.bin"),
                    target.as_os_str(),
                ],
                Duration::from_secs(15),
                &mut || false,
            )
            .unwrap();
        assert_eq!(std::fs::read(target).unwrap(), bytes);
    }
}
