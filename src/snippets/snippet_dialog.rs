//! The dialogs of 命令片段: a snippet's, and a category's.

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, IndexPath, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    form::{Field, Form},
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    select::{Select, SelectState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::shortcut_editor::SnippetShortcutEditor;
use crate::host::{
    ErrorMessage as _, HostStore, SnippetCategoryId, SnippetDraft, SnippetId, SnippetScope,
    SnippetStep, SnippetWait, by_name,
};
use crate::i18n::t;
use crate::settings::SettingsStore;
use crate::shared::{commit_footer, dismiss_form_error, form_error_notification};

/// How wide the snippet dialog is: room for a command of a fair length on
/// one line.
const DIALOG_WIDTH: f32 = 36.;

/// Which snippet dialog to open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnippetDialog {
    /// A new snippet, in this category or none.
    New(Option<SnippetCategoryId>),
    Edit(SnippetId),
}

/// The body of the new and edit snippet dialog. Validates on commit; the
/// store is only touched when that passes.
pub struct SnippetForm {
    store: Entity<HostStore>,
    settings: Entity<SettingsStore>,
    shortcut: Entity<SnippetShortcutEditor>,
    editing: Option<SnippetId>,
    /// Kept as it is: nothing sets it yet.
    scope: SnippetScope,
    name: Entity<InputState>,
    category: Entity<SelectState<Vec<SharedString>>>,
    /// Parallel to the category select's rows; `None` is 未分类.
    category_ids: Vec<Option<SnippetCategoryId>>,
    command: Entity<TextareaState>,
    run_on_click: bool,
    kind: Entity<SelectState<Vec<SharedString>>>,
    steps: Vec<StepFields>,
    next_step_id: u64,
    dispatch: FocusHandle,
    _kind_changed: Subscription,
}

struct StepFields {
    id: u64,
    content: Entity<TextareaState>,
    wait: Entity<SelectState<Vec<SharedString>>>,
    seconds: Entity<InputState>,
    prompt: Entity<InputState>,
    timeout: Entity<InputState>,
    enter: bool,
    _wait_changed: Subscription,
}

impl SnippetForm {
    fn new(
        dialog: SnippetDialog,
        store: Entity<HostStore>,
        settings: Entity<SettingsStore>,
        dispatch: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (editing, draft, mut categories) = {
            let read = store.read(cx);
            let (editing, draft) = match dialog {
                SnippetDialog::Edit(id) => match read.snippet(id) {
                    Some(snippet) => (Some(id), snippet.draft()),
                    None => (None, SnippetDraft::new("", "", None)),
                },
                // A new snippet runs on a click unless told otherwise.
                SnippetDialog::New(category) => (
                    None,
                    SnippetDraft::new("", "", category).with_run_on_click(true),
                ),
            };
            let categories: Vec<(SnippetCategoryId, SharedString)> = read
                .snippet_categories()
                .iter()
                .map(|category| (category.id, category.name.clone()))
                .collect();
            (editing, draft, categories)
        };
        categories.sort_by(|a, b| by_name((&a.1, a.0.0), (&b.1, b.0.0)));
        let mut category_ids: Vec<Option<SnippetCategoryId>> = vec![None];
        // The category list's row for a snippet of none.
        let mut labels: Vec<SharedString> = vec![t!("snippets.uncategorized")];
        for (id, name) in categories {
            category_ids.push(Some(id));
            labels.push(name);
        }
        let selected = category_ids
            .iter()
            .position(|id| *id == draft.category)
            .unwrap_or(0);

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("snippets.dialog.name_placeholder"))
                .default_value(draft.name.clone())
        });
        let category =
            cx.new(|cx| SelectState::new(labels, Some(IndexPath::new(selected)), window, cx));
        let command = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 12)
                .placeholder(t!("snippets.dialog.command_placeholder"))
                .default_value(draft.command.clone())
        });
        let public_id = editing.and_then(|id| {
            store
                .read(cx)
                .snippet(id)
                .map(|snippet| snippet.sync_id.to_string())
        });
        let shortcut =
            cx.new(|cx| SnippetShortcutEditor::new(settings.clone(), public_id, window, cx));
        let kind = cx.new(|cx| {
            SelectState::new(
                vec![t!("snippets.group.single"), t!("snippets.group.kind")],
                Some(IndexPath::new(usize::from(draft.steps.is_some()))),
                window,
                cx,
            )
        });
        let kind_changed = cx.observe(&kind, |_, _, cx| cx.notify());
        let mut form = Self {
            store,
            settings,
            shortcut,
            editing,
            scope: draft.scope,
            name,
            category,
            category_ids,
            command,
            run_on_click: draft.run_on_click,
            kind,
            steps: Vec::new(),
            next_step_id: 1,
            dispatch,
            _kind_changed: kind_changed,
        };
        for step in draft.steps.unwrap_or_else(|| {
            vec![SnippetStep::new(
                draft.command,
                SnippetWait::Immediate,
                true,
            )]
        }) {
            form.push_step(step, window, cx);
        }
        form
    }

    fn is_group(&self, cx: &App) -> bool {
        self.kind
            .read(cx)
            .selected_index(cx)
            .is_some_and(|index| index.row == 1)
    }

    fn push_step(&mut self, step: SnippetStep, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.next_step_id;
        self.next_step_id += 1;
        let (mode, seconds, prompt, timeout) = match step.wait {
            SnippetWait::Immediate => (0, "1".to_owned(), String::new(), "30".to_owned()),
            SnippetWait::Delay { milliseconds } => (
                1,
                (f64::from(milliseconds) / 1000.).to_string(),
                String::new(),
                "30".to_owned(),
            ),
            SnippetWait::Prompt { text, timeout_ms } => (
                2,
                "1".to_owned(),
                text,
                (f64::from(timeout_ms) / 1000.).to_string(),
            ),
        };
        let wait = cx.new(|cx| {
            SelectState::new(
                vec![
                    t!("snippets.group.immediate"),
                    t!("snippets.group.delay"),
                    t!("snippets.group.prompt"),
                ],
                Some(IndexPath::new(mode)),
                window,
                cx,
            )
        });
        let wait_changed = cx.observe(&wait, |_, _, cx| cx.notify());
        self.steps.push(StepFields {
            id,
            content: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(1, 3)
                    .default_value(step.content)
            }),
            wait,
            seconds: cx.new(|cx| InputState::new(window, cx).default_value(seconds)),
            prompt: cx.new(|cx| InputState::new(window, cx).default_value(prompt)),
            timeout: cx.new(|cx| InputState::new(window, cx).default_value(timeout)),
            enter: step.enter,
            _wait_changed: wait_changed,
        });
    }

    pub(crate) fn add_step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.steps.len() < 128 {
            self.push_step(
                SnippetStep::new("", SnippetWait::Immediate, true),
                window,
                cx,
            );
            cx.notify();
        }
    }

    pub(crate) fn move_step(&mut self, id: u64, down: bool, cx: &mut Context<Self>) {
        if let Some(index) = self.steps.iter().position(|step| step.id == id) {
            let other = if down {
                index.checked_add(1)
            } else {
                index.checked_sub(1)
            };
            if let Some(other) = other.filter(|other| *other < self.steps.len()) {
                self.steps.swap(index, other);
                cx.notify();
            }
        }
    }

    pub(crate) fn remove_step(&mut self, id: u64, cx: &mut Context<Self>) {
        self.steps.retain(|step| step.id != id);
        cx.notify();
    }

    fn group_steps(&self, cx: &App) -> Result<Vec<SnippetStep>, SharedString> {
        let milliseconds =
            |input: &Entity<InputState>, minimum: f64| -> Result<u32, SharedString> {
                let seconds = input
                    .read(cx)
                    .value()
                    .parse::<f64>()
                    .map_err(|_| t!("snippets.group.invalid_time"))?;
                if !seconds.is_finite() || !(minimum..=600.).contains(&seconds) {
                    return Err(t!("snippets.group.invalid_time"));
                }
                Ok((seconds * 1000.).round() as u32)
            };
        self.steps
            .iter()
            .map(|step| {
                let wait = match step
                    .wait
                    .read(cx)
                    .selected_index(cx)
                    .map_or(0, |index| index.row)
                {
                    1 => SnippetWait::Delay {
                        milliseconds: milliseconds(&step.seconds, 0.)?,
                    },
                    2 => SnippetWait::Prompt {
                        text: step.prompt.read(cx).value().trim().to_owned(),
                        timeout_ms: milliseconds(&step.timeout, 0.1)?,
                    },
                    _ => SnippetWait::Immediate,
                };
                Ok(SnippetStep::new(
                    step.content.read(cx).value().to_string(),
                    wait,
                    step.enter,
                ))
            })
            .collect()
    }

    /// Validate and write to the store. Returns whether the dialog may close.
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let category = self
            .category
            .read(cx)
            .selected_index(cx)
            .and_then(|index| self.category_ids.get(index.row).copied())
            .unwrap_or(None);
        let result = if self.is_group(cx) {
            self.group_steps(cx).and_then(|steps| {
                SnippetDraft::validated(&self.name.read(cx).value(), "group", category)
                    .and_then(|draft| draft.with_steps(steps))
                    .map_err(|error| error.message())
            })
        } else {
            SnippetDraft::validated(
                &self.name.read(cx).value(),
                &self.command.read(cx).value(),
                category,
            )
            .map_err(|error| error.message())
        };
        let draft = match result {
            Ok(draft) => SnippetDraft {
                scope: self.scope,
                ..draft.with_run_on_click(self.is_group(cx) || self.run_on_click)
            },
            Err(error) => {
                window.push_notification(form_error_notification(error), cx);
                return false;
            }
        };
        if let Err(error) = self.shortcut.read(cx).validate(cx) {
            window.push_notification(form_error_notification(error), cx);
            return false;
        }
        let shortcut = self.shortcut.read(cx).shortcut();
        let editing = self.editing;
        let public_id = self.store.update(cx, |store, cx| {
            let id = match editing {
                Some(id) => {
                    store.update_snippet(id, draft, cx);
                    id
                }
                None => store.insert_snippet(draft, cx),
            };
            store.snippet(id).map(|snippet| snippet.sync_id.to_string())
        });
        if let Some(id) = public_id {
            self.settings.update(cx, |store, cx| {
                store.update(|settings| settings.snippet_shortcuts.set(id, shortcut), cx)
            });
        }
        true
    }

    fn render_steps(&self, cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .children(self.steps.iter().enumerate().map(|(index, step)| {
                let id = step.id;
                let dispatch = self.dispatch.clone();
                let mode = step
                    .wait
                    .read(cx)
                    .selected_index(cx)
                    .map_or(0, |index| index.row);
                let control = |name: &'static str,
                               icon: IconName,
                               label: SharedString,
                               action: Box<dyn Action>,
                               disabled| {
                    let dispatch = dispatch.clone();
                    Button::new((name, id))
                        .ghost()
                        .small()
                        .icon(icon)
                        .tooltip(label.clone())
                        .accessibility_label(label)
                        .disabled(disabled)
                        .on_click(move |_, window, cx| {
                            dispatch.dispatch_action(&*action, window, cx)
                        })
                };
                v_flex()
                    .id(("snippet-step", id))
                    .gap_2()
                    .pb_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                div()
                                    .flex_1()
                                    .text_sm()
                                    .child(t!("snippets.group.step", number = index + 1)),
                            )
                            .child(control(
                                "snippet-step-up",
                                IconName::ChevronUp,
                                t!("snippets.group.up"),
                                Box::new(crate::app::MoveSnippetStep((id, false))),
                                index == 0,
                            ))
                            .child(control(
                                "snippet-step-down",
                                IconName::ChevronDown,
                                t!("snippets.group.down"),
                                Box::new(crate::app::MoveSnippetStep((id, true))),
                                index + 1 == self.steps.len(),
                            ))
                            .child(control(
                                "snippet-step-remove",
                                IconName::Close,
                                t!("snippets.group.remove"),
                                Box::new(crate::app::RemoveSnippetStep(id)),
                                false,
                            )),
                    )
                    .child(
                        Field::new().label(t!("snippets.group.content")).child(
                            div()
                                .id(("snippet-step-content", id))
                                .test_support()
                                .w_full()
                                .child(
                                    Textarea::new(&step.content)
                                        .text_sm()
                                        .font_family(cx.theme().mono_font_family.clone()),
                                ),
                        ),
                    )
                    .child(
                        Field::new().label(t!("snippets.group.condition")).child(
                            Select::new(&step.wait)
                                .id(("snippet-step-wait", id))
                                .small(),
                        ),
                    )
                    .when(mode == 1, |row| {
                        row.child(
                            Field::new().label(t!("snippets.group.seconds")).child(
                                Input::new(&step.seconds)
                                    .id(("snippet-step-seconds", id))
                                    .small(),
                            ),
                        )
                    })
                    .when(mode == 2, |row| {
                        row.child(
                            Field::new().label(t!("snippets.group.prompt_text")).child(
                                Input::new(&step.prompt)
                                    .id(("snippet-step-prompt", id))
                                    .small(),
                            ),
                        )
                        .child(
                            Field::new().label(t!("snippets.group.timeout")).child(
                                Input::new(&step.timeout)
                                    .id(("snippet-step-timeout", id))
                                    .small(),
                            ),
                        )
                    })
                    .child(
                        Checkbox::new(("snippet-step-enter", id))
                            .small()
                            .checked(step.enter)
                            .label(t!("snippets.group.enter"))
                            .on_click(cx.listener(move |form, value: &bool, _, cx| {
                                if let Some(step) = form.steps.iter_mut().find(|step| step.id == id)
                                {
                                    step.enter = *value;
                                    cx.notify();
                                }
                            })),
                    )
            }))
            .child(
                Button::new("snippet-add-step")
                    .small()
                    .outline()
                    .label(t!("snippets.group.add"))
                    .disabled(self.steps.len() >= 128)
                    .on_click({
                        let dispatch = self.dispatch.clone();
                        move |_, window, cx| {
                            dispatch.dispatch_action(&crate::app::AddSnippetStep, window, cx)
                        }
                    }),
            )
    }
}

impl Render for SnippetForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Form::new()
            .child(
                Field::new()
                    .label(t!("snippets.dialog.name"))
                    .required(true)
                    .child(Input::new(&self.name).id("snippet-name").small()),
            )
            .child(
                Field::new().label(t!("snippets.dialog.category")).child(
                    div()
                        .id("snippet-category")
                        .test_support()
                        .w_full()
                        .child(Select::new(&self.category).small()),
                ),
            )
            .child(
                Field::new()
                    .label(t!("snippets.group.type"))
                    .child(Select::new(&self.kind).id("snippet-type").small()),
            )
            .when(self.is_group(cx), |form| {
                form.child(
                    Field::new()
                        .description(t!("snippets.group.description"))
                        .child(self.render_steps(cx)),
                )
            })
            .when(!self.is_group(cx), |form| {
                form.child(
                    Field::new()
                        .label(t!("snippets.dialog.command"))
                        .required(true)
                        .description(t!("snippets.dialog.command_description"))
                        .child(
                            div().id("snippet-command").test_support().w_full().child(
                                Textarea::new(&self.command)
                                    .text_sm()
                                    .font_family(cx.theme().mono_font_family.clone()),
                            ),
                        ),
                )
            })
            .when(true, |form| {
                form.child(
                    Field::new()
                        .label(t!("snippets.dialog.shortcut"))
                        .child(self.shortcut.clone()),
                )
            })
            .when(true, |form| {
                form.child(
                    Field::new().child(
                        Checkbox::new("snippet-run-on-click")
                            .label(t!("snippets.dialog.run_on_click"))
                            .checked(self.run_on_click)
                            .small()
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.run_on_click = *checked;
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(t!("snippets.dialog.run_on_click_description")),
                            ),
                    ),
                )
            })
            .when(self.is_group(cx), |form| {
                form.child(
                    Field::new().child(
                        div()
                            .id("snippet-group-run-hint")
                            .test_support()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("snippets.group.run_options")),
                    ),
                )
            })
    }
}

/// Open the new or the edit snippet dialog.
pub fn open_snippet_dialog(
    dialog: SnippetDialog,
    store: Entity<HostStore>,
    settings: Entity<SettingsStore>,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) -> Entity<SnippetForm> {
    let form = cx.new(|cx| SnippetForm::new(dialog, store, settings, dispatch, window, cx));
    let editing = form.read(cx).editing.is_some();
    let (title, commit): (SharedString, SharedString) = if editing {
        (t!("snippets.dialog.edit_title"), t!("common.save"))
    } else {
        (
            t!("snippets.dialog.new_title"),
            t!("snippets.dialog.create"),
        )
    };
    window.open_dialog(cx, {
        let form = form.clone();
        move |dialog, window, _| {
            dialog
                .title(title.clone())
                // Dialog geometry is an API boundary that takes `Pixels`; the
                // width follows the interface zoom through the rem.
                .w(rems(DIALOG_WIDTH).to_pixels(window.rem_size()))
                .margin_top(window.viewport_size().height * 0.05)
                .max_h(window.viewport_size().height * 0.9)
                // Closed by its buttons or Escape, not by a click beside it.
                .overlay_closable(false)
                .child(form.clone())
                .footer(commit_footer("commit", commit.clone()))
                .on_ok({
                    let form = form.clone();
                    move |_, window, cx| form.update(cx, |form, cx| form.commit(window, cx))
                })
                .on_close(|_, window, cx| dismiss_form_error(window, cx))
        }
    });
    // Straight to typing: in the same update as the opening, or the
    // dialog's own focus wins.
    let name = form.read(cx).name.clone();
    name.update(cx, |name, cx| name.focus(window, cx));
    form
}

/// Which category dialog to open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CategoryDialog {
    New,
    Rename(SnippetCategoryId),
}

/// The body of the new and rename category dialog.
pub struct CategoryForm {
    store: Entity<HostStore>,
    editing: Option<SnippetCategoryId>,
    name: Entity<InputState>,
}

impl CategoryForm {
    fn new(
        dialog: CategoryDialog,
        store: Entity<HostStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (editing, current) = match dialog {
            CategoryDialog::New => (None, SharedString::default()),
            CategoryDialog::Rename(id) => match store.read(cx).snippet_category(id) {
                Some(category) => (Some(id), category.name.clone()),
                None => (None, SharedString::default()),
            },
        };
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("snippets.category.name_placeholder"))
                .default_value(current)
        });
        Self {
            store,
            editing,
            name,
        }
    }

    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let name = self.name.read(cx).value().trim().to_string();
        let editing = self.editing;
        let taken = self
            .store
            .read(cx)
            .snippet_categories()
            .iter()
            .any(|category| category.name == name.as_str() && Some(category.id) != editing);
        let error = if name.is_empty() {
            Some(t!("snippets.category.name_missing"))
        } else if taken {
            Some(t!("snippets.category.name_taken"))
        } else {
            None
        };
        if let Some(error) = error {
            window.push_notification(form_error_notification(error), cx);
            return false;
        }
        self.store.update(cx, |store, cx| match editing {
            Some(id) => {
                store.rename_snippet_category(id, name, cx);
            }
            None => {
                store.insert_snippet_category(name, cx);
            }
        });
        true
    }
}

impl Render for CategoryForm {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Form::new().child(
            Field::new()
                .label(t!("snippets.category.name"))
                .required(true)
                .child(Input::new(&self.name).id("snippet-category-name").small()),
        )
    }
}

/// Open the new or the rename category dialog.
pub fn open_category_dialog(
    dialog: CategoryDialog,
    store: Entity<HostStore>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| CategoryForm::new(dialog, store, window, cx));
    let renaming = form.read(cx).editing.is_some();
    let (title, commit): (SharedString, SharedString) = if renaming {
        (t!("snippets.category.rename_title"), t!("common.save"))
    } else {
        (
            t!("snippets.category.new_title"),
            t!("snippets.dialog.create"),
        )
    };
    window.open_dialog(cx, {
        let form = form.clone();
        move |dialog, _, _| {
            dialog
                .title(title.clone())
                // Closed by its buttons or Escape, not by a click beside it.
                .overlay_closable(false)
                .child(form.clone())
                .footer(commit_footer("commit", commit.clone()))
                .on_ok({
                    let form = form.clone();
                    move |_, window, cx| form.update(cx, |form, cx| form.commit(window, cx))
                })
                .on_close(|_, window, cx| dismiss_form_error(window, cx))
        }
    });
    let name = form.read(cx).name.clone();
    name.update(cx, |name, cx| name.focus(window, cx));
}
