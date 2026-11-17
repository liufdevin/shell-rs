use gpui_kit::component::menu::PopupMenu;

use crate::app::{CancelTerminalSplit, CenterTab, OpenTerminalSplit};
use crate::i18n::t;

/// Plain presentation snapshot. The workspace derives this from the Dock tree;
/// terminal panels never read the workspace or Dock while rendering a tab.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TerminalSplitMenu {
    available: bool,
    split: bool,
}

impl TerminalSplitMenu {
    pub(crate) fn new(available: bool, split: bool) -> Self {
        Self { available, split }
    }

    pub(crate) fn build(self, menu: PopupMenu, tab: CenterTab) -> PopupMenu {
        let menu = menu.menu_with_disabled(
            t!("shared.terminal_split.open"),
            Box::new(OpenTerminalSplit(tab)),
            !self.available,
        );
        if self.split {
            menu.menu(
                t!("shared.terminal_split.cancel"),
                Box::new(CancelTerminalSplit(tab)),
            )
        } else {
            menu
        }
    }
}
