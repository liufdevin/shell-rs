use std::{
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    component::{
        ActiveTheme as _, IconName, Sizable as _, ThemeMode, WindowExt as _,
        button::{Button, ButtonVariants as _},
        h_flex,
        notification::Notification,
        setting::{SettingField, SettingGroup, SettingItem},
        slider::{Slider, SliderEvent, SliderState},
        v_flex,
    },
    *,
};

use super::{
    DEFAULT_BACKGROUND_OPACITY, SettingsStore,
    settings_panel::keywords,
    terminal_themes::{column_id, sample_lines},
};
use crate::i18n::t;

pub(super) struct BackgroundImagesEditor {
    store: Entity<SettingsStore>,
    light: OpacitySlider,
    dark: OpacitySlider,
    _store_changes: Subscription,
}

struct OpacitySlider {
    state: Entity<SliderState>,
    pending: Option<Task<()>>,
    _subscriptions: [Subscription; 2],
}

impl BackgroundImagesEditor {
    pub fn new(store: Entity<SettingsStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let light = Self::slider(&store, ThemeMode::Light, cx);
        let dark = Self::slider(&store, ThemeMode::Dark, cx);
        let subscription = cx.observe_in(&store, window, |this, store, window, cx| {
            let settings = store.read(cx).settings();
            for mode in [ThemeMode::Light, ThemeMode::Dark] {
                let slider = this.opacity_slider(mode);
                if slider.pending.is_some() {
                    continue;
                }
                let stored = percent(settings.terminal_background.for_mode(mode).opacity);
                let state = slider.state.clone();
                if state.read(cx).value().end() != stored {
                    state.update(cx, |state, cx| state.set_value(stored, window, cx));
                }
            }
        });
        Self {
            store,
            light,
            dark,
            _store_changes: subscription,
        }
    }

    fn slider(
        store: &Entity<SettingsStore>,
        mode: ThemeMode,
        cx: &mut Context<Self>,
    ) -> OpacitySlider {
        let stored = store
            .read(cx)
            .settings()
            .terminal_background
            .for_mode(mode)
            .opacity;
        let state = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(100.)
                .step(1.)
                .default_value(percent(stored))
        });
        let subscriptions = [
            cx.observe(&state, move |this, _, cx| {
                let task = cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(300))
                        .await;
                    _ = this.update(cx, |this, cx| this.write_opacity(mode, cx));
                });
                this.opacity_slider_mut(mode).pending = Some(task);
                cx.notify();
            }),
            cx.subscribe(&state, move |this, _, event: &SliderEvent, cx| {
                if let SliderEvent::Release(_) = event {
                    this.write_opacity(mode, cx);
                }
            }),
        ];
        OpacitySlider {
            state,
            pending: None,
            _subscriptions: subscriptions,
        }
    }

    fn opacity_slider(&self, mode: ThemeMode) -> &OpacitySlider {
        match mode {
            ThemeMode::Light => &self.light,
            ThemeMode::Dark => &self.dark,
        }
    }

    fn opacity_slider_mut(&mut self, mode: ThemeMode) -> &mut OpacitySlider {
        match mode {
            ThemeMode::Light => &mut self.light,
            ThemeMode::Dark => &mut self.dark,
        }
    }

    fn shown_opacity(&self, mode: ThemeMode, cx: &App) -> f32 {
        self.opacity_slider(mode).state.read(cx).value().end() / 100.
    }

    fn write_opacity(&mut self, mode: ThemeMode, cx: &mut Context<Self>) {
        self.opacity_slider_mut(mode).pending = None;
        let opacity = self.shown_opacity(mode, cx);
        self.store.update(cx, |store, cx| {
            store.update(
                |settings| settings.terminal_background.for_mode_mut(mode).opacity = opacity,
                cx,
            )
        });
    }

    fn reset_opacity(&mut self, mode: ThemeMode, window: &mut Window, cx: &mut Context<Self>) {
        self.opacity_slider(mode)
            .state
            .clone()
            .update(cx, |state, cx| {
                state.set_value(percent(DEFAULT_BACKGROUND_OPACITY), window, cx)
            });
        self.write_opacity(mode, cx);
    }

    fn choose(&mut self, mode: ThemeMode, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(t!("settings.background.prompt")),
        });
        let dir = self.store.read(cx).backgrounds_dir();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(source) = paths.into_iter().next() else {
                return;
            };
            let copied = cx
                .background_spawn(async move { import(&source, dir.as_deref()) })
                .await;
            _ = this.update_in(cx, |this, window, cx| match copied {
                Ok(image) => this.show(mode, Some(image), cx),
                Err(error) => window.push_notification(
                    Notification::error(t!(
                        "settings.background.import_failed",
                        error = error.to_string()
                    )),
                    cx,
                ),
            });
        })
        .detach();
    }

    fn show(&mut self, mode: ThemeMode, image: Option<PathBuf>, cx: &mut Context<Self>) {
        let store = self.store.read(cx);
        let (before, dir) = (
            store.settings().terminal_background,
            store.backgrounds_dir(),
        );
        self.store.update(cx, |store, cx| {
            store.update(
                |settings| {
                    let background = settings.terminal_background.for_mode_mut(mode);
                    background.image = image.clone();
                    background.enabled = image.is_some();
                },
                cx,
            )
        });
        let other = match mode {
            ThemeMode::Light => &before.dark,
            ThemeMode::Dark => &before.light,
        };
        if let Some(replaced) = before.for_mode(mode).image.clone().filter(|replaced| {
            dir.as_deref().is_some_and(|dir| replaced.starts_with(dir))
                && Some(replaced) != image.as_ref()
                && Some(replaced) != other.image.as_ref()
        }) {
            cx.background_spawn(async move {
                _ = fs::remove_file(replaced);
            })
            .detach();
        }
    }
}

fn percent(opacity: f32) -> f32 {
    (opacity * 100.).round()
}

fn import(source: &Path, dir: Option<&Path>) -> io::Result<PathBuf> {
    let Some(dir) = dir else {
        return Ok(source.to_path_buf());
    };
    let name = source
        .file_name()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    fs::create_dir_all(dir)?;
    let target = dir.join(format!(
        "{}-{}",
        uuid::Uuid::new_v4().simple(),
        name.to_string_lossy()
    ));
    fs::copy(source, &target)?;
    Ok(target)
}

fn chosen_name(image: &Path) -> String {
    let name = image
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.split_once('-') {
        Some((id, original))
            if id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            original.into()
        }
        _ => name,
    }
}

pub(super) fn background_group(
    editor: &Entity<BackgroundImagesEditor>,
    store: &Entity<SettingsStore>,
    mode: ThemeMode,
) -> SettingGroup {
    let (title, description) = match mode {
        ThemeMode::Light => (
            t!("settings.background.light"),
            t!("settings.background.light_description"),
        ),
        ThemeMode::Dark => (
            t!("settings.background.dark"),
            t!("settings.background.dark_description"),
        ),
    };
    let (reader, writer) = (store.clone(), store.clone());
    let (image_editor, image_store, image_reset) = (editor.clone(), store.clone(), editor.clone());
    let (opacity_editor, opacity_store, opacity_reset) =
        (editor.clone(), store.clone(), editor.clone());
    let (preview_editor, preview_store) = (editor.clone(), store.clone());
    let words = keywords(t!("settings.background.keywords"));
    SettingGroup::new()
        .title(title)
        .description(description)
        .items([
            SettingItem::new(
                t!("settings.background.enabled"),
                SettingField::switch(
                    move |cx| {
                        reader
                            .read(cx)
                            .settings()
                            .terminal_background
                            .for_mode(mode)
                            .enabled
                    },
                    move |on, cx| {
                        writer.update(cx, |store, cx| {
                            store.update(
                                |settings| {
                                    settings.terminal_background.for_mode_mut(mode).enabled = on
                                },
                                cx,
                            )
                        })
                    },
                )
                .default_value(false),
            )
            .keywords(words.clone()),
            SettingItem::new(
                t!("settings.background.image"),
                SettingField::render(move |_, window, cx| {
                    image_field(&image_editor, mode, window, cx)
                })
                .on_reset(
                    move |cx| {
                        image_store
                            .read(cx)
                            .settings()
                            .terminal_background
                            .for_mode(mode)
                            .image
                            .is_some()
                    },
                    move |_, cx| image_reset.update(cx, |editor, cx| editor.show(mode, None, cx)),
                ),
            )
            .keywords(words.clone()),
            SettingItem::new(
                t!("settings.background.opacity"),
                SettingField::render(move |_, _, cx| opacity_field(&opacity_editor, mode, cx))
                    .on_reset(
                        move |cx| {
                            opacity_store
                                .read(cx)
                                .settings()
                                .terminal_background
                                .for_mode(mode)
                                .opacity
                                != DEFAULT_BACKGROUND_OPACITY
                        },
                        move |window, cx| {
                            opacity_reset
                                .update(cx, |editor, cx| editor.reset_opacity(mode, window, cx))
                        },
                    ),
            )
            .description(t!("settings.background.opacity_description"))
            .keywords(words.clone()),
            SettingItem::render(move |_, window, cx| {
                preview(&preview_editor, &preview_store, mode, window, cx)
            })
            .keywords(words),
        ])
}

fn image_field(
    editor: &Entity<BackgroundImagesEditor>,
    mode: ThemeMode,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let image = editor
        .read(cx)
        .store
        .read(cx)
        .settings()
        .terminal_background
        .for_mode(mode)
        .image
        .clone();
    let name: SharedString = image
        .as_deref()
        .map(chosen_name)
        .map(Into::into)
        .unwrap_or_else(|| t!("settings.background.none"));
    h_flex()
        .gap_2()
        .child(
            div()
                .id(column_id("background-image", mode))
                .test_support()
                .aria_label(name.clone())
                .max_w(rems(14.))
                .truncate()
                .text_sm()
                .child(name),
        )
        .child(
            Button::new(column_id("background-choose", mode))
                .outline()
                .small()
                .label(t!("settings.background.choose"))
                .on_click(window.listener_for(editor, move |editor, _, window, cx| {
                    editor.choose(mode, window, cx)
                })),
        )
        .when(image.is_some(), |field| {
            field.child(
                Button::new(column_id("background-clear", mode))
                    .ghost()
                    .small()
                    .icon(IconName::Close)
                    .tooltip(t!("settings.background.clear"))
                    .on_click(
                        window.listener_for(editor, move |editor, _, _, cx| {
                            editor.show(mode, None, cx)
                        }),
                    ),
            )
        })
}

fn opacity_field(editor: &Entity<BackgroundImagesEditor>, mode: ThemeMode, cx: &mut App) -> Div {
    let editor = editor.read(cx);
    let state = editor.opacity_slider(mode).state.clone();
    h_flex()
        .w(rems(14.))
        .gap_3()
        .child(
            div()
                .id(column_id("background-opacity", mode))
                .test_support()
                .flex_1()
                .child(Slider::new(&state).horizontal()),
        )
        .child(
            div()
                .flex_none()
                .w(rems(2.5))
                .text_right()
                .text_sm()
                .child(format!("{}%", percent(editor.shown_opacity(mode, cx)))),
        )
}

fn preview(
    editor: &Entity<BackgroundImagesEditor>,
    store: &Entity<SettingsStore>,
    mode: ThemeMode,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let settings = store.read(cx).settings();
    let theme = settings.terminal_theme.theme(mode);
    let image = settings.terminal_background.for_mode(mode).image.clone();
    let opacity = editor.read(cx).shown_opacity(mode, cx);
    let unreadable = image.as_ref().is_some_and(|image| {
        matches!(
            window.use_asset::<ImgResourceLoader>(&Resource::from(image.clone()), cx),
            Some(Err(_))
        )
    });
    v_flex()
        .w_full()
        .gap_2()
        .child(div().text_sm().child(t!("settings.preview")))
        .child(
            v_flex()
                .id(column_id("background-preview", mode))
                .test_support()
                .relative()
                .w_full()
                .px_3()
                .py_2()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .bg(theme.background())
                .overflow_hidden()
                .when_some(image.filter(|_| !unreadable), |preview, image| {
                    preview
                        .child(
                            img(image)
                                .absolute()
                                .inset_0()
                                .size_full()
                                .object_fit(ObjectFit::Cover),
                        )
                        .child(
                            div()
                                .absolute()
                                .inset_0()
                                .bg(theme.background().opacity(1. - opacity)),
                        )
                })
                .children(sample_lines(theme, cx))
                .when(unreadable, |view| {
                    view.child(
                        div()
                            .text_xs()
                            .text_color(theme.foreground())
                            .child(t!("settings.background.unreadable")),
                    )
                }),
        )
}

#[cfg(test)]
mod tests {
    use super::{chosen_name, import};

    #[test]
    fn background_import_copies_images_without_touching_originals() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("夕阳-sunset.png");
        std::fs::write(&source, b"image bytes").unwrap();
        let backgrounds = directory.path().join("data/backgrounds");
        let first = import(&source, Some(&backgrounds)).unwrap();
        let second = import(&source, Some(&backgrounds)).unwrap();
        assert_ne!(first, second);
        assert_eq!(first.parent(), Some(backgrounds.as_path()));
        assert_eq!(chosen_name(&first), "夕阳-sunset.png");
        assert_eq!(
            std::fs::read(first).unwrap(),
            std::fs::read(&source).unwrap()
        );
        assert_eq!(import(&source, None).unwrap(), source);
        assert!(source.exists());
    }
}
