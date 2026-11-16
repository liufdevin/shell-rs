use crate::{
    app::UtilityAction,
    i18n::t,
    terminal::{LineTimestamp, TerminalView},
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _,
    button::{Button, ButtonGroup},
    scroll::ScrollableElement as _,
    switch::Switch,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UtilityCommand {
    SelectService(crate::host::MonitorChannel),
    ChooseServiceAdb,
    SetBoundary(bool),
    Toggle(bool),
    Pick(EntityId, LineTimestamp),
    Clear,
    Copy,
    CopyDifference,
}

pub struct UtilitiesPanel {
    view: Option<WeakEntity<TerminalView>>,
    observer: Option<Subscription>,
    route_task: Option<Task<()>>,
}

impl UtilitiesPanel {
    pub fn new() -> Self {
        Self {
            view: None,
            observer: None,
            route_task: None,
        }
    }
    pub fn set_target(&mut self, view: Option<WeakEntity<TerminalView>>, cx: &mut Context<Self>) {
        if self.view == view {
            return;
        }
        self.observer = view
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .map(|view| cx.observe(&view, |_, _, cx| cx.notify()));
        self.view = view;
        self.route_task = None;
        if let Some(view) = self.view.clone() {
            self.route_task = Some(cx.spawn(async move |this, cx| {
                let mut previous = String::new();
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(500))
                        .await;
                    let Ok(label) = view.read_with(cx, |view, cx| view.serial_service_label(cx))
                    else {
                        break;
                    };
                    if label != previous {
                        previous = label;
                        if this.update(cx, |_, cx| cx.notify()).is_err() {
                            break;
                        }
                    }
                }
            }));
        }
        cx.notify();
    }
    pub fn target(&self) -> Option<Entity<TerminalView>> {
        self.view.as_ref()?.upgrade()
    }
}

impl Default for UtilitiesPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for UtilitiesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = self.target();
        let enabled = view
            .as_ref()
            .is_some_and(|view| view.read(cx).timestamp_enabled(cx));
        let supported = view
            .as_ref()
            .is_some_and(|view| view.read(cx).has_timestamp_target(cx));
        let selected_start = view
            .as_ref()
            .is_some_and(|view| view.read(cx).timestamp_selection_available(false, cx));
        let selected_end = view
            .as_ref()
            .is_some_and(|view| view.read(cx).timestamp_selection_available(true, cx));
        let copy_available = view
            .as_ref()
            .is_some_and(|view| view.read(cx).timestamp_copy_available(cx));
        let marks = view
            .as_ref()
            .map(|view| view.read(cx).timestamp_marks(cx))
            .unwrap_or([None, None]);
        let difference = view
            .as_ref()
            .and_then(|view| view.read(cx).timestamp_difference(cx));
        let label = |stamp: Option<LineTimestamp>| {
            stamp
                .map(LineTimestamp::full)
                .unwrap_or_else(|| t!("utilities.unselected").to_string())
        };
        v_flex()
            .id("utilities-panel")
            .test_support()
            .p_3()
            .gap_3()
            .size_full()
            .when(
                view.as_ref()
                    .is_some_and(|view| view.read(cx).has_serial_services()),
                |panel| {
                    let view = view.as_ref().unwrap().read(cx);
                    let status = view.serial_service_label(cx);
                    let selected = view.serial_service_channel();
                    panel.child(
                        v_flex()
                            .gap_2()
                            .child(div().text_sm().child(t!("monitor.route.shared")))
                            .child(
                                div()
                                    .id("serial-service-status")
                                    .test_support()
                                    .aria_label(status.clone())
                                    .text_xs()
                                    .child(status),
                            )
                            .child(ButtonGroup::new("serial-service-choices").children(
                                crate::host::MonitorChannel::ALL.into_iter().map(|channel| {
                                    let (id, label) = match channel {
                                        crate::host::MonitorChannel::Auto => {
                                            ("serial-service-auto", t!("monitor.route.redetect"))
                                        }
                                        crate::host::MonitorChannel::Ssh => {
                                            ("serial-service-ssh", "SSH".into())
                                        }
                                        crate::host::MonitorChannel::Adb => {
                                            ("serial-service-adb", "ADB".into())
                                        }
                                        crate::host::MonitorChannel::Serial => {
                                            ("serial-service-serial", t!("monitor.route.serial"))
                                        }
                                    };
                                    Button::new(id)
                                        .small()
                                        .label(label)
                                        .selected(selected == channel)
                                        .on_click(move |_, window, cx| {
                                            window.dispatch_action(
                                                Box::new(UtilityAction(
                                                    UtilityCommand::SelectService(channel),
                                                )),
                                                cx,
                                            )
                                        })
                                }),
                            ))
                            .child(
                                Button::new("serial-service-choose-adb")
                                    .small()
                                    .label(t!("monitor.route.choose_adb"))
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(
                                            Box::new(UtilityAction(
                                                UtilityCommand::ChooseServiceAdb,
                                            )),
                                            cx,
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(t!("monitor.route.shared_help")),
                            ),
                    )
                },
            )
            .child(
                Switch::new("utilities-timestamps")
                    .label(t!("utilities.timestamps"))
                    .checked(enabled)
                    .disabled(!supported)
                    .on_click(|enabled, window, cx| {
                        window.dispatch_action(
                            Box::new(UtilityAction(UtilityCommand::Toggle(*enabled))),
                            cx,
                        )
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("utilities.help")),
            )
            .child(div().child(t!("utilities.start", time = label(marks[0]))))
            .child(div().child(t!("utilities.end", time = label(marks[1]))))
            .child(
                div()
                    .id("timestamp-difference")
                    .test_support()
                    .role(Role::Status)
                    .aria_label(
                        difference
                            .clone()
                            .unwrap_or_else(|| t!("utilities.select_help").to_string()),
                    )
                    .child(
                        difference
                            .clone()
                            .unwrap_or_else(|| t!("utilities.select_help").to_string()),
                    ),
            )
            .child(
                Button::new("timestamp-set-start")
                    .small()
                    .label(t!("utilities.set_start"))
                    .disabled(!enabled || !selected_start)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(
                            Box::new(UtilityAction(UtilityCommand::SetBoundary(false))),
                            cx,
                        )
                    }),
            )
            .child(
                Button::new("timestamp-set-end")
                    .small()
                    .label(t!("utilities.set_end"))
                    .disabled(!enabled || !selected_end)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(
                            Box::new(UtilityAction(UtilityCommand::SetBoundary(true))),
                            cx,
                        )
                    }),
            )
            .child(
                Button::new("timestamp-clear")
                    .small()
                    .label(t!("utilities.clear"))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(UtilityAction(UtilityCommand::Clear)), cx)
                    }),
            )
            .child(
                Button::new("timestamp-copy-difference")
                    .small()
                    .label(t!("utilities.copy_difference"))
                    .disabled(difference.is_none())
                    .on_click(|_, window, cx| {
                        window.dispatch_action(
                            Box::new(UtilityAction(UtilityCommand::CopyDifference)),
                            cx,
                        )
                    }),
            )
            .child(
                Button::new("timestamp-copy")
                    .small()
                    .label(t!("utilities.copy"))
                    .disabled(!supported || !copy_available)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(UtilityAction(UtilityCommand::Copy)), cx)
                    }),
            )
            .overflow_y_scrollbar()
    }
}
