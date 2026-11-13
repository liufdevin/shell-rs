use super::{
    Connected, DeviceDiagnostic, DeviceState, DiagnosticReport, Measurement, PathSnapshot,
    ProbeStatus, RouteDiagnostic, Transport,
};
use crate::i18n::t;
use gpui_kit::{
    component::{ActiveTheme as _, v_flex},
    *,
};

pub(super) fn current_paths(
    id: impl Into<ElementId>,
    connection: &Connected,
    snapshot: Option<&PathSnapshot>,
    cx: &App,
) -> AnyElement {
    let label = if connection.devices.is_empty() {
        t!("remote.diagnostic.no_paths")
    } else if let Some(snapshot) = snapshot {
        let mut direct = 0;
        let mut relay = 0;
        let mut unknown = 0;
        for (_, route) in snapshot.devices() {
            match route.transport {
                Transport::Direct => direct += 1,
                Transport::Relay => relay += 1,
                Transport::Unknown => unknown += 1,
            }
        }
        t!(
            "remote.path.summary",
            direct = direct,
            relay = relay,
            unknown = unknown
        )
    } else {
        t!("remote.path.querying")
    };
    div()
        .id(id)
        .test_support()
        .role(Role::Status)
        .aria_label(label.clone())
        .text_sm()
        .text_color(cx.theme().foreground)
        .child(label)
        .into_any_element()
}

pub(super) fn current_device(
    device: &str,
    route: Option<&RouteDiagnostic>,
    cx: &App,
) -> AnyElement {
    let label = match route {
        Some(route) => t!("remote.path.device", path = path_label(route)),
        None => t!("remote.path.querying"),
    };
    div()
        .id(SharedString::from(format!("remote-device-path-{device}")))
        .test_support()
        .role(Role::Status)
        .aria_label(label.clone())
        .text_sm()
        .text_color(cx.theme().foreground)
        .child(label)
        .into_any_element()
}

fn path_label(route: &RouteDiagnostic) -> SharedString {
    match route.transport {
        Transport::Direct => t!("remote.diagnostic.direct"),
        Transport::Relay => t!(
            "remote.diagnostic.relay",
            hops = route.hops.unwrap_or_default()
        ),
        Transport::Unknown => t!("remote.diagnostic.unknown_path"),
    }
}

pub(super) fn summary(report: &DiagnosticReport, cx: &App) -> AnyElement {
    let stale = report.is_stale();
    let state = |ready| {
        if ready {
            t!("remote.diagnostic.ok")
        } else {
            t!("remote.diagnostic.unavailable")
        }
    };
    let checked =
        chrono::DateTime::from_timestamp_millis(report.checked_at.min(i64::MAX as u64) as i64)
            .map(|time| {
                time.with_timezone(&chrono::Local)
                    .format("%H:%M:%S")
                    .to_string()
            })
            .unwrap_or_default();
    let label = t!(
        "remote.diagnostic.local",
        adapter = state(report.local.adapter_ready),
        core = state(report.local.core_running),
        rpc = state(report.local.rpc_ready),
        time = checked,
        count = report.devices.len()
    );
    let paths = paths(report, stale);
    v_flex()
        .id("remote-network-diagnostic")
        .test_support()
        .role(Role::Status)
        .aria_label(label.clone())
        .gap_1()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(label)
        .child(
            div()
                .id("remote-network-paths")
                .test_support()
                .aria_label(paths.clone())
                .text_color(cx.theme().foreground)
                .child(paths),
        )
        .into_any_element()
}

fn paths(report: &DiagnosticReport, stale: bool) -> SharedString {
    if stale {
        return t!("remote.diagnostic.stale");
    }
    if report.devices.is_empty() {
        return t!("remote.diagnostic.no_paths");
    }
    let mut direct = 0;
    let mut relay = 0;
    let mut unknown = 0;
    let mut invalid = 0;
    for result in &report.devices {
        if result.state != DeviceState::Current {
            invalid += 1;
        } else {
            match result.path.transport {
                Transport::Direct => direct += 1,
                Transport::Relay => relay += 1,
                Transport::Unknown => unknown += 1,
            }
        }
    }
    t!(
        "remote.diagnostic.paths",
        direct = direct,
        relay = relay,
        unknown = unknown,
        invalid = invalid
    )
}

pub(super) fn device(result: &DeviceDiagnostic, stale: bool, cx: &App) -> AnyElement {
    let label = match result.state {
        DeviceState::AddressChanged => t!("remote.diagnostic.address_changed"),
        DeviceState::Offline => t!("remote.diagnostic.offline"),
        DeviceState::Current if stale => t!("remote.diagnostic.stale"),
        DeviceState::Current => {
            let path = path_label(&result.path);
            t!(
                "remote.diagnostic.device",
                tcp = status(result.tcp22.status),
                path = path,
                icmp = measurement(&result.icmp),
                tcp_ms = measured(result.tcp22.latency_ms),
                route_ms = measured(result.path.route_latency_ms),
                protocol = result
                    .path
                    .tunnel_protocol
                    .clone()
                    .unwrap_or_else(|| "—".into())
            )
        }
    };
    div()
        .id(SharedString::from(format!(
            "remote-device-diagnostic-{}",
            result.device_id
        )))
        .test_support()
        .aria_label(label.clone())
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(label)
        .into_any_element()
}

fn measured(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1} ms"))
        .unwrap_or_else(|| "—".into())
}

fn measurement(value: &Measurement) -> String {
    if value.status == ProbeStatus::Ok {
        measured(value.latency_ms)
    } else {
        status(value.status).to_string()
    }
}

fn status(value: ProbeStatus) -> SharedString {
    match value {
        ProbeStatus::NotTested => t!("remote.diagnostic.not_tested"),
        ProbeStatus::Open => t!("remote.diagnostic.open"),
        ProbeStatus::Ok => t!("remote.diagnostic.ok"),
        ProbeStatus::Refused => t!("remote.diagnostic.refused"),
        ProbeStatus::Timeout => t!("remote.diagnostic.timeout"),
        ProbeStatus::Error => t!("remote.diagnostic.error"),
    }
}
