use crate::support::*;
use shellrs::host::{SerialSettings, SnippetDraft, SnippetStep, SnippetWait};
use shellrs::serial::{SerialToolCommand, SerialToolPage};

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
        let mut diagnostics = shellrs::serial::SerialDiagnostics::default();
        diagnostics.configuration = "115200 8N1".into();
        events.send_blocking(TerminalTransportEvent::SerialDiagnostics(diagnostics))?;
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
            gpui_kit::ScrollWheelEvent {
                position,
                delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-400.))),
                modifiers: gpui_kit::Modifiers::default(),
                touch_phase: gpui_kit::TouchPhase::Moved,
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
            gpui_kit::ScrollWheelEvent {
                position,
                delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-200.))),
                modifiers: gpui_kit::Modifiers::default(),
                touch_phase: gpui_kit::TouchPhase::Moved,
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
        assert!(output.size.height > px(350.), "output: {output:?}");
        assert!(output.bottom() <= port.top());
        assert_eq!(port.top(), baud.top());
        assert!(port.bottom() <= commands.top());
        assert!(commands.bottom() <= message.top());
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
    cx.simulate_window_resize(handle.into(), size(px(960.), px(500.)));
    in_frame(cx, handle, |window, _| {
        assert!(window.find("serial-tool-send").visible());
        assert!(window.find("serial-tool-repeat").visible());
        assert!(window.find("serial-assistant-ending-crlf").visible());
    });
}
