use std::path::{Path, PathBuf};

use gpui_kit::{AppContext as _, Context, Task};

pub struct Cmder {
    configured: Option<PathBuf>,
    directory: Option<PathBuf>,
    scan: Option<Task<()>>,
}

impl Cmder {
    pub fn new(configured: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            configured,
            directory: None,
            scan: None,
        };
        this.scan(cx);
        this
    }

    pub fn directory(&self) -> Option<&Path> {
        self.directory.as_deref()
    }

    pub fn set_directory(&mut self, configured: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.configured == configured {
            return;
        }
        self.configured = configured;
        self.directory = None;
        self.scan(cx);
        cx.notify();
    }

    fn scan(&mut self, cx: &mut Context<Self>) {
        let configured = self.configured.clone();
        self.scan = Some(cx.spawn(async move |this, cx| {
            let directory = cx
                .background_spawn(async move { detect(configured.as_deref()) })
                .await;
            this.update(cx, |this, cx| {
                this.directory = directory;
                cx.notify();
            })
            .ok();
        }));
    }
}

fn installation(path: &Path) -> Option<PathBuf> {
    let directory = if path
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("Cmder.exe"))
    {
        path.parent()?
    } else {
        path
    };
    (directory.is_absolute() && directory.join("vendor/init.bat").is_file())
        .then(|| dunce::canonicalize(directory).ok())
        .flatten()
}

fn detect(configured: Option<&Path>) -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    if let Some(path) = configured {
        return installation(path);
    }
    let mut candidates = Vec::new();
    if let Some(root) = std::env::var_os("CMDER_ROOT") {
        candidates.push(root.into());
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for path in std::env::split_paths(&paths) {
            candidates.push(path.clone());
            if let Some(parent) = path.parent() {
                candidates.push(parent.into());
            }
        }
    }
    for variable in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(root) = std::env::var_os(variable) {
            candidates.push(PathBuf::from(&root).join("Cmder"));
            candidates.push(PathBuf::from(root).join("Programs/Cmder"));
        }
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("scoop/apps/cmder/current"));
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(parent) = executable.parent()
    {
        candidates.push(parent.join("cmder"));
    }
    for drive in b'C'..=b'Z' {
        candidates.push(PathBuf::from(format!("{}:\\cmder", char::from(drive))));
    }
    candidates.into_iter().find_map(|path| installation(&path))
}

#[cfg(test)]
mod tests {
    use super::{detect, installation};

    #[test]
    fn explicit_directory_and_launcher_resolve_without_running_the_gui() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("Cmder installation");
        std::fs::create_dir_all(root.join("vendor")).unwrap();
        std::fs::write(root.join("vendor/init.bat"), b"@echo off").unwrap();
        let expected = dunce::canonicalize(&root).unwrap();
        assert_eq!(installation(&root), Some(expected.clone()));
        assert_eq!(
            installation(&root.join("Cmder.exe")),
            Some(expected.clone())
        );
        assert_eq!(detect(Some(&root)), cfg!(windows).then_some(expected));
        assert!(detect(Some(&temporary.path().join("missing"))).is_none());
        assert!(installation(std::path::Path::new("cmder")).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn cmder_starts_inside_a_pty_and_preserves_large_output_in_order() {
        use crate::terminal::{
            LocalLaunch, LocalPtyTransportProvider, LocalShell,
            LocalTerminalTransportProvider as _, TerminalSize, TerminalTransportCommand,
            TerminalTransportEvent,
        };
        use std::{
            sync::mpsc,
            time::{Duration, Instant},
        };
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("Cmder path with spaces");
        std::fs::create_dir_all(root.join("vendor")).unwrap();
        std::fs::write(root.join("vendor/init.bat"), "@echo off\r\nchcp 65001>nul\r\nprompt __CMDER_PROMPT__$G\r\nfor /L %%G in (1,1,1500) do @echo 中文_CM_ROW_%%G_END\r\necho __CMDER_OUTPUT_DONE__\r\n").unwrap();
        let launch = LocalLaunch::new(LocalShell::cmder(root));
        let transport = LocalPtyTransportProvider.factory_for(&launch).create();
        let (commands, receiver) = mpsc::channel();
        let (sender, events) = async_channel::unbounded();
        let worker =
            std::thread::spawn(move || transport.run(TerminalSize::DEFAULT, receiver, sender));
        let started = Instant::now();
        let mut output = Vec::new();
        let mut done = false;
        while started.elapsed() < Duration::from_secs(15) {
            match events.try_recv() {
                Ok(TerminalTransportEvent::Output(bytes)) => {
                    if bytes.windows(4).any(|part| part == b"\x1b[6n") {
                        commands
                            .send(TerminalTransportCommand::WriteData(b"\x1b[1;1R".to_vec()))
                            .unwrap();
                    }
                    output.extend(bytes);
                    if String::from_utf8_lossy(&output).contains("__CMDER_OUTPUT_DONE__") {
                        done = true;
                        break;
                    }
                }
                Ok(TerminalTransportEvent::Failed(error)) => panic!("{error}"),
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        commands
            .send(TerminalTransportCommand::WriteData(b"exit\r".to_vec()))
            .unwrap();
        let exit_started = Instant::now();
        while !worker.is_finished() && exit_started.elapsed() < Duration::from_secs(5) {
            if let Ok(TerminalTransportEvent::Output(bytes)) = events.try_recv()
                && bytes.windows(4).any(|part| part == b"\x1b[6n")
            {
                commands
                    .send(TerminalTransportCommand::WriteData(b"\x1b[1;1R".to_vec()))
                    .ok();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !worker.is_finished() {
            commands.send(TerminalTransportCommand::Shutdown).ok();
        }
        assert!(
            worker.is_finished(),
            "Cmder test shell did not exit; output: {}",
            String::from_utf8_lossy(&output)
        );
        worker.join().unwrap().unwrap();
        let output = String::from_utf8(output).expect("ConPTY output is UTF-8");
        assert!(done, "Cmder initialization did not finish: {output}");
        let mut end = 0;
        for number in 1..=1500 {
            let expected = format!("中文_CM_ROW_{number}_END");
            let position = output[end..]
                .find(&expected)
                .unwrap_or_else(|| panic!("missing {expected}"));
            end += position + expected.len();
        }
    }
}
