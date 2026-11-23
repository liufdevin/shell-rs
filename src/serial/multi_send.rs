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
        let busy = self.group_task.is_some() || self.repeat_task.is_some();
        let automatic = self
            .target
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .is_some_and(|view| view.read(cx).serial_tool_snapshot(cx).automatic);
        let send_disabled = !running || busy || automatic || self.baud_task.is_some();
        let any_enabled = commands.iter().any(|command| command.enabled);
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
        let picker_label = current
            .map(|group| SharedString::from(group.name.clone()))
            .unwrap_or_else(|| t!("serial.assistant.no_groups"));
        let rows = commands.iter().enumerate().map(|(index, command)| {
            let id = command.id;
            let toggle = self.dispatch.clone();
            let send = self.dispatch.clone();
            let preview = command.content.replace('\r', "").replace('\n', " ↵ ");
            let ending = assistant::runtime_ending(&command.ending);
            let info = t!(
                "serial.assistant.command_info",
                format = if command.hex { "HEX" } else { "TEXT" },
                ending = ending.label(),
                delay = command.delay_ms
            );
            h_flex()
                .id(("serial-multi-row", id))
                .test_support()
                .items_start()
                .gap_2()
                .p_2()
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
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(div().text_sm().truncate().child(format!(
                            "{}. {}",
                            index + 1,
                            command.name
                        )))
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
                                .child(info),
                        )
                        .when(command.crc, |row| {
                            row.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(t!("serial.tool.crc")),
                            )
                        }),
                )
                .child(
                    Button::new(("serial-multi-send", id))
                        .small()
                        .outline()
                        .label(t!("serial.hex.send"))
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
                    .p_3()
                    .gap_2()
                    .flex_shrink_0()
                    .child(
                        h_flex()
                            .gap_2()
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
                            .gap_2()
                            .flex_wrap()
                            .child(
                                button(
                                    "serial-assistant-run-quick",
                                    t!("serial.assistant.send_once"),
                                    SerialAssistantCommand::Run(None),
                                )
                                .disabled(send_disabled || !any_enabled),
                            )
                            .child(
                                button(
                                    "serial-assistant-loop",
                                    t!("serial.assistant.loop"),
                                    SerialAssistantCommand::RunLoop,
                                )
                                .disabled(send_disabled || !any_enabled),
                            )
                            .child(
                                button(
                                    "serial-assistant-stop-quick",
                                    t!("serial.assistant.stop"),
                                    SerialAssistantCommand::Stop,
                                )
                                .outline()
                                .disabled(self.group_task.is_none()),
                            ),
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
                                ),
                        )
                    }),
            )
            .into_any_element()
    }
}
