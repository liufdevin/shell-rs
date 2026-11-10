use std::sync::{Arc, mpsc};

use anyhow::Result;
use async_channel::Sender;
use unicode_width::UnicodeWidthChar as _;

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

const PROMPT: &[u8] = b"ShellLF> ";
const MAX_LINE: usize = 4096;

#[derive(Default)]
struct LineEditor {
    line: Vec<u8>,
    cursor: usize,
    pending: Vec<u8>,
}

impl LineEditor {
    fn flush(&mut self) -> bool {
        if self.pending.is_empty() {
            return false;
        }
        let pending = std::mem::take(&mut self.pending);
        if self.line.len().saturating_add(pending.len()) > MAX_LINE {
            return false;
        }
        self.line
            .splice(self.cursor..self.cursor, pending.iter().copied());
        self.cursor += pending.len();
        true
    }

    fn left(&mut self) -> bool {
        let cursor = previous_boundary(&self.line, self.cursor);
        if cursor == self.cursor {
            return false;
        }
        self.cursor = cursor;
        true
    }

    fn right(&mut self) -> bool {
        let cursor = next_boundary(&self.line, self.cursor);
        if cursor == self.cursor {
            return false;
        }
        self.cursor = cursor;
        true
    }

    fn home(&mut self) -> bool {
        let changed = self.cursor != 0;
        self.cursor = 0;
        changed
    }

    fn end(&mut self) -> bool {
        let changed = self.cursor != self.line.len();
        self.cursor = self.line.len();
        changed
    }

    fn backspace(&mut self) -> bool {
        let start = previous_boundary(&self.line, self.cursor);
        if start == self.cursor {
            return false;
        }
        self.line.drain(start..self.cursor);
        self.cursor = start;
        true
    }

    fn delete(&mut self) -> bool {
        let end = next_boundary(&self.line, self.cursor);
        if end == self.cursor {
            return false;
        }
        self.line.drain(self.cursor..end);
        true
    }

    fn delete_to_end(&mut self) -> bool {
        let changed = self.cursor < self.line.len();
        self.line.truncate(self.cursor);
        changed
    }

    fn delete_word(&mut self) -> bool {
        let mut start = self.cursor;
        while start > 0 && self.line[start - 1].is_ascii_whitespace() {
            start -= 1;
        }
        while start > 0 && !self.line[start - 1].is_ascii_whitespace() {
            start = previous_boundary(&self.line, start);
        }
        if start == self.cursor {
            return false;
        }
        self.line.drain(start..self.cursor);
        self.cursor = start;
        true
    }

    fn clear(&mut self) {
        self.line.clear();
        self.cursor = 0;
        self.pending.clear();
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.line).trim().to_owned()
    }

    fn redraw(&self, events: &Sender<TerminalTransportEvent>) {
        let mut output = b"\r\x1b[2K".to_vec();
        output.extend_from_slice(PROMPT);
        output.extend_from_slice(&self.line);
        let columns = display_columns(&self.line[self.cursor..]);
        if columns > 0 {
            output.extend_from_slice(format!("\x1b[{columns}D").as_bytes());
        }
        send_event(events, TerminalTransportEvent::Output(output));
    }
}

fn previous_boundary(bytes: &[u8], index: usize) -> usize {
    if index == 0 {
        return 0;
    }
    let mut start = index - 1;
    while start > 0 && bytes[start] & 0xc0 == 0x80 {
        start -= 1;
    }
    start
}

fn next_boundary(bytes: &[u8], index: usize) -> usize {
    if index >= bytes.len() {
        return bytes.len();
    }
    let mut end = index + 1;
    while end < bytes.len() && bytes[end] & 0xc0 == 0x80 {
        end += 1;
    }
    end
}

fn display_columns(bytes: &[u8]) -> usize {
    String::from_utf8_lossy(bytes)
        .chars()
        .map(|character| character.width().unwrap_or(0))
        .sum()
}

fn apply_escape(sequence: &[u8], editor: &mut LineEditor) -> bool {
    let Some(final_byte) = sequence.last().copied() else {
        return false;
    };
    if sequence.len() < 3 {
        return false;
    }
    let body = &sequence[2..sequence.len() - 1];
    match final_byte {
        b'A' | b'B' => false,
        b'C' => editor.right(),
        b'D' => editor.left(),
        b'H' => editor.home(),
        b'F' => editor.end(),
        b'~' => match body.split(|byte| *byte == b';').next() {
            Some(b"1" | b"7") => editor.home(),
            Some(b"4" | b"8") => editor.end(),
            Some(b"3") => editor.delete(),
            _ => false,
        },
        _ => false,
    }
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
        let mut editor = LineEditor::default();
        let mut escape = Vec::new();
        let mut connecting = false;
        let mut previous_cr = false;
        while let Ok(command) = commands.recv() {
            match command {
                TerminalTransportCommand::Shutdown => break,
                TerminalTransportCommand::Write(bytes) if !connecting => {
                    for byte in bytes {
                        if !escape.is_empty() {
                            escape.push(byte);
                            if escape.len() == 2 && byte != b'[' && byte != b'O' {
                                escape.clear();
                            } else if escape.len() > 2 && (0x40..=0x7e).contains(&byte) {
                                let sequence = std::mem::take(&mut escape);
                                if apply_escape(&sequence, &mut editor) {
                                    editor.redraw(&events);
                                }
                            }
                            continue;
                        }
                        if byte == 27 {
                            if editor.flush() {
                                editor.redraw(&events);
                            }
                            escape.push(byte);
                            continue;
                        }
                        if byte >= 32 && byte != 127 {
                            editor.pending.push(byte);
                            continue;
                        }
                        let mut dirty = editor.flush();
                        match byte {
                            b'\r' | b'\n' => {
                                if byte == b'\n' && previous_cr {
                                    previous_cr = false;
                                    continue;
                                }
                                previous_cr = byte == b'\r';
                                if dirty {
                                    editor.redraw(&events);
                                    dirty = false;
                                }
                                let text = editor.text();
                                editor.clear();
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
                                    TerminalTransportEvent::Output(PROMPT.to_vec()),
                                );
                            }
                            3 | 21 => {
                                editor.clear();
                                editor.redraw(&events);
                                dirty = false;
                            }
                            8 | 127 => dirty |= editor.backspace(),
                            1 => dirty |= editor.home(),
                            5 => dirty |= editor.end(),
                            11 => dirty |= editor.delete_to_end(),
                            23 => dirty |= editor.delete_word(),
                            _ => {}
                        }
                        if dirty {
                            editor.redraw(&events);
                        }
                        previous_cr = byte == b'\r';
                    }
                    if editor.flush() {
                        editor.redraw(&events);
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

    #[test]
    fn line_edits_happen_at_the_cursor_and_cursor_keys_move_over_characters() {
        let mut editor = LineEditor::default();
        editor.pending.extend_from_slice(b"abcd");
        assert!(editor.flush());
        assert!(apply_escape(b"\x1b[D", &mut editor));
        assert!(apply_escape(b"\x1b[D", &mut editor));
        assert!(editor.backspace());
        editor.pending.extend_from_slice(b"X");
        assert!(editor.flush());
        assert!(apply_escape(b"\x1b[C", &mut editor));
        assert!(editor.backspace());
        assert!(apply_escape(b"\x1b[H", &mut editor));
        assert!(apply_escape(b"\x1b[3~", &mut editor));
        assert!(apply_escape(b"\x1b[F", &mut editor));
        editor.pending.extend_from_slice(b"!");
        assert!(editor.flush());
        assert_eq!(editor.text(), "Xd!");

        let mut wide = LineEditor::default();
        wide.pending.extend_from_slice("你a".as_bytes());
        assert!(wide.flush());
        assert!(wide.backspace());
        assert_eq!(wide.text(), "你");
        assert!(wide.backspace());
        assert_eq!(wide.text(), "");
    }
}
