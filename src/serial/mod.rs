//! 串口: the serial ports of this computer, for serial hosts. Which ports
//! there are, for the host form, and the terminal transport that opens one.

mod assistant_panel;
mod command_editor;
mod diagnostics;
mod file_picker;
pub mod files;
pub mod isp;
pub mod maintenance;
mod message;
mod ports;
mod preferences;
mod presence;
mod tools;
mod transport;
#[cfg(windows)]
mod windows;

pub use assistant_panel::SerialAssistantTabCommand;
pub(crate) use assistant_panel::{SerialAssistantPanel, SerialAssistantPanelEvent};
pub(crate) use command_editor::{SerialCommandEditorEvent, SerialCommandEditorPanel};
pub use diagnostics::SerialDiagnostics;
pub use preferences::SerialAssistantPreferences;
pub(crate) use preferences::SerialLogFilter;

pub use message::SerialLineEnding;
pub(crate) use message::prepare_message;
pub use tools::{SerialAssistantCommand, SerialToolCommand, SerialToolPage, SerialToolsPanel};

pub use file_picker::open_serial_download_dialog;
pub use ports::available_ports;
pub use presence::{SerialPortSource, SerialPresence, SerialPresenceState, SystemSerialPortSource};
pub(crate) use transport::open as open_shared_port;
pub use transport::{SerialTransportProvider, check};
