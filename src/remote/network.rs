use super::api::Account;
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    net::Ipv4Addr,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

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

    pub fn start(account: &Account, network: &str, root: &Path) -> Result<Self> {
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
        let name = account
            .catalog
            .networks
            .iter()
            .find(|item| item.network_id == network && item.enabled)
            .ok_or_else(|| anyhow::anyhow!("network_disabled"))?
            .name
            .clone();
        let client = client_id(root)?;
        let backend = std::env::current_exe()?
            .parent()
            .ok_or_else(|| anyhow::anyhow!("missing_backend"))?
            .to_path_buf();
        verify_backend(&backend)?;
        let probe = std::net::TcpListener::bind("127.0.0.1:15889")
            .map_err(|_| anyhow::anyhow!("network_in_use"))?;
        drop(probe);
        let started = Instant::now();
        let mut grant = account.request(
            "/api/client/ssh/session",
            Some(serde_json::json!({ "clientId": client, "networkId": network })),
        )?;
        let result = (|| -> Result<Self> {
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
            let directory = root.join(format!("session-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&directory)?;
            let mut running = Self {
                info: Connected {
                    server: account.server.id.clone(),
                    network: network.to_owned(),
                    name,
                    virtual_ip: grant["virtualIp"].as_str().unwrap().parse()?,
                    devices: targets,
                },
                directory,
                subnet: (address.parse()?, prefix.parse()?),
            };
            let context = serde_json::json!({
                "serverUrl": account.server.url, "token": account.token(), "clientId": client,
                "networkId": network, "authorization": grant, "parentPid": std::process::id(),
                "packageRoot": backend, "directory": running.directory,
            });
            if let Err(error) = prepare(&running.directory, &context) {
                let _ = std::fs::remove_file(running.directory.join("context.json"));
                return Err(error);
            }
            if let Err(error) = launch(&running.directory) {
                let _ = std::fs::write(running.directory.join("stop"), b"stop");
                let _ = std::fs::remove_file(running.directory.join("context.json"));
                return Err(error);
            }
            let deadline = Instant::now() + Duration::from_secs(45);
            while Instant::now() < deadline {
                match running.status()? {
                    Some((true, _)) => return Ok(running),
                    Some((false, error)) => bail!("{error}"),
                    None => std::thread::sleep(Duration::from_millis(200)),
                }
            }
            let _ = running.stop();
            bail!("network_start_timeout")
        })();
        if result.is_err() {
            let _ = account.request(
                "/api/client/ssh/disconnect",
                Some(serde_json::json!({"clientId":client,"networkId":network})),
            );
        }
        result
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

    pub fn status(&self) -> Result<Option<(bool, String)>> {
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
            return Ok(Some((false, "network_stopped".into())));
        }
        Ok(Some((
            value["state"] == "running",
            value["error"]
                .as_str()
                .filter(|text| !text.is_empty())
                .unwrap_or("network_stopped")
                .to_owned(),
        )))
    }

    pub fn stop(&mut self) -> Result<()> {
        std::fs::write(self.directory.join("stop"), b"stop")?;
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if self.status()?.is_some_and(|(running, _)| !running) {
                for name in ["context.json", "client.toml"] {
                    let _ = std::fs::remove_file(self.directory.join(name));
                }
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        bail!("network_stop_timeout")
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
