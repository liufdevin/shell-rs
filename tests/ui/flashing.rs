use crate::support::*;
use shellrs::app::{CloseFlashing, OpenFlashing};

fn enable_flashing(cx: &mut TestAppContext, workspace: &Entity<Workspace>) {
    let settings = workspace.read_with(cx, |workspace, _| workspace.settings().clone());
    settings.update(cx, |store, cx| {
        store.update(|settings| settings.flashing_enabled = true, cx);
    });
    cx.run_until_parked();
}

#[cfg(windows)]
#[gpui_kit::test]
fn flashing_is_hidden_until_enabled_in_application_settings(cx: &mut TestAppContext) {
    init_app(cx);
    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("open-flashing").is_none());
        window.dispatch_action(Box::new(OpenFlashing), cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("flashing").is_none());
        window.click("open-settings", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-7", cx);
    });
    for _ in 0..8 {
        if in_frame(cx, handle, |window, _| {
            window
                .try_find("flashing-enabled")
                .is_some_and(|field| field.visible())
        }) {
            break;
        }
        in_frame(cx, handle, |window, cx| {
            let bounds = window.find("settings").bounds();
            window.dispatch_event(
                gpui_kit::ScrollWheelEvent {
                    position: point(bounds.right() - px(50.), bounds.center().y),
                    delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-600.))),
                    modifiers: Default::default(),
                    touch_phase: gpui_kit::TouchPhase::Moved,
                }
                .to_platform_input(),
                cx,
            );
        });
    }
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("flashing-enabled").checked(), Some(false));
        window.click("flashing-enabled", cx);
    });
    workspace.read_with(cx, |workspace, cx| {
        assert!(workspace.settings().read(cx).settings().flashing_enabled);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("open-flashing").visible());
        window.click("flashing-enabled", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("open-flashing").is_none());
    });
}

#[gpui_kit::test]
fn opening_the_flashing_tab_shows_the_firmware_picker_and_log(cx: &mut TestAppContext) {
    init_app(cx);
    let (handle, workspace) = open_workspace(cx);
    enable_flashing(cx, &workspace);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(OpenFlashing), cx)
    });
    cx.run_until_parked();
    in_frame(cx, handle, |window, _cx| {
        assert!(window.try_find("flashing").is_some());
        assert!(window.try_find("flashing-choose").is_some());
        assert!(window.try_find("flashing-log").is_some());
        assert!(window.try_find("flashing-adb").is_none());
        assert!(window.try_find("flashing-elapsed").is_none());
        assert!(window.try_find("flashing-resume").is_none());
    });
}

#[gpui_kit::test]
fn partition_mode_is_saved_without_arming_an_empty_selection(cx: &mut TestAppContext) {
    init_app(cx);
    let (handle, workspace) = open_workspace(cx);
    enable_flashing(cx, &workspace);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(OpenFlashing), cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("flashing-mode-partitions", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("flashing-mode-partitions").checked(),
            Some(true)
        );
        assert_eq!(
            window.find("flashing-partitions-all").checked(),
            Some(false)
        );
        assert!(window.try_find("flashing-elapsed").is_none());
        window.dispatch_action(Box::new(CloseFlashing), cx);
    });
    workspace.read_with(cx, |workspace, cx| {
        assert!(
            workspace
                .settings()
                .read(cx)
                .settings()
                .flashing_partition_mode
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(OpenFlashing), cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("flashing-mode-partitions").checked(),
            Some(true)
        );
        assert!(window.try_find("flashing-partitions").is_some());
        assert!(window.try_find("flashing-resume").is_none());
    });
}

#[gpui_kit::test]
fn reopening_flashing_restores_the_selected_erase_mode(cx: &mut TestAppContext) {
    use shellrs::flashing::EraseMode;

    init_app(cx);
    let (handle, workspace) = open_workspace(cx);
    enable_flashing(cx, &workspace);
    for mode in [
        EraseMode::EraseLogical,
        EraseMode::EraseAll,
        EraseMode::KeepData,
    ] {
        in_frame(cx, handle, |window, cx| {
            window.dispatch_action(Box::new(OpenFlashing), cx)
        });
        cx.run_until_parked();
        let id = gpui_kit::SharedString::from(format!("flashing-mode-{}", mode.flag()));
        in_frame(cx, handle, |window, cx| window.click(id.clone(), cx));
        cx.run_until_parked();
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(
                workspace.settings().read(cx).settings().flashing_erase_mode,
                mode
            );
        });
        in_frame(cx, handle, |window, cx| {
            window.dispatch_action(Box::new(CloseFlashing), cx)
        });
        cx.run_until_parked();
        in_frame(cx, handle, |window, cx| {
            window.dispatch_action(Box::new(OpenFlashing), cx)
        });
        cx.run_until_parked();
        in_frame(cx, handle, |window, _| {
            assert_eq!(window.find(id.clone()).checked(), Some(true));
        });
    }
}

#[gpui_kit::test]
fn closing_the_flashing_tab_removes_it(cx: &mut TestAppContext) {
    init_app(cx);
    let (handle, workspace) = open_workspace(cx);
    enable_flashing(cx, &workspace);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(OpenFlashing), cx)
    });
    cx.run_until_parked();
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(CloseFlashing), cx)
    });
    cx.run_until_parked();
    in_frame(cx, handle, |window, _cx| {
        assert!(window.try_find("flashing").is_none());
    });
}

#[gpui_kit::test]
fn reopening_flashing_preserves_the_saved_firmware_even_when_its_tool_is_unavailable(
    cx: &mut TestAppContext,
) {
    init_app(cx);
    let folder = tempfile::tempdir().unwrap();
    let firmware = folder.path().join("offline.img");
    let (handle, workspace) = open_workspace(cx);
    enable_flashing(cx, &workspace);
    let settings = workspace.read_with(cx, |workspace, _| workspace.settings().clone());
    settings.update(cx, |store, cx| {
        store.update(
            |settings| {
                settings.flashing_firmware_path = Some(firmware.clone());
                settings.flashing_tool_directory = Some(folder.path().to_path_buf());
            },
            cx,
        );
    });
    for _ in 0..2 {
        in_frame(cx, handle, |window, cx| {
            window.dispatch_action(Box::new(OpenFlashing), cx);
        });
        in_frame(cx, handle, |window, cx| {
            assert_eq!(
                window.find("flashing-firmware-path").label(),
                firmware.to_str()
            );
            assert!(window.try_find("flashing-elapsed").is_none());
            window.dispatch_action(Box::new(CloseFlashing), cx);
        });
    }
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.settings().flashing_firmware_path),
        Some(firmware)
    );
}

#[cfg(windows)]
#[gpui_kit::test]
fn choosing_a_valid_tool_directory_is_saved_and_restored(cx: &mut TestAppContext) {
    init_app(cx);
    let directory = tempfile::tempdir().unwrap();
    let tool = directory.path().join("PhoenixTools");
    std::fs::create_dir(&tool).unwrap();
    std::fs::write(tool.join("PhoenixConsole.exe"), []).unwrap();
    std::fs::write(tool.join("adb.exe"), []).unwrap();
    let (handle, workspace) = open_workspace(cx);
    enable_flashing(cx, &workspace);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(OpenFlashing), cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("flashing-choose-tool", cx)
    });
    cx.simulate_path_prompt_response(|options| {
        assert!(options.directories && !options.files && !options.multiple);
        Some(vec![tool.clone()])
    });
    cx.run_until_parked();
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(
            workspace
                .settings()
                .read(cx)
                .settings()
                .flashing_tool_directory,
            Some(tool.clone())
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.click("flashing-choose-tool", cx)
    });
    cx.simulate_path_prompt_response(|_| Some(vec![directory.path().to_path_buf()]));
    cx.run_until_parked();
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(
            workspace
                .settings()
                .read(cx)
                .settings()
                .flashing_tool_directory,
            Some(tool.clone())
        );
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(CloseFlashing), cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(OpenFlashing), cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .find("flashing-tool-directory")
                .label()
                .unwrap()
                .contains(tool.to_str().unwrap())
        );
        assert!(enabled(&window.find("flashing-choose")));
    });
}
