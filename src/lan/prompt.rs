use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::{
    WindowExt as _,
    button::Button,
    checkbox::Checkbox,
    dialog::{DialogClose, DialogFooter},
    v_flex,
};
use gpui_kit::*;

use super::{LanCommand, LanService};
use crate::app::LanAction;
use crate::i18n::t;

pub(crate) fn show_pair_request(
    service: &Entity<LanService>,
    dispatch: &FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let active = service.read(cx).prompt_active.clone();
    if let Some(active) = active
        && !service
            .read(cx)
            .pending()
            .iter()
            .any(|pending| (pending.id.clone(), pending.token) == active)
    {
        service.update(cx, |service, _| service.prompt_active = None);
        if window.has_active_dialog(cx) {
            window.close_dialog(cx);
        }
        return;
    }
    if window.has_active_dialog(cx) || crate::app_lock::is_locked(cx) {
        return;
    }
    let pending = service.read(cx).pending().into_iter().find(|pending| {
        !service
            .read(cx)
            .prompt_seen
            .contains(&(pending.id.clone(), pending.token))
    });
    let Some(pending) = pending else { return };
    crate::app::bring_forward(window, cx);
    service.update(cx, |service, _| {
        service
            .prompt_seen
            .insert((pending.id.clone(), pending.token));
        service.prompt_active = Some((pending.id.clone(), pending.token));
    });
    let block = Rc::new(Cell::new(false));
    let chosen = Rc::new(Cell::new(false));
    let form = cx.new(|_| PairChoice {
        block: block.clone(),
    });
    let target = dispatch.clone();
    let owner = service.downgrade();
    let can_execute = service.read(cx).permits_execution();
    let allows_devices = service
        .read(cx)
        .core()
        .is_some_and(|core| core.device_access_enabled());
    window.open_dialog(cx, move |dialog, _, _| {
        let id = pending.id.clone();
        let token = pending.token;
        let approve = target.clone();
        let answered = chosen.clone();
        let reject = target.clone();
        let rejected = chosen.clone();
        let blacklist = block.clone();
        let close_id = id.clone();
        let closing_owner = owner.clone();
        let accept_id = id.clone();
        let detail = t!(
            "lan.pair_request_detail",
            name = pending.name,
            ip = pending.address.ip().to_string()
        );
        let code = t!("lan.pair_code", code = pending.code);
        dialog
            .title(t!("lan.pair_request_title"))
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .id("lan-pair-request-detail")
                            .role(Role::Label)
                            .aria_label(detail.clone())
                            .child(detail),
                    )
                    .child(
                        div()
                            .id("lan-pair-request-code")
                            .role(Role::Label)
                            .aria_label(code.clone())
                            .child(code),
                    )
                    .child(t!("lan.pair_permission"))
                    .child(form.clone()),
            )
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|button| button.label(t!("lan.reject"))))
                    .child(
                        Button::new("lan-accept-read")
                            .label(t!("lan.accept_read"))
                            .on_click(move |_, window, cx| {
                                answered.set(true);
                                approve.dispatch_action(
                                    &LanAction(LanCommand::Approve(
                                        accept_id.clone(),
                                        token,
                                        false,
                                    )),
                                    window,
                                    cx,
                                );
                                window.close_dialog(cx);
                            }),
                    )
                    .children(allows_devices.then(|| {
                        let approve = target.clone();
                        let id = id.clone();
                        let answered = chosen.clone();
                        Button::new("lan-accept-devices")
                            .label(t!("lan.devices.accept_control"))
                            .on_click(move |_, window, cx| {
                                answered.set(true);
                                approve.dispatch_action(
                                    &LanAction(LanCommand::ApproveDevices(id.clone(), token)),
                                    window,
                                    cx,
                                );
                                window.close_dialog(cx);
                            })
                    }))
                    .children(can_execute.then(|| {
                        let approve = target.clone();
                        let id = id.clone();
                        let answered = chosen.clone();
                        Button::new("lan-accept-exec")
                            .label(t!("lan.accept_exec"))
                            .on_click(move |_, window, cx| {
                                answered.set(true);
                                approve.dispatch_action(
                                    &LanAction(LanCommand::Approve(id.clone(), token, true)),
                                    window,
                                    cx,
                                );
                                window.close_dialog(cx);
                            })
                    })),
            )
            .on_close(move |_, window, cx| {
                let pending_exists = closing_owner.upgrade().is_some_and(|owner| {
                    let exists = owner
                        .read(cx)
                        .pending()
                        .iter()
                        .any(|pending| pending.id == close_id && pending.token == token);
                    owner.update(cx, |owner, _| owner.prompt_active = None);
                    exists
                });
                if !rejected.get() && pending_exists {
                    reject.dispatch_action(
                        &LanAction(LanCommand::Reject(close_id.clone(), token, blacklist.get())),
                        window,
                        cx,
                    );
                }
            })
    });
}

struct PairChoice {
    block: Rc<Cell<bool>>,
}
impl Render for PairChoice {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Checkbox::new("lan-block-on-reject")
            .label(t!("lan.block_on_reject"))
            .checked(self.block.get())
            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                this.block.set(*checked);
                cx.notify();
            }))
    }
}
