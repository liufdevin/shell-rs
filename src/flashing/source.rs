use std::{
    fs,
    path::Path,
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
    Missing,
    Unavailable(String),
}

pub(super) fn observe(path: &Path) -> Observation {
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

pub(super) fn probe(path: std::path::PathBuf) -> Receiver<Observation> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(observe(&path));
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
    pub(super) fn observation(&self) -> Option<&Observation> {
        self.observation.as_ref()
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
        let result = probe(path.clone())
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
}
