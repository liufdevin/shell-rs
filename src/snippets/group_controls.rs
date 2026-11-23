use crate::{
    app::StopCommandGroup,
    i18n::t,
    terminal::{RemoteTerminalId, TerminalView},
};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, button::Button, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub(crate) fn command_group_controls(
    terminal: RemoteTerminalId,
    view: &TerminalView,
    dispatch: FocusHandle,
    cx: &App,
) -> impl IntoElement {
    let status = view.command_group_status(cx);
    let message = status.message();
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
                        dispatch.dispatch_action(&StopCommandGroup(terminal), window, cx)
                    }),
            )
        })
}
