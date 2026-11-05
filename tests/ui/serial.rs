//! Serial hosts: the host form's 串口, their terminals on a serial port of
//! this computer, and what an SSH host has that they have not.

use crate::support::*;
use shellrs::app::{CloseTerminal, SendBreak, StartTerminalRecording, StopTerminalRecording};
use shellrs::host::{FlowControl, Protocol, SerialSettings};

/// A store with one serial host, `console` on `/dev/ttyUSB0`.
fn serial_store() -> (HostStore, HostId) {
    let mut store = HostStore::empty();
    let id = store.insert_unnotified(HostDraft::serial(
        "console",
        "/dev/ttyUSB0",
        SerialSettings::default(),
        None,
    ));
    (store, id)
}

async fn connect(cx: &mut TestAppContext, handle: WindowHandle<Root>, id: HostId) {
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(id)), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window
            .try_find("status-connection")
            .is_some_and(|status| status.label() == Some("已连接 console"))
    })
    .await;
}

fn replace_text(window: &mut gpui_kit::Window, id: &'static str, text: &str, cx: &mut App) {
    window.click(id, cx);
    window.press("secondary-a", cx);
    window.input(text, cx);
}

#[gpui_kit::test]
async fn a_serial_host_is_made_from_its_device_and_line(cx: &mut TestAppContext) {
    let tester = Arc::new(FakeConnectionTester::default());
    let (handle, workspace) = open_workspace_with_tester(cx, HostStore::empty(), tester.clone());
    in_frame(cx, handle, |window, cx| window.click("new-host-panel", cx));
    in_frame(cx, handle, |window, cx| {
        // A new host is an SSH host.
        assert_eq!(
            window.within("host-protocol").find(0usize).selected(),
            Some(true)
        );
        assert!(window.find("host-address").visible());
        window.within("host-protocol").click(2usize, cx);
    });
    in_frame(cx, handle, |window, cx| {
        // Nothing of an SSH host's: no address, login or route.
        for id in [
            "host-address",
            "host-port",
            "host-user",
            "host-auth-source",
            "host-route",
        ] {
            assert!(window.try_find(id).is_none(), "{id}");
        }
        // The device and its rate share a row; how each character is
        // framed takes the next, four across.
        let [name, device, baud] = ["host-name", "host-serial-device", "host-serial-baud"]
            .map(|id| window.find(id).bounds());
        assert_eq!(device.top(), baud.top());
        assert!(device.size.width > baud.size.width);
        // The rate takes a third of the row, more than the quarter each of
        // the framing gets, so 115200 fits beside its menu.
        assert_eq!((device.left(), baud.right()), (name.left(), name.right()));
        assert!(baud.size.width > name.size.width / 4.);
        let framing = [
            "host-serial-data-bits",
            "host-serial-parity",
            "host-serial-stop-bits",
            "host-serial-flow",
        ]
        .map(|id| window.find(id).bounds());
        assert!(
            framing
                .iter()
                .all(|field| field.top() == framing[0].top() && field.top() > device.bottom())
        );
        assert_eq!(window.find("host-serial-baud").value(), Some("115200"));
        assert_eq!(window.find("host-serial-flow").value(), Some("无"));
        window.click("host-name", cx);
        window.click("commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find("form-error")
            .is_some_and(|error| error.visible())
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("form-error").label(), Some("请输入串口设备"));
        window.click("host-serial-device", cx);
        window.input("/dev/ttyUSB0", cx);
        replace_text(window, "host-serial-baud", "9600", cx);
        window.within("host-serial-flow").click("input", cx);
    });
    for key in ["down", "enter"] {
        in_frame(cx, handle, |window, cx| window.press(key, cx));
    }
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("host-serial-flow").value(), Some("RTS/CTS"));
    });
    // 测试连接 opens the port with what the form shows, and says how it went
    // beside the form's error from before.
    let shown = in_frame(cx, handle, |window, cx| {
        window.click("test-connection", cx);
        window.notifications(cx).len()
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.notifications(cx).len() == shown + 1
    })
    .await;
    let line = SerialSettings {
        baud: 9600,
        flow: FlowControl::RtsCts,
        ..SerialSettings::default()
    };
    assert_eq!(
        tester.serial_lines(),
        [SerialLine {
            device: "/dev/ttyUSB0".into(),
            settings: line,
        }]
    );
    assert!(tester.requests().is_empty());
    in_frame(cx, handle, |window, cx| window.click("commit", cx));
    wait_for_dialog_to_close(cx, handle).await;
    cx.update(|cx| {
        let host = &workspace.read(cx).store().read(cx).hosts()[0];
        assert_eq!(host.name.as_ref(), "/dev/ttyUSB0");
        assert_eq!(host.address.as_ref(), "/dev/ttyUSB0");
        assert_eq!(host.protocol, Protocol::Serial(line));
    });
}

#[gpui_kit::test]
async fn an_open_serial_terminal_keeps_its_line_and_a_new_one_takes_the_new_rate(
    cx: &mut TestAppContext,
) {
    let (store, id) = serial_store();
    let serial = fake_serial_terminals();
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        serial.clone(),
    );
    connect(cx, handle, id).await;
    assert_eq!(
        serial.lines(),
        [SerialLine {
            device: "/dev/ttyUSB0".into(),
            settings: SerialSettings::default(),
        }]
    );

    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(EditHost(id)), cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.within("host-protocol").find(2usize).selected(),
            Some(true)
        );
        assert_eq!(
            window.find("host-serial-device").value(),
            Some("/dev/ttyUSB0")
        );
        replace_text(window, "host-serial-baud", "57600", cx);
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
    cx.run_until_parked();
    assert_eq!(serial.lines().len(), 1, "the open line stays as it is");

    // Reconnecting opens the line the tab first did; a new tab, the new one.
    in_frame(cx, handle, |window, cx| {
        window.click(("reconnect", 1_u64), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        serial.lines().len() == 2
    })
    .await;
    assert_eq!(
        serial.lines()[1].settings.baud,
        SerialSettings::default().baud
    );
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(id)), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        serial.lines().len() == 3
    })
    .await;
    assert_eq!(serial.lines()[2].settings.baud, 57600);
    cx.update(|cx| assert_eq!(workspace.read(cx).terminal_count(id, cx), 2));
}

#[gpui_kit::test]
async fn a_serial_terminal_has_no_sftp_and_only_snippets_beside_it(cx: &mut TestAppContext) {
    let (store, id) = serial_store();
    let line = Arc::new(FakeTerminalFactory::default());
    let (handle, _) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(line.clone())),
    );
    connect(cx, handle, id).await;
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find(("sftp", 1_u64)).is_none());
        assert!(window.find(("reconnect", 1_u64)).visible());
        assert_eq!(
            window.find(("terminal-tab-os", 1_u64)).label(),
            Some("串口")
        );
        // Snippets type into the terminal, which a serial line takes; the
        // rest run commands beside it, which it has no room for.
        assert!(window.find("tool-snippets").visible());
        for tool in [
            "tool-history",
            "tool-files",
            "tool-tmux",
            "tool-processes",
            "tool-monitor",
        ] {
            assert!(window.try_find(tool).is_none(), "{tool}");
        }
    });

    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(SendBreak(RemoteTerminalId(1))), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        line.breaks() == 1
    })
    .await;
}

#[gpui_kit::test]
async fn a_serial_port_is_connected_to_without_saving_it(cx: &mut TestAppContext) {
    let serial = fake_serial_terminals();
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        serial.clone(),
    );
    in_frame(cx, handle, |window, cx| {
        window.click("temporary-connection", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.within("host-protocol").click(2usize, cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("host-serial-device", cx);
        window.input("COM3", cx);
        window.click("commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        serial.lines().len() == 1
    })
    .await;
    assert_eq!(serial.lines()[0].device, "COM3");
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert!(store.hosts().is_empty());
        assert_eq!(store.temporary_hosts().len(), 1);
    });
}

fn connection_settings(
    workspace: &Entity<Workspace>,
    cx: &mut TestAppContext,
    change: impl FnOnce(&mut shellrs::terminal::TerminalConnectionSettings),
) {
    cx.update(|cx| {
        workspace
            .read(cx)
            .settings()
            .clone()
            .update(cx, |store, cx| {
                store.update(|settings| change(&mut settings.terminal_connection), cx);
            });
    });
    cx.run_until_parked();
}

fn serial_text(workspace: &Entity<Workspace>, id: HostId, cx: &mut App) -> String {
    let panel = workspace.read(cx).terminal(id, cx).unwrap().read(cx);
    panel.terminal().read(cx).screen_text(cx)
}

struct UnpluggedPortFactory {
    ready: Arc<FakeTerminalFactory>,
    starts: AtomicUsize,
}

struct UnpluggedPort;

impl TerminalTransport for UnpluggedPort {
    fn run(
        self: Box<Self>,
        _: TerminalSize,
        _: mpsc::Receiver<TerminalTransportCommand>,
        _: async_channel::Sender<TerminalTransportEvent>,
    ) -> anyhow::Result<()> {
        anyhow::bail!("找不到串口设备 COM3")
    }
}

impl TerminalTransportFactory for UnpluggedPortFactory {
    fn create(&self) -> Box<dyn TerminalTransport> {
        if self.starts.fetch_add(1, Ordering::SeqCst) < 2 {
            Box::new(UnpluggedPort)
        } else {
            self.ready.create()
        }
    }
}

#[gpui_kit::test]
async fn a_missing_serial_port_keeps_retrying_until_it_is_available(cx: &mut TestAppContext) {
    let (store, id) = serial_store();
    let factory = Arc::new(UnpluggedPortFactory {
        ready: Arc::new(FakeTerminalFactory::default()),
        starts: AtomicUsize::new(0),
    });
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    connection_settings(&workspace, cx, |settings| {
        settings.auto_reconnect = true;
        settings.reconnect_delay_secs = 1;
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(id)), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
        workspace
            .read(cx)
            .terminal(id, cx)
            .unwrap()
            .read(cx)
            .lifecycle(cx)
            == TerminalLifecycle::Running
    })
    .await;
    assert_eq!(factory.starts.load(Ordering::SeqCst), 3);
    assert!(
        cx.update(|cx| serial_text(&workspace, id, cx))
            .contains("console connected successfully")
    );
}

#[gpui_kit::test]
async fn serial_success_is_shown_without_sending_enter(cx: &mut TestAppContext) {
    let (store, id) = serial_store();
    let line = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(line.clone())),
    );
    connect(cx, handle, id).await;
    let text = cx.update(|cx| serial_text(&workspace, id, cx));
    assert!(text.contains("console connected successfully"), "{text}");
    assert!(line.written_text().is_empty());
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1_u64), cx);
        window.press("enter", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        line.written_text() == "\r"
    })
    .await;
}

#[gpui_kit::test]
async fn serial_reconnects_after_the_configured_delay_and_manual_disconnect_stops_it(
    cx: &mut TestAppContext,
) {
    let (store, id) = serial_store();
    let line = Arc::new(FakeTerminalFactory::exit_first());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(line.clone())),
    );
    connection_settings(&workspace, cx, |settings| {
        settings.auto_reconnect = true;
        settings.reconnect_delay_secs = 3;
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(id)), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        serial_text(&workspace, id, cx).contains("3 秒后自动重连")
    })
    .await;
    assert_eq!(line.starts(), 1);
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    assert_eq!(line.starts(), 1);
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        line.starts() == 2
            && workspace
                .read(cx)
                .terminal(id, cx)
                .unwrap()
                .read(cx)
                .lifecycle(cx)
                == TerminalLifecycle::Running
    })
    .await;
    assert!(
        cx.update(|cx| serial_text(&workspace, id, cx))
            .contains("console connected successfully")
    );
    assert!(line.written_text().is_empty());
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(DisconnectTerminal(RemoteTerminalId(1))), cx)
    });
    cx.executor().advance_clock(Duration::from_secs(20));
    cx.run_until_parked();
    assert_eq!(line.starts(), 2);
}

#[gpui_kit::test]
async fn disabling_reconnect_or_closing_a_serial_tab_cancels_pending_retries(
    cx: &mut TestAppContext,
) {
    for cancellation in ["disable", "close", "disconnect"] {
        let (store, id) = serial_store();
        let line = Arc::new(FakeTerminalFactory::fail_first());
        let (handle, workspace) = open_workspace_with_terminals(
            cx,
            store,
            Arc::new(FakeTerminalFactory::default()),
            Arc::new(FixedSerialTerminalTransportProvider::new(line.clone())),
        );
        connection_settings(&workspace, cx, |settings| {
            settings.auto_reconnect = true;
            settings.reconnect_delay_secs = 10;
        });
        in_frame(cx, handle, |window, cx| {
            window.dispatch_action(Box::new(ConnectHost(id)), cx)
        });
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            serial_text(&workspace, id, cx).contains("10 秒后自动重连")
        })
        .await;
        if cancellation == "close" {
            in_frame(cx, handle, |window, cx| {
                window.dispatch_action(Box::new(CloseTerminal(RemoteTerminalId(1))), cx)
            });
        } else if cancellation == "disconnect" {
            in_frame(cx, handle, |window, cx| {
                window.dispatch_action(Box::new(DisconnectTerminal(RemoteTerminalId(1))), cx)
            });
        } else {
            connection_settings(&workspace, cx, |settings| settings.auto_reconnect = false);
        }
        cx.executor().advance_clock(Duration::from_secs(20));
        cx.run_until_parked();
        assert_eq!(line.starts(), 1);
    }
}

#[gpui_kit::test]
async fn serial_hex_sends_actual_bytes_and_rejects_invalid_input_without_sending(
    cx: &mut TestAppContext,
) {
    let (store, id) = serial_store();
    let line = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(line.clone())),
    );
    connect(cx, handle, id).await;
    in_frame(cx, handle, |window, cx| {
        window.click(("serial-hex-receive", 1_u64), cx);
        window.click(("serial-hex-transmit", 1_u64), cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1_u64), cx);
        window.input("do not send", cx);
        window.click(("serial-hex-input", 1_u64), cx);
        window.input("00 FF 0D 0A", cx);
        window.click(("serial-hex-send", 1_u64), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        serial_text(&workspace, id, cx).contains("00 FF 0D 0A")
    })
    .await;
    assert_eq!(*line.writes.lock().unwrap(), vec![vec![0, 255, 13, 10]]);
    in_frame(cx, handle, |window, cx| {
        window.click(("serial-hex-input", 1_u64), cx);
        window.press("secondary-a", cx);
        window.input("GG 0", cx);
        window.press("enter", cx);
    });
    cx.run_until_parked();
    assert_eq!(line.writes.lock().unwrap().len(), 1);
    in_frame(cx, handle, |window, cx| {
        assert!(!window.notifications(cx).is_empty());
        window.click(("serial-hex-input", 1_u64), cx);
        window.press("secondary-a", cx);
        window.input("0x41 00", cx);
        window.press("enter", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        line.writes.lock().unwrap().len() == 2
    })
    .await;
    assert_eq!(line.writes.lock().unwrap()[1], vec![0x41, 0]);
    in_frame(cx, handle, |window, cx| {
        window.click(("serial-hex-transmit", 1_u64), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find(("serial-hex-input", 1_u64)).is_none())
    });
}

#[gpui_kit::test]
async fn serial_logs_can_be_saved_to_a_chosen_file_and_stopped(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("serial.log");
    let (store, id) = serial_store();
    let line = Arc::new(FakeTerminalFactory::default());
    let (handle, _) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(line.clone())),
    );
    connect(cx, handle, id).await;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-recording", 1_u64), cx)
    });
    assert!(cx.did_prompt_for_new_path());
    cx.simulate_new_path_selection(|_| Some(path.clone()));
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find(("terminal-recording-path", 1_u64))
            .is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1_u64), cx);
        window.input("serial log 中文\n", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        std::fs::read(&path).is_ok_and(|bytes| bytes == "serial log 中文\n".as_bytes())
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(StopTerminalRecording(RemoteTerminalId(1))), cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert!(
            window
                .try_find(("terminal-recording-path", 1_u64))
                .is_none()
        );
        window.click(("terminal", 1_u64), cx);
        window.input("not recorded", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        line.written_text().contains("not recorded")
    })
    .await;
    assert_eq!(
        std::fs::read(&path).unwrap(),
        "serial log 中文\n".as_bytes()
    );
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(StartTerminalRecording(RemoteTerminalId(1))), cx)
    });
    cx.simulate_new_path_selection(|_| None);
    cx.run_until_parked();
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .try_find(("terminal-recording-path", 1_u64))
                .is_none()
        )
    });
}

#[gpui_kit::test]
async fn a_hosts_automatic_log_starts_on_connect_and_can_be_cleared_or_deleted(
    cx: &mut TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("console.log");
    let (store, id) = serial_store();
    let line = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(line.clone())),
    );
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(EditHost(id)), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("host-log-choose", cx));
    cx.simulate_new_path_selection(|_| Some(path.clone()));
    let shown = path.display().to_string();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.find("host-log-file").label() == Some(shown.as_str())
    })
    .await;
    in_frame(cx, handle, |window, cx| window.click("commit", cx));
    wait_for_dialog_to_close(cx, handle).await;
    cx.update(|cx| {
        assert_eq!(
            workspace
                .read(cx)
                .store()
                .read(cx)
                .host(id)
                .unwrap()
                .log_file
                .as_deref(),
            Some(path.to_string_lossy().as_ref())
        );
    });

    // The same setting is there when the host is edited again, and a
    // connection starts appending without opening the picker.
    in_frame(cx, handle, |window, cx| {
        window.click(("host-row", id.0), cx);
        window.dispatch_action(Box::new(EditHost(id)), cx);
    });
    cx.run_until_parked();
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("host-log-file").label(), Some(shown.as_str()));
    });
    in_frame(cx, handle, |window, cx| window.click("cancel", cx));
    cx.run_until_parked();
    connect(cx, handle, id).await;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1_u64), cx);
        window.input("first line\n", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        std::fs::read(&path)
            .is_ok_and(|bytes| String::from_utf8_lossy(&bytes).contains("first line"))
    })
    .await;

    // 清空 empties it and keeps recording.
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-log-clear", 1_u64), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        std::fs::read(&path).is_ok_and(|bytes| bytes.is_empty())
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1_u64), cx);
        window.input("after clear\n", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        std::fs::read(&path).is_ok_and(|bytes| {
            String::from_utf8_lossy(&bytes).contains("after clear")
                && !String::from_utf8_lossy(&bytes).contains("first line")
        })
    })
    .await;

    // 删除 removes the file and the host's automatic setting, so the next
    // connection does not recreate it.
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-log-delete", 1_u64), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        !path.exists()
            && workspace
                .read(cx)
                .store()
                .read(cx)
                .host(id)
                .unwrap()
                .log_file
                .is_none()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert!(
            window
                .try_find(("terminal-recording-path", 1_u64))
                .is_none()
        );
        window.dispatch_action(Box::new(ReconnectTerminal(RemoteTerminalId(1))), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(1))
            .unwrap()
            .read(cx)
            .lifecycle(cx)
            == TerminalLifecycle::Running
    })
    .await;
    cx.run_until_parked();
    assert!(!path.exists());
}

#[gpui_kit::test]
async fn serial_hex_logs_are_append_only_and_a_bad_path_does_not_disconnect(
    cx: &mut TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hex.log");
    std::fs::write(&path, "previous\n").unwrap();
    let (store, id) = serial_store();
    let line = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(line.clone())),
    );
    connect(cx, handle, id).await;
    in_frame(cx, handle, |window, cx| {
        window.click(("serial-hex-receive", 1_u64), cx);
        window.click(("serial-hex-transmit", 1_u64), cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(StartTerminalRecording(RemoteTerminalId(1))), cx)
    });
    cx.simulate_new_path_selection(|_| Some(dir.path().join("missing").join("serial.log")));
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        !window.notifications(cx).is_empty()
    })
    .await;
    cx.update(|cx| {
        let panel = workspace.read(cx).terminal(id, cx).unwrap().read(cx);
        assert_eq!(panel.lifecycle(cx), TerminalLifecycle::Running);
        assert!(!panel.terminal().read(cx).is_recording(cx));
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(StartTerminalRecording(RemoteTerminalId(1))), cx)
    });
    cx.simulate_new_path_selection(|_| Some(path.clone()));
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find(("terminal-recording-path", 1_u64))
            .is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click(("serial-hex-input", 1_u64), cx);
        window.input("00 ff 0D", cx);
        window.press("enter", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        std::fs::read(&path).unwrap() == b"previous\n00 FF 0D\r\n"
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(CloseTerminal(RemoteTerminalId(1))), cx)
    });
    cx.update(|cx| assert_eq!(workspace.read(cx).terminal_count(id, cx), 0));
    assert_eq!(std::fs::read(&path).unwrap(), b"previous\n00 FF 0D\r\n");
}

#[gpui_kit::test]
async fn serial_hex_modes_are_independent_per_tab_and_remembered_per_host(cx: &mut TestAppContext) {
    let (mut store, first) = serial_store();
    let second = store.insert_unnotified(HostDraft::serial(
        "second",
        "COM4",
        SerialSettings::default(),
        None,
    ));
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        fake_serial_terminals(),
    );
    connect(cx, handle, first).await;
    for host in [second, first] {
        in_frame(cx, handle, |window, cx| {
            window.dispatch_action(Box::new(ConnectHost(host)), cx)
        });
    }
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(3))
            .is_some_and(|panel| panel.read(cx).lifecycle(cx) == TerminalLifecycle::Running)
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", 1_u64), cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find(("serial-hex-receive", 1_u64)).checked(),
            Some(false)
        );
        window.click(("serial-hex-receive", 1_u64), cx);
        window.click(("serial-hex-transmit", 1_u64), cx);
    });
    cx.update(|cx| {
        let workspace = workspace.read(cx);
        for (terminal, expected) in [(1, true), (2, false), (3, false)] {
            let modes = workspace
                .remote_terminal(RemoteTerminalId(terminal))
                .unwrap()
                .read(cx)
                .serial_modes(cx);
            assert_eq!(modes.receive_hex, expected);
            assert_eq!(modes.send_hex, expected);
        }
        let settings = workspace.settings().read(cx).settings();
        let hosts = workspace.store().read(cx);
        assert!(
            settings
                .serial_preferences
                .get(hosts.host(first).unwrap().public_id.as_str())
                .receive_hex
        );
        assert!(
            !settings
                .serial_preferences
                .get(hosts.host(second).unwrap().public_id.as_str())
                .receive_hex
        );
    });
    connection_settings(&workspace, cx, |settings| settings.reconnect_delay_secs = 7);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ReconnectTerminal(RemoteTerminalId(1))), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(1))
            .unwrap()
            .read(cx)
            .lifecycle(cx)
            == TerminalLifecycle::Running
    })
    .await;
    cx.update(|cx| {
        assert!(
            workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(1))
                .unwrap()
                .read(cx)
                .serial_modes(cx)
                .receive_hex
        )
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(CloseTerminal(RemoteTerminalId(1))), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(first)), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(4))
            .is_some_and(|panel| panel.read(cx).lifecycle(cx) == TerminalLifecycle::Running)
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find(("serial-hex-receive", 4_u64)).checked(),
            Some(true)
        );
        assert_eq!(
            window.find(("serial-hex-transmit", 4_u64)).checked(),
            Some(true)
        );
        window.dispatch_action(
            Box::new(shellrs::app::DuplicateTerminal(RemoteTerminalId(4))),
            cx,
        );
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find(("serial-hex-receive", 5_u64)).is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find(("serial-hex-receive", 5_u64)).checked(),
            Some(true)
        );
        window.click(("serial-hex-receive", 5_u64), cx);
    });
    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(
            workspace
                .remote_terminal(RemoteTerminalId(4))
                .unwrap()
                .read(cx)
                .serial_modes(cx)
                .receive_hex
        );
        assert!(
            !workspace
                .remote_terminal(RemoteTerminalId(5))
                .unwrap()
                .read(cx)
                .serial_modes(cx)
                .receive_hex
        );
        assert_eq!(
            workspace
                .remote_terminal(RemoteTerminalId(3))
                .unwrap()
                .read(cx)
                .serial_modes(cx),
            shellrs::terminal::SerialDataModes::default()
        );
    });
}
