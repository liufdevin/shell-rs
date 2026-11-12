use std::time::Instant;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SerialDiagnostics {
    pub configuration: String,
    pub received: u64,
    pub transmitted: u64,
    pub first_received_ms: Option<u64>,
    pub errors: u64,
    pub last_error: Option<String>,
    pub cts: Option<bool>,
    pub dsr: Option<bool>,
}

pub(super) struct Diagnostics {
    pub snapshot: SerialDiagnostics,
    started: Instant,
}

impl Diagnostics {
    pub fn new(configuration: String) -> Self {
        Self {
            snapshot: SerialDiagnostics {
                configuration,
                ..Default::default()
            },
            started: Instant::now(),
        }
    }

    pub fn received(&mut self, count: usize) {
        self.snapshot.received = self.snapshot.received.saturating_add(count as u64);
        if count > 0 && self.snapshot.first_received_ms.is_none() {
            self.snapshot.first_received_ms =
                Some(self.started.elapsed().as_millis().min(u64::MAX as u128) as u64);
        }
    }

    pub fn transmitted(&mut self, count: usize) {
        self.snapshot.transmitted = self.snapshot.transmitted.saturating_add(count as u64);
    }

    pub fn error(&mut self, error: &std::io::Error) {
        self.snapshot.errors = self.snapshot.errors.saturating_add(1);
        self.snapshot.last_error = Some(format!(
            "{:?}; OS={:?}: {}",
            error.kind(),
            error.raw_os_error(),
            error
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_wire_bytes_independently_of_capture_and_preserves_first_receive() {
        let mut state = Diagnostics::new("115200 8N1".into());
        state.received(0);
        assert_eq!(state.snapshot.first_received_ms, None);
        state.received(12);
        let first = state.snapshot.first_received_ms;
        state.received(5);
        state.transmitted(3);
        state.error(&std::io::Error::from_raw_os_error(5));
        assert_eq!(
            (
                state.snapshot.received,
                state.snapshot.transmitted,
                state.snapshot.errors
            ),
            (17, 3, 1)
        );
        assert_eq!(state.snapshot.first_received_ms, first);
        assert!(state.snapshot.last_error.unwrap().contains("OS=Some(5)"));
    }
}
