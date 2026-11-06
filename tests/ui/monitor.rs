//! 系统监控: the right sidebar's tool that reads the SSH terminal's host
//! over the terminal's own connection.

use crate::support::*;

#[gpui_kit::test]
async fn android_system_architecture_shows_the_application_abi(cx: &mut TestAppContext) {
    let output = FIRST.replace("@@arch\nx86_64", "@@arch\narm64-v8a");
    let factory = Arc::new(FakeTerminalFactory::answering(&[&output]));
    let handle = open_monitor(cx, factory);
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        window
            .try_find("monitor-arch")
            .is_some_and(|field| field.label() == Some("arm64-v8a"))
    })
    .await;
}

#[gpui_kit::test]
async fn dynamic_channel_choices_do_not_change_another_terminal(cx: &mut TestAppContext) {
    let output = format!("{FIRST}\n@@mtop\nunit: MB\npcie0 dsp_ddr\n10 20\n@@mtop-end\n");
    let factory = Arc::new(FakeTerminalFactory::answering(&[&output]));
    let handle = open_monitor(cx, factory);
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        window.try_find("monitor-hardware:pcie0").is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click("monitor-hardware-channels", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("monitor-channel:pcie0", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("monitor-hardware:pcie0").is_none())
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_STAGING_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("tool-monitor", cx));
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        window.try_find("monitor-hardware:pcie0").is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("monitor-hardware:pcie0").is_none());
        assert!(window.find("monitor-hardware:dsp_ddr").visible());
    });
}

#[gpui_kit::test]
async fn every_monitor_cycle_collects_new_hardware_bandwidth(cx: &mut TestAppContext) {
    let outputs: Vec<_> = [1, 2, 3]
        .iter()
        .map(|value| format!("{FIRST}\n@@mtop\nunit: MB\ngpu\n{value}\n@@mtop-end\n"))
        .collect();
    let samples: Vec<_> = outputs.iter().map(String::as_str).collect();
    let factory = Arc::new(FakeTerminalFactory::answering(&samples));
    let handle = open_monitor(cx, factory.clone());
    cx.wait_for(handle.into(), Duration::from_secs(6), |window, cx| {
        window.render_frame(cx);
        window
            .try_find("monitor-hardware:gpu")
            .is_some_and(|value| value.label() == Some("gpu 3.00 MB/s"))
    })
    .await;
    let commands = factory.exec_commands();
    assert!(commands.len() >= 3);
    assert!(
        commands
            .iter()
            .take(3)
            .all(|command| command.contains("@@mtop"))
    );
}

#[gpui_kit::test]
async fn helper_a133_mtop_channels_appear_without_an_absent_npu(cx: &mut TestAppContext) {
    let sample = include_str!("../fixtures/monitor/mtop-helper-a133.txt");
    let output = format!("{FIRST}\n@@mtop\n{sample}@@mtop-end\n");
    let handle = open_monitor(cx, Arc::new(FakeTerminalFactory::answering(&[&output])));
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        window.try_find("monitor-hardware:de0").is_some()
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("monitor-hardware:de0").label(),
            Some("de0 137.00 MB/s")
        );
        assert_eq!(
            window.find("monitor-hardware:gpu").label(),
            Some("gpu 0.00 MB/s")
        );
        assert!(window.find("monitor-hardware:de1").visible());
        assert!(window.try_find("monitor-hardware:isp").is_some());
        assert!(window.try_find("monitor-hardware:npu").is_none());
    });
}

#[gpui_kit::test]
async fn hardware_bandwidth_hides_absent_channels_and_obeys_visibility_changes(
    cx: &mut TestAppContext,
) {
    let output = format!(
        "{FIRST}\n@@mtop\nunit: MB\ntotal cpu gpuddr de_ddr isp_ddr\n100 5 12.5 4 0\n@@mtop-end\n"
    );
    let factory = Arc::new(FakeTerminalFactory::answering(&[&output]));
    let handle = open_monitor(cx, factory.clone());
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        window.try_find("monitor-hardware:gpuddr").is_some()
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("monitor-hardware:gpuddr").label(),
            Some("gpuddr 12.50 MB/s")
        );
        assert!(window.find("monitor-hardware:isp_ddr").visible());
        assert!(window.try_find("monitor-hardware:npu").is_none());
    });
    assert!(factory.exec_commands()[0].contains("timeout -s INT -k 1 1 mtop -n 1 -d 0.5 -m"));
    in_frame(cx, handle, |window, cx| {
        window.click("monitor-hardware-channels", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("monitor-channel:gpuddr", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("monitor-hardware:gpuddr").is_none());
        assert!(window.find("monitor-hardware:de_ddr").visible());
    });
    in_frame(cx, handle, |window, cx| {
        window.click("monitor-channel:gpuddr", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.find("monitor-hardware:gpuddr").visible())
    });
    let previous = factory.exec_count();
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.exec_count() > previous
    })
    .await;
    assert!(factory.exec_commands().last().unwrap().contains("mtop"));
}

#[gpui_kit::test]
async fn failed_hardware_reads_do_not_break_the_standard_monitor(cx: &mut TestAppContext) {
    let output = format!("{FIRST}\n@@mtop\nopen failed: permission denied\n@@mtop-end\n");
    let factory = Arc::new(FakeTerminalFactory::answering(&[&output]));
    let handle = open_monitor(cx, factory);
    wait_for_label(cx, handle, "monitor-host", "web-01-vm").await;
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("monitor-hardware").is_none());
        assert!(window.try_find("monitor-message").is_none());
        assert!(window.find("monitor-memory-amount").visible());
    });
}

/// A two-core Debian host, then the same host a moment later: 100 more busy
/// ticks of 200, and some traffic on eth0.
const FIRST: &str = "\
@@host
web-01-vm
@@arch
x86_64
@@os
Debian GNU/Linux 12 (bookworm)
@@uptime
5359162.33 1.00
@@cpu
model name\t: AMD EPYC-Milan Processor
@@stat
cpu  100 0 100 800 0 0 0 0
cpu0 50 0 50 400 0 0 0 0
cpu1 50 0 50 400 0 0 0 0
@@memory
MemTotal:         996608 kB
MemAvailable:     565480 kB
SwapTotal:             0 kB
SwapFree:              0 kB
@@net
    lo: 10 0 0 0 0 0 0 0 10 0 0 0 0 0 0 0
  eth0: 1000 0 0 0 0 0 0 0 500 0 0 0 0 0 0 0
docker0: 50000 0 0 0 0 0 0 0 50000 0 0 0 0 0 0 0
@@route
Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask
eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000
@@df
Filesystem     1024-blocks    Used Available Capacity Mounted on
/dev/vda1          9656904 3240180   5994708      36% /
tmpfs                99664     560     99104       1% /run
";

const SECOND: &str = "\
@@host
web-01-vm
@@arch
x86_64
@@os
Debian GNU/Linux 12 (bookworm)
@@uptime
5359164.33 1.00
@@cpu
model name\t: AMD EPYC-Milan Processor
@@stat
cpu  150 0 150 900 0 0 0 0
cpu0 100 0 50 400 0 0 0 0
cpu1 50 0 100 500 0 0 0 0
@@memory
MemTotal:         996608 kB
MemAvailable:     565480 kB
SwapTotal:             0 kB
SwapFree:              0 kB
@@net
    lo: 10 0 0 0 0 0 0 0 10 0 0 0 0 0 0 0
  eth0: 9192 0 0 0 0 0 0 0 4596 0 0 0 0 0 0 0
docker0: 60000 0 0 0 0 0 0 0 60000 0 0 0 0 0 0 0
@@route
Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask
eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000
@@df
Filesystem     1024-blocks    Used Available Capacity Mounted on
/dev/vda1          9656904 3240180   5994708      36% /
tmpfs                99664     560     99104       1% /run
";

#[derive(Default)]
struct IndependentMonitor {
    commands: Arc<Mutex<Vec<String>>>,
    samples: Arc<AtomicUsize>,
    failed: Arc<std::sync::atomic::AtomicBool>,
    discoveries: AtomicUsize,
}

struct IndependentLink {
    commands: Arc<Mutex<Vec<String>>>,
    samples: Arc<AtomicUsize>,
    failed: Arc<std::sync::atomic::AtomicBool>,
}

impl shellrs::monitor::MonitorLink for IndependentLink {
    fn label(&self) -> String {
        "ADB · Helper A133 (board-a)".into()
    }
    fn sample(
        &mut self,
        command: &str,
        _: &std::sync::atomic::AtomicBool,
    ) -> Result<String, String> {
        self.commands.lock().unwrap().push(command.to_owned());
        self.samples.fetch_add(1, Ordering::SeqCst);
        if self.failed.load(Ordering::Acquire) {
            Err("ADB disconnected".into())
        } else {
            Ok(format!(
                "{FIRST}\n@@mtop\nunit: MB\ngpu\n12.5\n@@mtop-end\n"
            ))
        }
    }
}

impl shellrs::monitor::MonitorConnector for IndependentMonitor {
    fn connect(
        &self,
        identity: &str,
        settings: &shellrs::host::MonitorSettings,
        _: &[(String, HostLogin)],
        _: &std::sync::atomic::AtomicBool,
    ) -> Result<Option<Box<dyn shellrs::monitor::MonitorLink>>, String> {
        assert!(identity.contains("@@boot_id"));
        assert_eq!(settings.channel, shellrs::host::MonitorChannel::Auto);
        self.discoveries.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Box::new(IndependentLink {
            commands: self.commands.clone(),
            samples: self.samples.clone(),
            failed: self.failed.clone(),
        })))
    }
}

async fn serial_monitor(
    cx: &mut TestAppContext,
    line: Arc<FakeTerminalFactory>,
    connector: Option<Arc<dyn shellrs::monitor::MonitorConnector>>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    let mut store = HostStore::empty();
    let id = store.insert_unnotified(HostDraft::serial(
        "console",
        "COM3",
        shellrs::host::SerialSettings {
            linux_shell: true,
            ..Default::default()
        },
        None,
    ));
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(line)),
    );
    if let Some(connector) = connector {
        cx.update(|cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.set_monitor_connector(connector, cx)
            })
        });
    }
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(id)), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
        workspace
            .read(cx)
            .terminal(id, cx)
            .is_some_and(|terminal| terminal.read(cx).lifecycle(cx) == TerminalLifecycle::Running)
    })
    .await;
    in_frame(cx, handle, |window, cx| window.click("tool-monitor", cx));
    (handle, workspace)
}

#[gpui_kit::test]
async fn serial_monitor_keeps_last_data_while_busy_and_resumes_without_an_error(
    cx: &mut TestAppContext,
) {
    let sample = format!("{FIRST}\n@@mtop\nunit: MB\ngpu\n12.5\n@@mtop-end\n");
    let line = Arc::new(FakeTerminalFactory::answering(&[&sample, SECOND]));
    let (handle, _workspace) = serial_monitor(cx, line.clone(), None).await;
    wait_for_label(cx, handle, "monitor-host", "web-01-vm").await;
    let top = in_frame(cx, handle, |window, _| {
        assert!(window.try_find("monitor-hardware:gpu").is_none());
        window.find("monitor-host").bounds().top()
    });
    assert!(
        line.exec_commands()
            .iter()
            .all(|command| !command.contains("@@mtop"))
    );
    line.monitor_busy.store(true, Ordering::Release);
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        window.try_find("monitor-paused").is_some()
    })
    .await;
    let before = line.exec_count();
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("monitor-host").label(), Some("web-01-vm"));
        assert!(window.try_find("monitor-message").is_none());
        assert_eq!(window.find("monitor-host").bounds().top(), top);
        assert!(
            window
                .find("monitor-paused")
                .label()
                .unwrap()
                .contains("上次更新")
        );
    });
    line.monitor_busy.store(false, Ordering::Release);
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        line.exec_count() > before && window.try_find("monitor-paused").is_none()
    })
    .await;
}

#[gpui_kit::test]
async fn independent_adb_monitor_never_competes_with_busy_serial_and_falls_back_on_disconnect(
    cx: &mut TestAppContext,
) {
    let identity =
        "@@boot_id\n00000000-0000-4000-8000-000000000001\n@@user\nroot\n@@addresses\n192.0.2.3\n";
    let line = Arc::new(FakeTerminalFactory::answering(&[identity, FIRST]));
    let connector = Arc::new(IndependentMonitor::default());
    let (handle, _workspace) = serial_monitor(cx, line.clone(), Some(connector.clone())).await;
    wait_for_label(cx, handle, "monitor-channel", "ADB · Helper A133 (board-a)").await;
    in_frame(cx, handle, |window, _| {
        assert!(window.find("monitor-hardware:gpu").visible())
    });
    line.monitor_busy.store(true, Ordering::Release);
    let samples = connector.samples.load(Ordering::SeqCst);
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        connector.samples.load(Ordering::SeqCst) > samples
    })
    .await;
    assert_eq!(line.exec_count(), 1);
    assert_eq!(connector.discoveries.load(Ordering::SeqCst), 1);
    assert!(
        connector
            .commands
            .lock()
            .unwrap()
            .iter()
            .all(|command| command.contains("@@mtop"))
    );
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("monitor-paused").is_none());
    });
    connector.failed.store(true, Ordering::Release);
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        window.try_find("monitor-paused").is_some()
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("monitor-host").label(), Some("web-01-vm"));
        assert!(window.try_find("monitor-message").is_none());
        assert!(window.try_find("monitor-hardware:gpu").is_none());
    });
    line.monitor_busy.store(false, Ordering::Release);
    wait_for_label(cx, handle, "monitor-channel", "串口空闲采集").await;
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        window.try_find("monitor-paused").is_none()
    })
    .await;
    assert_eq!(connector.discoveries.load(Ordering::SeqCst), 1);
    assert!(
        line.exec_commands()
            .iter()
            .all(|command| !command.contains("@@mtop"))
    );
}

#[gpui_kit::test]
async fn serial_monitor_preferences_are_edited_independently_per_host(cx: &mut TestAppContext) {
    let mut hosts = HostStore::empty();
    let first = hosts.insert_unnotified(HostDraft::serial(
        "COM3",
        "COM3",
        shellrs::host::SerialSettings {
            linux_shell: true,
            ..Default::default()
        },
        None,
    ));
    let other = hosts.insert_unnotified(HostDraft::serial(
        "COM5",
        "COM5",
        shellrs::host::SerialSettings {
            linux_shell: true,
            ..Default::default()
        },
        None,
    ));
    let (handle, workspace) = open_workspace_with_store(cx, hosts);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(EditHost(first)), cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("host-monitor-channel").value(),
            Some("自动（SSH → ADB → 串口）")
        );
        window.within("host-monitor-channel").click("input", cx);
    });
    for key in ["down", "enter"] {
        in_frame(cx, handle, |window, cx| window.press(key, cx));
    }
    in_frame(cx, handle, |window, cx| {
        window.click("host-monitor-adb", cx);
        window.input("board-a", cx);
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
    cx.update(|cx| {
        let hosts = workspace.read(cx).store().read(cx);
        assert_eq!(
            hosts.host(first).unwrap().monitor.channel,
            shellrs::host::MonitorChannel::Adb
        );
        assert_eq!(hosts.host(first).unwrap().monitor.adb_serial, "board-a");
        assert_eq!(
            hosts.host(other).unwrap().monitor,
            shellrs::host::MonitorSettings::default()
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.click("host-search", cx);
        window.dispatch_action(Box::new(EditHost(first)), cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("host-monitor-channel").value(), Some("ADB"));
        assert_eq!(window.find("host-monitor-adb").value(), Some("board-a"));
    });
}

/// The seeded workspace with web-01's terminal in front and the monitor
/// open; every terminal's commands are answered by `factory`.
fn open_monitor(cx: &mut TestAppContext, factory: Arc<FakeTerminalFactory>) -> WindowHandle<Root> {
    let (handle, _) = open_workspace_with_remote_factory(cx, HostStore::seed(), factory);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("tool-monitor", cx));
    handle
}

async fn wait_for_label(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    id: &'static str,
    label: &str,
) {
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window
            .try_find(id)
            .is_some_and(|element| element.label() == Some(label))
    })
    .await;
}

#[gpui_kit::test]
async fn the_monitor_shows_the_hosts_cpu_memory_network_and_disks(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[FIRST, SECOND]));
    let handle = open_monitor(cx, factory.clone());

    // The load is the second reading's against the first.
    wait_for_label(cx, handle, "monitor-cpu-usage", "50.0%").await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("monitor-host").label(), Some("web-01-vm"));
        assert_eq!(window.find("monitor-arch").label(), Some("x86_64"));
        assert_eq!(
            window.find("monitor-os").label(),
            Some("Debian GNU/Linux 12 (bookworm)")
        );
        // The second reading's uptime: 62 days and 2364 seconds.
        assert_eq!(
            window.find("monitor-uptime").label(),
            Some("62 天 00:39:24")
        );
        // Two cores fit the row shown at once: nothing to unfold.
        assert_eq!(
            window.find("monitor-cpu-cores").label(),
            Some("核 0：100.0%，核 1：33.3%")
        );
        assert!(window.try_find("monitor-cores-toggle").is_none());
        assert_eq!(
            window.find("monitor-memory-amount").label(),
            Some("421.02 MB / 973.25 MB")
        );
        // No swap, no swap row.
        assert!(window.try_find("monitor-swap-usage").is_none());
        // The main interface only, though docker0 carried more: the
        // default route goes out of eth0.
        let eth0 = window.find("monitor-interface:eth0");
        assert!(
            eth0.label()
                .is_some_and(|label| label.starts_with("上传 ") && !label.contains('—')),
            "{eth0:?}"
        );
        assert!(window.try_find("monitor-interface:docker0").is_none());
        assert_eq!(
            window.find("monitor-interfaces-toggle").checked(),
            Some(false)
        );
        // Loopback is not worth a row, nor is a memory file system a disk.
        assert!(window.try_find("monitor-interface:lo").is_none());
        assert_eq!(
            window.find("monitor-disk:/").label(),
            Some("35.1%，3.09 GB / 9.21 GB")
        );
        assert!(window.try_find("monitor-disk:/run").is_none());
    });

    // Unfolded, every interface; and folded away again.
    in_frame(cx, handle, |window, cx| {
        window.click("monitor-interfaces-toggle", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("monitor-interfaces-toggle").checked(),
            Some(true)
        );
        assert!(window.try_find("monitor-interface:eth0").is_some());
        assert!(window.try_find("monitor-interface:docker0").is_some());
    });
    in_frame(cx, handle, |window, cx| {
        window.click("monitor-interfaces-toggle", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("monitor-interface:docker0").is_none());
    });

    // The first reading asked for everything; the next one for the load
    // only, which starts no process on the host but its `sh`.
    let commands = factory.exec_commands();
    assert!(commands[0].contains("@@host") && commands[0].contains("@@df"));
    assert!(!commands[1].contains("@@host") && !commands[1].contains("@@df"));
}

#[gpui_kit::test]
async fn the_monitor_reads_the_host_only_while_it_shows(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[FIRST]));
    let handle = open_monitor(cx, factory.clone());
    wait_for_label(cx, handle, "monitor-host", "web-01-vm").await;

    // Hidden: nothing more runs on the host.
    in_frame(cx, handle, |window, cx| window.click("tool-monitor", cx));
    let hidden = factory.exec_count();
    cx.executor().advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
    assert_eq!(factory.exec_count(), hidden);

    // Back on screen, it reads again, and shows what it had meanwhile.
    in_frame(cx, handle, |window, cx| window.click("tool-monitor", cx));
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("monitor-host").label(), Some("web-01-vm"));
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.exec_count() > hidden
    })
    .await;

    // Nor while another tool is showing.
    in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
    let other = factory.exec_count();
    cx.executor().advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
    assert_eq!(factory.exec_count(), other);
}

#[gpui_kit::test]
async fn the_monitor_says_when_the_terminal_is_not_connected(cx: &mut TestAppContext) {
    // web-01's terminal is the first started, and exits at once.
    let handle = open_monitor(cx, Arc::new(FakeTerminalFactory::exit_first()));
    wait_for_label(
        cx,
        handle,
        "monitor-message",
        "终端没有连接。连接后这里显示主机的 CPU、内存、网络和磁盘。",
    )
    .await;
}

#[gpui_kit::test]
async fn the_monitor_says_it_reads_linux_hosts_only(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&["@@unsupported\nDarwin\n"]));
    let handle = open_monitor(cx, factory);
    wait_for_label(
        cx,
        handle,
        "monitor-message",
        "暂不支持 Darwin 的监控，目前只支持 Linux 主机。",
    )
    .await;
}

#[gpui_kit::test]
fn the_monitor_is_not_offered_on_a_host_known_not_to_run_linux(cx: &mut TestAppContext) {
    let mut store = HostStore::seed();
    store.set_host_os_unnotified(HostId(WEB_01), Some(HostOs::Debian));
    store.set_host_os_unnotified(HostId(STAGING_API), Some(HostOs::MacOs));
    let (handle, _) = open_workspace_with_store(cx, store);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("tool-monitor", cx));
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("tool-sidebar", INITIAL_WEB_TERMINAL)).label(),
            Some("系统监控")
        );
    });

    // On the Mac the monitor has no button, and the sidebar showing it
    // goes; the other tools stay on offer.
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_STAGING_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("tool-monitor").is_none());
        assert!(window.try_find("tool-connections").is_none());
        assert!(window.try_find("tool-processes").is_none());
        assert!(window.try_find("tool-services").is_none());
        assert!(window.find("tool-history").visible());
        assert!(
            window
                .try_find(("tool-sidebar", INITIAL_STAGING_TERMINAL))
                .is_none()
        );
    });

    // Back on the Linux host it is as it was.
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("tool-monitor").checked(), Some(true));
        assert!(
            window
                .try_find(("tool-sidebar", INITIAL_WEB_TERMINAL))
                .is_some()
        );
    });

    // The shortcut on the Mac shows the first tool it has instead.
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_STAGING_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ToggleToolSidebar), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window
                .find(("tool-sidebar", INITIAL_STAGING_TERMINAL))
                .label(),
            Some("命令片段")
        );
    });
}

#[gpui_kit::test]
async fn the_monitor_goes_once_the_host_turns_out_not_to_run_linux(cx: &mut TestAppContext) {
    // Every terminal reports a Mac as soon as it connects.
    let (handle, workspace) = open_workspace_with_remote_factory(
        cx,
        HostStore::seed(),
        Arc::new(FakeTerminalFactory::reports_os(HostOs::MacOs)),
    );
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("tool-monitor").is_none()
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert!(window.find("tool-snippets").visible());
    });
    let os = workspace.read_with(cx, |workspace, cx| {
        workspace
            .store()
            .read(cx)
            .host(HostId(WEB_01))
            .and_then(|host| host.os)
    });
    assert_eq!(os, Some(HostOs::MacOs));
}

/// A host with `cores` cores, every one a little busier than the last.
fn many_cores(cores: u64, busy: u64) -> String {
    let lines: String = (0..cores)
        .map(|core| format!("cpu{core} {} 0 0 1000 0 0 0 0\n", busy * core))
        .collect();
    FIRST.replace(
        "cpu  100 0 100 800 0 0 0 0\ncpu0 50 0 50 400 0 0 0 0\ncpu1 50 0 50 400 0 0 0 0\n",
        &format!(
            "cpu  {} 0 0 {} 0 0 0 0\n{lines}",
            busy * cores * cores / 2,
            1000 * cores
        ),
    )
}

#[gpui_kit::test]
async fn sixty_four_cores_show_one_row_until_unfolded(cx: &mut TestAppContext) {
    let (first, second) = (
        many_cores(64, 1),
        many_cores(64, 2).replace("5359162.33", "5359164.33"),
    );
    let factory = Arc::new(FakeTerminalFactory::answering(&[&first, &second]));
    let handle = open_monitor(cx, factory);
    // A reading against the one before, so there are loads to draw.
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window
            .try_find("monitor-cpu-usage")
            .is_some_and(|usage| usage.label() != Some("—"))
    })
    .await;
    // The first frame measures the row; the next one fills it.
    in_frame(cx, handle, |window, cx| window.simulate_next_frame(cx));
    let bars = |cx: &mut TestAppContext| {
        in_frame(cx, handle, |window, _| {
            (0..64_u64)
                .filter_map(|core| window.try_find(("monitor-core", core)))
                .map(|bar| bar.bounds())
                .collect::<Vec<_>>()
        })
    };
    let rows = |bars: &[gpui_kit::Bounds<gpui_kit::Pixels>]| {
        let mut tops: Vec<_> = bars.iter().map(|bar| bar.top()).collect();
        tops.dedup();
        tops.len()
    };

    // One row at first: as many as fit beside the gauge.
    let first_row = bars(cx);
    assert!(!first_row.is_empty() && first_row.len() < 64);
    assert_eq!(rows(&first_row), 1);
    in_frame(cx, handle, |window, _| {
        let toggle = window.find("monitor-cores-toggle");
        assert_eq!(toggle.label(), Some("64 核"));
        assert_eq!(toggle.checked(), Some(false));
    });

    // Unfolded, all of them, wrapping onto three rows inside the column;
    // the share sits at the column's right edge.
    in_frame(cx, handle, |window, cx| {
        window.click("monitor-cores-toggle", cx)
    });
    let all = bars(cx);
    assert_eq!(all.len(), 64);
    assert_eq!(rows(&all), 3);
    in_frame(cx, handle, |window, _| {
        let column_right = window.find("monitor-cpu-usage").bounds().right();
        assert!(all.iter().all(|bar| bar.right() <= column_right));
    });
}
