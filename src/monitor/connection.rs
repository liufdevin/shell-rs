use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use crate::{
    adb::{AdbConnection, AdbDetection, AdbDetector, canonical_boot_id},
    host::{HostLogin, MonitorChannel, MonitorSettings},
    i18n::t,
    ssh::{SshConnectionConfig, SshConnector, SshHandle, SshPrompts, run_exec},
};

pub trait MonitorLink: Send {
    fn label(&self) -> String;
    fn sample(&mut self, command: &str, cancel: &AtomicBool) -> Result<String, String>;
    fn adb_connection(&self) -> Option<AdbConnection> {
        None
    }
}

pub trait MonitorConnector: Send + Sync {
    fn connect(
        &self,
        identity: &str,
        settings: &MonitorSettings,
        candidates: &[(String, HostLogin)],
        cancel: &AtomicBool,
    ) -> Result<Option<Box<dyn MonitorLink>>, String>;
}

pub struct SystemMonitorConnector {
    adb: Arc<dyn AdbDetector>,
    ssh: Option<SshConnector>,
}

impl SystemMonitorConnector {
    pub fn new(adb: Arc<dyn AdbDetector>, ssh: Option<SshConnector>) -> Self {
        Self { adb, ssh }
    }
}

pub(crate) fn boot_identity(output: &str) -> Option<String> {
    output
        .split_once("@@boot_id\n")
        .and_then(|(_, tail)| tail.lines().next())
        .and_then(canonical_boot_id)
}

fn select_adb(detection: AdbDetection, boot: Option<&str>, serial: &str) -> Option<AdbConnection> {
    let devices = match detection {
        AdbDetection::Matched(device) => vec![device],
        AdbDetection::Choose(devices) => devices,
        AdbDetection::Unavailable(_) => return None,
    };
    let mut matches = devices.into_iter().filter(|device| {
        if !serial.is_empty() && device.serial() != serial {
            return false;
        }
        match boot {
            Some(boot) => device.boot_id() == Some(boot),
            None => !serial.is_empty(),
        }
    });
    let selected = matches.next()?;
    matches.next().is_none().then_some(selected)
}

impl MonitorConnector for SystemMonitorConnector {
    fn connect(
        &self,
        identity: &str,
        settings: &MonitorSettings,
        candidates: &[(String, HostLogin)],
        cancel: &AtomicBool,
    ) -> Result<Option<Box<dyn MonitorLink>>, String> {
        let boot = boot_identity(identity);
        if cancel.load(Ordering::Acquire) {
            return Ok(None);
        }
        let (_, addresses) = crate::serial::files::network_candidates(identity);
        let mut problem = None;
        if matches!(settings.channel, MonitorChannel::Auto | MonitorChannel::Ssh)
            && let Some(connector) = &self.ssh
        {
            for (id, login) in candidates {
                let explicit = !settings.ssh_host.is_empty();
                if (explicit && id != &settings.ssh_host)
                    || (!explicit
                        && !addresses
                            .iter()
                            .any(|address| address.to_string() == login.host))
                    || (boot.is_none() && !explicit)
                    || cancel.load(Ordering::Acquire)
                {
                    continue;
                }
                match SshMonitor::connect(connector, login, boot.as_deref(), cancel) {
                    Ok(link) => return Ok(Some(Box::new(link))),
                    Err(error) => problem = Some(error),
                }
            }
        }
        if settings.channel == MonitorChannel::Ssh {
            return Err(problem.unwrap_or_else(|| t!("monitor.route.ssh_unavailable").to_string()));
        }
        if cancel.load(Ordering::Acquire) {
            return Ok(None);
        }
        if matches!(settings.channel, MonitorChannel::Auto | MonitorChannel::Adb) {
            if let Some(device) = select_adb(
                self.adb.detect(boot.as_deref()),
                boot.as_deref(),
                &settings.adb_serial,
            ) && device.monitor_command(":", cancel).is_ok()
            {
                return Ok(Some(Box::new(device)));
            }
            if settings.channel == MonitorChannel::Adb {
                return Err(t!("monitor.route.adb_unmatched").to_string());
            }
        }
        Ok(None)
    }
}

impl MonitorLink for AdbConnection {
    fn adb_connection(&self) -> Option<AdbConnection> {
        Some(self.clone())
    }
    fn label(&self) -> String {
        format!("ADB · {} ({})", self.display_name(), self.serial())
    }
    fn sample(&mut self, command: &str, cancel: &AtomicBool) -> Result<String, String> {
        self.monitor_command(command, cancel)
            .map_err(|error| error.to_string())
    }
}

struct SshMonitor {
    runtime: tokio::runtime::Runtime,
    handle: SshHandle,
    endpoint: String,
    boot: Option<String>,
    _shutdown: tokio::sync::watch::Sender<bool>,
}

impl SshMonitor {
    fn connect(
        connector: &SshConnector,
        login: &HostLogin,
        boot: Option<&str>,
        cancel: &AtomicBool,
    ) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        let (shutdown, receiver) = tokio::sync::watch::channel(false);
        let prompts = Arc::new(SshPrompts::new(Arc::new(|_| false), receiver).non_interactive());
        let config = SshConnectionConfig::from(login);
        let (handle, _) = runtime.block_on(async {
            tokio::select! {
                result = tokio::time::timeout(Duration::from_secs(8), connector.connect(&config, prompts)) => {
                    result.map_err(|_| t!("monitor.route.ssh_unavailable").to_string())?
                        .map_err(|error| crate::ssh::describe_login_error(&error))
                }
                _ = cancelled(cancel) => Err(t!("adb.interrupted").to_string()),
            }
        })?;
        let mut link = Self {
            runtime,
            handle,
            endpoint: format!("{}@{}:{}", login.user, login.host, login.port),
            boot: boot.map(str::to_owned),
            _shutdown: shutdown,
        };
        link.sample(":", cancel)?;
        Ok(link)
    }
}

async fn cancelled(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

impl MonitorLink for SshMonitor {
    fn label(&self) -> String {
        format!("SSH · {}", self.endpoint)
    }
    fn sample(&mut self, command: &str, cancel: &AtomicBool) -> Result<String, String> {
        let command = match &self.boot {
            Some(boot) => format!(
                "if [ \"$(cat /proc/sys/kernel/random/boot_id)\" = '{boot}' ]; then printf '__shelllf_monitor_identity_ok__\\n'; {command}\nfi"
            ),
            None => command.to_owned(),
        };
        let output = self.runtime.block_on(async {
            tokio::select! {
                output = run_exec(&self.handle, &command, 4 * 1024 * 1024, Duration::from_secs(10)) => output.map_err(|error| error.to_string()),
                _ = cancelled(cancel) => Err(t!("adb.interrupted").to_string()),
            }
        })?;
        let output = String::from_utf8(output).map_err(|error| error.to_string())?;
        if self.boot.is_some() {
            output
                .strip_prefix("__shelllf_monitor_identity_ok__\n")
                .map(str::to_owned)
                .ok_or_else(|| t!("adb.device_changed").to_string())
        } else {
            Ok(output)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::server::{self, Server as _};
    use std::sync::atomic::AtomicUsize;
    const FIRST: &str = "00000000-0000-4000-8000-000000000001";
    const SECOND: &str = "00000000-0000-4000-8000-000000000002";

    fn devices() -> AdbDetection {
        AdbDetection::Choose(vec![
            AdbConnection::monitor_test_connection("board-b", Some(SECOND)),
            AdbConnection::monitor_test_connection("board-a", Some(FIRST)),
        ])
    }

    #[test]
    fn multiple_serial_boards_match_by_boot_identity_not_device_order() {
        assert_eq!(
            select_adb(devices(), Some(FIRST), "").unwrap().serial(),
            "board-a"
        );
        assert_eq!(
            select_adb(devices(), Some(SECOND), "").unwrap().serial(),
            "board-b"
        );
        assert!(select_adb(devices(), Some(FIRST), "board-b").is_none());
        assert!(select_adb(devices(), None, "").is_none());
        assert_eq!(
            select_adb(devices(), None, "board-b").unwrap().serial(),
            "board-b"
        );
    }

    #[test]
    fn duplicate_or_missing_identities_require_explicit_selection() {
        let duplicate = || {
            AdbDetection::Choose(vec![
                AdbConnection::monitor_test_connection("usb", Some(FIRST)),
                AdbConnection::monitor_test_connection("tcp", Some(FIRST)),
            ])
        };
        assert!(select_adb(duplicate(), Some(FIRST), "").is_none());
        assert_eq!(
            select_adb(duplicate(), Some(FIRST), "usb")
                .unwrap()
                .serial(),
            "usb"
        );
        assert!(
            select_adb(
                AdbDetection::Matched(AdbConnection::monitor_test_connection(
                    "other",
                    Some(SECOND)
                )),
                Some(FIRST),
                ""
            )
            .is_none()
        );
        assert_eq!(
            boot_identity(&format!("@@boot_id\n{FIRST}\n@@user\nroot\n")),
            Some(FIRST.into())
        );
        assert_eq!(boot_identity("@@boot_id\nnot-a-uuid\n"), None);
    }

    #[derive(Clone)]
    struct Board {
        connections: Arc<AtomicUsize>,
        rebooted: Arc<AtomicBool>,
    }

    struct UnexpectedAdb;
    impl AdbDetector for UnexpectedAdb {
        fn detect(&self, _: Option<&str>) -> AdbDetection {
            panic!("ADB must not be probed when SSH is available");
        }
    }

    impl server::Server for Board {
        type Handler = Self;
        fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self {
            self.connections.fetch_add(1, Ordering::SeqCst);
            self.clone()
        }
    }

    impl server::Handler for Board {
        type Error = anyhow::Error;
        async fn auth_none(&mut self, _: &str) -> anyhow::Result<server::Auth> {
            Ok(server::Auth::Accept)
        }
        async fn channel_open_session(
            &mut self,
            _: russh::Channel<server::Msg>,
            reply: server::ChannelOpenHandle,
            _: &mut server::Session,
        ) -> anyhow::Result<()> {
            reply.accept().await;
            Ok(())
        }
        async fn exec_request(
            &mut self,
            channel: russh::ChannelId,
            command: &[u8],
            session: &mut server::Session,
        ) -> anyhow::Result<()> {
            let command = std::str::from_utf8(command)?;
            assert!(command.contains("/proc/sys/kernel/random/boot_id"));
            session.channel_success(channel)?;
            if command.contains(&format!("= '{FIRST}'")) && !self.rebooted.load(Ordering::Acquire) {
                session.data(
                    channel,
                    b"__shelllf_monitor_identity_ok__\nboard output".to_vec(),
                )?;
            }
            session.eof(channel)?;
            session.close(channel)?;
            Ok(())
        }
    }

    #[test]
    fn ssh_monitor_reuses_its_connection_and_rejects_unknown_keys_and_changed_boards() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind(("127.0.0.1", 0)))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let key = russh::keys::PrivateKey::random(
            &mut russh::keys::key::safe_rng(),
            russh::keys::Algorithm::Ed25519,
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let known = directory.path().join("known_hosts");
        let public = key.public_key().clone();
        let connections = Arc::new(AtomicUsize::new(0));
        let rebooted = Arc::new(AtomicBool::new(false));
        let mut board = Board {
            connections: connections.clone(),
            rebooted: rebooted.clone(),
        };
        runtime.spawn(async move {
            board
                .run_on_socket(
                    Arc::new(server::Config {
                        keys: vec![key],
                        ..Default::default()
                    }),
                    &listener,
                )
                .await
                .unwrap();
        });
        let connector = SshConnector::new(
            &known,
            crate::secrets::KeychainSource::shared(Arc::new(
                crate::secrets::InMemorySecretStore::default(),
            )),
        );
        let login = HostLogin::new(
            "127.0.0.1",
            port,
            "root",
            crate::host::LoginMethod::NoPassword,
        );
        let cancel = AtomicBool::new(false);
        assert!(SshMonitor::connect(&connector, &login, Some(FIRST), &cancel).is_err());
        russh::keys::known_hosts::learn_known_hosts_path("127.0.0.1", port, &public, &known)
            .unwrap();
        let provider =
            SystemMonitorConnector::new(Arc::new(UnexpectedAdb), Some(connector.clone()));
        let mut settings = MonitorSettings::default();
        settings.ssh_host = "saved-board".into();
        let fallback = provider
            .connect(
                &format!("@@boot_id\n{FIRST}\n@@user\nroot\n@@addresses\n"),
                &settings,
                &[("saved-board".into(), login.clone())],
                &cancel,
            )
            .unwrap()
            .unwrap();
        assert!(fallback.label().starts_with("SSH · root@127.0.0.1:"));
        drop(fallback);
        let mut link = SshMonitor::connect(&connector, &login, Some(FIRST), &cancel).unwrap();
        let count = connections.load(Ordering::SeqCst);
        assert_eq!(
            link.sample("cat /proc/stat", &cancel).unwrap(),
            "board output"
        );
        assert_eq!(
            link.sample("cat /proc/meminfo", &cancel).unwrap(),
            "board output"
        );
        assert_eq!(connections.load(Ordering::SeqCst), count);
        rebooted.store(true, Ordering::Release);
        assert!(link.sample("cat /proc/stat", &cancel).is_err());
        assert!(SshMonitor::connect(&connector, &login, Some(SECOND), &cancel).is_err());
    }
}
