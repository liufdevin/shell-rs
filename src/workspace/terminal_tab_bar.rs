use std::{cell::RefCell, collections::HashMap, sync::Arc};

use gpui_kit::{
    component::{
        ActiveTheme as _, IconName, InteractiveElementExt as _, Selectable as _, Sizable as _,
        button::{Button, ButtonVariants as _},
        dock::{
            AnyDrag, BasePanelView, ClosePanel, DragPanel, NodeId, PanelHandle, PanelId,
            TabGroupContext, ToggleZoom,
        },
        h_flex,
        menu::DropdownMenu as _,
        tab::Tab,
    },
    prelude::FluentBuilder as _,
    *,
};

use crate::{
    i18n::t,
    terminal::{LocalTerminalPanel, TerminalPanel},
};

#[derive(Default)]
pub(super) struct TerminalTabBar {
    scroll: RefCell<HashMap<NodeId, (ScrollHandle, Option<PanelId>)>>,
}

impl TerminalTabBar {
    pub(super) fn render(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let active = group.active_panel().map(|panel| panel.panel_id(cx));
        let visible: Vec<_> = group
            .panels()
            .iter()
            .enumerate()
            .filter(|(_, panel)| panel.visible(cx))
            .collect();
        let scroll = {
            let mut handles = self.scroll.borrow_mut();
            let (handle, previous) = handles.entry(group.node()).or_default();
            if *previous != active {
                if let Some(index) = visible
                    .iter()
                    .position(|(_, panel)| Some(panel.panel_id(cx)) == active)
                {
                    handle.scroll_to_item(index);
                }
                *previous = active;
            }
            handle.clone()
        };
        let tabs = visible
            .into_iter()
            .map(|(index, panel)| {
                let id = panel.panel_id(cx);
                let title = title(panel, window, cx);
                let unread = unread(panel, cx);
                let drag = group
                    .is_draggable()
                    .then(|| group.drag_panel(index, cx))
                    .flatten();
                let tab = Tab::new()
                    .selected(!group.is_collapsed() && Some(id) == active)
                    .child(title)
                    .on_click({
                        let group = group.clone();
                        move |_, window, cx| group.select_tab(index, window, cx)
                    })
                    .when(!group.is_collapsed(), |tab| {
                        tab.when_some(drag, |tab, drag| {
                            tab.on_drag(drag, {
                                let panel = panel.clone();
                                move |drag, offset, _, cx| {
                                    cx.stop_propagation();
                                    drag.set_drag_offset(offset);
                                    drag.set_preview_size(size(px(96.), px(30.)));
                                    cx.new(|_| TabDragPreview {
                                        panel: panel.clone(),
                                    })
                                }
                            })
                        })
                        .when(group.is_droppable(), |tab| {
                            tab.drag_over::<DragPanel>(|tab, _, _, cx| {
                                tab.border_l_2().border_color(cx.theme().drag_border)
                            })
                            .on_drop({
                                let group = group.clone();
                                move |drag: &DragPanel, window, cx| {
                                    group.drop_panel(drag.clone(), Some(index), true, window, cx)
                                }
                            })
                            .drag_over::<AnyDrag>(|tab, _, _, cx| {
                                tab.border_l_2().border_color(cx.theme().drag_border)
                            })
                            .on_drop({
                                let group = group.clone();
                                move |drag: &AnyDrag, window, cx| {
                                    group.drop_item(drag.clone(), None, window, cx)
                                }
                            })
                        })
                    });
                div()
                    .id(("dock-tab-frame", id.as_u64()))
                    .test_support()
                    .relative()
                    .flex_shrink_0()
                    .child(tab)
                    .when(unread, |frame| {
                        frame.child(
                            div()
                                .id("new-output-strip")
                                .test_support()
                                .absolute()
                                .top_0()
                                .left_0()
                                .right_0()
                                .h(px(1.))
                                .bg(cx.theme().info),
                        )
                    })
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let empty = div()
            .id("tab-bar-empty-space")
            .h_full()
            .flex_grow_1()
            .min_w_16()
            .when(group.is_droppable(), |empty| {
                empty
                    .drag_over::<DragPanel>(|style, _, _, cx| {
                        style.bg(cx.theme().tokens.drop_target)
                    })
                    .on_drop({
                        let group = group.clone();
                        move |drag: &DragPanel, window, cx| {
                            let index = (drag.source() == group.node())
                                .then(|| group.panels().len().saturating_sub(1));
                            group.drop_panel(drag.clone(), index, false, window, cx);
                        }
                    })
                    .drag_over::<AnyDrag>(|style, _, _, cx| style.bg(cx.theme().tokens.drop_target))
                    .on_drop({
                        let group = group.clone();
                        move |drag: &AnyDrag, window, cx| {
                            group.drop_item(drag.clone(), None, window, cx)
                        }
                    })
            });
        let suffix = toolbar(group, window, cx);
        gpui_kit::base::Tabs::new("tab-bar")
            .flex()
            .items_stretch()
            .w_full()
            .bg(cx.theme().tokens.tab_bar)
            .text_color(cx.theme().tab_foreground)
            .child(
                h_flex()
                    .id("tabs")
                    .flex_1()
                    .min_w_0()
                    .overflow_x_hidden()
                    .child(
                        h_flex()
                            .id("tabs-inner")
                            .relative()
                            .overflow_x_scroll()
                            .lock_scroll_axis()
                            .track_scroll(&scroll)
                            .children(tabs)
                            .child(empty),
                    ),
            )
            .child(suffix)
            .into_any_element()
    }
}

fn unread(panel: &Arc<dyn BasePanelView>, cx: &App) -> bool {
    if let Ok(terminal) = panel.view().downcast::<TerminalPanel>() {
        return terminal.read(cx).terminal().read(cx).has_unread_output();
    }
    if let Ok(terminal) = panel.view().downcast::<LocalTerminalPanel>() {
        return terminal.read(cx).terminal().read(cx).has_unread_output();
    }
    false
}

fn title(panel: &Arc<dyn BasePanelView>, window: &mut Window, cx: &mut App) -> AnyElement {
    match PanelHandle::of(panel) {
        Some(handle) => match handle.tab_name(cx) {
            Some(name) => name.into_any_element(),
            None => handle.title(window, cx),
        },
        None => SharedString::from(panel.panel_name(cx)).into_any_element(),
    }
}

fn toolbar(group: &TabGroupContext, window: &mut Window, cx: &mut App) -> AnyElement {
    if group.is_collapsed() {
        return Empty.into_any_element();
    }
    let handle = group.active_panel().and_then(PanelHandle::of);
    let control = group
        .active_panel()
        .filter(|panel| panel.zoomable(cx))
        .and_then(|_| handle.and_then(|handle| handle.zoom_control(cx)));
    let toolbar_zoom = control.is_some_and(|control| control.toolbar_visible());
    let menu_zoom = control.is_some_and(|control| control.menu_visible());
    let zoomed = group.is_zoomed();
    let closable = group.is_closable();
    let panel = handle.map(|handle| handle.panel());
    let buttons = handle
        .and_then(|handle| handle.toolbar_buttons(window, cx))
        .unwrap_or_default();
    let suffix = handle.and_then(|handle| handle.title_suffix(window, cx));
    h_flex()
        .occlude()
        .items_center()
        .h_full()
        .px_2()
        .gap_1()
        .border_l_1()
        .border_b_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().tokens.tab_bar)
        .children(suffix)
        .children(
            buttons
                .into_iter()
                .map(|button| button.xsmall().ghost().tab_stop(false)),
        )
        .when(zoomed || toolbar_zoom, |toolbar| {
            toolbar.child(
                Button::new("zoom")
                    .ghost()
                    .xsmall()
                    .selected(zoomed)
                    .tab_stop(false)
                    .icon(if zoomed {
                        IconName::Minimize
                    } else {
                        IconName::Maximize
                    })
                    .tooltip(if zoomed {
                        t!("workspace.dock.zoom_out")
                    } else {
                        t!("workspace.dock.zoom_in")
                    })
                    .on_click({
                        let group = group.clone();
                        move |_, window, cx| group.toggle_zoom(window, cx)
                    }),
            )
        })
        .child(
            Button::new("menu")
                .icon(IconName::Ellipsis)
                .ghost()
                .xsmall()
                .tab_stop(false)
                .dropdown_menu(move |menu, window, cx| {
                    menu.when_some(panel.clone(), |menu, panel| {
                        panel.dropdown_menu(menu, window, cx)
                    })
                    .separator()
                    .menu_with_disabled(
                        if zoomed {
                            t!("workspace.dock.zoom_out")
                        } else {
                            t!("workspace.dock.zoom_in")
                        },
                        Box::new(ToggleZoom),
                        !menu_zoom,
                    )
                    .when(closable, |menu| {
                        menu.separator()
                            .menu(t!("common.close"), Box::new(ClosePanel))
                    })
                })
                .anchor(Anchor::TopRight),
        )
        .into_any_element()
}

struct TabDragPreview {
    panel: Arc<dyn BasePanelView>,
}

impl Render for TabDragPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .cursor_grab()
            .py_1()
            .px_3()
            .w_24()
            .overflow_hidden()
            .whitespace_nowrap()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
            .bg(cx.theme().tokens.tab_active)
            .text_color(cx.theme().tab_foreground)
            .opacity(0.75)
            .child(title(&self.panel, window, cx))
    }
}
