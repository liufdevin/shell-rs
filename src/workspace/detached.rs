use gpui_kit::component::{
    Root, TitleBar, WindowExt as _,
    dock::{BasePanel, DockPlacement, DragPanel, Panel, PanelId, panel_handle},
    notification::Notification,
};
use gpui_kit::*;

use super::{AppShell, Workspace};
use crate::app::{CenterTab, CloseDetachedTab, DetachTab, ReturnDetachedTab};
use crate::i18n::t;
use crate::shared::during_panel_transfer;

#[non_exhaustive]
pub(super) struct DetachedWindow {
    pub tab: CenterTab,
    pub panel: PanelId,
    pub handle: AnyWindowHandle,
}

struct DetachedView {
    workspace: Entity<Workspace>,
    tab: CenterTab,
    content: AnyView,
    _subscriptions: Vec<Subscription>,
}

impl DetachedView {
    fn new(
        workspace: Entity<Workspace>,
        tab: CenterTab,
        content: AnyView,
        focus: FocusHandle,
        root_focus: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = vec![
            cx.observe_global::<crate::i18n::UiLocale>(|_, cx| cx.notify()),
            cx.observe_global::<gpui_kit::component::Theme>(|_, cx| cx.notify()),
            cx.on_focus_in(&root_focus, window, |this, window, cx| {
                this.activate(window, cx)
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.activate(window, cx);
                }
            }),
        ];
        let terminal = {
            let owner = workspace.read(cx);
            match tab {
                CenterTab::Terminal(id) => owner
                    .terminals
                    .get(&id)
                    .map(|panel| panel.read(cx).terminal().clone()),
                CenterTab::LocalTerminal(id) => owner
                    .local_terminals
                    .get(&id)
                    .map(|panel| panel.read(cx).terminal().clone()),
                _ => None,
            }
        };
        if let Some(terminal) = terminal {
            subscriptions.extend(terminal.update(cx, |terminal, cx| {
                terminal.observe_detached_focus(window, cx)
            }));
        }
        window.focus(&focus, cx);
        Self {
            workspace,
            tab,
            content,
            _subscriptions: subscriptions,
        }
    }

    fn activate(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.update(cx, |workspace, cx| {
            if workspace.active_tab != Some(self.tab) {
                workspace.active_tab = Some(self.tab);
                cx.notify();
            }
        });
        window.refresh();
    }
}

impl Render for DetachedView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.workspace.update(cx, |workspace, cx| {
            workspace.render_window(Some((self.tab, self.content.clone())), window, cx)
        })
    }
}

impl Workspace {
    pub(super) fn on_tab_drag(
        &mut self,
        event: &DragMoveEvent<DragPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dragged_tab = self
            .center_tab_for_panel(event.drag(cx).panel())
            .map(|tab| (window.window_handle().window_id(), tab));
    }

    pub(super) fn on_tab_drag_outside(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !cx.has_active_drag()
            || Bounds::new(point(px(0.), px(0.)), window.viewport_size()).contains(&event.position)
        {
            self.dragged_tab = None;
            return;
        }
        if let Some((source, tab)) = self.dragged_tab.take()
            && source == window.window_handle().window_id()
        {
            cx.stop_active_drag(window);
            let dispatch = self.focus_handle.clone();
            window.defer(cx, move |window, cx| {
                dispatch.dispatch_action(&DetachTab(tab), window, cx)
            });
        }
    }

    fn detached_content(&self, tab: CenterTab, cx: &App) -> Option<(PanelId, AnyView)> {
        match tab {
            CenterTab::Terminal(id) => self
                .terminals
                .get(&id)
                .map(|panel| (PanelId::from(panel.entity_id()), panel.clone().into())),
            CenterTab::LocalTerminal(id) => self
                .local_terminals
                .get(&id)
                .map(|panel| (PanelId::from(panel.entity_id()), panel.clone().into())),
            CenterTab::Explorer(id) => self
                .explorers
                .get(&id)
                .map(|panel| (PanelId::from(panel.entity_id()), panel.clone().into())),
            CenterTab::Editor(id) => self
                .editors
                .get(&id)
                .map(|panel| (PanelId::from(panel.entity_id()), panel.clone().into())),
            CenterTab::Settings => self
                .settings_tab()
                .map(|panel| (PanelId::from(panel.entity_id()), panel.clone().into())),
        }
        .filter(|(panel, _)| self.dock_area.read(cx).panel(*panel).is_some())
    }

    pub(super) fn on_detach_tab(
        &mut self,
        action: &DetachTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if crate::app_lock::is_locked(cx) || window.has_active_dialog(cx) {
            return;
        }
        let tab = action.0;
        if self.detaching_tabs.contains(&tab) {
            return;
        }
        let Some((panel, content)) = self.detached_content(tab, cx) else {
            return;
        };
        let Some(focus) = self.tab_focus_handle(tab, cx) else {
            return;
        };
        let root_focus = self.focus_handle.clone();
        let dispatch = self.focus_handle.clone();
        let workspace = cx.entity();
        let owner = workspace.clone();
        let source = window.window_handle();
        let origin = window.bounds().origin + point(px(48.), px(48.));
        self.detaching_tabs.push(tab);
        cx.defer(move |cx| {
            let opened = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        origin,
                        size(px(960.), px(640.)),
                    ))),
                    window_min_size: Some(size(px(560.), px(360.))),
                    app_id: Some(crate::app::DESKTOP_APP_ID.into()),
                    ..TitleBar::window_options()
                },
                move |window, cx| {
                    window.set_window_title(crate::app::APP_NAME);
                    window.on_window_should_close(cx, move |window, cx| {
                        dispatch.dispatch_action(&CloseDetachedTab(tab), window, cx);
                        false
                    });
                    let view = cx.new(|cx| {
                        DetachedView::new(workspace, tab, content, focus, root_focus, window, cx)
                    });
                    if let Some(lock) = crate::app_lock::AppLock::global(cx) {
                        let shell = cx.new(|cx| {
                            AppShell::new(
                                lock,
                                None,
                                false,
                                move |_, _, _, _| view.into(),
                                window,
                                cx,
                            )
                        });
                        cx.new(|cx| Root::new(shell, window, cx))
                    } else {
                        cx.new(|cx| Root::new(view, window, cx))
                    }
                },
            );
            let handle = match opened {
                Ok(handle) => handle.into(),
                Err(error) => {
                    owner.update(cx, |owner, _| {
                        owner.detaching_tabs.retain(|pending| *pending != tab)
                    });
                    _ = source.update(cx, |_, window, cx| {
                        window.push_notification(
                            Notification::error(t!("workspace.detached.failed", error = error)),
                            cx,
                        )
                    });
                    return;
                }
            };
            let moved = source.update(cx, |_, window, cx| {
                owner.update(cx, |owner, cx| {
                    owner.complete_detach(tab, panel, handle, window, cx)
                })
            });
            if moved.is_err() {
                owner.update(cx, |owner, _| {
                    owner.detaching_tabs.retain(|pending| *pending != tab)
                });
                _ = handle.update(cx, |_, window, _| window.remove_window());
            }
        });
    }

    fn complete_detach(
        &mut self,
        tab: CenterTab,
        panel: PanelId,
        handle: AnyWindowHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.detaching_tabs.retain(|pending| *pending != tab);
        self.detached_windows
            .push(DetachedWindow { tab, panel, handle });
        let area = self.dock_area.clone();
        match tab {
            CenterTab::Terminal(id) => {
                if let Some(panel) = self.terminals.get(&id).cloned() {
                    self.transfer_out(panel.clone(), &area, window, cx);
                }
            }
            CenterTab::LocalTerminal(id) => {
                if let Some(panel) = self.local_terminals.get(&id).cloned() {
                    self.transfer_out(panel, &area, window, cx);
                }
            }
            CenterTab::Explorer(id) => {
                if let Some(panel) = self.explorers.get(&id).cloned() {
                    self.transfer_out(panel, &area, window, cx);
                }
            }
            CenterTab::Editor(id) => {
                if let Some(panel) = self.editors.get(&id).cloned() {
                    self.transfer_out(panel, &area, window, cx);
                }
            }
            CenterTab::Settings => {
                if let Some(panel) = self.settings_tab() {
                    self.transfer_out(panel.clone(), &area, window, cx);
                }
            }
        }
        cx.notify();
    }

    fn transfer_out<PanelType: Panel>(
        &self,
        panel: Entity<PanelType>,
        area: &Entity<gpui_kit::component::dock::DockArea>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        during_panel_transfer(panel.entity_id(), cx, |cx| {
            area.update(cx, |area, cx| area.remove_panel(panel, window, cx));
        });
    }

    pub(super) fn remove_tab_panel<PanelType: Panel>(
        &self,
        panel: Entity<PanelType>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .detached_windows
            .iter()
            .any(|detached| detached.panel == PanelId::from(panel.entity_id()))
        {
            panel.update(cx, |panel, cx| BasePanel::on_removed(panel, window, cx));
        } else {
            self.dock_area
                .update(cx, |area, cx| area.remove_panel(panel, window, cx));
        }
    }

    pub(super) fn show_detached_tab(
        &self,
        tab: CenterTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(detached) = self
            .detached_windows
            .iter()
            .find(|detached| detached.tab == tab)
        else {
            return false;
        };
        let Some(focus) = self.tab_focus_handle(tab, cx) else {
            return false;
        };
        let handle = detached.handle;
        if handle.window_id() == window.window_handle().window_id() {
            window.focus(&focus, cx);
        } else {
            cx.defer(move |cx| {
                _ = handle.update(cx, |_, window, cx| {
                    window.activate_window();
                    window.focus(&focus, cx);
                });
            });
        }
        true
    }

    pub(super) fn finish_detached_tab(&mut self, tab: CenterTab, cx: &mut Context<Self>) {
        if let Some(index) = self
            .detached_windows
            .iter()
            .position(|detached| detached.tab == tab)
        {
            let detached = self.detached_windows.remove(index);
            cx.defer(move |cx| {
                _ = detached
                    .handle
                    .update(cx, |_, window, _| window.remove_window());
            });
        }
    }

    pub(super) fn on_close_detached_tab(
        &mut self,
        action: &CloseDetachedTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_center_tab(action.0, window, cx);
    }

    pub(super) fn on_return_detached_tab(
        &mut self,
        action: &ReturnDetachedTab,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = action.0;
        if !self
            .detached_windows
            .iter()
            .any(|detached| detached.tab == tab)
        {
            return;
        }
        let area = self.dock_area.clone();
        let primary = self.primary_window;
        let workspace = cx.weak_entity();
        cx.defer(move |cx| {
            _ = primary.update(cx, |_, window, cx| {
                _ = workspace.update(cx, |workspace, cx| {
                    match tab {
                        CenterTab::Terminal(id) => {
                            if let Some(panel) = workspace.terminals.get(&id).cloned() {
                                area.update(cx, |area, cx| {
                                    area.add_panel_view(
                                        panel_handle(panel.clone()),
                                        DockPlacement::Center,
                                        None,
                                        window,
                                        cx,
                                    )
                                });
                            }
                        }
                        CenterTab::LocalTerminal(id) => {
                            if let Some(panel) = workspace.local_terminals.get(&id).cloned() {
                                area.update(cx, |area, cx| {
                                    area.add_panel_view(
                                        panel_handle(panel),
                                        DockPlacement::Center,
                                        None,
                                        window,
                                        cx,
                                    )
                                });
                            }
                        }
                        CenterTab::Explorer(id) => {
                            if let Some(panel) = workspace.explorers.get(&id).cloned() {
                                area.update(cx, |area, cx| {
                                    area.add_panel_view(
                                        panel_handle(panel),
                                        DockPlacement::Center,
                                        None,
                                        window,
                                        cx,
                                    )
                                });
                            }
                        }
                        CenterTab::Editor(id) => {
                            if let Some(panel) = workspace.editors.get(&id).cloned() {
                                area.update(cx, |area, cx| {
                                    area.add_panel_view(
                                        panel_handle(panel),
                                        DockPlacement::Center,
                                        None,
                                        window,
                                        cx,
                                    )
                                });
                            }
                        }
                        CenterTab::Settings => {
                            if let Some(panel) = workspace.settings_tab() {
                                area.update(cx, |area, cx| {
                                    area.add_panel_view(
                                        panel_handle(panel.clone()),
                                        DockPlacement::Center,
                                        None,
                                        window,
                                        cx,
                                    )
                                });
                            }
                        }
                    }
                    workspace.finish_detached_tab(tab, cx);
                    window.activate_window();
                    cx.notify();
                });
            });
        });
    }
}
