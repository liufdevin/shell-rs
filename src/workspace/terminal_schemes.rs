use std::io::Read as _;

use anyhow::{Context as _, Result, ensure};
use gpui_kit::component::{WindowExt as _, notification::Notification};
use gpui_kit::*;

use crate::{
    app::{ImportTerminalSchemes, RemoveTerminalScheme},
    host::{ImportLoad, ImportScan, read_mobaxterm_configuration},
    i18n::t,
    terminal::{TerminalTheme, import_xshell_schemes},
};

use super::Workspace;

impl Workspace {
    pub(super) fn on_import_terminal_schemes(
        &mut self,
        _: &ImportTerminalSchemes,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(t!("settings.theme.import")),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            if paths.is_empty() {
                return;
            }
            let result = cx
                .background_spawn(async move {
                    ensure!(
                        paths.len() <= 32,
                        "Select at most 32 configuration files at once"
                    );
                    let mut themes = Vec::new();
                    let mut moba = None;
                    let mut total = 0u64;
                    for path in paths {
                        total += std::fs::metadata(&path)?.len();
                        ensure!(total <= 32 * 1024 * 1024, "Selected files exceed 32 MiB");
                        let extension = path
                            .extension()
                            .and_then(|extension| extension.to_str())
                            .unwrap_or_default();
                        if !extension.eq_ignore_ascii_case("xcs") {
                            ensure!(
                                ["mobaconf", "ini", "mxtsessions", "moba"]
                                    .iter()
                                    .any(|allowed| extension.eq_ignore_ascii_case(allowed)),
                                "Unsupported color scheme file"
                            );
                            match read_mobaxterm_configuration(&path) {
                                ImportLoad::Loaded(scan) => {
                                    moba.get_or_insert_with(ImportScan::default).merge(scan)
                                }
                                ImportLoad::Failed(error) => anyhow::bail!("{error}"),
                                ImportLoad::Missing => {
                                    anyhow::bail!("Configuration file not found")
                                }
                            }
                            continue;
                        }
                        let load = || -> Result<Vec<TerminalTheme>> {
                            let mut bytes = Vec::new();
                            std::fs::File::open(&path)?
                                .take(1024 * 1024 + 1)
                                .read_to_end(&mut bytes)?;
                            import_xshell_schemes(&bytes)
                        };
                        themes.extend(load().with_context(|| path.display().to_string())?);
                    }
                    Ok::<_, anyhow::Error>((themes, moba))
                })
                .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok((themes, Some(mut scan))) => {
                    scan.add_schemes(themes);
                    this.preview_mobaxterm(scan, window, cx);
                }
                Ok((themes, None)) => {
                    let count = themes.len();
                    this.settings().update(cx, |store, cx| {
                        store.update(|settings| settings.terminal_theme.import(themes), cx)
                    });
                    window.push_notification(
                        Notification::success(t!("settings.theme.imported", count = count)),
                        cx,
                    );
                }
                Err(error) => window.push_notification(
                    Notification::error(t!(
                        "settings.theme.import_failed",
                        error = format!("{error:#}")
                    )),
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn on_remove_terminal_scheme(
        &mut self,
        action: &RemoveTerminalScheme,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings().update(cx, |store, cx| {
            store.update(
                |settings| settings.terminal_theme.remove_imported(&action.0),
                cx,
            )
        });
    }
}
