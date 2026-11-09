use super::*;
use crate::host::{SerialAssistant, SerialCommand, SerialCommandEnding, SerialCommandGroup};
use gpui_kit::component::switch::Switch;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SerialAssistantCommand {
    Enable(bool),
    SelectGroup(u64),
    CreateGroup,
    RenameGroup,
    DeleteGroup,
    SaveCommand,
    NewCommand,
    EditCommand(u64),
    DeleteCommand(u64),
    ToggleCommand(u64, bool),
    MoveUp(u64),
    Run(Option<u64>),
    Stop,
    Undo,
    CommandPage(bool),
}

pub(super) struct AssistantEditor {
    group: Option<u64>,
    command: Option<u64>,
    group_name: Entity<InputState>,
    name: Entity<InputState>,
    delay: Entity<InputState>,
    undo: Option<SerialAssistant>,
    page: usize,
}

impl AssistantEditor {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SerialToolsPanel>) -> Self {
        Self {
            group: None,
            command: None,
            group_name: cx
                .new(|cx| InputState::new(window, cx).validate(|text, _| text.len() <= 128)),
            name: cx.new(|cx| InputState::new(window, cx).validate(|text, _| text.len() <= 128)),
            delay: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value("500")
                    .validate(|text, _| text.len() <= 5)
            }),
            undo: None,
            page: 0,
        }
    }
}

impl SerialToolsPanel {
    pub(super) fn apply_assistant_layout(
        &mut self,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.assistant_layout_enabled == enabled {
            return;
        }
        self.assistant_layout_enabled = enabled;
        self.group_task = None;
        self.repeat_task = None;
        self.page = SerialToolPage::Transfer;
        self.body_scroll.set_offset(Point::default());
        if enabled && self.assistant.is_none() {
            self.assistant = Some(AssistantEditor::new(window, cx));
        }
    }

    fn assistant_value(&self, cx: &App) -> SerialAssistant {
        self.target
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .and_then(|view| view.read(cx).serial_assistant_target())
            .and_then(|(store, key, _)| store.read(cx).serial_assistant(&key).cloned())
            .unwrap_or_default()
    }

    pub(super) fn assistant_enabled(&self, cx: &App) -> bool {
        self.target
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .and_then(|view| view.read(cx).serial_assistant_target())
            .is_some_and(|(store, key, _)| {
                store
                    .read(cx)
                    .serial_assistant(&key)
                    .is_some_and(|value| value.enabled)
            })
    }

    fn save_assistant(
        &self,
        value: SerialAssistant,
        cx: &mut Context<Self>,
    ) -> Result<(), SharedString> {
        let (store, key, host) = self
            .target
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .and_then(|view| view.read(cx).serial_assistant_target())
            .ok_or_else(|| t!("serial.tool.unavailable"))?;
        if store.update(cx, |store, cx| {
            store.set_serial_assistant(key, host, value, cx)
        }) {
            Ok(())
        } else {
            Err(t!("serial.assistant.limits"))
        }
    }

    pub(super) fn assistant_command(
        &mut self,
        command: &SerialAssistantCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), SharedString> {
        if matches!(command, SerialAssistantCommand::Stop) {
            self.group_task = None;
            self.status = t!("serial.assistant.stopped");
            return Ok(());
        }
        let mut value = self.assistant_value(cx);
        if let SerialAssistantCommand::Enable(enabled) = command {
            self.group_task = None;
            self.repeat_task = None;
            value.enabled = *enabled;
            self.apply_assistant_layout(*enabled, window, cx);
            window.focus(&self.focus_handle, cx);
            return self.save_assistant(value, cx);
        }
        if !value.enabled || self.group_task.is_some() {
            return Err(t!("serial.tool.unavailable"));
        }
        if let SerialAssistantCommand::Run(id) = command {
            return self.run_group(*id, cx);
        }
        let previous = value.clone();
        let Some(editor) = self.assistant.as_mut() else {
            return Ok(());
        };
        let group_id = editor
            .group
            .or_else(|| value.groups.first().map(|group| group.id));
        let selection = (editor.group, editor.command);
        match command {
            SerialAssistantCommand::CreateGroup => {
                let name = editor.group_name.read(cx).value().trim().to_owned();
                if name.is_empty() {
                    return Err(t!("serial.assistant.name_required"));
                }
                let mut group = SerialCommandGroup::default();
                group.id = value.allocate_id();
                group.name = name;
                editor.group = Some(group.id);
                editor.command = None;
                editor.page = 0;
                value.groups.push(group);
            }
            SerialAssistantCommand::SelectGroup(id) => {
                if let Some(group) = value.groups.iter().find(|group| group.id == *id) {
                    editor.group = Some(*id);
                    editor.command = None;
                    editor.page = 0;
                    editor.group_name.update(cx, |input, cx| {
                        input.set_value(group.name.clone(), window, cx)
                    });
                }
            }
            SerialAssistantCommand::RenameGroup => {
                let name = editor.group_name.read(cx).value().trim().to_owned();
                if name.is_empty() {
                    return Err(t!("serial.assistant.name_required"));
                }
                if let Some(group) = value
                    .groups
                    .iter_mut()
                    .find(|group| Some(group.id) == group_id)
                {
                    group.name = name;
                }
            }
            SerialAssistantCommand::DeleteGroup => {
                value.groups.retain(|group| Some(group.id) != group_id);
                editor.group = None;
                editor.command = None;
                editor.undo = Some(previous.clone());
            }
            SerialAssistantCommand::NewCommand => {
                editor.command = None;
                editor
                    .name
                    .update(cx, |input, cx| input.set_value("", window, cx));
            }
            SerialAssistantCommand::SaveCommand => {
                let name = editor.name.read(cx).value().trim().to_owned();
                if name.is_empty() {
                    return Err(t!("serial.assistant.name_required"));
                }
                let delay_ms = editor
                    .delay
                    .read(cx)
                    .value()
                    .parse::<u32>()
                    .ok()
                    .filter(|delay| *delay <= 60000)
                    .ok_or_else(|| t!("serial.assistant.invalid_delay"))?;
                let view = self
                    .target
                    .as_ref()
                    .and_then(WeakEntity::upgrade)
                    .ok_or_else(|| t!("serial.tool.unavailable"))?;
                let content = self.message.read(cx).value().to_string();
                let hex = view.read(cx).serial_modes(cx).send_hex;
                view.read(cx)
                    .serial_tool_message(&content, hex, self.ending, self.crc, cx)?;
                let id = editor.command.unwrap_or_else(|| value.allocate_id());
                let mut saved = SerialCommand::default();
                saved.id = id;
                saved.name = name;
                saved.content = content;
                saved.hex = hex;
                saved.ending = stored_ending(self.ending);
                saved.crc = self.crc;
                saved.delay_ms = delay_ms;
                saved.enabled = true;
                let group = value
                    .groups
                    .iter_mut()
                    .find(|group| Some(group.id) == group_id)
                    .ok_or_else(|| t!("serial.assistant.create_first"))?;
                if let Some(existing) = group.commands.iter_mut().find(|command| command.id == id) {
                    saved.enabled = existing.enabled;
                    *existing = saved;
                } else {
                    group.commands.push(saved);
                }
                editor.command = Some(id);
            }
            SerialAssistantCommand::EditCommand(id) => {
                if let Some(saved) = value
                    .groups
                    .iter()
                    .flat_map(|group| &group.commands)
                    .find(|command| command.id == *id)
                {
                    editor.command = Some(*id);
                    editor.name.update(cx, |input, cx| {
                        input.set_value(saved.name.clone(), window, cx)
                    });
                    editor.delay.update(cx, |input, cx| {
                        input.set_value(saved.delay_ms.to_string(), window, cx)
                    });
                    self.message.update(cx, |input, cx| {
                        input.set_value(saved.content.clone(), window, cx)
                    });
                    self.ending = runtime_ending(&saved.ending);
                    self.crc = saved.crc;
                    if let Some(view) = self.target.as_ref().and_then(WeakEntity::upgrade) {
                        view.update(cx, |view, cx| {
                            let mut modes = view.serial_modes(cx);
                            modes.send_hex = saved.hex;
                            view.set_serial_modes(modes, cx);
                        });
                    }
                }
            }
            SerialAssistantCommand::DeleteCommand(id) => {
                for group in &mut value.groups {
                    group.commands.retain(|command| command.id != *id);
                }
                if editor.command == Some(*id) {
                    editor.command = None;
                }
                editor.undo = Some(previous.clone());
            }
            SerialAssistantCommand::ToggleCommand(id, enabled) => {
                if let Some(saved) = value
                    .groups
                    .iter_mut()
                    .flat_map(|group| &mut group.commands)
                    .find(|command| command.id == *id)
                {
                    saved.enabled = *enabled;
                }
            }
            SerialAssistantCommand::MoveUp(id) => {
                if let Some(group) = value
                    .groups
                    .iter_mut()
                    .find(|group| Some(group.id) == group_id)
                    && let Some(index) = group.commands.iter().position(|command| command.id == *id)
                    && index > 0
                {
                    group.commands.swap(index, index - 1);
                }
            }
            SerialAssistantCommand::Undo => {
                if let Some(undo) = editor.undo.take() {
                    value = undo;
                    editor.group = None;
                    editor.command = None;
                }
            }
            SerialAssistantCommand::CommandPage(next) => {
                let pages = value
                    .groups
                    .iter()
                    .find(|group| Some(group.id) == group_id)
                    .map_or(1, |group| group.commands.len().max(1).div_ceil(6));
                editor.page = if *next {
                    (editor.page + 1).min(pages - 1)
                } else {
                    editor.page.saturating_sub(1)
                };
            }
            SerialAssistantCommand::Run(_)
            | SerialAssistantCommand::Stop
            | SerialAssistantCommand::Enable(_) => unreachable!(),
        }
        if !value.is_valid() {
            editor.group = selection.0;
            editor.command = selection.1;
            return Err(t!("serial.assistant.limits"));
        }
        if !matches!(
            command,
            SerialAssistantCommand::DeleteCommand(_)
                | SerialAssistantCommand::DeleteGroup
                | SerialAssistantCommand::Undo
        ) && value != previous
        {
            editor.undo = None;
        }
        self.save_assistant(value, cx)
    }

    fn run_group(
        &mut self,
        single: Option<u64>,
        cx: &mut Context<Self>,
    ) -> Result<(), SharedString> {
        if self.repeat_task.is_some() || self.baud_task.is_some() {
            return Err(t!("serial.tool.unavailable"));
        }
        let view = self
            .target
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .ok_or_else(|| t!("serial.tool.unavailable"))?;
        if view.read(cx).serial_tool_snapshot(cx).automatic {
            return Err(t!("serial.tool.unavailable"));
        }
        let value = self.assistant_value(cx);
        let selected = self.assistant.as_ref().and_then(|editor| editor.group);
        let group = value
            .groups
            .iter()
            .find(|group| Some(group.id) == selected)
            .or_else(|| value.groups.first())
            .ok_or_else(|| t!("serial.assistant.create_first"))?;
        let commands = group
            .commands
            .iter()
            .filter(|command| single.map_or(command.enabled, |id| id == command.id))
            .map(|command| {
                Ok((
                    view.read(cx).serial_tool_message(
                        &command.content,
                        command.hex,
                        runtime_ending(&command.ending),
                        command.crc,
                        cx,
                    )?,
                    command.delay_ms,
                ))
            })
            .collect::<Result<Vec<_>, SharedString>>()?;
        if commands.is_empty() {
            return Err(t!("serial.assistant.no_commands"));
        }
        let generation = view.read(cx).generation(cx);
        let target = view.downgrade();
        self.status = t!("serial.assistant.running", count = commands.len());
        self.group_task = Some(cx.spawn(async move |owner, cx| {
            let result: Result<(), SharedString> = async {
                for (bytes, delay) in commands {
                    let answer = owner
                        .update(cx, |_, cx| {
                            let view = target
                                .upgrade()
                                .filter(|view| view.read(cx).generation(cx) == generation)
                                .ok_or_else(|| t!("serial.tool.unavailable"))?;
                            view.read(cx).serial_tool_send_checked(bytes, cx)
                        })
                        .map_err(|_| t!("serial.tool.unavailable"))??;
                    let deadline = Instant::now() + Duration::from_secs(12);
                    loop {
                        match answer.try_recv() {
                            Ok(Ok(())) => break,
                            Ok(Err(error)) => {
                                return Err(t!("serial.assistant.send_failed", error = error));
                            }
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                return Err(t!("serial.assistant.unsupported"));
                            }
                            Err(_) if Instant::now() >= deadline => {
                                return Err(t!("serial.assistant.timeout"));
                            }
                            Err(_) => {
                                cx.background_executor()
                                    .timer(Duration::from_millis(20))
                                    .await
                            }
                        }
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(u64::from(delay)))
                        .await;
                }
                Ok(())
            }
            .await;
            let _ = owner.update(cx, |owner, cx| {
                owner.group_task = None;
                owner.status = result
                    .err()
                    .unwrap_or_else(|| t!("serial.assistant.complete"));
                cx.notify();
            });
        }));
        Ok(())
    }

    pub(super) fn render_assistant_switch(&self, cx: &App) -> impl IntoElement + use<> {
        let dispatch = self.dispatch.clone();
        let available = self
            .target
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .is_some_and(|view| view.read(cx).serial_assistant_target().is_some());
        div().p_3().flex_shrink_0().child(
            Switch::new("serial-assistant-enable")
                .small()
                .label(t!("serial.assistant.enable"))
                .checked(self.assistant_enabled(cx))
                .disabled(!available)
                .tooltip(t!("serial.assistant.scope"))
                .on_click(move |enabled, window, cx| {
                    dispatch.dispatch_action(
                        &SerialToolAction(SerialToolCommand::Assistant(
                            SerialAssistantCommand::Enable(*enabled),
                        )),
                        window,
                        cx,
                    )
                }),
        )
    }

    pub(super) fn render_assistant(&self, running: bool, cx: &App) -> AnyElement {
        let Some(editor) = &self.assistant else {
            return div().into_any_element();
        };
        let source = self
            .target
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .and_then(|view| view.read(cx).serial_assistant_target());
        let default = SerialAssistant::default();
        let value = source
            .as_ref()
            .and_then(|(store, key, _)| store.read(cx).serial_assistant(key))
            .unwrap_or(&default);
        let group = value
            .groups
            .iter()
            .find(|group| Some(group.id) == editor.group)
            .or_else(|| value.groups.first());
        let busy = self.group_task.is_some() || self.repeat_task.is_some();
        let automatic = self
            .target
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .is_some_and(|view| view.read(cx).serial_tool_snapshot(cx).automatic);
        let page_count = group.map_or(1, |group| group.commands.len().max(1).div_ceil(6));
        let page = editor.page.min(page_count - 1);
        let button = |id: ElementId, label: SharedString, command: SerialAssistantCommand| {
            let dispatch = self.dispatch.clone();
            Button::new(id)
                .small()
                .label(label)
                .disabled(busy)
                .on_click(move |_, window, cx| {
                    dispatch.dispatch_action(
                        &SerialToolAction(SerialToolCommand::Assistant(command.clone())),
                        window,
                        cx,
                    )
                })
        };
        let tool = |id: &'static str, label: SharedString, command: SerialToolCommand| {
            let dispatch = self.dispatch.clone();
            Button::new(id)
                .small()
                .label(label)
                .on_click(move |_, window, cx| {
                    dispatch.dispatch_action(&SerialToolAction(command.clone()), window, cx)
                })
        };
        v_flex().id("serial-assistant").test_support().gap_4().min_w_0()
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(t!("serial.assistant.receive_left")))
            .child(v_flex().gap_2()
                .child(div().text_sm().child(t!("serial.assistant.groups")))
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(t!("serial.assistant.group_name")))
                .child(h_flex().gap_2().child(div().flex_1().min_w_0().child(Input::new(&editor.group_name).id("serial-assistant-group-name").small().disabled(busy).aria_label(t!("serial.assistant.group_name"))))
                    .child(button("serial-assistant-create-group".into(), t!("serial.assistant.create_group"), SerialAssistantCommand::CreateGroup)))
                .child(h_flex().gap_2().flex_wrap()
                    .child(button("serial-assistant-rename-group".into(), t!("serial.assistant.rename_group"), SerialAssistantCommand::RenameGroup).disabled(busy || group.is_none()))
                    .child(button("serial-assistant-delete-group".into(), t!("serial.assistant.delete_group"), SerialAssistantCommand::DeleteGroup).disabled(busy || group.is_none()))
                    .when(editor.undo.is_some(), |row| row.child(button("serial-assistant-undo".into(), t!("serial.assistant.undo"), SerialAssistantCommand::Undo))))
                .child(h_flex().gap_1().flex_wrap().children(value.groups.iter().map(|saved| button(("serial-assistant-group", saved.id).into(), saved.name.clone().into(), SerialAssistantCommand::SelectGroup(saved.id)).selected(group.is_some_and(|group| group.id == saved.id)))))
                .when(group.is_none_or(|group| group.commands.is_empty()), |body| body.child(div().text_xs().text_color(cx.theme().muted_foreground).child(t!("serial.assistant.empty"))))
                .children(group.into_iter().flat_map(|group| &group.commands).enumerate().skip(page * 6).take(6).map(|(index, command)| {
                    let dispatch = self.dispatch.clone();
                    let id = command.id;
                    v_flex().gap_1().py_2().border_b_1().border_color(cx.theme().border)
                        .child(h_flex().gap_2()
                            .child(Checkbox::new(("serial-assistant-command-enabled", id)).small().w_auto().checked(command.enabled).disabled(busy).label(t!("serial.assistant.include")).on_click(move |enabled, window, cx| dispatch.dispatch_action(&SerialToolAction(SerialToolCommand::Assistant(SerialAssistantCommand::ToggleCommand(id, *enabled))), window, cx)))
                            .child(div().flex_1().min_w_0().child(button(("serial-assistant-edit", id).into(), command.name.clone().into(), SerialAssistantCommand::EditCommand(id)).ghost().selected(editor.command == Some(id))))
                            .child(button(("serial-assistant-send", id).into(), t!("serial.hex.send"), SerialAssistantCommand::Run(Some(id))).disabled(!running || busy || automatic)))
                        .child(div().text_xs().truncate().font_family(cx.theme().mono_font_family.clone()).text_color(cx.theme().muted_foreground).child(command.content.chars().take(80).collect::<String>()))
                        .child(h_flex().gap_2()
                            .child(div().flex_1().text_xs().text_color(cx.theme().muted_foreground).child(t!("serial.assistant.command_info", format = if command.hex { "HEX" } else { "STR" }, ending = runtime_ending(&command.ending).label(), delay = command.delay_ms)))
                            .child(button(("serial-assistant-up", id).into(), t!("serial.assistant.move_up"), SerialAssistantCommand::MoveUp(id)).ghost().disabled(busy || index == 0))
                            .child(button(("serial-assistant-delete", id).into(), t!("serial.assistant.delete"), SerialAssistantCommand::DeleteCommand(id)).ghost()))
                }))
                .when(page_count > 1, |body| body.child(h_flex().gap_2()
                    .child(button("serial-assistant-previous".into(), t!("serial.assistant.previous"), SerialAssistantCommand::CommandPage(false)).disabled(busy || page == 0))
                    .child(div().flex_1().text_xs().child(t!("serial.assistant.page", page = page + 1, total = page_count)))
                    .child(button("serial-assistant-next".into(), t!("serial.assistant.next"), SerialAssistantCommand::CommandPage(true)).disabled(busy || page + 1 == page_count))))
                .child(h_flex().gap_2()
                    .child(button("serial-assistant-run".into(), t!("serial.assistant.run"), SerialAssistantCommand::Run(None)).disabled(!running || busy || automatic || group.is_none_or(|group| !group.commands.iter().any(|command| command.enabled))))
                    .child(tool("serial-assistant-stop", t!("serial.assistant.stop"), SerialToolCommand::Assistant(SerialAssistantCommand::Stop)).disabled(self.group_task.is_none())))
            )
            .child(v_flex().gap_2().pt_3().border_t_1().border_color(cx.theme().border)
                .child(h_flex().justify_between().child(div().text_sm().child(t!("serial.tool.message")))
                    .child(button("serial-assistant-new-command".into(), t!("serial.assistant.new_command"), SerialAssistantCommand::NewCommand).ghost()))
                .child(div().id("serial-tool-message").test_support().child(Textarea::new(&self.message).text_sm().disabled(busy).aria_label(t!("serial.tool.message")).font_family(cx.theme().mono_font_family.clone())))
                .child(h_flex().gap_1().flex_wrap().children(SerialLineEnding::ALL.into_iter().map(|ending| tool(match ending { SerialLineEnding::None => "serial-assistant-ending-none", SerialLineEnding::Lf => "serial-assistant-ending-lf", SerialLineEnding::Cr => "serial-assistant-ending-cr", SerialLineEnding::CrLf => "serial-assistant-ending-crlf" }, ending.label(), SerialToolCommand::Ending(ending)).selected(self.ending == ending).disabled(busy))))
                .child(Checkbox::new("serial-tool-crc").small().label(t!("serial.tool.crc")).checked(self.crc).disabled(busy).on_click({let dispatch = self.dispatch.clone(); move |enabled, window, cx| dispatch.dispatch_action(&SerialToolAction(SerialToolCommand::Crc(*enabled)), window, cx)}))
                .child(tool("serial-tool-send", t!("serial.hex.send"), SerialToolCommand::Send).primary().disabled(!running || busy))
                .child(h_flex().gap_2()
                    .child(Checkbox::new("serial-tool-repeat").small().w_auto().label(t!("serial.tool.repeat")).checked(self.repeat_task.is_some()).disabled(!running || self.group_task.is_some()).on_click({ let dispatch = self.dispatch.clone(); move |enabled, window, cx| dispatch.dispatch_action(&SerialToolAction(SerialToolCommand::Repeat(*enabled)), window, cx) }))
                    .child(div().flex_1().min_w_0().child(Input::new(&self.interval).id("serial-tool-interval").small().disabled(busy).aria_label(t!("serial.tool.repeat"))))
                    .child(div().text_xs().child("ms")))
                .child(div().text_sm().child(t!("serial.assistant.command_name")))
                .child(Input::new(&editor.name).id("serial-assistant-command-name").small().disabled(busy))
                .child(h_flex().gap_2().child(div().text_sm().child(t!("serial.assistant.delay"))).child(div().flex_1().min_w_0().child(Input::new(&editor.delay).id("serial-assistant-delay").small().disabled(busy))).child(div().text_xs().child("ms")))
                .child(button("serial-assistant-save-command".into(), if editor.command.is_some() {t!("serial.assistant.save_command")} else {t!("serial.assistant.add_command")}, SerialAssistantCommand::SaveCommand).disabled(busy || group.is_none()))
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(t!("serial.assistant.saved_hint")))
            ).into_any_element()
    }
}

fn stored_ending(ending: SerialLineEnding) -> SerialCommandEnding {
    match ending {
        SerialLineEnding::None => SerialCommandEnding::None,
        SerialLineEnding::Lf => SerialCommandEnding::Lf,
        SerialLineEnding::Cr => SerialCommandEnding::Cr,
        SerialLineEnding::CrLf => SerialCommandEnding::CrLf,
    }
}

fn runtime_ending(ending: &SerialCommandEnding) -> SerialLineEnding {
    match ending {
        SerialCommandEnding::None => SerialLineEnding::None,
        SerialCommandEnding::Lf => SerialLineEnding::Lf,
        SerialCommandEnding::Cr => SerialLineEnding::Cr,
        SerialCommandEnding::CrLf => SerialLineEnding::CrLf,
    }
}
