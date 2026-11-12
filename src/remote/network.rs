use super::api::Account;
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum NetworkStage {
    Preparing,
    RequestingPermission,
    CheckingBackend,
    CheckingRoute,
    PreparingFirewall,
    RequestingLease,
    ValidatingProfile,
    StartingCore,
    WaitingForInterface,
    Connected,
    RenewingLease,
    Stopping,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct NetworkProgress {
    stage: NetworkStage,
    started: Instant,
}

impl NetworkProgress {
    pub fn new(stage: NetworkStage) -> Self {
        Self {
            stage,
            started: Instant::now(),
        }
    }

    pub fn stage(&self) -> NetworkStage {
        self.stage
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn set_stage(&mut self, stage: NetworkStage) {
        self.stage = stage;
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Target {
    pub device_id: String,
    #[serde(default, deserialize_with = "super::api::null_string")]
    pub display_name: String,
    pub virtual_ip: Ipv4Addr,
    pub port: u16,
}

impl Target {
    pub fn new(device_id: String, display_name: String, virtual_ip: Ipv4Addr) -> Self {
        Self {
            device_id,
            display_name,
            virtual_ip,
            port: 22,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Connected {
    pub server: String,
    pub network: String,
    pub name: String,
    pub virtual_ip: Ipv4Addr,
    pub devices: Vec<Target>,
}

impl Connected {
    pub fn new(
        server: String,
        network: String,
        name: String,
        virtual_ip: Ipv4Addr,
        devices: Vec<Target>,
    ) -> Self {
        Self {
            server,
            network,
            name,
            virtual_ip,
            devices,
        }
    }
}

pub(super) struct Running {
    pub info: Connected,
    directory: PathBuf,
    subnet: (Ipv4Addr, u8),
}

#[derive(Debug)]
struct WorkerStatus {
    state: String,
    stage: String,
    error: String,
}

impl WorkerStatus {
    fn is_running(&self) -> bool {
        self.state == "running"
    }

    fn network_stage(&self) -> NetworkStage {
        match self.stage.as_str() {
            "backend_check" => NetworkStage::CheckingBackend,
            "route_check" => NetworkStage::CheckingRoute,
            "firewall" => NetworkStage::PreparingFirewall,
            "authorization" => NetworkStage::RequestingLease,
            "profile" => NetworkStage::ValidatingProfile,
            "core_start" => NetworkStage::StartingCore,
            "interface_wait" => NetworkStage::WaitingForInterface,
            "running" | "connected" => NetworkStage::Connected,
            "renewal" | "renewing" => NetworkStage::RenewingLease,
            "stopping" => NetworkStage::Stopping,
            _ => NetworkStage::Preparing,
        }
    }
}

fn report_progress(progress: &Arc<Mutex<Option<NetworkProgress>>>, stage: NetworkStage) {
    let mut progress = progress.lock().unwrap();
    match progress.as_mut() {
        Some(progress) => progress.set_stage(stage),
        None => *progress = Some(NetworkProgress::new(stage)),
    }
}

pub(super) fn validate_grant(
    value: &serde_json::Value,
    client: &str,
    network: &str,
) -> Result<Vec<Target>> {
    ensure!(
        value["clientId"].as_str() == Some(client) && value["networkId"].as_str() == Some(network),
        "invalid_profile"
    );
    ensure!(
        value["expiresIn"]
            .as_u64()
            .is_some_and(|seconds| (1..=3600).contains(&seconds)),
        "invalid_lease"
    );
    let text = value["files"]["client.toml"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("invalid_profile"))?;
    use sha2::{Digest as _, Sha256};
    let hash = Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    ensure!(
        value["profileHash"].as_str() == Some(&hash),
        "profile_hash_mismatch"
    );
    let config: serde_json::Value = serde_json::from_str(
        value["files"]["network.json"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("invalid_profile"))?,
    )?;
    let ip: Ipv4Addr = value["virtualIp"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("invalid_profile"))?
        .parse()?;
    ensure!(ip.is_private(), "invalid_profile");
    let cidr = config["virtual_ip"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("invalid_profile"))?;
    let (address, prefix) = cidr
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("invalid_profile"))?;
    let prefix: u8 = prefix.parse()?;
    ensure!(
        address.parse::<Ipv4Addr>()? == ip
            && (1..=30).contains(&prefix)
            && config["role"] == "client"
            && config["rpc_portal"] == "127.0.0.1:15889",
        "invalid_profile"
    );
    let profile: toml::Value =
        toml::from_str(text).map_err(|_| anyhow::anyhow!("invalid_profile"))?;
    let profile_ip = profile
        .get("ipv4")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("invalid_profile"))?;
    ensure!(
        profile_ip == cidr
            || profile_ip
                .parse::<Ipv4Addr>()
                .is_ok_and(|value| value == ip),
        "invalid_profile"
    );
    ensure!(
        profile.get("dhcp").and_then(toml::Value::as_bool) != Some(true),
        "invalid_profile"
    );
    for key in ["routes", "exit_nodes", "proxy_network"] {
        ensure!(
            profile
                .get(key)
                .is_none_or(|value| value.as_array().is_some_and(Vec::is_empty)),
            "invalid_profile"
        );
    }
    if let Some(rpc) = profile.get("rpc_portal") {
        ensure!(rpc.as_str() == Some("127.0.0.1:15889"), "invalid_profile");
    }
    if let Some(flags) = profile.get("flags") {
        for name in ["accept_dns", "enable_exit_node", "enable_vpn_portal"] {
            ensure!(
                flags.get(name).and_then(toml::Value::as_bool) != Some(true),
                "invalid_profile"
            );
        }
    }
    let mut targets: Vec<Target> = serde_json::from_value(value["devices"].clone())
        .map_err(|_| anyhow::anyhow!("invalid_profile"))?;
    for target in &mut targets {
        target.device_id.make_ascii_lowercase();
    }
    ensure!(targets.len() <= 10000, "invalid_profile");
    let mask = u32::MAX << (32 - prefix);
    let mut ids = std::collections::HashSet::new();
    let mut addresses = std::collections::HashSet::new();
    for target in &targets {
        ensure!(
            !target.device_id.is_empty()
                && ids.insert(&target.device_id)
                && addresses.insert(target.virtual_ip)
                && target.port == 22
                && target.virtual_ip != ip
                && u32::from(target.virtual_ip) & mask == u32::from(ip) & mask,
            "invalid_profile"
        );
    }
    Ok(targets)
}

pub(super) fn client_id(root: &Path) -> Result<String> {
    std::fs::create_dir_all(root)?;
    let path = root.join("client-id");
    if !path.exists() {
        let mut temporary = tempfile::NamedTempFile::new_in(root)?;
        use std::io::Write as _;
        temporary.write_all(uuid::Uuid::new_v4().to_string().as_bytes())?;
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(&path) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.error.into()),
        }
    }
    Ok(uuid::Uuid::parse_str(std::fs::read_to_string(path)?.trim())?.to_string())
}

impl Running {
    #[cfg(test)]
    pub(super) fn fixture(info: Connected, directory: PathBuf) -> Self {
        Self {
            subnet: (info.virtual_ip, 24),
            info,
            directory,
        }
    }

    pub fn start(
        account: &Account,
        network: &str,
        root: &Path,
        progress: &Arc<Mutex<Option<NetworkProgress>>>,
    ) -> Result<Self> {
        ensure!(
            cfg!(all(windows, target_arch = "x86_64")),
            "unsupported_platform"
        );
        ensure!(
            !["PROCESSOR_ARCHITECTURE", "PROCESSOR_ARCHITEW6432"]
                .iter()
                .any(
                    |key| std::env::var(key).is_ok_and(|value| value.eq_ignore_ascii_case("ARM64"))
                ),
            "unsupported_platform"
        );
        report_progress(progress, NetworkStage::CheckingBackend);
        let selected = account
            .catalog
            .networks
            .iter()
            .find(|item| item.network_id == network && item.enabled)
            .ok_or_else(|| anyhow::anyhow!("network_disabled"))?;
        let client = client_id(root)?;
        let backend = std::env::current_exe()?
            .parent()
            .ok_or_else(|| anyhow::anyhow!("missing_backend"))?
            .to_path_buf();
        verify_backend(&backend)?;
        report_progress(progress, NetworkStage::CheckingRoute);
        let probe = std::net::TcpListener::bind("127.0.0.1:15889")
            .map_err(|_| anyhow::anyhow!("network_in_use"))?;
        drop(probe);
        let directory = root.join(format!("session-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory)?;
        let mut running = Self {
            info: Connected::new(
                account.server.id.clone(),
                network.to_owned(),
                selected.name.clone(),
                Ipv4Addr::UNSPECIFIED,
                vec![],
            ),
            directory,
            subnet: (Ipv4Addr::UNSPECIFIED, 24),
        };
        let context = serde_json::json!({
            "serverUrl": account.server.url, "token": account.token(), "clientId": client,
            "networkId": network, "subnet": selected.subnet, "parentPid": std::process::id(),
            "packageRoot": backend, "directory": running.directory,
        });
        prepare(&running.directory, &context)?;
        report_progress(progress, NetworkStage::RequestingPermission);
        launch(&running.directory)?;
        let mut authorized = false;
        let result = (|| -> Result<()> {
            let deadline = Instant::now() + Duration::from_secs(120);
            loop {
                if let Some(status) = running.worker_status()? {
                    report_progress(progress, status.network_stage());
                    if status.state == "stopped" {
                        bail!("{}", status.error);
                    }
                    if status.stage == "authorization" {
                        break;
                    }
                }
                ensure!(Instant::now() < deadline, "network_start_timeout");
                std::thread::sleep(Duration::from_millis(100));
            }
            let started = Instant::now();
            report_progress(progress, NetworkStage::RequestingLease);
            let mut grant = account.request(
                "/api/client/ssh/session",
                Some(serde_json::json!({ "clientId": client, "networkId": network })),
            )?;
            authorized = true;
            report_progress(progress, NetworkStage::ValidatingProfile);
            let targets = validate_grant(&grant, &client, network)?;
            ensure!(!targets.is_empty(), "no_authorized_devices");
            let config: serde_json::Value =
                serde_json::from_str(grant["files"]["network.json"].as_str().unwrap())?;
            let cidr = config["virtual_ip"].as_str().unwrap();
            let (address, prefix) = cidr.split_once('/').unwrap();
            let remaining = grant["expiresIn"]
                .as_u64()
                .unwrap()
                .saturating_sub(started.elapsed().as_secs() + 1);
            ensure!(remaining > 0, "lease_expired");
            grant["expiresIn"] = remaining.into();
            running.info.virtual_ip = grant["virtualIp"].as_str().unwrap().parse()?;
            running.info.devices = targets;
            running.subnet = (address.parse()?, prefix.parse()?);
            let envelope = zeroize::Zeroizing::new(serde_json::to_vec(&serde_json::json!({
                "authorization": grant, "issuedAtTick": tick_count(),
            }))?);
            std::fs::write(running.directory.join("authorization.tmp"), &*envelope)?;
            std::fs::rename(
                running.directory.join("authorization.tmp"),
                running.directory.join("authorization.json"),
            )?;
            let deadline = Instant::now() + Duration::from_secs(45);
            while Instant::now() < deadline {
                if let Some(status) = running.worker_status()? {
                    report_progress(progress, status.network_stage());
                    if status.is_running() {
                        report_progress(progress, NetworkStage::Connected);
                        return Ok(());
                    }
                    if status.state == "stopped" {
                        bail!("{}", status.error);
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            bail!("network_start_timeout")
        })();
        if result.is_err() {
            let _ = running.stop();
            if authorized {
                let _ = account.request(
                    "/api/client/ssh/disconnect",
                    Some(serde_json::json!({"clientId":client,"networkId":network})),
                );
            }
        }
        result?;
        Ok(running)
    }

    pub fn refresh_devices(&mut self, devices: &[super::Device]) {
        let mask = u32::MAX << (32 - self.subnet.1);
        self.info.devices = devices
            .iter()
            .filter(|device| device.network_id == self.info.network && device.lease_valid())
            .filter_map(|device| {
                let ip = device.virtual_ip?;
                (ip != self.info.virtual_ip
                    && u32::from(ip) & mask == u32::from(self.subnet.0) & mask)
                    .then(|| Target::new(device.device_id.clone(), device.label().to_owned(), ip))
            })
            .collect();
    }

    fn worker_status(&self) -> Result<Option<WorkerStatus>> {
        let path = self.directory.join("status.json");
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(path)?;
        let value: serde_json::Value =
            serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes))?;
        if value["state"] == "running"
            && std::fs::metadata(self.directory.join("status.json"))?
                .modified()?
                .elapsed()
                .is_ok_and(|elapsed| elapsed > Duration::from_secs(60))
        {
            return Ok(Some(WorkerStatus {
                state: "unresponsive".into(),
                stage: "stopping".into(),
                error: "network_stopped".into(),
            }));
        }
        Ok(Some(WorkerStatus {
            state: value["state"].as_str().unwrap_or("stopped").to_owned(),
            stage: value["failureStage"]
                .as_str()
                .filter(|stage| !stage.is_empty())
                .or_else(|| value["stage"].as_str())
                .unwrap_or("preparing")
                .to_owned(),
            error: value["error"]
                .as_str()
                .filter(|text| !text.is_empty())
                .unwrap_or("network_stopped")
                .to_owned(),
        }))
    }

    pub fn status(&self) -> Result<Option<(bool, String)>> {
        Ok(self
            .worker_status()?
            .map(|status| (status.is_running(), status.error)))
    }

    pub fn stop(&mut self) -> Result<()> {
        std::fs::write(self.directory.join("stop"), b"stop")?;
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if self
                .worker_status()?
                .is_some_and(|status| status.state == "stopped")
            {
                for name in [
                    "context.json",
                    "client.toml",
                    "authorization.json",
                    "authorization.tmp",
                ] {
                    let _ = std::fs::remove_file(self.directory.join(name));
                }
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        bail!("network_stop_timeout")
    }
}

fn tick_count() -> u32 {
    #[cfg(windows)]
    {
        unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount() }
    }
    #[cfg(not(windows))]
    {
        0
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = std::fs::write(self.directory.join("stop"), b"stop");
    }
}

pub(super) fn verify_backend(root: &Path) -> Result<()> {
    use sha2::{Digest as _, Sha256};
    for (name, hash) in [
        (
            "Packet.dll",
            "ab3cce674f5216895fd26a073771f82b05d4c8b214a89f0f288a59774a06b14b",
        ),
        (
            "easytier-core.exe",
            "da7eb2d24b5416f3d3407636949e964a0750e3f9dc53a828cb6799a57ead445d",
        ),
        (
            "easytier-cli.exe",
            "d8783e851e944b44a9b71b39fd02f227ec0a2a82b3165c55ead5dd32dcde53a1",
        ),
        (
            "wintun.dll",
            "e5da8447dc2c320edc0fc52fa01885c103de8c118481f683643cacc3220dafce",
        ),
    ] {
        let bytes =
            std::fs::read(root.join(name)).map_err(|_| anyhow::anyhow!("missing_backend"))?;
        let actual = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        ensure!(actual == hash, "backend_hash_mismatch");
    }
    Ok(())
}

#[cfg(windows)]
fn powershell(script: &str, input: &[u8]) -> Result<()> {
    use base64::Engine as _;
    use std::os::windows::ffi::OsStringExt as _;
    use std::{
        io::Write as _,
        os::windows::process::CommandExt as _,
        process::{Command, Stdio},
    };
    let mut system = vec![0u16; 32768];
    let count = unsafe {
        windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(
            system.as_mut_ptr(),
            system.len() as u32,
        )
    } as usize;
    ensure!(count > 0 && count < system.len(), "powershell_unavailable");
    let executable = PathBuf::from(std::ffi::OsString::from_wide(&system[..count]))
        .join("WindowsPowerShell/v1.0/powershell.exe");
    let bytes: Vec<_> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut child = Command::new(executable)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encoded,
        ])
        .creation_flags(0x08000000)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input)?;
    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "network_launch_cancelled");
            return Ok(());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("network_start_timeout");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(windows)]
fn prepare(directory: &Path, context: &serde_json::Value) -> Result<()> {
    let script = include_str!("prepare.ps1");
    let input = zeroize::Zeroizing::new(serde_json::to_vec(context)?);
    powershell(script, &input)?;
    std::fs::write(
        directory.join("worker.ps1"),
        format!("\u{feff}{}", include_str!("worker.ps1")),
    )?;
    Ok(())
}

#[cfg(windows)]
fn launch(directory: &Path) -> Result<()> {
    let input = serde_json::to_vec(&serde_json::json!({"directory": directory}))?;
    powershell(include_str!("launch.ps1"), &input)
}

#[cfg(not(windows))]
fn prepare(_: &Path, _: &serde_json::Value) -> Result<()> {
    bail!("unsupported_platform")
}
#[cfg(not(windows))]
fn launch(_: &Path) -> Result<()> {
    bail!("unsupported_platform")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest as _, Sha256};

    fn grant() -> serde_json::Value {
        let profile = "ipv4 = '10.215.0.2/24'\nrpc_portal = '127.0.0.1:15889'\ndhcp = false\n";
        let hash = Sha256::digest(profile)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        serde_json::json!({"clientId":"client", "networkId":"network", "expiresIn":300,
            "virtualIp":"10.215.0.2", "profileHash":hash,
            "files":{"client.toml":profile,"network.json":serde_json::json!({"role":"client","rpc_portal":"127.0.0.1:15889","virtual_ip":"10.215.0.2/24"}).to_string()},
            "devices":[{"deviceId":"board","displayName":"Board","virtualIp":"10.215.0.11","port":22}]})
    }

    #[test]
    fn profiles_are_identity_bound_hashed_and_constrained_to_the_authorized_subnet() {
        let value = grant();
        assert!(validate_grant(&value, "client", "network").is_ok());
        assert!(validate_grant(&value, "other", "network").is_err());
        assert!(validate_grant(&value, "client", "other").is_err());
        for (field, replacement) in [
            ("profileHash", serde_json::json!("bad")),
            ("expiresIn", serde_json::json!(0)),
            ("virtualIp", serde_json::json!("8.8.8.8")),
        ] {
            let mut invalid = value.clone();
            invalid[field] = replacement;
            assert!(validate_grant(&invalid, "client", "network").is_err());
        }
        let mut invalid = value.clone();
        invalid["devices"][0]["virtualIp"] = "10.216.0.11".into();
        assert!(validate_grant(&invalid, "client", "network").is_err());
        invalid = value.clone();
        invalid["devices"][0]["port"] = 2222.into();
        assert!(validate_grant(&invalid, "client", "network").is_err());
        invalid = value;
        invalid["files"]["client.toml"] = "ipv4 = '10.215.0.3/24'".into();
        let hash = Sha256::digest(invalid["files"]["client.toml"].as_str().unwrap())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        invalid["profileHash"] = hash.into();
        assert!(validate_grant(&invalid, "client", "network").is_err());
    }

    #[test]
    fn client_identity_survives_restarts_and_corruption_is_not_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let id = client_id(directory.path()).unwrap();
        assert_eq!(client_id(directory.path()).unwrap(), id);
        std::fs::write(directory.path().join("client-id"), "corrupt").unwrap();
        assert!(client_id(directory.path()).is_err());
    }

    #[test]
    fn empty_online_targets_are_valid_metadata_for_an_existing_lease() {
        let mut grant = grant();
        grant["devices"] = serde_json::json!([]);
        assert!(
            validate_grant(&grant, "client", "network")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn worker_status_reports_the_current_startup_stage() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("status.json"),
            r#"{"state":"starting","stage":"interface_wait","error":"","elapsedSeconds":1.2}"#,
        )
        .unwrap();
        let running = Running::fixture(
            Connected::new(
                "server".into(),
                "network".into(),
                "Customer".into(),
                "10.215.0.2".parse().unwrap(),
                vec![],
            ),
            directory.path().into(),
        );
        let status = running.worker_status().unwrap().unwrap();
        assert_eq!(status.network_stage(), NetworkStage::WaitingForInterface);
        assert!(!status.is_running());
        std::fs::write(
            directory.path().join("status.json"),
            r#"{"state":"stopped","stage":"stopping","failureStage":"interface_wait","error":"interface_timeout"}"#,
        ).unwrap();
        let status = running.worker_status().unwrap().unwrap();
        assert_eq!(status.network_stage(), NetworkStage::WaitingForInterface);
        assert_eq!(status.error, "interface_timeout");
    }

    #[test]
    fn stop_waits_for_completed_cleanup_instead_of_accepting_starting_or_stopping() {
        for state in ["starting", "stopping"] {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(
                directory.path().join("status.json"),
                format!(r#"{{"state":"{state}"}}"#),
            )
            .unwrap();
            let mut running = Running::fixture(
                Connected::new(
                    "server".into(),
                    "network".into(),
                    "Customer".into(),
                    "10.215.0.2".parse().unwrap(),
                    vec![],
                ),
                directory.path().into(),
            );
            let path = directory.path().to_path_buf();
            let worker = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(150));
                std::fs::write(path.join("status.tmp"), r#"{"state":"stopped"}"#).unwrap();
                std::fs::rename(path.join("status.tmp"), path.join("status.json")).unwrap();
            });
            running.stop().unwrap();
            assert_eq!(running.worker_status().unwrap().unwrap().state, "stopped");
            worker.join().unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn worker_preflight_precedes_authorization_and_failed_preflight_never_revokes_a_lease() {
        for conflict in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let directory = root.path().join("worker");
            std::fs::create_dir(&directory).unwrap();
            prepare(
                &directory,
                &serde_json::json!({
                    "directory": directory, "token":"fixture-token", "parentPid":std::process::id(),
                    "packageRoot":root.path(), "serverUrl":"http://127.0.0.1", "clientId":"fixture",
                    "networkId":"network", "subnet":"10.215.0.0/24",
                }),
            )
            .unwrap();
            std::fs::write(directory.join("authorization.json"), serde_json::to_vec(&serde_json::json!({
                "authorization": {"files":{"client.toml":"fixture","network.json":"{}"}, "profileHash":"invalid"},
                "issuedAtTick":tick_count(),
            })).unwrap()).unwrap();
            let worker = include_str!("worker.ps1")
                .replace("function Adapters {", "function System-Adapters {")
                .replace("$rpc = New-Object Net.Sockets.TcpListener([Net.IPAddress]::Loopback, 15889)",
                    "$rpc = [PSCustomObject]@{}; $rpc | Add-Member ScriptMethod Start {}; $rpc | Add-Member ScriptMethod Stop {}")
                .replace(
                    "Local\\BaijieRemoteSsh-",
                    &format!("Local\\ShellLF-Test-{}-", uuid::Uuid::new_v4()),
                );
            let setup = r#"
function Get-FileHash { param($LiteralPath,$Algorithm) [PSCustomObject]@{Hash=$hashes[[IO.Path]::GetFileName($LiteralPath)]} }
function Adapters { }
function Get-NetRoute {
    param($AddressFamily)
    $prefix = if ($conflict) { '10.215.0.0/24' } else { '192.168.0.0/24' }
    [PSCustomObject]@{DestinationPrefix=$prefix;InterfaceIndex=42}
}
function New-NetFirewallRule { param($Name,$DisplayName,$Program,$Direction,$Action,$Profile) }
function Remove-NetFirewallRule { param($Name) }
function Invoke-RestMethod { param($Uri,$Method,$ContentType,$Headers,$Body,$MaximumRedirection,$TimeoutSec) $script:revoked=$true }
$script:revoked=$false
"#;
            let path = directory.to_string_lossy().replace('\'', "''");
            let test = format!(
                "{setup}\n$conflict=${conflict}\n. {{\n{worker}\n}} -StateDirectory '{path}'\n"
            );
            let test = test
                + if conflict {
                    "if ($script:revoked) { throw 'preflight revoked a lease' }\n"
                } else {
                    "if (-not $script:revoked) { throw ('grant was not revoked after profile failure: ' + [IO.File]::ReadAllText((Join-Path $StateDirectory 'status.json'))) }\n"
                };
            let test_path = root.path().join("worker-test.ps1");
            std::fs::write(&test_path, test).unwrap();
            let errors = root.path().join("test-error.txt");
            let result = powershell(
                &format!(
                    "try {{ & ([scriptblock]::Create([IO.File]::ReadAllText('{}'))) }} catch {{ [IO.File]::WriteAllText('{}', $_.ToString() + $_.ScriptStackTrace); exit 1 }}",
                    test_path.to_string_lossy().replace('\'', "''"),
                    errors.to_string_lossy().replace('\'', "''")
                ),
                &[],
            );
            assert!(
                result.is_ok(),
                "{}",
                std::fs::read_to_string(errors).unwrap_or_default()
            );
            let status: serde_json::Value = serde_json::from_str(
                std::fs::read_to_string(directory.join("status.json"))
                    .unwrap()
                    .trim_start_matches('\u{feff}'),
            )
            .unwrap();
            assert_eq!(status["state"], "stopped");
            assert_eq!(
                status["error"],
                if conflict {
                    "route_conflict"
                } else {
                    "profile_hash_mismatch"
                }
            );
            let log = std::fs::read_to_string(directory.join("diagnostics.log")).unwrap();
            if conflict {
                assert!(!log.contains("authorization"));
            } else {
                assert!(log.find("firewall").unwrap() < log.find("authorization").unwrap());
            }
            assert!(!directory.join("authorization.json").exists());
            assert!(!directory.join("context.json").exists());
            assert!(!log.contains("fixture-token"));
        }
    }

    #[cfg(windows)]
    #[test]
    fn adapter_checks_allow_only_our_inactive_routes_and_ready_adapter() {
        let script = include_str!("worker.ps1");
        let start = script.find("function Inactive-Route(").unwrap();
        let end = script.find("function Api(").unwrap();
        let test = format!(
            r#"{}
$route = [PSCustomObject]@{{InterfaceIndex=42}}
$adapter = [PSCustomObject]@{{Name='bjssh0';Description='Wintun';Index=42;State='Down';Addresses=@('10.215.0.2')}}
if (-not (Inactive-Route $route @($adapter))) {{ throw 'inactive route rejected' }}
if (Interface-Ready @($adapter) '10.215.0.2') {{ throw 'inactive adapter accepted' }}
$adapter.State = 'Up'
if (Inactive-Route $route @($adapter)) {{ throw 'active route accepted' }}
if (-not (Interface-Ready @($adapter) '10.215.0.2')) {{ throw 'ready adapter rejected' }}
if (Interface-Ready @($adapter) '10.215.0.3') {{ throw 'wrong IP accepted' }}
$adapter.Name = 'another-vpn'
$adapter.State = 'Down'
if (Inactive-Route $route @($adapter)) {{ throw 'foreign route accepted' }}
$adapter.Name = 'bjssh0'
$adapter.Index = 43
if (Inactive-Route $route @($adapter)) {{ throw 'wrong interface accepted' }}
"#,
            &script[start..end],
        );
        powershell(&test, &[]).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn core_output_bursts_do_not_delay_network_startup_or_wait_for_eof() {
        use base64::Engine as _;
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("ready");
        let elapsed = directory.path().join("elapsed");
        let child_script = format!(
            r#"$line = 'x' * 256
for ($index = 0; $index -lt 600; $index++) {{
    [Console]::Out.WriteLine($line)
    [Console]::Error.WriteLine($line)
}}
[IO.File]::WriteAllText('{}', 'ready')
Start-Sleep -Seconds 30
"#,
            marker.to_string_lossy().replace('\'', "''")
        );
        let encoded = base64::engine::general_purpose::STANDARD.encode(
            child_script
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        let script = include_str!("worker.ps1");
        let start = script.find("function Read-CoreOutput").unwrap();
        let end = script.find("function Status(").unwrap();
        let test = format!(
            r#"$ErrorActionPreference = 'Stop'
{}
$core = New-Object Diagnostics.Process
$core.StartInfo.FileName = Join-Path $PSHOME 'powershell.exe'
$core.StartInfo.Arguments = '-NoLogo -NoProfile -NonInteractive -EncodedCommand {encoded}'
$core.StartInfo.UseShellExecute = $false
$core.StartInfo.CreateNoWindow = $true
$core.StartInfo.RedirectStandardOutput = $true
$core.StartInfo.RedirectStandardError = $true
$timer = [Diagnostics.Stopwatch]::StartNew()
try {{
    if (-not $core.Start()) {{ throw 'fake core did not start' }}
    $script:outputTask = $null
    $script:errorTask = $null
    Read-CoreOutput
    Start-Sleep -Milliseconds 500
    do {{
        Read-CoreOutput
        if (Test-Path -LiteralPath '{marker}') {{ break }}
        Start-Sleep -Milliseconds 100
    }} while ($timer.Elapsed.TotalSeconds -lt 5)
    [IO.File]::WriteAllText('{elapsed}', $timer.Elapsed.TotalSeconds.ToString('F3', [Globalization.CultureInfo]::InvariantCulture))
    if (-not (Test-Path -LiteralPath '{marker}')) {{ throw 'core blocked on redirected output before readiness' }}
    if ($core.HasExited) {{ throw 'test requires a live core with no EOF' }}
    Read-CoreOutput
}} finally {{
    if (-not $core.HasExited) {{ $core.Kill(); $core.WaitForExit() }}
    $core.Dispose()
}}
"#,
            &script[start..end],
            marker = marker.to_string_lossy().replace('\'', "''"),
            elapsed = elapsed.to_string_lossy().replace('\'', "''"),
        );
        let result = powershell(&test, &[]);
        let seconds = std::fs::read_to_string(elapsed).unwrap_or_default();
        assert!(
            result.is_ok(),
            "fake core stalled for {seconds}s: {result:?}"
        );
        println!("600 stdout + 600 stderr lines drained; fake core ready in {seconds}s");
    }

    #[cfg(windows)]
    #[test]
    fn windows_tunnel_descriptions_do_not_block_an_up_adapter_with_the_leased_ip() {
        let script = include_str!("worker.ps1");
        let start = script.find("function Interface-Ready(").unwrap();
        let end = script.find("function Api(").unwrap();
        let test = format!(
            r#"{}
foreach ($description in @('bjssh0 Tunnel #2', 'bjssh0 Tunnel', 'Wintun', 'EasyTier')) {{
    $adapter = [PSCustomObject]@{{Name='bjssh0';Description=$description;Index=42;State='Up';Addresses=@('10.215.0.2')}}
    if (-not (Interface-Ready @($adapter) '10.215.0.2')) {{ throw "ready adapter rejected: $description" }}
    if (Interface-Ready @($adapter) '10.215.0.3') {{ throw 'wrong leased IP accepted' }}
    $adapter.State = 'Down'
    if (Interface-Ready @($adapter) '10.215.0.2') {{ throw 'inactive adapter accepted' }}
    $adapter.State = 'Up'
    $adapter.Name = 'another-vpn'
    if (Interface-Ready @($adapter) '10.215.0.2') {{ throw 'foreign adapter accepted' }}
}}
"#,
            &script[start..end],
        );
        powershell(&test, &[]).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn renewal_accepts_device_churn_but_rejects_identity_profile_and_lease_changes() {
        let script = include_str!("worker.ps1");
        let start = script.find("function Confirm-Renewal(").unwrap();
        let end = script[start..].find("\n}\n").unwrap() + start + 3;
        let test = format!(
            r#"{}
$original = [PSCustomObject]@{{profileHash='hash';virtualIp='10.215.0.2';devices=@('old')}}
$renewed = [PSCustomObject]@{{profileHash='hash';virtualIp='10.215.0.2';clientId='client';networkId='network';expiresIn=300;devices=@()}}
Confirm-Renewal $renewed $original 'client' 'network'
$renewed.devices = @('different-board')
Confirm-Renewal $renewed $original 'client' 'network'
foreach ($change in @(@('clientId','other'),@('networkId','other'),@('profileHash','other'),@('virtualIp','10.215.0.3'),@('expiresIn',0))) {{
    $copy = $renewed.PSObject.Copy()
    $copy.($change[0]) = $change[1]
    $rejected = $false
    try {{ Confirm-Renewal $copy $original 'client' 'network' }} catch {{ $rejected = $true }}
    if (-not $rejected) {{ exit 1 }}
}}
"#,
            &script[start..end]
        );
        powershell(&test, &[]).unwrap();
    }

    #[test]
    fn backend_verification_rejects_missing_or_modified_files() {
        let directory = tempfile::tempdir().unwrap();
        assert!(verify_backend(directory.path()).is_err());
        std::fs::write(directory.path().join("easytier-core.exe"), b"wrong binary").unwrap();
        assert!(verify_backend(directory.path()).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn worker_context_protects_the_token_without_elevation_and_stop_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let context = serde_json::json!({"directory":directory.path(),"token":"fixture-login-secret", "parentPid":std::process::id()});
        prepare(directory.path(), &context).unwrap();
        let bytes = std::fs::read(directory.path().join("context.json")).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("fixture-login-secret"));
        let json: serde_json::Value =
            serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes))
                .unwrap();
        assert!(json.get("token").is_none());
        assert!(
            json["protectedToken"]
                .as_str()
                .is_some_and(|token| !token.is_empty())
        );
        std::fs::write(
            directory.path().join("status.json"),
            "\u{feff}{\"state\":\"stopped\",\"error\":\"\"}",
        )
        .unwrap();
        let mut running = Running {
            directory: directory.path().to_path_buf(),
            subnet: ("10.215.0.0".parse().unwrap(), 24),
            info: Connected::new(
                "test".into(),
                "network".into(),
                "test".into(),
                "10.215.0.2".parse().unwrap(),
                vec![],
            ),
        };
        running.stop().unwrap();
        running.stop().unwrap();
        assert!(!directory.path().join("context.json").exists());
    }
}
