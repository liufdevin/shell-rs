use crate::support::*;
use shellrs_lan::{Core, Executor, Network, SharedDevice, SharedHost};
use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::AtomicBool;

struct NoCommands;
impl Executor for NoCommands {
    fn hosts(&self) -> Vec<SharedHost> {
        vec![SharedHost::new(
            "shared".into(),
            "test host".into(),
            "10.0.0.1".into(),
            22,
            "root".into(),
        )]
    }
    fn execute(
        &self,
        _: &str,
        _: &str,
        _: &AtomicBool,
        _: &mut dyn FnMut(bool, &[u8]) -> std::io::Result<()>,
    ) -> anyhow::Result<i32> {
        anyhow::bail!("not used")
    }
}

#[gpui_kit::test]
fn settings_search_finds_lan_and_exposes_its_status_to_assistive_technology(
    cx: &mut TestAppContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let core = Core::load(directory.path(), "B", Arc::new(NoCommands)).unwrap();
    let (handle, workspace) = open_workspace(cx);
    cx.update(|cx| {
        let lan = workspace.read(cx).lan().clone();
        lan.update(cx, |lan, cx| lan.attach_core(core, cx));
    });
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    in_frame(cx, handle, |window, cx| {
        let search = gpui_kit::base::test_support::snapshots(window)
            .into_iter()
            .find(|element| {
                element.role() == Some(gpui_kit::Role::TextInput)
                    && element.path().contains(&ElementId::from("settings"))
            })
            .expect("settings search");
        window.click(search.path().last().unwrap().clone(), cx);
        window.input("局域网协作", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("lan-enable").is_some());
        assert_eq!(
            window.find("lan-status").role(),
            Some(gpui_kit::Role::Status)
        );
    });
}

#[gpui_kit::test]
async fn lan_settings_require_a_matching_password_before_enabling(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let core = Core::load(directory.path(), "B", Arc::new(NoCommands)).unwrap();
    let (handle, workspace) = open_workspace(cx);
    cx.update(|cx| {
        let lan = workspace.read(cx).lan().clone();
        lan.update(cx, |lan, cx| lan.attach_core(core.clone(), cx));
    });
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-8", cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("lan-enable").label(), Some("启用局域网协作"));
        assert_eq!(
            window.find("lan-enable").role(),
            Some(gpui_kit::Role::Switch)
        );
        assert_eq!(window.find("lan-enable").checked(), Some(false));
        assert_eq!(
            window.find("lan-save-password").label(),
            Some("设置配对密码")
        );
        assert!(enabled(&window.find("lan-enable")));
        window.click("lan-enable", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(!core.is_running());
        assert!(!core.enabled_on_start());
        assert_eq!(window.find("lan-enable").checked(), Some(false));
        assert_eq!(
            window.find("lan-status").label(),
            Some("请先设置配对密码，再开启局域网协作")
        );
        window.click("lan-password", cx);
        window.input("strong pairing password", cx);
        window.click("lan-password-repeat", cx);
        window.input("wrong", cx);
        assert!(!enabled(&window.find("lan-save-password")));
    });
    in_frame(cx, handle, |window, cx| {
        window.click("lan-password-repeat", cx);
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-a"
            } else {
                "ctrl-a"
            },
            cx,
        );
        window.input("strong pairing password", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("lan-save-password", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(10), |window, _| {
        core.has_password() && enabled(&window.find("lan-enable"))
    })
    .await;
    assert!(!core.is_running());
    let file = std::fs::read_to_string(directory.path().join("access.json")).unwrap();
    assert!(!file.contains("strong pairing password"));
}

#[gpui_kit::test]
async fn discoverability_is_an_independent_persisted_switch_and_starts_off(
    cx: &mut TestAppContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let core = Core::load(directory.path(), "B", Arc::new(NoCommands)).unwrap();
    let (handle, workspace) = open_workspace(cx);
    cx.update(|cx| {
        let lan = workspace.read(cx).lan().clone();
        lan.update(cx, |lan, cx| lan.attach_core(core.clone(), cx));
    });
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-8", cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("lan-enable").checked(), Some(false));
        assert_eq!(window.find("lan-discoverable").checked(), Some(false));
        assert_eq!(
            window.find("lan-discoverable").role(),
            Some(gpui_kit::Role::Switch)
        );
        window.click("lan-discoverable", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        core.is_discoverable()
            && window.find("lan-discoverable").checked() == Some(true)
            && enabled(&window.find("lan-discoverable"))
    })
    .await;
    assert!(!core.is_running());
    assert!(!core.enabled_on_start());
    let reloaded = Core::load(directory.path(), "B", Arc::new(NoCommands)).unwrap();
    assert!(reloaded.is_discoverable());
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("lan-discoverable").focused(), Some(true));
        window.press("space", cx);
        window.dispatch_event(
            gpui_kit::PlatformInput::KeyUp(gpui_kit::KeyUpEvent {
                keystroke: gpui_kit::Keystroke::parse("space").unwrap(),
            }),
            cx,
        );
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        !core.is_discoverable() && window.find("lan-discoverable").checked() == Some(false)
    })
    .await;
    let reloaded = Core::load(directory.path(), "B", Arc::new(NoCommands)).unwrap();
    assert!(!reloaded.is_discoverable());
}

#[gpui_kit::test]
async fn incoming_pair_can_be_rejected_blacklisted_unblocked_and_then_accepted(
    cx: &mut TestAppContext,
) {
    let a_directory = tempfile::tempdir().unwrap();
    let b_directory = tempfile::tempdir().unwrap();
    let a = Core::load(a_directory.path(), "A", Arc::new(NoCommands)).unwrap();
    let b = Core::load(b_directory.path(), "B", Arc::new(NoCommands)).unwrap();
    a.set_password("strong pairing password").unwrap();
    b.set_password("strong pairing password").unwrap();
    b.set_sharing(BTreeSet::from(["shared".into()]), false)
        .unwrap();
    let _a_network = Network::start(a.clone(), Ipv4Addr::LOCALHOST, 0, 0).unwrap();
    let b_network = Network::start(b.clone(), Ipv4Addr::LOCALHOST, 0, 0).unwrap();
    let (handle, workspace) = open_workspace(cx);
    cx.update(|cx| {
        let lan = workspace.read(cx).lan().clone();
        lan.update(cx, |lan, cx| lan.attach_core(b.clone(), cx));
    });
    let device = a.inspect(b_network.tcp_address).unwrap();
    a.request_pair(&device).unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(4), |window, _| {
        window.try_find("lan-block-on-reject").is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click("lan-block-on-reject", cx);
        window.press("escape", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(4), |window, _| {
        b.is_blocked(IpAddr::V4(Ipv4Addr::LOCALHOST))
            && window.try_find("lan-block-on-reject").is_none()
    })
    .await;
    assert!(
        a.request_pair(&device)
            .unwrap_err()
            .to_string()
            .contains("blocked_by_device")
    );
    assert!(b.pending().is_empty());
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-8", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.scroll(
            "lan-enable",
            gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-10000.))),
            cx,
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.click("lan-unblock-127.0.0.1", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(4), |_, _| {
        b.blocked_ips().is_empty()
    })
    .await;
    a.request_pair(&device).unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(4), |window, _| {
        window.try_find("lan-accept-read").is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| window.click("lan-accept-read", cx));
    cx.wait_for(handle.into(), Duration::from_secs(4), |_, _| {
        !b.grants().is_empty()
    })
    .await;
    assert!(!b.grants()[0].execute);
    a.complete_pair(device).unwrap();
}

#[derive(Default)]
struct EchoDevices {
    inputs: Mutex<Vec<shellrs_lan::DeviceInput>>,
}

impl shellrs_lan::DeviceProvider for EchoDevices {
    fn devices(&self) -> Vec<SharedDevice> {
        vec![
            SharedDevice::new(
                "uart".into(),
                "B UART".into(),
                shellrs_lan::DeviceKind::Serial,
                "COM-ONLY-ON-B".into(),
                "9600 8N1".into(),
                "utf-8".into(),
            )
            .with_serial(shellrs_lan::SerialParameters::new(
                9600,
                8,
                "none".into(),
                1,
                "none".into(),
            )),
            SharedDevice::new(
                "android".into(),
                "B Android".into(),
                shellrs_lan::DeviceKind::Adb,
                "android-on-B".into(),
                "ADB shell".into(),
                "utf-8".into(),
            ),
        ]
    }

    fn run(
        &self,
        id: &str,
        _: shellrs_lan::DeviceSize,
        mut commands: tokio::sync::mpsc::Receiver<shellrs_lan::DeviceInput>,
        events: tokio::sync::mpsc::Sender<shellrs_lan::DeviceEvent>,
        cancelled: &AtomicBool,
    ) -> anyhow::Result<()> {
        use std::sync::atomic::Ordering;
        events.blocking_send(shellrs_lan::DeviceEvent::Started)?;
        events.blocking_send(shellrs_lan::DeviceEvent::Output(
            format!("{id}-ready\r\n").into_bytes(),
        ))?;
        while !cancelled.load(Ordering::Acquire) {
            match commands.try_recv() {
                Ok(command) => {
                    self.inputs.lock().unwrap().push(command.clone());
                    if let shellrs_lan::DeviceInput::Write(bytes) = command {
                        events.blocking_send(shellrs_lan::DeviceEvent::Output(bytes))?;
                    }
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
            }
        }
        Ok(())
    }
}

#[gpui_kit::test]
async fn shared_serial_and_adb_open_real_terminal_views_and_revoke_without_local_fallback(
    cx: &mut TestAppContext,
) {
    use shellrs::app::{
        CloseLocalTerminal, CloseTerminal, DuplicateTerminal, LanAction, ReconnectTerminal,
        RestartLocalTerminal, SendBreak,
    };
    use shellrs::lan::LanCommand;
    use shellrs_lan::{DeviceInput, DeviceProvider as _};
    let a_directory = tempfile::tempdir().unwrap();
    let b_directory = tempfile::tempdir().unwrap();
    let a = Core::load(a_directory.path(), "A", Arc::new(NoCommands)).unwrap();
    let b = Core::load(b_directory.path(), "B", Arc::new(NoCommands)).unwrap();
    let devices = Arc::new(EchoDevices::default());
    b.set_device_provider(devices.clone()).unwrap();
    a.set_password("strong pairing password").unwrap();
    b.set_password("strong pairing password").unwrap();
    b.set_device_sharing(
        devices
            .devices()
            .iter()
            .map(|device| device.id.clone())
            .collect(),
        true,
    )
    .unwrap();
    let _a_network = Network::start(a.clone(), Ipv4Addr::LOCALHOST, 0, 0).unwrap();
    let b_network = Network::start(b.clone(), Ipv4Addr::LOCALHOST, 0, 0).unwrap();
    let target = a.inspect(b_network.tcp_address).unwrap();
    a.pair_password(target, "strong pairing password").unwrap();
    let local_serial = Arc::new(FixedSerialTerminalTransportProvider::new(Arc::new(
        FakeTerminalFactory::default(),
    )));
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        local_serial.clone(),
    );
    cx.update(|cx| {
        let service = workspace.read(cx).lan().clone();
        service.update(cx, |service, cx| service.attach_core(a.clone(), cx));
    });
    in_frame(cx, handle, |window, cx| {
        window.click("host-search", cx);
        window.dispatch_action(
            Box::new(LanAction(LanCommand::RefreshDevices(b.id().to_owned()))),
            cx,
        );
    });
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-8", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(8), |window, _| {
        window.try_find("lan-open-device-uart").is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("lan-adb-tools-android").is_some());
        assert!(window.try_find("lan-adb-files-android").is_some());
        assert!(window.try_find("lan-adb-tools-uart").is_none());
        assert!(window.try_find("lan-adb-files-uart").is_none());
        window.click("host-search", cx);
        window.dispatch_action(
            Box::new(LanAction(LanCommand::OpenDevice {
                peer: b.id().to_owned(),
                device: "uart".into(),
            })),
            cx,
        );
    });
    cx.wait_for(handle.into(), Duration::from_secs(8), |_, cx| {
        workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(1))
            .is_some_and(|panel| {
                matches!(panel.read(cx).lifecycle(cx), TerminalLifecycle::Running)
                    && panel
                        .read(cx)
                        .terminal()
                        .read(cx)
                        .screen_text(cx)
                        .contains("uart-ready")
            })
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1u64), cx);
        window.input("serial-input", cx);
        window.dispatch_action(Box::new(SendBreak(RemoteTerminalId(1))), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(8), |_, cx| {
        workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(1))
            .unwrap()
            .read(cx)
            .terminal()
            .read(cx)
            .screen_text(cx)
            .contains("serial-input")
            && devices.inputs.lock().unwrap().contains(&DeviceInput::Break)
    })
    .await;
    assert!(local_serial.lines().is_empty());
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert!(store.hosts().is_empty());
        assert!(
            store.temporary_hosts()[0]
                .address
                .starts_with("lan-device:")
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(DuplicateTerminal(RemoteTerminalId(1))), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(8), |_, cx| {
        workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(2))
            .is_some_and(|panel| {
                matches!(panel.read(cx).lifecycle(cx), TerminalLifecycle::Failed(_))
            })
    })
    .await;
    assert!(local_serial.lines().is_empty());
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(CloseTerminal(RemoteTerminalId(2))), cx)
    });
    b.set_device_permission(a.id(), "uart", false).unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(8), |_, cx| {
        matches!(
            workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(1))
                .unwrap()
                .read(cx)
                .lifecycle(cx),
            TerminalLifecycle::Failed(_)
        )
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1u64), cx);
        window.dispatch_action(Box::new(ReconnectTerminal(RemoteTerminalId(1))), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(8), |_, cx| {
        matches!(
            workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(1))
                .unwrap()
                .read(cx)
                .lifecycle(cx),
            TerminalLifecycle::Failed(_)
        )
    })
    .await;
    assert!(local_serial.lines().is_empty());
    in_frame(cx, handle, |window, cx| {
        window.click("host-search", cx);
        window.dispatch_action(
            Box::new(LanAction(LanCommand::OpenDevice {
                peer: b.id().to_owned(),
                device: "android".into(),
            })),
            cx,
        );
    });
    cx.wait_for(handle.into(), Duration::from_secs(8), |_, cx| {
        workspace
            .read(cx)
            .local_terminal(LocalTerminalId(1))
            .is_some_and(|panel| {
                matches!(
                    panel.read(cx).terminal().read(cx).lifecycle(cx),
                    TerminalLifecycle::Running
                ) && panel
                    .read(cx)
                    .terminal()
                    .read(cx)
                    .screen_text(cx)
                    .contains("android-ready")
            })
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click(("local-terminal", 1u64), cx);
        window.input("adb-input", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(8), |_, cx| {
        workspace
            .read(cx)
            .local_terminal(LocalTerminalId(1))
            .unwrap()
            .read(cx)
            .terminal()
            .read(cx)
            .screen_text(cx)
            .contains("adb-input")
    })
    .await;
    b.revoke_device_access(a.id()).unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(8), |_, cx| {
        matches!(
            workspace
                .read(cx)
                .local_terminal(LocalTerminalId(1))
                .unwrap()
                .read(cx)
                .terminal()
                .read(cx)
                .lifecycle(cx),
            TerminalLifecycle::Failed(_)
        )
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click(("local-terminal", 1u64), cx);
        window.dispatch_action(Box::new(RestartLocalTerminal(LocalTerminalId(1))), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(8), |_, cx| {
        matches!(
            workspace
                .read(cx)
                .local_terminal(LocalTerminalId(1))
                .unwrap()
                .read(cx)
                .terminal()
                .read(cx)
                .lifecycle(cx),
            TerminalLifecycle::Failed(_)
        )
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click("host-search", cx);
        window.dispatch_action(Box::new(CloseLocalTerminal(LocalTerminalId(1))), cx);
        window.dispatch_action(Box::new(CloseTerminal(RemoteTerminalId(1))), cx);
    });
    cx.update(|cx| {
        assert!(
            workspace
                .read(cx)
                .store()
                .read(cx)
                .temporary_hosts()
                .is_empty()
        )
    });
}
