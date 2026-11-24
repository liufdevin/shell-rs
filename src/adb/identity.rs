//! Multi-factor identity matching between a serial console and ADB devices.
//!
//! Supports both Android (where `boot_id` may be blocked by SELinux, while
//! `ro.serialno` is directly available) and Ubuntu/Linux boards running `adbd`
//! (where `boot_id`, `/etc/machine-id`, and SoC chip IDs are available, but
//! multiple boards often share a hardcoded USB gadget serial like `20080411`).

use super::canonical_boot_id;

/// POSIX-universal identity probe run over `adb shell` in a single round trip.
pub(crate) const ADB_IDENTITY_COMMAND: &str = concat!(
    "printf '@@boot_id\\n'; cat /proc/sys/kernel/random/boot_id 2>/dev/null; ",
    "printf '\\n@@machine_id\\n'; cat /etc/machine-id /var/lib/dbus/machine-id 2>/dev/null; ",
    "printf '\\n@@hw_serial\\n'; cat /sys/class/sunxi_info/sys_info /sys/firmware/devicetree/base/serial-number 2>/dev/null"
);

/// Read and clean up the temporary nonce probe file over `adb shell`.
pub(crate) const NONCE_READ_COMMAND: &str = concat!(
    "for p in /data/local/tmp/.shelllf_probe /tmp/.shelllf_probe; do ",
    "if [ -f \"$p\" ]; then cat \"$p\" 2>/dev/null; printf '\\n'; break; fi; done"
);

pub(crate) const NONCE_CLEAN_COMMAND: &str =
    "rm -f /data/local/tmp/.shelllf_probe /tmp/.shelllf_probe 2>/dev/null";

pub(crate) fn generate_nonce() -> String {
    format!("shelllf-{}", uuid::Uuid::new_v4().simple())
}

pub(crate) fn nonce_write_command(nonce: &str) -> String {
    format!(
        "for d in /data/local/tmp /tmp; do if [ -d \"$d\" ] && [ -w \"$d\" ]; then printf '%s\\n' '{nonce}' > \"$d/.shelllf_probe\" && printf '@@nonce_ok\\n' && break; fi; done"
    )
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoardIdentity {
    pub boot_id: Option<String>,
    pub machine_id: Option<String>,
    pub hardware_serial: Option<String>,
    pub adb_serial: Option<String>,
}

impl BoardIdentity {
    pub fn from_boot_id(boot_id: Option<&str>) -> Self {
        Self {
            boot_id: boot_id.and_then(canonical_boot_id),
            ..Self::default()
        }
    }

    pub fn parse(output: &str) -> Self {
        if !output.contains("@@") {
            return Self::from_boot_id(Some(output));
        }
        let boot_id = section(output, "@@boot_id").and_then(|block| {
            block
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .and_then(canonical_boot_id)
        });
        let machine_id = section(output, "@@machine_id")
            .and_then(|block| block.lines().map(str::trim).find_map(canonical_machine_id));
        let hardware_serial = section(output, "@@hw_serial").and_then(parse_hardware_serial);
        let adb_serial = section(output, "@@adb_serial")
            .and_then(|block| block.lines().map(str::trim).find_map(canonical_adb_serial));
        Self {
            boot_id,
            machine_id,
            hardware_serial,
            adb_serial,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.boot_id.is_none()
            && self.machine_id.is_none()
            && self.hardware_serial.is_none()
            && self.adb_serial.is_none()
    }

    /// Returns `true` if `self` (typically collected from the serial console)
    /// strongly identifies the same board as `device_identity` (from ADB).
    ///
    /// - Conflicting `boot_id` values always reject a match (protects against
    ///   cloned OS images that accidentally share `/etc/machine-id` or a
    ///   hardcoded USB gadget serial).
    /// - `adb_serial` is only accepted when `unique_serial` is `true` (so two
    ///   Ubuntu boards sharing `20080411` will never falsely match by serial).
    pub fn matches(
        &self,
        device_serial: &str,
        device_identity: &BoardIdentity,
        unique_serial: bool,
    ) -> bool {
        if let (Some(expected), Some(actual)) = (&self.boot_id, &device_identity.boot_id)
            && expected != actual
        {
            return false;
        }
        if let (Some(expected), Some(actual)) =
            (&self.hardware_serial, &device_identity.hardware_serial)
            && expected != actual
        {
            return false;
        }
        if self.boot_id.is_some() && self.boot_id == device_identity.boot_id {
            return true;
        }
        if self.hardware_serial.is_some() && self.hardware_serial == device_identity.hardware_serial
        {
            return true;
        }
        if self.machine_id.is_some() && self.machine_id == device_identity.machine_id {
            return true;
        }
        if unique_serial
            && let Some(expected_serial) = &self.adb_serial
            && expected_serial == device_serial
        {
            return true;
        }
        false
    }

    /// Returns `true` when two ADB connections provably belong to the same
    /// running board (e.g., USB ADB + TCP/Wi-Fi ADB on the same device).
    pub fn same_board(&self, other: &BoardIdentity) -> bool {
        (self.boot_id.is_some() && self.boot_id == other.boot_id)
            || (self.hardware_serial.is_some() && self.hardware_serial == other.hardware_serial)
    }
}

/// Network ADB serials take `host:port` form (e.g. `192.168.1.50:5555`).
pub(crate) fn is_network_adb_serial(serial: &str) -> bool {
    serial.contains(':')
}

/// Resolve a list of matched ADB candidates into a single unambiguous device.
///
/// When multiple candidates match the same board over both USB and TCP/Wi-Fi
/// ADB, automatically prefers the single USB connection instead of forcing a
/// manual chooser dialog.
pub(crate) fn pick_preferred_match<T>(
    mut matches: Vec<T>,
    is_network: impl Fn(&T) -> bool,
    same_board: impl Fn(&T, &T) -> bool,
) -> Option<T> {
    if matches.is_empty() {
        return None;
    }
    if matches.len() == 1 {
        return matches.pop();
    }
    let all_same_board = matches
        .iter()
        .skip(1)
        .all(|item| same_board(&matches[0], item));
    if !all_same_board {
        return None;
    }
    let usb_indices: Vec<usize> = matches
        .iter()
        .enumerate()
        .filter(|(_, item)| !is_network(item))
        .map(|(index, _)| index)
        .collect();
    let has_network = matches.iter().any(&is_network);
    if usb_indices.len() == 1 && has_network {
        Some(matches.remove(usb_indices[0]))
    } else {
        None
    }
}

fn section<'a>(output: &'a str, marker: &str) -> Option<&'a str> {
    let (_, after) = output.split_once(marker)?;
    let after = after
        .strip_prefix("\r\n")
        .or_else(|| after.strip_prefix('\n'))?;
    let end = after.find("\n@@").unwrap_or(after.len());
    Some(&after[..end])
}

fn canonical_machine_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_matches('\0').to_ascii_lowercase();
    if !(16..=64).contains(&trimmed.len())
        || !trimmed.bytes().all(|b| b.is_ascii_hexdigit())
        || trimmed.bytes().all(|b| b == b'0')
    {
        return None;
    }
    Some(trimmed)
}

fn parse_hardware_serial(block: &str) -> Option<String> {
    // 1. Allwinner `/sys/class/sunxi_info/sys_info`:
    //    `sunxi_chipid      : 1234567890abcdef...`
    //    `sunxi_serial      : ...`
    for prefix in ["sunxi_chipid", "sunxi_serial", "serial"] {
        for line in block.lines() {
            let clean = line.replace('\0', "");
            let trimmed = clean.trim();
            if let Some((key, value)) = trimmed.split_once(':')
                && key.trim().eq_ignore_ascii_case(prefix)
                && let Some(token) = canonical_hw_token(value)
            {
                return Some(token);
            }
        }
    }
    // 2. Plain device-tree `/sys/firmware/devicetree/base/serial-number`
    for line in block.lines() {
        let clean = line.replace('\0', "");
        let trimmed = clean.trim();
        if !trimmed.contains(':')
            && let Some(token) = canonical_hw_token(trimmed)
        {
            return Some(token);
        }
    }
    None
}

fn canonical_hw_token(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_matches('\0').to_ascii_lowercase();
    if !(8..=128).contains(&trimmed.len())
        || !trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        || trimmed.bytes().all(|b| b == b'0' || b == b'-')
    {
        return None;
    }
    Some(trimmed)
}

fn canonical_adb_serial(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_matches('\0');
    if trimmed.is_empty()
        || trimmed.len() > 128
        || trimmed.starts_with('-')
        || trimmed
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
        || trimmed.eq_ignore_ascii_case("unknown")
    {
        return None;
    }
    Some(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOT_A: &str = "00000000-0000-4000-8000-000000000001";
    const BOOT_B: &str = "00000000-0000-4000-8000-000000000002";

    #[test]
    fn parses_android_and_ubuntu_multi_factor_identity() {
        let ubuntu = format!(
            "@@boot_id\n{BOOT_A}\n@@machine_id\n0123456789abcdef0123456789abcdef\n\
             @@hw_serial\nsunxi_platform    : sun50iw10p1\nsunxi_chipid      : 54005034000000000102030405060708\n\
             @@adb_serial\n20080411\n@@user\nroot\n"
        );
        let parsed = BoardIdentity::parse(&ubuntu);
        assert_eq!(parsed.boot_id.as_deref(), Some(BOOT_A));
        assert_eq!(
            parsed.machine_id.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(
            parsed.hardware_serial.as_deref(),
            Some("54005034000000000102030405060708")
        );
        assert_eq!(parsed.adb_serial.as_deref(), Some("20080411"));

        // Android without readable boot_id still matches via ro.serialno when unique.
        let android = "@@boot_id\n\n@@machine_id\n\n@@hw_serial\n\n@@adb_serial\nR5CN90ABCDE\n";
        let android_id = BoardIdentity::parse(android);
        assert!(android_id.boot_id.is_none());
        assert_eq!(android_id.adb_serial.as_deref(), Some("R5CN90ABCDE"));
        assert!(android_id.matches("R5CN90ABCDE", &BoardIdentity::default(), true));
        assert!(!android_id.matches("R5CN90ABCDE", &BoardIdentity::default(), false));
    }

    #[test]
    fn conflicting_boot_id_blocks_cloned_machine_id_or_shared_serial() {
        let serial_board = BoardIdentity {
            boot_id: Some(BOOT_A.into()),
            machine_id: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
            hardware_serial: None,
            adb_serial: Some("20080411".into()),
        };
        let other_board = BoardIdentity {
            boot_id: Some(BOOT_B.into()),
            machine_id: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
            hardware_serial: None,
            adb_serial: None,
        };
        assert!(!serial_board.matches("20080411", &other_board, true));
    }

    #[test]
    fn prefers_usb_over_network_adb_for_same_board_but_rejects_two_usb_duplicates() {
        let usb_and_tcp = vec![("192.168.1.50:5555", BOOT_A), ("A133-BOARD", BOOT_A)];
        let picked = pick_preferred_match(
            usb_and_tcp,
            |(serial, _)| is_network_adb_serial(serial),
            |a, b| a.1 == b.1,
        );
        assert_eq!(picked, Some(("A133-BOARD", BOOT_A)));

        let two_usb = vec![("usb-a", BOOT_A), ("usb-b", BOOT_A)];
        assert!(
            pick_preferred_match(
                two_usb,
                |(serial, _)| is_network_adb_serial(serial),
                |a, b| a.1 == b.1
            )
            .is_none()
        );
    }
}
