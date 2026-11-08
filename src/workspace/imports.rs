//! The workspace's share of 导入: opening the import dialog for a source,
//! and what follows an import.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::{WindowExt as _, notification::Notification};
use gpui_kit::*;

use crate::analytics::Counter;
use crate::app::{ImportKnownHosts, ImportSshConfig, ImportXshell};
use crate::host::{ImportOutcome, ImportSource, open_import_dialog};
use crate::i18n::tn;

use super::workspace_view::Workspace;

impl Workspace {
    pub(super) fn on_import_xshell(
        &mut self,
        _: &ImportXshell,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_import(ImportSource::Xshell, window, cx);
    }

    pub(super) fn on_import_ssh_config(
        &mut self,
        _: &ImportSshConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_import(ImportSource::SshConfig, window, cx);
    }

    pub(super) fn on_import_known_hosts(
        &mut self,
        _: &ImportKnownHosts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_import(ImportSource::KnownHosts, window, cx);
    }

    /// Where 导入 reads from: tests point it at a temporary directory.
    pub fn set_ssh_dir(&mut self, ssh_dir: Option<PathBuf>) {
        self.ssh_dir = ssh_dir;
    }

    fn open_import(&mut self, source: ImportSource, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = cx.entity().downgrade();
        let on_imported = Rc::new(
            move |outcome: ImportOutcome, window: &mut Window, cx: &mut App| {
                workspace
                    .update(cx, |workspace, cx| workspace.imported(outcome, window, cx))
                    .ok();
            },
        );
        let return_focus = if source == ImportSource::Xshell {
            self.settings_tab()
                .map(|panel| panel.read(cx).focus_handle(cx))
                .unwrap_or_else(|| self.host_panel.read(cx).focus_handle(cx))
        } else {
            self.host_panel.read(cx).focus_handle(cx)
        };
        open_import_dialog(
            source,
            self.ssh_dir.clone(),
            self.store.clone(),
            on_imported,
            return_focus,
            window,
            cx,
        );
    }

    /// Count an import and say what it added. A write that failed has
    /// said so already; success is not claimed on top of it.
    fn imported(&mut self, outcome: ImportOutcome, window: &mut Window, cx: &mut Context<Self>) {
        if outcome.hosts == 0 {
            return;
        }
        match outcome.source {
            ImportSource::SshConfig => self.count(Counter::ImportSshConfig),
            ImportSource::KnownHosts => self.count(Counter::ImportKnownHosts),
            ImportSource::Xshell => {}
        }
        if outcome.failed {
            return;
        }
        let title = tn!("host.import.done", outcome.hosts);
        let notification = match outcome.credentials {
            0 => Notification::success(title),
            credentials => {
                Notification::success(tn!("host.import.new_credentials", credentials)).title(title)
            }
        };
        window.push_notification(notification, cx);
    }
}
