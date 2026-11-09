#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SerialLineEnding {
    #[default]
    None,
    Lf,
    Cr,
    CrLf,
}

impl SerialLineEnding {
    pub(crate) const ALL: [Self; 4] = [Self::None, Self::Lf, Self::Cr, Self::CrLf];

    pub(crate) fn label(self) -> gpui_kit::SharedString {
        match self {
            Self::None => crate::i18n::t!("serial.tool.no_newline"),
            Self::Lf => "LF".into(),
            Self::Cr => "CR".into(),
            Self::CrLf => "CRLF".into(),
        }
    }
}

pub(crate) fn prepare_message(mut bytes: Vec<u8>, ending: SerialLineEnding, crc: bool) -> Vec<u8> {
    if crc {
        let mut checksum = 0xffffu16;
        for byte in &bytes {
            checksum ^= u16::from(*byte);
            for _ in 0..8 {
                checksum = if checksum & 1 != 0 {
                    (checksum >> 1) ^ 0xa001
                } else {
                    checksum >> 1
                };
            }
        }
        bytes.extend_from_slice(&checksum.to_le_bytes());
    }
    bytes.extend_from_slice(match ending {
        SerialLineEnding::None => b"",
        SerialLineEnding::Lf => b"\n",
        SerialLineEnding::Cr => b"\r",
        SerialLineEnding::CrLf => b"\r\n",
    });
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_endings_are_explicit_and_default_to_no_extra_bytes() {
        assert_eq!(
            prepare_message(b"hello".to_vec(), SerialLineEnding::default(), false),
            b"hello"
        );
        assert_eq!(
            prepare_message(b"hello".to_vec(), SerialLineEnding::Lf, false),
            b"hello\n"
        );
        assert_eq!(
            prepare_message(b"hello".to_vec(), SerialLineEnding::Cr, false),
            b"hello\r"
        );
        assert_eq!(
            prepare_message(b"hello".to_vec(), SerialLineEnding::CrLf, false),
            b"hello\r\n"
        );
    }

    #[test]
    fn modbus_crc_uses_wire_bytes_and_little_endian_before_the_line_ending() {
        assert_eq!(
            prepare_message(b"123456789".to_vec(), SerialLineEnding::None, true),
            b"123456789\x37\x4b"
        );
        assert_eq!(
            prepare_message(vec![1, 3, 0, 0, 0, 10], SerialLineEnding::CrLf, true),
            vec![1, 3, 0, 0, 0, 10, 0xc5, 0xcd, 13, 10]
        );
    }
}
