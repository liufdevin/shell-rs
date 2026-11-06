use super::{AdbConnection, client::Runner};
use anyhow::{Result, anyhow, bail};
use std::{
    collections::HashMap,
    ffi::OsString,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
    time::Duration,
};

pub(super) const BOARD_BOOT: &str = "00000000-0000-4000-8000-000000000001";
pub(super) const OTHER_BOOT: &str = "00000000-0000-4000-8000-000000000002";

#[derive(Default)]
pub(super) struct PtyOnlyAdb {
    pub inner: Arc<FakeAdb>,
    pub rejected: AtomicUsize,
    pub allocated: AtomicUsize,
    pub missing_base64: bool,
    pub truncated: bool,
}

impl PtyOnlyAdb {
    pub fn connection(self: &Arc<Self>) -> AdbConnection {
        AdbConnection::test_connection(self.clone())
    }
}

impl Runner for PtyOnlyAdb {
    fn run(
        &self,
        args: &[OsString],
        timeout: Duration,
        cancel: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<u8>> {
        use base64::Engine as _;
        if args.get(2).is_none_or(|arg| arg != "shell") {
            return self.inner.run(args, timeout, cancel);
        }
        if args.get(3).is_some_and(|arg| arg == "-T") {
            self.rejected.fetch_add(1, Ordering::SeqCst);
            return Err(super::client::PtyRequired.into());
        }
        assert_eq!(args[3], "-tt");
        self.allocated.fetch_add(1, Ordering::SeqCst);
        let wrapped = args.last().unwrap().to_str().unwrap();
        let marker = wrapped
            .split_once("printf '")
            .unwrap()
            .1
            .split_once("N\\n'")
            .unwrap()
            .0;
        if self.missing_base64 {
            return Ok(format!("{marker}N\r\n").into_bytes());
        }
        let original = wrapped
            .split_once("; { ")
            .unwrap()
            .1
            .rsplit_once("\n} 2>&1 | base64;")
            .unwrap()
            .0;
        let mut inner_args = args.to_vec();
        inner_args[3] = "-T".into();
        *inner_args.last_mut().unwrap() = original.into();
        let output = self.inner.run(&inner_args, timeout, cancel)?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(output);
        let encoded = encoded
            .as_bytes()
            .chunks(16)
            .map(|line| std::str::from_utf8(line).unwrap())
            .collect::<Vec<_>>()
            .join("\r\n");
        let end = if self.truncated {
            String::new()
        } else {
            format!("\r\n{marker}E\r\n")
        };
        Ok(format!("device banner\r\n{marker}B\r\n{encoded}{end}").into_bytes())
    }
}

#[derive(Clone)]
pub(super) struct Entry {
    pub mode: u32,
    pub bytes: Vec<u8>,
    pub modified: u32,
}

pub(super) struct FakeAdb {
    pub devices: Vec<(String, String)>,
    pub models: Mutex<HashMap<String, String>>,
    pub identities: Mutex<HashMap<String, String>>,
    pub entries: Mutex<HashMap<String, Entry>>,
    pub requests: Mutex<Vec<Vec<String>>>,
    pub hold_push: AtomicBool,
    pub change_during_push: AtomicBool,
    pub clock: AtomicU32,
}

impl Default for FakeAdb {
    fn default() -> Self {
        Self {
            devices: vec![("board".into(), "device".into())],
            models: Mutex::default(),
            identities: Mutex::new(HashMap::from([("board".into(), BOARD_BOOT.into())])),
            entries: Mutex::new(HashMap::from([(
                "/".into(),
                Entry {
                    mode: 0o40755,
                    bytes: Vec::new(),
                    modified: 1,
                },
            )])),
            requests: Mutex::new(Vec::new()),
            hold_push: AtomicBool::new(false),
            change_during_push: AtomicBool::new(false),
            clock: AtomicU32::new(2),
        }
    }
}

impl FakeAdb {
    pub fn connection(self: &Arc<Self>) -> AdbConnection {
        AdbConnection::test_connection(self.clone())
    }
    pub fn file(&self, path: &str, bytes: &[u8]) {
        self.entries.lock().unwrap().insert(
            path.to_owned(),
            Entry {
                mode: 0o100644,
                bytes: bytes.to_vec(),
                modified: self.clock.fetch_add(1, Ordering::SeqCst),
            },
        );
    }
}

fn quoted(script: &str) -> Vec<String> {
    script
        .split('\'')
        .enumerate()
        .filter(|(index, _)| index % 2 == 1)
        .map(|(_, value)| value.to_owned())
        .collect()
}

impl Runner for FakeAdb {
    fn run(
        &self,
        args: &[OsString],
        _: Duration,
        cancel: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<u8>> {
        let args: Vec<_> = args
            .iter()
            .map(|value| value.to_str().unwrap().to_owned())
            .collect();
        self.requests.lock().unwrap().push(args.clone());
        if args.first().is_some_and(|arg| arg == "devices") {
            let mut output = String::from("List of devices attached\n");
            let models = self.models.lock().unwrap();
            for (serial, state) in &self.devices {
                output.push_str(serial);
                output.push('\t');
                output.push_str(state);
                if let Some(model) = models.get(serial) {
                    output.push_str(" model:");
                    output.push_str(model);
                }
                output.push('\n');
            }
            return Ok(output.into_bytes());
        }
        let serial = &args[1];
        match args[2].as_str() {
            "get-state" => return Ok(b"device\n".to_vec()),
            "push" => {
                let bytes = std::fs::read(&args[3])?;
                self.file(&args[4], &bytes);
                if self.change_during_push.load(Ordering::SeqCst) {
                    self.file("/file.txt", b"changed elsewhere");
                }
                while self.hold_push.load(Ordering::SeqCst) {
                    if cancel() {
                        bail!("cancelled");
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                return Ok(Vec::new());
            }
            "pull" => {
                let data = self
                    .entries
                    .lock()
                    .unwrap()
                    .get(&args[3])
                    .ok_or_else(|| anyhow!("missing fake remote file"))?
                    .bytes
                    .clone();
                std::fs::write(&args[4], data)?;
                return Ok(Vec::new());
            }
            "shell" => {}
            _ => bail!("unsupported fake adb command: {args:?}"),
        }
        let script = args.last().unwrap();
        if script == "cat /proc/sys/kernel/random/boot_id" {
            return Ok(format!(
                "{}\n",
                self.identities
                    .lock()
                    .unwrap()
                    .get(serial)
                    .ok_or_else(|| anyhow!("unknown device"))?
            )
            .into_bytes());
        }
        let marker = script
            .rsplit_once("printf '\\000")
            .ok_or_else(|| anyhow!("missing marker"))?
            .1
            .split("%s")
            .next()
            .unwrap();
        let paths = quoted(script);
        let mut output = Vec::new();
        if script.starts_with("( cd ") {
            let path = &paths[0];
            output.extend_from_slice(format!("SHELLLF_ADB_LIST\0{path}\0").as_bytes());
            let prefix = format!("{}/", path.trim_end_matches('/'));
            for (name, entry) in self.entries.lock().unwrap().iter() {
                let Some(name) = name
                    .strip_prefix(&prefix)
                    .filter(|name| !name.is_empty() && !name.contains('/'))
                else {
                    continue;
                };
                output.extend_from_slice(
                    format!(
                        "{:x} {} {}\0{name}\0",
                        entry.mode,
                        entry.bytes.len(),
                        entry.modified
                    )
                    .as_bytes(),
                );
            }
            output.extend_from_slice(b"END\0");
        } else if script.starts_with("( if [ -e ") {
            if let Some(entry) = self.entries.lock().unwrap().get(&paths[0]) {
                output.extend_from_slice(
                    format!(
                        "{:x} {} {}\n",
                        entry.mode,
                        entry.bytes.len(),
                        entry.modified
                    )
                    .as_bytes(),
                );
            }
        } else if script.starts_with("( mv -f -- ") {
            let mut entries = self.entries.lock().unwrap();
            let entry = entries
                .remove(&paths[0])
                .ok_or_else(|| anyhow!("missing staging file"))?;
            entries.insert(paths[1].clone(), entry);
        } else if script.starts_with("( rm -f -- ") {
            self.entries.lock().unwrap().remove(&paths[0]);
        } else if script.starts_with("( chmod ") {
        } else if script.starts_with("( mkdir -- ") {
            self.entries.lock().unwrap().insert(
                paths[0].clone(),
                Entry {
                    mode: 0o40755,
                    bytes: Vec::new(),
                    modified: 1,
                },
            );
        } else {
            bail!("unsupported fake shell script: {script}");
        }
        output.extend_from_slice(format!("\0{marker}0\n").as_bytes());
        Ok(output)
    }
}
