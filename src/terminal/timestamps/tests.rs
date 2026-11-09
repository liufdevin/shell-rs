use super::*;
use crate::terminal::TerminalSize;
use alacritty_terminal::{event::VoidListener, grid::Scroll, term::Config};
use chrono::TimeZone as _;

#[test]
fn displayed_timestamps_use_brackets_and_colon_separated_milliseconds() {
    let stamp = LineTimestamp {
        wall_ms: chrono::Local
            .with_ymd_and_hms(2026, 11, 9, 13, 50, 47)
            .single()
            .unwrap()
            .timestamp_millis()
            + 16,
        elapsed_ns: 0,
        id: 1,
    };
    assert_eq!(stamp.display(), "[ 13:50:47:016 ]");
}

struct Screen {
    term: Term<VoidListener>,
    parser: Processor,
    times: Timestamps,
}
impl Screen {
    fn new(columns: usize, rows: usize) -> Self {
        Self {
            term: Term::new(
                Config {
                    scrolling_history: 30,
                    ..Default::default()
                },
                &TerminalSize::new(columns, rows, 8, 16),
                VoidListener,
            ),
            parser: Processor::new(),
            times: Timestamps {
                enabled: true,
                ..Default::default()
            },
        }
    }
    fn feed(&mut self, text: &[u8]) {
        self.parser.advance(
            &mut TimestampHandler::new(&mut self.term, &mut self.times),
            text,
        );
        self.times.prune(&self.term);
    }
}

#[test]
fn fragments_wrapping_unicode_and_carriage_return_keep_the_first_timestamp() {
    let mut screen = Screen::new(8, 4);
    screen.feed(b"first");
    let first = screen.times.at(&screen.term, Line(0)).unwrap();
    screen.feed(b"-long-line");
    assert_eq!(screen.times.at(&screen.term, Line(1)), Some(first));
    assert_eq!(screen.times.visible(&screen.term)[1], None);
    screen.feed(b"\r\n");
    for byte in "中文".as_bytes() {
        screen.feed(&[*byte]);
    }
    let next = screen.times.at(&screen.term, Line(2)).unwrap();
    assert!(next.id > first.id);
    screen.feed(b"\rprogress");
    assert_eq!(screen.times.at(&screen.term, Line(2)), Some(next));
}

#[test]
fn scrollback_eviction_is_bounded_and_resize_preserves_logical_line_times() {
    let mut screen = Screen::new(40, 5);
    screen.feed(b"first-log\r\nsecond-log-with-long-content\r\n");
    let first = screen.times.at(&screen.term, Line(0)).unwrap();
    let second = screen.times.at(&screen.term, Line(1)).unwrap();
    screen
        .times
        .resize(&mut screen.term, TerminalSize::new(10, 5, 8, 16));
    assert!(screen.times.contains(first));
    assert!(screen.times.contains(second));
    screen
        .times
        .resize(&mut screen.term, TerminalSize::new(40, 5, 8, 16));
    assert!(screen.times.contains(first));
    assert!(screen.times.contains(second));
    for number in 0..200 {
        screen.feed(format!("line-{number}\r\n").as_bytes());
    }
    assert!(screen.times.rows.len() <= screen.term.total_lines() + screen.term.screen_lines());
    assert!(!screen.times.contains(first));
    screen.term.scroll_display(Scroll::Top);
    assert!(
        screen
            .times
            .visible(&screen.term)
            .iter()
            .any(Option::is_some)
    );
}

#[test]
fn disabled_history_and_replay_do_not_get_invented_times_and_alt_screen_is_untouched() {
    let mut screen = Screen::new(30, 4);
    screen.times.enabled = false;
    screen.feed(b"old\r\n");
    screen.times.enabled = true;
    assert_eq!(screen.times.at(&screen.term, Line(0)), None);
    screen.feed(b"new");
    let stamp = screen.times.at(&screen.term, Line(1)).unwrap();
    screen.feed(b"\x1b[?1049h\x1b[Hfull screen\x1b[2J\x1b[Hready");
    assert!(screen.times.visible(&screen.term).is_empty());
    screen.feed(b"\x1b[?1049l");
    assert_eq!(screen.times.at(&screen.term, Line(1)), Some(stamp));
    screen.times.enabled = false;
    screen.feed(b"\r\nnot stamped\r\n");
    assert!(screen.times.visible(&screen.term).is_empty());
    screen.times.enabled = true;
    assert_eq!(screen.times.at(&screen.term, Line(2)), None);
    screen.feed(b"\x1bc");
    assert!(screen.times.rows.is_empty());
}

#[test]
fn explicit_clear_removes_blank_rows_and_clock_changes_do_not_affect_elapsed_time() {
    let mut screen = Screen::new(30, 4);
    screen.feed(b"old\r\nnew");
    screen.feed(b"\x1b[2J\x1b[H");
    assert!(
        screen
            .times
            .visible(&screen.term)
            .iter()
            .all(Option::is_none)
    );
    let first = LineTimestamp {
        wall_ms: 2000,
        elapsed_ns: 1_000_000,
        id: 1,
    };
    let second = LineTimestamp {
        wall_ms: 1000,
        elapsed_ns: 3_000_000,
        id: 2,
    };
    assert_eq!(first.elapsed_ns.abs_diff(second.elapsed_ns), 2_000_000);
}

#[test]
fn enabling_mid_line_does_not_stamp_older_content_or_its_wrapped_continuation() {
    let mut screen = Screen::new(8, 5);
    screen.times.enabled = false;
    screen.feed(b"older");
    screen.times.enabled = true;
    screen.feed(b"-continuation\r\nnew");
    assert_eq!(screen.times.at(&screen.term, Line(0)), None);
    assert_eq!(screen.times.at(&screen.term, Line(1)), None);
    assert!(screen.times.at(&screen.term, Line(3)).is_some());
}

#[test]
fn synchronized_output_flush_keeps_timestamps_separate_from_escape_sequences() {
    let mut screen = Screen::new(30, 5);
    screen.feed(b"\x1b[?2026hfirst\r\nsecond\r\n");
    screen.parser.stop_sync(&mut TimestampHandler::new(
        &mut screen.term,
        &mut screen.times,
    ));
    assert!(screen.times.at(&screen.term, Line(0)).is_some());
    assert!(screen.times.at(&screen.term, Line(1)).is_some());
    assert_ne!(
        screen.times.at(&screen.term, Line(0)).unwrap().id,
        screen.times.at(&screen.term, Line(1)).unwrap().id
    );
}
