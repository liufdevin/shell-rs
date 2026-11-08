//! Run one command on a saved host and pass on what it prints: the
//! external CLI's `exec`. Nobody is there to answer a question, so the
//! connection never asks one: a host not trusted yet or a password not saved
//! fails at once and says so.

use std::{
    io,
    sync::{Arc, Mutex},
    time::Duration,
};

use russh::ChannelMsg;
use tokio::sync::watch;

use super::connection::{
    SshConnectionConfig, SshConnector, SshPrompts, lock, timeout_excluding_prompts,
};
use super::error::{MissingCredential, wrap};
use super::prompt::{ConnectionPrompt, ConnectionPromptKind};

/// How long logging in may take.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(20);

/// Which of the command's outputs some bytes came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecStream {
    Stdout,
    Stderr,
}

/// How the command ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecExit {
    Code(u32),
    /// Killed by a signal, such as `TERM`.
    Signal(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecErrorKind {
    /// The host key is not in ShellRS's trust store yet.
    HostKeyUnknown,
    /// The host key is not the one trusted before.
    HostKeyChanged,
    /// Logging in needs a password, passphrase or answer ShellRS has not
    /// saved, or the saved one was refused.
    MissingCredential,
    /// Everything else between dialling and the command's end.
    Connect,
    /// Whoever wanted the output stopped taking it.
    Aborted,
}

/// Why a command did not run to its end: its kind, and the error that
/// says what happened, for the app to word.
#[derive(Debug)]
pub struct ExecError {
    pub kind: ExecErrorKind,
    pub error: anyhow::Error,
}

impl ExecError {
    fn new(kind: ExecErrorKind, error: impl Into<anyhow::Error>) -> Self {
        Self {
            kind,
            error: error.into(),
        }
    }
}

/// What went wrong running a command, beyond logging in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecFailure {
    /// Over the error that kept the runtime from starting.
    RuntimeFailed,
    /// Over the channel's error.
    CommandFailed,
    Aborted,
    Refused,
    Disconnected,
    HostKeyChanged {
        endpoint: String,
    },
    HostKeyUnknown {
        endpoint: String,
    },
}

impl std::fmt::Display for ExecFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RuntimeFailed => f.write_str("couldn't start the connection"),
            Self::CommandFailed => f.write_str("couldn't run the command"),
            Self::Aborted => f.write_str("nothing read the output, so the command was abandoned"),
            Self::Refused => f.write_str("the server refused to run the command"),
            Self::Disconnected => f.write_str("the connection closed before the command finished"),
            Self::HostKeyChanged { endpoint } => {
                write!(f, "the host key of {endpoint} doesn't match the saved one")
            }
            Self::HostKeyUnknown { endpoint } => {
                write!(f, "the host key of {endpoint} isn't trusted yet")
            }
        }
    }
}

impl std::error::Error for ExecFailure {}

/// Log in, run `command`, pass its output to `output` as it arrives, and
/// return how it ended. Blocks, with a runtime of its own, so it belongs on
/// a worker thread. The command gets no stdin: it is closed right away, so a
/// program waiting for input ends instead of hanging.
///
/// When `output` fails, the command is abandoned and the connection closed.
pub fn run_command(
    connector: &SshConnector,
    config: &SshConnectionConfig,
    command: &str,
    output: &mut dyn FnMut(ExecStream, &[u8]) -> io::Result<()>,
) -> Result<ExecExit, ExecError> {
    run_command_with_cancel(connector, config, command, output, &|| false)
}

pub fn run_command_with_cancel(
    connector: &SshConnector,
    config: &SshConnectionConfig,
    command: &str,
    output: &mut dyn FnMut(ExecStream, &[u8]) -> io::Result<()>,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<ExecExit, ExecError> {
    if cancelled() {
        return Err(ExecError::new(ExecErrorKind::Aborted, ExecFailure::Aborted));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            ExecError::new(
                ExecErrorKind::Connect,
                wrap(ExecFailure::RuntimeFailed, error),
            )
        })?;
    runtime.block_on(run(connector, config, command, output, cancelled))
}

/// What the host-key check saw, which the connect error alone does not say.
#[derive(Clone, Copy, Default)]
struct HostKeySeen {
    unknown: bool,
    changed: bool,
}

async fn run(
    connector: &SshConnector,
    config: &SshConnectionConfig,
    command: &str,
    output: &mut dyn FnMut(ExecStream, &[u8]) -> io::Result<()>,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<ExecExit, ExecError> {
    let seen = Arc::new(Mutex::new(HostKeySeen::default()));
    // Dropping the sender would read as a shutdown and cancel the login.
    let (_shutdown, shutdown) = watch::channel(false);
    let events = {
        let seen = seen.clone();
        Arc::new(move |prompt: ConnectionPrompt| match prompt.kind() {
            // Declining fails the login at once. Answering later would
            // leave it waiting for someone who is not there.
            ConnectionPromptKind::UnknownHost(_) => {
                lock(&seen).unknown = true;
                false
            }
            ConnectionPromptKind::HostKeyChanged(_) => {
                lock(&seen).changed = true;
                false
            }
            ConnectionPromptKind::Authentication(_) => false,
        })
    };
    let prompts = Arc::new(SshPrompts::new(events, shutdown).non_interactive());
    let connect = timeout_excluding_prompts(
        connector.connect(config, prompts.clone()),
        prompts.prompt_activity_receiver(),
        LOGIN_TIMEOUT,
    );
    let connected = tokio::select! {
        connected = connect => connected.and_then(|result| result),
        _ = wait_cancelled(cancelled) => return Err(ExecError::new(ExecErrorKind::Aborted, ExecFailure::Aborted)),
    };
    let (handle, _) = connected.map_err(|error| login_failure(error, *lock(&seen), config))?;

    let result = tokio::select! {
        result = run_on(&handle, command, output) => result,
        _ = wait_cancelled(cancelled) => Err(ExecError::new(ExecErrorKind::Aborted, ExecFailure::Aborted)),
    };
    let _ = tokio::time::timeout(
        Duration::from_secs(1),
        handle.disconnect(russh::Disconnect::ByApplication, "command finished", "en"),
    )
    .await;
    result
}

async fn run_on(
    handle: &super::SshHandle,
    command: &str,
    output: &mut dyn FnMut(ExecStream, &[u8]) -> io::Result<()>,
) -> Result<ExecExit, ExecError> {
    let channel_error = |error: russh::Error| {
        ExecError::new(
            ExecErrorKind::Connect,
            wrap(ExecFailure::CommandFailed, error),
        )
    };
    let mut channel = handle.channel_open_session().await.map_err(channel_error)?;
    channel.exec(true, command).await.map_err(channel_error)?;
    channel.eof().await.map_err(channel_error)?;
    let aborted = |_| ExecError::new(ExecErrorKind::Aborted, ExecFailure::Aborted);
    let mut exit = None;
    // Until the channel closes, not just until EOF: the exit status can
    // arrive after the last output.
    loop {
        let message = channel.wait().await;
        match message {
            Some(ChannelMsg::Data { data }) => {
                output(ExecStream::Stdout, &data).map_err(aborted)?
            }
            Some(ChannelMsg::ExtendedData { data, ext: 1 }) => {
                output(ExecStream::Stderr, &data).map_err(aborted)?
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => {
                exit = Some(ExecExit::Code(exit_status))
            }
            Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                exit = Some(ExecExit::Signal(format!("{signal_name:?}")))
            }
            Some(ChannelMsg::Failure) => {
                return Err(ExecError::new(ExecErrorKind::Connect, ExecFailure::Refused));
            }
            Some(ChannelMsg::Close) | None => break,
            Some(_) => {}
        }
    }
    exit.ok_or_else(|| ExecError::new(ExecErrorKind::Connect, ExecFailure::Disconnected))
}

async fn wait_cancelled(cancelled: &(dyn Fn() -> bool + Sync)) {
    loop {
        if cancelled() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Why logging in failed, sorted for the caller: the host key the check
/// saw, a credential nobody could give, or anything else.
fn login_failure(
    error: anyhow::Error,
    seen: HostKeySeen,
    config: &SshConnectionConfig,
) -> ExecError {
    let endpoint = config.endpoint();
    if seen.changed {
        return ExecError::new(
            ExecErrorKind::HostKeyChanged,
            ExecFailure::HostKeyChanged { endpoint },
        );
    }
    if seen.unknown {
        return ExecError::new(
            ExecErrorKind::HostKeyUnknown,
            ExecFailure::HostKeyUnknown { endpoint },
        );
    }
    if error.chain().any(|cause| cause.is::<MissingCredential>()) {
        return ExecError::new(ExecErrorKind::MissingCredential, error);
    }
    ExecError::new(ExecErrorKind::Connect, error)
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };

    struct NoSecrets;
    impl crate::SecretSource for NoSecrets {
        fn secret(
            &self,
            _: &crate::SecretRef,
        ) -> anyhow::Result<Option<zeroize::Zeroizing<String>>> {
            Ok(None)
        }
    }

    #[test]
    fn cancellation_interrupts_a_silent_login_without_waiting_for_its_timeout() {
        let directory = tempfile::tempdir().unwrap();
        let connector =
            SshConnector::new(directory.path().join("known_hosts"), Arc::new(NoSecrets));
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (accepted, connected) = mpsc::channel();
        let (close, closed) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            accepted.send(()).unwrap();
            let _ = closed.recv_timeout(Duration::from_secs(5));
            drop(stream);
        });
        let configuration = SshConnectionConfig::from(&crate::HostLogin::new(
            "127.0.0.1",
            port,
            "root",
            crate::LoginMethod::NoPassword,
        ));
        let cancel = Arc::new(AtomicBool::new(false));
        let check = cancel.clone();
        let worker = std::thread::spawn(move || {
            run_command_with_cancel(
                &connector,
                &configuration,
                "true",
                &mut |_, _| Ok(()),
                &|| check.load(Ordering::Acquire),
            )
        });
        connected.recv_timeout(Duration::from_secs(3)).unwrap();
        let started = std::time::Instant::now();
        cancel.store(true, Ordering::Release);
        assert_eq!(
            worker.join().unwrap().unwrap_err().kind,
            ExecErrorKind::Aborted
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        let _ = close.send(());
        server.join().unwrap();
    }

    struct SilentChannel {
        opened: mpsc::Sender<()>,
    }

    impl russh::server::Handler for SilentChannel {
        type Error = russh::Error;

        async fn auth_none(&mut self, _: &str) -> Result<russh::server::Auth, Self::Error> {
            Ok(russh::server::Auth::Accept)
        }

        async fn channel_open_session(
            &mut self,
            _: russh::Channel<russh::server::Msg>,
            _: russh::server::ChannelOpenHandle,
            _: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            let _ = self.opened.send(());
            std::future::pending().await
        }
    }

    #[test]
    fn cancellation_interrupts_a_server_that_never_answers_a_channel_request() {
        let directory = tempfile::tempdir().unwrap();
        let known_hosts = directory.path().join("known_hosts");
        let connector = SshConnector::new(known_hosts.clone(), Arc::new(NoSecrets));
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let key = russh::keys::PrivateKey::random(
            &mut russh::keys::key::safe_rng(),
            russh::keys::Algorithm::Ed25519,
        )
        .unwrap();
        russh::keys::known_hosts::learn_known_hosts_path(
            "127.0.0.1",
            port,
            key.public_key(),
            &known_hosts,
        )
        .unwrap();
        let (opened, channel_requested) = mpsc::channel();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let configuration = Arc::new(russh::server::Config {
                    keys: vec![key],
                    ..Default::default()
                });
                tokio::select! {
                    _ = async {
                        let (stream, _) = listener.accept().await.unwrap();
                        let session = russh::server::run_stream(configuration, stream, SilentChannel { opened }).await.unwrap();
                        let _ = session.await;
                    } => {},
                    _ = stopped => {},
                }
            });
        });
        let configuration = SshConnectionConfig::from(&crate::HostLogin::new(
            "127.0.0.1",
            port,
            "root",
            crate::LoginMethod::NoPassword,
        ));
        let cancel = Arc::new(AtomicBool::new(false));
        let check = cancel.clone();
        let (completed, result) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let outcome = run_command_with_cancel(
                &connector,
                &configuration,
                "true",
                &mut |_, _| Ok(()),
                &|| check.load(Ordering::Acquire),
            );
            let _ = completed.send(outcome);
        });
        channel_requested
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        cancel.store(true, Ordering::Release);
        let outcome = result.recv_timeout(Duration::from_secs(2));
        let _ = stop.send(());
        server.join().unwrap();
        worker.join().unwrap();
        assert_eq!(outcome.unwrap().unwrap_err().kind, ExecErrorKind::Aborted);
    }
}
