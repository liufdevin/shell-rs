use crate::support::*;
use shellrs::host::SerialSettings;
use shellrs::serial::{SerialPortSource, SerialPresenceState};

struct TestPorts {
    ports: Mutex<Option<Vec<String>>>,
    scans: AtomicUsize,
}

impl TestPorts {
    fn new(ports: &[&str]) -> Self {
        Self {
            ports: Mutex::new(Some(ports.iter().map(|port| (*port).to_owned()).collect())),
            scans: AtomicUsize::new(0),
        }
    }

    fn set(&self, ports: &[&str]) {
        *self.ports.lock().unwrap() = Some(ports.iter().map(|port| (*port).to_owned()).collect());
    }
}

impl SerialPortSource for TestPorts {
    fn ports(&self) -> std::io::Result<Vec<String>> {
        self.scans.fetch_add(1, Ordering::SeqCst);
        self.ports
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| std::io::Error::other("injected enumeration failure"))
    }
}

fn start_presence(cx: &mut TestAppContext, workspace: &Entity<Workspace>, ports: Arc<TestPorts>) {
    cx.update(|cx| {
        workspace
            .read(cx)
            .serial_presence()
            .clone()
            .update(cx, |presence, cx| presence.start(ports, cx));
    });
    cx.run_until_parked();
}

fn next_scan(cx: &mut TestAppContext) {
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
}

#[gpui_kit::test]
fn serial_presence_updates_marks_without_moving_rows_or_probing_network_hosts(
    cx: &mut TestAppContext,
) {
    let mut store = HostStore::empty();
    let serial = store.insert_unnotified(HostDraft::serial(
        "COM3",
        "COM3",
        SerialSettings::default(),
        None,
    ));
    let duplicate = store.insert_unnotified(HostDraft::serial(
        "second console",
        "COM3",
        SerialSettings::default(),
        None,
    ));
    let ssh = store.insert_unnotified(HostDraft::new(
        "server",
        "example.test",
        22,
        "root",
        AuthKind::NoPassword,
        None,
    ));
    let telnet = store.insert_unnotified(HostDraft::telnet("switch", "example.test", 23, None));
    let (handle, workspace) = open_workspace_with_store(cx, store);
    let bounds = in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("host-os", serial.0)).label(),
            Some("串口 · 正在检测串口设备")
        );
        window.find(("host-row", serial.0)).bounds()
    });
    let ports = Arc::new(TestPorts::new(&["COM3"]));
    start_presence(cx, &workspace, ports.clone());
    assert_eq!(ports.scans.load(Ordering::SeqCst), 1);
    next_scan(cx);
    assert_eq!(ports.scans.load(Ordering::SeqCst), 2);
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("host-os", serial.0)).label(),
            Some("串口 · 设备在线")
        );
        assert_eq!(
            window.find(("host-os", duplicate.0)).label(),
            Some("串口 · 设备在线")
        );
        assert_eq!(
            window.find(("host-os", ssh.0)).label(),
            Some("未探测到系统")
        );
        assert_eq!(window.find(("host-os", telnet.0)).label(), Some("Telnet"));
        assert_eq!(window.find(("host-row", serial.0)).bounds(), bounds);
    });
    ports.set(&[]);
    for _ in 0..3 {
        next_scan(cx);
        in_frame(cx, handle, |window, _| {
            assert_eq!(
                window.find(("host-os", serial.0)).label(),
                Some("串口 · 设备在线")
            );
        });
    }
    next_scan(cx);
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find(("host-os", serial.0)).label(),
            Some("串口 · 设备离线")
        );
        assert_eq!(window.find(("host-row", serial.0)).bounds(), bounds);
        window.click(("host-row", serial.0), cx);
        assert_eq!(window.find(("host-row", serial.0)).selected(), Some(true));
    });
    ports.set(&["COM3"]);
    next_scan(cx);
    next_scan(cx);
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("host-os", serial.0)).label(),
            Some("串口 · 设备在线")
        );
        assert_eq!(window.find(("host-row", serial.0)).bounds(), bounds);
    });
}

#[gpui_kit::test]
fn serial_presence_latches_flapping_and_resets_recovery_on_errors(cx: &mut TestAppContext) {
    let mut store = HostStore::empty();
    let serial = store.insert_unnotified(HostDraft::serial(
        "COM4",
        "COM4",
        SerialSettings::default(),
        None,
    ));
    let (handle, workspace) = open_workspace_with_store(cx, store);
    let ports = Arc::new(TestPorts::new(&["COM4"]));
    start_presence(cx, &workspace, ports.clone());
    next_scan(cx);
    for dropout in 0..3 {
        ports.set(&[]);
        next_scan(cx);
        if dropout < 2 {
            ports.set(&["COM4"]);
            next_scan(cx);
        }
    }
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("host-os", serial.0)).label(),
            Some("串口 · 设备连接不稳定，正在等待恢复")
        );
    });
    ports.set(&["COM4"]);
    for _ in 0..8 {
        next_scan(cx);
    }
    *ports.ports.lock().unwrap() = None;
    next_scan(cx);
    ports.set(&["COM4"]);
    for _ in 0..8 {
        next_scan(cx);
    }
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("host-os", serial.0)).label(),
            Some("串口 · 设备连接不稳定，正在等待恢复")
        );
    });
    next_scan(cx);
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("host-os", serial.0)).label(),
            Some("串口 · 设备在线")
        );
    });
}

#[gpui_kit::test]
fn serial_presence_handles_scan_failure_edits_and_removal(cx: &mut TestAppContext) {
    let mut store = HostStore::empty();
    let serial = store.insert_unnotified(HostDraft::serial(
        "console",
        "COM3",
        SerialSettings::default(),
        None,
    ));
    let (handle, workspace) = open_workspace_with_store(cx, store);
    let ports = Arc::new(TestPorts::new(&["COM3"]));
    start_presence(cx, &workspace, ports.clone());
    next_scan(cx);
    *ports.ports.lock().unwrap() = None;
    for _ in 0..4 {
        next_scan(cx);
    }
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("host-os", serial.0)).label(),
            Some("串口 · 串口状态未知，无法读取串口列表")
        );
    });
    cx.update(|cx| {
        let store = workspace.read(cx).store().clone();
        let mut draft = store.read(cx).host(serial).unwrap().draft();
        draft.address = "COM7".into();
        store.update(cx, |store, cx| store.update(serial, draft, cx));
    });
    cx.run_until_parked();
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("host-os", serial.0)).label(),
            Some("串口 · 正在检测串口设备")
        );
    });
    ports.set(&["COM7"]);
    next_scan(cx);
    next_scan(cx);
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("host-os", serial.0)).label(),
            Some("串口 · 设备在线")
        );
    });
    cx.update(|cx| {
        workspace
            .read(cx)
            .store()
            .clone()
            .update(cx, |store, cx| store.remove(serial, cx));
    });
    cx.run_until_parked();
    let scanned = ports.scans.load(Ordering::SeqCst);
    next_scan(cx);
    assert_eq!(ports.scans.load(Ordering::SeqCst), scanned);
    cx.update(|cx| {
        assert_eq!(
            workspace
                .read(cx)
                .serial_presence()
                .read(cx)
                .state_for("COM7"),
            SerialPresenceState::Checking
        );
        workspace.read(cx).store().clone().update(cx, |store, cx| {
            store.insert(
                HostDraft::serial("new", "COM7", SerialSettings::default(), None),
                cx,
            )
        });
    });
    cx.run_until_parked();
    next_scan(cx);
    next_scan(cx);
    assert_eq!(ports.scans.load(Ordering::SeqCst), scanned + 2);
}

#[gpui_kit::test]
async fn serial_presence_keeps_manual_connections_and_explains_gray_marks_with_details(
    cx: &mut TestAppContext,
) {
    let mut store = HostStore::empty();
    let serial = store.insert_unnotified(HostDraft::serial(
        "console",
        "COM3",
        SerialSettings::default(),
        None,
    ));
    let (handle, workspace) = open_workspace_with_store(cx, store);
    let ports = Arc::new(TestPorts::new(&[]));
    start_presence(cx, &workspace, ports);
    for _ in 0..3 {
        next_scan(cx);
    }
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find(("host-os", serial.0)).label(),
            Some("串口 · 设备离线")
        );
        window.click("toggle-host-details", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.hover(("host-row", serial.0), cx);
    });
    cx.executor().advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("host-tooltip").label(), Some("设备离线"));
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(serial)), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window
            .try_find("status-connection")
            .is_some_and(|status| status.label() == Some("已连接 console"))
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find(("terminal-tab-os", 1u64)).label(), Some("串口"));
        assert_eq!(
            window.find(("host-os", serial.0)).label(),
            Some("串口 · 设备离线")
        );
    });
}
