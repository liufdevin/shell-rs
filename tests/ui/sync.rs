//! 同步: setting it up from 设置 and joining it, edits reaching the other
//! computer, what waits for the user, and turning it off. The folder is one
//! `MemoryStore` shared by every computer of a test.

use shellrs::app::{RemoveSyncDevice, ReviewSync, SetUpSync, SyncNow, TurnOffSync};
use shellrs::settings::SyncLocation;
use shellrs::sync::{Attention, EDIT_PAUSE, SyncPhase, SyncService, SyncServices};
use shellrs_sync::store::Op;
use shellrs_sync::{DeviceInfo, KdfParams, MemoryStore, StoreError, SyncStore, layout};
use zeroize::Zeroizing;

use crate::support::*;

const FOLDER: &str = "/Users/me/Dropbox/ShellRS";
const PASSWORD: &str = "correct horse battery";

fn at_folder() -> SyncLocation {
    SyncLocation::Folder {
        path: FOLDER.into(),
    }
}

/// The folder, whatever path is picked, with keys that derive at once.
fn services(folder: &Arc<MemoryStore>, name: &str) -> SyncServices {
    let folder = folder.clone();
    SyncServices {
        open: Arc::new(move |_, _| Ok(folder.clone() as Arc<dyn SyncStore>)),
        kdf: KdfParams {
            memory_kib: 8,
            passes: 1,
            parallelism: 1,
        },
        device: DeviceInfo {
            name: name.into(),
            platform: "macos".into(),
            app_version: "0.1.15".into(),
        },
    }
}

/// The computer with the window: the seeded hosts, a keychain in memory.
struct Desk {
    handle: WindowHandle<Root>,
    workspace: Entity<Workspace>,
    secrets: Arc<InMemorySecretStore>,
    _keys: tempfile::TempDir,
}

fn desk(cx: &mut TestAppContext, folder: &Arc<MemoryStore>) -> Desk {
    desk_with(cx, services(folder, "desk"))
}

fn desk_with(cx: &mut TestAppContext, services: SyncServices) -> Desk {
    let secrets = Arc::new(InMemorySecretStore::default());
    let keys = tempfile::tempdir().unwrap();
    let store = HostStore::seed()
        .with_secrets(secrets.clone())
        .with_key_dir(keys.path());
    let (handle, workspace) = open_workspace_with_store(cx, store);
    cx.update(|cx| {
        let sync = workspace.read(cx).sync().clone();
        sync.update(cx, |sync, cx| sync.set_services(services, cx));
    });
    Desk {
        handle,
        workspace,
        secrets,
        _keys: keys,
    }
}

impl Desk {
    fn sync(&self, cx: &mut TestAppContext) -> Entity<SyncService> {
        cx.update(|cx| self.workspace.read(cx).sync().clone())
    }

    fn store(&self, cx: &mut TestAppContext) -> Entity<HostStore> {
        cx.update(|cx| self.workspace.read(cx).store().clone())
    }

    fn location_setting(&self, cx: &mut TestAppContext) -> Option<SyncLocation> {
        cx.update(|cx| {
            self.workspace
                .read(cx)
                .settings()
                .read(cx)
                .settings()
                .sync
                .location
        })
    }
}

/// Another computer, without a window: its own store, keychain and sync.
struct Laptop {
    store: Entity<HostStore>,
    sync: Entity<SyncService>,
    _keys: tempfile::TempDir,
}

fn laptop(cx: &mut TestAppContext, folder: &Arc<MemoryStore>) -> Laptop {
    let keys = tempfile::tempdir().unwrap();
    let store = HostStore::empty()
        .with_secrets(Arc::new(InMemorySecretStore::default()))
        .with_key_dir(keys.path());
    let store = cx.new(|_| store);
    let services = services(folder, "laptop");
    let sync = cx.new(|cx| {
        let mut sync = SyncService::new(store.clone(), cx);
        sync.set_services(services, cx);
        sync
    });
    Laptop {
        store,
        sync,
        _keys: keys,
    }
}

impl Laptop {
    /// Join the sync in the folder.
    async fn join(&self, cx: &mut TestAppContext) {
        let join = self.sync.update(cx, |sync, cx| {
            sync.join(at_folder(), None, Zeroizing::new(PASSWORD.into()), true, cx)
        });
        join.await.unwrap();
        cx.run_until_parked();
    }

    fn sync_now(&self, cx: &mut TestAppContext) {
        self.sync.update(cx, |sync, cx| sync.sync_now(cx));
        cx.run_until_parked();
    }
}

fn host_names(store: &Entity<HostStore>, cx: &mut TestAppContext) -> Vec<String> {
    store.read_with(cx, |store, _| {
        let mut names: Vec<String> = store.hosts().iter().map(|h| h.name.to_string()).collect();
        names.sort();
        names
    })
}

/// Open 设置 › 云同步.
fn open_sync_settings(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-6", cx)
    });
}

/// Set sync up from 设置 › 云同步 on the desk: the folder picked, the
/// password typed twice.
async fn set_up_on_the_desk(cx: &mut TestAppContext, desk: &Desk) {
    let handle = desk.handle;
    open_sync_settings(cx, handle);
    in_frame(cx, handle, |window, cx| window.click("sync-set-up", cx));
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|_| Some(vec![FOLDER.into()]));
    cx.run_until_parked();
    in_frame(cx, handle, |window, cx| {
        window.click("sync-password", cx);
        window.input(PASSWORD, cx);
        window.click("sync-password-repeat", cx);
        window.input(PASSWORD, cx);
        window.click("sync-commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("sync-commit").is_none()
    })
    .await;
    cx.run_until_parked();
}

#[gpui_kit::test]
async fn sync_is_set_up_on_one_computer_and_joined_on_another(cx: &mut TestAppContext) {
    let folder = Arc::new(MemoryStore::new());
    let desk = desk(cx, &folder);
    set_up_on_the_desk(cx, &desk).await;

    assert_eq!(desk.location_setting(cx), Some(at_folder()));
    assert!(folder.raw(layout::VAULT).is_some());
    // The keys went to the keychain, never to the folder in the clear.
    assert!(desk.secrets.get(&SecretRef::SyncKeyring).unwrap().is_some());
    let phase = desk
        .sync(cx)
        .read_with(cx, |sync, cx| sync.snapshot(cx).phase);
    assert!(matches!(phase, SyncPhase::Synced { .. }), "{phase:?}");
    in_frame(cx, desk.handle, |window, _| {
        let status = window.find("sync-status").label().unwrap().to_string();
        assert!(status.starts_with("上次同步："), "{status}");
        assert_eq!(window.find("sync-folder").label(), Some(FOLDER));
    });

    let laptop = laptop(cx, &folder);
    laptop.join(cx).await;
    let desk_hosts = host_names(&desk.store(cx), cx);
    assert_eq!(desk_hosts.len(), 6);
    assert_eq!(host_names(&laptop.store, cx), desk_hosts);

    // The desk lists the laptop once it has looked again.
    in_frame(cx, desk.handle, |window, cx| {
        window.dispatch_action(Box::new(SyncNow), cx)
    });
    let devices = desk
        .sync(cx)
        .read_with(cx, |sync, cx| sync.snapshot(cx).devices);
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].info.name, "laptop");
    let remove = ElementId::Name(format!("remove-sync-device-{}", devices[0].id).into());
    in_frame(cx, desk.handle, |window, _| {
        assert!(window.try_find(remove).is_some());
    });
}

#[gpui_kit::test]
async fn a_wrong_password_is_said_over_the_dialog(cx: &mut TestAppContext) {
    let folder = Arc::new(MemoryStore::new());
    let laptop = laptop(cx, &folder);
    let create = laptop.sync.update(cx, |sync, cx| {
        sync.create(at_folder(), None, Zeroizing::new(PASSWORD.into()), cx)
    });
    create.await.unwrap();
    cx.run_until_parked();

    let desk = desk(cx, &folder);
    let handle = desk.handle;
    open_sync_settings(cx, handle);
    in_frame(cx, handle, |window, cx| window.click("sync-set-up", cx));
    cx.simulate_path_prompt_response(|_| Some(vec![FOLDER.into()]));
    cx.run_until_parked();
    in_frame(cx, handle, |window, cx| {
        // The folder has sync: one field, to join it.
        assert!(window.try_find("sync-password-repeat").is_none());
        window.click("sync-password", cx);
        window.input("battery horse staple", cx);
        window.click("sync-commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("form-error").is_some()
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("form-error").label(), Some("同步密码不对。"));
        assert!(window.try_find("sync-commit").is_some());
    });
    assert_eq!(desk.location_setting(cx), None);
}

#[gpui_kit::test]
async fn a_new_password_is_at_least_twelve_characters_and_typed_the_same_twice(
    cx: &mut TestAppContext,
) {
    let folder = Arc::new(MemoryStore::new());
    let desk = desk(cx, &folder);
    let handle = desk.handle;
    open_sync_settings(cx, handle);
    in_frame(cx, handle, |window, cx| window.click("sync-set-up", cx));
    cx.simulate_path_prompt_response(|_| Some(vec![FOLDER.into()]));
    cx.run_until_parked();
    in_frame(cx, handle, |window, cx| {
        window.click("sync-password", cx);
        window.input("short", cx);
        window.click("sync-commit", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("form-error").label(),
            Some("同步密码至少要 12 个字符。")
        );
        window.click("sync-generate-password", cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("sync-password-repeat", cx);
        window.input("x", cx);
        window.click("sync-commit", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("form-error").label(),
            Some("两次输入的同步密码不一样。")
        );
    });
    assert!(folder.raw(layout::VAULT).is_none());
}

#[gpui_kit::test]
async fn an_edit_reaches_the_other_computer_after_a_pause(cx: &mut TestAppContext) {
    let folder = Arc::new(MemoryStore::new());
    let desk = desk(cx, &folder);
    set_up_on_the_desk(cx, &desk).await;
    let laptop = laptop(cx, &folder);
    laptop.join(cx).await;

    let store = desk.store(cx);
    store.update(cx, |store, cx| {
        store.insert(
            HostDraft::new("cache-01", "10.0.3.1", 22, "root", AuthKind::Password, None),
            cx,
        );
    });
    cx.run_until_parked();
    laptop.sync_now(cx);
    assert!(!host_names(&laptop.store, cx).contains(&"cache-01".to_string()));

    // Once the user has stopped editing for a while.
    cx.executor().advance_clock(EDIT_PAUSE);
    cx.run_until_parked();
    laptop.sync_now(cx);
    assert!(host_names(&laptop.store, cx).contains(&"cache-01".to_string()));

    // And back: the laptop's edit shows in the desk's host tree.
    laptop.store.update(cx, |store, cx| {
        store.insert(
            HostDraft::new("queue-01", "10.0.4.1", 22, "root", AuthKind::Password, None),
            cx,
        );
    });
    laptop.sync_now(cx);
    in_frame(cx, desk.handle, |window, cx| {
        window.dispatch_action(Box::new(SyncNow), cx)
    });
    assert!(host_names(&store, cx).contains(&"queue-01".to_string()));
}

#[gpui_kit::test]
async fn passwords_sync_with_the_hosts(cx: &mut TestAppContext) {
    let folder = Arc::new(MemoryStore::new());
    let desk = desk(cx, &folder);
    let store = desk.store(cx);
    let secret = SecretRef::password("root", "10.0.1.12", 22);
    store.update(cx, |store, cx| {
        store.save_secret(secret.clone(), Some("pw-web".into()), cx);
    });
    cx.run_until_parked();
    set_up_on_the_desk(cx, &desk).await;

    let laptop = laptop(cx, &folder);
    laptop.join(cx).await;
    let secrets = laptop.store.read_with(cx, |store, _| store.secrets());
    assert_eq!(
        secrets.get(&secret).unwrap().as_deref().map(String::as_str),
        Some("pw-web")
    );
}

#[gpui_kit::test]
async fn many_deletions_wait_for_the_user(cx: &mut TestAppContext) {
    let folder = Arc::new(MemoryStore::new());
    let desk = desk(cx, &folder);
    let store = desk.store(cx);
    store.update(cx, |store, cx| {
        for ix in 0..10 {
            store.insert(
                HostDraft::new(
                    format!("node-{ix:02}"),
                    format!("10.0.9.{ix}"),
                    22,
                    "root",
                    AuthKind::Password,
                    None,
                ),
                cx,
            );
        }
    });
    set_up_on_the_desk(cx, &desk).await;
    let laptop = laptop(cx, &folder);
    laptop.join(cx).await;

    // Every host and group gone at once, as a fault might do it.
    store.update(cx, |store, cx| {
        let groups: Vec<GroupId> = store.groups().iter().map(|group| group.id).collect();
        for group in groups {
            store.remove_group(group, cx);
        }
        let hosts: Vec<HostId> = store.hosts().iter().map(|host| host.id).collect();
        for host in hosts {
            store.remove(host, cx);
        }
    });
    in_frame(cx, desk.handle, |window, cx| {
        window.dispatch_action(Box::new(SyncNow), cx)
    });
    let attention = desk
        .sync(cx)
        .read_with(cx, |sync, cx| sync.snapshot(cx).attention);
    assert!(
        matches!(attention, Some(Attention::MassDelete { count, .. }) if count >= 10),
        "{attention:?}"
    );
    in_frame(cx, desk.handle, |window, cx| {
        assert_eq!(window.notifications(cx).len(), 1);
        assert!(window.try_find("review-sync-notice").is_some());
        assert!(window.within("settings").try_find("review-sync").is_some());
    });
    laptop.sync_now(cx);
    assert_eq!(host_names(&laptop.store, cx).len(), 16);

    // Agreed to, they go everywhere.
    in_frame(cx, desk.handle, |window, cx| {
        window.dispatch_action(Box::new(ReviewSync), cx)
    });
    in_frame(cx, desk.handle, |window, cx| window.click("ok", cx));
    cx.run_until_parked();
    laptop.sync_now(cx);
    assert!(host_names(&laptop.store, cx).is_empty());
}

#[gpui_kit::test]
async fn a_removed_computer_asks_before_it_comes_back(cx: &mut TestAppContext) {
    let folder = Arc::new(MemoryStore::new());
    let desk = desk(cx, &folder);
    set_up_on_the_desk(cx, &desk).await;
    let laptop = laptop(cx, &folder);
    laptop.join(cx).await;
    in_frame(cx, desk.handle, |window, cx| {
        window.dispatch_action(Box::new(SyncNow), cx)
    });

    let device = desk
        .sync(cx)
        .read_with(cx, |sync, cx| sync.snapshot(cx).devices[0].id.clone());
    in_frame(cx, desk.handle, |window, cx| {
        window.dispatch_action(Box::new(RemoveSyncDevice(device.clone())), cx)
    });
    in_frame(cx, desk.handle, |window, cx| window.click("ok", cx));
    cx.run_until_parked();
    assert!(folder.raw(&layout::device_file(&device)).is_none());
    let devices = desk
        .sync(cx)
        .read_with(cx, |sync, cx| sync.snapshot(cx).devices.len());
    assert_eq!(devices, 0);

    laptop.sync_now(cx);
    let attention = laptop
        .sync
        .read_with(cx, |sync, cx| sync.snapshot(cx).attention);
    assert_eq!(attention, Some(Attention::Removed));
    assert!(folder.raw(&layout::device_file(&device)).is_none());

    laptop.sync.update(cx, |sync, cx| sync.accept_removed(cx));
    cx.run_until_parked();
    assert!(folder.raw(&layout::device_file(&device)).is_some());
}

#[gpui_kit::test]
async fn quitting_waits_for_the_last_edit_to_sync(cx: &mut TestAppContext) {
    let folder = Arc::new(MemoryStore::new());
    let desk = desk(cx, &folder);
    set_up_on_the_desk(cx, &desk).await;
    let sync = desk.sync(cx);
    // Nothing new: nothing to wait for.
    assert!(
        sync.update(cx, |sync, cx| sync.sync_before_quit(cx))
            .is_none()
    );

    desk.store(cx).update(cx, |store, cx| {
        store.insert(
            HostDraft::new("cache-01", "10.0.3.1", 22, "root", AuthKind::Password, None),
            cx,
        );
    });
    let wait = sync.update(cx, |sync, cx| sync.sync_before_quit(cx));
    assert!(wait.is_some());
    cx.run_until_parked();
    let laptop = laptop(cx, &folder);
    laptop.join(cx).await;
    assert!(host_names(&laptop.store, cx).contains(&"cache-01".to_string()));
}

#[gpui_kit::test]
async fn turning_sync_off_keeps_the_data_and_can_take_the_file_too(cx: &mut TestAppContext) {
    let folder = Arc::new(MemoryStore::new());
    let desk = desk(cx, &folder);
    set_up_on_the_desk(cx, &desk).await;
    let files = || {
        folder
            .paths()
            .into_iter()
            .filter(|path| path.starts_with(layout::DEVICES))
            .count()
    };
    assert_eq!(files(), 1);

    in_frame(cx, desk.handle, |window, cx| {
        window.dispatch_action(Box::new(TurnOffSync), cx)
    });
    in_frame(cx, desk.handle, |window, cx| {
        window.click("sync-delete-own-file", cx);
        window.click("sync-turn-off", cx);
    });
    cx.wait_for(desk.handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("sync-turn-off").is_none()
    })
    .await;
    cx.run_until_parked();

    assert_eq!(desk.location_setting(cx), None);
    assert_eq!(files(), 0);
    assert!(desk.secrets.get(&SecretRef::SyncKeyring).unwrap().is_none());
    assert_eq!(host_names(&desk.store(cx), cx).len(), 6);
    assert_eq!(
        desk.store(cx).read_with(cx, |store, _| store.sync_state()),
        None
    );
    in_frame(cx, desk.handle, |window, _| {
        assert!(window.try_find("sync-set-up").is_some());
    });
}

#[gpui_kit::test]
async fn without_a_keychain_sync_cannot_be_turned_on(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let folder = Arc::new(MemoryStore::new());
    cx.update(|cx| {
        let sync = workspace.read(cx).sync().clone();
        sync.update(cx, |sync, cx| {
            sync.set_services(services(&folder, "desk"), cx)
        });
    });
    open_sync_settings(cx, handle);
    // The button does nothing, and the command says why.
    in_frame(cx, handle, |window, cx| window.click("sync-set-up", cx));
    assert!(!cx.did_prompt_for_paths());
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(SetUpSync), cx)
    });
    cx.simulate_path_prompt_response(|_| Some(vec![FOLDER.into()]));
    cx.run_until_parked();
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("sync-password").is_none());
        assert_eq!(window.notifications(cx).len(), 1);
    });
    assert!(folder.raw(layout::VAULT).is_none());
}

#[gpui_kit::test]
async fn a_new_password_and_key_are_asked_for_on_the_other_computer(cx: &mut TestAppContext) {
    let folder = Arc::new(MemoryStore::new());
    let desk = desk(cx, &folder);
    set_up_on_the_desk(cx, &desk).await;
    let laptop = laptop(cx, &folder);
    laptop.join(cx).await;

    let new_password = "staple battery horse";
    in_frame(cx, desk.handle, |window, cx| {
        window.click("change-sync-password", cx)
    });
    in_frame(cx, desk.handle, |window, cx| {
        window.click("sync-password", cx);
        window.input(new_password, cx);
        window.click("sync-password-repeat", cx);
        window.input(new_password, cx);
        window.click("sync-commit", cx);
    });
    cx.wait_for(desk.handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("sync-commit").is_none()
    })
    .await;
    cx.run_until_parked();
    // The data key changed with the password.
    let vault: serde_json::Value =
        serde_json::from_slice(&folder.raw(layout::VAULT).unwrap()).unwrap();
    assert_eq!(vault["current"], 2);

    desk.store(cx).update(cx, |store, cx| {
        store.insert(
            HostDraft::new("cache-01", "10.0.3.1", 22, "root", AuthKind::Password, None),
            cx,
        );
    });
    in_frame(cx, desk.handle, |window, cx| {
        window.dispatch_action(Box::new(SyncNow), cx)
    });
    laptop.sync_now(cx);
    let attention = laptop
        .sync
        .read_with(cx, |sync, cx| sync.snapshot(cx).attention);
    assert_eq!(attention, Some(Attention::NeedsPassword));
    assert!(!host_names(&laptop.store, cx).contains(&"cache-01".to_string()));

    // With the new password it goes on where it was.
    let join = laptop.sync.update(cx, |sync, cx| {
        sync.join(
            at_folder(),
            None,
            Zeroizing::new(new_password.into()),
            false,
            cx,
        )
    });
    join.await.unwrap();
    cx.run_until_parked();
    assert_eq!(
        laptop
            .sync
            .read_with(cx, |sync, cx| sync.snapshot(cx).attention),
        None
    );
    assert!(host_names(&laptop.store, cx).contains(&"cache-01".to_string()));
}

const DAV: &str = "https://dav.example/dav/ShellRS";
const DAV_USER: &str = "me@example.com";
const S3_ENDPOINT: &str = "https://abc123.r2.cloudflarestorage.com";
const S3_KEY: &str = "AKIAEXAMPLE";

/// A WebDAV server in front of the folder, taking one login: the logins it
/// was opened with are kept, for the tests to see none went astray.
struct Server {
    folder: Arc<MemoryStore>,
    password: Mutex<String>,
    logins: Mutex<Vec<Option<String>>>,
}

impl Server {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            folder: Arc::new(MemoryStore::new()),
            password: Mutex::new("app-password".into()),
            logins: Mutex::new(Vec::new()),
        })
    }

    fn password(&self) -> String {
        self.password.lock().unwrap().clone()
    }

    fn services(self: &Arc<Self>, name: &str) -> SyncServices {
        let server = self.clone();
        SyncServices {
            open: Arc::new(move |location, password| {
                let known = match location {
                    SyncLocation::WebDav { url, username } => url == DAV && username == DAV_USER,
                    SyncLocation::S3 {
                        endpoint,
                        bucket,
                        access_key,
                        ..
                    } => endpoint == S3_ENDPOINT && bucket == "shellrs" && access_key == S3_KEY,
                    SyncLocation::Folder { .. } => panic!("a folder: {location:?}"),
                };
                server
                    .logins
                    .lock()
                    .unwrap()
                    .push(password.map(str::to_owned));
                if known && password == Some(server.password().as_str()) {
                    Ok(server.folder.clone() as Arc<dyn SyncStore>)
                } else {
                    Err(StoreError::Unauthorized)
                }
            }),
            ..services(&self.folder, name)
        }
    }
}

fn at_dav() -> SyncLocation {
    SyncLocation::WebDav {
        url: DAV.into(),
        username: DAV_USER.into(),
    }
}

/// Fill the WebDAV dialog with `password`.
fn fill_webdav(window: &mut gpui_kit::Window, password: &str, cx: &mut App) {
    window.click("webdav-url", cx);
    window.input(DAV, cx);
    window.click("webdav-username", cx);
    window.input(DAV_USER, cx);
    window.click("webdav-password", cx);
    window.input(password, cx);
}

/// Set sync up on the server from 设置 › 云同步 › 连接 WebDAV….
async fn set_up_on_the_server(cx: &mut TestAppContext, desk: &Desk, server: &Server) {
    let handle = desk.handle;
    open_sync_settings(cx, handle);
    in_frame(cx, handle, |window, cx| {
        window.click("sync-set-up-webdav", cx)
    });
    let password = server.password();
    in_frame(cx, handle, |window, cx| {
        fill_webdav(window, &password, cx);
        window.click("remote-commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        assert!(window.try_find("sync-password").is_none());
        assert!(window.try_find("sync-password-repeat").is_none());
        window.try_find("remote-commit").is_none()
    })
    .await;
    cx.run_until_parked();
}

#[gpui_kit::test]
async fn sync_is_set_up_on_a_webdav_server(cx: &mut TestAppContext) {
    let server = Server::new();
    let desk = desk_with(cx, server.services("desk"));
    let handle = desk.handle;
    open_sync_settings(cx, handle);
    in_frame(cx, handle, |window, cx| {
        window.click("sync-set-up-webdav", cx)
    });

    // A login the server refuses is said over the dialog, and keeps it.
    in_frame(cx, handle, |window, cx| {
        fill_webdav(window, "wrong", cx);
        window.click("remote-commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("form-error").is_some()
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("remote-commit").is_some());
        assert!(window.try_find("sync-password").is_none());
    });

    // 测试连接 writes, reads and deletes a file there, and keeps nothing.
    in_frame(cx, handle, |window, cx| {
        window.click("webdav-password", cx);
        for _ in "wrong".chars() {
            window.press("backspace", cx);
        }
        window.input(&server.password(), cx);
        window.click("remote-test", cx);
    });
    cx.run_until_parked();
    let probes: Vec<Op> = server
        .folder
        .operations()
        .into_iter()
        .filter(|op| !matches!(op, Op::List(_)))
        .collect();
    assert!(
        matches!(probes.as_slice(), [Op::Put(put), Op::Get(got), Op::Delete(deleted)]
            if put.starts_with("shellrs-sync/probe-") && got == put && deleted == put),
        "{probes:?}"
    );
    let paths = server.folder.paths();
    assert!(paths.is_empty(), "{paths:?}");
    assert!(desk.secrets.get(&SecretRef::SyncLogin).unwrap().is_none());

    in_frame(cx, handle, |window, cx| window.click("remote-commit", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        assert!(window.try_find("sync-password").is_none());
        assert!(window.try_find("sync-password-repeat").is_none());
        window.try_find("remote-commit").is_none()
    })
    .await;
    cx.run_until_parked();

    assert_eq!(desk.location_setting(cx), Some(at_dav()));
    assert!(server.folder.raw(layout::VAULT).is_some());
    assert!(shellrs_sync::unlock_vault(&*server.folder, &server.password()).is_ok());
    let kept = desk.secrets.get(&SecretRef::SyncLogin).unwrap();
    assert_eq!(kept.as_deref().map(String::as_str), Some("app-password"));
    // Rounds sign in with what the keychain keeps.
    let logins = server.logins.lock().unwrap().clone();
    assert_eq!(logins.last(), Some(&Some("app-password".to_string())));
    let phase = desk
        .sync(cx)
        .read_with(cx, |sync, cx| sync.snapshot(cx).phase);
    assert!(matches!(phase, SyncPhase::Synced { .. }), "{phase:?}");
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("sync-webdav-url").label(), Some(DAV));
        assert!(window.try_find("change-sync-login").is_some());
    });
}

#[gpui_kit::test]
async fn webdav_joins_with_its_login_password(cx: &mut TestAppContext) {
    let server = Server::new();
    let first = desk_with(cx, server.services("first"));
    set_up_on_the_server(cx, &first, &server).await;
    let vault = server.folder.raw(layout::VAULT).unwrap();

    let second = desk_with(cx, server.services("second"));
    set_up_on_the_server(cx, &second, &server).await;

    assert_eq!(second.location_setting(cx), Some(at_dav()));
    assert_eq!(server.folder.raw(layout::VAULT).unwrap(), vault);
    assert_eq!(
        second
            .secrets
            .get(&SecretRef::SyncLogin)
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some(server.password().as_str())
    );
    let snapshot = second.sync(cx).read_with(cx, |sync, cx| sync.snapshot(cx));
    assert!(matches!(snapshot.phase, SyncPhase::Synced { .. }));
    assert_eq!(snapshot.attention, None);
}

#[gpui_kit::test]
async fn webdav_can_join_an_older_sync_with_a_separate_password(cx: &mut TestAppContext) {
    let server = Server::new();
    let first = desk_with(cx, server.services("first"));
    let create = first.sync(cx).update(cx, |sync, cx| {
        sync.create(
            at_dav(),
            Some(Zeroizing::new(server.password())),
            Zeroizing::new(PASSWORD.into()),
            cx,
        )
    });
    create.await.unwrap();
    cx.run_until_parked();
    let vault = server.folder.raw(layout::VAULT).unwrap();

    let second = desk_with(cx, server.services("second"));
    let handle = second.handle;
    open_sync_settings(cx, handle);
    in_frame(cx, handle, |window, cx| {
        window.click("sync-set-up-webdav", cx)
    });
    in_frame(cx, handle, |window, cx| {
        fill_webdav(window, &server.password(), cx);
        window.click("remote-commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("sync-password").is_some()
    })
    .await;
    assert_eq!(second.location_setting(cx), None);
    assert!(second.secrets.get(&SecretRef::SyncLogin).unwrap().is_none());
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("sync-password-repeat").is_none());
        window.click("sync-password", cx);
        window.input(PASSWORD, cx);
        window.click("sync-commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("sync-commit").is_none()
    })
    .await;
    cx.run_until_parked();
    assert_eq!(second.location_setting(cx), Some(at_dav()));
    assert_eq!(server.folder.raw(layout::VAULT).unwrap(), vault);
    let snapshot = second.sync(cx).read_with(cx, |sync, cx| sync.snapshot(cx));
    assert!(matches!(snapshot.phase, SyncPhase::Synced { .. }));
}

#[gpui_kit::test]
async fn webdav_setup_keeps_the_form_after_a_write_failure(cx: &mut TestAppContext) {
    let server = Server::new();
    let desk = desk_with(cx, server.services("desk"));
    let handle = desk.handle;
    open_sync_settings(cx, handle);
    in_frame(cx, handle, |window, cx| {
        window.click("sync-set-up-webdav", cx)
    });
    server
        .folder
        .fail_next(|op| matches!(op, Op::Put(_)), StoreError::Timeout);
    in_frame(cx, handle, |window, cx| {
        fill_webdav(window, &server.password(), cx);
        window.click("remote-commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("form-error").is_some()
    })
    .await;
    assert_eq!(desk.location_setting(cx), None);
    assert!(desk.secrets.get(&SecretRef::SyncLogin).unwrap().is_none());
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("sync-password").is_none());
        assert_eq!(window.find("webdav-url").value(), Some(DAV));
        window.click("remote-commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        assert!(window.try_find("sync-password-repeat").is_none());
        window.try_find("remote-commit").is_none()
    })
    .await;
    cx.run_until_parked();
    assert_eq!(desk.location_setting(cx), Some(at_dav()));
}

#[gpui_kit::test]
async fn a_refused_webdav_login_is_asked_for_again(cx: &mut TestAppContext) {
    let server = Server::new();
    let desk = desk_with(cx, server.services("desk"));
    set_up_on_the_server(cx, &desk, &server).await;
    let handle = desk.handle;

    // The app password was revoked on the server's website.
    *server.password.lock().unwrap() = "new-app-password".into();
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(SyncNow), cx)
    });
    let attention = desk
        .sync(cx)
        .read_with(cx, |sync, cx| sync.snapshot(cx).attention);
    assert_eq!(attention, Some(Attention::LoginRefused));
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.notifications(cx).len(), 1);
        assert_eq!(
            window.find("sync-status").label(),
            Some("同步的位置拒绝了这台电脑的登录信息，重新登录后继续同步。")
        );
        window.dispatch_action(Box::new(ReviewSync), cx);
    });
    in_frame(cx, handle, |window, cx| {
        // The same server: only the login changes.
        assert!(window.try_find("webdav-url").is_some());
        window.click("webdav-password", cx);
        window.input("new-app-password", cx);
        window.click("remote-commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("remote-commit").is_none()
    })
    .await;
    cx.run_until_parked();
    let snapshot = desk.sync(cx).read_with(cx, |sync, cx| sync.snapshot(cx));
    assert_eq!(snapshot.attention, None);
    assert!(matches!(snapshot.phase, SyncPhase::Synced { .. }));
    let kept = desk.secrets.get(&SecretRef::SyncLogin).unwrap();
    assert_eq!(
        kept.as_deref().map(String::as_str),
        Some("new-app-password")
    );

    // Turned off, sync forgets the password with its keys.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(TurnOffSync), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("sync-turn-off", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("sync-turn-off").is_none()
    })
    .await;
    cx.run_until_parked();
    assert_eq!(desk.location_setting(cx), None);
    assert!(desk.secrets.get(&SecretRef::SyncLogin).unwrap().is_none());
}

#[gpui_kit::test]
async fn sync_is_set_up_in_an_s3_bucket(cx: &mut TestAppContext) {
    let server = Server::new();
    let desk = desk_with(cx, server.services("desk"));
    let handle = desk.handle;
    open_sync_settings(cx, handle);
    in_frame(cx, handle, |window, cx| window.click("sync-set-up-s3", cx));
    // A bucket is all it takes, and a key for it.
    in_frame(cx, handle, |window, cx| window.click("remote-commit", cx));
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("form-error").is_some());
        window.click("s3-endpoint", cx);
        window.input(S3_ENDPOINT, cx);
        window.click("s3-region", cx);
        window.input("auto", cx);
        window.click("s3-bucket", cx);
        window.input("shellrs", cx);
        window.click("s3-access-key", cx);
        window.input(S3_KEY, cx);
        window.click("s3-secret-key", cx);
        window.input(&server.password(), cx);
        window.click("remote-commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("sync-password-repeat").is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click("sync-password", cx);
        window.input(PASSWORD, cx);
        window.click("sync-password-repeat", cx);
        window.input(PASSWORD, cx);
        window.click("sync-commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("sync-commit").is_none()
    })
    .await;
    cx.run_until_parked();

    assert_eq!(
        desk.location_setting(cx),
        Some(SyncLocation::S3 {
            endpoint: S3_ENDPOINT.into(),
            region: "auto".into(),
            bucket: "shellrs".into(),
            prefix: "ShellRS".into(),
            path_style: false,
            access_key: S3_KEY.into(),
        })
    );
    assert!(server.folder.raw(layout::VAULT).is_some());
    let kept = desk.secrets.get(&SecretRef::SyncLogin).unwrap();
    assert_eq!(kept.as_deref().map(String::as_str), Some("app-password"));
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("sync-s3-location").label(),
            Some(format!("shellrs/ShellRS · {S3_ENDPOINT}").as_str())
        );
        assert!(window.try_find("change-sync-login").is_some());
    });
}
