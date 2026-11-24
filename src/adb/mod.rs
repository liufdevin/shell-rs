mod android;
mod client;
mod device_manager;
mod identity;
mod lan_proxy;
mod mirror;
use crate::shared::process;
#[cfg(test)]
mod test_support;
mod transport;

pub use android::{
    AndroidApp, AndroidCapabilities, AndroidTools, MirrorProcess, SystemAndroidTools,
};
pub(crate) use client::find_adb;
pub use client::{
    AdbConnection, AdbDetection, AdbDetector, NoAdbDetector, SystemAdbDetector, canonical_boot_id,
};
pub use device_manager::{DeviceCommand, DeviceManager};
pub use identity::BoardIdentity;
pub(crate) use identity::{
    NONCE_CLEAN_COMMAND, generate_nonce, is_network_adb_serial, nonce_write_command,
    pick_preferred_match,
};
pub(crate) use lan_proxy::AdbProxy;

pub(crate) fn run_usb_adb(
    program: std::path::PathBuf,
    args: &[&str],
    cancel: &mut dyn FnMut() -> bool,
) -> anyhow::Result<Vec<u8>> {
    client::run_local_adb(program, args, cancel)
}
impl crate::sftp::FileTransportSource for AdbConnection {
    fn endpoint(&self) -> String {
        self.server_socket().map_or_else(
            || format!("adb:{}", self.serial()),
            |server| format!("lan-adb:{server}:{}", self.serial()),
        )
    }
    fn create(&self) -> Box<dyn crate::sftp::SftpTransport> {
        Box::new(transport::AdbTransport(self.clone()))
    }
}
