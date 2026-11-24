use anyhow::{Result, bail, ensure};
use reqwest::{Url, blocking::Client, redirect::Policy};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr},
    time::Duration,
};
use zeroize::Zeroizing;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Server {
    pub id: String,
    pub name: String,
    pub url: String,
    pub username: String,
}

impl Server {
    pub fn new(name: String, url: String, username: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            url,
            username,
        }
    }
    pub fn validate(&self) -> Result<()> {
        uuid::Uuid::parse_str(&self.id)?;
        ensure!(
            !self.name.trim().is_empty() && self.name.len() <= 128,
            "invalid_server"
        );
        ensure!(
            !self.username.trim().is_empty() && self.username.len() <= 256,
            "invalid_server"
        );
        server_url(&self.url)?;
        Ok(())
    }
}

pub(super) fn server_url(text: &str) -> Result<String> {
    let url = Url::parse(text).map_err(|_| anyhow::anyhow!("invalid_server"))?;
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none()
            && url.host_str().is_some(),
        "invalid_server"
    );
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && loopback),
        "https_required"
    );
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

pub(super) fn null_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<String, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

fn device_enabled<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<bool, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Flag {
        Boolean(bool),
        Integer(u8),
    }
    match Flag::deserialize(deserializer)? {
        Flag::Boolean(value) => Ok(value),
        Flag::Integer(0) => Ok(false),
        Flag::Integer(1) => Ok(true),
        Flag::Integer(_) => Err(serde::de::Error::custom("expected boolean or 0/1")),
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Device {
    pub device_id: String,
    #[serde(default, deserialize_with = "null_string")]
    pub display_name: String,
    #[serde(default, deserialize_with = "null_string")]
    pub name: String,
    #[serde(default, deserialize_with = "null_string")]
    pub customer_name: String,
    #[serde(default)]
    pub virtual_ip: Option<Ipv4Addr>,
    #[serde(default, deserialize_with = "null_string")]
    pub status: String,
    #[serde(default, deserialize_with = "device_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub network_id: String,
    #[serde(default)]
    pub expires_at: u64,
}

impl Device {
    pub fn lease_valid(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|time| time.as_millis() as u64)
            .unwrap_or(u64::MAX);
        self.enabled && self.virtual_ip.is_some() && self.expires_at > now
    }

    pub fn label(&self) -> &str {
        if !self.display_name.is_empty() {
            &self.display_name
        } else if !self.name.is_empty() {
            &self.name
        } else {
            &self.device_id
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Network {
    pub network_id: String,
    pub name: String,
    pub subnet: String,
    pub enabled: bool,
    #[serde(default)]
    pub device_count: usize,
}

#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Catalog {
    pub name: String,
    pub username: String,
    pub role: String,
    pub devices: Vec<Device>,
    pub networks: Vec<Network>,
}

pub(super) struct Account {
    pub server: Server,
    token: Zeroizing<String>,
    pub catalog: Catalog,
    client: Client,
}

impl Account {
    pub fn login(server: Server, password: &str) -> Result<Self> {
        server.validate()?;
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(8))
            .build()?;
        let mut account = Self {
            server,
            token: Zeroizing::new(String::new()),
            catalog: Catalog::default(),
            client,
        };
        let value = account.request(
            "/api/auth/login",
            Some(serde_json::json!({
                "username": account.server.username, "password": password
            })),
        )?;
        account.token = Zeroizing::new(
            value["token"]
                .as_str()
                .filter(|token| !token.is_empty() && token.len() <= 16384)
                .ok_or_else(|| anyhow::anyhow!("invalid_response"))?
                .to_owned(),
        );
        account.catalog.name = value["name"]
            .as_str()
            .unwrap_or(&account.server.username)
            .to_owned();
        account.catalog.username = value["username"]
            .as_str()
            .unwrap_or(&account.server.username)
            .to_owned();
        account.catalog.role = value["role"].as_str().unwrap_or_default().to_owned();
        if let Err(error) = account.refresh() {
            let _ = account.logout();
            return Err(error);
        }
        Ok(account)
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn request(
        &self,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let url = format!("{}{path}", server_url(&self.server.url)?);
        let mut request = if body.is_some() {
            self.client.post(url)
        } else {
            self.client.get(url)
        };
        request = request
            .header("X-Baijie-Request", "1")
            .header("X-Baijie-Client", "native");
        if !self.token.is_empty() {
            request = request.bearer_auth(self.token.as_str());
        }
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/json")
                .body(serde_json::to_vec(&body)?);
        }
        let response = request
            .send()
            .map_err(|_| anyhow::anyhow!("server_unreachable"))?;
        let status = response.status();
        let bytes = {
            use std::io::Read as _;
            let mut bytes = Vec::new();
            response.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
            Zeroizing::new(bytes)
        };
        ensure!(bytes.len() <= 4 * 1024 * 1024, "invalid_response");
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid_response"))?;
        if !status.is_success() {
            let code = value["error"].as_str().unwrap_or("request_failed");
            if [
                "client_in_use",
                "client_acl_not_configured",
                "client_address_pool_exhausted",
                "network_disabled",
                "no_authorized_devices",
                "not_ready",
            ]
            .contains(&code)
            {
                bail!("{code}");
            }
            bail!("http_{}", status.as_u16());
        }
        Ok(value)
    }

    pub fn refresh(&mut self) -> Result<()> {
        let mut devices = Vec::new();
        let mut seen = HashSet::new();
        let mut addresses = HashSet::new();
        let mut offset = 0usize;
        loop {
            let page = self.request(
                &format!("/api/client/ssh/devices?limit=100&offset={offset}"),
                None,
            )?;
            let total = page["total"]
                .as_u64()
                .filter(|total| *total <= 10000)
                .ok_or_else(|| anyhow::anyhow!("invalid_response"))?
                as usize;
            let rows: Vec<Device> = serde_json::from_value(page["devices"].clone())
                .map_err(|_| anyhow::anyhow!("invalid_device_response"))?;
            ensure!(
                rows.len() <= 100 && (!rows.is_empty() || offset >= total),
                "invalid_response"
            );
            offset += rows.len();
            for mut device in rows {
                device.device_id.make_ascii_lowercase();
                ensure!(
                    !device.device_id.is_empty() && device.device_id.len() <= 256,
                    "invalid_response"
                );
                uuid::Uuid::parse_str(&device.network_id)
                    .map_err(|_| anyhow::anyhow!("invalid_device_response"))?;
                if device.lease_valid() && seen.insert(device.device_id.clone()) {
                    ensure!(
                        addresses.insert((device.network_id.clone(), device.virtual_ip.unwrap())),
                        "invalid_device_response"
                    );
                    devices.push(device);
                }
            }
            if offset >= total {
                break;
            }
            ensure!(offset <= 10000, "invalid_response");
        }
        let value = self.request("/api/client/ssh/networks", None)?;
        let networks: Vec<Network> = serde_json::from_value(value["networks"].clone())
            .map_err(|_| anyhow::anyhow!("invalid_network_response"))?;
        ensure!(networks.len() <= 1000, "invalid_response");
        for network in &networks {
            uuid::Uuid::parse_str(&network.network_id)
                .map_err(|_| anyhow::anyhow!("invalid_response"))?;
        }
        self.catalog.devices = devices;
        self.catalog.networks = networks;
        Ok(())
    }

    pub fn logout(&self) -> Result<()> {
        self.request("/api/auth/logout", Some(serde_json::json!({})))?;
        Ok(())
    }

    pub fn resolve_screen(
        &self,
        device_id: &str,
        device_name: &str,
    ) -> Result<super::RemoteScreenSession> {
        let base = server_url(&self.server.url)?;
        let body = serde_json::json!({ "deviceId": device_id, "expiresIn": 300 });
        match self.request("/api/client/view-links", Some(body.clone())) {
            Ok(value) => {
                let url = screen_url_from_link_response(&base, device_id, &value)?;
                return Ok(super::RemoteScreenSession {
                    server_id: self.server.id.clone(),
                    device_id: device_id.to_owned(),
                    device_name: device_name.to_owned(),
                    url,
                    fallback_bearer: None,
                });
            }
            Err(error) if error.to_string() == "http_404" => {}
            Err(error) => return Err(error),
        }
        // Fallback for older baijie-remote servers before `/api/client/view-links`:
        // platform admins can use `/api/platform/access-links`, and regular customers
        // verify access with `/api/view-tickets` and bootstrap the WebView session.
        if self.catalog.role == "platform"
            && let Ok(value) = self.request("/api/platform/access-links", Some(body))
            && let Ok(url) = screen_url_from_link_response(&base, device_id, &value)
        {
            return Ok(super::RemoteScreenSession {
                server_id: self.server.id.clone(),
                device_id: device_id.to_owned(),
                device_name: device_name.to_owned(),
                url,
                fallback_bearer: None,
            });
        }
        let _ = self.request(
            "/api/view-tickets",
            Some(serde_json::json!({ "deviceId": device_id })),
        )?;
        Ok(super::RemoteScreenSession {
            server_id: self.server.id.clone(),
            device_id: device_id.to_owned(),
            device_name: device_name.to_owned(),
            url: format!("{base}/"),
            fallback_bearer: Some(self.token.as_str().to_owned()),
        })
    }
}

fn screen_url_from_link_response(
    base: &str,
    device_id: &str,
    value: &serde_json::Value,
) -> Result<String> {
    if let Some(access) = value["access"].as_str().filter(|token| !token.is_empty()) {
        return Ok(format!("{base}/#device={device_id}&access={access}"));
    }
    let url = value["url"]
        .as_str()
        .filter(|url| !url.is_empty())
        .ok_or_else(|| anyhow::anyhow!("invalid_response"))?;
    if url.starts_with('/') {
        Ok(format!("{base}{url}"))
    } else {
        Ok(url.to_owned())
    }
}

fn anonymous_post(base: &str, path: &str, body: serde_json::Value) -> Result<serde_json::Value> {
    let client = Client::builder()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(8))
        .build()?;
    let response = client
        .post(format!("{base}{path}"))
        .header("X-Baijie-Request", "1")
        .header("X-Baijie-Client", "native")
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(&body)?)
        .send()
        .map_err(|_| anyhow::anyhow!("server_unreachable"))?;
    let status = response.status();
    let bytes = {
        use std::io::Read as _;
        let mut bytes = Vec::new();
        response.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
        Zeroizing::new(bytes)
    };
    ensure!(bytes.len() <= 1024 * 1024, "invalid_response");
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid_response"))?;
    if !status.is_success() {
        let code = value["error"].as_str().unwrap_or("request_failed");
        if [
            "debug_unavailable",
            "rate_limited",
            "busy",
            "conflict",
            "invalid_params",
            "https_required",
            "forbidden",
            "unavailable",
        ]
        .contains(&code)
        {
            bail!("{code}");
        }
        bail!("http_{}", status.as_u16());
    }
    Ok(value)
}

/// Exchange a device ID (8-character short ID or 32-character chip ID) and
/// temporary debug key for a standalone remote screen session.
pub fn connect_debug_screen(
    url: &str,
    device_id: &str,
    key: &str,
) -> Result<super::RemoteScreenSession> {
    let base = server_url(url)?;
    let device_id = device_id.trim();
    let key = key.trim();
    ensure!(
        !device_id.is_empty() && !key.is_empty() && device_id.len() <= 64 && key.len() <= 64,
        "invalid_params"
    );
    let value = anonymous_post(
        &base,
        "/api/debug/connect",
        serde_json::json!({ "deviceId": device_id, "key": key }),
    )?;
    let resolved_id = value["deviceId"]
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 64)
        .ok_or_else(|| anyhow::anyhow!("invalid_response"))?
        .to_owned();
    let raw_url = value["url"]
        .as_str()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| anyhow::anyhow!("invalid_response"))?;
    let target_url = if raw_url.starts_with('/') {
        format!("{base}{raw_url}")
    } else {
        raw_url.to_owned()
    };
    let parsed_base = Url::parse(&base)?;
    let parsed_target = Url::parse(&target_url).map_err(|_| anyhow::anyhow!("invalid_response"))?;
    ensure!(
        parsed_target.origin() == parsed_base.origin(),
        "invalid_response"
    );
    let display_name = value["shortId"]
        .as_str()
        .filter(|short| !short.is_empty())
        .map(format_short_id)
        .unwrap_or_else(|| resolved_id.clone());
    Ok(super::RemoteScreenSession {
        server_id: format!("debug:{base}"),
        device_id: resolved_id,
        device_name: display_name,
        url: target_url,
        fallback_bearer: None,
    })
}

fn format_short_id(short: &str) -> String {
    let clean: String = short
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_uppercase())
        .collect();
    if clean.len() == 8 {
        format!("{}-{}", &clean[..4], &clean[4..])
    } else {
        short.to_owned()
    }
}

/// Submit a customer account registration request to a `baijie-remote` server.
pub fn register_account(url: &str, name: &str, username: &str, password: &str) -> Result<String> {
    let base = server_url(url)?;
    let name = name.trim();
    let username = username.trim();
    ensure!(
        !name.is_empty() && name.len() <= 100 && !username.is_empty() && username.len() <= 64,
        "invalid_params"
    );
    ensure!(
        password.len() >= 8 && password.len() <= 128,
        "invalid_params"
    );
    let value = anonymous_post(
        &base,
        "/api/auth/register",
        serde_json::json!({
            "name": name,
            "username": username,
            "password": password,
        }),
    )?;
    ensure!(
        value["status"].as_str() == Some("pending"),
        "invalid_response"
    );
    Ok(username.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_devices_require_a_current_ip_and_unexpired_enabled_lease() {
        let mut device = Device {
            enabled: true,
            virtual_ip: Some("10.215.0.11".parse().unwrap()),
            expires_at: 4102444800000,
            ..Device::default()
        };
        assert!(device.lease_valid());
        device.expires_at = 1;
        assert!(!device.lease_valid());
        device.expires_at = 4102444800000;
        device.enabled = false;
        assert!(!device.lease_valid());
        device.enabled = true;
        device.virtual_ip = None;
        assert!(!device.lease_valid());
    }

    #[test]
    fn device_flags_accept_booleans_and_sqlite_zero_one_values() {
        for (flag, expected) in [("true", true), ("false", false), ("1", true), ("0", false)] {
            let device: Device =
                serde_json::from_str(&format!(r#"{{"deviceId":"board","enabled":{flag}}}"#))
                    .unwrap();
            assert_eq!(device.enabled, expected);
        }
        for flag in ["2", "-1", "null", r#""1""#] {
            assert!(
                serde_json::from_str::<Device>(&format!(
                    r#"{{"deviceId":"board","enabled":{flag}}}"#
                ))
                .is_err()
            );
        }
    }

    #[test]
    #[ignore = "requires explicitly provided test server credentials; reads catalogs and logs out"]
    fn live_server_login_reads_catalog_and_logs_out() {
        let server = Server::new(
            "test".into(),
            std::env::var("SHELLLF_REMOTE_TEST_URL").unwrap(),
            std::env::var("SHELLLF_REMOTE_TEST_USER").unwrap(),
        );
        let password = Zeroizing::new(std::env::var("SHELLLF_REMOTE_TEST_PASSWORD").unwrap());
        let account = Account::login(server, &password).unwrap();
        println!(
            "Login OK; {} devices, {} networks; no network started",
            account.catalog.devices.len(),
            account.catalog.networks.len()
        );
        account.logout().unwrap();
    }
    #[test]
    fn server_addresses_require_final_https_origins() {
        assert_eq!(
            server_url("https://example.test:8180/").unwrap(),
            "https://example.test:8180"
        );
        assert!(server_url("http://127.0.0.1:1234").is_ok());
        for url in [
            "http://example.test",
            "https://user:secret@example.test",
            "https://example.test/path",
            "https://example.test/?token=x",
            "file:///etc/passwd",
        ] {
            assert!(server_url(url).is_err());
        }
    }

    #[test]
    fn login_uses_native_headers_pins_origin_and_reads_the_server_catalog() {
        use std::{
            io::{Read as _, Write as _},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let replies = [
                r#"{"token":"fixture-token","username":"operator","name":"Customer","role":"customer"}"#,
                r#"{"total":1,"devices":[{"deviceId":"board","displayName":"Board","virtualIp":"10.215.0.11","enabled":1,"networkId":"00000000-0000-4000-8000-000000000001","expiresAt":4102444800000}]}"#,
                r#"{"networks":[{"networkId":"00000000-0000-4000-8000-000000000001","name":"Customer","subnet":"10.215.0.0/24","enabled":true,"deviceCount":1}]}"#,
                r#"{}"#,
            ];
            for (index, reply) in replies.iter().enumerate() {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 1024];
                while !bytes.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let read = stream.read(&mut buffer).unwrap();
                    assert!(read > 0);
                    bytes.extend_from_slice(&buffer[..read]);
                }
                let request = String::from_utf8_lossy(&bytes).to_lowercase();
                assert!(
                    request.contains("x-baijie-client: native")
                        && request.contains("x-baijie-request: 1")
                );
                if index == 1 {
                    assert!(request.starts_with("get /api/client/ssh/devices?limit=100&offset=0 "));
                }
                if index > 0 {
                    assert!(request.contains("authorization: bearer fixture-token"));
                }
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}", reply.len(), reply).unwrap();
            }
        });
        let account = Account::login(
            Server::new("test".into(), url, "operator".into()),
            "test-password",
        )
        .unwrap();
        assert_eq!(account.catalog.name, "Customer");
        assert_eq!(account.catalog.devices.len(), 1);
        assert_eq!(account.catalog.networks.len(), 1);
        account.logout().unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn resolve_screen_uses_client_view_link_and_falls_back_to_view_tickets() {
        use std::{
            io::{Read as _, Write as _},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let replies = [
                (
                    "200 OK",
                    r#"{"token":"tok-1","username":"operator","name":"Customer","role":"customer"}"#,
                ),
                ("200 OK", r#"{"total":0,"devices":[]}"#),
                ("200 OK", r#"{"networks":[]}"#),
                // First resolve_screen: server supports /api/client/view-links
                (
                    "200 OK",
                    r#"{"url":"/#device=board01&access=abc123","deviceId":"board01","access":"abc123","expiresIn":300}"#,
                ),
                // Second resolve_screen: older server returns 404 on /api/client/view-links,
                // then succeeds on /api/view-tickets.
                ("404 Not Found", r#"{"error":"not_found"}"#),
                ("200 OK", r#"{"ticket":"tkt-999","expiresIn":30}"#),
            ];
            for (status, body) in replies {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 1024];
                while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    let read = stream.read(&mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..read]);
                }
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let account = Account::login(
            Server::new("test".into(), url.clone(), "operator".into()),
            "pw",
        )
        .unwrap();
        let direct = account.resolve_screen("board01", "开发板-01").unwrap();
        assert_eq!(direct.url, format!("{url}/#device=board01&access=abc123"));
        assert!(direct.fallback_bearer.is_none());

        let fallback = account.resolve_screen("board01", "开发板-01").unwrap();
        assert_eq!(fallback.url, format!("{url}/"));
        assert_eq!(fallback.fallback_bearer.as_deref(), Some("tok-1"));
        worker.join().unwrap();
    }

    #[test]
    fn debug_screen_and_account_registration_talk_to_server_endpoints() {
        use std::{
            io::{Read as _, Write as _},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let expected_url = format!(
            "{url}/#device=0123456789abcdef0123456789abcdef&access=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        let debug_reply = serde_json::json!({
            "deviceId": "0123456789abcdef0123456789abcdef",
            "shortId": "7K9M2R8X",
            "url": expected_url,
            "expiresIn": 3600
        })
        .to_string();
        let register_reply =
            r#"{"requestId":"req-1","username":"new_user","name":"测试客户","status":"pending"}"#
                .to_owned();
        let worker = std::thread::spawn(move || {
            for (index, (status, body)) in
                [("200 OK", debug_reply), ("202 Accepted", register_reply)]
                    .into_iter()
                    .enumerate()
            {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 1024];
                while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    let read = stream.read(&mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..read]);
                }
                let header = String::from_utf8_lossy(&bytes).to_lowercase();
                if index == 0 {
                    assert!(header.starts_with("post /api/debug/connect "));
                } else {
                    assert!(header.starts_with("post /api/auth/register "));
                }
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let session = connect_debug_screen(&url, "7K9M-2R8X", "5W3P-6T4H").unwrap();
        assert_eq!(session.device_id, "0123456789abcdef0123456789abcdef");
        assert_eq!(session.device_name, "7K9M-2R8X");
        assert!(session.fallback_bearer.is_none());

        let registered = register_account(&url, "测试客户", "new_user", "strong-pass-123").unwrap();
        assert_eq!(registered, "new_user");
        worker.join().unwrap();
    }
}
