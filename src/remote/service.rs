use super::{
    api::{Account, Catalog, Server},
    network::{Connected, Running},
};
use gpui_kit::{AppContext as _, Context, EventEmitter, Task};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub enum Operation {
    Login(Server, crate::cli::Secret),
    SavedLogin(Server),
    RememberLogin(Server, crate::cli::Secret, bool),
    Forget(String),
    Refresh(String),
    Resolve(String, String, String, Option<u64>),
    Connect(String, String),
    Disconnect,
    Logout(String),
    Poll,
    Shutdown,
}

#[derive(Default)]
#[non_exhaustive]
pub struct Reply {
    pub catalog: Option<(String, Catalog)>,
    pub connection: Option<Connected>,
    pub logged_out: Option<String>,
    pub error: Option<String>,
    pub resolved: Option<ResolvedSsh>,
    pub(super) prompt: Option<(Server, Option<crate::cli::Secret>)>,
    pub(super) forgotten: Option<String>,
    pub(super) keep_connection: bool,
}

pub trait Backend: Send + Sync + 'static {
    fn perform(&self, operation: Operation) -> Reply;
}

struct State {
    accounts: HashMap<String, Account>,
    running: Option<Running>,
    root: PathBuf,
    refreshed: Option<Instant>,
    bindings: HashMap<String, (String, String, String)>,
}

#[derive(Clone)]
pub struct ResolvedSsh {
    pub server: String,
    pub network: String,
    pub device: String,
    pub user: String,
    pub address: String,
    pub name: String,
    pub terminal: Option<u64>,
    pub alias: String,
}

pub struct SystemBackend(Mutex<State>);

impl SystemBackend {
    pub fn new(root: PathBuf) -> Self {
        Self(Mutex::new(State {
            accounts: HashMap::new(),
            running: None,
            root,
            refreshed: None,
            bindings: HashMap::new(),
        }))
    }
}

impl State {
    fn refresh(&mut self, id: &str, reply: &mut Reply) -> anyhow::Result<()> {
        if self
            .running
            .as_ref()
            .is_none_or(|running| running.info.server == id)
        {
            self.refreshed = Some(Instant::now());
        }
        if let Some(running) = self
            .running
            .as_mut()
            .filter(|running| running.info.server == id)
        {
            running.info.devices.clear();
        }
        let account = self
            .accounts
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("login_required"))?;
        account.refresh()?;
        if let Some(running) = self
            .running
            .as_mut()
            .filter(|running| running.info.server == id)
        {
            running.refresh_devices(&account.catalog.devices);
        }
        reply.catalog = Some((id.to_owned(), account.catalog.clone()));
        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        if let Some(running) = &mut self.running {
            running.stop()?;
        }
        self.running = None;
        Ok(())
    }

    fn run(&mut self, operation: Operation, reply: &mut Reply) -> anyhow::Result<()> {
        match operation {
            Operation::Login(server, password) => {
                if self
                    .running
                    .as_ref()
                    .is_some_and(|running| running.info.server == server.id)
                {
                    self.stop()?;
                }
                if let Some(old) = self.accounts.remove(&server.id) {
                    let _ = old.logout();
                }
                let account = Account::login(server, password.expose())?;
                reply.catalog = Some((account.server.id.clone(), account.catalog.clone()));
                self.accounts.insert(account.server.id.clone(), account);
            }
            Operation::Refresh(id) => {
                self.refresh(&id, reply)?;
            }
            Operation::Resolve(server, device, user, terminal) => {
                self.refresh(&server, reply)?;
                let active = self
                    .running
                    .as_ref()
                    .filter(|running| running.info.server == server)
                    .ok_or_else(|| anyhow::anyhow!("device_unavailable"))?;
                let target = active
                    .info
                    .devices
                    .iter()
                    .find(|target| target.device_id == device)
                    .ok_or_else(|| anyhow::anyhow!("device_unavailable"))?;
                let network = active.info.network.clone();
                let alias = device_alias(&self.accounts[&server].server.url, &network, &device);
                self.bindings.insert(
                    alias.clone(),
                    (server.clone(), network.clone(), device.clone()),
                );
                reply.resolved = Some(ResolvedSsh {
                    server,
                    network,
                    device,
                    user,
                    terminal,
                    alias,
                    address: target.virtual_ip.to_string(),
                    name: target.display_name.clone(),
                });
            }
            Operation::Connect(server, network) => {
                if self.running.as_ref().is_some_and(|running| {
                    running.info.server == server && running.info.network == network
                }) {
                    return self.run(Operation::Poll, reply);
                }
                self.stop()?;
                self.refresh(&server, reply)?;
                let account = self
                    .accounts
                    .get(&server)
                    .ok_or_else(|| anyhow::anyhow!("login_required"))?;
                self.running = Some(Running::start(account, &network, &self.root)?);
            }
            Operation::Disconnect => self.stop()?,
            Operation::Logout(id) => {
                if self
                    .running
                    .as_ref()
                    .is_some_and(|running| running.info.server == id)
                {
                    self.stop()?;
                }
                if let Some(account) = self.accounts.remove(&id) {
                    let _ = account.logout();
                }
                reply.logged_out = Some(id);
            }
            Operation::Poll => {
                if let Some(running) = &self.running
                    && let Some((false, error)) = running.status()?
                {
                    self.running = None;
                    anyhow::bail!("{error}");
                }
                if self
                    .refreshed
                    .is_none_or(|last| last.elapsed() >= Duration::from_secs(5))
                    && let Some(server) = self
                        .running
                        .as_ref()
                        .map(|running| running.info.server.clone())
                {
                    self.refresh(&server, reply)?;
                }
            }
            Operation::Shutdown => {
                self.stop()?;
                for (_, account) in self.accounts.drain() {
                    let _ = account.logout();
                }
            }
            Operation::SavedLogin(_) | Operation::RememberLogin(..) | Operation::Forget(_) => {
                anyhow::bail!("invalid_operation");
            }
        }
        Ok(())
    }
}

pub fn device_alias(origin: &str, network: &str, device: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let hash = Sha256::digest(serde_json::to_vec(&(origin, network, device)).unwrap());
    format!(
        "shelllf-device-{}",
        hash.iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

impl shellrs_ssh::EndpointResolver for SystemBackend {
    fn resolve(&self, login: &shellrs_ssh::HostLogin) -> anyhow::Result<(String, u16)> {
        let Some(alias) = login
            .host_key_alias
            .as_ref()
            .filter(|alias| alias.starts_with("shelllf-device-"))
        else {
            return Ok((login.host.clone(), login.port));
        };
        let mut state = self.0.lock().unwrap();
        let (server, network, device) = state
            .bindings
            .get(alias)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!(crate::i18n::t!("remote.device_unavailable")))?;
        anyhow::ensure!(
            state
                .accounts
                .get(&server)
                .is_some_and(
                    |account| device_alias(&account.server.url, &network, &device) == *alias
                ),
            "{}",
            crate::i18n::t!("remote.device_unavailable")
        );
        let mut reply = Reply::default();
        if let Err(error) = state.refresh(&server, &mut reply) {
            if matches!(error.to_string().as_str(), "http_401" | "http_403") {
                let _ = state.stop();
                state.accounts.remove(&server);
            }
            anyhow::bail!(crate::i18n::t!("remote.device_unavailable"));
        }
        let running = state
            .running
            .as_ref()
            .filter(|running| running.info.server == server && running.info.network == network)
            .ok_or_else(|| anyhow::anyhow!(crate::i18n::t!("remote.connect_first")))?;
        anyhow::ensure!(
            running.status()?.is_some_and(|(running, _)| running),
            "{}",
            crate::i18n::t!("remote.connect_first")
        );
        let target = running
            .info
            .devices
            .iter()
            .find(|target| target.device_id == device)
            .ok_or_else(|| anyhow::anyhow!(crate::i18n::t!("remote.device_unavailable")))?;
        Ok((target.virtual_ip.to_string(), target.port))
    }
}

impl Backend for SystemBackend {
    fn perform(&self, operation: Operation) -> Reply {
        let mut state = self.0.lock().unwrap();
        let mut reply = Reply::default();
        let account_id = match &operation {
            Operation::Login(server, _) => Some(server.id.clone()),
            Operation::Refresh(id) | Operation::Connect(id, _) | Operation::Resolve(id, ..) => {
                Some(id.clone())
            }
            Operation::Poll => state
                .running
                .as_ref()
                .map(|running| running.info.server.clone()),
            _ => None,
        };
        let login = matches!(operation, Operation::Login(..));
        if let Err(error) = state.run(operation, &mut reply) {
            let code = error.to_string();
            if (code == "http_401" || code == "http_403")
                && state
                    .running
                    .as_ref()
                    .is_some_and(|running| Some(&running.info.server) == account_id.as_ref())
            {
                let _ = state.stop();
            }
            if (login || code == "http_401" || code == "http_403")
                && let Some(id) = account_id
            {
                state.accounts.remove(&id);
                reply.logged_out = Some(id);
            }
            reply.error = Some(code);
        }
        reply.connection = state.running.as_ref().map(|running| running.info.clone());
        reply
    }
}

#[derive(Clone)]
pub enum Event {
    NetworkChanging,
    NetworkChanged,
    Updated,
    LoginRequired(Server, Option<crate::cli::Secret>),
    Forgotten(String),
    SshReady(ResolvedSsh),
    DeviceAddressesChanged(String, Vec<String>),
}

pub struct Service {
    pub(super) backend: Option<Arc<dyn Backend>>,
    pub(super) catalogs: HashMap<String, Catalog>,
    pub(super) connection: Option<Connected>,
    pub(super) busy: bool,
    pub(super) status: String,
    pub(super) status_error: bool,
    pub(super) statuses: HashMap<String, (String, bool)>,
    pub(super) secrets: crate::secrets::SharedSecretStore,
    pub(super) pending_server: Option<String>,
    task: Option<Task<()>>,
    poll: Option<Task<()>>,
}

impl EventEmitter<Event> for Service {}

impl Service {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut service = Self {
            backend: None,
            catalogs: HashMap::new(),
            connection: None,
            busy: false,
            status: String::new(),
            status_error: false,
            statuses: HashMap::new(),
            secrets: Arc::new(crate::secrets::NoSecretStore),
            pending_server: None,
            task: None,
            poll: None,
        };
        service.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this
                    .update(cx, |this, cx| {
                        if !this.busy && this.connection.is_some() {
                            this.run(Operation::Poll, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
        service
    }

    pub fn set_backend(&mut self, backend: Arc<dyn Backend>, cx: &mut Context<Self>) {
        self.backend = Some(backend);
        cx.notify();
    }

    pub fn set_secrets(
        &mut self,
        secrets: crate::secrets::SharedSecretStore,
        cx: &mut Context<Self>,
    ) {
        self.secrets = secrets;
        cx.notify();
    }

    pub fn connection(&self) -> Option<&Connected> {
        self.connection.as_ref()
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    pub fn is_logged_in(&self, server: &str) -> bool {
        self.catalogs.contains_key(server)
    }

    pub fn ssh_target(&self, server: &str, device: &str) -> Option<(String, String)> {
        if self.busy || !self.is_logged_in(server) {
            return None;
        }
        let connection = self
            .connection
            .as_ref()
            .filter(|connection| connection.server == server)?;
        let target = connection
            .devices
            .iter()
            .find(|target| target.device_id == device)?;
        let current = self.catalogs.get(server)?.devices.iter().find(|current| {
            current.device_id == device
                && current.network_id == connection.network
                && current.lease_valid()
                && current.virtual_ip == Some(target.virtual_ip)
        })?;
        Some((
            target.virtual_ip.to_string(),
            if target.display_name.is_empty() {
                target.device_id.clone()
            } else {
                current.label().to_owned()
            },
        ))
    }

    pub fn run(&mut self, operation: Operation, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(backend) = self.backend.clone() else {
            return;
        };
        let polling = matches!(operation, Operation::Poll);
        let server_id = match &operation {
            Operation::Login(server, _)
            | Operation::SavedLogin(server)
            | Operation::RememberLogin(server, ..) => Some(server.id.clone()),
            Operation::Refresh(id)
            | Operation::Connect(id, _)
            | Operation::Logout(id)
            | Operation::Forget(id)
            | Operation::Resolve(id, ..) => Some(id.clone()),
            _ => self.connection.as_ref().map(|active| active.server.clone()),
        };
        let secrets = self.secrets.clone();
        if !polling {
            self.pending_server = server_id.clone();
            let changing = match &operation {
                Operation::Connect(..) | Operation::Disconnect => true,
                Operation::Logout(id) | Operation::Forget(id) => self
                    .connection
                    .as_ref()
                    .is_some_and(|active| &active.server == id),
                Operation::Login(server, _)
                | Operation::SavedLogin(server)
                | Operation::RememberLogin(server, ..) => self
                    .connection
                    .as_ref()
                    .is_some_and(|active| active.server == server.id),
                _ => false,
            };
            if changing {
                cx.emit(Event::NetworkChanging);
            }
            self.status = crate::i18n::t!("remote.working").to_string();
            self.status_error = false;
            if let Some(id) = &server_id {
                self.statuses
                    .insert(id.clone(), (self.status.clone(), false));
            }
        }
        self.busy = true;
        self.task = Some(cx.spawn(async move |this, cx| {
            let reply = cx
                .background_spawn(async move {
                    super::credentials::perform(backend, secrets, operation)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.pending_server = None;
                let moved = this.connection.as_ref().and_then(|old| {
                    let new = reply.connection.as_ref()?;
                    if reply.keep_connection
                        || old.server != new.server
                        || old.network != new.network
                    {
                        return None;
                    }
                    let devices: Vec<_> = old
                        .devices
                        .iter()
                        .filter(|device| {
                            !new.devices.iter().any(|target| {
                                target.device_id == device.device_id
                                    && target.virtual_ip == device.virtual_ip
                            })
                        })
                        .map(|device| device.device_id.clone())
                        .collect();
                    (!devices.is_empty()).then(|| (old.server.clone(), devices))
                });
                let changed = !reply.keep_connection
                    && this
                        .connection
                        .as_ref()
                        .map(|active| (&active.server, &active.network, active.virtual_ip))
                        != reply
                            .connection
                            .as_ref()
                            .map(|active| (&active.server, &active.network, active.virtual_ip));
                if !reply.keep_connection {
                    this.connection = reply.connection;
                }
                let catalog_refreshed = reply.catalog.is_some();
                if let Some((id, catalog)) = reply.catalog {
                    this.catalogs.insert(id, catalog);
                }
                if let Some(id) = reply.logged_out {
                    this.catalogs.remove(&id);
                }
                let failed = reply.error.is_some();
                if let Some(error) = reply.error {
                    this.status = super::panel::describe(&error).to_string();
                    this.status_error = true;
                } else if !polling || catalog_refreshed {
                    this.status = crate::i18n::t!("remote.ready").to_string();
                    this.status_error = false;
                }
                if let Some(id) = server_id
                    && (!polling || failed || catalog_refreshed)
                {
                    this.statuses
                        .insert(id, (this.status.clone(), this.status_error));
                }
                if let Some((server, password)) = reply.prompt {
                    cx.emit(Event::LoginRequired(server, password));
                }
                if let Some(id) = reply.forgotten {
                    this.statuses.remove(&id);
                    cx.emit(Event::Forgotten(id));
                }
                if let Some((server, devices)) = moved {
                    cx.emit(Event::DeviceAddressesChanged(server, devices));
                }
                if let Some(resolved) = reply.resolved {
                    cx.emit(Event::SshReady(resolved));
                }
                if changed {
                    cx.emit(Event::NetworkChanged);
                }
                cx.emit(Event::Updated);
                cx.notify();
            });
        }));
        if !polling {
            cx.notify();
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if let Some(backend) = self.backend.take() {
            let _ = std::thread::Builder::new()
                .name("shelllf-remote-stop".into())
                .spawn(move || {
                    backend.perform(Operation::Shutdown);
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read as _, Write as _},
        net::TcpListener,
        sync::atomic::{AtomicBool, Ordering},
    };
    const NETWORK: &str = "00000000-0000-4000-8000-000000000001";

    struct CatalogServer {
        url: String,
        devices: Arc<Mutex<serde_json::Value>>,
        requests: Arc<Mutex<Vec<String>>>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl CatalogServer {
        fn new() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let devices = Arc::new(Mutex::new(serde_json::json!([])));
            let requests = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let (server_devices, server_requests, server_stop) =
                (devices.clone(), requests.clone(), stop.clone());
            let thread = std::thread::spawn(move || {
                while !server_stop.load(Ordering::Acquire) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut request = Vec::new();
                    let mut buffer = [0; 2048];
                    while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                        let length = stream.read(&mut buffer).unwrap();
                        if length == 0 {
                            break;
                        }
                        request.extend_from_slice(&buffer[..length]);
                    }
                    let header = String::from_utf8_lossy(&request);
                    let path = header
                        .lines()
                        .next()
                        .unwrap()
                        .split_whitespace()
                        .nth(1)
                        .unwrap()
                        .to_owned();
                    server_requests.lock().unwrap().push(path.clone());
                    let body = match path.as_str() {
                        "/api/auth/login" => serde_json::json!({"token":"fixture-token","name":"Test"}),
                        "/api/client/ssh/networks" => serde_json::json!({"networks":[{"networkId":NETWORK,"name":"Test","subnet":"10.215.0.0/24","enabled":true,"deviceCount":2}]}),
                        path if path.starts_with("/api/client/ssh/devices?") => {
                            let devices = server_devices.lock().unwrap().clone();
                            serde_json::json!({"total":devices.as_array().unwrap().len(),"devices":devices})
                        }
                        _ => panic!("unexpected API path {path}"),
                    }.to_string();
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                }
            });
            Self {
                url,
                devices,
                requests,
                stop,
                thread: Some(thread),
            }
        }
    }

    impl Drop for CatalogServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            self.thread.take().unwrap().join().unwrap();
        }
    }

    fn device(id: &str, ip: &str) -> serde_json::Value {
        serde_json::json!({"deviceId":id,"displayName":id,"networkId":NETWORK,"virtualIp":ip,"enabled":1,"expiresAt":4102444800000u64})
    }

    #[test]
    fn live_catalog_moves_and_empty_devices_do_not_stop_network_and_resolution_never_uses_old_ip() {
        use shellrs_ssh::EndpointResolver as _;
        let fixture = CatalogServer::new();
        *fixture.devices.lock().unwrap() = serde_json::json!([device("board-a", "10.215.0.11")]);
        let server = Server::new("fixture".into(), fixture.url.clone(), "operator".into());
        let account = Account::login(server.clone(), "fixture-password").unwrap();
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("status.json"),
            r#"{"state":"running","error":""}"#,
        )
        .unwrap();
        let backend = SystemBackend::new(directory.path().into());
        {
            let mut state = backend.0.lock().unwrap();
            state.accounts.insert(server.id.clone(), account);
            state.running = Some(Running::fixture(
                Connected::new(
                    server.id.clone(),
                    NETWORK.into(),
                    "fixture".into(),
                    "10.215.0.2".parse().unwrap(),
                    vec![],
                ),
                directory.path().into(),
            ));
        }
        let reply = backend.perform(Operation::Resolve(
            server.id.clone(),
            "board-a".into(),
            "root".into(),
            None,
        ));
        assert!(reply.error.is_none());
        let resolved = reply.resolved.unwrap();
        assert_eq!(resolved.address, "10.215.0.11");
        let login = shellrs_ssh::HostLogin::new(
            "10.215.0.11",
            22,
            "root",
            shellrs_ssh::LoginMethod::Password,
        )
        .with_host_key_alias(resolved.alias.clone());
        *fixture.devices.lock().unwrap() = serde_json::json!([
            device("board-a", "10.215.0.12"),
            device("board-b", "10.215.0.11")
        ]);
        assert_eq!(backend.resolve(&login).unwrap(), ("10.215.0.12".into(), 22));
        assert_ne!(
            device_alias(&fixture.url, NETWORK, "board-a"),
            device_alias(&fixture.url, NETWORK, "board-b")
        );
        *fixture.devices.lock().unwrap() = serde_json::json!([]);
        let reply = backend.perform(Operation::Refresh(server.id.clone()));
        assert!(reply.error.is_none());
        assert!(reply.connection.unwrap().devices.is_empty());
        assert!(backend.resolve(&login).is_err());
        assert!(backend.0.lock().unwrap().running.is_some());
        *fixture.devices.lock().unwrap() = serde_json::json!([device("board-a", "10.215.0.13")]);
        assert_eq!(backend.resolve(&login).unwrap(), ("10.215.0.13".into(), 22));
        assert!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|path| path != "/api/devices" && !path.contains("token="))
        );
    }
}
