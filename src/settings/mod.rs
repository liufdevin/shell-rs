//! 设置: the settings tab, the settings it edits and how they are applied.

mod app_theme;
mod apply;
mod background_images;
mod highlight_rules;
mod local_shell_editor;
mod model;
mod settings_panel;
mod shortcuts_editor;
mod sidebar_tools;
mod store;
mod sync_page;
mod terminal_themes;

pub use apply::apply;
pub use model::{
    AnalyticsSettings, AppSettings, Appearance, Choice, DEFAULT_INTERFACE_FONT_FAMILY,
    DEFAULT_INTERFACE_FONT_SIZE, DeviceLogAppearance, ExternalCliSettings,
    INTERFACE_FONT_SIZE_RANGE, InterfaceLanguage, InterfaceSettings, LocalTerminalSettings,
    NotificationSettings, SidebarTools, SyncLocation, SyncSettings, TerminalFontSettings,
    TerminalHighlightSettings, TerminalThemeSettings, UpdateSettings, WindowSettings,
};
pub use model::{BackgroundImageSettings, DEFAULT_BACKGROUND_OPACITY, TerminalBackgroundSettings};
pub use settings_panel::{SettingsPanel, SettingsPanelEvent};
pub use store::{SettingsProblem, SettingsStore, SettingsStoreEvent};
