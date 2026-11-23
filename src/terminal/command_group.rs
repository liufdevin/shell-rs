use std::collections::VecDeque;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

use super::{PromptLine, TerminalTransportCommand, encode_secret};
use crate::host::{SnippetStep, SnippetWait, TerminalEncoding};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum CommandGroupStatus {
    #[default]
    Idle,
    Running {
        name: String,
        step: usize,
        total: usize,
        wait: SnippetWait,
    },
    Sent,
    Stopped,
    TimedOut {
        step: usize,
    },
    Unavailable,
}

impl CommandGroupStatus {
    pub(crate) fn is_active(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    pub(crate) fn message(&self) -> Option<gpui_kit::SharedString> {
        match self {
            Self::Idle => None,
            Self::Running {
                name,
                step,
                total,
                wait,
            } => {
                let wait = match wait {
                    SnippetWait::Immediate => crate::i18n::t!("snippets.group.sending"),
                    SnippetWait::Delay { milliseconds } => crate::i18n::t!(
                        "snippets.group.waiting_seconds",
                        seconds = f64::from(*milliseconds) / 1000.
                    ),
                    SnippetWait::Prompt { text, .. } => {
                        crate::i18n::t!("snippets.group.waiting_prompt", text = text)
                    }
                };
                Some(crate::i18n::t!(
                    "snippets.group.progress",
                    name = name,
                    step = step,
                    total = total,
                    wait = wait
                ))
            }
            Self::Sent => Some(crate::i18n::t!("snippets.group.sent")),
            Self::Stopped => Some(crate::i18n::t!("snippets.group.stopped")),
            Self::TimedOut { step } => {
                Some(crate::i18n::t!("snippets.group.timed_out", step = step))
            }
            Self::Unavailable => Some(crate::i18n::t!("snippets.group.unavailable")),
        }
    }

    pub(crate) fn active_message(&self) -> Option<gpui_kit::SharedString> {
        if self.is_active() {
            self.message()
        } else {
            None
        }
    }
}

struct Step {
    bytes: Zeroizing<Vec<u8>>,
    wait: SnippetWait,
}

struct Run {
    name: String,
    steps: VecDeque<Step>,
    total: usize,
    ready_at: Instant,
    deadline: Instant,
    writing: Option<u64>,
    cancelled: Arc<AtomicBool>,
    encoding: TerminalEncoding,
    matched: bool,
}

impl Drop for Run {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

#[derive(Default)]
pub(super) struct CommandGroupState {
    line: PromptLine,
    run: Option<Run>,
    status: CommandGroupStatus,
    next_write: u64,
}

impl CommandGroupState {
    pub(super) fn start(
        &mut self,
        name: String,
        steps: Vec<SnippetStep>,
        encoding: TerminalEncoding,
        now: Instant,
    ) -> Result<(), gpui_kit::SharedString> {
        if self.status.is_active() || !crate::host::valid_snippet_steps(&steps) {
            return Err(crate::i18n::t!("snippets.group.unavailable"));
        }
        let steps = steps
            .into_iter()
            .map(|step| {
                let mut bytes = encode_secret(encoding, step.content.as_bytes())?;
                if step.enter {
                    bytes.push(b'\r');
                }
                Ok(Step {
                    bytes,
                    wait: step.wait,
                })
            })
            .collect::<Result<VecDeque<_>, gpui_kit::SharedString>>()?;
        self.line = PromptLine::default();
        let total = steps.len();
        self.run = Some(Run {
            name,
            steps,
            total,
            ready_at: now,
            deadline: now,
            writing: None,
            cancelled: Arc::default(),
            encoding,
            matched: false,
        });
        self.arm(now);
        Ok(())
    }

    fn arm(&mut self, now: Instant) {
        let Some(run) = &mut self.run else {
            return;
        };
        let Some(step) = run.steps.front() else {
            self.stop(CommandGroupStatus::Sent);
            return;
        };
        let wait = step.wait.clone();
        run.matched = false;
        run.ready_at = now
            + match wait {
                SnippetWait::Delay { milliseconds } => Duration::from_millis(milliseconds.into()),
                _ => Duration::ZERO,
            };
        run.deadline = now
            + match wait {
                SnippetWait::Prompt { timeout_ms, .. } => Duration::from_millis(timeout_ms.into()),
                _ => Duration::from_secs(30),
            };
        self.status = CommandGroupStatus::Running {
            name: run.name.clone(),
            step: run.total - run.steps.len() + 1,
            total: run.total,
            wait,
        };
    }

    pub(super) fn status(&self) -> CommandGroupStatus {
        self.status.clone()
    }

    pub(super) fn stop(&mut self, status: CommandGroupStatus) {
        self.line = PromptLine::default();
        if self.run.take().is_some() {
            self.status = status;
        }
    }

    pub(super) fn receive(&mut self, bytes: &[u8], _: Instant) {
        let Some(run) = &mut self.run else {
            return;
        };
        if run.writing.is_some() || run.matched {
            return;
        }
        let Some(Step {
            wait: SnippetWait::Prompt { text, .. },
            ..
        }) = run.steps.front()
        else {
            return;
        };
        let expected = text.to_lowercase();
        for segment in bytes.split_inclusive(|byte| matches!(byte, b'\r' | b'\n')) {
            let newline = segment
                .last()
                .is_some_and(|byte| matches!(byte, b'\r' | b'\n'));
            let length = segment.len() - usize::from(newline);
            self.line.feed(&segment[..length]);
            if self.line.normalized(run.encoding).contains(&expected) {
                run.matched = true;
                break;
            }
            if newline {
                self.line.clear();
            }
        }
    }

    pub(super) fn written(&mut self, id: u64, success: bool, now: Instant) {
        let Some(run) = &mut self.run else {
            return;
        };
        if run.writing != Some(id) {
            return;
        }
        if !success || now >= run.deadline {
            self.stop(CommandGroupStatus::Unavailable);
            return;
        }
        run.writing = None;
        run.steps.pop_front();
        self.arm(now);
    }

    pub(super) fn poll(
        &mut self,
        _encoding: TerminalEncoding,
        commands: &mpsc::Sender<TerminalTransportCommand>,
        now: Instant,
    ) {
        let Some(run) = &mut self.run else {
            return;
        };
        if run.writing.is_some() {
            if now >= run.deadline {
                self.stop(CommandGroupStatus::Unavailable);
            }
            return;
        }
        let Some(step) = run.steps.front_mut() else {
            return;
        };
        let ready = match &step.wait {
            SnippetWait::Immediate => true,
            SnippetWait::Delay { .. } => now >= run.ready_at,
            SnippetWait::Prompt { .. } => {
                if now >= run.deadline {
                    let step = run.total - run.steps.len() + 1;
                    self.stop(CommandGroupStatus::TimedOut { step });
                    return;
                }
                run.matched
            }
        };
        if !ready {
            return;
        }
        self.next_write += 1;
        run.writing = Some(self.next_write);
        run.deadline = now + Duration::from_secs(30);
        self.line = PromptLine::default();
        if commands
            .send(TerminalTransportCommand::CommandGroupWrite {
                id: self.next_write,
                bytes: std::mem::take(&mut step.bytes),
                cancelled: run.cancelled.clone(),
            })
            .is_err()
        {
            self.stop(CommandGroupStatus::Unavailable);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take_write(
        receiver: &mpsc::Receiver<TerminalTransportCommand>,
        expected: &[u8],
    ) -> (u64, Arc<AtomicBool>) {
        let TerminalTransportCommand::CommandGroupWrite {
            id,
            bytes,
            cancelled,
        } = receiver.try_recv().unwrap()
        else {
            panic!("expected a command group write");
        };
        assert_eq!(bytes.as_slice(), expected);
        (id, cancelled)
    }

    #[test]
    fn prompts_delays_and_enter_are_applied_in_order() {
        let mut group = CommandGroupState::default();
        let (sender, receiver) = mpsc::channel();
        let now = Instant::now();
        let steps = vec![
            SnippetStep::new(
                "root",
                SnippetWait::Prompt {
                    text: "login:".into(),
                    timeout_ms: 30000,
                },
                true,
            ),
            SnippetStep::new("command", SnippetWait::Delay { milliseconds: 2000 }, false),
            SnippetStep::new(
                "next",
                SnippetWait::Prompt {
                    text: "ready>".into(),
                    timeout_ms: 1000,
                },
                true,
            ),
        ];
        group
            .start("test".into(), steps, TerminalEncoding::Utf8, now)
            .unwrap();
        group.receive(b"lo", now);
        group.poll(
            TerminalEncoding::Utf8,
            &sender,
            now + Duration::from_millis(200),
        );
        assert!(receiver.try_recv().is_err());
        group.receive(b"gin:\x1b[0m", now + Duration::from_millis(200));
        let sent = now + Duration::from_millis(400);
        group.poll(TerminalEncoding::Utf8, &sender, sent);
        let (id, _) = take_write(&receiver, b"root\r");
        group.receive(b"ready>", sent);
        group.written(id, true, sent);
        group.poll(
            TerminalEncoding::Utf8,
            &sender,
            sent + Duration::from_millis(1999),
        );
        assert!(receiver.try_recv().is_err());
        let delayed = sent + Duration::from_secs(2);
        group.poll(TerminalEncoding::Utf8, &sender, delayed);
        let (id, _) = take_write(&receiver, b"command");
        group.written(id, true, delayed);
        group.poll(
            TerminalEncoding::Utf8,
            &sender,
            delayed + Duration::from_millis(200),
        );
        assert!(receiver.try_recv().is_err());
        group.receive(b"ready>", delayed + Duration::from_millis(200));
        group.poll(
            TerminalEncoding::Utf8,
            &sender,
            delayed + Duration::from_millis(400),
        );
        let (id, _) = take_write(&receiver, b"next\r");
        group.written(id, true, delayed + Duration::from_millis(400));
        assert_eq!(group.status(), CommandGroupStatus::Sent);
    }

    #[test]
    fn timeout_stop_and_write_failure_prevent_later_steps() {
        let now = Instant::now();
        let (sender, receiver) = mpsc::channel();
        let mut group = CommandGroupState::default();
        let prompt = SnippetStep::new(
            "never",
            SnippetWait::Prompt {
                text: ">".into(),
                timeout_ms: 200,
            },
            true,
        );
        group
            .start("timeout".into(), vec![prompt], TerminalEncoding::Utf8, now)
            .unwrap();
        group.poll(
            TerminalEncoding::Utf8,
            &sender,
            now + Duration::from_millis(200),
        );
        assert_eq!(group.status(), CommandGroupStatus::TimedOut { step: 1 });
        assert!(receiver.try_recv().is_err());
        for success in [false, true] {
            group
                .start(
                    "cancel".into(),
                    vec![
                        SnippetStep::new("first", SnippetWait::Immediate, true),
                        SnippetStep::new("second", SnippetWait::Immediate, true),
                    ],
                    TerminalEncoding::Utf8,
                    now,
                )
                .unwrap();
            group.poll(TerminalEncoding::Utf8, &sender, now);
            let (id, cancel) = take_write(&receiver, b"first\r");
            if success {
                group.stop(CommandGroupStatus::Stopped);
            }
            group.written(id, success, now);
            group.poll(TerminalEncoding::Utf8, &sender, now);
            assert!(cancel.load(Ordering::Acquire));
            assert!(!group.status().is_active());
            assert!(receiver.try_recv().is_err());
        }
    }
}
