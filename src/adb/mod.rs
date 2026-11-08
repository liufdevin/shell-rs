mod android;
mod client;
mod device_manager;
mod lan_proxy;
mod mirror;
mod process;
#[cfg(test)]
mod test_support;
mod transport;

pub use android::{
    AndroidApp, AndroidCapabilities, AndroidTools, MirrorProcess, SystemAndroidTools,
};
pub use client::{
    AdbConnection, AdbDetection, AdbDetector, NoAdbDetector, SystemAdbDetector, canonical_boot_id,
};
pub use device_manager::{DeviceCommand, DeviceManager};
pub(crate) use lan_proxy::AdbProxy;
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
