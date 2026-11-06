use std::sync::{Arc, mpsc};

use anyhow::Result;
use async_channel::Sender;

use crate::host::{AuthKind, HostDraft};
use crate::i18n::t;

use super::{
    SharedTerminalTransportFactory, TerminalSize, TerminalTransport, TerminalTransportCommand,
    TerminalTransportEvent, TerminalTransportFactory, send_event,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshCommand {
    address: String,
    user: String,
    port: u16,
}

impl SshCommand {
    pub fn parse(line: &str) -> Result<Self, String> {
        let invalid = || t!("terminal.command.usage").to_string();
        let words = super::local_shell::split_command(line, super::local_shell::Quoting::Posix)
            .ok_or_else(invalid)?;
        if words.first().map(String::as_str) != Some("ssh") {
            return Err(invalid());
        }
        let mut words = words.into_iter().skip(1);
        let mut user = None;
        let mut port = 22;
        let mut target = None;
        while let Some(word) = words.next() {
            match word.as_str() {
                "-p" => {
                    port = words
                        .next()
                        .ok_or_else(invalid)?
                        .parse()
                        .ok()
                        .filter(|value| *value > 0)
                        .ok_or_else(invalid)?
                }
                "-l" => user = Some(words.next().ok_or_else(invalid)?),
                _ if word.starts_with("-p") && word.len() > 2 => {
                    port = word[2..]
                        .parse()
                        .ok()
                        .filter(|value| *value > 0)
                        .ok_or_else(invalid)?
                }
                _ if word.starts_with('-') || target.is_some() => return Err(invalid()),
                _ => target = Some(word),
            }
        }
        let target = target.ok_or_else(invalid)?;
        let (target_user, address) = target
            .rsplit_once('@')
            .map_or((None, target.as_str()), |(user, address)| {
                (Some(user.to_string()), address)
            });
        let address = address
            .strip_prefix('[')
            .and_then(|address| address.strip_suffix(']'))
            .unwrap_or(address);
        let command = Self {
            address: address.to_owned(),
            user: user.or(target_user).unwrap_or_else(|| "root".into()),
            port,
        };
        command.draft().validated().map_err(|_| invalid())?;
        if command
            .address
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
            || command.user.starts_with('-')
        {
            return Err(invalid());
        }
        Ok(command)
    }

    pub fn draft(&self) -> HostDraft {
        HostDraft::new(
            &self.address,
            &self.address,
            self.port,
            &self.user,
            AuthKind::Password,
            None,
        )
    }
}

pub fn command_terminal_factory() -> SharedTerminalTransportFactory {
    Arc::new(CommandTerminal)
}

struct CommandTerminal;

impl TerminalTransportFactory for CommandTerminal {
    fn create(&self) -> Box<dyn TerminalTransport> {
        Box::new(Self)
    }
}

impl TerminalTransport for CommandTerminal {
    fn run(
        self: Box<Self>,
        _: TerminalSize,
        commands: mpsc::Receiver<TerminalTransportCommand>,
        events: Sender<TerminalTransportEvent>,
    ) -> Result<()> {
        send_event(&events, TerminalTransportEvent::Started);
        send_event(
            &events,
            TerminalTransportEvent::Output(
                format!("{}\r\n\r\nShellLF> ", t!("terminal.command.welcome")).into_bytes(),
            ),
        );
        let mut line = Vec::new();
        let mut escape = false;
        let mut connecting = false;
        let mut previous_cr = false;
        while let Ok(command) = commands.recv() {
            match command {
                TerminalTransportCommand::Shutdown => break,
                TerminalTransportCommand::Write(bytes) if !connecting => {
                    for byte in bytes {
                        if escape {
                            if byte.is_ascii_alphabetic() || byte == b'~' {
                                escape = false;
                            }
                            continue;
                        }
                        match byte {
                            27 => escape = true,
                            b'\r' | b'\n' => {
                                if byte == b'\n' && previous_cr {
                                    previous_cr = false;
                                    continue;
                                }
                                previous_cr = byte == b'\r';
                                let text = String::from_utf8_lossy(&line).trim().to_owned();
                                line.clear();
                                send_event(
                                    &events,
                                    TerminalTransportEvent::Output(b"\r\n".to_vec()),
                                );
                                if text == "exit" {
                                    send_event(
                                        &events,
                                        TerminalTransportEvent::Exited {
                                            code: 0,
                                            signal: None,
                                        },
                                    );
                                    return Ok(());
                                }
                                if !text.is_empty() && text != "help" {
                                    match SshCommand::parse(&text) {
                                        Ok(command) => {
                                            send_event(
                                                &events,
                                                TerminalTransportEvent::ConnectSsh(command),
                                            );
                                            connecting = true;
                                            break;
                                        }
                                        Err(error) => {
                                            send_event(
                                                &events,
                                                TerminalTransportEvent::Output(
                                                    format!("{error}\r\n").into_bytes(),
                                                ),
                                            );
                                        }
                                    }
                                } else if text == "help" {
                                    send_event(
                                        &events,
                                        TerminalTransportEvent::Output(
                                            format!("{}\r\n", t!("terminal.command.usage"))
                                                .into_bytes(),
                                        ),
                                    );
                                }
                                send_event(
                                    &events,
                                    TerminalTransportEvent::Output(b"ShellLF> ".to_vec()),
                                );
                            }
                            3 | 21 => {
                                line.clear();
                                send_event(
                                    &events,
                                    TerminalTransportEvent::Output(b"\r\x1b[2KShellLF> ".to_vec()),
                                );
                            }
                            8 | 127 => {
                                if let Some(start) =
                                    line.iter().rposition(|byte| byte & 0xc0 != 0x80)
                                {
                                    line.truncate(start);
                                    send_event(
                                        &events,
                                        TerminalTransportEvent::Output(b"\x08 \x08".to_vec()),
                                    );
                                }
                            }
                            32..=255 if line.len() < 4096 => {
                                previous_cr = false;
                                line.push(byte);
                                send_event(&events, TerminalTransportEvent::Output(vec![byte]));
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_commands_keep_the_port_user_and_ipv6_address() {
        let command = SshCommand::parse("ssh -p 2222 -l admin '[2001:db8::1]'").unwrap();
        assert_eq!(
            command,
            SshCommand {
                address: "2001:db8::1".into(),
                user: "admin".into(),
                port: 2222
            }
        );
        assert_eq!(SshCommand::parse("ssh root@board").unwrap().user, "root");
        for invalid in [
            "ssh",
            "ssh -p 0 root@board",
            "ssh root@board rm",
            "ssh -o ProxyCommand=bad board",
            "echo test",
            "ssh 'board",
        ] {
            assert!(SshCommand::parse(invalid).is_err(), "{invalid}");
        }
    }
}
