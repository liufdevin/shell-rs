//! The window shell: title bar, dock area with its sidebars, the switch of
//! the right sidebar's tools, status bar, and the start page (recent hosts)
//! the center shows while no tab is open.

mod analytics;
mod android_devices;
mod app_lock;
mod app_shell;
mod cli_changes;
mod compose;
mod compose_pane;
mod credentials;
mod detached;
mod dock_skin;
mod editors;
mod follow;
mod forwards;
mod front;
mod imports;
mod links;
mod local_terminal_options;
mod local_terminal_window;
mod notices;
mod projects;
mod recent_hosts;
mod serial_files;
mod sidebar;
mod snippets;
mod status_bar;
mod sync;
mod tabs;
mod terminal_groups;
mod terminal_schemes;
mod title_bar;
mod tmux;
mod tool_sidebar;
mod tools;
mod updates;
mod window_state;
mod workspace_view;
mod zmodem;

pub use links::open_link_once_open;
pub use local_terminal_window::{
    AdminTerminalRequest, AdministratorTerminalLauncher, is_administrator,
    open_admin_terminal_window,
};
pub use window_state::WindowState;
pub use workspace_view::{Workspace, notify_once_open, window_options};

pub use app_shell::{AppShell, StartupFailure};
