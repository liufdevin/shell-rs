use crate::support::*;
use shellrs::app::{ToggleTool, ToolKind};
use shellrs::host::SerialSettings;
use shellrs::serial::{SerialToolCommand, SerialToolPage};

fn page(cx: &mut TestAppContext, handle: WindowHandle<Root>, page: SerialToolPage) {
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(shellrs::app::SerialToolAction(SerialToolCommand::Page(
                page,
            ))),
            cx,
        );
    });
}

async fn connect(cx: &mut TestAppContext, handle: WindowHandle<Root>, host: HostId, name: &str) {
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(host)), cx)
    });
    let label = format!("已连接 {name}");
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        window
            .try_find("status-connection")
            .is_some_and(|status| status.label() == Some(label.as_str()))
    })
    .await;
}

#[derive(Default)]
struct SerialToolFactory {
    starts: Arc<AtomicUsize>,
    events: Arc<Mutex<Option<async_channel::Sender<TerminalTransportEvent>>>>,
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
    bauds: Arc<Mutex<Vec<u32>>>,
}

impl SerialToolFactory {
    fn output(&self, bytes: &[u8]) {
        self.events
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send_blocking(TerminalTransportEvent::Output(bytes.to_vec()))
            .unwrap();
    }
}

impl TerminalTransportFactory for SerialToolFactory {
    fn create(&self) -> Box<dyn TerminalTransport> {
        Box::new(SerialToolTransport {
            starts: self.starts.clone(),
            events: self.events.clone(),
            writes: self.writes.clone(),
            bauds: self.bauds.clone(),
        })
    }
}

struct SerialToolTransport {
    starts: Arc<AtomicUsize>,
    events: Arc<Mutex<Option<async_channel::Sender<TerminalTransportEvent>>>>,
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
    bauds: Arc<Mutex<Vec<u32>>>,
}

impl TerminalTransport for SerialToolTransport {
    fn run(
        self: Box<Self>,
        _: TerminalSize,
        commands: mpsc::Receiver<TerminalTransportCommand>,
        events: async_channel::Sender<TerminalTransportEvent>,
    ) -> anyhow::Result<()> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        *self.events.lock().unwrap() = Some(events.clone());
        events.send_blocking(TerminalTransportEvent::Started)?;
        while let Ok(command) = commands.recv() {
            match command {
                TerminalTransportCommand::WriteData(bytes)
                | TerminalTransportCommand::Write(bytes) => {
                    self.writes.lock().unwrap().push(bytes.clone());
                    events.send_blocking(TerminalTransportEvent::Output(bytes))?;
                }
                TerminalTransportCommand::SetSerialBaud { baud, reply } => {
                    self.bauds.lock().unwrap().push(baud);
                    let _ = reply.send(Ok(()));
                }
                TerminalTransportCommand::SerialWriteChecked { bytes, reply } => {
                    self.writes.lock().unwrap().push(bytes);
                    let _ = reply.send(Ok(()));
                }
                TerminalTransportCommand::Shutdown => break,
                _ => {}
            }
        }
        Ok(())
    }
}

#[gpui_kit::test]
async fn serial_tools_share_the_connection_capture_reply_send_and_reset_on_reconnect(
    cx: &mut TestAppContext,
) {
    let mut store = HostStore::empty();
    let host = store.insert_unnotified(HostDraft::serial(
        "console",
        "TEST-NOT-A-PORT",
        SerialSettings::default(),
        None,
    ));
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    cx.simulate_window_resize(handle.into(), size(px(1280.), px(1900.)));
    connect(cx, handle, host, "console").await;
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
        workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(1))
            .unwrap()
            .read(cx)
            .lifecycle(cx)
            == TerminalLifecycle::Running
    })
    .await;
    in_frame(cx, handle, |window, cx| window.click("tool-serial", cx));
    in_frame(cx, handle, |window, cx| {
        window.click(("serial-hex-receive", 1u64), cx);
        window.click(("serial-hex-transmit", 1u64), cx);
        window.click("serial-tool-capture", cx);
    });
    factory.output(&[0xaa, 0x55]);
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        window
            .find("serial-tool-preview")
            .label()
            .is_some_and(|text| text.contains("AA 55"))
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-copy", cx)
    });
    assert_eq!(
        cx.update(|cx| cx.read_from_clipboard().unwrap().text().unwrap()),
        "AA 55"
    );
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-use-capture", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-reply", cx);
        window.input("AA 55", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-auto", cx)
    });
    factory.output(&[0xaa]);
    factory.output(&[0x55]);
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.writes.lock().unwrap().len() == 1
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("serial-tool-auto").checked(), Some(true))
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-auto", cx)
    });
    page(cx, handle, SerialToolPage::Transfer);
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-message", cx);
        window.input("00 FF 0D 0A", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-send", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.writes.lock().unwrap().len() == 2
    })
    .await;
    assert_eq!(factory.writes.lock().unwrap()[1], vec![0, 0xff, 13, 10]);
    page(cx, handle, SerialToolPage::Connection);
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-baud", cx);
        window.input("230400", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-apply-baud", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        !factory.bauds.lock().unwrap().is_empty()
    })
    .await;
    assert_eq!(*factory.bauds.lock().unwrap(), vec![230400]);
    assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
    page(cx, handle, SerialToolPage::Reply);
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-auto", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("serial-tool-auto").checked(), Some(true))
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ReconnectTerminal(RemoteTerminalId(1))), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.starts.load(Ordering::SeqCst) == 2
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("serial-tool-auto").checked(), Some(false));
    });
    page(cx, handle, SerialToolPage::Transfer);
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("serial-tool-capture").checked(), Some(false))
    });
}

#[gpui_kit::test]
async fn serial_tools_validate_data_and_keep_drafts_separate_between_tabs(cx: &mut TestAppContext) {
    let mut store = HostStore::empty();
    let first = store.insert_unnotified(HostDraft::serial(
        "first",
        "TEST-A",
        SerialSettings::default(),
        None,
    ));
    let second = store.insert_unnotified(HostDraft::serial(
        "second",
        "TEST-B",
        SerialSettings::default(),
        None,
    ));
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, _) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    cx.simulate_window_resize(handle.into(), size(px(1280.), px(1900.)));
    connect(cx, handle, first, "first").await;
    in_frame(cx, handle, |window, cx| window.click("tool-serial", cx));
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-message", cx);
        window.input("draft for first", cx);
    });
    connect(cx, handle, second, "second").await;
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ToggleTool(ToolKind::Serial)), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-send", cx)
    });
    assert!(factory.writes.lock().unwrap().is_empty());
    page(cx, handle, SerialToolPage::Reply);
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-auto", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("serial-tool-auto").checked(), Some(false));
        assert!(
            window
                .find("serial-tool-status")
                .label()
                .is_some_and(|label| !label.is_empty())
        );
    });
    assert!(factory.writes.lock().unwrap().is_empty());
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", 1u64), cx)
    });
    page(cx, handle, SerialToolPage::Transfer);
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-send", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.writes.lock().unwrap().as_slice() == [b"draft for first".to_vec()]
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-repeat", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", 2u64), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("serial-tool-repeat").checked(), Some(false))
    });
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    assert_eq!(factory.writes.lock().unwrap().len(), 1);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", 1u64), cx)
    });
    cx.simulate_window_resize(handle.into(), size(px(960.), px(700.)));
    in_frame(cx, handle, |window, _| {
        let receive = window.find(("serial-hex-receive", 1u64)).bounds();
        let send = window.find(("serial-hex-transmit", 1u64)).bounds();
        assert!((receive.top() - send.top()).abs() <= px(1.));
        assert!(send.left() >= receive.right());
    });
    page(cx, handle, SerialToolPage::Connection);
    in_frame(cx, handle, |window, _| {
        let baud = window.find("serial-tool-baud");
        assert!(baud.visible());
        assert!(baud.bounds().size.width > px(0.));
    });
}

#[gpui_kit::test]
async fn serial_tools_newline_crc_repeat_and_disconnect_share_the_host_transport(
    cx: &mut TestAppContext,
) {
    let mut store = HostStore::empty();
    let host = store.insert_unnotified(HostDraft::serial(
        "console",
        "TEST-NOT-A-PORT",
        SerialSettings::default(),
        None,
    ));
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    connect(cx, handle, host, "console").await;
    in_frame(cx, handle, |window, cx| window.click("tool-serial", cx));
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-message", cx);
        window.input("123456789", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("serial-tool-ending", 3u64), cx);
        window.click("serial-tool-crc", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-send", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.writes.lock().unwrap().len() == 1
    })
    .await;
    assert_eq!(factory.writes.lock().unwrap()[0], b"123456789\x37\x4b\r\n");
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-repeat", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.writes.lock().unwrap().len() >= 2
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("serial-tool-repeat").checked(), Some(true))
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(DisconnectTerminal(RemoteTerminalId(1))), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
        workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(1))
            .unwrap()
            .read(cx)
            .lifecycle(cx)
            != TerminalLifecycle::Running
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("serial-tool-repeat").checked(), Some(false))
    });
    assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
    assert!(
        factory
            .writes
            .lock()
            .unwrap()
            .iter()
            .all(|bytes| bytes == b"123456789\x37\x4b\r\n")
    );
}

#[gpui_kit::test]
async fn serial_assistant_defaults_off_saves_groups_runs_in_order_and_stops_when_hidden(
    cx: &mut TestAppContext,
) {
    let mut store = HostStore::empty();
    let host = store.insert_unnotified(HostDraft::serial(
        "console",
        "TEST-NOT-A-PORT",
        SerialSettings::default(),
        None,
    ));
    let key = format!("host:{}", store.host(host).unwrap().public_id);
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    cx.simulate_window_resize(handle.into(), size(px(1600.), px(2400.)));
    connect(cx, handle, host, "console").await;
    in_frame(cx, handle, |window, cx| window.click("tool-serial", cx));
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("serial-assistant-enable").checked(),
            Some(false)
        );
        assert!(window.try_find("serial-assistant").is_none());
        window.click("serial-assistant-enable", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("serial-assistant").visible());
        assert!(window.try_find("serial-tool-preview").is_none());
        window.click("serial-assistant-group-name", cx);
        window.input("开发板", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-create-group", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-command-name", cx);
        window.input("启动", cx);
        window.click("serial-tool-message", cx);
        window.input("hello", cx);
        window.click("serial-assistant-ending-crlf", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-save-command", cx)
    });
    let saved = cx.read(|cx| {
        workspace
            .read(cx)
            .store()
            .read(cx)
            .serial_assistant(&key)
            .unwrap()
            .clone()
    });
    assert!(saved.enabled);
    assert_eq!(saved.groups[0].commands[0].content, "hello");
    let mut configured = saved.clone();
    let mut hex = configured.groups[0].commands[0].clone();
    hex.id = configured.allocate_id();
    hex.name = "binary".into();
    hex.content = "00 FF".into();
    hex.hex = true;
    hex.ending = shellrs::host::SerialCommandEnding::None;
    hex.delay_ms = 0;
    configured.groups[0].commands.push(hex);
    let mut skipped = configured.groups[0].commands[0].clone();
    skipped.id = configured.allocate_id();
    skipped.enabled = false;
    configured.groups[0].commands.push(skipped);
    cx.update(|cx| {
        workspace.read(cx).store().clone().update(cx, |store, cx| {
            store.set_serial_assistant(key.clone(), Some(host), configured, cx)
        });
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-run", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        let status = window.find("serial-tool-status");
        assert!(status.label().is_none_or(|status| status.contains("正在顺序发送") || status.contains("发送完成")), "Unexpected group status: {:?}", status.label());
        factory.writes.lock().unwrap().len() == 2
    })
    .await;
    assert_eq!(
        factory.writes.lock().unwrap().as_slice(),
        [b"hello\r\n".to_vec(), vec![0, 255]]
    );
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window.find("serial-tool-status").label() == Some("命令组发送完成。")
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-run", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.writes.lock().unwrap().len() == 3
    })
    .await;
    in_frame(cx, handle, |window, cx| window.click("tool-serial", cx));
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(factory.writes.lock().unwrap().len(), 3);
    in_frame(cx, handle, |window, cx| window.click("tool-serial", cx));
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("serial-assistant-enable").checked(), Some(true));
        window.click("serial-assistant-enable", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("serial-assistant").is_none());
        assert!(window.find("serial-tool-preview").visible());
        let receive = window.find(("serial-hex-receive", 1u64)).bounds();
        let send = window.find(("serial-hex-transmit", 1u64)).bounds();
        assert_eq!(receive.top(), send.top());
    });
    assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
}

#[gpui_kit::test]
async fn enabling_assistant_from_connection_opens_its_layout_and_detached_windows_keep_the_tools(
    cx: &mut TestAppContext,
) {
    use shellrs::app::{CenterTab, DetachTab};
    let mut store = HostStore::empty();
    let host = store.insert_unnotified(HostDraft::serial(
        "console",
        "TEST-NOT-A-PORT",
        SerialSettings::default(),
        None,
    ));
    let factory = Arc::new(SerialToolFactory::default());
    let (main, workspace) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    cx.simulate_window_resize(main.into(), size(px(1280.), px(800.)));
    connect(cx, main, host, "console").await;
    in_frame(cx, main, |window, cx| window.click("tool-serial", cx));
    page(cx, main, SerialToolPage::Connection);
    in_frame(cx, main, |window, cx| {
        assert!(window.find("serial-tool-baud").visible());
        window.click("serial-assistant-enable", cx);
    });
    in_frame(cx, main, |window, _| {
        assert!(window.find("serial-assistant-group-name").visible());
        assert!(window.try_find("serial-tool-baud").is_none());
    });
    in_frame(cx, main, |window, cx| {
        window.click(("terminal", 1u64), cx);
        window.dispatch_action(
            Box::new(DetachTab(CenterTab::Terminal(RemoteTerminalId(1)))),
            cx,
        );
    });
    let detached = cx.update(|cx| {
        cx.windows()
            .into_iter()
            .find(|window| window.window_id() != main.window_id())
            .unwrap()
            .downcast::<Root>()
            .unwrap()
    });
    cx.simulate_window_resize(detached.into(), size(px(1100.), px(800.)));
    in_frame(cx, detached, |window, cx| {
        assert!(window.find("detached-serial-tools").visible());
        assert_eq!(window.find("serial-assistant-enable").checked(), Some(true));
        assert!(window.find("serial-assistant-group-name").visible());
        let terminal = window.find(("terminal", 1u64)).bounds();
        let tools = window.find("detached-serial-tools").bounds();
        assert!(terminal.right() <= tools.left());
        window.click("serial-assistant-group-name", cx);
        window.input("from detached", cx);
    });
    in_frame(cx, detached, |window, cx| {
        window.click("serial-assistant-create-group", cx)
    });
    cx.read(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let key = format!("host:{}", store.host(host).unwrap().public_id);
        assert_eq!(
            store.serial_assistant(&key).unwrap().groups[0].name,
            "from detached"
        );
    });
    in_frame(cx, detached, |window, cx| {
        window.click(("serial-hex-transmit", 1u64), cx)
    });
    in_frame(cx, detached, |window, cx| {
        assert_eq!(
            window.find(("serial-hex-transmit", 1u64)).checked(),
            Some(true)
        );
        window.click("tool-serial", cx);
    });
    in_frame(cx, detached, |window, cx| {
        assert!(window.try_find("detached-serial-tools").is_none());
        window.click("tool-serial", cx);
    });
    page(cx, detached, SerialToolPage::Reply);
    in_frame(cx, detached, |window, cx| {
        window.click("serial-assistant-enable", cx)
    });
    in_frame(cx, detached, |window, _| {
        assert_eq!(
            window.find("serial-assistant-enable").checked(),
            Some(false)
        );
        assert!(window.find("serial-tool-preview").visible());
    });
    in_frame(cx, detached, |window, cx| {
        window.click("return-detached-tab", cx)
    });
    in_frame(cx, main, |window, _| {
        assert!(window.find(("terminal", 1u64)).visible());
        assert_eq!(
            window.find("serial-assistant-enable").checked(),
            Some(false)
        );
        assert!(window.find("serial-tool-preview").visible());
    });
    assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
}

#[gpui_kit::test]
async fn serial_assistant_uses_the_remaining_sidebar_height_at_normal_window_size(
    cx: &mut TestAppContext,
) {
    let mut store = HostStore::empty();
    let host = store.insert_unnotified(HostDraft::serial(
        "console",
        "TEST-NOT-A-PORT",
        SerialSettings::default(),
        None,
    ));
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, _) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory)),
    );
    cx.simulate_window_resize(handle.into(), size(px(1280.), px(800.)));
    connect(cx, handle, host, "console").await;
    in_frame(cx, handle, |window, cx| window.click("tool-serial", cx));
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-enable", cx)
    });
    in_frame(cx, handle, |window, _| {
        let assistant = window.find("serial-assistant").bounds();
        let commands = window.find("serial-assistant-commands").bounds();
        let message = window.find("serial-tool-message").bounds();
        let status = window.find("serial-tool-status").bounds();
        assert!(assistant.size.height > px(450.));
        assert!(commands.size.height > px(120.));
        assert!(message.size.height >= px(60.));
        assert!(commands.bottom() <= message.top() + px(1.));
        assert!(message.top() < status.top());
        assert!(window.find("serial-assistant-create-group").visible());
        assert!(window.find("serial-assistant-rename-group").visible());
        assert!(window.find("serial-assistant-delete-group").visible());
    });
}

#[gpui_kit::test]
async fn default_serial_tools_expand_capture_and_keep_sending_controls_separate(
    cx: &mut TestAppContext,
) {
    let mut store = HostStore::empty();
    let host = store.insert_unnotified(HostDraft::serial(
        "console",
        "TEST-NOT-A-PORT",
        SerialSettings::default(),
        None,
    ));
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, _) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    cx.simulate_window_resize(handle.into(), size(px(1280.), px(800.)));
    connect(cx, handle, host, "console").await;
    in_frame(cx, handle, |window, cx| window.click("tool-serial", cx));
    let (preview_height, sender_height, sender_bottom) = in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("serial-assistant-enable").checked(),
            Some(false)
        );
        let preview = window.find("serial-tool-preview").bounds();
        let sender = window.find("serial-default-sender").bounds();
        let message = window.find("serial-tool-message").bounds();
        let send = window.find("serial-tool-send").bounds();
        let status = window.find("serial-tool-status").bounds();
        assert!(preview.size.height > px(100.), "preview: {preview:?}");
        assert!(message.size.height >= px(60.));
        assert!(preview.bottom() < sender.top());
        assert!(sender.bottom() <= status.top());
        assert!(window.find("serial-tool-repeat").visible());
        assert!((send.size.width - message.size.width).abs() < px(1.));
        window.click("serial-tool-message", cx);
        window.input("still sends normally", cx);
        (preview.size.height, sender.size.height, sender.bottom())
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-send", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.writes.lock().unwrap().as_slice() == [b"still sends normally".to_vec()]
    })
    .await;
    cx.simulate_window_resize(handle.into(), size(px(1280.), px(1100.)));
    in_frame(cx, handle, |window, _| {
        let preview = window.find("serial-tool-preview").bounds();
        let sender = window.find("serial-default-sender").bounds();
        assert!(preview.size.height > preview_height + px(200.));
        assert!((sender.size.height - sender_height).abs() < px(1.));
        assert!(sender.bottom() > sender_bottom + px(200.));
    });
    cx.simulate_window_resize(handle.into(), size(px(960.), px(700.)));
    page(cx, handle, SerialToolPage::Reply);
    in_frame(cx, handle, |window, _| {
        let automatic = window.find("serial-tool-auto").bounds();
        let hex = window.find("serial-tool-hex").bounds();
        let pattern = window.find("serial-tool-pattern").bounds();
        let reply = window.find("serial-tool-reply").bounds();
        assert!(hex.top() > automatic.bottom());
        assert!(pattern.top() > hex.bottom());
        assert!(reply.top() > pattern.bottom());
        assert!(pattern.size.height >= px(28.));
    });
    page(cx, handle, SerialToolPage::Connection);
    in_frame(cx, handle, |window, _| {
        let baud = window.find("serial-tool-baud").bounds();
        let apply = window.find("serial-tool-apply-baud").bounds();
        assert!(apply.top() > baud.bottom());
        assert!((baud.size.width - apply.size.width).abs() < px(1.));
        assert!(window.find("serial-tool-apply-baud").visible());
    });
    cx.simulate_window_resize(handle.into(), size(px(960.), px(500.)));
    page(cx, handle, SerialToolPage::Transfer);
    in_frame(cx, handle, |window, cx| {
        window.scroll(
            "serial-tool-body",
            gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-1000.))),
            cx,
        );
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.find("serial-tool-send").visible());
        assert!(window.find("serial-tool-repeat").visible());
        let receive = window.find(("serial-hex-receive", 1u64)).bounds();
        let transmit = window.find(("serial-hex-transmit", 1u64)).bounds();
        assert_eq!(receive.top(), transmit.top());
    });
    assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
}
