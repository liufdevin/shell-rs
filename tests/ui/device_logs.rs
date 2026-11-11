use crate::support::*;
use shellrs::{
    app::DeviceLogAction,
    device_logs::{DeviceLogCommand, LogLevel, LogOptions},
};

#[gpui_kit::test]
async fn log_split_is_opt_in_equal_width_resizes_and_restores_the_previous_layout(
    cx: &mut TestAppContext,
) {
    let (handle, workspace, _, factory) = open_logs(cx, true, false, &[PROBE_ANDROID, FIRST]).await;
    line(cx, handle, 2).await;
    cx.simulate_window_resize(handle.into(), size(px(1400.), px(900.)));
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1u64), cx);
        let area = workspace.read(cx).dock_area().clone();
        area.update(cx, |area, cx| {
            area.set_dock_size(
                gpui_kit::component::dock::DockPlacement::Right,
                px(384.),
                window,
                cx,
            )
        });
    });
    let original = cx.update(|cx| {
        workspace
            .read(cx)
            .dock_area()
            .read(cx)
            .dock_size(gpui_kit::component::dock::DockPlacement::Right)
            .unwrap()
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("device-logs-split").checked(), Some(false));
        window.click("device-logs-split", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        let terminal = window.try_find(("terminal-pane", 1u64));
        let logs = window.try_find("device-logs");
        terminal.zip(logs).is_some_and(|(terminal, logs)| {
            (terminal.bounds().size.width - logs.bounds().size.width).abs() <= px(1.)
        })
    })
    .await;
    let calls = factory.exec_count();
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("device-logs-split").checked(), Some(true));
        assert!(
            window
                .find(("device-log-line", 2u64))
                .label()
                .unwrap()
                .contains("broken")
        );
    });
    cx.simulate_window_resize(handle.into(), size(px(1200.), px(800.)));
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        (window.find(("terminal-pane", 1u64)).bounds().size.width
            - window.find("device-logs").bounds().size.width)
            .abs()
            <= px(1.)
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-split", cx)
    });
    cx.update(|cx| {
        assert_eq!(
            workspace
                .read(cx)
                .dock_area()
                .read(cx)
                .dock_size(gpui_kit::component::dock::DockPlacement::Right),
            Some(original)
        )
    });
    assert_eq!(factory.exec_count(), calls);
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-split", cx)
    });
    in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
    cx.update(|cx| {
        assert_eq!(
            workspace
                .read(cx)
                .dock_area()
                .read(cx)
                .dock_size(gpui_kit::component::dock::DockPlacement::Right),
            Some(original)
        )
    });
    in_frame(cx, handle, |window, cx| window.click("tool-logs", cx));
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("device-logs-split").checked(), Some(false))
    });
}

#[gpui_kit::test]
async fn log_font_controls_enlarge_rows_keep_logs_and_remember_the_size(cx: &mut TestAppContext) {
    let (handle, workspace, _, factory) = open_logs(cx, true, false, &[PROBE_ANDROID, FIRST]).await;
    line(cx, handle, 2).await;
    let before = in_frame(cx, handle, |window, _| {
        window.find(("device-log-line", 2u64)).bounds().size.height
    });
    let calls = factory.exec_count();
    for _ in 0..4 {
        in_frame(cx, handle, |window, cx| {
            window.click("device-logs-font-larger", cx)
        });
    }
    in_frame(cx, handle, |window, _| {
        assert!(window.find(("device-log-line", 2u64)).bounds().size.height > before);
        assert_eq!(
            window.find("device-logs-font-size").label(),
            Some("日志字号：18")
        );
    });
    cx.update(|cx| {
        assert_eq!(
            workspace
                .read(cx)
                .settings()
                .read(cx)
                .settings()
                .device_logs
                .font_size,
            18
        )
    });
    assert_eq!(factory.exec_count(), calls);
    in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
    in_frame(cx, handle, |window, cx| window.click("tool-logs", cx));
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("device-logs-font-size").label(),
            Some("日志字号：18")
        )
    });
}

#[gpui_kit::test]
async fn log_search_supports_regex_case_matching_lines_and_navigation(cx: &mut TestAppContext) {
    let (handle, _, _, factory) = open_logs(cx, true, false, &[PROBE_ANDROID, FIRST]).await;
    line(cx, handle, 2).await;
    let calls = factory.exec_count();
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("device-logs-search-regex").checked(),
            Some(false)
        );
        assert_eq!(
            window.find("device-logs-search-case").checked(),
            Some(false)
        );
        assert_eq!(
            window.find("device-logs-search-lines").checked(),
            Some(false)
        );
        window.click("device-logs-keyword", cx);
        window.input("BROKEN", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("device-logs-search-count").label(), Some("1/1"));
        assert!(window.find(("device-log-line", 1u64)).visible());
        assert!(window.find(("device-log-line", 2u64)).visible());
    });
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-search-lines", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find(("device-log-line", 1u64)).is_none());
        assert!(window.find(("device-log-line", 2u64)).visible());
    });
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-search-case", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("device-logs-search-count").label(),
            Some("无结果")
        );
        assert!(window.try_find(("device-log-line", 1u64)).is_none());
        assert!(window.try_find(("device-log-line", 2u64)).is_none());
    });
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-search-clear", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-search-regex", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-keyword", cx);
        window.input("Demo: (startup|broken)", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("device-logs-search-count").label(), Some("1/2"));
        assert!(window.find(("device-log-line", 1u64)).visible());
        assert!(window.find(("device-log-line", 2u64)).visible());
    });
    in_frame(cx, handle, |window, cx| window.press("enter", cx));
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("device-logs-search-count").label(), Some("2/2"));
    });
    in_frame(cx, handle, |window, cx| window.press("shift-enter", cx));
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("device-logs-search-count").label(), Some("1/2"));
    });
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-search-next", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("device-logs-search-count").label(), Some("2/2"));
    });
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-search-previous", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("device-logs-search-count").label(), Some("1/2"));
    });
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-search-clear", cx);
        window.click("device-logs-keyword", cx);
        window.input("[", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.find("device-logs-search-error").visible());
        assert!(window.find(("device-log-line", 1u64)).visible());
    });
    assert_eq!(factory.exec_count(), calls);
}

#[gpui_kit::test]
async fn long_log_lines_can_scroll_horizontally_without_ellipsis(cx: &mut TestAppContext) {
    let output = format!(
        "__SHELLLF_LOG_OK__\n11-07 01:00:00.000 1 2 I Demo: {} end\n",
        "readable log ".repeat(120)
    );
    let (handle, _, _, _) = open_logs(cx, true, false, &[PROBE_ANDROID, &output]).await;
    line(cx, handle, 1).await;
    let before = in_frame(cx, handle, |window, _| {
        let text = window.find(("log-text", 1u64)).bounds();
        assert!(text.size.width > window.find("device-logs").bounds().size.width * 2.);
        text.origin.x
    });
    in_frame(cx, handle, |window, cx| {
        window.scroll(
            "device-logs",
            gpui_kit::ScrollDelta::Pixels(point(px(-300.), px(0.))),
            cx,
        )
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.find(("log-text", 1u64)).bounds().origin.x < before);
    });
}

#[gpui_kit::test]
async fn log_toolbar_stays_within_the_sidebar_when_interface_text_is_enlarged(
    cx: &mut TestAppContext,
) {
    let (handle, _, _, _) = open_logs(cx, true, false, &[PROBE_ANDROID, FIRST]).await;
    line(cx, handle, 2).await;
    in_frame(cx, handle, |window, cx| {
        gpui_kit::component::Theme::global_mut(cx).font_size = px(20.);
        gpui_kit::component::Theme::sync_base(cx);
        window.refresh();
    });
    in_frame(cx, handle, |window, _| {
        let panel = window.find("device-logs").bounds();
        for id in [
            "device-logs-split",
            "device-logs-follow",
            "device-logs-search-regex",
            "device-logs-search-case",
            "device-logs-search-lines",
            "device-logs-search-previous",
            "device-logs-search-next",
            "device-logs-font-smaller",
            "device-logs-font-size",
            "device-logs-font-larger",
            "device-logs-pause",
            "device-logs-clear",
            "device-logs-export",
            "device-logs-filters",
            "device-logs-bookmarks",
            "device-logs-diagnostics",
            "device-logs-keyword",
        ] {
            let control = window.find(id).bounds();
            assert!(control.origin.x >= panel.origin.x, "{id}");
            assert!(
                control.origin.x + control.size.width <= panel.origin.x + panel.size.width,
                "{id}: {control:?} outside {panel:?}"
            );
        }
        let search = window.find("device-logs-keyword").bounds();
        for id in [
            "device-logs-follow",
            "device-logs-search-regex",
            "device-logs-search-case",
            "device-logs-search-lines",
        ] {
            assert!(
                window.find(id).bounds().bottom() <= search.top(),
                "{id} must remain above search"
            );
        }
        let controls: Vec<_> = [
            "device-logs-pause",
            "device-logs-follow",
            "device-logs-search-regex",
            "device-logs-search-case",
            "device-logs-search-lines",
            "device-logs-filters",
            "device-logs-bookmarks",
            "device-logs-diagnostics",
            "device-logs-export",
            "device-logs-clear",
        ]
        .into_iter()
        .map(|id| (id, window.find(id).bounds()))
        .collect();
        for (index, (first_id, first)) in controls.iter().enumerate() {
            for (second_id, second) in &controls[index + 1..] {
                assert!(
                    first.right() <= second.left()
                        || second.right() <= first.left()
                        || first.bottom() <= second.top()
                        || second.bottom() <= first.top(),
                    "{first_id} overlaps {second_id}"
                );
            }
        }
    });
}

#[gpui_kit::test]
async fn log_search_options_follow_the_follow_control_and_stay_out_of_filters(
    cx: &mut TestAppContext,
) {
    let (handle, workspace, _, _) = open_logs(cx, true, false, &[PROBE_ANDROID, FIRST]).await;
    line(cx, handle, 2).await;
    cx.simulate_window_resize(handle.into(), size(px(1500.), px(900.)));
    in_frame(cx, handle, |window, cx| {
        let area = workspace.read(cx).dock_area().clone();
        area.update(cx, |area, cx| {
            area.set_dock_size(
                gpui_kit::component::dock::DockPlacement::Right,
                px(850.),
                window,
                cx,
            )
        });
    });
    in_frame(cx, handle, |window, cx| {
        let mut previous = window.find("device-logs-follow").bounds();
        for id in [
            "device-logs-search-regex",
            "device-logs-search-case",
            "device-logs-search-lines",
        ] {
            let control = window.find(id).bounds();
            assert!((control.top() - previous.top()).abs() <= px(2.));
            assert!(control.left() >= previous.right());
            assert!(control.bottom() <= window.find("device-logs-keyword").bounds().top());
            previous = control;
        }
        window.click("device-logs-search-regex", cx);
        window.click("device-logs-filters", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("log-filter-search-regex").is_none());
        assert!(window.try_find("log-filter-search-case").is_none());
        assert!(window.try_find("log-filter-search-lines").is_none());
        window.press("escape", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("device-logs-search-regex").checked(),
            Some(true)
        )
    });
}

#[gpui_kit::test]
async fn device_logs_bookmarks_copy_and_filtered_export_work_through_buttons(
    cx: &mut TestAppContext,
) {
    let (handle, _, _, _) = open_logs(cx, true, false, &[PROBE_ANDROID, FIRST]).await;
    line(cx, handle, 2).await;
    in_frame(cx, handle, |window, cx| {
        window.click(("log-bookmark", 2u64), cx);
        window.click("device-logs-bookmarks", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find(("device-log-line", 1u64)).is_none());
        assert!(window.find(("device-log-line", 2u64)).visible());
        window.click(("log-copy", 2u64), cx);
    });
    assert!(
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap()
            .contains("broken")
    );
}

#[gpui_kit::test]
async fn android_filters_validate_input_and_follow_application_pids(cx: &mut TestAppContext) {
    let (handle, _, _, factory) = open_logs(cx, true, false, &[PROBE_ANDROID, FIRST]).await;
    line(cx, handle, 2).await;
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-filters", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("log-filter-object", cx);
        window.input("com.example;reboot", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("log-filter-error").visible());
        window.press("escape", cx);
    });
    *factory.exec_outputs.lock().unwrap() = std::collections::VecDeque::from([
        PROBE_ANDROID.into(),
        "__SHELLLF_LOG_OK__\n__SHELLLF_LOG_PIDS__ 99\n11-07 01:00:00.000 1 2 I Other: unrelated\n11-07 01:00:01.000 99 2 E Demo: selected\n".into()
    ]);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1u64), cx);
        window.click("device-logs-filters", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("log-filter-object", cx);
        window.input("com.example.demo", cx);
        window.click("log-filter-tag", cx);
        window.input("Demo", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("log-filter-apply", cx);
    });
    line(cx, handle, 2).await;
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find(("device-log-line", 1u64)).is_none());
        assert!(
            window
                .find(("device-log-line", 2u64))
                .label()
                .unwrap()
                .contains("selected")
        );
    });
    assert!(
        factory
            .exec_commands()
            .iter()
            .any(|command| command.contains("pidof"))
    );
}

#[gpui_kit::test]
async fn ubuntu_service_log_shortcut_switches_to_journal_and_preserves_priority(
    cx: &mut TestAppContext,
) {
    let (handle, _, _, factory) = open_logs(
        cx,
        true,
        false,
        &[PROBE_KERNEL, "__SHELLLF_LOG_OK__\n<6>[1] ready\n"],
    )
    .await;
    line(cx, handle, 1).await;
    *factory.exec_outputs.lock().unwrap() = std::collections::VecDeque::from([
        PROBE_KERNEL.into(),
        "__SHELLLF_LOG_OK__\n{\"PRIORITY\":\"3\",\"_SYSTEMD_UNIT\":\"demo.service\",\"MESSAGE\":\"service failure\"}\n".into(),
    ]);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(DeviceLogAction(DeviceLogCommand::OpenService(
                "demo.service".into(),
            ))),
            cx,
        );
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        window
            .try_find(("device-log-line", 1u64))
            .is_some_and(|line| {
                line.label()
                    .is_some_and(|text| text.contains("service failure"))
            })
    })
    .await;
    assert!(
        factory
            .exec_commands()
            .iter()
            .any(|command| command.contains("journalctl") && command.contains("demo.service"))
    );
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .find("device-logs-source")
                .label()
                .unwrap()
                .contains("journalctl")
        );
    });
}

#[gpui_kit::test]
async fn ubuntu_file_logs_use_the_selected_absolute_path(cx: &mut TestAppContext) {
    let (handle, _, _, factory) = open_logs(
        cx,
        true,
        false,
        &[PROBE_KERNEL, "__SHELLLF_LOG_OK__\n<6>[1] ready\n"],
    )
    .await;
    line(cx, handle, 1).await;
    *factory.exec_outputs.lock().unwrap() = std::collections::VecDeque::from([
        PROBE_KERNEL.into(),
        "__SHELLLF_LOG_OK__\nERROR file failure\n".into(),
    ]);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(DeviceLogAction(DeviceLogCommand::ApplyOptions(
                LogOptions::file("/var/log/custom app.log"),
            ))),
            cx,
        );
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        window
            .try_find(("device-log-line", 1u64))
            .is_some_and(|line| {
                line.label()
                    .is_some_and(|text| text.contains("file failure"))
            })
    })
    .await;
    assert!(
        factory
            .exec_commands()
            .iter()
            .any(|command| command.contains("tail -n 2000 --")
                && command.contains("/var/log/custom app.log"))
    );
}

#[gpui_kit::test]
async fn diagnostic_bundle_exports_successes_and_permission_failures(cx: &mut TestAppContext) {
    use std::io::Read as _;
    let (handle, _, _, factory) = open_logs(
        cx,
        true,
        false,
        &[PROBE_KERNEL, "__SHELLLF_LOG_OK__\n<6>[1] ready\n"],
    )
    .await;
    line(cx, handle, 1).await;
    let mut replies = vec!["__SHELLLF_REPORT_EXIT__0\nUbuntu system information\n".to_string(); 9];
    replies[6] =
        "__SHELLLF_REPORT_EXIT__0\n__SHELLLF_LOG_ERROR__\nOperation not permitted\n".into();
    *factory.exec_outputs.lock().unwrap() = replies.into();
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-diagnostics", cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("diagnostic-bugreport").is_none());
        window.click("diagnostic-collect", cx);
    });
    cx.run_until_parked();
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("diagnostic.zip");
    cx.simulate_new_path_selection(|_| Some(output.clone()));
    cx.wait_for(handle.into(), Duration::from_secs(10), |_, _| {
        output.exists()
    })
    .await;
    let mut archive = zip::ZipArchive::new(std::fs::File::open(output).unwrap()).unwrap();
    let mut text = String::new();
    archive
        .by_name("manifest.json")
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(manifest["host"], "log-device");
    assert!(manifest["items"].as_array().unwrap().iter().any(|item| {
        item["status"] == "failed"
            && item["detail"]
                .as_str()
                .unwrap()
                .contains("Operation not permitted")
    }));
    assert!(archive.by_name("system.txt").is_ok());
    assert!(archive.by_name("displayed-logs.log").is_ok());
    assert!(archive.by_name("kernel-or-logcat.log").is_err());
}

#[gpui_kit::test]
async fn cancelling_diagnostic_destination_runs_no_collection_commands(cx: &mut TestAppContext) {
    let (handle, _, _, factory) = open_logs(cx, true, false, &[PROBE_ANDROID, FIRST]).await;
    line(cx, handle, 2).await;
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-diagnostics", cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("diagnostic-bugreport").visible());
        assert_eq!(window.find("diagnostic-screenshot").checked(), Some(false));
        window.click("diagnostic-collect", cx);
    });
    cx.run_until_parked();
    cx.simulate_new_path_selection(|_| None);
    cx.run_until_parked();
    assert!(
        factory
            .exec_commands()
            .iter()
            .all(|command| !command.contains("__SHELLLF_REPORT_EXIT__"))
    );
}

#[gpui_kit::test]
async fn android_diagnostic_screenshot_is_written_and_missing_adb_is_reported(
    cx: &mut TestAppContext,
) {
    use base64::Engine as _;
    use std::io::Read as _;
    let (handle, _, _, factory) = open_logs(cx, true, false, &[PROBE_ANDROID, FIRST]).await;
    line(cx, handle, 2).await;
    let image = include_bytes!("../../assets/logo/shelllf.png");
    let encoded = base64::engine::general_purpose::STANDARD.encode(image);
    let mut replies = vec!["__SHELLLF_REPORT_EXIT__0\nsample\n".to_string(); 9];
    replies.push(format!("__SHELLLF_REPORT_EXIT__0\n{encoded}\n"));
    replies.push("@@boot_id\n00000000-0000-4000-8000-000000000001\n".into());
    *factory.exec_outputs.lock().unwrap() = replies.into();
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-diagnostics", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("diagnostic-screenshot", cx);
        window.click("diagnostic-bugreport", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("diagnostic-collect", cx)
    });
    cx.run_until_parked();
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("android.zip");
    cx.simulate_new_path_selection(|_| Some(output.clone()));
    cx.wait_for(handle.into(), Duration::from_secs(10), |_, _| {
        output.exists()
    })
    .await;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(output).unwrap()).unwrap();
    let mut bytes = Vec::new();
    zip.by_name("screenshot.png")
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, image.as_slice());
    let mut manifest = String::new();
    zip.by_name("manifest.json")
        .unwrap()
        .read_to_string(&mut manifest)
        .unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert!(
        manifest["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["name"] == "android-bugreport.zip" && item["status"] == "failed")
    );
}

#[gpui_kit::test]
async fn cancelling_diagnostic_collection_preserves_an_existing_file(cx: &mut TestAppContext) {
    let (handle, _, _, factory) = open_logs(
        cx,
        true,
        false,
        &[PROBE_KERNEL, "__SHELLLF_LOG_OK__\n<6>[1] ready\n"],
    )
    .await;
    line(cx, handle, 1).await;
    *factory.exec_outputs.lock().unwrap() =
        std::collections::VecDeque::from(["__SHELLLF_REPORT_EXIT__0\nsample\n".into()]);
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-diagnostics", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("diagnostic-collect", cx)
    });
    cx.run_until_parked();
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("existing.zip");
    std::fs::write(&output, "original").unwrap();
    cx.simulate_new_path_selection(|_| Some(output.clone()));
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        window.try_find("diagnostic-stop").is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| window.click("diagnostic-stop", cx));
    cx.executor().advance_clock(Duration::from_secs(3));
    cx.run_until_parked();
    assert_eq!(std::fs::read_to_string(output).unwrap(), "original");
}

#[gpui_kit::test]
async fn device_logs_android_su_is_checked_before_reading(cx: &mut TestAppContext) {
    let (handle, _, _, factory) = open_logs(
        cx,
        true,
        true,
        &[PROBE_ANDROID, "__SHELLLF_LOG_SU__\n", FIRST],
    )
    .await;
    line(cx, handle, 2).await;
    let commands = factory.exec_commands();
    assert!(commands[1].contains("timeout 3 su -c 'id -u'"));
    assert!(commands[2].starts_with("timeout 8 su -c"));
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .find("device-logs-source")
                .label()
                .unwrap_or_default()
                .contains("logcat")
        );
    });
}

#[gpui_kit::test]
async fn device_logs_linux_sudo_reads_kernel_priorities(cx: &mut TestAppContext) {
    let (handle, _, _, factory) = open_logs(
        cx,
        true,
        true,
        &[
            PROBE_KERNEL,
            "__SHELLLF_LOG_SUDO__\n",
            "__SHELLLF_LOG_OK__\n<6>[1] kernel ready\n<3>[2] kernel failure\n",
        ],
    )
    .await;
    line(cx, handle, 2).await;
    assert!(factory.exec_commands()[1].contains("sudo -n -- dmesg -r"));
    assert!(factory.exec_commands()[2].contains("sudo -n -- dmesg -r"));
    assert!(!factory.exec_commands()[2].contains("sudo -n -- sh"));
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(DeviceLogAction(DeviceLogCommand::Level(LogLevel::Error))),
            cx,
        );
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find(("device-log-line", 1u64)).is_none());
        assert!(
            window
                .find(("device-log-line", 2u64))
                .label()
                .unwrap_or_default()
                .contains("kernel failure")
        );
    });
}

#[gpui_kit::test]
async fn device_logs_host_options_default_off_and_can_be_saved_and_reopened(
    cx: &mut TestAppContext,
) {
    let (handle, workspace, host, _) = open_logs(cx, false, false, &[]).await;
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(EditHost(host)), cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("host-device-logs-enabled").checked(),
            Some(false)
        );
        assert!(window.try_find("host-device-logs-elevated").is_none());
        window.click("host-device-logs-enabled", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("host-device-logs-elevated").checked(),
            Some(false)
        );
        window.click("host-device-logs-elevated", cx);
    });
    in_frame(cx, handle, |window, cx| window.click("commit", cx));
    wait_for_dialog_to_close(cx, handle).await;
    cx.update(|cx| {
        let settings = workspace
            .read(cx)
            .store()
            .read(cx)
            .host(host)
            .unwrap()
            .device_logs
            .clone();
        assert!(settings.enabled && settings.elevated);
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", 1u64), cx);
        window.dispatch_action(Box::new(EditHost(host)), cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("host-device-logs-enabled").checked(),
            Some(true)
        );
        assert_eq!(
            window.find("host-device-logs-elevated").checked(),
            Some(true)
        );
        window.press("escape", cx);
    });
}

#[derive(Default)]
struct LogConnector {
    commands: Arc<Mutex<Vec<String>>>,
    connections: AtomicUsize,
}

struct LogLink(Arc<Mutex<Vec<String>>>);

impl shellrs::monitor::MonitorConnector for LogConnector {
    fn connect(
        &self,
        identity: &str,
        _: &shellrs::host::MonitorSettings,
        _: &[(String, shellrs::host::HostLogin)],
        _: &std::sync::atomic::AtomicBool,
    ) -> Result<Option<Box<dyn shellrs::monitor::MonitorLink>>, String> {
        assert!(identity.contains("@@boot_id"));
        self.connections.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Box::new(LogLink(self.commands.clone()))))
    }
}

impl shellrs::monitor::MonitorLink for LogLink {
    fn label(&self) -> String {
        "ADB · test board".into()
    }
    fn sample(
        &mut self,
        command: &str,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<String, String> {
        if cancel.load(Ordering::Acquire) {
            return Err("cancelled".into());
        }
        self.0.lock().unwrap().push(command.into());
        if command.contains("__SHELLLF_LOG_UNSUPPORTED__") {
            Ok(PROBE_ANDROID.into())
        } else {
            Ok(FIRST.into())
        }
    }
}

#[gpui_kit::test]
async fn device_logs_serial_capture_uses_an_independent_channel_and_stops_when_hidden(
    cx: &mut TestAppContext,
) {
    let mut store = HostStore::empty();
    let mut draft = HostDraft::serial(
        "console",
        "COM3",
        shellrs::host::SerialSettings {
            linux_shell: true,
            ..Default::default()
        },
        None,
    );
    draft.device_logs.enabled = true;
    let host = store.insert_unnotified(draft);
    let serial = Arc::new(FakeTerminalFactory::answering(&[
        "@@boot_id\n00000000-0000-4000-8000-000000000001\n",
    ]));
    let (handle, workspace) = open_workspace_with_terminals(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedSerialTerminalTransportProvider::new(serial.clone())),
    );
    let connector = Arc::new(LogConnector::default());
    cx.update(|cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.set_monitor_connector(connector.clone(), cx)
        })
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(host)), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
        remote_lifecycle(&workspace, RemoteTerminalId(1), cx) == TerminalLifecycle::Running
    })
    .await;
    in_frame(cx, handle, |window, cx| window.click("tool-logs", cx));
    line(cx, handle, 2).await;
    assert_eq!(connector.connections.load(Ordering::SeqCst), 1);
    assert_eq!(serial.exec_count(), 1);
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-split", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(
            (window.find(("terminal-pane", 1u64)).bounds().size.width
                - window.find("device-logs").bounds().size.width)
                .abs()
                <= px(1.),
            "terminal={:?}, logs={:?}",
            window.find(("terminal-pane", 1u64)).bounds(),
            window.find("device-logs").bounds()
        );
    });
    assert_eq!(connector.connections.load(Ordering::SeqCst), 1);
    assert_eq!(serial.exec_count(), 1);
    serial.monitor_busy.store(true, Ordering::SeqCst);
    let count = connector.commands.lock().unwrap().len();
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        connector.commands.lock().unwrap().len() > count
    })
    .await;
    assert_eq!(serial.exec_count(), 1);
    in_frame(cx, handle, |window, cx| window.click("tool-logs", cx));
    let count = connector.commands.lock().unwrap().len();
    cx.executor().advance_clock(Duration::from_secs(3));
    cx.run_until_parked();
    assert_eq!(connector.commands.lock().unwrap().len(), count);
}

const PROBE_ANDROID: &str = "__SHELLLF_LOG_ANDROID__\n";

#[gpui_kit::test]
async fn device_logs_keep_different_hosts_buffers_separate(cx: &mut TestAppContext) {
    let (handle, workspace, _, factory) = open_logs(cx, true, false, &[PROBE_ANDROID, FIRST]).await;
    line(cx, handle, 2).await;
    let store = cx.update(|cx| workspace.read(cx).store().clone());
    let other = cx.update(|cx| {
        store.update(cx, |store, cx| {
            let mut draft = HostDraft::new(
                "other-device",
                "other.test",
                22,
                "tester",
                AuthKind::NoPassword,
                None,
            );
            draft.device_logs.enabled = true;
            store.insert(draft, cx)
        })
    });
    *factory.exec_outputs.lock().unwrap() = std::collections::VecDeque::from([
        PROBE_KERNEL.into(),
        "__SHELLLF_LOG_OK__\n<6>[1] kernel-only\n<4>[2] other-only\n".into(),
    ]);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(other)), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
        remote_lifecycle(&workspace, RemoteTerminalId(2), cx) == TerminalLifecycle::Running
    })
    .await;
    in_frame(cx, handle, |window, cx| window.click("tool-logs", cx));
    line(cx, handle, 2).await;
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .find(("device-log-line", 1u64))
                .label()
                .unwrap_or_default()
                .contains("kernel-only")
        );
    });
    *factory.exec_outputs.lock().unwrap() =
        std::collections::VecDeque::from([PROBE_ANDROID.into(), FIRST.into()]);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", 1u64), cx)
    });
    line(cx, handle, 2).await;
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .find(("device-log-line", 1u64))
                .label()
                .unwrap_or_default()
                .contains("startup")
        );
        assert!(
            !window
                .find(("device-log-line", 2u64))
                .label()
                .unwrap_or_default()
                .contains("other-only")
        );
    });
}
const PROBE_KERNEL: &str = "__SHELLLF_LOG_KERNEL__\n";
const FIRST: &str = "__SHELLLF_LOG_OK__\n11-07 01:00:00.000 1 2 I Demo: startup\n11-07 01:00:01.000 1 2 E Demo: broken\n";
const SECOND: &str = "__SHELLLF_LOG_OK__\n11-07 01:00:00.000 1 2 I Demo: startup\n11-07 01:00:01.000 1 2 E Demo: broken\n11-07 01:00:02.000 1 2 W Demo: later\n";

async fn open_logs(
    cx: &mut TestAppContext,
    enabled: bool,
    elevated: bool,
    replies: &[&str],
) -> (
    WindowHandle<Root>,
    Entity<Workspace>,
    HostId,
    Arc<FakeTerminalFactory>,
) {
    let mut store = HostStore::empty();
    let mut draft = HostDraft::new(
        "log-device",
        "example.test",
        22,
        "tester",
        AuthKind::NoPassword,
        None,
    );
    draft.device_logs.enabled = enabled;
    draft.device_logs.elevated = elevated;
    let id = store.insert_unnotified(draft);
    let factory = Arc::new(FakeTerminalFactory::answering(replies));
    let (handle, workspace) = open_workspace_with_remote_factory(cx, store, factory.clone());
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(id)), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
        remote_lifecycle(&workspace, RemoteTerminalId(1), cx) == TerminalLifecycle::Running
    })
    .await;
    if enabled {
        in_frame(cx, handle, |window, cx| window.click("tool-logs", cx));
    }
    (handle, workspace, id, factory)
}

async fn line(cx: &mut TestAppContext, handle: WindowHandle<Root>, id: u64) {
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        window.try_find(("device-log-line", id)).is_some()
    })
    .await;
}

#[gpui_kit::test]
async fn device_logs_are_opt_in_per_host_and_stop_when_disabled(cx: &mut TestAppContext) {
    let (handle, workspace, host, factory) =
        open_logs(cx, false, false, &[PROBE_ANDROID, FIRST]).await;
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("tool-logs").is_none())
    });
    assert_eq!(factory.exec_count(), 0);
    let store = cx.update(|cx| workspace.read(cx).store().clone());
    cx.update(|cx| {
        store.update(cx, |store, cx| {
            let mut draft = store.host(host).unwrap().draft();
            draft.device_logs.enabled = true;
            store.update(host, draft, cx);
        })
    });
    in_frame(cx, handle, |window, cx| window.click("tool-logs", cx));
    line(cx, handle, 2).await;
    cx.update(|cx| {
        store.update(cx, |store, cx| {
            let mut draft = store.host(host).unwrap().draft();
            draft.device_logs.enabled = false;
            store.update(host, draft, cx);
        })
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("tool-logs").is_none())
    });
    let count = factory.exec_count();
    cx.executor().advance_clock(Duration::from_secs(3));
    cx.run_until_parked();
    assert_eq!(factory.exec_count(), count);
}

#[gpui_kit::test]
async fn device_logs_filter_pause_resume_clear_and_export_without_clearing_the_device(
    cx: &mut TestAppContext,
) {
    let (handle, _, _, factory) = open_logs(cx, true, false, &[PROBE_ANDROID, FIRST]).await;
    line(cx, handle, 2).await;
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-pause", cx);
    });
    *factory.exec_outputs.lock().unwrap() = std::collections::VecDeque::from([SECOND.into()]);
    let count = factory.exec_count();
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.exec_count() > count
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find(("device-log-line", 3u64)).is_none());
        window.click("device-logs-pause", cx);
    });
    line(cx, handle, 3).await;
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(DeviceLogAction(DeviceLogCommand::Level(LogLevel::Error))),
            cx,
        );
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find(("device-log-line", 1u64)).is_none());
        assert!(window.find(("device-log-line", 2u64)).visible());
        window.click("device-logs-keyword", cx);
        window.input("BROKEN", cx);
    });
    let temporary = tempfile::tempdir().unwrap();
    let export = temporary.path().join("device.log");
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-export", cx)
    });
    cx.simulate_new_path_selection(|_| Some(export.clone()));
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        export.exists()
    })
    .await;
    let text = std::fs::read_to_string(export).unwrap();
    assert!(text.contains("broken") && !text.contains("startup") && !text.contains("later"));
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-clear", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find(("device-log-line", 2u64)).is_none())
    });
    assert!(
        factory
            .exec_commands()
            .iter()
            .all(|command| !command.contains("logcat -c")
                && !command.contains("dmesg -c")
                && !command.contains("BROKEN"))
    );
}

#[gpui_kit::test]
async fn device_logs_elevation_failure_is_visible_and_current_user_is_explicit(
    cx: &mut TestAppContext,
) {
    let (handle, _, _, factory) =
        open_logs(cx, true, true, &[PROBE_ANDROID, "__SHELLLF_LOG_DENIED__\n"]).await;
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        window.try_find("device-logs-current-user").is_some()
    })
    .await;
    assert!(
        !factory
            .exec_commands()
            .iter()
            .any(|command| command.contains("logcat -d"))
    );
    *factory.exec_outputs.lock().unwrap() =
        std::collections::VecDeque::from([PROBE_ANDROID.into(), FIRST.into()]);
    in_frame(cx, handle, |window, cx| {
        window.click("device-logs-current-user", cx)
    });
    line(cx, handle, 2).await;
    assert!(!factory.exec_commands().last().unwrap().contains("su -c"));
}

#[gpui_kit::test]
async fn device_logs_linux_permission_errors_remain_errors_instead_of_empty_logs(
    cx: &mut TestAppContext,
) {
    let (handle, _, _, factory) = open_logs(
        cx,
        true,
        false,
        &[
            PROBE_KERNEL,
            "__SHELLLF_LOG_ERROR__\ndmesg: read kernel buffer failed: Operation not permitted\n",
        ],
    )
    .await;
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, cx| {
        window.render_frame(cx);
        window.try_find("device-logs-error").is_some_and(|error| {
            error
                .label()
                .is_some_and(|text| text.contains("Operation not permitted"))
        })
    })
    .await;
    assert!(
        factory
            .exec_commands()
            .iter()
            .all(|command| !command.contains("sudo") && !command.contains("su -c"))
    );
}
