use crate::support::*;
use shellrs::{
    app::RemoteAction,
    cli::Secret,
    remote::{
        Backend, Catalog, Command, Connected, Device, Network, NetworkProgress, NetworkStage,
        Operation, Reply, Server, Target,
    },
    secrets::{InMemorySecretStore, SecretRef, SecretStore},
};
use std::sync::atomic::AtomicBool;

#[gpui_kit::test]
async fn remote_login_edit_prefills_saved_password_and_opt_out_removes_it(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (handle, workspace) =
        open_workspace_with_store(cx, HostStore::empty().with_secrets(secrets.clone()));
    cx.simulate_window_resize(handle.into(), size(px(1000.), px(750.)));
    let backend = Arc::new(FakeRemote::default());
    let server = Server::new(
        "Test".into(),
        "https://example.test".into(),
        "operator".into(),
    );
    cx.update(|cx| {
        workspace
            .read(cx)
            .remote_management()
            .clone()
            .update(cx, |service, cx| service.set_backend(backend.clone(), cx))
    });
    in_frame(cx, handle, |window, cx| {
        window.click("show-remote-management", cx)
    });
    dispatch(
        cx,
        handle,
        Command::SaveLogin(server.clone(), Secret::new("test password"), true),
    );
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        workspace
            .read(cx)
            .remote_management()
            .read(cx)
            .is_logged_in(&server.id)
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click(format!("remote-server-row-{}", server.id), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(
            window.try_find("remote-password").is_none()
                && window.try_find("remote-catalog-dialog").is_none()
        )
    });
    dispatch(cx, handle, Command::Edit(server.id.clone()));
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window.try_find("remote-loading-password").is_none()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("remote-password").value(), None);
        window.click("remote-remember-password", cx);
    });
    in_frame(cx, handle, |window, cx| window.click("remote-login", cx));
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        !workspace.read(cx).remote_management().read(cx).is_busy()
            && secrets
                .get(&SecretRef::credential(format!(
                    "remote-management:{}",
                    server.id
                )))
                .unwrap()
                .is_none()
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("remote-password").is_none())
    });
}

#[gpui_kit::test]
fn remote_login_without_a_keychain_shows_validation_and_disables_saving(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace_with_store(cx, HostStore::empty());
    cx.simulate_window_resize(handle.into(), size(px(1000.), px(750.)));
    in_frame(cx, handle, |window, cx| {
        window.click("show-remote-management", cx)
    });
    in_frame(cx, handle, |window, cx| window.click("remote-add", cx));
    in_frame(cx, handle, |window, cx| {
        assert!(!enabled(&window.find("remote-remember-password")));
        assert_eq!(
            window.find("remote-remember-password").checked(),
            Some(false)
        );
        assert_eq!(window.find("remote-server-url").value(), Some(""));
        window.click("remote-login", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("remote-form-error").visible());
        assert!(window.find("remote-login").bounds().bottom() <= px(750.));
        window.press("escape", cx);
    });
}

#[derive(Default)]
struct FakeRemote {
    active: Mutex<Option<Connected>>,
    calls: Mutex<Vec<String>>,
    fail: AtomicBool,
    fail_login: AtomicBool,
    fail_logout: AtomicBool,
    moved: AtomicBool,
    offline: AtomicBool,
    slow_connect: AtomicBool,
    connect_progress: Mutex<Option<NetworkProgress>>,
}

impl Backend for FakeRemote {
    fn perform(&self, operation: Operation) -> Reply {
        let mut reply = Reply::default();
        match operation {
            Operation::Login(server, password) => {
                assert_eq!(password.expose(), "test password");
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("login:{}", server.id));
                if self.fail_login.load(Ordering::SeqCst) {
                    reply.error = Some("http_401".into());
                    reply.logged_out = Some(server.id);
                } else {
                    let mut catalog = Catalog::default();
                    catalog.name = "测试客户".into();
                    for (id, name) in [("one", "客户一"), ("two", "客户二")] {
                        let mut network = Network::default();
                        network.network_id = id.into();
                        network.name = name.into();
                        network.enabled = true;
                        network.subnet = "10.215.0.0/24".into();
                        network.device_count = 25;
                        catalog.networks.push(network);
                    }
                    for index in 0..25 {
                        let mut device = Device::default();
                        device.device_id = format!("board-{index:02}");
                        device.display_name = format!("开发板-{index:02}");
                        device.customer_name = "测试客户".into();
                        device.virtual_ip =
                            Some(format!("10.215.0.{}", index + 11).parse().unwrap());
                        device.network_id = if index == 0 { "one" } else { "two" }.into();
                        device.enabled = true;
                        device.expires_at = 4102444800000;
                        catalog.devices.push(device);
                    }
                    reply.catalog = Some((server.id, catalog));
                }
            }
            Operation::Connect(server, network) => {
                let mut active = self.active.lock().unwrap();
                if let Some(old) = active.take() {
                    self.calls
                        .lock()
                        .unwrap()
                        .push(format!("stop:{}", old.network));
                }
                if self.fail.load(Ordering::SeqCst) {
                    reply.error = Some("network_launch_cancelled".into());
                } else {
                    if self.slow_connect.load(Ordering::SeqCst) {
                        *self.connect_progress.lock().unwrap() =
                            Some(NetworkProgress::new(NetworkStage::StartingCore));
                        let deadline = std::time::Instant::now() + Duration::from_secs(10);
                        while self.slow_connect.load(Ordering::SeqCst)
                            && std::time::Instant::now() < deadline
                        {
                            std::thread::yield_now();
                        }
                    }
                    self.calls.lock().unwrap().push(format!("start:{network}"));
                    *active = Some(Connected::new(
                        server,
                        network.clone(),
                        network,
                        "10.215.0.2".parse().unwrap(),
                        vec![Target::new(
                            "board-00".into(),
                            "开发板-00".into(),
                            "10.215.0.11".parse().unwrap(),
                        )],
                    ));
                    *self.connect_progress.lock().unwrap() = None;
                }
            }
            Operation::Logout(id) => {
                if self.fail_logout.load(Ordering::SeqCst) {
                    reply.error = Some("network_stop_timeout".into());
                } else {
                    let mut active = self.active.lock().unwrap();
                    if active.as_ref().is_some_and(|active| active.server == id) {
                        *active = None;
                    }
                    self.calls.lock().unwrap().push(format!("logout:{id}"));
                    reply.logged_out = Some(id);
                }
            }
            Operation::Refresh(server) => {
                let mut active = self.active.lock().unwrap();
                if let Some(active) = active.as_mut().filter(|active| active.server == server) {
                    let mut catalog = Catalog::default();
                    if self.offline.load(Ordering::SeqCst) {
                        active.devices.clear();
                    } else {
                        let ip = if self.moved.load(Ordering::SeqCst) {
                            "10.215.0.99"
                        } else {
                            "10.215.0.11"
                        };
                        let mut device = Device::default();
                        device.device_id = "board-00".into();
                        device.display_name = "Renamed".into();
                        device.virtual_ip = Some(ip.parse().unwrap());
                        device.network_id = active.network.clone();
                        device.enabled = true;
                        device.expires_at = 4102444800000;
                        catalog.devices.push(device);
                        active.devices = vec![Target::new(
                            "board-00".into(),
                            "Renamed".into(),
                            ip.parse().unwrap(),
                        )];
                    }
                    reply.catalog = Some((server, catalog));
                }
            }
            Operation::Resolve(server, device, user, terminal) => {
                self.calls.lock().unwrap().push(format!("resolve:{device}"));
                let active = self.active.lock().unwrap();
                if let Some(active) = active.as_ref().filter(|active| active.server == server)
                    && let Some(target) = active
                        .devices
                        .iter()
                        .find(|target| target.device_id == device)
                {
                    reply.resolved = Some(shellrs::remote::ResolvedSsh {
                        alias: shellrs::remote::device_alias(
                            "https://example.test",
                            &active.network,
                            &device,
                        ),
                        server,
                        network: active.network.clone(),
                        device,
                        user,
                        terminal,
                        address: target.virtual_ip.to_string(),
                        name: target.display_name.clone(),
                    });
                } else {
                    reply.error = Some("device_unavailable".into());
                }
            }
            Operation::Disconnect | Operation::Shutdown => *self.active.lock().unwrap() = None,
            Operation::Poll => {}
            _ => panic!("credential operations must not reach the network backend"),
        }
        reply.connection = self.active.lock().unwrap().clone();
        reply
    }

    fn progress(&self) -> Option<NetworkProgress> {
        self.connect_progress.lock().unwrap().clone()
    }
}

fn dispatch(cx: &mut TestAppContext, handle: WindowHandle<Root>, command: Command) {
    in_frame(cx, handle, |window, cx| {
        window.click("remote-search", cx);
        window.dispatch_action(Box::new(RemoteAction(command)), cx);
    });
}

#[gpui_kit::test]
async fn remote_device_metadata_changes_preserve_network_but_address_reuse_disconnects_old_ssh(
    cx: &mut TestAppContext,
) {
    let (handle, workspace) = open_workspace_with_store(cx, HostStore::empty());
    let backend = Arc::new(FakeRemote::default());
    let server = Server::new(
        "Test".into(),
        "https://example.test".into(),
        "operator".into(),
    );
    cx.update(|cx| {
        workspace
            .read(cx)
            .remote_management()
            .clone()
            .update(cx, |service, cx| service.set_backend(backend.clone(), cx))
    });
    in_frame(cx, handle, |window, cx| {
        window.click("show-remote-management", cx)
    });
    dispatch(
        cx,
        handle,
        Command::SaveLogin(server.clone(), Secret::new("test password"), false),
    );
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        workspace
            .read(cx)
            .remote_management()
            .read(cx)
            .is_logged_in(&server.id)
    })
    .await;
    dispatch(
        cx,
        handle,
        Command::ConnectServer(server.id.clone(), "one".into()),
    );
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        workspace
            .read(cx)
            .remote_management()
            .read(cx)
            .connection()
            .is_some()
    })
    .await;
    let connect = || Command::Ssh {
        server: server.id.clone(),
        device: "board-00".into(),
        user: "root".into(),
    };
    dispatch(cx, handle, connect());
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        remote_lifecycle(&workspace, RemoteTerminalId(1), cx) == TerminalLifecycle::Running
    })
    .await;
    dispatch(cx, handle, Command::RefreshServer(server.id.clone()));
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        !workspace.read(cx).remote_management().read(cx).is_busy()
    })
    .await;
    cx.update(|cx| {
        assert_eq!(
            remote_lifecycle(&workspace, RemoteTerminalId(1), cx),
            TerminalLifecycle::Running
        )
    });
    backend.moved.store(true, Ordering::SeqCst);
    dispatch(cx, handle, Command::RefreshServer(server.id.clone()));
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        remote_lifecycle(&workspace, RemoteTerminalId(1), cx) != TerminalLifecycle::Running
    })
    .await;
    cx.update(|cx| {
        assert!(
            workspace
                .read(cx)
                .remote_management()
                .read(cx)
                .connection()
                .is_some()
        )
    });
    dispatch(cx, handle, connect());
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        remote_lifecycle(&workspace, RemoteTerminalId(2), cx) == TerminalLifecycle::Running
    })
    .await;
    cx.update(|cx| {
        let workspace = workspace.read(cx);
        let first = workspace
            .remote_terminal(RemoteTerminalId(1))
            .unwrap()
            .read(cx);
        let second = workspace
            .remote_terminal(RemoteTerminalId(2))
            .unwrap()
            .read(cx);
        assert_eq!(second.host().address.as_ref(), "10.215.0.99");
        let store = workspace.store().read(cx);
        assert_eq!(
            store.login_of(first.host()).host_key_alias,
            store.login_of(second.host()).host_key_alias
        );
    });
    backend.offline.store(true, Ordering::SeqCst);
    dispatch(cx, handle, Command::RefreshServer(server.id.clone()));
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        remote_lifecycle(&workspace, RemoteTerminalId(2), cx) != TerminalLifecycle::Running
    })
    .await;
    cx.update(|cx| {
        let service = workspace.read(cx).remote_management().read(cx);
        assert!(service.connection().is_some());
        assert!(service.ssh_target(&server.id, "board-00").is_none());
    });
}

#[gpui_kit::test]
async fn remote_sidebar_is_server_only_and_dialogs_search_page_filter_and_gate_ssh(
    cx: &mut TestAppContext,
) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (handle, workspace) =
        open_workspace_with_store(cx, HostStore::empty().with_secrets(secrets.clone()));
    cx.simulate_window_resize(handle.into(), size(px(1150.), px(820.)));
    let backend = Arc::new(FakeRemote::default());
    cx.update(|cx| {
        workspace
            .read(cx)
            .remote_management()
            .clone()
            .update(cx, |service, cx| service.set_backend(backend.clone(), cx))
    });
    in_frame(cx, handle, |window, cx| {
        window.click("show-remote-management", cx)
    });
    in_frame(cx, handle, |window, cx| window.click("remote-add", cx));
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("remote-server-url").value(), Some(""));
        assert_eq!(
            window.find("remote-remember-password").checked(),
            Some(true)
        );
        let cancel = window
            .within("gpui_component::dialog::footer::DialogFooter")
            .find("close")
            .bounds();
        let login = window.find("remote-login").bounds();
        assert!((cancel.size.width - login.size.width).abs() <= px(1.));
        window.click("remote-server-name", cx);
        window.input("测试服务器", cx);
        window.click("remote-server-url", cx);
        window.input("https://example.test", cx);
        window.click("remote-username", cx);
        window.input("operator", cx);
        window.click("remote-password", cx);
        window.input("test password", cx);
        assert_eq!(window.find("remote-password").value(), None);
    });
    in_frame(cx, handle, |window, cx| window.click("remote-login", cx));
    let server = cx.update(|cx| {
        workspace
            .read(cx)
            .settings()
            .read(cx)
            .settings()
            .remote_servers[0]
            .clone()
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        workspace
            .read(cx)
            .remote_management()
            .read(cx)
            .is_logged_in(&server.id)
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("remote-device-count").is_none());
        assert!(window.try_find("remote-ssh-user").is_none());
        assert!(window.try_find("remote-connect-one").is_none());
    });
    assert!(
        secrets
            .get(&SecretRef::credential(format!(
                "remote-management:{}",
                server.id
            )))
            .unwrap()
            .is_some()
    );
    dispatch(cx, handle, Command::Devices(server.id.clone()));
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("remote-device-count").label(),
            Some("25 台设备")
        );
        assert!(!enabled(&window.find("remote-ssh-board-00")));
        let dialog = window.find("remote-catalog-dialog").bounds();
        assert!(dialog.size.width > px(700.));
        assert!(dialog.bottom() < px(820.));
        window.click("remote-page-next", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("remote-device-board-24").is_some());
        window.click("remote-catalog-search", cx);
        window.input("BOARD-00", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("remote-device-board-00").is_some());
        assert!(window.try_find("remote-device-board-24").is_none());
        window.press("escape", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("remote-choose-network", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("remote-connect-one", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        workspace
            .read(cx)
            .remote_management()
            .read(cx)
            .connection()
            .is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| window.press("escape", cx));
    in_frame(cx, handle, |window, cx| window.press("escape", cx));
    in_frame(cx, handle, |window, cx| {
        window.double_click(format!("remote-server-row-{}", server.id), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("remote-current-only", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("remote-device-count").label(), Some("1 台设备"));
        assert!(enabled(&window.find("remote-ssh-board-00")));
        window.click("remote-ssh-board-00", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        remote_lifecycle(&workspace, RemoteTerminalId(1), cx) == TerminalLifecycle::Running
    })
    .await;
    dispatch(
        cx,
        handle,
        Command::ConnectServer(server.id.clone(), "two".into()),
    );
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        workspace
            .read(cx)
            .remote_management()
            .read(cx)
            .connection()
            .is_some_and(|active| active.network == "two")
    })
    .await;
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        remote_lifecycle(&workspace, RemoteTerminalId(1), cx) != TerminalLifecycle::Running
    })
    .await;
    backend.fail.store(true, Ordering::SeqCst);
    dispatch(
        cx,
        handle,
        Command::ConnectServer(server.id.clone(), "one".into()),
    );
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window
            .try_find(format!("remote-status-{}", server.id))
            .is_some()
    })
    .await;
    dispatch(cx, handle, Command::RefreshServer(server.id.clone()));
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, cx| {
        !workspace.read(cx).remote_management().read(cx).is_busy()
            && window
                .try_find(format!("remote-status-{}", server.id))
                .is_none()
    })
    .await;
    backend.fail.store(false, Ordering::SeqCst);
    dispatch(cx, handle, Command::LogoutServer(server.id.clone()));
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        !workspace
            .read(cx)
            .remote_management()
            .read(cx)
            .is_logged_in(&server.id)
    })
    .await;
    dispatch(cx, handle, Command::SignIn(server.id.clone()));
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        workspace
            .read(cx)
            .remote_management()
            .read(cx)
            .is_logged_in(&server.id)
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("remote-password").is_none())
    });
    cx.update(|cx| {
        assert!(
            !serde_json::to_string(&workspace.read(cx).settings().read(cx).settings())
                .unwrap()
                .contains("test password")
        )
    });
}

#[gpui_kit::test]
async fn connecting_network_shows_the_current_stage_and_elapsed_time(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace_with_store(cx, HostStore::empty());
    cx.simulate_window_resize(handle.into(), size(px(1000.), px(750.)));
    let backend = Arc::new(FakeRemote::default());
    let server = Server::new(
        "Test".into(),
        "https://example.test".into(),
        "operator".into(),
    );
    cx.update(|cx| {
        workspace
            .read(cx)
            .remote_management()
            .clone()
            .update(cx, |service, cx| service.set_backend(backend.clone(), cx))
    });
    in_frame(cx, handle, |window, cx| {
        window.click("show-remote-management", cx)
    });
    dispatch(
        cx,
        handle,
        Command::SaveLogin(server.clone(), Secret::new("test password"), false),
    );
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        workspace
            .read(cx)
            .remote_management()
            .read(cx)
            .is_logged_in(&server.id)
    })
    .await;
    dispatch(cx, handle, Command::Networks(server.id.clone()));
    backend.slow_connect.store(true, Ordering::SeqCst);
    in_frame(cx, handle, |window, cx| {
        window.click("remote-connect-one", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window.try_find("remote-status").is_some_and(|status| {
            status
                .label()
                .is_some_and(|label| label.contains("启动组网核心") && label.contains("已用时"))
        })
    })
    .await;
    in_frame(cx, handle, |window, _| {
        let status = window.find("remote-status");
        assert!(status.label().is_some_and(|label| label.contains("已用时")));
        assert!(!enabled(&window.find("remote-connect-one")));
    });
    *backend.connect_progress.lock().unwrap() =
        Some(NetworkProgress::new(NetworkStage::WaitingForInterface));
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window.try_find("remote-status").is_some_and(|status| {
            status
                .label()
                .is_some_and(|label| label.contains("等待虚拟网卡就绪"))
        })
    })
    .await;
    in_frame(cx, handle, |window, cx| window.press("escape", cx));
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .find(format!("remote-status-{}", server.id))
                .label()
                .is_some_and(|label| label.contains("等待虚拟网卡就绪"))
        );
    });
    backend.slow_connect.store(false, Ordering::SeqCst);
    cx.wait_for(handle.into(), Duration::from_secs(4), |_, cx| {
        workspace
            .read(cx)
            .remote_management()
            .read(cx)
            .connection()
            .is_some()
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .try_find(format!("remote-status-{}", server.id))
                .is_none()
        );
    });
}

#[gpui_kit::test]
async fn deleting_server_cleans_password_and_never_stops_another_servers_network(
    cx: &mut TestAppContext,
) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (handle, workspace) =
        open_workspace_with_store(cx, HostStore::empty().with_secrets(secrets.clone()));
    let backend = Arc::new(FakeRemote::default());
    let first = Server::new(
        "First".into(),
        "https://first.test".into(),
        "operator".into(),
    );
    let second = Server::new(
        "Second".into(),
        "https://second.test".into(),
        "operator".into(),
    );
    cx.update(|cx| {
        workspace
            .read(cx)
            .remote_management()
            .clone()
            .update(cx, |service, cx| service.set_backend(backend.clone(), cx));
        workspace
            .read(cx)
            .settings()
            .clone()
            .update(cx, |store, cx| {
                store.update(
                    |settings| settings.remote_servers = vec![first.clone(), second.clone()],
                    cx,
                )
            });
    });
    in_frame(cx, handle, |window, cx| {
        window.click("show-remote-management", cx)
    });
    dispatch(
        cx,
        handle,
        Command::SaveLogin(first.clone(), Secret::new("test password"), true),
    );
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        workspace
            .read(cx)
            .remote_management()
            .read(cx)
            .is_logged_in(&first.id)
    })
    .await;
    dispatch(
        cx,
        handle,
        Command::ConnectServer(first.id.clone(), "one".into()),
    );
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        workspace
            .read(cx)
            .remote_management()
            .read(cx)
            .connection()
            .is_some()
    })
    .await;
    dispatch(cx, handle, Command::SignIn(second.id.clone()));
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window.try_find("remote-password").is_some()
    })
    .await;
    cx.update(|cx| {
        assert!(
            workspace
                .read(cx)
                .remote_management()
                .read(cx)
                .connection()
                .is_some_and(|active| active.server == first.id)
        )
    });
    in_frame(cx, handle, |window, cx| window.press("escape", cx));
    dispatch(cx, handle, Command::DisconnectServer(second.id.clone()));
    dispatch(cx, handle, Command::Delete(second.id.clone()));
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        workspace
            .read(cx)
            .settings()
            .read(cx)
            .settings()
            .remote_servers
            .len()
            == 1
    })
    .await;
    cx.update(|cx| {
        assert!(
            workspace
                .read(cx)
                .remote_management()
                .read(cx)
                .connection()
                .is_some()
        )
    });
    dispatch(cx, handle, Command::Delete(first.id.clone()));
    in_frame(cx, handle, |window, cx| window.press("escape", cx));
    backend.fail_logout.store(true, Ordering::SeqCst);
    dispatch(cx, handle, Command::Delete(first.id.clone()));
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        !workspace.read(cx).remote_management().read(cx).is_busy()
    })
    .await;
    cx.update(|cx| {
        assert_eq!(
            workspace
                .read(cx)
                .settings()
                .read(cx)
                .settings()
                .remote_servers
                .len(),
            1
        )
    });
    assert!(
        secrets
            .get(&SecretRef::credential(format!(
                "remote-management:{}",
                first.id
            )))
            .unwrap()
            .is_some()
    );
    backend.fail_logout.store(false, Ordering::SeqCst);
    dispatch(cx, handle, Command::Delete(first.id.clone()));
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        workspace
            .read(cx)
            .settings()
            .read(cx)
            .settings()
            .remote_servers
            .is_empty()
    })
    .await;
    assert!(
        secrets
            .get(&SecretRef::credential(format!(
                "remote-management:{}",
                first.id
            )))
            .unwrap()
            .is_none()
    );
}

#[gpui_kit::test]
async fn rejected_saved_login_opens_masked_retry_and_does_not_delete_password(
    cx: &mut TestAppContext,
) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (handle, workspace) =
        open_workspace_with_store(cx, HostStore::empty().with_secrets(secrets.clone()));
    let backend = Arc::new(FakeRemote::default());
    let server = Server::new(
        "Test".into(),
        "https://example.test".into(),
        "operator".into(),
    );
    cx.update(|cx| {
        workspace
            .read(cx)
            .remote_management()
            .clone()
            .update(cx, |service, cx| service.set_backend(backend.clone(), cx))
    });
    in_frame(cx, handle, |window, cx| {
        window.click("show-remote-management", cx)
    });
    dispatch(
        cx,
        handle,
        Command::SaveLogin(server.clone(), Secret::new("test password"), true),
    );
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        workspace
            .read(cx)
            .remote_management()
            .read(cx)
            .is_logged_in(&server.id)
    })
    .await;
    dispatch(cx, handle, Command::LogoutServer(server.id.clone()));
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        !workspace
            .read(cx)
            .remote_management()
            .read(cx)
            .is_logged_in(&server.id)
    })
    .await;
    backend.fail_login.store(true, Ordering::SeqCst);
    in_frame(cx, handle, |window, cx| {
        window.double_click(format!("remote-server-row-{}", server.id), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window.try_find("remote-password").is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("remote-password").value(), None);
        assert!(window.find("remote-form-error").visible());
        window.press("escape", cx);
    });
    assert!(
        secrets
            .get(&SecretRef::credential(format!(
                "remote-management:{}",
                server.id
            )))
            .unwrap()
            .is_some()
    );
    backend.fail_login.store(false, Ordering::SeqCst);
    dispatch(
        cx,
        handle,
        Command::SaveLogin(server.clone(), Secret::new("test password"), false),
    );
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        workspace
            .read(cx)
            .remote_management()
            .read(cx)
            .is_logged_in(&server.id)
    })
    .await;
    assert!(
        secrets
            .get(&SecretRef::credential(format!(
                "remote-management:{}",
                server.id
            )))
            .unwrap()
            .is_none()
    );
}
