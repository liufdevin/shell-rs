//! 导入主机: the hosts `~/.ssh/config` and `~/.ssh/known_hosts` name,
//! previewed in the import dialog and added to the store.

use std::path::{Path, PathBuf};

use shellrs::analytics::{Analytics, Counter, Signal};
use shellrs::app::{ImportKnownHosts, ImportSshConfig};

use crate::support::*;

/// A home directory whose `.ssh` holds `files`.
fn ssh_home(files: &[(&str, &str)]) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let ssh = home.path().join(".ssh");
    std::fs::create_dir(&ssh).unwrap();
    for (name, text) in files {
        std::fs::write(ssh.join(name), text).unwrap();
    }
    home
}

fn ssh_dir(home: &tempfile::TempDir) -> PathBuf {
    home.path().join(".ssh")
}

/// Have the workspace report to a channel the test reads.
fn report(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
) -> mpsc::Receiver<Signal> {
    let (sender, receiver) = mpsc::channel();
    cx.update_window(handle.into(), |_, window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.set_analytics(Analytics::new(sender, true), window, cx)
        });
    })
    .unwrap();
    cx.run_until_parked();
    receiver
}

/// Point 导入 at `dir`, open the dialog with `action` and wait for it to
/// have read its file.
async fn open_import(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
    dir: &Path,
    action: Box<dyn gpui_kit::Action>,
) {
    workspace.update(cx, |workspace, _| {
        workspace.set_ssh_dir(Some(dir.to_path_buf()))
    });
    in_frame(cx, handle, |window, cx| window.dispatch_action(action, cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.try_find(("import-address", 0usize)).is_some()
            || window.try_find("import-empty").is_some()
    })
    .await;
}

/// Type `text` into a row's field.
fn type_into(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    field: (&'static str, usize),
    text: &str,
) {
    in_frame(cx, handle, |window, cx| {
        window.click(field, cx);
        window.input(text, cx);
    });
}

#[gpui_kit::test]
async fn xshell_import_from_sync_previews_groups_accounts_and_protocols_before_persisting(
    cx: &mut TestAppContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let sessions = directory.path().join("Sessions");
    let group = sessions.join("生产").join("华东");
    std::fs::create_dir_all(&group).unwrap();
    std::fs::write(group.join("app.xsh"), "[CONNECTION]\nProtocol=SSH\nHost=server.example.com\nPort=2222\nDescription=迁移的主机\n[CONNECTION:AUTHENTICATION]\nUserName=deploy\nPassword=DO_NOT_IMPORT\n").unwrap();
    std::fs::write(group.join("root.xsh"), "[CONNECTION]\nProtocol=SSH\nHost=server.example.com\nPort=2222\n[CONNECTION:AUTHENTICATION]\nUserName=root\n").unwrap();
    std::fs::write(group.join("switch.xsh"), "[CONNECTION]\nProtocol=TELNET\nHost=10.0.0.5\n[CONNECTION:AUTHENTICATION]\nUserName=admin\n").unwrap();
    std::fs::write(
        group.join("unsupported.xsh"),
        "[CONNECTION]\nProtocol=RLOGIN\nHost=other\n",
    )
    .unwrap();
    let database = directory.path().join("hosts.db");
    let mut store = HostStore::load(HostDatabase::open(&database).unwrap()).unwrap();
    let existing = store.insert_unnotified(HostDraft::new(
        "已有主机",
        "server.example.com",
        2222,
        "root",
        AuthKind::Password,
        None,
    ));
    HostDatabase::open(&database)
        .unwrap()
        .insert_host(store.host(existing).unwrap())
        .unwrap();
    let (handle, workspace) = open_workspace_with_store(cx, store);
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-6", cx)
    });
    in_frame(cx, handle, |window, cx| window.click("import-xshell", cx));
    in_frame(cx, handle, |window, cx| {
        window.click("import-choose-folder", cx)
    });
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|_| Some(vec![sessions]));
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window.try_find(("import-address", 0usize)).is_some()
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find(("import-name", 0usize)).value(), Some("app"));
        assert_eq!(
            window.find(("import-group", 0usize)).value(),
            Some("生产/华东")
        );
        assert_eq!(window.find(("import-user", 0usize)).value(), Some("deploy"));
        assert_eq!(window.find(("import-auth", 0usize)).label(), Some("密码"));
        assert_eq!(
            window.find(("import-status", 1usize)).label(),
            Some("已存在")
        );
        assert_eq!(
            window.find(("import-port", 2usize)).label(),
            Some("TELNET 23")
        );
        assert!(
            window
                .find("import-note")
                .label()
                .unwrap()
                .contains("已跳过 1 个")
        );
        assert_eq!(
            window.find("import-counts").label(),
            Some("共 3 台 · 1 台已存在 · 已选 2 台")
        );
    });
    assert_eq!(
        cx.update(|cx| workspace.read(cx).store().read(cx).hosts().len()),
        1
    );
    in_frame(cx, handle, |window, cx| window.click("commit", cx));
    wait_for_dialog_to_close(cx, handle).await;
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(store.hosts().len(), 3);
        let imported = store
            .hosts()
            .iter()
            .find(|host| host.name.as_ref() == "app")
            .unwrap();
        assert_eq!(imported.user.as_ref(), "deploy");
        assert_eq!(imported.notes.as_ref(), "迁移的主机");
        assert_eq!(store.group_path(imported.group.unwrap()), "生产 / 华东");
        let telnet = store
            .hosts()
            .iter()
            .find(|host| host.name.as_ref() == "switch")
            .unwrap();
        assert!(telnet.protocol.is_telnet());
        assert_eq!(telnet.user.as_ref(), "admin");
        assert!(store.credentials().is_empty());
    });
    let reloaded = HostStore::load(HostDatabase::open(&database).unwrap()).unwrap();
    assert_eq!(reloaded.hosts().len(), 3);
    assert_eq!(reloaded.groups().len(), 2);
    assert!(!format!("{:?}", reloaded.hosts()).contains("DO_NOT_IMPORT"));
}

#[gpui_kit::test]
async fn xshell_file_selection_and_cancel_leave_existing_data_untouched(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let valid = directory.path().join("valid.xsh");
    let invalid = directory.path().join("invalid.xsh");
    std::fs::write(
        &valid,
        "[CONNECTION]\nProtocol=SSH\nHost=example.com\nUserName=ops\n",
    )
    .unwrap();
    std::fs::write(&invalid, "[CONNECTION]\nProtocol=FTP\nHost=invalid\n").unwrap();
    let (handle, workspace) = open_workspace_with_store(cx, HostStore::empty());
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-6", cx)
    });
    in_frame(cx, handle, |window, cx| window.click("import-xshell", cx));
    in_frame(cx, handle, |window, cx| {
        window.click("import-choose-file", cx)
    });
    cx.simulate_path_prompt_response(|_| Some(vec![valid, invalid]));
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find(("import-address", 0usize)).is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert!(
            window
                .find("import-note")
                .label()
                .unwrap()
                .contains("已跳过 1 个")
        );
        window.press("escape", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
    assert!(cx.update(|cx| workspace.read(cx).store().read(cx).hosts().is_empty()));
    in_frame(cx, handle, |window, cx| window.click("import-xshell", cx));
    in_frame(cx, handle, |window, cx| {
        window.click("import-choose-file", cx)
    });
    cx.simulate_path_prompt_response(|_| None);
    cx.run_until_parked();
    in_frame(cx, handle, |window, _| {
        assert!(!enabled(&window.find("commit")));
        assert!(enabled(&window.find("import-choose-file")));
    });
}

fn mobaxterm_bookmark(user: &str) -> String {
    let mut fields = vec![""; 36];
    fields[0] = "0";
    fields[1] = "server.example.test";
    fields[2] = "2222";
    fields[3] = user;
    fields[7] = "DO_NOT_EXECUTE";
    fields[14] = "C:\\not-imported-private-key.ppk";
    fields[19] = "0";
    format!(
        "#109#{}#MobaFont%12#0#导入备注__DIEZE__1#-1",
        fields.join("%")
    )
}

#[gpui_kit::test]
async fn mobaxterm_combined_import_selects_hosts_and_colors_independently(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("combined.mobaconf");
    let colors = include_str!("../fixtures/mobaxterm/colors.ini");
    std::fs::write(
        &path,
        format!(
            "{colors}\n[Bookmarks]\nSubRep=生产\\华东\napp={}\n",
            mobaxterm_bookmark("deploy")
        ),
    )
    .unwrap();
    let (handle, workspace) = open_workspace_with_store(cx, HostStore::empty());
    let settings = cx.update(|cx| workspace.read(cx).settings().clone());
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-0", cx)
    });
    for import_hosts in [false, true] {
        in_frame(cx, handle, |window, cx| {
            window.click("import-terminal-schemes", cx)
        });
        cx.simulate_path_prompt_response(|_| Some(vec![path.clone()]));
        cx.wait_for(handle.into(), Duration::from_secs(4), |window, _| {
            window.try_find("moba-import-schemes").is_some()
        })
        .await;
        assert!(settings.read_with(cx, |store, _| {
            store.settings().terminal_theme.imported.is_empty()
        }));
        assert!(cx.update(|cx| workspace.read(cx).store().read(cx).hosts().is_empty()));
        in_frame(cx, handle, |window, cx| {
            assert_eq!(window.find("moba-import-hosts").checked(), Some(false));
            assert_eq!(window.find("moba-import-schemes").checked(), Some(true));
            if import_hosts {
                window.click("moba-import-hosts", cx);
                window.click("moba-import-schemes", cx);
            } else {
                window.press("escape", cx);
            }
        });
        if !import_hosts {
            wait_for_dialog_to_close(cx, handle).await;
            continue;
        }
        in_frame(cx, handle, |window, cx| window.click("commit", cx));
        wait_for_dialog_to_close(cx, handle).await;
    }
    assert_eq!(
        cx.update(|cx| workspace.read(cx).store().read(cx).hosts().len()),
        1
    );
    assert!(settings.read_with(cx, |store, _| {
        store.settings().terminal_theme.imported.is_empty()
    }));
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-6", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("import-mobaxterm", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("import-choose-file", cx)
    });
    cx.simulate_path_prompt_response(|_| Some(vec![path.clone()]));
    cx.wait_for(handle.into(), Duration::from_secs(4), |window, _| {
        window.try_find("moba-import-schemes").is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("moba-import-hosts").checked(), Some(true));
        assert_eq!(window.find("moba-import-schemes").checked(), Some(false));
        assert!(!enabled(&window.find("commit")));
        window.click("moba-import-schemes", cx);
    });
    in_frame(cx, handle, |window, cx| window.click("commit", cx));
    wait_for_dialog_to_close(cx, handle).await;
    assert_eq!(
        settings.read_with(cx, |store, _| store
            .settings()
            .terminal_theme
            .imported
            .len()),
        3
    );
    assert_eq!(
        cx.update(|cx| workspace.read(cx).store().read(cx).hosts().len()),
        1
    );
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-0", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("import-terminal-schemes", cx)
    });
    std::fs::write(
        &path,
        format!(
            "{}\n[Bookmarks]\nother={}\n",
            colors.replace("ForegroundColour=236,236,236", "ForegroundColour=1,2,3"),
            mobaxterm_bookmark("another-user")
        ),
    )
    .unwrap();
    cx.simulate_path_prompt_response(|_| Some(vec![path.clone()]));
    cx.wait_for(handle.into(), Duration::from_secs(4), |window, _| {
        window.try_find("moba-import-schemes").is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| window.click("commit", cx));
    wait_for_dialog_to_close(cx, handle).await;
    assert_eq!(
        cx.update(|cx| workspace.read(cx).store().read(cx).hosts().len()),
        1
    );
    assert_eq!(
        settings.read_with(cx, |store, _| store
            .settings()
            .terminal_theme
            .imported
            .len()),
        3
    );
    let theme = settings.read_with(cx, |store, _| {
        store
            .settings()
            .terminal_theme
            .imported
            .iter()
            .find(|theme| theme.key() == "mobaxterm-current")
            .unwrap()
            .clone()
    });
    assert_eq!(
        theme.foreground(),
        gpui_kit::Hsla::from(gpui_kit::rgb(0x010203))
    );
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-6", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("import-mobaxterm", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("import-choose-file", cx)
    });
    cx.simulate_path_prompt_response(|_| Some(vec![path.clone()]));
    cx.wait_for(handle.into(), Duration::from_secs(4), |window, _| {
        window.try_find("moba-import-schemes").is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("moba-import-hosts").checked(), Some(true));
        window.click("moba-import-schemes", cx);
    });
    std::fs::remove_file(&path).unwrap();
    in_frame(cx, handle, |window, cx| window.click("commit", cx));
    wait_for_dialog_to_close(cx, handle).await;
    assert_eq!(
        cx.update(|cx| workspace.read(cx).store().read(cx).hosts().len()),
        2
    );
    assert_eq!(
        settings.read_with(cx, |store, _| store
            .settings()
            .terminal_theme
            .imported
            .len()),
        3
    );
    cx.update(|cx| {
        settings.update(cx, |store, cx| {
            store.update(
                |settings| {
                    settings.appearance = Appearance::Dark;
                },
                cx,
            )
        })
    });
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-0", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("terminal-theme-mobaxterm-current", cx)
    });
    cx.update(|cx| {
        let current = shellrs::terminal::TerminalColors::current(cx);
        assert_eq!(current.key(), "mobaxterm-current");
        assert_eq!(
            current.foreground(),
            gpui_kit::Hsla::from(gpui_kit::rgb(0x010203))
        );
    });
}

#[gpui_kit::test]
async fn mobaxterm_malformed_colors_warn_but_do_not_prevent_host_only_import(
    cx: &mut TestAppContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("broken.ini");
    std::fs::write(
        &path,
        format!(
            "[Colors]\nForegroundColour=broken\n[Bookmarks]\napp={}\n",
            mobaxterm_bookmark("deploy")
        ),
    )
    .unwrap();
    let (handle, workspace) = open_workspace_with_store(cx, HostStore::empty());
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-6", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("import-mobaxterm", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("import-choose-file", cx)
    });
    cx.simulate_path_prompt_response(|_| Some(vec![path]));
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window.try_find("moba-scheme-error").is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("moba-import-schemes").is_none());
        assert!(enabled(&window.find("commit")));
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
    assert_eq!(
        cx.update(|cx| workspace.read(cx).store().read(cx).hosts().len()),
        1
    );
}

#[gpui_kit::test]
async fn mobaxterm_import_from_sync_previews_and_persists_hosts_without_passwords_or_keys(
    cx: &mut TestAppContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("MobaXterm.mobaconf");
    let text = format!(
        "[Passwords]\nssh=DO_NOT_IMPORT_PASSWORD\n[Bookmarks_1]\nSubRep=生产\\华东\nImgNum=41\napp={}\nexisting={}\nunsupported={}\n",
        mobaxterm_bookmark("deploy"),
        mobaxterm_bookmark("root"),
        mobaxterm_bookmark("ops").replacen("#109#0%", "#91#4%", 1)
    );
    let mut utf16 = vec![0xff, 0xfe];
    utf16.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(&path, utf16).unwrap();
    let database = directory.path().join("hosts.db");
    let mut store = HostStore::load(HostDatabase::open(&database).unwrap()).unwrap();
    let existing = store.insert_unnotified(HostDraft::new(
        "已有主机",
        "server.example.test",
        2222,
        "root",
        AuthKind::Password,
        None,
    ));
    HostDatabase::open(&database)
        .unwrap()
        .insert_host(store.host(existing).unwrap())
        .unwrap();
    let (handle, workspace) = open_workspace_with_store(cx, store);
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-6", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("import-mobaxterm", cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert!(!enabled(&window.find("commit")));
        assert!(window.try_find("import-choose-folder").is_none());
        window.click("import-choose-file", cx);
    });
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|_| Some(vec![path]));
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window.try_find(("import-address", 0usize)).is_some()
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find(("import-name", 0usize)).value(), Some("app"));
        assert_eq!(
            window.find(("import-group", 0usize)).value(),
            Some("生产/华东")
        );
        assert_eq!(window.find(("import-user", 0usize)).value(), Some("deploy"));
        assert_eq!(
            window.find(("import-port", 0usize)).label(),
            Some("SSH 2222")
        );
        assert_eq!(window.find(("import-auth", 0usize)).label(), Some("密码"));
        assert_eq!(
            window.find(("import-status", 1usize)).label(),
            Some("已存在")
        );
        assert_eq!(
            window.find("import-counts").label(),
            Some("共 2 台 · 1 台已存在 · 已选 1 台")
        );
        assert!(
            window
                .find("import-note")
                .label()
                .unwrap()
                .contains("已跳过 1 个")
        );
    });
    assert_eq!(
        cx.update(|cx| workspace.read(cx).store().read(cx).hosts().len()),
        1
    );
    in_frame(cx, handle, |window, cx| window.click("commit", cx));
    wait_for_dialog_to_close(cx, handle).await;
    let reloaded = HostStore::load(HostDatabase::open(&database).unwrap()).unwrap();
    assert_eq!(reloaded.hosts().len(), 2);
    assert_eq!(reloaded.groups().len(), 2);
    let imported = reloaded
        .hosts()
        .iter()
        .find(|host| host.name.as_ref() == "app")
        .unwrap();
    assert_eq!(imported.notes.as_ref(), "导入备注#1");
    assert_eq!(imported.user.as_ref(), "deploy");
    assert_eq!(reloaded.group_path(imported.group.unwrap()), "生产 / 华东");
    assert!(reloaded.credentials().is_empty());
    let saved = format!("{:?}", reloaded.hosts());
    assert!(
        !saved.contains("DO_NOT_IMPORT_PASSWORD")
            && !saved.contains("DO_NOT_EXECUTE")
            && !saved.contains("private-key.ppk")
    );
}

#[gpui_kit::test]
async fn mobaxterm_interface_only_export_explains_missing_hosts_and_cancel_changes_nothing(
    cx: &mut TestAppContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("interface.mobaconf");
    std::fs::write(&path, format!("[Misc]\nLastSession=recent|{}\n[ImportExportSpecials]\nExportedSettings=MobaXterm interface settings\n", mobaxterm_bookmark("root"))).unwrap();
    let (handle, workspace) = open_workspace_with_store(cx, HostStore::empty());
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-6", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("import-mobaxterm", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("import-choose-file", cx)
    });
    cx.simulate_path_prompt_response(|_| Some(vec![path]));
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window.try_find("import-empty").is_some_and(|empty| {
            empty
                .label()
                .is_some_and(|label| label.contains("仅导出“界面设置”"))
        })
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert!(!enabled(&window.find("commit")));
        assert!(window.try_find(("import-address", 0usize)).is_none());
        window.press("escape", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
    assert!(cx.update(|cx| workspace.read(cx).store().read(cx).hosts().is_empty()));
}

#[gpui_kit::test]
async fn hosts_from_ssh_config_are_added_with_their_groups_and_keys(cx: &mut TestAppContext) {
    let home = ssh_home(&[
        (
            "config",
            "\
Host web-01
  HostName 10.0.1.1
  User deploy
  IdentityFile ~/.ssh/deploy_key
Host saved
  HostName WEB-02.EXAMPLE.COM
Host db-01
  HostName 10.0.1.3
  ProxyJump saved
Host *
  User ops
",
        ),
        ("deploy_key", "not really a key"),
    ]);
    let mut store = HostStore::empty();
    store.insert_unnotified(HostDraft::new(
        "web-02",
        "web-02.example.com",
        22,
        "root",
        AuthKind::Password,
        None,
    ));
    let (handle, workspace) = open_workspace_with_store(cx, store);
    let signals = report(cx, handle, &workspace);
    open_import(
        cx,
        handle,
        &workspace,
        &ssh_dir(&home),
        Box::new(ImportSshConfig),
    )
    .await;

    in_frame(cx, handle, |window, _| {
        // The host already saved is listed, greyed out of the import.
        assert_eq!(window.find(("import-status", 0usize)).label(), Some("新增"));
        assert_eq!(
            window.find(("import-status", 1usize)).label(),
            Some("已存在")
        );
        assert_eq!(
            window.find(("import-select", 1usize)).checked(),
            Some(false)
        );
        assert_eq!(window.find(("import-select", 0usize)).checked(), Some(true));
        // A key file of its own: a key credential, made by the import.
        assert_eq!(
            window.find(("import-auth", 0usize)).label(),
            Some("deploy_key（新建凭据）")
        );
        assert_eq!(window.find(("import-auth", 2usize)).label(), Some("无密码"));
        assert_eq!(window.find(("import-user", 2usize)).value(), Some("ops"));
        assert_eq!(
            window.find("import-counts").label(),
            Some("共 3 台 · 1 台已存在 · 已选 2 台")
        );
        assert!(enabled(&window.find("commit")));
    });
    type_into(cx, handle, ("import-group", 0), "生产/华东");
    type_into(cx, handle, ("import-group", 2), "生产/华东");
    in_frame(cx, handle, |window, cx| window.click("commit", cx));
    wait_for_dialog_to_close(cx, handle).await;

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let hosts: Vec<(String, String, String, String)> = store
            .hosts()
            .iter()
            .map(|host| {
                (
                    host.name.to_string(),
                    host.address.to_string(),
                    host.user.to_string(),
                    host.group
                        .map(|group| store.group_path(group))
                        .unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            hosts,
            [
                (
                    "web-02".into(),
                    "web-02.example.com".into(),
                    "root".into(),
                    String::new()
                ),
                (
                    "web-01".into(),
                    "10.0.1.1".into(),
                    "deploy".into(),
                    "生产 / 华东".into()
                ),
                (
                    "db-01".into(),
                    "10.0.1.3".into(),
                    "ops".into(),
                    "生产 / 华东".into()
                ),
            ]
        );
        assert_eq!(store.groups().len(), 2);
        let [credential] = store.credentials() else {
            panic!("one key credential: {:?}", store.credentials());
        };
        assert_eq!(credential.kind, CredentialKind::Key);
        assert_eq!(credential.user.as_ref(), "deploy");
        assert_eq!(
            credential.key_path.as_deref().map(Path::new),
            Some(ssh_dir(&home).join("deploy_key").as_path())
        );
        assert_eq!(store.hosts()[1].credential, Some(credential.id));
        assert_eq!(store.hosts()[2].auth, AuthKind::NoPassword);
        assert_eq!(store.hosts()[2].route, Route::Direct);
    });
    cx.update_window(handle.into(), |_, window, cx| {
        assert_eq!(window.notifications(cx).len(), 1);
    })
    .unwrap();
    let counted: Vec<Counter> = signals
        .try_iter()
        .filter_map(|signal| match signal {
            Signal::Count(counter) => Some(counter),
            _ => None,
        })
        .collect();
    assert!(counted.contains(&Counter::ImportSshConfig), "{counted:?}");
}

#[gpui_kit::test]
async fn hosts_from_known_hosts_log_in_as_root_and_hashed_ones_are_counted(
    cx: &mut TestAppContext,
) {
    let home = ssh_home(&[(
        "known_hosts",
        "\
|1|c2FsdA==|aGFzaA== ssh-ed25519 AAAA
[10.0.0.7]:2222 ssh-ed25519 AAAA
10.0.0.8 ssh-ed25519 AAAA
10.0.0.8 ssh-rsa BBBB
@cert-authority *.corp ssh-ed25519 CCCC
",
    )]);
    let (handle, workspace) = open_workspace_with_store(cx, HostStore::empty());
    open_import(
        cx,
        handle,
        &workspace,
        &ssh_dir(&home),
        Box::new(ImportKnownHosts),
    )
    .await;

    in_frame(cx, handle, |window, cx| {
        let note = window
            .find("import-note")
            .label()
            .unwrap_or_default()
            .to_string();
        assert!(note.contains("另有 1 条经过哈希的记录"), "{note}");
        assert_eq!(window.find(("import-user", 0usize)).value(), Some("root"));
        assert!(window.try_find(("import-address", 2usize)).is_none());
        window.click(("import-select", 1usize), cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("import-counts").label(),
            Some("共 2 台 · 已选 1 台")
        );
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let [host] = store.hosts() else {
            panic!("one host: {:?}", store.hosts());
        };
        assert_eq!(
            (
                host.name.as_ref(),
                host.address.as_ref(),
                host.port,
                host.user.as_ref()
            ),
            ("10.0.0.7", "10.0.0.7", 2222, "root")
        );
        assert_eq!(host.auth, AuthKind::NoPassword);
        assert!(store.groups().is_empty());
    });
}

#[gpui_kit::test]
async fn a_long_auth_label_stays_in_its_column(cx: &mut TestAppContext) {
    let home = ssh_home(&[
        (
            "config",
            "\
Host web-01
  HostName 10.0.1.1
  IdentityFile ~/.ssh/id_ed25519_a_rather_long_name_for_work
",
        ),
        ("id_ed25519_a_rather_long_name_for_work", "not really a key"),
    ]);
    let (handle, workspace) = open_workspace_with_store(cx, HostStore::empty());
    open_import(
        cx,
        handle,
        &workspace,
        &ssh_dir(&home),
        Box::new(ImportSshConfig),
    )
    .await;

    in_frame(cx, handle, |window, _| {
        let auth = window.find(("import-auth", 0usize));
        assert_eq!(
            auth.label(),
            Some("id_ed25519_a_rather_long_name_for_work（新建凭据）")
        );
        let status = window.find(("import-status", 0usize)).bounds();
        assert!(
            auth.bounds().right() <= status.left(),
            "{:?} runs into {status:?}",
            auth.bounds()
        );
    });
}

#[gpui_kit::test]
async fn without_a_file_or_a_selection_nothing_can_be_imported(cx: &mut TestAppContext) {
    let home = ssh_home(&[(
        "known_hosts",
        "10.0.0.1 ssh-ed25519 AAAA\n10.0.0.2 ssh-ed25519 AAAA\n",
    )]);
    let (handle, workspace) = open_workspace_with_store(cx, HostStore::empty());
    open_import(
        cx,
        handle,
        &workspace,
        &ssh_dir(&home),
        Box::new(ImportSshConfig),
    )
    .await;
    in_frame(cx, handle, |window, cx| {
        let missing = window
            .find("import-empty")
            .label()
            .unwrap_or_default()
            .to_string();
        assert!(missing.starts_with("找不到 "), "{missing}");
        assert!(!enabled(&window.find("commit")));
        // Enter commits a form; with nothing to import it does nothing.
        window.press("enter", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("commit").is_some());
        window.press("escape", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;

    open_import(
        cx,
        handle,
        &workspace,
        &ssh_dir(&home),
        Box::new(ImportKnownHosts),
    )
    .await;
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("import-select-all").checked(), Some(true));
        window.click("import-select-all", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find(("import-select", 0usize)).checked(),
            Some(false)
        );
        assert_eq!(
            window.find(("import-select", 1usize)).checked(),
            Some(false)
        );
        assert!(!enabled(&window.find("commit")));
        window.click("import-select-all", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find(("import-select", 1usize)).checked(), Some(true));
        assert!(enabled(&window.find("commit")));
    });
}
