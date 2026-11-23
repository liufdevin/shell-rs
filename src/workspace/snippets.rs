//! The workspace's share of 命令片段: the commands of the snippet list,
//! which open the dialogs and ask before deleting.

use std::rc::Rc;

use gpui_kit::*;

use crate::app::{
    DeleteSnippet, DeleteSnippetCategory, EditSnippet, InsertSnippet, NewSnippet,
    NewSnippetCategory, NewSnippetIn, RenameSnippetCategory, ToggleSnippetCategory,
};
use crate::app::{RunSnippet, StopCommandGroup};
use crate::i18n::{t, tn};
use crate::shared::confirm_delete;
use crate::snippets::{CategoryDialog, SnippetDialog, open_category_dialog, open_snippet_dialog};
use crate::terminal::{CommandGroupStatus, TerminalView};
use gpui_kit::component::{WindowExt as _, notification::Notification};

use super::workspace_view::Workspace;

impl Workspace {
    pub(super) fn on_add_snippet_step(
        &mut self,
        _: &crate::app::AddSnippetStep,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(form) = self.snippet_form.as_ref().and_then(WeakEntity::upgrade) {
            form.update(cx, |form, cx| form.add_step(window, cx));
        }
    }

    pub(super) fn on_move_snippet_step(
        &mut self,
        action: &crate::app::MoveSnippetStep,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(form) = self.snippet_form.as_ref().and_then(WeakEntity::upgrade) {
            form.update(cx, |form, cx| form.move_step(action.0.0, action.0.1, cx));
        }
    }

    pub(super) fn on_remove_snippet_step(
        &mut self,
        action: &crate::app::RemoveSnippetStep,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(form) = self.snippet_form.as_ref().and_then(WeakEntity::upgrade) {
            form.update(cx, |form, cx| form.remove_step(action.0, cx));
        }
    }
    pub(super) fn on_run_snippet(
        &mut self,
        action: &RunSnippet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(snippet) = self.store.read(cx).snippet(action.0).cloned() else {
            return;
        };
        let Some(steps) = snippet.steps.clone() else {
            self.on_enter_command(
                &crate::app::EnterCommand {
                    command: snippet.command,
                    run: true,
                },
                window,
                cx,
            );
            return;
        };
        let Some(terminal) = self.tool_terminal(cx) else {
            return;
        };
        let Some(view) = terminal.view.upgrade() else {
            return;
        };
        start_command_group_or_replace(view, snippet.name.to_string(), steps, window, cx);
    }

    pub(super) fn on_stop_command_group(
        &mut self,
        action: &StopCommandGroup,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.terminals.get(&action.0) {
            let terminal = panel.read(cx).terminal().clone();
            terminal.update(cx, |view, cx| view.stop_command_group(cx));
        }
    }

    pub(super) fn on_insert_snippet(
        &mut self,
        action: &InsertSnippet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let settings = self.settings().read(cx).settings();
        let Some(shortcut) = settings.snippet_shortcuts.get(&action.0) else {
            return;
        };
        let Some(snippet) = self
            .store
            .read(cx)
            .snippets()
            .iter()
            .find(|snippet| snippet.sync_id.as_str() == action.0)
            .cloned()
        else {
            return;
        };
        let focused = self
            .terminals
            .values()
            .map(|panel| panel.read(cx).terminal().clone())
            .chain(
                self.local_terminals
                    .values()
                    .map(|panel| panel.read(cx).terminal().clone()),
            )
            .find(|view| view.read(cx).focus_handle().is_focused(window));
        let Some(view) = focused else {
            return;
        };
        if let Some(steps) = &snippet.steps {
            start_command_group_or_replace(
                view,
                snippet.name.to_string(),
                steps.clone(),
                window,
                cx,
            );
        } else if let Err(error) = view.update(cx, |view, cx| {
            view.enter_command(&snippet.command, shortcut.runs(), false, cx)
        }) {
            let message = if shortcut.runs() {
                t!("tools.command.run_failed", error = error)
            } else {
                t!("tools.command.enter_failed", error = error)
            };
            window.push_notification(Notification::error(message), cx);
        }
    }

    pub(super) fn on_new_snippet(
        &mut self,
        _: &NewSnippet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.snippet_form = Some(
            open_snippet_dialog(
                SnippetDialog::New(None),
                self.store.clone(),
                self.settings().clone(),
                self.focus_handle.clone(),
                window,
                cx,
            )
            .downgrade(),
        );
    }

    pub(super) fn on_new_snippet_in(
        &mut self,
        action: &NewSnippetIn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.snippet_form = Some(
            open_snippet_dialog(
                SnippetDialog::New(Some(action.0)),
                self.store.clone(),
                self.settings().clone(),
                self.focus_handle.clone(),
                window,
                cx,
            )
            .downgrade(),
        );
    }

    pub(super) fn on_edit_snippet(
        &mut self,
        action: &EditSnippet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.read(cx).snippet(action.0).is_some() {
            self.snippet_form = Some(
                open_snippet_dialog(
                    SnippetDialog::Edit(action.0),
                    self.store.clone(),
                    self.settings().clone(),
                    self.focus_handle.clone(),
                    window,
                    cx,
                )
                .downgrade(),
            );
        }
    }

    pub(super) fn on_delete_snippet(
        &mut self,
        action: &DeleteSnippet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        let Some(name) = self
            .store
            .read(cx)
            .snippet(id)
            .map(|snippet| snippet.name.clone())
        else {
            return;
        };
        let store = self.store.clone();
        let settings = self.settings().clone();
        let public_id = store
            .read(cx)
            .snippet(id)
            .map(|snippet| snippet.sync_id.to_string());
        confirm_delete(
            &name,
            None,
            Rc::new(move |_, cx| {
                store.update(cx, |store, cx| {
                    store.remove_snippet(id, cx);
                });
                if let Some(id) = public_id.clone() {
                    settings.update(cx, |store, cx| {
                        store.update(|settings| settings.snippet_shortcuts.set(id, None), cx)
                    });
                }
            }),
            window,
            cx,
        );
    }

    pub(super) fn on_new_snippet_category(
        &mut self,
        _: &NewSnippetCategory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_category_dialog(CategoryDialog::New, self.store.clone(), window, cx);
    }

    pub(super) fn on_rename_snippet_category(
        &mut self,
        action: &RenameSnippetCategory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.read(cx).snippet_category(action.0).is_some() {
            open_category_dialog(
                CategoryDialog::Rename(action.0),
                self.store.clone(),
                window,
                cx,
            );
        }
    }

    /// Ask, saying how many snippets go with it, then delete a category.
    pub(super) fn on_delete_snippet_category(
        &mut self,
        action: &DeleteSnippetCategory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        let store = self.store.read(cx);
        let Some(name) = store
            .snippet_category(id)
            .map(|category| category.name.clone())
        else {
            return;
        };
        let snippets = store.snippets_in(id);
        let public_ids: Vec<String> = store
            .snippets()
            .iter()
            .filter(|snippet| snippet.category == Some(id))
            .map(|snippet| snippet.sync_id.to_string())
            .collect();
        let description =
            (snippets > 0).then(|| tn!("workspace.snippets.delete_category", snippets));
        let store = self.store.clone();
        let settings = self.settings().clone();
        confirm_delete(
            &name,
            description,
            Rc::new(move |_, cx| {
                store.update(cx, |store, cx| {
                    store.remove_snippet_category(id, cx);
                });
                settings.update(cx, |store, cx| {
                    store.update(
                        |settings| {
                            for id in &public_ids {
                                settings.snippet_shortcuts.set(id.clone(), None);
                            }
                        },
                        cx,
                    )
                });
            }),
            window,
            cx,
        );
    }

    /// A category's heading: fold it away, or unfold it.
    pub(super) fn on_toggle_snippet_category(
        &mut self,
        action: &ToggleSnippetCategory,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        self.tools
            .update(cx, |tools, cx| tools.toggle_snippet_category(id, cx));
    }
}

fn start_command_group_or_replace(
    view: Entity<TerminalView>,
    name: String,
    steps: Vec<crate::host::SnippetStep>,
    window: &mut Window,
    cx: &mut App,
) {
    let status = view.read(cx).command_group_status(cx);
    if let CommandGroupStatus::Running { name: current, .. } = &status {
        if current == &name {
            return;
        }
        let current = current.clone();
        let progress = status.message().unwrap_or_default();
        let title = t!(
            "snippets.group.replace_title",
            current = current,
            next = name.clone()
        );
        let description = Some(t!("snippets.group.replace_description", current = progress));
        let on_ok = Rc::new(move |window: &mut Window, cx: &mut App| {
            let result = view.update(cx, |view, cx| {
                view.stop_command_group(cx);
                view.start_command_group(name.clone(), steps.clone(), cx)
            });
            if let Err(error) = result {
                window.push_notification(Notification::error(error), cx);
            }
        });
        crate::shared::confirm_danger(
            title,
            description,
            t!("snippets.group.replace_action"),
            on_ok,
            window,
            cx,
        );
        return;
    }
    if let Err(error) = view.update(cx, |view, cx| view.start_command_group(name, steps, cx)) {
        window.push_notification(Notification::error(error), cx);
    }
}
