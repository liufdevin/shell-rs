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
