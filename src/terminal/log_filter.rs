use std::ops::Range;
use std::sync::Arc;

use alacritty_terminal::sync::FairMutex;
use gpui_kit::SharedString;

use super::engine::AlacrittyTerm;
use super::search::{SearchOptions, text_lines};

const MAX_RESULT_LINES: usize = 10_000;
const MAX_RESULT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub(super) struct LogSource(Arc<FairMutex<AlacrittyTerm>>);

impl LogSource {
    pub fn new(term: Arc<FairMutex<AlacrittyTerm>>) -> Self {
        Self(term)
    }

    pub fn filter(&self, query: &str, options: SearchOptions) -> LogResults {
        let Ok(regex) = options.compile(query) else {
            return LogResults::default();
        };
        let lines: Vec<_> = {
            let term = self.0.lock();
            text_lines(&term)
                .map(|line| (line.line, line.text))
                .collect()
        };
        let mut results = LogResults::default();
        let mut bytes = 0;
        for (line, text) in lines {
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
