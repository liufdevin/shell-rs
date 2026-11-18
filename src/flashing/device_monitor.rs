use std::{
    collections::BTreeSet,
    ffi::c_void,
    sync::mpsc::{self, Receiver, Sender},
    time::Instant,
};

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GETIDLIST_FILTER_ENUMERATOR, CM_GETIDLIST_FILTER_PRESENT, CM_Get_Device_ID_List_SizeW,
    CM_Get_Device_ID_ListW, CM_NOTIFY_ACTION, CM_NOTIFY_ACTION_DEVICEINSTANCEENUMERATED,
    CM_NOTIFY_ACTION_DEVICEINSTANCEREMOVED, CM_NOTIFY_ACTION_DEVICEINSTANCESTARTED,
    CM_NOTIFY_EVENT_DATA, CM_NOTIFY_FILTER, CM_NOTIFY_FILTER_FLAG_ALL_DEVICE_INSTANCES,
    CM_NOTIFY_FILTER_TYPE_DEVICEINSTANCE, CM_Register_Notification, CM_Unregister_Notification,
    CR_BUFFER_SMALL, CR_SUCCESS, HCMNOTIFICATION,
};

use super::process::RunEvent;

pub(super) struct DeviceWatch {
    commands: Sender<WatchCommand>,
    events: Receiver<RunEvent>,
}

enum WatchCommand {
    Device(RunEvent),
    Stop,
}

impl DeviceWatch {
    pub(super) fn start() -> Self {
        let (sender, events) = mpsc::channel();
        let (commands, pending) = mpsc::channel();
        let notifications = commands.clone();
        std::thread::spawn(move || {
            let _monitor = match DeviceMonitor::register(notifications).and_then(|monitor| {
                let devices = present_devices()?;
                let _ = sender.send(RunEvent::DevicesPresent {
                    at: Instant::now(),
                    devices,
                });
                Ok(monitor)
            }) {
                Ok(monitor) => monitor,
                Err(error) => {
                    let _ = sender.send(RunEvent::DetectionUnavailable {
                        at: Instant::now(),
                        error,
                    });
                    return;
                }
            };
            while let Ok(WatchCommand::Device(event)) = pending.recv() {
                if sender.send(event).is_err() {
                    break;
                }
            }
        });
        Self { commands, events }
    }

    pub(super) fn events(&self) -> &Receiver<RunEvent> {
        &self.events
    }
}

impl Drop for DeviceWatch {
    fn drop(&mut self) {
        let _ = self.commands.send(WatchCommand::Stop);
    }
}

struct DeviceMonitor {
    handle: HCMNOTIFICATION,
    sender: Option<Box<Sender<WatchCommand>>>,
}

impl DeviceMonitor {
    fn register(sender: Sender<WatchCommand>) -> Result<Self, String> {
        let sender = Box::new(sender);
        let filter = CM_NOTIFY_FILTER {
            cbSize: std::mem::size_of::<CM_NOTIFY_FILTER>() as u32,
            Flags: CM_NOTIFY_FILTER_FLAG_ALL_DEVICE_INSTANCES,
            FilterType: CM_NOTIFY_FILTER_TYPE_DEVICEINSTANCE,
            ..Default::default()
        };
        let mut handle = std::ptr::null_mut();
        let status = unsafe {
            CM_Register_Notification(
                &filter,
                (&*sender as *const Sender<WatchCommand>).cast(),
                Some(device_changed),
                &mut handle,
            )
        };
        if status != CR_SUCCESS {
            return Err(format!(
                "Windows device notification registration failed ({status})"
            ));
        }
        Ok(Self {
            handle,
            sender: Some(sender),
        })
    }
}

impl Drop for DeviceMonitor {
    fn drop(&mut self) {
        if unsafe { CM_Unregister_Notification(self.handle) } != CR_SUCCESS
            && let Some(sender) = self.sender.take()
        {
            Box::leak(sender);
        }
    }
}

fn fel_instance(instance: &str) -> Option<String> {
    let instance = instance.to_ascii_uppercase();
    let mut parts = instance.split('\\');
    if parts.next()? != "USB" {
        return None;
    }
    let hardware = parts.next()?;
    if !hardware.split('&').any(|part| part == "VID_1F3A")
        || !hardware.split('&').any(|part| part == "PID_EFE8")
        || parts.next().is_none_or(str::is_empty)
    {
        return None;
    }
    Some(instance)
}

fn present_devices() -> Result<BTreeSet<String>, String> {
    let enumerator: Vec<u16> = "USB\0".encode_utf16().collect();
    let flags = CM_GETIDLIST_FILTER_ENUMERATOR | CM_GETIDLIST_FILTER_PRESENT;
    for _ in 0..3 {
        let mut length = 0;
        let status =
            unsafe { CM_Get_Device_ID_List_SizeW(&mut length, enumerator.as_ptr(), flags) };
        if status != CR_SUCCESS || length > 1024 * 1024 {
            return Err(format!("Windows device enumeration failed ({status})"));
        }
        let mut buffer = vec![0; length.max(2) as usize];
        let status = unsafe {
            CM_Get_Device_ID_ListW(
                enumerator.as_ptr(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                flags,
            )
        };
        if status == CR_BUFFER_SMALL {
            continue;
        }
        if status != CR_SUCCESS {
            return Err(format!("Windows device enumeration failed ({status})"));
        }
        return Ok(buffer
            .split(|value| *value == 0)
            .filter_map(|instance| fel_instance(&String::from_utf16_lossy(instance)))
            .collect());
    }
    Err("Windows device list changed during enumeration".into())
}

unsafe extern "system" fn device_changed(
    _: HCMNOTIFICATION,
    context: *const c_void,
    action: CM_NOTIFY_ACTION,
    data: *const CM_NOTIFY_EVENT_DATA,
    size: u32,
) -> u32 {
    let offset = std::mem::offset_of!(CM_NOTIFY_EVENT_DATA, u);
    if context.is_null()
        || data.is_null()
        || (size as usize) < offset + 2
        || unsafe { (*data).FilterType } != CM_NOTIFY_FILTER_TYPE_DEVICEINSTANCE
        || !matches!(
            action,
            CM_NOTIFY_ACTION_DEVICEINSTANCEENUMERATED
                | CM_NOTIFY_ACTION_DEVICEINSTANCESTARTED
                | CM_NOTIFY_ACTION_DEVICEINSTANCEREMOVED
        )
    {
        return 0;
    }
    let instance = unsafe {
        std::slice::from_raw_parts(
            data.cast::<u8>().add(offset).cast::<u16>(),
            ((size as usize - offset) / 2).min(200),
        )
    };
    if let Some(end) = instance.iter().position(|value| *value == 0)
        && let Some(instance) = fel_instance(&String::from_utf16_lossy(&instance[..end]))
    {
        let sender = unsafe { &*context.cast::<Sender<WatchCommand>>() };
        let _ = sender.send(WatchCommand::Device(RunEvent::DeviceChanged {
            at: Instant::now(),
            instance,
            present: action != CM_NOTIFY_ACTION_DEVICEINSTANCEREMOVED,
        }));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn only_the_fel_usb_id_from_the_allwinner_driver_is_detected() {
        assert_eq!(
            fel_instance(r"USB\VID_1f3a&PID_efe8\board"),
            Some(r"USB\VID_1F3A&PID_EFE8\BOARD".into())
        );
        for instance in [
            r"USB\VID_1F3A&PID_1007\adb",
            r"USB\VID_1F3A&PID_EFE80\other",
            r"USB\VID_18D1&PID_EFE8\other",
            r"HID\VID_1F3A&PID_EFE8\other",
            "",
        ] {
            assert!(fel_instance(instance).is_none());
        }
    }

    #[test]
    fn notification_callback_emits_fel_arrival_without_waiting_for_tool_progress() {
        let (sender, receiver) = mpsc::channel();
        let instance: Vec<u16> = "USB\\VID_1F3A&PID_EFE8\\BOARD\0".encode_utf16().collect();
        let offset = std::mem::offset_of!(CM_NOTIFY_EVENT_DATA, u);
        let size = offset + instance.len() * 2;
        let mut storage = vec![0u64; size.div_ceil(8)];
        let data = storage.as_mut_ptr().cast::<CM_NOTIFY_EVENT_DATA>();
        unsafe {
            (*data).FilterType = CM_NOTIFY_FILTER_TYPE_DEVICEINSTANCE;
            std::ptr::copy_nonoverlapping(
                instance.as_ptr(),
                data.cast::<u8>().add(offset).cast::<u16>(),
                instance.len(),
            );
            for action in [
                CM_NOTIFY_ACTION_DEVICEINSTANCEENUMERATED,
                CM_NOTIFY_ACTION_DEVICEINSTANCEREMOVED,
            ] {
                device_changed(
                    std::ptr::null_mut(),
                    (&sender as *const Sender<WatchCommand>).cast(),
                    action,
                    data,
                    size as u32,
                );
            }
            device_changed(
                std::ptr::null_mut(),
                (&sender as *const Sender<WatchCommand>).cast(),
                CM_NOTIFY_ACTION_DEVICEINSTANCESTARTED,
                data,
                offset as u32,
            );
        }
        assert!(matches!(
            receiver.try_recv(),
            Ok(WatchCommand::Device(RunEvent::DeviceChanged {
                present: true,
                ..
            }))
        ));
        assert!(matches!(
            receiver.try_recv(),
            Ok(WatchCommand::Device(RunEvent::DeviceChanged {
                present: false,
                ..
            }))
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn windows_notification_subscription_provides_a_read_only_initial_snapshot() {
        let watch = DeviceWatch::start();
        assert!(matches!(
            watch.events().recv_timeout(Duration::from_secs(3)),
            Ok(RunEvent::DevicesPresent { .. })
        ));
    }
}
