use gpui_kit::component::{
    ActiveTheme as _, Icon, WindowExt as _,
    dock::{BasePanel, Panel, PanelEvent, TabGroup},
    menu::PopupMenu,
    v_flex,
};
use gpui_kit::*;

use crate::app::{CatalogIcon, CenterTab, CloseSerialCommandEditor};
use crate::i18n::t;
use crate::shared::{ClosableTabTitle, close_tab_items};

use super::SerialToolsPanel;

#[derive(Clone, Copy)]
pub(crate) enum SerialCommandEditorEvent {
    Activated,
    Closed,
}

pub(crate) struct SerialCommandEditorPanel {
    focus: FocusHandle,
    group: Option<WeakEntity<TabGroup>>,
    tools: Entity<SerialToolsPanel>,
    _subscription: Subscription,
}

impl SerialCommandEditorPanel {
    pub(crate) fn new(tools: Entity<SerialToolsPanel>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&tools, |_, _, cx| cx.notify());
        Self {
            focus: cx.focus_handle(),
            group: None,
            tools,
            _subscription: subscription,
        }
    }

    pub(crate) fn tab_group(&self) -> Option<WeakEntity<TabGroup>> {
        self.group.clone()
    }

    pub(crate) fn show(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        cx.emit(SerialCommandEditorEvent::Activated);
    }
}

impl Render for SerialCommandEditorPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tools = self.tools.read(cx).render_command_editor(cx);
        v_flex()
            .id("serial-command-editor")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .min_w_0()
            .bg(cx.theme().background)
            .child(div().flex_1().min_h_0().min_w_0().child(tools))
    }
}

impl EventEmitter<PanelEvent> for SerialCommandEditorPanel {}
impl EventEmitter<SerialCommandEditorEvent> for SerialCommandEditorPanel {}

impl Focusable for SerialCommandEditorPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BasePanel for SerialCommandEditorPanel {
    fn panel_name(&self) -> &'static str {
        "SerialCommandEditorPanel"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if active && !window.has_active_dialog(cx) {
            self.show(window, cx);
        }
    }

    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.group = Some(group);
    }

    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.group = None;
        if !crate::shared::is_panel_transfer(cx.entity_id(), cx) {
            cx.emit(SerialCommandEditorEvent::Closed);
        }
    }
}

impl Panel for SerialCommandEditorPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let group = self.group.clone();
        let panel = cx.entity_id();
        ClosableTabTitle::new(
            "serial-command-editor-tab",
            Icon::new(CatalogIcon::Pencil),
            t!("serial.assistant.commands_tab"),
        )
        .closable(
            "close-serial-command-editor",
            Box::new(CloseSerialCommandEditor),
        )
        .context_menu(move |menu, _, cx| {
            close_tab_items(
                menu,
                CenterTab::SerialCommandEditor,
                group.clone(),
                panel,
                cx,
            )
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
            CenterTab::SerialCommandEditor,
            self.group.clone(),
            cx.entity_id(),
            cx,
        )
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}
