use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    scroll::ScrollableElement as _,
    tab::{Tab, TabBar},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::SerialLineEnding;
use crate::{
    app::{CatalogIcon, SerialToolAction},
    i18n::t,
    terminal::TerminalView,
};

#[path = "assistant.rs"]
mod assistant;
pub use assistant::SerialAssistantCommand;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SerialToolPage {
    #[default]
    Transfer,
    Reply,
    Connection,
}

impl SerialToolPage {
    const ALL: [Self; 3] = [Self::Transfer, Self::Reply, Self::Connection];
    fn label(self) -> SharedString {
        match self {
            Self::Transfer => t!("serial.tool.tab_transfer"),
            Self::Reply => t!("serial.tool.tab_reply"),
            Self::Connection => t!("serial.tool.tab_connection"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SerialToolCommand {
    Assistant(SerialAssistantCommand),
    Capture(bool),
    Clear,
    Copy,
    UseCapture,
    Send,
    Hex(bool),
    Automatic(bool),
    Baud,
    BaudPreset(u32),
    Page(SerialToolPage),
    Ending(SerialLineEnding),
    Crc(bool),
    Repeat(bool),
}

pub struct SerialToolsPanel {
    assistant: Option<assistant::AssistantEditor>,
    assistant_layout_enabled: bool,
    body_scroll: ScrollHandle,
    preview_scroll: ScrollHandle,
    group_task: Option<Task<()>>,
    drafts: HashMap<EntityId, SerialToolDraft>,
    target: Option<WeakEntity<TerminalView>>,
    observer: Option<Subscription>,
    assistant_observer: Option<Subscription>,
    dispatch: FocusHandle,
    baud: Entity<InputState>,
    message: Entity<TextareaState>,
    pattern: Entity<InputState>,
    reply: Entity<InputState>,
    hex: bool,
    status: SharedString,
    baud_task: Option<Task<()>>,
    generation: Option<u64>,
    page: SerialToolPage,
    ending: SerialLineEnding,
    crc: bool,
    interval: Entity<InputState>,
    repeat_task: Option<Task<()>>,
    focus_handle: FocusHandle,
}

struct SerialToolDraft {
    target: WeakEntity<TerminalView>,
    baud: Entity<InputState>,
    message: Entity<TextareaState>,
    pattern: Entity<InputState>,
    reply: Entity<InputState>,
    hex: bool,
    ending: SerialLineEnding,
    crc: bool,
    interval: Entity<InputState>,
}

impl SerialToolsPanel {
    pub fn new(dispatch: FocusHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            assistant: None,
            assistant_layout_enabled: false,
            body_scroll: ScrollHandle::new(),
            preview_scroll: ScrollHandle::new(),
            group_task: None,
            drafts: HashMap::new(),
            target: None,
            observer: None,
            assistant_observer: None,
            dispatch,
            baud: cx.new(|cx| InputState::new(window, cx)),
            message: cx.new(|cx| TextareaState::new(window, cx).rows(4)),
            pattern: cx
                .new(|cx| InputState::new(window, cx).validate(|text, _| text.len() <= 4096)),
            reply: cx.new(|cx| InputState::new(window, cx).validate(|text, _| text.len() <= 4096)),
            hex: false,
            status: SharedString::default(),
            baud_task: None,
            generation: None,
            page: SerialToolPage::default(),
            ending: SerialLineEnding::default(),
            crc: false,
            interval: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value("1000")
                    .validate(|text, _| text.len() <= 5)
            }),
            repeat_task: None,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn set_target(
        &mut self,
        target: Option<WeakEntity<TerminalView>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.target == target {
            return;
        }
        self.baud_task = None;
        self.repeat_task = None;
        self.group_task = None;
        self.assistant = None;
        self.assistant_layout_enabled = false;
        self.body_scroll.set_offset(Point::default());
        self.preview_scroll.set_offset(Point::default());
        self.status = SharedString::default();
        self.drafts
            .retain(|_, draft| draft.target.upgrade().is_some());
        if let Some(previous) = self
            .target
            .as_ref()
            .filter(|target| target.upgrade().is_some())
        {
            self.drafts.insert(
                previous.entity_id(),
                SerialToolDraft {
                    target: previous.clone(),
                    baud: self.baud.clone(),
                    message: self.message.clone(),
                    pattern: self.pattern.clone(),
                    reply: self.reply.clone(),
                    hex: self.hex,
                    ending: self.ending,
                    crc: self.crc,
                    interval: self.interval.clone(),
                },
            );
        }
        if let Some(target) = &target {
            let draft =
                self.drafts
                    .remove(&target.entity_id())
                    .unwrap_or_else(|| SerialToolDraft {
                        target: target.clone(),
                        baud: cx.new(|cx| {
                            InputState::new(window, cx).validate(|text, _| text.len() <= 10)
                        }),
                        message: cx.new(|cx| TextareaState::new(window, cx).rows(4)),
                        pattern: cx.new(|cx| {
                            InputState::new(window, cx).validate(|text, _| text.len() <= 4096)
                        }),
                        reply: cx.new(|cx| {
                            InputState::new(window, cx).validate(|text, _| text.len() <= 4096)
                        }),
                        hex: false,
                        ending: SerialLineEnding::default(),
                        crc: false,
                        interval: cx.new(|cx| {
                            InputState::new(window, cx)
                                .default_value("1000")
                                .validate(|text, _| text.len() <= 5)
                        }),
                    });
            self.baud = draft.baud;
            self.message = draft.message;
            self.pattern = draft.pattern;
            self.reply = draft.reply;
            self.hex = draft.hex;
            self.ending = draft.ending;
            self.crc = draft.crc;
            self.interval = draft.interval;
        }
        self.observer = target.as_ref().and_then(WeakEntity::upgrade).map(|view| {
            cx.observe(&view, |owner, view, cx| {
                let generation = view.read(cx).generation(cx);
                if owner.generation != Some(generation) {
                    owner.generation = Some(generation);
                    owner.baud_task = None;
                    owner.repeat_task = None;
                    owner.group_task = None;
                    owner.status = SharedString::default();
                }
                if !matches!(
                    view.read(cx).lifecycle(cx),
                    crate::terminal::TerminalLifecycle::Running
                ) {
                    if owner.group_task.take().is_some() {
                        owner.status = t!("serial.assistant.stopped");
                    }
                    if owner.repeat_task.take().is_some() {
                        owner.status = t!("serial.tool.repeat_stopped");
                    }
                }
                cx.notify();
            })
        });
        self.generation = target
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .map(|view| view.read(cx).generation(cx));
        self.target = target;
        self.apply_assistant_layout(self.assistant_enabled(cx), window, cx);
        self.assistant_observer = self
            .target
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .and_then(|view| view.read(cx).serial_assistant_target())
            .map(|(store, key, _)| {
                cx.observe_in(&store, window, move |owner, store, window, cx| {
                    let enabled = store
                        .read(cx)
                        .serial_assistant(&key)
                        .is_some_and(|value| value.enabled);
                    owner.apply_assistant_layout(enabled, window, cx);
                    cx.notify();
                })
            });
        cx.notify();
    }

    pub fn command(
        &mut self,
        command: &SerialToolCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.target.as_ref().and_then(WeakEntity::upgrade) else {
            return;
        };
        self.status = SharedString::default();
        let result: Result<(), SharedString> = (|| {
            match command {
                SerialToolCommand::Assistant(command) => {
                    self.assistant_command(command, window, cx)?
                }
                SerialToolCommand::Capture(enabled) => {
                    view.update(cx, |view, cx| view.serial_tool_capture(*enabled, cx))
                }
                SerialToolCommand::Clear => view.update(cx, |view, cx| view.serial_tool_clear(cx)),
                SerialToolCommand::Copy | SerialToolCommand::UseCapture => {
                    let bytes = view.read(cx).serial_tool_snapshot(cx).bytes;
                    let data = if matches!(command, SerialToolCommand::UseCapture) {
                        &bytes[bytes.len().saturating_sub(1024)..]
                    } else {
                        &bytes
                    };
                    let hex = view.read(cx).serial_modes(cx).receive_hex;
                    let text = self.display_bytes(data, hex, cx);
                    if matches!(command, SerialToolCommand::Copy) {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    } else {
                        self.pattern
                            .update(cx, |input, cx| input.set_value(text, window, cx));
                        self.page = SerialToolPage::Reply;
                        let focus = self.pattern.read(cx).focus_handle(cx);
                        window.focus(&focus, cx);
                        self.hex = hex;
                    }
                }
                SerialToolCommand::Send => {
                    if self.group_task.is_some() {
                        return Err(t!("serial.tool.unavailable"));
                    }
                    let message = self.message.read(cx).value();
                    let hex = view.read(cx).serial_modes(cx).send_hex;
                    let bytes = view.read(cx).serial_tool_message(
                        &message,
                        hex,
                        self.ending,
                        self.crc,
                        cx,
                    )?;
                    view.read(cx).serial_tool_send_bytes(bytes, cx)?;
                    self.status = t!("serial.tool.queued");
                }
                SerialToolCommand::Hex(hex) => {
                    if !view.read(cx).serial_tool_snapshot(cx).automatic {
                        self.hex = *hex;
                    }
                }
                SerialToolCommand::Automatic(enabled) => {
                    if self.group_task.is_some() && *enabled {
                        return Err(t!("serial.tool.unavailable"));
                    }
                    let pattern = self.pattern.read(cx).value();
                    let reply = self.reply.read(cx).value();
                    let rule = enabled.then_some((pattern.as_str(), reply.as_str(), self.hex));
                    view.update(cx, |view, cx| view.serial_tool_rule(rule, cx))?;
                }
                SerialToolCommand::Baud => {
                    if self.baud_task.is_some() {
                        return Ok(());
                    }
                    if self.repeat_task.is_some()
                        || self.group_task.is_some()
                        || view.read(cx).serial_tool_snapshot(cx).automatic
                    {
                        return Err(t!("serial.tool.unavailable"));
                    }
                    let baud = self
                        .baud
                        .read(cx)
                        .value()
                        .trim()
                        .parse::<u32>()
                        .ok()
                        .filter(|baud| *baud > 0)
                        .ok_or_else(|| t!("serial.tool.invalid_baud"))?;
                    let answer = view.read(cx).serial_tool_baud(baud, cx)?;
                    let generation = view.read(cx).generation(cx);
                    let connection = view.downgrade();
                    self.status = t!("serial.tool.applying_baud");
                    self.baud_task = Some(cx.spawn(async move |owner, cx| {
                        let deadline = Instant::now() + Duration::from_secs(12);
                        let result = loop {
                            match answer.try_recv() {
                                Ok(result) => break result,
                                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                    break Err(t!("serial.tool.baud_unsupported").to_string());
                                }
                                Err(_) if Instant::now() >= deadline => {
                                    break Err(t!("serial.tool.baud_timeout").to_string());
                                }
                                Err(_) => {
                                    cx.background_executor()
                                        .timer(Duration::from_millis(50))
                                        .await;
                                }
                            }
                        };
                        let _ = owner.update(cx, |owner, cx| {
                            owner.status = if connection
                                .upgrade()
                                .is_none_or(|view| view.read(cx).generation(cx) != generation)
                            {
                                t!("serial.tool.unavailable")
                            } else {
                                match result {
                                    Ok(()) => t!("serial.tool.baud_applied", baud = baud),
                                    Err(error) => t!("serial.tool.baud_failed", error = error),
                                }
                            };
                            owner.baud_task = None;
                            cx.notify();
                        });
                    }));
                }
                SerialToolCommand::BaudPreset(baud) => {
                    self.baud.update(cx, |input, cx| {
                        input.set_value(baud.to_string(), window, cx)
                    });
                }
                SerialToolCommand::Page(page) => {
                    if self.page != *page {
                        if [
                            self.message.read(cx).focus_handle(cx),
                            self.pattern.read(cx).focus_handle(cx),
                            self.reply.read(cx).focus_handle(cx),
                            self.baud.read(cx).focus_handle(cx),
                            self.interval.read(cx).focus_handle(cx),
                        ]
                        .iter()
                        .any(|focus| focus.is_focused(window))
                        {
                            window.focus(&self.focus_handle, cx);
                        }
                        self.page = *page;
                        self.body_scroll.set_offset(Point::default());
                    }
                }
                SerialToolCommand::Ending(ending) => self.ending = *ending,
                SerialToolCommand::Crc(enabled) => self.crc = *enabled,
                SerialToolCommand::Repeat(false) => {
                    self.repeat_task = None;
                    self.status = t!("serial.tool.repeat_stopped");
                }
                SerialToolCommand::Repeat(true) => {
                    if self.group_task.is_some() {
                        return Err(t!("serial.tool.unavailable"));
                    }
                    if self.repeat_task.is_some() {
                        return Ok(());
                    }
                    let interval = self
                        .interval
                        .read(cx)
                        .value()
                        .parse::<u64>()
                        .ok()
                        .filter(|interval| (500..=60000).contains(interval))
                        .ok_or_else(|| t!("serial.tool.invalid_interval"))?;
                    let message = self.message.read(cx).value();
                    let hex = view.read(cx).serial_modes(cx).send_hex;
                    let bytes = view.read(cx).serial_tool_message(
                        &message,
                        hex,
                        self.ending,
                        self.crc,
                        cx,
                    )?;
                    if !matches!(
                        view.read(cx).lifecycle(cx),
                        crate::terminal::TerminalLifecycle::Running
                    ) {
                        return Err(t!("serial.tool.unavailable"));
                    }
                    let generation = view.read(cx).generation(cx);
                    let target = view.downgrade();
                    self.status = t!("serial.tool.repeating", interval = interval);
                    self.repeat_task = Some(cx.spawn(async move |owner, cx| {
                        for count in 1..=60 {
                            cx.background_executor()
                                .timer(Duration::from_millis(interval))
                                .await;
                            let active = owner
                                .update(cx, |owner, cx| {
                                    let result = target
                                        .upgrade()
                                        .filter(|view| {
                                            view.read(cx).generation(cx) == generation
                                                && view.read(cx).serial_modes(cx).send_hex == hex
                                        })
                                        .ok_or_else(|| t!("serial.tool.unavailable"))
                                        .and_then(|view| {
                                            view.read(cx).serial_tool_send_bytes(bytes.clone(), cx)
                                        });
                                    if let Err(error) = result {
                                        owner.repeat_task = None;
                                        owner.status = error;
                                        cx.notify();
                                        return false;
                                    }
                                    if count == 60 {
                                        owner.repeat_task = None;
                                        owner.status = t!("serial.tool.repeat_complete");
                                        cx.notify();
                                        return false;
                                    }
                                    true
                                })
                                .unwrap_or(false);
                            if !active {
                                break;
                            }
                        }
                    }));
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.status = error;
        }
        cx.notify();
    }

    fn display_bytes(&self, bytes: &[u8], hex: bool, cx: &App) -> String {
        if hex {
            bytes
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            let encoding = self
                .target
                .as_ref()
                .and_then(WeakEntity::upgrade)
                .map(|view| view.read(cx).serial_tool_encoding(cx))
                .unwrap_or_default();
            let mut decoder = crate::terminal::TerminalDecoder::new(encoding);
            String::from_utf8_lossy(&decoder.decode(bytes, true)).into_owned()
        }
    }
}

impl Render for SerialToolsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let target = self.target.as_ref().and_then(WeakEntity::upgrade);
        let snapshot = target
            .as_ref()
            .map(|view| view.read(cx).serial_tool_snapshot(cx))
            .unwrap_or_default();
        let modes = target
            .as_ref()
            .map(|view| view.read(cx).serial_modes(cx))
            .unwrap_or_default();
        let running = target.as_ref().is_some_and(|view| {
            matches!(
                view.read(cx).lifecycle(cx),
                crate::terminal::TerminalLifecycle::Running
            )
        });
        let repeating = self.repeat_task.is_some() || self.group_task.is_some();
        let assistant = self.assistant_enabled(cx);
        let preview = {
            let text = self.display_bytes(
                &snapshot.bytes[snapshot.bytes.len().saturating_sub(256)..],
                modes.receive_hex,
                cx,
            );
            let mut lines = text.lines().rev().take(8).collect::<Vec<_>>();
            lines.reverse();
            lines.join("\n")
        };
        let status = if snapshot.paused {
            t!("serial.tool.circuit_breaker")
        } else {
            self.status.clone()
        };
        let action = |id: &'static str, label: SharedString, command: SerialToolCommand| {
            let dispatch = self.dispatch.clone();
            Button::new(id)
                .small()
                .label(label)
                .on_click(move |_, window, cx| {
                    dispatch.dispatch_action(&SerialToolAction(command.clone()), window, cx)
                })
        };
        let check = |id: &'static str,
                     label: SharedString,
                     checked: bool,
                     command: fn(bool) -> SerialToolCommand| {
            let dispatch = self.dispatch.clone();
            Checkbox::new(id)
                .small()
                .w_auto()
                .flex_shrink_0()
                .label(label)
                .checked(checked)
                .on_click(move |enabled, window, cx| {
                    dispatch.dispatch_action(&SerialToolAction(command(*enabled)), window, cx)
                })
        };
        let body = match self.page {
            SerialToolPage::Transfer if assistant => self.render_assistant(running, cx),
            SerialToolPage::Transfer => v_flex()
                .id("serial-default-transfer")
                .test_support()
                .size_full()
                .min_h_0()
                .gap_4()
                .child(
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .gap_2()
                        .child(
                            h_flex()
                                .justify_between()
                                .child(div().text_sm().child(t!("serial.tool.receive")))
                                .child(
                                    check(
                                        "serial-tool-capture",
                                        t!("serial.tool.capture"),
                                        snapshot.capturing,
                                        SerialToolCommand::Capture,
                                    )
                                    .disabled(!running),
                                ),
                        )
                        .child(
                            div()
                                .id("serial-tool-preview")
                                .test_support()
                                .role(Role::Status)
                                .aria_label(preview.clone())
                                .flex_1()
                                .min_h_16()
                                .p_2()
                                .border_1()
                                .border_color(cx.theme().border)
                                .rounded(cx.theme().radius)
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_sm()
                                .overflow_y_scroll()
                                .track_scroll(&self.preview_scroll)
                                .vertical_scrollbar(&self.preview_scroll)
                                .child(if preview.is_empty() {
                                    t!("serial.tool.capture_empty").to_string()
                                } else {
                                    preview
                                }),
                        )
                        .child(
                            div()
                                .id("serial-tool-count")
                                .test_support()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!(
                                    "serial.tool.count",
                                    received = snapshot.received,
                                    replies = snapshot.replies
                                )),
                        )
                        .child(
                            h_flex()
                                .flex_wrap()
                                .gap_2()
                                .child(
                                    action(
                                        "serial-tool-copy",
                                        t!("serial.tool.copy"),
                                        SerialToolCommand::Copy,
                                    )
                                    .disabled(snapshot.bytes.is_empty()),
                                )
                                .child(action(
                                    "serial-tool-clear",
                                    t!("serial.tool.clear"),
                                    SerialToolCommand::Clear,
                                ))
                                .child(
                                    action(
                                        "serial-tool-use-capture",
                                        t!("serial.tool.use_capture"),
                                        SerialToolCommand::UseCapture,
                                    )
                                    .disabled(snapshot.bytes.is_empty() || snapshot.automatic)
                                    .tooltip(t!("serial.tool.capture_help")),
                                ),
                        ),
                )
                .child(
                    v_flex()
                        .id("serial-default-sender")
                        .test_support()
                        .flex_shrink_0()
                        .pt_3()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .gap_2()
                        .child(div().text_sm().child(t!("serial.tool.message")))
                        .child(
                            div()
                                .id("serial-tool-message")
                                .test_support()
                                .w_full()
                                .h(rems(6.))
                                .child(
                                    Textarea::new(&self.message)
                                        .h(relative(1.))
                                        .text_sm()
                                        .disabled(repeating)
                                        .aria_label(t!("serial.tool.message"))
                                        .font_family(cx.theme().mono_font_family.clone()),
                                ),
                        )
                        .child(h_flex().flex_wrap().gap_1().children(
                            SerialLineEnding::ALL.into_iter().map(|ending| {
                                let dispatch = self.dispatch.clone();
                                Button::new(("serial-tool-ending", ending as u64))
                                    .small()
                                    .label(ending.label())
                                    .selected(self.ending == ending)
                                    .disabled(repeating)
                                    .on_click(move |_, window, cx| {
                                        dispatch.dispatch_action(
                                            &SerialToolAction(SerialToolCommand::Ending(ending)),
                                            window,
                                            cx,
                                        )
                                    })
                            }),
                        ))
                        .child(
                            check(
                                "serial-tool-crc",
                                t!("serial.tool.crc"),
                                self.crc,
                                SerialToolCommand::Crc,
                            )
                            .disabled(repeating)
                            .tooltip(t!("serial.tool.crc_help")),
                        )
                        .child(
                            action(
                                "serial-tool-send",
                                t!("serial.hex.send"),
                                SerialToolCommand::Send,
                            )
                            .primary()
                            .w_full()
                            .tooltip(if modes.send_hex {
                                t!("serial.tool.send_hex_hint")
                            } else {
                                t!("serial.tool.send_text_hint")
                            })
                            .disabled(!running || repeating),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    check(
                                        "serial-tool-repeat",
                                        t!("serial.tool.repeat"),
                                        repeating,
                                        SerialToolCommand::Repeat,
                                    )
                                    .disabled(!running)
                                    .tooltip(t!("serial.tool.repeat_help")),
                                )
                                .child(
                                    div().flex_1().min_w_0().child(
                                        Input::new(&self.interval)
                                            .id("serial-tool-interval")
                                            .small()
                                            .aria_label(t!("serial.tool.repeat"))
                                            .disabled(repeating),
                                    ),
                                )
                                .child(div().text_xs().child("ms")),
                        ),
                )
                .into_any_element(),
            SerialToolPage::Reply => v_flex()
                .id("serial-default-reply")
                .test_support()
                .gap_4()
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            check(
                                "serial-tool-auto",
                                t!("serial.tool.automatic"),
                                snapshot.automatic,
                                SerialToolCommand::Automatic,
                            )
                            .disabled(!running)
                            .tooltip(t!("serial.tool.rule_help")),
                        )
                        .child(
                            check(
                                "serial-tool-hex",
                                t!("serial.tool.hex"),
                                self.hex,
                                SerialToolCommand::Hex,
                            )
                            .disabled(snapshot.automatic),
                        ),
                )
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child(t!("serial.tool.pattern")))
                        .child(
                            Input::new(&self.pattern)
                                .id("serial-tool-pattern")
                                .aria_label(t!("serial.tool.pattern"))
                                .disabled(snapshot.automatic),
                        ),
                )
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child(t!("serial.tool.reply")))
                        .child(
                            Input::new(&self.reply)
                                .id("serial-tool-reply")
                                .aria_label(t!("serial.tool.reply"))
                                .disabled(snapshot.automatic),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("serial.tool.rule_brief")),
                )
                .into_any_element(),
            SerialToolPage::Connection => v_flex()
                .id("serial-default-connection")
                .test_support()
                .gap_4()
                .when_some(snapshot.diagnostics.as_ref(), |body, diagnostics| {
                    body.child(
                        v_flex()
                            .gap_2()
                            .id("serial-wire-diagnostics")
                            .test_support()
                            .child(div().text_sm().child(t!("serial.diagnostics.title")))
                            .child(div().text_xs().child(diagnostics.configuration.clone()))
                            .child(
                                div()
                                    .id("serial-wire-count")
                                    .test_support()
                                    .role(Role::Status)
                                    .aria_label(t!(
                                        "serial.diagnostics.count",
                                        received = diagnostics.received,
                                        transmitted = diagnostics.transmitted,
                                        errors = diagnostics.errors
                                    ))
                                    .text_xs()
                                    .child(t!(
                                        "serial.diagnostics.count",
                                        received = diagnostics.received,
                                        transmitted = diagnostics.transmitted,
                                        errors = diagnostics.errors
                                    )),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(match diagnostics.first_received_ms {
                                        Some(ms) => t!("serial.diagnostics.first", ms = ms),
                                        None => t!("serial.diagnostics.no_receive"),
                                    }),
                            )
                            .child(div().text_xs().child(t!(
                                "serial.diagnostics.inputs",
                                cts = match diagnostics.cts {
                                    Some(true) => t!("serial.diagnostics.on"),
                                    Some(false) => t!("serial.diagnostics.off"),
                                    None => t!("serial.diagnostics.unknown"),
                                },
                                dsr = match diagnostics.dsr {
                                    Some(true) => t!("serial.diagnostics.on"),
                                    Some(false) => t!("serial.diagnostics.off"),
                                    None => t!("serial.diagnostics.unknown"),
                                }
                            )))
                            .when_some(diagnostics.last_error.as_ref(), |body, error| {
                                body.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().danger)
                                        .child(error.clone()),
                                )
                            }),
                    )
                })
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child(t!("serial.tool.baud")))
                        .child(
                            Input::new(&self.baud)
                                .id("serial-tool-baud")
                                .aria_label(t!("serial.tool.baud")),
                        ),
                )
                .child(h_flex().flex_wrap().gap_2().children(
                    [9600, 115200, 921600].into_iter().map(|baud| {
                        let dispatch = self.dispatch.clone();
                        Button::new(("serial-tool-baud-preset", baud as u64))
                            .small()
                            .label(baud.to_string())
                            .on_click(move |_, window, cx| {
                                dispatch.dispatch_action(
                                    &SerialToolAction(SerialToolCommand::BaudPreset(baud)),
                                    window,
                                    cx,
                                )
                            })
                    }),
                ))
                .child(
                    action(
                        "serial-tool-apply-baud",
                        t!("serial.tool.apply_baud"),
                        SerialToolCommand::Baud,
                    )
                    .w_full()
                    .disabled(
                        !running || repeating || snapshot.automatic || self.baud_task.is_some(),
                    ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("serial.tool.baud_help")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("serial.tool.scope")),
                )
                .into_any_element(),
        };
        let dispatch = self.dispatch.clone();
        let status_line = |status: SharedString| {
            div()
                .id("serial-tool-status")
                .test_support()
                .role(Role::Status)
                .aria_label(status.clone())
                .text_sm()
                .child(status)
        };
        let body_area = if self.page == SerialToolPage::Transfer {
            div()
                .id("serial-tool-body")
                .test_support()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .when(!assistant, |body| {
                    body.track_scroll(&self.body_scroll)
                        .vertical_scrollbar(&self.body_scroll)
                })
                .child(
                    v_flex()
                        .p_3()
                        .size_full()
                        .min_h_0()
                        .when(!assistant, |body| body.min_h(rems(34.)))
                        .gap_2()
                        .child(body)
                        .child(status_line(status.clone())),
                )
                .into_any_element()
        } else {
            div()
                .id("serial-tool-body")
                .test_support()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.body_scroll)
                .vertical_scrollbar(&self.body_scroll)
                .child(
                    v_flex()
                        .p_3()
                        .gap_3()
                        .child(body)
                        .child(status_line(status.clone()))
                        .when(
                            self.repeat_task.is_some() && self.page != SerialToolPage::Transfer,
                            |body| {
                                body.child(action(
                                    "serial-tool-stop-repeat",
                                    t!("serial.tool.stop_repeat"),
                                    SerialToolCommand::Repeat(false),
                                ))
                            },
                        ),
                )
                .into_any_element()
        };
        v_flex()
            .id("serial-tools")
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .min_h_0()
            .child(self.render_assistant_switch(cx))
            .child(
                TabBar::new("serial-tool-tabs")
                    .small()
                    .underline()
                    .selected_index(
                        SerialToolPage::ALL
                            .iter()
                            .position(|page| *page == self.page)
                            .unwrap_or(0),
                    )
                    .on_click(move |index: &usize, window, cx| {
                        dispatch.dispatch_action(
                            &SerialToolAction(SerialToolCommand::Page(SerialToolPage::ALL[*index])),
                            window,
                            cx,
                        )
                    })
                    .children(SerialToolPage::ALL.into_iter().map(|page| {
                        Tab::new().label(if assistant && page == SerialToolPage::Transfer {
                            t!("serial.assistant.title")
                        } else {
                            page.label()
                        })
                    })),
            )
            .child(body_area)
    }
}
