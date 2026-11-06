//! 终端: shared terminal emulation, transports and Dock panels.

mod box_drawing;
mod broadcast;
mod cmder;
mod command_terminal;
mod directory;
mod directory_setup;
mod encoding;
mod engine;
mod environment;
mod exec;
mod font;
mod highlight;
mod input;
mod kept_output;
mod links;
mod local_pty;
mod local_shell;
mod local_terminal_panel;
mod log_filter;
mod model;
mod mouse;
mod notices;
mod run;
mod scrollbar;
mod search;
mod session_io;
mod shell_query;
mod terminal_panel;
mod terminal_view;
mod theme;
mod transport;
mod zmodem;

pub(crate) use broadcast::BroadcastSelection;
pub use cmder::Cmder;
pub use command_terminal::{SshCommand, command_terminal_factory};
pub use directory::ReportedDirectory;
pub use directory_setup::{
    FollowCheck, FollowSetup, check_command as directory_check_command,
    install_command as directory_install_command, installed as directory_installed,
    parse_check as parse_directory_check,
};
pub(crate) use encoding::{TerminalDecoder, encode_input, encode_secret};
pub use engine::{TerminalCell, TerminalEngine, TerminalEvent, TerminalSnapshot};
pub(crate) use environment::strip_launcher_vars;
pub use exec::{ExecTarget, exec_answer};
pub use font::{
    DEFAULT_FONT_SIZE, DEFAULT_LINE_HEIGHT, FONT_SIZE_RANGE, LINE_HEIGHT_RANGE, TerminalFont,
    TerminalFontPreview, is_font_installed, monospace_font_families,
};
pub use highlight::{
    HighlightColor, HighlightRule, HighlightSet, PatternError, TerminalHighlights, default_rules,
};
pub use input::{TerminalInput, TerminalInputEvent};
pub use kept_output::KEPT_LINES;
pub use local_pty::LocalPtyTransportProvider;
pub use local_shell::{LocalLaunch, LocalShell, default_shell_name, installed_shells, quote_word};
pub use local_terminal_panel::{LocalTerminalPanel, LocalTerminalPanelEvent, SavedName};
pub use model::TerminalTabIndicators;
pub use model::{
    LocalTerminalId, MiddleClick, RemoteTerminalId, RightClick, TerminalCursorStyle,
    TerminalInteraction, TerminalLifecycle, TerminalSize, TerminalStatus,
};
pub use notices::TerminalNotice;
pub use run::{
    BusyReason, RunContext, RunEvent, RunFailure, RunHandle, RunProgress, RunShell, RunTiming,
    TerminalRun,
};
pub use search::{SearchDirection, SearchMark, SearchPosition, TerminalFindOption};
pub use session_io::default_log_directory;
pub use session_io::{SerialDataModes, SerialPreferences, TerminalConnectionSettings};
pub(crate) use shell_query::Shell as ShellQuery;
pub(crate) use terminal_panel::TerminalCopy;
pub use terminal_panel::{TerminalPanel, TerminalPanelEvent};
pub(crate) use terminal_view::terminal_key_bindings;
pub use terminal_view::{
    ComposePaneShown, TERMINAL_FIND_KEY_CONTEXT, TERMINAL_KEY_CONTEXT, TerminalMenuItems,
    TerminalView,
};
pub use theme::{TerminalColors, TerminalTheme, import_xshell_schemes};
pub(crate) use transport::MONITOR_PAUSED;
pub(crate) use transport::send_event;
pub use transport::{
    ChannelStream, ExecRequest, ExecResult, FixedLocalTerminalTransportProvider,
    FixedRemoteTerminalTransportProvider, FixedSerialTerminalTransportProvider,
    FixedTelnetTerminalTransportProvider, HostTerminalTransports, LocalTerminalTransportProvider,
    RemoteTerminalTransportProvider, SerialTerminalTransportProvider, SftpChannel,
    SftpChannelRequest, SharedLocalTerminalTransportProvider,
    SharedRemoteTerminalTransportProvider, SharedSerialTerminalTransportProvider,
    SharedTelnetTerminalTransportProvider, SharedTerminalTransportFactory,
    TelnetTerminalTransportProvider, TerminalChannels, TerminalTransport, TerminalTransportCommand,
    TerminalTransportEvent, TerminalTransportFactory,
};
pub use zmodem::{ZmodemDirection, ZmodemReply};
