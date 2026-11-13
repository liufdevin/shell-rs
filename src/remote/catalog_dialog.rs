use super::{Command, Server, Service};
use crate::{app::RemoteAction, i18n::t};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    component::{
        ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        checkbox::Checkbox,
        h_flex,
        input::{Input, InputEvent, InputState},
        scroll::ScrollableElement as _,
        spinner::Spinner,
        v_flex,
    },
    *,
};

struct CatalogDialog {
    server: Server,
    networks: bool,
    service: Entity<Service>,
    dispatch: FocusHandle,
    search: Entity<InputState>,
    user: Entity<InputState>,
    current_only: bool,
    page: usize,
    height: Pixels,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

pub(super) fn open(
    server: Server,
    networks: bool,
    service: Entity<Service>,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let dialog = cx.new(|cx| {
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("remote.device_search"))
                .clean_on_escape()
        });
        let user = cx.new(|cx| InputState::new(window, cx).default_value("root"));
        let subscriptions = vec![
            cx.observe(&service, |_, _, cx| cx.notify()),
            cx.observe(&user, |_, _, cx| cx.notify()),
            cx.subscribe(&search, |dialog: &mut CatalogDialog, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    dialog.page = 0;
                    dialog.scroll.set_offset(Point::default());
                    cx.notify();
                }
            }),
        ];
        CatalogDialog {
            server,
            networks,
            service,
            dispatch,
            search,
            user,
            current_only: false,
            page: 0,
            height: px((window.viewport_size().height.as_f32() - 180.).clamp(180., 560.)),
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    });
    window.open_dialog(cx, move |surface, window, _| {
        surface
            .title(if networks {
                t!("remote.network_dialog_title")
            } else {
                t!("remote.device_dialog_title")
            })
            .width(px(
                (window.viewport_size().width.as_f32() - 64.).clamp(280., 960.)
            ))
            .overlay_closable(false)
            .child(dialog.clone())
    });
}

impl CatalogDialog {
    fn action(
        &self,
        id: impl Into<ElementId>,
        label: SharedString,
        command: Command,
        disabled: bool,
    ) -> Button {
        let dispatch = self.dispatch.clone();
        Button::new(id)
            .label(label)
            .disabled(disabled)
            .on_click(move |_, window, cx| {
                if matches!(&command, Command::Ssh { .. } | Command::Devices(_)) {
                    window.close_dialog(cx);
                }
                dispatch.dispatch_action(&RemoteAction(command.clone()), window, cx);
            })
    }
}

impl Render for CatalogDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let service = self.service.read(cx);
        let busy = service.busy;
        let catalog = service.catalogs.get(&self.server.id);
        let active = service
            .connection
            .as_ref()
            .filter(|active| active.server == self.server.id);
        let query = self.search.read(cx).value().trim().to_lowercase();
        let user = self.user.read(cx).value().trim().to_string();
        let mut body = v_flex()
            .gap_3()
            .min_w_0()
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_base()
                            .font_weight(FontWeight::MEDIUM)
                            .truncate()
                            .child(self.server.name.clone()),
                    )
                    .when(self.networks, |row| {
                        row.child(
                            self.action(
                                "remote-view-device-paths",
                                t!("remote.diagnostic.view_devices"),
                                Command::Devices(self.server.id.clone()),
                                busy || catalog.is_none(),
                            )
                            .ghost(),
                        )
                    })
                    .child(
                        self.action(
                            "remote-diagnose",
                            t!("remote.diagnostic.check_network"),
                            Command::Diagnose(self.server.id.clone(), vec![]),
                            busy || active.is_none(),
                        )
                        .outline()
                        .tooltip(t!("remote.diagnostic.help")),
                    )
                    .child(
                        self.action(
                            "remote-refresh",
                            t!("remote.refresh"),
                            Command::RefreshServer(self.server.id.clone()),
                            busy || catalog.is_none(),
                        )
                        .ghost(),
                    )
                    .child(
                        self.action(
                            "remote-disconnect",
                            t!("remote.disconnect"),
                            Command::DisconnectServer(self.server.id.clone()),
                            busy || active.is_none(),
                        )
                        .outline(),
                    ),
            )
            .child(div().text_sm().child(if let Some(active) = active {
                t!(
                    "remote.active_network",
                    name = active.name.clone(),
                    ip = active.virtual_ip.to_string()
                )
            } else {
                t!("remote.device_help")
            }));
        if let Some(status) = service.statuses.get(&self.server.id) {
            let progress = status.progress.is_some()
                || (service.diagnosing && service.pending_server.as_ref() == Some(&self.server.id));
            body = body.child(
                h_flex()
                    .id("remote-status")
                    .test_support()
                    .role(Role::Status)
                    .aria_label(status.message.clone())
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .text_color(if status.failed {
                        cx.theme().danger
                    } else {
                        cx.theme().muted_foreground
                    })
                    .when(progress, |row| {
                        row.child(Spinner::new().xsmall().color(cx.theme().muted_foreground))
                    })
                    .child(status.message.clone()),
            );
        }
        let paths = service.paths(&self.server.id);
        if let Some(active) = active {
            body = body.child(super::diagnostic_view::current_paths(
                "remote-current-paths",
                active,
                paths,
                cx,
            ));
        }
        let diagnostic = service.diagnostics(&self.server.id);
        if let Some(report) = diagnostic {
            body = body.child(super::diagnostic_view::summary(report, cx));
        }
        if let Some(catalog) = catalog {
            body = body.child(
                Input::new(&self.search)
                    .id("remote-catalog-search")
                    .cleanable(true)
                    .prefix(Icon::new(IconName::Search).small()),
            );
            let count;
            let mut rows = v_flex().gap_2().min_w_0();
            if self.networks {
                let networks: Vec<_> = catalog
                    .networks
                    .iter()
                    .filter(|network| {
                        network.name.to_lowercase().contains(&query)
                            || network.subnet.to_lowercase().contains(&query)
                    })
                    .collect();
                count = networks.len();
                let page = self.page.min(count.div_ceil(20).max(1) - 1);
                for network in networks.iter().skip(page * 20).take(20) {
                    let connected =
                        active.is_some_and(|active| active.network == network.network_id);
                    rows = rows.child(
                        h_flex()
                            .gap_3()
                            .p_3()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_2()
                                    .child(div().text_base().truncate().child(network.name.clone()))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!(
                                                "{} · {}",
                                                network.subnet,
                                                t!(
                                                    "remote.device_count",
                                                    count = network.device_count
                                                )
                                            )),
                                    ),
                            )
                            .child(
                                self.action(
                                    format!("remote-connect-{}", network.network_id),
                                    if connected {
                                        t!("remote.connected")
                                    } else {
                                        t!("remote.connect")
                                    },
                                    Command::ConnectServer(
                                        self.server.id.clone(),
                                        network.network_id.clone(),
                                    ),
                                    busy || connected || !network.enabled,
                                )
                                .outline(),
                            ),
                    );
                }
            } else {
                body = body.child(
                    h_flex()
                        .gap_3()
                        .flex_wrap()
                        .child(
                            self.action(
                                "remote-choose-network",
                                t!("remote.choose_network"),
                                Command::Networks(self.server.id.clone()),
                                busy,
                            )
                            .outline(),
                        )
                        .child(
                            Checkbox::new("remote-current-only")
                                .label(t!("remote.current_only"))
                                .checked(self.current_only)
                                .on_click(cx.listener(|dialog, checked, _, cx| {
                                    dialog.current_only = *checked;
                                    dialog.page = 0;
                                    cx.notify();
                                })),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(t!("remote.ssh_user"))
                                .child(Input::new(&self.user).id("remote-ssh-user").w(rems(9.))),
                        ),
                );
                let devices: Vec<_> = catalog
                    .devices
                    .iter()
                    .filter(|device| {
                        let in_network =
                            active.is_some_and(|active| active.network == device.network_id);
                        device.lease_valid()
                            && (!self.current_only || in_network)
                            && [
                                device.label().to_string(),
                                device.customer_name.clone(),
                                device.device_id.clone(),
                                device
                                    .virtual_ip
                                    .map(|ip| ip.to_string())
                                    .unwrap_or_default(),
                            ]
                            .iter()
                            .any(|value| value.to_lowercase().contains(&query))
                    })
                    .collect();
                count = devices.len();
                body = body.child(
                    div()
                        .id("remote-device-count")
                        .test_support()
                        .aria_label(t!("remote.device_count", count = count))
                        .text_base()
                        .child(t!("remote.device_count", count = count)),
                );
                let page = self.page.min(count.div_ceil(20).max(1) - 1);
                for device in devices.iter().skip(page * 20).take(20) {
                    let allowed = service
                        .ssh_target(&self.server.id, &device.device_id)
                        .is_some();
                    let connected = active.is_some_and(|active| {
                        active.devices.iter().any(|target| {
                            target.device_id == device.device_id
                                && Some(target.virtual_ip) == device.virtual_ip
                        })
                    });
                    rows = rows.child(
                        h_flex()
                            .id(SharedString::from(format!(
                                "remote-device-{}",
                                device.device_id
                            )))
                            .test_support()
                            .gap_4()
                            .p_3()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_base()
                                            .font_weight(FontWeight::MEDIUM)
                                            .truncate()
                                            .child(device.label().to_owned()),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_4()
                                            .flex_wrap()
                                            .text_sm()
                                            .child(device.customer_name.clone())
                                            .child(
                                                device
                                                    .virtual_ip
                                                    .map(|ip| ip.to_string())
                                                    .unwrap_or_else(|| {
                                                        t!("remote.unconfigured").to_string()
                                                    }),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "remote-device-identity-{}",
                                                device.device_id
                                            )))
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .truncate()
                                            .tooltip({
                                                let id = device.device_id.clone();
                                                move |window, cx| {
                                                    gpui_kit::component::tooltip::Tooltip::new(
                                                        id.clone(),
                                                    )
                                                    .build(window, cx)
                                                }
                                            })
                                            .child(t!(
                                                "remote.device_id",
                                                id = device.device_id.clone()
                                            )),
                                    )
                                    .when(connected, |view| {
                                        view.child(super::diagnostic_view::current_device(
                                            &device.device_id,
                                            device.virtual_ip.and_then(|address| {
                                                paths.and_then(|snapshot| {
                                                    snapshot.route(&device.device_id, address)
                                                })
                                            }),
                                            cx,
                                        ))
                                    })
                                    .children(diagnostic.and_then(|report| {
                                        report
                                            .devices
                                            .iter()
                                            .find(|result| result.device_id == device.device_id)
                                            .map(|result| {
                                                super::diagnostic_view::device(
                                                    result,
                                                    report.is_stale(),
                                                    cx,
                                                )
                                            })
                                    })),
                            )
                            .child(
                                self.action(
                                    format!("remote-diagnose-{}", device.device_id),
                                    t!("remote.diagnostic.check_device"),
                                    Command::Diagnose(
                                        self.server.id.clone(),
                                        vec![device.device_id.clone()],
                                    ),
                                    busy || !allowed,
                                )
                                .ghost(),
                            )
                            .child(
                                self.action(
                                    format!("remote-ssh-{}", device.device_id),
                                    t!("remote.ssh"),
                                    Command::Ssh {
                                        server: self.server.id.clone(),
                                        device: device.device_id.clone(),
                                        user: user.clone(),
                                    },
                                    busy || !allowed || user.is_empty(),
                                )
                                .outline(),
                            ),
                    );
                }
            }
            if count == 0 {
                rows = rows.child(
                    div()
                        .p_4()
                        .text_base()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("remote.no_catalog_matches")),
                );
            }
            body = body.child(
                div()
                    .id("remote-catalog-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .vertical_scrollbar(&self.scroll)
                    .child(rows),
            );
            let pages = count.div_ceil(20).max(1);
            let page = self.page.min(pages - 1);
            body = body.child(
                h_flex()
                    .justify_between()
                    .child(
                        Button::new("remote-page-previous")
                            .label(t!("remote.previous"))
                            .disabled(page == 0)
                            .on_click(cx.listener(move |dialog, _, _, cx| {
                                dialog.page = page.saturating_sub(1);
                                dialog.scroll.set_offset(Point::default());
                                cx.notify();
                            })),
                    )
                    .child(format!("{}/{}", page + 1, pages))
                    .child(
                        Button::new("remote-page-next")
                            .label(t!("remote.next"))
                            .disabled(page + 1 >= pages)
                            .on_click(cx.listener(move |dialog, _, _, cx| {
                                dialog.page = (page + 1).min(pages - 1);
                                dialog.scroll.set_offset(Point::default());
                                cx.notify();
                            })),
                    ),
            );
        } else {
            body = body.child(div().text_base().child(t!("remote.select_login")));
        }
        body.id("remote-catalog-dialog")
            .test_support()
            .h(self.height)
    }
}
