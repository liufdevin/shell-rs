use std::{borrow::Cow, collections::HashSet, fs::File, io::Read as _, path::Path};

use anyhow::{Result, ensure};

use crate::{
    host::{AuthKind, HostDraft},
    i18n::t,
};

use super::{CandidateOptions, ImportCandidate, ImportLoad, ImportScan, parse_port};

const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;

#[derive(Default)]
struct Folder {
    group: Option<String>,
    invalid: bool,
    entries: Vec<(String, String)>,
}

pub fn read(path: &Path) -> ImportLoad {
    let result = (|| {
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 <= MAX_BYTES, "file limit");
        let single = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("moba"));
        let text = decode(&bytes)?;
        let mut scan = parse(&text, single)?;
        match crate::terminal::parse_mobaxterm_schemes(&text) {
            Ok(schemes) => scan.schemes = schemes,
            Err(error) => scan.scheme_error = Some(error.to_string()),
        }
        Ok(scan)
    })();
    match result {
        Ok(scan) => ImportLoad::Loaded(scan),
        Err(_) => ImportLoad::Failed(t!("host.import.mobaxterm.read_failed").to_string()),
    }
}

fn decode(bytes: &[u8]) -> Result<Cow<'_, str>> {
    let text = if let Some((encoding, offset)) = encoding_rs::Encoding::for_bom(bytes) {
        let (text, errors) = encoding.decode_without_bom_handling(&bytes[offset..]);
        ensure!(!errors, "invalid encoding");
        text
    } else if let Ok(text) = std::str::from_utf8(bytes) {
        Cow::Borrowed(text)
    } else {
        let (text, errors) = encoding_rs::GB18030.decode_without_bom_handling(bytes);
        if errors {
            let (text, errors) = encoding_rs::WINDOWS_1252.decode_without_bom_handling(bytes);
            ensure!(!errors, "invalid encoding");
            text
        } else {
            text
        }
    };
    ensure!(!text.contains('\0'), "invalid encoding");
    Ok(text)
}

fn parse(text: &str, single: bool) -> Result<ImportScan> {
    let mut scan = ImportScan::default();
    let mut folder = single.then(Folder::default);
    let mut entries = 0;
    let mut seen = HashSet::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with([';', '#']) {
            continue;
        }
        if let Some(section) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            if let Some(folder) = folder.take() {
                collect(folder, &mut scan, &mut seen);
            }
            let section = section.to_ascii_lowercase();
            let bookmarks = section == "bookmarks"
                || section.strip_prefix("bookmarks_").is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
                });
            folder = bookmarks.then(Folder::default);
            continue;
        }
        let Some(folder) = &mut folder else { continue };
        entries += 1;
        ensure!(entries <= MAX_ENTRIES, "entry limit");
        let Some((name, value)) = line.split_once('=') else {
            scan.skipped += 1;
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("SubRep") {
            if folder.group.replace(value.to_owned()).is_some() {
                folder.invalid = true;
            }
        } else if !name.eq_ignore_ascii_case("ImgNum") {
            folder.entries.push((name.to_owned(), value.to_owned()));
        }
    }
    if let Some(folder) = folder {
        collect(folder, &mut scan, &mut seen);
    }
    Ok(scan)
}

fn collect(folder: Folder, scan: &mut ImportScan, seen: &mut HashSet<(String, u16, String)>) {
    let group = group_path(folder.group.as_deref().unwrap_or_default());
    for (name, value) in folder.entries {
        let candidate = group
            .as_ref()
            .ok()
            .filter(|_| !folder.invalid)
            .and_then(|group| candidate(&name, &value, group).ok());
        if let Some(candidate) = candidate {
            let key = (
                candidate.address.to_lowercase(),
                candidate.port,
                candidate.user.clone(),
            );
            if seen.insert(key) {
                scan.candidates.push(candidate);
            }
        } else {
            scan.skipped += 1;
        }
    }
}

fn group_path(group: &str) -> Result<String> {
    let group = group.trim();
    if group.is_empty() {
        return Ok(String::new());
    }
    ensure!(
        group.len() <= 4096 && !group.starts_with(['/', '\\']) && !group.ends_with(['/', '\\']),
        "invalid group"
    );
    let levels: Vec<_> = group
        .split(['/', '\\'])
        .filter(|level| !level.is_empty())
        .collect();
    ensure!(levels.len() <= 32, "group depth limit");
    for level in &levels {
        ensure!(
            !matches!(*level, "." | "..")
                && !level.trim().is_empty()
                && !level.contains(':')
                && !level.chars().any(char::is_control),
            "invalid group"
        );
    }
    Ok(levels.join("/"))
}

fn candidate(name: &str, value: &str, group: &str) -> Result<ImportCandidate> {
    let blocks: Vec<_> = value.split('#').take(17).collect();
    ensure!(blocks.len() >= 3 && blocks.len() <= 16, "invalid bookmark");
    ensure!(
        matches!(blocks[0].trim(), "" | "; logout" | ";  logout")
            && blocks[1].parse::<u16>().is_ok(),
        "invalid bookmark"
    );
    let fields: Vec<_> = blocks[2].split('%').take(129).collect();
    ensure!(
        (22..=128).contains(&fields.len()) && fields[0] == "0",
        "unsupported protocol"
    );
    ensure!(
        fields[8].is_empty() && matches!(fields[19], "" | "0"),
        "gateway or proxy required"
    );
    ensure!(
        matches!(fields.get(27).copied().unwrap_or_default(), "" | "0" | "1"),
        "unsupported ssh version"
    );
    let address = fields[1].trim();
    ensure!(
        address.len() <= 1024 && !address.chars().any(char::is_whitespace),
        "invalid address"
    );
    let port = parse_port(fields[2]).ok_or_else(|| anyhow::anyhow!("invalid port"))?;
    let user = match fields[3].trim() {
        "<default>" => "",
        user => user,
    };
    let name = unescape(name);
    let draft = HostDraft::new(name.clone(), address, port, user, AuthKind::Password, None)
        .validated()
        .map_err(|_| anyhow::anyhow!("invalid host"))?;
    ensure!(
        draft.user.len() <= 256 && !draft.user.chars().any(char::is_control),
        "invalid user"
    );
    let notes = unescape(blocks.get(5).copied().unwrap_or_default())
        .trim()
        .chars()
        .take(2000)
        .collect();
    Ok(ImportCandidate {
        name,
        address: draft.address.to_string(),
        port,
        user: draft.user.to_string(),
        identity: None,
        options: CandidateOptions {
            group: group.to_owned(),
            password: true,
            match_user: true,
            notes,
            ..CandidateOptions::default()
        },
    })
}

fn unescape(text: &str) -> String {
    text.replace("__DIEZE__", "#")
        .replace("__PIPE__", "|")
        .replace("__DBLQUO__", "\"")
        .replace("__PTVIRG__", ";")
        .replace("__PERCENT__", "%")
}

#[cfg(test)]
mod tests {
    use super::{ImportLoad, MAX_BYTES, decode, parse, read};

    fn bookmark(host: &str, port: &str, user: &str) -> String {
        let mut fields = vec![""; 36];
        fields[0] = "0";
        fields[1] = host;
        fields[2] = port;
        fields[3] = user;
        fields[19] = "0";
        format!("#109#{}#MobaFont%12#0#备注__DIEZE__1#-1", fields.join("%"))
    }

    #[test]
    fn bookmarks_preserve_nested_folders_notes_ports_and_distinct_users_without_secrets() {
        let entry = bookmark("example.test", "2222", "deploy");
        let text = format!(
            "[Misc]\nLastSession=recent|{entry}\n[Passwords]\nssh=user:TOP_SECRET\n[Bookmarks]\nSubRep=\nImgNum=42\nroot={}\n[Bookmarks_2]\napp={entry}\nSubRep=生产\\\\华东\ncopy={entry}\nroot={}\n",
            bookmark("root.test", "22", "<default>"),
            bookmark("example.test", "2222", "root")
        );
        let scan = parse(&text, false).unwrap();
        assert_eq!(scan.candidates.len(), 3);
        assert_eq!(scan.skipped, 0);
        let host = &scan.candidates[1];
        assert_eq!(
            (&host.name[..], &host.options.group[..], &host.user[..]),
            ("app", "生产/华东", "deploy")
        );
        assert_eq!(host.port, 2222);
        assert_eq!(host.options.notes, "备注#1");
        assert!(host.options.password && host.options.match_user && host.identity.is_none());
        assert!(!format!("{scan:?}").contains("TOP_SECRET"));
    }

    #[test]
    fn invalid_unsupported_proxy_and_gateway_records_are_skipped() {
        let entry = bookmark("example.test", "22", "ops");
        let replace_field = |index: usize, value: &str| {
            let mut blocks: Vec<_> = entry.split('#').map(str::to_owned).collect();
            let mut fields: Vec<_> = blocks[2].split('%').map(str::to_owned).collect();
            fields[index] = value.to_owned();
            blocks[2] = fields.join("%");
            blocks.join("#")
        };
        let invalid = [
            replace_field(0, "4"),
            replace_field(2, "0"),
            replace_field(8, "bastion"),
            replace_field(19, "2"),
            replace_field(27, "2"),
            "broken".into(),
        ];
        let mut text = "[Bookmarks]\nSubRep=Valid\n".to_string();
        for (index, record) in invalid.iter().enumerate() {
            text.push_str(&format!("bad-{index}={record}\n"));
        }
        text.push_str(&format!("[Bookmarks_1]\nSubRep=../outside\nbad={entry}\n[Bookmarks_2]\nSubRep=First\nSubRep=Other\nbad={entry}\n"));
        let scan = parse(&text, false).unwrap();
        assert!(scan.candidates.is_empty());
        assert_eq!(scan.skipped, 8);
    }

    #[test]
    fn interface_exports_and_recent_connections_are_not_saved_hosts() {
        let text = format!(
            "[Misc]\nLastSession=recent|{}\n[ImportExportSpecials]\nExportedSettings=MobaXterm interface settings\n",
            bookmark("example.test", "22", "ops")
        );
        let scan = parse(&text, false).unwrap();
        assert!(scan.candidates.is_empty());
        assert_eq!(scan.skipped, 0);
        assert_eq!(
            parse(
                &format!("single={}\n", bookmark("single.test", "22", "ops")),
                true
            )
            .unwrap()
            .candidates
            .len(),
            1
        );
    }

    #[test]
    fn supported_encodings_and_size_limits_are_checked() {
        let text = format!(
            "[Bookmarks]\n中文={}\n",
            bookmark("example.test", "22", "ops")
        );
        let (encoded, _, errors) = encoding_rs::GB18030.encode(&text);
        assert!(!errors);
        assert_eq!(decode(&encoded).unwrap(), text);
        let mut utf16 = vec![0xff, 0xfe];
        utf16.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(decode(&utf16).unwrap(), text);
        assert!(decode(b"invalid\0text").is_err());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.mobaconf");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();
        assert!(matches!(read(&path), ImportLoad::Failed(_)));
    }

    #[test]
    #[ignore = "reads the user-provided local export without changing it"]
    fn supplied_export_contains_hosts_and_color_schemes() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("MobaXterm configuration (2026-11-12 20.55.25).mobaconf");
        let ImportLoad::Loaded(scan) = read(&path) else {
            panic!("could not read the supplied export")
        };
        assert_eq!(scan.candidates.len(), 4);
        assert_eq!(scan.skipped, 5);
        assert!(scan.scheme_error.is_none(), "{:?}", scan.scheme_error);
        assert_eq!(scan.schemes.len(), 12);
        assert!(scan.schemes.iter().any(|theme| theme.name() == "Monokai"));
        assert!(
            scan.schemes
                .iter()
                .any(|theme| theme.name() == "浅色背景/深色文字")
        );
    }
}
