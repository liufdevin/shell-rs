use std::collections::HashSet;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SerialCommandEnding {
    #[default]
    None,
    Lf,
    Cr,
    CrLf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SerialCommand {
    pub id: u64,
    pub name: String,
    pub content: String,
    pub hex: bool,
    pub ending: SerialCommandEnding,
    pub crc: bool,
    pub delay_ms: u32,
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SerialCommandGroup {
    pub id: u64,
    pub name: String,
    pub commands: Vec<SerialCommand>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SerialAssistant {
    pub enabled: bool,
    pub groups: Vec<SerialCommandGroup>,
    pub next_id: u64,
}

impl SerialAssistant {
    pub fn allocate_id(&mut self) -> u64 {
        self.next_id = self.next_id.saturating_add(1);
        self.next_id
    }

    pub fn is_valid(&self) -> bool {
        let mut ids = HashSet::new();
        let mut size = 0;
        self.groups.len() <= 32
            && self.groups.iter().all(|group| {
                group.id > 0
                    && group.id <= self.next_id
                    && ids.insert(group.id)
                    && !group.name.trim().is_empty()
                    && group.name.len() <= 128
                    && group.commands.len() <= 128
                    && group.commands.iter().all(|command| {
                        size += command.content.len();
                        command.id > 0
                            && command.id <= self.next_id
                            && ids.insert(command.id)
                            && !command.name.trim().is_empty()
                            && command.name.len() <= 128
                            && !command.content.is_empty()
                            && command.content.len() <= 4096
                            && command.delay_ms <= 60000
                            && size <= 256 * 1024
                    })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HostData, HostDatabase, HostDraft, SerialSettings};

    fn example() -> SerialAssistant {
        let mut value = SerialAssistant {
            enabled: true,
            ..Default::default()
        };
        let mut group = SerialCommandGroup {
            id: value.allocate_id(),
            name: "开发板启动".into(),
            ..Default::default()
        };
        let command = SerialCommand {
            id: value.allocate_id(),
            name: "启动".into(),
            content: "01 03 00 00 00 0A".into(),
            hex: true,
            ending: SerialCommandEnding::CrLf,
            crc: true,
            delay_ms: 500,
            enabled: true,
        };
        group.commands.push(command);
        value.groups.push(group);
        value
    }

    #[test]
    fn defaults_are_off_and_limits_reject_oversized_or_duplicate_entries() {
        assert!(!SerialAssistant::default().enabled);
        let mut value = example();
        assert!(value.is_valid());
        value.groups[0].commands[0].content = "a".repeat(4097);
        assert!(!value.is_valid());
        value = example();
        value.groups[0].commands[0].id = value.groups[0].id;
        assert!(!value.is_valid());
        value = example();
        value.groups[0].commands[0].delay_ms = 60001;
        assert!(!value.is_valid());
    }

    #[test]
    fn preferences_commands_and_order_persist_per_host_and_delete_with_host() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hosts.db");
        let mut data = HostData::load(HostDatabase::open(&path).unwrap()).unwrap();
        let first = data.insert(HostDraft::serial(
            "first",
            "TEST-A",
            SerialSettings::default(),
            None,
        ));
        let second = data.insert(HostDraft::serial(
            "second",
            "TEST-B",
            SerialSettings::default(),
            None,
        ));
        let key = format!("host:{}", data.host(first).unwrap().public_id);
        let second_key = format!("host:{}", data.host(second).unwrap().public_id);
        let value = example();
        assert!(data.set_serial_assistant(key.clone(), Some(first), value.clone()));
        assert!(data.serial_assistant(&second_key).is_none());
        drop(data);
        let mut data = HostData::load(HostDatabase::open(&path).unwrap()).unwrap();
        assert_eq!(data.serial_assistant(&key), Some(&value));
        assert!(data.set_serial_assistant("lan:peer:serial:device".into(), None, value.clone()));
        let mut disabled = value.clone();
        disabled.enabled = false;
        assert!(data.set_serial_assistant(key.clone(), Some(first), disabled.clone()));
        assert_eq!(data.serial_assistant(&key).unwrap().groups, value.groups);
        data.remove(first);
        assert!(data.serial_assistant(&key).is_none());
        drop(data);
        let data = HostData::load(HostDatabase::open(&path).unwrap()).unwrap();
        assert!(data.serial_assistant(&key).is_none());
        assert_eq!(
            data.serial_assistant("lan:peer:serial:device"),
            Some(&value)
        );
    }

    #[test]
    fn version_27_migrates_without_changing_hosts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("migration.db");
        let mut data = HostData::load(HostDatabase::open(&path).unwrap()).unwrap();
        let host = data.insert(HostDraft::serial(
            "console",
            "TEST-A",
            SerialSettings::default(),
            None,
        ));
        drop(data);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch("DROP TABLE serial_assistants; PRAGMA user_version = 27;")
            .unwrap();
        drop(connection);
        let data = HostData::load(HostDatabase::open(&path).unwrap()).unwrap();
        assert_eq!(data.host(host).unwrap().name.as_str(), "console");
        assert!(data.serial_assistant("none").is_none());
    }
}
