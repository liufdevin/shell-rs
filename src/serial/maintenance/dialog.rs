use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use gpui_kit::{
    component::{
        ActiveTheme as _, Disableable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        checkbox::Checkbox,
        h_flex,
        scroll::ScrollableElement as _,
        v_flex,
    },
    prelude::FluentBuilder as _,
    *,
};

use super::{
    MAX_REMOVALS, MaintenanceFailure, RemovalState, SerialDevice, SerialMaintenanceBackend,
    SerialRemoval, validate_selection,
};
use crate::{
    app::SerialMaintenanceAction,
    host::{HostStore, ProtocolKind},
    i18n::t,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SerialMaintenanceCommand {
    Open,
    Scan,
    Select(String, bool),
    Page(bool),
    Review,
    Back,
    Remove,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Idle,
    Scanning,
    Ready,
    Reviewing,
    Removing,
    Complete,
}

pub struct SerialMaintenance {
    store: Entity<HostStore>,
    dispatch: FocusHandle,
    backend: Option<Arc<dyn SerialMaintenanceBackend>>,
    devices: Vec<SerialDevice>,
    selected: HashSet<String>,
    confirmed: Vec<SerialDevice>,
    results: HashMap<String, SerialRemoval>,
    phase: Phase,
    error: Option<MaintenanceFailure>,
    page: usize,
    scroll: ScrollHandle,
    task: Option<Task<()>>,
    _subscription: Subscription,
}

impl SerialMaintenance {
    pub fn new(store: Entity<HostStore>, dispatch: FocusHandle, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&store, |_, _, cx| cx.notify());
        Self {
            store,
            dispatch,
            backend: None,
            devices: Vec::new(),
            selected: HashSet::new(),
            confirmed: Vec::new(),
            results: HashMap::new(),
            phase: Phase::Idle,
            error: None,
            page: 0,
            scroll: ScrollHandle::new(),
            task: None,
            _subscription: subscription,
        }
    }

    pub fn set_backend(
        &mut self,
        backend: Arc<dyn SerialMaintenanceBackend>,
        cx: &mut Context<Self>,
    ) {
        if self.is_busy() {
            return;
        }
        self.backend = Some(backend);
        self.devices.clear();
        self.selected.clear();
        self.confirmed.clear();
        self.results.clear();
        self.phase = Phase::Idle;
        self.error = None;
        cx.notify();
    }

    fn is_busy(&self) -> bool {
        matches!(self.phase, Phase::Scanning | Phase::Removing)
    }
    fn is_supported(&self) -> bool {
        self.backend
            .as_ref()
            .is_some_and(|backend| backend.is_supported())
    }

    pub fn command(&mut self, command: SerialMaintenanceCommand, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        match command {
            SerialMaintenanceCommand::Scan if self.is_supported() => self.scan(cx),
            SerialMaintenanceCommand::Select(id, checked) if self.phase == Phase::Ready => {
                if self
                    .devices
                    .iter()
                    .any(|device| device.instance_id() == id && device.is_candidate())
                {
                    if checked {
                        self.selected.insert(id);
                    } else {
                        self.selected.remove(&id);
                    }
                }
            }
            SerialMaintenanceCommand::Page(next) => {
                let pages = self.devices.len().div_ceil(20).max(1);
                self.page = if next {
                    (self.page + 1).min(pages - 1)
                } else {
                    self.page.saturating_sub(1)
                };
                self.scroll.set_offset(Point::default());
            }
            SerialMaintenanceCommand::Review if self.phase == Phase::Ready => {
                let plan: Vec<_> = self
                    .devices
                    .iter()
                    .filter(|device| self.selected.contains(device.instance_id()))
                    .cloned()
                    .collect();
                if validate_selection(&plan).is_ok() {
                    self.confirmed = plan;
                    self.page = 0;
                    self.scroll.set_offset(Point::default());
                    self.phase = Phase::Reviewing;
                }
            }
            SerialMaintenanceCommand::Back if self.phase == Phase::Reviewing => {
                self.confirmed.clear();
                self.phase = Phase::Ready;
            }
            SerialMaintenanceCommand::Remove
                if self.phase == Phase::Reviewing && self.is_supported() =>
            {
                self.remove(cx)
            }
            _ => {}
        }
        cx.notify();
    }

    fn scan(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone().unwrap();
        self.phase = Phase::Scanning;
        self.error = None;
        self.selected.clear();
        self.confirmed.clear();
        self.results.clear();
        self.devices.clear();
        self.page = 0;
        self.scroll.set_offset(Point::default());
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(backend.scan());
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = loop {
                match receiver.try_recv() {
                    Ok(result) => break result,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        break Err(MaintenanceFailure::Failed("scan_worker_stopped".into()));
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(50))
                            .await
                    }
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.phase = Phase::Ready;
                match result {
                    Ok(devices) => {
                        let mut ids = HashSet::new();
                        if devices.len() > 256
                            || devices.iter().any(|device| {
                                !ids.insert(device.instance_id().to_ascii_uppercase())
                            })
                        {
                            this.error =
                                Some(MaintenanceFailure::Failed("invalid_scan_response".into()));
                        } else {
                            this.devices = devices
                                .into_iter()
                                .filter(SerialDevice::is_candidate)
                                .collect();
                            this.devices.sort_by_key(|device| {
                                device
                                    .port()
                                    .strip_prefix("COM")
                                    .unwrap_or_default()
                                    .parse::<u16>()
                                    .unwrap_or_default()
                            });
                        }
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        }));
    }

    fn remove(&mut self, cx: &mut Context<Self>) {
        if validate_selection(&self.confirmed).is_err() {
            return;
        }
        let plan = self.confirmed.clone();
        let backend = self.backend.clone().unwrap();
        self.phase = Phase::Removing;
        self.error = None;
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(backend.remove(&plan));
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = loop {
                match receiver.try_recv() {
                    Ok(result) => break result,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        break Err(MaintenanceFailure::Failed("removal_worker_stopped".into()));
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(50))
                            .await
                    }
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.phase = Phase::Complete;
                match result {
                    Ok(results) => {
                        for device in &this.confirmed {
                            let result = results
                                .iter()
                                .find(|result| result.instance_id() == device.instance_id())
                                .cloned()
                                .unwrap_or_else(|| {
                                    SerialRemoval::new(
                                        device.instance_id().into(),
                                        RemovalState::Unverified,
                                        String::new(),
                                    )
                                });
                            this.results.insert(device.instance_id().into(), result);
                        }
                    }
                    Err(error) => this.error = Some(error),
                }
                this.selected.clear();
                this.confirmed.clear();
                cx.notify();
            });
        }));
    }

    fn button(
        &self,
        id: &'static str,
        label: SharedString,
        command: SerialMaintenanceCommand,
        disabled: bool,
    ) -> Button {
        let dispatch = self.dispatch.clone();
        Button::new(id)
            .label(label)
            .disabled(disabled)
            .on_click(move |_, window, cx| {
                dispatch.dispatch_action(&SerialMaintenanceAction(command.clone()), window, cx)
            })
    }

    pub fn open(entity: Entity<Self>, window: &mut Window, cx: &mut App) {
        window.open_dialog(cx, move |dialog, window, _| {
            dialog
                .title(t!("serial.maintenance.title"))
                .width(
                    rems(56.)
                        .to_pixels(window.rem_size())
                        .min(window.viewport_size().width - rems(4.).to_pixels(window.rem_size())),
                )
                .overlay_closable(false)
                .max_h(window.viewport_size().height * 0.85)
                .child(entity.clone())
        });
    }
}

impl Render for SerialMaintenance {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.is_busy();
        let supported = self.is_supported();
        let status = match self.phase {
            Phase::Idle => t!("serial.maintenance.idle"),
            Phase::Scanning => t!("serial.maintenance.scanning"),
            Phase::Ready => t!(
                "serial.maintenance.count",
                count = self.devices.len(),
                selected = self.selected.len()
            ),
            Phase::Reviewing => t!("serial.maintenance.confirm", count = self.confirmed.len()),
            Phase::Removing => t!("serial.maintenance.removing"),
            Phase::Complete => t!("serial.maintenance.completed"),
        };
        let references: HashMap<_, Vec<_>> = self
            .store
            .read(cx)
            .hosts()
            .iter()
            .filter(|host| host.kind() == ProtocolKind::Serial)
            .fold(HashMap::new(), |mut references, host| {
                references
                    .entry(
                        host.address
                            .trim()
                            .trim_start_matches("\\\\.\\")
                            .to_ascii_uppercase(),
                    )
                    .or_default()
                    .push(host.name.clone());
                references
            });
        let displayed: Vec<_> = if matches!(self.phase, Phase::Reviewing | Phase::Removing) {
            self.confirmed.iter().collect()
        } else {
            self.devices.iter().collect()
        };
        let pages = displayed.len().div_ceil(20).max(1);
        let page = self.page.min(pages - 1);
        let rows = displayed
            .into_iter()
            .skip(page * 20)
            .take(20)
            .map(|device| {
                let id = device.instance_id().to_owned();
                let dispatch = self.dispatch.clone();
                let result = self.results.get(&id);
                let selection = self.selected.contains(&id);
                v_flex()
                    .id(SharedString::from(format!("serial-maintenance-row:{id}")))
                    .test_support()
                    .gap_1()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex().gap_3().child(
                            Checkbox::new(SharedString::from(format!(
                                "serial-maintenance-select:{id}"
                            )))
                            .label(SharedString::from(format!(
                                "{} · {}",
                                device.port(),
                                device.name()
                            )))
                            .checked(selection)
                            .disabled(self.phase != Phase::Ready)
                            .on_click(move |checked, window, cx| {
                                dispatch.dispatch_action(
                                    &SerialMaintenanceAction(SerialMaintenanceCommand::Select(
                                        id.clone(),
                                        *checked,
                                    )),
                                    window,
                                    cx,
                                )
                            }),
                        ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(device.instance_id().to_owned())),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(references.get(device.port()).map_or_else(
                                || t!("serial.maintenance.no_references"),
                                |hosts| {
                                    t!(
                                        "serial.maintenance.references",
                                        hosts = hosts
                                            .iter()
                                            .map(|name| name.as_ref())
                                            .collect::<Vec<_>>()
                                            .join("、")
                                    )
                                },
                            )),
                    )
                    .when_some(result, |row, result| {
                        row.child(
                            div()
                                .id(SharedString::from(format!(
                                    "serial-maintenance-result:{}",
                                    device.instance_id()
                                )))
                                .test_support()
                                .aria_label(result_label(result))
                                .text_sm()
                                .text_color(
                                    if matches!(
                                        result.state(),
                                        RemovalState::Failed | RemovalState::Unverified
                                    ) {
                                        cx.theme().danger
                                    } else {
                                        cx.theme().foreground
                                    },
                                )
                                .child(result_label(result)),
                        )
                    })
            });
        v_flex()
            .id("serial-maintenance")
            .test_support()
            .gap_3()
            .min_w_0()
            .child(div().text_sm().child(t!("serial.maintenance.description")))
            .child(
                div()
                    .id("serial-maintenance-status")
                    .test_support()
                    .role(Role::Status)
                    .aria_label(status.clone())
                    .text_sm()
                    .child(status),
            )
            .when(!supported, |body| {
                body.child(
                    div()
                        .id("serial-maintenance-unsupported")
                        .test_support()
                        .child(t!("serial.maintenance.unsupported")),
                )
            })
            .when_some(self.error.as_ref(), |body, error| {
                body.child(
                    div()
                        .id("serial-maintenance-error")
                        .test_support()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .aria_label(error_label(error))
                        .child(error_label(error)),
                )
            })
            .when(self.phase == Phase::Reviewing, |body| {
                body.child(
                    div()
                        .id("serial-maintenance-warning")
                        .test_support()
                        .text_sm()
                        .text_color(cx.theme().warning)
                        .child(t!("serial.maintenance.warning")),
                )
            })
            .child(
                div()
                    .id("serial-maintenance-rows")
                    .max_h(
                        (window.viewport_size().height * 0.42)
                            .max(rems(6.).to_pixels(window.rem_size())),
                    )
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .vertical_scrollbar(&self.scroll)
                    .children(rows),
            )
            .when(
                self.phase == Phase::Ready && self.devices.is_empty() && self.error.is_none(),
                |body| body.child(div().child(t!("serial.maintenance.empty"))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(self.button(
                        "serial-maintenance-previous",
                        t!("serial.maintenance.previous"),
                        SerialMaintenanceCommand::Page(false),
                        busy || page == 0,
                    ))
                    .child(format!("{}/{}", page + 1, pages))
                    .child(self.button(
                        "serial-maintenance-next",
                        t!("serial.maintenance.next"),
                        SerialMaintenanceCommand::Page(true),
                        busy || page + 1 >= pages,
                    )),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        self.button(
                            "serial-maintenance-scan",
                            t!("serial.maintenance.scan"),
                            SerialMaintenanceCommand::Scan,
                            busy || !supported || self.phase == Phase::Reviewing,
                        )
                        .outline(),
                    )
                    .when(self.phase == Phase::Reviewing, |row| {
                        row.child(self.button(
                            "serial-maintenance-back",
                            t!("common.cancel"),
                            SerialMaintenanceCommand::Back,
                            false,
                        ))
                        .child(
                            self.button(
                                "serial-maintenance-remove",
                                t!("serial.maintenance.remove", count = self.confirmed.len()),
                                SerialMaintenanceCommand::Remove,
                                false,
                            )
                            .danger(),
                        )
                    })
                    .when(self.phase != Phase::Reviewing, |row| {
                        row.child(self.button(
                            "serial-maintenance-review",
                            t!("serial.maintenance.review"),
                            SerialMaintenanceCommand::Review,
                            busy || self.phase != Phase::Ready
                                || self.selected.is_empty()
                                || self.selected.len() > MAX_REMOVALS
                                || !supported,
                        ))
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("serial.maintenance.limit", count = MAX_REMOVALS)),
            )
    }
}

fn error_label(error: &MaintenanceFailure) -> SharedString {
    match error {
        MaintenanceFailure::Unsupported => t!("serial.maintenance.unsupported"),
        MaintenanceFailure::Cancelled => t!("serial.maintenance.cancelled"),
        MaintenanceFailure::TimedOut => t!("serial.maintenance.timeout"),
        MaintenanceFailure::Failed(detail) => t!("serial.maintenance.failed", detail = detail),
    }
}

fn result_label(result: &SerialRemoval) -> SharedString {
    match result.state() {
        RemovalState::Removed => t!("serial.maintenance.removed"),
        RemovalState::Present => t!("serial.maintenance.present"),
        RemovalState::Gone => t!("serial.maintenance.gone"),
        RemovalState::Changed => t!("serial.maintenance.changed"),
        RemovalState::RebootRequired => t!("serial.maintenance.reboot"),
        RemovalState::Unverified => t!("serial.maintenance.unverified"),
        RemovalState::Failed => t!("serial.maintenance.failed", detail = result.detail()),
    }
}
