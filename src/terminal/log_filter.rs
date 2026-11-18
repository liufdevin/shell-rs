use std::ops::Range;
use std::sync::{Arc, Mutex};

use alacritty_terminal::index::Line;
use alacritty_terminal::sync::FairMutex;
use gpui_kit::SharedString;

use super::engine::AlacrittyTerm;
use super::search::{SearchOptions, text_lines};
use super::timestamps::{LineTimestamp, Timestamps};

const MAX_RESULT_LINES: usize = 10_000;
const MAX_RESULT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub(super) struct LogSource {
    term: Arc<FairMutex<AlacrittyTerm>>,
    timestamps: Arc<Mutex<Timestamps>>,
}

impl LogSource {
    pub fn new(term: Arc<FairMutex<AlacrittyTerm>>, timestamps: Arc<Mutex<Timestamps>>) -> Self {
        Self { term, timestamps }
    }

    pub fn text(&self, timestamps: bool) -> String {
        let mut lines = self.lines();
        while lines.last().is_some_and(|(_, text, _)| text.is_empty()) {
            lines.pop();
        }
        let lines = lines.iter().skip_while(|(_, text, _)| text.is_empty());
        format_log(
            lines.map(|(_, text, stamp)| (text.as_str(), *stamp)),
            timestamps,
        )
    }

    fn lines(&self) -> Vec<(i32, String, Option<LineTimestamp>)> {
        let term = self.term.lock();
        let timestamps = self.timestamps.lock().unwrap();
        text_lines(&*term)
            .map(|line| (line.line, line.text, timestamps.at(&*term, Line(line.line))))
            .collect()
    }

    pub fn filter(&self, query: &str, options: SearchOptions) -> LogResults {
        let Ok(regex) = options.compile(query) else {
            return LogResults::default();
        };
        let mut results = LogResults::default();
        let mut bytes = 0;
        for (line, text, timestamp) in self.lines() {
            let spans: Vec<_> = regex
                .find_iter(&text)
                .filter(|found| !found.is_empty())
                .map(|found| found.range())
                .collect();
            if spans.is_empty() {
                continue;
            }
            bytes += text.len();
            if results.lines.len() >= MAX_RESULT_LINES || bytes > MAX_RESULT_BYTES {
                results.truncated = true;
                break;
            }
            results.lines.push(LogLine {
                line,
                text: text.into(),
                spans,
                timestamp,
            });
        }
        results
    }
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub(super) struct LogLine {
    pub line: i32,
    pub text: SharedString,
    pub spans: Vec<Range<usize>>,
    pub timestamp: Option<LineTimestamp>,
}

#[derive(Default)]
#[non_exhaustive]
pub(super) struct LogResults {
    pub lines: Vec<LogLine>,
    pub truncated: bool,
}

impl LogResults {
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(|line| line.text.as_ref())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub(super) fn format_log<'a>(
    lines: impl IntoIterator<Item = (&'a str, Option<LineTimestamp>)>,
    timestamps: bool,
) -> String {
    let mut output = String::new();
    for (line, stamp) in lines {
        if timestamps && let Some(stamp) = stamp {
            output.push_str(&stamp.full());
            output.push('\t');
        }
        output.push_str(line);
        output.push('\n');
    }
    output
}
