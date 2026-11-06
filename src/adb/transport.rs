use super::{AdbConnection, client::quote};
use crate::{
    i18n::t,
    sftp::{
        DownloadRequest, EDIT_LIMIT, EntryKind, FileBytes, FileMetadata, FileStamp, ReadFailure,
        RemoteOperation, RemotePath, SaveFailure, SftpCommand, SftpEvent, SftpTransport, TextFile,
        TransferChoice, TransferDetail, TransferDirection, TransferPhase, TransferProgress,
        TransferQuestion, TransferQuestionKind, UploadRequest,
    },
};
use anyhow::{Result, anyhow, bail};
use async_channel::{Receiver, Sender};
use std::{
    collections::VecDeque,
    ffi::OsStr,
    path::Path,
    time::{Duration, Instant},
};

pub(crate) struct AdbTransport(pub AdbConnection);

#[derive(Clone)]
enum Job {
    Upload(UploadRequest),
    Download(DownloadRequest),
}

struct Worker {
    connection: AdbConnection,
    commands: Receiver<SftpCommand>,
    events: Sender<SftpEvent>,
    pending: VecDeque<SftpCommand>,
    cancelled: bool,
    shutdown: bool,
    disconnected: bool,
    connected: bool,
    question_id: u64,
    conflict: Option<TransferChoice>,
    saved: Option<Job>,
    direction: TransferDirection,
    details: Vec<TransferDetail>,
    bytes: u64,
    total: u64,
    items: usize,
    visited: usize,
    checked: Instant,
}

#[derive(Debug)]
struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ADB transfer cancelled")
    }
}
impl std::error::Error for Cancelled {}

impl SftpTransport for AdbTransport {
    fn run(
        self: Box<Self>,
        commands: Receiver<SftpCommand>,
        events: Sender<SftpEvent>,
    ) -> Result<()> {
        let mut worker = Worker {
            connection: self.0,
            commands,
            events,
            pending: VecDeque::new(),
            cancelled: false,
            shutdown: false,
            disconnected: false,
            connected: true,
            question_id: 0,
            conflict: None,
            saved: None,
            direction: TransferDirection::Upload,
            details: Vec::new(),
            bytes: 0,
            total: 0,
            items: 0,
            visited: 0,
            checked: Instant::now(),
        };
        worker.emit(SftpEvent::Connecting);
        worker.connection.clone().verify(&mut || worker.control())?;
        worker.emit(SftpEvent::Connected {
            home: RemotePath::new("/")?,
        });
        while !worker.shutdown {
            let Some(command) = worker.next_command() else {
                break;
            };
            worker.cancelled = false;
            match command {
                SftpCommand::List { request_id, path } => {
                    let result = worker.list(&path).map_err(|error| error.to_string());
                    worker.emit(SftpEvent::Listed { request_id, result });
                }
                SftpCommand::Operate {
                    request_id,
                    operation,
                } => {
                    let result = worker.operate(operation).map_err(|error| error.to_string());
                    worker.emit(SftpEvent::Operated { request_id, result });
                }
                SftpCommand::Upload(request) => worker.transfer(Job::Upload(request)),
                SftpCommand::Download(request) => worker.transfer(Job::Download(request)),
                SftpCommand::ReadFile { request_id, path } => {
                    let result = worker
                        .read_file(&path, EDIT_LIMIT)
                        .and_then(|(bytes, stamp)| TextFile::decode(bytes, stamp));
                    worker.emit(SftpEvent::FileRead { request_id, result });
                }
                SftpCommand::ReadBytes {
                    request_id,
                    path,
                    limit,
                } => {
                    let result = worker
                        .read_file(&path, limit.min(32 * 1024 * 1024))
                        .map(|(bytes, _)| FileBytes::new(bytes));
                    worker.emit(SftpEvent::BytesRead { request_id, result });
                }
                SftpCommand::WriteFile {
                    request_id,
                    path,
                    bytes,
                    expected,
                } => {
                    let result = worker.write_file(&path, bytes, expected);
                    worker.emit(SftpEvent::FileWritten { request_id, result });
                }
                SftpCommand::Resume => match worker.verify() {
                    Ok(()) => {
                        let reconnected = !worker.connected;
                        worker.connected = true;
                        if reconnected {
                            worker.emit(SftpEvent::Connected {
                                home: RemotePath::new("/")?,
                            });
                        }
                        if let Some(job) = worker.saved.take() {
                            worker.transfer(job);
                        } else {
                            worker.emit(SftpEvent::Idle);
                        }
                    }
                    Err(error) => {
                        worker.emit(SftpEvent::Disconnected(error.to_string()));
                        worker.emit(SftpEvent::Idle);
                    }
                },
                SftpCommand::Discard => {
                    worker.saved = None;
                    worker.emit(SftpEvent::Idle);
                }
                SftpCommand::Disconnect => {
                    worker.connected = false;
                    worker.emit(SftpEvent::Disconnected(t!("adb.unavailable").to_string()));
                }
                SftpCommand::Shutdown => break,
                SftpCommand::Cancel
                | SftpCommand::Answer { .. }
                | SftpCommand::PromptReply { .. } => {}
            }
            if worker.disconnected {
                worker.disconnected = false;
                worker.connected = false;
                worker.emit(SftpEvent::Disconnected(t!("adb.unavailable").to_string()));
            }
        }
        Ok(())
    }
}

impl Worker {
    fn next_command(&mut self) -> Option<SftpCommand> {
        loop {
            if self.shutdown {
                return None;
            }
            if let Some(command) = self.pending.pop_front() {
                return Some(command);
            }
            match self.commands.try_recv() {
                Ok(command) => return Some(command),
                Err(async_channel::TryRecvError::Closed) => return None,
                Err(async_channel::TryRecvError::Empty) => {
                    if self.connected && self.checked.elapsed() >= Duration::from_secs(5) {
                        if let Err(error) = self.verify() {
                            self.connected = false;
                            self.emit(SftpEvent::Disconnected(error.to_string()));
                        }
                        self.checked = Instant::now();
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }
    fn emit(&self, event: SftpEvent) {
        let _ = self.events.try_send(event);
    }

    fn control(&mut self) -> bool {
        while let Ok(command) = self.commands.try_recv() {
            match command {
                SftpCommand::Cancel => self.cancelled = true,
                SftpCommand::Shutdown => {
                    self.shutdown = true;
                    self.cancelled = true;
                }
                SftpCommand::Disconnect => {
                    self.disconnected = true;
                    self.cancelled = true;
                }
                command => self.pending.push_back(command),
            }
        }
        if self.commands.is_closed() {
            self.shutdown = true;
            self.cancelled = true;
        }
        self.cancelled
    }

    fn check(&mut self) -> Result<()> {
        if self.control() {
            Err(Cancelled.into())
        } else {
            Ok(())
        }
    }

    fn shell(&mut self, script: &str) -> Result<Vec<u8>> {
        self.check()?;
        self.connection
            .clone()
            .shell(script, &mut || self.control())
    }

    fn verify(&mut self) -> Result<()> {
        self.connection.clone().verify(&mut || self.control())
    }

    fn metadata(&mut self, path: &RemotePath) -> Result<Option<FileMetadata>> {
        self.check()?;
        self.connection
            .clone()
            .metadata(path.as_str(), &mut || self.control())
    }

    fn list(&mut self, path: &RemotePath) -> Result<crate::sftp::DirectoryListing> {
        self.verify()?;
        self.connection
            .clone()
            .list(path.as_str(), &mut || self.control())
    }

    fn progress(&self, phase: TransferPhase, source: &str, target: &str) {
        self.emit(SftpEvent::Progress(
            TransferProgress::new(phase)
                .with_direction(self.direction)
                .with_bytes(self.bytes, self.total)
                .with_total(self.items)
                .with_current(source, target, 0, 0)
                .with_details(self.details.clone()),
        ));
    }

    fn transfer(&mut self, job: Job) {
        self.saved = None;
        self.details.clear();
        self.bytes = 0;
        self.total = 0;
        self.items = 0;
        self.visited = 0;
        self.conflict = None;
        self.direction = match &job {
            Job::Upload(_) => TransferDirection::Upload,
            Job::Download(_) => TransferDirection::Download,
        };
        self.progress(TransferPhase::Scanning, "", "");
        let result = self.verify().and_then(|()| match &job {
            Job::Upload(request) => self.upload(request),
            Job::Download(request) => self.download(request),
        });
        if let Err(error) = result {
            if self.cancelled || error.is::<Cancelled>() {
                self.saved = Some(job);
                self.progress(TransferPhase::Stopped, "", "");
            } else if self.verify().is_err() {
                self.connected = false;
                self.saved = Some(job);
                self.progress(TransferPhase::Stopped, "", "");
                self.emit(SftpEvent::Disconnected(error.to_string()));
            } else {
                self.details
                    .push(TransferDetail::failed(String::new(), error));
                self.progress(TransferPhase::Completed, "", "");
            }
        } else {
            self.progress(TransferPhase::Completed, "", "");
        }
        self.emit(SftpEvent::Idle);
    }

    fn overwrite(&mut self, path: &str) -> Result<bool> {
        if let Some(choice) = self.conflict {
            return Ok(choice == TransferChoice::Overwrite);
        }
        self.question_id += 1;
        let id = self.question_id;
        self.progress(TransferPhase::Waiting, "", path);
        self.emit(SftpEvent::Question(TransferQuestion::new(
            id,
            TransferQuestionKind::Conflict,
            path,
            anyhow!(t!("adb.conflict")),
        )));
        loop {
            self.check()?;
            if let Some(index) = self.pending.iter().position(|command| matches!(command, SftpCommand::Answer { request_id, .. } if *request_id == id)) {
                let SftpCommand::Answer { answer, .. } = self.pending.remove(index).unwrap() else { unreachable!() };
                match answer.choice() {
                    TransferChoice::Overwrite | TransferChoice::Skip => {
                        if answer.apply_to_all() { self.conflict = Some(answer.choice()); }
                        return Ok(answer.choice() == TransferChoice::Overwrite);
                    }
                    _ => { self.cancelled = true; return Err(Cancelled.into()); }
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn upload(&mut self, request: &UploadRequest) -> Result<()> {
        if request.sync_deletes().is_some() {
            bail!(t!("adb.unsupported"));
        }
        for source in request.sources() {
            if is_local_link(&std::fs::symlink_metadata(source)?) {
                bail!(t!("adb.link"));
            }
            let source = source.canonicalize()?;
            let name = match request.target_name() {
                Some(name) => name.to_owned(),
                None => source
                    .file_name()
                    .and_then(OsStr::to_str)
                    .ok_or_else(|| anyhow!(t!("adb.invalid_path")))?
                    .to_owned(),
            };
            let target = if request.is_scp()
                && self
                    .metadata(request.destination())?
                    .is_none_or(|metadata| metadata.kind() != EntryKind::Directory)
            {
                request.destination().clone()
            } else {
                request.destination().join(&name)?
            };
            self.upload_item(&source, &target, 0)?;
        }
        Ok(())
    }

    fn upload_item(&mut self, source: &Path, target: &RemotePath, depth: usize) -> Result<()> {
        self.check()?;
        validate_remote_mutation(target)?;
        self.visited += 1;
        if depth > 64 || self.visited > 10000 {
            bail!(t!("adb.limit"));
        }
        let metadata = std::fs::symlink_metadata(source)?;
        if is_local_link(&metadata) {
            bail!(t!("adb.link"));
        }
        let existing = self.metadata(target)?;
        if metadata.is_dir() {
            if existing
                .as_ref()
                .is_some_and(|metadata| metadata.kind() != EntryKind::Directory)
            {
                bail!(t!("adb.type_conflict"));
            }
            if existing.is_none() {
                self.shell(&format!("mkdir -- {}", quote(target.as_str())?))?;
            }
            for entry in std::fs::read_dir(source)? {
                let entry = entry?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow!(t!("adb.invalid_path")))?;
                self.upload_item(&entry.path(), &target.join(&name)?, depth + 1)?;
            }
            return Ok(());
        }
        if !metadata.is_file()
            || existing
                .as_ref()
                .is_some_and(|metadata| metadata.kind() != EntryKind::File)
        {
            bail!(t!("adb.type_conflict"));
        }
        self.items += 1;
        self.total = self.total.saturating_add(metadata.len());
        if existing.is_some() && !self.overwrite(target.as_str())? {
            self.details
                .push(TransferDetail::skipped(target.to_string()));
            return Ok(());
        }
        self.progress(
            TransferPhase::Transferring,
            &source.to_string_lossy(),
            target.as_str(),
        );
        self.push_file(source, target, existing.as_ref().map(FileStamp::of))?;
        self.bytes = self.bytes.saturating_add(metadata.len());
        self.details.push(TransferDetail::done(target.to_string()));
        self.progress(
            TransferPhase::Transferring,
            &source.to_string_lossy(),
            target.as_str(),
        );
        Ok(())
    }

    fn push_file(
        &mut self,
        source: &Path,
        target: &RemotePath,
        expected: Option<FileStamp>,
    ) -> Result<()> {
        validate_remote_mutation(target)?;
        self.verify()?;
        let temporary = target.parent().join(&format!(
            ".shelllf-adb-{}.filepart",
            uuid::Uuid::new_v4().simple()
        ))?;
        let connection = self.connection.clone();
        let result = connection
            .run(
                &[
                    OsStr::new("push"),
                    source.as_os_str(),
                    OsStr::new(temporary.as_str()),
                ],
                Duration::from_secs(1800),
                &mut || self.control(),
            )
            .and_then(|_| {
                self.verify()?;
                let current = self.metadata(target)?;
                if current.as_ref().map(FileStamp::of) != expected
                    || current
                        .as_ref()
                        .is_some_and(|metadata| metadata.kind() != EntryKind::File)
                {
                    bail!(t!("adb.file_changed"));
                }
                if let Some(mode) = current.as_ref().and_then(FileMetadata::permissions) {
                    self.shell(&format!("chmod {mode:o} -- {}", quote(temporary.as_str())?))?;
                }
                self.shell(&format!(
                    "mv -f -- {} {}",
                    quote(temporary.as_str())?,
                    quote(target.as_str())?
                ))?;
                Ok(())
            });
        if result.is_err() && connection.verify(&mut || false).is_ok() {
            let _ = connection.shell(
                &format!("rm -f -- {}", quote(temporary.as_str())?),
                &mut || false,
            );
        }
        result
    }

    fn download(&mut self, request: &DownloadRequest) -> Result<()> {
        let destination = request.destination().canonicalize()?;
        if !destination.is_dir() {
            bail!(t!("adb.invalid_path"));
        }
        for source in request.sources() {
            let name = request.target_name().unwrap_or(source.file_name());
            self.download_item(source, &destination.join(local_name(name)?), 0)?;
        }
        Ok(())
    }

    fn download_item(&mut self, source: &RemotePath, target: &Path, depth: usize) -> Result<()> {
        self.check()?;
        self.visited += 1;
        if depth > 64 || self.visited > 10000 {
            bail!(t!("adb.limit"));
        }
        let metadata = self
            .metadata(source)?
            .ok_or_else(|| anyhow!(t!("adb.invalid_path")))?;
        let existing = match std::fs::symlink_metadata(target) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if existing.as_ref().is_some_and(is_local_link) {
            bail!(t!("adb.link"));
        }
        if metadata.kind() == EntryKind::Directory {
            if existing.as_ref().is_some_and(|metadata| !metadata.is_dir()) {
                bail!(t!("adb.type_conflict"));
            }
            if existing.is_none() {
                std::fs::create_dir(target)?;
            }
            let listing = self.list(source)?;
            for entry in listing.entries() {
                self.download_item(
                    &source.join(entry.name())?,
                    &target.join(local_name(entry.name())?),
                    depth + 1,
                )?;
            }
            return Ok(());
        }
        if metadata.kind() != EntryKind::File
            || existing
                .as_ref()
                .is_some_and(|metadata| !metadata.is_file())
        {
            bail!(t!("adb.type_conflict"));
        }
        self.items += 1;
        self.total = self.total.saturating_add(metadata.size());
        if existing.is_some() && !self.overwrite(&target.to_string_lossy())? {
            self.details
                .push(TransferDetail::skipped(target.to_string_lossy()));
            return Ok(());
        }
        self.progress(
            TransferPhase::Transferring,
            source.as_str(),
            &target.to_string_lossy(),
        );
        let temporary = tempfile::NamedTempFile::new_in(
            target
                .parent()
                .ok_or_else(|| anyhow!(t!("adb.invalid_path")))?,
        )?;
        self.pull_file(source, temporary.path(), None)?;
        let current = std::fs::symlink_metadata(target).ok();
        if current.as_ref().map(FileStamp::local) != existing.as_ref().map(FileStamp::local) {
            bail!(t!("adb.file_changed"));
        }
        if existing.is_some() {
            temporary.persist(target).map_err(|error| error.error)?;
        } else {
            temporary
                .persist_noclobber(target)
                .map_err(|error| error.error)?;
        }
        self.bytes = self.bytes.saturating_add(metadata.size());
        self.details
            .push(TransferDetail::done(target.to_string_lossy()));
        self.progress(
            TransferPhase::Transferring,
            source.as_str(),
            &target.to_string_lossy(),
        );
        Ok(())
    }

    fn pull_file(&mut self, source: &RemotePath, target: &Path, limit: Option<u64>) -> Result<()> {
        self.verify()?;
        let expected = self.metadata(source)?.as_ref().map(FileStamp::of);
        self.connection.clone().run(
            &[
                OsStr::new("pull"),
                OsStr::new(source.as_str()),
                target.as_os_str(),
            ],
            Duration::from_secs(1800),
            &mut || {
                self.control()
                    || limit.is_some_and(|limit| {
                        std::fs::metadata(target).is_ok_and(|metadata| metadata.len() > limit)
                    })
            },
        )?;
        self.verify()?;
        if self.metadata(source)?.as_ref().map(FileStamp::of) != expected {
            bail!(t!("adb.file_changed"));
        }
        Ok(())
    }

    fn read_file(
        &mut self,
        path: &RemotePath,
        limit: u64,
    ) -> Result<(Vec<u8>, FileStamp), ReadFailure> {
        let read = |this: &mut Self| -> Result<_, ReadFailure> {
            let metadata = this
                .metadata(path)
                .map_err(|error| ReadFailure::Failed(error.to_string()))?
                .ok_or(ReadFailure::NotFile)?;
            if metadata.kind() != EntryKind::File {
                return Err(ReadFailure::NotFile);
            }
            if metadata.size() > limit {
                return Err(ReadFailure::TooLarge(metadata.size()));
            }
            let temporary = tempfile::NamedTempFile::new()
                .map_err(|error| ReadFailure::Failed(error.to_string()))?;
            this.pull_file(path, temporary.path(), Some(limit))
                .map_err(|error| match std::fs::metadata(temporary.path()) {
                    Ok(metadata) if metadata.len() > limit => ReadFailure::TooLarge(metadata.len()),
                    _ => ReadFailure::Failed(error.to_string()),
                })?;
            let actual = std::fs::metadata(temporary.path())
                .map_err(|error| ReadFailure::Failed(error.to_string()))?
                .len();
            if actual > limit {
                return Err(ReadFailure::TooLarge(actual));
            }
            let bytes = std::fs::read(temporary.path())
                .map_err(|error| ReadFailure::Failed(error.to_string()))?;
            if bytes.len() as u64 > limit {
                return Err(ReadFailure::TooLarge(bytes.len() as u64));
            }
            Ok((bytes, FileStamp::of(&metadata)))
        };
        read(self)
    }

    fn write_file(
        &mut self,
        path: &RemotePath,
        bytes: Vec<u8>,
        expected: Option<FileStamp>,
    ) -> Result<FileStamp, SaveFailure> {
        let write = |this: &mut Self| -> Result<FileStamp, SaveFailure> {
            let current = this
                .metadata(path)
                .map_err(|error| SaveFailure::Failed(error.to_string()))?;
            if expected.is_some() && current.as_ref().map(FileStamp::of) != expected {
                return Err(SaveFailure::Changed);
            }
            let temporary = tempfile::NamedTempFile::new()
                .map_err(|error| SaveFailure::Failed(error.to_string()))?;
            std::fs::write(temporary.path(), bytes)
                .map_err(|error| SaveFailure::Failed(error.to_string()))?;
            this.push_file(temporary.path(), path, current.as_ref().map(FileStamp::of))
                .map_err(|error| SaveFailure::Failed(error.to_string()))?;
            this.metadata(path)
                .map_err(|error| SaveFailure::Failed(error.to_string()))?
                .as_ref()
                .map(FileStamp::of)
                .ok_or_else(|| SaveFailure::Failed(t!("adb.invalid_response").to_string()))
        };
        write(self)
    }

    fn operate(&mut self, operation: RemoteOperation) -> Result<()> {
        self.verify()?;
        let script = match operation {
            RemoteOperation::Delete { paths } => {
                let mut scripts = Vec::new();
                for path in paths {
                    validate_remote_mutation(&path)?;
                    scripts.push(format!("rm -rf -- {}", quote(path.as_str())?));
                }
                scripts.join(" && ")
            }
            RemoteOperation::Rename { from, to } => {
                validate_remote_mutation(&from)?;
                validate_remote_mutation(&to)?;
                let (from, to) = (quote(from.as_str())?, quote(to.as_str())?);
                format!(
                    "[ ! -e {to} ] && [ ! -L {to} ] && mv -n -- {from} {to} && [ ! -e {from} ] && [ ! -L {from} ]"
                )
            }
            RemoteOperation::CreateDirectory { path } => {
                validate_remote_mutation(&path)?;
                format!("mkdir -- {}", quote(path.as_str())?)
            }
            RemoteOperation::CreateFile { path } => {
                validate_remote_mutation(&path)?;
                format!("set -C; : > {}", quote(path.as_str())?)
            }
            RemoteOperation::SetPermissions {
                paths,
                edit,
                recursive,
                add_x_to_dirs,
            } => {
                if recursive {
                    bail!(t!("adb.unsupported"));
                }
                let mut scripts = Vec::new();
                for path in paths {
                    validate_remote_mutation(&path)?;
                    let metadata = self
                        .metadata(&path)?
                        .ok_or_else(|| anyhow!(t!("adb.invalid_path")))?;
                    if metadata.kind() == EntryKind::Symlink {
                        continue;
                    }
                    let mode = edit.apply(
                        metadata.permissions().unwrap_or(0),
                        metadata.kind() == EntryKind::Directory,
                        add_x_to_dirs,
                    );
                    scripts.push(format!("chmod {mode:o} -- {}", quote(path.as_str())?));
                }
                scripts.join(" && ")
            }
        };
        if !script.is_empty() {
            self.shell(&script)?;
        }
        Ok(())
    }
}

fn validate_remote_mutation(path: &RemotePath) -> Result<()> {
    if !path.as_str().starts_with('/')
        || path.file_name().is_empty()
        || path
            .as_str()
            .split('/')
            .any(|part| matches!(part, "." | ".."))
    {
        bail!(t!("adb.invalid_path"));
    }
    Ok(())
}

fn local_name(name: &str) -> Result<&str> {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.contains(['/', '\\', ':', '\0'])
        || name.ends_with(['.', ' '])
        || matches!(
            stem.as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        )
    {
        bail!(t!("adb.invalid_path"));
    }
    Ok(name)
}

fn is_local_link(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    metadata.is_symlink()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{adb::test_support::FakeAdb, sftp::TransferAnswer};
    use std::{
        sync::{Arc, atomic::Ordering},
        thread::JoinHandle,
    };

    fn event(events: &Receiver<SftpEvent>, predicate: impl Fn(&SftpEvent) -> bool) -> SftpEvent {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            assert!(Instant::now() < deadline, "ADB event timed out");
            if let Ok(event) = events.try_recv() {
                if predicate(&event) {
                    return event;
                }
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    fn start(
        fake: &Arc<FakeAdb>,
    ) -> (
        Sender<SftpCommand>,
        Receiver<SftpEvent>,
        JoinHandle<Result<()>>,
    ) {
        let connection = fake.connection();
        start_connection(connection)
    }

    fn start_connection(
        connection: AdbConnection,
    ) -> (
        Sender<SftpCommand>,
        Receiver<SftpEvent>,
        JoinHandle<Result<()>>,
    ) {
        let (commands, receiver) = async_channel::unbounded();
        let (sender, events) = async_channel::unbounded();
        let thread =
            std::thread::spawn(move || Box::new(AdbTransport(connection)).run(receiver, sender));
        event(&events, |event| {
            matches!(event, SftpEvent::Connected { .. })
        });
        (commands, events, thread)
    }

    fn confirm(commands: &Sender<SftpCommand>, events: &Receiver<SftpEvent>) {
        let SftpEvent::Question(question) =
            event(events, |event| matches!(event, SftpEvent::Question(_)))
        else {
            unreachable!()
        };
        commands
            .send_blocking(SftpCommand::Answer {
                request_id: question.id(),
                answer: TransferAnswer::new(TransferChoice::Overwrite, false),
            })
            .unwrap();
    }

    fn completed(events: &Receiver<SftpEvent>) -> TransferProgress {
        let SftpEvent::Progress(progress) = event(
            events,
            |event| matches!(event, SftpEvent::Progress(progress) if progress.phase() == TransferPhase::Completed),
        ) else {
            unreachable!()
        };
        progress
    }

    #[test]
    fn adb_upload_edit_download_and_browse_use_the_existing_file_protocol() {
        let fake = Arc::new(FakeAdb::default());
        fake.file("/file.txt", b"old");
        let source = tempfile::tempdir().unwrap();
        let path = source.path().join("file.txt");
        std::fs::write(&path, b"uploaded\n").unwrap();
        let (commands, events, thread) = start(&fake);
        commands
            .send_blocking(SftpCommand::Upload(
                UploadRequest::new(vec![path], RemotePath::new("/").unwrap()).unwrap(),
            ))
            .unwrap();
        confirm(&commands, &events);
        assert_eq!(completed(&events).failed(), 0);
        assert_eq!(
            fake.entries.lock().unwrap()["/file.txt"].bytes,
            b"uploaded\n"
        );
        commands
            .send_blocking(SftpCommand::ReadFile {
                request_id: 7,
                path: RemotePath::new("/file.txt").unwrap(),
            })
            .unwrap();
        let SftpEvent::FileRead {
            result: Ok(file), ..
        } = event(&events, |event| matches!(event, SftpEvent::FileRead { .. }))
        else {
            panic!("file must open")
        };
        assert_eq!(file.text(), "uploaded\n");
        commands
            .send_blocking(SftpCommand::WriteFile {
                request_id: 8,
                path: RemotePath::new("/file.txt").unwrap(),
                bytes: b"edited\n".to_vec(),
                expected: Some(file.stamp()),
            })
            .unwrap();
        let written = event(&events, |event| {
            matches!(event, SftpEvent::FileWritten { .. })
        });
        assert!(
            matches!(written, SftpEvent::FileWritten { result: Ok(_), .. }),
            "{written:?}"
        );
        let target = tempfile::tempdir().unwrap();
        std::fs::write(target.path().join("file.txt"), b"local original").unwrap();
        commands
            .send_blocking(SftpCommand::Download(
                DownloadRequest::new(
                    vec![RemotePath::new("/file.txt").unwrap()],
                    target.path().to_owned(),
                )
                .unwrap(),
            ))
            .unwrap();
        confirm(&commands, &events);
        assert_eq!(completed(&events).failed(), 0);
        assert_eq!(
            std::fs::read(target.path().join("file.txt")).unwrap(),
            b"edited\n"
        );
        commands
            .send_blocking(SftpCommand::List {
                request_id: 9,
                path: RemotePath::new("/").unwrap(),
            })
            .unwrap();
        let SftpEvent::Listed {
            result: Ok(listing),
            ..
        } = event(&events, |event| matches!(event, SftpEvent::Listed { .. }))
        else {
            panic!("directory must list")
        };
        assert_eq!(listing.entries()[0].name(), "file.txt");
        commands.send_blocking(SftpCommand::Shutdown).unwrap();
        thread.join().unwrap().unwrap();
    }

    #[test]
    fn pty_only_boards_can_browse_upload_and_download_binary_files() {
        let fake = Arc::new(crate::adb::test_support::PtyOnlyAdb::default());
        let local = tempfile::tempdir().unwrap();
        let source = local.path().join("bytes.bin");
        let bytes = b"\0\xff\r\n\x1b\0\n";
        std::fs::write(&source, bytes).unwrap();
        let (commands, events, worker) = start_connection(fake.connection());
        commands
            .send_blocking(SftpCommand::Upload(
                UploadRequest::new(vec![source], RemotePath::new("/").unwrap()).unwrap(),
            ))
            .unwrap();
        assert_eq!(completed(&events).failed(), 0);
        commands
            .send_blocking(SftpCommand::List {
                request_id: 1,
                path: RemotePath::new("/").unwrap(),
            })
            .unwrap();
        let listing = event(&events, |event| matches!(event, SftpEvent::Listed { .. }));
        assert!(
            matches!(listing, SftpEvent::Listed { result: Ok(ref listing), .. } if listing.entries()[0].name() == "bytes.bin")
        );
        let target = tempfile::tempdir().unwrap();
        commands
            .send_blocking(SftpCommand::Download(
                DownloadRequest::new(
                    vec![RemotePath::new("/bytes.bin").unwrap()],
                    target.path().to_owned(),
                )
                .unwrap(),
            ))
            .unwrap();
        assert_eq!(completed(&events).failed(), 0);
        assert_eq!(
            std::fs::read(target.path().join("bytes.bin")).unwrap(),
            bytes
        );
        commands.send_blocking(SftpCommand::Shutdown).unwrap();
        worker.join().unwrap().unwrap();
        assert_eq!(fake.rejected.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cancelling_adb_upload_keeps_the_original_and_cleans_its_staging_file() {
        let fake = Arc::new(FakeAdb::default());
        fake.file("/file.txt", b"original");
        fake.hold_push.store(true, Ordering::SeqCst);
        let source = tempfile::tempdir().unwrap();
        let path = source.path().join("file.txt");
        std::fs::write(&path, b"new").unwrap();
        let (commands, events, thread) = start(&fake);
        commands
            .send_blocking(SftpCommand::Upload(
                UploadRequest::new(vec![path], RemotePath::new("/").unwrap()).unwrap(),
            ))
            .unwrap();
        confirm(&commands, &events);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|args| args.get(2).is_some_and(|arg| arg == "push"))
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        commands.send_blocking(SftpCommand::Cancel).unwrap();
        event(
            &events,
            |event| matches!(event, SftpEvent::Progress(progress) if progress.phase() == TransferPhase::Stopped),
        );
        assert_eq!(fake.entries.lock().unwrap()["/file.txt"].bytes, b"original");
        assert!(
            !fake
                .entries
                .lock()
                .unwrap()
                .keys()
                .any(|path| path.contains(".filepart"))
        );
        fake.hold_push.store(false, Ordering::SeqCst);
        commands.send_blocking(SftpCommand::Resume).unwrap();
        confirm(&commands, &events);
        assert_eq!(completed(&events).failed(), 0);
        assert_eq!(fake.entries.lock().unwrap()["/file.txt"].bytes, b"new");
        commands.send_blocking(SftpCommand::Shutdown).unwrap();
        thread.join().unwrap().unwrap();
    }

    #[test]
    fn adb_does_not_replace_a_file_changed_during_upload() {
        let fake = Arc::new(FakeAdb::default());
        fake.file("/file.txt", b"original");
        fake.change_during_push.store(true, Ordering::SeqCst);
        let source = tempfile::tempdir().unwrap();
        let path = source.path().join("file.txt");
        std::fs::write(&path, b"new").unwrap();
        let (commands, events, thread) = start(&fake);
        commands
            .send_blocking(SftpCommand::Upload(
                UploadRequest::new(vec![path], RemotePath::new("/").unwrap()).unwrap(),
            ))
            .unwrap();
        confirm(&commands, &events);
        assert_eq!(completed(&events).failed(), 1);
        assert_eq!(
            fake.entries.lock().unwrap()["/file.txt"].bytes,
            b"changed elsewhere"
        );
        commands.send_blocking(SftpCommand::Shutdown).unwrap();
        thread.join().unwrap().unwrap();
    }

    #[test]
    fn unsafe_remote_mutations_and_local_path_components_are_rejected() {
        for name in [
            "..",
            "../escape",
            "a\\b",
            "C:secret",
            "NUL",
            "trailing.",
            "CON.txt",
        ] {
            assert!(local_name(name).is_err(), "{name}");
        }
        for path in ["/", "/tmp/../", "/a/./b", "relative"] {
            assert!(validate_remote_mutation(&RemotePath::new(path).unwrap()).is_err());
        }
        assert_eq!(local_name("设备 file.txt").unwrap(), "设备 file.txt");
    }
}
