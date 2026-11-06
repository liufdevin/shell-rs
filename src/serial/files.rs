use std::net::{IpAddr, SocketAddr, TcpStream};
use std::time::Duration;

use crate::host::TerminalEncoding;
use crate::i18n::t;
use base64::Engine as _;

pub(crate) const COMMAND_SETTLE: Duration = Duration::from_millis(300);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerialFileEntry {
    name: String,
    path: String,
    directory: bool,
}

impl SerialFileEntry {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn is_directory(&self) -> bool {
        self.directory
    }
}

pub struct SerialDirectory {
    path: String,
    entries: Vec<SerialFileEntry>,
    truncated: bool,
}

impl SerialDirectory {
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn entries(&self) -> &[SerialFileEntry] {
        &self.entries
    }
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

pub fn directory_command(path: &str) -> Option<String> {
    let path = if path.starts_with('-') {
        format!("./{path}")
    } else {
        path.to_string()
    };
    let path = quote_path(&path)?;
    Some(format!(
        concat!(
            "export LC_ALL=C; cd {path} 2>&1 || exit; printf '@@shelllf-files\\n'; ",
            "{{ printf '%s\\000' P \"$PWD\"; count=0; ",
            "for entry in ./* ./.[!.]* ./..?*; do ",
            "if [ -d \"$entry\" ]; then kind=d; elif [ -f \"$entry\" ]; then kind=f; else continue; fi; ",
            "if [ \"$count\" -ge 1000 ]; then printf '%s\\000' T; break; fi; ",
            "printf '%s\\000' \"$kind\" \"${{entry#./}}\"; count=$((count+1)); done; ",
            "printf '%s\\000' E; }} | base64; printf '\\n@@shelllf-files-end\\n'"
        ),
        path = path
    ))
}

pub fn parent_directory(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        .unwrap_or("/")
}

pub fn parse_directory(
    output: &str,
    encoding: TerminalEncoding,
) -> Result<SerialDirectory, String> {
    let invalid = || t!("serial.files.invalid_listing").to_string();
    let body = output
        .split_once("@@shelllf-files\n")
        .and_then(|(_, tail)| tail.split_once("@@shelllf-files-end").map(|(body, _)| body))
        .ok_or_else(|| {
            let message = output.trim();
            if message.is_empty() {
                invalid()
            } else {
                message.chars().take(500).collect()
            }
        })?;
    let encoded: Vec<u8> = body
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| invalid())?;
    let mut fields = bytes.split(|byte| *byte == 0);
    if fields.next() != Some(b"P".as_slice()) {
        return Err(invalid());
    }
    let decode = |bytes: &[u8]| -> Result<String, String> {
        let decoded = crate::terminal::TerminalDecoder::new(encoding).decode(bytes, true);
        let text = String::from_utf8(decoded.into_owned()).map_err(|_| invalid())?;
        if crate::terminal::encode_input(encoding, text.as_bytes().to_vec())
            .ok()
            .as_deref()
            != Some(bytes)
            || quote_path(&text).is_none()
        {
            return Err(invalid());
        }
        Ok(text)
    };
    let path = decode(fields.next().ok_or_else(invalid)?)?;
    if !path.starts_with('/') {
        return Err(invalid());
    }
    let mut entries = Vec::new();
    let mut truncated = false;
    loop {
        match fields.next().ok_or_else(invalid)? {
            b"E" => break,
            b"T" => truncated = true,
            kind @ (b"d" | b"f") => {
                let name = decode(fields.next().ok_or_else(invalid)?)?;
                if name.contains('/')
                    || matches!(name.as_str(), "." | "..")
                    || entries.len() >= 1000
                {
                    return Err(invalid());
                }
                let full_path = format!("{}/{}", path.trim_end_matches('/'), name);
                entries.push(SerialFileEntry {
                    name,
                    path: full_path,
                    directory: kind == b"d",
                });
            }
            _ => return Err(invalid()),
        }
    }
    if fields.any(|field| !field.is_empty()) {
        return Err(invalid());
    }
    entries.sort_by(|left, right| {
        (!left.directory, left.name.to_lowercase(), &left.name).cmp(&(
            !right.directory,
            right.name.to_lowercase(),
            &right.name,
        ))
    });
    Ok(SerialDirectory {
        path,
        entries,
        truncated,
    })
}

pub const NETWORK_COMMAND: &str = "printf '@@boot_id\\n'; cat /proc/sys/kernel/random/boot_id 2>/dev/null; printf '@@user\\n'; id -un; printf '@@addresses\\n'; ip -o addr show scope global 2>/dev/null || hostname -I 2>/dev/null";

pub fn network_candidates(output: &str) -> (String, Vec<IpAddr>) {
    let user = output
        .split_once("@@user\n")
        .and_then(|(_, tail)| tail.lines().next())
        .unwrap_or("root")
        .trim();
    let user = if user.is_empty()
        || user.starts_with('-')
        || user
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        "root"
    } else {
        user
    };
    let mut addresses = Vec::new();
    if let Some((_, output)) = output.split_once("@@addresses\n") {
        let words: Vec<&str> = output
            .lines()
            .flat_map(|line| {
                let words: Vec<&str> = line.split_whitespace().collect();
                match words
                    .iter()
                    .position(|word| matches!(*word, "inet" | "inet6"))
                {
                    Some(index) => words.get(index + 1).copied().into_iter().collect(),
                    None => words,
                }
            })
            .collect();
        for word in words {
            if let Ok(address) = word.split('/').next().unwrap_or(word).parse::<IpAddr>() {
                if address.is_loopback()
                    || address.is_unspecified()
                    || address.is_multicast()
                    || matches!(address, IpAddr::V4(address) if address.is_link_local())
                    || matches!(address, IpAddr::V6(address) if address.is_unicast_link_local())
                {
                    continue;
                }
                if !addresses.contains(&address) && addresses.len() < 8 {
                    addresses.push(address);
                }
            }
        }
    }
    (user.to_string(), addresses)
}

pub fn reachable_address(addresses: &[IpAddr]) -> Option<IpAddr> {
    addresses.iter().copied().find(|address| {
        TcpStream::connect_timeout(&SocketAddr::new(*address, 22), Duration::from_millis(400))
            .is_ok()
    })
}

pub fn quote_path(path: &str) -> Option<String> {
    (!path.is_empty() && !path.chars().any(char::is_control))
        .then(|| format!("'{}'", path.replace('\'', "'\\''")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(bytes: &[u8]) -> String {
        format!(
            "@@shelllf-files\n{}\n@@shelllf-files-end\n",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }

    #[test]
    fn serial_directories_sort_folders_first_and_keep_literal_names() {
        let listing = parse_directory(
            &packet(b"P\0/home/root\0f\0z 'quoted' $HOME.log\0d\0logs\0f\0a.txt\0E\0"),
            TerminalEncoding::Utf8,
        )
        .unwrap();
        assert_eq!(listing.path(), "/home/root");
        assert_eq!(
            listing
                .entries()
                .iter()
                .map(SerialFileEntry::name)
                .collect::<Vec<_>>(),
            ["logs", "a.txt", "z 'quoted' $HOME.log"]
        );
        assert!(listing.entries()[0].is_directory());
        assert_eq!(
            listing.entries()[2].path(),
            "/home/root/z 'quoted' $HOME.log"
        );
        assert_eq!(parent_directory("/home/root/"), "/home");
        assert_eq!(parent_directory("/"), "/");
        assert!(!listing.truncated());
    }

    #[test]
    fn incomplete_or_invalid_serial_directory_records_are_rejected() {
        for bytes in [
            b"P\0/tmp\0f\0../bad\0E\0".as_slice(),
            b"P\0/tmp\0f\0missing-end\0",
            b"P\0relative\0E\0",
            b"P\0/tmp\0d\0..\0E\0",
            b"P\0/tmp\0f\0\xff\0E\0",
        ] {
            assert!(parse_directory(&packet(bytes), TerminalEncoding::Utf8).is_err());
        }
        assert!(parse_directory("permission denied", TerminalEncoding::Utf8).is_err());
        let listing = parse_directory(&packet(b"P\0/\0T\0E\0"), TerminalEncoding::Utf8).unwrap();
        assert!(listing.truncated());
    }

    #[test]
    fn serial_directory_commands_quote_the_path_and_reject_controls() {
        let command = directory_command("/tmp/a'b $HOME").unwrap();
        assert!(command.contains("cd '/tmp/a'\\''b $HOME'"));
        assert!(directory_command("bad\npath").is_none());
        assert!(directory_command("-P").unwrap().contains("cd './-P'"));
    }

    #[cfg(unix)]
    #[test]
    fn the_serial_directory_script_reads_real_files_without_writing_them() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("folder")).unwrap();
        std::fs::write(directory.path().join("a'b $HOME.log"), "test").unwrap();
        let output = crate::testing::sh(
            &directory_command(directory.path().to_str().unwrap()).unwrap(),
            None,
        );
        assert!(output.status.success());
        let listing = parse_directory(
            &String::from_utf8(output.stdout).unwrap(),
            TerminalEncoding::Utf8,
        )
        .unwrap();
        assert_eq!(listing.entries().len(), 2);
        assert!(listing.entries()[0].is_directory());
        assert_eq!(
            std::fs::read_to_string(directory.path().join("a'b $HOME.log")).unwrap(),
            "test"
        );
    }

    #[test]
    fn addresses_come_only_from_the_board_and_exclude_loopback() {
        let (user, addresses) = network_candidates(
            "@@user\nubuntu\n@@addresses\n2: eth0 inet 192.168.1.2/24 brd 192.168.1.255 scope global eth0\n127.0.0.1 169.254.0.1 ::1 fe80::1 192.168.1.2",
        );
        assert_eq!(user, "ubuntu");
        assert_eq!(addresses, ["192.168.1.2".parse::<IpAddr>().unwrap()]);
        assert!(network_candidates("login: ").1.is_empty());
    }

    #[test]
    fn paths_are_one_literal_shell_argument() {
        assert_eq!(quote_path("/tmp/a'b $x"), Some("'/tmp/a'\\''b $x'".into()));
        assert_eq!(quote_path("/tmp/a\nrm"), None);
    }
}
