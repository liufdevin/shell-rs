use std::collections::VecDeque;
use std::time::{Duration, Instant};

const CAPTURE_LIMIT: usize = 8192;
pub(crate) const MESSAGE_LIMIT: usize = 4096;
const COOLDOWN: Duration = Duration::from_millis(500);

struct Matcher {
    pattern: Vec<u8>,
    prefixes: Vec<usize>,
    matched: usize,
}

impl Matcher {
    fn new(pattern: Vec<u8>) -> Self {
        let mut prefixes = vec![0; pattern.len()];
        let mut matched = 0;
        for offset in 1..pattern.len() {
            while matched > 0 && pattern[offset] != pattern[matched] {
                matched = prefixes[matched - 1];
            }
            if pattern[offset] == pattern[matched] {
                matched += 1;
            }
            prefixes[offset] = matched;
        }
        Self {
            pattern,
            prefixes,
            matched: 0,
        }
    }

    fn feed(&mut self, byte: u8) -> bool {
        while self.matched > 0 && byte != self.pattern[self.matched] {
            self.matched = self.prefixes[self.matched - 1];
        }
        if byte == self.pattern[self.matched] {
            self.matched += 1;
        }
        if self.matched == self.pattern.len() {
            self.matched = 0;
            true
        } else {
            false
        }
    }
}

struct ReplyRule {
    matcher: Matcher,
    reply: Vec<u8>,
    last_sent: Option<Instant>,
    echo_offset: Option<usize>,
    window: Instant,
    window_count: u32,
}

#[derive(Default)]
pub(crate) struct SerialToolState {
    pub(crate) diagnostics: Option<crate::serial::SerialDiagnostics>,
    pub(crate) capturing: bool,
    captured: VecDeque<u8>,
    rule: Option<ReplyRule>,
    received: u64,
    replies: u64,
    paused: bool,
}

#[derive(Default)]
pub(crate) struct SerialToolSnapshot {
    pub(crate) diagnostics: Option<crate::serial::SerialDiagnostics>,
    pub(crate) capturing: bool,
    pub(crate) received: u64,
    pub(crate) replies: u64,
    pub(crate) automatic: bool,
    pub(crate) paused: bool,
    pub(crate) bytes: Vec<u8>,
}

impl SerialToolState {
    pub(crate) fn stop(&mut self) {
        self.capturing = false;
        self.rule = None;
    }

    pub(crate) fn snapshot(&self) -> SerialToolSnapshot {
        SerialToolSnapshot {
            diagnostics: self.diagnostics.clone(),
            capturing: self.capturing,
            received: self.received,
            replies: self.replies,
            automatic: self.rule.is_some(),
            paused: self.paused,
            bytes: self.captured.iter().copied().collect(),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.captured.clear();
        self.received = 0;
        self.replies = 0;
    }

    pub(crate) fn configure(&mut self, rule: Option<(Vec<u8>, Vec<u8>)>) {
        self.paused = false;
        self.rule = rule.map(|(pattern, reply)| ReplyRule {
            matcher: Matcher::new(pattern),
            reply,
            last_sent: None,
            echo_offset: None,
            window: Instant::now(),
            window_count: 0,
        });
    }

    pub(crate) fn receive(
        &mut self,
        bytes: &[u8],
        now: Instant,
        suspended: bool,
    ) -> Option<Vec<u8>> {
        if self.capturing {
            self.received = self.received.saturating_add(bytes.len() as u64);
            if bytes.len() >= CAPTURE_LIMIT {
                self.captured.clear();
                self.captured.extend(&bytes[bytes.len() - CAPTURE_LIMIT..]);
            } else {
                let remove = (self.captured.len() + bytes.len()).saturating_sub(CAPTURE_LIMIT);
                self.captured.drain(..remove);
                self.captured.extend(bytes);
            }
        }
        let rule = self.rule.as_mut()?;
        if suspended {
            rule.matcher.matched = 0;
            rule.echo_offset = None;
            return None;
        }
        let mut matched = false;
        for &byte in bytes {
            if let Some(offset) = rule.echo_offset {
                if rule
                    .last_sent
                    .is_some_and(|last| now.duration_since(last) < Duration::from_secs(2))
                {
                    if byte == rule.reply[offset] {
                        rule.echo_offset = (offset + 1 < rule.reply.len()).then_some(offset + 1);
                        continue;
                    }
                    for &pending in &rule.reply[..offset] {
                        matched |= rule.matcher.feed(pending);
                    }
                }
                rule.echo_offset = None;
            }
            matched |= rule.matcher.feed(byte);
        }
        if !matched
            || rule
                .last_sent
                .is_some_and(|last| now.duration_since(last) < COOLDOWN)
        {
            return None;
        }
        if now.duration_since(rule.window) >= Duration::from_secs(60) {
            rule.window = now;
            rule.window_count = 0;
        }
        if rule.window_count >= 60 {
            self.rule = None;
            self.paused = true;
            return None;
        }
        rule.window_count += 1;
        rule.last_sent = Some(now);
        rule.echo_offset = Some(0);
        self.replies = self.replies.saturating_add(1);
        Some(rule.reply.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured(pattern: &[u8], reply: &[u8]) -> SerialToolState {
        let mut state = SerialToolState::default();
        state.configure(Some((pattern.to_vec(), reply.to_vec())));
        state
    }

    #[test]
    fn matching_spans_chunks_and_consumes_old_matches() {
        let mut state = configured(b"ready", b"go\r\n");
        let now = Instant::now();
        assert!(state.receive(b"rea", now, false).is_none());
        assert_eq!(
            state.receive(b"dyready", now, false),
            Some(b"go\r\n".to_vec())
        );
        assert!(state.receive(b"!", now + COOLDOWN, false).is_none());
    }

    #[test]
    fn fragmented_echo_does_not_trigger_and_new_data_does() {
        let mut state = configured(b"ping", b"ping");
        let now = Instant::now();
        assert!(state.receive(b"ping", now, false).is_some());
        assert!(state.receive(b"pi", now + COOLDOWN, false).is_none());
        assert!(state.receive(b"ng", now + COOLDOWN, false).is_none());
        assert!(state.receive(b"ping", now + COOLDOWN, false).is_some());
    }

    #[test]
    fn capture_is_opt_in_and_bounded_and_handles_binary() {
        let mut state = SerialToolState::default();
        state.receive(b"ignored", Instant::now(), false);
        assert!(state.snapshot().bytes.is_empty());
        state.capturing = true;
        state.receive(&vec![0xff; CAPTURE_LIMIT * 2], Instant::now(), false);
        assert_eq!(state.snapshot().bytes.len(), CAPTURE_LIMIT);
        assert_eq!(state.snapshot().received, (CAPTURE_LIMIT * 2) as u64);
        state.clear();
        assert_eq!(state.snapshot().received, 0);
    }

    #[test]
    fn transfer_suspends_rules_and_drops_partial_match() {
        let mut state = configured(b"ready", b"go");
        let now = Instant::now();
        state.receive(b"rea", now, false);
        assert!(state.receive(b"dy", now, true).is_none());
        assert!(state.receive(b"dy", now, false).is_none());
        state.configure(None);
        assert!(state.receive(b"ready", now, false).is_none());
    }

    #[test]
    fn disconnect_preserves_capture_but_disarms_replies() {
        let mut state = configured(b"ready", b"go");
        state.capturing = true;
        let now = Instant::now();
        state.receive(b"saved data", now, false);
        state.stop();
        assert_eq!(state.snapshot().bytes, b"saved data");
        assert!(!state.snapshot().automatic);
        assert!(!state.snapshot().capturing);
        assert!(state.receive(b"ready", now, false).is_none());
    }

    #[test]
    fn repeated_prefixes_match_and_expired_echo_does_not_hide_new_data() {
        let mut state = configured(b"aab", b"aab");
        let now = Instant::now();
        assert!(state.receive(b"aaab", now, false).is_some());
        assert!(
            state
                .receive(b"aab", now + Duration::from_secs(3), false)
                .is_some()
        );
    }

    #[test]
    fn binary_rules_and_rate_limit_and_circuit_breaker() {
        let mut state = configured(&[0xff, 0], &[0xaa]);
        let now = Instant::now();
        for number in 0..60 {
            let time = now + COOLDOWN * number;
            assert_eq!(state.receive(&[0xff, 0], time, false), Some(vec![0xaa]));
            assert!(state.receive(&[0xff, 0], time, false).is_none());
        }
        assert!(
            state
                .receive(&[0xff, 0], now + COOLDOWN * 60, false)
                .is_none()
        );
        assert!(state.snapshot().paused);
        assert!(!state.snapshot().automatic);
    }
}
