use std::collections::VecDeque;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use base64::Engine as _;
use regex::Regex;

use crate::i18n::t;
use crate::terminal::{ExecRequest, TerminalDecoder};

const LIMIT: usize = 4 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(20);
const QUIET: Duration = Duration::from_millis(250);
const MONITOR_IDLE: Duration = Duration::from_secs(1);
const WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const WAIT_LIMIT: usize = 4;

struct Waiting {
    request: ExecRequest,
    since: Instant,
}

struct Running {
    monitoring: bool,
    request: ExecRequest,
    start: Vec<u8>,
    end: Vec<u8>,
    output: Vec<u8>,
    since: Instant,
}

pub(crate) struct Shell {
    enabled: bool,
    prompt: Vec<u8>,
    ready: bool,
    edited: bool,
    last_output: Instant,
    last_input: Option<Instant>,
    running: Option<Running>,
    waiting: VecDeque<Waiting>,
    hide_prompt: bool,
    suppress_until_prompt: bool,
    encoding: crate::host::TerminalEncoding,
    posix_prompt_only: bool,
}

impl Shell {
    pub(crate) fn new(enabled: bool, encoding: crate::host::TerminalEncoding) -> Self {
        Self {
            enabled,
            encoding,
            posix_prompt_only: false,
            prompt: Vec::new(),
            ready: false,
            edited: false,
            last_output: Instant::now(),
            last_input: None,
            running: None,
            waiting: VecDeque::new(),
            hide_prompt: false,
            suppress_until_prompt: false,
        }
    }

    pub(crate) fn require_posix_prompt(mut self) -> Self {
        self.posix_prompt_only = true;
        self
    }

    pub(crate) fn input(&mut self, bytes: &[u8]) -> bool {
        if !bytes.is_empty() {
            self.last_input = Some(Instant::now());
            self.cancel_waiting();
        }
        if bytes.contains(&3) && self.running.is_some() {
            if self
                .running
                .as_ref()
                .is_some_and(|running| running.monitoring)
                && let Some(running) = self.running.take()
            {
                let _ = running
                    .request
                    .reply
                    .send(Err(super::transport::MONITOR_PAUSED.into()));
            }
            self.fail();
        }
        if self.running.is_some() {
            return false;
        }
        if !bytes.is_empty() {
            self.ready = false;
            self.hide_prompt = false;
            self.suppress_until_prompt = false;
            self.prompt.clear();
            self.edited = !matches!(bytes.last(), Some(b'\r' | b'\n' | 3));
        }
        true
    }

    pub(crate) fn busy(&self) -> bool {
        self.running.is_some()
    }

    pub(crate) fn transfer(&mut self, request: ExecRequest) -> Option<Vec<u8>> {
        self.cancel_waiting();
        if !self.enabled
            || self.running.is_some()
            || !self.ready
            || self.edited
            || self.last_output.elapsed() < QUIET
        {
            let _ = request.reply.send(Err(t!("serial.shell.busy").to_string()));
            return None;
        }
        let encoded = crate::terminal::encode_input(self.encoding, request.command.into_bytes());
        let mut encoded = match encoded {
            Ok(bytes)
                if bytes.len() < 1000 && !bytes.iter().any(|byte| matches!(byte, 0 | 10 | 13)) =>
            {
                bytes
            }
            _ => {
                let _ = request
                    .reply
                    .send(Err(t!("serial.shell.command_large").to_string()));
                return None;
            }
        };
        encoded.push(b'\r');
        self.ready = false;
        self.edited = false;
        self.prompt.clear();
        self.hide_prompt = false;
        let _ = request.reply.send(Ok(String::new()));
        Some(encoded)
    }

    pub(crate) fn execute(&mut self, request: ExecRequest) -> Option<Vec<Vec<u8>>> {
        if !self.enabled || self.edited || self.waiting.len() >= WAIT_LIMIT {
            let _ = request.reply.send(Err(t!("serial.shell.busy").to_string()));
            return None;
        }
        if request.command.len() > 64 * 1024 || request.command.contains('\0') {
            let _ = request
                .reply
                .send(Err(t!("serial.shell.command_large").to_string()));
            return None;
        }
        if self.running.is_some()
            || !self.ready
            || self.last_output.elapsed() < QUIET
            || !self.waiting.is_empty()
        {
            self.waiting.push_back(Waiting {
                request,
                since: Instant::now(),
            });
            return None;
        }
        self.start(request)
    }

    pub(crate) fn monitor(&mut self, request: ExecRequest) -> Option<Vec<Vec<u8>>> {
        if !self.enabled
            || self.running.is_some()
            || !self.ready
            || self.edited
            || !self.waiting.is_empty()
            || self.last_output.elapsed() < QUIET
            || self
                .last_input
                .is_some_and(|input| input.elapsed() < MONITOR_IDLE)
        {
            let _ = request
                .reply
                .send(Err(super::transport::MONITOR_PAUSED.into()));
            return None;
        }
        let script = self.execute(request);
        if let Some(running) = &mut self.running {
            running.monitoring = true;
        }
        script
    }

    pub(crate) fn poll(&mut self) -> Option<Vec<Vec<u8>>> {
        if self.running.is_some()
            || !self.ready
            || self.edited
            || self.last_output.elapsed() < QUIET
        {
            return None;
        }
        let waiting = self.waiting.pop_front()?;
        self.start(waiting.request)
    }

    fn start(&mut self, request: ExecRequest) -> Option<Vec<Vec<u8>>> {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let start = format!("\x1e{nonce}B\x1f").into_bytes();
        let end = format!("\x1e{nonce}E\x1f").into_bytes();
        let command =
            match crate::terminal::encode_input(self.encoding, request.command.as_bytes().to_vec())
            {
                Ok(command) => command,
                Err(error) => {
                    let _ = request.reply.send(Err(error.to_string()));
                    return None;
                }
            };
        let encoded = base64::engine::general_purpose::STANDARD.encode(command);
        let variable = format!("_shelllf_{nonce}");
        let mut lines = vec![format!("{variable}=''\r").into_bytes()];
        lines.extend(encoded.as_bytes().chunks(384).map(|chunk| {
            format!(
                "{variable}=\"${{{variable}}}{}\"\r",
                std::str::from_utf8(chunk).unwrap()
            )
            .into_bytes()
        }));
        lines.push(format!(
            "printf '\\036{nonce}B\\037'; sh -c \"$(printf %s \"${variable}\" | base64 -d)\" </dev/null 2>&1; unset {variable}; printf '\\036{nonce}E\\037'\r"
        ).into_bytes());
        self.running = Some(Running {
            monitoring: false,
            request,
            start,
            end,
            output: Vec::new(),
            since: Instant::now(),
        });
        self.ready = false;
        self.prompt.clear();
        self.hide_prompt = true;
        self.suppress_until_prompt = true;
        Some(lines)
    }

    pub(crate) fn output(&mut self, bytes: &[u8]) -> Vec<u8> {
        if !self.enabled {
            return bytes.to_vec();
        }
        self.last_output = Instant::now();
        if let Some(running) = &mut self.running {
            running.output.extend_from_slice(bytes);
            if running.output.len() > LIMIT {
                self.fail();
                return Vec::new();
            }
            if let Some(start) = position(&running.output, &running.start) {
                let start = start + running.start.len();
                if let Some(end) = position(&running.output[start..], &running.end) {
                    let running = self.running.take().unwrap();
                    let output = &running.output[start..start + end];
                    let decoded = TerminalDecoder::new(self.encoding).decode(output, true);
                    let _ = running
                        .request
                        .reply
                        .send(Ok(String::from_utf8_lossy(&decoded).replace("\r\n", "\n")));
                    let tail = &running.output[start + end + running.end.len()..];
                    self.observe_prompt(tail);
                    if self.ready {
                        self.suppress_until_prompt = false;
                    }
                }
            }
            return Vec::new();
        }
        let was_ready = self.ready;
        self.observe_prompt(bytes);
        if self.hide_prompt {
            if was_ready
                && !self.suppress_until_prompt
                && bytes.iter().any(|byte| !byte.is_ascii_whitespace())
            {
                self.hide_prompt = false;
                return bytes.to_vec();
            }
            if self.ready {
                self.suppress_until_prompt = false;
            }
            Vec::new()
        } else {
            bytes.to_vec()
        }
    }

    fn observe_prompt(&mut self, bytes: &[u8]) {
        self.prompt.extend_from_slice(bytes);
        if self.prompt.len() > 8192 {
            self.prompt.drain(..self.prompt.len() - 8192);
        }
        static ANSI: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*(?:\x07|\x1b\\)").unwrap()
        });
        let text = String::from_utf8_lossy(&self.prompt);
        let text = ANSI.replace_all(&text, "");
        let line = text.rsplit(['\r', '\n']).next().unwrap_or_default().trim();
        static PROMPT: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^(?:(?:\[[^\]\r\n]+\]|[^\s@]+@[^\s:]+:[^#$\r\n]*|[^\s:]+:[/~][^#$\r\n]*|[/~][^#$\r\n]*|(?:ba|da|a|z)?sh-[0-9.]+|)[#$]\s*)+$").unwrap()
        });
        self.ready =
            !self.edited && PROMPT.is_match(line) && !line.contains('\x1b') && line.len() < 1024;
        if self.posix_prompt_only {
            self.ready &= (line.contains('@')
                && (line.contains(':') || line.contains(" ~") || line.contains(" /")))
                || line.starts_with('/')
                || line.starts_with('~')
                || line.contains(":/")
                || line.starts_with("sh-");
        }
    }

    pub(crate) fn tick(&mut self) {
        while self
            .waiting
            .front()
            .is_some_and(|waiting| waiting.since.elapsed() >= WAIT_TIMEOUT)
        {
            let waiting = self.waiting.pop_front().unwrap();
            let _ = waiting
                .request
                .reply
                .send(Err(t!("serial.shell.busy").to_string()));
        }
        if self
            .running
            .as_ref()
            .is_some_and(|running| running.since.elapsed() >= TIMEOUT)
        {
            self.fail();
        }
    }

    fn fail(&mut self) {
        self.cancel_waiting();
        if let Some(running) = self.running.take() {
            let _ = running
                .request
                .reply
                .send(Err(t!("serial.shell.timeout").to_string()));
        }
        self.ready = false;
        self.edited = true;
        self.hide_prompt = false;
        self.prompt.clear();
    }

    fn cancel_waiting(&mut self) {
        for waiting in self.waiting.drain(..) {
            let _ = waiting
                .request
                .reply
                .send(Err(t!("serial.shell.busy").to_string()));
        }
    }
}

fn position(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    bytes
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn shell() -> Shell {
        let mut shell = Shell::new(true, crate::host::TerminalEncoding::Utf8);
        shell.output(b"root@board:~# ");
        shell.last_output -= QUIET;
        shell
    }

    #[test]
    fn queries_require_an_idle_unedited_prompt() {
        let mut shell = shell();
        shell.input(b"vi");
        let (reply, answer) = mpsc::channel();
        assert!(
            shell
                .execute(ExecRequest {
                    command: "ps".into(),
                    reply
                })
                .is_none()
        );
        assert!(answer.recv().unwrap().is_err());
    }

    #[test]
    fn monitoring_skips_busy_shell_without_queuing_and_resumes_at_the_prompt() {
        let mut shell = shell();
        shell.input(b"top\r");
        for _ in 0..10 {
            let (reply, answer) = mpsc::channel();
            assert!(
                shell
                    .monitor(ExecRequest {
                        command: "cat /proc/stat".into(),
                        reply
                    })
                    .is_none()
            );
            assert_eq!(
                answer.recv().unwrap().unwrap_err(),
                super::super::transport::MONITOR_PAUSED
            );
        }
        assert!(shell.waiting.is_empty());
        assert!(shell.running.is_none());
        shell.output(b"\r\nroot@board:~# ");
        shell.last_output -= QUIET;
        shell.last_input = Some(Instant::now() - MONITOR_IDLE);
        let (reply, _answer) = mpsc::channel();
        assert!(
            shell
                .monitor(ExecRequest {
                    command: "cat /proc/stat".into(),
                    reply
                })
                .is_some()
        );
    }

    #[test]
    fn user_interrupts_a_monitor_read_without_a_timeout_error() {
        let mut shell = shell();
        let (reply, answer) = mpsc::channel();
        assert!(
            shell
                .monitor(ExecRequest {
                    command: "cat /proc/stat".into(),
                    reply
                })
                .is_some()
        );
        assert!(shell.input(&[3]));
        assert_eq!(
            answer.recv().unwrap().unwrap_err(),
            super::super::transport::MONITOR_PAUSED
        );
        assert!(shell.running.is_none());
    }

    #[test]
    fn log_messages_ending_in_hash_are_not_shell_prompts() {
        let mut shell = shell();
        shell.output(b"\r\nprocessing job #");
        assert!(!shell.ready);
        shell.output(b"\r\nroot@board:~# sleep 100 #");
        assert!(!shell.ready);
        for prompt in [
            b"\r\nroot@board:~# ".as_slice(),
            b"\r\n[ubuntu@board ~]$ ",
            b"\r\n/ # ",
        ] {
            shell.output(prompt);
            assert!(shell.ready);
        }
    }

    #[test]
    fn helper_a133_repeated_prompts_are_idle_but_command_echo_is_not() {
        let mut shell = shell();
        shell.output(b"\r\nroot@HelperA133:~# root@HelperA133:~# ");
        assert!(shell.ready);
        shell.output(b"mtop -n 1 -d 1 -m");
        assert!(!shell.ready);
        shell.input(b"unfinished");
        shell.output(b"\r\nroot@HelperA133:~# ");
        assert!(!shell.ready);
    }

    #[test]
    fn telnet_queries_require_a_recognizable_posix_prompt() {
        for prompt in [b"Router# ".as_slice(), b"[router]# ", b"admin@router# "] {
            let mut shell =
                Shell::new(true, crate::host::TerminalEncoding::Utf8).require_posix_prompt();
            shell.output(prompt);
            assert!(!shell.ready);
        }
        for prompt in [
            b"root@ubuntu:~# ".as_slice(),
            b"console:/ # ",
            b"[root@board ~]# ",
        ] {
            let mut shell =
                Shell::new(true, crate::host::TerminalEncoding::Utf8).require_posix_prompt();
            shell.output(prompt);
            assert!(shell.ready);
        }
    }

    #[test]
    fn a_query_waits_for_the_prompt_quiet_interval() {
        let mut shell = shell();
        shell.last_output = Instant::now();
        let (reply, answer) = mpsc::channel();
        assert!(
            shell
                .execute(ExecRequest {
                    command: "ss -tunap".into(),
                    reply
                })
                .is_none()
        );
        assert!(matches!(answer.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert!(shell.poll().is_none());
        shell.last_output -= QUIET;
        assert!(shell.poll().is_some());
        let running = shell.running.as_ref().unwrap();
        let output = [
            &running.start[..],
            b"connections\r\n",
            &running.end[..],
            b"root@HelperA133:~# ",
        ]
        .concat();
        assert!(shell.output(&output).is_empty());
        assert_eq!(answer.try_recv().unwrap().unwrap(), "connections\n");
    }

    #[test]
    fn background_queries_wait_in_order_instead_of_failing_on_switch() {
        let mut shell = shell();
        let (first_reply, first_answer) = mpsc::channel();
        assert!(
            shell
                .execute(ExecRequest {
                    command: "mtop".into(),
                    reply: first_reply
                })
                .is_some()
        );
        let (second_reply, second_answer) = mpsc::channel();
        assert!(
            shell
                .execute(ExecRequest {
                    command: "ss".into(),
                    reply: second_reply
                })
                .is_none()
        );
        let running = shell.running.as_ref().unwrap();
        let output = [
            &running.start[..],
            b"bandwidth\n",
            &running.end[..],
            b"root@board:~# ",
        ]
        .concat();
        shell.output(&output);
        assert_eq!(first_answer.try_recv().unwrap().unwrap(), "bandwidth\n");
        assert!(matches!(
            second_answer.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(shell.poll().is_none());
        shell.last_output -= QUIET;
        assert!(shell.poll().is_some());
        assert_eq!(shell.running.as_ref().unwrap().request.command, "ss");
    }

    #[test]
    fn user_input_cancels_waiting_queries_without_inserting_them() {
        let mut shell = shell();
        shell.last_output = Instant::now();
        let (reply, answer) = mpsc::channel();
        assert!(
            shell
                .execute(ExecRequest {
                    command: "ss".into(),
                    reply
                })
                .is_none()
        );
        assert!(shell.input(b"vi file"));
        assert!(answer.try_recv().unwrap().is_err());
        shell.last_output -= QUIET;
        assert!(shell.poll().is_none());
        assert!(shell.waiting.is_empty());
    }

    #[test]
    fn foreground_programs_do_not_get_queries_or_automatic_control_c() {
        let mut shell = shell();
        shell.input(b"mtop\r");
        let (reply, answer) = mpsc::channel();
        assert!(
            shell
                .execute(ExecRequest {
                    command: "ss".into(),
                    reply
                })
                .is_none()
        );
        assert!(shell.poll().is_none());
        shell.waiting.front_mut().unwrap().since -= WAIT_TIMEOUT;
        shell.tick();
        assert!(answer.try_recv().unwrap().is_err());
        assert!(shell.poll().is_none());
    }

    #[test]
    fn the_background_waiting_queue_is_bounded() {
        let mut shell = shell();
        shell.last_output = Instant::now();
        let mut answers = Vec::new();
        for _ in 0..WAIT_LIMIT {
            let (reply, answer) = mpsc::channel();
            assert!(
                shell
                    .execute(ExecRequest {
                        command: "ss".into(),
                        reply
                    })
                    .is_none()
            );
            answers.push(answer);
        }
        let (reply, answer) = mpsc::channel();
        assert!(
            shell
                .execute(ExecRequest {
                    command: "ss".into(),
                    reply
                })
                .is_none()
        );
        assert!(answer.try_recv().unwrap().is_err());
        assert_eq!(shell.waiting.len(), WAIT_LIMIT);
        shell.input(b"\x03");
        assert!(
            answers
                .iter()
                .all(|answer| answer.try_recv().unwrap().is_err())
        );
    }

    #[test]
    fn fragmented_results_exclude_echo_and_markers() {
        let mut shell = shell();
        let (reply, answer) = mpsc::channel();
        shell
            .execute(ExecRequest {
                command: "printf hello".into(),
                reply,
            })
            .unwrap();
        let running = shell.running.as_ref().unwrap();
        let bytes = [
            b"echoed script\r\n".as_slice(),
            &running.start,
            b"hello\r\n",
            &running.end,
            b"root@board:~# ",
        ]
        .concat();
        for byte in bytes {
            assert!(shell.output(&[byte]).is_empty());
        }
        assert_eq!(answer.recv().unwrap().unwrap(), "hello\n");
        assert!(shell.ready);
        assert!(shell.input(b"ls\r"));
        assert_eq!(shell.output(b"files\r\n"), b"files\r\n");
    }

    #[test]
    fn timeout_pauses_queries_without_interrupting_the_foreground() {
        let mut shell = shell();
        let (reply, answer) = mpsc::channel();
        shell
            .execute(ExecRequest {
                command: "sleep 99".into(),
                reply,
            })
            .unwrap();
        shell.running.as_mut().unwrap().since -= TIMEOUT;
        shell.tick();
        assert!(answer.recv().unwrap().is_err());
        assert!(!shell.ready);
        assert!(shell.input(b"\x03"));
        shell.output(b"\r\nroot@board:~# ");
        assert!(shell.ready);
    }

    #[test]
    fn long_queries_are_split_into_short_ascii_lines() {
        let mut shell = shell();
        let command = "printf '%s\\n' \"设备 'quoted' $HOME\"; ".repeat(100);
        let (reply, _answer) = mpsc::channel();
        let lines = shell
            .execute(ExecRequest {
                command: command.clone(),
                reply,
            })
            .unwrap();
        assert!(lines.len() > 2);
        assert!(
            lines
                .iter()
                .all(|line| line.is_ascii() && line.len() < 1000 && !line.contains(&3))
        );
        let encoded: String = lines[1..lines.len() - 1]
            .iter()
            .map(|line| {
                let text = std::str::from_utf8(line).unwrap();
                text.split_once('}').unwrap().1.trim_end_matches("\"\r")
            })
            .collect();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
            command.as_bytes()
        );
    }

    #[cfg(unix)]
    #[test]
    fn staged_queries_execute_in_a_real_posix_shell() {
        let mut shell = shell();
        let (reply, answer) = mpsc::channel();
        let lines = shell
            .execute(ExecRequest {
                command: "printf '%s\\n' \"first 'quoted'\" 'second $HOME'".into(),
                reply,
            })
            .unwrap();
        let script = String::from_utf8(lines.concat())
            .unwrap()
            .replace('\r', "\n");
        let output = crate::testing::sh(&script, None);
        assert!(output.status.success());
        shell.output(&output.stdout);
        assert_eq!(
            answer
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap(),
            "first 'quoted'\nsecond $HOME\n"
        );
    }
}
