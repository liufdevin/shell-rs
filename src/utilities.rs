use crate::{
    app::UtilityAction,
    i18n::t,
    terminal::{LineTimestamp, TerminalView},
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, button::Button, switch::Switch, v_flex,
};
use gpui_kit::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UtilityCommand {
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
}

impl UtilitiesPanel {
    pub fn new() -> Self {
        Self {
            view: None,
            observer: None,
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
    }
}
