use crate::support::*;
use shellrs::{
    app::SerialMaintenanceAction,
    host::SerialSettings,
    serial::maintenance::{
        MaintenanceFailure, RemovalState, SerialDevice, SerialMaintenanceBackend,
        SerialMaintenanceCommand, SerialRemoval,
    },
};
use std::sync::atomic::AtomicBool;

const FIRST: &str = "USB\\VID_1234&PID_5678\\ONE";
const SECOND: &str = "USB\\VID_1234&PID_5678\\TWO";

struct FakeMaintenance {
    devices: Vec<SerialDevice>,
    scans: AtomicUsize,
    selections: Mutex<Vec<Vec<String>>>,
    cancel: AtomicBool,
    fail_scan: AtomicBool,
    hold: AtomicBool,
    supported: bool,
    omit_results: bool,
}

impl Default for FakeMaintenance {
    fn default() -> Self {
        Self {
            devices: vec![
                SerialDevice::new(
                    FIRST.into(),
                    "COM18".into(),
                    "USB Serial One".into(),
                    Some(false),
                ),
                SerialDevice::new(
                    SECOND.into(),
                    "COM26".into(),
                    "USB Serial Two".into(),
                    Some(false),
                ),
                SerialDevice::new(
                    "USB\\ONLINE".into(),
                    "COM3".into(),
                    "Online".into(),
                    Some(true),
                ),
                SerialDevice::new("USB\\UNKNOWN".into(), "COM4".into(), "Unknown".into(), None),
                SerialDevice::new(
                    "USB\\PRINTER".into(),
                    "LPT1".into(),
                    "Printer".into(),
                    Some(false),
                ),
                SerialDevice::new(
                    "ROOT\\OTHER".into(),
                    "COM8".into(),
                    "Other".into(),
                    Some(false),
                ),
            ],
            scans: AtomicUsize::new(0),
            selections: Mutex::default(),
            cancel: AtomicBool::new(false),
            fail_scan: AtomicBool::new(false),
            hold: AtomicBool::new(false),
            supported: true,
            omit_results: false,
        }
    }
}

impl SerialMaintenanceBackend for FakeMaintenance {
    fn is_supported(&self) -> bool {
        self.supported
    }
    fn scan(&self) -> Result<Vec<SerialDevice>, MaintenanceFailure> {
        self.scans.fetch_add(1, Ordering::SeqCst);
        if self.fail_scan.load(Ordering::SeqCst) {
            Err(MaintenanceFailure::Failed("fixture scan error".into()))
        } else {
            Ok(self.devices.clone())
        }
    }
    fn remove(&self, devices: &[SerialDevice]) -> Result<Vec<SerialRemoval>, MaintenanceFailure> {
        assert!(devices.iter().all(SerialDevice::is_candidate));
        self.selections.lock().unwrap().push(
            devices
                .iter()
                .map(|device| device.instance_id().to_owned())
                .collect(),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while self.hold.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        if self.cancel.load(Ordering::SeqCst) {
            return Err(MaintenanceFailure::Cancelled);
        }
        if self.omit_results {
            return Ok(Vec::new());
        }
        Ok(devices
            .iter()
            .map(|device| {
                SerialRemoval::new(
                    device.instance_id().into(),
                    if device.instance_id() == SECOND {
                        RemovalState::Present
                    } else {
                        RemovalState::Removed
                    },
                    String::new(),
                )
            })
            .collect())
    }
}

fn open(
    cx: &mut TestAppContext,
    backend: Arc<FakeMaintenance>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    let mut store = HostStore::empty();
    store.insert_unnotified(HostDraft::serial(
        "Saved board",
        "COM18",
        SerialSettings::default(),
        None,
    ));
    let (handle, workspace) = open_workspace_with_store(cx, store);
    cx.update(|cx| {
        workspace
            .read(cx)
            .serial_maintenance()
            .clone()
            .update(cx, |maintenance, cx| maintenance.set_backend(backend, cx))
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(SerialMaintenanceAction(SerialMaintenanceCommand::Open)),
            cx,
        )
    });
    (handle, workspace)
}

async fn scan(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    in_frame(cx, handle, |window, cx| {
        window.click("serial-maintenance-scan", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window
            .find("serial-maintenance-status")
            .label()
            .is_some_and(|label| label.contains("未连接的 USB 串口："))
    })
    .await;
}

#[gpui_kit::test]
async fn serial_maintenance_requires_scan_selection_and_confirmation_and_reports_each_device(
    cx: &mut TestAppContext,
) {
    let backend = Arc::new(FakeMaintenance::default());
    let (handle, workspace) = open(cx, backend.clone());
    in_frame(cx, handle, |window, _| {
        assert_eq!(backend.scans.load(Ordering::SeqCst), 0);
        assert!(!enabled(&window.find("serial-maintenance-review")));
    });
    scan(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("serial-maintenance-status").label(),
            Some("未连接的 USB 串口：2 个 · 已选择 0 个")
        );
        for id in [FIRST, SECOND] {
            assert_eq!(
                window
                    .find(format!("serial-maintenance-select:{id}"))
                    .checked(),
                Some(false)
            );
            window.click(format!("serial-maintenance-select:{id}"), cx);
        }
        assert!(
            window
                .try_find("serial-maintenance-select:USB\\ONLINE")
                .is_none()
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-maintenance-review", cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("serial-maintenance-warning").visible());
        assert!(backend.selections.lock().unwrap().is_empty());
        window.click("serial-maintenance-back", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(backend.selections.lock().unwrap().is_empty());
        window.click("serial-maintenance-review", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-maintenance-remove", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window
            .try_find(format!("serial-maintenance-result:{FIRST}"))
            .is_some()
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window
                .find(format!("serial-maintenance-result:{FIRST}"))
                .label(),
            Some("已移除设备记录")
        );
        assert_eq!(
            window
                .find(format!("serial-maintenance-result:{SECOND}"))
                .label(),
            Some("设备已连接，已跳过，未移除")
        );
        assert!(!enabled(&window.find("serial-maintenance-review")));
    });
    assert_eq!(
        *backend.selections.lock().unwrap(),
        [vec![FIRST.to_owned(), SECOND.to_owned()]]
    );
    cx.update(|cx| {
        assert_eq!(
            workspace.read(cx).store().read(cx).hosts()[0]
                .address
                .as_ref(),
            "COM18"
        )
    });
}

#[gpui_kit::test]
async fn serial_maintenance_missing_results_are_not_reported_as_removed(cx: &mut TestAppContext) {
    let backend = Arc::new(FakeMaintenance {
        omit_results: true,
        ..Default::default()
    });
    let (handle, _) = open(cx, backend);
    scan(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click(format!("serial-maintenance-select:{FIRST}"), cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-maintenance-review", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-maintenance-remove", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window
            .try_find(format!("serial-maintenance-result:{FIRST}"))
            .is_some()
    })
    .await;
    in_frame(cx, handle, |window, _| {
        let result = window.find(format!("serial-maintenance-result:{FIRST}"));
        let label = result.label().unwrap();
        assert!(label.contains("未收到可核验的移除结果"));
        assert!(!label.contains("已移除"));
        assert!(!enabled(&window.find("serial-maintenance-review")));
    });
}

#[gpui_kit::test]
async fn serial_maintenance_cancelled_elevation_preserves_devices_and_requires_rescan(
    cx: &mut TestAppContext,
) {
    let backend = Arc::new(FakeMaintenance::default());
    backend.cancel.store(true, Ordering::SeqCst);
    let (handle, _) = open(cx, backend.clone());
    scan(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click(format!("serial-maintenance-select:{FIRST}"), cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-maintenance-review", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-maintenance-remove", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window.try_find("serial-maintenance-error").is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert!(
            window
                .find("serial-maintenance-error")
                .label()
                .unwrap()
                .contains("已取消管理员授权")
        );
        assert!(!enabled(&window.find("serial-maintenance-review")));
        window.press("escape", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("serial-maintenance").is_none())
    });
    assert_eq!(backend.selections.lock().unwrap().len(), 1);
}

#[gpui_kit::test]
async fn serial_maintenance_close_and_reopen_during_removal_keeps_one_operation(
    cx: &mut TestAppContext,
) {
    let backend = Arc::new(FakeMaintenance::default());
    backend.hold.store(true, Ordering::SeqCst);
    let (handle, _) = open(cx, backend.clone());
    scan(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click(format!("serial-maintenance-select:{FIRST}"), cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-maintenance-review", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-maintenance-remove", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, _| {
        backend.selections.lock().unwrap().len() == 1
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert!(!enabled(&window.find("serial-maintenance-scan")));
        window.within("dialog-close").click("close", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("host-search", cx);
        window.dispatch_action(
            Box::new(SerialMaintenanceAction(SerialMaintenanceCommand::Open)),
            cx,
        );
    });
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .find("serial-maintenance-status")
                .label()
                .unwrap()
                .contains("正在请求管理员授权")
        )
    });
    backend.hold.store(false, Ordering::SeqCst);
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window
            .try_find(format!("serial-maintenance-result:{FIRST}"))
            .is_some()
    })
    .await;
    assert_eq!(backend.selections.lock().unwrap().len(), 1);
}

#[gpui_kit::test]
async fn serial_maintenance_scan_failures_and_unsupported_platforms_never_enable_removal(
    cx: &mut TestAppContext,
) {
    let backend = Arc::new(FakeMaintenance::default());
    backend.fail_scan.store(true, Ordering::SeqCst);
    let (handle, _) = open(cx, backend.clone());
    in_frame(cx, handle, |window, cx| {
        window.click("serial-maintenance-scan", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window.try_find("serial-maintenance-error").is_some()
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert!(!enabled(&window.find("serial-maintenance-review")))
    });
    assert!(backend.selections.lock().unwrap().is_empty());
    let unsupported = Arc::new(FakeMaintenance {
        supported: false,
        ..Default::default()
    });
    let (handle, _) = open(cx, unsupported.clone());
    in_frame(cx, handle, |window, _| {
        assert!(window.find("serial-maintenance-unsupported").visible());
        assert!(!enabled(&window.find("serial-maintenance-scan")));
    });
    assert_eq!(unsupported.scans.load(Ordering::SeqCst), 0);
}

#[cfg(windows)]
#[gpui_kit::test]
fn serial_maintenance_entry_is_in_application_settings_and_opens_without_scanning(
    cx: &mut TestAppContext,
) {
    let (handle, _) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-7", cx)
    });
    for _ in 0..8 {
        let visible = in_frame(cx, handle, |window, cx| {
            if window
                .try_find("open-serial-maintenance")
                .is_some_and(|button| button.visible())
            {
                return true;
            }
            let bounds = window.find("settings").bounds();
            window.dispatch_event(
                gpui_kit::ScrollWheelEvent {
                    position: point(bounds.right() - px(50.), bounds.center().y),
                    delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-600.))),
                    modifiers: Default::default(),
                    touch_phase: gpui_kit::TouchPhase::Moved,
                }
                .to_platform_input(),
                cx,
            );
            false
        });
        if visible {
            break;
        }
    }
    in_frame(cx, handle, |window, cx| {
        window.click("open-serial-maintenance", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .find("serial-maintenance-status")
                .label()
                .unwrap()
                .contains("点击扫描")
        )
    });
}
