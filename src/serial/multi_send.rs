use super::*;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};

impl SerialToolsPanel {
    pub(super) fn render_multi_send(&self, running: bool, cx: &App) -> AnyElement {
        let value = self.assistant_value(cx);
        let group_options: Vec<_> = value
            .groups
            .iter()
            .map(|group| (group.id, group.name.clone()))
            .collect();
        let selected = self.assistant.as_ref().and_then(|editor| editor.group);
        let current = value
            .groups
            .iter()
            .find(|group| Some(group.id) == selected)
            .or_else(|| value.groups.first());
        let group_id = current.map(|group| group.id);
        let commands = current.map_or(&[][..], |group| group.commands.as_slice());
        let enabled_count = commands.iter().filter(|command| command.enabled).count();
        let busy = self.group_task.is_some() || self.repeat_task.is_some();
        let automatic = self
            .target
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .is_some_and(|view| view.read(cx).serial_tool_snapshot(cx).automatic);
        let send_disabled = !running || busy || automatic || self.baud_task.is_some();
        let any_enabled = enabled_count > 0;
        let button = |id, label: SharedString, command: SerialAssistantCommand| {
            let dispatch = self.dispatch.clone();
            Button::new(id)
                .small()
                .label(label)
                .on_click(move |_, window, cx| {
                    dispatch.send(SerialToolCommand::Assistant(command.clone()), window, cx);
                })
        };
        let choose = self.dispatch.clone();
        let edit = self.dispatch.focus.clone();
        let new_group_dispatch = self.dispatch.clone();
        let add_dialog_dispatch = self.dispatch.clone();
        let empty_add_dispatch = self.dispatch.clone();
        let picker_label = current
            .map(|group| SharedString::from(group.name.clone()))
            .unwrap_or_else(|| t!("serial.assistant.no_groups"));
        let total_commands = commands.len();
        let rows = commands.iter().enumerate().map(|(index, command)| {
            let id = command.id;
            let toggle = self.dispatch.clone();
            let toggle_hex = self.dispatch.clone();
            let send = self.dispatch.clone();
            let menu_dispatch = self.dispatch.clone();
            let load_focus = self.dispatch.focus.clone();
            let preview = command.content.replace('\r', "").replace('\n', " ↵ ");
            let ending = assistant::runtime_ending(&command.ending);
            let mut info = t!(
                "serial.assistant.command_info",
                format = if command.hex { "HEX" } else { "TEXT" },
                ending = ending.label(),
                delay = command.delay_ms
            )
            .to_string();
            if command.crc {
                info.push_str(" · CRC");
            }
            let info = SharedString::from(info);
            h_flex()
                .id(("serial-multi-row", id))
                .test_support()
                .items_center()
                .gap_1p5()
                .px_2()
                .py_1p5()
                .border_b_1()
                .border_color(cx.theme().border)
                .when(self.group_current == Some(id), |row| {
                    row.bg(cx.theme().list_active)
                })
                .child(
                    Checkbox::new(("serial-multi-enabled", id))
                        .small()
                        .w_auto()
                        .accessibility_label(t!(
                            "serial.assistant.include_command",
                            name = command.name
                        ))
                        .checked(command.enabled)
                        .disabled(busy)
                        .on_click(move |enabled, window, cx| {
                            toggle.send(
                                SerialToolCommand::Assistant(
                                    SerialAssistantCommand::ToggleCommand(id, *enabled),
                                ),
                                window,
                                cx,
                            )
                        }),
                )
                .child(
                    Checkbox::new(("serial-multi-hex", id))
                        .small()
                        .w_auto()
                        .accessibility_label(t!(
                            "serial.assistant.hex_command",
                            name = command.name
                        ))
                        .checked(command.hex)
                        .disabled(busy)
                        .on_click(move |enabled, window, cx| {
                            toggle_hex.send(
                                SerialToolCommand::Assistant(SerialAssistantCommand::ToggleHex(
                                    id, *enabled,
                                )),
                                window,
                                cx,
                            )
                        }),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .id(("serial-multi-content", id))
                                .test_support()
                                .aria_label(command.content.clone())
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_xs()
                                .truncate()
                                .child(preview),
                        )
                        .child(
                            div()
                                .id(("serial-multi-info", id))
                                .test_support()
                                .aria_label(info.clone())
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .truncate()
                                .child(info),
                        ),
                )
                .child(
                    Button::new(("serial-multi-send", id))
                        .xsmall()
                        .outline()
                        .w(rems(5.5))
                        .flex_shrink_0()
                        .truncate()
                        .label(command.name.clone())
                        .tooltip(t!("serial.assistant.send_command", name = command.name))
                        .accessibility_label(t!(
                            "serial.assistant.send_command",
                            name = command.name
                        ))
                        .disabled(send_disabled)
                        .on_click(move |_, window, cx| {
                            send.send(
                                SerialToolCommand::Assistant(SerialAssistantCommand::Run(Some(id))),
                                window,
                                cx,
                            )
                        }),
                )
                .child(
                    Button::new(("serial-multi-menu", id))
                        .xsmall()
                        .ghost()
                        .flex_shrink_0()
                        .icon(IconName::Ellipsis)
                        .label(SharedString::default())
                        .tooltip(t!("serial.assistant.row_actions"))
                        .accessibility_label(t!("serial.assistant.row_actions"))
                        .disabled(busy)
                        .dropdown_menu(move |menu, _, _| {
                            let edit_cmd = menu_dispatch.clone();
                            let dup_cmd = menu_dispatch.clone();
                            let up_cmd = menu_dispatch.clone();
                            let down_cmd = menu_dispatch.clone();
                            let del_cmd = menu_dispatch.clone();
                            let load_focus = load_focus.clone();
                            menu.item(
                                PopupMenuItem::new(t!("serial.assistant.edit_command_button"))
                                    .icon(Icon::new(CatalogIcon::Pencil))
                                    .on_click(move |_, window, cx| {
                                        edit_cmd.send(
                                            SerialToolCommand::Assistant(
                                                SerialAssistantCommand::EditCommand(id),
                                            ),
                                            window,
                                            cx,
                                        );
                                    }),
                            )
                            .item(
                                PopupMenuItem::new(t!("serial.assistant.load_draft"))
                                    .icon(Icon::new(IconName::ArrowLeft))
                                    .on_click(move |_, window, cx| {
                                        load_focus.dispatch_action(
                                            &crate::app::LoadSerialCommand(id),
                                            window,
                                            cx,
                                        );
                                    }),
                            )
                            .item(
                                PopupMenuItem::new(t!("serial.assistant.duplicate"))
                                    .icon(Icon::new(IconName::Copy))
                                    .disabled(total_commands >= 128)
                                    .on_click(move |_, window, cx| {
                                        dup_cmd.send(
                                            SerialToolCommand::Assistant(
                                                SerialAssistantCommand::DuplicateCommand(id),
                                            ),
                                            window,
                                            cx,
                                        );
                                    }),
                            )
                            .separator()
                            .item(
                                PopupMenuItem::new(t!("serial.assistant.move_up"))
                                    .icon(Icon::new(IconName::ArrowUp))
                                    .disabled(index == 0)
                                    .on_click(move |_, window, cx| {
                                        up_cmd.send(
                                            SerialToolCommand::Assistant(
                                                SerialAssistantCommand::MoveUp(id),
                                            ),
                                            window,
                                            cx,
                                        );
                                    }),
                            )
                            .item(
                                PopupMenuItem::new(t!("serial.assistant.move_down"))
                                    .icon(Icon::new(IconName::ArrowDown))
                                    .disabled(index + 1 == total_commands)
                                    .on_click(move |_, window, cx| {
                                        down_cmd.send(
                                            SerialToolCommand::Assistant(
                                                SerialAssistantCommand::MoveDown(id),
                                            ),
                                            window,
                                            cx,
                                        );
                                    }),
                            )
                            .separator()
                            .item(
                                PopupMenuItem::new(t!("serial.assistant.delete"))
                                    .icon(Icon::new(CatalogIcon::Trash))
                                    .on_click(move |_, window, cx| {
                                        del_cmd.send(
                                            SerialToolCommand::Assistant(
                                                SerialAssistantCommand::DeleteCommand(id),
                                            ),
                                            window,
                                            cx,
                                        );
                                    }),
                            )
                        }),
                )
        });
        let quick_add_bar = self.assistant.as_ref().map(|editor| {
            let quick_empty = editor.quick_content.read(cx).value().is_empty();
            let quick_add_dispatch = self.dispatch.clone();
            let quick_hex_dispatch = self.dispatch.clone();
            v_flex()
                .id("serial-multi-quick-add")
                .test_support()
                .flex_shrink_0()
                .p_2()
                .gap_1p5()
                .border_t_1()
                .border_color(cx.theme().border)
                .child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .child(
                            Input::new(&editor.quick_content)
                                .id("serial-multi-quick-content")
                                .small()
                                .flex_1()
                                .min_w_0()
                                .disabled(busy)
                                .font_family(cx.theme().mono_font_family.clone())
                                .aria_label(t!("serial.assistant.command_content")),
                        )
                        .child(
                            Button::new("serial-multi-quick-add-btn")
                                .small()
                                .outline()
                                .icon(IconName::Plus)
                                .label(t!("serial.assistant.quick_add"))
                                .disabled(busy || total_commands >= 128 || quick_empty)
                                .on_click(move |_, window, cx| {
                                    quick_add_dispatch.send(
                                        SerialToolCommand::Assistant(
                                            SerialAssistantCommand::QuickAdd,
                                        ),
                                        window,
                                        cx,
                                    );
                                }),
                        ),
                )
                .child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .child(
                            Input::new(&editor.quick_name)
                                .id("serial-multi-quick-name")
                                .xsmall()
                                .flex_1()
                                .min_w_0()
                                .disabled(busy)
                                .aria_label(t!("serial.assistant.command_name")),
                        )
                        .child(
                            Checkbox::new("serial-multi-quick-hex")
                                .small()
                                .w_auto()
                                .label("HEX")
                                .checked(editor.quick_hex)
                                .disabled(busy)
                                .on_click(move |enabled, window, cx| {
                                    quick_hex_dispatch.send(
                                        SerialToolCommand::Assistant(
                                            SerialAssistantCommand::QuickHex(*enabled),
                                        ),
                                        window,
                                        cx,
                                    );
                                }),
                        )
                        .child(
                            Input::new(&editor.quick_delay)
                                .id("serial-multi-quick-delay")
                                .xsmall()
                                .w_12()
                                .disabled(busy)
                                .aria_label(t!("serial.assistant.command_delay")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("ms"),
                        ),
                )
        });
        // The toolbar and status stay outside the scrolling command list, so
        // Stop remains reachable even with a full group or a narrow window.
        v_flex()
            .id("serial-multi-send")
            .test_support()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(
                v_flex()
                    .p_2()
                    .gap_1p5()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(
                                Button::new("serial-assistant-group-picker")
                                    .small()
                                    .outline()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .label(picker_label)
                                    .icon(IconName::ChevronDown)
                                    .disabled(value.groups.is_empty() || busy)
                                    .dropdown_menu(move |menu, _, _| {
                                        group_options.iter().fold(menu, |menu, (id, name)| {
                                            let id = *id;
                                            let choose = choose.clone();
                                            menu.item(
                                                PopupMenuItem::new(name.clone())
                                                    .checked(group_id == Some(id))
                                                    .on_click(move |_, window, cx| {
                                                        choose.send(
                                                            SerialToolCommand::Assistant(
                                                                SerialAssistantCommand::SelectGroup(
                                                                    id,
                                                                ),
                                                            ),
                                                            window,
                                                            cx,
                                                        )
                                                    }),
                                            )
                                        })
                                    }),
                            )
                            .child(
                                Button::new("serial-multi-new-group")
                                    .xsmall()
                                    .ghost()
                                    .icon(CatalogIcon::FolderPlus)
                                    .label(SharedString::default())
                                    .tooltip(t!("serial.assistant.create_group_button"))
                                    .accessibility_label(t!("serial.assistant.create_group_button"))
                                    .disabled(busy || value.groups.len() >= 32)
                                    .on_click(move |_, window, cx| {
                                        new_group_dispatch.send(
                                            SerialToolCommand::Assistant(
                                                SerialAssistantCommand::CreateGroup,
                                            ),
                                            window,
                                            cx,
                                        );
                                    }),
                            )
                            .child(
                                Button::new("serial-multi-add-dialog")
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::Plus)
                                    .label(SharedString::default())
                                    .tooltip(t!("serial.assistant.add_command_button"))
                                    .accessibility_label(t!("serial.assistant.add_command_button"))
                                    .disabled(busy || total_commands >= 128)
                                    .on_click(move |_, window, cx| {
                                        add_dialog_dispatch.send(
                                            SerialToolCommand::Assistant(
                                                SerialAssistantCommand::QuickNewCommand,
                                            ),
                                            window,
                                            cx,
                                        );
                                    }),
                            )
                            .child(
                                Button::new("serial-assistant-edit-groups")
                                    .xsmall()
                                    .ghost()
                                    .icon(CatalogIcon::SquarePen)
                                    .label(SharedString::default())
                                    .tooltip(t!("serial.assistant.edit_groups"))
                                    .accessibility_label(t!("serial.assistant.edit_groups"))
                                    .on_click(move |_, window, cx| {
                                        edit.dispatch_action(
                                            &crate::app::OpenSerialCommandEditor,
                                            window,
                                            cx,
                                        );
                                    }),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_1p5()
                            .flex_wrap()
                            .items_center()
                            .child(
                                button(
                                    "serial-assistant-run-quick",
                                    t!("serial.assistant.send_once"),
                                    SerialAssistantCommand::Run(None),
                                )
                                .xsmall()
                                .icon(CatalogIcon::Play)
                                .disabled(send_disabled || !any_enabled),
                            )
                            .child(
                                button(
                                    "serial-assistant-loop",
                                    t!("serial.assistant.loop"),
                                    SerialAssistantCommand::RunLoop,
                                )
                                .xsmall()
                                .icon(CatalogIcon::RotateCw)
                                .disabled(send_disabled || !any_enabled),
                            )
                            .child(
                                button(
                                    "serial-assistant-stop-quick",
                                    t!("serial.assistant.stop"),
                                    SerialAssistantCommand::Stop,
                                )
                                .xsmall()
                                .outline()
                                .icon(CatalogIcon::Square)
                                .disabled(self.group_task.is_none()),
                            )
                            .when(total_commands > 0, |row| {
                                row.child(
                                    div()
                                        .ml_auto()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .child(format!("{enabled_count}/{total_commands}")),
                                )
                            }),
                    )
                    .when(self.repeat_task.is_some(), |toolbar| {
                        let dispatch = self.dispatch.clone();
                        toolbar.child(
                            Button::new("serial-assistant-stop-repeat-quick")
                                .small()
                                .label(t!("serial.tool.stop_repeat"))
                                .on_click(move |_, window, cx| {
                                    dispatch.send(SerialToolCommand::Repeat(false), window, cx);
                                }),
                        )
                    })
                    .when(!self.status.is_empty(), |toolbar| {
                        toolbar.child(
                            div()
                                .id("serial-tool-status")
                                .test_support()
                                .role(Role::Status)
                                .aria_label(self.status.clone())
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(self.status.clone()),
                        )
                    }),
            )
            .when(!commands.is_empty(), |panel| {
                panel.child(
                    h_flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap_1p5()
                        .px_2()
                        .py_1()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            div()
                                .w_4()
                                .text_center()
                                .child(t!("serial.assistant.include")),
                        )
                        .child(div().w_6().text_center().child("HEX"))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(t!("serial.assistant.col_content")),
                        )
                        .child(
                            div()
                                .w(rems(5.5))
                                .flex_shrink_0()
                                .text_center()
                                .child(t!("serial.assistant.col_send")),
                        )
                        .child(div().w_5().flex_shrink_0()),
                )
            })
            .child(
                v_flex()
                    .id("serial-multi-commands")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.body_scroll)
                    .vertical_scrollbar(&self.body_scroll)
                    .children(rows)
                    .when(commands.is_empty(), |list| {
                        list.child(
                            v_flex()
                                .id("serial-multi-empty")
                                .test_support()
                                .size_full()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .p_6()
                                .text_center()
                                .child(
                                    Icon::new(CatalogIcon::LayoutList)
                                        .text_color(cx.theme().muted_foreground),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(t!("serial.assistant.multi_empty_title")),
                                )
                                .child(
                                    div()
                                        .max_w(rems(24.))
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(t!("serial.assistant.multi_empty")),
                                )
                                .child(
                                    Button::new("serial-multi-empty-add")
                                        .small()
                                        .outline()
                                        .icon(IconName::Plus)
                                        .label(t!("serial.assistant.add_command_button"))
                                        .disabled(busy)
                                        .on_click(move |_, window, cx| {
                                            empty_add_dispatch.send(
                                                SerialToolCommand::Assistant(
                                                    SerialAssistantCommand::QuickNewCommand,
                                                ),
                                                window,
                                                cx,
                                            );
                                        }),
                                ),
                        )
                    }),
            )
            .when_some(quick_add_bar, |panel, bar| panel.child(bar))
            .into_any_element()
    }
}
