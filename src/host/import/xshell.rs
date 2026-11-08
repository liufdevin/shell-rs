use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail, ensure};

use crate::host::{HostDraft, Protocol, ProtocolKind};
use crate::i18n::t;

use super::{CandidateOptions, ImportCandidate, ImportLoad, ImportScan, parse_port};

const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;
const MAX_DEPTH: usize = 32;

#[derive(Default)]
struct Reader {
    scan: ImportScan,
    bytes: u64,
    entries: usize,
    seen_files: HashSet<PathBuf>,
    seen_hosts: HashSet<(String, String, u16, String)>,
}

pub(super) fn read_paths(paths: &[PathBuf]) -> ImportLoad {
    let mut reader = Reader::default();
    let result = paths.iter().try_for_each(|path| {
        reader.entry()?;
        if fs::symlink_metadata(path)?.is_dir() {
            reader.directory(path, path, 0)
        } else {
            reader.file(path, path.parent().unwrap_or(Path::new("")))
        }
    });
    match result {
        Ok(()) => ImportLoad::Loaded(reader.scan),
        Err(_) => ImportLoad::Failed(t!("host.import.xshell.read_failed").to_string()),
    }
}

impl Reader {
    fn entry(&mut self) -> Result<()> {
        self.entries += 1;
        ensure!(self.entries <= MAX_ENTRIES, "entry limit");
        Ok(())
    }

    fn directory(&mut self, path: &Path, root: &Path, depth: usize) -> Result<()> {
        ensure!(depth <= MAX_DEPTH, "directory depth limit");
        let mut entries = fs::read_dir(path)?
            .take(MAX_ENTRIES + 1)
            .collect::<std::io::Result<Vec<_>>>()?;
        ensure!(entries.len() <= MAX_ENTRIES, "entry limit");
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            self.entry()?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                self.directory(&entry.path(), root, depth + 1)?;
            } else if kind.is_file() && extension(&entry.path()) == "xsh" {
                self.file(&entry.path(), root)?;
            }
        }
        Ok(())
    }

    fn file(&mut self, path: &Path, root: &Path) -> Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            self.scan.skipped += 1;
            return Ok(());
        }
        if !self.seen_files.insert(fs::canonicalize(path)?) {
            return Ok(());
        }
        match extension(path).as_str() {
            "xsh" => {
                if metadata.len() > MAX_FILE_BYTES {
                    self.scan.skipped += 1;
                    return Ok(());
                }
                let bytes = self.read(File::open(path)?)?;
                let relative = path.strip_prefix(root).unwrap_or(path);
                self.candidate(relative, &bytes);
            }
            "xts" | "zip" => {
                ensure!(metadata.len() <= MAX_TOTAL_BYTES, "archive limit");
                self.archive(File::open(path)?)?;
            }
            _ => self.scan.skipped += 1,
        }
        Ok(())
    }

    fn read(&mut self, input: impl std::io::Read) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        input.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 <= MAX_FILE_BYTES, "file limit");
        self.bytes += bytes.len() as u64;
        ensure!(self.bytes <= MAX_TOTAL_BYTES, "total limit");
        Ok(bytes)
    }

    fn archive(&mut self, file: File) -> Result<()> {
        let mut archive = zip::ZipArchive::new(file)?;
        ensure!(archive.len() <= MAX_ENTRIES, "archive entry limit");
        for index in 0..archive.len() {
            self.entry()?;
            let entry = archive.by_index(index)?;
            let Some(path) = archive_path(&entry) else {
                self.scan.skipped += 1;
                continue;
            };
            if entry.is_dir() || extension(&path) != "xsh" {
                continue;
            }
            if entry.size() > MAX_FILE_BYTES
                || entry
                    .unix_mode()
                    .is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                self.scan.skipped += 1;
                continue;
            }
            let bytes = self.read(entry)?;
            let path = path.strip_prefix("Sessions").unwrap_or(&path);
            self.candidate(path, &bytes);
        }
        Ok(())
    }

    fn candidate(&mut self, path: &Path, bytes: &[u8]) {
        match parse(path, bytes) {
            Ok(candidate) => {
                let key = (
                    candidate.options.protocol.as_str().to_owned(),
                    candidate.address.to_lowercase(),
                    candidate.port,
                    candidate.user.clone(),
                );
                if self.seen_hosts.insert(key) {
                    self.scan.candidates.push(candidate);
                }
            }
            Err(_) => self.scan.skipped += 1,
        }
    }
}

fn archive_path(entry: &zip::read::ZipFile<'_, File>) -> Option<PathBuf> {
    let name = decode(entry.name_raw())
        .ok()
        .or_else(|| entry.name().ok())?;
    if name.starts_with(['/', '\\']) {
        return None;
    }
    let mut path = PathBuf::new();
    for component in name.split(['/', '\\']) {
        match component {
            "" | "." => {}
            ".." => {
                if !path.pop() {
                    return None;
                }
            }
            component => {
                if component.contains(':') || component.chars().any(char::is_control) {
                    return None;
                }
                path.push(component);
            }
        }
    }
    (!path.as_os_str().is_empty()).then_some(path)
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn parse(path: &Path, bytes: &[u8]) -> Result<ImportCandidate> {
    let text = decode(bytes)?;
    let mut fields = HashMap::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with([';', '#']) {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            section = name.trim().to_ascii_lowercase();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            bail!("invalid field");
        };
        let key = key.trim().to_ascii_lowercase();
        if (section == "connection"
            && matches!(
                key.as_str(),
                "host" | "port" | "protocol" | "username" | "description" | "proxy" | "useproxy"
            ))
            || (section == "connection:authentication" && key == "username")
        {
            ensure!(
                fields
                    .insert((section.clone(), key), value.trim().to_owned())
                    .is_none(),
                "duplicate field"
            );
        }
    }
    let field = |section: &str, key: &str| -> &str {
        fields
            .get(&(section.to_owned(), key.to_owned()))
            .map(String::as_str)
            .unwrap_or_default()
    };
    let protocol = match field("connection", "protocol")
        .to_ascii_lowercase()
        .as_str()
    {
        "ssh" | "ssh2" => ProtocolKind::Ssh,
        "telnet" => ProtocolKind::Telnet,
        _ => bail!("unsupported protocol"),
    };
    ensure!(
        matches!(field("connection", "proxy"), "" | "0" | "-1"),
        "proxy configuration required"
    );
    ensure!(
        matches!(field("connection", "useproxy"), "" | "0"),
        "proxy configuration required"
    );
    let address = field("connection", "host").to_owned();
    ensure!(
        address.len() <= 1024 && !address.chars().any(char::is_whitespace),
        "invalid address"
    );
    let port = match field("connection", "port") {
        "" if protocol == ProtocolKind::Telnet => 23,
        "" => 22,
        value => parse_port(value).ok_or_else(|| anyhow::anyhow!("invalid port"))?,
    };
    let user = match field("connection:authentication", "username") {
        "" => field("connection", "username"),
        user => user,
    }
    .to_owned();
    let name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid name"))?
        .to_owned();
    ensure!(
        path.components().count() <= MAX_DEPTH + 1,
        "group depth limit"
    );
    let group = path
        .parent()
        .unwrap_or(Path::new(""))
        .components()
        .map(|component| match component {
            Component::Normal(level) => {
                let level = level
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("invalid group"))?;
                ensure!(
                    !level.contains(['/', '\\', ':']) && !level.trim().is_empty(),
                    "invalid group"
                );
                Ok(level.to_owned())
            }
            _ => bail!("invalid group path"),
        })
        .collect::<Result<Vec<_>>>()?
        .join("/");
    let notes = field("connection", "description")
        .chars()
        .take(2000)
        .collect::<String>();
    let mut draft = HostDraft::new(
        name.clone(),
        address.clone(),
        port,
        user.clone(),
        super::super::AuthKind::Password,
        None,
    );
    if protocol == ProtocolKind::Telnet {
        draft.protocol = Protocol::Telnet(Default::default());
    }
    let draft = draft
        .validated()
        .map_err(|_| anyhow::anyhow!("invalid host"))?;
    ensure!(
        draft.user.len() <= 256 && !draft.user.chars().any(char::is_control),
        "invalid user"
    );
    Ok(ImportCandidate {
        name,
        address,
        port,
        user: draft.user.to_string(),
        identity: None,
        options: CandidateOptions {
            protocol,
            group,
            password: true,
            match_user: true,
            notes,
        },
    })
}

fn decode(bytes: &[u8]) -> Result<std::borrow::Cow<'_, str>> {
    let (encoding, offset) = if let Some(pair) = encoding_rs::Encoding::for_bom(bytes) {
        pair
    } else if let Ok(text) = std::str::from_utf8(bytes) {
        ensure!(!text.contains('\0'), "invalid encoding");
        return Ok(std::borrow::Cow::Borrowed(text));
    } else {
        (encoding_rs::GB18030, 0)
    };
    let (text, errors) = encoding.decode_without_bom_handling(&bytes[offset..]);
    ensure!(!errors && !text.contains('\0'), "invalid encoding");
    Ok(text)
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write as _};

    use super::*;

    fn config(protocol: &str, user: &str) -> String {
        format!(
            "[CONNECTION]\r\nProtocol={protocol}\r\nHost=server.example.com\r\nPort=2222\r\nDescription=生产主机\r\n[CONNECTION:AUTHENTICATION]\r\nUserName={user}\r\nPassword=SECRET_NOT_TO_IMPORT\r\nUserKeyPassphrase=SECRET_NOT_TO_IMPORT\r\nUserKey=C:\\private\\secret.key\r\n"
        )
    }

    fn loaded(path: &Path) -> ImportScan {
        match read_paths(&[path.to_path_buf()]) {
            ImportLoad::Loaded(scan) => scan,
            other => panic!("{other:?}"),
        }
    }

    fn archive_with_raw_name(name: &[u8]) -> Vec<u8> {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        archive
            .start_file(
                "x".repeat(name.len()),
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        archive.write_all(config("SSH", "root").as_bytes()).unwrap();
        let mut bytes = archive.finish().unwrap().into_inner();
        let (local_name, central_name) = {
            let mut reader = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
            let entry = reader.by_index(0).unwrap();
            (
                entry.header_start() as usize + 30,
                entry.central_header_start() as usize + 46,
            )
        };
        bytes[local_name..local_name + name.len()].copy_from_slice(name);
        bytes[central_name..central_name + name.len()].copy_from_slice(name);
        bytes
    }

    #[test]
    fn legacy_chinese_archive_names_and_groups_are_decoded_before_path_parsing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy.xts");
        for name in [
            "Sessions/生产/华东/公司服务器.xsh",
            "Sessions\\生产\\华东\\公司服务器.xsh",
            "Sessions/生产/华东/乗.xsh",
        ] {
            let (encoded, _, errors) = encoding_rs::GB18030.encode(name);
            assert!(!errors);
            fs::write(&path, archive_with_raw_name(&encoded)).unwrap();
            let scan = loaded(&path);
            assert_eq!(scan.candidates.len(), 1);
            assert_eq!(scan.skipped, 0);
            let expected = name
                .rsplit(['/', '\\'])
                .next()
                .unwrap()
                .strip_suffix(".xsh")
                .unwrap();
            assert_eq!(scan.candidates[0].name, expected);
            assert_eq!(scan.candidates[0].options.group, "生产/华东");
        }
    }

    #[test]
    fn utf8_and_cp437_archive_names_remain_supported() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("names.xts");
        for (name, expected) in [
            ("Sessions/主机.xsh".as_bytes(), "主机"),
            (b"Sessions/caf\x82.xsh".as_slice(), "café"),
            (b"Sessions/server.xsh".as_slice(), "server"),
        ] {
            fs::write(&path, archive_with_raw_name(name)).unwrap();
            let scan = loaded(&path);
            assert_eq!(scan.candidates.len(), 1);
            assert_eq!(scan.candidates[0].name, expected);
            assert!(scan.candidates[0].options.group.is_empty());
        }
    }

    #[test]
    fn decoded_archive_names_cannot_escape_the_archive_root() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unsafe.xts");
        for name in [
            "../outside.xsh",
            "生产/../../outside.xsh",
            "..\\outside.xsh",
            "/outside.xsh",
            "C:/outside.xsh",
            "C:outside.xsh",
            "\\\\server\\share\\outside.xsh",
            "Sessions/invalid\0.xsh",
        ] {
            let (encoded, _, errors) = encoding_rs::GB18030.encode(name);
            assert!(!errors);
            fs::write(&path, archive_with_raw_name(&encoded)).unwrap();
            let scan = loaded(&path);
            assert!(scan.candidates.is_empty(), "{name}");
            assert_eq!(scan.skipped, 1, "{name}");
        }
    }

    #[test]
    #[ignore = "requires a local backup and expected host name through SHELLRS_XSHELL_TEST_ARCHIVE and SHELLRS_XSHELL_TEST_HOST_NAME"]
    fn local_xshell_backup_preserves_the_expected_host_name_without_modifying_the_backup() {
        let path = PathBuf::from(std::env::var_os("SHELLRS_XSHELL_TEST_ARCHIVE").unwrap());
        let expected = std::env::var("SHELLRS_XSHELL_TEST_HOST_NAME").unwrap();
        let original = fs::read(&path).unwrap();
        let scan = loaded(&path);
        assert!(
            scan.candidates
                .iter()
                .any(|candidate| candidate.name == expected)
        );
        assert!(fs::read(&path).unwrap() == original);
        println!(
            "Imported {} candidates in memory; skipped {}; backup unchanged",
            scan.candidates.len(),
            scan.skipped
        );
    }

    #[test]
    fn connection_fields_and_folder_groups_are_preserved_without_secrets() {
        let candidate = parse(
            Path::new("生产/华东/应用.xsh"),
            config("SSH", "deploy").as_bytes(),
        )
        .unwrap();
        assert_eq!(candidate.name, "应用");
        assert_eq!(candidate.options.group, "生产/华东");
        assert_eq!(candidate.address, "server.example.com");
        assert_eq!(candidate.port, 2222);
        assert_eq!(candidate.user, "deploy");
        assert_eq!(candidate.options.notes, "生产主机");
        assert_eq!(candidate.options.protocol, ProtocolKind::Ssh);
        assert!(candidate.options.password);
        assert!(candidate.identity.is_none());
        assert!(!format!("{candidate:?}").contains("SECRET_NOT_TO_IMPORT"));
        assert!(!format!("{candidate:?}").contains("secret.key"));
    }

    #[test]
    fn utf8_utf16_and_legacy_chinese_are_read() {
        let text = config("SSH", "运维");
        let utf16le: Vec<u8> = [0xff, 0xfe]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let utf16be: Vec<u8> = [0xfe, 0xff]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        let (legacy, _, _) = encoding_rs::GB18030.encode(&text);
        for bytes in [
            text.as_bytes(),
            utf16le.as_slice(),
            utf16be.as_slice(),
            legacy.as_ref(),
        ] {
            assert_eq!(parse(Path::new("test.xsh"), bytes).unwrap().user, "运维");
        }
        assert!(
            parse(
                Path::new("test.xsh"),
                b"[CONNECTION]\nProtocol=SSH\nHost=x\0evil"
            )
            .is_err()
        );
    }

    #[test]
    fn telnet_is_kept_and_proxy_dependent_or_invalid_hosts_are_not_misdirected() {
        let candidate = parse(
            Path::new("switch.xsh"),
            b"[CONNECTION]\nProtocol=TELNET\nHost=10.0.0.1\n",
        )
        .unwrap();
        assert_eq!(candidate.options.protocol, ProtocolKind::Telnet);
        assert_eq!(candidate.port, 23);
        assert_eq!(candidate.user, "");
        for text in [
            config("RLOGIN", "root"),
            config("SSH", "root").replace("Port=2222", "Port=0"),
            config("SSH", "root").replace("Port=2222", "Port=70000"),
            config("SSH", "root").replace("Host=server.example.com", "Host=bad address"),
            config("SSH", "root").replace("Port=2222", "Port=2222\nProxy=1"),
            config("SSH", "root").replace("Port=2222", "Port=2222\nUseProxy=1"),
            config("SSH", "root").replace("Port=2222", "Port=2222\nHost=other"),
        ] {
            assert!(parse(Path::new("test.xsh"), text.as_bytes()).is_err());
        }
    }

    #[test]
    fn directory_import_is_sorted_bounded_and_deduplicated_by_protocol_and_user() {
        let directory = tempfile::tempdir().unwrap();
        let group = directory.path().join("生产");
        fs::create_dir(&group).unwrap();
        fs::write(group.join("a.xsh"), config("SSH", "root")).unwrap();
        fs::write(group.join("b.XSH"), config("SSH", "deploy")).unwrap();
        fs::write(group.join("copy.xsh"), config("SSH", "root")).unwrap();
        fs::write(group.join("switch.xsh"), config("TELNET", "root")).unwrap();
        fs::write(group.join("unsupported.xsh"), config("RLOGIN", "root")).unwrap();
        fs::write(
            group.join("too-big.xsh"),
            vec![b'x'; MAX_FILE_BYTES as usize + 1],
        )
        .unwrap();
        let scan = loaded(directory.path());
        assert_eq!(scan.candidates.len(), 3);
        assert_eq!(scan.skipped, 2);
        assert_eq!(scan.candidates[0].name, "a");
        assert!(
            scan.candidates
                .iter()
                .all(|candidate| candidate.options.group == "生产")
        );
        let repeated = read_paths(&[directory.path().to_path_buf(), group.join("a.xsh")]);
        assert!(matches!(repeated, ImportLoad::Loaded(scan) if scan.candidates.len() == 3));
    }

    #[test]
    fn archives_are_read_in_memory_with_traversal_entries_skipped() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("backup.xts");
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        archive
            .start_file("Sessions/生产/server.xsh", options)
            .unwrap();
        archive.write_all(config("SSH", "root").as_bytes()).unwrap();
        archive.start_file("../outside.xsh", options).unwrap();
        archive.write_all(config("SSH", "bad").as_bytes()).unwrap();
        let bytes = archive.finish().unwrap().into_inner();
        fs::write(&path, bytes).unwrap();
        let scan = loaded(&path);
        assert_eq!(scan.candidates.len(), 1);
        assert_eq!(scan.candidates[0].options.group, "生产");
        assert_eq!(scan.skipped, 1);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        fs::write(&path, b"not a zip archive").unwrap();
        assert!(matches!(read_paths(&[path]), ImportLoad::Failed(_)));
    }
}
