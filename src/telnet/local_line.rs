//! Typing before the server echoes. A server that has not agreed to echo
//! (`WILL ECHO`) expects the NVT's default terminal, one that shows what
//! is typed and sends a line at a time: chess servers, MUDs, and SMTP,
//! HTTP or Redis typed at by hand. So the line is edited here, as PuTTY
//! does by default: the typing shows, Backspace takes back a character
//! and ^U the line, and Enter sends it. Pure, so the rules are tested here.

use unicode_width::UnicodeWidthChar as _;

const BACKSPACE: u8 = 0x08;
const DELETE: u8 = 0x7f;
/// ^U, the line's erase in a Unix terminal.
const KILL: u8 = 0x15;
const ESC: u8 = 0x1b;

#[derive(Default)]
pub(crate) struct LocalLine {
    encoding: crate::host::TerminalEncoding,
    /// Typed and shown, not sent yet.
    line: Vec<u8>,
    /// The last byte typed was Enter's CR: an LF right after it (a pasted
    /// CR LF) is the same Enter.
    cr: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Edited {
    /// What goes to the server, before the protocol's CR LF.
    pub send: Vec<u8>,
    /// What the terminal shows, as an echoing server would.
    pub echo: Vec<u8>,
}

impl LocalLine {
    pub fn with_encoding(mut self, encoding: crate::host::TerminalEncoding) -> Self {
        self.encoding = encoding;
        self
    }

    pub fn edit(&mut self, typed: &[u8]) -> Edited {
        let mut edited = Edited::default();
        // Arrow and function keys and the terminal's replies go as they are:
        // a key's bytes come in one write, starting with ESC.
        if typed.first() == Some(&ESC) {
            edited.send.extend_from_slice(typed);
            return edited;
        }
        for &byte in typed {
            if std::mem::take(&mut self.cr) && byte == b'\n' {
                continue;
            }
            match byte {
                b'\r' | b'\n' => {
                    edited.send.append(&mut self.line);
                    edited.send.push(b'\r');
                    edited.echo.extend_from_slice(b"\r\n");
                    self.cr = byte == b'\r';
                }
                BACKSPACE | DELETE => self.erase(&mut edited.echo),
                KILL => {
                    while !self.line.is_empty() {
                        self.erase(&mut edited.echo);
                    }
                }
                b'\t' => {
                    self.line.push(byte);
                    edited.echo.push(byte);
                }
                // ^C, ^D and the like: they go now, after what is typed.
                0x00..=0x1f => {
                    edited.send.append(&mut self.line);
                    edited.send.push(byte);
                }
                _ => {
                    self.line.push(byte);
                    edited.echo.push(byte);
                }
            }
        }
        edited
    }

    /// What was typed and not sent, for when the server takes over the
    /// echo: it goes at once.
    pub fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.line)
    }

    /// Takes back the last character, a wide one over both its columns.
    fn erase(&mut self, echo: &mut Vec<u8>) {
        let start = if self.encoding == crate::host::TerminalEncoding::Utf8 {
            self.line.iter().rposition(|byte| byte & 0xc0 != 0x80)
        } else {
            self.line.len().checked_sub(1)
        };
        let Some(mut start) = start else {
            self.line.clear();
            return;
        };
        let decoded;
        let character = if self.encoding == crate::host::TerminalEncoding::Utf8 {
            std::str::from_utf8(&self.line[start..])
                .ok()
                .and_then(|text| text.chars().next())
        } else {
            decoded = crate::terminal::TerminalDecoder::new(self.encoding).decode(&self.line, true);
            let character = std::str::from_utf8(&decoded)
                .ok()
                .and_then(|text| text.chars().last());
            let size = character
                .and_then(|character| {
                    crate::terminal::encode_input(self.encoding, character.to_string().into_bytes())
                        .ok()
                })
                .map_or(1, |bytes| bytes.len());
            start = self.line.len().saturating_sub(size);
            character
        };
        let width = character
            .and_then(|character| character.width())
            .unwrap_or(1);
        self.line.truncate(start);
        echo.extend(std::iter::repeat_n(BACKSPACE, width));
        echo.extend(std::iter::repeat_n(b' ', width));
        echo.extend(std::iter::repeat_n(BACKSPACE, width));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_line_editing_erases_whole_multibyte_characters() {
        for (encoding, charset, text) in [
            (
                crate::host::TerminalEncoding::Gbk,
                encoding_rs::GBK,
                "a中文",
            ),
            (
                crate::host::TerminalEncoding::Gb18030,
                encoding_rs::GB18030,
                "a中😀",
            ),
        ] {
            let mut line = LocalLine::default().with_encoding(encoding);
            let (bytes, _, _) = charset.encode(text);
            assert_eq!(line.edit(&bytes).echo, bytes.as_ref());
            assert_eq!(line.edit(&[BACKSPACE]).echo, b"\x08\x08  \x08\x08");
            let (expected, _, _) = charset.encode("a中");
            assert_eq!(line.take(), expected.as_ref());
        }
    }

    fn edit(line: &mut LocalLine, typed: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let edited = line.edit(typed);
        (edited.send, edited.echo)
    }

    #[test]
    fn the_typing_shows_and_goes_on_enter() {
        let mut line = LocalLine::default();
        assert_eq!(edit(&mut line, b"he"), (vec![], b"he".to_vec()));
        assert_eq!(edit(&mut line, b"lo"), (vec![], b"lo".to_vec()));
        assert_eq!(
            edit(&mut line, b"\r"),
            (b"helo\r".to_vec(), b"\r\n".to_vec())
        );
        assert_eq!(
            edit(&mut line, b"\r"),
            (b"\r".to_vec(), b"\r\n".to_vec()),
            "an empty line is an Enter"
        );
    }

    #[test]
    fn backspace_takes_back_what_was_not_sent() {
        let mut line = LocalLine::default();
        edit(&mut line, b"datx");
        assert_eq!(edit(&mut line, &[DELETE]), (vec![], b"\x08 \x08".to_vec()));
        edit(&mut line, b"e");
        assert_eq!(edit(&mut line, b"\r").0, b"date\r");
        assert_eq!(
            edit(&mut line, &[BACKSPACE, DELETE]),
            (vec![], vec![]),
            "nothing to take back"
        );
    }

    #[test]
    fn a_wide_character_is_taken_back_whole() {
        let mut line = LocalLine::default();
        edit(&mut line, "a中".as_bytes());
        assert_eq!(
            edit(&mut line, &[DELETE]),
            (vec![], b"\x08\x08  \x08\x08".to_vec())
        );
        assert_eq!(edit(&mut line, b"\r").0, b"a\r");
    }

    #[test]
    fn ctrl_u_takes_back_the_line() {
        let mut line = LocalLine::default();
        edit(&mut line, b"ab");
        assert_eq!(
            edit(&mut line, &[KILL]),
            (vec![], b"\x08 \x08\x08 \x08".to_vec())
        );
        assert_eq!(edit(&mut line, b"\r").0, b"\r");
    }

    #[test]
    fn a_pasted_text_goes_a_line_at_a_time() {
        let mut line = LocalLine::default();
        assert_eq!(
            edit(&mut line, b"HELO me\r\nQUIT\r\nRS"),
            (
                b"HELO me\rQUIT\r".to_vec(),
                b"HELO me\r\nQUIT\r\nRS".to_vec()
            )
        );
        assert_eq!(
            edit(&mut line, b"\n"),
            (b"RS\r".to_vec(), b"\r\n".to_vec()),
            "a lone LF is an Enter too"
        );
    }

    #[test]
    fn keys_and_terminal_replies_go_as_they_are() {
        let mut line = LocalLine::default();
        edit(&mut line, b"ab");
        assert_eq!(edit(&mut line, b"\x1b[A"), (b"\x1b[A".to_vec(), vec![]));
        assert_eq!(edit(&mut line, b"\x1b[?1;2c").1, b"");
        assert_eq!(edit(&mut line, b"\r").0, b"ab\r", "the line is kept");
    }

    #[test]
    fn ctrl_c_goes_at_once_after_the_line() {
        let mut line = LocalLine::default();
        edit(&mut line, b"ab");
        assert_eq!(edit(&mut line, b"\x03"), (b"ab\x03".to_vec(), vec![]));
        assert!(line.take().is_empty());
    }

    #[test]
    fn what_was_not_sent_can_be_taken() {
        let mut line = LocalLine::default();
        edit(&mut line, b"pass");
        assert_eq!(line.take(), b"pass");
        assert_eq!(edit(&mut line, b"\r").0, b"\r");
    }
}
