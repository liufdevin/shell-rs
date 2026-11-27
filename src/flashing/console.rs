#[cfg(any(not(windows), test))]
use std::time::Duration;
use std::{
    ffi::OsString,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Instant,
};

#[cfg(not(windows))]
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use super::process::{RunEvent, decode};

pub(super) struct ConsoleCommand {
    program: OsString,
    cwd: Option<PathBuf>,
    args: Vec<OsString>,
}

pub(super) fn command(program: &Path, tool_dir: &Path, arguments: &[OsString]) -> ConsoleCommand {
    ConsoleCommand {
        program: program.as_os_str().to_owned(),
        cwd: Some(tool_dir.to_path_buf()),
        args: arguments.to_vec(),
    }
}

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

#[cfg(not(windows))]
pub(super) fn run_console(
    command: ConsoleCommand,
    stop: &AtomicBool,
    sender: mpsc::SyncSender<RunEvent>,
) -> Result<(Option<i32>, bool), String> {
    let mut builder = CommandBuilder::new(&command.program);
    if let Some(cwd) = &command.cwd {
        builder.cwd(cwd);
    }
    builder.args(&command.args);
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
        .spawn_command(builder)
        .map_err(|error| error.to_string())?;
    drop(pair.slave);
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
    drop(writer);
    drop(pair.master);
    let _ = output.join();
    result
}

/// Windows ConPTY launcher that explicitly sets `STARTF_USESHOWWINDOW` + `SW_HIDE`
/// on `STARTUPINFOEXW` so GUI builds do not flash a visible console window every
/// time `PhoenixConsole.exe` starts or restarts its wait cycle.
#[cfg(windows)]
pub(super) fn run_console(
    command: ConsoleCommand,
    stop: &AtomicBool,
    sender: mpsc::SyncSender<RunEvent>,
) -> Result<(Option<i32>, bool), String> {
    use std::{
        fs::File,
        mem::{size_of, zeroed},
        os::windows::{
            ffi::OsStrExt as _,
            io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
        },
        ptr::{null, null_mut},
    };
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, HANDLE, INVALID_HANDLE_VALUE, STILL_ACTIVE, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        System::{
            Console::{COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON},
            Pipes::CreatePipe,
            Threading::{
                CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
                EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
                InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, STARTF_USESHOWWINDOW,
                STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
                WaitForSingleObject,
            },
        },
        UI::WindowsAndMessaging::SW_HIDE,
    };

    struct PseudoConsole(HPCON);
    impl Drop for PseudoConsole {
        fn drop(&mut self) {
            unsafe { ClosePseudoConsole(self.0) };
        }
    }

    struct AttrList {
        _buf: Vec<u64>,
        ptr: LPPROC_THREAD_ATTRIBUTE_LIST,
    }
    impl Drop for AttrList {
        fn drop(&mut self) {
            unsafe { DeleteProcThreadAttributeList(self.ptr) };
        }
    }

    fn create_pipe() -> Result<(OwnedHandle, OwnedHandle), String> {
        let mut read: HANDLE = null_mut();
        let mut write: HANDLE = null_mut();
        let ok = unsafe { CreatePipe(&mut read, &mut write, null(), 0) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(unsafe {
            (
                OwnedHandle::from_raw_handle(read),
                OwnedHandle::from_raw_handle(write),
            )
        })
    }

    if stop.load(Ordering::Relaxed) {
        return Ok((None, true));
    }

    let (pty_in_read, pty_in_write) = create_pipe()?;
    let (pty_out_read, pty_out_write) = create_pipe()?;

    let mut hpc: HPCON = 0;
    let hr = unsafe {
        CreatePseudoConsole(
            COORD { X: 4096, Y: 64 },
            pty_in_read.as_raw_handle(),
            pty_out_write.as_raw_handle(),
            0,
            &mut hpc,
        )
    };
    if hr != 0 {
        return Err(format!("CreatePseudoConsole failed (0x{:08x})", hr as u32));
    }
    let pseudo = PseudoConsole(hpc);
    // ConPTY duplicated the slave-side ends; close ours so EOF works cleanly.
    drop(pty_in_read);
    drop(pty_out_write);

    let mut attr_size: usize = 0;
    unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut attr_size) };
    let mut attr_buf = vec![0u64; attr_size.div_ceil(size_of::<u64>()).max(1)];
    let attr_ptr = attr_buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
    if unsafe { InitializeProcThreadAttributeList(attr_ptr, 1, 0, &mut attr_size) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let attr_list = AttrList {
        _buf: attr_buf,
        ptr: attr_ptr,
    };
    if unsafe {
        UpdateProcThreadAttribute(
            attr_list.ptr,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            pseudo.0 as *const std::ffi::c_void,
            size_of::<HPCON>(),
            null_mut(),
            null(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().to_string());
    }

    let mut si: STARTUPINFOEXW = unsafe { zeroed() };
    si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
    si.StartupInfo.wShowWindow = SW_HIDE as u16;
    si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
    si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
    si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
    si.lpAttributeList = attr_list.ptr;

    let mut cmdline = build_windows_command_line(&command.program, &command.args);
    let cwd_wide: Option<Vec<u16>> = command.cwd.as_ref().map(|dir| {
        dir.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    });
    let mut pi: PROCESS_INFORMATION = unsafe { zeroed() };
    let created = unsafe {
        CreateProcessW(
            null(),
            cmdline.as_mut_ptr(),
            null(),
            null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            null(),
            cwd_wide.as_ref().map_or(null(), |w| w.as_ptr()),
            &si.StartupInfo,
            &mut pi,
        )
    };
    if created == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    unsafe { CloseHandle(pi.hThread) };
    let process = unsafe { OwnedHandle::from_raw_handle(pi.hProcess) };
    drop(attr_list);

    let group = match crate::shared::process::CommandGroup::for_handle(process.as_raw_handle()) {
        Ok(group) => group,
        Err(error) => {
            unsafe {
                TerminateProcess(process.as_raw_handle(), 1);
                WaitForSingleObject(process.as_raw_handle(), 2_000);
            }
            return Err(error.to_string());
        }
    };

    let mut reader = File::from(pty_out_read);
    let mut writer = File::from(pty_in_write);
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

    let read_exit_code = |handle: HANDLE| -> Option<i32> {
        let mut code: u32 = 0;
        if unsafe { GetExitCodeProcess(handle, &mut code) } != 0 && code != STILL_ACTIVE as u32 {
            Some(code as i32)
        } else {
            None
        }
    };

    let result = loop {
        if stop.load(Ordering::Relaxed) {
            group.terminate();
            unsafe {
                TerminateProcess(process.as_raw_handle(), 1);
                WaitForSingleObject(process.as_raw_handle(), 2_000);
            }
            break Ok((read_exit_code(process.as_raw_handle()), true));
        }
        for _ in requests.try_iter() {
            let _ = writer.write_all(b"\x1b[1;1R").and_then(|_| writer.flush());
        }
        match unsafe { WaitForSingleObject(process.as_raw_handle(), 20) } {
            WAIT_OBJECT_0 => break Ok((read_exit_code(process.as_raw_handle()), false)),
            WAIT_TIMEOUT => {}
            _ => {
                group.terminate();
                unsafe {
                    TerminateProcess(process.as_raw_handle(), 1);
                    WaitForSingleObject(process.as_raw_handle(), 2_000);
                }
                break Err(std::io::Error::last_os_error().to_string());
            }
        }
    };

    group.terminate();
    drop(writer);
    drop(pseudo);
    let _ = output.join();
    result
}

#[cfg(windows)]
fn build_windows_command_line(program: &std::ffi::OsStr, args: &[OsString]) -> Vec<u16> {
    let mut wide = Vec::new();
    append_windows_arg(&mut wide, program);
    for arg in args {
        wide.push(b' ' as u16);
        append_windows_arg(&mut wide, arg);
    }
    wide.push(0);
    wide
}

#[cfg(windows)]
fn append_windows_arg(out: &mut Vec<u16>, arg: &std::ffi::OsStr) {
    use std::os::windows::ffi::OsStrExt as _;
    let units: Vec<u16> = arg.encode_wide().collect();
    let needs_quotes = units.is_empty()
        || units
            .iter()
            .any(|&u| u == b' ' as u16 || u == b'\t' as u16 || u == b'"' as u16);
    if !needs_quotes {
        out.extend_from_slice(&units);
        return;
    }
    out.push(b'"' as u16);
    let mut backslashes = 0usize;
    for u in units {
        if u == b'\\' as u16 {
            backslashes += 1;
        } else if u == b'"' as u16 {
            for _ in 0..backslashes * 2 + 1 {
                out.push(b'\\' as u16);
            }
            out.push(b'"' as u16);
            backslashes = 0;
        } else {
            for _ in 0..backslashes {
                out.push(b'\\' as u16);
            }
            backslashes = 0;
            out.push(u);
        }
    }
    for _ in 0..backslashes * 2 {
        out.push(b'\\' as u16);
    }
    out.push(b'"' as u16);
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
        let command = ConsoleCommand {
            program: std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into()),
            cwd: None,
            args: [
                "/D",
                "/C",
                "echo device:[3] fes plugin & ping -n 5 127.0.0.1 >NUL",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
        };
        #[cfg(unix)]
        let command = ConsoleCommand {
            program: OsString::from("sh"),
            cwd: None,
            args: ["-c", "printf 'device:[3] fes plugin\\n'; sleep 5"]
                .into_iter()
                .map(OsString::from)
                .collect(),
        };
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

    #[cfg(windows)]
    #[test]
    fn windows_command_line_quotes_spaces_and_quotes() {
        let wide = build_windows_command_line(
            std::ffi::OsStr::new(r"C:\Program Files\AW\PhoenixConsole.exe"),
            &[
                OsString::from("-p"),
                OsString::from(r"D:\my images\a133 image.img"),
                OsString::from("--no-adb"),
            ],
        );
        let text = String::from_utf16(&wide[..wide.len() - 1]).unwrap();
        assert_eq!(
            text,
            r#""C:\Program Files\AW\PhoenixConsole.exe" -p "D:\my images\a133 image.img" --no-adb"#
        );
    }
}
