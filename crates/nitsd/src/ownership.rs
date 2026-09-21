//! Process ownership outlives listener shutdown and every background Core user.
//!
//! The lock file is never removed or replaced: every opener must lock the same
//! inode, including when the data directory is reached through a symlink. Its
//! single byte is a diagnostic phase; the OS lock alone proves occupancy.
//! Core users hold shared locks; a probe tries an exclusive lock. Redb remains
//! the authoritative exclusive store owner. Shared acquisition cannot fail just
//! because another read-only probe is briefly inspecting an unowned directory.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const FILE_NAME: &str = "daemon.lock";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[derive(serde::Serialize)]
pub enum Phase {
    Starting = 1,
    Serving = 2,
    Stopping = 3,
    /// The endpoint is occupied but has no live store association.
    Unknown = 4,
}

impl TryFrom<u8> for Phase {
    type Error = io::Error;

    fn try_from(byte: u8) -> Result<Self, Self::Error> {
        match byte {
            1 => Ok(Self::Starting),
            2 => Ok(Self::Serving),
            3 => Ok(Self::Stopping),
            4 => Ok(Self::Unknown),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid daemon ownership phase",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    Free,
    Held { phase: Phase },
}

/// Absence of a guard is different from a known endpoint whose owner released it.
/// Untracked legacy endpoints can only be probed through their own listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketOwnership {
    Untracked,
    Tracked(Ownership),
}

/// Retained alongside Core, and dropped only after Core and its stores.
#[derive(Debug)]
pub(crate) struct Lease {
    file: Mutex<File>,
}

impl Lease {
    pub(crate) fn acquire(data_dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        Self::acquire_file(&data_dir.join(FILE_NAME))
    }

    pub(crate) fn for_socket(socket: &Path) -> io::Result<Self> {
        let path = socket_owner_path(socket)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::acquire_file(&path)
    }

    fn acquire_file(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.lock_shared()?;
        // Only a successfully opened Core may publish a phase. A competing
        // process that redb rejects must not overwrite the incumbent's state.
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    pub(crate) fn set_phase(&self, phase: Phase) -> io::Result<()> {
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&[phase as u8])?;
        file.flush()
    }
}

/// Read-only, nonblocking probe. Old daemons without a lease retain their
/// listener-based lifecycle semantics; the probe never creates a lock file.
pub fn probe(data_dir: &Path) -> io::Result<Ownership> {
    match probe_file(&data_dir.join(FILE_NAME))? {
        Probe::Untracked | Probe::Free => Ok(Ownership::Free),
        Probe::Held(mut file) => {
            let mut byte = [Phase::Starting as u8];
            // Acquisition can precede the first successful Core/phase write.
            let _ = file.read(&mut byte)?;
            Ok(Ownership::Held {
                phase: Phase::try_from(byte[0])?,
            })
        }
    }
}

/// The endpoint's own guard determines occupancy; its published association
/// supplies the phase of the store actually served here, regardless of client
/// configuration. A missing/stale association cannot certify a held guard free.
pub fn probe_socket(socket: &Path) -> io::Result<SocketOwnership> {
    Ok(match probe_file(&socket_owner_path(socket)?)? {
        Probe::Untracked => SocketOwnership::Untracked,
        Probe::Free => SocketOwnership::Tracked(Ownership::Free),
        Probe::Held(_) => {
            let phase = match probe_file(&socket_data_path(socket)?)? {
                Probe::Untracked | Probe::Free => Phase::Unknown,
                Probe::Held(mut file) => {
                    let mut byte = [Phase::Starting as u8];
                    let _ = file.read(&mut byte)?;
                    Phase::try_from(byte[0])?
                }
            };
            SocketOwnership::Tracked(Ownership::Held { phase })
        }
    })
}

/// Publish only after both Core open and Unix bind succeed. This separate,
/// atomically replaced symlink never replaces either stable lock inode. Old
/// owners update their own store's phase, so cannot clobber a newer endpoint's
/// association after it successfully binds a different store.
pub(crate) fn associate_socket(socket: &Path, data_dir: &Path) -> io::Result<()> {
    let target = std::fs::canonicalize(data_dir.join(FILE_NAME))?;
    let association = socket_data_path(socket)?;
    match std::fs::symlink_metadata(&association) {
        Ok(metadata) if !metadata.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "refusing to replace a non-symlink socket association",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut temporary = association.as_os_str().to_os_string();
    temporary.push(format!(".{}.{}", std::process::id(), fastrand::u64(..)));
    let temporary = PathBuf::from(temporary);
    std::os::unix::fs::symlink(target, &temporary)?;
    if let Err(error) = std::fs::rename(&temporary, association) {
        let _ = std::fs::remove_file(temporary);
        return Err(error);
    }
    Ok(())
}

fn socket_data_path(socket: &Path) -> io::Result<PathBuf> {
    let mut name = socket_owner_path(socket)?.into_os_string();
    name.push("-data");
    Ok(PathBuf::from(name))
}

enum Probe {
    Untracked,
    Free,
    Held(File),
}

fn probe_file(path: &Path) -> io::Result<Probe> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Probe::Untracked),
        Err(error) => return Err(error),
    };
    match file.try_lock() {
        Ok(()) => Ok(Probe::Free),
        Err(TryLockError::Error(error)) => Err(error),
        Err(TryLockError::WouldBlock) => Ok(Probe::Held(file)),
    }
}

fn socket_owner_path(socket: &Path) -> io::Result<PathBuf> {
    let mut name = socket_target(socket)?.into_os_string();
    name.push(".owner");
    Ok(PathBuf::from(name))
}

/// The bind path and occupancy path must follow the same socket alias, including
/// after the old listener has removed its target during shutdown.
pub(crate) fn socket_target(socket: &Path) -> io::Result<PathBuf> {
    let mut path = socket.to_path_buf();
    // Follow the socket itself even after shutdown makes its symlink dangling.
    // Parent-directory aliases are resolved by the OS when the guard is opened.
    for _ in 0..40 {
        match std::fs::read_link(&path) {
            Ok(target) => {
                path = if target.is_absolute() {
                    target
                } else {
                    path.parent().unwrap_or(Path::new(".")).join(target)
                };
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::InvalidInput | io::ErrorKind::NotFound
                ) =>
            {
                return Ok(path);
            }
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "too many socket symlink indirections",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Daemon;
    use nits_protocol::BuildInfo;
    use nits_review_core::{Core, DataDir};
    use std::sync::Arc;
    use std::time::Duration;

    fn build() -> BuildInfo {
        BuildInfo {
            name: "ownership-test".into(),
            version: "0".into(),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_read_retains_ownership_until_its_core_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let data = DataDir::new(dir.path());
        let socket = dir.path().join("daemon.sock");
        let daemon = Daemon::open_at_socket(&data, build(), &socket).unwrap();
        let weak = Arc::downgrade(&daemon);
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let reader = Arc::clone(&daemon);
        let reading = tokio::spawn(async move {
            reader
                .read(move |core| {
                    entered.send(()).unwrap();
                    gate.recv().unwrap();
                    core.last_seq()
                })
                .await
        });
        started.await.unwrap();
        daemon.set_phase(Phase::Stopping);
        reading.abort();
        assert!(reading.await.unwrap_err().is_cancelled());
        drop(daemon);
        assert!(weak.upgrade().is_none());
        assert_eq!(
            probe(&data.root).unwrap(),
            Ownership::Held {
                phase: Phase::Stopping
            }
        );
        assert!(Core::open(&data).is_err());
        assert_eq!(
            probe_socket(&socket).unwrap(),
            SocketOwnership::Tracked(Ownership::Held {
                phase: Phase::Unknown
            })
        );
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while probe(&data.root).unwrap() != Ownership::Free
                || probe_socket(&socket).unwrap() != SocketOwnership::Tracked(Ownership::Free)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // Free ownership must imply the stores have already been dropped.
        assert_eq!(
            probe_socket(&socket).unwrap(),
            SocketOwnership::Tracked(Ownership::Free)
        );
        let reopened = Core::open(&data).unwrap();
        drop(reopened);
        assert!(
            data.root.join(FILE_NAME).exists(),
            "never unlink the shared lock inode"
        );
    }

    #[cfg(unix)]
    #[test]
    fn duplicate_open_through_symlink_cannot_publish_over_an_incumbent_phase() {
        let dir = tempfile::tempdir().unwrap();
        let data = DataDir::new(dir.path().join("data"));
        let daemon = Daemon::open(&data, build()).unwrap();
        daemon.set_phase(Phase::Serving);
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&data.root, &alias).unwrap();
        assert!(Daemon::open(&DataDir::new(&alias), build()).is_err());
        assert_eq!(
            probe(&alias).unwrap(),
            Ownership::Held {
                phase: Phase::Serving
            }
        );
        assert_eq!(
            probe(&data.root).unwrap(),
            Ownership::Held {
                phase: Phase::Serving
            }
        );
    }

    #[test]
    fn concurrent_probes_never_reject_shared_occupancy_acquisition_or_change_phase() {
        let dir = tempfile::tempdir().unwrap();
        let first = Lease::acquire(dir.path()).unwrap();
        first.set_phase(Phase::Stopping).unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..200 {
                        assert_eq!(
                            probe(dir.path()).unwrap(),
                            Ownership::Held {
                                phase: Phase::Stopping
                            }
                        );
                        let extra = Lease::acquire(dir.path()).unwrap();
                        assert_eq!(
                            probe(dir.path()).unwrap(),
                            Ownership::Held {
                                phase: Phase::Stopping
                            }
                        );
                        drop(extra);
                    }
                });
            }
        });
        drop(first);
        assert_eq!(probe(dir.path()).unwrap(), Ownership::Free);
        // Stale diagnostic bytes never make an unowned directory look occupied.
        assert_eq!(
            std::fs::read(dir.path().join(FILE_NAME)).unwrap(),
            [Phase::Stopping as u8]
        );
    }
    #[test]
    fn acquiring_a_new_owner_during_free_probes_never_fails_or_looks_free() {
        let dir = tempfile::tempdir().unwrap();
        // Leave an existing inode with a stale phase, as after an ordinary stop.
        let initial = Lease::acquire(dir.path()).unwrap();
        initial.set_phase(Phase::Stopping).unwrap();
        drop(initial);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..500 {
                        probe(dir.path()).unwrap();
                        std::thread::yield_now();
                    }
                });
            }
            for _ in 0..100 {
                let owner = Lease::acquire(dir.path()).unwrap();
                // Before Core opens and publishes Starting, stale diagnostic
                // bytes must still describe occupancy, never a stopped owner.
                assert!(matches!(probe(dir.path()).unwrap(), Ownership::Held { .. }));
                owner.set_phase(Phase::Starting).unwrap();
                assert_eq!(
                    probe(dir.path()).unwrap(),
                    Ownership::Held {
                        phase: Phase::Starting
                    }
                );
                drop(owner);
            }
        });
        assert_eq!(probe(dir.path()).unwrap(), Ownership::Free);
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_writer_retains_the_store_until_the_started_job_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let data = DataDir::new(dir.path());
        let daemon = Daemon::open(&data, build()).unwrap();
        let mut released = daemon.core_released();
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let writer = Arc::clone(&daemon);
        let writing = tokio::spawn(async move {
            writer
                .write(move |core| {
                    entered.send(()).unwrap();
                    gate.recv().unwrap();
                    core.last_seq()
                })
                .await
        });
        started.await.unwrap();
        daemon.set_phase(Phase::Stopping);
        writing.abort();
        assert!(writing.await.unwrap_err().is_cancelled());
        drop(daemon);
        assert_eq!(
            probe(&data.root).unwrap(),
            Ownership::Held {
                phase: Phase::Stopping
            }
        );
        assert!(Core::open(&data).is_err());
        assert!(!released.has_changed().unwrap(), "writer still owns Core");
        release.send(()).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), released.changed())
                .await
                .unwrap()
                .is_err()
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while probe(&data.root).unwrap() != Ownership::Free {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(Core::open(&data).unwrap());
    }
    #[cfg(unix)]
    #[test]
    fn socket_guards_follow_dangling_relative_aliases_without_publishing_phase() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let alias = dir.path().join("alias.sock");
        std::os::unix::fs::symlink("daemon.sock", &alias).unwrap();
        let guard = Lease::for_socket(&socket).unwrap();
        assert_eq!(
            probe_socket(&alias).unwrap(),
            SocketOwnership::Tracked(Ownership::Held {
                phase: Phase::Unknown
            })
        );
        let extra = Lease::for_socket(&alias).unwrap();
        drop(guard);
        assert_eq!(
            probe_socket(&socket).unwrap(),
            SocketOwnership::Tracked(Ownership::Held {
                phase: Phase::Unknown
            })
        );
        assert!(
            std::fs::read(socket_owner_path(&socket).unwrap())
                .unwrap()
                .is_empty()
        );
        drop(extra);
        assert_eq!(
            probe_socket(&alias).unwrap(),
            SocketOwnership::Tracked(Ownership::Free)
        );
        assert!(socket_owner_path(&socket).unwrap().exists());
        std::os::unix::fs::symlink("loop", dir.path().join("loop")).unwrap();
        assert!(probe_socket(&dir.path().join("loop")).is_err());
    }

    #[tokio::test]
    async fn only_a_successfully_bound_owner_publishes_its_store_association() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        assert_eq!(probe_socket(&socket).unwrap(), SocketOwnership::Untracked);
        let first_data = DataDir::new(dir.path().join("first"));
        let first = Daemon::open_at_socket(&first_data, build(), &socket).unwrap();
        let bound = crate::server::UnixServer::bind(&socket).unwrap();
        associate_socket(&socket, &first_data.root).unwrap();
        first.set_phase(Phase::Serving);
        let first_link = std::fs::read_link(socket_data_path(&socket).unwrap()).unwrap();
        for data in [&first_data, &DataDir::new(dir.path().join("rejected"))] {
            let opts = crate::serve::ServeOpts {
                socket: socket.clone(),
                ..crate::serve::ServeOpts::new(data.root.clone())
            };
            assert!(crate::serve::serve(opts).await.is_err());
            assert_eq!(
                std::fs::read_link(socket_data_path(&socket).unwrap()).unwrap(),
                first_link
            );
            assert_eq!(
                probe_socket(&socket).unwrap(),
                SocketOwnership::Tracked(Ownership::Held {
                    phase: Phase::Serving
                })
            );
        }
        // A new binding may use another store while the old owner still has
        // background work. Late phase updates belong only to that old store.
        drop(bound);
        let second_data = DataDir::new(dir.path().join("second"));
        let second = Daemon::open_at_socket(&second_data, build(), &socket).unwrap();
        let _bound = crate::server::UnixServer::bind(&socket).unwrap();
        associate_socket(&socket, &second_data.root).unwrap();
        second.set_phase(Phase::Serving);
        first.set_phase(Phase::Stopping);
        assert_eq!(
            probe_socket(&socket).unwrap(),
            SocketOwnership::Tracked(Ownership::Held {
                phase: Phase::Serving
            })
        );
        drop(second);
        // Its writer may briefly keep Core; wait for the associated store to
        // release, then the remaining old endpoint owner has unknown phase.
        tokio::time::timeout(Duration::from_secs(5), async {
            while probe(&second_data.root).unwrap() != Ownership::Free {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            probe_socket(&socket).unwrap(),
            SocketOwnership::Tracked(Ownership::Held {
                phase: Phase::Unknown
            })
        );
        drop(first);
        tokio::time::timeout(Duration::from_secs(5), async {
            while probe_socket(&socket).unwrap() != SocketOwnership::Tracked(Ownership::Free) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn socket_association_preserves_an_unrelated_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let _lease = Lease::acquire(dir.path()).unwrap();
        let path = socket_data_path(&socket).unwrap();
        std::fs::write(&path, "user data").unwrap();
        assert_eq!(
            associate_socket(&socket, dir.path()).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "user data");
    }
}
