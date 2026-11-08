use std::collections::VecDeque;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

const MAX_ENTRIES: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    SharingChanged,
    PermissionChanged,
    Open,
    Denied,
    Closed,
    Break,
    Revoked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AuditEntry {
    #[serde(default)]
    pub sequence: u64,
    pub timestamp: u64,
    pub action: AuditAction,
    pub peer: String,
    pub source: Option<IpAddr>,
    pub device: String,
    #[serde(default)]
    pub device_name: String,
    pub outcome: String,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

impl AuditEntry {
    pub(super) fn new(action: AuditAction, peer: &str, device: &str, outcome: &str) -> Self {
        Self {
            sequence: 0,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            action,
            peer: peer
                .chars()
                .filter(|character| !character.is_control())
                .take(128)
                .collect(),
            source: None,
            device: device
                .chars()
                .filter(|character| !character.is_control())
                .take(128)
                .collect(),
            device_name: String::new(),
            outcome: outcome
                .chars()
                .filter(|character| !character.is_control())
                .take(128)
                .collect(),
            bytes_in: 0,
            bytes_out: 0,
        }
    }
}

pub(super) struct AuditLog {
    path: PathBuf,
    entries: VecDeque<AuditEntry>,
}

impl AuditLog {
    pub fn load(directory: &Path) -> Result<Self> {
        let path = directory.join("device-audit.json");
        let entries: VecDeque<AuditEntry> = match crate::storage::read_file(&path, 1024 * 1024) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => VecDeque::new(),
            Err(error) => return Err(error.into()),
        };
        ensure!(entries.len() <= MAX_ENTRIES, "device_audit_invalid");
        Ok(Self { path, entries })
    }

    pub fn entries(&self) -> Vec<AuditEntry> {
        self.entries.iter().cloned().collect()
    }

    pub fn append(&mut self, mut entry: AuditEntry) -> Result<()> {
        let mut entries = self.entries.clone();
        entry.sequence = entries
            .back()
            .map_or(1, |previous| previous.sequence.saturating_add(1));
        entries.push_back(entry);
        while entries.len() > MAX_ENTRIES {
            entries.pop_front();
        }
        let bytes = serde_json::to_vec(&entries)?;
        ensure!(bytes.len() <= 1024 * 1024, "device_audit_full");
        let mut file = tempfile::NamedTempFile::new_in(self.path.parent().unwrap())?;
        std::io::Write::write_all(&mut file, &bytes)?;
        file.as_file().sync_all()?;
        file.persist(&self.path)?;
        self.entries = entries;
        Ok(())
    }
}
