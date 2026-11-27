//! Detect when `htop`'s per-CPU meters fill the terminal screen and hide the
//! process list, so the view can offer non-intrusive shortcuts (zoom out, open
//! the Processes sidebar, or copy a safe config-compacting command).

use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;

use super::TerminalSnapshot;

/// Minimum terminal height considered for the crowded-`htop` heuristic.
const MIN_ROWS: usize = 6;
/// Minimum terminal width considered for the crowded-`htop` heuristic.
const MIN_COLUMNS: usize = 20;

/// POSIX shell command that backs up `htoprc` and switches per-CPU meters to a
/// single average `CPU` bar for both `htop` 2.x and 3.x, preserving all other
/// user settings.
pub const HTOP_COMPACT_COMMAND: &str = r#"sh -c 'f="${XDG_CONFIG_HOME:-$HOME/.config}/htop/htoprc"; mkdir -p "$(dirname "$f")"; if [ -f "$f" ]; then cp "$f" "$f.bak"; awk "/^(left_meters|column_meters_0)=/{sub(/=.*/,\"=CPU Memory Swap\")} /^(left_meter_modes|column_meter_modes_0)=/{sub(/=.*/,\"=1 1 1\")} /^(right_meters|column_meters_1)=/{sub(/=.*/,\"=Tasks LoadAverage Uptime\")} /^(right_meter_modes|column_meter_modes_1)=/{sub(/=.*/,\"=2 2 2\")} /^header_layout=/{sub(/=.*/,\"=two_50_50\")} /^column_meters_[2-9]=|^column_meter_modes_[2-9]=/{next} {print}" "$f.bak" > "$f"; else printf "left_meters=CPU Memory Swap\nleft_meter_modes=1 1 1\nright_meters=Tasks LoadAverage Uptime\nright_meter_modes=2 2 2\n" > "$f"; fi'"#;

/// Returns `true` when the terminal is in the alternate screen and nearly every
/// visible row looks like `htop` per-core CPU bars with no process table header
/// in sight.
pub fn is_htop_cpu_crowded(snapshot: &TerminalSnapshot, mode: TermMode) -> bool {
    if !mode.contains(TermMode::ALT_SCREEN)
        || snapshot.rows < MIN_ROWS
        || snapshot.columns < MIN_COLUMNS
        || snapshot.cells.len() < snapshot.rows * snapshot.columns
    {
        return false;
    }

    let mut cpu_rows = 0usize;
    let mut non_empty_rows = 0usize;

    for row in snapshot
        .cells
        .chunks_exact(snapshot.columns)
        .take(snapshot.rows)
    {
        let line: String = row
            .iter()
            .filter(|cell| !cell.flags.contains(Flags::WIDE_CHAR_SPACER))
            .map(|cell| {
                if cell.flags.contains(Flags::HIDDEN) {
                    " "
                } else {
                    cell.character.as_str()
                }
            })
            .collect();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        non_empty_rows += 1;

        if is_process_header_line(trimmed) {
            return false;
        }
        if is_cpu_meter_line(trimmed) {
            cpu_rows += 1;
        }
    }

    if cpu_rows < MIN_ROWS || non_empty_rows == 0 {
        return false;
    }

    // At least 75% of visible non-empty lines are per-CPU meter rows.
    cpu_rows * 4 >= non_empty_rows * 3
}

/// Whether `line` looks like `htop`'s process table column header.
fn is_process_header_line(line: &str) -> bool {
    let mut has_pid = false;
    let mut has_user = false;
    let mut has_metric = false;
    for token in line.split_whitespace() {
        match token {
            "PID" => has_pid = true,
            "USER" => has_user = true,
            "CPU%" | "MEM%" | "TIME+" | "Command" => has_metric = true,
            _ => {}
        }
    }
    has_pid && has_user && has_metric
}

/// Whether `line` contains at least one `htop` per-core CPU bar of the form
/// `<cpu_index>[...<pct>%]` (e.g. `1  [|||   0.6%]`, `127[   0.0%]`).
fn is_cpu_meter_line(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    let mut matches = 0usize;

    while i < bytes.len() {
        // Skip leading spaces before a CPU number.
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        let num_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let num_len = i - num_start;
        if num_len == 0 || num_len > 4 {
            // Advance past this token.
            while i < bytes.len() && bytes[i] != b' ' {
                i += 1;
            }
            continue;
        }
        // Optional spaces between CPU index and `[`.
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'[' {
            continue;
        }
        // Find the matching `]`.
        let bar_start = i + 1;
        let Some(rel_end) = bytes[bar_start..].iter().position(|&b| b == b']') else {
            break;
        };
        let bar = &line[bar_start..bar_start + rel_end];
        if bar_ends_with_percent(bar) {
            matches += 1;
        }
        i = bar_start + rel_end + 1;
    }

    matches > 0
}

fn bar_ends_with_percent(bar: &str) -> bool {
    let Some(before_pct) = bar.trim_end().strip_suffix('%') else {
        return false;
    };
    let tail = before_pct
        .rsplit(|c: char| !c.is_ascii_digit() && c != '.')
        .next()
        .unwrap_or("");
    !tail.is_empty() && tail.bytes().any(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::term::TermMode;
    use alacritty_terminal::term::cell::Flags;
    use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
    use smol_str::SmolStr;

    use super::{HTOP_COMPACT_COMMAND, is_htop_cpu_crowded};
    use crate::terminal::{SearchMark, TerminalCell, TerminalSnapshot};

    fn snapshot_from_lines(lines: &[&str], columns: usize) -> TerminalSnapshot {
        let rows = lines.len();
        let mut cells = Vec::with_capacity(rows * columns);
        for line in lines {
            let mut chars = line.chars();
            for _ in 0..columns {
                let ch = chars.next().unwrap_or(' ');
                cells.push(TerminalCell {
                    character: SmolStr::new_inline(&ch.to_string()),
                    foreground: Color::Named(NamedColor::Foreground),
                    background: Color::Named(NamedColor::Background),
                    flags: Flags::empty(),
                    selected: false,
                    search: SearchMark::None,
                    link: None,
                    highlight: None,
                });
            }
        }
        TerminalSnapshot {
            timestamps: vec![None; rows],
            columns,
            rows,
            cells,
            cursor_row: None,
            cursor_column: 0,
            cursor_shape: CursorShape::Block,
            cursor_blinking: false,
            display_offset: 0,
            history_size: 0,
            colors: [None; alacritty_terminal::term::color::COUNT],
            links: Vec::new(),
        }
    }

    #[test]
    fn detects_screen_filled_by_htop_cpu_bars() {
        let lines: Vec<String> = (1..=20)
            .map(|i| {
                format!(
                    "{:>3} [        0.0%] {:>3} [|       0.6%] {:>3} [        0.0%] {:>3} [        0.0%]",
                    i,
                    i + 32,
                    i + 64,
                    i + 96
                )
            })
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let snapshot = snapshot_from_lines(&refs, 100);

        assert!(is_htop_cpu_crowded(&snapshot, TermMode::ALT_SCREEN));
        assert!(!is_htop_cpu_crowded(&snapshot, TermMode::empty()));
    }

    #[test]
    fn does_not_trigger_when_process_header_is_visible() {
        let mut lines: Vec<String> = (1..=16)
            .map(|i| format!("{:>3} [        0.0%] {:>3} [        0.0%]", i, i + 16))
            .collect();
        lines.push("  Mem[||||   12.0G/256G]   Tasks: 412, 120 thr; 2 running".into());
        lines.push("  PID USER      PRI  NI  VIRT   RES   SHR S CPU% MEM%   TIME+  Command".into());
        lines.push(" 1001 root       20   0  1.2G  120M   10M R 12.3  0.1  1:23.45 python".into());
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let snapshot = snapshot_from_lines(&refs, 100);

        assert!(!is_htop_cpu_crowded(&snapshot, TermMode::ALT_SCREEN));
    }

    #[test]
    fn ignores_non_htop_fullscreen_output() {
        let lines = vec!["~"; 20];
        let snapshot = snapshot_from_lines(&lines, 80);
        assert!(!is_htop_cpu_crowded(&snapshot, TermMode::ALT_SCREEN));
    }

    #[test]
    fn compact_command_covers_both_htop_2_and_3() {
        assert!(HTOP_COMPACT_COMMAND.contains("left_meters"));
        assert!(HTOP_COMPACT_COMMAND.contains("column_meters_0"));
        assert!(HTOP_COMPACT_COMMAND.contains("$f.bak"));
    }
}
