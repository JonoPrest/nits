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
use std::path::Path;
use std::sync::Mutex;

const FILE_NAME: &str = "daemon.lock";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[derive(serde::Serialize)]
pub enum Phase {
    Starting = 1,
    Serving = 2,
    Stopping = 3,
}

impl TryFrom<u8> for Phase {
    type Error = io::Error;

    fn try_from(byte: u8) -> Result<Self, Self::Error> {
        match byte {
            1 => Ok(Self::Starting),
            2 => Ok(Self::Serving),
            3 => Ok(Self::Stopping),
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

/// Retained alongside Core, and dropped only after Core and its stores.
#[derive(Debug)]
pub(crate) struct Lease {
    file: Mutex<File>,
}

impl Lease {
    pub(crate) fn acquire(data_dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(data_dir.join(FILE_NAME))?;
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
    let mut file = match File::open(data_dir.join(FILE_NAME)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Ownership::Free),
        Err(error) => return Err(error),
    };
    match file.try_lock() {
        Ok(()) => Ok(Ownership::Free),
        Err(TryLockError::Error(error)) => Err(error),
        Err(TryLockError::WouldBlock) => {
            let mut byte = [Phase::Starting as u8];
            // A first opener may have acquired the lock just before its initial
            // phase write. An empty file then honestly means it is starting.
            let _ = file.read(&mut byte)?;
            Ok(Ownership::Held {
                phase: Phase::try_from(byte[0])?,
            })
        }
    }
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
        let daemon = Daemon::open(&data, build()).unwrap();
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
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while probe(&data.root).unwrap() != Ownership::Free {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // Free ownership must imply the stores have already been dropped.
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
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while probe(&data.root).unwrap() != Ownership::Free {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(Core::open(&data).unwrap());
    }
}
