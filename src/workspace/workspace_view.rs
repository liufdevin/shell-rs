use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui_kit::component::{
    ActiveTheme as _, Root, Sizable as _, TitleBar, WindowExt as _,
    button::ButtonVariant,
    dialog::DialogButtonProps,
    dock::{DockArea, DockEvent, DockLayout, DockPlacement, PanelId, TabGroup, panel_handle},
    form::{Field, Form},
    h_flex,
    input::{Input, InputContentType, InputState},
    notification::Notification,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::editors::describe_files;
use super::front::follow_focus;
use crate::analytics::{Analytics, AnalyticsServices, Counter};
use crate::app::ExplorerDispatch as _;
use crate::app::{
    CenterTab, ClearTerminal, CloseActiveTab, CloseEditor, CloseExplorer, CloseLocalTerminal,
    CloseSettings, CloseTabs, CloseTerminal, CollapseAllGroups, ConnectGroup, ConnectHost,
    CopyAgentSkill, CopyHostAddress, CopyHostId, CopyTerminal, CopyTerminalFindResults,
    DeleteGroup, DeleteHost, DisconnectTerminal, DismissTerminalFind, DuplicateHost,
    DuplicateTerminal, EditHost, ExpandAllGroups, ExplorerAction, ExplorerCommand,
    ExplorerShortcut, FindInTerminal, FindNextInTerminal, FindPreviousInTerminal, FocusSearch,
    InstallAgentSkill, InstallCliCommand, MaximizeWindow, MoveHostNode, NewChildGroup,
    NewCommandTerminal, NewGroup, NewHost, NewHostInGroup, NewLocalTerminal,
    NewTemporaryConnection, OpenExplorer, OpenLocalTerminalOptions, OpenSettings, PasteTerminal,
    QuickConnect, ReconnectTerminal, RefreshCliIntegration, RemoveAgentSkill, RemoveCliCommand,
    RenameExplorer, RenameGroup, RenameTerminal, RestartLocalTerminal, RestoreWindow,
    ReturnDetachedTab, SelectAllTerminal, SendBreak, SendSerialHex, SetFileSizeFormat,
    ShowOrHideApp, StartLocalTerminal, StartTerminalRecording, StopTerminalRecording,
    ToggleHiddenFiles, ToggleHostDetails, ToggleHostPanel, ToggleTerminalFindOption, ToggleTheme,
    ToolKind, ZoomIn, ZoomOut, ZoomReset, ZoomTerminalIn, ZoomTerminalOut, ZoomTerminalReset,
};
use crate::app::{
    ClearTerminalLog, DeleteTerminalLog, SetSerialBackspace, ToggleSerialReceiveHex,
    ToggleSerialSendHex,
};
use crate::cli::{CliIntegration, CliServer, IntegrationPaths, SshCliBackend};
use crate::connection::{
    ConnectionPrompt, ConnectionPromptField, ConnectionPromptKind, ConnectionPromptReply,
    ConnectionSecret, SharedConnectionTester,
};
use crate::credential::CredentialPanel;
use crate::editor::{EditorId, EditorPanel};
use crate::explorer::{
    ExplorerId, ExplorerPanel, ExplorerPanelEvent, PreviewKind, confirm_close_transfer,
};
use crate::forward::{
    ForwardManager, ForwardManagerEvent, ForwardPanel, SharedForwardTransportProvider,
    SshForwardTransportProvider,
};
use crate::host::{
    ConnectionState, Dependents, ForwardId, GroupId, Host, HostConnection, HostId, HostLogin,
    HostNode, HostPanel, HostStore, HostStoreEvent, SavedTerminal, SavedTerminalId,
    confirm_delete_group, confirm_delete_host, open_group_dialog, open_host_dialog,
    open_quick_connect_dialog, open_temporary_connection_dialog,
};
use crate::i18n::t;
use crate::settings::{
    Appearance, DEFAULT_INTERFACE_FONT_SIZE, SettingsPanel, SettingsPanelEvent, SettingsStore,
    SettingsStoreEvent, WindowSettings,
};
use crate::sftp::{
    SftpRoute, SharedLocalDirectoryProvider, SharedSftpTransportProvider, SshSftpTransportProvider,
    SystemLocalDirectoryProvider, TransferDirection,
};
use crate::shared::{commit_footer, confirm_danger, open_rename_tab_dialog};
use crate::ssh::{
    authentication_fields, authentication_instructions, authentication_title,
    unknown_host_description,
};
use crate::sync::{SyncEvent, SyncService};
use crate::terminal::{
    DEFAULT_FONT_SIZE, HostTerminalTransports, LocalPtyTransportProvider, LocalTerminalId,
    LocalTerminalKind, LocalTerminalPanel, LocalTerminalPanelEvent, RemoteTerminalId, SavedName,
    SearchDirection, SharedLocalTerminalTransportProvider, SharedRemoteTerminalTransportProvider,
    SharedSerialTerminalTransportProvider, SharedTelnetTerminalTransportProvider,
    SharedTerminalTransportFactory, TerminalCopy, TerminalPanel, TerminalPanelEvent, TerminalView,
};
use crate::update::{UpdateServices, Updater, UpdaterEvent};

use super::{
    compose::COMPOSE_HEIGHT,
    compose_pane::ComposePane,
    dock_skin::WorkspaceDockSkin,
    recent_hosts::RecentHosts,
    sidebar::{SIDEBAR_WIDTH, Sidebar},
    status_bar::WorkspaceStatus,
    title_bar::{render_sidebar_toggle, render_title_bar},
    tool_sidebar::{ToolSidebar, render_tool_switch},
    tools::{TOOL_SIDEBAR_WIDTH, set_right_dock_open},
    window_state::{WindowState, window_placement},
};

const DOCK_ID: &str = "shellrs-dock";
const DOCK_VERSION: usize = 1;
/// How far the zoom keys move 界面字号 a press, in pixels.
const INTERFACE_ZOOM_STEP: f32 = 2.;
/// Paint order of the notification layer. gpui-base draws a dialog, backdrop
/// and all, deferred at priority `10 + its layer` (the dock skin's start page
/// at 0); anything lower lands under the backdrop, dimmed, and a click on it
/// hits the backdrop. Just below popups (`POPUP_PRIORITY`) keeps a
/// notification above any stack of dialogs without covering an open menu.
const NOTIFICATION_PRIORITY: usize = gpui_kit::base::POPUP_PRIORITY - 1;
/// Show a notification from code that runs while the window is still being
/// built: loading what is on disk, starting the services.
///
/// Notifications belong to the window's `Root`, which is only in place once
/// the closure that builds the window has returned; pushing one before that
/// panics. Deferred, it arrives as soon as the window is there.
pub fn notify_once_open(notification: Notification, window: &mut Window, cx: &mut App) {
    window.defer(cx, move |window, cx| {
        window.push_notification(notification, cx)
    });
}

/// Window options for the main workspace window: where it was last, as much
/// of it as 设置 › 应用 › 窗口 remembers.
pub fn window_options(
    saved: Option<&WindowState>,
    settings: WindowSettings,
    cx: &mut App,
) -> WindowOptions {
    let (window_bounds, display_id) = window_placement(saved, settings, cx);
    WindowOptions {
        window_bounds: Some(window_bounds),
        display_id,
        // Window geometry is a platform boundary; `px` is the API's unit.
        window_min_size: Some(gpui_kit::Size {
            width: px(960.),
            height: px(600.),
        }),
        app_id: Some(crate::app::DESKTOP_APP_ID.into()),
        ..TitleBar::window_options()
    }
}

/// Who a connection question belongs to, and so who gets the answer. The
/// number tells one connection attempt of the owner from the next: an answer
/// to a question of an attempt that is over is dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PromptOwner {
    Terminal(RemoteTerminalId),
    Sftp(ExplorerId, u64),
    Forward(ForwardId, u64),
}

/// The main window content: title bar above the dock, status bar below.
///
/// Owns the host store, the dock and the registry of open per-host
/// panels, and handles every application action.
pub struct Workspace {
    pub(super) primary_window: AnyWindowHandle,
    pub(super) detached_windows: Vec<super::detached::DetachedWindow>,
    pub(super) dragged_tab: Option<(WindowId, CenterTab)>,
    pub(super) detaching_tabs: Vec<CenterTab>,
    pub(super) broadcast: super::terminal_groups::TerminalBroadcast,
    /// 撰写窗格, in the bottom dock while it shows.
    pub(super) compose: Entity<ComposePane>,
    /// How tall 撰写窗格 was last: it opens again so.
    pub(super) compose_height: Pixels,
    pub(super) store: Entity<HostStore>,
    pub(super) dock_area: Entity<DockArea>,
    pub(super) skin: Rc<WorkspaceDockSkin>,
    /// The left dock's panel: the host tree or the forward list.
    pub(super) sidebar: Entity<Sidebar>,
    pub(super) remote: Entity<crate::remote::Service>,
    remote_panel: Entity<crate::remote::RemotePanel>,
    remote_hosts: HashMap<HostId, (String, String, String, String)>,
    /// The right dock's panel: the tools for the host of the tab in front.
    pub(super) tools: Entity<ToolSidebar>,
    pub(super) host_panel: Entity<HostPanel>,
    /// The port forwards that are running, each on a connection of its own.
    pub(super) forwards: Entity<ForwardManager>,
    /// The start page the dock skin shows while the center has no tab.
    recent: Entity<RecentHosts>,
    pub(super) terminals: HashMap<RemoteTerminalId, Entity<TerminalPanel>>,
    /// SFTP tabs; a host can have several, like terminals. The right
    /// sidebar's 文件 are here too, so commands, questions and editors reach
    /// them the same way.
    pub(super) explorers: HashMap<ExplorerId, Entity<ExplorerPanel>>,
    /// Each remote terminal's 文件, once shown: on the terminal's own
    /// connection, and gone with the terminal.
    pub(super) file_sidebars: HashMap<RemoteTerminalId, ExplorerId>,
    /// 目录跟随: what each terminal's current connection was found to be.
    pub(super) directory_follow: HashMap<RemoteTerminalId, super::follow::FollowState>,
    pub(super) local_terminals: HashMap<LocalTerminalId, Entity<LocalTerminalPanel>>,
    pub(super) lan_local_factories: HashMap<LocalTerminalId, SharedTerminalTransportFactory>,
    pub(super) lan_serial_transports: HashMap<HostId, HostTerminalTransports>,
    cmder: Entity<crate::terminal::Cmder>,
    pub(super) local_launches: HashMap<LocalTerminalId, crate::terminal::LocalLaunch>,
    /// The local terminals kept in 项目, by the tab each is open in.
    pub(super) saved_terminals: HashMap<LocalTerminalId, SavedTerminalId>,
    /// The saved terminal's tab in front last, for the next launch to put
    /// in front again.
    pub(super) front_saved_terminal: Option<LocalTerminalId>,
    /// The settings tab, while it is open. There is only ever one.
    settings_tab: Option<Entity<SettingsPanel>>,
    /// What the settings tab edits; the workspace applies it to the window.
    settings: Entity<SettingsStore>,
    /// Where the `shelllf-cli` command and the agent skills go, and whether
    /// they are there. No paths in UI tests, so nothing reaches the home
    /// directory unless a test hands it a temporary one.
    cli_integration: Entity<CliIntegration>,
    /// Answers the `shelllf-cli` command. Production only: UI tests never
    /// listen on the real socket.
    pub(super) cli_server: Option<CliServer>,
    pub(super) locked_links: Vec<crate::cli::OpenLink>,
    /// Looks for, downloads and installs newer versions. Without services
    /// in UI tests, so it never reaches the network unless a test hands it
    /// fakes.
    pub(super) updater: Entity<Updater>,
    /// 同步. Without services in UI tests, so it never reaches a folder
    /// unless a test hands it a store.
    pub(super) sync: Entity<SyncService>,
    lan: Entity<crate::lan::LanService>,
    /// 匿名使用统计. Production only, like the CLI server: UI tests hand in
    /// one that only records.
    pub(super) analytics: Option<super::analytics::Reporting>,
    local_terminal_provider: SharedLocalTerminalTransportProvider,
    administrator_launcher:
        Option<Arc<dyn super::local_terminal_window::AdministratorTerminalLauncher>>,
    administrator_launch_pending: bool,
    host_terminals: HostTerminalTransports,
    sftp_provider: SharedSftpTransportProvider,
    pub(super) adb_detector: Arc<dyn crate::adb::AdbDetector>,
    pub(super) android_manager: Option<Entity<crate::adb::DeviceManager>>,
    pub(super) android_manager_remote: bool,
    pub(super) android_tools: Arc<dyn crate::adb::AndroidTools>,
    pub(super) local_directory_provider: SharedLocalDirectoryProvider,
    /// Backs the host dialog's 「测试连接」.
    connection_tester: SharedConnectionTester,
    next_remote_terminal_id: u64,
    next_local_terminal_id: u64,
    next_explorer_id: u64,
    /// The center tab in front: shown most recently, or, with the center
    /// split, the one the focus went into last (see `front.rs`).
    /// `CloseActiveTab` closes it. Changed through `set_active_tab`, which
    /// brings the right sidebar in line.
    pub(super) active_tab: Option<CenterTab>,
    /// The terminal the right sidebar goes with while it shows: the last
    /// one in front, SSH or local. Another tab in front of the other half
    /// of a split leaves it be.
    pub(super) tool_target_choice: Option<super::tools::ToolTarget>,
    pub(super) log_split: Option<super::tools::LogSplit>,
    /// The right sidebar's tool for the terminals of a host deleted while
    /// they are open: the store no longer has the host to keep it on.
    pub(super) deleted_host_tools: HashMap<HostId, ToolKind>,
    pub(super) prompt_queue: VecDeque<(PromptOwner, HostId, ConnectionPrompt)>,
    pub(super) active_prompt: Option<(PromptOwner, HostId, u64)>,
    /// Dispatch target for the title bar and start page: actions sent to it
    /// reach the workspace handlers whatever is focused.
    pub(super) focus_handle: FocusHandle,
    /// Editor tabs, one per file.
    pub(super) editors: HashMap<EditorId, Entity<EditorPanel>>,
    pub(super) next_editor_id: u64,
    /// When each terminal last notified, of each kind, keyed by
    /// `notices::notice_tag` and the kind.
    pub(super) notice_times: HashMap<String, std::time::Instant>,
    /// The pending write of the window's size and place, see
    /// `window_state.rs`.
    pub(super) window_state_save: Option<Task<()>>,
    /// Where 导入 reads `~/.ssh/config` and `known_hosts`. Production only:
    /// UI tests hand in a temporary one, so nothing reads the home
    /// directory.
    pub(super) ssh_dir: Option<PathBuf>,
    pub(super) _subscriptions: Vec<Subscription>,
}

impl Workspace {
    /// `store` is built by `main` from the database on disk, and by the UI
    /// tests from `HostStore::seed`; `settings` likewise from the settings
    /// file, or kept in memory.
    pub fn new(
        store: Entity<HostStore>,
        settings: Entity<SettingsStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_cli_server(store, settings, None, window, cx)
    }

    /// Take over the activation listener reserved by the startup password gate.
    pub fn new_with_cli_server(
        store: Entity<HostStore>,
        settings: Entity<SettingsStore>,
        server: Option<CliServer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let remote_backend = Arc::new(crate::remote::SystemBackend::new(
            crate::app::data_dir().join("remote"),
        ));
        let connector = crate::ssh::SshConnector::new(
            crate::app::data_dir().join("known_hosts"),
            crate::secrets::KeychainSource::shared(store.read(cx).secrets()),
        )
        .with_endpoint_resolver(remote_backend.clone());
        let remote = Arc::new(crate::ssh::SshTerminalTransportProvider::with_connector(
            connector.clone(),
        ));
        let tester = Arc::new(crate::ssh::SshConnectionTester::new(connector.clone()));
        let sftp: SharedSftpTransportProvider = Arc::new(SshSftpTransportProvider::new(
            connector.clone(),
            crate::app::data_dir().join("upload-resume"),
            crate::app::data_dir().join("download-resume"),
        ));
        let forward = Arc::new(SshForwardTransportProvider::new(connector.clone()));
        let mut this = Self::new_with_services(
            store,
            settings,
            remote,
            Arc::new(crate::telnet::TelnetTransportProvider::new(
                connector.clone(),
            )),
            Arc::new(crate::serial::SerialTransportProvider),
            Arc::new(LocalPtyTransportProvider),
            sftp.clone(),
            Arc::new(SystemLocalDirectoryProvider),
            tester,
            forward,
            window,
            cx,
        );
        this.ssh_dir = dirs::home_dir().map(|home| home.join(".ssh"));
        this.administrator_launcher = Some(Arc::new(
            super::local_terminal_window::SystemAdministratorTerminalLauncher,
        ));
        this.adb_detector = Arc::new(crate::adb::SystemAdbDetector);
        this.tools.update(cx, |tools, cx| {
            tools.set_monitor_connector(
                Arc::new(crate::monitor::SystemMonitorConnector::new(
                    this.adb_detector.clone(),
                    Some(connector.clone()),
                )),
                cx,
            )
        });
        this.cli_integration.update(cx, |integration, cx| {
            integration.set_paths(IntegrationPaths::system(), cx);
            integration.update_outdated(cx);
        });
        let secrets = this.store.read(cx).secrets();
        let lan_connector = connector.clone();
        let backend = Arc::new(SshCliBackend::new(connector, sftp, secrets));
        let server = match server {
            Some(server) => {
                server.set_backend(backend);
                Ok(server)
            }
            None => CliServer::start(crate::app::cli_socket_path(), backend),
        };
        match server {
            Ok(server) => {
                this.cli_server = Some(server);
                this.serve_cli(window, cx);
                this.serve_cli_runs(window, cx);
            }
            Err(error) => notify_once_open(
                Notification::error(error.to_string()).title(t!("workspace.cli.start_failed")),
                window,
                cx,
            ),
        }
        this.sync_cli_server(cx);
        let lan_store = this.store.clone();
        this.lan.update(cx, |lan, cx| {
            lan.attach(
                crate::app::data_dir().join("lan"),
                lan_connector,
                lan_store,
                crate::app_lock::is_locked(cx),
                cx,
            )
        });
        this.remember_window(crate::app::window_state_path(), window, cx);
        this.remote
            .update(cx, |service, cx| service.set_backend(remote_backend, cx));
        let bundle = cx.app_path().ok();
        this.updater.update(cx, |updater, cx| {
            updater.set_services(UpdateServices::system(bundle), cx);
            updater.start(cx);
        });
        this.sync.update(cx, |sync, cx| {
            sync.set_services(crate::sync::system_services(), cx)
        });
        if let Some(services) = AnalyticsServices::system() {
            let enabled = this.settings.read(cx).settings().analytics.enabled;
            this.set_analytics(Analytics::start(services, enabled), window, cx);
            this.report_started(cx);
            this.count_automatic_forwards(cx);
        }
        this
    }

    /// Tell the CLI server what it may use: whether 启用外部 CLI is on, and
    /// the hosts as they are now.
    fn sync_cli_server(&self, cx: &App) {
        self.lan.read(cx).set_locked(crate::app_lock::is_locked(cx));
        self.lan.read(cx).set_hosts(self.store.read(cx));
        if let Some(server) = &self.cli_server {
            server.set_lan(self.lan.read(cx).core());
            let ready =
                crate::app_lock::AppLock::global(cx).is_none_or(|lock| lock.read(cx).is_ready());
            server.set_enabled(
                ready
                    && self
                        .settings
                        .read(cx)
                        .settings()
                        .is_external_cli_allowed(crate::app_lock::is_locked(cx)),
            );
            server.set_hosts(self.store.read(cx));
        }
    }

    /// Inject every filesystem and transport service; tests require neither a server nor a keychain.
    // One parameter per injected service, which is the point of this
    // constructor; bundling them would only move the list elsewhere.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_services(
        store: Entity<HostStore>,
        settings: Entity<SettingsStore>,
        remote_terminal_provider: SharedRemoteTerminalTransportProvider,
        telnet_terminal_provider: SharedTelnetTerminalTransportProvider,
        serial_terminal_provider: SharedSerialTerminalTransportProvider,
        local_terminal_provider: SharedLocalTerminalTransportProvider,
        sftp_provider: SharedSftpTransportProvider,
        local_directory_provider: SharedLocalDirectoryProvider,
        connection_tester: SharedConnectionTester,
        forward_provider: SharedForwardTransportProvider,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let host_terminals = HostTerminalTransports {
            ssh: remote_terminal_provider,
            telnet: telnet_terminal_provider,
            serial: serial_terminal_provider,
        };
        let recent = cx.new(|cx| RecentHosts::new(store.clone(), focus_handle.clone(), cx));
        let compose =
            cx.new(|cx| ComposePane::new(store.clone(), focus_handle.clone(), window, cx));
        let (dock_area, skin) = WorkspaceDockSkin::dock_area(
            DOCK_ID,
            Some(DOCK_VERSION),
            recent.clone().into(),
            focus_handle.clone(),
            window,
            cx,
        );
        let host_panel = cx.new(|cx| HostPanel::new(store.clone(), window, cx));
        let forwards = cx.new(|cx| ForwardManager::new(store.clone(), forward_provider, cx));
        let update_settings = settings.read(cx).settings().update.clone();
        let updater = cx.new(|cx| {
            let mut updater = Updater::new();
            updater.set_automatic(update_settings.automatic, cx);
            updater.set_channel(update_settings.channel, cx);
            updater
        });
        let sync_location = settings.read(cx).settings().sync.location.clone();
        let sync = cx.new(|cx| {
            let mut sync = SyncService::new(store.clone(), cx);
            sync.set_location(sync_location, cx);
            sync
        });
        let lan = cx.new(crate::lan::LanService::new);
        let forward_panel = cx.new(|cx| {
            ForwardPanel::new(
                store.clone(),
                forwards.clone(),
                focus_handle.clone(),
                window,
                cx,
            )
        });
        let credential_panel =
            cx.new(|cx| CredentialPanel::new(store.clone(), focus_handle.clone(), window, cx));
        let remote = cx.new(crate::remote::Service::new);
        remote.update(cx, |remote, cx| {
            remote.set_secrets(store.read(cx).secrets(), cx)
        });
        let remote_panel = cx.new(|cx| {
            crate::remote::RemotePanel::new(
                remote.clone(),
                settings.clone(),
                focus_handle.clone(),
                window,
                cx,
            )
        });
        let sidebar = cx.new(|_| {
            Sidebar::new(
                host_panel.clone(),
                forward_panel,
                credential_panel,
                remote_panel.clone(),
            )
        });
        let tools = cx.new(|cx| {
            ToolSidebar::new(
                store.clone(),
                settings.clone(),
                focus_handle.clone(),
                window,
                cx,
            )
        });
        // Start with focus in the host panel so window-level actions have a
        // dispatch path. The workspace's own handle is never focused: the
        // dialog layer is its child, and a focused ancestor would keep the
        // dialog's focus trap from taking focus.
        let panel_focus = host_panel.read(cx).focus_handle(cx);
        window.focus(&panel_focus, cx);
        // Before the first frame, so a dark theme never starts out light.
        crate::settings::apply(settings.read(cx).settings(), window, cx);
        let cmder = cx.new(|cx| {
            crate::terminal::Cmder::new(
                settings.read(cx).settings().local_terminal.cmder_directory,
                cx,
            )
        });

        let mut sidebar_width = dock_area.read(cx).dock_size(DockPlacement::Left);
        let mut subscriptions = vec![
            cx.subscribe_in(&remote, window, |this, _, event, window, cx| {
                if let crate::remote::Event::DeviceAddressesChanged(server, devices) = event {
                    let hosts: std::collections::HashSet<_> = this
                        .remote_hosts
                        .iter()
                        .filter(|(_, (owner, _, device, _))| {
                            owner == server && devices.contains(device)
                        })
                        .map(|(host, _)| *host)
                        .collect();
                    this.disconnect_remote_hosts(Some(&hosts), window, cx);
                }
                if let crate::remote::Event::SshReady(resolved) = event {
                    this.open_resolved_remote(resolved, window, cx);
                }
                let invalidated: std::collections::HashSet<_> = this
                    .remote_hosts
                    .iter()
                    .filter(|(_, (server, network, device, _))| {
                        this.remote.read(cx).connection().is_none_or(|active| {
                            active.server != *server
                                || active.network != *network
                                || !active
                                    .devices
                                    .iter()
                                    .any(|target| target.device_id == *device)
                        })
                    })
                    .map(|(host, _)| *host)
                    .collect();
                if matches!(
                    event,
                    crate::remote::Event::NetworkChanging | crate::remote::Event::NetworkChanged
                ) {
                    this.disconnect_remote_hosts(None, window, cx);
                } else if matches!(event, crate::remote::Event::Updated) && !invalidated.is_empty()
                {
                    this.disconnect_remote_hosts(Some(&invalidated), window, cx);
                }
            }),
            cx.observe(&lan, |this, _, cx| this.sync_cli_server(cx)),
            cx.subscribe_in(
                &lan,
                window,
                |this, service, event: &crate::lan::LanEvent, window, cx| match event {
                    crate::lan::LanEvent::Tick => {
                        crate::lan::show_pair_request(service, &this.focus_handle, window, cx)
                    }
                    crate::lan::LanEvent::PairSucceeded { name, incoming } => {
                        let message = if *incoming {
                            t!("lan.pair_success.incoming", name = name.clone())
                        } else {
                            t!("lan.pair_success.outgoing", name = name.clone())
                        };
                        window.push_notification(
                            Notification::success(message).title(t!("lan.pair_success.title")),
                            cx,
                        );
                    }
                },
            ),
            // What the saved terminals show, for the next launch to draw: the
            // workspace goes with its window, as ShellRS quits (and on
            // Windows and Linux as the window closes, before it quits).
            cx.on_release(|this, cx| this.keep_on_close(cx)),
            // Keep the sidebars and compose pane usable while resizing.
            cx.observe_in(&dock_area, window, move |this, area, window, cx| {
                this.hold_sidebar_width(window, cx);
                this.hold_tool_sidebar_width(window, cx);
                this.hold_compose_height(window, cx);
                // Keep every entry point, including search and the list
                // switches that reopen the sidebar, for the next launch.
                let collapsed = !area.read(cx).is_dock_open(DockPlacement::Left);
                if this.settings.read(cx).settings().sidebar_collapsed != collapsed {
                    this.settings.update(cx, |store, cx| {
                        store.update(|settings| settings.sidebar_collapsed = collapsed, cx);
                    });
                }
                // The title-bar toggle follows the left dock's current width.
                let width = area.read(cx).dock_size(DockPlacement::Left);
                if sidebar_width != width {
                    sidebar_width = width;
                    cx.notify();
                }
            }),
            cx.observe_in(&store, window, |this, _, window, cx| {
                this.sync_cli_server(cx);
                // The system a host runs decides which tools it is offered.
                this.sync_tool_sidebar(window, cx);
                cx.notify()
            }),
            cx.subscribe_in(
                &store,
                window,
                |_, _, event: &HostStoreEvent, window, cx| match event {
                    HostStoreEvent::PersistFailed(message) => {
                        window.push_notification(Notification::error(message.clone()), cx);
                    }
                    // The forward manager restarts the rule itself.
                    HostStoreEvent::ForwardSettingsChanged(_) => {}
                },
            ),
            cx.observe(&store, |this, _, cx| this.on_saved_terminals_changed(cx)),
            cx.subscribe_in(
                &forwards,
                window,
                |this, _, event: &ForwardManagerEvent, window, cx| {
                    this.on_forward_event(event, window, cx)
                },
            ),
            cx.observe_in(&settings, window, |this, settings, window, cx| {
                crate::settings::apply(settings.read(cx).settings(), window, cx);
                let directory = settings.read(cx).settings().local_terminal.cmder_directory;
                this.cmder
                    .update(cx, |cmder, cx| cmder.set_directory(directory, cx));
                if !settings.read(cx).settings().local_terminal.keep_output {
                    this.store.read(cx).forget_kept_outputs();
                }
                this.sync_cli_server(cx);
                this.sync_updater(cx);
                this.follow_sync_settings(cx);
                this.sync_analytics(window, cx);
                if !settings
                    .read(cx)
                    .settings()
                    .sidebar_tools
                    .shows(crate::app::ToolKind::Kubernetes)
                {
                    this.tools
                        .update(cx, |tools, cx| tools.disable_kubernetes(cx));
                }
                this.sync_tool_sidebar(window, cx);
            }),
            cx.subscribe_in(
                &updater,
                window,
                |this, _, event: &UpdaterEvent, window, cx| {
                    this.on_updater_event(event, window, cx)
                },
            ),
            cx.subscribe_in(&sync, window, |this, _, event: &SyncEvent, window, cx| {
                this.on_sync_event(event, window, cx)
            }),
            // Coming back to ShellRS, the other computers' edits come too.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.sync.update(cx, |sync, cx| sync.window_activated(cx));
                }
            }),
            cx.subscribe_in(
                &settings,
                window,
                |_, _, event: &SettingsStoreEvent, window, cx| match event {
                    SettingsStoreEvent::PersistFailed(message) => {
                        window.push_notification(Notification::error(message.clone()), cx);
                    }
                },
            ),
            // 跟随系统 follows the system appearance as it changes.
            cx.observe_window_appearance(window, |this, window, cx| {
                crate::settings::apply(this.settings.read(cx).settings(), window, cx);
            }),
            cx.subscribe_in(
                &dock_area,
                window,
                |this, _, event: &DockEvent, window, cx| {
                    if matches!(event, DockEvent::LayoutChanged) {
                        this.save_sidebar_width(cx);
                        this.on_layout_changed(window, cx);
                    }
                },
            ),
            // Zoom and sidebar toggles only notify. Keep the permanent
            // broadcast control on the leading visible tab bar.
            cx.observe(&dock_area, |this, _, cx| this.sync_broadcast(cx)),
        ];

        // A fixture's hosts start with a terminal tab each.
        let connected: Vec<HostId> = store.read(cx).starting_terminals().to_vec();
        // A click on a terminal's system notification shows that terminal.
        let this = cx.weak_entity();
        let handle = window.window_handle();
        cx.on_system_notification_response(move |response, cx| {
            _ = handle.update(cx, |_, window, cx| {
                _ = this.update(cx, |this, cx| {
                    this.on_notice_clicked(&response.tag, window, cx)
                });
            });
        });
        // ⌘Q (and on Windows and Linux the window's close button) asks first
        // when an editor has changes not saved yet.
        let this = cx.weak_entity();
        crate::app::set_quit_guard(
            window.window_handle(),
            move |window, cx| {
                this.update(cx, |this, cx| this.ask_before_quit(window, cx))
                    .unwrap_or(false)
            },
            cx,
        );
        let mut terminals = HashMap::new();
        let mut center = DockLayout::tabs();
        let mut next_remote_terminal_id = 1;
        let (restore, restore_front) = super::projects::terminals_to_restore(store.read(cx));
        for id in connected {
            let terminal_id = RemoteTerminalId(next_remote_terminal_id);
            next_remote_terminal_id += 1;
            let (panel, subscription) = new_terminal_panel(
                &store,
                host_terminals.clone(),
                terminal_id,
                id,
                None,
                window,
                cx,
            );
            center = center.panel_view(panel_handle(panel.clone()), cx);
            subscriptions.push(subscription);
            let focus = panel.read(cx).focus_handle(cx);
            subscriptions.push(follow_focus(
                CenterTab::Terminal(terminal_id),
                &focus,
                window,
                cx,
            ));
            terminals.insert(terminal_id, panel);
        }
        // The saved terminals the last launch left open, after a fixture's
        // terminals, with the one in front then in front again.
        let mut local_terminals = HashMap::new();
        let mut saved_terminals = HashMap::new();
        let mut next_local_terminal_id = 1;
        let launch_settings = settings.read(cx).settings();
        for saved in restore {
            let id = LocalTerminalId(next_local_terminal_id);
            next_local_terminal_id += 1;
            let replay = super::projects::kept_replay(store.read(cx), &launch_settings, saved.id);
            let factory = local_terminal_provider.factory_for(&super::projects::local_launch(
                &launch_settings,
                Some(&saved),
                replay,
            ));
            let (panel, subscription) = new_local_terminal_panel(
                id,
                factory,
                Some(super::projects::saved_name(&saved)),
                window,
                cx,
            );
            center = center.panel_view(panel_handle(panel.clone()), cx);
            subscriptions.push(subscription);
            let focus = panel.read(cx).focus_handle(cx);
            subscriptions.push(follow_focus(
                CenterTab::LocalTerminal(id),
                &focus,
                window,
                cx,
            ));
            local_terminals.insert(id, panel);
            saved_terminals.insert(id, saved.id);
        }
        let front_saved_terminal = restore_front.map(|index| LocalTerminalId(index as u64 + 1));
        if let Some(index) = restore_front {
            center = center.active_index(terminals.len() + index);
        }
        let center_empty = terminals.is_empty() && local_terminals.is_empty();
        let minimum_sidebar_width = rems(10.).to_pixels(window.rem_size());
        let maximum_sidebar_width = (window.viewport_size().width
            - rems(12.).to_pixels(window.rem_size()))
        .max(minimum_sidebar_width);
        let restored_sidebar_width = launch_settings
            .sidebar_width
            .map(px)
            .unwrap_or(SIDEBAR_WIDTH)
            .clamp(minimum_sidebar_width, maximum_sidebar_width);

        dock_area.update(cx, |area, cx| {
            // With nothing to show, the center keeps the empty layout the
            // area starts with; the skin draws the start page over it.
            if !center_empty {
                area.set_center(center, window, cx);
            }
            area.set_dock(
                DockPlacement::Left,
                DockLayout::tabs().panel_view(panel_handle(sidebar.clone()), cx),
                window,
                cx,
            );
            // Dock geometry is an API boundary that takes `Pixels`.
            area.set_dock_size(DockPlacement::Left, restored_sidebar_width, window, cx);
            area.set_dock_collapsible(DockPlacement::Left, true, window, cx);
            if launch_settings.sidebar_collapsed {
                area.toggle_dock(DockPlacement::Left, window, cx);
            }
            area.set_dock(
                DockPlacement::Right,
                DockLayout::tabs().panel_view(panel_handle(tools.clone()), cx),
                window,
                cx,
            );
            area.set_dock_size(DockPlacement::Right, TOOL_SIDEBAR_WIDTH, window, cx);
            // Hidden until a tool is picked; see `set_right_dock_open`.
            set_right_dock_open(area, false, window, cx);
            area.set_dock_collapsible(DockPlacement::Right, false, window, cx);
        });
        // Settled before the first `LayoutChanged` arrives, so the handler
        // does not take the initial state for a tab having just closed.
        skin.set_center_empty(center_empty, cx);
        if let Some(lock) = crate::app_lock::AppLock::global(cx) {
            lock.update(cx, |lock, cx| lock.record_activity(cx));
            Self::watch_idle_lock(lock.clone(), window, cx);
            subscriptions.push(cx.observe_in(&lock, window, |this, _, window, cx| {
                this.sync_cli_server(cx);
                if !crate::app_lock::is_locked(cx) {
                    for link in std::mem::take(&mut this.locked_links) {
                        this.open_link(link, window, cx);
                    }
                }
            }));
        }
        // Rules marked to start with the application. What they need to ask
        // arrives as events, once the workspace is there to show it.
        forwards.update(cx, |forwards, cx| forwards.start_automatic(cx));

        subscriptions.push(cx.observe(&cmder, |_, _, cx| cx.notify()));
        let this = Self {
            primary_window: window.window_handle(),
            detached_windows: Vec::new(),
            dragged_tab: None,
            detaching_tabs: Vec::new(),
            store,
            dock_area,
            skin,
            sidebar,
            remote,
            remote_panel,
            remote_hosts: HashMap::new(),
            tools,
            host_panel,
            forwards,
            recent,
            terminals,
            explorers: HashMap::new(),
            file_sidebars: HashMap::new(),
            directory_follow: HashMap::new(),
            local_terminals,
            lan_local_factories: HashMap::new(),
            lan_serial_transports: HashMap::new(),
            cmder,
            local_launches: HashMap::new(),
            saved_terminals,
            front_saved_terminal,
            settings_tab: None,
            settings,
            cli_integration: cx.new(|_| CliIntegration::new(None)),
            cli_server: None,
            locked_links: Vec::new(),
            updater,
            sync,
            lan,
            analytics: None,
            local_terminal_provider,
            administrator_launcher: None,
            administrator_launch_pending: false,
            host_terminals,
            sftp_provider,
            adb_detector: Arc::new(crate::adb::NoAdbDetector),
            android_manager: None,
            android_manager_remote: false,
            android_tools: Arc::new(crate::adb::SystemAndroidTools),
            local_directory_provider,
            connection_tester,
            next_remote_terminal_id,
            next_local_terminal_id,
            next_explorer_id: 1,
            active_tab: None,
            broadcast: Default::default(),
            compose,
            compose_height: COMPOSE_HEIGHT,
            tool_target_choice: None,
            log_split: None,
            deleted_host_tools: HashMap::new(),
            prompt_queue: VecDeque::new(),
            active_prompt: None,
            focus_handle,
            editors: HashMap::new(),
            next_editor_id: 1,
            notice_times: HashMap::new(),
            window_state_save: None,
            ssh_dir: None,
            _subscriptions: subscriptions,
        };
        if launch_settings.sidebar_collapsed {
            // The hidden host panel cannot provide a keyboard dispatch path.
            let focus = this
                .visible_center_tabs(cx)
                .first()
                .and_then(|tab| this.tab_focus_handle(*tab, cx))
                .unwrap_or_else(|| this.recent.read(cx).focus_handle(cx));
            window.focus(&focus, cx);
        }
        this
    }

    /// The center tab in front.
    pub fn active_tab(&self) -> Option<CenterTab> {
        self.active_tab
    }

    /// The host store, for tests and for panels created later.
    /// The dock area: the center's tabs and the two sidebars.
    pub fn dock_area(&self) -> &Entity<DockArea> {
        &self.dock_area
    }

    pub fn store(&self) -> &Entity<HostStore> {
        &self.store
    }

    pub fn terminal(&self, id: HostId, cx: &App) -> Option<&Entity<TerminalPanel>> {
        self.terminals
            .values()
            .filter(|panel| panel.read(cx).host_id() == id)
            .max_by_key(|panel| panel.read(cx).id().0)
    }

    pub fn remote_terminal(&self, id: RemoteTerminalId) -> Option<&Entity<TerminalPanel>> {
        self.terminals.get(&id)
    }

    pub fn terminal_count(&self, id: HostId, cx: &App) -> usize {
        self.terminals_of(id, cx).count()
    }

    /// A host's terminal tabs, in no particular order.
    fn terminals_of<'a>(
        &'a self,
        host: HostId,
        cx: &'a App,
    ) -> impl Iterator<Item = &'a Entity<TerminalPanel>> {
        self.terminals
            .values()
            .filter(move |panel| panel.read(cx).host_id() == host)
    }

    pub fn explorer(&self, id: ExplorerId) -> Option<&Entity<ExplorerPanel>> {
        self.explorers.get(&id)
    }

    /// A host's SFTP tabs, oldest first. Not its terminals' 文件, which
    /// come and go with the terminals.
    pub fn explorers_of(&self, host: HostId, cx: &App) -> Vec<Entity<ExplorerPanel>> {
        let mut explorers: Vec<_> = self
            .explorers
            .iter()
            .filter(|(_, panel)| {
                let panel = panel.read(cx);
                panel.host_id() == host && !panel.is_sidebar()
            })
            .collect();
        explorers.sort_by_key(|(id, _)| id.0);
        explorers
            .into_iter()
            .map(|(_, panel)| panel.clone())
            .collect()
    }

    /// Whether a host has any tab open in the center.
    pub(super) fn has_tabs(&self, host: HostId, cx: &App) -> bool {
        self.terminals_of(host, cx).next().is_some() || !self.explorers_of(host, cx).is_empty()
    }

    /// Whether one of the host's terminals or SFTP tabs is connected. Each
    /// tab goes by its own connection; this is for whoever asks after the
    /// host as a whole.
    pub fn has_connection(&self, host: HostId, cx: &App) -> bool {
        self.terminals_of(host, cx)
            .any(|panel| panel.read(cx).connection_state(cx).is_connected())
            || self
                .explorers_of(host, cx)
                .iter()
                .any(|panel| panel.read(cx).connection_state().is_connected())
    }

    /// A remote terminal's 文件, once it has been shown.
    pub fn file_sidebar(&self, terminal: RemoteTerminalId) -> Option<&Entity<ExplorerPanel>> {
        self.explorers.get(self.file_sidebars.get(&terminal)?)
    }

    /// The terminal whose 文件 an explorer is, if it is one.
    pub(super) fn sidebar_terminal(&self, explorer: ExplorerId) -> Option<RemoteTerminalId> {
        self.file_sidebars
            .iter()
            .find(|(_, id)| **id == explorer)
            .map(|(terminal, _)| *terminal)
    }

    /// A remote terminal's 文件, made the first time it is shown: SFTP on a
    /// channel of the terminal's own connection, no login of its own.
    pub(super) fn ensure_file_sidebar(
        &mut self,
        terminal: RemoteTerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<ExplorerPanel>> {
        if let Some(panel) = self.file_sidebar(terminal) {
            return Some(panel.clone());
        }
        // On the terminal's connection, so as the terminal took the host.
        let panel = self.terminals.get(&terminal)?.read(cx);
        let HostConnection::Ssh(login) = panel.connection().clone() else {
            return None;
        };
        let host = panel.host().clone();
        let channels = panel.terminal().read(cx).channels(cx);
        let id = ExplorerId(self.next_explorer_id);
        self.next_explorer_id += 1;
        let route = SftpRoute::Terminal(channels);
        let (panel, subscription) = new_explorer_panel(self, id, host, login, route, window, cx);
        self._subscriptions.push(subscription);
        self.explorers.insert(id, panel.clone());
        self.file_sidebars.insert(terminal, id);
        let follow = self.directory_follow_setting(cx);
        panel.update(cx, |panel, cx| panel.set_follow(follow, cx));
        // Where the terminal is, rather than home, if its shell said.
        self.follow_current_directory(terminal, window, cx);
        Some(panel)
    }

    pub(super) fn install_serial_network_files(
        &mut self,
        terminal: RemoteTerminalId,
        address: String,
        user: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.file_sidebar(terminal).is_some() {
            return;
        }
        let draft = crate::host::HostDraft::new(
            &address,
            &address,
            22,
            user,
            crate::host::AuthKind::Password,
            None,
        );
        self.install_serial_file_route(terminal, draft, SftpRoute::NetworkSidebar, window, cx);
    }

    pub(super) fn install_serial_adb_files(
        &mut self,
        terminal: RemoteTerminalId,
        connection: crate::adb::AdbConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = format!("ADB · {}", connection.serial());
        let draft = crate::host::HostDraft::new(
            &name,
            "adb",
            22,
            "shell",
            crate::host::AuthKind::NoPassword,
            None,
        );
        self.install_serial_file_route(
            terminal,
            draft,
            SftpRoute::ExternalSidebar(Arc::new(connection)),
            window,
            cx,
        );
    }

    fn install_serial_file_route(
        &mut self,
        terminal: RemoteTerminalId,
        draft: crate::host::HostDraft,
        route: SftpRoute,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.file_sidebar(terminal).is_some() {
            return;
        }
        let host_id = self
            .store
            .update(cx, |store, cx| store.insert_temporary(draft, None, cx));
        let (host, login) = {
            let store = self.store.read(cx);
            (
                store.host(host_id).unwrap().clone(),
                store.login(host_id).unwrap(),
            )
        };
        let id = ExplorerId(self.next_explorer_id);
        self.next_explorer_id += 1;
        let (panel, subscription) = new_explorer_panel(self, id, host, login, route, window, cx);
        self._subscriptions.push(subscription);
        self.explorers.insert(id, panel.clone());
        self.file_sidebars.insert(terminal, id);
        self.sync_tool_sidebar(window, cx);
    }

    /// End a terminal's 文件 with the terminal. Its `Closed` does the rest,
    /// as for an SFTP tab: its editors, its place in the registers.
    fn close_file_sidebar(
        &mut self,
        terminal: RemoteTerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // It logs in nowhere, so no question of its own can be waiting.
        if let Some(panel) = self.file_sidebar(terminal).cloned() {
            let owner = PromptOwner::Sftp(panel.read(cx).id(), panel.read(cx).generation());
            self.cancel_prompts_for_owner(owner, window, cx);
            panel.update(cx, |panel, cx| panel.close(window, cx));
        }
    }

    pub fn local_terminal(&self, id: LocalTerminalId) -> Option<&Entity<LocalTerminalPanel>> {
        self.local_terminals.get(&id)
    }

    pub fn settings_tab(&self) -> Option<&Entity<SettingsPanel>> {
        self.settings_tab.as_ref()
    }

    pub fn set_adb_detector(&mut self, detector: Arc<dyn crate::adb::AdbDetector>) {
        self.adb_detector = detector;
    }

    pub fn set_android_tools(&mut self, tools: Arc<dyn crate::adb::AndroidTools>) {
        self.android_tools = tools;
    }

    pub fn set_monitor_connector(
        &mut self,
        connector: Arc<dyn crate::monitor::MonitorConnector>,
        cx: &mut Context<Self>,
    ) {
        self.tools
            .update(cx, |tools, cx| tools.set_monitor_connector(connector, cx));
    }

    pub fn settings(&self) -> &Entity<SettingsStore> {
        &self.settings
    }

    pub fn lan(&self) -> &Entity<crate::lan::LanService> {
        &self.lan
    }

    pub fn set_administrator_launcher(
        &mut self,
        launcher: Arc<dyn super::local_terminal_window::AdministratorTerminalLauncher>,
    ) {
        self.administrator_launcher = Some(launcher);
    }

    /// The running port forwards, for tests.
    pub fn forwards(&self) -> &Entity<ForwardManager> {
        &self.forwards
    }

    /// Tests hand it a temporary home and bin directory.
    pub fn cli_integration(&self) -> &Entity<CliIntegration> {
        &self.cli_integration
    }

    pub fn updater(&self) -> &Entity<Updater> {
        &self.updater
    }

    pub fn sync(&self) -> &Entity<SyncService> {
        &self.sync
    }

    /// Give the focus to the center: the tab shown last, or the start page
    /// while there is none. For when the focus is about to go off screen.
    pub(super) fn focus_center(&self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self
            .active_tab
            .and_then(|tab| self.tab_focus_handle(tab, cx));
        let handle = tab.or_else(|| {
            self.skin
                .is_center_empty()
                .then(|| self.recent.read(cx).focus_handle(cx))
        });
        if let Some(handle) = handle {
            window.focus(&handle, cx);
        }
    }

    /// Where the focus goes in `tab`.
    pub(super) fn tab_focus_handle(&self, tab: CenterTab, cx: &App) -> Option<FocusHandle> {
        match tab {
            CenterTab::Terminal(id) => self
                .terminals
                .get(&id)
                .map(|panel| panel.read(cx).focus_handle(cx)),
            CenterTab::Explorer(id) => self
                .explorers
                .get(&id)
                .map(|panel| panel.read(cx).focus_handle(cx)),
            CenterTab::LocalTerminal(id) => self
                .local_terminals
                .get(&id)
                .map(|panel| panel.read(cx).focus_handle(cx)),
            CenterTab::Settings => self
                .settings_tab
                .as_ref()
                .map(|panel| panel.read(cx).focus_handle(cx)),
            CenterTab::Editor(id) => self
                .editors
                .get(&id)
                .map(|panel| panel.read(cx).focus_handle(cx)),
        }
    }

    /// Keep the start page and focus in step with the center: once its last
    /// tab closes, the page fills it and takes the focus the tab held, so
    /// window-level shortcuts keep a dispatch path.
    fn on_layout_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let empty = self.dock_area.read(cx).is_empty(DockPlacement::Center, cx);
        let was_empty = self.skin.is_center_empty();
        self.skin.set_center_empty(empty, cx);
        if empty && !was_empty {
            self.recent
                .update(cx, |recent, cx| recent.clear_selection(cx));
            let page = self.recent.read(cx).focus_handle(cx);
            window.focus(&page, cx);
        }
        // A tab dragged into another group may hide the one in front.
        self.sync_front(window, cx);
        self.sync_broadcast(cx);
        self.sync_compose(window, cx);
        // Tabs reordered, moved, opened or closed: what the next launch
        // opens again.
        self.sync_saved_terminals(cx);
    }

    pub(super) fn enqueue_prompt(
        &mut self,
        terminal_id: PromptOwner,
        host_id: HostId,
        prompt: ConnectionPrompt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prompt_queue.push_back((terminal_id, host_id, prompt));
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(40))
                    .await;
                let done = this
                    .update_in(cx, |this, window, cx| {
                        if this.prompt_queue.is_empty() {
                            return true;
                        }
                        if crate::app_lock::is_locked(cx)
                            || this.active_prompt.is_some()
                            || window.has_active_dialog(cx)
                        {
                            return false;
                        }
                        this.open_next_prompt(window, cx);
                        true
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
            }
        })
        .detach();
    }

    fn open_next_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (terminal_id, host_id, prompt) = loop {
            let Some(next) = self.prompt_queue.pop_front() else {
                return;
            };
            if self.prompt_owner_is_live(next.0, cx) {
                break next;
            }
        };
        let request_id = prompt.request_id();
        self.active_prompt = Some((terminal_id, host_id, request_id));
        let origin = match terminal_id {
            PromptOwner::Forward(id, _) => self.forward_prompt_origin(id, cx),
            PromptOwner::Terminal(_) | PromptOwner::Sftp(..) => None,
        };
        // What a question says first when it needs saying who is asking.
        let introduced = |text: String| match &origin {
            Some(origin) if text.trim().is_empty() => origin.clone(),
            Some(origin) => format!("{origin}\n\n{text}"),
            None => text,
        };
        let workspace = cx.entity().downgrade();
        // Every button of a prompt ends it the same way, with its own reply.
        let answer = move |reply: ConnectionPromptReply, cx: &mut App| {
            workspace
                .update(cx, |this, cx| {
                    this.finish_prompt(terminal_id, request_id, reply, cx)
                })
                .ok();
        };
        match prompt.kind().clone() {
            ConnectionPromptKind::UnknownHost(prompt) => {
                let description = introduced(unknown_host_description(&prompt));
                window.open_alert_dialog(cx, move |alert, _, _| {
                    alert
                        .title(t!("workspace.host_key.unknown_title"))
                        .description(description.clone())
                        .button_props(
                            DialogButtonProps::default()
                                .ok_text(t!("workspace.host_key.trust"))
                                .cancel_text(t!("common.cancel")),
                        )
                        .show_cancel(true)
                        .on_ok({
                            let answer = answer.clone();
                            move |_, _, cx| {
                                answer(ConnectionPromptReply::TrustAndSave, cx);
                                true
                            }
                        })
                        .on_cancel({
                            let answer = answer.clone();
                            move |_, _, cx| {
                                answer(ConnectionPromptReply::Cancel, cx);
                                true
                            }
                        })
                        .on_close({
                            let answer = answer.clone();
                            move |_, _, cx| answer(ConnectionPromptReply::Cancel, cx)
                        })
                });
            }
            ConnectionPromptKind::HostKeyChanged(prompt) => {
                let old = prompt.old_fingerprints().join(&t!("common.list.separator"));
                let description = introduced(
                    match prompt.jump_host() {
                        Some(jump_host) => t!(
                            "workspace.host_key.changed_via_jump_host",
                            jump_host = jump_host,
                            host = prompt.host(),
                            port = prompt.port(),
                            algorithm = prompt.algorithm(),
                            old = old,
                            fingerprint = prompt.fingerprint()
                        ),
                        None => t!(
                            "workspace.host_key.changed",
                            host = prompt.host(),
                            port = prompt.port(),
                            algorithm = prompt.algorithm(),
                            old = old,
                            fingerprint = prompt.fingerprint()
                        ),
                    }
                    .to_string(),
                );
                // Updating is the risky answer: it trusts a key that may be
                // someone posing as the server.
                window.open_alert_dialog(cx, move |alert, _, _| {
                    alert
                        .title(t!("workspace.host_key.changed_title"))
                        .description(description.clone())
                        .button_props(
                            DialogButtonProps::default()
                                .ok_text(t!("workspace.host_key.update"))
                                .ok_variant(ButtonVariant::Danger)
                                .cancel_text(t!("common.cancel")),
                        )
                        .show_cancel(true)
                        .on_ok({
                            let answer = answer.clone();
                            move |_, _, cx| {
                                answer(ConnectionPromptReply::TrustAndSave, cx);
                                true
                            }
                        })
                        .on_cancel({
                            let answer = answer.clone();
                            move |_, _, cx| {
                                answer(ConnectionPromptReply::Cancel, cx);
                                true
                            }
                        })
                        .on_close({
                            let answer = answer.clone();
                            move |_, _, cx| answer(ConnectionPromptReply::Cancel, cx)
                        })
                });
            }
            ConnectionPromptKind::Authentication(prompt) => {
                let title = authentication_title(&prompt);
                let instructions = introduced(authentication_instructions(&prompt));
                let fields = authentication_fields(&prompt);
                let dialog_title: SharedString = if title.trim().is_empty() {
                    t!("workspace.auth.title")
                } else {
                    title.clone().into()
                };
                let passwords = matches!(
                    prompt.request(),
                    crate::connection::AuthenticationRequest::Password { .. }
                )
                .then(|| self.store.clone());
                let form =
                    cx.new(|cx| AuthenticationPromptForm::new(fields, passwords, window, cx));
                window.open_dialog(cx, move |dialog, _, _| {
                    dialog
                        .title(dialog_title.clone())
                        .child(
                            v_flex()
                                .gap_3()
                                .when(!instructions.trim().is_empty(), |view| {
                                    view.child(div().text_sm().child(instructions.clone()))
                                })
                                .child(form.clone()),
                        )
                        .footer(commit_footer(
                            "ssh-auth-submit",
                            t!("workspace.auth.continue"),
                        ))
                        .on_ok({
                            let answer = answer.clone();
                            let form = form.clone();
                            move |_, window, cx| {
                                let Some(answers) =
                                    form.update(cx, |form, cx| form.take_answers(window, cx))
                                else {
                                    return false;
                                };
                                answer(ConnectionPromptReply::Answers(answers), cx);
                                true
                            }
                        })
                        .on_cancel({
                            let answer = answer.clone();
                            move |_, _, cx| {
                                answer(ConnectionPromptReply::Cancel, cx);
                                true
                            }
                        })
                        .on_close({
                            let answer = answer.clone();
                            move |_, _, cx| answer(ConnectionPromptReply::Cancel, cx)
                        })
                        .overlay_closable(false)
                });
            }
        }
    }

    fn finish_prompt(
        &mut self,
        terminal_id: PromptOwner,
        request_id: u64,
        reply: ConnectionPromptReply,
        cx: &mut Context<Self>,
    ) {
        let Some((active_terminal, _, active_request)) = self.active_prompt else {
            return;
        };
        if (active_terminal, active_request) != (terminal_id, request_id) {
            return;
        }
        self.active_prompt = None;
        let canceled = matches!(&reply, ConnectionPromptReply::Cancel);
        self.reply_to_owner(terminal_id, request_id, reply, cx);
        if canceled {
            match terminal_id {
                PromptOwner::Terminal(id) => {
                    if let Some(panel) = self.terminals.get(&id).cloned() {
                        panel.update(cx, |panel, cx| panel.cancel_connection(cx));
                    }
                }
                // Declining the question is declining the forward: it ends
                // as stopped, not as failed.
                PromptOwner::Forward(id, _) => {
                    self.forwards
                        .update(cx, |forwards, cx| forwards.stop(id, cx));
                }
                PromptOwner::Sftp(..) => {}
            }
        }
    }

    fn prompt_owner_is_live(&self, owner: PromptOwner, cx: &App) -> bool {
        match owner {
            PromptOwner::Terminal(id) => self.terminals.contains_key(&id),
            PromptOwner::Sftp(id, generation) => self
                .explorers
                .get(&id)
                .is_some_and(|p| p.read(cx).generation() == generation),
            PromptOwner::Forward(id, generation) => {
                self.forwards.read(cx).generation(id) == Some(generation)
            }
        }
    }

    fn reply_to_owner(
        &self,
        owner: PromptOwner,
        request_id: u64,
        reply: ConnectionPromptReply,
        cx: &App,
    ) {
        match owner {
            PromptOwner::Terminal(id) => {
                if let Some(panel) = self.terminals.get(&id) {
                    panel.read(cx).reply_to_prompt(request_id, reply, cx);
                }
            }
            PromptOwner::Sftp(id, generation) => {
                if let Some(panel) = self.explorers.get(&id)
                    && panel.read(cx).generation() == generation
                {
                    panel.read(cx).reply_to_prompt(request_id, reply);
                }
            }
            PromptOwner::Forward(id, generation) => {
                self.forwards
                    .read(cx)
                    .reply_to_prompt(id, generation, request_id, reply);
            }
        }
    }

    pub(super) fn cancel_prompts_for_owner(
        &mut self,
        owner: PromptOwner,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prompt_queue.retain(|(id, _, _)| *id != owner);
        if let Some((id, _, request_id)) = self.active_prompt
            && id == owner
        {
            self.active_prompt = None;
            self.reply_to_owner(id, request_id, ConnectionPromptReply::Cancel, cx);
            if window.has_active_dialog(cx) {
                window.close_dialog(cx);
            }
        }
    }
    fn cancel_prompts_for_terminal(
        &mut self,
        terminal_id: RemoteTerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel_prompts_for_owner(PromptOwner::Terminal(terminal_id), window, cx);
    }
    pub(super) fn cancel_prompts_for_host(
        &mut self,
        host_id: HostId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A port forward through the host is on its own: what the
        // host's tabs are told does not reach it. Its questions end when
        // the forward does.
        let of_a_tab = |owner: &PromptOwner| !matches!(owner, PromptOwner::Forward(..));
        self.prompt_queue
            .retain(|(owner, id, _)| *id != host_id || !of_a_tab(owner));
        if let Some((owner, id, _)) = self.active_prompt
            && id == host_id
            && of_a_tab(&owner)
        {
            self.cancel_prompts_for_owner(owner, window, cx);
        }
    }

    fn on_connect_host(
        &mut self,
        action: &ConnectHost,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.connect_host(action.0, window, cx);
    }

    fn on_connect_group(
        &mut self,
        action: &ConnectGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let host_ids = self.store.read(cx).hosts_under(action.0);
        for host_id in host_ids {
            self.connect_host(host_id, window, cx);
        }
    }

    pub(super) fn connect_host(
        &mut self,
        host_id: HostId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_remote_terminal(host_id, None, window, cx);
    }

    fn on_duplicate_terminal(
        &mut self,
        action: &DuplicateTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = self.terminals.get(&action.0) else {
            return;
        };
        let source = source.read(cx);
        let (host_id, copy) = (source.host_id(), source.copy_for_new_tab(cx));
        self.open_remote_terminal(host_id, Some(copy), window, cx);
    }

    fn on_duplicate_active_terminal(
        &mut self,
        _: &crate::app::DuplicateActiveTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(CenterTab::Terminal(id)) = self.tab_in_window(window) {
            self.on_duplicate_terminal(&DuplicateTerminal(id), window, cx);
        }
    }

    fn open_remote_terminal(
        &mut self,
        host_id: HostId,
        copy: Option<TerminalCopy>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.remote_host_allowed(host_id, cx) {
            window.push_notification(Notification::info(t!("remote.connect_first")), cx);
            return;
        }
        if self.store.read(cx).host(host_id).is_none() {
            return;
        }
        let terminal_id = RemoteTerminalId(self.next_remote_terminal_id);
        self.next_remote_terminal_id += 1;
        let (panel, subscription) = new_terminal_panel(
            &self.store,
            self.lan_serial_transports
                .get(&host_id)
                .cloned()
                .unwrap_or_else(|| self.host_terminals.clone()),
            terminal_id,
            host_id,
            copy,
            window,
            cx,
        );
        self._subscriptions.push(subscription);
        let focus = panel.read(cx).focus_handle(cx);
        self._subscriptions.push(follow_focus(
            CenterTab::Terminal(terminal_id),
            &focus,
            window,
            cx,
        ));
        self.terminals.insert(terminal_id, panel.clone());
        self.dock_area.update(cx, |area, cx| {
            area.add_panel_view(panel_handle(panel), DockPlacement::Center, None, window, cx);
        });
    }

    fn on_open_explorer(
        &mut self,
        action: &OpenExplorer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_explorer(action.0, window, cx);
    }

    pub(super) fn open_explorer(
        &mut self,
        host_id: HostId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.remote_host_allowed(host_id, cx) {
            window.push_notification(Notification::info(t!("remote.connect_first")), cx);
            return;
        }
        // Only an SSH host has SFTP; nothing offers it another.
        let store = self.store.read(cx);
        let Some((host, login)) = store
            .host(host_id)
            .filter(|host| host.is_ssh())
            .cloned()
            .zip(store.login(host_id))
        else {
            return;
        };
        // Every request opens a tab of its own, as connecting does for
        // terminals: two directories of one host side by side.
        let id = ExplorerId(self.next_explorer_id);
        self.next_explorer_id += 1;
        let (panel, subscription) =
            new_explorer_panel(self, id, host, login, SftpRoute::Login, window, cx);
        self._subscriptions.push(subscription);
        let focus = panel.read(cx).focus_handle(cx);
        self._subscriptions
            .push(follow_focus(CenterTab::Explorer(id), &focus, window, cx));
        self.explorers.insert(id, panel.clone());
        self.dock_area.update(cx, |area, cx| {
            area.add_panel_view(panel_handle(panel), DockPlacement::Center, None, window, cx);
        });
    }

    fn on_new_local_terminal(
        &mut self,
        _: &NewLocalTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.on_start_local_terminal(
            &StartLocalTerminal(crate::terminal::LocalTerminalStart::new(
                LocalTerminalKind::Shell,
            )),
            window,
            cx,
        );
    }

    fn on_open_local_terminal_options(
        &mut self,
        action: &OpenLocalTerminalOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        super::local_terminal_options::open_local_terminal_options(
            action.0,
            self.settings.clone(),
            self.focus_handle.clone(),
            window,
            cx,
        );
    }

    fn on_start_local_terminal(
        &mut self,
        action: &StartLocalTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let start = &action.0;
        if start.administrator {
            if self.administrator_launch_pending {
                return;
            }
            let cmder = self
                .cmder
                .read(cx)
                .directory()
                .map(std::path::Path::to_path_buf);
            let request = super::local_terminal_window::AdminTerminalRequest::new(
                start,
                &self.settings.read(cx).settings(),
                cmder,
            );
            let Some(launcher) = self.administrator_launcher.clone() else {
                return;
            };
            self.administrator_launch_pending = true;
            cx.spawn_in(window, async move |this, cx| {
                let result = cx
                    .background_spawn(async move { launcher.launch(&request) })
                    .await;
                this.update(cx, |this, _| {
                    this.administrator_launch_pending = false;
                })
                .ok();
                cx.update(|window, cx| match result {
                    Ok(true) => {}
                    Ok(false) => window.push_notification(
                        Notification::info(t!("workspace.local_terminal.options.cancelled")),
                        cx,
                    ),
                    Err(error) => window.push_notification(
                        Notification::error(t!(
                            "workspace.local_terminal.options.start_failed",
                            error = error
                        )),
                        cx,
                    ),
                })
                .ok();
            })
            .detach();
            return;
        }
        let settings = self.settings.read(cx).settings();
        let shell = match start.kind {
            LocalTerminalKind::Shell => crate::terminal::LocalShell::new(
                settings.local_terminal.shell.as_deref().unwrap_or_default(),
            ),
            LocalTerminalKind::Cmder => {
                let Some(directory) = self.cmder.read(cx).directory() else {
                    return;
                };
                crate::terminal::LocalShell::cmder(directory.to_path_buf())
            }
        };
        let mut launch = crate::terminal::LocalLaunch::new(shell);
        launch.directory = start
            .directory
            .clone()
            .or(settings.local_terminal.start_directory);
        let directory = launch.directory.clone();
        if let Some(directory) = directory {
            cx.spawn_in(window, async move |this, cx| {
                let valid = cx
                    .background_spawn(async move { directory.is_absolute() && directory.is_dir() })
                    .await;
                this.update_in(cx, |this, window, cx| {
                    if valid {
                        this.open_configured_local_terminal(launch, window, cx);
                    } else {
                        window.push_notification(
                            Notification::error(t!(
                                "workspace.local_terminal.options.invalid_directory"
                            )),
                            cx,
                        );
                    }
                })
                .ok();
            })
            .detach();
        } else {
            self.open_configured_local_terminal(launch, window, cx);
        }
    }

    fn on_new_cmder_terminal(
        &mut self,
        _: &crate::app::NewCmderTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.on_start_local_terminal(
            &StartLocalTerminal(crate::terminal::LocalTerminalStart::new(
                LocalTerminalKind::Cmder,
            )),
            window,
            cx,
        );
    }

    fn open_configured_local_terminal(
        &mut self,
        launch: crate::terminal::LocalLaunch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cmder = launch.shell.cmder_directory().is_some();
        let factory = self.local_terminal_provider.factory_for(&launch);
        let id = LocalTerminalId(self.next_local_terminal_id);
        self.next_local_terminal_id += 1;
        let name = cmder.then(|| SavedName {
            name: "Cmder".into(),
            fixed: true,
            title: None,
        });
        let (panel, subscription) = new_local_terminal_panel(id, factory, name, window, cx);
        self._subscriptions.push(subscription);
        let focus = panel.read(cx).focus_handle(cx);
        self._subscriptions.push(follow_focus(
            CenterTab::LocalTerminal(id),
            &focus,
            window,
            cx,
        ));
        self.local_terminals.insert(id, panel.clone());
        self.local_launches.insert(id, launch);
        if !cmder {
            self.count(Counter::LocalTerminal);
        }
        self.dock_area.update(cx, |area, cx| {
            area.add_panel_view(panel_handle(panel), DockPlacement::Center, None, window, cx);
        });
    }

    fn on_new_command_terminal(
        &mut self,
        _: &NewCommandTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = LocalTerminalId(self.next_local_terminal_id);
        self.next_local_terminal_id += 1;
        let name = Some(SavedName {
            name: "ShellLF".into(),
            fixed: true,
            title: None,
        });
        let (panel, subscription) = new_local_terminal_panel(
            id,
            crate::terminal::command_terminal_factory(),
            name,
            window,
            cx,
        );
        panel.update(cx, |panel, cx| panel.set_command_terminal(cx));
        self._subscriptions.push(subscription);
        let focus = panel.read(cx).focus_handle(cx);
        self._subscriptions.push(follow_focus(
            CenterTab::LocalTerminal(id),
            &focus,
            window,
            cx,
        ));
        self.local_terminals.insert(id, panel.clone());
        self.dock_area.update(cx, |area, cx| {
            area.add_panel_view(panel_handle(panel), DockPlacement::Center, None, window, cx);
        });
    }

    pub fn remote_management(&self) -> &Entity<crate::remote::Service> {
        &self.remote
    }

    fn on_show_remote_management(
        &mut self,
        _: &crate::app::ShowRemoteManagement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_sidebar(super::sidebar::SidebarMode::Remote, window, cx);
    }

    fn on_remote_action(
        &mut self,
        action: &crate::app::RemoteAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if crate::app_lock::is_locked(cx) {
            return;
        }
        if let crate::remote::Command::Ssh {
            server,
            device,
            user,
        } = &action.0
        {
            if self.remote.read(cx).ssh_target(server, device).is_none() {
                window.push_notification(Notification::info(t!("remote.connect_first")), cx);
                return;
            }
            if user.is_empty()
                || user.len() > 128
                || user
                    .chars()
                    .any(|character| character.is_control() || character.is_whitespace())
            {
                return;
            }
            self.remote.update(cx, |service, cx| {
                service.run(
                    crate::remote::Operation::Resolve(
                        server.clone(),
                        device.clone(),
                        user.clone(),
                        None,
                    ),
                    cx,
                )
            });
            return;
        }
        self.remote_panel
            .update(cx, |panel, cx| panel.command(action.0.clone(), window, cx));
    }

    fn open_resolved_remote(
        &mut self,
        resolved: &crate::remote::ResolvedSsh,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if crate::app_lock::is_locked(cx)
            || self
                .remote
                .read(cx)
                .ssh_target(&resolved.server, &resolved.device)
                .is_none()
        {
            return;
        }
        if let Some(terminal) = resolved.terminal {
            if let Some(panel) = self.terminals.get(&RemoteTerminalId(terminal)).cloned()
                && self
                    .remote_hosts
                    .get(&panel.read(cx).host_id())
                    .is_some_and(|(server, network, device, _)| {
                        *server == resolved.server
                            && *network == resolved.network
                            && *device == resolved.device
                    })
            {
                panel.update(cx, |panel, cx| panel.reconnect(window, cx));
            }
            return;
        }
        let draft = crate::host::HostDraft::new(
            &resolved.name,
            &resolved.address,
            22,
            &resolved.user,
            crate::host::AuthKind::Password,
            None,
        );
        let host = self.store.update(cx, |store, cx| {
            let host = store.insert_temporary(draft, None, cx);
            store.set_temporary_host_key_alias(host, resolved.alias.clone());
            host
        });
        self.remote_hosts.insert(
            host,
            (
                resolved.server.clone(),
                resolved.network.clone(),
                resolved.device.clone(),
                resolved.user.clone(),
            ),
        );
        self.connect_host(host, window, cx);
    }

    fn disconnect_remote_hosts(
        &mut self,
        hosts: Option<&std::collections::HashSet<HostId>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let managed = |host: HostId| {
            self.remote_hosts.contains_key(&host) && hosts.is_none_or(|hosts| hosts.contains(&host))
        };
        let terminals: Vec<_> = self
            .terminals
            .iter()
            .filter(|(_, panel)| managed(panel.read(cx).host_id()))
            .map(|(id, _)| *id)
            .collect();
        let explorers: Vec<_> = self
            .explorers
            .values()
            .filter(|panel| managed(panel.read(cx).host_id()))
            .cloned()
            .collect();
        for id in terminals {
            self.on_disconnect_terminal(&DisconnectTerminal(id), window, cx);
        }
        for panel in explorers {
            panel.update(cx, |panel, cx| panel.disconnect(window, cx));
        }
    }

    fn remote_host_allowed(&self, host: HostId, cx: &App) -> bool {
        self.remote_hosts
            .get(&host)
            .is_none_or(|(server, network, device, _)| {
                !self.remote.read(cx).is_busy()
                    && self.remote.read(cx).is_logged_in(server)
                    && self.remote.read(cx).connection().is_some_and(|current| {
                        current.server == *server && current.network == *network
                    })
                    && self.remote.read(cx).ssh_target(server, device).is_some()
            })
    }

    /// Open a local terminal tab: in the home folder, or a saved terminal's
    /// in the folder it was last in, named as it is.
    pub(super) fn open_local_terminal(
        &mut self,
        saved: Option<&SavedTerminal>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> LocalTerminalId {
        self.open_local_terminal_at(None, saved, window, cx)
    }

    pub(super) fn open_local_terminal_at(
        &mut self,
        directory: Option<PathBuf>,
        saved: Option<&SavedTerminal>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> LocalTerminalId {
        let id = LocalTerminalId(self.next_local_terminal_id);
        self.next_local_terminal_id += 1;
        let settings = self.settings.read(cx).settings();
        let replay = saved.and_then(|saved| {
            super::projects::kept_replay(self.store.read(cx), &settings, saved.id)
        });
        let mut launch = super::projects::local_launch(&settings, saved, replay);
        if directory.is_some() {
            launch.directory = directory;
        }
        let factory = self.local_terminal_provider.factory_for(&launch);
        if saved.is_none() {
            self.local_launches.insert(id, launch);
        }
        let name = saved.map(super::projects::saved_name);
        let (panel, subscription) = new_local_terminal_panel(id, factory, name, window, cx);
        self._subscriptions.push(subscription);
        let focus = panel.read(cx).focus_handle(cx);
        self._subscriptions.push(follow_focus(
            CenterTab::LocalTerminal(id),
            &focus,
            window,
            cx,
        ));
        self.local_terminals.insert(id, panel.clone());
        if let Some(saved) = saved {
            self.saved_terminals.insert(id, saved.id);
        }
        self.dock_area.update(cx, |area, cx| {
            area.add_panel_view(panel_handle(panel), DockPlacement::Center, None, window, cx);
        });
        id
    }

    /// Open the settings tab, or bring the open one forward: settings are
    /// one place, not one tab per visit.
    fn on_open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_detached_tab(CenterTab::Settings, window, cx) {
            return;
        }
        if let Some(panel) = self.settings_tab.clone() {
            let group = panel.read(cx).tab_group();
            self.activate_tab(group, panel.entity_id(), window, cx);
            // Already displayed, the group reports nothing, so the panel
            // takes the focus itself.
            panel.update(cx, |panel, cx| panel.show(window, cx));
            return;
        }
        let store = self.settings.clone();
        let integration = self.cli_integration.clone();
        // Something may have been installed or removed since last time.
        integration.update(cx, |integration, cx| integration.refresh(cx));
        let updater = self.updater.clone();
        let sync = self.sync.clone();
        let cmder = self.cmder.clone();
        let panel =
            cx.new(|cx| SettingsPanel::new(store, integration, updater, sync, cmder, window, cx));
        panel.update(cx, |panel, cx| panel.set_lan(self.lan.clone(), window, cx));
        let subscription = cx.subscribe_in(
            &panel,
            window,
            |this, _, event: &SettingsPanelEvent, window, cx| {
                match event {
                    SettingsPanelEvent::Activated => {
                        this.set_active_tab(Some(CenterTab::Settings), window, cx);
                    }
                    SettingsPanelEvent::Closed => {
                        this.finish_detached_tab(CenterTab::Settings, cx);
                        this.settings_tab = None;
                        if this.active_tab == Some(CenterTab::Settings) {
                            this.set_active_tab(None, window, cx);
                        }
                    }
                }
                cx.notify();
            },
        );
        self._subscriptions.push(subscription);
        let focus = panel.read(cx).focus_handle(cx);
        self._subscriptions
            .push(follow_focus(CenterTab::Settings, &focus, window, cx));
        self.settings_tab = Some(panel.clone());
        self.dock_area.update(cx, |area, cx| {
            area.add_panel_view(panel_handle(panel), DockPlacement::Center, None, window, cx);
        });
    }

    fn on_install_cli_command(
        &mut self,
        _: &InstallCliCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_cli_integration(
            t!("workspace.cli.install_command_failed"),
            crate::cli::install_binary,
            window,
            cx,
        );
    }

    fn on_remove_cli_command(
        &mut self,
        _: &RemoveCliCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_cli_integration(
            t!("workspace.cli.remove_command_failed"),
            crate::cli::remove_binary,
            window,
            cx,
        );
    }

    fn on_install_agent_skill(
        &mut self,
        action: &InstallAgentSkill,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let agent = action.0;
        self.change_cli_integration(
            t!("workspace.cli.install_skill_failed"),
            move |paths| crate::cli::install_skill(paths, agent),
            window,
            cx,
        );
    }

    fn on_remove_agent_skill(
        &mut self,
        action: &RemoveAgentSkill,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let agent = action.0;
        self.change_cli_integration(
            t!("workspace.cli.remove_skill_failed"),
            move |paths| crate::cli::remove_skill(paths, agent),
            window,
            cx,
        );
    }

    fn on_refresh_cli_integration(
        &mut self,
        _: &RefreshCliIntegration,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cli_integration
            .update(cx, |integration, cx| integration.refresh(cx));
    }

    fn on_copy_agent_skill(
        &mut self,
        _: &CopyAgentSkill,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.write_to_clipboard(ClipboardItem::new_string(crate::cli::SKILL.to_string()));
        window.push_notification(Notification::success(t!("workspace.cli.skill_copied")), cx);
    }

    /// Install or remove part of the external CLI off the main thread, then
    /// look again at what is there. Installing the command can wait on the
    /// system's administrator prompt.
    fn change_cli_integration(
        &mut self,
        failure: SharedString,
        change: impl FnOnce(&IntegrationPaths) -> std::io::Result<()> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let integration = self.cli_integration.clone();
        let Some(paths) = integration.read(cx).paths().cloned() else {
            return;
        };
        cx.spawn_in(window, async move |_, cx| {
            let result = cx.background_spawn(async move { change(&paths) }).await;
            let _ = cx.update(|window, cx| {
                match result {
                    // The user closed the administrator prompt.
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => window.push_notification(
                        Notification::error(error.to_string()).title(failure),
                        cx,
                    ),
                    Ok(()) => {}
                }
                integration.update(cx, |integration, cx| integration.refresh(cx));
            });
        })
        .detach();
    }

    fn on_close_settings(
        &mut self,
        _: &CloseSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.settings_tab.clone() {
            self.remove_tab_panel(panel, window, cx);
        }
    }

    /// Display one tab of a tab group, found by its panel's entity.
    pub(super) fn activate_tab(
        &self,
        group: Option<WeakEntity<TabGroup>>,
        panel: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.center_tab_for_panel(PanelId::from(panel))
            && self.show_detached_tab(tab, window, cx)
        {
            return;
        }
        let Some(group) = group.and_then(|group| group.upgrade()) else {
            return;
        };
        group.update(cx, |group, cx| {
            let target = PanelId::from(panel);
            if let Some(ix) = group
                .panels()
                .iter()
                .position(|panel| panel.panel_id(cx) == target)
            {
                group.select_tab(ix, window, cx);
            }
        });
    }

    /// Close a host's terminal tab. Goes through the dock area rather
    /// than the tab group: the group refuses to close the last tab of the
    /// center, and here every tab is closable.
    fn on_close_terminal(
        &mut self,
        action: &CloseTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_terminal(action.0, true, window, cx);
    }

    /// Close a remote terminal tab, asking first while its 文件 transfers or
    /// has files open with changes not saved yet, unless that was asked.
    pub(super) fn close_terminal(
        &mut self,
        id: RemoteTerminalId,
        ask_about_unsaved: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(&sidebar) = self.file_sidebars.get(&id)
            && self.ask_before_closing(sidebar, ask_about_unsaved, true, window, cx)
        {
            return;
        }
        self.remove_terminal(id, window, cx);
    }

    fn remove_terminal(
        &mut self,
        id: RemoteTerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.terminals.get(&id).cloned() {
            self.remove_tab_panel(panel, window, cx);
        }
    }

    fn on_close_explorer(
        &mut self,
        action: &CloseExplorer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_explorer(action.0, true, window, cx);
    }

    /// Close an SFTP tab, asking first while it transfers, and while files
    /// it opened have changes not saved yet unless that was asked already.
    pub(super) fn close_explorer(
        &mut self,
        id: ExplorerId,
        ask_about_unsaved: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.explorers.contains_key(&id)
            && !self.ask_before_closing(id, ask_about_unsaved, false, window, cx)
        {
            self.remove_explorer(id, window, cx);
        }
    }

    /// Ask before closing an explorer that transfers, or whose files have
    /// changes not saved yet (unless that was asked already): an SFTP tab,
    /// or a terminal's 文件 as the terminal closes. Confirming sends the
    /// explorer `CloseConfirmed`. `false` when there is nothing to ask.
    fn ask_before_closing(
        &mut self,
        id: ExplorerId,
        ask_about_unsaved: bool,
        terminal: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(panel) = self.explorers.get(&id).cloned() else {
            return false;
        };
        let generation = panel.read(cx).generation();
        // Its remote files close with it: changes not saved yet would be
        // lost, so the question names them.
        let unsaved = if ask_about_unsaved {
            self.unsaved_files(&self.editors_of(id, cx), cx)
        } else {
            Vec::new()
        };
        let unsaved = (!unsaved.is_empty()).then(|| {
            let files = describe_files(&unsaved);
            if terminal {
                t!("workspace.unsaved.close_terminal", files = files)
            } else {
                t!("workspace.unsaved.close_sftp", files = files)
            }
            .to_string()
        });
        if panel.read(cx).is_transferring() {
            let direction = panel.read(cx).transfer_direction();
            confirm_close_transfer(
                id,
                generation,
                direction,
                unsaved,
                self.focus_handle.clone(),
                window,
                cx,
            );
        } else if let Some(unsaved) = unsaved {
            let dispatch = self.focus_handle.clone();
            confirm_danger(
                if terminal {
                    t!("workspace.unsaved.close_terminal_title")
                } else {
                    t!("workspace.unsaved.close_sftp_title")
                },
                Some(unsaved.into()),
                t!("workspace.unsaved.discard_and_close"),
                Rc::new(move |window, cx| {
                    dispatch.dispatch_explorer_action(
                        &ExplorerAction::new(id, ExplorerCommand::CloseConfirmed)
                            .with_generation(generation),
                        window,
                        cx,
                    )
                }),
                window,
                cx,
            );
        } else {
            return false;
        }
        true
    }

    /// Close an explorer the user agreed to close. A terminal's 文件 was
    /// asked about as its terminal closed: the terminal goes, and it with it.
    fn remove_explorer(&mut self, id: ExplorerId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(terminal) = self.sidebar_terminal(id) {
            self.remove_terminal(terminal, window, cx);
            return;
        }
        if let Some(panel) = self.explorers.get(&id).cloned() {
            self.cancel_prompts_for_owner(
                PromptOwner::Sftp(id, panel.read(cx).generation()),
                window,
                cx,
            );
            self.remove_tab_panel(panel, window, cx);
        }
    }
    fn on_explorer_action(
        &mut self,
        action: &ExplorerAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.explorers.get(&action.explorer()).cloned() else {
            return;
        };
        if !matches!(action.command(), ExplorerCommand::CancelTransfer)
            && !self.remote_host_allowed(panel.read(cx).host_id(), cx)
        {
            window.push_notification(Notification::info(t!("remote.connect_first")), cx);
            return;
        }
        if action
            .generation()
            .is_some_and(|g| g != panel.read(cx).generation())
        {
            return;
        }
        if matches!(action.command(), ExplorerCommand::CancelTransfer) {
            self.cancel_prompts_for_owner(
                PromptOwner::Sftp(action.explorer(), panel.read(cx).generation()),
                window,
                cx,
            );
        }
        // A file open in an editor already shows that editor: no second read,
        // no second tab.
        if let ExplorerCommand::Edit { remote, path } = action.command()
            && let Some(location) = panel.read(cx).edit_target(*remote, path.as_deref(), cx)
            && let Some(editor) = self.editor_for_location(action.explorer(), &location, cx)
        {
            self.show_editor(&editor, window, cx);
            return;
        }
        match action.command() {
            ExplorerCommand::Enqueue(job) => self.count(match job.direction() {
                TransferDirection::Upload => Counter::Upload,
                TransferDirection::Download => Counter::Download,
            }),
            // The sidebar's 文件 has no 查找文件 to count.
            ExplorerCommand::FindFiles
                if panel.read(cx).connection_state() == ConnectionState::Connected
                    && !panel.read(cx).is_sidebar() =>
            {
                self.count(Counter::SftpFind)
            }
            ExplorerCommand::Preview { remote, path } => {
                if let Some((_, kind)) = panel.read(cx).preview_target(*remote, path.as_deref(), cx)
                {
                    self.count(match kind {
                        PreviewKind::Image(_) => Counter::PreviewImage,
                        PreviewKind::Markdown => Counter::PreviewMarkdown,
                    });
                }
            }
            _ => {}
        }
        if matches!(action.command(), ExplorerCommand::CloseConfirmed) {
            self.remove_explorer(action.explorer(), window, cx);
        } else {
            panel.update(cx, |panel, cx| panel.execute(action.command(), window, cx));
        }
    }
    /// File-list shortcuts go to the explorer holding focus, not the tab
    /// activated last: two SFTP tabs can sit side by side in split groups.
    fn on_explorer_shortcut(
        &mut self,
        action: &ExplorerShortcut,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = self
            .explorers
            .iter()
            .find(|(_, panel)| panel.read(cx).contains_focus(window, cx))
            .map(|(id, _)| *id);
        if let Some(id) = focused {
            self.on_explorer_action(&ExplorerAction::new(id, action.0.clone()), window, cx);
        }
    }

    pub(super) fn on_close_local_terminal(
        &mut self,
        action: &CloseLocalTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.local_terminals.get(&action.0).cloned() {
            self.remove_tab_panel(panel, window, cx);
        }
    }

    fn on_close_active_tab(
        &mut self,
        _: &CloseActiveTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.tab_in_window(window) {
            self.close_center_tab(tab, window, cx);
        }
    }

    /// Close one center tab through its own close path, so an explorer that
    /// is uploading still asks first.
    pub(super) fn close_center_tab(
        &mut self,
        tab: CenterTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match tab {
            CenterTab::Terminal(id) => self.on_close_terminal(&CloseTerminal(id), window, cx),
            CenterTab::Explorer(id) => self.on_close_explorer(&CloseExplorer(id), window, cx),
            CenterTab::LocalTerminal(id) => {
                self.on_close_local_terminal(&CloseLocalTerminal(id), window, cx)
            }
            CenterTab::Settings => self.on_close_settings(&CloseSettings, window, cx),
            CenterTab::Editor(id) => self.on_close_editor(&CloseEditor(id), window, cx),
        }
    }

    /// The tab group holding a center tab, and the tab's panel id in it.
    pub(super) fn center_tab_location(
        &self,
        tab: CenterTab,
        cx: &App,
    ) -> Option<(Entity<TabGroup>, PanelId)> {
        let (group, entity) = match tab {
            CenterTab::Terminal(id) => {
                let panel = self.terminals.get(&id)?;
                (panel.read(cx).tab_group(), panel.entity_id())
            }
            CenterTab::Explorer(id) => {
                let panel = self.explorers.get(&id)?;
                (panel.read(cx).tab_group(), panel.entity_id())
            }
            CenterTab::LocalTerminal(id) => {
                let panel = self.local_terminals.get(&id)?;
                (panel.read(cx).tab_group(), panel.entity_id())
            }
            CenterTab::Settings => {
                let panel = self.settings_tab.as_ref()?;
                (panel.read(cx).tab_group(), panel.entity_id())
            }
            CenterTab::Editor(id) => {
                let panel = self.editors.get(&id)?;
                (panel.read(cx).tab_group(), panel.entity_id())
            }
        };
        Some((group?.upgrade()?, PanelId::from(entity)))
    }

    /// The center tab a dock panel stands for, if it is one of ours.
    pub(super) fn center_tab_for_panel(&self, panel: PanelId) -> Option<CenterTab> {
        let is = |entity: EntityId| PanelId::from(entity) == panel;
        self.terminals
            .iter()
            .find(|(_, entity)| is(entity.entity_id()))
            .map(|(id, _)| CenterTab::Terminal(*id))
            .or_else(|| {
                self.explorers
                    .iter()
                    .find(|(_, entity)| is(entity.entity_id()))
                    .map(|(id, _)| CenterTab::Explorer(*id))
            })
            .or_else(|| {
                self.local_terminals
                    .iter()
                    .find(|(_, entity)| is(entity.entity_id()))
                    .map(|(id, _)| CenterTab::LocalTerminal(*id))
            })
            .or_else(|| {
                self.settings_tab
                    .as_ref()
                    .filter(|panel| is(panel.entity_id()))
                    .map(|_| CenterTab::Settings)
            })
            .or_else(|| {
                self.editors
                    .iter()
                    .find(|(_, entity)| is(entity.entity_id()))
                    .map(|(id, _)| CenterTab::Editor(*id))
            })
    }

    fn on_close_tabs(&mut self, action: &CloseTabs, window: &mut Window, cx: &mut Context<Self>) {
        let Some((group, anchor)) = self.center_tab_location(action.tab, cx) else {
            return;
        };
        let panels: Vec<PanelId> = group
            .read(cx)
            .panels()
            .iter()
            .map(|panel| panel.panel_id(cx))
            .collect();
        let Some(ix) = panels.iter().position(|panel| *panel == anchor) else {
            return;
        };
        let tabs: Vec<CenterTab> = action
            .scope
            .targets(panels.len(), ix)
            .into_iter()
            .filter_map(|target| self.center_tab_for_panel(panels[target]))
            .collect();
        self.close_center_tabs(tabs, window, cx);
    }

    fn on_rename_terminal(
        &mut self,
        action: &RenameTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.terminals.get(&action.0).cloned() {
            open_rename_tab_dialog(panel, window, cx);
        }
    }

    fn on_rename_explorer(
        &mut self,
        action: &RenameExplorer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.explorers.get(&action.0).cloned() {
            open_rename_tab_dialog(panel, window, cx);
        }
    }

    fn on_copy_host_address(
        &mut self,
        action: &CopyHostAddress,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(host) = self
            .store
            .read(cx)
            .host(action.0)
            .map(|host| host.address.clone())
        else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(host.to_string()));
        window.push_notification(
            Notification::success(t!("workspace.copied", text = host)),
            cx,
        );
    }

    fn on_copy_host_id(
        &mut self,
        action: &CopyHostId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(public_id) = self
            .store
            .read(cx)
            .host(action.0)
            .map(|host| host.public_id.clone())
        else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(public_id.to_string()));
        window.push_notification(
            Notification::success(t!("workspace.copied_id", id = public_id)),
            cx,
        );
    }

    fn on_restart_local_terminal(
        &mut self,
        action: &RestartLocalTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.local_terminals.get(&action.0).cloned() {
            // A saved terminal starts again where its shell was last.
            let saved = self
                .saved_terminals
                .get(&action.0)
                .and_then(|id| self.store.read(cx).saved_terminal(*id))
                .cloned();
            let factory = if let Some(factory) = self.lan_local_factories.get(&action.0) {
                factory.clone()
            } else if panel.read(cx).is_command_terminal() {
                crate::terminal::command_terminal_factory()
            } else if let Some(launch) = self.local_launches.get(&action.0) {
                let mut launch = launch.clone();
                if launch.shell.cmder_directory().is_none() {
                    launch.shell = crate::terminal::LocalShell::new(
                        self.settings
                            .read(cx)
                            .settings()
                            .local_terminal
                            .shell
                            .as_deref()
                            .unwrap_or_default(),
                    );
                }
                self.local_terminal_provider.factory_for(&launch)
            } else {
                self.local_terminal_factory(saved.as_ref(), None, false, cx)
            };
            panel.update(cx, |panel, cx| panel.restart(factory, window, cx));
        }
    }

    /// A transport for a local terminal starting now: the shell
    /// 设置 › 终端 › 本地终端 names at this moment, in the folder a saved
    /// terminal was last in; opening (rather than restarting) it, drawing
    /// what it kept first.
    fn local_terminal_factory(
        &self,
        saved: Option<&SavedTerminal>,
        directory: Option<PathBuf>,
        opening: bool,
        cx: &App,
    ) -> SharedTerminalTransportFactory {
        let settings = self.settings.read(cx).settings();
        let replay = saved.filter(|_| opening).and_then(|saved| {
            super::projects::kept_replay(self.store.read(cx), &settings, saved.id)
        });
        let mut launch = super::projects::local_launch(&settings, saved, replay);
        if directory.is_some() {
            launch.directory = directory;
        }
        self.local_terminal_provider.factory_for(&launch)
    }

    /// The terminal of the center tab shown most recently, remote or local.
    fn tab_in_window(&self, window: &Window) -> Option<CenterTab> {
        self.detached_windows
            .iter()
            .find(|detached| detached.handle.window_id() == window.window_handle().window_id())
            .map(|detached| detached.tab)
            .or(self.active_tab)
    }

    fn active_terminal(&self, window: &Window, cx: &App) -> Option<Entity<TerminalView>> {
        match self.tab_in_window(window)? {
            CenterTab::Terminal(id) => Some(self.terminals.get(&id)?.read(cx).terminal().clone()),
            CenterTab::LocalTerminal(id) => {
                Some(self.local_terminals.get(&id)?.read(cx).terminal().clone())
            }
            CenterTab::Explorer(_) | CenterTab::Settings | CenterTab::Editor(_) => None,
        }
    }

    fn on_copy_terminal(&mut self, _: &CopyTerminal, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(terminal) = self.active_terminal(window, cx) {
            terminal.update(cx, |terminal, cx| terminal.copy_selection(cx));
        }
    }

    fn on_paste_terminal(
        &mut self,
        _: &PasteTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = self.active_terminal(window, cx) {
            terminal.update(cx, |terminal, cx| terminal.paste_clipboard(cx));
        }
    }

    fn on_select_all_terminal(
        &mut self,
        _: &SelectAllTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = self.active_terminal(window, cx) {
            terminal.update(cx, |terminal, cx| terminal.select_all(cx));
        }
    }

    fn on_find_in_terminal(
        &mut self,
        _: &FindInTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = self.active_terminal(window, cx) {
            terminal.update(cx, |terminal, cx| terminal.open_find(window, cx));
            self.count(Counter::Find);
        }
    }

    fn on_find_next_in_terminal(
        &mut self,
        _: &FindNextInTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = self.active_terminal(window, cx) {
            terminal.update(cx, |terminal, cx| {
                terminal.step_find(SearchDirection::Down, cx)
            });
        }
    }

    fn on_find_previous_in_terminal(
        &mut self,
        _: &FindPreviousInTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = self.active_terminal(window, cx) {
            terminal.update(cx, |terminal, cx| {
                terminal.step_find(SearchDirection::Up, cx)
            });
        }
    }

    fn on_dismiss_terminal_find(
        &mut self,
        _: &DismissTerminalFind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = self.active_terminal(window, cx) {
            terminal.update(cx, |terminal, cx| terminal.dismiss_find(window, cx));
        }
    }

    fn on_toggle_terminal_find_option(
        &mut self,
        action: &ToggleTerminalFindOption,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = self.active_terminal(window, cx) {
            terminal.update(cx, |terminal, cx| {
                terminal.toggle_find_option(action.0, window, cx)
            });
        }
    }

    fn on_copy_terminal_find_results(
        &mut self,
        _: &CopyTerminalFindResults,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = self.active_terminal(window, cx) {
            terminal.update(cx, |terminal, cx| terminal.copy_find_results(cx));
        }
    }

    fn on_clear_terminal(
        &mut self,
        _: &ClearTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = self.active_terminal(window, cx) {
            terminal.update(cx, |terminal, cx| terminal.clear(cx));
        }
    }

    fn on_send_break(&mut self, action: &SendBreak, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(terminal) = self.terminals.get(&action.0) {
            terminal.read(cx).send_break(cx);
        }
    }

    fn on_send_serial_hex(
        &mut self,
        action: &SendSerialHex,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = self.terminals.get(&action.0).cloned() {
            terminal.update(cx, |terminal, cx| terminal.send_hex(window, cx));
        }
    }

    fn toggle_serial_mode(&mut self, id: RemoteTerminalId, receive: bool, cx: &mut Context<Self>) {
        self.update_serial_modes(
            id,
            |modes| {
                if receive {
                    modes.receive_hex = !modes.receive_hex;
                } else {
                    modes.send_hex = !modes.send_hex;
                }
            },
            cx,
        );
    }

    fn update_serial_modes(
        &mut self,
        id: RemoteTerminalId,
        update: impl FnOnce(&mut crate::terminal::SerialDataModes),
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.terminals.get(&id).cloned() else {
            return;
        };
        let host = panel.read(cx).host();
        if host.kind() != crate::host::ProtocolKind::Serial {
            return;
        }
        let host_id = host.id;
        let public_id = host.public_id.as_str().to_owned();
        let modes = panel.update(cx, |panel, cx| {
            let mut modes = panel.serial_modes(cx);
            update(&mut modes);
            panel.set_serial_modes(modes, cx);
            modes
        });
        if !self.store.read(cx).is_temporary(host_id) {
            self.settings.update(cx, |store, cx| {
                store.update(
                    |settings| settings.serial_preferences.set(public_id, modes),
                    cx,
                )
            });
        }
    }

    fn on_toggle_serial_receive_hex(
        &mut self,
        action: &ToggleSerialReceiveHex,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_serial_mode(action.0, true, cx);
    }

    fn on_toggle_serial_send_hex(
        &mut self,
        action: &ToggleSerialSendHex,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_serial_mode(action.0, false, cx);
    }

    fn on_set_serial_backspace(
        &mut self,
        action: &SetSerialBackspace,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let choice = action.0;
        self.update_serial_modes(choice.terminal, |modes| modes.backspace = choice.key, cx);
    }

    fn on_start_terminal_recording(
        &mut self,
        action: &StartTerminalRecording,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = self.terminals.get(&action.0).cloned() {
            terminal.update(cx, |terminal, cx| terminal.start_recording(window, cx));
        }
    }

    fn on_stop_terminal_recording(
        &mut self,
        action: &StopTerminalRecording,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = self.terminals.get(&action.0).cloned() {
            terminal.update(cx, |terminal, cx| terminal.stop_recording(cx));
        }
    }

    fn on_clear_terminal_log(
        &mut self,
        action: &ClearTerminalLog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = self.terminals.get(&action.0).cloned() {
            terminal.update(cx, |terminal, cx| terminal.clear_log(window, cx));
        }
    }

    fn on_delete_terminal_log(
        &mut self,
        action: &DeleteTerminalLog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = self.terminals.get(&action.0).cloned() {
            terminal.update(cx, |terminal, cx| terminal.delete_log(window, cx));
        }
    }

    fn on_disconnect_terminal(
        &mut self,
        action: &DisconnectTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        self.cancel_prompts_for_terminal(id, window, cx);
        if let Some(terminal) = self.terminals.get(&id).cloned() {
            terminal.update(cx, |terminal, cx| terminal.disconnect(cx));
            self.disconnect_file_sidebar(id, window, cx);
        }
    }

    /// A terminal's 文件 goes down with it at once, rather than finding out
    /// when its channel ends and then retrying for a while.
    fn disconnect_file_sidebar(
        &mut self,
        terminal: RemoteTerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.file_sidebar(terminal).cloned() {
            panel.update(cx, |panel, cx| panel.disconnect(window, cx));
        }
    }

    fn on_reconnect_terminal(
        &mut self,
        action: &ReconnectTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        self.cancel_prompts_for_terminal(id, window, cx);
        if let Some(terminal) = self.terminals.get(&id).cloned() {
            if !self.remote_host_allowed(terminal.read(cx).host_id(), cx) {
                window.push_notification(Notification::info(t!("remote.connect_first")), cx);
                return;
            }
            if let Some((server, _, device, user)) =
                self.remote_hosts.get(&terminal.read(cx).host_id()).cloned()
            {
                self.remote.update(cx, |service, cx| {
                    service.run(
                        crate::remote::Operation::Resolve(server, device, user, Some(id.0)),
                        cx,
                    )
                });
                return;
            }
            terminal.update(cx, |terminal, cx| terminal.reconnect(window, cx));
        }
    }

    fn on_new_host(&mut self, _: &NewHost, window: &mut Window, cx: &mut Context<Self>) {
        open_host_dialog(
            None,
            false,
            None,
            self.store.clone(),
            self.connection_tester.clone(),
            window,
            cx,
        );
    }

    fn on_new_temporary_connection(
        &mut self,
        _: &NewTemporaryConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_temporary_connection_dialog(
            self.store.clone(),
            self.connection_tester.clone(),
            self.focus_handle.clone(),
            window,
            cx,
        );
    }

    fn on_quick_connect(&mut self, _: &QuickConnect, window: &mut Window, cx: &mut Context<Self>) {
        open_quick_connect_dialog(self.store.clone(), self.focus_handle.clone(), window, cx);
    }

    fn on_edit_host(&mut self, action: &EditHost, window: &mut Window, cx: &mut Context<Self>) {
        // A host opened from a link is not saved, so there is nothing to
        // edit.
        if self
            .store
            .read(cx)
            .hosts()
            .iter()
            .any(|host| host.id == action.0)
        {
            open_host_dialog(
                Some(action.0),
                self.has_tabs(action.0, cx),
                None,
                self.store.clone(),
                self.connection_tester.clone(),
                window,
                cx,
            );
        }
    }

    fn on_duplicate_host(
        &mut self,
        action: &DuplicateHost,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let copy = self
            .store
            .update(cx, |store, cx| store.duplicate(action.0, cx));
        if let Some(copy) = copy {
            self.host_panel
                .update(cx, |panel, cx| panel.select_node(HostNode::Host(copy), cx));
        }
    }

    fn on_move_host_node(&mut self, action: &MoveHostNode, _: &mut Window, cx: &mut Context<Self>) {
        let moved = self.store.update(cx, |store, cx| {
            store.move_node(action.source, action.destination, cx)
        });
        if moved {
            self.host_panel
                .update(cx, |panel, cx| panel.reveal_node(action.source, cx));
        }
    }

    fn on_automatically_order_hosts(
        &mut self,
        action: &crate::app::AutomaticallyOrderHosts,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.store.update(cx, |store, cx| {
            store.automatically_order_hosts(action.parent, cx);
        });
    }

    fn on_delete_host(&mut self, action: &DeleteHost, window: &mut Window, cx: &mut Context<Self>) {
        let id = action.0;
        // Saved hosts only: one opened from a link goes with its tabs.
        let Some(host) = self
            .store
            .read(cx)
            .hosts()
            .iter()
            .find(|host| host.id == id)
            .cloned()
        else {
            return;
        };
        let workspace = cx.entity().downgrade();
        let store = self.store.read(cx);
        let dependents = Dependents {
            forwards: store.forwards_of(id).count(),
            jump_users: store.jump_users(&[id]),
        };
        confirm_delete_host(
            &host,
            dependents,
            Rc::new(move |_, cx| {
                workspace
                    .update(cx, |this, cx| this.remove_host(id, cx))
                    .ok();
            }),
            window,
            cx,
        );
    }

    /// Its open tabs stay: each has the host as it took it.
    fn remove_host(&mut self, id: HostId, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| {
            store.remove(id, cx);
        });
    }

    fn on_new_host_in_group(
        &mut self,
        action: &NewHostInGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_host_dialog(
            None,
            false,
            Some(action.0),
            self.store.clone(),
            self.connection_tester.clone(),
            window,
            cx,
        );
    }

    fn on_new_group(&mut self, _: &NewGroup, window: &mut Window, cx: &mut Context<Self>) {
        open_group_dialog(None, None, self.store.clone(), window, cx);
    }

    fn on_expand_all_groups(
        &mut self,
        _: &ExpandAllGroups,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_all_groups_expanded(true, cx);
    }

    fn on_collapse_all_groups(
        &mut self,
        _: &CollapseAllGroups,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_all_groups_expanded(false, cx);
    }

    fn set_all_groups_expanded(&mut self, expanded: bool, cx: &mut Context<Self>) {
        self.store
            .update(cx, |store, cx| store.set_all_groups_expanded(expanded, cx));
        self.host_panel
            .update(cx, |panel, cx| panel.set_all_groups_expanded(expanded, cx));
    }

    fn on_new_child_group(
        &mut self,
        action: &NewChildGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.read(cx).group(action.0).is_some() {
            open_group_dialog(None, Some(action.0), self.store.clone(), window, cx);
        }
    }

    fn on_rename_group(
        &mut self,
        action: &RenameGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.read(cx).group(action.0).is_some() {
            open_group_dialog(Some(action.0), None, self.store.clone(), window, cx);
        }
    }

    fn on_delete_group(
        &mut self,
        action: &DeleteGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        let store = self.store.read(cx);
        let Some(group) = store.group(id) else {
            return;
        };
        let name = group.name.to_string();
        let subgroups = store.descendant_groups(id).len();
        let doomed = store.hosts_under(id);
        let dependents = Dependents {
            forwards: doomed.iter().map(|id| store.forwards_of(*id).count()).sum(),
            jump_users: store.jump_users(&doomed),
        };
        let workspace = cx.entity().downgrade();
        confirm_delete_group(
            &name,
            doomed.len(),
            subgroups,
            dependents,
            Rc::new(move |_, cx| {
                workspace
                    .update(cx, |this, cx| this.remove_group(id, cx))
                    .ok();
            }),
            window,
            cx,
        );
    }

    /// The store cascades the delete; the hosts' open tabs stay.
    fn remove_group(&mut self, id: GroupId, cx: &mut Context<Self>) {
        self.store
            .update(cx, |store, cx| store.remove_group(id, cx));
    }

    fn on_toggle_host_panel(
        &mut self,
        _: &ToggleHostPanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dock_area.update(cx, |area, cx| {
            area.toggle_dock(DockPlacement::Left, window, cx);
        });
        cx.notify();
    }

    fn on_maximize_window(
        &mut self,
        _: &MaximizeWindow,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        if !window.is_maximized() {
            window.zoom_window();
        }
    }

    fn on_restore_window(&mut self, _: &RestoreWindow, window: &mut Window, _: &mut Context<Self>) {
        if window.is_maximized() {
            crate::app::restore_window(window);
        }
    }

    /// 显示或隐藏 ShellRS: in front of other apps, or away when it already
    /// is.
    pub(super) fn on_show_or_hide_app(
        &mut self,
        _: &ShowOrHideApp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.is_window_active() {
            crate::app::send_back(window, cx);
        } else {
            crate::app::bring_forward(window, cx);
        }
    }

    /// The title bar's quick switch: it picks 应用外观 outright, leaving
    /// 跟随系统 if that was chosen, and the settings observer applies it.
    fn on_toggle_theme(&mut self, _: &ToggleTheme, _: &mut Window, cx: &mut Context<Self>) {
        let appearance = if cx.theme().is_dark() {
            Appearance::Light
        } else {
            Appearance::Dark
        };
        self.settings.update(cx, |settings, cx| {
            settings.update(|settings| settings.appearance = appearance, cx)
        });
    }

    /// Saved like any setting; applying it redraws every SFTP tab.
    fn on_set_file_size_format(
        &mut self,
        action: &SetFileSizeFormat,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let format = action.0;
        self.settings.update(cx, |settings, cx| {
            settings.update(|settings| settings.file_size_format = format, cx)
        });
    }

    /// Saved like any setting; applying it lists that side of every SFTP
    /// tab again.
    fn on_toggle_hidden_files(
        &mut self,
        action: &ToggleHiddenFiles,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let side = action.0;
        self.settings.update(cx, |settings, cx| {
            settings.update(|settings| settings.show_hidden.toggle(side), cx)
        });
    }

    fn on_toggle_host_details(
        &mut self,
        _: &ToggleHostDetails,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.update(cx, |store, cx| {
            store.update(
                |settings| settings.host_details = !settings.host_details,
                cx,
            )
        });
    }

    fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        if !self.dock_area.read(cx).is_dock_open(DockPlacement::Left) {
            self.dock_area.update(cx, |area, cx| {
                area.toggle_dock(DockPlacement::Left, window, cx);
            });
        }
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.focus_search(window, cx));
    }

    /// The interface larger or smaller, through the settings (外观 › 界面字号),
    /// which keep it in range and for the next launch; applying them sizes
    /// the interface.
    fn change_interface_font_size(&mut self, size: impl FnOnce(f32) -> f32, cx: &mut App) {
        self.settings.update(cx, |store, cx| {
            store.update(
                |settings| settings.interface.font_size = size(settings.interface.font_size),
                cx,
            )
        });
    }

    fn on_zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        self.change_interface_font_size(|size| size + INTERFACE_ZOOM_STEP, cx);
    }

    fn on_zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        self.change_interface_font_size(|size| size - INTERFACE_ZOOM_STEP, cx);
    }

    fn on_zoom_reset(&mut self, _: &ZoomReset, _: &mut Window, cx: &mut Context<Self>) {
        self.change_interface_font_size(|_| DEFAULT_INTERFACE_FONT_SIZE, cx);
    }

    /// The terminals' text a pixel larger or smaller, through the settings
    /// (终端 › 字号), which keep it in range and for the next launch.
    fn change_terminal_font_size(&mut self, size: impl FnOnce(f32) -> f32, cx: &mut App) {
        self.settings.update(cx, |store, cx| {
            store.update(
                |settings| settings.terminal_font.size = size(settings.terminal_font.size),
                cx,
            )
        });
    }

    fn on_zoom_terminal_in(&mut self, _: &ZoomTerminalIn, _: &mut Window, cx: &mut Context<Self>) {
        self.change_terminal_font_size(|size| size + 1., cx);
    }

    fn on_zoom_terminal_out(
        &mut self,
        _: &ZoomTerminalOut,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_terminal_font_size(|size| size - 1., cx);
    }

    fn on_zoom_terminal_reset(
        &mut self,
        _: &ZoomTerminalReset,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_terminal_font_size(|_| DEFAULT_FONT_SIZE, cx);
    }
}

fn new_terminal_panel(
    store: &Entity<HostStore>,
    transports: HostTerminalTransports,
    terminal_id: RemoteTerminalId,
    host_id: HostId,
    copy: Option<TerminalCopy>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> (Entity<TerminalPanel>, Subscription) {
    let panel = cx.new(|cx| match copy {
        Some(copy) => {
            TerminalPanel::from_copy(terminal_id, copy, store.clone(), transports, window, cx)
        }
        None => TerminalPanel::new(terminal_id, host_id, store.clone(), transports, window, cx),
    });
    let host = panel.read(cx).host();
    let shared_serial = host.address.starts_with("lan-device:");
    let key = if shared_serial {
        host.address.to_string()
    } else {
        format!("host:{}", host.public_id)
    };
    let timestamp_owner = (!shared_serial).then_some(host_id);
    panel.read(cx).terminal().clone().update(cx, |view, cx| {
        view.bind_timestamps(store.clone(), key, timestamp_owner, cx)
    });
    let subscription = cx.subscribe_in(
        &panel,
        window,
        |this, _, event: &TerminalPanelEvent, window, cx| match event {
            TerminalPanelEvent::Input(id, input) => {
                this.broadcast_input(CenterTab::Terminal(*id), input, cx);
            }
            TerminalPanelEvent::Activated(terminal_id, _) => {
                this.set_active_tab(Some(CenterTab::Terminal(*terminal_id)), window, cx)
            }
            TerminalPanelEvent::Closed(terminal_id, host_id) => {
                this.finish_detached_tab(CenterTab::Terminal(*terminal_id), cx);
                this.cancel_prompts_for_terminal(*terminal_id, window, cx);
                this.terminals.remove(terminal_id);
                this.tools
                    .update(cx, |tools, cx| tools.forget_log_terminal(*terminal_id, cx));
                this.close_file_sidebar(*terminal_id, window, cx);
                this.directory_follow.remove(terminal_id);
                this.sync_broadcast(cx);
                this.sync_compose(window, cx);
                if this.active_tab == Some(CenterTab::Terminal(*terminal_id)) {
                    this.set_active_tab(None, window, cx);
                }
                this.forget_unused_temporary_host(*host_id, window, cx);
            }
            TerminalPanelEvent::StatusChanged(..) => {
                this.sync_broadcast(cx);
                this.sync_compose(window, cx);
                cx.notify();
            }
            TerminalPanelEvent::Connected(terminal_id, host_id) => {
                this.count_connection(*host_id, false, cx);
                this.store
                    .update(cx, |store, cx| store.record_connection(*host_id, cx));
                // A Telnet or serial terminal has no 文件 beside it.
                if this
                    .terminals
                    .get(terminal_id)
                    .is_none_or(|panel| !panel.read(cx).host().is_ssh())
                {
                    this.sync_tool_sidebar(window, cx);
                    return;
                }
                // A new connection, maybe to another machine: its folder is
                // what it reports from now on, and it is checked afresh,
                // first, so the check runs before 文件's channel opens.
                if let Some(panel) = this.file_sidebar(*terminal_id).cloned() {
                    panel.update(cx, |panel, _| panel.forget_follow_target());
                }
                if this.tool_showing(cx) == Some(ToolKind::Files)
                    && this.tool_terminal(cx).map(|shown| shown.id) == Some(*terminal_id)
                {
                    this.check_directory_follow(*terminal_id, window, cx);
                }
                // 文件 comes back with its terminal: a channel on the new
                // connection, whatever the old one's state.
                if let Some(panel) = this.file_sidebar(*terminal_id).cloned()
                    && panel.read(cx).can_reconnect()
                {
                    let id = panel.read(cx).id();
                    this.on_explorer_action(
                        &ExplorerAction::new(id, ExplorerCommand::Reconnect),
                        window,
                        cx,
                    );
                }
            }
            TerminalPanelEvent::PromptRequested(terminal_id, host_id, prompt) => this
                .enqueue_prompt(
                    PromptOwner::Terminal(*terminal_id),
                    *host_id,
                    prompt.clone(),
                    window,
                    cx,
                ),
            TerminalPanelEvent::HostOsDetected(host_id, os) => {
                let (host_id, os) = (*host_id, *os);
                this.store
                    .update(cx, |store, cx| store.set_host_os(host_id, Some(os), cx));
            }
            TerminalPanelEvent::Notice(terminal_id, notice) => {
                this.on_terminal_notice(CenterTab::Terminal(*terminal_id), notice, window, cx)
            }
            TerminalPanelEvent::DirectoryChanged(terminal_id, directory) => {
                this.follow_directory(*terminal_id, directory, window, cx)
            }
            TerminalPanelEvent::ZmodemAsk(terminal_id, direction) => {
                this.ask_zmodem(CenterTab::Terminal(*terminal_id), *direction, window, cx)
            }
            TerminalPanelEvent::FilesDropped(terminal_id, files) => {
                this.upload_dropped(*terminal_id, files, window, cx)
            }
        },
    );
    (panel, subscription)
}

struct AuthenticationPromptForm {
    fields: Vec<(ConnectionPromptField, Entity<InputState>)>,
    password_picker: Option<Entity<crate::shared::SavedPasswordPicker>>,
}

impl AuthenticationPromptForm {
    fn new(
        fields: Vec<ConnectionPromptField>,
        passwords: Option<Entity<HostStore>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let fields: Vec<_> = fields
            .into_iter()
            .map(|field| {
                let echo = field.echo();
                let input = cx.new(|cx| InputState::new(window, cx).masked(!echo));
                (field, input)
            })
            .collect();
        let password_picker = passwords.and_then(|store| {
            fields.first().map(|(_, input)| {
                cx.new(|cx| {
                    crate::shared::SavedPasswordPicker::new(store, input.clone(), window, cx)
                })
            })
        });
        Self {
            fields,
            password_picker,
        }
    }

    fn take_answers(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Vec<ConnectionSecret>> {
        if self
            .password_picker
            .as_ref()
            .and_then(|picker| picker.read(cx).problem())
            .is_some()
        {
            return None;
        }
        if let Some(picker) = &self.password_picker {
            picker.update(cx, |picker, _| picker.cancel());
        }
        Some(
            self.fields
                .iter()
                .map(|(_, input)| {
                    let answer = ConnectionSecret::new(input.read(cx).value().to_string());
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                    answer
                })
                .collect(),
        )
    }
}

impl Render for AuthenticationPromptForm {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Form::new()
            .children(
                self.fields
                    .iter()
                    .enumerate()
                    .map(|(index, (field, input))| {
                        let mut input = Input::new(input).id(("ssh-auth-answer", index)).small();
                        if !field.echo() {
                            input = input.content_type(InputContentType::Password).mask_toggle();
                        }
                        Field::new().label(field.label().to_string()).child(input)
                    }),
            )
            .children(
                self.password_picker
                    .clone()
                    .map(|picker| Field::new().child(picker)),
            )
    }
}

/// An SFTP tab, or a terminal's 文件, for `host` as it is now, logging in
/// with `login`.
fn new_explorer_panel(
    workspace: &Workspace,
    id: ExplorerId,
    host: Host,
    login: HostLogin,
    route: SftpRoute,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> (Entity<ExplorerPanel>, Subscription) {
    let panel = cx.new(|cx| {
        ExplorerPanel::new(
            id,
            host,
            login,
            workspace.store.clone(),
            workspace.settings.clone(),
            workspace.sftp_provider.clone(),
            route,
            workspace.local_directory_provider.clone(),
            workspace.focus_handle.clone(),
            window,
            cx,
        )
    });
    let subscription = cx.subscribe_in(
        &panel,
        window,
        |this, _, event: &ExplorerPanelEvent, window, cx| match event {
            ExplorerPanelEvent::Activated(id) => {
                this.set_active_tab(Some(CenterTab::Explorer(*id)), window, cx)
            }
            ExplorerPanelEvent::Closed(id, host_id) => {
                this.finish_detached_tab(CenterTab::Explorer(*id), cx);
                // Its remote files can no longer be written: their editors
                // go with it (anything unsaved was asked about first).
                for editor in this.editors_of(*id, cx) {
                    this.remove_editor(editor, window, cx);
                }
                this.explorers.remove(id);
                if this.active_tab == Some(CenterTab::Explorer(*id)) {
                    this.set_active_tab(None, window, cx);
                }
                let sidebars = this.file_sidebars.len();
                this.file_sidebars.retain(|_, sidebar| sidebar != id);
                if this.file_sidebars.len() != sidebars {
                    // The right sidebar lets go of it.
                    this.sync_tool_sidebar(window, cx);
                }
                this.forget_unused_temporary_host(*host_id, window, cx);
            }
            ExplorerPanelEvent::StateChanged(id, host_id) => {
                if let Some(panel) = this.explorers.get(id) {
                    let generation = panel.read(cx).generation();
                    this.prompt_queue.retain(|(owner,_,_)| !matches!(owner,PromptOwner::Sftp(s,g) if s == id && *g != generation));
                    if let Some((owner @ PromptOwner::Sftp(s,g),_,_)) = this.active_prompt && s == *id && g != generation {
                        this.cancel_prompts_for_owner(owner,window,cx);
                    }
                }
                // Only connecting, connected and disconnected change it. A
                // terminal's 文件 logs in nowhere: `tool_files` counts it.
                if this.explorers.get(id).is_some_and(|panel| {
                    let panel = panel.read(cx);
                    panel.connection_state() == ConnectionState::Connected && !panel.is_sidebar()
                }) {
                    this.count_connection(*host_id, true, cx);
                    this.store
                        .update(cx, |store, cx| store.record_connection(*host_id, cx));
                }
                let id = *id;
                cx.defer_in(window, move |this, window, cx| this.fallback_serial_network(id, window, cx));
            },
            ExplorerPanelEvent::StatusChanged(id) => {
                if this.active_tab == Some(CenterTab::Explorer(*id)) {
                    cx.notify();
                }
            }
            ExplorerPanelEvent::PromptRequested(id, host_id, generation, prompt) => this.enqueue_prompt(
                PromptOwner::Sftp(*id, *generation),
                *host_id,
                prompt.clone(),
                window,
                cx,
            ),
            ExplorerPanelEvent::EditFile(id, location, file) => {
                this.open_editor(*id, location.clone(), file.clone(), window, cx)
            }
        },
    );
    (panel, subscription)
}

fn new_local_terminal_panel(
    id: LocalTerminalId,
    factory: SharedTerminalTransportFactory,
    name: Option<SavedName>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> (Entity<LocalTerminalPanel>, Subscription) {
    let panel = cx.new(|cx| LocalTerminalPanel::new(id, factory, name, window, cx));
    let subscription = cx.subscribe_in(
        &panel,
        window,
        |this, panel, event: &LocalTerminalPanelEvent, window, cx| match event {
            LocalTerminalPanelEvent::ConnectSsh(id, command) => {
                if !panel.read(cx).is_command_terminal() {
                    return;
                }
                let host = this.store.update(cx, |store, cx| {
                    store.insert_temporary(command.draft(), None, cx)
                });
                this.connect_host(host, window, cx);
                let id = *id;
                cx.defer_in(window, move |this, window, cx| {
                    this.on_close_local_terminal(&CloseLocalTerminal(id), window, cx);
                });
            }
            LocalTerminalPanelEvent::Input(id, input) => {
                this.broadcast_input(CenterTab::LocalTerminal(*id), input, cx);
            }
            LocalTerminalPanelEvent::Activated(id) => {
                this.set_active_tab(Some(CenterTab::LocalTerminal(*id)), window, cx);
                cx.notify();
            }
            LocalTerminalPanelEvent::Closed(id) => {
                this.finish_detached_tab(CenterTab::LocalTerminal(*id), cx);
                this.keep_what_it_shows(*id, panel, cx);
                this.local_terminals.remove(id);
                this.lan_local_factories.remove(id);
                this.local_launches.remove(id);
                this.saved_terminals.remove(id);
                if this.front_saved_terminal == Some(*id) {
                    this.front_saved_terminal = None;
                }
                this.sync_broadcast(cx);
                this.sync_compose(window, cx);
                if this.active_tab == Some(CenterTab::LocalTerminal(*id)) {
                    this.set_active_tab(None, window, cx);
                }
                this.sync_saved_terminals(cx);
                cx.notify();
            }
            LocalTerminalPanelEvent::StatusChanged(id) => {
                this.on_local_terminal_retitled(*id, cx);
                this.sync_broadcast(cx);
                this.sync_compose(window, cx);
                cx.notify();
            }
            LocalTerminalPanelEvent::WorkingDirectory(id, folder) => {
                this.on_local_terminal_moved(*id, folder, cx);
            }
            LocalTerminalPanelEvent::Notice(id, notice) => {
                this.on_terminal_notice(CenterTab::LocalTerminal(*id), notice, window, cx)
            }
            LocalTerminalPanelEvent::ZmodemAsk(id, direction) => {
                this.ask_zmodem(CenterTab::LocalTerminal(*id), *direction, window, cx)
            }
        },
    );
    (panel, subscription)
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_window(None, window, cx)
    }
}

impl Workspace {
    fn on_lan_action(
        &mut self,
        action: &crate::app::LanAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let crate::lan::LanCommand::OpenDevice { peer, device } = &action.0 {
            self.open_lan_device(peer, device, window, cx);
            return;
        }
        match &action.0 {
            crate::lan::LanCommand::ShowPowerHelp => {
                crate::lan::show_power_help(window, cx);
                return;
            }
            crate::lan::LanCommand::OpenHost { peer, host } => {
                self.open_lan_host(peer, host, window, cx);
                return;
            }
            crate::lan::LanCommand::ManageAdb { peer, device } => {
                self.open_lan_adb(peer, device, false, window, cx);
                return;
            }
            crate::lan::LanCommand::AdbFiles { peer, device } => {
                self.open_lan_adb(peer, device, true, window, cx);
                return;
            }
            _ => {}
        }
        self.lan
            .update(cx, |lan, cx| lan.dispatch(action.0.clone(), cx));
    }

    fn open_lan_adb(
        &mut self,
        peer: &str,
        device: &str,
        files: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((core, target, owner)) = self
            .lan
            .read(cx)
            .device_target(peer, device)
            .filter(|(_, target, _)| target.kind == shellrs_lan::DeviceKind::Adb)
        else {
            return;
        };
        let result = crate::adb::AdbProxy::start(
            core,
            peer.to_owned(),
            device.to_owned(),
            target.address.clone(),
        )
        .and_then(|proxy| {
            crate::adb::AdbConnection::remote(
                target.address,
                format!("LAN · {owner} · {}", target.name),
                proxy,
            )
        });
        let connection = match result {
            Ok(connection) => connection,
            Err(error) => {
                window.push_notification(Notification::error(error.to_string()), cx);
                return;
            }
        };
        if !files {
            self.open_remote_android_manager(connection, window, cx);
            return;
        }
        let draft = crate::host::HostDraft::new(
            connection.display_name(),
            "lan-adb",
            22,
            "shell",
            crate::host::AuthKind::NoPassword,
            None,
        );
        let host_id = self
            .store
            .update(cx, |store, cx| store.insert_temporary(draft, None, cx));
        let store = self.store.read(cx);
        let host = store.host(host_id).unwrap().clone();
        let login = store.login(host_id).unwrap();
        let id = ExplorerId(self.next_explorer_id);
        self.next_explorer_id += 1;
        let (panel, subscription) = new_explorer_panel(
            self,
            id,
            host,
            login,
            SftpRoute::External(Arc::new(connection)),
            window,
            cx,
        );
        self._subscriptions.push(subscription);
        let focus = panel.read(cx).focus_handle(cx);
        self._subscriptions
            .push(follow_focus(CenterTab::Explorer(id), &focus, window, cx));
        self.explorers.insert(id, panel.clone());
        self.dock_area.update(cx, |area, cx| {
            area.add_panel_view(panel_handle(panel), DockPlacement::Center, None, window, cx)
        });
    }

    fn open_lan_device(
        &mut self,
        peer: &str,
        device: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((core, target, owner)) = self.lan.read(cx).device_target(peer, device) else {
            window.push_notification(Notification::error(t!("lan.devices.refresh_first")), cx);
            return;
        };
        let factory = Arc::new(crate::lan::DeviceTerminalFactory::new(
            core,
            peer.to_owned(),
            target.clone(),
        ));
        let title = format!("LAN · {owner} · {} · {}", target.name, target.detail);
        if target.kind == shellrs_lan::DeviceKind::Serial {
            let mut settings = crate::host::SerialSettings::default();
            if let Some(parameters) = &target.serial {
                settings.baud = parameters.baud;
                settings.data_bits =
                    crate::host::DataBits::from_value(i64::from(parameters.data_bits))
                        .unwrap_or(settings.data_bits);
                settings.parity =
                    crate::host::Parity::from_stored(&parameters.parity).unwrap_or(settings.parity);
                settings.stop_bits =
                    crate::host::StopBits::from_value(i64::from(parameters.stop_bits))
                        .unwrap_or(settings.stop_bits);
                settings.flow = crate::host::FlowControl::from_stored(&parameters.flow)
                    .unwrap_or(settings.flow);
            }
            let draft = crate::host::HostDraft::serial(
                title,
                format!("lan-device:{peer}:{device}"),
                settings,
                None,
            )
            .with_terminal_encoding(
                crate::host::TerminalEncoding::from_key(&target.encoding).unwrap_or_default(),
            );
            let host = self
                .store
                .update(cx, |store, cx| store.insert_temporary(draft, None, cx));
            let mut transports = self.host_terminals.clone();
            transports.serial = factory;
            self.lan_serial_transports.insert(host, transports);
            self.open_remote_terminal(host, None, window, cx);
            let key = format!("lan-device:{peer}:{device}");
            let panels: Vec<_> = self
                .terminals
                .values()
                .filter(|panel| panel.read(cx).host_id() == host)
                .cloned()
                .collect();
            for panel in panels {
                panel.read(cx).terminal().clone().update(cx, |view, cx| {
                    view.bind_timestamps(self.store.clone(), key.clone(), None, cx)
                });
            }
        } else {
            let id = LocalTerminalId(self.next_local_terminal_id);
            self.next_local_terminal_id += 1;
            let factory: SharedTerminalTransportFactory = factory;
            let name = Some(SavedName {
                name: title.into(),
                fixed: true,
                title: None,
            });
            let (panel, subscription) =
                new_local_terminal_panel(id, factory.clone(), name, window, cx);
            let key = format!("lan:{peer}:device:{device}");
            panel.read(cx).terminal().clone().update(cx, |view, cx| {
                view.bind_timestamps(self.store.clone(), key, None, cx)
            });
            self._subscriptions.push(subscription);
            let focus = panel.read(cx).focus_handle(cx);
            self._subscriptions.push(follow_focus(
                CenterTab::LocalTerminal(id),
                &focus,
                window,
                cx,
            ));
            self.lan_local_factories.insert(id, factory);
            self.local_terminals.insert(id, panel.clone());
            self.dock_area.update(cx, |area, cx| {
                area.add_panel_view(panel_handle(panel), DockPlacement::Center, None, window, cx)
            });
        }
    }

    pub(super) fn render_window(
        &mut self,
        detached: Option<super::detached::DetachedContent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let serial_tools = detached.as_ref().and_then(|content| content.serial.clone());
        let serial_commands = serial_tools.clone();
        let serial_toggle = serial_tools.clone();
        let serial_sidebar_toggle = serial_tools;
        let status = match self.active_tab {
            // A terminal tells its own connection, to the host as it took it.
            Some(CenterTab::Terminal(id)) => match self.terminals.get(&id) {
                Some(panel) => {
                    let panel = panel.read(cx);
                    WorkspaceStatus::Host(
                        Some((panel.host().clone(), panel.connection_state(cx))),
                        Some(panel.status(cx)),
                    )
                }
                None => WorkspaceStatus::Host(None, None),
            },
            Some(CenterTab::LocalTerminal(id)) => self
                .local_terminals
                .get(&id)
                .map(|panel| {
                    WorkspaceStatus::Local(
                        panel.read(cx).status(cx),
                        panel.read(cx).is_command_terminal(),
                    )
                })
                .unwrap_or(WorkspaceStatus::Host(None, None)),
            // An SFTP tab tells its own connection, and what went wrong in it.
            Some(CenterTab::Explorer(id)) => match self.explorers.get(&id) {
                Some(panel) => {
                    let panel = panel.read(cx);
                    WorkspaceStatus::Explorer(Some(panel.host().clone()), panel.status(cx))
                }
                None => WorkspaceStatus::Host(None, None),
            },
            // An editor of a remote file tells the connection of the SFTP
            // tab its saves go through.
            Some(CenterTab::Editor(id)) => match self.editors.get(&id) {
                Some(editor) => {
                    let editor = editor.read(cx);
                    let remote = editor.explorer_id().map(|explorer| {
                        let panel = self.explorers.get(&explorer).map(|panel| panel.read(cx));
                        (
                            panel.map(|panel| panel.host().clone()),
                            panel.map_or(ConnectionState::Disconnected, |panel| {
                                panel.connection_state()
                            }),
                        )
                    });
                    WorkspaceStatus::Editor {
                        remote,
                        cursor: editor.cursor_label(),
                        format: editor.format_label(),
                    }
                }
                None => WorkspaceStatus::Host(None, None),
            },
            _ => WorkspaceStatus::Host(None, None),
        };
        let sidebar = self.sidebar_showing(cx);
        let running_forwards = self.forwards.read(cx).active_count();
        let tool = self.tool_showing(cx);
        let offered_tools = self.offered_tools(cx);

        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_drag_move(cx.listener(Self::on_tab_drag))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_tab_drag_outside))
            .on_action(cx.listener(Self::on_detach_tab))
            .on_action(cx.listener(Self::on_return_detached_tab))
            .on_action(cx.listener(Self::on_close_detached_tab))
            .on_action(cx.listener(Self::on_new_host))
            .on_action(cx.listener(Self::on_lan_action))
            .on_action(cx.listener(Self::on_utility_action))
            .on_action(cx.listener(
                move |this, action: &crate::app::SerialToolAction, window, cx| {
                    if let Some(tools) = &serial_commands {
                        tools
                            .panel
                            .update(cx, |panel, cx| panel.command(&action.0, window, cx));
                    } else {
                        this.on_serial_tool_action(action, window, cx);
                    }
                },
            ))
            .on_action(cx.listener(Self::on_new_temporary_connection))
            .on_action(cx.listener(Self::on_quick_connect))
            .on_action(cx.listener(Self::on_new_local_terminal))
            .on_action(cx.listener(Self::on_open_local_terminal_options))
            .on_action(cx.listener(Self::on_start_local_terminal))
            .on_action(cx.listener(Self::on_new_cmder_terminal))
            .on_action(cx.listener(Self::on_new_command_terminal))
            .on_action(cx.listener(Self::on_toggle_terminal_broadcast))
            .on_action(cx.listener(Self::on_set_broadcast_target))
            .on_action(cx.listener(Self::on_toggle_compose_pane))
            .on_action(cx.listener(Self::on_send_compose))
            .on_action(cx.listener(Self::on_send_compose_snippet))
            .on_action(cx.listener(Self::on_insert_snippet))
            .on_action(cx.listener(Self::on_send_compose_interrupt))
            .on_action(cx.listener(Self::on_toggle_broadcast_keys))
            .on_action(cx.listener(Self::on_edit_host))
            .on_action(cx.listener(Self::on_duplicate_host))
            .on_action(cx.listener(Self::on_move_host_node))
            .on_action(cx.listener(Self::on_automatically_order_hosts))
            .on_action(cx.listener(Self::on_delete_host))
            .on_action(cx.listener(Self::on_new_host_in_group))
            .on_action(cx.listener(Self::on_new_group))
            .on_action(cx.listener(Self::on_import_ssh_config))
            .on_action(cx.listener(Self::on_import_xshell))
            .on_action(cx.listener(Self::on_import_mobaxterm))
            .on_action(cx.listener(Self::on_import_terminal_schemes))
            .on_action(cx.listener(Self::on_remove_terminal_scheme))
            .on_action(cx.listener(Self::on_import_known_hosts))
            .on_action(cx.listener(Self::on_expand_all_groups))
            .on_action(cx.listener(Self::on_collapse_all_groups))
            .on_action(cx.listener(Self::on_new_child_group))
            .on_action(cx.listener(Self::on_rename_group))
            .on_action(cx.listener(Self::on_delete_group))
            .on_action(cx.listener(Self::on_connect_host))
            .on_action(cx.listener(Self::on_duplicate_terminal))
            .on_action(cx.listener(Self::on_duplicate_active_terminal))
            .on_action(cx.listener(Self::on_connect_group))
            .on_action(cx.listener(Self::on_reconnect_terminal))
            .on_action(cx.listener(Self::on_disconnect_terminal))
            .on_action(cx.listener(Self::on_send_break))
            .on_action(cx.listener(Self::on_send_serial_hex))
            .on_action(cx.listener(Self::on_toggle_serial_receive_hex))
            .on_action(cx.listener(Self::on_toggle_serial_send_hex))
            .on_action(cx.listener(Self::on_set_serial_backspace))
            .on_action(cx.listener(Self::on_start_terminal_recording))
            .on_action(cx.listener(Self::on_stop_terminal_recording))
            .on_action(cx.listener(Self::on_clear_terminal_log))
            .on_action(cx.listener(Self::on_delete_terminal_log))
            .on_action(cx.listener(Self::on_open_explorer))
            .on_action(cx.listener(Self::on_explorer_action))
            .on_action(cx.listener(Self::on_explorer_shortcut))
            .on_action(cx.listener(Self::on_close_terminal))
            .on_action(cx.listener(Self::on_close_explorer))
            .on_action(cx.listener(Self::on_close_editor))
            .on_action(cx.listener(Self::on_editor_action))
            .on_action(cx.listener(Self::on_editor_shortcut))
            .on_action(cx.listener(Self::on_close_local_terminal))
            .on_action(cx.listener(Self::on_enable_app_lock))
            .on_action(cx.listener(Self::on_change_app_lock_password))
            .on_action(cx.listener(Self::on_disable_app_lock))
            .on_action(cx.listener(Self::on_lock_app))
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(Self::on_install_cli_command))
            .on_action(cx.listener(Self::on_remove_cli_command))
            .on_action(cx.listener(Self::on_install_agent_skill))
            .on_action(cx.listener(Self::on_set_file_size_format))
            .on_action(cx.listener(Self::on_toggle_hidden_files))
            .on_action(cx.listener(Self::on_toggle_host_details))
            .on_action(cx.listener(Self::on_remove_agent_skill))
            .on_action(cx.listener(Self::on_refresh_cli_integration))
            .on_action(cx.listener(Self::on_check_for_updates))
            .on_action(cx.listener(Self::on_download_update))
            .on_action(cx.listener(Self::on_show_update))
            .on_action(cx.listener(Self::on_restart_to_update))
            .on_action(cx.listener(Self::on_open_download_page))
            .on_action(cx.listener(Self::on_open_changelog))
            .on_action(cx.listener(Self::on_open_website))
            .on_action(cx.listener(Self::on_set_up_sync))
            .on_action(cx.listener(Self::on_set_up_sync_in))
            .on_action(cx.listener(Self::on_set_up_webdav_sync))
            .on_action(cx.listener(Self::on_set_up_s3_sync))
            .on_action(cx.listener(Self::on_change_sync_login))
            .on_action(cx.listener(Self::on_sync_now))
            .on_action(cx.listener(Self::on_review_sync))
            .on_action(cx.listener(Self::on_change_sync_password))
            .on_action(cx.listener(Self::on_turn_off_sync))
            .on_action(cx.listener(Self::on_remove_sync_device))
            .on_action(cx.listener(Self::on_copy_agent_skill))
            .on_action(cx.listener(Self::on_close_settings))
            .on_action(cx.listener(Self::on_close_active_tab))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_previous_tab))
            .on_action(cx.listener(Self::on_switch_to_tab))
            .on_action(cx.listener(Self::on_close_tabs))
            .on_action(cx.listener(Self::on_rename_terminal))
            .on_action(cx.listener(Self::on_rename_explorer))
            .on_action(cx.listener(Self::on_copy_host_address))
            .on_action(cx.listener(Self::on_copy_host_id))
            .on_action(cx.listener(Self::on_restart_local_terminal))
            .on_action(cx.listener(Self::on_copy_terminal))
            .on_action(cx.listener(Self::on_paste_terminal))
            .on_action(cx.listener(Self::on_select_all_terminal))
            .on_action(cx.listener(Self::on_find_in_terminal))
            .on_action(cx.listener(Self::on_find_next_in_terminal))
            .on_action(cx.listener(Self::on_find_previous_in_terminal))
            .on_action(cx.listener(Self::on_dismiss_terminal_find))
            .on_action(cx.listener(Self::on_toggle_terminal_find_option))
            .on_action(cx.listener(Self::on_copy_terminal_find_results))
            .on_action(cx.listener(Self::on_clear_terminal))
            .on_action(cx.listener(Self::on_toggle_host_panel))
            .on_action(cx.listener(Self::on_maximize_window))
            .on_action(cx.listener(Self::on_restore_window))
            .on_action(cx.listener(Self::on_show_or_hide_app))
            .on_action(cx.listener(
                move |this, action: &crate::app::ToggleToolSidebar, window, cx| {
                    if let Some(tools) = &serial_sidebar_toggle {
                        tools.toggle(window, cx);
                    } else {
                        this.on_toggle_tool_sidebar(action, window, cx);
                    }
                },
            ))
            .on_action(
                cx.listener(move |this, action: &crate::app::ToggleTool, window, cx| {
                    if let Some(tools) = &serial_toggle
                        && action.0 == crate::app::ToolKind::Serial
                    {
                        tools.toggle(window, cx);
                    } else {
                        this.on_toggle_tool(action, window, cx);
                    }
                }),
            )
            .on_action(cx.listener(Self::on_auto_serial_files))
            .on_action(cx.listener(Self::on_upload_serial_file))
            .on_action(cx.listener(Self::on_download_serial_file))
            .on_action(cx.listener(Self::on_serial_file_download))
            .on_action(cx.listener(Self::on_choose_adb_device))
            .on_action(cx.listener(Self::on_open_adb_devices))
            .on_action(cx.listener(Self::on_adb_device_action))
            .on_action(cx.listener(Self::on_open_adb_files))
            .on_action(cx.listener(Self::on_refresh_gpu))
            .on_action(cx.listener(Self::on_toggle_gpu_device))
            .on_action(cx.listener(Self::on_toggle_all_gpu_devices))
            .on_action(cx.listener(Self::on_refresh_tmux))
            .on_action(cx.listener(Self::on_new_tmux_session))
            .on_action(cx.listener(Self::on_attach_tmux_session))
            .on_action(cx.listener(Self::on_detach_tmux))
            .on_action(cx.listener(Self::on_rename_tmux_session))
            .on_action(cx.listener(Self::on_kill_tmux_session))
            .on_action(cx.listener(Self::on_toggle_directory_follow))
            .on_action(cx.listener(Self::on_install_directory_follow))
            .on_action(cx.listener(Self::on_toggle_monitor_detail))
            .on_action(cx.listener(Self::on_toggle_monitor_channel))
            .on_action(cx.listener(Self::on_refresh_connections))
            .on_action(cx.listener(Self::on_refresh_processes))
            .on_action(cx.listener(Self::on_sort_processes))
            .on_action(cx.listener(Self::on_end_process))
            .on_action(cx.listener(Self::on_show_process))
            .on_action(cx.listener(Self::on_refresh_services))
            .on_action(cx.listener(Self::on_show_service))
            .on_action(cx.listener(Self::on_control_service))
            .on_action(cx.listener(Self::on_refresh_history))
            .on_action(cx.listener(Self::on_enter_command))
            .on_action(cx.listener(Self::on_copy_command))
            .on_action(cx.listener(Self::on_new_terminal_group))
            .on_action(cx.listener(Self::on_rename_terminal_group))
            .on_action(cx.listener(Self::on_delete_terminal_group))
            .on_action(cx.listener(Self::on_new_saved_terminal))
            .on_action(cx.listener(Self::on_open_saved_terminal))
            .on_action(cx.listener(Self::on_close_saved_terminal))
            .on_action(cx.listener(Self::on_rename_saved_terminal))
            .on_action(cx.listener(Self::on_delete_saved_terminal))
            .on_action(cx.listener(Self::on_move_terminal_node))
            .on_action(cx.listener(Self::on_toggle_terminal_group))
            .on_action(cx.listener(Self::on_new_snippet))
            .on_action(cx.listener(Self::on_new_snippet_in))
            .on_action(cx.listener(Self::on_edit_snippet))
            .on_action(cx.listener(Self::on_delete_snippet))
            .on_action(cx.listener(Self::on_new_snippet_category))
            .on_action(cx.listener(Self::on_rename_snippet_category))
            .on_action(cx.listener(Self::on_delete_snippet_category))
            .on_action(cx.listener(Self::on_toggle_snippet_category))
            .on_action(cx.listener(Self::on_refresh_docker))
            .on_action(cx.listener(Self::on_kubernetes_action))
            .on_action(cx.listener(Self::on_device_log_action))
            .on_action(cx.listener(Self::on_toggle_docker_project))
            .on_action(cx.listener(Self::on_show_docker_object))
            .on_action(cx.listener(Self::on_control_containers))
            .on_action(cx.listener(Self::on_remove_docker_object))
            .on_action(cx.listener(Self::on_show_hosts))
            .on_action(cx.listener(Self::on_show_forwards))
            .on_action(cx.listener(Self::on_new_forward))
            .on_action(cx.listener(Self::on_edit_forward))
            .on_action(cx.listener(Self::on_delete_forward))
            .on_action(cx.listener(Self::on_start_forward))
            .on_action(cx.listener(Self::on_stop_forward))
            .on_action(cx.listener(Self::on_show_credentials))
            .on_action(cx.listener(Self::on_show_remote_management))
            .on_action(cx.listener(Self::on_remote_action))
            .on_action(cx.listener(Self::on_new_credential))
            .on_action(cx.listener(Self::on_generate_credential_key))
            .on_action(cx.listener(Self::on_edit_credential))
            .on_action(cx.listener(Self::on_copy_credential_public_key))
            .on_action(cx.listener(Self::on_delete_credential))
            .on_action(cx.listener(Self::on_toggle_theme))
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_zoom_in))
            .on_action(cx.listener(Self::on_zoom_out))
            .on_action(cx.listener(Self::on_zoom_reset))
            .on_action(cx.listener(Self::on_zoom_terminal_in))
            .on_action(cx.listener(Self::on_zoom_terminal_out))
            .on_action(cx.listener(Self::on_zoom_terminal_reset))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .map(|root| {
                if let Some(detached) = detached {
                    let tab = detached.tab;
                    let content = if let Some(serial) = detached.serial {
                        serial.render(detached.view, &self.focus_handle, cx)
                    } else {
                        detached.view.into_any_element()
                    };
                    root.child(
                        TitleBar::new().child(crate::app::APP_NAME).child(
                            div()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .child(
                                    gpui_kit::component::button::Button::new("return-detached-tab")
                                        .small()
                                        .label(t!("workspace.detached.return"))
                                        .on_click(move |_, window, cx| {
                                            window.dispatch_action(
                                                Box::new(ReturnDetachedTab(tab)),
                                                cx,
                                            )
                                        }),
                                ),
                        ),
                    )
                    .child(
                        div()
                            .id("detached-tab-content")
                            .test_support()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .child(content),
                    )
                } else {
                    root.child(render_title_bar(
                        sidebar,
                        running_forwards,
                        self.updater.read(cx).snapshot().badge(),
                        self.cmder.read(cx).directory().is_some(),
                        &self.focus_handle,
                        cx,
                    ))
                    .child(render_sidebar_toggle(
                        sidebar.is_some(),
                        self.dock_area
                            .read(cx)
                            .dock_size(DockPlacement::Left)
                            .unwrap_or_default(),
                        &self.focus_handle,
                    ))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .child(
                                div()
                                    .relative()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .child(self.dock_area.clone())
                                    .children(self.log_split_measure(cx)),
                            )
                            // The switch comes and goes with the SSH terminals.
                            .when(!offered_tools.is_empty(), |area| {
                                area.child(render_tool_switch(
                                    tool,
                                    &offered_tools,
                                    &self.focus_handle,
                                    cx,
                                ))
                            }),
                    )
                    .child(status)
                }
            })
            .children(Root::render_sheet_layer(window, cx))
            .children(Root::render_dialog_layer(window, cx))
            .children(
                Root::render_notification_layer(window, cx)
                    .map(|layer| deferred(layer).with_priority(NOTIFICATION_PRIORITY)),
            )
            .into_any_element()
    }

    fn open_lan_host(
        &mut self,
        peer: &str,
        host: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((core, target, owner)) = self.lan.read(cx).host_target(peer, host) else {
            window.push_notification(Notification::error(t!("lan.ssh_refresh_first")), cx);
            return;
        };
        let title = format!("LAN · {owner} · SSH · {}", target.name);
        let timestamp_key = format!("lan:{peer}:ssh:{}", target.id);
        let factory: SharedTerminalTransportFactory = Arc::new(
            crate::lan::DeviceTerminalFactory::ssh(core, peer.to_owned(), target),
        );
        let id = LocalTerminalId(self.next_local_terminal_id);
        self.next_local_terminal_id += 1;
        let name = Some(SavedName {
            name: title.into(),
            fixed: true,
            title: None,
        });
        let (panel, subscription) = new_local_terminal_panel(id, factory.clone(), name, window, cx);
        panel.read(cx).terminal().clone().update(cx, |view, cx| {
            view.bind_timestamps(self.store.clone(), timestamp_key, None, cx)
        });
        self._subscriptions.push(subscription);
        let focus = panel.read(cx).focus_handle(cx);
        self._subscriptions.push(follow_focus(
            CenterTab::LocalTerminal(id),
            &focus,
            window,
            cx,
        ));
        self.lan_local_factories.insert(id, factory);
        self.local_terminals.insert(id, panel.clone());
        self.dock_area.update(cx, |area, cx| {
            area.add_panel_view(panel_handle(panel), DockPlacement::Center, None, window, cx)
        });
    }
}

impl Workspace {
    fn on_serial_tool_action(
        &mut self,
        action: &crate::app::SerialToolAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tools.update(cx, |tools, cx| {
            tools.serial_tool_command(&action.0, window, cx)
        });
    }

    fn on_utility_action(
        &mut self,
        action: &crate::app::UtilityAction,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = if let crate::utilities::UtilityCommand::Pick(owner, _) = &action.0 {
            self.terminals
                .values()
                .map(|panel| panel.read(cx).terminal().clone())
                .chain(
                    self.local_terminals
                        .values()
                        .map(|panel| panel.read(cx).terminal().clone()),
                )
                .find(|view| view.entity_id() == *owner)
        } else {
            self.tools.read(cx).utilities_target(cx)
        };
        if let Some(view) = view {
            view.update(cx, |view, cx| view.timestamp_command(&action.0, cx));
        }
    }
}
