use std::{
    fs,
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant, SystemTime},
};

pub(super) const CHECK_INTERVAL: Duration = Duration::from_secs(2);
pub(super) const STABLE_INTERVAL: Duration = Duration::from_secs(4);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FileStamp {
    size: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Observation {
    Present(FileStamp),
    Packing(Option<String>),
    Missing,
    Unavailable(String),
}

pub(super) fn observe(path: &Path) -> Observation {
    match fs::symlink_metadata(sidecar(path, ".packing")) {
        Ok(_) => return Observation::Packing(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Observation::Unavailable(error.to_string()),
    }
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Observation::Present(FileStamp {
            size: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
        }),
        Ok(_) => Observation::Missing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Observation::Missing,
        Err(error) => Observation::Unavailable(error.to_string()),
    }
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn pack_token(path: &Path) -> io::Result<String> {
    let mut token = String::new();
    fs::File::open(path)?.take(65).read_to_string(&mut token)?;
    let token = token.trim();
    uuid::Uuid::parse_str(token)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(token.to_owned())
}

fn acknowledge_pack(path: &Path) -> Result<(), String> {
    let _lease = super::process::ConsoleLease::acquire()?;
    write_pack_ack(path)
}

fn write_pack_ack(path: &Path) -> Result<(), String> {
    let token = pack_token(&sidecar(path, ".packing")).map_err(|error| error.to_string())?;
    let acknowledgement = sidecar(path, ".packing.ack");
    if pack_token(&acknowledgement).is_ok_and(|previous| previous == token) {
        return Ok(());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    file.write_all(token.as_bytes())
        .map_err(|error| error.to_string())?;
    file.persist(acknowledgement)
        .map_err(|error| error.error.to_string())?;
    Ok(())
}

pub(super) fn check_revision(path: &Path, expected: &FileStamp) -> Result<(), Observation> {
    match observe(path) {
        Observation::Present(stamp) if &stamp == expected => Ok(()),
        observation => Err(observation),
    }
}

pub(super) fn probe(path: PathBuf, may_acknowledge: bool) -> Receiver<Observation> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let observation = match observe(&path) {
            Observation::Packing(_) if may_acknowledge => {
                Observation::Packing(acknowledge_pack(&path).err())
            }
            observation => observation,
        };
        let _ = sender.send(observation);
    });
    receiver
}

#[derive(Default)]
pub(super) struct SourceState {
    observation: Option<Observation>,
    since: Option<Instant>,
    ready: bool,
}

impl SourceState {
    pub(super) fn update(&mut self, observation: Observation, at: Instant) -> bool {
        if self.observation.as_ref() != Some(&observation) {
            self.ready = self.observation.is_none()
                && matches!(&observation, Observation::Present(stamp) if stamp.size > 0);
            self.observation = Some(observation);
            self.since = Some(at);
            return true;
        }
        if !self.ready
            && self
                .since
                .is_some_and(|since| at.saturating_duration_since(since) >= STABLE_INTERVAL)
            && matches!(&self.observation, Some(Observation::Present(stamp)) if stamp.size > 0)
        {
            self.ready = true;
            return true;
        }
        false
    }

    pub(super) fn ready(&self) -> bool {
        self.ready
    }

    pub(super) fn revision(&self) -> Option<&FileStamp> {
        match &self.observation {
            Some(Observation::Present(stamp)) if self.ready => Some(stamp),
            _ => None,
        }
    }
    pub(super) fn observation(&self) -> Option<&Observation> {
        self.observation.as_ref()
    }

    pub(super) fn created_at(&self) -> Option<SystemTime> {
        match self.observation() {
            Some(Observation::Present(stamp)) => stamp.created,
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(size: u64) -> Observation {
        Observation::Present(FileStamp {
            size,
            modified: None,
            created: None,
        })
    }

    #[test]
    fn a_missing_image_returns_only_after_it_stops_changing() {
        let mut source = SourceState::default();
        let at = Instant::now();
        assert!(source.update(present(10), at));
        assert!(source.ready());
        assert!(source.update(Observation::Missing, at));
        assert!(!source.update(Observation::Missing, at + Duration::from_secs(300)));
        assert!(!source.ready());
        assert!(source.update(present(10), at + Duration::from_secs(301)));
        assert!(!source.ready());
        assert!(!source.update(present(10), at + Duration::from_secs(303)));
        assert!(source.update(present(20), at + Duration::from_secs(304)));
        assert!(!source.update(present(20), at + Duration::from_secs(306)));
        assert!(source.update(present(20), at + Duration::from_secs(308)));
        assert!(source.ready());
        assert!(!source.update(present(20), at + Duration::from_secs(310)));
    }

    #[test]
    fn observing_delete_and_recreate_reads_only_the_original_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("board.img");
        fs::write(&path, b"old firmware").unwrap();
        assert!(matches!(observe(&path), Observation::Present(_)));
        fs::remove_file(&path).unwrap();
        assert_eq!(observe(&path), Observation::Missing);
        fs::write(&path, b"new firmware").unwrap();
        let result = probe(path.clone(), false)
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        assert!(matches!(result, Observation::Present(_)));
        assert_eq!(fs::read(&path).unwrap(), b"new firmware");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn empty_and_unavailable_files_do_not_become_ready() {
        let mut source = SourceState::default();
        let at = Instant::now();
        source.update(present(0), at);
        source.update(present(0), at + Duration::from_secs(60));
        assert!(!source.ready());
        source.update(Observation::Unavailable("offline".into()), at);
        assert!(!source.update(
            Observation::Unavailable("offline".into()),
            at + Duration::from_secs(120)
        ));
        assert!(!source.ready());
    }

    #[test]
    fn a_pack_request_blocks_the_old_revision_and_restarts_the_stability_period() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("board.img");
        let marker = sidecar(&path, ".packing");
        let acknowledgement = sidecar(&path, ".packing.ack");
        let mut source = SourceState::default();
        let at = Instant::now();
        fs::write(&path, b"old firmware").unwrap();
        source.update(observe(&path), at);
        let old = source.revision().unwrap().clone();

        let token = uuid::Uuid::new_v4().to_string();
        fs::write(&marker, &token).unwrap();
        fs::write(&acknowledgement, uuid::Uuid::new_v4().to_string()).unwrap();
        assert_eq!(check_revision(&path, &old), Err(Observation::Packing(None)));
        let observation = probe(path.clone(), false)
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        assert_eq!(observation, Observation::Packing(None));
        assert_ne!(pack_token(&acknowledgement).unwrap(), token);
        source.update(observation, at);
        assert!(source.revision().is_none());

        write_pack_ack(&path).unwrap();
        assert_eq!(pack_token(&acknowledgement).unwrap(), token);
        fs::write(&path, b"new complete firmware").unwrap();
        assert!(matches!(observe(&path), Observation::Packing(_)));
        fs::remove_file(marker).unwrap();

        let published_at = at + Duration::from_secs(10);
        source.update(observe(&path), published_at);
        assert!(source.revision().is_none());
        source.update(observe(&path), published_at + STABLE_INTERVAL);
        assert!(check_revision(&path, source.revision().unwrap()).is_ok());
        assert!(check_revision(&path, &old).is_err());
    }
}
