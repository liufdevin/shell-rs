use std::sync::Arc;

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IndexPath, Selectable as _, Sizable as _,
    WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    dock::{BasePanel, Panel, PanelEvent, TabGroup},
    h_flex,
    input::{Input, InputState},
    menu::PopupMenu,
    searchable_list::SearchableListItem,
    select::{Select, SelectEvent, SelectState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    CatalogIcon, CenterTab, CloseSerialAssistant, SERIAL_ICON, SerialAssistantAction,
};
use crate::host::{
    COMMON_BAUD_RATES, DataBits, HostStore, KindLabel as _, Parity, SerialLine, SerialSettings,
    StopBits, TerminalEncoding,
};
use crate::i18n::t;
use crate::shared::{ClosableTabTitle, close_tab_items};
use crate::terminal::{SharedSerialTerminalTransportProvider, TerminalLifecycle, TerminalView};

use super::{SerialPortSource, SerialToolCommand, SerialToolsPanel};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SerialAssistantTabCommand {
    ToggleConnection,
    RefreshPorts,
    ClearOutput,
    ReceiveHex(bool),
    SendHex(bool),
    Timestamps(bool),
    Follow(bool),
    ToggleFilter,
    SaveLog,
    Tool(SerialToolCommand),
}

#[derive(Clone, Copy)]
pub(crate) enum SerialAssistantPanelEvent {
    Activated,
    Closed,
}

#[derive(Clone)]
struct Parameter<Value> {
    value: Value,
    label: SharedString,
}

impl<Value: Clone + PartialEq + 'static> SearchableListItem for Parameter<Value> {
    type Value = Value;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.value
    }
}

type Choices<Value> = SelectState<Vec<Parameter<Value>>>;
const TIMESTAMP_TARGET: &str = "serial-assistant:timestamps";

fn choices<Value: Copy + PartialEq + 'static>(
    values: impl IntoIterator<Item = Value>,
    selected: Value,
    label: impl Fn(Value) -> SharedString,
    window: &mut Window,
    cx: &mut App,
) -> Entity<Choices<Value>> {
    let items: Vec<_> = values
        .into_iter()
        .map(|value| Parameter {
            value,
            label: label(value),
        })
        .collect();
    let index = items
        .iter()
        .position(|item| item.value == selected)
        .map(IndexPath::new);
    cx.new(|cx| SelectState::new(items, index, window, cx))
}

pub(crate) struct SerialAssistantPanel {
    dispatch: FocusHandle,
    focus: FocusHandle,
    group: Option<WeakEntity<TabGroup>>,
    store: Entity<HostStore>,
    tools: Entity<SerialToolsPanel>,
    ports: Arc<dyn SerialPortSource>,
    transport: SharedSerialTerminalTransportProvider,
    terminal: Option<Entity<TerminalView>>,
    port: Entity<SelectState<Vec<SharedString>>>,
    baud: Entity<Choices<Option<u32>>>,
    custom_baud: Entity<InputState>,
    parity: Entity<Choices<Parity>>,
    data_bits: Entity<Choices<DataBits>>,
    stop_bits: Entity<Choices<StopBits>>,
    encoding: Entity<Choices<TerminalEncoding>>,
    port_scan: Option<Task<()>>,
    error: Option<SharedString>,
    receive_hex: bool,
    follow_output: bool,
    log_save: Option<Task<()>>,
    log_status: Option<(SharedString, bool)>,
    _subscriptions: Vec<Subscription>,
    terminal_observer: Option<Subscription>,
}

impl SerialAssistantPanel {
    pub(crate) fn new(
        dispatch: FocusHandle,
        store: Entity<HostStore>,
        ports: Arc<dyn SerialPortSource>,
        transport: SharedSerialTerminalTransportProvider,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let defaults = SerialSettings::default();
        let port = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx).searchable(true));
        let baud = choices(
            COMMON_BAUD_RATES
                .into_iter()
                .map(Some)
                .chain([Some(2_000_000), None]),
            Some(defaults.baud),
            |rate| {
                rate.map_or_else(
                    || t!("serial.assistant.custom_baud"),
                    |rate| rate.to_string().into(),
                )
            },
            window,
            cx,
        );
        let custom_baud = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(defaults.baud.to_string())
                .validate(|value, _| {
                    value.len() <= 10 && value.bytes().all(|byte| byte.is_ascii_digit())
                })
        });
        let parity = choices(
            Parity::ALL,
            defaults.parity,
            |parity| parity.label(),
            window,
            cx,
        );
        let data_bits = choices(
            DataBits::ALL,
            defaults.data_bits,
            |bits| bits.value().to_string().into(),
            window,
            cx,
        );
        let stop_bits = choices(
            StopBits::ALL,
            defaults.stop_bits,
            |bits| bits.value().to_string().into(),
            window,
            cx,
        );
        let encoding = choices(
            TerminalEncoding::ALL,
            TerminalEncoding::default(),
            |encoding| encoding.label().into(),
            window,
            cx,
        );
        let tools = cx
            .new(|cx| SerialToolsPanel::new_assistant(dispatch.clone(), store.clone(), window, cx));
        let subscriptions = vec![
            cx.subscribe_in(
                &port,
                window,
                |this, _, _: &SelectEvent<Vec<SharedString>>, window, cx| {
                    this.port_changed(window, cx);
                },
            ),
            cx.subscribe(
                &baud,
                |this, _, _: &SelectEvent<Vec<Parameter<Option<u32>>>>, cx| {
                    this.error = None;
                    cx.notify();
                },
            ),
            cx.observe(&tools, |_, _, cx| cx.notify()),
            cx.observe(&store, |this, _, cx| {
                this.tools.update(cx, |_, cx| cx.notify())
            }),
        ];
        let mut this = Self {
            dispatch,
            focus: cx.focus_handle(),
            group: None,
            store,
            tools,
            ports,
            transport,
            terminal: None,
            port,
            baud,
            custom_baud,
            parity,
            data_bits,
            stop_bits,
            encoding,
            port_scan: None,
            error: None,
            receive_hex: false,
            follow_output: true,
            log_save: None,
            log_status: None,
            _subscriptions: subscriptions,
            terminal_observer: None,
        };
        this.refresh_ports(window, cx);
        this
    }

    pub(crate) fn tab_group(&self) -> Option<WeakEntity<TabGroup>> {
        self.group.clone()
    }

    pub(crate) fn terminal(&self) -> Option<&Entity<TerminalView>> {
        self.terminal.as_ref()
    }

    pub(crate) fn tools(&self) -> &Entity<SerialToolsPanel> {
        &self.tools
    }

    pub(crate) fn load_command(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.tools
            .update(cx, |tools, cx| tools.load_saved_command(id, window, cx))
    }

    pub(crate) fn sender_focus(&self, cx: &App) -> FocusHandle {
        self.tools.read(cx).sender_focus(cx)
    }

    pub(crate) fn show(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        cx.emit(SerialAssistantPanelEvent::Activated);
    }

    fn lifecycle(&self, cx: &App) -> Option<TerminalLifecycle> {
        self.terminal
            .as_ref()
            .map(|terminal| terminal.read(cx).lifecycle(cx))
    }

    fn connected_or_opening(&self, cx: &App) -> bool {
        matches!(
            self.lifecycle(cx),
            Some(TerminalLifecycle::Starting | TerminalLifecycle::Running)
        )
    }

    fn refresh_ports(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.port_scan.is_some() || self.connected_or_opening(cx) {
            return;
        }
        let source = self.ports.clone();
        self.port_scan = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { source.ports() }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.port_scan = None;
                match result {
                    Ok(ports) => {
                        let previous = this.port.read(cx).selected_value().cloned();
                        let ports: Vec<SharedString> = ports.into_iter().map(Into::into).collect();
                        let index = previous
                            .as_ref()
                            .and_then(|previous| ports.iter().position(|port| port == previous))
                            .or_else(|| (!ports.is_empty()).then_some(0));
                        this.port.update(cx, |select, cx| {
                            select.set_items(ports, window, cx);
                            select.set_selected_index(index.map(IndexPath::new), window, cx);
                        });
                        this.port_changed(window, cx);
                    }
                    Err(error) => {
                        this.error = Some(t!("serial.assistant.scan_failed", error = error))
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn port_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let port = self.port.read(cx).selected_value().cloned();
        self.tools.update(cx, |tools, cx| {
            tools.set_assistant_port(self.store.clone(), port.as_deref(), window, cx);
        });
        self.error = None;
        cx.notify();
    }

    fn selected_encoding(&self, cx: &App) -> TerminalEncoding {
        self.encoding
            .read(cx)
            .selected_value()
            .copied()
            .unwrap_or_default()
    }

    fn using_custom_baud(&self, cx: &App) -> bool {
        self.baud.read(cx).selected_value() == Some(&None)
    }

    fn selected_line(&self, cx: &App) -> Result<SerialLine, SharedString> {
        let device = self
            .port
            .read(cx)
            .selected_value()
            .ok_or_else(|| t!("serial.assistant.select_port"))?
            .to_string();
        let baud = if self.using_custom_baud(cx) {
            self.custom_baud.read(cx).value().parse::<u32>().ok()
        } else {
            self.baud.read(cx).selected_value().copied().flatten()
        }
        .filter(|rate| *rate > 0)
        .ok_or_else(|| t!("serial.tool.invalid_baud"))?;
        let defaults = SerialSettings::default();
        let settings = SerialSettings {
            baud,
            parity: self
                .parity
                .read(cx)
                .selected_value()
                .copied()
                .unwrap_or(defaults.parity),
            data_bits: self
                .data_bits
                .read(cx)
                .selected_value()
                .copied()
                .unwrap_or(defaults.data_bits),
            stop_bits: self
                .stop_bits
                .read(cx)
                .selected_value()
                .copied()
                .unwrap_or(defaults.stop_bits),
            ..defaults
        };
        Ok(SerialLine { device, settings })
    }

    fn toggle_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.connected_or_opening(cx) {
            self.tools.update(cx, |tools, cx| tools.stop_sending(cx));
            if let Some(terminal) = &self.terminal {
                terminal.update(cx, |terminal, cx| {
                    terminal.stop(&t!("terminal.notice.disconnected"), cx)
                });
            }
            return;
        }
        if self.port_scan.is_some() {
            return;
        }
        let line = match self.selected_line(cx) {
            Ok(line) => line,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        self.error = None;
        self.log_status = None;
        let factory = self.transport.factory_for(&line);
        let encoding = self.selected_encoding(cx);
        let send_hex = self.tools.read(cx).assistant_send_hex();
        let modes = crate::terminal::SerialDataModes {
            receive_hex: self.receive_hex,
            send_hex,
            ..Default::default()
        };
        let terminal = if let Some(terminal) = &self.terminal {
            terminal.update(cx, |terminal, cx| {
                terminal.set_connection_encoding(encoding, cx);
                terminal.set_serial_modes(modes, cx);
                terminal.restart_with_factory(factory, cx);
            });
            terminal.clone()
        } else {
            let timestamps = self.store.read(cx).timestamps_enabled(TIMESTAMP_TARGET);
            let terminal = cx.new(|cx| {
                let mut terminal = TerminalView::new_serial_assistant(
                    factory, encoding, modes, timestamps, window, cx,
                );
                terminal.bind_timestamps(self.store.clone(), TIMESTAMP_TARGET.into(), None, cx);
                terminal.set_output_follow(self.follow_output, cx);
                terminal
            });
            self.terminal_observer = Some(cx.observe(&terminal, |_, _, cx| cx.notify()));
            self.terminal = Some(terminal.clone());
            terminal
        };
        self.tools.update(cx, |tools, cx| {
            tools.set_target(Some(terminal.downgrade()), window, cx);
        });
        window.focus(&terminal.read(cx).focus_handle(), cx);
    }

    pub(crate) fn command(
        &mut self,
        command: &SerialAssistantTabCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            SerialAssistantTabCommand::ToggleConnection => self.toggle_connection(window, cx),
            SerialAssistantTabCommand::RefreshPorts => self.refresh_ports(window, cx),
            SerialAssistantTabCommand::ClearOutput => {
                if let Some(terminal) = &self.terminal {
                    terminal.update(cx, |terminal, cx| terminal.clear(cx));
                }
            }
            SerialAssistantTabCommand::ReceiveHex(enabled) => {
                self.receive_hex = *enabled;
                if let Some(terminal) = &self.terminal {
                    terminal.update(cx, |terminal, cx| {
                        let mut modes = terminal.serial_modes(cx);
                        modes.receive_hex = *enabled;
                        terminal.set_serial_modes(modes, cx);
                    });
                }
            }
            SerialAssistantTabCommand::SendHex(enabled) => self
                .tools
                .update(cx, |tools, cx| tools.set_assistant_send_hex(*enabled, cx)),
            SerialAssistantTabCommand::Timestamps(enabled) => self.store.update(cx, |store, cx| {
                store.set_timestamps_enabled(TIMESTAMP_TARGET.into(), None, *enabled, cx);
            }),
            SerialAssistantTabCommand::Follow(enabled) => {
                self.follow_output = *enabled;
                if let Some(terminal) = &self.terminal {
                    terminal.update(cx, |terminal, cx| terminal.set_output_follow(*enabled, cx));
                }
            }
            SerialAssistantTabCommand::ToggleFilter => {
                if let Some(terminal) = &self.terminal {
                    terminal.update(cx, |terminal, cx| terminal.toggle_log_filter(window, cx));
                }
            }
            SerialAssistantTabCommand::SaveLog => self.save_log(window, cx),
            SerialAssistantTabCommand::Tool(command) => self.tool_command(command, window, cx),
        }
        cx.notify();
    }

    fn save_log(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.log_save.is_some() {
            return;
        }
        let Some(terminal) = &self.terminal else {
            return;
        };
        let export = terminal.read(cx).log_export(cx);
        self.log_status = None;
        let filename = format!(
            "serial-{}.log",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        );
        let chosen = cx.prompt_for_new_path(
            &dirs::download_dir().unwrap_or_else(std::env::temp_dir),
            Some(&filename),
        );
        self.log_save = Some(cx.spawn_in(window, async move |this, cx| {
            let status = match chosen.await {
                Ok(Ok(Some(path))) => {
                    let result = cx
                        .background_spawn(async move {
                            let text = export();
                            if text.is_empty() {
                                return Ok(false);
                            }
                            std::fs::write(path, text).map(|()| true)
                        })
                        .await;
                    Some(match result {
                        Ok(true) => (t!("device_logs.exported"), false),
                        Ok(false) => (t!("serial.assistant.no_log"), false),
                        Err(error) => (t!("device_logs.export_failed", error = error), true),
                    })
                }
                Ok(Err(error)) => Some((t!("device_logs.export_failed", error = error), true)),
                _ => None,
            };
            let _ = this.update(cx, |this, cx| {
                this.log_save = None;
                this.log_status = status;
                cx.notify();
            });
        }));
    }

    fn tool_command(
        &mut self,
        command: &SerialToolCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let encoding = self.selected_encoding(cx);
        self.tools.update(cx, |tools, cx| {
            tools.set_assistant_encoding(encoding);
            tools.command(command, window, cx);
        });
    }

    fn button(
        &self,
        id: &'static str,
        label: SharedString,
        command: SerialAssistantTabCommand,
    ) -> Button {
        let dispatch = self.dispatch.clone();
        Button::new(id)
            .small()
            .label(label)
            .on_click(move |_, window, cx| {
                dispatch.dispatch_action(&SerialAssistantAction(command.clone()), window, cx);
            })
    }

    fn parameter<Value: Clone + PartialEq + 'static>(
        id: &'static str,
        label: SharedString,
        state: &Entity<Choices<Value>>,
        locked: bool,
        cx: &App,
    ) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(label.clone()),
            )
            .child(
                Select::new(state)
                    .id(id)
                    .small()
                    .w(rems(if id == "serial-assistant-baud" {
                        9.
                    } else {
                        6.5
                    }))
                    .disabled(locked)
                    .accessibility_label(label),
            )
    }

    fn parameters(&self, locked: bool, cx: &App) -> impl IntoElement {
        h_flex()
            .gap_x_3()
            .gap_y_2()
            .flex_wrap()
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Select::new(&self.port)
                            .id("serial-assistant-port")
                            .small()
                            .w(rems(10.))
                            .placeholder(if self.port_scan.is_some() {
                                t!("host.dialog.looking_for_ports")
                            } else {
                                t!("serial.assistant.select_port")
                            })
                            .disabled(locked || self.port_scan.is_some())
                            .accessibility_label(t!("host.dialog.serial_device")),
                    )
                    .child(
                        self.button(
                            "serial-assistant-refresh",
                            t!("host.dialog.look_for_ports_again"),
                            SerialAssistantTabCommand::RefreshPorts,
                        )
                        .label(SharedString::default())
                        .icon(CatalogIcon::RefreshCw)
                        .ghost()
                        .accessibility_label(t!("host.dialog.look_for_ports_again"))
                        .tooltip(t!("host.dialog.look_for_ports_again"))
                        .disabled(locked || self.port_scan.is_some()),
                    ),
            )
            .child(Self::parameter(
                "serial-assistant-baud",
                t!("host.dialog.baud_rate"),
                &self.baud,
                locked,
                cx,
            ))
            .when(self.using_custom_baud(cx), |row| {
                row.child(
                    Input::new(&self.custom_baud)
                        .id("serial-assistant-custom-baud")
                        .small()
                        .w(rems(7.))
                        .disabled(locked)
                        .aria_label(t!("serial.assistant.custom_baud")),
                )
            })
            .child(Self::parameter(
                "serial-assistant-encoding",
                t!("serial.assistant.encoding"),
                &self.encoding,
                locked,
                cx,
            ))
            .child(Self::parameter(
                "serial-assistant-data-bits",
                t!("host.dialog.data_bits"),
                &self.data_bits,
                locked,
                cx,
            ))
            .child(Self::parameter(
                "serial-assistant-parity",
                t!("host.dialog.parity"),
                &self.parity,
                locked,
                cx,
            ))
            .child(Self::parameter(
                "serial-assistant-stop-bits",
                t!("host.dialog.stop_bits"),
                &self.stop_bits,
                locked,
                cx,
            ))
    }
}

impl Render for SerialAssistantPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let locked = self.connected_or_opening(cx);
        let timestamps = self.store.read(cx).timestamps_enabled(TIMESTAMP_TARGET);
        let filter_active = self
            .terminal
            .as_ref()
            .is_some_and(|terminal| terminal.read(cx).log_filter_active());
        let lifecycle = self.lifecycle(cx);
        let status = match &lifecycle {
            Some(TerminalLifecycle::Running) => t!("serial.assistant.connected"),
            Some(TerminalLifecycle::Starting) => t!("serial.assistant.connecting"),
            Some(TerminalLifecycle::Failed(_)) => t!("serial.assistant.connection_failed"),
            _ => t!("serial.assistant.disconnected"),
        };
        let status_color = if matches!(&lifecycle, Some(TerminalLifecycle::Running)) {
            cx.theme().success
        } else if matches!(&lifecycle, Some(TerminalLifecycle::Failed(_))) {
            cx.theme().danger
        } else {
            cx.theme().muted_foreground
        };
        let check = |id,
                     label: SharedString,
                     checked,
                     command: fn(bool) -> SerialAssistantTabCommand| {
            let dispatch = self.dispatch.clone();
            Checkbox::new(id)
                .small()
                .w_auto()
                .label(label)
                .checked(checked)
                .on_click(move |value, window, cx| {
                    dispatch.dispatch_action(&SerialAssistantAction(command(*value)), window, cx)
                })
        };
        let has_port = self.port.read(cx).selected_value().is_some();
        let empty = if self.port_scan.is_some() {
            t!("host.dialog.looking_for_ports")
        } else if has_port {
            t!("serial.assistant.waiting")
        } else {
            t!("serial.assistant.no_ports_hint")
        };
        let error = self.error.clone().or_else(|| match lifecycle {
            Some(TerminalLifecycle::Failed(error)) => Some(error.into()),
            _ => None,
        });
        let diagnostics = self
            .terminal
            .as_ref()
            .and_then(|terminal| terminal.read(cx).serial_diagnostics(cx));
        let counts = t!(
            "serial.diagnostics.count",
            received = diagnostics.as_ref().map_or(0, |counts| counts.received),
            transmitted = diagnostics.as_ref().map_or(0, |counts| counts.transmitted),
            errors = diagnostics.as_ref().map_or(0, |counts| counts.errors)
        );
        v_flex()
            .id("serial-assistant-page")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .min_w_0()
            .p_3()
            .gap_2()
            .bg(cx.theme().background)
            .child(
                div()
                    .id("serial-assistant-output")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .when_some(self.terminal.clone(), |view, terminal| view.child(terminal))
                    .when(self.terminal.is_none(), |view| {
                        view.bg(cx.theme().muted).child(
                            v_flex()
                                .size_full()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .p_4()
                                .text_center()
                                .child(
                                    Icon::default()
                                        .path(SERIAL_ICON)
                                        .text_color(cx.theme().muted_foreground),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(empty),
                                ),
                        )
                    }),
            )
            .child(
                v_flex()
                    .flex_shrink_0()
                    .gap_2()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                self.button(
                                    "serial-assistant-connect",
                                    if locked {
                                        t!("serial.assistant.disconnect")
                                    } else {
                                        t!("serial.assistant.connect")
                                    },
                                    SerialAssistantTabCommand::ToggleConnection,
                                )
                                .when(!locked, |button| button.primary())
                                .disabled(!locked && (!has_port || self.port_scan.is_some())),
                            )
                            .child(
                                self.button(
                                    "serial-assistant-clear",
                                    t!("serial.tool.clear"),
                                    SerialAssistantTabCommand::ClearOutput,
                                )
                                .ghost()
                                .disabled(self.terminal.is_none()),
                            )
                            .child(check(
                                "serial-assistant-receive-hex",
                                t!("serial.mode.receive_hex"),
                                self.receive_hex,
                                SerialAssistantTabCommand::ReceiveHex,
                            ))
                            .child(check(
                                "serial-assistant-send-hex",
                                t!("serial.mode.send_hex"),
                                self.tools.read(cx).assistant_send_hex(),
                                SerialAssistantTabCommand::SendHex,
                            ))
                            .child(
                                div()
                                    .id("serial-assistant-connection-status")
                                    .test_support()
                                    .role(Role::Status)
                                    .aria_label(status.clone())
                                    .text_color(status_color)
                                    .text_xs()
                                    .child(status),
                            )
                            .child(
                                div()
                                    .id("serial-wire-count")
                                    .test_support()
                                    .aria_label(counts.clone())
                                    .ml_auto()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .child(counts),
                            ),
                    )
                    .child(self.parameters(locked, cx))
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(check(
                                "serial-assistant-follow",
                                t!("serial.assistant.follow"),
                                self.follow_output,
                                SerialAssistantTabCommand::Follow,
                            ))
                            .child(check(
                                "serial-assistant-timestamps",
                                t!("serial.assistant.timestamps"),
                                timestamps,
                                SerialAssistantTabCommand::Timestamps,
                            ))
                            .child(
                                self.button(
                                    "serial-assistant-filter",
                                    t!("serial.assistant.filter"),
                                    SerialAssistantTabCommand::ToggleFilter,
                                )
                                .ghost()
                                .selected(filter_active)
                                .disabled(self.terminal.is_none()),
                            )
                            .child(
                                self.button(
                                    "serial-assistant-save-log",
                                    t!("serial.assistant.save_log"),
                                    SerialAssistantTabCommand::SaveLog,
                                )
                                .ghost()
                                .icon(CatalogIcon::Download)
                                .disabled(self.terminal.is_none() || self.log_save.is_some()),
                            )
                            .when_some(self.log_status.clone(), |row, (status, error)| {
                                row.child(
                                    div()
                                        .id("serial-assistant-log-status")
                                        .test_support()
                                        .role(Role::Status)
                                        .aria_label(status.clone())
                                        .text_xs()
                                        .text_color(if error {
                                            cx.theme().danger
                                        } else {
                                            cx.theme().muted_foreground
                                        })
                                        .child(status),
                                )
                            }),
                    )
                    .when_some(error, |row, error| {
                        row.child(
                            div()
                                .id("serial-assistant-error")
                                .test_support()
                                .role(Role::Status)
                                .aria_label(error.clone())
                                .text_sm()
                                .text_color(cx.theme().danger)
                                .child(error),
                        )
                    }),
            )
            .child(
                div()
                    .min_h_0()
                    .min_w_0()
                    .flex_shrink_0()
                    .child(self.tools.clone()),
            )
    }
}

impl EventEmitter<PanelEvent> for SerialAssistantPanel {}
impl EventEmitter<SerialAssistantPanelEvent> for SerialAssistantPanel {}

impl Focusable for SerialAssistantPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BasePanel for SerialAssistantPanel {
    fn panel_name(&self) -> &'static str {
        "SerialAssistantPanel"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(terminal) = &self.terminal {
            terminal.update(cx, |terminal, cx| terminal.set_tab_active(active, cx));
        }
        if active {
            if !window.has_active_dialog(cx) {
                self.show(window, cx);
            }
        } else {
            self.tools.update(cx, |tools, cx| tools.stop_sending(cx));
        }
    }

    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.group = Some(group);
    }

    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.group = None;
        if crate::shared::is_panel_transfer(cx.entity_id(), cx) {
            return;
        }
        self.port_scan = None;
        self.log_save = None;
        self.tools.update(cx, |tools, cx| tools.stop_sending(cx));
        if let Some(terminal) = self.terminal.take() {
            terminal.update(cx, |terminal, cx| terminal.shutdown(cx));
        }
        cx.emit(SerialAssistantPanelEvent::Closed);
    }
}

impl Panel for SerialAssistantPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let group = self.group.clone();
        let panel = cx.entity_id();
        ClosableTabTitle::new(
            "serial-assistant-tab",
            Icon::default().path(SERIAL_ICON),
            t!("serial.assistant.title"),
        )
        .closable("close-serial-assistant", Box::new(CloseSerialAssistant))
        .context_menu(move |menu, _, cx| {
            close_tab_items(menu, CenterTab::SerialAssistant, group.clone(), panel, cx)
        })
    }

    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        close_tab_items(
            menu,
            CenterTab::SerialAssistant,
            self.group.clone(),
            cx.entity_id(),
            cx,
        )
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}
