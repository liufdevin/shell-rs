use super::{Event, Operation, Server, Service};
use crate::{
    app::{CatalogIcon, RemoteAction},
    cli::Secret,
    i18n::{UiLocale, t},
    settings::SettingsStore,
    shared::confirm_delete,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    component::{
        ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _,
        button::Button,
        h_flex,
        input::{Input, InputState},
        list::ListItem,
        menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem},
        scroll::ScrollableElement as _,
        spinner::Spinner,
        v_flex,
    },
    *,
};
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Add,
    Select(String),
    Activate(String),
    SignIn(String),
    Edit(String),
    SaveLogin(Server, Secret, bool),
    Devices(String),
    Networks(String),
    RefreshServer(String),
    Diagnose(String, Vec<String>),
    ConnectServer(String, String),
    DisconnectServer(String),
    LogoutServer(String),
    Delete(String),
    ConfirmDelete(String),
    Ssh {
        server: String,
        device: String,
        user: String,
    },
}

pub struct RemotePanel {
    service: Entity<Service>,
    settings: Entity<SettingsStore>,
    dispatch: FocusHandle,
    focus: FocusHandle,
    selected: Option<String>,
    search: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl RemotePanel {
    pub fn new(
        service: Entity<Service>,
        settings: Entity<SettingsStore>,
        dispatch: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("remote.search"))
                .clean_on_escape()
        });
        let subscriptions = vec![
            cx.observe(&service, |_, _, cx| cx.notify()),
            cx.observe(&settings, |_, _, cx| cx.notify()),
            cx.observe(&search, |_, _, cx| cx.notify()),
            cx.observe_global_in::<UiLocale>(window, |panel, window, cx| {
                panel.search.update(cx, |search, cx| {
                    search.set_placeholder(t!("remote.search"), window, cx)
                });
            }),
            cx.subscribe_in(&service, window, |panel, _, event, window, cx| {
                match event {
                    Event::LoginRequired(server, password) => super::login_dialog::open(
                        Some(server.clone()),
                        password.clone(),
                        panel.service.clone(),
                        panel.dispatch.clone(),
                        window,
                        cx,
                    ),
                    Event::Forgotten(id) => {
                        panel.settings.update(cx, |store, cx| {
                            store.update(
                                |settings| {
                                    settings.remote_servers.retain(|server| &server.id != id)
                                },
                                cx,
                            )
                        });
                        if panel.selected.as_ref() == Some(id) {
                            panel.selected = None;
                        }
                    }
                    _ => {}
                }
                cx.notify();
            }),
        ];
        Self {
            service,
            settings,
            dispatch,
            focus: cx.focus_handle(),
            selected: None,
            search,
            _subscriptions: subscriptions,
        }
    }

    pub fn title(&self) -> SharedString {
        t!("remote.title")
    }

    pub fn toolbar_buttons(&self) -> Vec<Button> {
        let dispatch = self.dispatch.clone();
        vec![
            Button::new("remote-add")
                .icon(IconName::Plus)
                .tooltip(t!("remote.add"))
                .accessibility_label(t!("remote.add"))
                .on_click(move |_, window, cx| {
                    dispatch.dispatch_action(&RemoteAction(Command::Add), window, cx)
                }),
        ]
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |search, cx| search.focus(window, cx));
    }

    fn server(&self, id: &str, cx: &App) -> Option<Server> {
        self.settings
            .read(cx)
            .settings()
            .remote_servers
            .into_iter()
            .find(|server| server.id == id)
    }

    pub fn command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        if let Command::Select(id) = &command {
            self.selected = Some(id.clone());
            cx.notify();
            return;
        }
        if self.service.read(cx).busy {
            return;
        }
        let networks = matches!(&command, Command::Networks(_));
        match command {
            Command::Add => super::login_dialog::open(
                None,
                None,
                self.service.clone(),
                self.dispatch.clone(),
                window,
                cx,
            ),
            Command::Activate(id) => {
                let next = if self.service.read(cx).is_logged_in(&id) {
                    Command::Devices(id)
                } else {
                    Command::SignIn(id)
                };
                self.command(next, window, cx);
            }
            Command::SignIn(id) => {
                if let Some(server) = self.server(&id, cx) {
                    self.selected = Some(id);
                    self.service.update(cx, |service, cx| {
                        service.run(Operation::SavedLogin(server), cx)
                    });
                }
            }
            Command::Edit(id) => {
                if let Some(server) = self.server(&id, cx) {
                    super::login_dialog::open(
                        Some(server),
                        None,
                        self.service.clone(),
                        self.dispatch.clone(),
                        window,
                        cx,
                    );
                }
            }
            Command::SaveLogin(server, password, remember) => {
                self.login(server, password, remember, cx)
            }
            Command::Devices(id) | Command::Networks(id) => {
                if self.service.read(cx).is_logged_in(&id)
                    && let Some(server) = self.server(&id, cx)
                {
                    super::catalog_dialog::open(
                        server,
                        networks,
                        self.service.clone(),
                        self.dispatch.clone(),
                        window,
                        cx,
                    );
                }
            }
            Command::RefreshServer(id) => self
                .service
                .update(cx, |service, cx| service.run(Operation::Refresh(id), cx)),
            Command::Diagnose(id, devices) => {
                if self
                    .service
                    .read(cx)
                    .connection()
                    .is_some_and(|active| active.server == id)
                {
                    if !window.has_active_dialog(cx)
                        && let Some(server) = self.server(&id, cx)
                    {
                        super::catalog_dialog::open(
                            server,
                            false,
                            self.service.clone(),
                            self.dispatch.clone(),
                            window,
                            cx,
                        );
                    }
                    self.service.update(cx, |service, cx| {
                        service.run(Operation::Diagnose(id, devices), cx)
                    });
                }
            }
            Command::ConnectServer(id, network) => {
                if self
                    .service
                    .read(cx)
                    .catalogs
                    .get(&id)
                    .is_some_and(|catalog| {
                        catalog
                            .networks
                            .iter()
                            .any(|item| item.network_id == network && item.enabled)
                    })
                {
                    self.service.update(cx, |service, cx| {
                        service.run(Operation::Connect(id, network), cx)
                    });
                }
            }
            Command::DisconnectServer(id) => {
                if self
                    .service
                    .read(cx)
                    .connection
                    .as_ref()
                    .is_some_and(|active| active.server == id)
                {
                    self.service
                        .update(cx, |service, cx| service.run(Operation::Disconnect, cx));
                }
            }
            Command::LogoutServer(id) => self
                .service
                .update(cx, |service, cx| service.run(Operation::Logout(id), cx)),
            Command::Delete(id) => {
                if let Some(server) = self.server(&id, cx) {
                    let dispatch = self.dispatch.clone();
                    confirm_delete(
                        &server.name,
                        Some(t!("remote.delete_help")),
                        Rc::new(move |window, cx| {
                            dispatch.dispatch_action(
                                &RemoteAction(Command::ConfirmDelete(id.clone())),
                                window,
                                cx,
                            );
                        }),
                        window,
                        cx,
                    );
                }
            }
            Command::ConfirmDelete(id) => {
                if self.server(&id, cx).is_some() {
                    self.service
                        .update(cx, |service, cx| service.run(Operation::Forget(id), cx));
                }
            }
            Command::Select(_) | Command::Ssh { .. } => {}
        }
        cx.notify();
    }

    fn login(
        &mut self,
        mut server: Server,
        password: Secret,
        remember: bool,
        cx: &mut Context<Self>,
    ) {
        if server.validate().is_err() {
            return;
        }
        server.url = super::api::server_url(&server.url).unwrap();
        self.selected = Some(server.id.clone());
        self.settings.update(cx, |store, cx| {
            store.update(
                |settings| {
                    if let Some(saved) = settings
                        .remote_servers
                        .iter_mut()
                        .find(|saved| saved.id == server.id)
                    {
                        *saved = server.clone();
                    } else if settings.remote_servers.len() < 64 {
                        settings.remote_servers.push(server.clone());
                    }
                },
                cx,
            )
        });
        self.service.update(cx, |service, cx| {
            service.run(Operation::RememberLogin(server, password, remember), cx)
        });
    }
}

impl Focusable for RemotePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for RemotePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let service = self.service.read(cx);
        let busy = service.busy;
        let query = self.search.read(cx).value().trim().to_lowercase();
        let servers = self.settings.read(cx).settings().remote_servers;
        let mut rows = v_flex().gap_1().px_1();
        let mut count = 0;
        for server in &servers {
            if ![&server.name, &server.username, &server.url]
                .iter()
                .any(|value| value.to_lowercase().contains(&query))
            {
                continue;
            }
            count += 1;
            let logged_in = service.is_logged_in(&server.id);
            let active = service
                .connection
                .as_ref()
                .filter(|active| active.server == server.id);
            let label = if service.pending_server.as_ref() == Some(&server.id) {
                t!("remote.working")
            } else if active.is_some() {
                t!("remote.connected")
            } else if logged_in {
                t!("remote.signed_in")
            } else {
                t!("remote.signed_out")
            };
            let id = server.id.clone();
            let dispatch = self.dispatch.clone();
            let menu_id = id.clone();
            let connected = active.is_some();
            let status = service.statuses.get(&id).cloned();
            rows = rows.child(
                ListItem::new(SharedString::from(format!("remote-server-row-{id}")))
                    .selected(self.selected.as_ref() == Some(&id))
                    .on_click(move |event, window, cx| {
                        dispatch.dispatch_action(
                            &RemoteAction(if event.click_count() == 2 {
                                Command::Activate(id.clone())
                            } else {
                                Command::Select(id.clone())
                            }),
                            window,
                            cx,
                        )
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .min_w_0()
                            .child(Icon::new(CatalogIcon::Server).small().text_color(
                                if connected {
                                    cx.theme().success
                                } else {
                                    cx.theme().muted_foreground
                                },
                            ))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .child(div().text_sm().truncate().child(server.name.clone()))
                                    .child(
                                        div()
                                            .text_xs()
                                            .truncate()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(server.username.clone()),
                                    )
                                    .when_some(active, |view, active| {
                                        view.child(
                                            div()
                                                .text_xs()
                                                .truncate()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(active.name.clone()),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(if connected {
                                        cx.theme().success
                                    } else {
                                        cx.theme().muted_foreground
                                    })
                                    .child(label),
                            ),
                    )
                    .context_menu({
                        let target = self.dispatch.clone();
                        move |menu, _, _| {
                            server_menu(
                                menu.action_context(target.clone()),
                                &menu_id,
                                logged_in,
                                connected,
                                busy,
                            )
                        }
                    }),
            );
            if let Some(active) = active {
                rows = rows.child(div().px_3().child(super::diagnostic_view::current_paths(
                    format!("remote-sidebar-paths-{}", server.id),
                    active,
                    service.paths(&server.id),
                    cx,
                )));
            }
            if let Some(status) = status.filter(|status| status.failed || status.progress.is_some())
            {
                let progress = status.progress.is_some();
                rows = rows.child(
                    div()
                        .id(SharedString::from(format!("remote-status-{}", server.id)))
                        .test_support()
                        .role(Role::Status)
                        .aria_label(status.message.clone())
                        .px_3()
                        .text_sm()
                        .text_color(if status.failed {
                            cx.theme().danger
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .when(progress, |row| {
                                    row.child(
                                        Spinner::new().xsmall().color(cx.theme().muted_foreground),
                                    )
                                })
                                .child(status.message),
                        ),
                );
            }
        }
        if servers.is_empty() {
            rows = rows.child(
                div()
                    .p_3()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("remote.intro")),
            );
        } else if count == 0 {
            rows = rows.child(
                div()
                    .p_3()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("remote.no_matches")),
            );
        }
        v_flex()
            .id("remote-management")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(
                div().p_2().child(
                    Input::new(&self.search)
                        .id("remote-search")
                        .small()
                        .cleanable(true)
                        .prefix(Icon::new(IconName::Search).small()),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_y_scrollbar()
                    .child(rows),
            )
    }
}

fn server_menu(
    mut menu: PopupMenu,
    id: &str,
    logged_in: bool,
    connected: bool,
    busy: bool,
) -> PopupMenu {
    let item = |label: SharedString, command: Command, disabled: bool| {
        PopupMenuItem::new(label)
            .action(Box::new(RemoteAction(command)))
            .disabled(disabled)
    };
    if logged_in {
        menu = menu
            .item(item(
                t!("remote.show_devices"),
                Command::Devices(id.into()),
                busy,
            ))
            .item(item(
                t!("remote.choose_network"),
                Command::Networks(id.into()),
                busy,
            ))
            .item(item(
                t!("remote.disconnect"),
                Command::DisconnectServer(id.into()),
                busy || !connected,
            ))
            .item(item(
                t!("remote.refresh"),
                Command::RefreshServer(id.into()),
                busy,
            ))
            .item(item(
                t!("remote.diagnostic.check_network"),
                Command::Diagnose(id.into(), vec![]),
                busy || !connected,
            ))
            .item(item(
                t!("remote.logout"),
                Command::LogoutServer(id.into()),
                busy,
            ));
    } else {
        menu = menu.item(item(t!("remote.login"), Command::SignIn(id.into()), busy));
    }
    menu.separator()
        .item(item(
            t!("remote.edit_login"),
            Command::Edit(id.into()),
            busy,
        ))
        .item(item(t!("common.delete"), Command::Delete(id.into()), busy))
}

pub(super) fn describe(code: &str) -> SharedString {
    let key = match code {
        "http_401" | "login_required" => "remote.error.auth",
        "http_403" | "authorization_revoked" => "remote.error.denied",
        "http_409" | "client_in_use" | "network_in_use" => "remote.error.in_use",
        "client_address_pool_exhausted" => "remote.error.pool",
        "client_acl_not_configured" => "remote.error.acl",
        "network_disabled" | "no_authorized_devices" => "remote.no_networks",
        "missing_backend" | "backend_hash_mismatch" => "remote.error.backend",
        "route_conflict" => "remote.error.route",
        "network_launch_cancelled" | "network_cancelled" => "remote.error.cancelled",
        "network_stop_timeout" => "remote.error.stop",
        "lease_expired" | "profile_changed_reconnect" => "remote.error.expired",
        "authorization_failed" => "remote.error.authorization",
        "renewal_failed" => "remote.error.renewal",
        "core_start_failed" | "core_exited" => "remote.error.core",
        "interface_timeout" | "network_start_timeout" => "remote.error.interface",
        "rpc_in_use" | "adapter_in_use" => "remote.error.in_use",
        "invalid_profile" | "profile_hash_mismatch" | "invalid_lease" => "remote.error.profile",
        "unsupported_platform" => "remote.error.platform",
        "invalid_server" | "https_required" => "remote.invalid_server",
        "invalid_device_response" => "remote.error.devices",
        "invalid_network_response" => "remote.error.networks",
        "invalid_response" => "remote.error.response",
        "server_unreachable" => "remote.error.unreachable",
        "credential_read" => "remote.error.credential_read",
        "credential_save" => "remote.error.credential_save",
        "credential_delete" => "remote.error.credential_delete",
        "device_unavailable" => "remote.device_unavailable",
        "diagnostic_failed" => "remote.diagnostic.failed",
        "diagnostic_too_many" => "remote.diagnostic.too_many",
        "http_404" => "remote.error.api_version",
        _ => "remote.error.failed",
    };
    t!(key)
}
