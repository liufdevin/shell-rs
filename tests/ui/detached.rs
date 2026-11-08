use crate::support::*;
use shellrs::app::{DetachTab, ReturnDetachedTab};

fn other_window(cx: &mut TestAppContext, main: WindowHandle<Root>) -> WindowHandle<Root> {
    cx.update(|cx| {
        cx.windows()
            .into_iter()
            .find(|handle| handle.window_id() != main.window_id())
            .unwrap()
            .downcast::<Root>()
            .unwrap()
    })
}

#[gpui_kit::test]
async fn dragging_a_terminal_out_opens_a_window_without_reconnecting_and_can_return(
    cx: &mut TestAppContext,
) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (main, workspace) =
        open_workspace_with_remote_factory(cx, HostStore::seed(), factory.clone());
    in_frame(cx, main, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    cx.wait_for(main.into(), Duration::from_secs(3), |_, cx| {
        remote_lifecycle(&workspace, RemoteTerminalId(INITIAL_WEB_TERMINAL), cx).accepts_input()
    })
    .await;
    let terminal = cx.update(|cx| {
        workspace
            .read(cx)
            .terminal(HostId(WEB_01), cx)
            .unwrap()
            .entity_id()
    });
    let starts = factory.starts.load(Ordering::SeqCst);
    in_frame(cx, main, |window, cx| {
        let from = window
            .find(("terminal-tab", INITIAL_WEB_TERMINAL))
            .bounds()
            .center();
        let outside = point(window.viewport_size().width + px(80.), from.y);
        window.drag(from, outside, cx);
    });
    let detached = other_window(cx, main);
    in_frame(cx, detached, |window, cx| {
        assert!(window.find("detached-tab-content").visible());
        assert!(window.find(("terminal", INITIAL_WEB_TERMINAL)).visible());
        window.click(("terminal", INITIAL_WEB_TERMINAL), cx);
        window.input("kept-connection", cx);
    });
    in_frame(cx, detached, |window, cx| {
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-f"
            } else {
                "ctrl-f"
            },
            cx,
        );
    });
    in_frame(cx, detached, |window, cx| {
        assert!(window.find("terminal-find").visible());
        window.press("escape", cx);
    });
    cx.wait_for(detached.into(), Duration::from_secs(2), |_, _| {
        factory.written_text().contains("kept-connection")
    })
    .await;
    cx.update(|cx| {
        assert_eq!(
            workspace
                .read(cx)
                .terminal(HostId(WEB_01), cx)
                .unwrap()
                .entity_id(),
            terminal
        );
        assert_eq!(factory.starts.load(Ordering::SeqCst), starts);
    });
    in_frame(cx, detached, |window, cx| {
        window.click("return-detached-tab", cx)
    });
    in_frame(cx, main, |window, _| {
        assert!(window.find(("terminal", INITIAL_WEB_TERMINAL)).visible());
    });
    assert_eq!(cx.update(|cx| cx.windows().len()), 1);
    assert_eq!(factory.starts.load(Ordering::SeqCst), starts);
}

#[gpui_kit::test]
async fn closing_a_detached_terminal_closes_only_its_connection(cx: &mut TestAppContext) {
    let (main, workspace) = open_workspace(cx);
    in_frame(cx, main, |window, cx| {
        window.dispatch_action(
            Box::new(DetachTab(CenterTab::Terminal(RemoteTerminalId(
                INITIAL_WEB_TERMINAL,
            )))),
            cx,
        );
    });
    let detached = other_window(cx, main);
    in_frame(cx, detached, |window, cx| {
        window.click(("terminal", INITIAL_WEB_TERMINAL), cx);
        window.dispatch_action(
            Box::new(shellrs::app::CloseDetachedTab(CenterTab::Terminal(
                RemoteTerminalId(INITIAL_WEB_TERMINAL),
            ))),
            cx,
        );
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(workspace.read(cx).terminal(HostId(WEB_01), cx).is_none());
        assert!(
            workspace
                .read(cx)
                .terminal(HostId(STAGING_API), cx)
                .is_some()
        );
        assert_eq!(cx.windows().len(), 1);
    });
}

#[gpui_kit::test]
async fn detached_window_language_changes_without_observing_workspace_updates(
    cx: &mut TestAppContext,
) {
    let (main, workspace) = open_workspace(cx);
    in_frame(cx, main, |window, cx| {
        window.dispatch_action(
            Box::new(DetachTab(CenterTab::Terminal(RemoteTerminalId(
                INITIAL_WEB_TERMINAL,
            )))),
            cx,
        );
    });
    let detached = other_window(cx, main);
    in_frame(cx, detached, |window, _| {
        assert_eq!(
            window.find("return-detached-tab").label(),
            Some("移回主窗口")
        );
    });
    cx.update(|cx| {
        let settings = workspace.read(cx).settings().clone();
        settings.update(cx, |settings, cx| {
            settings.update(
                |settings| settings.language = InterfaceLanguage::English,
                cx,
            );
        });
    });
    cx.wait_for(detached.into(), Duration::from_secs(2), |window, _| {
        window.find("return-detached-tab").label() == Some("Move to main window")
    })
    .await;
    in_frame(cx, detached, |window, cx| {
        window.click("return-detached-tab", cx);
    });
    assert_eq!(cx.update(|cx| cx.windows().len()), 1);
}

#[gpui_kit::test]
fn settings_can_move_to_a_window_and_return_without_duplication(cx: &mut TestAppContext) {
    let (main, _) = open_workspace(cx);
    in_frame(cx, main, |window, cx| window.click("open-settings", cx));
    in_frame(cx, main, |window, cx| {
        window.dispatch_action(Box::new(DetachTab(CenterTab::Settings)), cx)
    });
    let detached = other_window(cx, main);
    in_frame(cx, detached, |window, cx| {
        assert!(window.find("settings").visible());
        window.dispatch_action(Box::new(ReturnDetachedTab(CenterTab::Settings)), cx);
    });
    in_frame(cx, main, |window, _| {
        assert!(window.find("settings").visible())
    });
    assert_eq!(cx.update(|cx| cx.windows().len()), 1);
}

#[gpui_kit::test]
async fn a_detached_editor_keeps_unsaved_changes_and_confirms_close(cx: &mut TestAppContext) {
    use shellrs::editor::EditorId;
    let provider = Arc::new(FakeSftpProvider::with_files(&[(
        "/home/tester/文件 甲.txt",
        b"original\n",
    )]));
    let (main, workspace) = open_workspace_with_sftp(cx, provider.clone());
    open_test_explorer(cx, main).await;
    in_frame(cx, main, |window, cx| {
        window
            .within(("remote-pane", SFTP_TAB))
            .double_click(ElementId::Name("name:文件 甲.txt".into()), cx);
    });
    cx.wait_for(main.into(), Duration::from_secs(3), |window, _| {
        window.try_find(("editor", 1u64)).is_some()
    })
    .await;
    in_frame(cx, main, |window, cx| {
        window.input("unsaved-", cx);
        window.dispatch_action(Box::new(DetachTab(CenterTab::Editor(EditorId(1)))), cx);
    });
    let detached = other_window(cx, main);
    in_frame(cx, detached, |window, cx| {
        assert!(window.find(("editor", 1u64)).visible());
        window.dispatch_action(Box::new(shellrs::app::CloseActiveTab), cx);
    });
    cx.wait_for(detached.into(), Duration::from_secs(2), |window, _| {
        window.try_find("editor-close-discard").is_some()
    })
    .await;
    in_frame(cx, detached, |window, cx| {
        window.click("editor-close-cancel", cx)
    });
    cx.update(|cx| {
        let editor = workspace.read(cx).editor(EditorId(1)).unwrap().read(cx);
        assert_eq!(
            editor.state().read(cx).value().to_string(),
            "unsaved-original\n"
        );
    });
    in_frame(cx, detached, |window, cx| {
        window.click(("editor", 1u64), cx);
        window.dispatch_action(Box::new(shellrs::app::CloseActiveTab), cx);
    });
    cx.wait_for(detached.into(), Duration::from_secs(2), |window, _| {
        window.try_find("editor-close-discard").is_some()
    })
    .await;
    in_frame(cx, detached, |window, cx| {
        window.click("editor-close-discard", cx)
    });
    cx.update(|cx| {
        assert!(workspace.read(cx).editor(EditorId(1)).is_none());
        assert_eq!(cx.windows().len(), 1);
    });
    assert!(provider.writes.lock().unwrap().is_empty());
}

#[gpui_kit::test]
async fn an_sftp_tab_moves_without_opening_another_connection(cx: &mut TestAppContext) {
    let provider = Arc::new(FakeSftpProvider::default());
    let (main, workspace) = open_workspace_with_sftp(cx, provider.clone());
    open_test_explorer(cx, main).await;
    let generation = cx.update(|cx| {
        workspace
            .read(cx)
            .explorer(ExplorerId(SFTP_TAB))
            .unwrap()
            .read(cx)
            .generation()
    });
    let connections = provider.events.lock().unwrap().len();
    in_frame(cx, main, |window, cx| {
        window.dispatch_action(
            Box::new(DetachTab(CenterTab::Explorer(ExplorerId(SFTP_TAB)))),
            cx,
        )
    });
    let detached = other_window(cx, main);
    in_frame(cx, detached, |window, cx| {
        assert!(window.find(("explorer", SFTP_TAB)).visible());
        assert_eq!(window.find("remote-path").value(), Some("/home/tester"));
        window.click("return-detached-tab", cx);
    });
    assert_eq!(provider.events.lock().unwrap().len(), connections);
    cx.update(|cx| {
        assert_eq!(
            workspace
                .read(cx)
                .explorer(ExplorerId(SFTP_TAB))
                .unwrap()
                .read(cx)
                .generation(),
            generation
        )
    });
    in_frame(cx, main, |window, _| {
        assert!(window.find(("explorer", SFTP_TAB)).visible())
    });
}
