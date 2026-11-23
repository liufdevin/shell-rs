//! Serial ISP jobs. Protocol bytes never pass through terminal decoding.
mod firmware;
pub(crate) mod panel;
pub(crate) mod stc;
pub(crate) mod stm32;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

pub(crate) use firmware::Firmware;
pub(crate) use panel::IspPanel;
pub use panel::{IspCommand, IspPage};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IspEvent {
    Identity(String),
    Progress { percent: u8, message: String },
    Finished(Result<(), String>),
}

/// Fixed snapshot of one confirmed operation. Dropping a request also releases
/// the terminal's input gate, including unsupported/disconnected transports.
pub struct IspRequest {
    pub(crate) image: Option<Firmware>,
    pub(crate) baud: u32,
    pub(crate) verify: bool,
    pub(crate) run: bool,
    pub(crate) stc: Option<stc::Options>,
    pub(crate) cancelled: Arc<AtomicBool>,
    pub(crate) active: Arc<AtomicBool>,
    pub(crate) events: mpsc::SyncSender<IspEvent>,
}

impl IspRequest {
    pub fn firmware_bytes(&self) -> Option<&[u8]> {
        self.image.as_ref().map(|image| image.bytes.as_slice())
    }
    pub fn firmware_address(&self) -> Option<u32> {
        self.image.as_ref().map(|image| image.address)
    }
    pub fn is_stc(&self) -> bool {
        self.stc.is_some()
    }
    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
    pub fn baud_rate(&self) -> u32 {
        self.baud
    }
    pub fn identify(&self, identity: String) {
        let _ = self.events.try_send(IspEvent::Identity(identity));
    }
    pub(crate) fn check_cancelled(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(crate::i18n::t!("serial.isp.cancelled").to_string())
        } else {
            Ok(())
        }
    }

    pub fn report(&self, percent: u8, message: String) {
        let _ = self
            .events
            .try_send(IspEvent::Progress { percent, message });
    }

    pub fn finish(self, result: Result<(), String>) {
        let events = self.events.clone();
        // Release exactly once, before publishing completion. A late Drop
        // must never clear the gate belonging to a subsequent operation.
        drop(self);
        let _ = events.send(IspEvent::Finished(result));
    }
}

impl Drop for IspRequest {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishing_completion_cannot_later_release_a_subsequent_job() {
        let active = Arc::new(AtomicBool::new(true));
        let (send, receive) = mpsc::sync_channel(4);
        let request = IspRequest {
            image: None,
            baud: 115200,
            verify: true,
            run: true,
            stc: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            active: active.clone(),
            events: send,
        };
        let (resume, wait) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            request.finish(Ok(()));
            wait.recv().unwrap();
        });
        assert_eq!(receive.recv().unwrap(), IspEvent::Finished(Ok(())));
        assert!(!active.load(Ordering::Acquire));
        active.store(true, Ordering::Release); // next job acquired the port
        resume.send(()).unwrap();
        worker.join().unwrap();
        assert!(active.load(Ordering::Acquire));
    }
}
