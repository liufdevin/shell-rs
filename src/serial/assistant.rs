use super::*;
use crate::host::{SerialAssistant, SerialCommand, SerialCommandEnding, SerialCommandGroup};
use gpui_kit::component::table::{Table, TableBody, TableCell, TableHead, TableHeader, TableRow};
use gpui_kit::component::{
    WindowExt as _,
    form::{Field, Form},
};

const COMMANDS_PER_PAGE: usize = 16;
const COMMAND_COLUMNS: [f32; 5] = [4.5, 9.5, 3., 10., 5.5];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SerialAssistantCommand {
    SelectGroup(u64),
    CreateGroup,
    RenameGroup,
    DeleteGroup,
    NewCommand,
    InsertCommand,
    SelectCommand(u64),
    EditCommand(u64),
    DuplicateCommand(u64),
    DeleteCommand(u64),
    ToggleCommand(u64, bool),
    MoveUp(u64),
    MoveDown(u64),
    Run(Option<u64>),
    RunLoop,
    Stop,
    Undo,
    CommandPage(bool),
}

pub(super) struct AssistantEditor {
    pub(super) group: Option<u64>,
    command: Option<u64>,
    group_search: Entity<InputState>,
    command_scroll: ScrollHandle,
    _group_search_observer: Subscription,
    undo: Option<SerialAssistant>,
    page: usize,
}

impl AssistantEditor {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SerialToolsPanel>) -> Self {
        let group_search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("serial.assistant.search_groups"))
        });
        let group_search_observer = cx.observe(&group_search, |_, _, cx| cx.notify());
        Self {
            group: None,
            command: None,
            group_search,
            command_scroll: ScrollHandle::new(),
            _group_search_observer: group_search_observer,
            undo: None,
            page: 0,
        }
    }
}

#[derive(Clone, Copy)]
enum CommandPlacement {
    Append,
    Before(u64),
    Replace(u64),
}

struct CommandForm {
    name: Entity<InputState>,
    content: Entity<TextareaState>,
    delay: Entity<InputState>,
    ending: SerialLineEnding,
    hex: bool,
    crc: bool,
}

impl CommandForm {
    fn new(saved: Option<&SerialCommand>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            name: cx.new(|cx| {
                InputState::new(window, cx).default_value(saved.map_or("", |command| &command.name))
            }),
            content: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(4)
                    .default_value(saved.map_or("", |command| &command.content))
            }),
            delay: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(saved.map_or(500, |command| command.delay_ms).to_string())
            }),
            ending: saved.map_or_else(SerialLineEnding::default, |command| {
                runtime_ending(&command.ending)
            }),
            hex: saved.is_some_and(|command| command.hex),
            crc: saved.is_some_and(|command| command.crc),
        }
    }

    fn draft(&self, cx: &App) -> Result<SerialCommand, SharedString> {
        let name = required_name(self.name.read(cx).value().as_str())?;
        let delay_ms = self
            .delay
            .read(cx)
            .value()
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|delay| *delay <= 60000)
            .ok_or_else(|| t!("serial.assistant.invalid_delay"))?;
        let mut draft = SerialCommand::default();
        draft.name = name;
        draft.content = self.content.read(cx).value().to_string();
        draft.delay_ms = delay_ms;
        draft.ending = stored_ending(self.ending);
        draft.hex = self.hex;
        draft.crc = self.crc;
        draft.enabled = true;
        Ok(draft)
    }
}

impl Render for CommandForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content_label = t!("serial.assistant.command_content");
        Form::new()
            .child(
                Field::new()
                    .label(t!("serial.assistant.command_name"))
                    .required(true)
                    .child(Input::new(&self.name).id("serial-assistant-command-name")),
            )
            .child(
                Field::new()
                    .label(t!("serial.assistant.command_delay"))
                    .child(Input::new(&self.delay).id("serial-assistant-delay")),
            )
            .child(
                Field::new()
                    .label(content_label.clone())
                    .required(true)
                    .child(
                        div()
                            .id("serial-assistant-command-content")
                            .test_support()
                            .h(rems(7.))
                            .child(
                                Textarea::new(&self.content)
                                    .h_full()
                                    .aria_label(content_label)
                                    .font_family(cx.theme().mono_font_family.clone()),
                            ),
                    ),
            )
            .child(Field::new().child(h_flex().gap_2().flex_wrap().children(
                SerialLineEnding::ALL.into_iter().map(|ending| {
                    ending_style(
                        Button::new(ending_id(ending))
                            .small()
                            .label(ending.label())
                            .on_click(cx.listener(move |form, _, _, cx| {
                                form.ending = ending;
                                cx.notify();
                            })),
                        self.ending == ending,
                    )
                }),
            )))
            .child(
                Field::new().child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            Checkbox::new("serial-assistant-draft-hex")
                                .small()
                                .w_auto()
                                .label(t!("serial.mode.send_hex"))
                                .checked(self.hex)
                                .on_click(cx.listener(|form, enabled, _, cx| {
                                    form.hex = *enabled;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Checkbox::new("serial-assistant-draft-crc")
                                .small()
                                .w_auto()
                                .label(t!("serial.tool.crc"))
                                .checked(self.crc)
                                .on_click(cx.listener(|form, enabled, _, cx| {
                                    form.crc = *enabled;
                                    cx.notify();
                                })),
                        ),
                ),
            )
    }
}

fn required_name(name: &str) -> Result<String, SharedString> {
    let name = name.trim();
    if name.is_empty() {
        Err(t!("serial.assistant.name_required"))
    } else {
        Ok(name.to_owned())
    }
}

fn dialog_result(result: Result<(), SharedString>, window: &mut Window, cx: &mut App) -> bool {
    if let Err(error) = result {
        window.push_notification(crate::shared::form_error_notification(error), cx);
        false
    } else {
        true
    }
}

fn ending_id(ending: SerialLineEnding) -> &'static str {
    match ending {
        SerialLineEnding::None => "serial-assistant-ending-none",
        SerialLineEnding::Lf => "serial-assistant-ending-lf",
        SerialLineEnding::Cr => "serial-assistant-ending-cr",
        SerialLineEnding::CrLf => "serial-assistant-ending-crlf",
    }
}

fn ending_style(button: Button, selected: bool) -> Button {
    button.selected(selected).toggled(selected).map(|button| {
        if selected {
            button.primary().font_weight(FontWeight::SEMIBOLD)
        } else {
            button.outline()
        }
    })
}

impl SerialToolsPanel {
    pub(in crate::serial) fn render_assistant_sender(&self, locked: bool, cx: &App) -> AnyElement {
        let dispatch = self.dispatch.clone();
        let busy = locked || self.group_task.is_some() || self.repeat_task.is_some();
        let tool_button = |id, label: SharedString, command: SerialToolCommand| {
            let dispatch = dispatch.clone();
            Button::new(id)
                .small()
                .label(label)
                .on_click(move |_, window, cx| {
                    dispatch.send(command.clone(), window, cx);
                })
        };
        let automatic = self
            .target
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .is_some_and(|view| view.read(cx).serial_tool_snapshot(cx).automatic);
        v_flex()
            .id("serial-assistant")
            .test_support()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .child(
                        div()
                            .id("serial-tool-message")
                            .test_support()
                            .capture_action(|action: &gpui_kit::base::input::Enter, window, cx| {
                                if !action.shift && !action.secondary {
                                    cx.stop_propagation();
                                    window.dispatch_action(
                                        Box::new(crate::app::SerialAssistantAction(
                                            super::super::SerialAssistantTabCommand::Tool(
                                                SerialToolCommand::Send,
                                            ),
                                        )),
                                        cx,
                                    );
                                }
                            })
                            .flex_1()
                            .min_w_0()
                            .h(rems(3.5))
                            .child(
                                Textarea::new(&self.message)
                                    .h_full()
                                    .text_sm()
                                    .disabled(busy)
                                    .aria_label(t!("serial.tool.message"))
                                    .font_family(cx.theme().mono_font_family.clone()),
                            ),
                    )
                    .child(
                        tool_button(
                            "serial-tool-send",
                            t!("serial.hex.send"),
                            SerialToolCommand::Send,
                        )
                        .primary()
                        .disabled(!self.can_send_assistant(cx)),
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .children(SerialLineEnding::ALL.into_iter().map(|ending| {
                        ending_style(
                            tool_button(
                                ending_id(ending),
                                ending.label(),
                                SerialToolCommand::Ending(ending),
                            ),
                            self.ending == ending,
                        )
                        .disabled(busy)
                    }))
                    .child(
                        Checkbox::new("serial-tool-crc")
                            .small()
                            .w_auto()
                            .label(t!("serial.tool.crc"))
                            .checked(self.crc)
                            .disabled(busy)
                            .on_click({
                                let dispatch = dispatch.clone();
                                move |enabled, window, cx| {
                                    dispatch.send(SerialToolCommand::Crc(*enabled), window, cx)
                                }
                            }),
                    )
                    .child(
                        Checkbox::new("serial-tool-repeat")
                            .small()
                            .w_auto()
                            .label(t!("serial.tool.repeat"))
                            .checked(self.repeat_task.is_some())
                            .disabled(
                                self.repeat_task.is_none()
                                    && (!self.can_send_assistant(cx) || automatic),
                            )
                            .on_click(move |enabled, window, cx| {
                                dispatch.send(SerialToolCommand::Repeat(*enabled), window, cx)
                            }),
                    )
                    .child(
                        Input::new(&self.interval)
                            .id("serial-tool-interval")
                            .small()
                            .w_16()
                            .disabled(busy)
                            .aria_label(t!("serial.tool.repeat")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("ms"),
                    )
                    .child(
                        div()
                            .ml_auto()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("serial.assistant.send_keys")),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn assistant_value(&self, cx: &App) -> SerialAssistant {
        self.assistant_storage
            .as_ref()
            .and_then(|(store, key, _)| store.read(cx).serial_assistant(key).cloned())
            .unwrap_or_default()
    }

    pub(in crate::serial) fn load_saved_command(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.group_task.is_some() || self.repeat_task.is_some() {
            return false;
        }
        let value = self.assistant_value(cx);
        let Some(command) = value
            .groups
            .iter()
            .flat_map(|group| &group.commands)
            .find(|command| command.id == id)
        else {
            return false;
        };
        self.message.update(cx, |input, cx| {
            input.set_value(command.content.clone(), window, cx)
        });
        self.ending = runtime_ending(&command.ending);
        self.crc = command.crc;
        self.change_assistant_send_hex(command.hex, cx);
        self.status = t!("serial.assistant.loaded_draft");
        true
    }

    fn save_assistant(
        &self,
        value: SerialAssistant,
        cx: &mut Context<Self>,
    ) -> Result<(), SharedString> {
        let (store, key, host) = self
            .assistant_storage
            .as_ref()
            .ok_or_else(|| t!("serial.tool.unavailable"))?;
        if store.update(cx, |store, cx| {
            store.set_serial_assistant(key.clone(), *host, value, cx)
        }) {
            Ok(())
        } else {
            Err(t!("serial.assistant.limits"))
        }
    }

    fn open_group_form(
        &self,
        group: Option<&SerialCommandGroup>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let key = self
            .assistant_storage
            .as_ref()
            .map(|(_, key, _)| key.clone());
        let group_id = group.map(|group| group.id);
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(group.map_or("", |group| &group.name))
                .placeholder(t!("serial.assistant.group_name"))
        });
        let owner = cx.weak_entity();
        let title = if group_id.is_some() {
            t!("serial.assistant.rename_group")
        } else {
            t!("serial.assistant.create_group")
        };
        window.open_dialog(cx, move |dialog, _, _| {
            let (name, owner, key) = (name.clone(), owner.clone(), key.clone());
            dialog
                .title(title.clone())
                .overlay_closable(false)
                .child(
                    Form::new().child(
                        Field::new()
                            .label(t!("serial.assistant.group_name"))
                            .required(true)
                            .child(Input::new(&name).id("serial-assistant-group-name")),
                    ),
                )
                .footer(crate::shared::commit_footer(
                    "serial-assistant-save-group",
                    t!("common.save"),
                ))
                .on_ok(move |_, window, cx| {
                    let result = required_name(name.read(cx).value().as_str()).and_then(|name| {
                        owner
                            .update(cx, |tools, cx| {
                                tools.commit_group(key.as_deref(), group_id, name, cx)
                            })
                            .map_err(|_| t!("serial.tool.unavailable"))?
                    });
                    dialog_result(result, window, cx)
                })
                .on_close(|_, window, cx| crate::shared::dismiss_form_error(window, cx))
        });
    }

    fn commit_group(
        &mut self,
        key: Option<&str>,
        group_id: Option<u64>,
        name: String,
        cx: &mut Context<Self>,
    ) -> Result<(), SharedString> {
        self.check_editor_target(key)?;
        let mut value = self.assistant_value(cx);
        let id = if let Some(id) = group_id {
            let group = value
                .groups
                .iter_mut()
                .find(|group| group.id == id)
                .ok_or_else(|| t!("serial.assistant.create_first"))?;
            group.name = name;
            id
        } else {
            let mut group = SerialCommandGroup::default();
            group.id = value.allocate_id();
            group.name = name;
            let id = group.id;
            value.groups.push(group);
            id
        };
        self.save_assistant(value, cx)?;
        if let Some(editor) = &mut self.assistant {
            editor.undo = None;
            if editor.group != Some(id) {
                editor.group = Some(id);
                editor.command = None;
                editor.page = 0;
            }
        }
        self.status = SharedString::default();
        cx.notify();
        Ok(())
    }

    fn check_editor_target(&self, key: Option<&str>) -> Result<(), SharedString> {
        if self.group_task.is_some()
            || self.repeat_task.is_some()
            || !self
                .assistant_storage
                .as_ref()
                .is_some_and(|(_, current, _)| key == Some(current.as_str()))
        {
            return Err(t!("serial.tool.unavailable"));
        }
        Ok(())
    }

    fn open_command_form(
        &self,
        group: &SerialCommandGroup,
        placement: CommandPlacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let key = self
            .assistant_storage
            .as_ref()
            .map(|(_, key, _)| key.clone());
        let group_id = group.id;
        let saved = match placement {
            CommandPlacement::Replace(id) => group.commands.iter().find(|command| command.id == id),
            _ => None,
        };
        let title = match placement {
            CommandPlacement::Append => t!("serial.assistant.add_command"),
            CommandPlacement::Before(_) => t!("serial.assistant.insert_command"),
            CommandPlacement::Replace(_) => t!("serial.assistant.edit_command"),
        };
        let form = cx.new(|cx| CommandForm::new(saved, window, cx));
        let owner = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let (form, owner, key) = (form.clone(), owner.clone(), key.clone());
            dialog
                .title(title.clone())
                .overlay_closable(false)
                .child(form.clone())
                .footer(crate::shared::commit_footer(
                    "serial-assistant-save-command",
                    t!("common.save"),
                ))
                .on_ok(move |_, window, cx| {
                    let result = form.read(cx).draft(cx).and_then(|draft| {
                        owner
                            .update(cx, |tools, cx| {
                                tools.commit_command(key.as_deref(), group_id, placement, draft, cx)
                            })
                            .map_err(|_| t!("serial.tool.unavailable"))?
                    });
                    dialog_result(result, window, cx)
                })
                .on_close(|_, window, cx| crate::shared::dismiss_form_error(window, cx))
        });
    }

    fn commit_command(
        &mut self,
        key: Option<&str>,
        group_id: u64,
        placement: CommandPlacement,
        mut draft: SerialCommand,
        cx: &mut Context<Self>,
    ) -> Result<(), SharedString> {
        self.check_editor_target(key)?;
        crate::terminal::encode_serial_tool(self.assistant_encoding, &draft.content, draft.hex)?;
        let mut value = self.assistant_value(cx);
        draft.id = match placement {
            CommandPlacement::Replace(id) => id,
            _ => value.allocate_id(),
        };
        let group = value
            .groups
            .iter_mut()
            .find(|group| group.id == group_id)
            .ok_or_else(|| t!("serial.assistant.create_first"))?;
        let index = match placement {
            CommandPlacement::Append => group.commands.len(),
            CommandPlacement::Before(id) | CommandPlacement::Replace(id) => group
                .commands
                .iter()
                .position(|command| command.id == id)
                .ok_or_else(|| t!("serial.assistant.command_missing"))?,
        };
        let id = draft.id;
        if matches!(placement, CommandPlacement::Replace(_)) {
            draft.enabled = group.commands[index].enabled;
            group.commands[index] = draft;
        } else {
            group.commands.insert(index, draft);
        }
        self.save_assistant(value, cx)?;
        if let Some(editor) = &mut self.assistant {
            editor.group = Some(group_id);
            editor.command = Some(id);
            editor.page = index / COMMANDS_PER_PAGE;
            editor.undo = None;
        }
        self.status = SharedString::default();
        cx.notify();
        Ok(())
    }

    pub(super) fn assistant_command(
        &mut self,
        command: &SerialAssistantCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), SharedString> {
        if matches!(command, SerialAssistantCommand::Stop) {
            self.group_cancel.store(true, Ordering::Release);
            self.group_task = None;
            self.group_current = None;
            self.status = t!("serial.assistant.stopped");
            return Ok(());
        }
        let mut value = self.assistant_value(cx);
        if self.group_task.is_some() || self.repeat_task.is_some() {
            return Err(t!("serial.tool.unavailable"));
        }
        if let SerialAssistantCommand::Run(id) = command {
            return self.run_group(*id, false, cx);
        }
        if matches!(command, SerialAssistantCommand::RunLoop) {
            return self.run_group(None, true, cx);
        }
        if matches!(command, SerialAssistantCommand::SelectGroup(_)) {
            self.body_scroll.set_offset(Point::default());
        }
        let selected_group = self.assistant.as_ref().and_then(|editor| editor.group);
        let group = value
            .groups
            .iter()
            .find(|group| Some(group.id) == selected_group)
            .or_else(|| value.groups.first());
        match command {
            SerialAssistantCommand::CreateGroup => {
                self.open_group_form(None, window, cx);
                return Ok(());
            }
            SerialAssistantCommand::RenameGroup => {
                let group = group.ok_or_else(|| t!("serial.assistant.create_first"))?;
                self.open_group_form(Some(group), window, cx);
                return Ok(());
            }
            SerialAssistantCommand::NewCommand
            | SerialAssistantCommand::InsertCommand
            | SerialAssistantCommand::EditCommand(_) => {
                let group = group.ok_or_else(|| t!("serial.assistant.create_first"))?;
                let placement = match command {
                    SerialAssistantCommand::NewCommand => CommandPlacement::Append,
                    SerialAssistantCommand::InsertCommand => CommandPlacement::Before(
                        self.assistant
                            .as_ref()
                            .and_then(|editor| editor.command)
                            .filter(|id| group.commands.iter().any(|saved| saved.id == *id))
                            .ok_or_else(|| t!("serial.assistant.command_missing"))?,
                    ),
                    SerialAssistantCommand::EditCommand(id)
                        if group.commands.iter().any(|saved| saved.id == *id) =>
                    {
                        if let Some(editor) = &mut self.assistant {
                            editor.command = Some(*id);
                        }
                        CommandPlacement::Replace(*id)
                    }
                    _ => return Err(t!("serial.assistant.command_missing")),
                };
                self.open_command_form(group, placement, window, cx);
                return Ok(());
            }
            _ => {}
        }
        let group_id = group.map(|group| group.id);
        let previous = value.clone();
        let Some(editor) = self.assistant.as_mut() else {
            return Ok(());
        };
        let selection = (editor.group, editor.command);
        match command {
            SerialAssistantCommand::SelectGroup(id) => {
                if value.groups.iter().any(|group| group.id == *id) {
                    editor.group = Some(*id);
                    editor.command = None;
                    editor.page = 0;
                    editor.command_scroll.set_offset(Point::default());
                }
            }
            SerialAssistantCommand::SelectCommand(id) => {
                if group.is_some_and(|group| group.commands.iter().any(|command| command.id == *id))
                {
                    editor.command = Some(*id);
                }
            }
            SerialAssistantCommand::DeleteGroup => {
                value.groups.retain(|group| Some(group.id) != group_id);
                editor.group = None;
                editor.command = None;
                editor.undo = Some(previous.clone());
            }
            SerialAssistantCommand::DuplicateCommand(id) => {
                let new_id = value.allocate_id();
                if let Some(group) = value
                    .groups
                    .iter_mut()
                    .find(|group| Some(group.id) == group_id)
                    && let Some(index) = group.commands.iter().position(|command| command.id == *id)
                {
                    let mut copy = group.commands[index].clone();
                    copy.id = new_id;
                    group.commands.insert(index + 1, copy);
                    if value.is_valid() {
                        editor.page = (index + 1) / COMMANDS_PER_PAGE;
                        editor.command = Some(new_id);
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
            SerialAssistantCommand::MoveUp(id) | SerialAssistantCommand::MoveDown(id) => {
                if let Some(group) = value
                    .groups
                    .iter_mut()
                    .find(|group| Some(group.id) == group_id)
                    && let Some(index) = group.commands.iter().position(|command| command.id == *id)
                {
                    let neighbor = if matches!(command, SerialAssistantCommand::MoveUp(_)) {
                        index.checked_sub(1)
                    } else {
                        (index + 1 < group.commands.len()).then_some(index + 1)
                    };
                    if let Some(neighbor) = neighbor {
                        group.commands.swap(index, neighbor);
                        editor.page = neighbor / COMMANDS_PER_PAGE;
                    }
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
                    .map_or(1, |group| {
                        group.commands.len().max(1).div_ceil(COMMANDS_PER_PAGE)
                    });
                editor.page = if *next {
                    (editor.page + 1).min(pages - 1)
                } else {
                    editor.page.saturating_sub(1)
                };
                editor.command_scroll.set_offset(Point::default());
            }
            SerialAssistantCommand::Run(_)
            | SerialAssistantCommand::RunLoop
            | SerialAssistantCommand::Stop
            | SerialAssistantCommand::CreateGroup
            | SerialAssistantCommand::RenameGroup
            | SerialAssistantCommand::NewCommand
            | SerialAssistantCommand::InsertCommand
            | SerialAssistantCommand::EditCommand(_) => unreachable!(),
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
        looping: bool,
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
                    command.id,
                    command.name.clone(),
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
        let cancel = Arc::new(AtomicBool::new(false));
        self.group_cancel = cancel.clone();
        self.status = t!("serial.assistant.running", count = commands.len());
        self.group_task = Some(cx.spawn(async move |owner, cx| {
            let result: Result<(), SharedString> = async {
                let mut cycle = 1_u64;
                loop {
                    for (index, (id, name, bytes, delay)) in commands.iter().enumerate() {
                        if cancel.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        let answer = owner
                            .update(cx, |owner, cx| {
                                let view = target
                                    .upgrade()
                                    .filter(|view| view.read(cx).generation(cx) == generation)
                                    .ok_or_else(|| t!("serial.tool.unavailable"))?;
                                owner.group_current = Some(*id);
                                owner.status = if looping {
                                    t!(
                                        "serial.assistant.loop_progress",
                                        cycle = cycle,
                                        index = index + 1,
                                        total = commands.len(),
                                        name = name
                                    )
                                } else {
                                    t!(
                                        "serial.assistant.send_progress",
                                        index = index + 1,
                                        total = commands.len(),
                                        name = name
                                    )
                                };
                                cx.notify();
                                view.read(cx).serial_tool_send_checked(bytes.clone(), cx)
                            })
                            .map_err(|_| t!("serial.tool.unavailable"))??;
                        let deadline = Instant::now() + Duration::from_secs(12);
                        loop {
                            if cancel.load(Ordering::Acquire) {
                                return Ok(());
                            }
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
                        // A zero-delay loop must still yield so Stop and connection
                        // changes can be processed between consecutive sends.
                        let delay = if looping { (*delay).max(1) } else { *delay };
                        cx.background_executor()
                            .timer(Duration::from_millis(u64::from(delay)))
                            .await;
                    }
                    if !looping || cancel.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    cycle = cycle.saturating_add(1);
                }
            }
            .await;
            let cancelled = cancel.load(Ordering::Acquire);
            let _ = owner.update(cx, |owner, cx| {
                // A cancelled run must never clear a newer run's state.
                if !Arc::ptr_eq(&owner.group_cancel, &cancel) {
                    return;
                }
                owner.group_task = None;
                owner.group_current = None;
                if !cancelled {
                    owner.status = result
                        .err()
                        .unwrap_or_else(|| t!("serial.assistant.complete"));
                }
                cx.notify();
            });
        }));
        Ok(())
    }

    pub(super) fn render_assistant(&self, cx: &App) -> AnyElement {
        let Some(editor) = &self.assistant else {
            return div().into_any_element();
        };
        let default = SerialAssistant::default();
        let value = self
            .assistant_storage
            .as_ref()
            .and_then(|(store, key, _)| store.read(cx).serial_assistant(key))
            .unwrap_or(&default);
        let group = value
            .groups
            .iter()
            .find(|group| Some(group.id) == editor.group)
            .or_else(|| value.groups.first());
        let busy = self.group_task.is_some() || self.repeat_task.is_some();
        let page_count = group.map_or(1, |group| {
            group.commands.len().max(1).div_ceil(COMMANDS_PER_PAGE)
        });
        let page = editor.page.min(page_count - 1);
        let button = |id: ElementId, label: SharedString, command: SerialAssistantCommand| {
            let dispatch = self.dispatch.clone();
            Button::new(id)
                .small()
                .label(label)
                .disabled(busy)
                .on_click(move |_, window, cx| {
                    dispatch.send(SerialToolCommand::Assistant(command.clone()), window, cx)
                })
        };
        let commands = group
            .map(|group| group.commands.as_slice())
            .unwrap_or_default();
        let command_list = self.render_command_table(commands, editor.command, page, busy, cx);
        let query = editor.group_search.read(cx).value().trim().to_lowercase();
        h_flex()
            .id("serial-assistant")
            .test_support()
            .size_full()
            .min_h_0()
            .items_start()
            .child(
                v_flex()
                    .w(rems(13.))
                    .h_full()
                    .flex_shrink_0()
                    .min_h_0()
                    .p_2()
                    .gap_2()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().sidebar)
                    .child(
                        button(
                            "serial-assistant-create-group".into(),
                            t!("serial.assistant.create_group_button"),
                            SerialAssistantCommand::CreateGroup,
                        )
                        .icon(IconName::Plus)
                        .outline()
                        .w_full()
                        .disabled(busy || value.groups.len() >= 32),
                    )
                    .child(
                        Input::new(&editor.group_search)
                            .id("serial-assistant-group-search")
                            .small()
                            .aria_label(t!("serial.assistant.search_groups")),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(self.command_icon(
                                "serial-assistant-rename-group".into(),
                                Icon::new(CatalogIcon::Pencil),
                                t!("serial.assistant.rename_group_button"),
                                SerialAssistantCommand::RenameGroup,
                                busy || group.is_none(),
                            ))
                            .child(self.command_icon(
                                "serial-assistant-delete-group".into(),
                                Icon::new(CatalogIcon::Trash),
                                t!("serial.assistant.delete_group"),
                                SerialAssistantCommand::DeleteGroup,
                                busy || group.is_none(),
                            ))
                            .when(editor.undo.is_some(), |row| {
                                row.child(self.command_icon(
                                    "serial-assistant-undo".into(),
                                    Icon::new(CatalogIcon::RotateCcw),
                                    t!("serial.assistant.undo"),
                                    SerialAssistantCommand::Undo,
                                    busy,
                                ))
                            }),
                    )
                    .child(
                        v_flex()
                            .id("serial-assistant-groups")
                            .test_support()
                            .gap_1()
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .children(
                                value
                                    .groups
                                    .iter()
                                    .filter(|saved| saved.name.to_lowercase().contains(&query))
                                    .map(|saved| {
                                        button(
                                            ("serial-assistant-group", saved.id).into(),
                                            saved.name.clone().into(),
                                            SerialAssistantCommand::SelectGroup(saved.id),
                                        )
                                        .ghost()
                                        .w_full()
                                        .selected(group.is_some_and(|group| group.id == saved.id))
                                    }),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .min_h_0()
                    .p_2()
                    .gap_3()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_h_0()
                            .gap_2()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .flex_wrap()
                                    .child(
                                        button(
                                            "serial-assistant-new-command".into(),
                                            t!("serial.assistant.add_command_button"),
                                            SerialAssistantCommand::NewCommand,
                                        )
                                        .icon(IconName::Plus)
                                        .outline()
                                        .disabled(busy || group.is_none() || commands.len() >= 128),
                                    )
                                    .child(
                                        button(
                                            "serial-assistant-insert-command".into(),
                                            t!("serial.assistant.insert_command_button"),
                                            SerialAssistantCommand::InsertCommand,
                                        )
                                        .outline()
                                        .disabled(
                                            busy || commands.len() >= 128
                                                || !commands.iter().any(|command| {
                                                    Some(command.id) == editor.command
                                                }),
                                        ),
                                    )
                                    .child(div().flex_1())
                                    .child(self.editor_page_button(SerialToolPage::Reply))
                                    .child(self.return_to_console_button()),
                            )
                            .child(
                                div()
                                    .id("serial-assistant-commands")
                                    .test_support()
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_scroll()
                                    .track_scroll(&editor.command_scroll)
                                    .vertical_scrollbar(&editor.command_scroll)
                                    .horizontal_scrollbar(&editor.command_scroll)
                                    .child(
                                        v_flex()
                                            .min_w(rems(45.))
                                            .w_full()
                                            .child(command_list)
                                            .when(commands.is_empty(), |column| {
                                                column.child(
                                                    div()
                                                        .id("serial-assistant-command-empty")
                                                        .test_support()
                                                        .text_center()
                                                        .py_6()
                                                        .text_sm()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child(if group.is_some() {
                                                            t!("serial.assistant.empty")
                                                        } else {
                                                            t!("serial.assistant.create_first")
                                                        }),
                                                )
                                            }),
                                    ),
                            )
                            .when(page_count > 1, |body| {
                                body.child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(
                                            button(
                                                "serial-assistant-previous".into(),
                                                t!("serial.assistant.previous"),
                                                SerialAssistantCommand::CommandPage(false),
                                            )
                                            .disabled(busy || page == 0),
                                        )
                                        .child(div().flex_1().text_xs().child(t!(
                                            "serial.assistant.page",
                                            page = page + 1,
                                            total = page_count
                                        )))
                                        .child(
                                            button(
                                                "serial-assistant-next".into(),
                                                t!("serial.assistant.next"),
                                                SerialAssistantCommand::CommandPage(true),
                                            )
                                            .disabled(busy || page + 1 == page_count),
                                        ),
                                )
                            })
                            .when(!self.status.is_empty(), |body| {
                                body.child(
                                    div()
                                        .id("serial-tool-status")
                                        .test_support()
                                        .text_sm()
                                        .aria_label(self.status.clone())
                                        .child(self.status.clone()),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }

    fn command_icon(
        &self,
        id: ElementId,
        icon: Icon,
        label: SharedString,
        command: SerialAssistantCommand,
        disabled: bool,
    ) -> Button {
        let dispatch = self.dispatch.clone();
        Button::new(id)
            .ghost()
            .xsmall()
            .icon(icon)
            .tooltip(label.clone())
            .accessibility_label(label)
            .disabled(disabled)
            .on_click(move |_, window, cx| {
                dispatch.send(SerialToolCommand::Assistant(command.clone()), window, cx)
            })
    }

    fn render_command_table(
        &self,
        commands: &[SerialCommand],
        selected: Option<u64>,
        page: usize,
        busy: bool,
        cx: &App,
    ) -> Table {
        let fixed_head = |width, label: SharedString| {
            TableHead::new()
                .w(rems(width))
                .min_w_0()
                .flex_shrink_0()
                .child(label)
        };
        let header = TableRow::new()
            .child(fixed_head(
                COMMAND_COLUMNS[0],
                t!("serial.assistant.command_sort"),
            ))
            .child(fixed_head(
                COMMAND_COLUMNS[1],
                t!("serial.assistant.command_actions"),
            ))
            .child(fixed_head(
                COMMAND_COLUMNS[2],
                t!("serial.assistant.command_index"),
            ))
            .child(fixed_head(
                COMMAND_COLUMNS[3],
                t!("serial.assistant.command_name"),
            ))
            .child(fixed_head(
                COMMAND_COLUMNS[4],
                t!("serial.assistant.command_delay"),
            ))
            .child(
                TableHead::new()
                    .flex_1()
                    .min_w_0()
                    .child(t!("serial.assistant.command_content")),
            );
        Table::new()
            .small()
            .accessibility_label(t!("serial.assistant.commands"))
            .child(TableHeader::new().child(header))
            .child(
                TableBody::new().children(
                    commands
                        .iter()
                        .enumerate()
                        .skip(page * COMMANDS_PER_PAGE)
                        .take(COMMANDS_PER_PAGE)
                        .map(|(index, command)| {
                            self.render_command_row(
                                index,
                                command,
                                commands.len(),
                                selected == Some(command.id),
                                busy,
                                cx,
                            )
                        }),
                ),
            )
    }

    fn render_command_row(
        &self,
        index: usize,
        command: &SerialCommand,
        count: usize,
        selected: bool,
        busy: bool,
        cx: &App,
    ) -> TableRow {
        let cell = |width| {
            TableCell::new()
                .w(rems(width))
                .min_w_0()
                .flex_shrink_0()
                .items_center()
        };
        let id = command.id;
        let dispatch = self.dispatch.clone();
        let select = dispatch.clone();
        let load = self.dispatch.focus.clone();
        let load_label = t!("serial.assistant.load_draft");
        let preview = command.content.replace('\r', "").replace('\n', " ↵ ");
        TableRow::new()
            .when(selected, |row| row.bg(cx.theme().list_active))
            .child(
                cell(COMMAND_COLUMNS[0]).child(
                    h_flex()
                        .child(self.command_icon(
                            ("serial-assistant-up", id).into(),
                            Icon::new(IconName::ArrowUp),
                            t!("serial.assistant.move_up"),
                            SerialAssistantCommand::MoveUp(id),
                            busy || index == 0,
                        ))
                        .child(self.command_icon(
                            ("serial-assistant-down", id).into(),
                            Icon::new(IconName::ArrowDown),
                            t!("serial.assistant.move_down"),
                            SerialAssistantCommand::MoveDown(id),
                            busy || index + 1 == count,
                        )),
                ),
            )
            .child(
                cell(COMMAND_COLUMNS[1]).child(
                    h_flex()
                        .items_center()
                        .child(
                            Checkbox::new(("serial-assistant-command-enabled", id))
                                .small()
                                .w_auto()
                                .accessibility_label(t!("serial.assistant.include"))
                                .checked(command.enabled)
                                .disabled(busy)
                                .on_click(move |enabled, window, cx| {
                                    dispatch.send(
                                        SerialToolCommand::Assistant(
                                            SerialAssistantCommand::ToggleCommand(id, *enabled),
                                        ),
                                        window,
                                        cx,
                                    )
                                }),
                        )
                        .child(self.command_icon(
                            ("serial-assistant-edit", id).into(),
                            Icon::new(CatalogIcon::Pencil),
                            t!("serial.assistant.edit_command_button"),
                            SerialAssistantCommand::EditCommand(id),
                            busy,
                        ))
                        .child(self.command_icon(
                            ("serial-assistant-duplicate", id).into(),
                            Icon::new(IconName::Copy),
                            t!("serial.assistant.duplicate"),
                            SerialAssistantCommand::DuplicateCommand(id),
                            busy || count >= 128,
                        ))
                        .child(
                            Button::new(("serial-assistant-load", id))
                                .ghost()
                                .xsmall()
                                .icon(IconName::ArrowLeft)
                                .tooltip(load_label.clone())
                                .accessibility_label(load_label)
                                .disabled(busy)
                                .on_click(move |_, window, cx| {
                                    load.dispatch_action(
                                        &crate::app::LoadSerialCommand(id),
                                        window,
                                        cx,
                                    )
                                }),
                        )
                        .child(self.command_icon(
                            ("serial-assistant-delete", id).into(),
                            Icon::new(CatalogIcon::Trash),
                            t!("serial.assistant.delete"),
                            SerialAssistantCommand::DeleteCommand(id),
                            busy,
                        )),
                ),
            )
            .child(cell(COMMAND_COLUMNS[2]).child((index + 1).to_string()))
            .child(
                cell(COMMAND_COLUMNS[3]).child(
                    Button::new(("serial-assistant-select-command", id))
                        .xsmall()
                        .ghost()
                        .label(command.name.clone())
                        .w_full()
                        .truncate()
                        .toggled(selected)
                        .disabled(busy)
                        .on_click(move |_, window, cx| {
                            select.send(
                                SerialToolCommand::Assistant(
                                    SerialAssistantCommand::SelectCommand(id),
                                ),
                                window,
                                cx,
                            )
                        }),
                ),
            )
            .child(cell(COMMAND_COLUMNS[4]).child(command.delay_ms.to_string()))
            .child(
                TableCell::new()
                    .min_w_0()
                    .flex_1()
                    .items_center()
                    .overflow_hidden()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .whitespace_nowrap()
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(preview),
                    ),
            )
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

pub(super) fn runtime_ending(ending: &SerialCommandEnding) -> SerialLineEnding {
    match ending {
        SerialCommandEnding::None => SerialLineEnding::None,
        SerialCommandEnding::Lf => SerialLineEnding::Lf,
        SerialCommandEnding::Cr => SerialLineEnding::Cr,
        SerialCommandEnding::CrLf => SerialLineEnding::CrLf,
    }
}
