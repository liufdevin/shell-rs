use crate::{
    app::StopCommandGroup,
    host::SnippetWait,
    i18n::t,
    terminal::{CommandGroupStatus, TerminalView},
};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, button::Button, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub(crate) fn command_group_controls(
    view: &TerminalView,
    dispatch: FocusHandle,
    cx: &App,
) -> impl IntoElement {
    let status = view.command_group_status(cx);
    let message = match &status {
        CommandGroupStatus::Idle => None,
        CommandGroupStatus::Running {
            name,
            step,
            total,
            wait,
        } => {
            let wait = match wait {
                SnippetWait::Immediate => t!("snippets.group.sending"),
                SnippetWait::Delay { milliseconds } => t!(
                    "snippets.group.waiting_seconds",
                    seconds = f64::from(*milliseconds) / 1000.
                ),
                SnippetWait::Prompt { text, .. } => {
                    t!("snippets.group.waiting_prompt", text = text)
                }
            };
            Some(t!(
                "snippets.group.progress",
                name = name,
                step = step,
                total = total,
                wait = wait
            ))
        }
        CommandGroupStatus::Sent => Some(t!("snippets.group.sent")),
        CommandGroupStatus::Stopped => Some(t!("snippets.group.stopped")),
        CommandGroupStatus::TimedOut { step } => Some(t!("snippets.group.timed_out", step = step)),
        CommandGroupStatus::Unavailable => Some(t!("snippets.group.unavailable")),
    };
    v_flex()
        .flex_shrink_0()
        .when_some(message, |panel, message| {
            panel.px_3().pt_3().gap_2().child(
                div()
                    .id("command-group-status")
                    .test_support()
                    .aria_label(message.clone())
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(message),
            )
        })
        .when(status.is_active(), |panel| {
            panel.child(
                Button::new("command-group-stop")
                    .small()
                    .label(t!("snippets.group.stop"))
                    .on_click(move |_, window, cx| {
                        dispatch.dispatch_action(&StopCommandGroup, window, cx)
                    }),
            )
        })
}
