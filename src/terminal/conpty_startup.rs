/// ConPTY cannot apply a resize until its inherited-cursor handshake finishes.
/// Output after the initial cursor query marks the console as ready.
#[derive(Default)]
pub(super) struct ConPtyStartup {
    matched: usize,
    ready: bool,
}

impl ConPtyStartup {
    pub(super) fn observe(&mut self, bytes: &[u8]) -> bool {
        const CURSOR_QUERY: &[u8] = b"\x1b[6n";
        for byte in bytes {
            if self.ready {
                break;
            }
            if CURSOR_QUERY.get(self.matched) == Some(byte) {
                self.matched += 1;
            } else {
                self.ready = true;
            }
        }
        self.ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_query_is_not_ready_until_console_output_follows() {
        for split in 0..=4 {
            let mut startup = ConPtyStartup::default();
            assert!(!startup.observe(&b"\x1b[6n"[..split]));
            assert!(!startup.observe(&b"\x1b[6n"[split..]));
            assert!(!startup.observe(b""));
            assert!(startup.observe(b"\x1b[m\x1b]0;cmd.exe\x07"));
            assert!(startup.observe(b"\x1b[6n"));
        }
    }

    #[test]
    fn fragmented_and_combined_startup_output_is_supported() {
        let mut startup = ConPtyStartup::default();
        for byte in b"\x1b[6n" {
            assert!(!startup.observe(std::slice::from_ref(byte)));
        }
        assert!(startup.observe(b"prompt>"));
        assert!(ConPtyStartup::default().observe(b"\x1b[6n\x1b[m"));
        assert!(ConPtyStartup::default().observe(b"prompt>"));
        assert!(ConPtyStartup::default().observe(b"\x1b[m"));
    }
}
