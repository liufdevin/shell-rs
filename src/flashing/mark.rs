use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::*;

use crate::app::FLASHING_ICON;

#[derive(IntoElement)]
pub(crate) struct FlashingMark;

impl RenderOnce for FlashingMark {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .size_4()
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .child(
                Icon::default()
                    .path(FLASHING_ICON)
                    .size_4()
                    .text_color(cx.theme().blue),
            )
    }
}
