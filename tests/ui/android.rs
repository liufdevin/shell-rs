use crate::support::*;
use anyhow::{Result, bail};
use shellrs::adb::{
    AdbConnection, AdbDetection, AdbDetector, AndroidApp, AndroidCapabilities, AndroidTools,
    MirrorProcess,
};
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

struct Devices(Vec<(&'static str, &'static str)>);

struct DisconnectingDevices(Arc<AtomicBool>);

impl AdbDetector for DisconnectingDevices {
    fn detect(&self, _: Option<&str>) -> AdbDetection {
        if self.0.load(Ordering::Acquire) {
            AdbDetection::Unavailable("injected detection failure".into())
        } else {
            Devices(vec![("android", "Helper A133")]).detect(None)
        }
    }
}

impl AdbDetector for Devices {
    fn detect(&self, _: Option<&str>) -> AdbDetection {
        AdbDetection::Choose(
            self.0
                .iter()
                .map(|(serial, name)| {
                    AdbConnection::new("unused-adb".into(), (*serial).into())
                        .unwrap()
                        .with_name(name)
                })
                .collect(),
        )
    }
}

#[derive(Default)]
struct Tools {
    calls: Mutex<Vec<String>>,
    fail: AtomicBool,
    hold: AtomicBool,
    missing_scrcpy: bool,
    stops: Arc<AtomicUsize>,
}

impl Tools {
    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }

    fn count(&self, prefix: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.starts_with(prefix))
            .count()
    }

    fn result(&self, cancel: &AtomicBool) -> Result<()> {
        while self.hold.load(Ordering::SeqCst) && !cancel.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(5));
        }
        if self.fail.load(Ordering::SeqCst) {
            bail!("injected Android failure");
        }
        Ok(())
    }
}

struct Mirror(Arc<AtomicUsize>);

impl MirrorProcess for Mirror {
    fn poll(&mut self) -> Result<bool> {
        Ok(true)
    }

    fn stop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl AndroidTools for Tools {
    fn probe(&self, device: &AdbConnection, _: &AtomicBool) -> Result<AndroidCapabilities> {
        Ok(match device.serial() {
            "ubuntu" => AndroidCapabilities::default(),
            "old" => AndroidCapabilities::new(19, true, true, true),
            _ => AndroidCapabilities::new(35, true, true, true),
        })
    }

    fn apps(&self, device: &AdbConnection, cancel: &AtomicBool) -> Result<Vec<AndroidApp>> {
        self.record(format!("apps:{}", device.serial()));
        self.result(cancel)?;
        Ok(vec![AndroidApp::new("com.example.app")?])
    }

    fn install(&self, device: &AdbConnection, apk: &Path, cancel: &AtomicBool) -> Result<()> {
        self.record(format!("install:{}:{}", device.serial(), apk.display()));
        self.result(cancel)
    }

    fn launch(&self, device: &AdbConnection, package: &str, cancel: &AtomicBool) -> Result<()> {
        self.record(format!("launch:{}:{package}", device.serial()));
        self.result(cancel)
    }

    fn mirror(
        &self,
        device: &AdbConnection,
        _: &Path,
        cancel: &AtomicBool,
    ) -> Result<Box<dyn MirrorProcess>> {
        self.record(format!("mirror:{}", device.serial()));
        self.result(cancel)?;
        Ok(Box::new(Mirror(self.stops.clone())))
    }

    fn scrcpy(&self) -> Option<PathBuf> {
        (!self.missing_scrcpy).then(|| PathBuf::from("injected-scrcpy"))
    }
}

fn open(
    cx: &mut TestAppContext,
    devices: Vec<(&'static str, &'static str)>,
    tools: Arc<Tools>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    let (handle, workspace) = open_workspace_with_store(cx, HostStore::empty());
    cx.update(|cx| {
        workspace.update(cx, |workspace, _| {
            workspace.set_adb_detector(Arc::new(Devices(devices)));
            workspace.set_android_tools(tools);
        });
    });
    in_frame(cx, handle, |window, cx| {
        window.click("open-adb-devices", cx)
    });
    (handle, workspace)
}

async fn detected(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        window.try_find("android-device-manager").is_some()
            && window.try_find("android-scanning").is_none()
    })
    .await;
}

async fn message(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    id: &'static str,
    text: &str,
) {
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        window
            .try_find(id)
            .is_some_and(|element| element.label().is_some_and(|label| label.contains(text)))
    })
    .await;
}

fn select(cx: &mut TestAppContext, handle: WindowHandle<Root>, keys: &[&str]) {
    in_frame(cx, handle, |window, cx| {
        window.within("android-device-select").click("input", cx);
    });
    for key in keys {
        in_frame(cx, handle, |window, cx| window.press(key, cx));
    }
}

#[gpui_kit::test]
async fn android_actions_follow_the_named_device_and_hide_unsupported_capabilities(
    cx: &mut TestAppContext,
) {
    let tools = Arc::new(Tools::default());
    let (handle, _workspace) = open(
        cx,
        vec![
            ("android", "Helper A133"),
            ("ubuntu", "Ubuntu Board"),
            ("old", "Old Android"),
        ],
        tools.clone(),
    );
    detected(cx, handle).await;
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("android-install").is_none());
        assert!(window.try_find("android-mirror").is_none());
    });
    select(cx, handle, &["down", "enter"]);
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("android-device-select").value(),
            Some("Helper A133（android）")
        );
        assert!(window.find("android-mirror").visible());
        assert!(window.find("android-install").visible());
        assert!(window.find("android-launch").visible());
    });
    select(cx, handle, &["down", "enter"]);
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("android-device-select").value(),
            Some("Ubuntu Board（ubuntu）")
        );
        assert!(window.find("android-unsupported").visible());
        for id in ["android-mirror", "android-install", "android-launch"] {
            assert!(window.try_find(id).is_none());
        }
    });
    select(cx, handle, &["down", "enter"]);
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("android-mirror").is_none());
        assert!(window.find("android-install").visible());
        assert!(window.find("android-launch").visible());
        window.click("android-apps-refresh", cx);
    });
    message(cx, handle, "android-result", "应用列表已更新").await;
    assert_eq!(tools.count("apps:old"), 1);
    assert_eq!(tools.count("apps:android"), 0);
    assert_eq!(tools.count("install:"), 0);
}

#[gpui_kit::test]
async fn apk_picker_and_app_selection_run_on_the_selected_android_device(cx: &mut TestAppContext) {
    let tools = Arc::new(Tools::default());
    let (handle, _workspace) = open(cx, vec![("android", "Helper A133")], tools.clone());
    detected(cx, handle).await;
    let dir = tempfile::tempdir().unwrap();
    let apk = dir.path().join("example app.apk");
    std::fs::write(&apk, "test APK").unwrap();
    in_frame(cx, handle, |window, cx| window.click("android-install", cx));
    cx.simulate_path_prompt_response(|options| {
        assert!(options.files && !options.directories && !options.multiple);
        Some(vec![apk.clone()])
    });
    message(cx, handle, "android-result", "APK 安装成功").await;
    assert!(
        tools
            .calls
            .lock()
            .unwrap()
            .contains(&format!("install:android:{}", apk.display()))
    );
    in_frame(cx, handle, |window, cx| {
        window.click("android-apps-refresh", cx);
    });
    message(cx, handle, "android-result", "应用列表已更新").await;
    in_frame(cx, handle, |window, cx| {
        window.within("android-app-select").click("input", cx);
    });
    for key in ["down", "enter"] {
        in_frame(cx, handle, |window, cx| window.press(key, cx));
    }
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("android-package").value(),
            Some("com.example.app")
        );
        window.click("android-launch", cx);
    });
    message(cx, handle, "android-result", "应用已启动").await;
    assert_eq!(tools.count("launch:android:com.example.app"), 1);
}

#[gpui_kit::test]
async fn mirror_survives_dialog_close_and_stops_on_request(cx: &mut TestAppContext) {
    let tools = Arc::new(Tools::default());
    let (handle, _workspace) = open(cx, vec![("android", "Helper A133")], tools.clone());
    detected(cx, handle).await;
    in_frame(cx, handle, |window, cx| window.click("android-mirror", cx));
    message(cx, handle, "android-mirror-status", "投屏进程运行中").await;
    in_frame(cx, handle, |window, cx| {
        assert!(!enabled(&window.find("android-mirror")));
        window.click("android-mirror", cx);
        window.press("escape", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
    assert_eq!(tools.count("mirror:android"), 1);
    assert_eq!(tools.stops.load(Ordering::SeqCst), 0);
    in_frame(cx, handle, |window, cx| {
        window.click("open-adb-devices", cx)
    });
    detected(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("android-mirror-status").visible());
        window.click("android-stop-mirror", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        tools.stops.load(Ordering::SeqCst) == 1
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("android-mirror-status").is_none());
    });
}

#[gpui_kit::test]
async fn missing_scrcpy_can_be_selected_without_blocking_apk_actions(cx: &mut TestAppContext) {
    let tools = Arc::new(Tools {
        missing_scrcpy: true,
        ..Default::default()
    });
    let (handle, _workspace) = open(cx, vec![("android", "Helper A133")], tools.clone());
    detected(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        assert!(!enabled(&window.find("android-mirror")));
        assert!(window.find("android-scrcpy-missing").visible());
        assert!(enabled(&window.find("android-install")));
        window.click("android-scrcpy-choose", cx);
    });
    cx.simulate_path_prompt_response(|_| Some(vec![PathBuf::from("chosen-scrcpy")]));
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        window.try_find("android-scrcpy-missing").is_none()
    })
    .await;
    in_frame(cx, handle, |window, cx| window.click("android-mirror", cx));
    message(cx, handle, "android-mirror-status", "投屏进程运行中").await;
    in_frame(cx, handle, |window, cx| {
        window.click("android-stop-mirror", cx)
    });
}

#[gpui_kit::test]
async fn failed_and_cancelled_android_operations_do_not_report_success(cx: &mut TestAppContext) {
    let tools = Arc::new(Tools {
        fail: AtomicBool::new(true),
        ..Default::default()
    });
    let (handle, _workspace) = open(cx, vec![("android", "Helper A133")], tools.clone());
    detected(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click("android-package", cx);
        window.input("com.example.app", cx);
        window.press("enter", cx);
    });
    message(cx, handle, "android-error", "injected Android failure").await;
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("android-result").is_none());
    });
    tools.fail.store(false, Ordering::SeqCst);
    tools.hold.store(true, Ordering::SeqCst);
    in_frame(cx, handle, |window, cx| window.click("android-launch", cx));
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        tools.count("launch:android:") == 2
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert!(!enabled(&window.find("android-launch")));
        window.click("android-cancel", cx);
    });
    message(cx, handle, "android-result", "已停止等待").await;
    tools.hold.store(false, Ordering::SeqCst);
    in_frame(cx, handle, |window, cx| {
        window.click("android-launch", cx);
    });
    message(cx, handle, "android-result", "应用已启动").await;
    assert_eq!(tools.count("launch:android:"), 3);
}

#[gpui_kit::test]
async fn detection_failure_clears_old_devices_and_refresh_can_recover(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace_with_store(cx, HostStore::empty());
    let failed = Arc::new(AtomicBool::new(false));
    cx.update(|cx| {
        workspace.update(cx, |workspace, _| {
            workspace.set_adb_detector(Arc::new(DisconnectingDevices(failed.clone())));
            workspace.set_android_tools(Arc::new(Tools::default()));
        });
    });
    in_frame(cx, handle, |window, cx| {
        window.click("open-adb-devices", cx)
    });
    detected(cx, handle).await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("android-device-select").value(),
            Some("Helper A133（android）")
        );
    });
    failed.store(true, Ordering::Release);
    in_frame(cx, handle, |window, cx| window.click("android-refresh", cx));
    message(cx, handle, "android-error", "injected detection failure").await;
    in_frame(cx, handle, |window, _| {
        assert_ne!(
            window.find("android-device-select").value(),
            Some("Helper A133（android）")
        );
        for id in ["android-mirror", "android-install", "android-launch"] {
            assert!(window.try_find(id).is_none());
        }
    });
    failed.store(false, Ordering::Release);
    in_frame(cx, handle, |window, cx| window.click("android-refresh", cx));
    detected(cx, handle).await;
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("android-error").is_none());
        assert_eq!(
            window.find("android-device-select").value(),
            Some("Helper A133（android）")
        );
        assert!(window.find("android-install").visible());
    });
}
