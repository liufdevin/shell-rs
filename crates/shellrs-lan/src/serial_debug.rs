use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::Core;

pub(super) struct ClientExecutor;

impl crate::Executor for ClientExecutor {
    fn hosts(&self) -> Vec<crate::SharedHost> {
        Vec::new()
    }

    fn execute(
        &self,
        _: &str,
        _: &str,
        _: &std::sync::atomic::AtomicBool,
        _: &mut dyn FnMut(bool, &[u8]) -> std::io::Result<()>,
    ) -> Result<i32> {
        anyhow::bail!("client_only")
    }
}

const RETAINED_BYTES: usize = 128 * 1024;
const PAGE_BYTES: usize = 32 * 1024;
pub const DEBUG_COMMAND_LIMIT: usize = 512;
pub(super) const DEBUG_WAIT_LIMIT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SerialDebugTab {
    pub id: String,
    pub label: String,
    pub execute: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SerialDebugLog {
    pub cursor: u64,
    pub gap: bool,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SerialDebugResult {
    pub exit_code: i32,
    pub output: String,
    pub truncated: bool,
}

#[derive(Default)]
struct State {
    revoked: bool,
    bytes: VecDeque<u8>,
    end: u64,
    running: bool,
    pending: Option<SerialDebugCommand>,
}

pub struct SerialDebugShare {
    tab: SerialDebugTab,
    peer: String,
    state: Mutex<State>,
}

impl SerialDebugShare {
    pub fn is_active(&self) -> bool {
        !self.state.lock().unwrap().revoked
    }

    pub fn peer(&self) -> &str {
        &self.peer
    }

    pub fn is_execution_allowed(&self) -> bool {
        self.tab.execute
    }

    pub fn revoke(&self) {
        let mut state = self.state.lock().unwrap();
        state.revoked = true;
        state.bytes.clear();
        let pending = state.pending.take();
        drop(state);
        drop(pending);
    }

    pub fn append(&self, text: &str) {
        let mut state = self.state.lock().unwrap();
        if state.revoked {
            return;
        }
        state.end += text.len() as u64;
        state.bytes.extend(text.bytes());
        let discard = state.bytes.len().saturating_sub(RETAINED_BYTES);
        state.bytes.drain(..discard);
        while state.bytes.front().is_some_and(|byte| byte & 0xc0 == 0x80) {
            state.bytes.pop_front();
        }
    }

    pub(super) fn logs(&self, after: Option<u64>) -> Result<SerialDebugLog> {
        let state = self.state.lock().unwrap();
        ensure!(!state.revoked, "serial_debug_not_authorized");
        let first = state.end - state.bytes.len() as u64;
        let requested = after.unwrap_or_else(|| state.end.saturating_sub(PAGE_BYTES as u64));
        ensure!(requested <= state.end, "invalid_serial_debug_cursor");
        let mut start = requested.saturating_sub(first) as usize;
        while state
            .bytes
            .get(start)
            .is_some_and(|byte| byte & 0xc0 == 0x80)
        {
            start += 1;
        }
        let mut end = (start + PAGE_BYTES).min(state.bytes.len());
        while state.bytes.get(end).is_some_and(|byte| byte & 0xc0 == 0x80) {
            end -= 1;
        }
        let bytes: Vec<_> = state.bytes.range(start..end).copied().collect();
        Ok(SerialDebugLog {
            cursor: first + end as u64,
            gap: after.is_some_and(|cursor| cursor < first),
            text: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }

    pub fn take_command(&self) -> Option<SerialDebugCommand> {
        self.state.lock().unwrap().pending.take()
    }

    pub(super) fn execute(
        self: &Arc<Self>,
        command: String,
        cancel: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<tokio::sync::oneshot::Receiver<CommandResult>> {
        ensure!(self.tab.execute, "serial_debug_read_only");
        let mut state = self.state.lock().unwrap();
        ensure!(!state.revoked, "serial_debug_not_authorized");
        ensure!(!state.running, "serial_debug_busy");
        let (reply, answer) = tokio::sync::oneshot::channel();
        state.running = true;
        state.pending = Some(SerialDebugCommand {
            command,
            share: Arc::downgrade(self),
            cancel,
            reply: Some(reply),
        });
        Ok(answer)
    }
}

type CommandResult = Result<SerialDebugResult, &'static str>;

pub struct SerialDebugCommand {
    command: String,
    share: Weak<SerialDebugShare>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    reply: Option<tokio::sync::oneshot::Sender<CommandResult>>,
}

impl SerialDebugCommand {
    pub fn command(&self) -> &str {
        &self.command
    }

    pub fn is_authorized(&self) -> bool {
        !self.cancel.load(std::sync::atomic::Ordering::Acquire)
            && self.share.upgrade().is_some_and(|share| share.is_active())
    }

    pub fn finish(mut self, exit_code: i32, output: String, truncated: bool) {
        self.respond(Ok(SerialDebugResult {
            exit_code,
            output,
            truncated,
        }));
    }

    pub fn fail(mut self, reason: &'static str) {
        self.respond(Err(reason));
    }

    fn respond(&mut self, result: CommandResult) {
        if let Some(share) = self.share.upgrade() {
            share.state.lock().unwrap().running = false;
        }
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(result);
        }
    }
}

impl Drop for SerialDebugCommand {
    fn drop(&mut self) {
        if self.reply.is_some() {
            self.respond(Err("serial_debug_disconnected"));
        }
    }
}

impl Core {
    pub fn share_serial_tab(
        &self,
        peer: &str,
        label: String,
        execute: bool,
    ) -> Result<Arc<SerialDebugShare>> {
        let state = self.state.lock().unwrap();
        ensure!(
            self.is_active() && self.is_running(),
            "LAN cooperation unavailable or locked"
        );
        ensure!(
            state.configuration.grants.contains_key(peer),
            "device_not_authorized"
        );
        let mut tabs = self.serial_debug.lock().unwrap();
        tabs.retain(|tab| tab.upgrade().is_some_and(|share| share.is_active()));
        ensure!(tabs.len() < 32, "too_many_serial_debug_tabs");
        let share = Arc::new(SerialDebugShare {
            tab: SerialDebugTab {
                id: format!("{:032x}", rand::random::<u128>()),
                label: label.chars().take(160).collect(),
                execute,
            },
            peer: peer.to_owned(),
            state: Mutex::default(),
        });
        tabs.push(Arc::downgrade(&share));
        Ok(share)
    }

    pub(super) fn serial_tabs(&self, peer: &str) -> Vec<SerialDebugTab> {
        self.serial_debug
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|share| share.peer == peer && share.is_active())
            .map(|share| share.tab.clone())
            .collect()
    }

    pub(super) fn serial_tab(&self, peer: &str, tab: &str) -> Result<Arc<SerialDebugShare>> {
        self.serial_debug
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
            .find(|share| share.peer == peer && share.tab.id == tab && share.is_active())
            .ok_or_else(|| anyhow::anyhow!("serial_debug_not_authorized"))
    }

    pub(super) fn revoke_serial_tabs(&self, peer: Option<&str>) {
        self.serial_debug.lock().unwrap().retain(|tab| {
            let Some(share) = tab.upgrade() else {
                return false;
            };
            if peer.is_none_or(|peer| share.peer == peer) {
                share.revoke();
                false
            } else {
                share.is_active()
            }
        });
    }
}
