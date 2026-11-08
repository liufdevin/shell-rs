use std::io;
use std::sync::{
    RwLock,
    atomic::{AtomicBool, Ordering},
};

use shellrs_lan::{Executor, SharedHost};

use crate::host::{HostLogin, HostStore};
use crate::ssh::{
    ExecErrorKind, ExecExit, ExecStream, SshConnectionConfig, SshConnector, run_command_with_cancel,
};

pub(super) struct HostExecutor {
    connector: SshConnector,
    hosts: RwLock<Vec<(SharedHost, HostLogin)>>,
}

impl HostExecutor {
    pub fn new(connector: SshConnector) -> Self {
        Self {
            connector,
            hosts: RwLock::default(),
        }
    }
    pub fn update(&self, store: &HostStore) -> bool {
        let hosts: Vec<_> = store
            .hosts()
            .iter()
            .filter(|host| host.is_ssh())
            .map(|host| {
                let login = store.login_of(host);
                (
                    SharedHost::new(
                        host.public_id.as_str().into(),
                        host.name.chars().take(256).collect(),
                        host.address.chars().take(1024).collect(),
                        host.port,
                        login.user.clone(),
                    ),
                    login,
                )
            })
            .collect();
        let mut stored = self.hosts.write().unwrap();
        let changed = !stored
            .iter()
            .map(|(host, _)| host)
            .eq(hosts.iter().map(|(host, _)| host));
        *stored = hosts;
        changed
    }
}

impl Executor for HostExecutor {
    fn hosts(&self) -> Vec<SharedHost> {
        self.hosts
            .read()
            .unwrap()
            .iter()
            .map(|(host, _)| host.clone())
            .collect()
    }
    fn execute(
        &self,
        host: &str,
        command: &str,
        cancelled: &AtomicBool,
        output: &mut dyn FnMut(bool, &[u8]) -> io::Result<()>,
    ) -> anyhow::Result<i32> {
        let login = self
            .hosts
            .read()
            .unwrap()
            .iter()
            .find(|(item, _)| item.id == host)
            .map(|(_, login)| login.clone())
            .ok_or_else(|| anyhow::anyhow!("host_no_longer_available"))?;
        let configuration = SshConnectionConfig::from(&login);
        match run_command_with_cancel(
            &self.connector,
            &configuration,
            command,
            &mut |stream, bytes| output(stream == ExecStream::Stderr, bytes),
            &|| cancelled.load(Ordering::Acquire),
        ) {
            Ok(ExecExit::Code(code)) => Ok(code as i32),
            Ok(ExecExit::Signal(_)) => Ok(255),
            Err(error) => Err(anyhow::anyhow!(match error.kind {
                ExecErrorKind::HostKeyUnknown => "shared_host_key_unknown",
                ExecErrorKind::HostKeyChanged => "shared_host_key_changed",
                ExecErrorKind::MissingCredential => "shared_host_credentials_missing",
                ExecErrorKind::Connect => "shared_host_connect_failed",
                ExecErrorKind::Aborted => "command_cancelled_or_timed_out",
            })),
        }
    }
}
