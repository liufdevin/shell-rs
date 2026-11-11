use super::{Command, Server, Service};
use crate::{app::RemoteAction, cli::Secret, i18n::t, shared::commit_footer};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    component::{
        ActiveTheme as _, Disableable as _, WindowExt as _,
        checkbox::Checkbox,
        input::{Input, InputContentType, InputEvent, InputState},
        v_flex,
    },
    *,
};

struct LoginForm {
    id: String,
    name: Entity<InputState>,
    url: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    remember: bool,
    available: bool,
    loading: bool,
    touched: bool,
    error: Option<SharedString>,
    task: Option<Task<()>>,
    _subscription: Subscription,
}

pub(super) fn open(
    server: Option<Server>,
    password: Option<Secret>,
    service: Entity<Service>,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let saved = server.is_some();
    let server = server.unwrap_or_else(|| Server::new(String::new(), String::new(), String::new()));
    let secrets = service.read(cx).secrets.clone();
    let available = secrets.is_available();
    let loading = saved && password.is_none() && available;
    let form = cx.new(|cx| {
        let password_input = cx.new(|cx| {
            InputState::new(window, cx).masked(true).default_value(
                password
                    .as_ref()
                    .map(|value| value.expose().to_owned())
                    .unwrap_or_default(),
            )
        });
        let subscription = cx.subscribe(&password_input, |form: &mut LoginForm, _, event, _| {
            if matches!(event, InputEvent::Change) {
                form.touched = true;
            }
        });
        LoginForm {
            id: server.id.clone(),
            name: cx.new(|cx| InputState::new(window, cx).default_value(server.name.clone())),
            url: cx.new(|cx| InputState::new(window, cx).default_value(server.url.clone())),
            user: cx.new(|cx| InputState::new(window, cx).default_value(server.username.clone())),
            password: password_input,
            remember: available,
            available,
            loading,
            touched: false,
            error: if password.is_some() {
                Some(t!("remote.login_retry"))
            } else {
                None
            },
            task: None,
            _subscription: subscription,
        }
    });
    if loading {
        form.update(cx, |form, cx| {
            form.task = Some(cx.spawn_in(window, async move |form, cx| {
                let result = cx
                    .background_spawn(async move {
                        super::credentials::read_password(&secrets, &server)
                    })
                    .await;
                let _ = form.update_in(cx, |form, window, cx| {
                    form.loading = false;
                    match result {
                        Ok(Some(password)) if !form.touched => {
                            form.password.update(cx, |input, cx| {
                                input.set_value(password.expose(), window, cx)
                            })
                        }
                        Err(_) => form.error = Some(t!("remote.error.credential_read")),
                        _ => {}
                    }
                    cx.notify();
                });
            }));
        });
    }
    window.open_dialog(cx, move |dialog, _, _| {
        let submit = form.clone();
        let dispatch = dispatch.clone();
        dialog
            .title(t!("remote.login_title"))
            .overlay_closable(false)
            .child(form.clone())
            .footer(commit_footer("remote-login", t!("remote.login")))
            .on_ok(move |_, window, cx| {
                submit.update(cx, |form, cx| form.submit(&dispatch, window, cx))
            })
    });
}

impl LoginForm {
    fn submit(
        &mut self,
        dispatch: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.loading {
            return false;
        }
        let server = Server {
            id: self.id.clone(),
            name: self.name.read(cx).value().trim().into(),
            url: self.url.read(cx).value().trim().into(),
            username: self.user.read(cx).value().trim().into(),
        };
        let password = Secret::new(self.password.read(cx).value().to_string());
        if server.validate().is_err() || password.expose().is_empty() {
            self.error = Some(t!("remote.invalid_server"));
            cx.notify();
            return false;
        }
        self.password
            .update(cx, |input, cx| input.set_value("", window, cx));
        dispatch.dispatch_action(
            &RemoteAction(Command::SaveLogin(server, password, self.remember)),
            window,
            cx,
        );
        true
    }
}

impl Render for LoginForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("remote.login_help")),
            )
            .child(t!("remote.server_name"))
            .child(Input::new(&self.name).id("remote-server-name"))
            .child(t!("remote.server_url"))
            .child(Input::new(&self.url).id("remote-server-url"))
            .child(t!("remote.username"))
            .child(Input::new(&self.user).id("remote-username"))
            .child(t!("remote.password"))
            .child(
                Input::new(&self.password)
                    .id("remote-password")
                    .content_type(InputContentType::Password)
                    .mask_toggle(),
            )
            .child(
                Checkbox::new("remote-remember-password")
                    .label(t!("remote.remember_password"))
                    .checked(self.remember)
                    .disabled(!self.available)
                    .on_click(cx.listener(|form, checked, _, cx| {
                        form.remember = *checked;
                        cx.notify();
                    })),
            )
            .when(!self.available, |view| {
                view.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("remote.no_keychain")),
                )
            })
            .when(self.loading, |view| {
                view.child(
                    div()
                        .id("remote-loading-password")
                        .test_support()
                        .text_sm()
                        .child(t!("remote.loading_password")),
                )
            })
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .id("remote-form-error")
                        .test_support()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
    }
}
