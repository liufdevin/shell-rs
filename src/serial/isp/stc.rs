use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::{atomic::Ordering, mpsc},
    thread,
    time::{Duration, Instant},
};

use super::{IspEvent, IspRequest};
use crate::i18n::t;

#[derive(Clone, Debug)]
pub(crate) struct Options {
    pub(crate) python: String,
    pub(crate) protocol: String,
}

fn arguments(options: &Options, device: &str, baud: u32) -> Result<Vec<String>, String> {
    if !["auto", "stc15", "stc15a"].contains(&options.protocol.as_str())
        || options.python.trim().is_empty()
        || baud == 0
    {
        return Err(t!("serial.isp.invalid_settings").to_string());
    }
    Ok(vec![
        "-u".into(),
        "-c".into(),
        include_str!("stc_bridge.py").into(),
        "-P".into(),
        options.protocol.clone(),
        "-p".into(),
        device.into(),
        "-l".into(),
        "2400".into(),
        "-b".into(),
        baud.to_string(),
    ])
}

fn drain(
    reader: impl Read + Send + 'static,
    sender: mpsc::SyncSender<String>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = reader;
        let mut bytes = [0; 1024];
        let mut line = Vec::new();
        while let Ok(count) = reader.read(&mut bytes) {
            if count == 0 {
                break;
            }
            for byte in &bytes[..count] {
                if matches!(byte, b'\r' | b'\n') || line.len() == 4096 {
                    if !line.is_empty() {
                        let _ = sender.try_send(String::from_utf8_lossy(&line).to_string());
                        line.clear();
                    }
                } else {
                    line.push(*byte);
                }
            }
            // stcgal prints "Waiting for MCU: " without a newline. Expose
            // that prompt while retaining the buffer for complete JSON lines.
            if !line.is_empty()
                && !line.starts_with(b"SHELLLF_ISP ")
                && !b"SHELLLF_ISP ".starts_with(&line)
            {
                let _ = sender.try_send(String::from_utf8_lossy(&line).to_string());
            }
        }
        if !line.is_empty() {
            let _ = sender.try_send(String::from_utf8_lossy(&line).to_string());
        }
    })
}

fn backend_message(line: &str) -> Option<serde_json::Value> {
    serde_json::from_str(line.strip_prefix("SHELLLF_ISP ")?).ok()
}

pub(crate) fn execute(device: &str, request: &IspRequest) -> Result<(), String> {
    request.check_cancelled()?;
    let options = request
        .stc
        .as_ref()
        .ok_or_else(|| t!("serial.isp.invalid_settings").to_string())?;
    let mut args = arguments(options, device, request.baud)?;
    // Feed an immutable, validated BIN snapshot, never re-read a changing
    // original file after the user's Start action.
    let mut file = tempfile::Builder::new()
        .suffix(".bin")
        .tempfile()
        .map_err(|error| error.to_string())?;
    if let Some(image) = &request.image {
        if image.address != 0 || image.bytes.len() > 65536 {
            return Err(t!("serial.isp.stc_range").to_string());
        }
        file.write_all(&image.bytes)
            .and_then(|_| file.flush())
            .map_err(|error| error.to_string())?;
        args.push("--".into());
        args.push(file.path().to_string_lossy().into_owned());
    }
    let mut command = Command::new(options.python.trim());
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000);
    }
    request.report(0, t!("serial.isp.stc_waiting").to_string());
    let (mut child, group) = crate::shared::process::CommandGroup::spawn(&mut command)
        .map_err(|error| t!("serial.isp.stc_unavailable", error = error).to_string())?;
    if let Err(error) = group.terminate_on_close() {
        group.terminate();
        let _ = child.wait();
        return Err(error.to_string());
    }
    let (send, receive) = mpsc::sync_channel(128);
    let stdout = drain(child.stdout.take().unwrap(), send.clone());
    let stderr = drain(child.stderr.take().unwrap(), send);
    let deadline =
        Instant::now() + Duration::from_secs(if request.image.is_some() { 600 } else { 60 });
    let mut last_lines = std::collections::VecDeque::new();
    let mut percent = 0;
    let seen_output = std::cell::Cell::new(false);
    let identified = std::cell::Cell::new(false);
    let completed = std::cell::Cell::new(false);
    let started = Instant::now();
    let mut consume = |line: String| {
        seen_output.set(true);
        let mut display = line.clone();
        if let Some(event) = backend_message(&line) {
            if let Some(value) = event["percent"].as_u64() {
                percent = value.min(99) as u8;
            }
            if let Some(identity) = event["identity"].as_str() {
                identified.set(true);
                request.identify(identity.into());
                display = identity.into();
            }
            if let Some(message) = event["message"].as_str() {
                display = message.into();
            }
            if event["complete"].as_bool() == Some(true) {
                completed.set(true);
                display = t!("serial.isp.backend_complete").to_string();
            }
        }
        if last_lines.len() == 12 {
            last_lines.pop_front();
        }
        last_lines.push_back(display.clone());
        let _ = request.events.try_send(IspEvent::Progress {
            percent,
            message: display,
        });
    };
    let result = loop {
        while let Ok(line) = receive.try_recv() {
            consume(line);
        }
        let startup_expired = !seen_output.get() && started.elapsed() >= Duration::from_secs(15);
        let handshake_expired = !identified.get() && started.elapsed() >= Duration::from_secs(60);
        if request.cancelled.load(Ordering::Acquire)
            || Instant::now() >= deadline
            || startup_expired
            || handshake_expired
        {
            group.terminate();
            let _ = child.wait();
            break Err(if request.cancelled.load(Ordering::Acquire) {
                t!("serial.isp.cancelled").to_string()
            } else if startup_expired {
                t!("serial.isp.stc_start_timeout").to_string()
            } else {
                t!("serial.isp.timeout").to_string()
            });
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status.success()),
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                group.terminate();
                let _ = child.wait();
                break Err(error.to_string());
            }
        }
    };
    group.terminate();
    let _ = stdout.join();
    let _ = stderr.join();
    while let Ok(line) = receive.try_recv() {
        consume(line);
    }
    match result? {
        true if completed.get() && identified.get() => Ok(()),
        _ => Err(t!(
            "serial.isp.stc_failed",
            error = last_lines.into_iter().collect::<Vec<_>>().join("\n")
        )
        .to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invocation_keeps_device_names_and_protocols_as_separate_arguments() {
        let options = Options {
            python: "C:/Program Files/Python/python.exe".into(),
            protocol: "stc15".into(),
        };
        let args = arguments(&options, "COM5;echo injected", 115200).unwrap();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-p", "COM5;echo injected"])
        );
        assert!(args.windows(2).any(|pair| pair == ["-P", "stc15"]));
        assert!(
            !args
                .iter()
                .any(|arg| arg == "-r" || arg == "-o" || arg == "-e")
        );
        assert!(
            arguments(
                &Options {
                    protocol: "usb15".into(),
                    ..options
                },
                "COM5",
                115200
            )
            .is_err()
        );
    }
    #[test]
    fn only_a_valid_adapter_message_can_confirm_completion() {
        assert!(backend_message("exit code: 0").is_none());
        assert!(backend_message("SHELLLF_ISP {invalid").is_none());
        let done = backend_message("SHELLLF_ISP {\"complete\":true}").unwrap();
        assert_eq!(done["complete"].as_bool(), Some(true));
        let fake = backend_message("SHELLLF_ISP {\"complete\":\"true\"}").unwrap();
        assert_ne!(fake["complete"].as_bool(), Some(true));
    }

    #[test]
    fn prompts_are_visible_before_newlines_but_partial_control_records_are_not() {
        struct Fragments(std::collections::VecDeque<&'static [u8]>);
        impl Read for Fragments {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                let Some(bytes) = self.0.pop_front() else {
                    return Ok(0);
                };
                buffer[..bytes.len()].copy_from_slice(bytes);
                Ok(bytes.len())
            }
        }
        let reader = Fragments(
            [
                b"Waiting for MCU: ".as_slice(),
                b"\nSHELLLF_ISP {\"compl".as_slice(),
                b"ete\":true}\n".as_slice(),
            ]
            .into(),
        );
        let (send, receive) = mpsc::sync_channel(16);
        drain(reader, send).join().unwrap();
        let lines: Vec<_> = receive.try_iter().collect();
        assert!(lines.iter().any(|line| line == "Waiting for MCU: "));
        assert!(
            lines
                .iter()
                .any(|line| backend_message(line).is_some_and(|event| event["complete"] == true))
        );
        assert!(!lines.iter().any(|line| line == "SHELLLF_ISP {\"compl"));
    }

    #[test]
    fn missing_backend_and_cancelled_jobs_report_errors_without_opening_a_port() {
        use std::sync::{Arc, atomic::AtomicBool};
        let request = IspRequest {
            image: None,
            baud: 115200,
            verify: true,
            run: true,
            stc: Some(Options {
                python: "shelllf-nonexistent-isp-python-test".into(),
                protocol: "auto".into(),
            }),
            cancelled: Arc::new(AtomicBool::new(false)),
            active: Arc::new(AtomicBool::new(false)),
            events: mpsc::sync_channel(128).0,
        };
        assert!(execute("NOT-A-REAL-PORT", &request).is_err());
        request.cancelled.store(true, Ordering::Release);
        assert!(execute("NOT-A-REAL-PORT", &request).is_err());
    }
}
