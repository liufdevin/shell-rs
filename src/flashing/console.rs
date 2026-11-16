use std::{
    io::{Read as _, Write as _},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use super::process::{RunEvent, decode};

struct ConsoleLines {
    pending: Vec<u8>,
    sender: mpsc::SyncSender<RunEvent>,
    queries: mpsc::Sender<()>,
    connected: bool,
    escape: ParserState,
}

#[derive(Default)]
enum ParserState {
    #[default]
    Text,
    Start,
    Control(Vec<u8>),
    Osc,
    OscEnd,
}

impl ConsoleLines {
    fn flush(&mut self) {
        let bytes = std::mem::take(&mut self.pending);
        let text = decode(&bytes).trim_end_matches('\0').to_string();
        if !text.trim().is_empty() && self.connected {
            self.connected = self
                .sender
                .send(RunEvent::Line {
                    at: Instant::now(),
                    text,
                })
                .is_ok();
        }
    }

    fn advance(&mut self, bytes: &[u8]) {
        for byte in bytes {
            let byte = *byte;
            match std::mem::take(&mut self.escape) {
                ParserState::Text if byte == 0x1b => self.escape = ParserState::Start,
                ParserState::Text if matches!(byte, b'\r' | b'\n') => self.flush(),
                ParserState::Text if byte == 0x08 => {
                    if self.pending.last().is_some_and(|value| value.is_ascii()) {
                        self.pending.pop();
                    }
                }
                ParserState::Text if byte == b'\t' => {
                    if self.pending.len() < 8192 {
                        self.pending.push(byte);
                    }
                }
                ParserState::Text if byte >= 0x20 && byte != 0x7f => {
                    if self.pending.len() < 8192 {
                        self.pending.push(byte);
                    }
                }
                ParserState::Text => {}
                ParserState::Start if byte == b'[' => {
                    self.escape = ParserState::Control(Vec::new())
                }
                ParserState::Start if matches!(byte, b']' | b'P' | b'X' | b'^' | b'_') => {
                    self.escape = ParserState::Osc;
                }
                ParserState::Start => {}
                ParserState::Control(mut parameters) => {
                    if (0x40..=0x7e).contains(&byte) {
                        if byte == b'n' && parameters == b"6" {
                            let _ = self.queries.send(());
                        } else if matches!(byte, b'H' | b'f' | b'J')
                            || (byte == b'K' && parameters.contains(&b'2'))
                        {
                            self.flush();
                        }
                    } else {
                        if parameters.len() < 64 {
                            parameters.push(byte);
                        }
                        self.escape = ParserState::Control(parameters);
                    }
                }
                ParserState::Osc if byte == 0x07 => {}
                ParserState::Osc if byte == 0x1b => self.escape = ParserState::OscEnd,
                ParserState::Osc => self.escape = ParserState::Osc,
                ParserState::OscEnd if byte == b'\\' => {}
                ParserState::OscEnd if byte == 0x1b => self.escape = ParserState::OscEnd,
                ParserState::OscEnd => self.escape = ParserState::Osc,
            }
        }
    }
}

pub(super) fn run_console(
    command: CommandBuilder,
    stop: &AtomicBool,
    sender: mpsc::SyncSender<RunEvent>,
) -> Result<(Option<i32>, bool), String> {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 64,
            cols: 4096,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| error.to_string())?;
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| error.to_string())?;
    let mut writer = pair
        .master
        .take_writer()
        .map_err(|error| error.to_string())?;
    if stop.load(Ordering::Relaxed) {
        return Ok((None, true));
    }
    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| error.to_string())?;
    drop(pair.slave);
    #[cfg(windows)]
    let group = match child
        .as_raw_handle()
        .ok_or_else(|| "Missing console process handle".to_string())
        .and_then(|handle| {
            crate::shared::process::CommandGroup::for_handle(handle)
                .map_err(|error| error.to_string())
        }) {
        Ok(group) => group,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let (queries, requests) = mpsc::channel();
    let output = std::thread::spawn(move || {
        let mut lines = ConsoleLines {
            pending: Vec::new(),
            sender,
            queries,
            connected: true,
            escape: ParserState::Text,
        };
        let mut buffer = [0u8; 4096];
        while lines.connected {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => lines.advance(&buffer[..count]),
            }
        }
        lines.flush();
    });
    let result = loop {
        if stop.load(Ordering::Relaxed) {
            #[cfg(windows)]
            group.terminate();
            let _ = child.kill();
            break child
                .wait()
                .map(|status| (Some(status.exit_code() as i32), true))
                .map_err(|error| error.to_string());
        }
        for _ in requests.try_iter() {
            let _ = writer.write_all(b"\x1b[1;1R").and_then(|_| writer.flush());
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok((Some(status.exit_code() as i32), false)),
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(error.to_string());
            }
        }
    };
    #[cfg(windows)]
    group.terminate();
    drop(writer);
    drop(pair.master);
    let _ = output.join();
    result
}

pub(super) fn command(
    program: &Path,
    tool_dir: &Path,
    arguments: &[std::ffi::OsString],
) -> CommandBuilder {
    let mut command = CommandBuilder::new(program);
    command.cwd(tool_dir);
    command.args(arguments);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_escape_sequences_preserve_progress_and_device_results() {
        let (sender, receiver) = mpsc::sync_channel(32);
        let (queries, requests) = mpsc::channel();
        let mut lines = ConsoleLines {
            pending: Vec::new(),
            sender,
            queries,
            connected: true,
            escape: ParserState::Text,
        };
        for chunk in [
            b"\x1b[6n\x1b[32mline[2141], device:[3] fes plugin\x1b[0m\r\n".as_slice(),
            b"line[2153], device [3] progress = 12%\r",
            b"\nline[2172], device:[3] burn finish, result=Success\r\n",
        ] {
            lines.advance(chunk);
        }
        assert!(requests.try_recv().is_ok());
        let parsed: Vec<_> = receiver
            .try_iter()
            .map(|event| match event {
                RunEvent::Line { text, .. } => crate::flashing::parse_line(&text),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(
            parsed,
            [
                crate::flashing::ParsedLine::DeviceStarted { device: 3 },
                crate::flashing::ParsedLine::Progress {
                    device: 3,
                    percent: 12
                },
                crate::flashing::ParsedLine::DeviceFinished {
                    device: 3,
                    success: true
                },
            ]
        );
    }

    #[test]
    fn gbk_suffixes_backspaces_and_repaint_sequences_do_not_corrupt_results() {
        let (sender, receiver) = mpsc::sync_channel(32);
        let (queries, _requests) = mpsc::channel();
        let mut lines = ConsoleLines {
            pending: Vec::new(),
            sender,
            queries,
            connected: true,
            escape: ParserState::Text,
        };
        let (gbk, _, _) = encoding_rs::GBK.encode("烧录完成");
        let mut output = b"\x1b[32mline[2199], device burn result=2 ".to_vec();
        output.extend_from_slice(&gbk);
        output.extend_from_slice(b"\x08\x08\x08\x08 continue\r\n");
        output.extend_from_slice(b"line[2200], device burn result=1\x1b[2K\r\n");
        lines.advance(&output);
        let messages: Vec<_> = receiver
            .try_iter()
            .filter_map(|event| match event {
                RunEvent::Line { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert!(messages[0].contains("device burn result=2 烧录完成 continue"));
        assert!(!messages[0].contains('\u{fffd}'));
        assert_eq!(messages[1], "line[2200], device burn result=1");
    }

    #[test]
    fn console_output_is_delivered_before_the_process_exits() {
        #[cfg(windows)]
        let mut command =
            CommandBuilder::new(std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into()));
        #[cfg(windows)]
        command.args([
            "/D",
            "/C",
            "echo device:[3] fes plugin & ping -n 5 127.0.0.1 >NUL",
        ]);
        #[cfg(unix)]
        let mut command = CommandBuilder::new("sh");
        #[cfg(unix)]
        command.args(["-c", "printf 'device:[3] fes plugin\\n'; sleep 5"]);
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (sender, receiver) = mpsc::sync_channel(32);
        let worker = std::thread::spawn(move || run_console(command, &worker_stop, sender));
        let event = receiver.recv_timeout(Duration::from_secs(3));
        stop.store(true, Ordering::Relaxed);
        drop(receiver);
        assert!(
            matches!(event, Ok(RunEvent::Line { text, .. }) if text.contains("device:[3] fes plugin"))
        );
        assert!(worker.join().unwrap().unwrap().1);
    }
}
