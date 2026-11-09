use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::Button,
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputContentType, InputEvent, InputState},
    scroll::ScrollableElement as _,
    switch::Switch,
    v_flex,
};
use gpui_kit::*;

use super::{LanCommand, LanService};
use crate::app::LanAction;
use crate::cli::Secret;
use crate::i18n::t;

pub struct LanPage {
    service: Entity<LanService>,
    password: Entity<InputState>,
    repeat: Entity<InputState>,
    address: Entity<InputState>,
    remote_password: Entity<InputState>,
    command: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl LanPage {
    pub fn new(service: Entity<LanService>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let password = cx.new(|cx| InputState::new(window, cx));
        let repeat = cx.new(|cx| InputState::new(window, cx));
        let remote_password = cx.new(|cx| InputState::new(window, cx));
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("192.168.1.10:45838"));
        let command = cx.new(|cx| InputState::new(window, cx).placeholder("uname -a"));
        let mut subscriptions = vec![cx.observe(&service, |_, _, cx| cx.notify())];
        for input in [&password, &repeat, &remote_password, &address, &command] {
            subscriptions.push(cx.subscribe(input, |_, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }));
        }
        Self {
            service,
            password,
            repeat,
            address,
            remote_password,
            command,
            _subscriptions: subscriptions,
        }
    }
}

pub(super) fn button(
    id: impl Into<ElementId>,
    label: SharedString,
    command: LanCommand,
    disabled: bool,
) -> Button {
    Button::new(id)
        .small()
        .outline()
        .label(label)
        .disabled(disabled)
        .on_click(move |_, window, cx| {
            window.dispatch_action(Box::new(LanAction(command.clone())), cx)
        })
}

impl Render for LanPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let service = self.service.read(cx);
        let enabled = service.enabled();
        let busy = service.busy;
        let available = service.available();
        let password = self.password.read(cx).value().to_string();
        let repeat = self.repeat.read(cx).value().to_string();
        let valid_password = password.chars().count() >= shellrs_lan::MIN_PAIRING_PASSWORD_CHARS
            && password.len() <= 1024
            && password == repeat;
        let secret = Secret::new(password);
        let clear_password = self.password.clone();
        let clear_repeat = self.repeat.clone();
        let save = Button::new("lan-save-password")
            .small()
            .outline()
            .label(t!("lan.save_password"))
            .disabled(!valid_password || !available || busy)
            .on_click(move |_, window, cx| {
                window.dispatch_action(
                    Box::new(LanAction(LanCommand::SavePassword(secret.clone()))),
                    cx,
                );
                clear_password.update(cx, |input, cx| input.set_value("", window, cx));
                clear_repeat.update(cx, |input, cx| input.set_value("", window, cx));
            });
        let selected = service.selected_hosts();
        let shared_hosts = service.local_hosts();
        let identity = service
            .core()
            .map(|core| core.id().to_owned())
            .unwrap_or_default();
        let status = if !available && !busy && service.status.is_empty() {
            t!("lan.unavailable").to_string()
        } else {
            service.status.clone()
        };
        let wanted = enabled || service.core().is_some_and(|core| core.enabled_on_start());
        let mut view = v_flex()
            .id("lan-settings")
            .test_support()
            .w_full()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("lan.description")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("lan.network_help")),
            )
            .children(super::firewall::section(service, cx))
            .child(
                Switch::new("lan-enable")
                    .label(t!("lan.enable"))
                    .checked(wanted)
                    .disabled(busy || !available)
                    .on_change(|enabled, window, cx| {
                        let command = if *enabled {
                            LanCommand::Enable
                        } else {
                            LanCommand::Disable
                        };
                        window.dispatch_action(Box::new(LanAction(command)), cx);
                    }),
            )
            .child(
                Switch::new("lan-discoverable")
                    .label(t!("lan.discoverable"))
                    .checked(service.core().is_some_and(|core| core.is_discoverable()))
                    .disabled(busy || !available)
                    .on_change(|discoverable, window, cx| {
                        window.dispatch_action(
                            Box::new(LanAction(LanCommand::SetDiscoverable(*discoverable))),
                            cx,
                        );
                    }),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("lan.discoverable_help")),
            )
            .child(button(
                "lan-discover",
                t!("lan.discover"),
                LanCommand::Discover,
                !enabled || busy,
            ))
            .child(
                div()
                    .id("lan-status")
                    .role(Role::Status)
                    .test_support()
                    .aria_label(status.clone())
                    .text_sm()
                    .child(status),
            )
            .child(
                div()
                    .id("lan-identity")
                    .role(Role::Label)
                    .aria_label(identity.clone())
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(identity.clone()),
            )
            .child(div().text_sm().child(t!("lan.password_help")))
            .child(
                Input::new(&self.password)
                    .id("lan-password")
                    .content_type(InputContentType::Password)
                    .mask_toggle()
                    .small(),
            )
            .child(div().text_sm().child(t!("lan.repeat_password")))
            .child(
                Input::new(&self.repeat)
                    .id("lan-password-repeat")
                    .content_type(InputContentType::Password)
                    .mask_toggle()
                    .small(),
            )
            .child(save)
            .children((!repeat.is_empty() && !valid_password).then(|| {
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(t!("lan.password_mismatch"))
            }))
            .child(super::device_page::local(service, cx))
            .child(div().text_sm().child(t!("lan.shared_hosts")))
            .child(
                Checkbox::new("lan-allow-execution")
                    .label(t!("lan.allow_execution"))
                    .checked(service.permits_execution())
                    .disabled(!available || busy)
                    .on_click(|checked, window, cx| {
                        window.dispatch_action(
                            Box::new(LanAction(LanCommand::AllowExecution(*checked))),
                            cx,
                        )
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("lan.execution_help")),
            );
        for host in shared_hosts {
            let id = host.id.clone();
            let checked = selected.contains(&id);
            view = view.child(
                Checkbox::new(SharedString::from(format!("lan-share-{id}")))
                    .label(format!(
                        "{} · {}@{}:{}",
                        host.name, host.user, host.address, host.port
                    ))
                    .checked(checked)
                    .disabled(busy)
                    .on_click(move |value, window, cx| {
                        window.dispatch_action(
                            Box::new(LanAction(LanCommand::ShareHost(id.clone(), *value))),
                            cx,
                        )
                    }),
            );
        }
        view = view
            .child(div().text_sm().child(t!("lan.connect")))
            .child(Input::new(&self.address).id("lan-address").small())
            .child(button(
                "lan-inspect",
                t!("lan.inspect"),
                LanCommand::Inspect(self.address.read(cx).value().to_string()),
                !enabled || busy,
            ));
        for device in &service.devices {
            view = view.child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(format!("{} · {}", device.name, device.endpoint))
                    .child(button(
                        SharedString::from(format!("lan-connect-{}", device.id)),
                        t!("lan.inspect"),
                        LanCommand::Inspect(device.endpoint.to_string()),
                        busy,
                    )),
            );
        }
        if let Some(device) = &service.candidate {
            let remote_secret = Secret::new(self.remote_password.read(cx).value().to_string());
            let clear = self.remote_password.clone();
            view = view
                .child({
                    let identity = t!(
                        "lan.target_identity",
                        address = device.endpoint.to_string(),
                        identity = device.id.clone()
                    );
                    div()
                        .id("lan-target-identity")
                        .role(Role::Label)
                        .aria_label(identity.clone())
                        .text_sm()
                        .child(identity)
                })
                .child(
                    div()
                        .text_sm()
                        .child(t!("lan.pair_code", code = device.pairing_code(&identity))),
                )
                .child(button(
                    "lan-request-pair",
                    t!("lan.request_pair"),
                    LanCommand::RequestPair,
                    busy || !enabled,
                ))
                .child(div().text_sm().child(t!("lan.remote_password")))
                .child(
                    Input::new(&self.remote_password)
                        .id("lan-remote-password")
                        .content_type(InputContentType::Password)
                        .mask_toggle()
                        .small(),
                )
                .child(
                    Button::new("lan-password-pair")
                        .small()
                        .outline()
                        .label(t!("lan.password_pair"))
                        .disabled(
                            busy || !enabled || self.remote_password.read(cx).value().is_empty(),
                        )
                        .on_click(move |_, window, cx| {
                            window.dispatch_action(
                                Box::new(LanAction(LanCommand::PasswordPair(
                                    remote_secret.clone(),
                                ))),
                                cx,
                            );
                            clear.update(cx, |input, cx| input.set_value("", window, cx));
                        }),
                );
        }
        view = view.child(div().text_sm().child(t!("lan.paired_devices")));
        for device in service.peers() {
            let id = device.id.clone();
            view = view.child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(format!("{} · {}", device.name, device.endpoint))
                    .child(button(
                        SharedString::from(format!("lan-peer-devices-{id}")),
                        t!("lan.devices.view"),
                        LanCommand::RefreshDevices(id.clone()),
                        !enabled || busy,
                    ))
                    .child(button(
                        SharedString::from(format!("lan-hosts-{id}")),
                        t!("lan.view_hosts"),
                        LanCommand::RefreshHosts(id.clone()),
                        !enabled || busy,
                    ))
                    .child(button(
                        SharedString::from(format!("lan-forget-{id}")),
                        t!("lan.forget"),
                        LanCommand::Forget(id),
                        busy,
                    )),
            );
        }
        view = view.child(super::device_page::remote(service, cx));
        for host in &service.remote {
            let id = host.id.clone();
            view = view.child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Checkbox::new(SharedString::from(format!("lan-remote-host-{id}")))
                            .label(format!(
                                "{} · {}@{}:{}",
                                host.name, host.user, host.address, host.port
                            ))
                            .checked(service.host.as_deref() == Some(id.as_str()))
                            .on_click(move |_, window, cx| {
                                window.dispatch_action(
                                    Box::new(LanAction(LanCommand::SelectHost(id.clone()))),
                                    cx,
                                )
                            }),
                    )
                    .children(service.peer.as_ref().map(|peer| {
                        button(
                            SharedString::from(format!("lan-open-host-{}", host.id)),
                            t!("lan.open_ssh_terminal"),
                            LanCommand::OpenHost {
                                peer: peer.clone(),
                                host: host.id.clone(),
                            },
                            !enabled || !service.is_remote_execution_allowed(),
                        )
                    })),
            );
        }
        if !service.remote.is_empty() {
            view = view.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("lan.ssh_terminal_help")),
            );
        }
        view = view
            .child(Input::new(&self.command).id("lan-command").small())
            .child(
                h_flex()
                    .gap_2()
                    .child(button(
                        "lan-execute",
                        t!("lan.execute"),
                        LanCommand::Execute(self.command.read(cx).value().to_string()),
                        !enabled
                            || !service.is_remote_execution_allowed()
                            || service.running
                            || service.host.is_none(),
                    ))
                    .child(button(
                        "lan-cancel",
                        t!("lan.cancel"),
                        LanCommand::Cancel,
                        !service.running,
                    )),
            )
            .child(
                div()
                    .id("lan-output")
                    .test_support()
                    .aria_label(String::from_utf8_lossy(&service.output).to_string())
                    .max_h_64()
                    .overflow_y_scrollbar()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_sm()
                    .child(String::from_utf8_lossy(&service.output).to_string()),
            )
            .child(div().text_sm().child(t!("lan.authorized_devices")));
        for grant in service.grants() {
            let id = grant.id.clone();
            let permission_id = id.clone();
            view = view.child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(grant.name)
                    .child(
                        Checkbox::new(SharedString::from(format!("lan-grant-exec-{id}")))
                            .label(t!("lan.grant_execute"))
                            .checked(grant.execute && service.permits_execution())
                            .disabled(!service.permits_execution())
                            .on_click(move |execute, window, cx| {
                                window.dispatch_action(
                                    Box::new(LanAction(LanCommand::SetExecutionPermission(
                                        permission_id.clone(),
                                        *execute,
                                    ))),
                                    cx,
                                )
                            }),
                    )
                    .child(button(
                        SharedString::from(format!("lan-revoke-{id}")),
                        t!("lan.revoke"),
                        LanCommand::Revoke(id),
                        false,
                    )),
            );
        }
        view = view.child(div().text_sm().child(t!("lan.blacklist")));
        for address in service.blocked() {
            view = view.child(h_flex().gap_2().child(address.to_string()).child(button(
                SharedString::from(format!("lan-unblock-{address}")),
                t!("lan.unblock"),
                LanCommand::Unblock(address),
                false,
            )));
        }
        view.child(super::device_page::activity(service, cx))
    }
}
