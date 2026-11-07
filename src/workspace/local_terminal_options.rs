use std::path::PathBuf;

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    dialog::{DialogAction, DialogClose, DialogFooter},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::StartLocalTerminal;
use crate::i18n::t;
use crate::settings::SettingsStore;
use crate::terminal::{LocalTerminalKind, LocalTerminalStart};

pub(super) fn open_local_terminal_options(
    kind: LocalTerminalKind,
    settings: Entity<SettingsStore>,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let directory = settings
        .read(cx)
        .settings()
        .local_terminal
        .start_directory
        .clone()
        .or_else(dirs::home_dir)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    let form = cx.new(|cx| TerminalOptionsForm {
        kind,
        directory: cx.new(|cx| InputState::new(window, cx).default_value(directory)),
        administrator: false,
        inherited_elevation: super::local_terminal_window::is_administrator(),
        subscription: None,
        validated: None,
        validating: true,
        validation: None,
    });
    form.update(cx, |form, cx| form.subscribe(window, cx));
    let footer = cx.new(|cx| {
        let subscription = cx.observe(&form, |_, _, cx| cx.notify());
        TerminalOptionsFooter {
            form: form.clone(),
            _subscription: subscription,
        }
    });
    window.open_dialog(cx, move |dialog, window, _cx| {
        let view = form.clone();
        let target = dispatch.clone();
        dialog
            .title(match kind {
                LocalTerminalKind::Shell => t!("workspace.local_terminal.options.local"),
                LocalTerminalKind::Cmder => t!("workspace.local_terminal.options.cmder"),
            })
            .w(rems(30.).to_pixels(window.rem_size()))
            .child(form.clone())
            .footer(footer.clone())
            .on_ok(move |_, window, cx| {
                let start = match view.read(cx).start(cx) {
                    Ok(start) => start,
                    Err(_) => return false,
                };
                let target = target.clone();
                window.defer(cx, move |window, cx| {
                    target.dispatch_action(&StartLocalTerminal(start), window, cx);
                });
                true
            })
    });
}

struct TerminalOptionsFooter {
    form: Entity<TerminalOptionsForm>,
    _subscription: Subscription,
}

impl Render for TerminalOptionsFooter {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        DialogFooter::new()
            .child(DialogClose::new().trigger(|button| button.label(t!("common.cancel"))))
            .child(
                DialogAction::new().child(
                    Button::new("start-local-terminal")
                        .label(t!("workspace.local_terminal.options.start"))
                        .primary()
                        .disabled(self.form.read(cx).start(cx).is_err()),
                ),
            )
    }
}

struct TerminalOptionsForm {
    kind: LocalTerminalKind,
    directory: Entity<InputState>,
    administrator: bool,
    inherited_elevation: bool,
    subscription: Option<Subscription>,
    validated: Option<PathBuf>,
    validating: bool,
    validation: Option<Task<()>>,
}

impl TerminalOptionsForm {
    fn subscribe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.subscription = Some(cx.subscribe_in(
            &self.directory,
            window,
            |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.validate(cx);
                }
            },
        ));
        self.validate(cx);
    }

    fn validate(&mut self, cx: &mut Context<Self>) {
        let directory = self.directory(cx);
        self.validated = None;
        self.validating = true;
        self.validation = Some(cx.spawn(async move |this, cx| {
            let validated = cx
                .background_spawn(async move {
                    (directory.is_absolute() && directory.is_dir()).then_some(directory)
                })
                .await;
            this.update(cx, |this, cx| {
                this.validated = validated;
                this.validating = false;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn directory(&self, cx: &App) -> PathBuf {
        let value = self.directory.read(cx).value().trim().to_owned();
        if value.is_empty() {
            dirs::home_dir().unwrap_or_default()
        } else {
            PathBuf::from(value)
        }
    }

    fn start(&self, cx: &App) -> Result<LocalTerminalStart, ()> {
        let directory = self.directory(cx);
        if self.validated.as_ref() != Some(&directory) {
            return Err(());
        }
        Ok(LocalTerminalStart {
            kind: self.kind,
            directory: Some(directory),
            administrator: self.administrator,
        })
    }

    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(t!("workspace.local_terminal.options.browse")),
        });
        let form = cx.entity().downgrade();
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            form.update_in(cx, |form, window, cx| {
                form.directory.update(cx, |input, cx| {
                    input.set_value(path.to_string_lossy(), window, cx)
                });
                form.validate(cx);
            })
            .ok();
        })
        .detach();
    }
}

impl Render for TerminalOptionsForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let valid = self.start(cx).is_ok();
        let browse = cx.listener(|this, _, window, cx| this.browse(window, cx));
        v_flex()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .child(t!("workspace.local_terminal.options.directory")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Input::new(&self.directory)
                            .id("local-terminal-directory")
                            .aria_label(t!("workspace.local_terminal.options.directory"))
                            .flex_1()
                            .min_w_0(),
                    )
                    .child(
                        Button::new("browse-local-terminal-directory")
                            .label(t!("workspace.local_terminal.options.browse"))
                            .on_click(browse),
                    ),
            )
            .when_some(
                (!valid && !self.validating)
                    .then(|| t!("workspace.local_terminal.options.invalid_directory")),
                |view, message| {
                    view.child(
                        div()
                            .id("local-terminal-directory-error")
                            .test_support()
                            .text_sm()
                            .text_color(cx.theme().danger)
                            .child(message),
                    )
                },
            )
            .when(self.validating, |view| {
                view.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("workspace.local_terminal.options.checking_directory")),
                )
            })
            .when(cfg!(windows), |view| {
                view.child(
                    Checkbox::new("local-terminal-administrator")
                        .label(t!("workspace.local_terminal.options.administrator"))
                        .checked(self.administrator)
                        .on_change(cx.listener(|this, checked, _, cx| {
                            this.administrator = *checked;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("workspace.local_terminal.options.administrator_note")),
                )
                .when(self.inherited_elevation, |view| {
                    view.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("workspace.local_terminal.options.current_elevation")),
                    )
                })
            })
    }
}
