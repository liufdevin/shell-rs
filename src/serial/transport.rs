use std::collections::VecDeque;
use std::io::{self, ErrorKind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use async_channel::Sender;
use gpui_kit::SharedString;
use serial2::SerialPort;

use crate::host::{DataBits, FlowControl, Parity, SerialLine, StopBits};
use crate::i18n::t;
use crate::terminal::{
    SerialTerminalTransportProvider, SharedTerminalTransportFactory, TerminalSize,
    TerminalTransport, TerminalTransportCommand, TerminalTransportEvent, TerminalTransportFactory,
    send_event,
};

/// How long a read waits before the reader looks whether to stop.
const READ_TIMEOUT: Duration = Duration::from_millis(200);

/// How long a write may wait for the line: long enough for a paste at a low
/// rate, or for the other end to let go of RTS/CTS for a moment.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long 发送 Break holds the line, within what consoles look for.
const BREAK_LENGTH: Duration = Duration::from_millis(300);

enum SerialRead {
    Output(Vec<u8>),
    Lost(String),
}

/// Serial hosts' terminals on the ports of this computer.
#[derive(Default)]
pub struct SerialTransportProvider;

impl SerialTerminalTransportProvider for SerialTransportProvider {
    fn factory_for(&self, line: &SerialLine) -> SharedTerminalTransportFactory {
        Arc::new(SerialTransportFactory { line: line.clone() })
    }
}

struct SerialTransportFactory {
    line: SerialLine,
}

impl TerminalTransportFactory for SerialTransportFactory {
    fn create(&self) -> Box<dyn TerminalTransport> {
        Box::new(SerialTransport {
            line: self.line.clone(),
            encoding: crate::host::TerminalEncoding::Utf8,
        })
    }
}

struct SerialTransport {
    line: SerialLine,
    encoding: crate::host::TerminalEncoding,
}

impl TerminalTransport for SerialTransport {
    fn set_encoding(&mut self, encoding: crate::host::TerminalEncoding) {
        self.encoding = encoding;
    }
    fn run(
        self: Box<Self>,
        _: TerminalSize,
        commands: mpsc::Receiver<TerminalTransportCommand>,
        events: Sender<TerminalTransportEvent>,
    ) -> Result<()> {
        let device = self.line.device.as_str();
        let port = open(&self.line).map_err(|error| anyhow!(open_error(device, &error)))?;
        serve_shell(
            Arc::new(port),
            device,
            commands,
            &events,
            crate::terminal::ShellQuery::new(self.line.settings.linux_shell, self.encoding),
            self.line.settings.baud,
        )
    }
}

/// Whether `line` opens, as the host form's 测试连接 asks: open it as its
/// terminal would and close it again, or why it would not.
pub fn check(line: &SerialLine) -> Result<(), String> {
    open(line)
        .map(drop)
        .map_err(|error| open_error(&line.device, &error).to_string())
}

/// Open `line`'s device and set the line up. On Unix the port is then held
/// exclusively, as screen and minicom hold it: a second terminal, or another
/// program, is told the port is taken rather than share its bytes.
///
/// macOS sets the rate with an ioctl that only a real port takes, so a
/// pseudo-terminal posing as one (socat's) does not open there.
pub(crate) fn open(line: &SerialLine) -> io::Result<SerialPort> {
    let settings = line.settings;
    let port = SerialPort::open(&line.device, move |mut port: serial2::Settings| {
        port.set_raw();
        port.set_baud_rate(settings.baud)?;
        port.set_char_size(char_size(settings.data_bits));
        port.set_parity(parity(settings.parity));
        port.set_stop_bits(stop_bits(settings.stop_bits));
        port.set_flow_control(flow_control(settings.flow));
        Ok(port)
    })?;
    hold(port)
}

/// Hold an open port for this terminal alone, and set how long its reads
/// and writes wait.
fn hold(mut port: SerialPort) -> io::Result<SerialPort> {
    #[cfg(unix)]
    hold_exclusively(&port)?;
    port.set_read_timeout(READ_TIMEOUT)?;
    port.set_write_timeout(WRITE_TIMEOUT)?;
    Ok(port)
}

#[cfg(unix)]
fn hold_exclusively(port: &SerialPort) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: TIOCEXCL takes no argument, and the descriptor is the port's
    // own for as long as the call.
    if unsafe { libc::ioctl(port.as_raw_fd(), libc::TIOCEXCL as _) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Why `device` would not open, as the terminal says it.
fn open_error(device: &str, error: &io::Error) -> SharedString {
    match error.kind() {
        ErrorKind::NotFound => t!("serial.open.not_found", device = device),
        ErrorKind::ResourceBusy => t!("serial.open.busy", device = device),
        // Windows tells a program that another one has the port open that
        // it may not open it.
        ErrorKind::PermissionDenied if cfg!(windows) => t!("serial.open.busy", device = device),
        ErrorKind::PermissionDenied if cfg!(target_os = "linux") => {
            t!("serial.open.denied_linux", device = device)
        }
        ErrorKind::PermissionDenied => t!("serial.open.denied", device = device),
        _ => t!("serial.open.failed", device = device, error = error),
    }
}

/// Carry the bytes between the terminal and `port` until the terminal shuts
/// down or the port goes away, say with its USB cable.
#[cfg(all(test, unix))]
fn serve(
    port: Arc<SerialPort>,
    device: &str,
    commands: mpsc::Receiver<TerminalTransportCommand>,
    events: &Sender<TerminalTransportEvent>,
) -> Result<()> {
    serve_shell(
        port,
        device,
        commands,
        events,
        crate::terminal::ShellQuery::new(false, crate::host::TerminalEncoding::Utf8),
        115200,
    )
}

fn serve_shell(
    port: Arc<SerialPort>,
    device: &str,
    commands: mpsc::Receiver<TerminalTransportCommand>,
    events: &Sender<TerminalTransportEvent>,
    mut shell: crate::terminal::ShellQuery,
    mut baud: u32,
) -> Result<()> {
    // Set by the reader when the port is gone, and by the command loop when
    // the terminal shuts down.
    let stop = Arc::new(AtomicBool::new(false));
    let (raw, received) = mpsc::sync_channel(32);
    let reader = {
        let port = port.clone();
        let stop = stop.clone();
        let device = device.to_owned();
        thread::Builder::new()
            .name("shellrs-serial-reader".into())
            .spawn(move || read(&port, &device, &stop, raw))
            .context(t!("serial.reader_thread"))?
    };

    send_event(events, TerminalTransportEvent::Started);
    let mut lines = VecDeque::new();
    let mut next_line = Instant::now();
    let mut pending_input = Vec::new();
    let result = (|| -> Result<()> {
        while !stop.load(Ordering::Acquire) {
            while let Ok(event) = received.try_recv() {
                match event {
                    SerialRead::Output(bytes) => {
                        let visible = shell.output(&bytes);
                        if !visible.is_empty()
                            && !send_event(events, TerminalTransportEvent::Output(visible))
                        {
                            stop.store(true, Ordering::Release);
                            break;
                        }
                    }
                    SerialRead::Lost(error) => {
                        send_event(events, TerminalTransportEvent::Failed(error));
                        stop.store(true, Ordering::Release);
                        break;
                    }
                }
            }
            if stop.load(Ordering::Acquire) {
                break;
            }
            shell.tick();
            if !shell.busy() {
                lines.clear();
                if !pending_input.is_empty() {
                    shell.input(&pending_input);
                    port.write_all(&pending_input)?;
                    pending_input.clear();
                }
            }
            if let Some(script) = shell.poll() {
                lines.extend(script);
                next_line = Instant::now();
            }
            if Instant::now() >= next_line
                && let Some(line) = lines.pop_front()
            {
                let line: Vec<u8> = line;
                port.write_all(&line)?;
                next_line = Instant::now()
                    + Duration::from_millis(
                        (line.len() as u64 * 12000 / u64::from(baud.max(1))).max(100),
                    );
            }
            match commands.recv_timeout(Duration::from_millis(20)) {
                Ok(TerminalTransportCommand::SetSerialBaud {
                    baud: requested,
                    reply,
                }) => {
                    let result = (|| -> io::Result<()> {
                        if requested == 0 || shell.busy() {
                            return Err(io::Error::other(
                                t!("serial.tool.unavailable").to_string(),
                            ));
                        }
                        let previous = port.get_configuration()?;
                        let mut settings = previous.clone();
                        settings.set_baud_rate(requested)?;
                        let mut handle = port.try_clone()?;
                        handle.set_configuration(&settings)?;
                        if handle.get_configuration()?.get_baud_rate()? != requested {
                            handle.set_configuration(&previous)?;
                            return Err(io::Error::other(
                                t!("serial.tool.invalid_baud").to_string(),
                            ));
                        }
                        baud = requested;
                        Ok(())
                    })();
                    let _ = reply.send(result.map_err(|error| error.to_string()));
                }
                Ok(TerminalTransportCommand::WriteData(bytes)) => {
                    port.write_all(&bytes)?;
                }
                Ok(TerminalTransportCommand::Write(bytes)) => {
                    if bytes.contains(&3) {
                        pending_input.clear();
                    }
                    if !shell.input(&bytes) {
                        pending_input.extend_from_slice(&bytes);
                        continue;
                    }
                    if let Err(error) = port.write_all(&bytes)
                        && !stop.swap(true, Ordering::AcqRel)
                    {
                        send_event(
                            events,
                            TerminalTransportEvent::Failed(
                                t!("serial.write_error", device = device, error = error).into(),
                            ),
                        );
                    }
                }
                Ok(TerminalTransportCommand::Exec(request)) => {
                    if let Some(script) = shell.execute(request) {
                        lines.extend(script);
                        next_line = Instant::now();
                    }
                }
                Ok(TerminalTransportCommand::Monitor(request)) => {
                    if let Some(script) = shell.monitor(request) {
                        lines.extend(script);
                        next_line = Instant::now();
                    }
                }
                Ok(TerminalTransportCommand::SerialTransfer(request)) => {
                    if let Some(command) = shell.transfer(request) {
                        port.write_all(&command)?;
                    }
                }
                Ok(TerminalTransportCommand::SendBreak) => send_break(&port),
                Ok(TerminalTransportCommand::Shutdown)
                | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    stop.store(true, Ordering::Release);
                }
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        Ok(())
    })();
    stop.store(true, Ordering::Release);
    drop(received);
    let _ = reader.join();
    result
}

/// Hand what comes in to the terminal until `stop`, or until the port goes
/// away, which ends the terminal with why.
fn read(port: &SerialPort, device: &str, stop: &AtomicBool, raw: mpsc::SyncSender<SerialRead>) {
    let mut buffer = [0_u8; 8192];
    while !stop.load(Ordering::Acquire) {
        let lost = match port.read(&mut buffer) {
            Ok(0) => t!("serial.lost", device = device),
            Ok(count) => {
                if raw
                    .send(SerialRead::Output(buffer[..count].to_vec()))
                    .is_err()
                {
                    break;
                }
                continue;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::TimedOut | ErrorKind::WouldBlock | ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(error) => t!("serial.lost_error", device = device, error = error),
        };
        // Said once, unless the terminal was shutting down anyway.
        if !stop.load(Ordering::Acquire) {
            let _ = raw.send(SerialRead::Lost(lost.into()));
        }
        break;
    }
}

/// Hold the line in the break condition for [`BREAK_LENGTH`]. A port that
/// cannot (a pseudo-terminal) carries on as it was.
fn send_break(port: &SerialPort) {
    if port.set_break(true).is_ok() {
        thread::sleep(BREAK_LENGTH);
        let _ = port.set_break(false);
    }
}

fn char_size(bits: DataBits) -> serial2::CharSize {
    match bits {
        DataBits::Five => serial2::CharSize::Bits5,
        DataBits::Six => serial2::CharSize::Bits6,
        DataBits::Seven => serial2::CharSize::Bits7,
        DataBits::Eight => serial2::CharSize::Bits8,
    }
}

fn parity(parity: Parity) -> serial2::Parity {
    match parity {
        Parity::None => serial2::Parity::None,
        Parity::Odd => serial2::Parity::Odd,
        Parity::Even => serial2::Parity::Even,
    }
}

fn stop_bits(bits: StopBits) -> serial2::StopBits {
    match bits {
        StopBits::One => serial2::StopBits::One,
        StopBits::Two => serial2::StopBits::Two,
    }
}

fn flow_control(flow: FlowControl) -> serial2::FlowControl {
    match flow {
        FlowControl::None => serial2::FlowControl::None,
        FlowControl::RtsCts => serial2::FlowControl::RtsCts,
        FlowControl::XonXoff => serial2::FlowControl::XonXoff,
    }
}

/// The other end of the line is a pseudo-terminal: its slave is opened by
/// name like a real device, and the test reads and writes its master. A
/// pseudo-terminal has no rate to set (macOS refuses), so the tests open it
/// as it is and hold it as [`open`] does.
#[cfg(all(test, unix))]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::time::Instant;

    use portable_pty::{MasterPty, PtySize, native_pty_system};

    use super::*;
    use crate::host::SerialSettings;

    struct Line {
        master: Box<dyn MasterPty + Send>,
        device: String,
    }

    fn line() -> Line {
        let pair = native_pty_system()
            .openpty(PtySize::default())
            .expect("pty opens");
        let device = pair
            .master
            .tty_name()
            .expect("pty has a name")
            .to_string_lossy()
            .into_owned();
        // Only the transport keeps the device open, as with a real port.
        drop(pair.slave);
        Line {
            master: pair.master,
            device,
        }
    }

    struct Running {
        commands: mpsc::Sender<TerminalTransportCommand>,
        events: async_channel::Receiver<TerminalTransportEvent>,
        worker: thread::JoinHandle<Result<()>>,
    }

    /// `device` held as [`open`] holds a port, raw as [`open`] makes it
    /// but at whatever rate it has.
    fn open_as_is(device: &str) -> io::Result<SerialPort> {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
            .open(device)?;
        // SAFETY: the termios is filled in by `tcgetattr` before it is
        // read, and the descriptor is the file's own.
        unsafe {
            let mut termios = std::mem::zeroed();
            if libc::tcgetattr(file.as_raw_fd(), &mut termios) == 0 {
                libc::cfmakeraw(&mut termios);
                libc::tcsetattr(file.as_raw_fd(), libc::TCSANOW, &termios);
            }
        }
        hold(SerialPort::from(std::os::fd::OwnedFd::from(file)))
    }

    /// The transport's loop on `device`.
    fn serve_line(device: &str) -> Running {
        let port = open_as_is(device).expect("the line opens");
        let (commands, command_receiver) = mpsc::channel();
        let (events, event_receiver) = async_channel::unbounded();
        let device = device.to_owned();
        let worker =
            thread::spawn(move || serve(Arc::new(port), &device, command_receiver, &events));
        Running {
            commands,
            events: event_receiver,
            worker,
        }
    }

    /// The next event other than output.
    fn next_event(running: &Running) -> Option<TerminalTransportEvent> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match running.events.try_recv() {
                Ok(TerminalTransportEvent::Output(_)) => {}
                Ok(event) => return Some(event),
                Err(async_channel::TryRecvError::Empty) => thread::sleep(Duration::from_millis(5)),
                Err(async_channel::TryRecvError::Closed) => return None,
            }
        }
        None
    }

    fn output_until(running: &Running, expected: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        while Instant::now() < deadline && !String::from_utf8_lossy(&output).contains(expected) {
            match running.events.try_recv() {
                Ok(TerminalTransportEvent::Output(bytes)) => output.extend(bytes),
                Ok(_) | Err(async_channel::TryRecvError::Empty) => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(async_channel::TryRecvError::Closed) => break,
            }
        }
        String::from_utf8_lossy(&output).into_owned()
    }

    #[test]
    fn bytes_go_both_ways_until_shutdown() {
        let line = line();
        let running = serve_line(&line.device);
        assert_eq!(next_event(&running), Some(TerminalTransportEvent::Started));

        let mut writer = line.master.take_writer().unwrap();
        writer.write_all(b"login: ").unwrap();
        writer.flush().unwrap();
        assert!(output_until(&running, "login: ").contains("login: "));

        let mut reader = line.master.try_clone_reader().unwrap();
        running
            .commands
            .send(TerminalTransportCommand::SendBreak)
            .unwrap();
        running
            .commands
            .send(TerminalTransportCommand::Write(b"root\r".to_vec()))
            .unwrap();
        let mut typed = [0_u8; 5];
        reader.read_exact(&mut typed).unwrap();
        assert_eq!(&typed, b"root\r");

        let started = Instant::now();
        running
            .commands
            .send(TerminalTransportCommand::Shutdown)
            .unwrap();
        running.worker.join().unwrap().unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// macOS keeps a real port to its holder, but not a pseudo-terminal.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_second_terminal_finds_the_port_taken() {
        let line = line();
        let _first = open_as_is(&line.device).unwrap();
        let error = open_as_is(&line.device).unwrap_err();
        assert_eq!(
            open_error(&line.device, &error),
            format!("串口 {} 已被占用：另一个标签或程序正在用它", line.device)
        );
    }

    #[test]
    fn the_port_going_away_ends_the_terminal_with_why() {
        let line = line();
        let running = serve_line(&line.device);
        assert_eq!(next_event(&running), Some(TerminalTransportEvent::Started));

        drop(line.master);
        match next_event(&running) {
            Some(TerminalTransportEvent::Failed(message)) => {
                assert!(message.contains("已断开"), "{message}");
            }
            other => panic!("the terminal was told {other:?}"),
        }
        running.worker.join().unwrap().unwrap();
    }

    #[test]
    fn a_test_says_why_a_line_would_not_open() {
        let line = SerialLine {
            device: "/dev/no-such-serial-port".into(),
            settings: SerialSettings::default(),
        };
        assert_eq!(
            check(&line),
            Err("找不到串口设备 /dev/no-such-serial-port".to_string())
        );
    }

    #[test]
    fn a_device_that_is_not_there_says_so() {
        let (_commands, command_receiver) = mpsc::channel();
        let (events, _event_receiver) = async_channel::unbounded();
        let error = SerialTransportProvider
            .factory_for(&SerialLine {
                device: "/dev/no-such-serial-port".into(),
                settings: SerialSettings::default(),
            })
            .create()
            .run(TerminalSize::DEFAULT, command_receiver, events)
            .unwrap_err()
            .to_string();
        assert_eq!(error, "找不到串口设备 /dev/no-such-serial-port");
    }
}
