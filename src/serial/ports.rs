use std::cmp::Ordering;
use std::path::Path;

/// The serial ports of this computer, in the order a person looks for one
/// (`COM3` before `COM10`): what the host form's 串口设备 offers. Reading
/// them is quick, but it is a file system walk; callers keep it off the UI
/// thread.
pub fn available_ports() -> Vec<String> {
    read_ports().unwrap_or_default()
}

pub(super) fn read_ports() -> std::io::Result<Vec<String>> {
    let ports = serial2::SerialPort::available_ports()?;
    let mut names: Vec<String> = ports
        .iter()
        .filter(|path| offered(path))
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    names.sort_by(|a, b| natural_order(a, b));
    names.dedup();
    Ok(names)
}

/// macOS has every port twice: a `tty.*` that waits for the other end's
/// carrier, and the `cu.*` for calling out, which is what a console wants.
fn offered(path: &Path) -> bool {
    !cfg!(target_os = "macos")
        || path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("cu."))
}

/// By name, but a trailing number by its value.
fn natural_order(a: &str, b: &str) -> Ordering {
    fn split(name: &str) -> (&str, Option<u64>) {
        let stem = name.trim_end_matches(|c: char| c.is_ascii_digit());
        (stem, name[stem.len()..].parse().ok())
    }
    split(a).cmp(&split(b)).then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trailing_number_counts_by_value() {
        let mut names = vec!["COM10", "COM3", "/dev/ttyUSB1", "/dev/ttyACM0", "COM1"];
        names.sort_by(|a, b| natural_order(a, b));
        assert_eq!(
            names,
            ["/dev/ttyACM0", "/dev/ttyUSB1", "COM1", "COM3", "COM10"]
        );
    }

    #[test]
    fn macos_offers_the_call_out_devices_only() {
        let calls_out = offered(Path::new("/dev/cu.usbserial-1410"));
        let waits = offered(Path::new("/dev/tty.usbserial-1410"));
        assert!(calls_out);
        assert_eq!(waits, !cfg!(target_os = "macos"));
    }
}
