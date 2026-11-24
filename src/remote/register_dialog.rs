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

struct RegisterForm {
    url: Entity<InputState>,
    name: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    confirm: Entity<InputState>,
    error: Option<SharedString>,
}

pub(super) fn open(
    initial_url: Option<String>,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let default_url = initial_url.unwrap_or_default();
    let form = cx.new(|cx| RegisterForm {
        url: cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(default_url)
                .placeholder("https://remote.example.com:8180")
        }),
        name: cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("remote.register.name_placeholder"))
                .validate(|text, _| text.len() <= 100)
        }),
        username: cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("remote.register.username_placeholder"))
                .validate(|text, _| text.len() <= 64)
        }),
        password: cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(t!("remote.register.password_placeholder"))
                .validate(|text, _| text.len() <= 128)
        }),
        confirm: cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(t!("remote.register.confirm_placeholder"))
                .validate(|text, _| text.len() <= 128)
        }),
        error: None,
    });
    window.open_dialog(cx, move |dialog, _, _| {
        let submit = form.clone();
        let dispatch = dispatch.clone();
        dialog
            .title(t!("remote.register.title"))
            .overlay_closable(false)
            .child(form.clone())
            .footer(commit_footer(
                "remote-register-submit",
                t!("remote.register.submit"),
            ))
            .on_ok(move |_, window, cx| {
                submit.update(cx, |form, cx| form.submit(&dispatch, window, cx))
            })
    });
}

impl RegisterForm {
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
        let name = self.name.read(cx).value().trim().to_owned();
        let username = self.username.read(cx).value().trim().to_owned();
        let password = self.password.read(cx).value().to_string();
        let confirm = self.confirm.read(cx).value().to_string();
        if name.is_empty() || name.len() > 100 {
            self.error = Some(t!("remote.register.invalid_name"));
            cx.notify();
            return false;
        }
        let valid_username = (3..=64).contains(&username.len())
            && username
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if !valid_username {
            self.error = Some(t!("remote.register.invalid_username"));
            cx.notify();
            return false;
        }
        if password.len() < 8 || password.len() > 128 {
            self.error = Some(t!("remote.register.invalid_password"));
            cx.notify();
            return false;
        }
        if password != confirm {
            self.error = Some(t!("remote.register.password_mismatch"));
            cx.notify();
            return false;
        }
        self.error = None;
        dispatch.dispatch_action(
            &RemoteAction(Command::RegisterAccount {
                url,
                name,
                username,
                password: Secret::new(password),
            }),
            window,
            cx,
        );
        true
    }
}

impl Render for RegisterForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut form = v_flex()
            .id("remote-register-dialog")
            .test_support()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("remote.register.help")),
            )
            .child(field(
                t!("remote.server_url"),
                Input::new(&self.url)
                    .id("remote-register-url")
                    .content_type(InputContentType::Url),
            ))
            .child(field(
                t!("remote.register.name"),
                Input::new(&self.name).id("remote-register-name"),
            ))
            .child(field(
                t!("remote.username"),
                Input::new(&self.username).id("remote-register-username"),
            ))
            .child(field(
                t!("remote.password"),
                Input::new(&self.password)
                    .id("remote-register-password")
                    .mask_toggle()
                    .content_type(InputContentType::Password),
            ))
            .child(field(
                t!("remote.register.confirm_password"),
                Input::new(&self.confirm)
                    .id("remote-register-confirm")
                    .mask_toggle()
                    .content_type(InputContentType::Password),
            ));
        if let Some(error) = &self.error {
            form = form.child(
                div()
                    .id("remote-register-error")
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
