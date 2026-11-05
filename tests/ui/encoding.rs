use crate::support::*;
use shellrs::host::{SerialSettings, TerminalEncoding};

#[gpui_kit::test]
async fn host_encoding_defaults_to_utf8_and_is_editable_per_host(cx: &mut TestAppContext) {
    let mut hosts = HostStore::empty();
    let first = hosts.insert_unnotified(HostDraft::serial(
        "console",
        "COM3",
        SerialSettings::default(),
        None,
    ));
    let other = hosts.insert_unnotified(HostDraft::serial(
        "other",
        "COM4",
        SerialSettings::default(),
        None,
    ));
    let (handle, workspace) = open_workspace_with_store(cx, hosts);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(EditHost(first)), cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("host-terminal-encoding").value(), Some("UTF-8"));
        assert!(window.find("host-terminal-encoding-note").visible());
        let encoding = window.find("host-terminal-encoding").bounds();
        let note = window.find("host-terminal-encoding-note").bounds();
        let group = window.find("host-group").bounds();
        assert!(note.top() >= encoding.bottom(), "{encoding:?} {note:?}");
        assert!(group.top() >= note.bottom(), "{note:?} {group:?}");
        window.within("host-terminal-encoding").click("input", cx);
    });
    for key in ["down", "enter"] {
        in_frame(cx, handle, |window, cx| window.press(key, cx));
    }
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("host-terminal-encoding").value(), Some("GBK"));
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
    cx.update(|cx| {
        let hosts = workspace.read(cx).store().read(cx);
        assert_eq!(
            hosts.host(first).unwrap().terminal_encoding,
            TerminalEncoding::Gbk
        );
        assert_eq!(
            hosts.host(other).unwrap().terminal_encoding,
            TerminalEncoding::Utf8
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.click("host-search", cx);
        window.dispatch_action(Box::new(EditHost(first)), cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("host-terminal-encoding").value(), Some("GBK"));
        window.press("escape", cx);
    });
}

async fn connected(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
    host: HostId,
    tab: u64,
) {
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(host)), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, cx| {
        window.render_frame(cx);
        workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(tab))
            .is_some_and(|panel| panel.read(cx).lifecycle(cx) == TerminalLifecycle::Running)
    })
    .await;
}

fn screen(workspace: &Entity<Workspace>, tab: u64, cx: &App) -> String {
    workspace
        .read(cx)
        .remote_terminal(RemoteTerminalId(tab))
        .unwrap()
        .read(cx)
        .terminal()
        .read(cx)
        .screen_text(cx)
}

fn wrote(wire: &FakeTerminalFactory, expected: &[u8]) -> bool {
    wire.writes
        .lock()
        .unwrap()
        .concat()
        .windows(expected.len())
        .any(|bytes| bytes == expected)
}

#[gpui_kit::test]
async fn encoded_hosts_send_receive_reject_and_reconnect_without_changing_other_tabs(
    cx: &mut TestAppContext,
) {
    let mut hosts = HostStore::empty();
    let first = hosts.insert_unnotified(
        HostDraft::serial("gbk", "COM3", SerialSettings::default(), None)
            .with_terminal_encoding(TerminalEncoding::Gbk),
    );
    let wide = hosts.insert_unnotified(
        HostDraft::serial("gb18030", "COM4", SerialSettings::default(), None)
            .with_terminal_encoding(TerminalEncoding::Gb18030),
    );
    let utf8 = hosts.insert_unnotified(HostDraft::serial(
        "utf8",
        "COM5",
        SerialSettings::default(),
        None,
    ));
    let wire = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        hosts,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(wire.clone())),
    );
    connected(cx, handle, &workspace, first, 1).await;
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("status-terminal-encoding").label(),
            Some("编码 GBK")
        );
        window.click(("terminal", 1_u64), cx);
        window.input("中文", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        screen(&workspace, 1, cx).contains("中文")
    })
    .await;
    assert!(wrote(&wire, &[0xd6, 0xd0, 0xce, 0xc4]));
    let before = wire.writes.lock().unwrap().len();
    in_frame(cx, handle, |window, cx| window.input("😀", cx));
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, cx| {
        !window.notifications(cx).is_empty()
    })
    .await;
    assert_eq!(wire.writes.lock().unwrap().len(), before);
    connected(cx, handle, &workspace, wide, 2).await;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 2_u64), cx);
        window.input("中文😀", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        screen(&workspace, 2, cx).contains("中文😀")
    })
    .await;
    let (encoded, _, _) = encoding_rs::GB18030.encode("中文😀");
    assert!(wrote(&wire, encoded.as_ref()));
    connected(cx, handle, &workspace, utf8, 3).await;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 3_u64), cx);
        window.input("中文😀", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        screen(&workspace, 3, cx).contains("中文😀")
    })
    .await;
    assert!(wrote(&wire, "中文😀".as_bytes()));
    cx.update(|cx| {
        let store = workspace.read(cx).store().clone();
        let draft = store
            .read(cx)
            .host(first)
            .unwrap()
            .draft()
            .with_terminal_encoding(TerminalEncoding::Gb18030);
        store.update(cx, |store, cx| store.update(first, draft, cx));
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(1))
                .unwrap()
                .read(cx)
                .status(cx)
                .encoding(),
            TerminalEncoding::Gbk
        );
        assert_eq!(
            workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(3))
                .unwrap()
                .read(cx)
                .status(cx)
                .encoding(),
            TerminalEncoding::Utf8
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", 1_u64), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ReconnectTerminal(RemoteTerminalId(1))), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        let panel = workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(1))
            .unwrap()
            .read(cx);
        panel.lifecycle(cx) == TerminalLifecycle::Running
            && panel.status(cx).encoding() == TerminalEncoding::Gb18030
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1_u64), cx);
        window.input("重连😀", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        screen(&workspace, 1, cx).contains("重连😀")
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(shellrs::app::ToggleSerialReceiveHex(RemoteTerminalId(1))),
            cx,
        );
        window.dispatch_action(
            Box::new(shellrs::app::ToggleSerialSendHex(RemoteTerminalId(1))),
            cx,
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("serial-hex-input", 1_u64), cx);
        window.input("D6 D0 00 FF", cx);
        window.click(("serial-hex-send", 1_u64), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
        screen(&workspace, 1, cx).contains("D6 D0 00 FF")
    })
    .await;
    assert!(
        wire.writes
            .lock()
            .unwrap()
            .iter()
            .any(|bytes| bytes == &[0xd6, 0xd0, 0, 0xff])
    );
}
