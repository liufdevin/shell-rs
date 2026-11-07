use std::{
    io::Write as _,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use gpui_kit::{
    component::{
        ActiveTheme as _, Disableable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        checkbox::Checkbox,
        dialog::{DialogAction, DialogClose, DialogFooter},
        v_flex,
    },
    prelude::FluentBuilder as _,
    *,
};
use serde::Serialize;

use super::{
    DeviceLogCommand,
    commands::{self, Privilege, Source},
};
use crate::{app::DeviceLogAction, i18n::t};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportOptions {
    pub(super) system: bool,
    pub(super) resources: bool,
    pub(super) logs: bool,
    pub(super) services: bool,
    pub(super) screenshot: bool,
    pub(super) bugreport: bool,
}

impl Default for ReportOptions {
    fn default() -> Self {
        Self {
            system: true,
            resources: true,
            logs: true,
            services: true,
            screenshot: false,
            bugreport: false,
        }
    }
}

impl ReportOptions {
    fn selected(&self) -> bool {
        self.system
            || self.resources
            || self.logs
            || self.services
            || self.screenshot
            || self.bugreport
    }
}

pub(super) fn open_report(
    android: bool,
    name: SharedString,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|_| ReportForm {
        android,
        options: ReportOptions::default(),
    });
    window.open_dialog(cx, move |dialog, window, cx| {
        let dispatch = dispatch.clone();
        let selected = form.read(cx).options.selected();
        let view = form.clone();
        dialog
            .title(t!("device_logs.report.dialog_title"))
            .w(rems(28.).to_pixels(window.rem_size()))
            .child(div().text_sm().child(name.clone()))
            .child(form.clone())
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|button| button.label(t!("common.cancel"))))
                    .child(
                        DialogAction::new().child(
                            Button::new("diagnostic-collect")
                                .label(t!("device_logs.report.collect"))
                                .primary()
                                .disabled(!selected),
                        ),
                    ),
            )
            .on_ok(move |_, window, cx| {
                let options = view.read(cx).options.clone();
                if !options.selected() {
                    return false;
                }
                let dispatch = dispatch.clone();
                window.defer(cx, move |window, cx| {
                    dispatch.dispatch_action(
                        &DeviceLogAction(DeviceLogCommand::CollectReport(options)),
                        window,
                        cx,
                    );
                });
                true
            })
    });
}

struct ReportForm {
    android: bool,
    options: ReportOptions,
}

impl Render for ReportForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let row = |id: &'static str,
                   label: SharedString,
                   checked: bool,
                   change: fn(&mut ReportOptions) -> &mut bool| {
            Checkbox::new(id)
                .label(label)
                .checked(checked)
                .on_change(cx.listener(move |this, value, _, cx| {
                    *change(&mut this.options) = *value;
                    cx.notify();
                }))
        };
        v_flex()
            .gap_3()
            .child(row(
                "diagnostic-system",
                t!("device_logs.report.system"),
                self.options.system,
                |options| &mut options.system,
            ))
            .child(row(
                "diagnostic-resources",
                t!("device_logs.report.resources"),
                self.options.resources,
                |options| &mut options.resources,
            ))
            .child(row(
                "diagnostic-logs",
                t!("device_logs.report.logs"),
                self.options.logs,
                |options| &mut options.logs,
            ))
            .child(row(
                "diagnostic-services",
                if self.android {
                    t!("device_logs.report.apps")
                } else {
                    t!("device_logs.report.services")
                },
                self.options.services,
                |options| &mut options.services,
            ))
            .when(self.android, |view| {
                view.child(row(
                    "diagnostic-screenshot",
                    t!("device_logs.report.screenshot"),
                    self.options.screenshot,
                    |options| &mut options.screenshot,
                ))
                .child(row(
                    "diagnostic-bugreport",
                    t!("device_logs.report.bugreport"),
                    self.options.bugreport,
                    |options| &mut options.bugreport,
                ))
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("device_logs.report.note")),
            )
            .when(self.android && self.options.bugreport, |view| {
                view.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("device_logs.report.bugreport_note")),
                )
            })
    }
}

pub(super) struct ReportItem {
    pub name: &'static str,
    pub command: String,
    pub screenshot: bool,
}

pub(super) fn items(
    source: Source,
    privilege: Privilege,
    options: &ReportOptions,
) -> Vec<ReportItem> {
    let android = source == Source::Android;
    let mut items = Vec::new();
    let mut add = |name, command: String| {
        items.push(ReportItem {
            name,
            command,
            screenshot: false,
        })
    };
    if options.system {
        add(
            "system.txt",
            if android {
                "date -Is; id; uname -a; for property in ro.product.manufacturer ro.product.model ro.product.cpu.abi ro.build.version.release ro.build.version.sdk ro.build.fingerprint; do printf '%s=' \"$property\"; getprop \"$property\"; done".into()
            } else {
                "date -Is; id; hostname; uname -a; cat /etc/os-release".into()
            },
        );
    }
    if options.resources {
        add("load.txt", "uptime; cat /proc/loadavg".into());
        add("memory.txt", "cat /proc/meminfo".into());
        add("disks.txt", "df -h".into());
        add("inodes.txt", "df -i".into());
        add(
            "processes.txt",
            if android {
                "ps -A".into()
            } else {
                "ps -eo pid,comm,pcpu,pmem --sort=-pcpu".into()
            },
        );
        if android {
            add("android-memory.txt", "dumpsys meminfo".into());
        }
    }
    if options.logs {
        add(
            "kernel-or-logcat.log",
            commands::query_command(
                if android {
                    Source::Android
                } else {
                    Source::Kernel
                },
                privilege,
                &super::LogOptions::default(),
            ),
        );
        if !android {
            add(
                "system-journal.log",
                commands::query_command(Source::Journal, privilege, &super::LogOptions::default()),
            );
        }
    }
    if options.services {
        add(
            if android {
                "android-apps.txt"
            } else {
                "services.txt"
            },
            if android {
                "pm list packages -f; dumpsys activity activities".into()
            } else {
                "systemctl --failed --no-pager --plain; systemctl list-units --type=service --state=running --no-pager --plain".into()
            },
        );
    }
    if android && options.screenshot {
        items.push(ReportItem {
            name: "screenshot.png",
            command:
                "image=$(screencap -p | base64); test -n \"$image\" && printf '%s\\n' \"$image\""
                    .into(),
            screenshot: true,
        });
    }
    items
}

pub(super) fn bounded(command: &str, limit: usize) -> String {
    let command = commands::quote(command);
    format!(
        "if command -v timeout >/dev/null 2>&1; then output=$(timeout 8 sh -c {command} 2>&1); else output=$(sh -c {command} 2>&1); fi; code=$?; printf '__SHELLLF_REPORT_EXIT__%s\\n' \"$code\"; printf '%s\\n' \"$output\" | head -c {limit}"
    )
}

#[derive(Serialize)]
pub(super) struct ReportResult {
    pub name: String,
    pub status: String,
    pub detail: String,
}

pub(super) struct ReportFile {
    pub name: String,
    pub bytes: Vec<u8>,
}

pub(super) fn unpack(output: &str) -> Result<&str, String> {
    let (code, text) = output.split_once('\n').ok_or_else(|| output.to_string())?;
    match code.trim().strip_prefix("__SHELLLF_REPORT_EXIT__") {
        Some("0") if !text.starts_with("__SHELLLF_LOG_ERROR__") => Ok(text),
        Some(code) => Err(format!(
            "exit {code}: {}",
            text.chars().take(2000).collect::<String>()
        )),
        None => Err(output.chars().take(2000).collect()),
    }
}

pub(super) fn write_zip(
    destination: &Path,
    name: &str,
    route: &str,
    files: &[ReportFile],
    results: &[ReportResult],
    cancel: &AtomicBool,
) -> Result<(), String> {
    let write = || -> anyhow::Result<()> {
        let directory = destination
            .parent()
            .ok_or_else(|| anyhow::anyhow!("missing destination directory"))?;
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        {
            let mut zip = zip::ZipWriter::new(temporary.as_file_mut());
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            let manifest = serde_json::json!({
                "format": 1,
                "application": "ShellLF",
                "version": env!("CARGO_PKG_VERSION"),
                "host": name,
                "route": route,
                "collected_at": chrono::Local::now().to_rfc3339(),
                "items": results,
            });
            zip.start_file("manifest.json", options)?;
            zip.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
            for file in files {
                anyhow::ensure!(
                    !cancel.load(Ordering::Acquire),
                    t!("device_logs.report.cancelled").to_string()
                );
                zip.start_file(&file.name, options)?;
                zip.write_all(&file.bytes)?;
            }
            zip.finish()?;
        }
        anyhow::ensure!(
            !cancel.load(Ordering::Acquire),
            t!("device_logs.report.cancelled").to_string()
        );
        temporary.persist(destination)?;
        Ok(())
    };
    write().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{ReportFile, ReportOptions, ReportResult, bounded, items, unpack, write_zip};
    use crate::device_logs::commands::{Privilege, Source};
    use std::io::Read as _;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn zip_contains_collection_results_and_preserves_partial_failures() {
        let temporary = tempfile::tempdir().unwrap();
        let output = temporary.path().join("diagnostic.zip");
        std::fs::write(&output, "old archive").unwrap();
        let results = [
            ReportResult {
                name: "system.txt".into(),
                status: "ok".into(),
                detail: String::new(),
            },
            ReportResult {
                name: "kernel.log".into(),
                status: "failed".into(),
                detail: "permission denied".into(),
            },
        ];
        write_zip(
            &output,
            "example",
            "SSH",
            &[ReportFile {
                name: "system.txt".into(),
                bytes: b"Ubuntu".to_vec(),
            }],
            &results,
            &AtomicBool::new(false),
        )
        .unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(output).unwrap()).unwrap();
        let mut text = String::new();
        zip.by_name("manifest.json")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(manifest["items"][1]["status"], "failed");
        assert_eq!(manifest["items"][1]["detail"], "permission denied");
        assert!(zip.by_name("system.txt").is_ok());
        assert!(zip.by_name("kernel.log").is_err());
    }

    #[test]
    fn cancellation_does_not_replace_an_existing_archive() {
        let temporary = tempfile::tempdir().unwrap();
        let output = temporary.path().join("diagnostic.zip");
        std::fs::write(&output, "original").unwrap();
        assert!(write_zip(&output, "example", "SSH", &[], &[], &AtomicBool::new(true)).is_err());
        assert_eq!(std::fs::read_to_string(output).unwrap(), "original");
        assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 1);
    }

    #[test]
    fn report_options_omit_unselected_items_and_report_exit_codes() {
        let options = ReportOptions {
            system: true,
            resources: false,
            logs: false,
            services: false,
            screenshot: false,
            bugreport: false,
        };
        let sections = items(Source::Kernel, Privilege::User, &options);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].name, "system.txt");
        assert!(unpack("__SHELLLF_REPORT_EXIT__1\nPermission denied\n").is_err());
        assert!(unpack("__SHELLLF_REPORT_EXIT__0\n__SHELLLF_LOG_ERROR__\ndenied").is_err());
        let _forks = crate::testing::no_forks();
        let script = bounded("printf 'sample'; exit 7", 524288);
        let output = std::process::Command::new("sh")
            .args(["-c", &script])
            .output()
            .unwrap();
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .starts_with("__SHELLLF_REPORT_EXIT__7\nsample")
        );
    }
}
