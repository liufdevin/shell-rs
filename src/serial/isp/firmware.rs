use std::{collections::BTreeMap, path::Path};

const MAX_IMAGE: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct Firmware {
    pub(crate) address: u32,
    pub(crate) bytes: Vec<u8>,
}

fn invalid() -> String {
    crate::i18n::t!("serial.isp.invalid_image").to_string()
}

impl Firmware {
    pub(crate) fn load(path: &Path, binary_address: u32) -> Result<Self, String> {
        use std::io::Read as _;
        if !path.extension().is_some_and(|extension| {
            ["bin", "hex", "ihx", "ihex"]
                .iter()
                .any(|name| extension.eq_ignore_ascii_case(name))
        }) {
            return Err(invalid());
        }
        let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
        let mut bytes = Vec::new();
        file.take((MAX_IMAGE * 4 + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > MAX_IMAGE * 4 {
            return Err(invalid());
        }
        let hex = path.extension().is_some_and(|extension| {
            ["hex", "ihx", "ihex"]
                .iter()
                .any(|name| extension.eq_ignore_ascii_case(name))
        });
        Self::parse(&bytes, hex, binary_address)
    }

    pub(crate) fn parse(bytes: &[u8], hex: bool, address: u32) -> Result<Self, String> {
        if !hex {
            if bytes.is_empty() || bytes.len() > MAX_IMAGE {
                return Err(invalid());
            }
            address
                .checked_add(bytes.len() as u32)
                .ok_or_else(invalid)?;
            return Ok(Self {
                address,
                bytes: bytes.to_vec(),
            });
        }
        let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
        let mut data = BTreeMap::new();
        let mut base = 0_u32;
        let mut ended = false;
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            if ended {
                return Err(invalid());
            }
            let line = line.strip_prefix(':').ok_or_else(invalid)?;
            if line.len() % 2 != 0 || line.len() > 522 || !line.is_ascii() {
                return Err(invalid());
            }
            let record = line
                .as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| {
                    let pair = std::str::from_utf8(pair).map_err(|_| invalid())?;
                    u8::from_str_radix(pair, 16).map_err(|_| invalid())
                })
                .collect::<Result<Vec<_>, _>>()?;
            if record.len() < 5
                || usize::from(record[0]) + 5 != record.len()
                || record
                    .iter()
                    .fold(0_u8, |sum, byte| sum.wrapping_add(*byte))
                    != 0
            {
                return Err(invalid());
            }
            let offset = u32::from(u16::from_be_bytes([record[1], record[2]]));
            let content = &record[4..record.len() - 1];
            match record[3] {
                0 => {
                    if offset + content.len() as u32 > 65536 {
                        return Err(invalid());
                    }
                    for (index, byte) in content.iter().enumerate() {
                        let at = base
                            .checked_add(offset)
                            .and_then(|at| at.checked_add(index as u32))
                            .ok_or_else(invalid)?;
                        if data.insert(at, *byte).is_some() || data.len() > MAX_IMAGE {
                            return Err(invalid());
                        }
                    }
                }
                1 if content.is_empty() && offset == 0 => ended = true,
                2 | 4 if content.len() == 2 && offset == 0 => {
                    base = u32::from(u16::from_be_bytes([content[0], content[1]]))
                        << if record[3] == 2 { 4 } else { 16 };
                }
                3 | 5 if content.len() == 4 && offset == 0 => {}
                _ => return Err(invalid()),
            }
        }
        if !ended || data.is_empty() {
            return Err(invalid());
        }
        let first = *data.first_key_value().unwrap().0;
        let last = *data.last_key_value().unwrap().0;
        let length = last
            .checked_sub(first)
            .and_then(|length| length.checked_add(1))
            .ok_or_else(invalid)? as usize;
        if length > MAX_IMAGE || first.checked_add(length as u32).is_none() {
            return Err(invalid());
        }
        let mut bytes = vec![0xff; length];
        for (at, byte) in data {
            bytes[(at - first) as usize] = byte;
        }
        Ok(Self {
            address: first,
            bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_preserves_absolute_addresses_and_fills_holes() {
        let image = Firmware::parse(
            b":020000040800F2\n:0400000001020304F2\n:01000800AA4D\n:00000001FF\n",
            true,
            0,
        )
        .unwrap();
        assert_eq!(image.address, 0x08000000);
        assert_eq!(image.bytes, [1, 2, 3, 4, 255, 255, 255, 255, 0xaa]);
    }

    #[test]
    fn malformed_overlapping_unterminated_and_overflowing_images_are_rejected() {
        for text in [
            ":01000000AA54\n:00000001FF",
            ":01000000AA55\n:01000000AA55\n:00000001FF",
            ":01000000AA55",
            ":00000001FF\n:01000000AA55",
            ":0100000600F9\n:00000001FF",
            ":02000004FFFFFC\n:01FFFF00AA57\n:00000001FF",
            ":é",
        ] {
            assert!(Firmware::parse(text.as_bytes(), true, 0).is_err(), "{text}");
        }
        assert!(Firmware::parse(&[], false, 0).is_err());
        assert!(Firmware::parse(&[1, 2], false, u32::MAX).is_err());
    }
}
