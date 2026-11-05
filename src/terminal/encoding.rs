use std::borrow::Cow;

use encoding_rs::{CoderResult, Decoder, Encoding};
use gpui_kit::SharedString;

use crate::{host::TerminalEncoding, i18n::t};

fn charset(encoding: TerminalEncoding) -> &'static Encoding {
    match encoding {
        TerminalEncoding::Utf8 => encoding_rs::UTF_8,
        TerminalEncoding::Gbk => encoding_rs::GBK,
        TerminalEncoding::Gb18030 => encoding_rs::GB18030,
    }
}

pub(crate) fn encode_input(
    encoding: TerminalEncoding,
    bytes: Vec<u8>,
) -> Result<Vec<u8>, SharedString> {
    if encoding == TerminalEncoding::Utf8 || bytes.is_ascii() {
        return Ok(bytes);
    }
    let error = || {
        t!(
            "terminal.encoding.unrepresentable",
            encoding = encoding.label()
        )
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| error())?;
    let (encoded, _, errors) = charset(encoding).encode(text);
    if errors {
        return Err(error());
    }
    Ok(encoded.into_owned())
}

pub(crate) struct TerminalDecoder {
    encoding: TerminalEncoding,
    decoder: Option<Decoder>,
}

pub(crate) fn encode_secret(
    encoding: TerminalEncoding,
    bytes: &[u8],
) -> Result<zeroize::Zeroizing<Vec<u8>>, SharedString> {
    let error = || {
        t!(
            "terminal.encoding.unrepresentable",
            encoding = encoding.label()
        )
    };
    let text = std::str::from_utf8(bytes).map_err(|_| error())?;
    let (encoded, _, errors) = charset(encoding).encode(text);
    let encoded = zeroize::Zeroizing::new(encoded.into_owned());
    if errors { Err(error()) } else { Ok(encoded) }
}

impl TerminalDecoder {
    pub fn new(encoding: TerminalEncoding) -> Self {
        Self {
            encoding,
            decoder: (encoding != TerminalEncoding::Utf8)
                .then(|| charset(encoding).new_decoder_without_bom_handling()),
        }
    }

    pub fn decode<'a>(&mut self, bytes: &'a [u8], last: bool) -> Cow<'a, [u8]> {
        let Some(decoder) = &mut self.decoder else {
            return Cow::Borrowed(bytes);
        };
        let mut text = String::with_capacity(bytes.len().saturating_mul(3).saturating_add(16));
        let mut read = 0;
        loop {
            let (result, consumed, _) = decoder.decode_to_string(&bytes[read..], &mut text, last);
            read += consumed;
            if result == CoderResult::InputEmpty {
                break;
            }
            text.reserve(bytes.len().saturating_mul(3).saturating_add(16));
        }
        Cow::Owned(text.into_bytes())
    }

    pub fn finish(&mut self) -> Vec<u8> {
        let tail = self.decode(&[], true).into_owned();
        *self = Self::new(self.encoding);
        tail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_text_decodes_across_every_byte_boundary_with_ansi_intact() {
        for encoding in [TerminalEncoding::Gbk, TerminalEncoding::Gb18030] {
            let text = "\x1b[31m中文 测试\x1b[0m\r\n";
            let wire = encode_input(encoding, text.as_bytes().to_vec()).unwrap();
            for split in 0..=wire.len() {
                let mut decoder = TerminalDecoder::new(encoding);
                let mut output = decoder.decode(&wire[..split], false).into_owned();
                output.extend_from_slice(&decoder.decode(&wire[split..], false));
                output.extend(decoder.finish());
                assert_eq!(output, text.as_bytes());
            }
            let mut decoder = TerminalDecoder::new(encoding);
            let mut output = Vec::new();
            for byte in &wire {
                output.extend_from_slice(&decoder.decode(std::slice::from_ref(byte), false));
            }
            output.extend(decoder.finish());
            assert_eq!(output, text.as_bytes());
        }
    }

    #[test]
    fn gb18030_supports_four_byte_characters_and_gbk_rejects_them() {
        let text = "中文😀𠮷";
        assert!(encode_input(TerminalEncoding::Gbk, text.as_bytes().to_vec()).is_err());
        let bytes = encode_input(TerminalEncoding::Gb18030, text.as_bytes().to_vec()).unwrap();
        let mut decoder = TerminalDecoder::new(TerminalEncoding::Gb18030);
        let mut output = Vec::new();
        for byte in bytes {
            output.extend_from_slice(&decoder.decode(&[byte], false));
        }
        assert_eq!(output, text.as_bytes());
    }

    #[test]
    fn disconnect_flushes_an_incomplete_character_and_resets_the_decoder() {
        let mut decoder = TerminalDecoder::new(TerminalEncoding::Gbk);
        assert!(decoder.decode(&[0xd6], false).is_empty());
        assert_eq!(decoder.finish(), "�".as_bytes());
        assert_eq!(
            decoder.decode(b"new connection", false).as_ref(),
            b"new connection"
        );
        let bytes = encode_input(TerminalEncoding::Utf8, vec![0xff, 0, 0x1b]).unwrap();
        assert_eq!(
            TerminalDecoder::new(TerminalEncoding::Utf8)
                .decode(&bytes, false)
                .as_ref(),
            bytes
        );
    }
}
