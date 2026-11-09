use cursor_icon::CursorIcon;
use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Instant;

use alacritty_terminal::{
    event::EventListener,
    grid::Dimensions,
    index::{Column, Line},
    term::{Term, TermMode, cell::Flags},
    vte::ansi::*,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct LineTimestamp {
    pub wall_ms: i64,
    pub elapsed_ns: u64,
    pub id: u64,
}

impl LineTimestamp {
    pub fn display(self) -> String {
        chrono::DateTime::from_timestamp_millis(self.wall_ms)
            .map(|date| {
                date.with_timezone(&chrono::Local)
                    .format("[ %H:%M:%S:%3f ]")
                    .to_string()
            })
            .unwrap_or_default()
    }
    pub fn full(self) -> String {
        chrono::DateTime::from_timestamp_millis(self.wall_ms)
            .map(|date| {
                date.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S%.3f %:z")
                    .to_string()
            })
            .unwrap_or_default()
    }
}

pub(super) struct Timestamps {
    unknown_rows: std::collections::HashSet<usize>,
    pub enabled: bool,
    rows: HashMap<usize, LineTimestamp>,
    origin: Instant,
    next: u64,
}

impl Default for Timestamps {
    fn default() -> Self {
        Self {
            unknown_rows: std::collections::HashSet::new(),
            enabled: false,
            rows: HashMap::new(),
            origin: Instant::now(),
            next: 1,
        }
    }
}

fn row_key<T>(term: &Term<T>, line: Line) -> usize {
    &term.grid()[line][Column(0)] as *const _ as usize
}

fn wraps<T>(term: &Term<T>, line: Line) -> bool {
    term.grid()[line][Column(term.columns() - 1)]
        .flags
        .contains(Flags::WRAPLINE)
}

impl Timestamps {
    pub fn contains(&self, stamp: LineTimestamp) -> bool {
        self.rows.values().any(|value| *value == stamp)
    }
    pub fn is_tracking(&self) -> bool {
        self.enabled || !self.rows.is_empty()
    }
    pub fn clear(&mut self) {
        self.rows.clear();
        self.unknown_rows.clear();
    }
    pub fn at<T>(&self, term: &Term<T>, line: Line) -> Option<LineTimestamp> {
        self.rows.get(&row_key(term, line)).copied()
    }
    pub fn visible<T>(&self, term: &Term<T>) -> Vec<Option<LineTimestamp>> {
        if !self.enabled || term.mode().contains(TermMode::ALT_SCREEN) {
            return Vec::new();
        }
        let top = -(term.grid().display_offset() as i32);
        (top..top + term.screen_lines() as i32)
            .map(|line| {
                let line = Line(line);
                if line.0 > -(term.history_size() as i32) && wraps(term, line - 1) {
                    None
                } else {
                    self.at(term, line)
                }
            })
            .collect()
    }
    pub fn prune<T>(&mut self, term: &Term<T>) {
        if self.rows.len() + self.unknown_rows.len() <= term.total_lines() + term.screen_lines() {
            return;
        }
        let keys: std::collections::HashSet<_> = (-(term.history_size() as i32)
            ..term.screen_lines() as i32)
            .map(|line| row_key(term, Line(line)))
            .collect();
        self.rows.retain(|key, _| keys.contains(key));
        self.unknown_rows.retain(|key| keys.contains(key));
    }
    pub fn resize<T: EventListener>(&mut self, term: &mut Term<T>, size: super::TerminalSize) {
        if self.rows.is_empty() || term.mode().contains(TermMode::ALT_SCREEN) {
            if term.mode().contains(TermMode::ALT_SCREEN) {
                self.clear();
            }
            term.resize(size);
            return;
        }
        let old = self.paragraphs(term);
        let mut stamps: HashMap<u64, VecDeque<Option<LineTimestamp>>> = HashMap::new();
        for (hash, _, stamp) in old {
            stamps.entry(hash).or_default().push_back(stamp);
        }
        term.resize(size);
        let new = self.paragraphs(term);
        self.rows.clear();
        self.unknown_rows.clear();
        for (hash, keys, _) in new.into_iter().rev() {
            if let Some(stamp) = stamps.get_mut(&hash).and_then(VecDeque::pop_back).flatten() {
                for key in keys {
                    self.rows.insert(key, stamp);
                }
            }
        }
    }
    fn paragraphs<T>(&self, term: &Term<T>) -> Vec<(u64, Vec<usize>, Option<LineTimestamp>)> {
        let mut result = Vec::new();
        let mut hash = DefaultHasher::new();
        let mut keys = Vec::new();
        let mut stamp = None;
        for line in -(term.history_size() as i32)..term.screen_lines() as i32 {
            let line = Line(line);
            keys.push(row_key(term, line));
            stamp = stamp.or(self.at(term, line));
            let row = &term.grid()[line];
            let end = if wraps(term, line) {
                row.len()
            } else {
                (0..row.len())
                    .rfind(|column| row[Column(*column)].c != ' ')
                    .map_or(0, |column| column + 1)
            };
            for column in 0..end {
                let cell = &row[Column(column)];
                if !cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    cell.c.hash(&mut hash);
                    cell.zerowidth().unwrap_or_default().hash(&mut hash);
                }
            }
            if !wraps(term, line) {
                result.push((hash.finish(), std::mem::take(&mut keys), stamp.take()));
                hash = DefaultHasher::new();
            }
        }
        if !keys.is_empty() {
            result.push((hash.finish(), keys, stamp));
        }
        result
    }
}

pub(super) struct TimestampHandler<'a, T> {
    pub term: &'a mut Term<T>,
    pub times: &'a mut Timestamps,
    now: LineTimestamp,
}

impl<'a, T> TimestampHandler<'a, T> {
    pub fn new(term: &'a mut Term<T>, times: &'a mut Timestamps) -> Self {
        let now = LineTimestamp {
            wall_ms: chrono::Local::now().timestamp_millis(),
            elapsed_ns: times.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            id: times.next,
        };
        Self { term, times, now }
    }
    fn forget_cursor(&mut self) {
        let point = self.term.grid().cursor.point;
        self.times.rows.remove(&row_key(self.term, point.line));
        self.times
            .unknown_rows
            .remove(&row_key(self.term, point.line));
    }

    fn forget_blank_rows(&mut self) {
        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        for line in 0..self.term.screen_lines() as i32 {
            let row = &self.term.grid()[Line(line)];
            if (0..row.len()).all(|column| row[Column(column)].c == ' ') {
                self.times.rows.remove(&row_key(self.term, Line(line)));
            }
        }
        self.times.prune(self.term);
    }
}

macro_rules! forward {
    ($($name:ident($($arg:ident: $kind:ty),*);)*) => { $(fn $name(&mut self, $($arg: $kind),*) { self.term.$name($($arg),*); })* };
}

impl<T: EventListener> Handler for TimestampHandler<'_, T> {
    fn input(&mut self, character: char) {
        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            self.term.input(character);
            return;
        }
        let before = self.term.grid().cursor.point;
        let before_key = row_key(self.term, before.line);
        let parent = self.times.at(self.term, before.line);
        let unrecorded_content = parent.is_none()
            && (0..self.term.columns())
                .any(|column| self.term.grid()[before.line][Column(column)].c != ' ');
        let pending_wrap = self.term.grid().cursor.input_needs_wrap;
        self.term.input(character);
        let after = self.term.grid().cursor.point;
        let key = row_key(self.term, after.line);
        if self.times.enabled {
            if unrecorded_content
                || self.times.unknown_rows.contains(&before_key)
                || (pending_wrap || before_key != key) && parent.is_none()
            {
                self.times.rows.remove(&key);
                self.times.unknown_rows.insert(key);
                return;
            }
            let stamp = if pending_wrap || before_key != key {
                parent.unwrap_or(self.now)
            } else {
                self.times.rows.get(&key).copied().unwrap_or(self.now)
            };
            self.times.rows.insert(key, stamp);
            self.times.next = self.times.next.max(stamp.id + 1);
            self.now.id = self.times.next;
        } else {
            self.times.rows.remove(&key);
        }
    }
    fn linefeed(&mut self) {
        self.term.linefeed();
        if !self.term.mode().contains(TermMode::ALT_SCREEN) {
            self.forget_cursor();
        }
    }
    fn newline(&mut self) {
        self.term.newline();
        if !self.term.mode().contains(TermMode::ALT_SCREEN) {
            self.forget_cursor();
        }
    }
    fn clear_screen(&mut self, mode: ClearMode) {
        self.term.clear_screen(mode);
        if !self.term.mode().contains(TermMode::ALT_SCREEN) {
            for line in 0..self.term.screen_lines() as i32 {
                let row = &self.term.grid()[Line(line)];
                if (0..row.len()).all(|column| row[Column(column)].c == ' ') {
                    self.times.rows.remove(&row_key(self.term, Line(line)));
                }
            }
            self.times.prune(self.term);
        }
    }
    fn clear_line(&mut self, mode: LineClearMode) {
        let clears = matches!(mode, LineClearMode::All)
            || matches!(mode, LineClearMode::Right) && self.term.grid().cursor.point.column.0 == 0;
        self.term.clear_line(mode);
        if clears {
            self.forget_cursor();
        }
    }
    fn reset_state(&mut self) {
        self.term.reset_state();
        self.times.clear();
    }
    fn scroll_up(&mut self, count: usize) {
        self.term.scroll_up(count);
        self.forget_blank_rows();
    }
    fn scroll_down(&mut self, count: usize) {
        self.term.scroll_down(count);
        self.forget_blank_rows();
    }
    fn insert_blank_lines(&mut self, count: usize) {
        self.term.insert_blank_lines(count);
        self.forget_blank_rows();
    }
    fn delete_lines(&mut self, count: usize) {
        self.term.delete_lines(count);
        self.forget_blank_rows();
    }
    fn reverse_index(&mut self) {
        self.term.reverse_index();
        self.forget_blank_rows();
    }
    forward! {
        set_title(title: Option<String>); set_cursor_style(style: Option<CursorStyle>); set_cursor_shape(shape: CursorShape);
        goto(line: i32, column: usize); goto_line(line: i32); goto_col(column: usize); insert_blank(count: usize);
        move_up(count: usize); move_down(count: usize); identify_terminal(intermediate: Option<char>); device_status(status: usize);
        move_forward(count: usize); move_backward(count: usize); move_down_and_cr(count: usize); move_up_and_cr(count: usize);
        put_tab(count: u16); backspace(); carriage_return(); bell(); substitute(); set_horizontal_tabstop();
        erase_chars(count: usize); delete_chars(count: usize); move_backward_tabs(count: u16); move_forward_tabs(count: u16);
        save_cursor_position(); restore_cursor_position(); clear_tabs(mode: TabulationClearMode); set_tabs(interval: u16);
        terminal_attribute(attribute: Attr); set_mode(mode: Mode); unset_mode(mode: Mode); report_mode(mode: Mode);
        set_private_mode(mode: PrivateMode); unset_private_mode(mode: PrivateMode); report_private_mode(mode: PrivateMode);
        set_scrolling_region(top: usize, bottom: Option<usize>); set_keypad_application_mode(); unset_keypad_application_mode();
        set_active_charset(index: CharsetIndex); configure_charset(index: CharsetIndex, charset: StandardCharset);
        set_color(index: usize, color: Rgb); dynamic_color_sequence(prefix: String, index: usize, terminator: &str);
        reset_color(index: usize); clipboard_store(selection: u8, text: &[u8]); clipboard_load(selection: u8, terminator: &str);
        decaln(); push_title(); pop_title(); text_area_size_pixels(); text_area_size_chars(); set_hyperlink(link: Option<Hyperlink>);
        set_mouse_cursor_icon(icon: CursorIcon); report_keyboard_mode(); push_keyboard_mode(mode: KeyboardModes); pop_keyboard_modes(count: u16);
        set_keyboard_mode(mode: KeyboardModes, behavior: KeyboardModesApplyBehavior); set_modify_other_keys(mode: ModifyOtherKeys);
        report_modify_other_keys(); set_scp(path: ScpCharPath, mode: ScpUpdateMode);
    }
}

#[cfg(test)]
mod tests;
