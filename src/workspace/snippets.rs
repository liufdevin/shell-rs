//! The workspace's share of 命令片段: the commands of the snippet list,
//! which open the dialogs and ask before deleting.

use std::rc::Rc;

use gpui_kit::*;

use crate::app::{
    DeleteSnippet, DeleteSnippetCategory, EditSnippet, InsertSnippet, NewSnippet,
    NewSnippetCategory, NewSnippetIn, RenameSnippetCategory, ToggleSnippetCategory,
};
use crate::i18n::{t, tn};
use crate::shared::confirm_delete;
use crate::snippets::{CategoryDialog, SnippetDialog, open_category_dialog, open_snippet_dialog};

use super::workspace_view::Workspace;

impl Workspace {
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
        let Some(command) = self
            .store
            .read(cx)
            .snippets()
            .iter()
            .find(|snippet| snippet.sync_id.as_str() == action.0)
            .map(|snippet| snippet.command.clone())
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
        if let Err(error) = view.update(cx, |view, cx| {
            view.enter_command(&command, shortcut.runs(), false, cx)
        }) {
            use gpui_kit::component::{WindowExt as _, notification::Notification};
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
        open_snippet_dialog(
            SnippetDialog::New(None),
            self.store.clone(),
            self.settings().clone(),
            window,
            cx,
        );
    }

    pub(super) fn on_new_snippet_in(
        &mut self,
        action: &NewSnippetIn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_snippet_dialog(
            SnippetDialog::New(Some(action.0)),
            self.store.clone(),
            self.settings().clone(),
            window,
            cx,
        );
    }

    pub(super) fn on_edit_snippet(
        &mut self,
        action: &EditSnippet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.read(cx).snippet(action.0).is_some() {
            open_snippet_dialog(
                SnippetDialog::Edit(action.0),
                self.store.clone(),
                self.settings().clone(),
                window,
                cx,
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
