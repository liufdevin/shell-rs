use zeroize::{Zeroize, Zeroizing};

use super::TerminalDecoder;
use crate::host::TerminalEncoding;

pub(crate) const PROMPT_LIMIT: usize = 512;

#[derive(Default)]
enum Escape {
    #[default]
    None,
    Esc,
    Csi,
    String,
    StringEsc,
}

#[derive(Default)]
pub(crate) struct PromptLine {
    line: Zeroizing<Vec<u8>>,
    escape: Escape,
}

impl PromptLine {
    pub(crate) fn clear(&mut self) {
        self.line.zeroize();
    }

    pub(crate) fn feed(&mut self, data: &[u8]) {
        for &byte in data {
            self.escape = match self.escape {
                Escape::None => {
                    match byte {
                        0x1b => {
                            self.escape = Escape::Esc;
                            continue;
                        }
                        b'\r' | b'\n' => self.clear(),
                        0x08 => {
                            self.line.pop();
                        }
                        0x00..=0x1f | 0x7f => {}
                        _ => self.line.push(byte),
                    }
                    Escape::None
                }
                Escape::Esc => match byte {
                    b'[' => Escape::Csi,
                    b']' | b'P' | b'X' | b'^' | b'_' => Escape::String,
                    0x20..=0x2f => Escape::Esc,
                    _ => Escape::None,
                },
                Escape::Csi if (0x40..=0x7e).contains(&byte) => Escape::None,
                Escape::Csi => Escape::Csi,
                Escape::String => match byte {
                    0x07 => Escape::None,
                    0x1b => Escape::StringEsc,
                    _ => Escape::String,
                },
                Escape::StringEsc if byte == b'\\' => Escape::None,
                Escape::StringEsc => Escape::String,
            };
        }
        if self.line.len() > PROMPT_LIMIT {
            let excess = self.line.len() - PROMPT_LIMIT;
            self.line.drain(..excess);
        }
    }

    pub(crate) fn normalized(&self, encoding: TerminalEncoding) -> Zeroizing<String> {
        let bytes = Zeroizing::new(
            TerminalDecoder::new(encoding)
                .decode(&self.line, false)
                .into_owned(),
        );
        Zeroizing::new(String::from_utf8_lossy(&bytes).trim_end().to_lowercase())
    }
}
