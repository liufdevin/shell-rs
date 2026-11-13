use super::{Catalog, Connected};
use serde::{Deserialize, Serialize};
use std::{
    net::Ipv4Addr,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    #[default]
    NotTested,
    Open,
    Ok,
    Refused,
    Timeout,
    Error,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Measurement {
    pub status: ProbeStatus,
    pub latency_ms: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    #[default]
    Unknown,
    Direct,
    Relay,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RouteDiagnostic {
    pub transport: Transport,
    pub hops: Option<u8>,
    pub route_latency_ms: Option<f64>,
    pub tunnel_protocol: Option<String>,
}

impl RouteDiagnostic {
    pub fn from_core(
        address: Ipv4Addr,
        routes: &[serde_json::Value],
        peers: &[serde_json::Value],
    ) -> Self {
        let matches = |row: &&serde_json::Value| {
            row["ipv4"]
                .as_str()
                .and_then(|ip| ip.split('/').next()?.parse::<Ipv4Addr>().ok())
                == Some(address)
        };
        let mut result = Self::default();
        if let Some(route) = routes.iter().find(matches) {
            result.hops = route["path_len"]
                .as_u64()
                .filter(|hops| (1..=64).contains(hops))
                .map(|hops| hops as u8);
            result.transport = match result.hops {
                Some(1) => Transport::Direct,
                Some(_) => Transport::Relay,
                None => Transport::Unknown,
            };
            result.route_latency_ms = latency(route["path_latency"].as_f64());
        }
        if let Some(peer) = peers.iter().find(matches) {
            result.tunnel_protocol = peer["tunnel_proto"]
                .as_str()
                .filter(|protocol| {
                    ["udp", "tcp", "ws", "wss", "quic", "wireguard"].contains(protocol)
                })
                .map(str::to_owned);
            let cost = peer["cost"].as_str().unwrap_or_default();
            let hops = if cost == "p2p" {
                Some(1)
            } else {
                cost.strip_prefix("relay(")
                    .and_then(|cost| cost.strip_suffix(')'))
                    .and_then(|cost| cost.parse::<u8>().ok())
                    .filter(|hops| (2..=64).contains(hops))
            };
            if (result.transport == Transport::Direct && cost.starts_with("relay("))
                || (result.transport == Transport::Relay && cost == "p2p")
                || result
                    .hops
                    .zip(hops)
                    .is_some_and(|(route, peer)| route != peer)
            {
                result.transport = Transport::Unknown;
                result.hops = None;
                result.route_latency_ms = None;
            } else if result.transport == Transport::Unknown {
                result.hops = hops;
                result.transport = match hops {
                    Some(1) => Transport::Direct,
                    Some(_) => Transport::Relay,
                    None => Transport::Unknown,
                };
            }
        }
        result
    }
}

fn latency(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && (0.0..=60000.0).contains(value))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct LocalDiagnostic {
    pub adapter_ready: bool,
    pub core_running: bool,
    pub rpc_ready: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceState {
    #[default]
    Current,
    AddressChanged,
    Offline,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DeviceDiagnostic {
    pub device_id: String,
    pub virtual_ip: Ipv4Addr,
    pub tcp22: Measurement,
    pub icmp: Measurement,
    #[serde(default)]
    pub path: RouteDiagnostic,
    #[serde(default)]
    pub state: DeviceState,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct DiagnosticReport {
    pub server: String,
    pub network: String,
    pub virtual_ip: Ipv4Addr,
    pub checked_at: u64,
    pub local: LocalDiagnostic,
    pub devices: Vec<DeviceDiagnostic>,
}

impl DiagnosticReport {
    pub fn new(
        connection: &Connected,
        local: LocalDiagnostic,
        devices: Vec<DeviceDiagnostic>,
    ) -> Self {
        Self {
            server: connection.server.clone(),
            network: connection.network.clone(),
            virtual_ip: connection.virtual_ip,
            checked_at: now(),
            local,
            devices,
        }
    }

    pub fn is_current(&self, connection: &Connected) -> bool {
        self.server == connection.server
            && self.network == connection.network
            && self.virtual_ip == connection.virtual_ip
    }

    pub fn is_stale(&self) -> bool {
        let time = now();
        time < self.checked_at || time.saturating_sub(self.checked_at) >= 180000
    }

    pub fn revalidate(&mut self, connection: &Connected, catalog: &Catalog) {
        for result in &mut self.devices {
            let fresh = catalog.devices.iter().find(|device| {
                device.device_id == result.device_id
                    && device.network_id == self.network
                    && device.lease_valid()
            });
            let state = match fresh {
                Some(device) if device.virtual_ip != Some(result.virtual_ip) => {
                    DeviceState::AddressChanged
                }
                Some(_)
                    if connection.devices.iter().any(|target| {
                        target.device_id == result.device_id
                            && target.virtual_ip == result.virtual_ip
                    }) =>
                {
                    continue;
                }
                _ => DeviceState::Offline,
            };
            result.state = state;
            result.tcp22 = Measurement::default();
            result.icmp = Measurement::default();
            result.path = RouteDiagnostic::default();
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exact_target_routes_distinguish_direct_relay_and_changing_paths() {
        let address = "10.215.0.11".parse().unwrap();
        let direct = RouteDiagnostic::from_core(
            address,
            &[json!({"ipv4":"10.215.0.11/24","path_len":1,"path_latency":10.5})],
            &[json!({"ipv4":"10.215.0.11","cost":"p2p","tunnel_proto":"udp"})],
        );
        assert_eq!(direct.transport, Transport::Direct);
        assert_eq!(direct.hops, Some(1));
        assert_eq!(direct.route_latency_ms, Some(10.5));
        let relay = RouteDiagnostic::from_core(
            address,
            &[json!({"ipv4":"10.215.0.11","path_len":3,"path_latency":25})],
            &[json!({"ipv4":"10.215.0.11","cost":"relay(3)","tunnel_proto":"tcp"})],
        );
        assert_eq!(relay.transport, Transport::Relay);
        assert_eq!(relay.hops, Some(3));
        let conflict = RouteDiagnostic::from_core(
            address,
            &[json!({"ipv4":"10.215.0.11","path_len":1,"path_latency":10})],
            &[json!({"ipv4":"10.215.0.11","cost":"relay(2)"})],
        );
        assert_eq!(conflict.transport, Transport::Unknown);
        assert!(conflict.hops.is_none() && conflict.route_latency_ms.is_none());
        assert_eq!(
            RouteDiagnostic::from_core(
                address,
                &[json!({"ipv4":"10.215.0.11","path_len":3})],
                &[json!({"ipv4":"10.215.0.11","cost":"relay(2)"})]
            )
            .transport,
            Transport::Unknown
        );
        let coordinator = RouteDiagnostic::from_core(
            address,
            &[json!({"ipv4":"10.215.0.1","path_len":1})],
            &[json!({"ipv4":"10.215.0.1","cost":"p2p"})],
        );
        assert_eq!(coordinator.transport, Transport::Unknown);
    }

    #[test]
    fn absent_or_invalid_path_measurements_are_not_fabricated() {
        let address = "10.215.0.11".parse().unwrap();
        for cost in ["relay(0)", "relay(65)", "relay(999)", "other"] {
            let value = RouteDiagnostic::from_core(
                address,
                &[],
                &[json!({"ipv4":"10.215.0.11","cost":cost,"tunnel_proto":"secret"})],
            );
            assert_eq!(value.transport, Transport::Unknown);
            assert!(value.hops.is_none() && value.tunnel_protocol.is_none());
        }
        assert!(latency(Some(-1.0)).is_none());
        assert!(latency(Some(f64::NAN)).is_none());
        assert!(latency(Some(f64::INFINITY)).is_none());
        assert_eq!(latency(Some(0.0)), Some(0.0));
        assert_eq!(
            RouteDiagnostic::from_core(
                address,
                &[],
                &[json!({"ipv4":"10.215.0.11","cost":"relay(2)"})]
            )
            .transport,
            Transport::Relay
        );
    }

    #[test]
    fn address_changes_offline_devices_and_expiry_invalidate_success() {
        let mut connection = Connected::new(
            "server".into(),
            "network".into(),
            "test".into(),
            "10.215.0.2".parse().unwrap(),
            vec![super::super::Target::new(
                "device".into(),
                "test".into(),
                "10.215.0.11".parse().unwrap(),
            )],
        );
        let device: DeviceDiagnostic = serde_json::from_value(json!({"deviceId":"device","virtualIp":"10.215.0.11",
            "tcp22":{"status":"open","latencyMs":12.0},"icmp":{"status":"timeout","latencyMs":null}})).unwrap();
        let mut report =
            DiagnosticReport::new(&connection, LocalDiagnostic::default(), vec![device]);
        let mut catalog = Catalog::default();
        catalog.devices.push(serde_json::from_value(json!({"deviceId":"device","virtualIp":"10.215.0.11","networkId":"network","enabled":true,"expiresAt":4102444800000_u64})).unwrap());
        report.revalidate(&connection, &catalog);
        assert_eq!(report.devices[0].tcp22.status, ProbeStatus::Open);
        assert_eq!(report.devices[0].icmp.status, ProbeStatus::Timeout);
        catalog.devices[0].virtual_ip = Some("10.215.0.99".parse().unwrap());
        report.revalidate(&connection, &catalog);
        assert_eq!(report.devices[0].state, DeviceState::AddressChanged);
        assert_eq!(report.devices[0].tcp22.status, ProbeStatus::NotTested);
        assert!(report.devices[0].tcp22.latency_ms.is_none());
        catalog.devices.clear();
        report.revalidate(&connection, &catalog);
        assert_eq!(report.devices[0].state, DeviceState::Offline);
        connection.network = "another".into();
        assert!(!report.is_current(&connection));
        report.checked_at = now().saturating_sub(180001);
        assert!(report.is_stale());
    }

    #[cfg(windows)]
    #[test]
    fn diagnostic_scope_rejects_unknown_devices_and_excessive_batches_before_probing() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("status.json"),
            br#"{"state":"running","stage":"connected"}"#,
        )
        .unwrap();
        let targets = (0..101)
            .map(|index| {
                super::super::Target::new(
                    format!("device-{index}"),
                    "test".into(),
                    "10.215.0.11".parse().unwrap(),
                )
            })
            .collect();
        let connection = Connected::new(
            "server".into(),
            "network".into(),
            "test".into(),
            "10.215.0.2".parse().unwrap(),
            targets,
        );
        let running =
            super::super::network::Running::fixture(connection, directory.path().to_path_buf());
        assert_eq!(
            check(&running, &[]).unwrap_err().to_string(),
            "diagnostic_too_many"
        );
        assert_eq!(
            check(&running, &["foreign-device".into()])
                .unwrap_err()
                .to_string(),
            "device_unavailable"
        );
    }

    #[cfg(windows)]
    #[test]
    fn local_checks_do_not_probe_without_an_owned_core_and_adapter() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("result.json");
        let request = json!({"directory":directory.path(),"packageRoot":directory.path(),"virtualIp":"10.215.0.2",
            "devices":[{"deviceId":"fixture","virtualIp":"10.215.0.11"}],"output":output});
        super::super::network::powershell(
            include_str!("diagnostics.ps1"),
            &serde_json::to_vec(&request).unwrap(),
        )
        .unwrap();
        let result: RawReport = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
        assert!(!result.local.core_running && !result.local.rpc_ready);
        assert_eq!(result.devices.len(), 1);
        assert_eq!(result.devices[0].tcp22.status, ProbeStatus::NotTested);
        assert!(result.devices[0].tcp22.latency_ms.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn automatic_paths_skip_remote_probes_and_the_full_diagnostic_batch_limit() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("paths.json");
        let devices: Vec<_> = (0..101)
            .map(|index| json!({"deviceId":index.to_string(),"virtualIp":"192.0.2.1"}))
            .collect();
        let request = json!({"directory":directory.path(),"packageRoot":directory.path(),"virtualIp":"10.215.0.2",
            "devices":devices,"output":output,"pathsOnly":true});
        super::super::network::powershell(
            include_str!("diagnostics.ps1"),
            &serde_json::to_vec(&request).unwrap(),
        )
        .unwrap();
        let result: RawReport = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
        assert!(result.devices.is_empty() && result.routes.is_empty() && result.peers.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn native_tcp_probes_bind_the_source_and_distinguish_open_from_refused() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("probes.json");
        let script = include_str!("diagnostics.ps1");
        let start = script.find("Add-Type -TypeDefinition").unwrap();
        let end = script.find("\nfunction Core-Json").unwrap();
        let test = format!(
            r#"$ErrorActionPreference='Stop'
{}
$listener = New-Object Net.Sockets.TcpListener([Net.IPAddress]::Loopback, 0)
$self = [Diagnostics.Process]::GetCurrentProcess()
$ticks = $self.StartTime.ToUniversalTime().Ticks.ToString()
if (-not [ShellLfRemoteDiagnostics.Probe]::OwnedCore($self.Id, $ticks, $self.MainModule.FileName)) {{ throw 'owned process rejected' }}
if ([ShellLfRemoteDiagnostics.Probe]::OwnedCore($self.Id, '0', $self.MainModule.FileName)) {{ throw 'reused PID accepted' }}
if ([ShellLfRemoteDiagnostics.Probe]::OwnedCore($self.Id, $ticks, 'C:\not-the-owned-core.exe')) {{ throw 'foreign executable accepted' }}
$listener.Start()
$port = $listener.LocalEndpoint.Port
$open = [ShellLfRemoteDiagnostics.Probe]::Tcp('127.0.0.1','127.0.0.1',$port,1000).GetAwaiter().GetResult()
$listener.Stop()
$refused = [ShellLfRemoteDiagnostics.Probe]::Tcp('127.0.0.1','127.0.0.1',$port,5000).GetAwaiter().GetResult()
$invalid = [ShellLfRemoteDiagnostics.Probe]::Tcp('192.0.2.123','127.0.0.1',$port,1000).GetAwaiter().GetResult()
@{{open=$open;refused=$refused;invalid=$invalid}} | ConvertTo-Json | Set-Content -LiteralPath '{}' -Encoding UTF8
"#,
            &script[start..end],
            output.to_string_lossy().replace('\'', "''")
        );
        super::super::network::powershell(&test, &[]).unwrap();
        let bytes = std::fs::read(output).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes))
                .unwrap();
        assert_eq!(value["open"]["Status"], "open");
        assert!(value["open"]["LatencyMs"].is_number());
        assert_eq!(value["refused"]["Status"], "refused");
        assert_eq!(value["invalid"]["Status"], "error");
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReport {
    local: LocalDiagnostic,
    devices: Vec<DeviceDiagnostic>,
    routes: Vec<serde_json::Value>,
    peers: Vec<serde_json::Value>,
}

fn read_report(
    directory: &std::path::Path,
    virtual_ip: Ipv4Addr,
    targets: &[&super::Target],
    paths_only: bool,
) -> anyhow::Result<RawReport> {
    use anyhow::ensure;
    let backend = std::env::current_exe()?
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing_backend"))?
        .to_path_buf();
    super::network::verify_backend(&backend)?;
    let output = directory.join(format!("check-{}.json", uuid::Uuid::new_v4()));
    let request = serde_json::to_vec(&serde_json::json!({
        "directory": directory, "packageRoot": backend, "virtualIp": virtual_ip,
        "devices": targets, "output": output, "pathsOnly": paths_only,
    }))?;
    let result = (|| {
        super::network::powershell_timeout(
            include_str!("diagnostics.ps1"),
            &request,
            std::time::Duration::from_secs(if paths_only { 12 } else { 120 }),
        )?;
        ensure!(
            std::fs::metadata(&output)?.len() <= 2 * 1024 * 1024,
            "diagnostic_failed"
        );
        Ok(serde_json::from_slice(&std::fs::read(&output)?)?)
    })();
    let _ = std::fs::remove_file(output);
    result
}

pub(super) fn query_paths(
    connection: &Connected,
    directory: &std::path::Path,
) -> anyhow::Result<super::PathSnapshot> {
    let raw = read_report(directory, connection.virtual_ip, &[], true)?;
    anyhow::ensure!(
        raw.local.adapter_ready && raw.local.core_running && raw.local.rpc_ready,
        "diagnostic_failed"
    );
    Ok(super::PathSnapshot::from_core(
        connection,
        &raw.routes,
        &raw.peers,
    ))
}

pub(super) fn check(
    running: &super::network::Running,
    selected: &[String],
) -> anyhow::Result<DiagnosticReport> {
    use anyhow::ensure;
    let checked_at = now();
    ensure!(
        cfg!(all(windows, target_arch = "x86_64")),
        "unsupported_platform"
    );
    ensure!(
        running.status()?.is_some_and(|(active, _)| active),
        "network_stopped"
    );
    let targets: Vec<_> = running
        .info
        .devices
        .iter()
        .filter(|target| selected.is_empty() || selected.contains(&target.device_id))
        .collect();
    ensure!(targets.len() <= 100, "diagnostic_too_many");
    ensure!(
        selected.is_empty()
            || selected
                .iter()
                .all(|id| targets.iter().any(|target| target.device_id == *id)),
        "device_unavailable"
    );
    let result = (|| {
        let raw = read_report(
            running.directory(),
            running.info.virtual_ip,
            &targets,
            false,
        )?;
        ensure!(raw.devices.len() == targets.len(), "diagnostic_failed");
        let mut seen = std::collections::HashSet::new();
        let mut devices = raw.devices;
        for device in &mut devices {
            ensure!(
                seen.insert(device.device_id.clone())
                    && targets
                        .iter()
                        .any(|target| target.device_id == device.device_id
                            && target.virtual_ip == device.virtual_ip),
                "diagnostic_failed"
            );
            device.state = DeviceState::Current;
            device.tcp22.latency_ms = if device.tcp22.status == ProbeStatus::Open {
                latency(device.tcp22.latency_ms)
            } else {
                None
            };
            device.icmp.latency_ms = if device.icmp.status == ProbeStatus::Ok {
                latency(device.icmp.latency_ms)
            } else {
                None
            };
            device.path = RouteDiagnostic::from_core(device.virtual_ip, &raw.routes, &raw.peers);
        }
        ensure!(
            running.status()?.is_some_and(|(active, _)| active),
            "network_stopped"
        );
        let mut report = DiagnosticReport::new(&running.info, raw.local, devices);
        report.checked_at = checked_at;
        Ok(report)
    })();
    result.map_err(|error: anyhow::Error| match error.to_string().as_str() {
        "network_stopped" | "backend_hash_mismatch" | "missing_backend" => error,
        _ => anyhow::anyhow!("diagnostic_failed"),
    })
}
