use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState},
    h_flex,
};
use gpui_kit::*;

use crate::app::CatalogIcon;
use crate::i18n::t;
use crate::terminal::HighlightColor;

use super::SettingsStore;

/// The color picker keeps its open state while edits are written through to
/// settings. It controls the background of the active Dock tab.
pub(super) struct TerminalTabAppearanceEditor {
    store: Entity<SettingsStore>,
    picker: Entity<ColorPickerState>,
    _subscriptions: [Subscription; 2],
}

impl TerminalTabAppearanceEditor {
    pub(super) fn new(
        store: Entity<SettingsStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial = store
            .read(cx)
            .settings()
            .terminal_tab_active_color
            .unwrap_or_else(|| HighlightColor::from_hsla(*cx.theme().tokens.tab_active));
        let picker = cx.new(|cx| ColorPickerState::new(window, cx).default_value(initial.hsla()));
        let store_changes = cx.observe_in(&store, window, |this, store, window, cx| {
            let stored = store.read(cx).settings().terminal_tab_active_color;
            let color =
                stored.unwrap_or_else(|| HighlightColor::from_hsla(*cx.theme().tokens.tab_active));
            if this.picker.read(cx).value() != Some(color.hsla()) {
                this.picker
                    .update(cx, |picker, cx| picker.set_value(color.hsla(), window, cx));
            }
        });
        let picker_changes = cx.subscribe_in(
            &picker,
            window,
            |this, _, event: &ColorPickerEvent, _, cx| {
                if let ColorPickerEvent::Change(Some(color)) = event {
                    this.set_color(HighlightColor::from_hsla(*color), cx);
                }
            },
        );
        Self {
            store,
            picker,
            _subscriptions: [store_changes, picker_changes],
        }
    }

    fn set_color(&mut self, color: HighlightColor, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| {
            store.update(
                |settings| settings.terminal_tab_active_color = Some(color),
                cx,
            );
        });
    }

    pub(super) fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| {
            store.update(|settings| settings.terminal_tab_active_color = None, cx);
        });
        let color = HighlightColor::from_hsla(*cx.theme().tokens.tab_active);
        self.picker
            .update(cx, |picker, cx| picker.set_value(color.hsla(), window, cx));
    }

    pub(super) fn render_control(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let settings = self.store.read(cx).settings();
        let color = settings
            .terminal_tab_active_color
            .unwrap_or_else(|| HighlightColor::from_hsla(*cx.theme().tokens.tab_active));
        let selected = settings.terminal_tab_active_color.is_some();
        h_flex()
            .items_center()
            .gap_2()
            .child(
                div().id("terminal-tab-color-picker").test_support().child(
                    ColorPicker::new(&self.picker)
                        .small()
                        .label(color.to_hex())
                        .accessibility_label(t!("settings.appearance.terminal_tab_color")),
                ),
            )
            .child(
                Button::new("terminal-tab-color-reset")
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(CatalogIcon::RefreshCw))
                    .accessibility_label(t!("settings.appearance.terminal_tab_color_reset"))
                    .tooltip(t!("settings.appearance.terminal_tab_color_reset"))
                    .disabled(!selected)
                    .on_click(cx.listener(|this, _, window, cx| this.reset(window, cx))),
            )
            .into_any_element()
    }
}

impl Render for TerminalTabAppearanceEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_control(window, cx)
    }
}
