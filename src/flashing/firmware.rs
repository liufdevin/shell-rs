use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct FirmwareCapabilities {
    pub plugin_product_version: Option<[u16; 4]>,
}

impl FirmwareCapabilities {
    pub fn full_erase(&self) -> bool {
        self.plugin_product_version
            .is_some_and(|version| version >= [1, 0, 0, 1])
    }
}

pub(super) fn read_capabilities(path: &Path) -> FirmwareCapabilities {
    File::open(path)
        .and_then(|mut file| inspect(&mut file))
        .unwrap_or_default()
}

fn invalid_image() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Invalid IMAGEWTY metadata")
}

fn word(bytes: &[u8], offset: usize) -> io::Result<u32> {
    bytes
        .get(offset..offset + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(invalid_image)
}

fn inspect(reader: &mut (impl Read + Seek)) -> io::Result<FirmwareCapabilities> {
    let size = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;
    let mut header = [0u8; 64];
    reader.read_exact(&mut header)?;
    if &header[..8] != b"IMAGEWTY" {
        return Err(invalid_image());
    }
    let version = word(&header, 8)?;
    let (stride_offset, count_offset) = match version {
        0x100 => (28, 56),
        0x300 | 0x403 => (32, 60),
        _ => return Err(invalid_image()),
    };
    let stride = u64::from(word(&header, stride_offset)?);
    let count = u64::from(word(&header, count_offset)?);
    if !(512..=4096).contains(&stride)
        || count > 4096
        || stride.checked_mul(count + 1).is_none_or(|end| end > size)
    {
        return Err(invalid_image());
    }
    let mut capabilities = FirmwareCapabilities::default();
    for index in 0..count {
        reader.seek(SeekFrom::Start(stride * (index + 1)))?;
        let mut entry = vec![0u8; stride as usize];
        reader.read_exact(&mut entry)?;
        let name_length = word(&entry, 0)? as usize;
        if name_length > 256 {
            return Err(invalid_image());
        }
        let (name_offset, length_offset, position_offset) = if version == 0x100 {
            (52, 36, 44)
        } else {
            (36, 36 + name_length, 52 + name_length)
        };
        let name = entry
            .get(name_offset..name_offset + name_length)
            .ok_or_else(invalid_image)?;
        let name = String::from_utf8_lossy(name)
            .trim_matches('\0')
            .to_ascii_lowercase();
        let plugin = name == "usbtool.fex" || entry.get(8..16) == Some(b"PXTOOLSB");
        if !plugin {
            continue;
        }
        let mut length = u64::from(word(&entry, length_offset)?);
        let position = u64::from(word(&entry, position_offset)?);
        if version != 0x100 {
            length |= u64::from(word(&entry, length_offset + 4)?) << 32;
        }
        if position.checked_add(length).is_none_or(|end| end > size) || length > 4 * 1024 * 1024 {
            return Err(invalid_image());
        }
        reader.seek(SeekFrom::Start(position))?;
        let mut payload = vec![0u8; length as usize];
        reader.read_exact(&mut payload)?;
        capabilities.plugin_product_version = plugin_product_version(&payload);
        break;
    }
    Ok(capabilities)
}

fn plugin_product_version(bytes: &[u8]) -> Option<[u16; 4]> {
    if !bytes.starts_with(b"MZ") {
        return None;
    }
    let pe = word(bytes, 60).ok()? as usize;
    if bytes.get(pe..pe.checked_add(4)?)? != b"PE\0\0" {
        return None;
    }
    bytes.windows(52).find_map(|info| {
        if word(info, 0).ok()? != 0xfeef04bd || word(info, 4).ok()? != 0x10000 {
            return None;
        }
        let major = word(info, 16).ok()?;
        let minor = word(info, 20).ok()?;
        Some([
            (major >> 16) as u16,
            major as u16,
            (minor >> 16) as u16,
            minor as u16,
        ])
    })
}

#[cfg(test)]
mod tests {
    use super::{FirmwareCapabilities, inspect, plugin_product_version};
    use std::io::Cursor;

    #[test]
    fn only_full_erase_requires_the_plugin_product_version() {
        assert!(!FirmwareCapabilities::default().full_erase());
        let mut info = FirmwareCapabilities {
            plugin_product_version: Some([1, 0, 0, 0]),
        };
        assert!(!info.full_erase());
        info.plugin_product_version = Some([1, 0, 0, 1]);
        assert!(info.full_erase());
    }

    #[test]
    fn reads_the_product_version_instead_of_the_file_version_without_loading_code() {
        let mut bytes = vec![0u8; 160];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        for (offset, value) in [
            (80, 0xfeef04bdu32),
            (84, 0x10000),
            (88, 0x90000),
            (92, 9),
            (96, 0x10000),
            (100, 1),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        assert_eq!(plugin_product_version(&bytes), Some([1, 0, 0, 1]));
        bytes[100..104].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(plugin_product_version(&bytes), Some([1, 0, 0, 0]));
        bytes[64] = 0;
        assert!(plugin_product_version(&bytes).is_none());
    }

    #[test]
    fn validates_image_table_offsets_and_unknown_formats() {
        let mut bytes = vec![0u8; 1024];
        bytes[..8].copy_from_slice(b"IMAGEWTY");
        bytes[8..12].copy_from_slice(&0x300u32.to_le_bytes());
        bytes[32..36].copy_from_slice(&1024u32.to_le_bytes());
        assert_eq!(
            inspect(&mut Cursor::new(&bytes)).unwrap(),
            FirmwareCapabilities::default()
        );
        bytes[60..64].copy_from_slice(&2u32.to_le_bytes());
        assert!(inspect(&mut Cursor::new(&bytes)).is_err());
        assert!(inspect(&mut Cursor::new([0u8; 64])).is_err());
    }

    #[test]
    fn reads_embedded_product_versions_in_each_supported_image_format() {
        for version in [0x100u32, 0x300, 0x403] {
            let mut bytes = vec![0u8; 4096];
            bytes[..8].copy_from_slice(b"IMAGEWTY");
            bytes[8..12].copy_from_slice(&version.to_le_bytes());
            let (stride_offset, count_offset) = if version == 0x100 { (28, 56) } else { (32, 60) };
            bytes[stride_offset..stride_offset + 4].copy_from_slice(&1024u32.to_le_bytes());
            bytes[count_offset..count_offset + 4].copy_from_slice(&2u32.to_le_bytes());
            for (index, name, position, length) in [
                (1, b"usbtool.fex".as_slice(), 3072u32, 160u32),
                (2, b"u-boot.fex".as_slice(), 3584u32, 16u32),
            ] {
                let entry = &mut bytes[index * 1024..(index + 1) * 1024];
                entry[..4].copy_from_slice(&256u32.to_le_bytes());
                let (name_offset, length_offset, position_offset) = if version == 0x100 {
                    (52, 36, 44)
                } else {
                    (36, 292, 308)
                };
                entry[name_offset..name_offset + name.len()].copy_from_slice(name);
                entry[length_offset..length_offset + 4].copy_from_slice(&length.to_le_bytes());
                entry[position_offset..position_offset + 4]
                    .copy_from_slice(&position.to_le_bytes());
            }
            let plugin = &mut bytes[3072..3232];
            plugin[..2].copy_from_slice(b"MZ");
            plugin[60..64].copy_from_slice(&64u32.to_le_bytes());
            plugin[64..68].copy_from_slice(b"PE\0\0");
            for (offset, value) in [(80, 0xfeef04bdu32), (84, 0x10000), (96, 0x10000), (100, 1)] {
                plugin[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            }
            bytes[3584..3600].copy_from_slice(b"U-Boot 2020.04\0\0");
            let info = inspect(&mut Cursor::new(&bytes)).unwrap();
            assert_eq!(info.plugin_product_version, Some([1, 0, 0, 1]));
            assert!(info.full_erase());
            bytes[3152..3204].fill(0);
            let legacy = inspect(&mut Cursor::new(&bytes)).unwrap();
            assert!(legacy.plugin_product_version.is_none());
            assert!(!legacy.full_erase());
            bytes.truncate(3190);
            assert!(inspect(&mut Cursor::new(&bytes)).is_err());
        }
    }

    #[test]
    #[ignore = "reads an explicitly selected local or network IMG without starting a flashing tool"]
    fn inspects_an_explicit_firmware_without_starting_a_tool() {
        let path =
            std::env::var_os("SHELLRS_TEST_FIRMWARE").expect("SHELLRS_TEST_FIRMWARE is required");
        let mut file = std::fs::File::open(path).unwrap();
        let capabilities = inspect(&mut file).unwrap();
        println!("Firmware capabilities: {capabilities:?}");
        assert_eq!(
            capabilities.full_erase(),
            std::env::var("SHELLRS_TEST_FULL_ERASE").unwrap() == "1"
        );
    }
}
