mod android;
mod client;
mod device_manager;
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
impl crate::sftp::FileTransportSource for AdbConnection {
    fn endpoint(&self) -> String {
        format!("adb:{}", self.serial())
    }
    fn create(&self) -> Box<dyn crate::sftp::SftpTransport> {
        Box::new(transport::AdbTransport(self.clone()))
    }
}
