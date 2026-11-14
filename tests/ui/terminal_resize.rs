use crate::support::*;
use shellrs::connection::Latency;
use shellrs::terminal::TerminalView;

const INITIAL: &str = "\x1b[?2026h\x1b[2J\x1b[H完整画面 Codex ready\r\n> \x1b[?2026l";

struct RedrawFactory {
    ready: Mutex<mpsc::Receiver<async_channel::Sender<TerminalTransportEvent>>>,
    sender: mpsc::Sender<async_channel::Sender<TerminalTransportEvent>>,
    resizes: Arc<Mutex<Vec<TerminalSize>>>,
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
    initial: &'static str,
}

impl RedrawFactory {
    fn new(initial: &'static str) -> Self {
        let (sender, ready) = mpsc::channel();
        Self {
            ready: Mutex::new(ready),
            sender,
            resizes: Arc::default(),
            writes: Arc::default(),
            initial,
        }
    }
}

impl TerminalTransportFactory for RedrawFactory {
    fn create(&self) -> Box<dyn TerminalTransport> {
        Box::new(RedrawTransport {
            sender: self.sender.clone(),
            resizes: self.resizes.clone(),
            writes: self.writes.clone(),
            initial: self.initial,
        })
    }
}

struct RedrawTransport {
    sender: mpsc::Sender<async_channel::Sender<TerminalTransportEvent>>,
    resizes: Arc<Mutex<Vec<TerminalSize>>>,
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
    initial: &'static str,
}

impl TerminalTransport for RedrawTransport {
    fn run(
        self: Box<Self>,
        _: TerminalSize,
        commands: mpsc::Receiver<TerminalTransportCommand>,
        events: async_channel::Sender<TerminalTransportEvent>,
    ) -> anyhow::Result<()> {
        self.sender.send(events.clone())?;
        events.send_blocking(TerminalTransportEvent::Started)?;
        events.send_blocking(TerminalTransportEvent::Output(
            self.initial.as_bytes().to_vec(),
        ))?;
        while let Ok(command) = commands.recv() {
            match command {
                TerminalTransportCommand::Resize(size) => self.resizes.lock().unwrap().push(size),
                TerminalTransportCommand::Write(bytes) => self.writes.lock().unwrap().push(bytes),
                TerminalTransportCommand::Shutdown => break,
                _ => {}
            }
        }
        Ok(())
    }
}

async fn open_redraw(
    cx: &mut TestAppContext,
    serial: bool,
    initial: &'static str,
) -> (
    WindowHandle<Root>,
    Entity<Workspace>,
    Entity<TerminalView>,
    Arc<RedrawFactory>,
    async_channel::Sender<TerminalTransportEvent>,
) {
    let factory = Arc::new(RedrawFactory::new(initial));
    let (store, host) = if serial {
        let mut store = HostStore::empty();
        let host = store.insert_unnotified(HostDraft::serial(
            "console",
            "TEST-COM",
            shellrs::host::SerialSettings::default(),
            None,
        ));
        (store, host)
    } else {
        one_host_store(AuthKind::NoPassword)
    };
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        store,
        factory.clone(),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(host)), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(1))
            .is_some_and(|panel| {
                panel
                    .read(cx)
                    .terminal()
                    .read(cx)
                    .screen_text(cx)
                    .contains("ready")
            })
    })
    .await;
    cx.executor().advance_clock(Duration::from_millis(350));
    cx.run_until_parked();
    let terminal = cx.update(|cx| {
        workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(1))
            .unwrap()
            .read(cx)
            .terminal()
            .clone()
    });
    let sender = factory
        .ready
        .lock()
        .unwrap()
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    (handle, workspace, terminal, factory, sender)
}

async fn output(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    terminal: &Entity<TerminalView>,
    sender: &async_channel::Sender<TerminalTransportEvent>,
    bytes: &str,
    marker: u64,
) {
    sender
        .send_blocking(TerminalTransportEvent::Output(bytes.as_bytes().to_vec()))
        .unwrap();
    let latency = Latency::Measured(Duration::from_millis(marker));
    sender
        .send_blocking(TerminalTransportEvent::Latency(latency))
        .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        terminal.read(cx).latency(cx) == Some(latency)
    })
    .await;
}

#[gpui_kit::test]
async fn sidebar_resize_retains_the_complete_frame_until_synchronized_redraw_finishes(
    cx: &mut TestAppContext,
) {
    let (handle, _workspace, terminal, factory, sender) = open_redraw(cx, false, INITIAL).await;
    let before = cx.update(|cx| terminal.read(cx).screen_text(cx));
    let sizes = factory.resizes.lock().unwrap().len();
    let width = in_frame(cx, handle, |window, cx| {
        let width = window
            .within(("terminal", 1u64))
            .find("terminal-grid")
            .bounds()
            .size
            .width;
        window.click("tool-snippets", cx);
        width
    });
    in_frame(cx, handle, |window, cx| {
        assert!(
            window
                .within(("terminal", 1u64))
                .find("terminal-grid")
                .bounds()
                .size
                .width
                < width
        );
        assert_eq!(terminal.read(cx).screen_text(cx), before);
    });
    output(
        cx,
        handle,
        &terminal,
        &sender,
        "\x1b[?2026h\x1b[2J\x1b[H半帧",
        1,
    )
    .await;
    cx.update(|cx| assert_eq!(terminal.read(cx).screen_text(cx), before));
    output(
        cx,
        handle,
        &terminal,
        &sender,
        "\r\n完整新画面\x1b[?2026l",
        2,
    )
    .await;
    cx.update(|cx| assert!(terminal.read(cx).screen_text(cx).contains("完整新画面")));
    in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
    output(
        cx,
        handle,
        &terminal,
        &sender,
        "\x1b[?2026h\x1b[2J\x1b[H恢复宽屏\x1b[?2026l",
        3,
    )
    .await;
    cx.update(|cx| assert!(terminal.read(cx).screen_text(cx).contains("恢复宽屏")));
    assert!(factory.writes.lock().unwrap().is_empty());
    let resizes = factory.resizes.lock().unwrap();
    assert_eq!(resizes.len(), sizes + 2);
    assert!(resizes.windows(2).all(|pair| pair[0] != pair[1]));
}

#[gpui_kit::test]
async fn sidebar_resize_has_a_bounded_fallback_when_a_tui_stops_redrawing(cx: &mut TestAppContext) {
    let (handle, _workspace, terminal, _, sender) = open_redraw(cx, false, INITIAL).await;
    in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
    output(
        cx,
        handle,
        &terminal,
        &sender,
        "\x1b[2J\x1b[H未同步的新内容",
        1,
    )
    .await;
    cx.update(|cx| assert!(!terminal.read(cx).screen_text(cx).contains("未同步的新内容")));
    cx.executor().advance_clock(Duration::from_millis(350));
    cx.run_until_parked();
    in_frame(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(terminal.read(cx).screen_text(cx).contains("未同步的新内容"));
    });
}

#[gpui_kit::test]
async fn sidebar_resize_does_not_hold_normal_shell_or_serial_output(cx: &mut TestAppContext) {
    for (serial, initial) in [(false, "shell ready"), (true, INITIAL)] {
        let (handle, _workspace, terminal, factory, sender) =
            open_redraw(cx, serial, initial).await;
        in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
        output(
            cx,
            handle,
            &terminal,
            &sender,
            "\r\n立即显示串口或普通输出",
            1,
        )
        .await;
        cx.update(|cx| {
            assert!(
                terminal
                    .read(cx)
                    .screen_text(cx)
                    .contains("立即显示串口或普通输出")
            )
        });
        assert!(factory.writes.lock().unwrap().is_empty());
    }
}

#[gpui_kit::test]
async fn sidebar_resize_releases_on_input_disconnect_and_reconnect(cx: &mut TestAppContext) {
    let (handle, workspace, terminal, factory, sender) = open_redraw(cx, false, INITIAL).await;
    in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
    output(cx, handle, &terminal, &sender, "\x1b[2J\x1b[H等待输入", 1).await;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1u64), cx);
        window.input("x", cx);
    });
    cx.update(|cx| assert!(terminal.read(cx).screen_text(cx).contains("等待输入")));
    output(cx, handle, &terminal, &sender, INITIAL, 2).await;
    in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
    sender
        .send_blocking(TerminalTransportEvent::Exited {
            code: 0,
            signal: None,
        })
        .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        !terminal.read(cx).lifecycle(cx).accepts_input()
    })
    .await;
    cx.update(|cx| assert!(terminal.read(cx).screen_text(cx).contains("进程已退出")));
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ReconnectTerminal(RemoteTerminalId(1))), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(1))
            .unwrap()
            .read(cx)
            .terminal()
            .read(cx)
            .screen_text(cx)
            .contains("ready")
            && terminal.read(cx).generation(cx) == 2
    })
    .await;
    assert!(
        factory
            .writes
            .lock()
            .unwrap()
            .iter()
            .any(|bytes| bytes == b"x")
    );
}
