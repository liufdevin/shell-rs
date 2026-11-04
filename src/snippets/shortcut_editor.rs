use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex, v_flex,
};
use gpui_kit::*;

use crate::app::{Refusal, SnippetShortcut, check_snippet, key_text};
use crate::i18n::t;
use crate::settings::SettingsStore;

pub(super) struct SnippetShortcutEditor {
    settings: Entity<SettingsStore>,
    editing: Option<String>,
    keys: Option<String>,
    run: bool,
    recording: bool,
    focus_handle: FocusHandle,
    previous_focus: Option<FocusHandle>,
    intercept: Option<Subscription>,
    _blur: Subscription,
}

impl SnippetShortcutEditor {
    pub(super) fn new(
        settings: Entity<SettingsStore>,
        editing: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let current = editing.as_deref().and_then(|id| {
            settings
                .read(cx)
                .settings()
                .snippet_shortcuts
                .get(id)
                .cloned()
        });
        let focus_handle = cx.focus_handle();
        let blur = cx.on_blur(&focus_handle, window, |this, _, cx| this.stop(cx));
        Self {
            settings,
            editing,
            keys: current.as_ref().map(|shortcut| shortcut.keys().to_string()),
            run: current.is_some_and(|shortcut| shortcut.runs()),
            recording: false,
            focus_handle,
            previous_focus: None,
            intercept: None,
            _blur: blur,
        }
    }

    pub(super) fn shortcut(&self) -> Option<SnippetShortcut> {
        self.keys
            .clone()
            .map(|keys| SnippetShortcut::new(keys, self.run))
    }

    pub(super) fn validate(&self, cx: &App) -> Result<(), String> {
        let Some(keys) = &self.keys else {
            return Ok(());
        };
        let settings = self.settings.read(cx).settings();
        let keystroke = Keystroke::parse(keys).map_err(|error| error.to_string())?;
        check_snippet(
            &keystroke,
            self.editing.as_deref(),
            &settings.shortcuts,
            &settings.snippet_shortcuts,
        )
        .map(|_| ())
        .map_err(|refusal| refusal.message(&key_text(keys, false)))
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.previous_focus = window.focused(cx);
        self.recording = true;
        window.focus(&self.focus_handle, cx);
        let editor = cx.weak_entity();
        let focus = self.focus_handle.clone();
        self.intercept = Some(cx.intercept_keystrokes(move |event, window, cx| {
            if focus.is_focused(window) {
                cx.stop_propagation();
                let keystroke = event.keystroke.clone();
                _ = editor.update(cx, |editor, cx| editor.record(&keystroke, window, cx));
            }
        }));
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        self.recording = false;
        self.intercept = None;
        cx.notify();
    }

    fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop(cx);
        if let Some(previous) = self.previous_focus.take() {
            window.focus(&previous, cx);
        }
    }

    fn record(&mut self, key: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(
            key.key.as_str(),
            "shift" | "control" | "alt" | "platform" | "function"
        ) {
            return;
        }
        if key.key == "escape" && !key.modifiers.modified() {
            self.finish(window, cx);
            return;
        }
        let settings = self.settings.read(cx).settings();
        let checked = check_snippet(
            key,
            self.editing.as_deref(),
            &settings.shortcuts,
            &settings.snippet_shortcuts,
        );
        match checked {
            Ok(keys) => {
                self.keys = Some(keys);
                self.finish(window, cx);
            }
            Err(refusal) => self.show_refusal(refusal, key, window, cx),
        }
    }

    fn show_refusal(
        &self,
        refusal: Refusal,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.push_notification(
            crate::shared::form_error_notification(
                refusal.message(&key_text(&key.unparse(), false)),
            ),
            cx,
        );
    }
}

impl Render for SnippetShortcutEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label: SharedString = self
            .keys
            .as_deref()
            .map(|keys| key_text(keys, false).into())
            .unwrap_or_else(|| t!("settings.shortcuts.none"));
        v_flex()
            .id("snippet-shortcut-editor")
            .track_focus(&self.focus_handle)
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("snippet-shortcut")
                            .outline()
                            .small()
                            .label(if self.recording {
                                t!("settings.shortcuts.recording")
                            } else {
                                label
                            })
                            .on_click(cx.listener(|this, _, window, cx| this.start(window, cx))),
                    )
                    .child(
                        Button::new("snippet-shortcut-clear")
                            .ghost()
                            .small()
                            .label(t!("snippets.shortcut.clear"))
                            .disabled(self.keys.is_none() && !self.recording)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.keys = None;
                                this.run = false;
                                this.stop(cx);
                            })),
                    ),
            )
            .child(
                Checkbox::new("snippet-run-on-shortcut")
                    .small()
                    .label(t!("snippets.shortcut.run"))
                    .checked(self.run)
                    .disabled(self.keys.is_none())
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.run = *checked;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("snippets.shortcut.description")),
            )
    }
}
