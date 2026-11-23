use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::host::{SerialAssistant, SerialCommand, SerialCommandEnding, SerialCommandGroup};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImportError {
    Empty,
    InvalidFormat,
}

#[derive(Serialize, Deserialize)]
struct GroupFile {
    name: String,
    #[serde(default)]
    commands: Vec<CommandFile>,
}

#[derive(Serialize, Deserialize)]
struct CommandFile {
    #[serde(default)]
    name: String,
    content: String,
    #[serde(default)]
    hex: bool,
    #[serde(default)]
    ending: SerialCommandEnding,
    #[serde(default)]
    crc: bool,
    #[serde(default = "default_delay_ms")]
    delay_ms: u32,
    #[serde(default = "default_enabled")]
    enabled: bool,
}

fn default_delay_ms() -> u32 {
    500
}

fn default_enabled() -> bool {
    true
}

/// Export a single command group as pretty-printed JSON.
pub(crate) fn export_group_json(group: &SerialCommandGroup) -> Result<String, serde_json::Error> {
    let payload = GroupFile {
        name: group.name.clone(),
        commands: group
            .commands
            .iter()
            .map(|command| CommandFile {
                name: command.name.clone(),
                content: command.content.clone(),
                hex: command.hex,
                ending: command.ending.clone(),
                crc: command.crc,
                delay_ms: command.delay_ms,
                enabled: command.enabled,
            })
            .collect(),
    };
    serde_json::to_string_pretty(&payload)
}

/// Export a command group in SSCOM-compatible `.ini` format (`[SCOMM]`).
///
/// Uses `N<index>=H,<content>` for HEX and `N<index>=A,<content>` for ASCII,
/// plus `S<index>=<name>`, `T<index>=<delay_ms>`, `E<index>=<0|1>`,
/// `L<index>=<NONE|LF|CR|CRLF>`, and `R<index>=<0|1>` (CRC) so both SSCOM and
/// lossless re-import work cleanly.
pub(crate) fn export_group_sscom(group: &SerialCommandGroup) -> String {
    let mut out = String::new();
    out.push_str("[SCOMM]\r\n");
    out.push_str(&format!(
        "GroupName={}\r\n",
        sanitize_ini_value(&group.name)
    ));
    for (idx, command) in group.commands.iter().enumerate() {
        let number = idx + 1;
        let mode = if command.hex { 'H' } else { 'A' };
        let content = escape_ini_content(&command.content);
        out.push_str(&format!("N{number}={mode},{content}\r\n"));
        out.push_str(&format!(
            "S{number}={}\r\n",
            sanitize_ini_value(&command.name)
        ));
        out.push_str(&format!("T{number}={}\r\n", command.delay_ms));
        out.push_str(&format!(
            "E{number}={}\r\n",
            if command.enabled { 1 } else { 0 }
        ));
        out.push_str(&format!("L{number}={}\r\n", ending_tag(&command.ending)));
        if command.crc {
            out.push_str(&format!("R{number}=1\r\n"));
        }
    }
    out
}

/// Parse command groups from either JSON (single group or full library) or
/// SSCOM `.ini` bytes (UTF-8 or GBK/GB18030).
pub(crate) fn import_groups_from_bytes(
    bytes: &[u8],
    fallback_group_name: &str,
) -> Result<Vec<SerialCommandGroup>, ImportError> {
    let text = decode_text(bytes);
    let trimmed = text.trim_start_matches('\u{feff}').trim();
    if trimmed.is_empty() {
        return Err(ImportError::Empty);
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') && !is_ini_header(trimmed) {
        return parse_json_groups(trimmed, fallback_group_name);
    }
    parse_sscom_ini(trimmed, fallback_group_name)
}

fn decode_text(bytes: &[u8]) -> String {
    if let Ok(utf8) = std::str::from_utf8(bytes) {
        return utf8.to_owned();
    }
    let (decoded, _, _) = encoding_rs::GB18030.decode(bytes);
    decoded.into_owned()
}

fn is_ini_header(trimmed: &str) -> bool {
    let Some(first_line) = trimmed.lines().next() else {
        return false;
    };
    let line = first_line.trim();
    line.starts_with('[') && line.ends_with(']') && !line.contains('{') && !line.contains('"')
}

fn parse_json_groups(
    text: &str,
    fallback_group_name: &str,
) -> Result<Vec<SerialCommandGroup>, ImportError> {
    if let Ok(library) = serde_json::from_str::<SerialAssistant>(text)
        && !library.groups.is_empty()
    {
        return Ok(library
            .groups
            .into_iter()
            .map(|group| normalize_group(group, fallback_group_name))
            .collect());
    }
    if let Ok(group) = serde_json::from_str::<GroupFile>(text) {
        let mut converted = SerialCommandGroup::default();
        converted.name = group.name;
        converted.commands = group
            .commands
            .into_iter()
            .map(|command| {
                let mut item = SerialCommand::default();
                item.name = command.name;
                item.content = command.content;
                item.hex = command.hex;
                item.ending = command.ending;
                item.crc = command.crc;
                item.delay_ms = command.delay_ms.min(60000);
                item.enabled = command.enabled;
                item
            })
            .collect();
        if converted.commands.is_empty() {
            return Err(ImportError::Empty);
        }
        return Ok(vec![normalize_group(converted, fallback_group_name)]);
    }
    if let Ok(commands) = serde_json::from_str::<Vec<CommandFile>>(text) {
        if commands.is_empty() {
            return Err(ImportError::Empty);
        }
        let mut converted = SerialCommandGroup::default();
        converted.name = fallback_group_name.to_owned();
        converted.commands = commands
            .into_iter()
            .map(|command| {
                let mut item = SerialCommand::default();
                item.name = command.name;
                item.content = command.content;
                item.hex = command.hex;
                item.ending = command.ending;
                item.crc = command.crc;
                item.delay_ms = command.delay_ms.min(60000);
                item.enabled = command.enabled;
                item
            })
            .collect();
        return Ok(vec![normalize_group(converted, fallback_group_name)]);
    }
    Err(ImportError::InvalidFormat)
}

#[derive(Default)]
struct IniEntry {
    content: Option<String>,
    hex: bool,
    name: Option<String>,
    delay_ms: Option<u32>,
    enabled: Option<bool>,
    ending: Option<SerialCommandEnding>,
    crc: Option<bool>,
}

fn parse_sscom_ini(
    text: &str,
    fallback_group_name: &str,
) -> Result<Vec<SerialCommandGroup>, ImportError> {
    let mut entries: BTreeMap<usize, IniEntry> = BTreeMap::new();
    let mut group_name: Option<String> = None;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty()
            || line.starts_with(';')
            || line.starts_with('#')
            || (line.starts_with('[') && line.ends_with(']'))
        {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim_end_matches('\r');
        if key.eq_ignore_ascii_case("GroupName") && !value.trim().is_empty() {
            group_name = Some(value.trim().to_owned());
            continue;
        }
        let mut chars = key.chars();
        let Some(prefix) = chars.next() else {
            continue;
        };
        let rest = chars.as_str();
        let Ok(index) = rest.parse::<usize>() else {
            continue;
        };
        if index == 0 {
            continue;
        }
        match prefix.to_ascii_uppercase() {
            'N' => {
                if value.is_empty() {
                    continue;
                }
                let (hex, raw_content) = parse_sscom_n_value(value);
                let content = unescape_ini_content(raw_content);
                if content.is_empty() {
                    continue;
                }
                let entry = entries.entry(index).or_default();
                entry.hex = hex;
                entry.content = Some(content);
            }
            'S' | 'B' | 'C' => {
                if !value.trim().is_empty() {
                    entries.entry(index).or_default().name = Some(value.trim().to_owned());
                }
            }
            'T' | 'D' => {
                if let Ok(ms) = value.trim().parse::<u32>() {
                    entries.entry(index).or_default().delay_ms = Some(ms.min(60000));
                }
            }
            'E' => {
                let flag = value.trim();
                if flag == "0" || flag.eq_ignore_ascii_case("false") {
                    entries.entry(index).or_default().enabled = Some(false);
                } else if flag == "1" || flag.eq_ignore_ascii_case("true") {
                    entries.entry(index).or_default().enabled = Some(true);
                }
            }
            'L' => {
                if let Some(ending) = parse_ending_tag(value.trim()) {
                    entries.entry(index).or_default().ending = Some(ending);
                }
            }
            'R' => {
                let flag = value.trim();
                if flag == "1" || flag.eq_ignore_ascii_case("true") {
                    entries.entry(index).or_default().crc = Some(true);
                }
            }
            _ => {}
        }
    }
    let commands: Vec<SerialCommand> = entries
        .into_iter()
        .filter_map(|(index, entry)| {
            let content = entry.content?;
            let mut command = SerialCommand::default();
            command.name = entry.name.unwrap_or_else(|| format!("{index}"));
            command.content = content;
            command.hex = entry.hex;
            command.ending = entry.ending.unwrap_or(if entry.hex {
                SerialCommandEnding::None
            } else {
                SerialCommandEnding::CrLf
            });
            command.crc = entry.crc.unwrap_or(false);
            command.delay_ms = entry.delay_ms.unwrap_or(500);
            command.enabled = entry.enabled.unwrap_or(true);
            Some(command)
        })
        .collect();
    if commands.is_empty() {
        return Err(ImportError::Empty);
    }
    let mut group = SerialCommandGroup::default();
    group.name = group_name.unwrap_or_else(|| fallback_group_name.to_owned());
    group.commands = commands;
    Ok(vec![normalize_group(group, fallback_group_name)])
}

fn parse_sscom_n_value(value: &str) -> (bool, &str) {
    if let Some(rest) = value
        .strip_prefix("H,")
        .or_else(|| value.strip_prefix("h,"))
    {
        (true, rest)
    } else if let Some(rest) = value
        .strip_prefix("A,")
        .or_else(|| value.strip_prefix("a,"))
    {
        (false, rest)
    } else if let Some(rest) = value.strip_prefix("1,") {
        (true, rest)
    } else if let Some(rest) = value.strip_prefix("0,") {
        (false, rest)
    } else {
        (false, value)
    }
}

fn normalize_group(mut group: SerialCommandGroup, fallback_name: &str) -> SerialCommandGroup {
    if group.name.trim().is_empty() {
        group.name = fallback_name.to_owned();
    }
    group.name = truncate_utf8(group.name.trim(), 128);
    group.commands.retain(|command| !command.content.is_empty());
    group.commands.truncate(128);
    for (idx, command) in group.commands.iter_mut().enumerate() {
        if command.name.trim().is_empty() {
            command.name = format!("{}", idx + 1);
        }
        command.name = truncate_utf8(command.name.trim(), 128);
        command.content = truncate_utf8(&command.content, 4096);
        command.delay_ms = command.delay_ms.min(60000);
    }
    group
}

fn truncate_utf8(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    input[..end].to_owned()
}

fn sanitize_ini_value(value: &str) -> String {
    value.replace(['\r', '\n'], " ")
}

fn escape_ini_content(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

fn unescape_ini_content(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('r') => out.push('\r'),
                Some('n') => out.push('\n'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn ending_tag(ending: &SerialCommandEnding) -> &'static str {
    match ending {
        SerialCommandEnding::None => "NONE",
        SerialCommandEnding::Lf => "LF",
        SerialCommandEnding::Cr => "CR",
        SerialCommandEnding::CrLf => "CRLF",
    }
}

fn parse_ending_tag(tag: &str) -> Option<SerialCommandEnding> {
    if tag.eq_ignore_ascii_case("NONE") {
        Some(SerialCommandEnding::None)
    } else if tag.eq_ignore_ascii_case("LF") {
        Some(SerialCommandEnding::Lf)
    } else if tag.eq_ignore_ascii_case("CR") {
        Some(SerialCommandEnding::Cr)
    } else if tag.eq_ignore_ascii_case("CRLF") {
        Some(SerialCommandEnding::CrLf)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_group() -> SerialCommandGroup {
        let mut first = SerialCommand::default();
        first.id = 2;
        first.name = "查询版本".into();
        first.content = "AT+GMR".into();
        first.hex = false;
        first.ending = SerialCommandEnding::CrLf;
        first.crc = false;
        first.delay_ms = 300;
        first.enabled = true;

        let mut second = SerialCommand::default();
        second.id = 3;
        second.name = "读寄存器".into();
        second.content = "01 03 00 00 00 0A".into();
        second.hex = true;
        second.ending = SerialCommandEnding::None;
        second.crc = true;
        second.delay_ms = 1000;
        second.enabled = false;

        let mut group = SerialCommandGroup::default();
        group.id = 1;
        group.name = "Modbus 调试".into();
        group.commands = vec![first, second];
        group
    }

    #[test]
    fn json_round_trip_preserves_all_command_fields() {
        let group = sample_group();
        let json = export_group_json(&group).unwrap();
        let imported = import_groups_from_bytes(json.as_bytes(), "fallback").unwrap();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].name, "Modbus 调试");
        assert_eq!(imported[0].commands.len(), 2);
        assert_eq!(imported[0].commands[0].content, "AT+GMR");
        assert_eq!(imported[0].commands[0].delay_ms, 300);
        assert!(imported[0].commands[1].hex);
        assert!(imported[0].commands[1].crc);
        assert!(!imported[0].commands[1].enabled);
    }

    #[test]
    fn sscom_ini_round_trip_and_gbk_import_work() {
        let group = sample_group();
        let ini = export_group_sscom(&group);
        let imported = import_groups_from_bytes(ini.as_bytes(), "fallback").unwrap();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].name, "Modbus 调试");
        assert_eq!(imported[0].commands.len(), 2);
        assert_eq!(imported[0].commands[0].name, "查询版本");
        assert_eq!(imported[0].commands[0].content, "AT+GMR");
        assert_eq!(imported[0].commands[0].ending, SerialCommandEnding::CrLf);
        assert!(imported[0].commands[1].hex);
        assert!(imported[0].commands[1].crc);
        assert!(!imported[0].commands[1].enabled);

        // Simulate classic Windows SSCOM5.13 GBK-encoded sscom51.ini.
        let classic =
            "[SCOMM]\r\nN1=A,AT+RST\r\nS1=重启模块\r\nN2=H,AA 55 01 02\r\nN10=A,AT+CSQ\r\n";
        let (gbk_bytes, _, _) = encoding_rs::GB18030.encode(classic);
        let parsed = import_groups_from_bytes(&gbk_bytes, "SSCOM 导入").unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "SSCOM 导入");
        assert_eq!(parsed[0].commands.len(), 3);
        assert_eq!(parsed[0].commands[0].name, "重启模块");
        assert_eq!(parsed[0].commands[0].content, "AT+RST");
        assert!(!parsed[0].commands[0].hex);
        assert_eq!(parsed[0].commands[1].name, "2");
        assert_eq!(parsed[0].commands[1].content, "AA 55 01 02");
        assert!(parsed[0].commands[1].hex);
        assert_eq!(parsed[0].commands[2].name, "10");
        assert_eq!(parsed[0].commands[2].content, "AT+CSQ");
    }
}
