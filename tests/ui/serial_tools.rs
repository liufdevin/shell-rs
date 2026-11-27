use crate::support::*;
use shellrs::host::{SerialSettings, SnippetDraft, SnippetStep, SnippetWait};
use shellrs::serial::{SerialToolCommand, SerialToolPage};
use std::sync::atomic::AtomicBool;

struct AssistantPorts;

impl shellrs::serial::SerialPortSource for AssistantPorts {
    fn ports(&self) -> std::io::Result<Vec<String>> {
        Ok(vec!["COM5".into(), "COM7".into()])
    }
}

async fn open_assistant(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
) {
    let presence = workspace.read_with(cx, |workspace, _| workspace.serial_presence().clone());
    presence.update(cx, |presence, cx| {
        presence.start(Arc::new(AssistantPorts), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("open-serial-assistant", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window
            .try_find("serial-assistant-port")
            .is_some_and(|port| port.value() == Some("COM5"))
    })
    .await;
}

fn page(cx: &mut TestAppContext, handle: WindowHandle<Root>, page: SerialToolPage) {
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(shellrs::app::SerialAssistantAction(
                shellrs::serial::SerialAssistantTabCommand::Tool(SerialToolCommand::Page(page)),
            )),
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
    checked_error: Arc<Mutex<Option<String>>>,
    isp_jobs: Arc<Mutex<Vec<IspCapture>>>,
    hold_isp: Arc<AtomicBool>,
}

struct IspCapture {
    stc: bool,
    image: Option<Vec<u8>>,
    address: Option<u32>,
    baud: u32,
}

impl SerialToolFactory {
    fn diagnostics(&self, received: u64, transmitted: u64) {
        let mut snapshot = shellrs::serial::SerialDiagnostics::default();
        snapshot.configuration = "115200 8N1 · DTR=Asserted".into();
        snapshot.received = received;
        snapshot.transmitted = transmitted;
        snapshot.first_received_ms = Some(25);
        snapshot.errors = 1;
        snapshot.last_error = Some("Windows CE=0x0004".into());
        self.events
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send_blocking(TerminalTransportEvent::SerialDiagnostics(snapshot))
            .unwrap();
    }
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
            checked_error: self.checked_error.clone(),
            isp_jobs: self.isp_jobs.clone(),
            hold_isp: self.hold_isp.clone(),
        })
    }
}

struct SerialToolTransport {
    starts: Arc<AtomicUsize>,
    events: Arc<Mutex<Option<async_channel::Sender<TerminalTransportEvent>>>>,
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
    bauds: Arc<Mutex<Vec<u32>>>,
    checked_error: Arc<Mutex<Option<String>>>,
    isp_jobs: Arc<Mutex<Vec<IspCapture>>>,
    hold_isp: Arc<AtomicBool>,
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
        let mut diagnostics = shellrs::serial::SerialDiagnostics::default();
        diagnostics.configuration = "115200 8N1".into();
        events.send_blocking(TerminalTransportEvent::SerialDiagnostics(diagnostics))?;
        while let Ok(command) = commands.recv() {
            match command {
                TerminalTransportCommand::SerialIsp(request) => {
                    self.isp_jobs.lock().unwrap().push(IspCapture {
                        stc: request.is_stc(),
                        image: request.firmware_bytes().map(ToOwned::to_owned),
                        address: request.firmware_address(),
                        baud: request.baud_rate(),
                    });
                    request.identify("TEST chip".into());
                    request.report(20, "TEST flashing".into());
                    while self.hold_isp.load(Ordering::Acquire) && !request.cancelled() {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    let result = if request.cancelled() {
                        Err("TEST ISP stopped".into())
                    } else {
                        self.checked_error
                            .lock()
                            .unwrap()
                            .take()
                            .map_or(Ok(()), Err)
                    };
                    request.finish(result);
                }
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
                    let result = self
                        .checked_error
                        .lock()
                        .unwrap()
                        .take()
                        .map_or(Ok(()), Err);
                    let _ = reply.send(result);
                }
                TerminalTransportCommand::CommandGroupWrite {
                    id,
                    bytes,
                    cancelled,
                } => {
                    let success = !cancelled.load(Ordering::Acquire);
                    if success {
                        self.writes.lock().unwrap().push(bytes.to_vec());
                    }
                    events.send_blocking(TerminalTransportEvent::CommandGroupWritten {
                        id,
                        success,
                    })?;
                }
                TerminalTransportCommand::Shutdown => break,
                _ => {}
            }
        }
        Ok(())
    }
}

#[gpui_kit::test]
async fn snippet_command_groups_are_edited_and_sent_in_order(cx: &mut TestAppContext) {
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
    in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
    in_frame(cx, handle, |window, cx| window.click("snippets-new", cx));
    in_frame(cx, handle, |window, cx| {
        window.input("开发板命令组", cx);
        window.within("snippet-type").click("input", cx);
    });
    for key in ["down", "enter"] {
        in_frame(cx, handle, |window, cx| window.press(key, cx));
    }
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("snippet-type").value(), Some("命令组"));
        window.click(("snippet-step-content", 1_u64), cx);
        window.input("operator", cx);
        window
            .within(("snippet-step-wait", 1_u64))
            .click("input", cx);
    });
    for key in ["down", "down", "enter"] {
        in_frame(cx, handle, |window, cx| window.press(key, cx));
    }
    in_frame(cx, handle, |window, cx| {
        window.click(("snippet-step-prompt", 1_u64), cx);
        window.input("User ID:", cx);
        window.click("snippet-add-step", cx);
    });
    in_frame(cx, handle, |window, cx| {
        let position = window.find("snippet-type").bounds().center();
        window.dispatch_event(
            gpui_kit::ScrollEvent {
                position,
                delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-400.))),
                modifiers: gpui_kit::Modifiers::default(),
                touch_phase: gpui_kit::TouchPhase::Moved,
                source: gpui_kit::ScrollSource::Pointer,
            }
            .to_platform_input(),
            cx,
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("snippet-step-content", 2_u64), cx);
        window.input("next-command", cx);
        window.click(("snippet-step-enter", 2_u64), cx);
        window
            .within(("snippet-step-wait", 2_u64))
            .click("input", cx);
    });
    for key in ["down", "enter"] {
        in_frame(cx, handle, |window, cx| window.press(key, cx));
    }
    in_frame(cx, handle, |window, cx| {
        let position = window.find(("snippet-step-wait", 2_u64)).bounds().center();
        window.dispatch_event(
            gpui_kit::ScrollEvent {
                position,
                delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-200.))),
                modifiers: gpui_kit::Modifiers::default(),
                touch_phase: gpui_kit::TouchPhase::Moved,
                source: gpui_kit::ScrollSource::Pointer,
            }
            .to_platform_input(),
            cx,
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("snippet-step-seconds", 2_u64), cx);
        window.press("secondary-a", cx);
        window.input("0.2", cx);
        window.click("commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window.try_find(("snippet-run", 1_u64)).is_some()
    })
    .await;
    workspace.read_with(cx, |workspace, cx| {
        let steps = workspace.store().read(cx).snippets()[0]
            .steps
            .as_ref()
            .unwrap();
        assert_eq!(steps.len(), 2);
        assert!(!steps[1].enter);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("serial-login-open").is_none());
        window.click(("snippet-run", 1_u64), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window
            .try_find("command-group-status")
            .is_some_and(|status| status.label().is_some_and(|text| text.contains("User ID:")))
    })
    .await;
    assert!(factory.writes.lock().unwrap().is_empty());
    factory.output(b"User ");
    factory.output(b"ID: ");
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window
            .try_find("command-group-status")
            .is_some_and(|status| status.label().is_some_and(|text| text.contains("0.2")))
    })
    .await;
    assert_eq!(
        factory.writes.lock().unwrap().as_slice(),
        [b"operator\r".to_vec()]
    );
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window
            .try_find("command-group-status")
            .is_some_and(|status| {
                status
                    .label()
                    .is_some_and(|label| label == "全部步骤已发送")
            })
    })
    .await;
    assert_eq!(
        factory.writes.lock().unwrap().as_slice(),
        [b"operator\r".to_vec(), b"next-command".to_vec()]
    );
    in_frame(cx, handle, |window, cx| {
        window.click(("snippet-run", 1_u64), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("command-group-stop", cx)
    });
    factory.output(b"\r\nUser ID:");
    cx.run_until_parked();
    assert_eq!(factory.writes.lock().unwrap().len(), 2);
}

#[gpui_kit::test]
async fn command_group_running_strip_and_replace_confirmation_cover_clicks_and_shortcuts(
    cx: &mut TestAppContext,
) {
    let mut store = HostStore::empty();
    let host = store.insert_unnotified(HostDraft::serial(
        "console",
        "TEST-NOT-A-PORT",
        SerialSettings::default(),
        None,
    ));
    store.insert_snippet_unnotified(
        SnippetDraft::new("login", "", None)
            .with_steps(vec![SnippetStep::new(
                "operator",
                SnippetWait::Prompt {
                    text: "login:".into(),
                    timeout_ms: 60_000,
                },
                true,
            )])
            .unwrap(),
    );
    store.insert_snippet_unnotified(
        SnippetDraft::new("next", "", None)
            .with_steps(vec![SnippetStep::new(
                "next-command",
                SnippetWait::Immediate,
                true,
            )])
            .unwrap(),
    );
    let next_public_id = store
        .snippets()
        .iter()
        .find(|snippet| snippet.name == "next")
        .unwrap()
        .sync_id
        .to_string();
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    connect(cx, handle, host, "console").await;
    cx.update(|cx| {
        let settings = workspace.read(cx).settings().clone();
        settings.update(cx, |store, cx| {
            store.update(
                |settings| {
                    settings.snippet_shortcuts.set(
                        next_public_id.clone(),
                        Some(shellrs::app::SnippetShortcut::new("ctrl-2".into(), true)),
                    )
                },
                cx,
            )
        });
    });

    in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
    in_frame(cx, handle, |window, cx| {
        window.click(("snippet-run", 1_u64), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window
            .try_find(("terminal-command-group-status", 1_u64))
            .is_some_and(|status| {
                status
                    .label()
                    .is_some_and(|label| label.contains("login") && label.contains("login:"))
            })
    })
    .await;

    // The strip belongs to the terminal, not the currently selected tool.
    in_frame(cx, handle, |window, cx| window.click("tool-utilities", cx));
    in_frame(cx, handle, |window, _| {
        assert!(window.find(("terminal-command-group", 1_u64)).visible());
        assert!(
            window
                .find(("terminal-command-group-stop", 1_u64))
                .visible()
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-command-group-stop", 1_u64), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window.try_find(("terminal-command-group", 1_u64)).is_none()
    })
    .await;

    in_frame(cx, handle, |window, cx| {
        window.click("tool-snippets", cx);
        window.click(("snippet-run", 1_u64), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window.try_find(("terminal-command-group", 1_u64)).is_some()
    })
    .await;

    // Clicking another group asks first; cancelling keeps the current group.
    in_frame(cx, handle, |window, cx| {
        window.click(("snippet-run", 2_u64), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window
            .try_find("ok")
            .is_some_and(|button| button.label() == Some("停止并运行"))
    })
    .await;
    in_frame(cx, handle, |window, cx| window.click("cancel", cx));
    cx.run_until_parked();
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("ok").is_none());
        assert!(window.find(("terminal-command-group", 1_u64)).visible());
    });
    assert!(factory.writes.lock().unwrap().is_empty());

    // A shortcut takes the same confirmation path.
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1_u64), cx);
        window.dispatch_action(
            Box::new(shellrs::app::InsertSnippet(next_public_id.clone())),
            cx,
        );
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window
            .try_find("ok")
            .is_some_and(|button| button.label() == Some("停止并运行"))
    })
    .await;
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window.try_find(("terminal-command-group", 1_u64)).is_none()
            && factory.writes.lock().unwrap().as_slice() == [b"next-command\r".to_vec()]
    })
    .await;
}

#[gpui_kit::test]
async fn serial_diagnostics_work_before_capture_and_reset_when_reconnecting(
    cx: &mut TestAppContext,
) {
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    open_assistant(cx, handle, &workspace).await;
    connect_assistant(cx, handle).await;
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window
            .try_find("serial-wire-count")
            .is_some_and(|item| item.label() == Some("RX 0 字节 · TX 0 字节 · 错误 0"))
    })
    .await;
    factory.diagnostics(1234, 56);
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window
            .try_find("serial-wire-count")
            .is_some_and(|item| item.label() == Some("RX 1234 字节 · TX 56 字节 · 错误 1"))
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-connect", cx)
    });
    connect_assistant(cx, handle).await;
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, _| {
        factory.starts.load(Ordering::SeqCst) == 2
    })
    .await;
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window
            .try_find("serial-wire-count")
            .is_some_and(|item| item.label() == Some("RX 0 字节 · TX 0 字节 · 错误 0"))
    })
    .await;
}

#[gpui_kit::test]
async fn serial_assistant_auto_reply_uses_the_open_port_and_resets_on_reconnect(
    cx: &mut TestAppContext,
) {
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    open_assistant(cx, handle, &workspace).await;
    connect_assistant(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-edit-groups", cx)
    });
    page(cx, handle, SerialToolPage::Reply);
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-pattern", cx);
        window.input("PING", cx);
        window.click("serial-tool-reply", cx);
        window.input("PONG", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-auto", cx)
    });
    factory.output(b"PI");
    factory.output(b"NG");
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.writes.lock().unwrap().len() == 1
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("serial-tool-auto").checked(), Some(true))
    });
    assert_eq!(
        factory.writes.lock().unwrap().as_slice(),
        [b"PONG".to_vec()]
    );
    assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-back", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-connect", cx)
    });
    connect_assistant(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-edit-groups", cx)
    });
    page(cx, handle, SerialToolPage::Reply);
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("serial-tool-auto").checked(), Some(false));
    });
}

async fn connect_assistant(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-connect", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window.find("serial-assistant-connection-status").label() == Some("已连接")
    })
    .await;
}

#[gpui_kit::test]
async fn serial_assistant_rejects_invalid_hex_without_sending(cx: &mut TestAppContext) {
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    open_assistant(cx, handle, &workspace).await;
    connect_assistant(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-send-hex", cx);
        window.click("serial-tool-message", cx);
        window.input("invalid HEX", cx);
        window.press("enter", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .find("serial-tool-status")
                .label()
                .is_some_and(|label| !label.is_empty())
        );
    });
    assert!(factory.writes.lock().unwrap().is_empty());
}

#[gpui_kit::test]
async fn serial_assistant_newline_crc_repeat_and_disconnect_share_the_transport(
    cx: &mut TestAppContext,
) {
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    open_assistant(cx, handle, &workspace).await;
    connect_assistant(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-message", cx);
        window.input("123456789", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-ending-crlf", cx);
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
        window.click("serial-assistant-connect", cx)
    });
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
async fn serial_assistant_saves_groups_runs_in_order_and_stops_when_hidden(
    cx: &mut TestAppContext,
) {
    let store = HostStore::empty();
    let key = "serial-assistant:COM5".to_owned();
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    open_assistant(cx, handle, &workspace).await;
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("serial-assistant-output").visible());
        assert!(window.try_find("serial-assistant-enable").is_none());
        window.click("serial-assistant-edit-groups", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("serial-assistant").visible());
        assert!(window.find("serial-command-editor").visible());
        assert!(window.try_find("serial-tool-preview").is_none());
        assert!(window.try_find("serial-tool-message").is_none());
        assert!(
            window
                .try_find("serial-assistant-command-content")
                .is_none()
        );
        window.click("serial-assistant-create-group", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-group-name", cx);
        window.input("开发板", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-save-group", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-new-command", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-command-name", cx);
        window.input("启动", cx);
        window.click("serial-assistant-command-content", cx);
        window.input("hello", cx);
        window.click("serial-assistant-ending-crlf", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-save-command", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .try_find("serial-assistant-command-content")
                .is_none()
        );
        assert!(window.try_find("serial-tool-message").is_none());
        assert!(
            window
                .find(("serial-assistant-select-command", 2_u64))
                .visible()
        );
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
            store.set_serial_assistant(key.clone(), None, configured, cx)
        });
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-back", cx);
        window.render_frame(cx);
        window.click("serial-assistant-connect", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window.find("serial-assistant-connection-status").label() == Some("已连接")
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-run-quick", cx)
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
    cx.update(|cx| {
        workspace.read(cx).store().clone().update(cx, |store, cx| {
            let mut library = store.serial_assistant(&key).unwrap().clone();
            library.groups[0].commands[0].delay_ms = 20_000;
            assert!(store.set_serial_assistant(key.clone(), None, library, cx));
        });
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-run-quick", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.writes.lock().unwrap().len() == 3
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-edit-groups", cx)
    });
    // Dock activation changes are deferred. Let the serial assistant see
    // that it is hidden before advancing past its pending command delay.
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(21));
    cx.run_until_parked();
    assert_eq!(factory.writes.lock().unwrap().len(), 3);
    in_frame(cx, handle, |window, cx| {
        window.click("open-serial-assistant", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.find("serial-assistant-output").visible());
        assert_eq!(
            window.find("serial-assistant-connection-status").label(),
            Some("已连接")
        );
    });
    assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
}

#[gpui_kit::test]
async fn serial_terminals_have_no_serial_tools_sidebar_in_main_or_detached_windows(
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
    let (main, _) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    cx.simulate_window_resize(main.into(), size(px(1280.), px(800.)));
    connect(cx, main, host, "console").await;
    in_frame(cx, main, |window, _| {
        assert!(window.try_find("tool-serial").is_none());
        assert!(window.try_find("serial-tools").is_none());
        assert!(window.find("open-serial-assistant").visible());
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
    in_frame(cx, detached, |window, _| {
        assert!(window.try_find("detached-serial-tools").is_none());
        assert!(window.try_find("tool-serial").is_none());
        assert!(window.find(("terminal", 1_u64)).visible());
    });
    in_frame(cx, detached, |window, cx| {
        window.click("return-detached-tab", cx)
    });
    in_frame(cx, main, |window, _| {
        assert!(window.find(("terminal", 1u64)).visible());
        assert!(window.try_find("serial-assistant-enable").is_none());
    });
    assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
}

#[gpui_kit::test]
async fn serial_assistant_reserves_the_main_area_for_received_output(cx: &mut TestAppContext) {
    init_app(cx);
    let (handle, workspace) = open_workspace(cx);
    cx.simulate_window_resize(handle.into(), size(px(1280.), px(800.)));
    open_assistant(cx, handle, &workspace).await;
    in_frame(cx, handle, |window, _| {
        let output = window.find("serial-assistant-output").bounds();
        let port = window.find("serial-assistant-port").bounds();
        let baud = window.find("serial-assistant-baud").bounds();
        let commands = window.find("serial-assistant-edit-groups").bounds();
        let message = window.find("serial-tool-message").bounds();
        let multi = window.find("serial-multi-send").bounds();
        assert!(output.size.height > px(350.), "output: {output:?}");
        assert!(output.bottom() <= message.top());
        assert!(message.bottom() <= port.top());
        assert_eq!(port.top(), baud.top());
        assert!(output.right() <= multi.left());
        assert!(commands.left() >= multi.left());
        assert!(commands.bottom() <= port.top());
    });
    let width = |cx: &mut TestAppContext| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.find("serial-multi-send").bounds().size.width
        })
        .unwrap()
    };
    let original = width(cx);
    in_frame(cx, handle, |window, cx| {
        let multi = window.find("serial-multi-send").bounds();
        let divider = point(multi.left(), multi.center().y);
        window.drag(divider, divider - point(px(60.), px(0.)), cx);
    });
    cx.run_until_parked();
    assert!(width(cx) > original + px(40.));
    in_frame(cx, handle, |window, _| {
        assert!(window.find("serial-assistant-loop").visible());
        assert!(window.find("serial-assistant-stop-quick").visible());
        assert!(window.find("serial-tool-send").visible());
    });
}

#[gpui_kit::test]
async fn serial_tools_hide_and_restore_with_the_selected_page_and_keep_tabs_on_one_row(
    cx: &mut TestAppContext,
) {
    init_app(cx);
    let (handle, workspace) = open_workspace(cx);
    cx.simulate_window_resize(handle.into(), size(px(1280.), px(800.)));
    open_assistant(cx, handle, &workspace).await;
    let visible_width = in_frame(cx, handle, |window, cx| {
        let multi = window.find("serial-page-multi").bounds();
        let stm = window.find("serial-page-stm32").bounds();
        let stc = window.find("serial-page-stc15").bounds();
        assert_eq!(multi.top(), stm.top());
        assert_eq!(stm.top(), stc.top());
        assert!(stc.right() <= window.find("serial-tools").bounds().right());
        assert!(window.find("serial-assistant-tools-toggle").visible());
        let width = window.find("serial-assistant-output").bounds().size.width;
        window.click("serial-page-stc15", cx);
        width
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("serial-isp").visible());
        assert!(window.find("serial-isp-python").visible());
        window.click("serial-assistant-tools-toggle", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("serial-tools").is_none());
        assert!(window.try_find("serial-isp").is_none());
        assert!(window.try_find("serial-multi-send").is_none());
        assert!(window.find("serial-assistant-tools-toggle").visible());
        assert!(
            window.find("serial-assistant-output").bounds().size.width > visible_width + px(180.)
        );
    });
    cx.read(|cx| {
        let settings =
            serde_json::to_value(workspace.read(cx).settings().read(cx).settings()).unwrap();
        assert_eq!(settings["serial_assistant"]["sidebar_visible"], false);
        assert_eq!(settings["serial_assistant"]["sidebar_page"], "stc15");
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-tools-toggle", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.find("serial-isp").visible());
        assert!(window.find("serial-isp-python").visible());
    });
    cx.read(|cx| {
        let settings =
            serde_json::to_value(workspace.read(cx).settings().read(cx).settings()).unwrap();
        assert_eq!(settings["serial_assistant"]["sidebar_visible"], true);
    });
}

#[gpui_kit::test]
async fn serial_assistant_editing_keeps_a_live_port_open_and_filters_command_groups(
    cx: &mut TestAppContext,
) {
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    open_assistant(cx, handle, &workspace).await;
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-connect", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window.find("serial-assistant-connection-status").label() == Some("已连接")
    })
    .await;
    for _ in 0..3 {
        in_frame(cx, handle, |window, cx| {
            window.click("serial-assistant-terminal", cx);
            window.click("serial-assistant-edit-groups", cx);
        });
        factory.output(b"reading during edit\r\n");
        in_frame(cx, handle, |window, cx| {
            let groups = window.find("serial-assistant-groups").bounds();
            let commands = window.find("serial-assistant-commands").bounds();
            assert!(groups.right() < commands.left());
            assert!(window.find("serial-assistant-group-search").visible());
            assert!(window.find("serial-command-editor-tab").visible());
            assert!(window.find("serial-assistant-tab").visible());
            window.click("serial-assistant-back", cx);
        });
    }
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-edit-groups", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-create-group", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-group-name", cx);
        window.input("开发板", cx);
        window.click("serial-assistant-save-group", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find(("serial-assistant-group", 1_u64)).visible());
        window.click("serial-assistant-group-search", cx);
        window.input("other", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find(("serial-assistant-group", 1_u64)).is_none());
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-back", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("serial-assistant-connection-status").label(),
            Some("已连接")
        );
    });
    assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
}

#[gpui_kit::test]
async fn serial_assistant_enter_sends_crlf_and_shift_enter_keeps_editing(cx: &mut TestAppContext) {
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    open_assistant(cx, handle, &workspace).await;
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-message", cx);
        window.input("first", cx);
        window.press("enter", cx);
    });
    assert!(factory.writes.lock().unwrap().is_empty());
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-connect", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window.find("serial-assistant-connection-status").label() == Some("已连接")
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-ending-crlf", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("serial-assistant-ending-crlf").checked(),
            Some(true)
        );
        window.click("serial-tool-message", cx);
        window.press("secondary-a", cx);
        window.input("first", cx);
        window.press("shift-enter", cx);
        window.input("second", cx);
    });
    assert!(factory.writes.lock().unwrap().is_empty());
    in_frame(cx, handle, |window, cx| window.press("enter", cx));
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        !factory.writes.lock().unwrap().is_empty()
    })
    .await;
    assert_eq!(
        factory.writes.lock().unwrap().as_slice(),
        [b"first\nsecond\r\n".to_vec()]
    );
}

#[gpui_kit::test]
async fn serial_command_editor_copies_reorders_and_loads_without_sending(cx: &mut TestAppContext) {
    use shellrs::app::CenterTab;
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    open_assistant(cx, handle, &workspace).await;
    let store = workspace.read_with(cx, |workspace, _| workspace.store().clone());
    store.update(cx, |store, cx| {
        let mut library = shellrs::host::SerialAssistant::default();
        let mut group = shellrs::host::SerialCommandGroup::default();
        group.id = library.allocate_id();
        group.name = "AT commands".into();
        let mut command = shellrs::host::SerialCommand::default();
        command.id = library.allocate_id();
        command.name = "Read status".into();
        command.content = "AT+STATUS".into();
        command.ending = shellrs::host::SerialCommandEnding::CrLf;
        group.commands.push(command);
        library.groups.push(group);
        assert!(store.set_serial_assistant("serial-assistant:COM5".into(), None, library, cx));
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-message", cx);
        window.input("console draft", cx);
        window.click("serial-assistant-edit-groups", cx);
    });
    assert_eq!(
        workspace.read_with(cx, |workspace, _| workspace.active_tab()),
        Some(CenterTab::SerialCommandEditor)
    );
    in_frame(cx, handle, |window, cx| {
        window.click(("serial-assistant-duplicate", 2_u64), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("serial-assistant-down", 2_u64), cx)
    });
    store.read_with(cx, |store, _| {
        let commands = &store
            .serial_assistant("serial-assistant:COM5")
            .unwrap()
            .groups[0]
            .commands;
        assert_eq!(
            commands
                .iter()
                .map(|command| command.id)
                .collect::<Vec<_>>(),
            [3, 2]
        );
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("serial-tool-message").is_none());
        window.click(("serial-assistant-select-command", 2_u64), cx);
        window.click("serial-assistant-insert-command", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-command-name", cx);
        window.input("Inserted", cx);
        window.click("serial-assistant-command-content", cx);
        window.input("AT", cx);
        window.click("serial-assistant-save-command", cx);
    });
    store.read_with(cx, |store, _| {
        let commands = &store
            .serial_assistant("serial-assistant:COM5")
            .unwrap()
            .groups[0]
            .commands;
        assert_eq!(
            commands
                .iter()
                .map(|command| command.id)
                .collect::<Vec<_>>(),
            [3, 4, 2]
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("serial-assistant-edit", 4_u64), cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("serial-assistant-command-content").visible());
        window.press("escape", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-back", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-message", cx);
        window.press("secondary-a", cx);
        window.press("secondary-c", cx);
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("console draft")
        );
        window.click("serial-assistant-edit-groups", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("serial-assistant-load", 3_u64), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-message", cx);
        window.press("secondary-a", cx);
        window.press("secondary-c", cx);
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("AT+STATUS")
        );
        window.click("close-serial-assistant", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("serial-command-editor-tab").is_none())
    });
    assert!(factory.writes.lock().unwrap().is_empty());
}

#[gpui_kit::test]
async fn serial_assistant_filters_and_saves_timestamped_logs_without_dropping_other_lines(
    cx: &mut TestAppContext,
) {
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("filtered.log");
    let all_path = folder.path().join("full.log");
    open_assistant(cx, handle, &workspace).await;
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("serial-assistant-follow").checked(), Some(true));
        window.click("serial-assistant-follow", cx);
        window.click("serial-assistant-timestamps", cx);
        window.click("serial-assistant-connect", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window.find("serial-assistant-connection-status").label() == Some("已连接")
    })
    .await;
    factory.output(b"INFO startup\r\nERROR sensor disconnected\r\n");
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window.try_find("terminal-timestamps").is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("serial-assistant-follow").checked(),
            Some(false)
        );
        assert_eq!(
            window.find("serial-assistant-timestamps").checked(),
            Some(true)
        );
        window.click("serial-assistant-filter", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("terminal-filtered-logs").visible());
        window.input("ERROR", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window
            .try_find("terminal-find-count")
            .is_some_and(|count| count.label() == Some("1/1"))
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-save-log", cx)
    });
    assert!(cx.did_prompt_for_new_path());
    cx.simulate_new_path_selection(|_| Some(path.clone()));
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window
            .try_find("serial-assistant-log-status")
            .is_some_and(|status| status.label() == Some("日志已导出。"))
    })
    .await;
    let filtered = std::fs::read_to_string(path).unwrap();
    assert!(
        filtered.contains("\tERROR sensor disconnected\n"),
        "{filtered:?}"
    );
    assert!(!filtered.contains("INFO startup"));

    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-filter", cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("terminal-filtered-logs").is_none());
        window.click("serial-assistant-save-log", cx);
    });
    assert!(cx.did_prompt_for_new_path());
    cx.simulate_new_path_selection(|_| Some(all_path.clone()));
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window
            .try_find("serial-assistant-log-status")
            .is_some_and(|status| status.label() == Some("日志已导出。"))
    })
    .await;
    let full = std::fs::read_to_string(all_path).unwrap();
    assert!(full.contains("\tINFO startup\n"), "{full:?}");
    assert!(full.contains("\tERROR sensor disconnected\n"));
}

#[gpui_kit::test]
async fn serial_assistant_opens_the_selected_port_and_custom_baud(cx: &mut TestAppContext) {
    use shellrs::app::CenterTab;

    let factory = Arc::new(SerialToolFactory::default());
    let provider = Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone()));
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        provider.clone(),
    );
    open_assistant(cx, handle, &workspace).await;
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.active_tab(), Some(CenterTab::SerialAssistant));
    });
    assert!(provider.lines().is_empty());
    in_frame(cx, handle, |window, cx| {
        window.within("serial-assistant-port").click("input", cx);
        window.press("down", cx);
        window.press("enter", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("serial-assistant-port").value(), Some("COM7"));
        window.within("serial-assistant-baud").click("input", cx);
        for _ in 0..6 {
            window.press("down", cx);
        }
        window.press("enter", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("serial-assistant-custom-baud").visible());
        window.click("serial-assistant-custom-baud", cx);
        window.press("secondary-a", cx);
        window.input("0", cx);
        window.click("serial-assistant-connect", cx);
    });
    assert!(provider.lines().is_empty());
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("serial-assistant-error").visible());
        window.click("serial-assistant-custom-baud", cx);
        window.press("secondary-a", cx);
        window.input("1234567", cx);
        window.click("serial-assistant-connect", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window.find("serial-assistant-connection-status").label() == Some("已连接")
    })
    .await;
    let lines = provider.lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].device, "COM7");
    assert_eq!(lines[0].settings.baud, 1_234_567);
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("serial-assistant-terminal").visible());
        window.click("serial-assistant-connect", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("serial-assistant-connection-status").label(),
            Some("未连接")
        );
        window.within("serial-assistant-baud").click("input", cx);
        window.press("up", cx);
        window.press("enter", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("serial-assistant-custom-baud").is_none());
        window.click("serial-assistant-connect", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window.find("serial-assistant-connection-status").label() == Some("已连接")
    })
    .await;
    assert_eq!(provider.lines()[1].settings.baud, 2_000_000);
    in_frame(cx, handle, |window, cx| {
        window.click("close-serial-assistant", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("serial-assistant-page").is_none());
    });
}

#[gpui_kit::test]
async fn serial_assistant_restores_preferences_until_reset(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    let (settings, problem) = SettingsStore::load(path.clone());
    assert!(problem.is_none());
    let (handle, workspace) = open_workspace_with_settings_and_tester(
        cx,
        HostStore::empty(),
        settings,
        Arc::new(FakeConnectionTester::default()),
    );
    open_assistant(cx, handle, &workspace).await;
    in_frame(cx, handle, |window, cx| {
        window.within("serial-assistant-port").click("input", cx);
        window.press("down", cx);
        window.press("enter", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.within("serial-assistant-baud").click("input", cx);
        for _ in 0..6 {
            window.press("down", cx);
        }
        window.press("enter", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-custom-baud", cx);
        window.press("secondary-a", cx);
        window.input("1234567", cx);
        window
            .within("serial-assistant-encoding")
            .click("input", cx);
        window.press("down", cx);
        window.press("enter", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-ending-crlf", cx);
        window.click("serial-assistant-receive-hex", cx);
        window.click("serial-assistant-send-hex", cx);
        window.click("serial-assistant-follow", cx);
        window.click("serial-assistant-timestamps", cx);
        window.click("serial-tool-crc", cx);
        window.click("serial-tool-message", cx);
        window.input("A0 B1", cx);
    });
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(json["serial_assistant"]["sender"]["message"], "A0 B1");
    let (saved, problem) = SettingsStore::load(path.clone());
    assert!(problem.is_none());
    let saved = saved.settings().serial_assistant;
    assert_ne!(saved, Default::default());
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(
            workspace.settings().read(cx).settings().serial_assistant,
            saved
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.click("close-serial-assistant", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("open-serial-assistant", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window
            .try_find("serial-assistant-port")
            .is_some_and(|port| port.value() == Some("COM7"))
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("serial-assistant-custom-baud").value(),
            Some("1234567")
        );
        assert_eq!(
            window.find("serial-assistant-encoding").value(),
            Some("GBK")
        );
        for id in [
            "serial-assistant-ending-crlf",
            "serial-assistant-receive-hex",
            "serial-assistant-send-hex",
            "serial-assistant-timestamps",
            "serial-tool-crc",
        ] {
            assert_eq!(window.find(id).checked(), Some(true), "{id}");
        }
        assert_eq!(
            window.find("serial-assistant-follow").checked(),
            Some(false)
        );
        assert_eq!(
            window.find("serial-assistant-connection-status").label(),
            Some("未连接")
        );
        window.click("serial-assistant-reset", cx);
    });
    let (saved, problem) = SettingsStore::load(path);
    assert!(problem.is_none());
    assert_eq!(saved.settings().serial_assistant, Default::default());
    in_frame(cx, handle, |window, cx| {
        window.click("close-serial-assistant", cx)
    });
    open_assistant(cx, handle, &workspace).await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("serial-assistant-baud").value(), Some("115200"));
        assert_eq!(
            window.find("serial-assistant-encoding").value(),
            Some("UTF-8")
        );
        assert_eq!(
            window.find("serial-assistant-ending-none").checked(),
            Some(true)
        );
        assert_eq!(window.find("serial-assistant-follow").checked(), Some(true));
        for id in [
            "serial-assistant-receive-hex",
            "serial-assistant-send-hex",
            "serial-assistant-timestamps",
            "serial-tool-crc",
        ] {
            assert_eq!(window.find(id).checked(), Some(false), "{id}");
        }
    });
}

#[gpui_kit::test]
fn serial_assistant_entry_defaults_to_visible_and_follows_app_settings(cx: &mut TestAppContext) {
    init_app(cx);
    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("open-serial-assistant").visible());
        window.click("open-settings", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-7", cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("show-serial-assistant").checked(), Some(true));
        window.click("show-serial-assistant", cx);
    });
    workspace.read_with(cx, |workspace, cx| {
        assert!(
            !workspace
                .settings()
                .read(cx)
                .settings()
                .window
                .show_serial_assistant
        );
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("open-serial-assistant").is_none());
    });
}

#[gpui_kit::test]
async fn serial_assistant_send_controls_fit_a_narrow_window(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    open_assistant(cx, handle, &workspace).await;
    let mut library = multi_send_library();
    while library.groups[0].commands.len() < 128 {
        let mut command = library.groups[0].commands[0].clone();
        command.id = library.allocate_id();
        library.groups[0].commands.push(command);
    }
    cx.update(|cx| {
        workspace.read(cx).store().clone().update(cx, |store, cx| {
            assert!(store.set_serial_assistant("serial-assistant:COM5".into(), None, library, cx));
        });
    });
    cx.simulate_window_resize(handle.into(), size(px(960.), px(500.)));
    in_frame(cx, handle, |window, _| {
        assert!(window.find("serial-tool-send").visible());
        assert!(window.find("serial-tool-repeat").visible());
        assert!(window.find("serial-assistant-ending-crlf").visible());
        assert!(window.find("serial-multi-send").visible());
        assert!(window.find("serial-assistant-stop-quick").visible());
        assert!(window.find("serial-assistant-loop").visible());
        let list = window.find("serial-multi-commands").bounds();
        let stop = window.find("serial-assistant-stop-quick").bounds();
        assert!(stop.bottom() <= list.top());
        let panel = window.find("serial-multi-send").bounds();
        assert!(stop.right() <= panel.right());
        assert!(window.find("serial-assistant-edit-groups").visible());
        assert!(window.find("serial-multi-quick-content").visible());
        assert!(window.find("serial-multi-quick-add-btn").visible());
    });
    for page in ["serial-page-stm32", "serial-page-stc15"] {
        in_frame(cx, handle, |window, cx| window.click(page, cx));
        in_frame(cx, handle, |window, _| {
            assert!(window.find("serial-isp-detect").visible());
            assert!(window.find("serial-isp-start").visible());
            assert!(window.find("serial-isp-stop").visible());
            assert!(window.find("serial-isp-status").visible());
        });
    }
}

fn multi_send_library() -> shellrs::host::SerialAssistant {
    use shellrs::host::{SerialAssistant, SerialCommand, SerialCommandEnding, SerialCommandGroup};
    let mut library = SerialAssistant::default();
    let mut group = SerialCommandGroup::default();
    group.id = library.allocate_id();
    group.name = "已有命令组".into();
    for (name, content, hex, crc, ending, enabled) in [
        (
            "第一条",
            "hello",
            false,
            false,
            SerialCommandEnding::CrLf,
            true,
        ),
        (
            "二进制",
            "01 03 00 00 00 0A",
            true,
            true,
            SerialCommandEnding::Cr,
            true,
        ),
        (
            "未勾选",
            "skip",
            false,
            false,
            SerialCommandEnding::Lf,
            false,
        ),
    ] {
        let mut command = SerialCommand::default();
        command.id = library.allocate_id();
        command.name = name.into();
        command.content = content.into();
        command.hex = hex;
        command.crc = crc;
        command.ending = ending;
        command.delay_ms = 100;
        command.enabled = enabled;
        group.commands.push(command);
    }
    library.groups.push(group);
    library
}

#[gpui_kit::test]
async fn serial_multi_send_reuses_saved_configuration_and_sends_individual_rows(
    cx: &mut TestAppContext,
) {
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    let library = multi_send_library();
    cx.update(|cx| {
        workspace.read(cx).store().clone().update(cx, |store, cx| {
            assert!(store.set_serial_assistant(
                "serial-assistant:COM5".into(),
                None,
                library.clone(),
                cx
            ));
        });
    });
    open_assistant(cx, handle, &workspace).await;
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("serial-multi-send").visible());
        assert_eq!(
            window.find("serial-assistant-group-picker").label(),
            Some("已有命令组")
        );
        assert_eq!(
            window.find(("serial-multi-content", 3_u64)).label(),
            Some("01 03 00 00 00 0A")
        );
        assert_eq!(
            window.find(("serial-multi-enabled", 4_u64)).checked(),
            Some(false)
        );
        assert!(
            window
                .find(("serial-multi-info", 3_u64))
                .label()
                .unwrap()
                .contains("100 ms")
        );
        window.click(("serial-multi-send", 3_u64), cx);
    });
    assert!(factory.writes.lock().unwrap().is_empty());
    connect_assistant(cx, handle).await;
    for (id, expected) in [
        (3_u64, b"\x01\x03\x00\x00\x00\x0a\xc5\xcd\r".to_vec()),
        (4, b"skip\n".to_vec()),
    ] {
        in_frame(cx, handle, |window, cx| {
            window.click(("serial-multi-send", id), cx);
        });
        cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
            window.find("serial-tool-status").label() == Some("命令组发送完成。")
        })
        .await;
        assert_eq!(factory.writes.lock().unwrap().last(), Some(&expected));
    }
    assert_eq!(factory.writes.lock().unwrap().len(), 2);
    cx.read(|cx| {
        assert_eq!(
            workspace
                .read(cx)
                .store()
                .read(cx)
                .serial_assistant("serial-assistant:COM5"),
            Some(&library)
        );
    });
    // Participation is edited through the same stored command, with all of
    // its original format and timing settings intact.
    in_frame(cx, handle, |window, cx| {
        window.click(("serial-multi-enabled", 4_u64), cx);
    });
    cx.read(|cx| {
        let mut expected = library.clone();
        expected.groups[0].commands[2].enabled = true;
        assert_eq!(
            workspace
                .read(cx)
                .store()
                .read(cx)
                .serial_assistant("serial-assistant:COM5"),
            Some(&expected)
        );
    });
}

#[gpui_kit::test]
async fn serial_multi_send_loops_in_order_and_stop_prevents_later_sends(cx: &mut TestAppContext) {
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    cx.update(|cx| {
        workspace.read(cx).store().clone().update(cx, |store, cx| {
            assert!(store.set_serial_assistant(
                "serial-assistant:COM5".into(),
                None,
                multi_send_library(),
                cx
            ));
        });
    });
    open_assistant(cx, handle, &workspace).await;
    connect_assistant(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-loop", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        let enough = factory.writes.lock().unwrap().len() >= 4;
        if enough {
            assert!(
                window
                    .find("serial-tool-status")
                    .label()
                    .unwrap()
                    .contains("循环第")
            );
        }
        enough
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-run-quick", cx);
        window.click(("serial-multi-send", 4_u64), cx);
        window.click(("serial-multi-enabled", 2_u64), cx);
        assert_eq!(
            window.find(("serial-multi-enabled", 2_u64)).checked(),
            Some(true)
        );
        assert!(
            window
                .find("serial-tool-status")
                .label()
                .unwrap()
                .contains("循环第")
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-stop-quick", cx)
    });
    cx.run_until_parked();
    let stopped_count = factory.writes.lock().unwrap().len();
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    assert_eq!(factory.writes.lock().unwrap().len(), stopped_count);
    for (index, bytes) in factory.writes.lock().unwrap().iter().enumerate() {
        let expected: &[u8] = if index % 2 == 0 {
            b"hello\r\n"
        } else {
            b"\x01\x03\x00\x00\x00\x0a\xc5\xcd\r"
        };
        assert_eq!(bytes, expected);
    }
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .find("serial-tool-status")
                .label()
                .unwrap()
                .contains("已停止")
        );
    });
    // A stopped loop releases the sender immediately.
    in_frame(cx, handle, |window, cx| {
        window.click(("serial-multi-send", 4_u64), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window.find("serial-tool-status").label() == Some("命令组发送完成。")
    })
    .await;
    assert_eq!(factory.writes.lock().unwrap().len(), stopped_count + 1);
    assert_eq!(factory.writes.lock().unwrap().last().unwrap(), b"skip\n");
}

#[gpui_kit::test]
async fn serial_multi_send_loop_stops_on_editing_disconnect_and_tab_change(
    cx: &mut TestAppContext,
) {
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    let mut library = multi_send_library();
    library.groups[0].commands[0].delay_ms = 20_000;
    cx.update(|cx| {
        workspace.read(cx).store().clone().update(cx, |store, cx| {
            assert!(store.set_serial_assistant("serial-assistant:COM5".into(), None, library, cx));
        });
    });
    open_assistant(cx, handle, &workspace).await;
    connect_assistant(cx, handle).await;
    for (index, action) in [
        "serial-assistant-edit-groups",
        "serial-assistant-connect",
        "new-local-terminal",
    ]
    .into_iter()
    .enumerate()
    {
        if index == 1 {
            in_frame(cx, handle, |window, cx| {
                window.click("serial-assistant-back", cx)
            });
        } else if index == 2 {
            connect_assistant(cx, handle).await;
        }
        in_frame(cx, handle, |window, cx| {
            window.click("serial-assistant-loop", cx)
        });
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
            factory.writes.lock().unwrap().len() == index + 1
        })
        .await;
        in_frame(cx, handle, |window, cx| {
            if action == "new-local-terminal" {
                window.dispatch_action(Box::new(shellrs::app::NewLocalTerminal), cx);
            } else {
                window.click(action, cx);
            }
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(21));
        cx.run_until_parked();
        assert_eq!(factory.writes.lock().unwrap().len(), index + 1);
    }
}

#[gpui_kit::test]
async fn serial_multi_send_error_stops_the_loop_and_allows_retry(cx: &mut TestAppContext) {
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    cx.update(|cx| {
        workspace.read(cx).store().clone().update(cx, |store, cx| {
            assert!(store.set_serial_assistant(
                "serial-assistant:COM5".into(),
                None,
                multi_send_library(),
                cx,
            ));
        });
    });
    open_assistant(cx, handle, &workspace).await;
    connect_assistant(cx, handle).await;
    *factory.checked_error.lock().unwrap() = Some("test write failure".into());
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-loop", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window
            .find("serial-tool-status")
            .label()
            .unwrap_or_default()
            .contains("test write failure")
    })
    .await;
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    assert_eq!(factory.writes.lock().unwrap().len(), 1);
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-stop-quick", cx);
        assert!(
            window
                .find("serial-tool-status")
                .label()
                .unwrap()
                .contains("test write failure")
        );
        window.click("serial-assistant-run-quick", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window.find("serial-tool-status").label() == Some("命令组发送完成。")
    })
    .await;
    assert_eq!(
        factory.writes.lock().unwrap().as_slice(),
        [
            b"hello\r\n".to_vec(),
            b"hello\r\n".to_vec(),
            b"\x01\x03\x00\x00\x00\x0a\xc5\xcd\r".to_vec(),
        ]
    );
}

fn confirm_isp_flash(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    in_frame(cx, handle, |window, cx| {
        window.click("serial-isp-start", cx)
    });
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
}

#[gpui_kit::test]
async fn serial_isp_loads_bin_and_hex_selects_backends_and_preserves_groups(
    cx: &mut TestAppContext,
) {
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    let library = multi_send_library();
    cx.update(|cx| {
        workspace.read(cx).store().clone().update(cx, |store, cx| {
            assert!(store.set_serial_assistant(
                "serial-assistant:COM5".into(),
                None,
                library.clone(),
                cx
            ));
        });
    });
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("firmware.bin");
    let hex = directory.path().join("firmware.hex");
    std::fs::write(&binary, [1, 2, 3]).unwrap();
    std::fs::write(&hex, ":03000000010203F7\n:00000001FF\n").unwrap();
    open_assistant(cx, handle, &workspace).await;
    connect_assistant(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click("serial-page-stm32", cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("serial-isp").visible());
        assert_eq!(window.find("serial-isp-verify").checked(), Some(true));
        window.click("serial-isp-firmware", cx);
        window.input(&binary.to_string_lossy(), cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-isp-start", cx)
    });
    in_frame(cx, handle, |window, cx| window.click("cancel", cx));
    assert!(factory.isp_jobs.lock().unwrap().is_empty());
    confirm_isp_flash(cx, handle);
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window.find("serial-isp-status").label() == Some("烧录完成，串口已恢复。")
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("serial-isp-chip").label(), Some("TEST chip"));
        window.click("serial-page-stc15", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("serial-isp-address").is_none());
        assert!(window.find("serial-isp-python").visible());
        assert_eq!(window.find("serial-isp-firmware").value(), Some(""));
        window.click("serial-isp-firmware", cx);
        window.input(&hex.to_string_lossy(), cx);
        window.click("serial-isp-python", cx);
        window.press("secondary-a", cx);
        window.input("C:/Test/Python/python.exe", cx);
        window.click("serial-isp-stc15", cx);
    });
    confirm_isp_flash(cx, handle);
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        factory.isp_jobs.lock().unwrap().len() == 2
            && window.find("serial-isp-status").label() == Some("烧录完成，串口已恢复。")
    })
    .await;
    let jobs = factory.isp_jobs.lock().unwrap();
    assert_eq!(jobs.len(), 2);
    assert!(!jobs[0].stc);
    assert_eq!(jobs[0].image.as_deref(), Some([1, 2, 3].as_slice()));
    assert_eq!(jobs[0].address, Some(0x08000000));
    assert_eq!(jobs[0].baud, 115200);
    assert!(jobs[1].stc);
    assert_eq!(jobs[1].image.as_deref(), Some([1, 2, 3].as_slice()));
    assert_eq!(jobs[1].address, Some(0));
    drop(jobs);
    cx.read(|cx| {
        assert_eq!(
            workspace
                .read(cx)
                .store()
                .read(cx)
                .serial_assistant("serial-assistant:COM5"),
            Some(&library)
        );
        let settings =
            serde_json::to_value(workspace.read(cx).settings().read(cx).settings()).unwrap();
        assert_eq!(
            settings["serial_assistant"]["isp"]["python"],
            "C:/Test/Python/python.exe"
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-page-multi", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.find(("serial-multi-send", 2_u64)).visible());
        assert_eq!(
            window.find("serial-assistant-connection-status").label(),
            Some("已连接")
        );
    });
    assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
}

#[gpui_kit::test]
async fn serial_isp_excludes_other_sends_and_cancels_on_stop_and_leaving(cx: &mut TestAppContext) {
    let factory = Arc::new(SerialToolFactory::default());
    factory.hold_isp.store(true, Ordering::Release);
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("firmware.bin");
    std::fs::write(&binary, [1, 2, 3]).unwrap();
    open_assistant(cx, handle, &workspace).await;
    connect_assistant(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-message", cx);
        window.input("hello", cx);
        window.click("serial-assistant-ending-crlf", cx);
        window.click("serial-page-stm32", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-isp-firmware", cx);
        window.input(&binary.to_string_lossy(), cx);
    });
    for index in 0..2 {
        confirm_isp_flash(cx, handle);
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
            factory.isp_jobs.lock().unwrap().len() == index + 1
        })
        .await;
        in_frame(cx, handle, |window, cx| {
            window.click("serial-assistant-terminal", cx);
            window.input("unexpected", cx);
            window.click("serial-tool-send", cx);
            window.click("serial-isp-detect", cx);
            window.click("serial-page-multi", cx);
            assert!(window.find("serial-isp-stop").visible());
            assert!(window.find("serial-isp").visible());
            window.click("serial-assistant-tools-toggle", cx);
            assert!(window.find("serial-isp").visible());
        });
        assert!(factory.writes.lock().unwrap().is_empty());
        assert_eq!(factory.isp_jobs.lock().unwrap().len(), index + 1);
        in_frame(cx, handle, |window, cx| {
            if index == 0 {
                window.click("serial-isp-stop", cx);
            } else {
                window.dispatch_action(Box::new(shellrs::app::OpenSerialCommandEditor), cx);
            }
        });
        if index == 1 {
            cx.run_until_parked();
            in_frame(cx, handle, |window, cx| {
                window.click("serial-assistant-back", cx)
            });
        }
        cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
            window.find("serial-isp-status").label() == Some("TEST ISP stopped")
        })
        .await;
    }
    in_frame(cx, handle, |window, cx| {
        window.click("serial-page-multi", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-send", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.writes.lock().unwrap().len() == 1
    })
    .await;
    assert_eq!(factory.writes.lock().unwrap()[0], b"hello\r\n");
}

#[gpui_kit::test]
async fn serial_isp_invalid_images_fail_before_transport_and_errors_allow_retry(
    cx: &mut TestAppContext,
) {
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("firmware.hex");
    std::fs::write(&file, ":01000000AA54\n:00000001FF\n").unwrap();
    open_assistant(cx, handle, &workspace).await;
    connect_assistant(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click("serial-page-stm32", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-isp-firmware", cx);
        window.input(&file.to_string_lossy(), cx);
    });
    confirm_isp_flash(cx, handle);
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window
            .find("serial-isp-status")
            .label()
            .unwrap_or_default()
            .contains("HEX")
    })
    .await;
    assert!(factory.isp_jobs.lock().unwrap().is_empty());
    std::fs::write(&file, ":020000040800F2\n:03000000010203F7\n:00000001FF\n").unwrap();
    *factory.checked_error.lock().unwrap() = Some(format!(
        "TEST bootloader timeout {}",
        "detail ".repeat(1000)
    ));
    confirm_isp_flash(cx, handle);
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window
            .find("serial-isp-status")
            .label()
            .unwrap_or_default()
            .contains("TEST bootloader timeout")
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert!(window.find("serial-isp-stop").visible());
        assert!(window.find("serial-isp-status").bounds().size.height <= px(80.));
    });
    confirm_isp_flash(cx, handle);
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window.find("serial-isp-status").label() == Some("烧录完成，串口已恢复。")
    })
    .await;
    assert_eq!(factory.isp_jobs.lock().unwrap().len(), 2);
}

#[gpui_kit::test]
async fn serial_multi_send_quick_add_draft_save_and_inline_hex_work_without_leaving_panel(
    cx: &mut TestAppContext,
) {
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    cx.simulate_window_resize(handle.into(), size(px(1280.), px(800.)));
    open_assistant(cx, handle, &workspace).await;
    connect_assistant(cx, handle).await;
    // Opening the full command form from an empty state and cancelling must
    // not leave an empty default group behind.
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("serial-multi-empty-add").visible());
        window.click("serial-multi-empty-add", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("serial-assistant-command-name").visible());
        window.press("escape", cx);
    });
    cx.read(|cx| {
        assert!(
            workspace
                .read(cx)
                .store()
                .read(cx)
                .serial_assistant("serial-assistant:COM5")
                .is_none()
        );
    });
    // Quick-adding a text command auto-creates the default group and inherits
    // the assistant's active line ending.
    in_frame(cx, handle, |window, cx| {
        window.click("serial-assistant-ending-crlf", cx);
        window.click("serial-multi-quick-content", cx);
        window.input("AT+GMR", cx);
        window.click("serial-multi-quick-name", cx);
        window.input("查询版本", cx);
        window.press("enter", cx);
    });
    // Quick-adding a HEX command defaults to no trailing line ending.
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("serial-multi-quick-content").value(), Some(""));
        window.click("serial-multi-quick-hex", cx);
        window.click("serial-multi-quick-content", cx);
        window.input("13 00 FF 88", cx);
        window.click("serial-multi-quick-name", cx);
        window.input("十六进制1", cx);
        window.click("serial-multi-quick-delay", cx);
        window.press("secondary-a", cx);
        window.input("200", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-multi-quick-add-btn", cx);
    });
    // Saving the bottom draft adds a third command with its ending and CRC.
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-message", cx);
        window.input("123456789", cx);
        window.click("serial-tool-crc", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("serial-tool-save-command", cx);
    });
    let saved = cx.read(|cx| {
        workspace
            .read(cx)
            .store()
            .read(cx)
            .serial_assistant("serial-assistant:COM5")
            .unwrap()
            .clone()
    });
    assert_eq!(saved.groups.len(), 1);
    assert_eq!(saved.groups[0].name, "默认命令组");
    assert_eq!(saved.groups[0].commands.len(), 3);
    let first = &saved.groups[0].commands[0];
    let second = &saved.groups[0].commands[1];
    let third = &saved.groups[0].commands[2];
    assert_eq!(first.name, "查询版本");
    assert_eq!(first.content, "AT+GMR");
    assert!(!first.hex);
    assert_eq!(first.ending, shellrs::host::SerialCommandEnding::CrLf);
    assert_eq!(second.name, "十六进制1");
    assert!(second.hex);
    assert_eq!(second.ending, shellrs::host::SerialCommandEnding::None);
    assert_eq!(second.delay_ms, 200);
    assert_eq!(third.content, "123456789");
    assert!(third.crc);
    // Shared group newline toggle applies to inheriting rows (first) without
    // overriding custom rows (second HEX row, third CRC row).
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("serial-multi-group-newline").checked(),
            Some(true)
        );
        window.click("serial-multi-group-newline", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("serial-multi-group-newline").checked(),
            Some(false)
        );
        window.click(("serial-multi-send", first.id), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window.find("serial-tool-status").label() == Some("命令组发送完成。")
    })
    .await;
    assert_eq!(
        factory.writes.lock().unwrap().last(),
        Some(&b"AT+GMR".to_vec())
    );
    // Re-enable shared newline (restores CrLf) and verify all three rows send as expected.
    in_frame(cx, handle, |window, cx| {
        window.click("serial-multi-group-newline", cx);
    });
    // Send buttons send each configured row immediately.
    for (id, expected) in [
        (first.id, b"AT+GMR\r\n".to_vec()),
        (second.id, vec![0x13, 0x00, 0xff, 0x88]),
        (third.id, b"123456789\x37\x4b\r\n".to_vec()),
    ] {
        in_frame(cx, handle, |window, cx| {
            window.click(("serial-multi-send", id), cx);
        });
        cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
            window.find("serial-tool-status").label() == Some("命令组发送完成。")
        })
        .await;
        assert_eq!(factory.writes.lock().unwrap().last(), Some(&expected));
    }
}

#[gpui_kit::test]
async fn serial_multi_send_sscom_inline_edit_select_all_and_quick_toggles(cx: &mut TestAppContext) {
    let factory = Arc::new(SerialToolFactory::default());
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(factory.clone())),
    );
    cx.simulate_window_resize(handle.into(), size(px(1280.), px(800.)));
    cx.update(|cx| {
        workspace.read(cx).store().clone().update(cx, |store, cx| {
            assert!(store.set_serial_assistant(
                "serial-assistant:COM5".into(),
                None,
                multi_send_library(),
                cx
            ));
        });
    });
    open_assistant(cx, handle, &workspace).await;
    connect_assistant(cx, handle).await;
    // Inline edit content on command 2 and shared group delay, then send immediately.
    in_frame(cx, handle, |window, cx| {
        window.click(("serial-multi-content", 2_u64), cx);
        window.press("secondary-a", cx);
        window.input("AT+RST", cx);
        window.click("serial-multi-group-delay", cx);
        window.press("secondary-a", cx);
        window.input("250", cx);
        window.press("enter", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("serial-multi-send", 2_u64), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window.find("serial-tool-status").label() == Some("命令组发送完成。")
    })
    .await;
    assert_eq!(
        factory.writes.lock().unwrap().last(),
        Some(&b"AT+RST\r\n".to_vec())
    );
    cx.read(|cx| {
        let saved = workspace
            .read(cx)
            .store()
            .read(cx)
            .serial_assistant("serial-assistant:COM5")
            .unwrap();
        assert_eq!(saved.groups[0].commands[0].content, "AT+RST");
        assert_eq!(saved.groups[0].default_delay_ms, 250);
    });
    // Shared group newline toggle + ending cycle updates the group default cleanly.
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("serial-multi-group-newline").checked(),
            Some(false)
        );
        window.click("serial-multi-group-newline", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("serial-multi-group-ending").label(),
            Some("CRLF")
        );
        window.click("serial-multi-group-ending", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("serial-multi-group-ending").label(), Some("LF"));
    });
    // Select-all toggles all rows enabled, then all rows disabled.
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("serial-multi-select-all").checked(),
            Some(false)
        );
        window.click("serial-multi-select-all", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("serial-multi-select-all").checked(), Some(true));
        assert_eq!(
            window.find(("serial-multi-enabled", 4_u64)).checked(),
            Some(true)
        );
        window.click("serial-multi-select-all", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("serial-multi-select-all").checked(),
            Some(false)
        );
        assert_eq!(
            window.find(("serial-multi-enabled", 2_u64)).checked(),
            Some(false)
        );
        assert_eq!(
            window.find(("serial-multi-enabled", 3_u64)).checked(),
            Some(false)
        );
        assert_eq!(
            window.find(("serial-multi-enabled", 4_u64)).checked(),
            Some(false)
        );
    });
    // Duplicate group creates an independent copy and selects it.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(shellrs::app::SerialAssistantAction(
                shellrs::serial::SerialAssistantTabCommand::Tool(SerialToolCommand::Assistant(
                    shellrs::serial::SerialAssistantCommand::DuplicateGroup,
                )),
            )),
            cx,
        );
    });
    cx.read(|cx| {
        let saved = workspace
            .read(cx)
            .store()
            .read(cx)
            .serial_assistant("serial-assistant:COM5")
            .unwrap();
        assert_eq!(saved.groups.len(), 2);
        assert_eq!(saved.groups[1].name, "已有命令组 (2)");
        assert_eq!(saved.groups[1].commands.len(), 3);
    });
}
