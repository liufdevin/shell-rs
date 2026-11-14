mod dialog;
#[cfg(windows)]
mod windows;

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

pub use dialog::{SerialMaintenance, SerialMaintenanceCommand};

pub const MAX_REMOVALS: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct SerialDevice {
    instance_id: String,
    port: String,
    name: String,
    present: Option<bool>,
}

impl SerialDevice {
    pub fn new(instance_id: String, port: String, name: String, present: Option<bool>) -> Self {
        Self {
            instance_id,
            port,
            name,
            present,
        }
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }
    pub fn port(&self) -> &str {
        &self.port
    }
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_candidate(&self) -> bool {
        self.present == Some(false) && valid_id(&self.instance_id) && valid_port(&self.port)
    }
}

fn valid_id(value: &str) -> bool {
    value.len() > 4
        && value.len() <= 512
        && value.to_ascii_uppercase().starts_with("USB\\")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"\\&_.#{}+-".contains(&byte))
}

fn valid_port(value: &str) -> bool {
    value.strip_prefix("COM").is_some_and(|number| {
        !number.starts_with('0')
            && number.bytes().all(|byte| byte.is_ascii_digit())
            && number.len() <= 5
            && number.parse::<u16>().is_ok_and(|number| number != 0)
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalState {
    Removed,
    Present,
    Gone,
    Changed,
    RebootRequired,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct SerialRemoval {
    instance_id: String,
    state: RemovalState,
    detail: String,
}

impl SerialRemoval {
    pub fn new(instance_id: String, state: RemovalState, detail: String) -> Self {
        Self {
            instance_id,
            state,
            detail,
        }
    }
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }
    pub fn state(&self) -> &RemovalState {
        &self.state
    }
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MaintenanceFailure {
    Unsupported,
    Cancelled,
    TimedOut,
    Failed(String),
}

pub trait SerialMaintenanceBackend: Send + Sync + 'static {
    fn is_supported(&self) -> bool;
    fn scan(&self) -> Result<Vec<SerialDevice>, MaintenanceFailure>;
    fn remove(&self, devices: &[SerialDevice]) -> Result<Vec<SerialRemoval>, MaintenanceFailure>;
}

pub struct SystemSerialMaintenance;

impl SerialMaintenanceBackend for SystemSerialMaintenance {
    fn is_supported(&self) -> bool {
        cfg!(windows)
    }

    fn scan(&self) -> Result<Vec<SerialDevice>, MaintenanceFailure> {
        #[cfg(windows)]
        {
            windows::scan()
        }
        #[cfg(not(windows))]
        {
            Err(MaintenanceFailure::Unsupported)
        }
    }

    fn remove(&self, devices: &[SerialDevice]) -> Result<Vec<SerialRemoval>, MaintenanceFailure> {
        validate_selection(devices)?;
        #[cfg(windows)]
        {
            windows::remove(devices)
        }
        #[cfg(not(windows))]
        {
            Err(MaintenanceFailure::Unsupported)
        }
    }
}

fn validate_selection(devices: &[SerialDevice]) -> Result<(), MaintenanceFailure> {
    let mut ids = HashSet::new();
    if devices.is_empty()
        || devices.len() > MAX_REMOVALS
        || devices.iter().any(|device| {
            !device.is_candidate() || !ids.insert(device.instance_id.to_ascii_uppercase())
        })
    {
        return Err(MaintenanceFailure::Failed("invalid_selection".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicitly_absent_usb_com_devices_are_candidates() {
        let absent = SerialDevice::new(
            "USB\\VID_1234&PID_5678\\ABC".into(),
            "COM18".into(),
            "USB Serial".into(),
            Some(false),
        );
        assert!(absent.is_candidate());
        for presence in [None, Some(true)] {
            assert!(
                !SerialDevice {
                    present: presence,
                    ..absent.clone()
                }
                .is_candidate()
            );
        }
        for port in ["LPT1", "COM0", "COM01", "COM+1", "COM1 /force", "COM65536"] {
            assert!(
                !SerialDevice {
                    port: port.into(),
                    ..absent.clone()
                }
                .is_candidate()
            );
        }
        for id in [
            "FTDIBUS\\OTHER",
            "USB\\*",
            "USB\\A';&",
            "USB\\[ABC]",
            "USB\\A\nB",
        ] {
            assert!(
                !SerialDevice {
                    instance_id: id.into(),
                    ..absent.clone()
                }
                .is_candidate()
            );
        }
    }

    #[test]
    fn removal_rejects_empty_duplicate_and_oversized_plans() {
        let device = SerialDevice::new("USB\\ONE".into(), "COM1".into(), "one".into(), Some(false));
        assert!(validate_selection(std::slice::from_ref(&device)).is_ok());
        assert!(validate_selection(&[]).is_err());
        assert!(validate_selection(&[device.clone(), device.clone()]).is_err());
        assert!(validate_selection(&vec![device; MAX_REMOVALS + 1]).is_err());
    }
}
