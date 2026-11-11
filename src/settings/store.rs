use std::{
    fs, io,
    path::{Path, PathBuf},
};

use gpui_kit::*;

use super::AppSettings;
use crate::i18n::t;

#[derive(Clone, Debug)]
pub enum SettingsStoreEvent {
    /// A change stayed in memory but could not be written.
    PersistFailed(SharedString),
}

/// Why the settings file is not in use. Told once the window is open, by
/// when the settings have chosen the interface language to tell it in.
#[derive(Clone, Debug)]
pub enum SettingsProblem {
    /// The file cannot be read: this run has the defaults.
    Unreadable(String),
    /// There is no data directory to keep the file in: changes last only
    /// this run.
    Unsaved(String),
}

impl SettingsProblem {
    pub fn message(&self) -> SharedString {
        match self {
            Self::Unreadable(error) => t!("settings.store.unreadable", error = error),
            Self::Unsaved(error) => t!("settings.store.unsaved", error = error),
        }
    }
}

/// The settings, shared by the settings page and the workspace that applies
/// them. Like `HostStore`, memory is the source of truth and every change
/// is written through to disk at once; a failed write keeps the change and
/// says so.
pub struct SettingsStore {
    settings: AppSettings,
    /// `None` keeps everything in memory.
    path: Option<PathBuf>,
}

impl EventEmitter<SettingsStoreEvent> for SettingsStore {}

impl SettingsStore {
    /// Settings that are never written anywhere: for UI tests, and for a
    /// machine without a data directory.
    pub fn in_memory() -> Self {
        Self {
            settings: AppSettings::default(),
            path: None,
        }
    }

    /// Read the settings saved at `path`. A missing file means the defaults.
    /// An unreadable one also gives the defaults, together with the problem
    /// to tell; the file stays as it is until the next change replaces it.
    pub fn load(path: PathBuf) -> (Self, Option<SettingsProblem>) {
        let loaded = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(AppSettings::default()),
            Err(error) => Err(error.to_string()),
        };
        let (settings, problem) = match loaded {
            Ok(settings) => (AppSettings::normalized(settings), None),
            Err(error) => {
                tracing::warn!(%error, "Cannot read application settings; using defaults");
                (
                    AppSettings::default(),
                    Some(SettingsProblem::Unreadable(error)),
                )
            }
        };
        (
            Self {
                settings,
                path: Some(path),
            },
            problem,
        )
    }

    pub fn settings(&self) -> AppSettings {
        self.settings.clone()
    }

    /// Change the settings, write them and tell observers.
    pub fn update(&mut self, change: impl FnOnce(&mut AppSettings), cx: &mut Context<Self>) {
        let Some(written) = self.update_unnotified(change) else {
            return;
        };
        cx.notify();
        if let Err(error) = written {
            tracing::error!(%error, "Cannot persist application settings");
            cx.emit(SettingsStoreEvent::PersistFailed(t!(
                "settings.store.save_failed",
                error = error
            )));
        }
    }

    /// The in-memory half of `update`: `None` when the change changed
    /// nothing, otherwise how writing it went. Memory keeps the change
    /// either way.
    pub fn update_unnotified(
        &mut self,
        change: impl FnOnce(&mut AppSettings),
    ) -> Option<io::Result<()>> {
        let before = self.settings.clone();
        change(&mut self.settings);
        self.settings = self.settings.clone().normalized();
        if self.settings == before {
            return None;
        }
        Some(match &self.path {
            Some(path) => write(path, &self.settings),
            None => Ok(()),
        })
    }
}

/// Replace the file in one step, so a crash mid-write cannot leave half a
/// file behind.
fn write(path: &Path, settings: &AppSettings) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(settings).map_err(io::Error::other)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, json)?;
    fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    // Not `super::*`: it brings in `gpui_kit::*`, whose `test` shadows `#[test]`.
    use std::{fs, path::PathBuf};

    use super::{SettingsProblem, SettingsStore};
    use crate::settings::{AppSettings, Appearance, InterfaceLanguage};

    #[test]
    fn remote_server_accounts_persist_without_secrets() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let server = crate::remote::Server::new(
            "Customer".into(),
            "https://example.test:8180".into(),
            "operator".into(),
        );
        let (mut store, _) = SettingsStore::load(path.clone());
        store
            .update_unnotified(|settings| settings.remote_servers.push(server.clone()))
            .unwrap()
            .unwrap();
        let (reloaded, problem) = SettingsStore::load(path.clone());
        assert!(problem.is_none());
        assert_eq!(reloaded.settings().remote_servers, vec![server]);
        let json = fs::read_to_string(path).unwrap();
        let data: serde_json::Value = serde_json::from_str(&json).unwrap();
        let remote = &data["remote_servers"][0];
        assert!(remote.get("password").is_none() && remote.get("token").is_none());
    }

    #[test]
    fn sidebar_download_directory_is_optional_persistent_and_rejects_relative_paths() {
        let defaults: AppSettings = serde_json::from_str("{}").unwrap();
        assert!(defaults.sidebar_download_directory.is_none());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let target = directory.path().join("downloads");
        let (mut store, _) = SettingsStore::load(path.clone());
        store
            .update_unnotified(|settings| {
                settings.sidebar_download_directory = Some(target.clone());
            })
            .unwrap()
            .unwrap();
        let (reloaded, problem) = SettingsStore::load(path.clone());
        assert!(problem.is_none());
        assert_eq!(reloaded.settings().sidebar_download_directory, Some(target));
        fs::write(
            path.clone(),
            r#"{"sidebar_download_directory":"relative/folder"}"#,
        )
        .unwrap();
        let (reloaded, problem) = SettingsStore::load(path);
        assert!(problem.is_none());
        assert!(reloaded.settings().sidebar_download_directory.is_none());
    }

    #[test]
    fn device_log_font_defaults_are_bounded_and_survive_reloading() {
        let defaults: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(defaults.device_logs.font_size, 14);
        for (written, kept) in [(0, 12), (18, 18), (255, 24)] {
            let settings: AppSettings =
                serde_json::from_str(&format!(r#"{{"device_logs":{{"font_size":{written}}}}}"#))
                    .unwrap();
            assert_eq!(settings.normalized().device_logs.font_size, kept);
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let (mut store, _) = SettingsStore::load(path.clone());
        store
            .update_unnotified(|settings| settings.device_logs.font_size = 20)
            .unwrap()
            .unwrap();
        let (reloaded, error) = SettingsStore::load(path.clone());
        assert!(error.is_none());
        assert_eq!(reloaded.settings().device_logs.font_size, 20);
        assert!(!fs::read_to_string(path).unwrap().contains("log_split"));
    }

    #[test]
    fn a_first_start_has_the_defaults_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let (mut store, problem) = SettingsStore::load(path.clone());
        assert!(problem.is_none());
        assert_eq!(store.settings(), AppSettings::default());

        // Choosing what is already chosen is not a change.
        let default = AppSettings::default().appearance;
        assert!(
            store
                .update_unnotified(|settings| settings.appearance = default)
                .is_none()
        );
        assert!(!path.exists());
    }

    #[test]
    fn a_change_is_written_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        // The data directory may not exist yet.
        let path = dir.path().join("shellrs").join("settings.json");
        let (mut store, _) = SettingsStore::load(path.clone());
        let written = store.update_unnotified(|settings| {
            settings.appearance = Appearance::Dark;
            settings.app_lock_idle_minutes = 15;
            settings.app_lock_allow_cli = true;
            settings.window.close_to_tray = true;
            settings.sidebar_width = Some(196.);
            settings.language = InterfaceLanguage::English;
            settings.terminal_interaction.cursor_style = crate::terminal::TerminalCursorStyle::Beam;
            settings.terminal_interaction.cursor_blinking = false;
            settings.terminal_interaction.hide_ai_citations = true;
            settings.notifications.tab_output = false;
            settings.local_terminal.cmder_directory = Some(PathBuf::from("D:/Cmder installation"));
            settings.terminal_interaction.right_click = crate::terminal::RightClick::Enter;
            settings.terminal_interaction.middle_click = crate::terminal::MiddleClick::Paste;
            settings.snippet_shortcuts.set(
                "stable-snippet-id".into(),
                Some(crate::app::SnippetShortcut::new("ctrl-1".into(), true)),
            );
            settings
                .sidebar_tools
                .set(crate::app::ToolKind::Kubernetes, true);
        });
        assert!(matches!(written, Some(Ok(()))));

        let (reloaded, problem) = SettingsStore::load(path);
        assert!(problem.is_none());
        assert_eq!(reloaded.settings().appearance, Appearance::Dark);
        assert_eq!(reloaded.settings().app_lock_idle_minutes, 15);
        assert!(reloaded.settings().app_lock_allow_cli);
        assert!(reloaded.settings().window.close_to_tray);
        assert_eq!(reloaded.settings().sidebar_width, Some(196.));
        assert_eq!(reloaded.settings().language, InterfaceLanguage::English);
        assert_eq!(
            reloaded.settings().terminal_interaction.cursor_style,
            crate::terminal::TerminalCursorStyle::Beam
        );
        assert!(!reloaded.settings().terminal_interaction.cursor_blinking);
        assert!(reloaded.settings().terminal_interaction.hide_ai_citations);
        assert!(!reloaded.settings().notifications.tab_output);
        assert_eq!(
            reloaded.settings().local_terminal.cmder_directory,
            Some(PathBuf::from("D:/Cmder installation"))
        );
        assert_eq!(
            reloaded.settings().terminal_interaction.right_click,
            crate::terminal::RightClick::Enter
        );
        assert_eq!(
            reloaded.settings().terminal_interaction.middle_click,
            crate::terminal::MiddleClick::Paste
        );
        let shortcut = reloaded
            .settings()
            .snippet_shortcuts
            .get("stable-snippet-id")
            .cloned()
            .unwrap();
        assert_eq!(shortcut.keys(), "ctrl-1");
        assert!(shortcut.runs());
        assert!(
            reloaded
                .settings()
                .sidebar_tools
                .shows(crate::app::ToolKind::Kubernetes)
        );
    }

    #[test]
    fn imported_xshell_palette_and_selection_survive_a_settings_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let (mut store, _) = SettingsStore::load(path.clone());
        let theme = crate::terminal::import_xshell_schemes(include_bytes!(
            "../../tests/fixtures/xshell/3024 Day.xcs"
        ))
        .unwrap()
        .remove(0);
        let saved = store.update_unnotified(|settings| {
            settings.terminal_theme.import(vec![theme.clone()]);
            settings.terminal_theme.choose(&theme);
        });
        assert!(matches!(saved, Some(Ok(()))));
        let (loaded, problem) = SettingsStore::load(path);
        assert!(problem.is_none());
        assert_eq!(
            loaded
                .settings()
                .terminal_theme
                .theme(gpui_kit::component::ThemeMode::Light),
            &theme
        );
    }

    #[test]
    fn kubernetes_is_hidden_until_turned_on() {
        for json in [r#"{}"#, r#"{"sidebar_tools":{}}"#] {
            let settings: AppSettings = serde_json::from_str(json).unwrap();
            assert!(
                !settings
                    .sidebar_tools
                    .shows(crate::app::ToolKind::Kubernetes)
            );
        }
    }

    #[test]
    fn serial_preferences_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let (mut store, _) = SettingsStore::load(path.clone());
        store
            .update_unnotified(|settings| {
                let modes = crate::terminal::SerialDataModes {
                    receive_hex: true,
                    ..Default::default()
                };
                settings
                    .serial_preferences
                    .set("serial-host-1".into(), modes);
            })
            .unwrap()
            .unwrap();
        let (reloaded, problem) = SettingsStore::load(path);
        assert!(problem.is_none());
        assert_eq!(
            reloaded.settings().serial_preferences,
            store.settings().serial_preferences
        );
        assert!(
            reloaded
                .settings()
                .serial_preferences
                .get("serial-host-1")
                .receive_hex
        );
        assert_eq!(
            reloaded.settings().serial_preferences.get("serial-host-2"),
            crate::terminal::SerialDataModes::default()
        );
    }

    #[test]
    fn an_unreadable_file_falls_back_to_the_defaults_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{ not json").unwrap();
        let (store, problem) = SettingsStore::load(path.clone());
        assert_eq!(store.settings(), AppSettings::default());
        assert!(matches!(problem, Some(SettingsProblem::Unreadable(_))));
        assert!(problem.unwrap().message().starts_with("无法读取设置"));
        // Left alone until the user changes something.
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
    }

    #[test]
    fn a_failed_write_keeps_the_change_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file should be cannot be replaced by it.
        let path = dir.path().join("settings.json");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), "").unwrap();
        let mut store = SettingsStore {
            settings: AppSettings::default(),
            path: Some(path),
        };
        let written = store.update_unnotified(|settings| settings.appearance = Appearance::Light);
        assert!(matches!(written, Some(Err(_))));
        assert_eq!(store.settings().appearance, Appearance::Light);
    }

    #[test]
    fn in_memory_settings_touch_no_file() {
        let mut store = SettingsStore::in_memory();
        let written = store.update_unnotified(|settings| settings.appearance = Appearance::Dark);
        assert!(matches!(written, Some(Ok(()))));
        assert_eq!(store.settings().appearance, Appearance::Dark);
    }

    #[test]
    fn sidebar_width_rejects_invalid_numbers_but_keeps_other_settings() {
        for width in [0., -1., f32::NAN, f32::INFINITY] {
            let mut store = SettingsStore::in_memory();
            store.update_unnotified(|settings| {
                settings.sidebar_width = Some(width);
                settings.appearance = Appearance::Dark;
            });
            assert_eq!(store.settings().sidebar_width, None);
            assert_eq!(store.settings().appearance, Appearance::Dark);
        }
        let settings: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.sidebar_width, None);
    }
}
