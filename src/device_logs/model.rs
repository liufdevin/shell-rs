use std::{collections::VecDeque, rc::Rc};

use super::{LogOptions, commands::Source};

pub(super) const MAX_LINES: usize = 10_000;
const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogLevel {
    #[default]
    All,
    Verbose,
    Debug,
    Info,
    Warning,
    Error,
    Fatal,
}

impl LogLevel {
    pub const ALL: [Self; 7] = [
        Self::All,
        Self::Verbose,
        Self::Debug,
        Self::Info,
        Self::Warning,
        Self::Error,
        Self::Fatal,
    ];
}

#[derive(Clone, Debug)]
pub(super) struct LogLine {
    pub id: u64,
    pub text: Rc<str>,
    pub level: LogLevel,
    pub pid: Option<u32>,
    pub tag: Option<Rc<str>>,
    pub application: bool,
    pub time: Option<chrono::NaiveDateTime>,
}

pub(super) struct LogBuffer {
    lines: VecDeque<LogLine>,
    snapshot: Vec<String>,
    next_id: u64,
    bytes: usize,
    level: LogLevel,
    pub trimmed: bool,
    pub gap: bool,
    clock: Option<chrono::NaiveDateTime>,
    android_pid: Option<u32>,
    android_tag: Option<Rc<str>>,
    android_stamp: Option<chrono::NaiveDateTime>,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self {
            lines: VecDeque::new(),
            snapshot: Vec::new(),
            next_id: 1,
            bytes: 0,
            level: LogLevel::Info,
            trimmed: false,
            gap: false,
            clock: None,
            android_pid: None,
            android_tag: None,
            android_stamp: None,
        }
    }
}

impl LogBuffer {
    pub fn ingest(&mut self, source: Source, output: &str, baseline: bool) {
        let mut pids = None;
        for line in output.lines() {
            if let Some(value) = line.strip_prefix("__SHELLLF_LOG_CLOCK__ ") {
                self.clock =
                    chrono::NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%dT%H:%M:%S").ok();
            }
            if let Some(value) = line.strip_prefix("__SHELLLF_LOG_PIDS__ ") {
                pids = Some(
                    value
                        .split_whitespace()
                        .filter_map(|pid| pid.parse::<u32>().ok())
                        .collect::<Vec<_>>(),
                );
            }
        }
        let snapshot: Vec<String> = output
            .lines()
            .filter(|line| !line.starts_with("--------- ") && !line.starts_with("__SHELLLF_LOG_"))
            .map(|line| line.trim_end_matches('\r').to_string())
            .collect();
        let overlap = overlap(&self.snapshot, &snapshot);
        self.gap |= !self.snapshot.is_empty() && !snapshot.is_empty() && overlap == 0;
        if !baseline {
            for text in &snapshot[overlap..] {
                let level = parse_level(source, text).unwrap_or(self.level);
                let fields = text.split_whitespace().collect::<Vec<_>>();
                let pid = (source == Source::Android && fields.len() >= 6)
                    .then(|| fields.get(2)?.parse().ok())
                    .flatten();
                let tag = (source == Source::Android && pid.is_some())
                    .then(|| {
                        text.split_whitespace()
                            .skip(5)
                            .collect::<Vec<_>>()
                            .join(" ")
                            .split_once(':')
                            .map(|(tag, _)| Rc::from(tag.trim()))
                    })
                    .flatten();
                let time = if source == Source::Android {
                    android_time(text, self.clock)
                } else {
                    None
                };
                if source == Source::Android && pid.is_some() {
                    self.android_pid = pid;
                    self.android_tag = tag;
                    self.android_stamp = time;
                }
                let (pid, tag, time) = if source == Source::Android {
                    (
                        self.android_pid,
                        self.android_tag.clone(),
                        self.android_stamp,
                    )
                } else {
                    (None, None, None)
                };
                let text = if source == Source::Journal {
                    journal_text(text)
                } else {
                    text.clone()
                };
                self.level = level;
                self.bytes += text.len();
                self.lines.push_back(LogLine {
                    id: self.next_id,
                    text: Rc::from(text.as_str()),
                    level,
                    pid,
                    tag,
                    application: pids
                        .as_ref()
                        .is_none_or(|pids| pid.is_some_and(|pid| pids.contains(&pid))),
                    time,
                });
                self.next_id += 1;
                while self.lines.len() > MAX_LINES || self.bytes > MAX_BYTES {
                    if let Some(line) = self.lines.pop_front() {
                        self.bytes -= line.text.len();
                        self.trimmed = true;
                    }
                }
            }
        }
        self.snapshot = snapshot;
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.bytes = 0;
        self.trimmed = false;
        self.gap = false;
    }
    pub fn lines(&self) -> Vec<LogLine> {
        self.lines.iter().cloned().collect()
    }
    pub fn clock(&self) -> Option<chrono::NaiveDateTime> {
        self.clock
    }
}

pub(super) fn matches_options(
    line: &LogLine,
    options: &LogOptions,
    clock: Option<chrono::NaiveDateTime>,
) -> bool {
    (options.object.is_empty() || line.application)
        && (options.pid.is_empty() || line.pid == options.pid.parse().ok())
        && (options.tag.is_empty()
            || line
                .tag
                .as_ref()
                .is_some_and(|tag| tag.as_ref() == options.tag))
        && options
            .period
            .seconds()
            .is_none_or(|seconds| match (line.time, clock) {
                (Some(time), Some(now)) => {
                    now.signed_duration_since(time).num_seconds() <= seconds as i64
                }
                _ => true,
            })
}

fn android_time(text: &str, clock: Option<chrono::NaiveDateTime>) -> Option<chrono::NaiveDateTime> {
    use chrono::Datelike as _;
    let clock = clock?;
    let stamp = text.get(..18)?;
    let mut time = chrono::NaiveDateTime::parse_from_str(
        &format!("{}-{stamp}", clock.year()),
        "%Y-%m-%d %H:%M:%S%.f",
    )
    .ok()?;
    if time.signed_duration_since(clock).num_days() > 1 {
        time = time.with_year(clock.year() - 1)?;
    }
    Some(time)
}

fn journal_text(text: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return text.into();
    };
    let message = value["MESSAGE"].as_str().unwrap_or_default();
    let unit = value["_SYSTEMD_UNIT"]
        .as_str()
        .or_else(|| value["SYSLOG_IDENTIFIER"].as_str())
        .unwrap_or("journal");
    let time = value["__REALTIME_TIMESTAMP"]
        .as_str()
        .and_then(|value| value.parse::<i64>().ok())
        .and_then(chrono::DateTime::from_timestamp_micros)
        .map(|time| time.format("%Y-%m-%d %H:%M:%S%.3f UTC").to_string())
        .unwrap_or_default();
    format!("{time} {unit}: {message}")
}

pub(super) fn matches(line: &LogLine, minimum: LogLevel, keyword: &str) -> bool {
    (minimum == LogLevel::All || line.level as usize >= minimum as usize)
        && (keyword.is_empty() || line.text.to_lowercase().contains(keyword))
}

fn parse_level(source: Source, text: &str) -> Option<LogLevel> {
    match source {
        Source::Android => match text.split_whitespace().nth(4)? {
            "V" => Some(LogLevel::Verbose),
            "D" => Some(LogLevel::Debug),
            "I" => Some(LogLevel::Info),
            "W" => Some(LogLevel::Warning),
            "E" => Some(LogLevel::Error),
            "F" | "A" => Some(LogLevel::Fatal),
            _ => None,
        },
        Source::Kernel => {
            let (level, _) = text.strip_prefix('<')?.split_once('>')?;
            match level.parse::<u32>().ok()? % 8 {
                0..=2 => Some(LogLevel::Fatal),
                3 => Some(LogLevel::Error),
                4 => Some(LogLevel::Warning),
                5 | 6 => Some(LogLevel::Info),
                7 => Some(LogLevel::Debug),
                _ => None,
            }
        }
        Source::Journal => {
            let value = serde_json::from_str::<serde_json::Value>(text).ok()?;
            let level = value["PRIORITY"].as_str()?.parse::<u32>().ok()?;
            Some(priority(level))
        }
        Source::File => {
            let text = text.to_ascii_lowercase();
            Some(if text.contains("fatal") || text.contains("panic") {
                LogLevel::Fatal
            } else if text.contains("error") || text.contains("failed") {
                LogLevel::Error
            } else if text.contains("warn") {
                LogLevel::Warning
            } else if text.contains("debug") {
                LogLevel::Debug
            } else {
                LogLevel::Info
            })
        }
    }
}

fn priority(value: u32) -> LogLevel {
    match value % 8 {
        0..=2 => LogLevel::Fatal,
        3 => LogLevel::Error,
        4 => LogLevel::Warning,
        7 => LogLevel::Debug,
        _ => LogLevel::Info,
    }
}

fn overlap(previous: &[String], current: &[String]) -> usize {
    if current.is_empty() {
        return 0;
    }
    let mut prefix = vec![0; current.len()];
    for index in 1..current.len() {
        let mut matched = prefix[index - 1];
        while matched > 0 && current[index] != current[matched] {
            matched = prefix[matched - 1];
        }
        if current[index] == current[matched] {
            matched += 1;
        }
        prefix[index] = matched;
    }
    let mut matched = 0;
    for text in previous {
        while matched > 0 && (matched == current.len() || text != &current[matched]) {
            matched = prefix[matched - 1];
        }
        if text == &current[matched] {
            matched += 1;
        }
    }
    matched
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_app_tracking_keeps_stack_traces_and_follows_restarted_processes() {
        let mut logs = LogBuffer::default();
        let options = LogOptions::application("com.example.demo");
        logs.ingest(Source::Android, "__SHELLLF_LOG_CLOCK__ 2026-11-07T11:00:00\n__SHELLLF_LOG_PIDS__ 100 101\n11-07 10:59:00.000 100 200 E Demo: error\n    stack trace\n11-07 10:59:01.000 999 200 I Other: other\n", false);
        let matched = logs
            .lines()
            .into_iter()
            .filter(|line| matches_options(line, &options, logs.clock()))
            .collect::<Vec<_>>();
        assert_eq!(matched.len(), 2);
        assert_eq!(matched[1].pid, Some(100));
        assert_eq!(matched[1].tag.as_deref(), Some("Demo"));
        logs.ingest(Source::Android, "__SHELLLF_LOG_PIDS__ 300\n11-07 10:59:01.000 999 200 I Other: other\n11-07 10:59:02.000 300 200 I Demo: restarted\n", false);
        assert!(matches_options(
            logs.lines().last().unwrap(),
            &options,
            logs.clock()
        ));
        let options = LogOptions::application("com.example.demo")
            .with_period(super::super::LogPeriod::Minutes10);
        logs.ingest(Source::Android, "__SHELLLF_LOG_CLOCK__ 2026-11-07T12:00:00\n__SHELLLF_LOG_PIDS__ 300\n11-07 10:59:02.000 300 200 I Demo: restarted\n", false);
        assert!(
            logs.lines()
                .iter()
                .all(|line| !matches_options(line, &options, logs.clock()))
        );
    }

    #[test]
    fn journal_priorities_and_multiline_messages_keep_human_readable_text() {
        let mut logs = LogBuffer::default();
        logs.ingest(Source::Journal, r#"{"__REALTIME_TIMESTAMP":"1700000000000000","PRIORITY":"3","_SYSTEMD_UNIT":"demo.service","MESSAGE":"failed\nstack trace"}
{"PRIORITY":"6","MESSAGE":"ready"}
"#, false);
        assert_eq!(logs.lines()[0].level, LogLevel::Error);
        assert!(
            logs.lines()[0]
                .text
                .contains("demo.service: failed\nstack trace")
        );
        assert_eq!(logs.lines()[1].level, LogLevel::Info);
    }
    #[test]
    fn snapshots_preserve_duplicates_and_clear_does_not_replay_old_logs() {
        let mut logs = LogBuffer::default();
        logs.ingest(Source::Kernel, "<6>[1] same\n<6>[2] same\n", false);
        logs.ingest(Source::Kernel, "<6>[2] same\n<3>[3] error\n", false);
        assert_eq!(logs.lines().len(), 3);
        assert_eq!(logs.lines()[2].level, LogLevel::Error);
        assert!(matches(&logs.lines()[2], LogLevel::Warning, "error"));
        logs.clear();
        logs.ingest(Source::Kernel, "<6>[2] same\n<3>[3] error\n", false);
        assert!(logs.lines().is_empty());
        logs.ingest(Source::Kernel, "<3>[3] error\n<4>[4] later\n", false);
        assert_eq!(logs.lines().len(), 1);
        assert_eq!(logs.lines()[0].id, 4);
    }

    #[test]
    fn android_levels_keywords_and_multiline_entries_are_parsed() {
        let mut logs = LogBuffer::default();
        logs.ingest(Source::Android, "--------- beginning of main\n11-07 01:02:03.000 100 200 E Demo: FAILED\n    stack trace\n11-07 01:02:04.000 100 200 D Demo: debug\n", false);
        assert_eq!(logs.lines().len(), 3);
        assert_eq!(logs.lines()[1].level, LogLevel::Error);
        assert!(matches(&logs.lines()[0], LogLevel::Error, "failed"));
        assert!(!matches(&logs.lines()[2], LogLevel::Error, ""));
    }

    #[test]
    fn retained_logs_are_bounded_and_ring_gaps_are_visible() {
        let mut logs = LogBuffer::default();
        let output = (0..MAX_LINES + 1)
            .map(|index| format!("<6>[{index}] line"))
            .collect::<Vec<_>>()
            .join("\n");
        logs.ingest(Source::Kernel, &output, false);
        assert_eq!(logs.lines().len(), MAX_LINES);
        assert!(logs.trimmed);
        logs.ingest(Source::Kernel, "<6>[999999] new boot\n", false);
        assert!(logs.gap);
    }
}
