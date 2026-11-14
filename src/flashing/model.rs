use std::collections::BTreeMap;

/// How much of the device's storage PhoenixConsole erases before writing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
}

/// What one line of PhoenixConsole's output means to the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParsedLine {
    Listening,
    NoDeviceTimeout,
    DeviceDetected,
    Progress { device: u32, percent: u8 },
    Stage(String),
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
    if (text.starts_with("device:[") && text.ends_with("fes plugin"))
        || (text.starts_with("Detected ") && text.ends_with(" device, flashing..."))
    {
        return ParsedLine::DeviceDetected;
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
        return ParsedLine::Stage(partition.trim().to_string());
    }

    ParsedLine::Other
}

fn bracketed_number(text: &str) -> Option<u32> {
    let start = text.find('[')? + 1;
    let end = text[start..].find(']')? + start;
    text[start..end].trim().parse().ok()
}

/// The progress of every device seen in one run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlashProgress {
    devices: BTreeMap<u32, DeviceProgress>,
    stage: Option<String>,
    result: Option<bool>,
}

impl FlashProgress {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn apply(&mut self, parsed: ParsedLine) {
        match parsed {
            ParsedLine::Progress { device, percent } => {
                let entry = self.devices.entry(device).or_default();
                // A retry or a duplicated line must not make the bar jump
                // backwards. A new run starts with a fresh `FlashProgress`.
                entry.percent = entry.percent.max(percent);
            }
            ParsedLine::Stage(stage) => {
                self.stage = Some(stage.clone());
                if let Some((_, device)) = self.devices.iter_mut().next() {
                    device.stage = Some(stage);
                }
            }
            ParsedLine::DeviceFinished { device, success } => {
                let entry = self.devices.entry(device).or_default();
                entry.finished = true;
                entry.success = success;
                if success {
                    entry.percent = 100;
                }
            }
            ParsedLine::RunFinished { success } => {
                self.result = Some(self.result != Some(false) && success)
            }
            ParsedLine::Listening
            | ParsedLine::NoDeviceTimeout
            | ParsedLine::DeviceDetected
            | ParsedLine::Other => {}
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

    pub fn completion(&self, exit_code: Option<i32>, timed_out: bool) -> Completion {
        if self.result == Some(true)
            && exit_code == Some(0)
            && self
                .devices
                .values()
                .all(|device| !device.finished || device.success)
        {
            Completion::Success
        } else if timed_out {
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
    Waiting,
    Burning,
    Failed,
    Stopped,
}

impl Phase {
    pub fn is_waiting(self) -> bool {
        self == Phase::Waiting
    }

    pub fn is_burning(self) -> bool {
        self == Phase::Burning
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Completion, DeviceProgress, EraseMode, FlashProgress, ParsedLine, Phase, parse_line,
    };

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
        assert_eq!(progress.completion(Some(1), false), Completion::Failed);
        progress.apply(parse_line("line[58], device burn result=0"));
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
        assert_eq!(
            progress.devices().next().map(|(_, device)| device),
            Some(&DeviceProgress {
                percent: 100,
                stage: None,
                finished: true,
                success: true,
            })
        );
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
}
