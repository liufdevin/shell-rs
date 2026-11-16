use std::{
    io::{Read as _, Write as _},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use alacritty_terminal::vte::{Params, Parser, Perform};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use super::process::RunEvent;

struct ConsoleLines {
    pending: String,
    sender: mpsc::SyncSender<RunEvent>,
    queries: mpsc::Sender<()>,
    connected: bool,
}

impl ConsoleLines {
    fn flush(&mut self) {
        let text = std::mem::take(&mut self.pending);
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
}

impl Perform for ConsoleLines {
    fn print(&mut self, character: char) {
        if self.pending.len() < 8192 {
            self.pending.push(character);
        }
    }

    fn execute(&mut self, byte: u8) {
        if matches!(byte, b'\r' | b'\n') {
            self.flush();
        }
    }

    fn csi_dispatch(&mut self, params: &Params, _: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        if action == 'n' && params.iter().next() == Some([6].as_slice()) {
            let _ = self.queries.send(());
        } else if matches!(action, 'H' | 'f') {
            self.flush();
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
        let mut parser = Parser::<1024>::new();
        let mut lines = ConsoleLines {
            pending: String::new(),
            sender,
            queries,
            connected: true,
        };
        let mut buffer = [0u8; 4096];
        while lines.connected {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => parser.advance(&mut lines, &buffer[..count]),
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
            pending: String::new(),
            sender,
            queries,
            connected: true,
        };
        let mut parser = Parser::<1024>::new();
        for chunk in [
            b"\x1b[6n\x1b[32mline[2141], device:[3] fes plugin\x1b[0m\r\n".as_slice(),
            b"line[2153], device [3] progress = 12%\r",
            b"\nline[2172], device:[3] burn finish, result=Success\r\n",
        ] {
            parser.advance(&mut lines, chunk);
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
