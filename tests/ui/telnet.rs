//! Telnet hosts: the host form's Telnet with its 自动登录, their terminals,
//! and what an SSH host has that they have not.

use crate::support::*;
use shellrs::app::{CopyHostAddress, SendBreak};
use shellrs::host::{
    CredentialDraft, CredentialKind, Protocol, ProtocolKind, TelnetLogin, TelnetSettings,
};

/// A store with one Telnet host, `switch` at 10.0.0.5:23, logging in as
/// admin on its own.
fn telnet_store() -> (HostStore, HostId) {
    let mut store = HostStore::empty();
    let mut draft = HostDraft::telnet("switch", "10.0.0.5", 23, None);
    draft.auth = AuthKind::Password;
    draft.user = "admin".into();
    let id = store.insert_unnotified(draft);
    (store, id)
}

async fn connect(cx: &mut TestAppContext, handle: WindowHandle<Root>, id: HostId) {
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(id)), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window
            .try_find("status-connection")
            .is_some_and(|status| status.label() == Some("已连接 switch"))
    })
    .await;
}

fn replace_text(window: &mut gpui_kit::Window, id: &'static str, text: &str, cx: &mut App) {
    window.click(id, cx);
    window.press("secondary-a", cx);
    window.input(text, cx);
}

#[gpui_kit::test]
async fn a_telnet_host_is_made_with_its_automatic_login(cx: &mut TestAppContext) {
    let keychain = Arc::new(InMemorySecretStore::default());
    let tester = Arc::new(FakeConnectionTester::default());
    let (handle, workspace) = open_workspace_with_tester(
        cx,
        HostStore::empty().with_secrets(keychain.clone()),
        tester.clone(),
    );
    in_frame(cx, handle, |window, cx| window.click("new-host-panel", cx));
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("host-port").value(), Some("22"));
        window.within("host-protocol").click(1usize, cx);
    });
    in_frame(cx, handle, |window, cx| {
        // The port follows the protocol while nobody has changed it.
        assert_eq!(window.find("host-port").value(), Some("23"));
        assert!(window.find("host-address").visible());
        assert!(window.within("host-route").find(0usize).visible());
        // Without 自动登录, nothing of a login.
        for id in [
            "host-auth-source",
            "host-user",
            "host-telnet-auth",
            "host-telnet-user",
            "host-telnet-user-prompt",
        ] {
            assert!(window.try_find(id).is_none(), "{id}");
        }
        window.within("host-protocol").click(0usize, cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("host-port").value(), Some("22"));
        window.within("host-protocol").click(1usize, cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("host-port").value(), Some("23"));
        window.click("host-telnet-auto-login", cx);
    });
    in_frame(cx, handle, |window, cx| {
        // Its own user and password on one row, the prompts on the next.
        let [user, password] =
            ["host-telnet-user", "host-telnet-password"].map(|id| window.find(id).bounds());
        assert_eq!(user.top(), password.top());
        let [user_prompt, password_prompt] =
            ["host-telnet-user-prompt", "host-telnet-password-prompt"]
                .map(|id| window.find(id).bounds());
        assert_eq!(user_prompt.top(), password_prompt.top());
        assert!(user_prompt.top() > user.bottom());
        window.click("host-address", cx);
        window.input("10.0.0.5", cx);
        window.click("host-telnet-user", cx);
        window.input("admin", cx);
        window.click("host-telnet-password", cx);
        window.input("hunter2", cx);
        window.click("host-telnet-user-prompt", cx);
        window.input("账号：", cx);
    });
    // 测试连接 reaches the port the way the terminal would, and logs in
    // nowhere.
    let shown = in_frame(cx, handle, |window, cx| {
        window.click("test-connection", cx);
        window.notifications(cx).len()
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.notifications(cx).len() == shown + 1
    })
    .await;
    assert_eq!(
        tester.telnet_tests(),
        [("10.0.0.5".to_string(), 23, LoginRoute::Direct, None)]
    );
    assert!(tester.requests().is_empty());
    in_frame(cx, handle, |window, cx| window.click("commit", cx));
    wait_for_dialog_to_close(cx, handle).await;
    cx.run_until_parked();
    cx.update(|cx| {
        let host = &workspace.read(cx).store().read(cx).hosts()[0];
        assert_eq!(host.name.as_ref(), "10.0.0.5");
        assert_eq!((host.address.as_ref(), host.port), ("10.0.0.5", 23));
        assert_eq!(
            (host.auth, host.user.as_ref()),
            (AuthKind::Password, "admin")
        );
        assert_eq!(
            host.protocol,
            Protocol::Telnet(TelnetSettings {
                user_prompt: "账号：".into(),
                password_prompt: "".into(),
            })
        );
    });
    assert_eq!(
        keychain
            .get(&SecretRef::password("admin", "10.0.0.5", 23))
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("hunter2")
    );
}

#[gpui_kit::test]
async fn a_telnet_host_logs_in_only_with_password_credentials(cx: &mut TestAppContext) {
    let mut store = HostStore::empty();
    store.insert_credential_unnotified(
        CredentialDraft::new("钥匙", CredentialKind::Key, "root").with_key_path("/tmp/id"),
    );
    let (handle, _) =
        open_workspace_with_tester(cx, store, Arc::new(FakeConnectionTester::default()));
    in_frame(cx, handle, |window, cx| window.click("new-host-panel", cx));
    in_frame(cx, handle, |window, cx| {
        window.within("host-protocol").click(1usize, cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("host-telnet-auto-login", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.within("host-telnet-auth").click(1usize, cx)
    });
    in_frame(cx, handle, |window, _| {
        // A key is no use to Telnet: there is no credential to pick.
        assert_eq!(
            window.find("host-telnet-credential-summary").label(),
            Some("还没有密码凭据，可在侧栏的「凭据」中新建")
        );
        assert!(window.try_find("host-telnet-user").is_none());
    });
}

#[gpui_kit::test]
async fn an_open_telnet_terminal_keeps_its_prompts_and_a_new_one_takes_the_new_ones(
    cx: &mut TestAppContext,
) {
    let (store, id) = telnet_store();
    let telnet = fake_telnet_terminals();
    let (handle, workspace) = open_workspace_with_protocols(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        telnet.clone(),
        fake_serial_terminals(),
    );
    connect(cx, handle, id).await;
    let logins = telnet.logins();
    let auto_login = logins[0].auto_login.clone().unwrap();
    assert_eq!((logins[0].host.as_str(), logins[0].port), ("10.0.0.5", 23));
    assert_eq!(auto_login.user, "admin");

    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(EditHost(id)), cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.within("host-protocol").find(1usize).selected(),
            Some(true)
        );
        assert_eq!(window.find("host-telnet-auto-login").checked(), Some(true));
        assert_eq!(window.find("host-telnet-user").value(), Some("admin"));
        replace_text(window, "host-telnet-password-prompt", "口令：", cx);
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
    cx.run_until_parked();
    assert_eq!(
        telnet.logins().len(),
        1,
        "the open connection stays as it is"
    );
    cx.update(|cx| assert!(workspace.read(cx).has_connection(id, cx)));

    // Reconnecting logs in as the tab first did; a new tab, as now.
    in_frame(cx, handle, |window, cx| {
        window.click(("reconnect", 1_u64), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        telnet.logins().len() == 2
    })
    .await;
    assert_eq!(telnet.logins()[1], telnet.logins()[0]);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(id)), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        telnet.logins().len() == 3
    })
    .await;
    assert_eq!(
        telnet.logins()[2]
            .auto_login
            .clone()
            .unwrap()
            .password_prompt,
        "口令："
    );
}

#[gpui_kit::test]
async fn a_telnet_terminal_offers_linux_tools_but_not_sftp(cx: &mut TestAppContext) {
    let (store, id) = telnet_store();
    let line = Arc::new(FakeTerminalFactory::default());
    let (handle, _) = open_workspace_with_protocols(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedTelnetTerminalTransportProvider::new(line.clone())),
        fake_serial_terminals(),
    );
    connect(cx, handle, id).await;
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find(("sftp", 1_u64)).is_none());
        assert!(window.find(("reconnect", 1_u64)).visible());
        assert_eq!(
            window.find(("terminal-tab-os", 1_u64)).label(),
            Some("Telnet")
        );
        assert!(window.find("tool-snippets").visible());
        for tool in ["tool-history", "tool-files", "tool-tmux"] {
            assert!(window.try_find(tool).is_none(), "{tool}");
        }
        assert!(window.find("tool-processes").visible());
        assert!(window.find("tool-monitor").visible());
    });

    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(SendBreak(RemoteTerminalId(1))), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        line.breaks() == 1
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(CopyHostAddress(id)), cx)
    });
    cx.update(|cx| {
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("10.0.0.5")
        )
    });
}

#[gpui_kit::test]
async fn a_telnet_host_is_connected_to_without_saving_it(cx: &mut TestAppContext) {
    let telnet = fake_telnet_terminals();
    let (handle, workspace) = open_workspace_with_protocols(
        cx,
        HostStore::empty(),
        Arc::new(FakeTerminalFactory::default()),
        telnet.clone(),
        fake_serial_terminals(),
    );
    in_frame(cx, handle, |window, cx| {
        window.click("temporary-connection", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.within("host-protocol").click(1usize, cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("host-address", cx);
        window.input("10.0.0.7", cx);
        window.click("host-telnet-auto-login", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("host-telnet-password", cx);
        window.input("secret", cx);
        window.click("commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        telnet.logins().len() == 1
    })
    .await;
    let login: TelnetLogin = telnet.logins().remove(0);
    let id = cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert!(store.hosts().is_empty());
        assert_eq!(store.temporary_hosts().len(), 1);
        let host = &store.temporary_hosts()[0];
        assert_eq!(host.kind(), ProtocolKind::Telnet);
        host.id
    });
    assert_eq!((login.host.as_str(), login.port), ("10.0.0.7", 23));
    // The password is the temporary host's own, in memory; no user name.
    let auto_login = login.auto_login.unwrap();
    assert_eq!(auto_login.user, "");
    assert_eq!(auto_login.password, Some(SecretRef::temporary(id.0)));
}
