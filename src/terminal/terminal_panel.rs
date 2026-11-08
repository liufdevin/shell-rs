use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelEvent, TabGroup},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::PopupMenu,
    notification::Notification,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    CatalogIcon, CenterTab, ClearTerminalLog, CloseTerminal, CopyHostAddress, CopyHostId,
    DeleteTerminalLog, DisconnectTerminal, DuplicateTerminal, EditHost, OpenExplorer,
    ReconnectTerminal, RenameTerminal, SendBreak, SendSerialHex,
};
use crate::connection::{ConnectionPrompt, ConnectionPromptReply};
use crate::host::{ConnectionState, Host, HostConnection, HostId, HostOs, HostStore, ProtocolKind};
use crate::i18n::t;
use crate::shared::{
    ClosableTabTitle, HostMark, LatencyLabel, RenamableTab, close_tab_items, confirm_danger,
};

use super::session_io::{DailyLog, TerminalOptions};
use super::terminal_view::TerminalPresentation;
use super::{
    HostTerminalTransports, RemoteTerminalId, ReportedDirectory, TerminalEvent, TerminalLifecycle,
    TerminalMenuItems, TerminalNotice, TerminalStatus, TerminalView, ZmodemDirection, ZmodemReply,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalPanelEvent {
    Input(RemoteTerminalId, super::TerminalInputEvent),
    Activated(RemoteTerminalId, HostId),
    Closed(RemoteTerminalId, HostId),
    StatusChanged(RemoteTerminalId, HostId),
    /// Connected, again after each reconnection.
    Connected(RemoteTerminalId, HostId),
    PromptRequested(RemoteTerminalId, HostId, ConnectionPrompt),
    HostOsDetected(HostId, HostOs),
    Notice(RemoteTerminalId, TerminalNotice),
    /// The shell reported another folder; the right sidebar's 文件 follows.
    DirectoryChanged(RemoteTerminalId, ReportedDirectory),
    /// `rz` or `sz` started: the user chooses, and the answer goes to
    /// [`TerminalPanel::zmodem_reply`].
    ZmodemAsk(RemoteTerminalId, ZmodemDirection),
    /// Files dropped on the terminal, for [`TerminalPanel::upload_dropped`].
    FilesDropped(RemoteTerminalId, Vec<PathBuf>),
}

/// A remote-host Dock panel backed by the shared terminal engine: an SSH
/// host's, a Telnet host's or a serial host's.
pub struct TerminalPanel {
    id: RemoteTerminalId,
    host_id: HostId,
    store: Entity<HostStore>,
    terminal: Entity<TerminalView>,
    presentation: TerminalPresentation,
    transports: HostTerminalTransports,
    /// The host as it was when the tab opened: what the tab shows, what
    /// it offers (only SSH has SFTP, only a serial port no address to copy)
    /// and what it connects with, every reconnection included. Editing or
    /// deleting the host leaves it alone: connecting the host anew takes
    /// the new settings, while duplicating a tab copies its snapshot.
    /// Only the system this tab's own connections find is written in.
    host: Host,
    connection: HostConnection,
    /// Opened from a link (外部连接): nothing beside the terminal.
    external: bool,
    tab_group: Option<WeakEntity<TabGroup>>,
    broadcast: Option<super::BroadcastSelection>,
    /// A title the user gave this tab, to tell apart several connections to
    /// the same host. Lives as long as the tab, like the rest of the layout.
    custom_title: Option<SharedString>,
    /// Applied once, after this copy's first successful connection.
    initial_directory: Option<ReportedDirectory>,
    hex_input: Entity<InputState>,
    recording_path: Option<PathBuf>,
    recording_opening: bool,
    recording_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// A new tab's own snapshot, independent of later changes to its source.
pub(crate) struct TerminalCopy {
    host: Host,
    connection: HostConnection,
    external: bool,
    title: Option<SharedString>,
    directory: Option<ReportedDirectory>,
    serial_modes: Option<super::SerialDataModes>,
}

impl TerminalPanel {
    pub fn new(
        id: RemoteTerminalId,
        host_id: HostId,
        store: Entity<HostStore>,
        transports: HostTerminalTransports,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (host, connection, external) = {
            let store = store.read(cx);
            let host = store
                .host(host_id)
                .cloned()
                .expect("terminal hosts must exist in the store");
            let connection = store.connection_of(&host);
            (host, connection, store.is_external(host_id))
        };
        Self::from_copy(
            id,
            TerminalCopy {
                host,
                connection,
                external,
                title: None,
                directory: None,
                serial_modes: None,
            },
            store,
            transports,
            window,
            cx,
        )
    }

    pub(crate) fn copy_for_new_tab(&self, cx: &App) -> TerminalCopy {
        TerminalCopy {
            host: self.host.clone(),
            connection: self.connection.clone(),
            external: self.external,
            title: self.custom_title.clone(),
            serial_modes: Some(self.serial_modes(cx)),
            directory: self
                .host
                .is_ssh()
                .then(|| self.terminal.read(cx).directory(cx))
                .flatten(),
        }
    }

    pub(crate) fn from_copy(
        id: RemoteTerminalId,
        copy: TerminalCopy,
        store: Entity<HostStore>,
        transports: HostTerminalTransports,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let TerminalCopy {
            host,
            connection,
            external,
            title,
            directory,
            serial_modes,
        } = copy;
        let host_id = host.id;
        let protocol = host.kind();
        let serial_modes = serial_modes
            .unwrap_or_else(|| super::SerialPreferences::current(host.public_id.as_str(), cx));
        let terminal = cx.new(|cx| {
            let name = host.name.clone();
            let mut terminal = TerminalView::new_connection(
                ("terminal", id.0),
                move |_| t!("terminal.remote.label", host = name),
                transports.factory_for(&connection),
                TerminalOptions {
                    encoding: host.terminal_encoding,
                    remote: true,
                    shared_shell: protocol != ProtocolKind::Ssh,
                    serial: (protocol == ProtocolKind::Serial)
                        .then(|| (host.name.to_string(), serial_modes)),
                    log: host.log_enabled.then(|| {
                        DailyLog::new(
                            host.log_directory
                                .as_ref()
                                .map(|path| PathBuf::from(path.as_ref()))
                                .unwrap_or_else(super::default_log_directory),
                            &host.name,
                        )
                    }),
                    reconnect: super::TerminalConnectionSettings {
                        auto_reconnect: host.auto_reconnect,
                        reconnect_delay_secs: host.reconnect_delay_secs,
                    },
                },
                window,
                cx,
            );
            terminal.set_menu_items(remote_menu_items(
                id,
                host_id,
                protocol,
                host.address_is_ip(),
                store.clone(),
            ));
            terminal.set_ended_actions(ReconnectTerminal(id), CloseTerminal(id), cx);
            terminal.take_dropped_files();
            terminal
        });
        let hex_input = cx.new(|cx| InputState::new(window, cx).placeholder("00 FF 0D 0A"));
        let presentation = terminal.read(cx).presentation(cx);
        let subscriptions = vec![
            cx.subscribe_in(&hex_input, window, move |_, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    window.dispatch_action(Box::new(SendSerialHex(id)), cx);
                }
            }),
            cx.observe(&store, |this, store, cx| {
                if let Some(host) = store.read(cx).host(this.host_id) {
                    let settings = super::TerminalConnectionSettings {
                        auto_reconnect: host.auto_reconnect,
                        reconnect_delay_secs: host.reconnect_delay_secs,
                    };
                    let encoding = host.terminal_encoding;
                    this.terminal.update(cx, |terminal, cx| {
                        terminal.set_reconnect_settings(settings, cx);
                        terminal.set_connection_encoding(encoding, cx);
                    });
                }
                cx.notify();
            }),
            cx.observe(&terminal, |this, terminal, cx| {
                let presentation = terminal.read(cx).presentation(cx);
                if this.presentation == presentation {
                    return;
                }
                this.presentation = presentation;
                cx.emit(TerminalPanelEvent::StatusChanged(this.id, this.host_id));
                cx.notify();
            }),
            cx.subscribe_in(
                &terminal,
                window,
                |this, _, event: &TerminalEvent, window, cx| match event {
                    TerminalEvent::RecordingFailed(error) => {
                        window.push_notification(
                            Notification::error(t!("terminal.recording.failed", error = error)),
                            cx,
                        );
                        cx.notify();
                    }
                    TerminalEvent::Input(input) => {
                        cx.emit(TerminalPanelEvent::Input(this.id, input.clone()));
                    }
                    TerminalEvent::Started => {
                        if let Some(command) = this
                            .initial_directory
                            .take()
                            .and_then(|directory| directory.cd_command())
                        {
                            // Started means authentication and the shell channel
                            // are ready. The command goes only to the new tab.
                            if let Err(error) = this.terminal.update(cx, |terminal, cx| {
                                terminal.enter_command(&command, true, false, cx)
                            }) {
                                window.push_notification(
                                    Notification::error(t!(
                                        "terminal.remote.restore_directory_failed",
                                        error = error
                                    )),
                                    cx,
                                );
                            }
                        }
                        cx.emit(TerminalPanelEvent::Connected(this.id, this.host_id));
                    }
                    TerminalEvent::PromptRequested(prompt) => {
                        cx.emit(TerminalPanelEvent::PromptRequested(
                            this.id,
                            this.host_id,
                            prompt.clone(),
                        ));
                    }
                    TerminalEvent::HostOsDetected(os) => {
                        this.host.os = Some(*os);
                        cx.emit(TerminalPanelEvent::HostOsDetected(this.host_id, *os));
                    }
                    TerminalEvent::Notice(notice) => {
                        cx.emit(TerminalPanelEvent::Notice(this.id, notice.clone()));
                    }
                    TerminalEvent::DirectoryChanged(directory) => {
                        cx.emit(TerminalPanelEvent::DirectoryChanged(
                            this.id,
                            directory.clone(),
                        ));
                    }
                    TerminalEvent::ZmodemAsk(direction) => {
                        cx.emit(TerminalPanelEvent::ZmodemAsk(this.id, *direction));
                    }
                    TerminalEvent::FilesDropped(files) => {
                        cx.emit(TerminalPanelEvent::FilesDropped(this.id, files.clone()));
                    }
                    // Only a local transport tells.
                    TerminalEvent::WorkingDirectory(_)
                    | TerminalEvent::InputRejected(_)
                    | TerminalEvent::ConnectSsh(_) => {}
                },
            ),
        ];

        Self {
            id,
            host_id,
            store,
            terminal,
            presentation,
            transports,
            host,
            connection,
            external,
            tab_group: None,
            broadcast: None,
            custom_title: title,
            initial_directory: directory,
            hex_input,
            recording_path: None,
            recording_opening: false,
            recording_task: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn id(&self) -> RemoteTerminalId {
        self.id
    }

    pub fn host_id(&self) -> HostId {
        self.host_id
    }

    /// The host as the tab took it (see the field).
    pub fn host(&self) -> &Host {
        &self.host
    }

    pub fn connection(&self) -> &HostConnection {
        &self.connection
    }

    /// Whether only the terminal runs on its connection: a link's, a
    /// Telnet host's or a serial host's.
    pub fn shell_only(&self) -> bool {
        (self.external || self.host.is_serial())
            && !self
                .host
                .protocol
                .serial()
                .is_some_and(|settings| settings.linux_shell)
    }

    pub fn tab_group(&self) -> Option<WeakEntity<TabGroup>> {
        self.tab_group.clone()
    }

    fn tab_menu(&self, cx: &Context<Self>) -> TabMenu {
        let store = self.store.read(cx);
        TabMenu {
            id: self.id,
            host_id: self.host_id,
            host_is_ip: self.host.address_is_ip(),
            protocol: self.host.kind(),
            running: self.lifecycle(cx) == TerminalLifecycle::Running,
            host_exists: store.host(self.host_id).is_some(),
            host_is_ssh: store.host(self.host_id).is_some_and(|host| host.is_ssh()),
            editable: !store.is_temporary(self.host_id),
            group: self.tab_group.clone(),
            panel: cx.entity_id(),
        }
    }

    pub fn terminal(&self) -> &Entity<TerminalView> {
        &self.terminal
    }

    pub(crate) fn set_broadcast_selection(
        &mut self,
        selection: Option<super::BroadcastSelection>,
        cx: &mut Context<Self>,
    ) {
        if self.broadcast != selection {
            self.broadcast = selection;
            cx.notify();
        }
    }

    pub fn lifecycle(&self, cx: &App) -> TerminalLifecycle {
        self.terminal.read(cx).lifecycle(cx)
    }

    /// Where this tab's own connection stands.
    pub fn connection_state(&self, cx: &App) -> ConnectionState {
        match self.lifecycle(cx) {
            TerminalLifecycle::Starting => ConnectionState::Connecting,
            TerminalLifecycle::Running => ConnectionState::Connected,
            TerminalLifecycle::Exited { .. }
            | TerminalLifecycle::Failed(_)
            | TerminalLifecycle::Closing => ConnectionState::Disconnected,
        }
    }

    /// The terminal's state and size, for the window's status bar.
    pub fn status(&self, cx: &App) -> TerminalStatus {
        self.terminal.read(cx).status(cx)
    }

    /// Connect again as the tab first did.
    pub fn reconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let factory = self.transports.factory_for(&self.connection);
        self.terminal.update(cx, |terminal, cx| {
            terminal.restart_with_factory(factory, cx)
        });
        let focus = self.terminal.read(cx).focus_handle();
        window.focus(&focus, cx);
    }

    pub fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.terminal.update(cx, |terminal, cx| {
            terminal.stop(&t!("terminal.notice.disconnected"), cx)
        });
    }

    pub fn send_hex(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.host.kind() != ProtocolKind::Serial || !self.serial_modes(cx).send_hex {
            return;
        }
        let text = self.hex_input.read(cx).value();
        if let Err(error) = self.terminal.read(cx).send_hex(&text, cx) {
            window.push_notification(Notification::error(error), cx);
        }
    }

    pub fn serial_modes(&self, cx: &App) -> super::SerialDataModes {
        self.terminal.read(cx).serial_modes(cx)
    }

    pub fn set_serial_modes(&mut self, modes: super::SerialDataModes, cx: &mut Context<Self>) {
        if self.host.kind() != ProtocolKind::Serial {
            return;
        }
        self.terminal
            .update(cx, |terminal, cx| terminal.set_serial_modes(modes, cx));
        cx.notify();
    }

    pub fn start_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.recording_opening || self.terminal.read(cx).is_recording(cx) {
            return;
        }
        let directory = self
            .recording_path
            .as_ref()
            .and_then(|path| path.parent())
            .map(PathBuf::from)
            .or_else(dirs::document_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let chosen = cx.prompt_for_new_path(&directory, Some("terminal.log"));
        self.recording_opening = true;
        cx.notify();
        self.recording_task = Some(cx.spawn_in(window, async move |this, cx| {
            let path = match chosen.await {
                Ok(Ok(Some(path))) => Some(path),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => {
                    this.update_in(cx, |this, window, cx| {
                        this.recording_opening = false;
                        window.push_notification(
                            Notification::error(t!("terminal.recording.failed", error = error)),
                            cx,
                        );
                        cx.notify();
                    })
                    .ok();
                    return;
                }
                Err(error) => {
                    this.update_in(cx, |this, window, cx| {
                        this.recording_opening = false;
                        window.push_notification(
                            Notification::error(t!("terminal.recording.failed", error = error)),
                            cx,
                        );
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let Some(path) = path else {
                this.update_in(cx, |this, _, cx| {
                    this.recording_opening = false;
                    cx.notify();
                })
                .ok();
                return;
            };
            this.update_in(cx, |this, window, cx| {
                this.begin_recording(path, window, cx);
            })
            .ok();
        }));
    }

    fn begin_recording(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let hex = self.serial_modes(cx).receive_hex;
        let file = path.clone();
        self.recording_opening = true;
        cx.notify();
        self.recording_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { super::session_io::Recording::open(&file, hex) })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.recording_opening = false;
                if this.lifecycle(cx) == TerminalLifecycle::Closing {
                    return;
                }
                match result {
                    Ok(recording) => {
                        this.recording_path = Some(path);
                        this.terminal
                            .update(cx, |terminal, cx| terminal.set_recording(recording, cx));
                    }
                    Err(error) => window.push_notification(
                        Notification::error(t!("terminal.recording.failed", error = error)),
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn stop_recording(&mut self, cx: &mut Context<Self>) {
        self.recording_task = None;
        self.recording_opening = false;
        self.terminal
            .update(cx, |terminal, cx| terminal.stop_recording(cx));
        cx.notify();
    }

    pub fn clear_log(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self
            .terminal
            .read(cx)
            .recording_path(cx)
            .or_else(|| self.recording_path.clone())
        else {
            return;
        };
        let this = cx.entity();
        let shown = path.display().to_string();
        confirm_danger(
            t!("terminal.recording.clear_title"),
            Some(t!("terminal.recording.clear_description", path = shown)),
            t!("terminal.recording.clear"),
            Rc::new(move |window, cx| {
                this.update(cx, |this, cx| this.clear_log_now(path.clone(), window, cx));
            }),
            window,
            cx,
        );
    }

    fn clear_log_now(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let previous = self.terminal.read(cx).take_recording(cx);
        let hex = self.serial_modes(cx).receive_hex;
        let file = path.clone();
        self.recording_opening = true;
        cx.notify();
        self.recording_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { super::session_io::clear_log(&file, previous, hex) })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.recording_opening = false;
                match result {
                    Ok(recording) => {
                        this.recording_path = Some(path);
                        this.terminal
                            .update(cx, |terminal, cx| terminal.set_recording(recording, cx));
                    }
                    Err(error) => window.push_notification(
                        Notification::error(t!("terminal.recording.failed", error = error)),
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn delete_log(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self
            .terminal
            .read(cx)
            .recording_path(cx)
            .or_else(|| self.recording_path.clone())
        else {
            return;
        };
        let this = cx.entity();
        let shown = path.display().to_string();
        confirm_danger(
            t!("terminal.recording.delete_title"),
            Some(t!("terminal.recording.delete_description", path = shown)),
            t!("terminal.recording.delete"),
            Rc::new(move |window, cx| {
                this.update(cx, |this, cx| this.delete_log_now(path.clone(), window, cx));
            }),
            window,
            cx,
        );
    }

    fn delete_log_now(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let previous = self.terminal.read(cx).take_recording(cx);
        self.recording_path = None;
        self.recording_opening = true;
        cx.notify();
        self.recording_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { super::session_io::delete_log(&path, previous) })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.recording_opening = false;
                match result {
                    Ok(()) => {
                        let mut draft = this
                            .store
                            .read(cx)
                            .host(this.host_id)
                            .map(Host::draft)
                            .unwrap_or_else(|| this.host.draft());
                        draft.log_enabled = false;
                        this.host.log_enabled = false;
                        let store = this.store.clone();
                        let host_id = this.host_id;
                        store.update(cx, |store, cx| {
                            store.update(host_id, draft, cx);
                        });
                    }
                    Err(error) => window.push_notification(
                        Notification::error(t!("terminal.recording.failed", error = error)),
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// What the user chose for the transfer `rz` or `sz` started.
    pub fn zmodem_reply(&self, reply: ZmodemReply, cx: &App) {
        self.terminal.read(cx).zmodem_reply(reply, cx);
    }

    /// Upload `files`, dropped on the terminal, with ZMODEM: `rz` typed for
    /// them. Says why not.
    pub fn upload_dropped(
        &mut self,
        files: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Result<(), SharedString> {
        self.terminal
            .update(cx, |terminal, cx| terminal.upload_dropped(files, cx))
    }

    /// 发送 Break, for a Telnet or serial host's terminal.
    pub fn send_break(&self, cx: &App) {
        if self.host.kind().sends_break() {
            self.terminal.read(cx).send_break(cx);
        }
    }

    pub fn cancel_connection(&mut self, cx: &mut Context<Self>) {
        self.terminal.update(cx, |terminal, cx| {
            terminal.stop(&t!("terminal.notice.cancelled"), cx)
        });
    }

    pub fn reply_to_prompt(&self, request_id: u64, reply: ConnectionPromptReply, cx: &App) {
        self.terminal
            .read(cx)
            .reply_to_prompt(request_id, reply, cx);
    }
}

impl EventEmitter<PanelEvent> for TerminalPanel {}
impl EventEmitter<TerminalPanelEvent> for TerminalPanel {}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.terminal.read(cx).focus_handle()
    }
}

impl BasePanel for TerminalPanel {
    fn panel_name(&self) -> &'static str {
        "TerminalPanel"
    }

    /// Closing goes through `CloseTerminal`; see `ClosableTabTitle`. Saying
    /// no here also keeps the dock from adding a second 「关闭」 to the 「…」
    /// menu, beside the tab's own close commands.
    fn closable(&self, _: &App) -> bool {
        false
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.terminal
            .update(cx, |terminal, cx| terminal.set_tab_active(active, cx));
        if !active {
            return;
        }
        self.terminal
            .update(cx, |terminal, cx| terminal.attach_window(window, cx));
        let focus = self.terminal.read(cx).focus_handle();
        window.focus(&focus, cx);
        cx.emit(TerminalPanelEvent::Activated(self.id, self.host_id));
    }

    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.tab_group = Some(group);
    }

    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.tab_group = None;
        if crate::shared::is_panel_transfer(cx.entity_id(), cx) {
            return;
        }
        self.recording_task = None;
        self.terminal
            .update(cx, |terminal, cx| terminal.shutdown(cx));
        let id = self.host_id;
        cx.emit(TerminalPanelEvent::Closed(self.id, id));
    }
}

impl Panel for TerminalPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id;
        let (address, os) = (self.host.address.clone(), self.host.os);
        let tab_menu = self.tab_menu(cx);
        let state = self.connection_state(cx);
        let mark = super::BroadcastSelection::lead(
            self.broadcast.clone(),
            CenterTab::Terminal(id),
            self.tab_title(cx),
            HostMark::new(
                ("terminal-tab-os", id.0),
                self.default_title(cx),
                address,
                os,
            )
            .protocol(self.host.kind())
            .connection_state(state)
            .small(),
        );
        ClosableTabTitle::new(("terminal-tab", id.0), mark, self.tab_title(cx))
            .unread(self.terminal.read(cx).has_unread_output())
            .connection_state(state)
            .closable(("close-terminal", id.0), Box::new(CloseTerminal(id)))
            .context_menu(move |menu, _, cx| tab_menu.build(menu, cx))
    }

    /// The connection's latest round trip, beside the toolbar. Shown only
    /// while the shell runs, so a dropped connection leaves no stale number.
    fn title_suffix(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let terminal = self.terminal.read(cx);
        if terminal.lifecycle(cx) != TerminalLifecycle::Running {
            return None;
        }
        let latency = terminal.latency(cx)?;
        Some(LatencyLabel::new(("terminal-latency", self.id.0), latency))
    }

    fn toolbar_buttons(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Option<Vec<Button>> {
        let host_id = self.host_id;
        let terminal_id = self.id;
        let reconnect_dispatch = self.focus_handle(cx);
        // SFTP is a new connection to the host as it is now: none once it
        // is gone or no longer an SSH host.
        let offers_sftp = self
            .store
            .read(cx)
            .host(self.host_id)
            .is_some_and(|host| host.is_ssh());
        let sftp = offers_sftp.then(|| {
            Button::new(("sftp", terminal_id.0))
                .icon(Icon::new(CatalogIcon::FolderTree))
                .label("SFTP")
                .tooltip(t!("terminal.remote.sftp_tooltip"))
                .on_click(move |_, window, cx| {
                    window.dispatch_action(Box::new(OpenExplorer(host_id)), cx)
                })
        });
        let reconnect = Button::new(("reconnect", terminal_id.0))
            .icon(Icon::new(CatalogIcon::RefreshCw))
            .tooltip(t!("terminal.remote.reconnect"))
            .on_click(move |_, window, cx| {
                reconnect_dispatch.dispatch_action(&ReconnectTerminal(terminal_id), window, cx)
            });
        Some(sftp.into_iter().chain([reconnect]).collect())
    }

    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        self.tab_menu(cx).build(menu, cx)
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

/// What a remote terminal adds to its context menu: copying the host's
/// address and ID below 复制, and the connection's commands at the bottom.
/// Only an SSH host has SFTP to open, and a serial host no address to copy;
/// a deleted host has neither.
fn remote_menu_items(
    id: RemoteTerminalId,
    host_id: HostId,
    protocol: ProtocolKind,
    address_is_ip: bool,
    store: Entity<HostStore>,
) -> TerminalMenuItems {
    let copy = Rc::new(move |menu: PopupMenu, cx: &App| {
        // Named after what the address is, as in the tab's menu.
        let copy_host = if address_is_ip {
            t!("terminal.remote.copy_ip")
        } else {
            t!("terminal.remote.copy_host_name")
        };
        let exists = store.read(cx).host(host_id).is_some();
        menu.when(protocol.has_address() && exists, |menu| {
            menu.menu_with_icon(
                copy_host,
                Icon::new(CatalogIcon::ClipboardCopy),
                Box::new(CopyHostAddress(host_id)),
            )
        })
        .menu_with_icon(
            t!("terminal.remote.copy_id"),
            Icon::new(CatalogIcon::ClipboardCopy),
            Box::new(CopyHostId(host_id)),
        )
    });
    let commands = Rc::new(move |menu: PopupMenu, lifecycle: &TerminalLifecycle| {
        let connected = matches!(
            lifecycle,
            TerminalLifecycle::Starting | TerminalLifecycle::Running
        );
        menu.when(protocol.is_ssh(), |menu| {
            menu.menu_with_icon(
                t!("terminal.remote.open_sftp"),
                Icon::new(CatalogIcon::FolderTree),
                Box::new(OpenExplorer(host_id)),
            )
        })
        .menu_with_icon(
            t!("terminal.remote.reconnect"),
            Icon::new(CatalogIcon::RefreshCw),
            Box::new(ReconnectTerminal(id)),
        )
        .menu_with_icon_and_disabled(
            t!("terminal.remote.disconnect"),
            Icon::new(CatalogIcon::Unplug),
            Box::new(DisconnectTerminal(id)),
            !connected,
        )
    });
    TerminalMenuItems { copy, commands }
}

/// The commands of one remote terminal tab, as a snapshot taken while the tab
/// renders. The tab's context menu and the tab bar's 「…」 menu both build
/// from it, so the two always list the same commands.
#[derive(Clone)]
struct TabMenu {
    id: RemoteTerminalId,
    host_id: HostId,
    host_is_ip: bool,
    /// What the host is: an SSH host's tab has SFTP, a Telnet or serial
    /// host's 发送 Break, and all but a serial host's the address.
    protocol: ProtocolKind,
    /// Whether the terminal is connected, which a break needs.
    running: bool,
    /// The host is still there: deleted, it has nothing to connect to,
    /// open SFTP on, copy or edit.
    host_exists: bool,
    /// It is an SSH host now, which a new SFTP connection needs whatever
    /// the tab's own protocol.
    host_is_ssh: bool,
    /// A saved host; a temporary one has nothing to edit.
    editable: bool,
    group: Option<WeakEntity<TabGroup>>,
    panel: EntityId,
}

impl RenamableTab for TerminalPanel {
    /// The host's name when the tab opened, as the tab shows it without a
    /// title of its own.
    fn default_title(&self, _: &App) -> SharedString {
        self.host.name.clone()
    }
    /// The tab's label: its own title when it has one, else the host name.
    fn tab_title(&self, cx: &App) -> SharedString {
        self.custom_title
            .clone()
            .unwrap_or_else(|| self.default_title(cx))
    }
    fn set_custom_title(&mut self, title: Option<SharedString>, cx: &mut Context<Self>) {
        self.custom_title = title;
        cx.notify();
    }
}

impl TabMenu {
    fn build(&self, menu: PopupMenu, cx: &App) -> PopupMenu {
        let (id, host_id) = (self.id, self.host_id);
        let copy_host = if self.host_is_ip {
            t!("terminal.remote.copy_ip")
        } else {
            t!("terminal.remote.copy_host_name")
        };
        let exists = self.host_exists;
        let mut menu = menu
            .menu_with_icon(
                t!("terminal.remote.rename"),
                Icon::new(CatalogIcon::Pencil),
                Box::new(RenameTerminal(id)),
            )
            .when(exists, |menu| {
                menu.menu_with_icon(
                    t!("terminal.remote.duplicate"),
                    Icon::new(IconName::Copy),
                    Box::new(DuplicateTerminal(id)),
                )
            });
        let protocol = self.protocol;
        menu = menu
            .when(protocol.sends_break(), |menu| {
                menu.menu_with_icon_and_disabled(
                    t!("terminal.remote.send_break"),
                    Icon::new(CatalogIcon::Zap),
                    Box::new(SendBreak(id)),
                    !self.running,
                )
            })
            .when(self.host_is_ssh, |menu| {
                menu.menu_with_icon(
                    t!("terminal.remote.open_sftp"),
                    Icon::new(CatalogIcon::FolderTree),
                    Box::new(OpenExplorer(host_id)),
                )
            })
            .when(protocol.has_address() && exists, |menu| {
                menu.menu_with_icon(
                    copy_host,
                    Icon::new(IconName::Copy),
                    Box::new(CopyHostAddress(host_id)),
                )
            });
        menu = menu.menu_with_icon(
            t!("terminal.remote.reconnect"),
            Icon::new(CatalogIcon::RefreshCw),
            Box::new(ReconnectTerminal(id)),
        );
        if self.editable && exists {
            menu = menu.menu_with_icon(
                t!("terminal.remote.edit_host"),
                Icon::new(CatalogIcon::Pencil),
                Box::new(EditHost(host_id)),
            );
        }
        let menu = menu.separator();
        close_tab_items(
            menu,
            CenterTab::Terminal(id),
            self.group.clone(),
            self.panel,
            cx,
        )
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let serial = self.host.kind() == ProtocolKind::Serial;
        let modes = self.serial_modes(cx);
        let hex = serial && modes.send_hex;
        let recording = self.terminal.read(cx).is_recording(cx);
        let path = self
            .terminal
            .read(cx)
            .recording_path(cx)
            .or_else(|| self.recording_path.clone())
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let dispatch = self.focus_handle(cx);
        let id = self.id;
        v_flex()
            .size_full()
            .child(div().flex_1().min_h_0().child(self.terminal.clone()))
            .when(recording, |view| {
                view.child(
                    h_flex()
                        .id(("terminal-recording-path", id.0))
                        .test_support()
                        .text_sm()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .px_2()
                        .py_1()
                        .text_color(cx.theme().muted_foreground)
                        .aria_label(path.clone())
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(t!("terminal.recording.path", path = path.clone())),
                        )
                        .child(
                            h_flex()
                                .gap_1()
                                .child(
                                    Button::new(("terminal-log-clear", id.0))
                                        .xsmall()
                                        .ghost()
                                        .label(t!("terminal.recording.clear"))
                                        .on_click({
                                            let dispatch = dispatch.clone();
                                            move |_, window, cx| {
                                                dispatch.dispatch_action(
                                                    &ClearTerminalLog(id),
                                                    window,
                                                    cx,
                                                )
                                            }
                                        }),
                                )
                                .child(
                                    Button::new(("terminal-log-delete", id.0))
                                        .xsmall()
                                        .ghost()
                                        .label(t!("terminal.recording.delete"))
                                        .on_click({
                                            let dispatch = dispatch.clone();
                                            move |_, window, cx| {
                                                dispatch.dispatch_action(
                                                    &DeleteTerminalLog(id),
                                                    window,
                                                    cx,
                                                )
                                            }
                                        }),
                                ),
                        ),
                )
            })
            .when(hex, |view| {
                view.child(
                    v_flex()
                        .p_2()
                        .gap_1()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("serial.hex.hint")),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div().flex_1().min_w_0().child(
                                        Input::new(&self.hex_input)
                                            .id(("serial-hex-input", id.0))
                                            .small(),
                                    ),
                                )
                                .child(
                                    Button::new(("serial-hex-send", id.0))
                                        .small()
                                        .label(t!("serial.hex.send"))
                                        .disabled(self.lifecycle(cx) != TerminalLifecycle::Running)
                                        .on_click(move |_, window, cx| {
                                            dispatch.dispatch_action(&SendSerialHex(id), window, cx)
                                        }),
                                ),
                        ),
                )
            })
    }
}
