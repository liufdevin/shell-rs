//! ZMODEM in a terminal: the files or the folder the user chooses when
//! `rz` or `sz` starts on the other end, and files dropped on a remote
//! terminal.

use std::{path::PathBuf, time::Duration};

use gpui_kit::component::{WindowExt as _, notification::Notification};
use gpui_kit::*;

use super::Workspace;
use crate::analytics::Counter;
use crate::app::{CenterTab, ExplorerAction, ExplorerCommand};
use crate::explorer::TransferJob;
use crate::host::{ConnectionState, HostConnection};
use crate::i18n::t;
use crate::terminal::{
    RemoteTerminalId, TerminalLifecycle, ZmodemDirection, ZmodemReply, exec_answer,
};

impl Workspace {
    /// `rz` or `sz` started in the terminal of `tab`: the system's picker
    /// for the files to upload, or the folder to download into. Choosing
    /// nothing cancels the transfer.
    pub(super) fn ask_zmodem(
        &mut self,
        tab: CenterTab,
        direction: ZmodemDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let options = match direction {
            ZmodemDirection::Upload => PathPromptOptions {
                files: true,
                directories: false,
                multiple: true,
                prompt: Some(t!("workspace.zmodem.choose_files")),
            },
            ZmodemDirection::Download => PathPromptOptions {
                files: false,
                directories: true,
                multiple: false,
                prompt: Some(t!("workspace.zmodem.choose_folder")),
            },
        };
        let chosen = cx.prompt_for_paths(options);
        cx.spawn_in(window, async move |this, cx| {
            let chosen = chosen
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .unwrap_or_default();
            this.update(cx, |this, cx| {
                let reply = match (direction, chosen.first()) {
                    (_, None) => ZmodemReply::Declined,
                    (ZmodemDirection::Upload, Some(_)) => {
                        this.count(Counter::ZmodemUpload);
                        ZmodemReply::Files(chosen)
                    }
                    (ZmodemDirection::Download, Some(folder)) => {
                        this.count(Counter::ZmodemDownload);
                        ZmodemReply::Folder(folder.clone())
                    }
                };
                this.reply_zmodem(tab, reply, cx);
            })
            .ok();
        })
        .detach();
    }

    fn reply_zmodem(&self, tab: CenterTab, reply: ZmodemReply, cx: &App) {
        match tab {
            CenterTab::Terminal(id) => {
                if let Some(panel) = self.terminals.get(&id) {
                    panel.read(cx).zmodem_reply(reply, cx);
                }
            }
            CenterTab::LocalTerminal(id) => {
                if let Some(panel) = self.local_terminals.get(&id) {
                    panel.read(cx).zmodem_reply(reply, cx);
                }
            }
            _ => {}
        }
    }

    pub(super) fn upload_dropped(
        &mut self,
        id: RemoteTerminalId,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.terminals.get(&id).cloned() else {
            return;
        };
        if matches!(panel.read(cx).connection(), HostConnection::Ssh(_)) {
            self.upload_dropped_sftp(id, paths.to_vec(), window, cx);
            return;
        }
        let files: Vec<PathBuf> = paths
            .iter()
            .filter(|path| path.is_file())
            .cloned()
            .collect();
        let uploaded = if files.is_empty() {
            Err(t!("workspace.zmodem.files_only"))
        } else {
            panel.update(cx, |panel, cx| panel.upload_dropped(files, cx))
        };
        match uploaded {
            Ok(()) => self.count(Counter::ZmodemUpload),
            Err(message) => window.push_notification(Notification::warning(message), cx),
        }
    }

    fn upload_dropped_sftp(
        &mut self,
        id: RemoteTerminalId,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        let Some(panel) = self.terminals.get(&id).cloned() else {
            return;
        };
        let view = panel.read(cx).terminal().clone();
        if view.read(cx).lifecycle(cx) != TerminalLifecycle::Running {
            window.push_notification(Notification::warning(t!("remote.connect_first")), cx);
            return;
        }
        let generation = view.read(cx).generation(cx);
        let directory = view.read(cx).directory(cx);
        let hostname = self
            .follow_check(id, cx)
            .map(|check| check.hostname.clone());
        let check = directory
            .as_ref()
            .filter(|directory| directory.host.is_some() && hostname.is_none())
            .and_then(|_| view.read(cx).exec("uname -n".into(), cx));
        if !self.show_terminal_files(id, window, cx) {
            window.push_notification(
                Notification::warning(t!("workspace.terminal_upload.sftp_unavailable")),
                cx,
            );
            return;
        }
        let Some(explorer) = self.file_sidebar(id).cloned() else {
            return;
        };
        let explorer_id = explorer.read(cx).id();
        if explorer.read(cx).connection_state() == ConnectionState::Disconnected {
            self.on_explorer_action(
                &ExplorerAction::new(explorer_id, ExplorerCommand::Reconnect),
                window,
                cx,
            );
        }
        let view = view.downgrade();
        *self.pending_terminal_uploads.entry(id).or_default() += 1;
        cx.spawn_in(window, async move |this, cx| {
            let hostname = match check {
                Some(reply) => exec_answer(reply, cx)
                    .await
                    .and_then(Result::ok)
                    .map(|name| name.trim().to_owned()),
                None => hostname,
            };
            let target = directory
                .filter(|directory| directory.is_from(hostname.as_deref()))
                .map(|directory| directory.path);
            let mut connected_generation = None;
            loop {
                let finished = this
                    .update_in(cx, |this, window, cx| {
                        let Some(view) = view.upgrade() else {
                            return true;
                        };
                        if !this.terminals.contains_key(&id)
                            || view.read(cx).generation(cx) != generation
                            || view.read(cx).lifecycle(cx) != TerminalLifecycle::Running
                        {
                            return true;
                        }
                        let Some(panel) = this.explorers.get(&explorer_id) else {
                            return true;
                        };
                        match panel.read(cx).connection_state() {
                            ConnectionState::Connecting => return false,
                            ConnectionState::Disconnected => return true,
                            ConnectionState::Connected => {}
                        }
                        let current = panel.read(cx).generation();
                        if connected_generation.is_some_and(|previous| previous != current) {
                            return true;
                        }
                        connected_generation = Some(current);
                        if target.is_none() && window.has_active_dialog(cx) {
                            return false;
                        }
                        let command = match &target {
                            Some(target) => ExplorerCommand::Enqueue(TransferJob::Upload {
                                paths: paths.clone(),
                                target: target.clone(),
                            }),
                            None => ExplorerCommand::UploadPaths {
                                paths: paths.clone(),
                                target: panel.read(cx).remote().read(cx).path(),
                            },
                        };
                        this.on_explorer_action(
                            &ExplorerAction::new(explorer_id, command),
                            window,
                            cx,
                        );
                        true
                    })
                    .unwrap_or(true);
                if finished {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
            }
            this.update(cx, |this, _| {
                if let Some(pending) = this.pending_terminal_uploads.get_mut(&id) {
                    *pending -= 1;
                    if *pending == 0 {
                        this.pending_terminal_uploads.remove(&id);
                    }
                }
            })
            .ok();
        })
        .detach();
    }
}
