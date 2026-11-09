use crate::support::*;
use shellrs::{
    app::{CopyTerminal, DuplicateTerminal, SelectAllTerminal},
    utilities::UtilityCommand,
};

#[gpui_kit::test]
async fn timestamp_utility_tracks_hosts_measures_two_lines_and_copies_without_changing_raw_text(
    cx: &mut TestAppContext,
) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let serial = Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone()));
    let (handle, workspace) =
        open_workspace_with_terminals(cx, HostStore::seed(), factory.clone(), serial);
    let key = cx.update(|cx| {
        format!(
            "host:{}",
            workspace
                .read(cx)
                .store()
                .read(cx)
                .host(HostId(1))
                .unwrap()
                .public_id
        )
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1u64), cx);
    });
    in_frame(cx, handle, |window, cx| window.click("tool-utilities", cx));
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("utilities-timestamps").checked(), Some(false));
        window.click("utilities-timestamps", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
        workspace.read(cx).store().read(cx).timestamps_enabled(&key)
    })
    .await;
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
        matches!(
            workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(1))
                .unwrap()
                .read(cx)
                .terminal()
                .read(cx)
                .lifecycle(cx),
            TerminalLifecycle::Running
        )
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1u64), cx);
        window.input("first-log\r\n", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
        let view = workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(1))
            .unwrap()
            .read(cx)
            .terminal()
            .read(cx);
        view.screen_text(cx).contains("first-log") && !view.visible_timestamps(cx).is_empty()
    })
    .await;
    let first = cx.update(|cx| {
        workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(1))
            .unwrap()
            .read(cx)
            .terminal()
            .read(cx)
            .visible_timestamps(cx)
            .last()
            .unwrap()
            .id
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1u64), cx);
        window.input("second-log\r\n", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
        let view = workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(1))
            .unwrap()
            .read(cx)
            .terminal()
            .read(cx);
        view.screen_text(cx).contains("second-log")
            && view
                .visible_timestamps(cx)
                .last()
                .is_some_and(|stamp| stamp.id != first)
    })
    .await;
    let second = cx.update(|cx| {
        workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(1))
            .unwrap()
            .read(cx)
            .terminal()
            .read(cx)
            .visible_timestamps(cx)
            .last()
            .unwrap()
            .id
    });
    let settings = cx.update(|cx| workspace.read(cx).settings().clone());
    for size in [10., 13., 20., 28.] {
        cx.update(|cx| {
            settings.update(cx, |settings, cx| {
                settings.update(|settings| settings.terminal_font.size = size, cx)
            })
        });
        in_frame(cx, handle, |window, cx| {
            let font = shellrs::terminal::TerminalFont::current(cx);
            let gutter = window
                .within(("terminal", 1u64))
                .find("terminal-timestamps")
                .bounds();
            let text = window.find(("timestamp-text", second));
            let label = text.label().unwrap();
            assert_eq!(label.len(), 16);
            assert!(label.starts_with("[ ") && label.ends_with(" ]"));
            assert_eq!(label.matches(':').count(), 3);
            let expected = window
                .text_system()
                .shape_line(
                    label.to_owned().into(),
                    font.size,
                    &[gpui_kit::TextRun {
                        len: label.len(),
                        font: gpui_kit::font(font.family(cx)),
                        color: cx.theme().foreground,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    None,
                )
                .width();
            assert!((gutter.size.width - expected).abs() <= px(1.));
            assert!((text.bounds().left() - gutter.left()).abs() <= px(1.));
            assert!((text.bounds().size.height - font.row_height(window)).abs() <= px(1.));
            let grid = window
                .within(("terminal", 1u64))
                .find("terminal-grid")
                .bounds();
            assert!((grid.left() - gutter.right()).abs() <= px(1.));
        });
    }
    cx.update(|cx| {
        settings.update(cx, |settings, cx| {
            settings.update(|settings| settings.terminal_font.size = 13., cx)
        })
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("timestamp-row", first), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("timestamp-row", second), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .find("timestamp-difference")
                .label()
                .unwrap_or_default()
                .contains("毫秒")
        )
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1u64), cx);
        window.dispatch_action(Box::new(SelectAllTerminal), cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(CopyTerminal), cx)
    });
    let raw = cx.update(|cx| cx.read_from_clipboard().unwrap().text().unwrap());
    assert!(raw.contains("first-log") && raw.contains("second-log"));
    assert!(!raw.contains("[--]"));
    in_frame(cx, handle, |window, cx| window.click("timestamp-copy", cx));
    let decorated = cx.update(|cx| cx.read_from_clipboard().unwrap().text().unwrap());
    assert!(decorated.contains("first-log") && decorated.contains("second-log"));
    assert!(decorated.contains('[') && decorated != raw);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1u64), cx);
        window.dispatch_action(Box::new(DuplicateTerminal(RemoteTerminalId(1))), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
        workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(3))
            .is_some()
    })
    .await;
    cx.update(|cx| {
        let panel = workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(3))
            .unwrap();
        assert!(panel.read(cx).terminal().read(cx).timestamp_enabled(cx));
        assert!(
            !workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(2))
                .unwrap()
                .read(cx)
                .terminal()
                .read(cx)
                .timestamp_enabled(cx)
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 3u64), cx);
        window.dispatch_action(
            Box::new(shellrs::app::UtilityAction(UtilityCommand::Toggle(false))),
            cx,
        );
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
        !workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(1))
            .unwrap()
            .read(cx)
            .terminal()
            .read(cx)
            .timestamp_enabled(cx)
    })
    .await;
}
