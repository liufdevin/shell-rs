use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, IndexPath, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{DialogClose, DialogFooter},
    h_flex,
    input::{Input, InputEvent, InputState},
    list::{List, ListDelegate, ListItem, ListState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{CatalogIcon, SerialFileDownload};
use crate::i18n::t;
use crate::terminal::{RemoteTerminalId, TerminalView, exec_answer};

use super::files::{
    COMMAND_SETTLE, SerialFileEntry, directory_command, parent_directory, parse_directory,
};

struct FileList {
    owner: WeakEntity<SerialFilePicker>,
    entries: Vec<SerialFileEntry>,
    shown: Vec<usize>,
    selected: Option<usize>,
    query: String,
    loading: bool,
}

impl FileList {
    fn refilter(&mut self) {
        self.shown = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.name().to_lowercase().contains(&self.query))
            .map(|(index, _)| index)
            .collect();
        self.selected = None;
    }

    fn selected_file(&self) -> Option<&SerialFileEntry> {
        self.selected
            .and_then(|index| self.entries.get(index))
            .filter(|entry| !entry.is_directory())
    }
}

impl ListDelegate for FileList {
    type Item = ListItem;

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.shown.len()
    }
    fn loading(&self, _: &App) -> bool {
        self.loading
    }

    fn perform_search(
        &mut self,
        query: &str,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.query = query.to_lowercase();
        self.refilter();
        Task::ready(())
    }

    fn set_selected_index(
        &mut self,
        index: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
        self.selected = index.and_then(|index| self.shown.get(index.row)).copied();
    }

    fn render_item(
        &mut self,
        index: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let entry = self.entries.get(*self.shown.get(index.row)?)?;
        let owner = self.owner.clone();
        let path = entry.path().to_string();
        let directory = entry.is_directory();
        Some(
            ListItem::new(SharedString::from(format!("serial-file-entry:{path}")))
                .py_1p5()
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Icon::new(if directory {
                                IconName::Folder
                            } else {
                                IconName::File
                            })
                            .small()
                            .text_color(cx.theme().muted_foreground),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(entry.name().to_string()),
                        ),
                )
                .on_click(move |event, window, cx| {
                    if !directory && event.click_count() == 2 {
                        let owner = owner.clone();
                        let path = path.clone();
                        window.defer(cx, move |window, cx| {
                            owner
                                .update(cx, |picker, cx| {
                                    picker.commit_path(&path, true, window, cx)
                                })
                                .ok();
                        });
                    }
                }),
        )
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        if self.loading {
            return;
        }
        let entry = self
            .selected
            .and_then(|index| self.entries.get(index))
            .filter(|entry| entry.is_directory());
        if let Some(entry) = entry {
            let path = entry.path().to_string();
            let owner = self.owner.clone();
            window.defer(cx, move |window, cx| {
                owner
                    .update(cx, |picker, cx| picker.navigate(&path, window, cx))
                    .ok();
            });
        }
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        window.close_dialog(cx);
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        div()
            .id("serial-directory-empty")
            .test_support()
            .p_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(t!("serial.files.empty"))
    }
}

struct SerialFilePicker {
    terminal: RemoteTerminalId,
    view: WeakEntity<TerminalView>,
    generation: u64,
    dispatch: FocusHandle,
    directory: String,
    path: Entity<InputState>,
    search: Entity<InputState>,
    list: Entity<ListState<FileList>>,
    request: u64,
    task: Task<()>,
    error: Option<SharedString>,
    truncated: bool,
    stale: bool,
    _subscriptions: Vec<Subscription>,
}

impl SerialFilePicker {
    fn new(
        terminal: RemoteTerminalId,
        view: Entity<TerminalView>,
        dispatch: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let path =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("serial.files.directory_path")));
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("serial.files.search")));
        let owner = cx.entity().downgrade();
        let list = cx.new(|cx| {
            ListState::new(
                FileList {
                    owner,
                    entries: Vec::new(),
                    shown: Vec::new(),
                    selected: None,
                    query: String::new(),
                    loading: true,
                },
                window,
                cx,
            )
            .searchable(false)
        });
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, input, event, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = input.read(cx).value().to_string();
                    this.list
                        .update(cx, |list, cx| list.set_query(&query, window, cx));
                }
            }),
            cx.observe_in(&list, window, |_, _, window, cx| {
                window.refresh();
                cx.notify();
            }),
            cx.observe_in(&view, window, |this, view, window, cx| {
                if view.read(cx).generation(cx) != this.generation && !this.stale {
                    this.stale = true;
                    this.fail(t!("serial.files.connection_changed"), window, cx);
                    window.refresh();
                }
            }),
            cx.subscribe_in(&path, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let path = this.path.read(cx).value().to_string();
                    this.navigate(&path, window, cx);
                }
            }),
        ];
        Self {
            terminal,
            generation: view.read(cx).generation(cx),
            view: view.downgrade(),
            dispatch,
            directory: String::new(),
            path,
            search,
            list,
            request: 0,
            task: Task::ready(()),
            error: None,
            truncated: false,
            stale: false,
            _subscriptions: subscriptions,
        }
    }

    fn navigate(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.request > 0 && self.list.read(cx).delegate().loading {
            return;
        }
        let Some(view) = self.view.upgrade() else {
            self.fail(t!("tools.not_connected"), window, cx);
            return;
        };
        if view.read(cx).generation(cx) != self.generation {
            self.fail(t!("serial.files.connection_changed"), window, cx);
            return;
        }
        let Some(command) = directory_command(path) else {
            self.fail(t!("serial.files.invalid_path"), window, cx);
            return;
        };
        let encoding = view.read(cx).status(cx).encoding();
        self.request += 1;
        let request = self.request;
        let generation = self.generation;
        self.error = None;
        self.search
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.list.update(cx, |list, cx| {
            list.delegate_mut().query.clear();
            list.set_query("", window, cx);
            list.delegate_mut().loading = true;
            list.set_selected_index(None, window, cx);
            cx.notify();
        });
        self.task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(COMMAND_SETTLE).await;
            let Ok(answer) = this.update(cx, |this, cx| {
                let view = this.view.upgrade()?;
                if this.request != request || view.read(cx).generation(cx) != generation {
                    return None;
                }
                view.read(cx).exec(command, cx)
            }) else {
                return;
            };
            let result = match answer {
                Some(answer) => match exec_answer(answer, cx).await {
                    Some(Ok(output)) => parse_directory(&output, encoding),
                    Some(Err(error)) => Err(error),
                    None => Err(t!("tools.not_connected").to_string()),
                },
                None => Err(t!("tools.not_connected").to_string()),
            };
            this.update_in(cx, |this, window, cx| {
                if this.request != request {
                    return;
                }
                if this
                    .view
                    .upgrade()
                    .is_none_or(|view| view.read(cx).generation(cx) != generation)
                {
                    this.fail(t!("serial.files.connection_changed"), window, cx);
                    return;
                }
                match result {
                    Ok(directory) => {
                        this.directory = directory.path().to_string();
                        this.path.update(cx, |path, cx| {
                            path.set_value(this.directory.clone(), window, cx)
                        });
                        this.truncated = directory.truncated();
                        this.list.update(cx, |list, cx| {
                            list.delegate_mut().entries = directory.entries().to_vec();
                            list.delegate_mut().refilter();
                            list.delegate_mut().loading = false;
                            list.set_selected_index(None, window, cx);
                            cx.notify();
                        });
                        cx.notify();
                    }
                    Err(error) => this.fail(error.into(), window, cx),
                }
            })
            .ok();
        });
        cx.notify();
    }

    fn fail(&mut self, error: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.error = Some(error);
        self.list.update(cx, |list, cx| {
            list.delegate_mut().loading = false;
            cx.notify();
        });
        if !self.directory.is_empty() {
            self.path.update(cx, |path, cx| {
                path.set_value(self.directory.clone(), window, cx)
            });
        }
        cx.notify();
    }

    fn can_download(&self, cx: &App) -> bool {
        let list = self.list.read(cx).delegate();
        !self.stale && !list.loading && list.selected_file().is_some()
    }

    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.can_download(cx) {
            return false;
        }
        let path = self
            .list
            .read(cx)
            .delegate()
            .selected_file()
            .unwrap()
            .path()
            .to_string();
        self.commit_path(&path, false, window, cx)
    }

    fn commit_path(
        &mut self,
        path: &str,
        close: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let list = self.list.read(cx).delegate();
        if list.loading
            || !list
                .entries
                .iter()
                .any(|entry| !entry.is_directory() && entry.path() == path)
        {
            return false;
        }
        if self
            .view
            .upgrade()
            .is_none_or(|view| view.read(cx).generation(cx) != self.generation)
        {
            self.fail(t!("serial.files.connection_changed"), window, cx);
            return false;
        }
        let action = SerialFileDownload::new(self.terminal, self.generation, path.to_string());
        let dispatch = self.dispatch.clone();
        if close {
            window.close_dialog(cx);
        }
        window.defer(cx, move |window, cx| {
            dispatch.dispatch_action(&action, window, cx)
        });
        true
    }
}

impl Render for SerialFilePicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let loading = self.list.read(cx).delegate().loading;
        let selected = self
            .list
            .read(cx)
            .delegate()
            .selected_file()
            .map(|entry| entry.path().to_string());
        v_flex()
            .id("serial-download-browser")
            .test_support()
            .w_full()
            .gap_2()
            .child(div().text_sm().child(t!("serial.files.directory_path")))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("serial-directory-up")
                            .small()
                            .icon(IconName::ArrowUp)
                            .tooltip(t!("serial.files.up"))
                            .accessibility_label(t!("serial.files.up"))
                            .disabled(loading || self.directory.is_empty() || self.directory == "/")
                            .on_click(cx.listener(|this, _, window, cx| {
                                let parent = parent_directory(&this.directory).to_string();
                                this.navigate(&parent, window, cx);
                            })),
                    )
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.path)
                                .id("serial-directory-path")
                                .small()
                                .disabled(loading),
                        ),
                    )
                    .child(
                        Button::new("serial-directory-open")
                            .small()
                            .label(t!("serial.files.open"))
                            .disabled(loading)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let path = this.path.read(cx).value().to_string();
                                this.navigate(&path, window, cx);
                            })),
                    )
                    .child(
                        Button::new("serial-directory-refresh")
                            .small()
                            .icon(CatalogIcon::RefreshCw)
                            .tooltip(t!("serial.files.refresh"))
                            .accessibility_label(t!("serial.files.refresh"))
                            .disabled(loading)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let path = if this.directory.is_empty() {
                                    ".".to_string()
                                } else {
                                    this.directory.clone()
                                };
                                this.navigate(&path, window, cx);
                            })),
                    ),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .id("serial-directory-error")
                        .test_support()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .child(
                div()
                    .h_80()
                    .w_full()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius)
                    .child(
                        v_flex()
                            .size_full()
                            .child(Input::new(&self.search).id("serial-file-search").small())
                            .child(List::new(&self.list).small()),
                    ),
            )
            .when(self.truncated, |view| {
                view.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("serial.files.truncated")),
                )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("serial.files.browse_hint")),
            )
            .child(
                div()
                    .id("serial-download-selection")
                    .test_support()
                    .text_sm()
                    .truncate()
                    .child(selected.unwrap_or_else(|| t!("serial.files.select_file").to_string())),
            )
    }
}

pub fn open_serial_download_dialog(
    terminal: RemoteTerminalId,
    view: Entity<TerminalView>,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let picker = cx.new(|cx| SerialFilePicker::new(terminal, view, dispatch, window, cx));
    picker.update(cx, |picker, cx| picker.navigate(".", window, cx));
    window.open_dialog(cx, move |dialog, window, cx| {
        let commit = picker.clone();
        let download = picker.clone();
        dialog
            .title(t!("serial.files.download"))
            .max_h(window.viewport_size().height * 0.9)
            .child(picker.clone())
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|button| button.label(t!("common.cancel"))))
                    .child(
                        Button::new("serial-download")
                            .primary()
                            .label(t!("serial.files.download_commit"))
                            .disabled(!picker.read(cx).can_download(cx))
                            .on_click(move |_, window, cx| {
                                if download.update(cx, |picker, cx| picker.commit(window, cx)) {
                                    window.close_dialog(cx);
                                }
                            }),
                    ),
            )
            .on_ok(move |_, window, cx| commit.update(cx, |picker, cx| picker.commit(window, cx)))
    });
}
