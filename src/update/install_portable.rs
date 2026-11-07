//! Windows, portable: a folder holding `shellrs.exe`, its console program
//! and the `data` folder, updated from the release's zip.
//!
//! The zip is unpacked where it was downloaded, inside `data` and so on the
//! volume the copy runs from. Putting the new version in place is renames:
//! Windows lets a running program be renamed, though not deleted or written
//! over, so each file of the old version is moved aside under a hidden name
//! and the new one moved in, and ShellRS restarts from the new program. The
//! version that starts then deletes what was moved aside, the old process
//! being gone by then. Nothing under `data`, or `bin` with the `shelllf-cli`
//! command on the PATH, is ever touched.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use semver::Version;

use super::error::UpdateError;
use super::feed::write_error;
use super::install::{InstallKind, Installer, Relaunch, Staged};
use super::manifest::Release;
use crate::app::{PORTABLE_BIN, PORTABLE_DATA};
use crate::i18n::t;

/// The program a portable copy is started as.
const PROGRAM: &str = "shellrs.exe";

/// Far more than a release unpacks to: a package claiming more is not one.
const UNPACKED_LIMIT: u64 = 1 << 30;

/// How often a rename is tried while something, a virus scanner looking at
/// the new program say, still holds the file.
const RENAME_TRIES: u32 = 3;
const RENAME_PAUSE: Duration = Duration::from_millis(100);

pub struct PortableInstaller {
    kind: InstallKind,
    root: PathBuf,
}

impl PortableInstaller {
    pub fn new(root: PathBuf) -> Self {
        Self {
            kind: InstallKind::WindowsPortable { root: root.clone() },
            root,
        }
    }
}

impl Installer for PortableInstaller {
    fn kind(&self) -> &InstallKind {
        &self.kind
    }

    /// Unpack the verified zip beside it. The updater removes the folder
    /// with the download once that version is the one running.
    fn stage(&self, package: &Path, release: &Release) -> Result<Staged, UpdateError> {
        let unpacked = package.with_file_name("unpacked");
        if unpacked.exists() {
            fs::remove_dir_all(&unpacked)?;
        }
        fs::create_dir_all(&unpacked)?;
        match unpack(package, &unpacked) {
            Ok(()) => Ok(Staged {
                version: release.version.clone(),
                path: unpacked,
            }),
            Err(error) => {
                let _ = fs::remove_dir_all(&unpacked);
                Err(error)
            }
        }
    }

    fn apply(&self, staged: &Staged, relaunch: bool) -> Result<Relaunch, UpdateError> {
        swap(&self.root, &staged.path, &rename)?;
        let _ = fs::remove_dir(&staged.path);
        Ok(if relaunch {
            Relaunch::Restart(self.root.join(PROGRAM))
        } else {
            Relaunch::Nothing
        })
    }

    /// Delete what the last update moved aside.
    fn clean_up(&self, _: &Version) {
        for entry in fs::read_dir(&self.root).into_iter().flatten().flatten() {
            if !is_set_aside(&entry.file_name().to_string_lossy()) {
                continue;
            }
            let path = entry.path();
            let _ = match entry.file_type() {
                Ok(kind) if kind.is_dir() => fs::remove_dir_all(&path),
                _ => fs::remove_file(&path),
            };
        }
    }
}

/// Whether a part of the package is left out: the person's data, the
/// command on the PATH, and hidden names, which are for moving things aside.
fn left_out(name: &str) -> bool {
    name.starts_with('.')
        || name.eq_ignore_ascii_case(PORTABLE_DATA)
        || name.eq_ignore_ascii_case(PORTABLE_BIN)
}

/// `.shellrs.exe.old-<id>`: a file of the previous version.
fn set_aside_name(name: &str) -> String {
    format!(".{name}.old-{}", uuid::Uuid::new_v4().simple())
}

fn is_set_aside(name: &str) -> bool {
    name.starts_with('.') && name.contains(".old-")
}

fn unpack(package: &Path, into: &Path) -> Result<(), UpdateError> {
    let bad = |error: &dyn std::fmt::Display| {
        UpdateError::Install(t!("update.install.bad_package", error = error).to_string())
    };
    let mut archive = zip::ZipArchive::new(fs::File::open(package)?).map_err(|e| bad(&e))?;
    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|e| bad(&e))?;
        let raw = String::from_utf8_lossy(entry.name_raw()).into_owned();
        let name = entry
            .enclosed_name()
            .filter(|name| is_plain(name))
            .filter(|_| !entry.is_symlink())
            .ok_or_else(|| bad(&raw))?;
        let first = name
            .components()
            .next()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();
        if left_out(&first) {
            continue;
        }
        total = total.saturating_add(entry.size());
        if total > UNPACKED_LIMIT {
            return Err(bad(&t!("update.feed.too_large")));
        }
        let path = into.join(&name);
        if entry.is_dir() {
            fs::create_dir_all(&path)?;
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = fs::File::create(&path).map_err(write_error)?;
        io::copy(&mut entry, &mut file).map_err(|error| match error.kind() {
            io::ErrorKind::InvalidData => bad(&error),
            _ => write_error(error),
        })?;
    }
    if into.join(PROGRAM).is_file() {
        Ok(())
    } else {
        Err(UpdateError::Install(
            t!("update.install.no_app").to_string(),
        ))
    }
}

/// Only plain names: no drive or root, and no `:`, which on Windows names
/// a stream of another file.
fn is_plain(name: &Path) -> bool {
    name.components().all(|part| match part {
        Component::Normal(part) => !part.to_string_lossy().contains(':'),
        _ => false,
    })
}

/// Move every top-level entry of `staged` into `root`, setting aside what
/// is there. When a step fails, the steps before it are undone in reverse,
/// so the old version stays whole.
fn swap(
    root: &Path,
    staged: &Path,
    rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> Result<(), UpdateError> {
    if !staged.join(PROGRAM).is_file() {
        return Err(UpdateError::Install(
            t!("update.install.no_app").to_string(),
        ));
    }
    let mut names: Vec<OsString> = fs::read_dir(staged)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<io::Result<_>>()?;
    names.sort();
    let mut done: Vec<(OsString, Option<PathBuf>)> = Vec::new();
    for name in names {
        let target = root.join(&name);
        let aside = match fs::symlink_metadata(&target) {
            Ok(_) => {
                let aside = root.join(set_aside_name(&name.to_string_lossy()));
                if let Err(error) = rename(&target, &aside) {
                    undo(root, staged, &done, rename);
                    return Err(UpdateError::Install(error.to_string()));
                }
                Some(aside)
            }
            Err(_) => None,
        };
        if let Err(error) = rename(&staged.join(&name), &target) {
            if let Some(aside) = &aside {
                let _ = rename(aside, &target);
            }
            undo(root, staged, &done, rename);
            return Err(UpdateError::Install(error.to_string()));
        }
        done.push((name, aside));
    }
    Ok(())
}

fn undo(
    root: &Path,
    staged: &Path,
    done: &[(OsString, Option<PathBuf>)],
    rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
) {
    for (name, aside) in done.iter().rev() {
        let target = root.join(name);
        let _ = rename(&target, &staged.join(name));
        if let Some(aside) = aside {
            let _ = rename(aside, &target);
        }
    }
}

/// `fs::rename`, tried again for a moment while the file is held.
fn rename(from: &Path, to: &Path) -> io::Result<()> {
    let mut tries = 1;
    loop {
        match fs::rename(from, to) {
            Err(error) if tries < RENAME_TRIES && is_held(&error) => {
                tries += 1;
                std::thread::sleep(RENAME_PAUSE);
            }
            result => return result,
        }
    }
}

/// Access denied, or Windows' sharing violation.
fn is_held(error: &io::Error) -> bool {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    error.kind() == io::ErrorKind::PermissionDenied
        || (cfg!(windows) && error.raw_os_error() == Some(ERROR_SHARING_VIOLATION))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use semver::Version;
    use zip::write::SimpleFileOptions;

    use super::*;
    use crate::update::manifest::Asset;

    fn release() -> Release {
        Release {
            version: Version::new(0, 2, 0),
            published_at: None,
            notes: String::new(),
            notes_url: None,
            asset: Asset {
                urls: Vec::new(),
                size: 0,
                sha256: String::new(),
            },
            installer: None,
        }
    }

    /// A zip of `entries`: a file with its text, or a folder without.
    fn package(path: &Path, entries: &[(&str, Option<&str>)]) {
        let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, text) in entries {
            match text {
                Some(text) => {
                    zip.start_file(*name, options).unwrap();
                    zip.write_all(text.as_bytes()).unwrap();
                }
                None => zip.add_directory(*name, options).unwrap(),
            }
        }
        zip.finish().unwrap();
    }

    /// A portable folder with the old version, some data and the command
    /// on the PATH, and where its download of 0.2.0 goes.
    fn portable() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("shellrs.exe"), "old").unwrap();
        fs::write(root.path().join("shelllf-cli.exe"), "old cli").unwrap();
        fs::create_dir_all(root.path().join("data/updates/0.2.0")).unwrap();
        fs::write(root.path().join("data/shellrs.db"), "hosts").unwrap();
        fs::create_dir(root.path().join("bin")).unwrap();
        fs::write(root.path().join("bin/shellrs.exe"), "command").unwrap();
        let download = root
            .path()
            .join("data/updates/0.2.0/ShellRS-0.2.0-windows-x86_64-portable.zip");
        (root, download)
    }

    fn read(path: impl AsRef<Path>) -> String {
        fs::read_to_string(path).unwrap()
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_staged_package_replaces_the_program_and_leaves_data_and_bin_alone() {
        let (root, download) = portable();
        package(
            &download,
            &[
                ("shellrs.exe", Some("new")),
                ("shelllf-cli.exe", Some("new cli")),
                ("data/", None),
                ("Data/shellrs.db", Some("empty")),
                ("bin/shellrs.exe", Some("other")),
                (".hidden", Some("no")),
            ],
        );
        let installer = PortableInstaller::new(root.path().to_path_buf());
        let staged = installer.stage(&download, &release()).unwrap();
        assert_eq!(names(&staged.path), ["shelllf-cli.exe", "shellrs.exe"]);
        assert_eq!(read(root.path().join("shellrs.exe")), "old");

        assert_eq!(
            installer.apply(&staged, true),
            Ok(Relaunch::Restart(root.path().join("shellrs.exe")))
        );
        assert_eq!(read(root.path().join("shellrs.exe")), "new");
        assert_eq!(read(root.path().join("shelllf-cli.exe")), "new cli");
        assert_eq!(read(root.path().join("data/shellrs.db")), "hosts");
        assert_eq!(read(root.path().join("bin/shellrs.exe")), "command");
        assert!(!staged.path.exists());
        let aside: Vec<_> = names(root.path())
            .into_iter()
            .filter(|name| is_set_aside(name))
            .collect();
        assert_eq!(aside.len(), 2, "{aside:?}");
        assert!(
            aside
                .iter()
                .any(|name| name.starts_with(".shellrs.exe.old-"))
        );

        installer.clean_up(&Version::new(0, 2, 0));
        assert_eq!(
            names(root.path()),
            ["bin", "data", "shelllf-cli.exe", "shellrs.exe"]
        );
    }

    #[test]
    fn installing_on_quit_does_not_restart() {
        let (root, download) = portable();
        package(&download, &[("shellrs.exe", Some("new"))]);
        let installer = PortableInstaller::new(root.path().to_path_buf());
        let staged = installer.stage(&download, &release()).unwrap();
        assert_eq!(installer.apply(&staged, false), Ok(Relaunch::Nothing));
        assert_eq!(read(root.path().join("shellrs.exe")), "new");
        // A file only the old version had stays.
        assert_eq!(read(root.path().join("shelllf-cli.exe")), "old cli");
    }

    #[test]
    fn a_package_without_the_program_is_refused() {
        let (root, download) = portable();
        package(&download, &[("shelllf-cli.exe", Some("new cli"))]);
        let installer = PortableInstaller::new(root.path().to_path_buf());
        assert!(matches!(
            installer.stage(&download, &release()),
            Err(UpdateError::Install(_))
        ));
        assert!(!download.with_file_name("unpacked").exists());
        fs::write(download.clone(), "not a zip").unwrap();
        assert!(matches!(
            installer.stage(&download, &release()),
            Err(UpdateError::Install(_))
        ));
    }

    #[test]
    fn unsafe_names_are_refused() {
        for name in [
            "../shellrs.exe",
            "tools/../../shellrs.exe",
            "shellrs.exe:stream",
        ] {
            let (root, download) = portable();
            package(
                &download,
                &[("shellrs.exe", Some("new")), (name, Some("x"))],
            );
            let installer = PortableInstaller::new(root.path().to_path_buf());
            assert!(
                matches!(
                    installer.stage(&download, &release()),
                    Err(UpdateError::Install(_))
                ),
                "{name}"
            );
            assert!(!root.path().parent().unwrap().join("shellrs.exe").exists());
        }
    }

    #[test]
    fn a_failed_swap_puts_everything_back() {
        let (root, download) = portable();
        package(
            &download,
            &[
                ("shelllf-cli.exe", Some("new cli")),
                ("shellrs.exe", Some("new")),
                ("zzz.dll", Some("new dll")),
            ],
        );
        let installer = PortableInstaller::new(root.path().to_path_buf());
        let staged = installer.stage(&download, &release()).unwrap();
        // The last step fails, after the program itself was swapped.
        let failing = |from: &Path, to: &Path| {
            if to.ends_with("zzz.dll") {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                fs::rename(from, to)
            }
        };
        assert!(matches!(
            swap(root.path(), &staged.path, &failing),
            Err(UpdateError::Install(_))
        ));
        assert_eq!(read(root.path().join("shellrs.exe")), "old");
        assert_eq!(read(root.path().join("shelllf-cli.exe")), "old cli");
        assert_eq!(
            names(root.path()),
            ["bin", "data", "shelllf-cli.exe", "shellrs.exe"]
        );
        // Still staged, for the next try.
        assert_eq!(
            names(&staged.path),
            ["shelllf-cli.exe", "shellrs.exe", "zzz.dll"]
        );
        assert_eq!(read(staged.path.join("shellrs.exe")), "new");
    }

    #[test]
    fn the_clean_up_removes_only_what_an_update_set_aside() {
        let (root, _) = portable();
        fs::write(root.path().join(set_aside_name("shellrs.exe")), "old").unwrap();
        fs::create_dir(root.path().join(set_aside_name("locales"))).unwrap();
        fs::write(root.path().join(".keep"), "mine").unwrap();
        PortableInstaller::new(root.path().to_path_buf()).clean_up(&Version::new(0, 2, 0));
        assert_eq!(
            names(root.path()),
            [".keep", "bin", "data", "shelllf-cli.exe", "shellrs.exe"]
        );
    }

    /// What the whole design rests on: Windows lets a running program be
    /// renamed, though not deleted.
    #[cfg(windows)]
    #[test]
    fn a_running_program_is_moved_aside_and_replaced() {
        let (root, _) = portable();
        let system = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        let program = root.path().join("shellrs.exe");
        // `sort` waits for its input to end, which here it never does.
        fs::copy(Path::new(&system).join(r"System32\sort.exe"), &program).unwrap();
        let mut running = {
            let _forks = crate::testing::no_forks();
            std::process::Command::new(&program)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap()
        };
        assert!(running.try_wait().unwrap().is_none(), "the program runs");
        let staged = root.path().join("data/updates/0.2.0/unpacked");
        fs::create_dir_all(&staged).unwrap();
        fs::write(staged.join("shellrs.exe"), "new").unwrap();
        let installer = PortableInstaller::new(root.path().to_path_buf());
        let applied = installer.apply(
            &Staged {
                version: Version::new(0, 2, 0),
                path: staged,
            },
            true,
        );
        let aside = names(root.path())
            .into_iter()
            .find(|name| name.starts_with(".shellrs.exe.old-"))
            .map(|name| root.path().join(name));
        // Deleting the running program is what Windows refuses.
        let deleted = aside.as_ref().map(fs::remove_file);
        let still_running = running.try_wait().unwrap().is_none();
        let _ = running.kill();
        let _ = running.wait();
        assert!(still_running, "the program ran throughout");
        assert_eq!(applied, Ok(Relaunch::Restart(program.clone())));
        assert_eq!(read(&program), "new");
        assert!(matches!(deleted, Some(Err(_))), "{deleted:?}");

        installer.clean_up(&Version::new(0, 2, 0));
        assert!(!aside.unwrap().exists());
    }
}
