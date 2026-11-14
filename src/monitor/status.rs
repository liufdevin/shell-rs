use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, separator::Separator, tooltip::Tooltip,
};
use gpui_kit::*;

use super::model::{DiskUsage, Memory, Snapshot, SystemInfo, format_rate};
use crate::i18n::t;
use crate::shared::{format_bytes, format_duration, format_percent, load_color};

#[derive(Clone)]
struct StatusValue {
    text: SharedString,
    detail: SharedString,
    load: Option<f32>,
}

#[derive(Clone, Copy)]
enum StatusTone {
    Muted,
    Info,
    Load(f32),
}

struct StatusMetric {
    id: &'static str,
    label: SharedString,
    icon: IconName,
    tone: StatusTone,
    values: Vec<StatusValue>,
    details: SharedString,
}

#[derive(IntoElement)]
pub struct MonitorStatus {
    metrics: Vec<StatusMetric>,
    label: SharedString,
}

impl MonitorStatus {
    pub(super) fn message(text: SharedString, details: SharedString) -> Self {
        let metric = StatusMetric {
            id: "status-metric-system",
            label: text.clone(),
            icon: IconName::Info,
            tone: StatusTone::Muted,
            values: vec![StatusValue {
                text: text.clone(),
                detail: details.clone(),
                load: None,
            }],
            details,
        };
        Self {
            metrics: vec![metric],
            label: text,
        }
    }

    pub(super) fn reading(
        snapshot: &Snapshot,
        system: Option<&SystemInfo>,
        disks: &[DiskUsage],
    ) -> Self {
        let cpu = snapshot.cpu.as_ref().map(|load| load.total);
        let cpu_text = cpu.map(format_percent).unwrap_or_else(|| "—".into());
        let memory_text = format!(
            "{} / {}",
            format_bytes(snapshot.memory.used),
            format_bytes(snapshot.memory.total)
        );
        let memory_percent = snapshot.memory.percent();
        let interface = snapshot.main_interface();
        let upload = interface
            .and_then(|interface| interface.transmitted)
            .map(format_rate)
            .unwrap_or_else(|| "—".into());
        let download = interface
            .and_then(|interface| interface.received)
            .map(format_rate)
            .unwrap_or_else(|| "—".into());
        let uptime = snapshot
            .uptime
            .map(format_duration)
            .unwrap_or_else(|| "—".into());

        let mut system_details = Vec::new();
        if let Some(system) = system {
            system_details.extend(
                [
                    system.host_name.as_str(),
                    system.os.as_str(),
                    system.arch.as_str(),
                    system.cpu_model.as_str(),
                ]
                .into_iter()
                .filter(|value| !value.is_empty()),
            );
        }
        let cpu_detail = t!("monitor.status.cpu_detail", value = cpu_text.clone());
        let cpu_details = if system_details.is_empty() {
            cpu_detail.to_string()
        } else {
            format!("{cpu_detail}\n{}", system_details.join(" · "))
        };
        let interface_detail = interface
            .map(|interface| t!("monitor.status.interface", name = interface.name).to_string())
            .unwrap_or_else(|| t!("monitor.status.no_interface").to_string());
        let upload_detail = t!(
            "monitor.status.upload_detail",
            value = upload.clone(),
            interface = interface_detail.clone()
        );
        let download_detail = t!(
            "monitor.status.download_detail",
            value = download.clone(),
            interface = interface_detail
        );
        let memory_details = memory_detail(&snapshot.memory, memory_percent);
        let uptime_detail = t!("monitor.status.uptime_detail", value = uptime.clone());
        let mut metrics = vec![
            StatusMetric {
                id: "status-metric-cpu",
                label: t!("monitor.status.cpu"),
                icon: IconName::Cpu,
                tone: StatusTone::Load(cpu.unwrap_or(0.)),
                values: vec![StatusValue {
                    text: cpu_text.into(),
                    detail: cpu_details.into(),
                    load: cpu,
                }],
                details: cpu_detail,
            },
            StatusMetric {
                id: "status-metric-memory",
                label: t!("monitor.status.memory"),
                icon: IconName::MemoryStick,
                tone: StatusTone::Load(memory_percent),
                values: vec![StatusValue {
                    text: memory_text.into(),
                    detail: memory_details.clone().into(),
                    load: Some(memory_percent),
                }],
                details: memory_details.into(),
            },
            StatusMetric {
                id: "status-metric-upload",
                label: t!("monitor.status.upload"),
                icon: IconName::ArrowUp,
                tone: StatusTone::Info,
                values: vec![StatusValue {
                    text: upload.into(),
                    detail: upload_detail.clone(),
                    load: None,
                }],
                details: upload_detail,
            },
            StatusMetric {
                id: "status-metric-download",
                label: t!("monitor.status.download"),
                icon: IconName::ArrowDown,
                tone: StatusTone::Info,
                values: vec![StatusValue {
                    text: download.into(),
                    detail: download_detail.clone(),
                    load: None,
                }],
                details: download_detail,
            },
            StatusMetric {
                id: "status-metric-uptime",
                label: t!("monitor.status.uptime"),
                icon: IconName::Calendar,
                tone: StatusTone::Muted,
                values: vec![StatusValue {
                    text: uptime.into(),
                    detail: uptime_detail.clone(),
                    load: None,
                }],
                details: uptime_detail,
            },
        ];

        if !disks.is_empty() {
            let values: Vec<_> = disks
                .iter()
                .map(|disk| StatusValue {
                    text: format!("{} {}", disk.mount, format_percent(disk.percent())).into(),
                    detail: t!(
                        "monitor.status.disk_detail",
                        mount = disk.mount,
                        percent = format_percent(disk.percent()),
                        used = format_bytes(disk.used),
                        total = format_bytes(disk.total)
                    ),
                    load: Some(disk.percent()),
                })
                .collect();
            let details = values
                .iter()
                .map(|value| value.detail.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            metrics.push(StatusMetric {
                id: "status-metric-disks",
                label: t!("monitor.status.disks"),
                icon: IconName::HardDrive,
                tone: StatusTone::Info,
                values,
                details: details.into(),
            });
        }

        let label = metrics
            .iter()
            .map(|metric| {
                format!(
                    "{} {}",
                    metric.label,
                    metric
                        .values
                        .iter()
                        .map(|value| value.text.as_ref())
                        .collect::<Vec<_>>()
                        .join(" · ")
                )
            })
            .collect::<Vec<_>>()
            .join(" | ");
        Self {
            metrics,
            label: label.into(),
        }
    }
}

fn memory_detail(memory: &Memory, percent: f32) -> String {
    t!(
        "monitor.status.memory_detail",
        value = format!(
            "{} / {}",
            format_bytes(memory.used),
            format_bytes(memory.total)
        ),
        percent = format_percent(percent)
    )
    .to_string()
}

impl RenderOnce for MonitorStatus {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut row = Vec::new();
        for (index, metric) in self.metrics.into_iter().enumerate() {
            if index > 0 {
                row.push(
                    Separator::vertical()
                        .h_3()
                        .color(cx.theme().border)
                        .into_any_element(),
                );
            }
            let tone = match metric.tone {
                StatusTone::Muted => cx.theme().muted_foreground,
                StatusTone::Info => cx.theme().info,
                StatusTone::Load(load) => load_color(load, cx),
            };
            let accessibility = format!(
                "{} {}",
                metric.label,
                metric
                    .values
                    .iter()
                    .map(|value| value.text.as_ref())
                    .collect::<Vec<_>>()
                    .join(" · ")
            );
            let mut values = Vec::new();
            for (value_index, value) in metric.values.into_iter().enumerate() {
                if value_index > 0 {
                    values.push(
                        Separator::vertical()
                            .h_3()
                            .color(cx.theme().border)
                            .into_any_element(),
                    );
                }
                let muted = cx.theme().muted_foreground;
                let color = value.load.map_or(muted, |load| {
                    if load >= 70. {
                        load_color(load, cx)
                    } else {
                        muted
                    }
                });
                values.push(
                    div()
                        .id(SharedString::from(format!("{}-{value_index}", metric.id)))
                        .min_w_0()
                        .truncate()
                        .text_color(color)
                        .font_family(cx.theme().mono_font_family.clone())
                        .tooltip(move |window, cx| {
                            Tooltip::new(value.detail.clone()).build(window, cx)
                        })
                        .child(value.text)
                        .into_any_element(),
                );
            }
            let shrink = if matches!(metric.id, "status-metric-memory" | "status-metric-disks") {
                1.
            } else {
                0.
            };
            row.push(
                h_flex()
                    .id(metric.id)
                    .test_support()
                    .aria_label(accessibility)
                    .min_w_0()
                    .flex_shrink(shrink)
                    .items_center()
                    .gap_1()
                    .tooltip(move |window, cx| {
                        Tooltip::new(metric.details.clone()).build(window, cx)
                    })
                    .child(Icon::new(metric.icon).xsmall().text_color(tone))
                    .children(values)
                    .into_any_element(),
            );
        }
        h_flex()
            .id("status-system-info")
            .test_support()
            .aria_label(self.label)
            .flex_1()
            .min_w_0()
            .items_center()
            .gap_1p5()
            .overflow_hidden()
            .children(row)
    }
}
