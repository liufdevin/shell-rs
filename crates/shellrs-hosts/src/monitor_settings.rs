use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorChannel {
    #[default]
    Auto,
    Adb,
    Ssh,
    Serial,
}

impl MonitorChannel {
    pub const ALL: [Self; 4] = [Self::Auto, Self::Adb, Self::Ssh, Self::Serial];
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct MonitorSettings {
    pub channel: MonitorChannel,
    pub adb_serial: String,
    pub ssh_host: String,
}
