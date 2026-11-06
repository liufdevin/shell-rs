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
    pub(super) fn on_open_adb_devices(
        &mut self,
        _: &OpenAdbDevices,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let manager = self
            .android_manager
            .get_or_insert_with(|| {
                cx.new(|cx| {
                    DeviceManager::new(
                        self.adb_detector.clone(),
                        self.android_tools.clone(),
                        self.focus_handle.clone(),
                        window,
                        cx,
                    )
                })
            })
            .clone();
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
