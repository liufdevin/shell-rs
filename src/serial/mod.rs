//! 串口: the serial ports of this computer, for serial hosts. Which ports
//! there are, for the host form, and the terminal transport that opens one.

mod file_picker;
pub mod files;
mod ports;
mod transport;

pub use file_picker::open_serial_download_dialog;
pub use ports::available_ports;
pub(crate) use transport::open as open_shared_port;
pub use transport::{SerialTransportProvider, check};
