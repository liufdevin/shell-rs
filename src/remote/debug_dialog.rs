use super::{Command, api};
use crate::{app::RemoteAction, cli::Secret, i18n::t, shared::commit_footer};
use gpui_kit::{
    component::{
        ActiveTheme as _, WindowExt as _,
        input::{Input, InputContentType, InputState},
        v_flex,
    },
    *,
};

struct DebugDeviceForm {
    url: Entity<InputState>,
    device_id: Entity<InputState>,
    key: Entity<InputState>,
    error: Option<SharedString>,
}

pub(super) fn open(
    initial_url: Option<String>,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let default_url = initial_url.unwrap_or_default();
    let form = cx.new(|cx| DebugDeviceForm {
        url: cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(default_url)
                .placeholder("https://remote.example.com:8180")
        }),
        device_id: cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("7K9M-2R8X")
                .validate(|text, _| text.len() <= 64)
        }),
        key: cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("5W3P-6T4H")
                .validate(|text, _| text.len() <= 64)
        }),
        error: None,
    });
    window.open_dialog(cx, move |dialog, _, _| {
        let submit = form.clone();
        let dispatch = dispatch.clone();
        dialog
            .title(t!("remote.debug.title"))
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
        dispatch.dispatch_action(
            &RemoteAction(Command::OpenDebugScreen {
                url,
                device_id,
                key: Secret::new(key),
            }),
            window,
            cx,
        );
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
            ));
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
