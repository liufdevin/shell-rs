//! 系统监控: CPU, memory, network and disks in the right sidebar and optional
//! status bar, read over the front SSH terminal's own connection. Linux
//! hosts only, for now.

mod connection;
mod hardware;
mod linux;
mod model;
mod monitor_panel;
mod status;

pub use connection::{MonitorConnector, MonitorLink, SystemMonitorConnector};
pub use monitor_panel::MonitorPanel;
pub use status::MonitorStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonitorActivity {
    Inactive,
    StatusBar,
    Sidebar,
}

/// A part of the system monitor that is folded away until asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonitorDetail {
    /// Every core's load, under the average.
    Cores,
    /// Every network interface, not only the main one.
    Interfaces,
    HardwareChannels,
}
