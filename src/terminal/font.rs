//! The terminal font: family, size and line height. The settings set it as a
//! global and every terminal view reads it, so a change reaches open tabs at
//! once. Also the preview of it that the settings page shows.

use std::{collections::HashSet, ops::RangeInclusive, sync::OnceLock};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use unicode_width::UnicodeWidthChar as _;

use super::TerminalColors;
use super::box_drawing::{self, CellMetrics};
use crate::i18n::t;

/// The size terminals had before it could be set: the theme's `mono_md`.
pub const DEFAULT_FONT_SIZE: f32 = 13.;
/// The rows of 20 px that terminals had at the default size.
pub const DEFAULT_LINE_HEIGHT: f32 = 1.54;
/// Font sizes a terminal accepts, in pixels.
pub const FONT_SIZE_RANGE: RangeInclusive<f32> = 10.0..=28.0;
/// Line heights a terminal accepts, as multiples of the font size.
pub const LINE_HEIGHT_RANGE: RangeInclusive<f32> = 1.0..=2.0;

#[derive(Clone, Debug, PartialEq)]
pub struct TerminalFont {
    /// `None` is the theme's monospace family.
    pub family: Option<SharedString>,
    pub size: Pixels,
    /// A multiple of `size`.
    pub line_height: f32,
}

impl Global for TerminalFont {}

impl Default for TerminalFont {
    fn default() -> Self {
        Self {
            family: None,
            size: px(DEFAULT_FONT_SIZE),
            line_height: DEFAULT_LINE_HEIGHT,
        }
    }
}

impl TerminalFont {
    /// The font the terminals use now.
    pub fn current(cx: &App) -> Self {
        cx.try_global::<Self>().cloned().unwrap_or_default()
    }

    pub fn family(&self, cx: &App) -> SharedString {
        self.family
            .clone()
            .unwrap_or_else(|| cx.theme().mono_font_family.clone())
    }

    /// The height of one row, on whole device pixels.
    pub fn row_height(&self, window: &Window) -> Pixels {
        window.pixel_snap(self.size * self.line_height).max(px(1.))
    }
}

/// The width of one cell: the font's `M`, on whole device pixels like the
/// row height. Every column then starts on a pixel and a character looks the
/// same in each; at a fractional width each column falls at another point
/// between pixels and its strokes blur, at 150 % scaling most of all.
pub(super) fn cell_width(font: Font, size: Pixels, window: &Window) -> Pixels {
    let run = TextRun {
        len: 1,
        font,
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let width = window
        .text_system()
        .shape_line("M".into(), size, &[run], None)
        .width();
    window.pixel_snap(width).max(px(1.))
}

/// Whether the system has `family`. GPUI substitutes another font for a
/// family it cannot find, so a family from the settings file is only used
/// when it is installed. The list is read once per process.
pub fn is_font_installed(family: &str, cx: &App) -> bool {
    static INSTALLED: OnceLock<HashSet<String>> = OnceLock::new();
    INSTALLED
        .get_or_init(|| cx.text_system().all_font_names().into_iter().collect())
        .contains(family)
}

/// The installed font families, alphabetically, except hidden system
/// families. Loads every family once per process, so call it off the main
/// thread when the settings page opens.
pub fn system_font_families(text_system: &TextSystem) -> &'static [SharedString] {
    static FAMILIES: OnceLock<Vec<SharedString>> = OnceLock::new();
    FAMILIES.get_or_init(|| {
        let mut families: Vec<_> = text_system
            .all_font_names()
            .into_iter()
            .filter(|family| !family.starts_with('.') && !family.trim().is_empty())
            .map(SharedString::from)
            .collect();
        families.sort_by_key(|family| family.to_lowercase());
        families.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        families
    })
}

/// The installed families fit for a terminal, alphabetically: every letter
/// as wide as the next, and narrower than a square, which leaves out symbol
/// fonts such as Webdings drawn on fixed squares. Hidden system families
/// (named with a leading dot) are left out.
///
/// Loads every family, around 150 ms on macOS, so it belongs off the main
/// thread. The answer is kept for the rest of the process.
pub fn monospace_font_families(text_system: &TextSystem) -> &'static [SharedString] {
    static MONOSPACE: OnceLock<Vec<SharedString>> = OnceLock::new();
    MONOSPACE.get_or_init(|| {
        let size = px(16.);
        system_font_families(text_system)
            .iter()
            .filter(|family| {
                let id = text_system.resolve_font(&font((*family).clone()));
                match (
                    text_system.advance(id, size, 'i'),
                    text_system.advance(id, size, 'M'),
                ) {
                    (Ok(narrow), Ok(wide)) => {
                        (narrow.width - wide.width).abs() < px(0.01) && wide.width < size * 0.8
                    }
                    _ => false,
                }
            })
            .cloned()
            .collect()
    })
}

/// Sample lines in the terminal font, laid out the way a terminal lays them
/// out: one glyph per cell, wide characters such as Chinese over two, rows
/// at the terminal's row height. Colors are the terminal's.
#[derive(IntoElement)]
pub struct TerminalFontPreview {
    id: ElementId,
    lines: &'static [&'static str],
}

impl TerminalFontPreview {
    pub fn new(id: impl Into<ElementId>, lines: &'static [&'static str]) -> Self {
        Self {
            id: id.into(),
            lines,
        }
    }
}

impl RenderOnce for TerminalFontPreview {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let font = TerminalFont::current(cx);
        let family = font.family(cx);
        let row_height = font.row_height(window);
        let lines = self.lines;
        let color = TerminalColors::current(cx).foreground();

        div()
            .id(self.id)
            .test_support()
            // What the preview shows, for tests and screen readers.
            .aria_label(t!(
                "terminal.font.preview",
                family = family,
                size = font.size.as_f32(),
                line_height = font.line_height
            ))
            .w_full()
            .h(row_height * lines.len() as f32)
            .overflow_hidden()
            .font_family(family)
            .text_size(font.size)
            .child(
                canvas(
                    move |_, window, _| {
                        let text_style = window.text_style();
                        let size = text_style.font_size.to_pixels(window.rem_size());
                        let run = |len| TextRun {
                            len,
                            font: text_style.font(),
                            color,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        let cell_width = cell_width(text_style.font(), size, window);
                        let scale = window.scale_factor();
                        let metrics = CellMetrics::new(cell_width, row_height, size, scale);
                        // Box drawing as the terminal draws it, each a space
                        // in its line.
                        let mut drawn = Vec::new();
                        let lines = lines
                            .iter()
                            .enumerate()
                            .map(|(row, line)| {
                                let cells: String = in_cells(line)
                                    .chars()
                                    .enumerate()
                                    .map(|(column, character)| {
                                        match box_drawing::shapes(character, metrics) {
                                            Some(shapes) => {
                                                let origin = point(
                                                    cell_width * column as f32,
                                                    row_height * row as f32,
                                                );
                                                drawn.extend(box_drawing::quads(
                                                    &shapes, origin, scale, color,
                                                ));
                                                ' '
                                            }
                                            None => character,
                                        }
                                    })
                                    .collect();
                                let len = cells.len();
                                window.text_system().shape_line(
                                    cells.into(),
                                    size,
                                    &[run(len)],
                                    Some(cell_width),
                                )
                            })
                            .collect::<Vec<_>>();
                        (lines, drawn)
                    },
                    move |bounds, (lines, drawn), window, cx| {
                        for mut quad in drawn {
                            quad.bounds.origin += bounds.origin;
                            window.paint_quad(quad);
                        }
                        for (row, line) in lines.iter().enumerate() {
                            let _ = line.paint(
                                point(bounds.left(), bounds.top() + row_height * row as f32),
                                row_height,
                                TextAlign::Left,
                                Some(bounds.size.width),
                                window,
                                cx,
                            );
                        }
                    },
                )
                .size_full(),
            )
    }
}

/// `line` as terminal cells: a wide character is followed by the blank its
/// second cell holds, as in the emulator's grid.
fn in_cells(line: &str) -> String {
    let mut cells = String::with_capacity(line.len() * 2);
    for character in line.chars() {
        cells.push(character);
        if character.width() == Some(2) {
            cells.push(' ');
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    use gpui_kit::{TestAppContext, font, px};

    use super::{cell_width, in_cells};

    /// The test text system's `M` is 0.6 of the font size, its window at 2×.
    #[gpui_kit::test]
    fn a_cell_is_whole_device_pixels_wide(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let mut width =
            |size: f32| cx.update(|window, _| cell_width(font("Menlo"), px(size), window));
        // 7.8 px, 15.6 device pixels.
        assert_eq!(width(13.), px(8.));
        // 8.4 px, 16.8 device pixels.
        assert_eq!(width(14.), px(8.5));
        assert_eq!(width(15.), px(9.));
    }

    #[test]
    fn wide_characters_take_two_cells() {
        assert_eq!(in_cells("ab"), "ab");
        assert_eq!(in_cells("中文：a"), "中 文 ： a");
        // Box drawing is narrow, as the emulator counts it.
        assert_eq!(in_cells("┌─┐"), "┌─┐");
    }
}
