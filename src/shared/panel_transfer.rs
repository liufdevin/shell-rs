use gpui_kit::{App, EntityId, Global};

#[derive(Default)]
struct TransferringPanel(Option<EntityId>);

impl Global for TransferringPanel {}

pub(crate) fn is_panel_transfer(panel: EntityId, cx: &App) -> bool {
    cx.try_global::<TransferringPanel>()
        .is_some_and(|moving| moving.0 == Some(panel))
}

pub(crate) fn during_panel_transfer<Result>(
    panel: EntityId,
    cx: &mut App,
    transfer: impl FnOnce(&mut App) -> Result,
) -> Result {
    let previous = cx
        .try_global::<TransferringPanel>()
        .and_then(|moving| moving.0);
    cx.set_global(TransferringPanel(Some(panel)));
    let result = transfer(cx);
    cx.set_global(TransferringPanel(previous));
    result
}
