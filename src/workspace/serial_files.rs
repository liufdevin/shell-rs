use gpui_kit::component::{WindowExt as _, notification::Notification};
use gpui_kit::*;

use super::Workspace;
use crate::adb::{AdbConnection, AdbDetection};
use crate::app::{AutoSerialFiles, DownloadSerialFile, SerialFileDownload, UploadSerialFile};
use crate::app::{ChooseAdbDevice, OpenAdbFiles};
use crate::host::ConnectionState;
use crate::i18n::t;
use crate::serial::files::{
    COMMAND_SETTLE, NETWORK_COMMAND, network_candidates, quote_path, reachable_address,
};
use crate::terminal::{RemoteTerminalId, TerminalView, exec_answer};

impl Workspace {
    fn serial_file_target(&self, id: RemoteTerminalId, cx: &App) -> Option<Entity<TerminalView>> {
        let panel = self.terminals.get(&id)?.read(cx);
        if !panel
            .host()
            .protocol
            .serial()
            .is_some_and(|settings| settings.linux_shell)
        {
            return None;
        }
        Some(panel.terminal().clone())
    }

    pub(super) fn on_auto_serial_files(
        &mut self,
        action: &AutoSerialFiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        if self.file_sidebar(id).is_some() {
            return;
        }
        let Some(view) = self.serial_file_target(id, cx) else {
            return;
        };
        let generation = view.read(cx).generation(cx);
        if !self
            .tools
            .update(cx, |tools, cx| tools.begin_serial_check(id, generation, cx))
        {
            return;
        }
        let Some(reply) = view.read(cx).exec(NETWORK_COMMAND.into(), cx) else {
            self.tools.update(cx, |tools, cx| {
                tools.finish_serial_check(id, generation, t!("serial.shell.busy"), cx)
            });
            return;
        };
        let detector = self.adb_detector.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = exec_answer(reply, cx).await;
            let boot_id = result
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .and_then(|output| output.split_once("@@boot_id\n"))
                .and_then(|(_, output)| output.lines().next())
                .and_then(crate::adb::canonical_boot_id);
            let (user, addresses) = match result {
                Some(Ok(output)) => network_candidates(&output),
                result => {
                    let error = match result {
                        Some(Err(error)) => error,
                        _ => t!("tools.not_connected").to_string(),
                    };
                    this.update(cx, |this, cx| {
                        this.tools.update(cx, |tools, cx| {
                            tools.finish_serial_check(id, generation, error.into(), cx)
                        })
                    })
                    .ok();
                    return;
                }
            };
            let (adb, address) = cx
                .background_spawn(async move {
                    let adb = detector.detect(boot_id.as_deref());
                    let address = if matches!(adb, AdbDetection::Matched(_)) {
                        None
                    } else {
                        reachable_address(&addresses)
                    };
                    (adb, address)
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                let valid = this.serial_file_target(id, cx).is_some_and(|current| {
                    current == view && current.read(cx).generation(cx) == generation
                });
                if !valid {
                    return;
                }
                if let AdbDetection::Matched(connection) = adb {
                    this.tools.update(cx, |tools, cx| {
                        tools.finish_serial_check(
                            id,
                            generation,
                            t!("adb.connected", serial = connection.serial()),
                            cx,
                        )
                    });
                    this.install_serial_adb_files(id, connection, window, cx);
                    return;
                }
                let status = match &address {
                    Some(address) => t!("serial.files.network", address = address),
                    None => t!("serial.files.fallback"),
                };
                this.tools.update(cx, |tools, cx| {
                    tools.finish_serial_check(id, generation, status, cx)
                });
                if let Some(address) = address {
                    this.install_serial_network_files(id, address.to_string(), user, window, cx);
                } else if let AdbDetection::Choose(devices) = adb
                    && !window.has_active_dialog(cx)
                    && this
                        .tool_terminal(cx)
                        .is_some_and(|terminal| terminal.id == id)
                {
                    open_adb_chooser(
                        id,
                        generation,
                        devices,
                        this.focus_handle.clone(),
                        window,
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn on_choose_adb_device(
        &mut self,
        action: &ChooseAdbDevice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        let Some(view) = self.serial_file_target(id, cx) else {
            return;
        };
        let generation = view.read(cx).generation(cx);
        let detector = self.adb_detector.clone();
        cx.spawn_in(window, async move |this, cx| {
            let detection = cx
                .background_spawn(async move { detector.detect(None) })
                .await;
            this.update_in(cx, |this, window, cx| {
                if this
                    .serial_file_target(id, cx)
                    .is_none_or(|view| view.read(cx).generation(cx) != generation)
                {
                    return;
                }
                match detection {
                    AdbDetection::Matched(connection) => open_adb_chooser(
                        id,
                        generation,
                        vec![connection],
                        this.focus_handle.clone(),
                        window,
                        cx,
                    ),
                    AdbDetection::Choose(devices) => open_adb_chooser(
                        id,
                        generation,
                        devices,
                        this.focus_handle.clone(),
                        window,
                        cx,
                    ),
                    AdbDetection::Unavailable(reason) => window.push_notification(
                        Notification::warning(if reason.is_empty() {
                            t!("adb.unavailable").to_string()
                        } else {
                            reason
                        }),
                        cx,
                    ),
                }
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn on_open_adb_files(
        &mut self,
        action: &OpenAdbFiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (terminal, generation, connection) = &action.0;
        if self
            .serial_file_target(*terminal, cx)
            .is_none_or(|view| view.read(cx).generation(cx) != *generation)
        {
            window.push_notification(
                Notification::warning(t!("serial.files.connection_changed")),
                cx,
            );
            return;
        }
        if self.file_sidebar(*terminal).is_some() {
            window.push_notification(Notification::warning(t!("adb.transfer_active")), cx);
            return;
        }
        self.tools.update(cx, |tools, cx| {
            tools.finish_serial_check(
                *terminal,
                *generation,
                t!("adb.connected", serial = connection.serial()),
                cx,
            )
        });
        self.install_serial_adb_files(*terminal, connection.clone(), window, cx);
    }

    pub(super) fn on_upload_serial_file(
        &mut self,
        action: &UploadSerialFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_serial_file_command(action.0, "if command -v rz >/dev/null 2>&1; then rz; else printf '\\nInstall lrzsz (rz/sz) on the device first\\n'; fi".into(), window, cx);
    }

    pub(super) fn on_download_serial_file(
        &mut self,
        action: &DownloadSerialFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        let Some(view) = self.serial_file_target(id, cx) else {
            return;
        };
        crate::serial::open_serial_download_dialog(id, view, self.focus_handle.clone(), window, cx);
    }

    pub(super) fn on_serial_file_download(
        &mut self,
        action: &SerialFileDownload,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.serial_file_target(action.terminal(), cx) else {
            return;
        };
        if view.read(cx).generation(cx) != action.generation() {
            window.push_notification(
                Notification::warning(t!("serial.files.connection_changed")),
                cx,
            );
            return;
        }
        let Some(quoted) = quote_path(action.path()) else {
            return;
        };
        let command = format!(
            "if command -v sz >/dev/null 2>&1; then sz -- {quoted}; else printf '\\nInstall lrzsz (rz/sz) on the device first\\n'; fi"
        );
        self.start_serial_file_command(action.terminal(), command, window, cx);
    }

    fn start_serial_file_command(
        &mut self,
        id: RemoteTerminalId,
        command: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.serial_file_target(id, cx) else {
            return;
        };
        let generation = view.read(cx).generation(cx);
        let view = view.downgrade();
        window
            .spawn(cx, async move |cx| {
                cx.background_executor().timer(COMMAND_SETTLE).await;
                let Ok(reply) = cx.update(|_, cx| {
                    let view = view.upgrade()?;
                    if view.read(cx).generation(cx) != generation {
                        return None;
                    }
                    view.read(cx).serial_transfer(command, cx)
                }) else {
                    return;
                };
                let result = match reply {
                    Some(reply) => exec_answer(reply, cx).await,
                    None => Some(Err(t!("serial.shell.busy").to_string())),
                };
                if let Some(Err(error)) = result {
                    cx.update(|window, cx| {
                        window.push_notification(Notification::warning(error), cx)
                    })
                    .ok();
                }
            })
            .detach();
    }

    pub(super) fn fallback_serial_network(
        &mut self,
        explorer: crate::explorer::ExplorerId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self
            .file_sidebars
            .iter()
            .find(|(_, id)| **id == explorer)
            .map(|(terminal, _)| *terminal)
        else {
            return;
        };
        if self.serial_file_target(terminal, cx).is_none() {
            return;
        }
        let Some(panel) = self.explorers.get(&explorer).cloned() else {
            return;
        };
        if panel.read(cx).connection_state() != ConnectionState::Disconnected
            || panel.read(cx).is_transferring()
            || !self.editors_of(explorer, cx).is_empty()
        {
            return;
        }
        let generation = self.terminals[&terminal]
            .read(cx)
            .terminal()
            .read(cx)
            .generation(cx);
        self.tools.update(cx, |tools, cx| {
            tools.finish_serial_check(terminal, generation, t!("serial.files.network_failed"), cx)
        });
        panel.update(cx, |panel, cx| panel.close(window, cx));
    }
}

fn open_adb_chooser(
    terminal: RemoteTerminalId,
    generation: u64,
    devices: Vec<AdbConnection>,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    use gpui_kit::component::{
        Sizable as _,
        button::Button,
        dialog::{DialogClose, DialogFooter},
        v_flex,
    };
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(t!("adb.choose"))
            .child(
                v_flex()
                    .id("adb-device-picker")
                    .test_support()
                    .gap_2()
                    .child(div().text_sm().child(t!("adb.choose_note")))
                    .children(devices.iter().map(|device| {
                        let action = OpenAdbFiles((terminal, generation, device.clone()));
                        let dispatch = dispatch.clone();
                        let label = if device.display_name() == device.serial() {
                            SharedString::from(device.serial())
                        } else {
                            t!(
                                "adb.device_label",
                                name = device.display_name(),
                                serial = device.serial()
                            )
                        };
                        Button::new(SharedString::from(format!(
                            "adb-device:{}",
                            device.serial()
                        )))
                        .small()
                        .label(label.clone())
                        .accessibility_label(label)
                        .on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            let dispatch = dispatch.clone();
                            let action = action.clone();
                            window.defer(cx, move |window, cx| {
                                dispatch.dispatch_action(&action, window, cx)
                            });
                        })
                    })),
            )
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|button| button.label(t!("common.cancel")))),
            )
    });
}
