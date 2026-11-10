use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};

#[derive(Default)]
pub(super) struct SleepInhibitor {
    state: Mutex<State>,
    error: Mutex<Option<String>>,
}

#[derive(Default)]
struct State {
    enabled: bool,
    generation: u64,
    users: usize,
    request: Option<Request>,
}

pub(super) struct Usage {
    owner: Arc<SleepInhibitor>,
    generation: u64,
}

impl crate::Core {
    pub(super) fn keep_awake(&self) -> Result<Usage> {
        anyhow::ensure!(self.is_active(), "LAN cooperation unavailable or locked");
        let usage = self.sleep.acquire()?;
        anyhow::ensure!(self.is_active(), "LAN cooperation unavailable or locked");
        Ok(usage)
    }

    pub fn is_sleep_inhibited(&self) -> bool {
        self.sleep.is_inhibited()
    }

    pub fn take_sleep_error(&self) -> Option<String> {
        self.sleep.take_error()
    }
}

impl SleepInhibitor {
    pub fn acquire(self: &Arc<Self>) -> Result<Usage> {
        let mut state = self.state.lock().unwrap();
        anyhow::ensure!(state.enabled, "LAN cooperation unavailable or locked");
        if state.users == 0 {
            state.request = match Request::new() {
                Ok(request) => Some(request),
                Err(error) => {
                    *self.error.lock().unwrap() = Some(error.to_string());
                    return Err(error).context("sleep_prevention_failed");
                }
            };
        }
        state.users += 1;
        Ok(Usage {
            owner: self.clone(),
            generation: state.generation,
        })
    }

    pub fn reset(&self) {
        let mut state = self.state.lock().unwrap();
        state.request = None;
        state.enabled = false;
        state.users = 0;
        state.generation = state.generation.wrapping_add(1);
    }

    pub fn enable(&self) {
        self.state.lock().unwrap().enabled = true;
    }

    pub fn is_inhibited(&self) -> bool {
        cfg!(windows) && self.state.lock().unwrap().users > 0
    }

    pub fn take_error(&self) -> Option<String> {
        self.error.lock().unwrap().take()
    }
}

impl Drop for Usage {
    fn drop(&mut self) {
        let mut state = self.owner.state.lock().unwrap();
        if state.generation == self.generation {
            state.users -= 1;
            if state.users == 0 {
                state.request = None;
            }
        }
    }
}

#[cfg(windows)]
struct Request(std::os::windows::io::OwnedHandle);

#[cfg(windows)]
impl Request {
    fn new() -> std::io::Result<Self> {
        use std::os::windows::io::{FromRawHandle as _, OwnedHandle};
        use windows_sys::Win32::{
            Foundation::INVALID_HANDLE_VALUE,
            System::{
                Power::{PowerCreateRequest, PowerRequestSystemRequired, PowerSetRequest},
                Threading::{
                    POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0,
                },
            },
        };
        let mut reason: Vec<u16> = "ShellLF LAN remote access is in use\0"
            .encode_utf16()
            .collect();
        let context = REASON_CONTEXT {
            Version: 0,
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 {
                SimpleReasonString: reason.as_mut_ptr(),
            },
        };
        let handle = unsafe { PowerCreateRequest(&context) };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let owned = unsafe { OwnedHandle::from_raw_handle(handle) };
        if unsafe { PowerSetRequest(handle, PowerRequestSystemRequired) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(owned))
    }
}

#[cfg(windows)]
impl Drop for Request {
    fn drop(&mut self) {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::System::Power::{PowerClearRequest, PowerRequestSystemRequired};
        unsafe {
            PowerClearRequest(self.0.as_raw_handle(), PowerRequestSystemRequired);
        }
    }
}

#[cfg(not(windows))]
struct Request;

#[cfg(not(windows))]
impl Request {
    fn new() -> std::io::Result<Self> {
        Ok(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_user_releases_request_and_old_guards_cannot_release_new_requests() {
        let owner = Arc::new(SleepInhibitor::default());
        assert!(owner.acquire().is_err());
        owner.enable();
        let first = owner.acquire().unwrap();
        let second = owner.acquire().unwrap();
        assert_eq!(owner.state.lock().unwrap().users, 2);
        drop(first);
        assert!(owner.state.lock().unwrap().request.is_some());
        owner.reset();
        assert!(owner.state.lock().unwrap().request.is_none());
        assert!(owner.acquire().is_err());
        owner.enable();
        let third = owner.acquire().unwrap();
        drop(second);
        assert_eq!(owner.state.lock().unwrap().users, 1);
        drop(third);
        assert!(owner.state.lock().unwrap().request.is_none());
    }
}
