//! 设置 › 终端 › 本地终端 › Shell: a text field for the program and its
//! arguments across the row, with a menu of the shells installed here at
//! its end and the system's file picker beside it. Every change goes to the settings at
//! once; a local terminal takes it when it opens or restarts.

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    setting::RenderOptions,
};
use gpui_kit::*;

use crate::app::CatalogIcon;
use crate::i18n::{UiLocale, t};
use crate::terminal::{default_shell_name, installed_shells, quote_word};

use super::SettingsStore;

/// The field's text, which keeps the caret while the settings change under
/// it, and what the menu offers.
pub struct LocalShellEditor {
    store: Entity<SettingsStore>,
    input: Entity<InputState>,
    cmder_input: Entity<InputState>,
    cmder: Entity<crate::terminal::Cmder>,
    /// What 自动检测 runs here, and the shells installed; `None` until the
    /// look in the background comes back.
    found: Option<FoundShells>,
    _subscriptions: Vec<Subscription>,
    _scan: Task<()>,
}

struct FoundShells {
    default: String,
    installed: Vec<String>,
}

impl LocalShellEditor {
    pub fn new(
        store: Entity<SettingsStore>,
        cmder: Entity<crate::terminal::Cmder>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let shell = stored(&store, cx).unwrap_or_default();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("settings.local_terminal.automatic_unknown"))
                .default_value(shell)
        });
        let cmder_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("settings.local_terminal.automatic_unknown"))
                .default_value(
                    store
                        .read(cx)
                        .settings()
                        .local_terminal
                        .cmder_directory
                        .map(|path| path.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                )
        });
        let subscriptions = vec![
            cx.observe(&cmder, |_, _, cx| cx.notify()),
            cx.subscribe(&cmder_input, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().trim().to_owned();
                    let path = (!value.is_empty()).then(|| std::path::PathBuf::from(value));
                    this.store.update(cx, |store, cx| {
                        store.update(
                            |settings| settings.local_terminal.cmder_directory = path,
                            cx,
                        )
                    });
                }
            }),
            cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let shell = input.read(cx).value().to_string();
                    this.write(Some(shell), cx);
                }
            }),
            // The field's own edits come back the same, but for the blanks
            // around them; anything else (重置, the settings file read
            // anew) shows in it.
            cx.observe_in(&store, window, |this, store, window, cx| {
                let cmder = store
                    .read(cx)
                    .settings()
                    .local_terminal
                    .cmder_directory
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if this.cmder_input.read(cx).value().trim() != cmder {
                    this.cmder_input
                        .update(cx, |input, cx| input.set_value(cmder, window, cx));
                }
                let shell = stored(&store, cx).unwrap_or_default();
                if this.input.read(cx).value().trim() != shell {
                    this.input
                        .update(cx, |input, cx| input.set_value(shell, window, cx));
                }
            }),
            cx.observe_global_in::<UiLocale>(window, |this, window, cx| {
                this.show_placeholder(window, cx)
            }),
        ];
        let scan = cx.spawn_in(window, async move |this, cx| {
            let found = cx
                .background_spawn(async {
                    FoundShells {
                        default: default_shell_name(),
                        installed: installed_shells(),
                    }
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.found = Some(found);
                this.show_placeholder(window, cx);
                cx.notify();
            })
            .ok();
        });
        Self {
            store,
            input,
            cmder_input,
            cmder,
            found: None,
            _subscriptions: subscriptions,
            _scan: scan,
        }
    }

    /// 「自动检测（/bin/zsh）」, once the default shell is known.
    fn automatic_label(&self) -> SharedString {
        match &self.found {
            Some(found) => t!(
                "settings.local_terminal.automatic",
                shell = found.default.as_str()
            ),
            None => t!("settings.local_terminal.automatic_unknown"),
        }
    }

    /// The empty field says what it runs.
    fn show_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = self.automatic_label();
        self.input.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx)
        });
    }

    /// Run `shell` from now on, `None` for the default: from the menu, the
    /// file picker or 重置.
    pub(super) fn choose(
        &mut self,
        shell: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.input.update(cx, |input, cx| {
            input.set_value(shell.clone().unwrap_or_default(), window, cx)
        });
        self.write(shell, cx);
    }

    /// Ask for the program with the system's picker.
    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(t!("settings.local_terminal.browse_prompt")),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                this.choose(Some(quote_word(&path.to_string_lossy())), window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn write(&self, shell: Option<String>, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| {
            store.update(|settings| settings.local_terminal.shell = shell, cx)
        });
    }
}

/// The shell the settings hold, `None` for the default.
fn stored(store: &Entity<SettingsStore>, cx: &App) -> Option<String> {
    store.read(cx).settings().local_terminal.shell
}

pub(super) fn cmder_field(
    editor: &Entity<LocalShellEditor>,
    options: &RenderOptions,
    cx: &App,
) -> Div {
    let this = editor.read(cx);
    let (choose, automatic) = (editor.downgrade(), editor.downgrade());
    let detected = this
        .cmder
        .read(cx)
        .directory()
        .map(|path| path.to_string_lossy().into_owned());
    let status = detected.map_or_else(
        || t!("settings.local_terminal.cmder_missing"),
        |path| t!("settings.local_terminal.cmder_found", path = path),
    );
    gpui_kit::component::v_flex()
        .gap_2()
        .w_full()
        .child(
            h_flex()
                .gap_2()
                .child(
                    Input::new(&this.cmder_input)
                        .id("cmder-directory")
                        .aria_label(t!("settings.local_terminal.cmder"))
                        .with_size(options.size())
                        .flex_1()
                        .min_w_0()
                        .disabled(!cfg!(windows)),
                )
                .child(
                    Button::new("browse-cmder")
                        .outline()
                        .with_size(options.size())
                        .label(t!("settings.local_terminal.cmder_browse"))
                        .disabled(!cfg!(windows))
                        .on_click(move |_, _, cx| {
                            let receiver = cx.prompt_for_paths(PathPromptOptions {
                                files: false,
                                directories: true,
                                multiple: false,
                                prompt: Some(t!("settings.local_terminal.cmder_browse")),
                            });
                            let editor = choose.clone();
                            cx.spawn(async move |cx| {
                                let Ok(Ok(Some(paths))) = receiver.await else {
                                    return;
                                };
                                let Some(path) = paths.into_iter().next() else {
                                    return;
                                };
                                editor
                                    .update(cx, |editor, cx| {
                                        editor.store.update(cx, |store, cx| {
                                            store.update(
                                                |settings| {
                                                    settings.local_terminal.cmder_directory =
                                                        Some(path)
                                                },
                                                cx,
                                            )
                                        })
                                    })
                                    .ok();
                            })
                            .detach();
                        }),
                )
                .child(
                    Button::new("cmder-automatic")
                        .ghost()
                        .with_size(options.size())
                        .label(t!("settings.local_terminal.automatic_unknown"))
                        .disabled(
                            !cfg!(windows)
                                || this
                                    .store
                                    .read(cx)
                                    .settings()
                                    .local_terminal
                                    .cmder_directory
                                    .is_none(),
                        )
                        .on_click(move |_, _, cx| {
                            automatic
                                .update(cx, |editor, cx| {
                                    editor.store.update(cx, |store, cx| {
                                        store.update(
                                            |settings| {
                                                settings.local_terminal.cmder_directory = None
                                            },
                                            cx,
                                        )
                                    })
                                })
                                .ok();
                        }),
                ),
        )
        .child(
            div()
                .id("cmder-detection")
                .test_support()
                .aria_label(status.clone())
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(status),
        )
}

/// The row's control, under its title: the field across the row, with the
/// installed shells in a menu at its end, and 选择文件… beside it. The menu
/// checks the shell in use.
pub(super) fn local_shell_field(
    editor: &Entity<LocalShellEditor>,
    options: &RenderOptions,
    cx: &App,
) -> Div {
    let this = editor.read(cx);
    let current = stored(&this.store, cx);
    let automatic = this.automatic_label();
    let installed = this
        .found
        .as_ref()
        .map(|found| found.installed.clone())
        .unwrap_or_default();
    let (menu_editor, browse_editor) = (editor.downgrade(), editor.downgrade());
    let menu = Button::new("local-shell-menu")
        .ghost()
        .xsmall()
        .icon(IconName::ChevronDown)
        .tooltip(t!("settings.local_terminal.installed"))
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
            let item = |label: SharedString, shell: Option<String>| {
                let editor = menu_editor.clone();
                PopupMenuItem::new(label)
                    .checked(current == shell)
                    .on_click(move |_, window, cx| {
                        editor
                            .update(cx, |editor, cx| editor.choose(shell.clone(), window, cx))
                            .ok();
                    })
            };
            let mut menu = menu.item(item(automatic.clone(), None));
            if !installed.is_empty() {
                menu = menu.separator();
            }
            installed.iter().fold(menu, |menu, shell| {
                menu.item(item(shell.clone().into(), Some(shell.clone())))
            })
        });
    h_flex()
        .w_full()
        .gap_2()
        .child(
            Input::new(&this.input)
                .id("local-shell")
                .aria_label(t!("settings.local_terminal.shell"))
                .with_size(options.size())
                .font_family(cx.theme().mono_font_family.clone())
                .suffix(menu)
                .flex_1()
                .min_w_0(),
        )
        .child(
            Button::new("browse-local-shell")
                .outline()
                .with_size(options.size())
                .icon(Icon::new(CatalogIcon::FileSearch))
                .label(t!("settings.local_terminal.browse"))
                .on_click(move |_, window, cx| {
                    browse_editor
                        .update(cx, |editor, cx| editor.browse(window, cx))
                        .ok();
                }),
        )
}
