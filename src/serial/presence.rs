use std::{
    collections::{HashMap, HashSet, VecDeque},
    io,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::{AppContext as _, Context, Entity, SharedString, Subscription, Task};

use crate::host::{HostStore, ProtocolKind};
use crate::i18n::t;

const SCAN_INTERVAL: Duration = Duration::from_secs(2);
const ONLINE_CONFIRM: Duration = Duration::from_secs(2);
const OFFLINE_CONFIRM: Duration = Duration::from_secs(6);
const FLAP_WINDOW: Duration = Duration::from_secs(60);
const STABLE_RECOVERY: Duration = Duration::from_secs(15);
const MAX_SAMPLE_GAP: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SerialPresenceState {
    #[default]
    Checking,
    Online,
    Offline,
    Unstable,
    Unknown,
}

impl SerialPresenceState {
    pub fn is_online(self) -> bool {
        self == Self::Online
    }

    pub fn label(self) -> SharedString {
        match self {
            Self::Checking => t!("serial.presence.checking"),
            Self::Online => t!("serial.presence.online"),
            Self::Offline => t!("serial.presence.offline"),
            Self::Unstable => t!("serial.presence.unstable"),
            Self::Unknown => t!("serial.presence.unknown"),
        }
    }
}

pub trait SerialPortSource: Send + Sync {
    fn ports(&self) -> io::Result<Vec<String>>;
}

pub struct SystemSerialPortSource;

impl SerialPortSource for SystemSerialPortSource {
    fn ports(&self) -> io::Result<Vec<String>> {
        super::ports::read_ports()
    }
}

#[derive(Default)]
struct PresenceTracker {
    state: SerialPresenceState,
    last_sample: Option<Instant>,
    observed: Option<bool>,
    since: Option<Instant>,
    samples: usize,
    drops: VecDeque<Instant>,
    unstable: bool,
}

impl PresenceTracker {
    fn interrupt(&mut self) {
        self.last_sample = None;
        self.observed = None;
        self.since = None;
        self.samples = 0;
        self.state = if self.unstable {
            SerialPresenceState::Unstable
        } else {
            SerialPresenceState::Unknown
        };
    }

    fn observe(&mut self, present: bool, now: Instant) {
        if self
            .last_sample
            .is_some_and(|last| now.saturating_duration_since(last) > MAX_SAMPLE_GAP)
        {
            self.interrupt();
            if !self.unstable {
                self.state = SerialPresenceState::Checking;
            }
        }
        self.last_sample = Some(now);
        while self
            .drops
            .front()
            .is_some_and(|drop| now.saturating_duration_since(*drop) > FLAP_WINDOW)
        {
            self.drops.pop_front();
        }
        if self.observed == Some(true) && !present {
            self.drops.push_back(now);
            if self.drops.len() >= 3 {
                self.unstable = true;
            }
        }
        if self.observed != Some(present) {
            self.observed = Some(present);
            self.since = Some(now);
            self.samples = 0;
        }
        self.samples = self.samples.saturating_add(1);
        let elapsed = now.saturating_duration_since(self.since.unwrap_or(now));
        if self.unstable {
            self.state = SerialPresenceState::Unstable;
            if present && self.samples >= 2 && elapsed >= STABLE_RECOVERY {
                self.unstable = false;
                self.drops.clear();
                self.state = SerialPresenceState::Online;
            }
        } else if present && self.samples >= 2 && elapsed >= ONLINE_CONFIRM {
            self.state = SerialPresenceState::Online;
        } else if !present && self.samples >= 4 && elapsed >= OFFLINE_CONFIRM {
            self.state = SerialPresenceState::Offline;
        }
    }
}

pub(super) fn device_key(device: &str) -> String {
    let device = device.trim();
    if cfg!(windows) {
        device
            .strip_prefix("\\\\.\\")
            .unwrap_or(device)
            .to_ascii_uppercase()
    } else {
        device.to_owned()
    }
}

pub struct SerialPresence {
    store: Entity<HostStore>,
    source: Arc<dyn SerialPortSource>,
    devices: HashMap<String, PresenceTracker>,
    generation: u64,
    poll_task: Option<Task<()>>,
    _subscription: Subscription,
}

impl SerialPresence {
    pub fn new(store: Entity<HostStore>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&store, |this, _, cx| this.sync_hosts(cx));
        let mut this = Self {
            store,
            source: Arc::new(SystemSerialPortSource),
            devices: HashMap::new(),
            generation: 0,
            poll_task: None,
            _subscription: subscription,
        };
        this.sync_hosts(cx);
        this
    }

    pub fn state_for(&self, device: &str) -> SerialPresenceState {
        self.devices
            .get(&device_key(device))
            .map_or(SerialPresenceState::Checking, |tracker| tracker.state)
    }

    fn sync_hosts(&mut self, cx: &mut Context<Self>) {
        let targets: HashSet<String> = self
            .store
            .read(cx)
            .hosts()
            .iter()
            .filter(|host| host.kind() == ProtocolKind::Serial)
            .map(|host| device_key(&host.address))
            .collect();
        if targets.len() == self.devices.len()
            && targets
                .iter()
                .all(|device| self.devices.contains_key(device))
        {
            return;
        }
        self.devices.retain(|device, _| targets.contains(device));
        for device in targets {
            self.devices.entry(device).or_default();
        }
        self.generation = self.generation.wrapping_add(1);
        cx.notify();
    }

    pub fn start(&mut self, source: Arc<dyn SerialPortSource>, cx: &mut Context<Self>) {
        self.source = source.clone();
        self.poll_task = None;
        for tracker in self.devices.values_mut() {
            *tracker = PresenceTracker::default();
        }
        self.generation = self.generation.wrapping_add(1);
        cx.notify();
        self.poll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok((generation, has_devices)) =
                    this.update(cx, |this, _| (this.generation, !this.devices.is_empty()))
                else {
                    break;
                };
                if has_devices {
                    let source = source.clone();
                    let ports = cx.background_spawn(async move { source.ports() }).await;
                    if this
                        .update(cx, |this, cx| {
                            this.apply_scan(generation, ports, cx);
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                cx.background_executor().timer(SCAN_INTERVAL).await;
            }
        }));
    }

    pub(crate) fn source(&self) -> Arc<dyn SerialPortSource> {
        self.source.clone()
    }

    fn apply_scan(
        &mut self,
        generation: u64,
        ports: io::Result<Vec<String>>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        let ports = ports.map(|ports| {
            ports
                .iter()
                .map(|port| device_key(port))
                .collect::<HashSet<_>>()
        });
        let now = cx.background_executor().now();
        let mut changed = false;
        for (device, tracker) in &mut self.devices {
            let previous = tracker.state;
            match &ports {
                Ok(ports) => tracker.observe(ports.contains(device), now),
                Err(_) => tracker.interrupt(),
            }
            changed |= previous != tracker.state;
        }
        if changed {
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PresenceTracker, SerialPresence, SerialPresenceState, device_key};
    use crate::host::{HostDraft, HostStore, SerialSettings};
    use gpui_kit::{AppContext as _, TestAppContext};
    use std::time::{Duration, Instant};

    fn sample(tracker: &mut PresenceTracker, start: Instant, second: u64, present: bool) {
        tracker.observe(present, start + Duration::from_secs(second));
    }

    #[test]
    fn presence_requires_two_online_and_four_offline_samples() {
        let start = Instant::now();
        let mut tracker = PresenceTracker::default();
        sample(&mut tracker, start, 0, true);
        assert_eq!(tracker.state, SerialPresenceState::Checking);
        sample(&mut tracker, start, 2, true);
        assert_eq!(tracker.state, SerialPresenceState::Online);
        for second in [4, 6, 8] {
            sample(&mut tracker, start, second, false);
            assert_eq!(tracker.state, SerialPresenceState::Online);
        }
        sample(&mut tracker, start, 10, false);
        assert_eq!(tracker.state, SerialPresenceState::Offline);
        sample(&mut tracker, start, 12, true);
        assert_eq!(tracker.state, SerialPresenceState::Offline);
        sample(&mut tracker, start, 14, true);
        assert_eq!(tracker.state, SerialPresenceState::Online);
    }

    #[test]
    fn a_short_disappearance_does_not_change_the_icon() {
        let start = Instant::now();
        let mut tracker = PresenceTracker::default();
        for (second, present) in [(0, true), (2, true), (4, false), (6, false), (8, true)] {
            sample(&mut tracker, start, second, present);
        }
        assert_eq!(tracker.state, SerialPresenceState::Online);
        assert_eq!(tracker.drops.len(), 1);
    }

    #[test]
    fn one_long_disappearance_is_only_one_dropout() {
        let start = Instant::now();
        let mut tracker = PresenceTracker::default();
        sample(&mut tracker, start, 0, true);
        sample(&mut tracker, start, 2, true);
        for second in (4..=60).step_by(2) {
            sample(&mut tracker, start, second, false);
        }
        assert_eq!(tracker.state, SerialPresenceState::Offline);
        assert_eq!(tracker.drops.len(), 1);
        assert!(!tracker.unstable);
    }

    #[test]
    fn three_raw_dropouts_latch_grey_until_fifteen_seconds_of_stability() {
        let start = Instant::now();
        let mut tracker = PresenceTracker::default();
        for (second, present) in [
            (0, true),
            (2, true),
            (4, false),
            (6, true),
            (8, false),
            (10, true),
            (12, false),
        ] {
            sample(&mut tracker, start, second, present);
        }
        assert_eq!(tracker.state, SerialPresenceState::Unstable);
        for second in (14..=28).step_by(2) {
            sample(&mut tracker, start, second, true);
            assert_eq!(tracker.state, SerialPresenceState::Unstable);
        }
        sample(&mut tracker, start, 30, true);
        assert_eq!(tracker.state, SerialPresenceState::Online);
        assert!(tracker.drops.is_empty());
        sample(&mut tracker, start, 32, false);
        assert_eq!(tracker.state, SerialPresenceState::Online);
    }

    #[test]
    fn recovery_is_restarted_by_another_dropout_or_scan_error() {
        let start = Instant::now();
        let mut tracker = PresenceTracker {
            unstable: true,
            ..Default::default()
        };
        for second in (0..=14).step_by(2) {
            sample(&mut tracker, start, second, true);
        }
        sample(&mut tracker, start, 16, false);
        for second in (18..=32).step_by(2) {
            sample(&mut tracker, start, second, true);
        }
        tracker.interrupt();
        for second in (34..=48).step_by(2) {
            sample(&mut tracker, start, second, true);
            assert_eq!(tracker.state, SerialPresenceState::Unstable);
        }
        sample(&mut tracker, start, 50, true);
        assert_eq!(tracker.state, SerialPresenceState::Online);
    }

    #[test]
    fn old_dropouts_expire_and_missing_ports_at_startup_become_offline() {
        let start = Instant::now();
        let mut tracker = PresenceTracker::default();
        for second in (0..=6).step_by(2) {
            sample(&mut tracker, start, second, false);
        }
        assert_eq!(tracker.state, SerialPresenceState::Offline);
        for second in (8..=72).step_by(2) {
            sample(&mut tracker, start, second, second != 12 && second != 16);
        }
        sample(&mut tracker, start, 74, false);
        assert_eq!(tracker.state, SerialPresenceState::Online);
        assert_eq!(tracker.drops.len(), 2);
    }

    #[test]
    fn scan_failures_and_long_gaps_are_not_evidence_of_offline_or_recovery() {
        let start = Instant::now();
        let mut tracker = PresenceTracker::default();
        sample(&mut tracker, start, 0, true);
        sample(&mut tracker, start, 2, true);
        tracker.interrupt();
        assert_eq!(tracker.state, SerialPresenceState::Unknown);
        assert!(tracker.drops.is_empty());
        sample(&mut tracker, start, 4, true);
        assert_eq!(tracker.state, SerialPresenceState::Unknown);
        sample(&mut tracker, start, 30, true);
        assert_eq!(tracker.state, SerialPresenceState::Checking);
        sample(&mut tracker, start, 32, true);
        assert_eq!(tracker.state, SerialPresenceState::Online);
    }

    #[test]
    fn device_matching_respects_platform_path_rules() {
        assert_eq!(device_key(" COM3 "), "COM3");
        if cfg!(windows) {
            assert_eq!(device_key("\\\\.\\com3"), "COM3");
        } else {
            assert_ne!(device_key("/dev/ttyUSB0"), device_key("/dev/ttyusb0"));
        }
    }

    #[gpui_kit::test]
    fn a_scan_started_before_host_edits_cannot_update_new_targets(cx: &mut TestAppContext) {
        let store = cx.new(|_| HostStore::empty());
        let presence = cx.new(|cx| SerialPresence::new(store.clone(), cx));
        let old_generation = presence.read_with(cx, |presence, _| presence.generation);
        store.update(cx, |store, cx| {
            store.insert(
                HostDraft::serial("console", "COM3", SerialSettings::default(), None),
                cx,
            );
        });
        cx.run_until_parked();
        presence.update(cx, |presence, cx| {
            assert_ne!(presence.generation, old_generation);
            presence.apply_scan(old_generation, Ok(vec!["COM3".into()]), cx);
            let tracker = presence.devices.get("COM3").unwrap();
            assert_eq!(tracker.state, SerialPresenceState::Checking);
            assert_eq!(tracker.samples, 0);
            assert!(tracker.last_sample.is_none());
        });
    }
}
