use super::Workspace;
use crate::{
    adb::{DeviceCommand, DeviceManager},
    app::{AdbDeviceAction, OpenAdbDevices},
    i18n::t,
};
use gpui_kit::{
    component::{
        WindowExt as _,
        dialog::{DialogClose, DialogFooter},
    },
    *,
};

impl Workspace {
    pub(super) fn open_remote_android_manager(
        &mut self,
        connection: crate::adb::AdbConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        struct RemoteDetector(crate::adb::AdbConnection);
        impl crate::adb::AdbDetector for RemoteDetector {
            fn detect(&self, _: Option<&str>) -> crate::adb::AdbDetection {
                crate::adb::AdbDetection::Matched(self.0.clone())
            }
        }
        self.android_manager = Some(cx.new(|cx| {
            DeviceManager::new(
                std::sync::Arc::new(RemoteDetector(connection)),
                self.android_tools.clone(),
                self.focus_handle.clone(),
                window,
                cx,
            )
        }));
        self.android_manager_remote = true;
        self.show_android_manager(window, cx);
    }
    pub(super) fn on_open_adb_devices(
        &mut self,
        _: &OpenAdbDevices,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        if self.android_manager_remote {
            self.android_manager = None;
            self.android_manager_remote = false;
        }
        self.android_manager.get_or_insert_with(|| {
            cx.new(|cx| {
                DeviceManager::new(
                    self.adb_detector.clone(),
                    self.android_tools.clone(),
                    self.focus_handle.clone(),
                    window,
                    cx,
                )
            })
        });
        self.show_android_manager(window, cx);
    }

    fn show_android_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(manager) = self.android_manager.clone() else {
            return;
        };
        manager.update(cx, |manager, cx| {
            manager.command(DeviceCommand::Refresh, window, cx)
        });
        window.open_dialog(cx, move |dialog, window, _| {
            let manager = manager.clone();
            dialog
                .title(t!("android.title"))
                .w(px(620.))
                .max_h(window.viewport_size().height * 0.9)
                .on_ok(|_, _, _| false)
                .child(manager.clone())
                .footer(
                    DialogFooter::new().child(
                        DialogClose::new().trigger(|button| button.label(t!("android.close"))),
                    ),
                )
                .on_close(move |_, _, cx| manager.update(cx, |manager, _| manager.closed()))
        });
    }
    pub(super) fn on_adb_device_action(
        &mut self,
        action: &AdbDeviceAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(manager) = &self.android_manager {
            manager.update(cx, |manager, cx| {
                manager.command(action.0.clone(), window, cx)
            });
        }
    }
}
