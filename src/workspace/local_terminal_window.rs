use std::ffi::OsString;
use std::path::PathBuf;

use gpui_kit::component::{ActiveTheme as _, Root, TitleBar, dock::BasePanel as _, v_flex};
use gpui_kit::*;

use crate::app::{
    ClearTerminal, CloseActiveTab, CloseLocalTerminal, CopyTerminal, CopyTerminalFindResults,
    DismissTerminalFind, FindInTerminal, FindNextInTerminal, FindPreviousInTerminal, PasteTerminal,
    RestartLocalTerminal, SelectAllTerminal, ToggleTerminalFindOption,
};
use crate::i18n::t;
use crate::settings::AppSettings;
use crate::terminal::{
    LocalLaunch, LocalPtyTransportProvider, LocalShell, LocalTerminalId, LocalTerminalKind,
    LocalTerminalPanel, LocalTerminalStart, LocalTerminalTransportProvider as _, SavedName,
    SharedTerminalTransportFactory,
};

const KIND_ARGUMENT: &str = "--admin-terminal=";
const DIRECTORY_ARGUMENT: &str = "--admin-directory=";
const CMDER_ARGUMENT: &str = "--admin-cmder=";
const SHELL_ARGUMENT: &str = "--admin-shell=";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminTerminalRequest {
    kind: LocalTerminalKind,
    directory: Option<PathBuf>,
    cmder: Option<PathBuf>,
    shell: Option<String>,
}

impl AdminTerminalRequest {
    pub fn parse_startup(args: &[OsString]) -> Result<Option<Self>, String> {
        if !args
            .iter()
            .any(|argument| argument.to_string_lossy().starts_with("--admin-"))
        {
            return Ok(None);
        }
        Self::from_args(args)
            .map(Some)
            .ok_or_else(|| t!("workspace.local_terminal.options.invalid_arguments").to_string())
    }

    pub fn validate_elevation(&self) -> Result<(), String> {
        if is_administrator() {
            Ok(())
        } else {
            Err(t!("workspace.local_terminal.options.elevation_required").into())
        }
    }

    pub fn kind(&self) -> LocalTerminalKind {
        self.kind
    }

    pub fn directory(&self) -> Option<&std::path::Path> {
        self.directory.as_deref()
    }

    pub fn shell(&self) -> Option<&str> {
        self.shell.as_deref()
    }

    pub(super) fn new(
        start: &LocalTerminalStart,
        settings: &AppSettings,
        cmder: Option<PathBuf>,
    ) -> Self {
        Self {
            kind: start.kind,
            directory: start
                .directory
                .clone()
                .or_else(|| settings.local_terminal.start_directory.clone())
                .or_else(dirs::home_dir),
            cmder,
            shell: settings.local_terminal.shell.clone(),
        }
    }

    pub fn from_args(args: &[OsString]) -> Option<Self> {
        if !cfg!(windows) {
            return None;
        }
        let mut kind = None;
        let mut directory = None;
        let mut cmder = None;
        let mut shell = None;
        for argument in args {
            let argument = argument.to_str()?;
            if let Some(value) = argument.strip_prefix(KIND_ARGUMENT) {
                if kind.is_some() {
                    return None;
                }
                kind = Some(match value {
                    "shell" => LocalTerminalKind::Shell,
                    "cmder" => LocalTerminalKind::Cmder,
                    _ => return None,
                });
            } else if let Some(value) = argument.strip_prefix(DIRECTORY_ARGUMENT) {
                if directory.is_some() {
                    return None;
                }
                directory = Some(PathBuf::from(decode_hex(value)?));
            } else if let Some(value) = argument.strip_prefix(CMDER_ARGUMENT) {
                if cmder.is_some() {
                    return None;
                }
                cmder = Some(PathBuf::from(decode_hex(value)?));
            } else {
                let value = argument.strip_prefix(SHELL_ARGUMENT)?;
                if shell.is_some() {
                    return None;
                }
                shell = Some(decode_hex(value)?);
            }
        }
        let kind = kind?;
        if directory.as_ref().is_some_and(|path| !path.is_absolute())
            || (kind == LocalTerminalKind::Cmder
                && cmder.as_ref().is_none_or(|path| !path.is_absolute()))
        {
            return None;
        }
        Some(Self {
            kind,
            directory,
            cmder,
            shell,
        })
    }

    #[cfg(any(windows, test))]
    fn arguments(&self) -> String {
        let kind = match self.kind {
            LocalTerminalKind::Shell => "shell",
            LocalTerminalKind::Cmder => "cmder",
        };
        let mut arguments = format!("{KIND_ARGUMENT}{kind}");
        if let Some(directory) = &self.directory {
            arguments.push(' ');
            arguments.push_str(DIRECTORY_ARGUMENT);
            arguments.push_str(&encode_hex(&directory.to_string_lossy()));
        }
        if let Some(cmder) = &self.cmder {
            arguments.push(' ');
            arguments.push_str(CMDER_ARGUMENT);
            arguments.push_str(&encode_hex(&cmder.to_string_lossy()));
        }
        if let Some(shell) = &self.shell {
            arguments.push(' ');
            arguments.push_str(SHELL_ARGUMENT);
            arguments.push_str(&encode_hex(shell));
        }
        arguments
    }
}

#[cfg(any(windows, test))]
fn encode_hex(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn decode_hex(value: &str) -> Option<String> {
    if !value.is_ascii() || !value.len().is_multiple_of(2) {
        return None;
    }
    let bytes = (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).ok())
        .collect::<Option<Vec<_>>>()?;
    let decoded = String::from_utf8(bytes).ok()?;
    (!decoded.contains('\0')).then_some(decoded)
}

pub trait AdministratorTerminalLauncher: Send + Sync {
    fn launch(&self, request: &AdminTerminalRequest) -> Result<bool, String>;
}

pub(super) struct SystemAdministratorTerminalLauncher;

impl AdministratorTerminalLauncher for SystemAdministratorTerminalLauncher {
    fn launch(&self, request: &AdminTerminalRequest) -> Result<bool, String> {
        let request = request.clone();
        std::thread::Builder::new()
            .name("shelllf-administrator".into())
            .spawn(move || launch_administrator_terminal(&request))
            .map_err(|error| error.to_string())?
            .join()
            .map_err(|_| "Administrator launcher thread failed".to_string())?
    }
}

fn launch_administrator_terminal(request: &AdminTerminalRequest) -> Result<bool, String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_CANCELLED};
        use windows_sys::Win32::System::Com::{
            COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
        };
        use windows_sys::Win32::UI::Shell::{
            SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
            ShellExecuteExW,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        struct Com;
        impl Drop for Com {
            fn drop(&mut self) {
                unsafe { CoUninitialize() };
            }
        }
        let initialized = unsafe {
            CoInitializeEx(
                std::ptr::null(),
                (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
            )
        };
        if initialized < 0 {
            return Err(format!("COM error: {initialized:#x}"));
        }
        let _com = Com;
        if let Some(directory) = &request.directory
            && (!directory.is_absolute() || !directory.is_dir())
        {
            return Err(t!("workspace.local_terminal.options.invalid_directory").into());
        }
        if request.kind == LocalTerminalKind::Cmder
            && request
                .cmder
                .as_ref()
                .is_none_or(|path| !path.join("vendor/init.bat").is_file())
        {
            return Err(t!("terminal.cmder.missing").into());
        }
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let wide = |value: &std::ffi::OsStr| -> Vec<u16> {
            value.encode_wide().chain(std::iter::once(0)).collect()
        };
        let operation = wide(std::ffi::OsStr::new("runas"));
        let file = wide(executable.as_os_str());
        let parameters = wide(std::ffi::OsStr::new(&request.arguments()));
        let directory = executable.parent().map(|path| wide(path.as_os_str()));
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
            lpVerb: operation.as_ptr(),
            lpFile: file.as_ptr(),
            lpParameters: parameters.as_ptr(),
            lpDirectory: directory
                .as_ref()
                .map_or(std::ptr::null(), |path| path.as_ptr()),
            nShow: SW_SHOWNORMAL,
            ..unsafe { std::mem::zeroed() }
        };
        if unsafe { ShellExecuteExW(&mut info) } == 0 {
            let error = std::io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_CANCELLED as i32) {
                Ok(false)
            } else {
                Err(error.to_string())
            };
        }
        if !info.hProcess.is_null() {
            unsafe { CloseHandle(info.hProcess) };
        }
        Ok(true)
    }
    #[cfg(not(windows))]
    {
        let _ = request;
        Err(t!("workspace.local_terminal.options.administrator_unsupported").into())
    }
}

pub fn is_administrator() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
            System::Threading::{GetCurrentProcess, OpenProcessToken},
        };
        let mut token = std::ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut length = 0;
        let result = unsafe {
            GetTokenInformation(
                token,
                TokenElevation,
                (&mut elevation as *mut TOKEN_ELEVATION).cast(),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut length,
            )
        };
        unsafe { CloseHandle(token) };
        result != 0 && elevation.TokenIsElevated != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub fn open_admin_terminal_window(
    request: AdminTerminalRequest,
    settings: &AppSettings,
    cx: &mut App,
) {
    let launch = launch_for(&request);
    let settings = settings.clone();
    let factory = LocalPtyTransportProvider.factory_for(&launch);
    let title: SharedString = match request.kind {
        LocalTerminalKind::Shell => t!("workspace.local_terminal.options.admin_window"),
        LocalTerminalKind::Cmder => t!("workspace.local_terminal.options.admin_cmder_window"),
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(120.), px(120.)),
            size(px(1000.), px(680.)),
        ))),
        window_min_size: Some(size(px(560.), px(360.))),
        app_id: Some(crate::app::DESKTOP_APP_ID.into()),
        ..TitleBar::window_options()
    };
    cx.spawn(async move |cx| {
        let opened = cx.open_window(options, move |window, cx| {
            window.set_window_title(&title);
            window.on_window_should_close(cx, |_, cx| {
                cx.quit();
                true
            });
            crate::settings::apply(settings.clone(), window, cx);
            let panel = cx.new(|cx| {
                LocalTerminalPanel::new(
                    LocalTerminalId(1),
                    factory.clone(),
                    Some(SavedName {
                        name: title.clone(),
                        fixed: true,
                        title: None,
                    }),
                    window,
                    cx,
                )
            });
            panel.update(cx, |panel, cx| panel.set_active(true, window, cx));
            let focus = panel.read(cx).terminal().read(cx).focus_handle();
            window.focus(&focus, cx);
            let view = cx.new(|_| AdminTerminalWindow {
                panel,
                title: title.clone(),
                factory,
            });
            cx.new(|cx| Root::new(view, window, cx))
        });
        if let Err(error) = opened {
            tracing::error!(%error, "Cannot open the administrator terminal");
            cx.update(|cx| cx.quit());
        }
    })
    .detach();
}

fn launch_for(request: &AdminTerminalRequest) -> LocalLaunch {
    let shell = match request.kind {
        LocalTerminalKind::Shell => LocalShell::new(request.shell.as_deref().unwrap_or_default()),
        LocalTerminalKind::Cmder => LocalShell::cmder(request.cmder.clone().unwrap_or_default()),
    };
    let mut launch = LocalLaunch::new(shell);
    launch.directory = request.directory.clone();
    launch
}

struct AdminTerminalWindow {
    panel: Entity<LocalTerminalPanel>,
    title: SharedString,
    factory: SharedTerminalTransportFactory,
}

impl Render for AdminTerminalWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("Workspace")
            .on_action(cx.listener(|this, _: &RestartLocalTerminal, window, cx| {
                this.panel.update(cx, |panel, cx| {
                    panel.restart(this.factory.clone(), window, cx)
                });
            }))
            .on_action(cx.listener(|_, _: &CloseLocalTerminal, _, cx| cx.quit()))
            .on_action(cx.listener(|_, _: &CloseActiveTab, _, cx| cx.quit()))
            .on_action(cx.listener(|this, _: &CopyTerminal, _, cx| {
                let terminal = this.panel.read(cx).terminal().clone();
                terminal.update(cx, |terminal, cx| terminal.copy_selection(cx));
            }))
            .on_action(cx.listener(|this, _: &PasteTerminal, _, cx| {
                let terminal = this.panel.read(cx).terminal().clone();
                terminal.update(cx, |terminal, cx| terminal.paste_clipboard(cx));
            }))
            .on_action(cx.listener(|this, _: &SelectAllTerminal, _, cx| {
                let terminal = this.panel.read(cx).terminal().clone();
                terminal.update(cx, |terminal, cx| terminal.select_all(cx));
            }))
            .on_action(cx.listener(|this, _: &ClearTerminal, _, cx| {
                let terminal = this.panel.read(cx).terminal().clone();
                terminal.update(cx, |terminal, cx| terminal.clear(cx));
            }))
            .on_action(cx.listener(|this, _: &FindInTerminal, window, cx| {
                let terminal = this.panel.read(cx).terminal().clone();
                terminal.update(cx, |terminal, cx| terminal.open_find(window, cx));
            }))
            .on_action(cx.listener(|this, _: &FindNextInTerminal, _, cx| {
                let terminal = this.panel.read(cx).terminal().clone();
                terminal.update(cx, |terminal, cx| {
                    terminal.step_find(crate::terminal::SearchDirection::Down, cx)
                });
            }))
            .on_action(cx.listener(|this, _: &FindPreviousInTerminal, _, cx| {
                let terminal = this.panel.read(cx).terminal().clone();
                terminal.update(cx, |terminal, cx| {
                    terminal.step_find(crate::terminal::SearchDirection::Up, cx)
                });
            }))
            .on_action(cx.listener(|this, _: &DismissTerminalFind, window, cx| {
                let terminal = this.panel.read(cx).terminal().clone();
                terminal.update(cx, |terminal, cx| terminal.dismiss_find(window, cx));
            }))
            .on_action(cx.listener(|this, _: &CopyTerminalFindResults, _, cx| {
                let terminal = this.panel.read(cx).terminal().clone();
                terminal.update(cx, |terminal, cx| terminal.copy_find_results(cx));
            }))
            .on_action(
                cx.listener(|this, action: &ToggleTerminalFindOption, window, cx| {
                    let terminal = this.panel.read(cx).terminal().clone();
                    terminal.update(cx, |terminal, cx| {
                        terminal.toggle_find_option(action.0, window, cx)
                    });
                }),
            )
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                TitleBar::new().child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(self.title.clone()),
                ),
            )
            .child(div().flex_1().min_h_0().child(self.panel.clone()))
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::{AdminTerminalRequest, LocalTerminalKind, decode_hex, launch_for};
    use crate::{settings::AppSettings, terminal::LocalTerminalStart};
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn administrator_arguments_round_trip_paths_with_spaces() {
        let request = AdminTerminalRequest {
            kind: LocalTerminalKind::Cmder,
            directory: Some(PathBuf::from(r"D:\工作目录\project with spaces")),
            cmder: Some(PathBuf::from(r"C:\Program Files\Cmder")),
            shell: Some(r#""C:\Program Files\PowerShell\7\pwsh.exe" -NoLogo"#.into()),
        };
        let args = request
            .arguments()
            .split_whitespace()
            .map(OsString::from)
            .collect::<Vec<_>>();
        assert_eq!(
            AdminTerminalRequest::from_args(&args),
            cfg!(windows).then_some(request)
        );
        assert!(
            AdminTerminalRequest::from_args(&[OsString::from("--admin-terminal=other")]).is_none()
        );
    }

    #[test]
    fn administrator_launch_inherits_the_configured_shell_and_start_folder() {
        let mut settings = AppSettings::default();
        settings.local_terminal.shell = Some("cmd.exe /D".into());
        settings.local_terminal.start_directory = Some(PathBuf::from("/work/default"));
        let request = AdminTerminalRequest::new(
            &LocalTerminalStart::new(LocalTerminalKind::Shell),
            &settings,
            None,
        );
        let launch = launch_for(&request);
        assert_eq!(launch.shell.command(), Some("cmd.exe /D"));
        assert_eq!(launch.directory, settings.local_terminal.start_directory);
        let request = AdminTerminalRequest::new(
            &LocalTerminalStart::new(LocalTerminalKind::Shell)
                .with_directory("/work/selected".into()),
            &settings,
            None,
        );
        assert_eq!(
            launch_for(&request).directory,
            Some(PathBuf::from("/work/selected"))
        );
    }

    #[test]
    fn malformed_administrator_arguments_are_rejected_without_panicking() {
        for invalid in ["é1", "abc", "gg", "00"] {
            assert!(decode_hex(invalid).is_none());
            let args = [
                OsString::from("--admin-terminal=shell"),
                OsString::from(format!("--admin-directory={invalid}")),
            ];
            assert!(AdminTerminalRequest::from_args(&args).is_none());
        }
        assert!(
            AdminTerminalRequest::from_args(&[
                OsString::from("--admin-terminal=shell"),
                OsString::from("--admin-terminal=cmder"),
            ])
            .is_none()
        );
    }

    #[test]
    fn invalid_administrator_requests_never_fall_back_to_normal_startup() {
        assert_eq!(AdminTerminalRequest::parse_startup(&[]), Ok(None));
        assert_eq!(
            AdminTerminalRequest::parse_startup(&["hosts".into(), "list".into()]),
            Ok(None)
        );
        for args in [
            vec!["--admin-terminal=unknown"],
            vec!["--admin-directory=00"],
            vec!["--admin-terminal=shell", "--admin-directory="],
            vec!["--admin-terminal=shell", "--admin-directory=616263"],
            vec!["--admin-terminal=cmder"],
            vec!["--admin-terminal=shell", "unexpected"],
        ] {
            let args = args.into_iter().map(OsString::from).collect::<Vec<_>>();
            assert!(
                AdminTerminalRequest::parse_startup(&args).is_err(),
                "{args:?}"
            );
        }
    }
}
