use super::{Command, SavedDebugDevice, Service, api};
use crate::{app::RemoteAction, cli::Secret, i18n::t, shared::commit_footer};
use gpui_kit::{
    component::{
        ActiveTheme as _, Disableable as _, WindowExt as _,
        checkbox::Checkbox,
        input::{Input, InputContentType, InputState},
        v_flex,
    },
    *,
};

struct DebugDeviceForm {
    existing_id: Option<String>,
    name: Entity<InputState>,
    url: Entity<InputState>,
    device_id: Entity<InputState>,
    key: Entity<InputState>,
    save: bool,
    can_save: bool,
    error: Option<SharedString>,
}

pub(super) fn open(
    initial_url: Option<String>,
    existing: Option<SavedDebugDevice>,
    service: Entity<Service>,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let can_save = service.read(cx).secrets.is_available();
    let editing = existing.is_some();
    let default_name = existing
        .as_ref()
        .map(|device| device.name.clone())
        .unwrap_or_default();
    let default_url = existing
        .as_ref()
        .map(|device| device.url.clone())
        .or(initial_url)
        .unwrap_or_default();
    let default_device_id = existing
        .as_ref()
        .map(|device| device.device_id.clone())
        .unwrap_or_default();
    let existing_id = existing.map(|device| device.id);

    let form = cx.new(|cx| DebugDeviceForm {
        existing_id,
        name: cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(default_name)
                .placeholder(t!("remote.debug.name_placeholder"))
                .validate(|text, _| text.len() <= 128)
        }),
        url: cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(default_url)
                .placeholder("https://remote.example.com:8180")
        }),
        device_id: cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(default_device_id)
                .placeholder("7K9M-2R8X")
                .validate(|text, _| text.len() <= 64)
        }),
        key: cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("5W3P-6T4H")
                .validate(|text, _| text.len() <= 64)
        }),
        save: can_save,
        can_save,
        error: None,
    });
    let title = if editing {
        t!("remote.debug.edit_title")
    } else {
        t!("remote.debug.title")
    };
    window.open_dialog(cx, move |dialog, _, _| {
        let submit = form.clone();
        let dispatch = dispatch.clone();
        dialog
            .title(title.clone())
            .overlay_closable(false)
            .child(form.clone())
            .footer(commit_footer(
                "remote-debug-connect",
                t!("remote.debug.connect"),
            ))
            .on_ok(move |_, window, cx| {
                submit.update(cx, |form, cx| form.submit(&dispatch, window, cx))
            })
    });
}

impl DebugDeviceForm {
    fn submit(
        &mut self,
        dispatch: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let name = self.name.read(cx).value().trim().to_owned();
        let url_raw = self.url.read(cx).value().trim().to_owned();
        let url = match api::server_url(&url_raw) {
            Ok(url) => url,
            Err(error) => {
                self.error = Some(if error.to_string() == "https_required" {
                    t!("remote.https_required")
                } else {
                    t!("remote.invalid_server")
                });
                cx.notify();
                return false;
            }
        };
        let device_id = self.device_id.read(cx).value().trim().to_owned();
        let key = self.key.read(cx).value().trim().to_owned();
        if device_id.is_empty() || key.is_empty() {
            self.error = Some(t!("remote.debug.required"));
            cx.notify();
            return false;
        }
        self.error = None;
        let save = self.save && self.can_save;
        if save {
            let mut device = SavedDebugDevice::new(name, url, device_id);
            if let Some(id) = &self.existing_id {
                device.id = id.clone();
            }
            dispatch.dispatch_action(
                &RemoteAction(Command::SaveDebugScreen {
                    device,
                    key: Secret::new(key),
                    save: true,
                }),
                window,
                cx,
            );
        } else {
            dispatch.dispatch_action(
                &RemoteAction(Command::OpenDebugScreen {
                    url,
                    device_id,
                    key: Secret::new(key),
                }),
                window,
                cx,
            );
        }
        true
    }
}

impl Render for DebugDeviceForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut form = v_flex()
            .id("remote-debug-dialog")
            .test_support()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("remote.debug.help")),
            )
            .child(field(
                t!("remote.debug.name"),
                Input::new(&self.name).id("remote-debug-name"),
            ))
            .child(field(
                t!("remote.server_url"),
                Input::new(&self.url)
                    .id("remote-debug-url")
                    .content_type(InputContentType::Url),
            ))
            .child(field(
                t!("remote.debug.device_id"),
                Input::new(&self.device_id)
                    .id("remote-debug-device-id")
                    .font_family(cx.theme().mono_font_family.clone()),
            ))
            .child(field(
                t!("remote.debug.key"),
                Input::new(&self.key)
                    .id("remote-debug-key")
                    .mask_toggle()
                    .font_family(cx.theme().mono_font_family.clone())
                    .content_type(InputContentType::Password),
            ))
            .child(
                Checkbox::new("remote-debug-save")
                    .label(t!("remote.debug.save"))
                    .checked(self.save && self.can_save)
                    .disabled(!self.can_save)
                    .on_click(cx.listener(|form, checked, _, cx| {
                        form.save = *checked;
                        cx.notify();
                    })),
            );
        if let Some(error) = &self.error {
            form = form.child(
                div()
                    .id("remote-debug-error")
                    .test_support()
                    .aria_label(error.clone())
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        form
    }
}

fn field(label: SharedString, input: impl IntoElement) -> Div {
    v_flex()
        .gap_1()
        .child(div().text_sm().child(label))
        .child(input)
}
