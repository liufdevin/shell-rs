use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

pub(super) const MAX_DEVICES: u32 = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FirmwarePartition {
    pub index: u32,
    pub name: String,
}

pub(super) fn parse_partitions(output: &str) -> Result<Vec<FirmwarePartition>, String> {
    let (_, listing) = output
        .split_once("Firmware partition list:")
        .ok_or_else(|| output.trim().to_string())?;
    let mut partitions = Vec::new();
    for line in listing.lines() {
        let Some((index, name)) = line.trim().split_once('.') else {
            continue;
        };
        let Ok(index) = index.trim().parse::<u32>() else {
            continue;
        };
        let name = name.trim();
        if name.is_empty()
            || name.len() > 256
            || partitions.len() >= 256
            || partitions
                .iter()
                .any(|part: &FirmwarePartition| part.index == index)
        {
            return Err("Invalid firmware partition list".into());
        }
        partitions.push(FirmwarePartition {
            index,
            name: name.to_string(),
        });
    }
    Ok(partitions)
}

/// How much of the device's storage PhoenixConsole erases before writing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EraseMode {
    /// `-e 10`: upgrade mode, keep user data.
    #[default]
    KeepData,
    /// `-e 11`: upgrade mode, erase every logical partition.
    EraseLogical,
    /// `-e 12`: upgrade mode, erase everything.
    EraseAll,
}

impl EraseMode {
    pub const ALL: [EraseMode; 3] = [
        EraseMode::KeepData,
        EraseMode::EraseLogical,
        EraseMode::EraseAll,
    ];

    pub fn flag(self) -> &'static str {
        match self {
            EraseMode::KeepData => "10",
            EraseMode::EraseLogical => "11",
            EraseMode::EraseAll => "12",
        }
    }
}

/// One device's progress in the current run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceProgress {
    pub percent: u8,
    pub stage: Option<String>,
    pub finished: bool,
    pub success: bool,
    pub interrupted: bool,
    pub started_at: Option<Instant>,
    pub finished_at: Option<Instant>,
}

impl DeviceProgress {
    pub fn elapsed(&self, now: Instant) -> Duration {
        self.started_at.map_or(Duration::ZERO, |started| {
            self.finished_at
                .unwrap_or(now)
                .saturating_duration_since(started)
        })
    }
}

/// What one line of PhoenixConsole's output means to the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParsedLine {
    Listening,
    NoDeviceTimeout,
    DeviceDetected,
    DeviceStarted { device: u32 },
    Progress { device: u32, percent: u8 },
    Stage(String),
    DeviceStage { device: u32, stage: String },
    DeviceMissing { device: u32 },
    DeviceFinished { device: u32, success: bool },
    RunFinished { success: bool },
    Other,
}

/// Read the progress and result lines PhoenixConsole prints.
///
/// The tool has used both `device [1] progress = 12%` and
/// `device id[1] progress = 12%`, so the device number is read from the
/// first bracketed number rather than from one fixed prefix.
pub fn parse_line(line: &str) -> ParsedLine {
    let text = line.trim();
    let text = text
        .strip_prefix("line[")
        .and_then(|line| line.split_once(']'))
        .map_or(text, |(_, message)| {
            message.trim_start_matches([',', ':', ' '])
        });
    if text == "usbproproc service is ready, listening device..." {
        return ParsedLine::Listening;
    }
    if text.starts_with("error: no device checkIn in ") {
        return ParsedLine::NoDeviceTimeout;
    }
    if text.ends_with("fes plugin")
        && let Some(device) = text.strip_prefix("device:").and_then(bracketed_number)
    {
        return ParsedLine::DeviceStarted { device };
    }
    if text
        .strip_prefix("Detected ")
        .and_then(|text| text.strip_suffix(" device, flashing..."))
        .and_then(|count| count.parse::<u32>().ok())
        .is_some_and(|count| count > 0)
    {
        return ParsedLine::DeviceDetected;
    }

    if text.contains("device not plug in")
        && text.ends_with("result=No device")
        && let Some(device) = text.strip_prefix("device:").and_then(bracketed_number)
    {
        return ParsedLine::DeviceMissing { device };
    }

    if let Some(result) = text.strip_prefix("device burn result=") {
        let success = result.trim() == "1";
        return ParsedLine::RunFinished { success };
    }

    if text.contains("burn finish")
        && let Some(device) = text.strip_prefix("device:").and_then(bracketed_number)
        && let Some((_, result)) = text.split_once("result=")
    {
        return ParsedLine::DeviceFinished {
            device,
            success: result.trim().eq_ignore_ascii_case("Success"),
        };
    }

    if text.contains("progress")
        && let Some(percent) = text
            .split_once('%')
            .and_then(|(before, _)| before.rsplit_once('='))
            .and_then(|(_, value)| value.trim().parse::<u8>().ok())
        && let Some(device) = text.strip_prefix("device ").and_then(bracketed_number)
        && percent <= 100
    {
        return ParsedLine::Progress { device, percent };
    }

    if let Some((_, rest)) = text.split_once("Burn ")
        && let Some((partition, _)) = rest.split_once(" partition data")
    {
        if let Some(device) = text
            .strip_prefix("device:")
            .or_else(|| text.strip_prefix("device "))
            .and_then(bracketed_number)
        {
            return ParsedLine::DeviceStage {
                device,
                stage: partition.trim().to_string(),
            };
        }
        return ParsedLine::Stage(partition.trim().to_string());
    }

    ParsedLine::Other
}

fn bracketed_number(text: &str) -> Option<u32> {
    let start = text.find('[')? + 1;
    let end = text[start..].find(']')? + start;
    text[start..end]
        .trim()
        .parse()
        .ok()
        .filter(|device| *device <= MAX_DEVICES)
}

/// The progress of every device seen in one run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlashProgress {
    devices: BTreeMap<u32, DeviceProgress>,
    stage: Option<String>,
}

impl FlashProgress {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    #[cfg(test)]
    pub fn apply(&mut self, parsed: ParsedLine) {
        self.apply_at(parsed, Instant::now());
    }

    pub fn is_activity(&self, parsed: &ParsedLine) -> bool {
        match parsed {
            ParsedLine::DeviceStarted { device } => *device <= MAX_DEVICES,
            ParsedLine::Progress { device, .. }
            | ParsedLine::DeviceFinished { device, .. }
            | ParsedLine::DeviceStage { device, .. } => {
                *device <= MAX_DEVICES
                    && self.devices.get(device).is_none_or(|state| !state.finished)
            }
            ParsedLine::DeviceDetected => true,
            ParsedLine::Stage(_) => self.is_empty() || self.has_active(),
            _ => false,
        }
    }

    pub fn apply_at(&mut self, parsed: ParsedLine, at: Instant) {
        match parsed {
            ParsedLine::DeviceStarted { device } if device <= MAX_DEVICES => {
                if self.devices.get(&device).is_none_or(|state| {
                    state.finished && state.finished_at.is_none_or(|finished| at > finished)
                }) && (self.devices.contains_key(&device)
                    || self.devices.len() < MAX_DEVICES as usize)
                {
                    self.devices.insert(
                        device,
                        DeviceProgress {
                            started_at: Some(at),
                            ..Default::default()
                        },
                    );
                }
            }
            ParsedLine::Progress { device, percent } => {
                if let Some(entry) = self.active_entry(device, at) {
                    entry.percent = entry.percent.max(percent.min(100));
                }
            }
            ParsedLine::Stage(stage) => {
                self.stage = Some(stage.clone());
                if self
                    .devices
                    .values()
                    .filter(|state| !state.finished)
                    .count()
                    == 1
                    && let Some(device) = self.devices.values_mut().find(|state| !state.finished)
                {
                    device.stage = Some(stage);
                }
            }
            ParsedLine::DeviceStage { device, stage } => {
                if let Some(entry) = self.active_entry(device, at) {
                    entry.stage = Some(stage);
                }
            }
            ParsedLine::DeviceFinished { device, success } => {
                if let Some(entry) = self.active_entry(device, at) {
                    entry.finished = true;
                    entry.success = success;
                    entry.finished_at = Some(at);
                    if success {
                        entry.percent = 100;
                    }
                }
            }
            ParsedLine::Listening
            | ParsedLine::NoDeviceTimeout
            | ParsedLine::DeviceDetected
            | ParsedLine::DeviceStarted { .. }
            | ParsedLine::DeviceMissing { .. }
            | ParsedLine::RunFinished { .. }
            | ParsedLine::Other => {}
        }
    }

    fn active_entry(&mut self, device: u32, at: Instant) -> Option<&mut DeviceProgress> {
        if device > MAX_DEVICES
            || (!self.devices.contains_key(&device) && self.devices.len() >= MAX_DEVICES as usize)
        {
            return None;
        }
        let entry = self
            .devices
            .entry(device)
            .or_insert_with(|| DeviceProgress {
                started_at: Some(at),
                ..Default::default()
            });
        (!entry.finished && entry.started_at.is_none_or(|started| at >= started)).then_some(entry)
    }

    pub fn has_active(&self) -> bool {
        self.devices.values().any(|state| !state.finished)
    }

    pub fn all_finished(&self) -> bool {
        !self.is_empty() && !self.has_active()
    }

    pub fn interrupt_active(&mut self, at: Instant) {
        for state in self.devices.values_mut().filter(|state| !state.finished) {
            state.finished = true;
            state.success = false;
            state.interrupted = true;
            state.finished_at = Some(at);
        }
    }

    pub fn devices(&self) -> impl Iterator<Item = (&u32, &DeviceProgress)> {
        self.devices.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn stage(&self) -> Option<&str> {
        self.stage.as_deref()
    }

    pub fn completion(&self, _exit_code: Option<i32>, timed_out: bool) -> Completion {
        let confirmed_devices = !self.devices.is_empty()
            && self
                .devices
                .values()
                .all(|device| device.finished && device.success);
        if confirmed_devices {
            Completion::Success
        } else if timed_out && !self.has_active() {
            Completion::WaitAgain
        } else {
            Completion::Failed
        }
    }

    /// The average of the devices seen. A single device is exactly its own
    /// percentage; several devices move the bar by their mean.
    pub fn overall_percent(&self) -> Option<u8> {
        if self.devices.is_empty() {
            return None;
        }
        let total: u32 = self
            .devices
            .values()
            .map(|device| u32::from(device.percent))
            .sum();
        Some((total / self.devices.len() as u32) as u8)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Completion {
    Success,
    WaitAgain,
    Failed,
}

/// The state of the page, from no firmware to a finished run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    NoFirmware,
    ReadingFirmware,
    FirmwareMissing,
    FirmwareUpdating,
    Ready,
    Waiting,
    Preparing,
    Unclaimed,
    Burning,
    Failed,
    Stopped,
}

impl Phase {
    pub fn is_waiting(self) -> bool {
        self == Phase::Waiting
    }

    pub fn is_burning(self) -> bool {
        matches!(self, Phase::Preparing | Phase::Burning)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Completion, EraseMode, FlashProgress, ParsedLine, Phase, parse_line, parse_partitions,
    };

    #[test]
    fn partition_listing_preserves_tool_indices_and_case() {
        let partitions = parse_partitions(
            "List partitions of image: firmware.img\nFirmware partition list:\n    0. BOOT-RESOURCE\n    1. ENV\n    2. BOOT\n    3. ROOTFS\n",
        ).unwrap();
        assert_eq!(
            partitions
                .iter()
                .map(|part| (part.index, part.name.as_str()))
                .collect::<Vec<_>>(),
            [(0, "BOOT-RESOURCE"), (1, "ENV"), (2, "BOOT"), (3, "ROOTFS")]
        );
        assert!(
            parse_partitions("Firmware partition list:\n  (no partition found)\n")
                .unwrap()
                .is_empty()
        );
        assert!(parse_partitions("Firmware partition list:\n  1. BOOT\n  1. ROOTFS\n").is_err());
        assert!(parse_partitions("this tool does not support --list-parts").is_err());
    }

    #[test]
    fn reads_both_progress_prefixes() {
        assert_eq!(
            parse_line("line[35], device [1] progress = 12%"),
            ParsedLine::Progress {
                device: 1,
                percent: 12
            }
        );
        assert_eq!(
            parse_line("device id[2] progress = 87%"),
            ParsedLine::Progress {
                device: 2,
                percent: 87
            }
        );
    }

    #[test]
    fn reads_stage_and_results() {
        assert_eq!(
            parse_line("Detected 0 device, flashing..."),
            ParsedLine::Other
        );
        assert_eq!(
            parse_line("Detected 1 device, flashing..."),
            ParsedLine::DeviceDetected
        );
        assert_eq!(
            parse_line("Burn bootloader_a partition data"),
            ParsedLine::Stage("bootloader_a".into())
        );
        assert_eq!(
            parse_line("device:[1] burn finish, result=Success"),
            ParsedLine::DeviceFinished {
                device: 1,
                success: true
            }
        );
        assert_eq!(
            parse_line("line[77], device burn result=1"),
            ParsedLine::RunFinished { success: true }
        );
        assert_eq!(
            parse_line("device burn result=00"),
            ParsedLine::RunFinished { success: false }
        );
    }

    #[test]
    fn only_confirmed_success_or_idle_timeout_rearms() {
        let mut progress = FlashProgress::default();
        assert_eq!(progress.completion(Some(0), false), Completion::Failed);
        let timeout =
            parse_line("line[50], error: no device checkIn in 30 senconds, will end this process!");
        assert_eq!(timeout, ParsedLine::NoDeviceTimeout);
        assert_eq!(progress.completion(Some(1), true), Completion::WaitAgain);
        progress.apply(parse_line("line[55], device [1] progress = 100%"));
        assert_eq!(progress.completion(Some(0), false), Completion::Failed);
        progress.apply(parse_line(
            "line[56], device:[1] burn finish, result=Success",
        ));
        progress.apply(parse_line("line[57], device burn result=1"));
        assert_eq!(progress.completion(Some(0), false), Completion::Success);
        assert_eq!(progress.completion(Some(1), false), Completion::Success);
        progress.apply(parse_line("line[58], device burn result=0"));
        assert_eq!(progress.completion(Some(0), false), Completion::Success);
    }

    #[test]
    fn unused_device_slots_do_not_override_confirmed_device_success() {
        let mut progress = FlashProgress::default();
        for line in [
            "line[2153], device [3] progress = 83%",
            "line[2172], device:[3] burn finish, result=Success",
            "line[2192], device:[1]: None, device not plug in, result=No device",
            "line[2199], device burn result=2",
            "line[2235], Time out, result = 2",
        ] {
            progress.apply(parse_line(line));
        }
        assert_eq!(progress.devices().count(), 1);
        assert_eq!(progress.completion(Some(2), false), Completion::Success);
        progress.apply(parse_line("device:[4] burn finish, result=Failed"));
        assert_eq!(progress.completion(Some(0), false), Completion::Failed);
    }

    #[test]
    fn unfinished_devices_and_percent_alone_are_not_confirmed_success() {
        let mut progress = FlashProgress::default();
        progress.apply(parse_line("device [3] progress = 100%"));
        progress.apply(parse_line("device burn result=2"));
        assert_eq!(progress.completion(Some(0), false), Completion::Failed);
        progress.apply(parse_line("device:[4] burn finish, result=Success"));
        assert_eq!(progress.completion(Some(0), false), Completion::Failed);
    }

    #[test]
    fn progress_never_goes_backwards_and_success_completes_it() {
        let mut progress = FlashProgress::default();
        progress.apply(ParsedLine::Progress {
            device: 1,
            percent: 45,
        });
        progress.apply(ParsedLine::Progress {
            device: 1,
            percent: 30,
        });
        assert_eq!(progress.overall_percent(), Some(45));
        progress.apply(ParsedLine::DeviceFinished {
            device: 1,
            success: true,
        });
        assert_eq!(progress.overall_percent(), Some(100));
        let device = progress.devices().next().unwrap().1;
        assert_eq!(device.percent, 100);
        assert!(device.finished && device.success);
        assert!(device.started_at.is_some() && device.finished_at.is_some());
    }

    #[test]
    fn several_devices_average_their_progress() {
        let mut progress = FlashProgress::default();
        for (device, percent) in [(1, 20), (2, 60)] {
            progress.apply(ParsedLine::Progress { device, percent });
        }
        assert_eq!(progress.overall_percent(), Some(40));
    }

    #[test]
    fn erase_modes_use_the_documented_flags() {
        assert_eq!(EraseMode::default(), EraseMode::KeepData);
        assert_eq!(EraseMode::KeepData.flag(), "10");
        assert_eq!(EraseMode::EraseLogical.flag(), "11");
        assert_eq!(EraseMode::EraseAll.flag(), "12");
    }

    #[test]
    fn phase_starts_without_firmware() {
        assert_eq!(Phase::default(), Phase::NoFirmware);
    }

    #[test]
    fn eight_devices_have_independent_progress_clocks_results_and_slot_reuse() {
        use std::time::{Duration, Instant};
        let start = Instant::now();
        let mut progress = FlashProgress::default();
        for device in 1..=8 {
            progress.apply_at(
                ParsedLine::DeviceStarted { device },
                start + Duration::from_secs(device as u64),
            );
            progress.apply_at(
                ParsedLine::Progress {
                    device,
                    percent: device as u8 * 10,
                },
                start + Duration::from_secs(10),
            );
        }
        assert_eq!(progress.devices().count(), 8);
        progress.apply_at(
            ParsedLine::DeviceFinished {
                device: 1,
                success: true,
            },
            start + Duration::from_secs(30),
        );
        progress.apply_at(
            ParsedLine::DeviceFinished {
                device: 3,
                success: false,
            },
            start + Duration::from_secs(35),
        );
        progress.apply_at(
            ParsedLine::RunFinished { success: true },
            start + Duration::from_secs(36),
        );
        progress.apply_at(
            ParsedLine::RunFinished { success: false },
            start + Duration::from_secs(37),
        );
        let now = start + Duration::from_secs(40);
        assert_eq!(progress.devices[&1].elapsed(now), Duration::from_secs(29));
        assert_eq!(progress.devices[&2].elapsed(now), Duration::from_secs(38));
        assert_eq!(progress.devices[&3].elapsed(now), Duration::from_secs(32));
        assert_eq!(progress.devices[&2].percent, 20);
        assert!(!progress.devices[&2].finished);
        let second = progress.devices[&2].clone();
        progress.apply_at(
            ParsedLine::Progress {
                device: 1,
                percent: 1,
            },
            now,
        );
        assert!(progress.devices[&1].success);
        progress.apply_at(ParsedLine::DeviceStarted { device: 1 }, now);
        assert_eq!(progress.devices[&1].percent, 0);
        assert_eq!(progress.devices[&1].elapsed(now), Duration::ZERO);
        assert_eq!(progress.devices[&2], second);
        progress.apply_at(ParsedLine::DeviceStarted { device: 2 }, now);
        assert_eq!(progress.devices[&2], second);
        progress.apply_at(
            ParsedLine::DeviceStage {
                device: 2,
                stage: "ROOTFS".into(),
            },
            now,
        );
        assert_eq!(progress.devices[&2].stage.as_deref(), Some("ROOTFS"));
        assert!(progress.devices[&4].stage.is_none());
        progress.apply_at(ParsedLine::Stage("ambiguous".into()), now);
        assert!(progress.devices[&4].stage.is_none());
        progress.apply_at(
            ParsedLine::DeviceFinished {
                device: 8,
                success: true,
            },
            now,
        );
        progress.interrupt_active(now + Duration::from_secs(20));
        assert!(progress.devices[&8].success && !progress.devices[&8].interrupted);
        assert!(!progress.devices[&3].success && !progress.devices[&3].interrupted);
        assert!(progress.devices[&2].interrupted);
        assert_eq!(
            progress.devices[&2].elapsed(now + Duration::from_secs(100)),
            Duration::from_secs(58)
        );
        assert!(progress.all_finished());
    }

    #[test]
    fn unused_slots_missing_devices_and_invalid_numbers_never_affect_other_devices() {
        let mut progress = FlashProgress::default();
        progress.apply(parse_line("device:[2] fes plugin"));
        progress.apply(parse_line("device [2] progress = 38%"));
        progress.apply(parse_line("device:[3] fes plugin"));
        progress.apply(parse_line(
            "device:[1]: None, device not plug in, result=No device",
        ));
        assert_eq!(progress.devices().count(), 2);
        progress.apply(parse_line(
            "device:[2]: None, device not plug in, result=No device",
        ));
        assert!(!progress.devices[&2].finished);
        progress.apply(parse_line("device:[2] burn finish, result=Failed"));
        assert!(progress.devices[&2].finished && !progress.devices[&2].success);
        assert!(!progress.devices[&3].finished);
        for device in [9, u32::MAX] {
            assert_eq!(
                parse_line(&format!("device:[{device}] fes plugin")),
                ParsedLine::Other
            );
            progress.apply(ParsedLine::Progress {
                device,
                percent: 100,
            });
        }
        assert_eq!(progress.devices().count(), 2);
        assert_eq!(
            parse_line("device:[3] Burn ROOTFS partition data"),
            ParsedLine::DeviceStage {
                device: 3,
                stage: "ROOTFS".into()
            }
        );
    }

    #[test]
    fn zero_based_tool_ids_are_preserved_without_exceeding_eight_slots() {
        let mut progress = FlashProgress::default();
        for device in 0..8 {
            progress.apply(parse_line(&format!("device:[{device}] fes plugin")));
        }
        assert_eq!(progress.devices().count(), 8);
        assert!(progress.devices.contains_key(&0));
        progress.apply(parse_line("device:[8] fes plugin"));
        progress.apply(parse_line("device [8] progress = 12%"));
        assert_eq!(progress.devices().count(), 8);
        assert!(!progress.devices.contains_key(&8));
    }
}
