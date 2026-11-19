use std::io::{Read as _, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, ensure};
use clap::Subcommand;
use serde_json::json;
use shellrs_lan::{Core, RemoteRequest, Response};

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show the remote computer's fingerprint and the pairing code.
    Inspect { address: String },
    /// Request pairing, then wait for approval in the remote ShellLF window.
    Pair {
        address: String,
        #[arg(long)]
        fingerprint: String,
    },
    /// List computers paired by this headless CLI identity.
    Peers,
    /// List live serial tabs explicitly shared with this CLI.
    Tabs { peer: String },
    /// Read serial output. --follow emits one JSON record per batch.
    Logs {
        peer: String,
        tab: String,
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, value_parser = clap::value_parser!(u32).range(0..=20000), default_value_t = 0)]
        wait_ms: u32,
        #[arg(long)]
        follow: bool,
    },
    /// Run one non-interactive command at an idle Linux Shell prompt.
    Exec {
        peer: String,
        tab: String,
        #[arg(required_unless_present = "stdin")]
        command: Option<String>,
        #[arg(long, conflicts_with = "command")]
        stdin: bool,
    },
}

pub fn run(
    command: Command,
    directory: &Path,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<i32> {
    let command = match command {
        Command::Exec {
            peer,
            tab,
            stdin: true,
            ..
        } => {
            let mut input = String::new();
            std::io::stdin()
                .take(shellrs_lan::DEBUG_COMMAND_LIMIT as u64 + 3)
                .read_to_string(&mut input)?;
            ensure!(
                input.len() <= shellrs_lan::DEBUG_COMMAND_LIMIT + 2,
                "invalid_serial_debug_command"
            );
            Command::Exec {
                peer,
                tab,
                command: Some(input.trim_end_matches(['\r', '\n']).into()),
                stdin: false,
            }
        }
        command => command,
    };
    run_command(command, directory, stdout, stderr)
}

fn run_command(
    command: Command,
    directory: &Path,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<i32> {
    if let Command::Exec { command, .. } = &command {
        let text = command.as_deref().unwrap_or_default();
        ensure!(
            !text.trim().is_empty()
                && text.len() <= shellrs_lan::DEBUG_COMMAND_LIMIT
                && !text.chars().any(char::is_control),
            "invalid_serial_debug_command"
        );
    }
    let core = load_client(directory)?;
    match command {
        Command::Inspect { address } => {
            let peer = core.inspect(shellrs_lan::parse_endpoint(&address)?)?;
            write_json(
                stdout,
                &json!({
                    "peer": peer.id,
                    "pairing_code": peer.pairing_code(core.id()),
                }),
            )?;
        }
        Command::Pair {
            address,
            fingerprint,
        } => {
            let peer = core.inspect(shellrs_lan::parse_endpoint(&address)?)?;
            ensure!(peer.id == fingerprint, "device_identity_changed");
            core.request_pair(&peer)?;
            writeln!(
                stderr,
                "Approve the request in ShellLF. Pairing code: {}",
                peer.pairing_code(core.id())
            )?;
            let deadline = Instant::now() + Duration::from_secs(120);
            loop {
                match core.complete_pair(peer.clone()) {
                    Ok(()) => break,
                    Err(error)
                        if error.to_string() == "pairing_pending" && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_secs(1));
                    }
                    Err(error) => return Err(error),
                }
            }
            write_json(stdout, &json!({ "peer": peer.id }))?;
        }
        Command::Peers => write_json(stdout, &core.peers())?,
        Command::Tabs { peer } => {
            write_json(
                stdout,
                &request(&core, &peer, RemoteRequest::SerialTabs)?.serial_tabs,
            )?;
        }
        Command::Logs {
            peer,
            tab,
            mut after,
            wait_ms,
            follow,
        } => loop {
            let log = request(
                &core,
                &peer,
                RemoteRequest::SerialLogs {
                    tab: tab.clone(),
                    after,
                    wait_ms: if follow { 20_000 } else { wait_ms },
                },
            )?
            .serial_log
            .context("missing_serial_debug_log")?;
            if !follow || !log.text.is_empty() || log.gap {
                write_json(stdout, &log)?;
            }
            if !follow {
                break;
            }
            after = Some(log.cursor);
        },
        Command::Exec {
            peer, tab, command, ..
        } => {
            let result = request(
                &core,
                &peer,
                RemoteRequest::SerialExec {
                    tab,
                    command: command.unwrap_or_default(),
                },
            )?
            .serial_result
            .context("missing_serial_debug_result")?;
            write_json(stdout, &result)?;
            return Ok(result.exit_code);
        }
    }
    Ok(0)
}

fn load_client(directory: &Path) -> Result<Arc<Core>> {
    let name = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "shelllf-cli".into());
    Core::load_client(&directory.join("lan-cli"), &name)
}

fn request(core: &Core, peer: &str, request: RemoteRequest) -> Result<Response> {
    core.request(peer, request, &AtomicBool::new(false), &mut |_, _| Ok(()))
}

fn write_json(stdout: &mut dyn Write, value: &impl serde::Serialize) -> Result<()> {
    serde_json::to_writer(&mut *stdout, value)?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}
