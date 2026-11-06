use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    progress::Progress,
    scroll::ScrollableElement as _,
    spinner::Spinner,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::MonitorDetail;
use super::hardware::Bandwidth;
use super::linux::{self, Parsed, Parts};
use super::model::{
    DiskUsage, InterfaceRate, Memory, Snapshot, SystemInfo, Tracker, bars_per_row, format_rate,
};
use super::{MonitorConnector, MonitorLink};
use crate::app::{CatalogIcon, ToggleMonitorChannel, ToggleMonitorDetail};
use crate::host::{HostId, HostStore, MonitorChannel, MonitorSettings};
use crate::i18n::{t, tn};
use crate::shared::{
    amount, card, format_bytes, format_duration, format_percent, gauge, load_color, meter, numeric,
};
use crate::terminal::{ExecResult, ExecTarget, RemoteTerminalId, exec_answer};

/// Time between readings of the load. The second comes sooner: the CPU's
/// load and the network's rates need two, and the panel should not sit on
/// dashes.
const INTERVAL: Duration = Duration::from_secs(2);
const SECOND_READING: Duration = Duration::from_secs(1);
/// Disks are read this seldom: they fill slowly, and `df` is the slowest
/// part of a reading.
const DISK_INTERVAL: Duration = Duration::from_secs(30);
/// 系统监控: the CPU, memory, network and disks of the host of the SSH
/// terminal in front, read every two seconds while the panel shows.
///
/// Each reading runs `linux::command` beside the shell, on the terminal's
/// own connection: no second login, nothing asked again. What does not
/// change while connected is asked once per connection, the disks every
/// half minute, so most readings start no process on the host but the `sh`
/// running them. Nothing runs on the host while the panel is hidden. What
/// each terminal's host showed last is kept, so coming back to a terminal
/// shows its numbers at once.
pub struct MonitorPanel {
    store: Entity<HostStore>,
    connector: Option<Arc<dyn MonitorConnector>>,
    serial: Option<(HostId, MonitorSettings)>,
    native_ssh: bool,
    target: Option<ExecTarget>,
    active: bool,
    hosts: HashMap<RemoteTerminalId, HostState>,
    /// Every core's load, not only the first row of it.
    cores_unfolded: bool,
    /// How wide the row of per-core bars was last frame, which decides how
    /// many make the first row.
    cores_width: Rc<Cell<Option<Pixels>>>,
    /// Every interface, not only the main one.
    interfaces_unfolded: bool,
    /// The workspace's focus handle: the fold buttons dispatch on it, so
    /// they work whatever is focused.
    dispatch: FocusHandle,
    sampling: Option<Task<()>>,
    channels_unfolded: bool,
}

#[derive(Default)]
struct HostState {
    hardware_enabled: bool,
    generation: u64,
    paused: bool,
    source: String,
    updated: Option<String>,
    tracker: Tracker,
    snapshot: Option<Snapshot>,
    /// With the terminal's connection it was read on: a reconnect may land
    /// on another machine.
    system: Option<(u64, SystemInfo)>,
    /// With when they were read.
    disks: Option<(Instant, Vec<DiskUsage>)>,
    problem: Option<Problem>,
    hardware: Vec<Bandwidth>,
    hidden_channels: HashSet<String>,
}

impl HostState {
    /// What the next reading on `connection` asks for besides the load.
    fn parts_due(&self, connection: u64, now: Instant) -> Parts {
        let system = self
            .system
            .as_ref()
            .is_none_or(|(read_on, _)| *read_on != connection);
        let disks = system
            || self
                .disks
                .as_ref()
                .is_none_or(|(at, _)| now.saturating_duration_since(*at) >= DISK_INTERVAL);
        Parts { system, disks }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Problem {
    /// The terminal is not connected, so there is no connection to read on.
    NotConnected,
    /// Not Linux; `uname -s` when the host said it.
    Unsupported(Option<String>),
    /// The command did not run, or did not finish.
    Failed(String),
}

/// What one try at a reading came back with.
enum Outcome {
    Output(ExecResult),
    NotConnected,
    Paused,
}

struct SamplingGuard(Arc<AtomicBool>);
impl Drop for SamplingGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

struct SerialRoute {
    link: Option<Box<dyn MonitorLink>>,
    identity: Option<String>,
    retry_at: Instant,
    problem: Option<String>,
}

impl Default for SerialRoute {
    fn default() -> Self {
        Self {
            link: None,
            identity: None,
            retry_at: Instant::now(),
            problem: None,
        }
    }
}

impl MonitorPanel {
    pub fn new(dispatch: FocusHandle, store: Entity<HostStore>, _: &mut Context<Self>) -> Self {
        Self {
            store,
            connector: None,
            serial: None,
            native_ssh: false,
            target: None,
            active: false,
            hosts: HashMap::new(),
            cores_unfolded: false,
            cores_width: Rc::new(Cell::new(None)),
            interfaces_unfolded: false,
            dispatch,
            sampling: None,
            channels_unfolded: false,
        }
    }

    /// Unfold a folded part, or fold it away again. Stays so for every
    /// terminal's host.
    pub fn toggle(&mut self, detail: MonitorDetail, cx: &mut Context<Self>) {
        let unfolded = match detail {
            MonitorDetail::Cores => &mut self.cores_unfolded,
            MonitorDetail::Interfaces => &mut self.interfaces_unfolded,
            MonitorDetail::HardwareChannels => &mut self.channels_unfolded,
        };
        *unfolded = !*unfolded;
        cx.notify();
    }

    /// Read `target`'s host while `active`, the panel showing it.
    pub fn set_target(
        &mut self,
        target: Option<ExecTarget>,
        serial: Option<(HostId, MonitorSettings)>,
        native_ssh: bool,
        active: bool,
        cx: &mut Context<Self>,
    ) {
        if self.target == target
            && self.serial == serial
            && self.native_ssh == native_ssh
            && self.active == active
        {
            return;
        }
        self.target = target;
        self.serial = serial;
        self.native_ssh = native_ssh;
        self.active = active;
        self.restart(cx);
        cx.notify();
    }

    pub fn set_connector(&mut self, connector: Arc<dyn MonitorConnector>, cx: &mut Context<Self>) {
        self.connector = Some(connector);
        self.restart(cx);
    }

    pub fn toggle_channel(
        &mut self,
        terminal: RemoteTerminalId,
        name: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(host) = self.hosts.get_mut(&terminal)
            && host.hardware.iter().any(|value| value.name == name)
        {
            if !host.hidden_channels.remove(name) {
                host.hidden_channels.insert(name.to_owned());
            }
            cx.notify();
        }
    }

    /// Start reading the target's host over, or stop when there is nothing
    /// to show. Dropping the task stops it; a reading still on its way is
    /// answered to no one.
    fn restart(&mut self, cx: &mut Context<Self>) {
        self.sampling = None;
        let Some(target) = self.target.clone().filter(|_| self.active) else {
            return;
        };
        let serial = self.serial.clone();
        let native_ssh = self.native_ssh;
        if !native_ssh && let Some(host) = self.hosts.get_mut(&target.terminal) {
            host.hardware_enabled = false;
            host.hardware.clear();
        }
        let connector = self.connector.clone();
        let store = self.store.clone();
        let guard = SamplingGuard(Arc::new(AtomicBool::new(false)));
        self.sampling = Some(cx.spawn(async move |this, cx| {
            let guard = guard;
            let mut route = SerialRoute::default();
            let mut generation = None;
            let mut first = true;
            loop {
                let started = cx.background_executor().now();
                let request = this.update(cx, |this, cx| {
                    let view = target.view.upgrade()?;
                    let view = view.read(cx);
                    let connection = view.generation(cx);
                    let now = cx.background_executor().now();
                    let parts = this.hosts.get(&target.terminal).map_or(
                        Parts {
                            system: true,
                            disks: true,
                        },
                        |host| host.parts_due(connection, now),
                    );
                    Some((
                        parts,
                        connection,
                        view.lifecycle(cx) == crate::terminal::TerminalLifecycle::Running,
                    ))
                });
                let Ok(request) = request else { break };
                let Some((mut parts, connection, running)) = request else {
                    break;
                };
                if generation != Some(connection) {
                    route = SerialRoute::default();
                    generation = Some(connection);
                    parts = Parts {
                        system: true,
                        disks: true,
                    };
                }
                let mut source = String::new();
                let mut hardware_enabled = native_ssh && running;
                let outcome = if !running {
                    route = SerialRoute::default();
                    Outcome::NotConnected
                } else if let Some((_, settings)) = &serial {
                    let discover = connector.is_some()
                        && settings.channel != MonitorChannel::Serial
                        && route.link.is_none()
                        && Instant::now() >= route.retry_at;
                    let mut paused = false;
                    if discover {
                        let identity = match serial_command(
                            &target,
                            crate::serial::files::NETWORK_COMMAND.into(),
                            cx,
                        )
                        .await
                        {
                            Outcome::Output(Ok(identity)) => {
                                route.identity = Some(identity.clone());
                                Some(identity)
                            }
                            Outcome::Paused => {
                                paused = route.identity.is_none();
                                route.identity.clone()
                            }
                            Outcome::NotConnected => {
                                route.identity = None;
                                paused = true;
                                None
                            }
                            Outcome::Output(Err(error)) => {
                                route.problem = Some(error);
                                route.retry_at = Instant::now() + Duration::from_secs(30);
                                None
                            }
                        };
                        if let Some(identity) = identity {
                            let candidates = store.read_with(cx, |store, _| {
                                store
                                    .hosts()
                                    .iter()
                                    .filter(|host| host.is_ssh())
                                    .filter_map(|host| {
                                        store.login(host.id).map(|login| {
                                            (host.public_id.as_str().to_owned(), login)
                                        })
                                    })
                                    .collect::<Vec<_>>()
                            });
                            let connector = connector.clone().unwrap();
                            let settings = settings.clone();
                            let cancel = guard.0.clone();
                            let result = cx
                                .background_spawn(async move {
                                    connector.connect(&identity, &settings, &candidates, &cancel)
                                })
                                .await;
                            route.retry_at = Instant::now() + Duration::from_secs(30);
                            match result {
                                Ok(link) => {
                                    route.link = link;
                                    route.problem = None;
                                }
                                Err(error) => route.problem = Some(error),
                            }
                            parts = Parts {
                                system: true,
                                disks: true,
                            };
                        }
                    }
                    let command = linux::command_with_hardware(parts, route.link.is_some());
                    if let Some(mut link) = route.link.take() {
                        hardware_enabled = true;
                        source = link.label();
                        let cancel = guard.0.clone();
                        let (link, result) = cx
                            .background_spawn(async move {
                                let result = link.sample(&command, &cancel);
                                (link, result)
                            })
                            .await;
                        match result {
                            Ok(output) => {
                                route.link = Some(link);
                                Outcome::Output(Ok(output))
                            }
                            Err(error) => {
                                hardware_enabled = false;
                                route.problem = Some(error.clone());
                                route.retry_at = Instant::now() + Duration::from_secs(30);
                                if settings.channel == MonitorChannel::Auto {
                                    source = t!("monitor.route.serial").to_string();
                                    serial_command(
                                        &target,
                                        linux::command_with_hardware(
                                            Parts {
                                                system: true,
                                                disks: true,
                                            },
                                            false,
                                        ),
                                        cx,
                                    )
                                    .await
                                } else {
                                    Outcome::Output(Err(error))
                                }
                            }
                        }
                    } else if paused {
                        source = t!("monitor.route.serial").to_string();
                        Outcome::Paused
                    } else if matches!(
                        settings.channel,
                        MonitorChannel::Auto | MonitorChannel::Serial
                    ) {
                        source = t!("monitor.route.serial").to_string();
                        serial_command(&target, command, cx).await
                    } else {
                        Outcome::Output(Err(route
                            .problem
                            .clone()
                            .unwrap_or_else(|| t!("monitor.route.unavailable").to_string())))
                    }
                } else {
                    let reply = target
                        .view
                        .update(cx, |view, cx| {
                            view.exec(linux::command_with_hardware(parts, native_ssh), cx)
                        })
                        .ok()
                        .flatten();
                    match reply {
                        Some(reply) => exec_answer(reply, cx)
                            .await
                            .map_or(Outcome::NotConnected, Outcome::Output),
                        None => Outcome::NotConnected,
                    }
                };
                if this
                    .update(cx, |this, cx| {
                        if target
                            .view
                            .upgrade()
                            .is_some_and(|view| view.read(cx).generation(cx) == connection)
                        {
                            this.record(
                                target.terminal,
                                connection,
                                source,
                                hardware_enabled,
                                outcome,
                                cx,
                            )
                        }
                    })
                    .is_err()
                {
                    break;
                }
                let elapsed = cx
                    .background_executor()
                    .now()
                    .saturating_duration_since(started);
                let pause = sample_pause(first, elapsed);
                first = false;
                cx.background_executor().timer(pause).await;
            }
        }));
    }

    fn record(
        &mut self,
        terminal: RemoteTerminalId,
        connection: u64,
        source: String,
        hardware_enabled: bool,
        outcome: Outcome,
        cx: &mut Context<Self>,
    ) {
        let now = cx.background_executor().now();
        let host = self.hosts.entry(terminal).or_default();
        if host.generation != connection {
            let hidden_channels = std::mem::take(&mut host.hidden_channels);
            *host = HostState {
                generation: connection,
                hidden_channels,
                ..Default::default()
            };
        }
        host.source = source;
        host.hardware_enabled = hardware_enabled;
        if !hardware_enabled {
            host.hardware.clear();
        }
        host.paused = matches!(outcome, Outcome::Paused);
        host.problem = match outcome {
            Outcome::Paused => None,
            Outcome::NotConnected => Some(Problem::NotConnected),
            Outcome::Output(Err(error)) => Some(Problem::Failed(error)),
            Outcome::Output(Ok(output)) => match linux::parse(&output) {
                Parsed::Reading(mut reading) => {
                    host.updated = Some(chrono::Local::now().format("%H:%M:%S").to_string());
                    if let Some(hardware) = reading.hardware.take().filter(|_| hardware_enabled) {
                        host.hardware = hardware;
                    }
                    if let Some(system) = reading.system.take() {
                        host.system = Some((connection, system));
                    }
                    if let Some(disks) = reading.disks.take() {
                        host.disks = Some((now, disks));
                    }
                    host.snapshot = Some(host.tracker.update(*reading, now));
                    None
                }
                Parsed::Unsupported(system) => Some(Problem::Unsupported(system)),
            },
        };
        if host.problem.is_some() {
            host.hardware.clear();
        }
        cx.notify();
    }
}

async fn serial_command(target: &ExecTarget, command: String, cx: &mut AsyncApp) -> Outcome {
    let reply = target
        .view
        .update(cx, |view, cx| view.monitor(command, cx))
        .ok()
        .flatten();
    match reply {
        Some(reply) => match exec_answer(reply, cx).await {
            Some(Err(error)) if error == crate::terminal::MONITOR_PAUSED => Outcome::Paused,
            Some(result) => Outcome::Output(result),
            None => Outcome::NotConnected,
        },
        None => Outcome::NotConnected,
    }
}

fn sample_pause(first: bool, elapsed: Duration) -> Duration {
    let interval = if first { SECOND_READING } else { INTERVAL };
    interval.saturating_sub(elapsed)
}

impl Render for MonitorPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = self
            .target
            .as_ref()
            .and_then(|target| self.hosts.get(&target.terminal));
        let snapshot = host.and_then(|host| host.snapshot.as_ref());
        let system = host.and_then(|host| host.system.as_ref().map(|(_, system)| system));
        let disks = host.and_then(|host| host.disks.as_ref().map(|(_, disks)| disks.as_slice()));
        let problem = host.and_then(|host| host.problem.clone());
        let hardware: Vec<_> = host.into_iter().flat_map(|host| &host.hardware).collect();
        let hidden = host.map(|host| &host.hidden_channels);
        let hardware_terminal = self.target.as_ref().map(|target| target.terminal);
        let interfaces_unfolded = self.interfaces_unfolded;
        let cores = CoresLayout {
            unfolded: self.cores_unfolded,
            per_row: self
                .cores_width
                .get()
                .map(|width| bars_per_row(f32::from(width), f32::from(window.rem_size()))),
            width: self.cores_width.clone(),
            panel: cx.entity_id(),
        };
        let dispatch = &self.dispatch;
        // The scrollbar goes on a layer beside the scrolled content, which
        // `overflow_y_scrollbar` lays out: on the content itself it would
        // scroll away with it, and stop matching where the content is.
        let content = v_flex()
            .size_full()
            .p_3()
            .gap_3()
            .when_some(
                host.filter(|host| !host.source.is_empty()),
                |panel, host| {
                    let message = t!(
                        "monitor.route.paused",
                        time = host.updated.as_deref().unwrap_or("—")
                    );
                    panel.child(
                        h_flex()
                            .h_5()
                            .gap_2()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .id("monitor-channel")
                                    .test_support()
                                    .aria_label(host.source.clone())
                                    .child(host.source.clone()),
                            )
                            .when(host.paused, |row| {
                                row.child(
                                    div()
                                        .id("monitor-paused")
                                        .test_support()
                                        .flex_shrink_0()
                                        .aria_label(message.clone())
                                        .tooltip(move |window, cx| {
                                            Tooltip::new(message.clone()).build(window, cx)
                                        })
                                        .child(t!("monitor.route.paused_short")),
                                )
                            })
                            .when(!host.paused, |row| {
                                row.child(div().flex_shrink_0().child(t!("monitor.route.live")))
                            }),
                    )
                },
            )
            .map(|panel| match (snapshot, problem) {
                // Numbers from before a failed reading still say something;
                // the failure goes above them.
                (Some(snapshot), problem @ (None | Some(Problem::Failed(_)))) => panel
                    .when_some(problem, |panel, problem| {
                        panel.child(render_problem(&problem, cx))
                    })
                    .when_some(system, |panel, system| {
                        panel.child(render_system(system, snapshot.uptime, cx))
                    })
                    .child(render_cpu(system, snapshot, &cores, dispatch, cx))
                    .child(render_memory(&snapshot.memory, cx))
                    .when(!hardware.is_empty(), |panel| {
                        panel.child(render_hardware(
                            &hardware,
                            hidden.unwrap(),
                            hardware_terminal.unwrap(),
                            self.channels_unfolded,
                            dispatch,
                            cx,
                        ))
                    })
                    .child(render_network(snapshot, interfaces_unfolded, dispatch, cx))
                    .when_some(disks, |panel, disks| panel.child(render_disks(disks, cx))),
                (_, Some(problem)) => panel.child(render_problem(&problem, cx)),
                (None, None) if host.is_some_and(|host| host.paused) => panel,
                (None, None) => panel.child(
                    h_flex()
                        .id("monitor-loading")
                        .test_support()
                        .gap_2()
                        .justify_center()
                        .py_8()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(Spinner::new().small())
                        .child(t!("tools.reading")),
                ),
            })
            .overflow_y_scrollbar();
        div()
            .id("monitor")
            .test_support()
            .size_full()
            .child(content)
    }
}

fn render_hardware(
    values: &[&Bandwidth],
    hidden: &HashSet<String>,
    terminal: RemoteTerminalId,
    expanded: bool,
    dispatch: &FocusHandle,
    cx: &App,
) -> impl IntoElement {
    let toggle = dispatch.clone();
    card(
        "monitor-hardware",
        IconName::Cpu,
        t!("monitor.hardware.title"),
        Some(
            Button::new("monitor-hardware-channels")
                .ghost()
                .xsmall()
                .label(t!("monitor.hardware.channels"))
                .on_click(move |_, window, cx| {
                    toggle.dispatch_action(
                        &ToggleMonitorDetail(MonitorDetail::HardwareChannels),
                        window,
                        cx,
                    )
                })
                .into_any_element(),
        ),
        cx,
    )
    .when(expanded, |card| {
        card.child(
            h_flex()
                .flex_wrap()
                .gap_2()
                .children(values.iter().map(|value| {
                    let dispatch = dispatch.clone();
                    let name = value.name.clone();
                    Checkbox::new(SharedString::from(format!("monitor-channel:{name}")))
                        .small()
                        .label(name.clone())
                        .checked(!hidden.contains(&name))
                        .on_click(move |_, window, cx| {
                            dispatch.dispatch_action(
                                &ToggleMonitorChannel((terminal, name.clone())),
                                window,
                                cx,
                            )
                        })
                })),
        )
    })
    .child(
        v_flex().gap_2().children(
            values
                .iter()
                .filter(|value| !hidden.contains(&value.name))
                .map(|value| {
                    let text = format!("{:.2} {}", value.value, value.unit);
                    h_flex()
                        .id(SharedString::from(format!(
                            "monitor-hardware:{}",
                            value.name
                        )))
                        .test_support()
                        .aria_label(format!("{} {text}", value.name))
                        .justify_between()
                        .gap_3()
                        .text_sm()
                        .child(value.name.clone())
                        .child(numeric(text, cx))
                }),
        ),
    )
}

fn render_problem(problem: &Problem, cx: &App) -> impl IntoElement {
    let (text, color) = match problem {
        Problem::NotConnected => (
            t!("monitor.panel.not_connected"),
            cx.theme().muted_foreground,
        ),
        Problem::Unsupported(Some(system)) => (
            t!("monitor.panel.unsupported_system", system = system),
            cx.theme().muted_foreground,
        ),
        Problem::Unsupported(None) => {
            (t!("monitor.panel.unsupported"), cx.theme().muted_foreground)
        }
        Problem::Failed(error) => (t!("tools.read_failed", error = error), cx.theme().danger),
    };
    div()
        .id("monitor-message")
        .test_support()
        .aria_label(text.clone())
        .text_sm()
        .text_color(color)
        .child(text)
}

/// A card's quiet note at the end of its title row: 「1 核」.
fn note(text: SharedString, cx: &App) -> AnyElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
        .into_any_element()
}

/// In place of the note, when the card folds something away: 「8 核 ⌄」.
fn unfold_button(
    id: &'static str,
    label: SharedString,
    detail: MonitorDetail,
    unfolded: bool,
    tooltip: SharedString,
    dispatch: &FocusHandle,
) -> AnyElement {
    let dispatch = dispatch.clone();
    Button::new(id)
        .ghost()
        .xsmall()
        .label(label)
        .child(Icon::new(if unfolded {
            IconName::ChevronUp
        } else {
            IconName::ChevronDown
        }))
        .toggled(unfolded)
        .tooltip(tooltip)
        .on_click(move |_, window, cx| {
            dispatch.dispatch_action(&ToggleMonitorDetail(detail), window, cx)
        })
        .into_any_element()
}

fn render_system(system: &SystemInfo, uptime: Option<Duration>, cx: &App) -> impl IntoElement {
    let field = |id: &'static str, label: SharedString, value: String| {
        v_flex()
            .min_w_0()
            .gap_0p5()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(
                div()
                    .id(id)
                    .test_support()
                    .aria_label(value.clone())
                    .text_sm()
                    .child(value),
            )
    };
    card(
        "monitor-system",
        CatalogIcon::Monitor,
        t!("monitor.system.title"),
        None,
        cx,
    )
    .child(
        div()
            .grid()
            .grid_cols(2)
            .gap_x_4()
            .gap_y_3()
            .child(field(
                "monitor-host",
                t!("monitor.system.host"),
                system.host_name.clone(),
            ))
            .child(field(
                "monitor-arch",
                t!("monitor.system.arch"),
                system.arch.clone(),
            ))
            .child(field(
                "monitor-os",
                t!("monitor.system.os"),
                system.os.clone(),
            ))
            .child(field(
                "monitor-uptime",
                t!("monitor.system.uptime"),
                uptime.map(format_duration).unwrap_or_default(),
            )),
    )
}

/// A ring with the share in its middle, as big as the eye needs to catch a
/// change across the room.
/// How the per-core bars lay out this frame.
struct CoresLayout {
    /// Every row, not only the first.
    unfolded: bool,
    /// How many bars make a row, from last frame's width; `None` until the
    /// row has been laid out once.
    per_row: Option<usize>,
    /// Where the row records its width for the next frame.
    width: Rc<Cell<Option<Pixels>>>,
    /// The panel to redraw when that width changes.
    panel: EntityId,
}

fn render_cpu(
    system: Option<&SystemInfo>,
    snapshot: &Snapshot,
    layout: &CoresLayout,
    dispatch: &FocusHandle,
    cx: &App,
) -> impl IntoElement {
    let model = system
        .map(|system| system.cpu_model.clone())
        .filter(|model| !model.is_empty());
    let total = snapshot.cpu.as_ref().map(|load| load.total);
    let cores = snapshot
        .cpu
        .as_ref()
        .map(|load| load.cores.as_slice())
        .filter(|cores| cores.len() > 1);
    // A host whose cores fill more than the one row shown can unfold the
    // rest; most hosts fit, and have no button.
    let more = layout
        .per_row
        .filter(|per_row| cores.is_some_and(|cores| cores.len() > *per_row));
    let count = tn!("monitor.cpu.cores", snapshot.cores);
    let meta = if more.is_some() {
        unfold_button(
            "monitor-cores-toggle",
            count,
            MonitorDetail::Cores,
            layout.unfolded,
            if layout.unfolded {
                t!("monitor.cpu.fold_cores")
            } else {
                t!("monitor.cpu.unfold_cores")
            },
            dispatch,
        )
    } else {
        note(count, cx)
    };
    let shown = cores.map(|cores| match more {
        Some(per_row) if !layout.unfolded => &cores[..per_row],
        _ => cores,
    });
    card("monitor-cpu", IconName::Cpu, "CPU", Some(meta), cx)
        .when_some(model, |card, model| {
            card.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(model),
            )
        })
        .child(
            h_flex()
                .gap_4()
                .child(gauge("monitor-cpu-gauge", total, cx))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_3()
                        .child(meter(
                            "monitor-cpu-usage",
                            t!("monitor.cpu.average"),
                            total,
                            cx,
                        ))
                        .when_some(shown, |column, shown| {
                            column.child(render_cores(shown, layout, cx))
                        }),
                ),
        )
}

/// The cores' loads as a column each, wrapping onto more rows the more
/// there are: 64 take three rows at the sidebar's usual width. Folded, only
/// the first row; it measures its width for the next frame to know how many
/// make one. Pointing at a bar tells its core's number and share.
fn render_cores(cores: &[f32], layout: &CoresLayout, cx: &App) -> impl IntoElement {
    let core = |index: usize, load: f32| {
        t!(
            "monitor.cpu.core",
            index = index,
            load = format_percent(load)
        )
    };
    let label = cores
        .iter()
        .enumerate()
        .map(|(index, load)| core(index, *load))
        .reduce(|list, next| t!("monitor.cpu.core_list", list = list, core = next))
        .unwrap_or_default();
    let (width, panel) = (layout.width.clone(), layout.panel);
    h_flex()
        .id("monitor-cpu-cores")
        .test_support()
        .aria_label(label)
        .relative()
        .w_full()
        .flex_wrap()
        .gap_0p5()
        // Before the first measurement, a row's height keeps the rest out.
        .when(!layout.unfolded, |row| {
            row.max_h(rems(1.5)).overflow_hidden()
        })
        .children(cores.iter().enumerate().map(|(index, load)| {
            let color = load_color(*load, cx);
            let tooltip = core(index, *load);
            div()
                .id(("monitor-core", index as u64))
                .test_support()
                .relative()
                .w(rems(0.375))
                .h(rems(1.5))
                .rounded_sm()
                .bg(color.opacity(0.2))
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .w_full()
                        .h(relative(load.clamp(0., 100.) / 100.))
                        .rounded_sm()
                        .bg(color),
                )
        }))
        .child(
            canvas(
                move |bounds, window, _| {
                    if width.get() != Some(bounds.size.width) {
                        width.set(Some(bounds.size.width));
                        window.on_next_frame(move |_, cx| cx.notify(panel));
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
}

/// 「421.02 MB / 973.25 MB」, the total quieter.
fn render_memory(memory: &Memory, cx: &App) -> impl IntoElement {
    card(
        "monitor-memory",
        IconName::MemoryStick,
        t!("monitor.memory.title"),
        None,
        cx,
    )
    .child(
        h_flex()
            .gap_4()
            .child(gauge("monitor-memory-gauge", Some(memory.percent()), cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_3()
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(meter(
                                "monitor-memory-usage",
                                t!("monitor.memory.physical"),
                                Some(memory.percent()),
                                cx,
                            ))
                            .child(amount(
                                "monitor-memory-amount",
                                memory.used,
                                memory.total,
                                cx,
                            )),
                    )
                    .when(memory.swap_total > 0, |column| {
                        column.child(
                            v_flex()
                                .gap_1p5()
                                .child(meter(
                                    "monitor-swap-usage",
                                    t!("monitor.memory.swap"),
                                    Some(memory.swap_percent()),
                                    cx,
                                ))
                                .child(amount(
                                    "monitor-swap-amount",
                                    memory.swap_used,
                                    memory.swap_total,
                                    cx,
                                )),
                        )
                    }),
            ),
    )
}

fn render_network(
    snapshot: &Snapshot,
    unfolded: bool,
    dispatch: &FocusHandle,
    cx: &App,
) -> impl IntoElement {
    let busy = snapshot.busy_interfaces();
    let meta = (busy.len() > 1).then(|| {
        unfold_button(
            "monitor-interfaces-toggle",
            tn!("monitor.network.interfaces", busy.len()),
            MonitorDetail::Interfaces,
            unfolded,
            if unfolded {
                t!("monitor.network.fold")
            } else {
                t!("monitor.network.unfold")
            },
            dispatch,
        )
    });
    let shown: Vec<&InterfaceRate> = if unfolded {
        busy
    } else {
        snapshot.main_interface().into_iter().collect()
    };
    let rate = |rate: Option<f64>| rate.map_or("—".into(), format_rate);
    card(
        "monitor-network",
        IconName::Network,
        t!("monitor.network.title"),
        meta,
        cx,
    )
    .when(shown.is_empty(), |card| {
        card.child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(t!("monitor.network.none")),
        )
    })
    .children(shown.into_iter().map(|interface| {
        let (up, down) = (rate(interface.transmitted), rate(interface.received));
        h_flex()
            .id(SharedString::from(format!(
                "monitor-interface:{}",
                interface.name
            )))
            .test_support()
            .aria_label(t!("monitor.network.label", up = up, down = down))
            .gap_3()
            .text_sm()
            .child(
                numeric(interface.name.clone(), cx)
                    .flex_1()
                    .min_w_0()
                    .truncate(),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_3()
                    .text_xs()
                    .child(direction(IconName::ArrowUp, up, cx))
                    .child(direction(IconName::ArrowDown, down, cx)),
            )
    }))
}

fn direction(icon: IconName, rate: String, cx: &App) -> impl IntoElement {
    h_flex()
        .gap_0p5()
        .child(
            Icon::new(icon)
                .xsmall()
                .text_color(cx.theme().muted_foreground),
        )
        .child(numeric(rate, cx))
}

fn render_disks(disks: &[DiskUsage], cx: &App) -> impl IntoElement {
    card(
        "monitor-disks",
        IconName::HardDrive,
        t!("monitor.disks.title"),
        None,
        cx,
    )
    .when(disks.is_empty(), |card| {
        card.child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(t!("monitor.disks.none")),
        )
    })
    .children(disks.iter().map(|disk| {
        let percent = disk.percent();
        let color = load_color(percent, cx);
        let (used, total) = (format_bytes(disk.used), format_bytes(disk.total));
        v_flex()
            .id(SharedString::from(format!("monitor-disk:{}", disk.mount)))
            .test_support()
            .aria_label(t!(
                "monitor.disks.label",
                percent = format_percent(percent),
                used = used,
                total = total
            ))
            .gap_1p5()
            .child(
                h_flex()
                    .gap_3()
                    .text_sm()
                    .child(numeric(disk.mount.clone(), cx).flex_1().min_w_0())
                    .child(
                        numeric(format_percent(percent), cx)
                            .flex_shrink_0()
                            .text_color(color)
                            .font_weight(FontWeight::SEMIBOLD),
                    ),
            )
            .child(
                Progress::new(SharedString::from(format!(
                    "monitor-disk-bar:{}",
                    disk.mount
                )))
                .small()
                .value(percent)
                .color(color),
            )
            .child(
                numeric(format!("{used} / {total}"), cx)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
            )
    }))
}

#[cfg(test)]
mod polling_tests {
    use super::sample_pause;
    use std::time::Duration;

    #[test]
    fn sampling_time_is_part_of_the_two_second_refresh_period() {
        assert_eq!(sample_pause(false, Duration::ZERO), Duration::from_secs(2));
        assert_eq!(
            sample_pause(false, Duration::from_secs(1)),
            Duration::from_secs(1)
        );
        assert_eq!(
            sample_pause(false, Duration::from_millis(1500)),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn slow_samples_do_not_add_another_two_second_delay() {
        assert_eq!(sample_pause(false, Duration::from_secs(2)), Duration::ZERO);
        assert_eq!(sample_pause(false, Duration::from_secs(5)), Duration::ZERO);
    }

    #[test]
    fn the_initial_cpu_warmup_keeps_its_shorter_interval() {
        assert_eq!(sample_pause(true, Duration::ZERO), Duration::from_secs(1));
        assert_eq!(
            sample_pause(true, Duration::from_millis(500)),
            Duration::from_millis(500)
        );
        assert_eq!(sample_pause(true, Duration::from_secs(2)), Duration::ZERO);
    }
}
