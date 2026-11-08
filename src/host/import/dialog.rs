//! The import dialog: the hosts a source names, one row each, to pick,
//! name, group and decide how each logs in before they are added.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    dialog::{DialogAction, DialogClose, DialogFooter},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    scroll::ScrollableElement as _,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::i18n::{UiLocale, t, tn};
use crate::shared::{dismiss_form_error, form_error_notification};

use super::super::host_dialog::credential_option;
use super::super::{HostStore, join_sentences};
use super::plan::refresh_rows;
use super::{
    ImportLoad, ImportRow, ImportSource, RowAuth, apply_import, import_rows, plan_import,
    read_source,
};

/// What an import added, for the workspace to count and report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportOutcome {
    pub source: ImportSource,
    pub hosts: usize,
    pub credentials: usize,
    /// Whether some of it did not reach the database: that is reported on
    /// its own, so the import does not also claim success.
    pub failed: bool,
}

/// What runs once an import has been written.
pub type ImportHandler = Rc<dyn Fn(ImportOutcome, &mut Window, &mut App)>;

/// Every row is this tall: `uniform_list` measures the first and lays the
/// rest out alike, so rows without inputs must not be shorter.
const ROW_HEIGHT: f32 = 2.75;

/// Open the dialog for `source`. It reads the source's file in `ssh_dir`
/// (none: there is nothing to read) and, once the user imports, writes the
/// hosts to `store` and hands the outcome to `on_imported`. Focus goes to
/// `return_focus` when it closes.
pub fn open_import_dialog(
    source: ImportSource,
    ssh_dir: Option<PathBuf>,
    store: Entity<HostStore>,
    on_imported: ImportHandler,
    return_focus: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let view = cx.new(|cx| ImportDialog::new(source, ssh_dir, store, window, cx));
    window.open_dialog(cx, move |dialog, window, cx| {
        let count = view.read(cx).import_count();
        let viewport = window.viewport_size();
        let return_focus = return_focus.clone();
        dialog
            .title(match source {
                ImportSource::SshConfig => t!("host.import.title.ssh_config"),
                ImportSource::KnownHosts => t!("host.import.title.known_hosts"),
                ImportSource::Xshell => t!("host.import.title.xshell"),
            })
            .w(rems(72.)
                .to_pixels(window.rem_size())
                .min(viewport.width * 0.94))
            .margin_top(viewport.height * 0.05)
            .max_h(viewport.height * 0.9)
            .overlay_closable(false)
            .child(view.clone())
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|button| button.label(t!("common.cancel"))))
                    .child(
                        DialogAction::new().child(
                            Button::new("commit")
                                .primary()
                                .label(tn!("host.import.commit", count))
                                .disabled(count == 0),
                        ),
                    ),
            )
            .on_ok({
                let view = view.clone();
                let on_imported = on_imported.clone();
                move |_, window, cx| {
                    view.update(cx, |view, cx| view.commit(&on_imported, window, cx))
                }
            })
            .on_close(move |_, window, cx| {
                dismiss_form_error(window, cx);
                return_focus.focus(window, cx);
            })
    });
}

/// Where the source's file stands.
#[derive(Clone, Debug, PartialEq, Eq)]
enum LoadState {
    Loading,
    Missing,
    Failed(String),
    /// Read; `hashed` known_hosts entries could not be.
    Loaded {
        hashed: usize,
        skipped: usize,
    },
}

/// A row's inputs, made the first time the row is drawn: a known_hosts
/// file can name thousands of hosts, and each input holds window
/// listeners of its own.
struct RowEditors {
    name: Entity<InputState>,
    group: Entity<InputState>,
    user: Entity<InputState>,
    _subscriptions: [Subscription; 3],
}

pub struct ImportDialog {
    source: ImportSource,
    ssh_dir: Option<PathBuf>,
    /// The file read: the source's own in `ssh_dir`, or one the user chose.
    path: Option<PathBuf>,
    extra_paths: Vec<PathBuf>,
    choosing: bool,
    store: Entity<HostStore>,
    state: LoadState,
    /// The rows' values; the inputs write into them.
    rows: Vec<ImportRow>,
    editors: HashMap<usize, RowEditors>,
    scroll: UniformListScrollHandle,
    _load: Task<()>,
    _choose: Option<Task<()>>,
    _store_changes: Subscription,
    _locale: Subscription,
}

impl ImportDialog {
    fn new(
        source: ImportSource,
        ssh_dir: Option<PathBuf>,
        store: Entity<HostStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // The external CLI changes the store while the dialog is up too.
        let store_changes = cx.observe(&store, |this, store, cx| {
            refresh_rows(&mut this.rows, store.read(cx));
            cx.notify();
        });
        let locale = cx.observe_global_in::<UiLocale>(window, |this, window, cx| {
            for editors in this.editors.values() {
                editors.group.update(cx, |input, cx| {
                    input.set_placeholder(t!("host.import.group_placeholder"), window, cx)
                });
            }
        });
        let path = (source != ImportSource::Xshell)
            .then(|| ssh_dir.as_deref().map(|dir| source.default_path(dir)))
            .flatten();
        let mut this = Self {
            source,
            ssh_dir,
            path,
            extra_paths: Vec::new(),
            choosing: false,
            store,
            state: LoadState::Loading,
            rows: Vec::new(),
            editors: HashMap::new(),
            scroll: UniformListScrollHandle::new(),
            _load: Task::ready(()),
            _choose: None,
            _store_changes: store_changes,
            _locale: locale,
        };
        this.load(window, cx);
        this
    }

    /// Read the file anew, off the UI thread. Closing the dialog drops the
    /// task, and the read with it.
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rows.clear();
        self.editors.clear();
        let Some(path) = self.path.clone() else {
            self.state = LoadState::Missing;
            cx.notify();
            return;
        };
        self.state = LoadState::Loading;
        let source = self.source;
        let extra_paths = self.extra_paths.clone();
        let ssh_dir = self
            .ssh_dir
            .clone()
            .or_else(|| path.parent().map(PathBuf::from))
            .unwrap_or_default();
        self._load = cx.spawn_in(window, async move |this, cx| {
            let load = cx
                .background_spawn(async move {
                    if source == ImportSource::Xshell {
                        let paths: Vec<_> = std::iter::once(path).chain(extra_paths).collect();
                        super::xshell::read_paths(&paths)
                    } else {
                        read_source(source, &path, &ssh_dir)
                    }
                })
                .await;
            this.update(cx, |this, cx| this.loaded(load, cx)).ok();
        });
        cx.notify();
    }

    fn loaded(&mut self, load: ImportLoad, cx: &mut Context<Self>) {
        self.state = match load {
            ImportLoad::Loaded(scan) => {
                self.rows = import_rows(&scan, self.store.read(cx));
                LoadState::Loaded {
                    hashed: scan.hashed,
                    skipped: scan.skipped,
                }
            }
            ImportLoad::Missing => LoadState::Missing,
            ImportLoad::Failed(error) => LoadState::Failed(error),
        };
        cx.notify();
    }

    /// Read another file of the same kind, picked with the system's picker.
    fn choose_file(&mut self, directory: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.choosing {
            return;
        }
        self.choosing = true;
        cx.notify();
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: !directory,
            directories: directory,
            multiple: self.source == ImportSource::Xshell && !directory,
            prompt: Some(match self.source {
                ImportSource::SshConfig => t!("host.import.choose_prompt.ssh_config"),
                ImportSource::KnownHosts => t!("host.import.choose_prompt.known_hosts"),
                ImportSource::Xshell if directory => t!("host.import.choose_prompt.xshell_folder"),
                ImportSource::Xshell => t!("host.import.choose_prompt.xshell"),
            }),
        });
        self._choose = Some(cx.spawn_in(window, async move |this, cx| {
            let result = receiver.await;
            this.update_in(cx, |this, window, cx| {
                this.choosing = false;
                if let Ok(Ok(Some(paths))) = result {
                    let mut paths = paths.into_iter();
                    if let Some(path) = paths.next() {
                        this.path = Some(path);
                        this.extra_paths = paths.collect();
                        this.load(window, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// How many hosts the commit button would import.
    fn import_count(&self) -> usize {
        self.rows.iter().filter(|row| row.will_import()).count()
    }

    fn selectable_count(&self) -> usize {
        self.rows.iter().filter(|row| !row.existing).count()
    }

    /// Check the rows, then write them; `false` keeps the dialog open.
    fn commit(
        &mut self,
        on_imported: &ImportHandler,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.import_count() == 0 {
            return false;
        }
        let plan = match plan_import(&self.rows, self.store.read(cx)) {
            Ok(plan) => plan,
            Err(error) => {
                window.push_notification(form_error_notification(error.message()), cx);
                return false;
            }
        };
        let imported = self
            .store
            .update(cx, |store, cx| apply_import(plan, store, cx));
        on_imported(
            ImportOutcome {
                source: self.source,
                hosts: imported.hosts,
                credentials: imported.credentials,
                failed: !imported.failures.is_empty(),
            },
            window,
            cx,
        );
        true
    }

    fn set_all_selected(&mut self, selected: bool, cx: &mut Context<Self>) {
        for row in self.rows.iter_mut().filter(|row| !row.existing) {
            row.selected = selected;
        }
        cx.notify();
    }

    fn set_auth(&mut self, ix: usize, auth: RowAuth, cx: &mut Context<Self>) {
        if let Some(row) = self.rows.get_mut(ix) {
            row.auth = auth;
            cx.notify();
        }
    }

    /// The ways a row can log in, as its menu lists them.
    fn auth_choices(&self, ix: usize, cx: &App) -> Vec<(SharedString, RowAuth)> {
        let mut choices = vec![
            (t!("host.auth.no_password"), RowAuth::NoPassword),
            (t!("host.auth.password"), RowAuth::Password),
        ];
        let store = self.store.read(cx);
        if let Some(path) = self
            .rows
            .get(ix)
            .and_then(|row| row.candidate.identity.as_ref())
        {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            choices.push((t!("host.import.auth.new_key", name = name), RowAuth::NewKey));
        }
        let telnet = self
            .rows
            .get(ix)
            .is_some_and(|row| row.candidate.options.protocol.is_telnet());
        choices.extend(
            store
                .credentials()
                .iter()
                .filter(|credential| {
                    !telnet || credential.kind == super::super::CredentialKind::Password
                })
                .map(|credential| {
                    (
                        credential_option(credential, store.key_dir()),
                        RowAuth::Credential(credential.id),
                    )
                }),
        );
        choices
    }

    /// What the row's auth button says.
    fn auth_label(&self, row: &ImportRow, cx: &App) -> SharedString {
        match row.auth {
            RowAuth::NoPassword => t!("host.auth.no_password"),
            RowAuth::Password => t!("host.auth.password"),
            RowAuth::NewKey => t!(
                "host.import.auth.new_key",
                name = row
                    .candidate
                    .identity
                    .as_ref()
                    .and_then(|path| path.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
            RowAuth::Credential(id) => {
                let store = self.store.read(cx);
                store
                    .credential(id)
                    .map(|credential| credential_option(credential, store.key_dir()))
                    .unwrap_or_else(|| t!("host.auth.no_password"))
            }
        }
    }

    /// Make a row's inputs if it has none yet. Rows already saved get none.
    fn ensure_editors(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix) else {
            return;
        };
        if row.existing || self.editors.contains_key(&ix) {
            return;
        }
        let (name, group, user) = (row.name.clone(), row.group.clone(), row.user.clone());
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(row_address(&self.rows[ix]))
                .default_value(name)
        });
        let group = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("host.import.group_placeholder"))
                .default_value(group)
        });
        let user = cx.new(|cx| InputState::new(window, cx).default_value(user));
        let subscriptions = [
            self.watch(&name, ix, |row, value| row.name = value, cx),
            self.watch(&group, ix, |row, value| row.group = value, cx),
            self.watch(&user, ix, |row, value| row.user = value, cx),
        ];
        self.editors.insert(
            ix,
            RowEditors {
                name,
                group,
                user,
                _subscriptions: subscriptions,
            },
        );
    }

    /// Write an input's text into its row as it is typed, and keep the
    /// row in view when Tab reaches it.
    fn watch(
        &self,
        input: &Entity<InputState>,
        ix: usize,
        write: fn(&mut ImportRow, String),
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe(
            input,
            move |this, input, event: &InputEvent, cx| match event {
                InputEvent::Change => {
                    let value = input.read(cx).value().to_string();
                    if let Some(row) = this.rows.get_mut(ix) {
                        write(row, value);
                    }
                    cx.notify();
                }
                InputEvent::Focus => this.scroll.scroll_to_item(ix, ScrollStrategy::Nearest),
                _ => {}
            },
        )
    }

    fn render_note(&self, cx: &App) -> impl IntoElement {
        let mut sentences = vec![match self.source {
            ImportSource::SshConfig => t!("host.import.note.ssh_config"),
            ImportSource::KnownHosts => t!("host.import.note.known_hosts"),
            ImportSource::Xshell => t!("host.import.note.xshell"),
        }];
        if let LoadState::Loaded { hashed, .. } = self.state
            && hashed > 0
        {
            sentences.push(tn!("host.import.note.hashed", hashed));
        }
        if let LoadState::Loaded { skipped, .. } = self.state
            && skipped > 0
        {
            sentences.push(tn!("host.import.note.xshell_skipped", skipped));
        }
        let note = join_sentences(sentences).unwrap_or_default();
        div()
            .id("import-note")
            .test_support()
            .aria_label(note.clone())
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(note)
    }

    fn render_source(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let path = match &self.path {
            Some(path) => path.to_string_lossy().into_owned(),
            None => match self.source {
                ImportSource::SshConfig => "~/.ssh/config".to_string(),
                ImportSource::KnownHosts => "~/.ssh/known_hosts".to_string(),
                ImportSource::Xshell => t!("host.import.xshell.choose_source").to_string(),
            },
        };
        let source = t!("host.import.source", path = path);
        let mut counts = Vec::new();
        if matches!(self.state, LoadState::Loaded { .. }) {
            counts.push(tn!("host.import.count.found", self.rows.len()));
            let existing = self.rows.len() - self.selectable_count();
            if existing > 0 {
                counts.push(tn!("host.import.count.existing", existing));
            }
            counts.push(tn!("host.import.count.selected", self.import_count()));
        }
        let counts = counts.join(" · ");
        h_flex()
            .gap_3()
            .justify_between()
            .text_sm()
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(
                        div()
                            .id("import-source")
                            .test_support()
                            .aria_label(source.clone())
                            .min_w_0()
                            .truncate()
                            .text_color(cx.theme().muted_foreground)
                            .tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx))
                            .child(source),
                    )
                    .child(
                        Button::new("import-choose-file")
                            .outline()
                            .small()
                            .label(t!("host.import.choose_file"))
                            .disabled(self.choosing)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose_file(false, window, cx)
                            })),
                    ),
            )
            .when(self.source == ImportSource::Xshell, |row| {
                row.child(
                    Button::new("import-choose-folder")
                        .outline()
                        .small()
                        .label(t!("host.import.choose_folder"))
                        .disabled(self.choosing)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.choose_file(true, window, cx)),
                        ),
                )
            })
            .child(
                div()
                    .id("import-counts")
                    .test_support()
                    .aria_label(counts.clone())
                    .flex_shrink_0()
                    .text_color(cx.theme().muted_foreground)
                    .child(counts),
            )
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selectable = self.selectable_count();
        let all = selectable > 0 && self.import_count() == selectable;
        let theme = cx.theme();
        row_frame(
            h_flex()
                .id("import-header")
                .h(rems(2.25))
                .border_b_1()
                .border_color(theme.border)
                .text_xs()
                .text_color(theme.muted_foreground),
            Checkbox::new("import-select-all")
                .small()
                .checked(all)
                .disabled(selectable == 0)
                .accessibility_label(t!("host.import.select_all"))
                .on_click(cx.listener(move |this, _: &bool, _, cx| this.set_all_selected(!all, cx)))
                .into_any_element(),
            [
                t!("host.import.column.name"),
                t!("host.import.column.group"),
                t!("host.import.column.address"),
                if self.source == ImportSource::Xshell {
                    t!("host.import.column.protocol_port")
                } else {
                    t!("host.import.column.port")
                },
                t!("host.import.column.user"),
                t!("host.import.column.auth"),
                t!("host.import.column.status"),
            ]
            .map(|label| div().truncate().child(label).into_any_element()),
        )
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let row = &self.rows[ix];
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        // A row saved meanwhile (by the external CLI) keeps its inputs but
        // shows its values as text, like any other saved row.
        let editors = self.editors.get(&ix).filter(|_| !row.existing);
        let text = |id: &'static str, value: String| {
            let full = value.clone();
            div()
                .id((id, ix))
                .test_support()
                .aria_label(value.clone())
                .truncate()
                .text_color(muted)
                .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                .child(value)
                .into_any_element()
        };
        let name = match editors {
            Some(editors) => Input::new(&editors.name)
                .id(("import-name", ix))
                .small()
                .into_any_element(),
            None => text("import-name", row.name.clone()),
        };
        let group = match editors {
            Some(editors) => Input::new(&editors.group)
                .id(("import-group", ix))
                .small()
                .into_any_element(),
            None => text("import-group", row.group.clone()),
        };
        let credential_user = match row.auth {
            RowAuth::Credential(id) => self
                .store
                .read(cx)
                .credential(id)
                .map(|credential| credential.user.to_string()),
            _ => None,
        };
        let user = match (editors, credential_user) {
            (_, Some(user)) => div()
                .id(("import-user", ix))
                .test_support()
                .aria_label(user.clone())
                .truncate()
                .text_color(muted)
                .tooltip(|window, cx| {
                    Tooltip::new(t!("host.import.user_from_credential")).build(window, cx)
                })
                .child(user)
                .into_any_element(),
            (Some(editors), None) => Input::new(&editors.user)
                .id(("import-user", ix))
                .small()
                .into_any_element(),
            (None, None) => text("import-user", row.user.clone()),
        };
        let label = self.auth_label(row, cx);
        let auth = if row.existing {
            text("import-auth", label.to_string())
        } else {
            let this = cx.entity().downgrade();
            // The menu wraps the button in a box as wide as the label; the
            // column stretches that box to the cell, so a long credential
            // ends in an ellipsis instead of running over 状态. The tooltip
            // has it in full.
            v_flex()
                .w_full()
                .child(
                    Button::new(("import-auth", ix))
                        .outline()
                        .small()
                        .w_full()
                        .dropdown_caret(true)
                        .label(label.clone())
                        .tooltip(label)
                        .dropdown_menu(move |menu, window, cx| {
                            let Some(view) = this.upgrade() else {
                                return menu;
                            };
                            let (choices, chosen) = {
                                let dialog = view.read(cx);
                                let chosen = dialog.rows.get(ix).map(|row| row.auth);
                                (dialog.auth_choices(ix, cx), chosen)
                            };
                            let menu = menu
                                .max_h(rems(20.).to_pixels(window.rem_size()))
                                .scrollable(true);
                            auth_menu(menu, choices, chosen, view.downgrade(), ix)
                        }),
                )
                .into_any_element()
        };
        let status = if row.existing {
            t!("host.import.existing")
        } else {
            t!("host.import.new")
        };
        let status = div()
            .id(("import-status", ix))
            .test_support()
            .aria_label(status.clone())
            .when(row.existing, |status| status.text_color(muted))
            .child(status)
            .into_any_element();
        let full_address = row.candidate.address.clone();
        let address = div()
            .id(("import-address", ix))
            .test_support()
            .aria_label(row.candidate.address.clone())
            .truncate()
            .font_family(theme.mono_font_family.clone())
            .when(row.existing, |address| address.text_color(muted))
            .tooltip(move |window, cx| Tooltip::new(full_address.clone()).build(window, cx))
            .child(row.candidate.address.clone())
            .into_any_element();
        let port_text = if self.source == ImportSource::Xshell {
            format!(
                "{} {}",
                row.candidate.options.protocol.as_str().to_uppercase(),
                row.candidate.port
            )
        } else {
            row.candidate.port.to_string()
        };
        let port = div()
            .id(("import-port", ix))
            .test_support()
            .aria_label(port_text.clone())
            .font_family(theme.mono_font_family.clone())
            .when(row.existing, |port| port.text_color(muted))
            .child(port_text)
            .into_any_element();
        let checkbox = Checkbox::new(("import-select", ix))
            .small()
            .checked(row.will_import())
            .disabled(row.existing)
            .accessibility_label(row.name.clone())
            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                if let Some(row) = this.rows.get_mut(ix) {
                    row.selected = *checked;
                }
                cx.notify();
            }))
            .into_any_element();
        row_frame(
            h_flex()
                .id(("import-row", ix))
                .h(rems(ROW_HEIGHT))
                .border_b_1()
                .border_color(theme.border)
                .text_sm(),
            checkbox,
            [name, group, address, port, user, auth, status],
        )
        .into_any_element()
    }

    /// The table's box: the header over the rows, or over what stands in
    /// for them while there are none. Its height does not change, so the
    /// dialog does not jump when the file has been read.
    fn render_table(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem = window.rem_size();
        let height = (window.viewport_size().height * 0.45)
            .max(rems(10.).to_pixels(rem))
            .min(rems(26.).to_pixels(rem));
        let placeholder = match &self.state {
            LoadState::Loading => Some(t!("host.import.loading")),
            LoadState::Missing if self.source == ImportSource::Xshell && self.path.is_none() => {
                Some(t!("host.import.xshell.choose_source"))
            }
            LoadState::Missing => Some(t!(
                "host.import.missing",
                path = self
                    .path
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_else(|| match self.source {
                        ImportSource::SshConfig => "~/.ssh/config".to_string(),
                        ImportSource::KnownHosts => "~/.ssh/known_hosts".to_string(),
                        ImportSource::Xshell => t!("host.import.xshell.choose_source").to_string(),
                    })
            )),
            LoadState::Failed(error) => Some(t!("host.import.failed", error = error)),
            LoadState::Loaded { .. } if self.rows.is_empty() => Some(match self.source {
                ImportSource::SshConfig => t!("host.import.empty.ssh_config"),
                ImportSource::KnownHosts => t!("host.import.empty.known_hosts"),
                ImportSource::Xshell => t!("host.import.empty.xshell"),
            }),
            LoadState::Loaded { .. } => None,
        };
        let theme = cx.theme();
        let body = div()
            .id("import-rows")
            .test_support()
            .relative()
            .flex_1()
            .min_h_0()
            .map(|body| match placeholder {
                Some(placeholder) => body.child(
                    div()
                        .id("import-empty")
                        .test_support()
                        .aria_label(placeholder.clone())
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .px_4()
                        .text_sm()
                        .text_center()
                        .text_color(cx.theme().muted_foreground)
                        .child(placeholder),
                ),
                None => body.child(
                    uniform_list(
                        "import-list",
                        self.rows.len(),
                        cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                            range
                                .map(|ix| {
                                    this.ensure_editors(ix, window, cx);
                                    this.render_row(ix, cx)
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .size_full()
                    .track_scroll(&self.scroll),
                ),
            })
            .vertical_scrollbar(&self.scroll);
        v_flex()
            .h(height)
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius)
            .overflow_hidden()
            .child(self.render_header(cx))
            .child(body)
    }
}

impl Render for ImportDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("import-dialog")
            .w_full()
            .gap_3()
            .child(self.render_note(cx))
            .child(self.render_source(cx))
            .child(self.render_table(window, cx))
    }
}

/// The columns, the same for the header and every row: the checkbox, then
/// 名称, 分组, 地址, 端口, 用户名, 认证, 状态.
fn row_frame(frame: Stateful<Div>, checkbox: AnyElement, cells: [AnyElement; 7]) -> Stateful<Div> {
    let [name, group, address, port, user, auth, status] = cells;
    let cell = |width: Option<f32>, child: AnyElement| {
        div()
            .h_full()
            .flex()
            .items_center()
            .min_w_0()
            .map(|cell| match width {
                Some(width) => cell.w(rems(width)).flex_shrink_0(),
                None => cell.flex_1(),
            })
            .child(child)
    };
    frame
        .w_full()
        .px_2()
        .gap_2()
        .child(cell(Some(1.25), checkbox))
        .child(cell(None, name))
        .child(cell(None, group))
        .child(cell(None, address))
        .child(cell(Some(6.), port))
        .child(cell(Some(7.), user))
        .child(cell(Some(14.), auth))
        .child(cell(Some(4.5), status))
}

/// A row's auth menu: the choices, the current one checked.
fn auth_menu(
    menu: PopupMenu,
    choices: Vec<(SharedString, RowAuth)>,
    chosen: Option<RowAuth>,
    view: WeakEntity<ImportDialog>,
    ix: usize,
) -> PopupMenu {
    let credentials_start = choices
        .iter()
        .position(|(_, auth)| matches!(auth, RowAuth::NewKey | RowAuth::Credential(_)));
    let mut menu = menu;
    for (position, (label, auth)) in choices.into_iter().enumerate() {
        if Some(position) == credentials_start {
            menu = menu.separator();
        }
        let view = view.clone();
        menu = menu.item(
            PopupMenuItem::new(label)
                .checked(chosen == Some(auth))
                .on_click(move |_, _, cx| {
                    view.update(cx, |view, cx| view.set_auth(ix, auth, cx)).ok();
                }),
        );
    }
    menu
}

/// What a row's name field shows while empty: the host is then named
/// after its address.
fn row_address(row: &ImportRow) -> String {
    row.candidate.address.clone()
}
