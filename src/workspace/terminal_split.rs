//! Manual terminal layout commands. Dock is the only layout owner; splitting
//! moves existing panels and never changes connections or broadcast membership.

use gpui_kit::component::{
    IndexPath, Placement, Sizable as _, WindowExt as _,
    dock::{DockPlacement, InsertTarget, NodeId, PaneRef, PanelId},
    form::{Field, Form},
    select::{Select, SelectState},
    v_flex,
};
use gpui_kit::*;

use super::Workspace;
use crate::app::{CancelTerminalSplit, CenterTab, OpenTerminalSplit, SplitTerminal};
use crate::i18n::t;
use crate::shared::{
    TerminalSplitMenu, commit_footer, dismiss_form_error, form_error_notification,
};

fn is_terminal(tab: CenterTab) -> bool {
    matches!(tab, CenterTab::Terminal(_) | CenterTab::LocalTerminal(_))
}

impl Workspace {
    /// Center order is stable even when several terminals have the same title.
    fn split_terminals(&self, cx: &App) -> Vec<CenterTab> {
        self.dock_area
            .read(cx)
            .layout(DockPlacement::Center)
            .map(|tree| {
                tree.panels()
                    .filter_map(|panel| self.center_tab_for_panel(panel))
                    .filter(|tab| is_terminal(*tab))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn sync_terminal_split_menus(&self, cx: &mut Context<Self>) {
        let terminals = self.split_terminals(cx);
        let mut groups = 0;
        if let Some(tree) = self.dock_area.read(cx).layout(DockPlacement::Center) {
            tree.root().walk(&mut |node| {
                if matches!(node.kind(), PaneRef::Tabs { .. }) {
                    groups += 1;
                }
            });
        }
        let state = |tab| {
            let present = terminals.contains(&tab);
            TerminalSplitMenu::new(present && terminals.len() > 1, present && groups > 1)
        };
        for (id, panel) in &self.terminals {
            panel.update(cx, |panel, cx| {
                panel.set_split_menu(state(CenterTab::Terminal(*id)), cx)
            });
        }
        for (id, panel) in &self.local_terminals {
            panel.update(cx, |panel, cx| {
                panel.set_split_menu(state(CenterTab::LocalTerminal(*id)), cx)
            });
        }
    }

    /// Resolve again when committing: a terminal can close or detach while the
    /// dialog is open. Never move a panel unless both endpoints are still here.
    fn split_pair(
        &self,
        source: CenterTab,
        target: CenterTab,
        cx: &App,
    ) -> Option<(PanelId, PanelId, NodeId)> {
        if source == target || !is_terminal(source) || !is_terminal(target) {
            return None;
        }
        let (_, source) = self.center_tab_location(source, cx)?;
        let (_, target) = self.center_tab_location(target, cx)?;
        let area = self.dock_area.read(cx);
        let tree = area.layout(DockPlacement::Center)?;
        let node = tree.find_panel_node(source)?;
        tree.find_panel_node(target)?;
        Some((source, target, node))
    }

    pub(super) fn on_open_terminal_split(
        &mut self,
        action: &OpenTerminalSplit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source = action.0;
        let terminals = self.split_terminals(cx);
        if !terminals.contains(&source) {
            return;
        }
        let targets: Vec<_> = terminals.into_iter().filter(|tab| *tab != source).collect();
        if targets.is_empty() {
            return;
        }
        let Some(recipient) = self.recipient(source, cx) else {
            return;
        };
        // Include an ordinal so duplicate connections with identical titles are
        // distinguishable, in the same order as the Dock's tab bars.
        let labels: Vec<SharedString> = targets
            .iter()
            .enumerate()
            .map(|(ix, tab)| {
                let name = self
                    .recipient(*tab, cx)
                    .map(|recipient| recipient.name)
                    .unwrap_or_default();
                format!("{} · {}", name, ix + 1).into()
            })
            .collect();
        let form = cx.new(|cx| SplitForm {
            targets,
            target: cx.new(|cx| SelectState::new(labels, Some(IndexPath::new(0)), window, cx)),
            layout: cx.new(|cx| {
                SelectState::new(
                    vec![
                        t!("shared.terminal_split.side_by_side"),
                        t!("shared.terminal_split.stacked"),
                    ],
                    Some(IndexPath::new(0)),
                    window,
                    cx,
                )
            }),
        });
        let owner = cx.weak_entity();
        let dispatch = self.focus_handle.clone();
        let title = t!("shared.terminal_split.title", name = recipient.name);
        window.open_dialog(cx, move |dialog, _, _| {
            let form = form.clone();
            let owner = owner.clone();
            let dispatch = dispatch.clone();
            dialog
                .title(title.clone())
                .overlay_closable(false)
                .child(form.clone())
                .footer(commit_footer("commit", t!("shared.terminal_split.apply")))
                .on_ok(move |_, window, cx| {
                    let Some(action) = form.read(cx).action(source, cx) else {
                        return false;
                    };
                    let valid = owner.upgrade().is_some_and(|owner| {
                        owner
                            .read(cx)
                            .split_pair(action.source(), action.target(), cx)
                            .is_some()
                    });
                    if !valid {
                        window.push_notification(
                            form_error_notification(t!("shared.terminal_split.unavailable")),
                            cx,
                        );
                        return false;
                    }
                    let dispatch = dispatch.clone();
                    window.defer(cx, move |window, cx| {
                        dispatch.dispatch_action(&action, window, cx)
                    });
                    true
                })
                .on_close(|_, window, cx| dismiss_form_error(window, cx))
        });
    }

    pub(super) fn on_split_terminal(
        &mut self,
        action: &SplitTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, target, node)) = self.split_pair(action.source(), action.target(), cx) else {
            return;
        };
        self.dock_area.update(cx, |area, cx| {
            area.set_zoomed_out(window, cx);
            area.move_panel(
                target,
                InsertTarget::Split {
                    node,
                    placement: if action.is_stacked() {
                        Placement::Bottom
                    } else {
                        Placement::Right
                    },
                    size: None,
                },
                window,
                cx,
            );
        });
        self.activate_split_source(action.source(), window, cx);
    }

    pub(super) fn on_cancel_terminal_split(
        &mut self,
        action: &CancelTerminalSplit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !is_terminal(action.0) {
            return;
        }
        let Some((_, source)) = self.center_tab_location(action.0, cx) else {
            return;
        };
        let Some(tree) = self.dock_area.read(cx).layout(DockPlacement::Center) else {
            return;
        };
        if tree.find_panel_node(source).is_none() {
            return;
        }
        let panels: Vec<_> = tree.panels().collect();
        self.dock_area.update(cx, |area, cx| {
            area.set_zoomed_out(window, cx);
            for panel in panels {
                // Resolve on every edit: normalizing an emptied group can
                // collapse its parent while the source group keeps its id.
                let Some(node) = area
                    .layout(DockPlacement::Center)
                    .and_then(|tree| tree.find_panel_node(source))
                else {
                    break;
                };
                if area
                    .layout(DockPlacement::Center)
                    .and_then(|tree| tree.find_panel_node(panel))
                    == Some(node)
                {
                    continue;
                }
                area.move_panel(
                    panel,
                    InsertTarget::Tabs {
                        node,
                        ix: None,
                        activate: false,
                    },
                    window,
                    cx,
                );
            }
        });
        self.activate_split_source(action.0, window, cx);
    }

    fn activate_split_source(
        &mut self,
        source: CenterTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((group, panel)) = self.center_tab_location(source, cx) {
            group.update(cx, |group, cx| {
                if let Some(ix) = group
                    .panels()
                    .iter()
                    .position(|candidate| candidate.panel_id(cx) == panel)
                {
                    group.select_tab(ix, window, cx);
                }
            });
        }
        self.set_active_tab(Some(source), window, cx);
        if !window.has_active_dialog(cx)
            && let Some(focus) = self.tab_focus_handle(source, cx)
        {
            window.focus(&focus, cx);
        }
        self.sync_broadcast(cx);
        self.sync_compose(window, cx);
        cx.notify();
    }
}

struct SplitForm {
    targets: Vec<CenterTab>,
    target: Entity<SelectState<Vec<SharedString>>>,
    layout: Entity<SelectState<Vec<SharedString>>>,
}

impl SplitForm {
    fn action(&self, source: CenterTab, cx: &App) -> Option<SplitTerminal> {
        let target = *self
            .targets
            .get(self.target.read(cx).selected_index(cx)?.row)?;
        let stacked = self.layout.read(cx).selected_index(cx)?.row == 1;
        Some(SplitTerminal::new(source, target, stacked))
    }
}

impl Render for SplitForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::component::ActiveTheme as _;
        v_flex()
            .gap_3()
            .child(
                Form::new()
                    .child(
                        Field::new()
                            .label(t!("shared.terminal_split.target"))
                            .child(
                                Select::new(&self.target)
                                    .id("terminal-split-target")
                                    .small(),
                            ),
                    )
                    .child(
                        Field::new()
                            .label(t!("shared.terminal_split.layout"))
                            .child(
                                Select::new(&self.layout)
                                    .id("terminal-split-layout")
                                    .small(),
                            ),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("shared.terminal_split.merge_hint")),
            )
    }
}
