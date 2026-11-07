use gpui_kit::{
    component::{
        ActiveTheme as _, Disableable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        checkbox::Checkbox,
        dialog::{DialogAction, DialogClose, DialogFooter},
        input::{Input, InputState},
        searchable_list::SearchableListItem,
        select::{Select, SelectState},
        v_flex,
    },
    prelude::FluentBuilder as _,
    *,
};

use super::{DeviceLogCommand, commands::Source};
use crate::{app::DeviceLogAction, i18n::t};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogSource {
    #[default]
    Device,
    System,
    Service,
    File,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogPeriod {
    #[default]
    Recent,
    Minutes10,
    Hour,
    Today,
    PreviousBoot,
}

impl LogPeriod {
    pub(super) fn seconds(self) -> Option<u64> {
        match self {
            Self::Minutes10 => Some(600),
            Self::Hour => Some(3600),
            Self::Today => Some(86400),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogOptions {
    pub(super) source: LogSource,
    pub(super) period: LogPeriod,
    pub(super) object: String,
    pub(super) pid: String,
    pub(super) tag: String,
    pub(super) crashes: bool,
}

impl LogOptions {
    pub fn file(path: impl Into<String>) -> Self {
        Self {
            source: LogSource::File,
            object: path.into(),
            ..Self::default()
        }
    }

    pub fn application(package: impl Into<String>) -> Self {
        Self {
            object: package.into(),
            ..Self::default()
        }
    }

    pub fn with_period(mut self, period: LogPeriod) -> Self {
        self.period = period;
        self
    }

    pub fn service(name: impl Into<String>) -> Self {
        Self {
            source: LogSource::Service,
            object: name.into(),
            ..Self::default()
        }
    }

    pub(super) fn validate(&self, android: bool) -> bool {
        let plain = |value: &str| {
            value.len() <= 255
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-:@".contains(&byte))
        };
        if android {
            (self.object.is_empty()
                || (self.object.len() <= 255
                    && self
                        .object
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"._".contains(&byte))))
                && (self.pid.is_empty() || self.pid.parse::<u32>().is_ok_and(|pid| pid > 0))
                && self.tag.len() <= 255
                && !self.tag.chars().any(char::is_control)
        } else {
            match self.source {
                LogSource::Service => !self.object.is_empty() && plain(&self.object),
                LogSource::File => {
                    self.object.starts_with('/')
                        && self.object.len() <= 4096
                        && !self.object.chars().any(char::is_control)
                }
                _ => true,
            }
        }
    }
}

#[derive(Clone)]
struct Choice<T> {
    value: T,
    label: SharedString,
}

impl<T: Clone + PartialEq + 'static> SearchableListItem for Choice<T> {
    type Value = T;
    fn title(&self) -> SharedString {
        self.label.clone()
    }
    fn value(&self) -> &T {
        &self.value
    }
}

pub(super) fn open_filters(
    options: LogOptions,
    source: Source,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let view = cx.new(|cx| FilterForm::new(options, source, window, cx));
    window.open_dialog(cx, move |dialog, window, cx| {
        let apply = dispatch.clone();
        let form = view.clone();
        let valid = view
            .read(cx)
            .options(cx)
            .validate(source == Source::Android);
        dialog
            .title(t!("device_logs.filter_title"))
            .w(rems(28.).to_pixels(window.rem_size()))
            .child(view.clone())
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|button| button.label(t!("common.cancel"))))
                    .child(
                        DialogAction::new().child(
                            Button::new("log-filter-apply")
                                .label(t!("device_logs.apply"))
                                .primary()
                                .disabled(!valid),
                        ),
                    ),
            )
            .on_ok(move |_, window, cx| {
                let options = form.read(cx).options(cx);
                if !options.validate(source == Source::Android) {
                    return false;
                }
                let apply = apply.clone();
                window.defer(cx, move |window, cx| {
                    apply.dispatch_action(
                        &DeviceLogAction(DeviceLogCommand::ApplyOptions(options)),
                        window,
                        cx,
                    );
                });
                true
            })
    });
}

struct FilterForm {
    android: bool,
    source: Entity<SelectState<Vec<Choice<LogSource>>>>,
    period: Entity<SelectState<Vec<Choice<LogPeriod>>>>,
    object: Entity<InputState>,
    pid: Entity<InputState>,
    tag: Entity<InputState>,
    crashes: bool,
    _subscriptions: Vec<Subscription>,
}

impl FilterForm {
    fn new(
        options: LogOptions,
        source: Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let android = source == Source::Android;
        let choices = [
            (LogSource::Device, t!("device_logs.kind.kernel")),
            (LogSource::System, t!("device_logs.kind.system")),
            (LogSource::Service, t!("device_logs.kind.service")),
            (LogSource::File, t!("device_logs.kind.file")),
        ];
        let sources = cx.new(|cx| {
            let mut state = SelectState::new(
                choices
                    .into_iter()
                    .map(|(value, label)| Choice { value, label })
                    .collect(),
                None,
                window,
                cx,
            );
            state.set_selected_value(&options.source, window, cx);
            state
        });
        let mut periods = vec![
            (LogPeriod::Recent, t!("device_logs.period.recent")),
            (LogPeriod::Minutes10, t!("device_logs.period.minutes10")),
            (LogPeriod::Hour, t!("device_logs.period.hour")),
            (LogPeriod::Today, t!("device_logs.period.day")),
        ];
        if !android {
            periods.push((LogPeriod::PreviousBoot, t!("device_logs.period.previous")));
        }
        let period = cx.new(|cx| {
            let mut state = SelectState::new(
                periods
                    .into_iter()
                    .map(|(value, label)| Choice { value, label })
                    .collect(),
                None,
                window,
                cx,
            );
            state.set_selected_value(&options.period, window, cx);
            state
        });
        let input = |value: &str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).default_value(value))
        };
        let object = input(&options.object, window, cx);
        let pid = input(&options.pid, window, cx);
        let tag = input(&options.tag, window, cx);
        let mut subscriptions = Vec::new();
        for entity in [&object, &pid, &tag] {
            subscriptions.push(cx.observe(entity, |_, _, cx| cx.notify()));
        }
        subscriptions.push(cx.observe(&sources, |_, _, cx| cx.notify()));
        subscriptions.push(cx.observe(&period, |_, _, cx| cx.notify()));
        Self {
            android,
            source: sources,
            period,
            object,
            pid,
            tag,
            crashes: options.crashes,
            _subscriptions: subscriptions,
        }
    }

    fn options(&self, cx: &App) -> LogOptions {
        LogOptions {
            source: self
                .source
                .read(cx)
                .selected_value()
                .copied()
                .unwrap_or_default(),
            period: self
                .period
                .read(cx)
                .selected_value()
                .copied()
                .unwrap_or_default(),
            object: self.object.read(cx).value().trim().to_string(),
            pid: self.pid.read(cx).value().trim().to_string(),
            tag: self.tag.read(cx).value().trim().to_string(),
            crashes: self.crashes,
        }
    }
}

impl Render for FilterForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let options = self.options(cx);
        let journal = matches!(options.source, LogSource::System | LogSource::Service);
        v_flex()
            .gap_3()
            .when(!self.android, |view| {
                view.child(t!("device_logs.origin"))
                    .child(Select::new(&self.source).id("log-filter-source").w_full())
            })
            .when(self.android || journal, |view| {
                view.child(t!("device_logs.period"))
                    .child(Select::new(&self.period).id("log-filter-period").w_full())
            })
            .when(
                self.android || matches!(options.source, LogSource::Service | LogSource::File),
                |view| {
                    view.child(if self.android {
                        t!("device_logs.package")
                    } else if options.source == LogSource::File {
                        t!("device_logs.path")
                    } else {
                        t!("device_logs.service")
                    })
                    .child(
                        Input::new(&self.object).id("log-filter-object").aria_label(
                            if self.android {
                                t!("device_logs.package")
                            } else if options.source == LogSource::File {
                                t!("device_logs.path")
                            } else {
                                t!("device_logs.service")
                            },
                        ),
                    )
                },
            )
            .when(self.android, |view| {
                view.child(t!("device_logs.pid"))
                    .child(
                        Input::new(&self.pid)
                            .id("log-filter-pid")
                            .aria_label(t!("device_logs.pid")),
                    )
                    .child(t!("device_logs.tag"))
                    .child(
                        Input::new(&self.tag)
                            .id("log-filter-tag")
                            .aria_label(t!("device_logs.tag")),
                    )
                    .child(
                        Checkbox::new("log-filter-crashes")
                            .label(t!("device_logs.crashes"))
                            .checked(self.crashes)
                            .on_change(cx.listener(|this, checked, _, cx| {
                                this.crashes = *checked;
                                cx.notify();
                            })),
                    )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("device_logs.filter_note")),
            )
            .when(!options.validate(self.android), |view| {
                view.child(
                    div()
                        .id("log-filter-error")
                        .test_support()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(t!("device_logs.invalid_filter")),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::LogOptions;

    #[test]
    fn filter_validation_rejects_commands_and_accepts_quoted_absolute_paths() {
        assert!(!LogOptions::application("com.example;reboot").validate(true));
        assert!(!LogOptions::service("$(reboot).service").validate(false));
        assert!(!LogOptions::file("relative/log").validate(false));
        assert!(LogOptions::file("/var/log/a file's.log").validate(false));
        assert!(LogOptions::application("com.example.demo").validate(true));
        assert!(
            !LogOptions {
                pid: "0".into(),
                ..Default::default()
            }
            .validate(true)
        );
    }
}
