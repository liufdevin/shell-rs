use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, StyledExt as _,
    button::Button, checkbox::Checkbox, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use shellrs_lan::{AuditAction, DeviceKind};

use super::{LanCommand, LanService, page::button};
use crate::app::LanAction;
use crate::i18n::t;

fn kind_label(kind: DeviceKind) -> SharedString {
    match kind {
        DeviceKind::Serial => t!("lan.devices.serial"),
        DeviceKind::Adb => t!("lan.devices.adb"),
    }
}

pub(super) fn local(service: &LanService, cx: &App) -> AnyElement {
    let core = service.core();
    let devices = core
        .as_ref()
        .map(|core| core.local_devices())
        .unwrap_or_default();
    let shared = core
        .as_ref()
        .map(|core| core.shared_devices())
        .unwrap_or_default();
    let enabled = core
        .as_ref()
        .is_some_and(|core| core.device_access_enabled());
    let mut section = v_flex()
        .gap_3()
        .child(
            div()
                .text_sm()
                .font_medium()
                .child(t!("lan.devices.local_title")),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(t!("lan.devices.help")),
        )
        .child(
            Checkbox::new("lan-device-access")
                .label(t!("lan.devices.enable"))
                .checked(enabled)
                .disabled(core.is_none() || service.busy)
                .on_click(|enabled, window, cx| {
                    window.dispatch_action(
                        Box::new(LanAction(LanCommand::EnableDeviceAccess(*enabled))),
                        cx,
                    )
                }),
        )
        .child(button(
            "lan-refresh-local-devices",
            t!("lan.devices.refresh_local"),
            LanCommand::RefreshLocalDevices,
            core.is_none() || service.busy,
        ))
        .child(div().text_sm().child(t!("lan.devices.whitelist")));
    if devices.is_empty() {
        section = section.child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(t!("lan.devices.no_local")),
        );
    }
    for device in &devices {
        let id = device.id.clone();
        section = section.child(
            Checkbox::new(SharedString::from(format!("lan-device-share-{id}")))
                .label(format!(
                    "{} · {} · {} · {}",
                    kind_label(device.kind),
                    device.name,
                    device.address,
                    device.detail
                ))
                .checked(shared.contains(&id))
                .disabled(service.busy)
                .on_click(move |shared, window, cx| {
                    window.dispatch_action(
                        Box::new(LanAction(LanCommand::ShareDevice(id.clone(), *shared))),
                        cx,
                    )
                }),
        );
    }
    if core.is_some() {
        for stale in shared
            .iter()
            .filter(|id| !devices.iter().any(|device| &device.id == *id))
        {
            section = section.child(button(
                SharedString::from(format!("lan-device-stale-{stale}")),
                t!("lan.devices.remove_missing", device = stale.clone()),
                LanCommand::ShareDevice(stale.clone(), false),
                false,
            ));
        }
    }
    let grants = service.grants();
    let selected = service
        .permission_peer
        .as_ref()
        .filter(|peer| grants.iter().any(|grant| &grant.id == *peer))
        .or_else(|| grants.first().map(|grant| &grant.id));
    section = section.child(
        div()
            .text_sm()
            .font_medium()
            .child(t!("lan.devices.permissions")),
    );
    let mut choices = h_flex().gap_2().flex_wrap();
    for grant in &grants {
        let peer = grant.id.clone();
        choices = choices.child(
            Button::new(SharedString::from(format!("lan-device-peer-{peer}")))
                .small()
                .outline()
                .label(grant.name.clone())
                .selected(selected == Some(&peer))
                .on_click(move |_, window, cx| {
                    window.dispatch_action(
                        Box::new(LanAction(LanCommand::ManageDevicePermissions(peer.clone()))),
                        cx,
                    )
                }),
        );
    }
    section = section.child(choices);
    if let (Some(core), Some(peer)) = (&core, selected) {
        let allowed = core.device_permissions(peer);
        section = section.child(button(
            SharedString::from(format!("lan-device-revoke-{peer}")),
            t!("lan.devices.revoke_control"),
            LanCommand::RevokeDevices(peer.clone()),
            false,
        ));
        for device in devices.iter().filter(|device| shared.contains(&device.id)) {
            let (peer, id) = (peer.clone(), device.id.clone());
            section = section.child(
                Checkbox::new(SharedString::from(format!("lan-device-grant-{peer}-{id}")))
                    .label(format!(
                        "{} · {} · {}",
                        kind_label(device.kind),
                        device.name,
                        device.address
                    ))
                    .checked(allowed.contains(&id))
                    .on_click(move |allowed, window, cx| {
                        window.dispatch_action(
                            Box::new(LanAction(LanCommand::GrantDevice {
                                peer: peer.clone(),
                                device: id.clone(),
                                allowed: *allowed,
                            })),
                            cx,
                        )
                    }),
            );
        }
    }
    section
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(t!("lan.devices.permission_help")),
        )
        .into_any_element()
}

pub(super) fn remote(service: &LanService, cx: &App) -> AnyElement {
    let mut section = v_flex().gap_3().child(
        div()
            .text_sm()
            .font_medium()
            .child(t!("lan.devices.remote_title")),
    );
    if let Some(peer) = &service.device_peer {
        if service.remote_devices.is_empty() {
            section = section.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("lan.devices.no_remote")),
            );
        }
        for device in &service.remote_devices {
            section = section.child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(format!(
                        "{} · {} · {} · {}",
                        kind_label(device.kind),
                        device.name,
                        device.address,
                        device.detail
                    ))
                    .child(button(
                        SharedString::from(format!("lan-open-device-{}", device.id)),
                        t!("lan.devices.open"),
                        LanCommand::OpenDevice {
                            peer: peer.clone(),
                            device: device.id.clone(),
                        },
                        !service.enabled() || service.busy,
                    ))
                    .when(device.kind == DeviceKind::Adb, |row| {
                        row.child(button(
                            SharedString::from(format!("lan-adb-tools-{}", device.id)),
                            t!("lan.devices.adb_tools"),
                            LanCommand::ManageAdb {
                                peer: peer.clone(),
                                device: device.id.clone(),
                            },
                            !service.enabled() || service.busy,
                        ))
                        .child(button(
                            SharedString::from(format!("lan-adb-files-{}", device.id)),
                            t!("lan.devices.adb_files"),
                            LanCommand::AdbFiles {
                                peer: peer.clone(),
                                device: device.id.clone(),
                            },
                            !service.enabled() || service.busy,
                        ))
                    }),
            );
        }
    } else {
        section = section.child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(t!("lan.devices.select_peer")),
        );
    }
    section.into_any_element()
}

pub(super) fn activity(service: &LanService, cx: &App) -> AnyElement {
    let mut section = v_flex().gap_3().child(
        div()
            .text_sm()
            .font_medium()
            .child(t!("lan.devices.active")),
    );
    if let Some(core) = service.core() {
        for active in core.active_devices() {
            section = section.child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(format!(
                        "{} · {} · {}",
                        active.peer, active.source, active.device
                    ))
                    .child(button(
                        SharedString::from(format!("lan-device-disconnect-{}", active.id)),
                        t!("lan.devices.disconnect"),
                        LanCommand::DisconnectDevice(active.id),
                        false,
                    )),
            );
        }
        section = section
            .child(div().text_sm().font_medium().child(t!("lan.devices.audit")))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("lan.devices.audit_help")),
            );
        let mut entries = v_flex().id("lan-device-audit").role(Role::List).gap_2();
        let grants = service.grants();
        for entry in core.audit().into_iter().rev().take(50) {
            let action = match entry.action {
                AuditAction::SharingChanged => t!("lan.devices.audit_sharing"),
                AuditAction::PermissionChanged => t!("lan.devices.audit_permission"),
                AuditAction::Open => t!("lan.devices.audit_open"),
                AuditAction::Denied => t!("lan.devices.audit_denied"),
                AuditAction::Closed => t!("lan.devices.audit_closed"),
                AuditAction::Break => t!("lan.devices.audit_break"),
                AuditAction::Revoked => t!("lan.devices.audit_revoked"),
            };
            let time = chrono::DateTime::from_timestamp(entry.timestamp as i64, 0)
                .map(|time| {
                    time.with_timezone(&chrono::Local)
                        .format("%m-%d %H:%M:%S")
                        .to_string()
                })
                .unwrap_or_default();
            let peer = grants
                .iter()
                .find(|grant| grant.id == entry.peer)
                .map(|grant| grant.name.clone())
                .unwrap_or(entry.peer);
            let text = t!(
                "lan.devices.audit_line",
                time = time,
                action = action,
                peer = peer,
                device = if entry.device_name.is_empty() {
                    entry.device
                } else {
                    format!("{} ({})", entry.device_name, entry.device)
                },
                source = entry
                    .source
                    .map(|source| source.to_string())
                    .unwrap_or_default(),
                incoming = entry.bytes_in,
                outgoing = entry.bytes_out,
                outcome = entry.outcome
            );
            entries = entries.child(
                div()
                    .id(SharedString::from(format!(
                        "lan-device-audit-{}",
                        entry.sequence
                    )))
                    .role(Role::ListItem)
                    .aria_label(text.clone())
                    .text_xs()
                    .child(text),
            );
        }
        section = section.child(entries);
    }
    section.into_any_element()
}
