//! Descriptor-relative access to a suggestion's original working-tree file.
//!
//! Repository paths are untrusted filesystem names even after `RepoPath` rules
//! out `..`: every component can be replaced with a link after anchoring.

use std::ffi::OsString;
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

use nits_protocol::RepoPath;
use rustix::fs::{Mode, OFlags, open, openat};

use crate::CoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplacePoint {
    BeforeClaim,
    Claimed,
    BeforeInstall,
    Installed,
}

/// A private directory inside Git metadata. Once the original is claimed,
/// cleanup must never remove it on an error or during unwinding.
#[derive(Debug)]
struct Staging {
    path: std::path::PathBuf,
    parent: File,
    name: OsString,
    dir: File,
}

impl Staging {
    fn create(
        metadata: &Path,
        parent: &File,
        checkout: &Path,
        path: &RepoPath,
        bytes: &[u8],
        mode: &Metadata,
    ) -> Result<Self, CoreError> {
        let metadata_dir = File::open(metadata)?;
        let temporary = tempfile::Builder::new()
            .prefix("nits-suggestion-")
            .tempdir_in(metadata)?;
        let name = temporary
            .path()
            .file_name()
            .ok_or_else(|| refused(path, "expected a staging directory name"))?
            .to_os_string();
        let dir = directory_at(&metadata_dir, &name)?;
        if dir.metadata()?.dev() != parent.metadata()?.dev() {
            return Err(refused(
                path,
                "Git metadata and target are on different filesystems; safe replacement is unavailable",
            ));
        }
        let create = |name: &str| -> std::io::Result<File> {
            Ok(openat(
                &dir,
                name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )?
            .into())
        };
        let mut proposed = create("proposed")?;
        proposed.write_all(bytes)?;
        proposed.set_permissions(mode.permissions())?;
        proposed.sync_all()?;
        // Exercise the exact primitive before claiming a checkout entry. An
        // unsupported kernel/filesystem fails without an overwriting fallback.
        move_if_absent(&dir, "proposed".as_ref(), &dir, "prepared".as_ref()).map_err(|error| {
            refused(
                path,
                format!("safe no-replace rename is unavailable: {error}"),
            )
        })?;
        move_if_absent(&dir, "prepared".as_ref(), &dir, "proposed".as_ref())?;
        let mut recovery = create("RECOVERY.txt")?;
        writeln!(
            recovery,
            "Interrupted Nits suggestion replacement.\nCheckout: {}\nRelative path: {path}\n\noriginal contains the claimed file, if the replacement reached that stage.\nproposed contains the suggested bytes until installation.\nDo not overwrite a newer checkout file. Restore original only after inspecting\nthe checkout and this directory; a successful application cleans this directory.\nNo files here are automatically deleted while original is present.",
            checkout.display()
        )?;
        recovery.sync_all()?;
        dir.sync_all()?;
        metadata_dir.sync_all()?;
        Ok(Self {
            path: temporary.keep(),
            parent: metadata_dir,
            name,
            dir,
        })
    }

    fn recovery_error(&self, path: &RepoPath, reason: impl std::fmt::Display) -> CoreError {
        refused(
            path,
            format!(
                "{reason}; original/proposed files are preserved for recovery at {} (inside the moved Git metadata if the checkout was renamed)",
                self.path.display()
            ),
        )
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        // A crash or failed rollback leaves a discoverable original. Never let
        // a generic temporary-directory destructor silently erase that data.
        if matches!(
            rustix::fs::statat(&self.dir, "original", rustix::fs::AtFlags::SYMLINK_NOFOLLOW),
            Err(rustix::io::Errno::NOENT)
        ) {
            // The checkout itself can move. Clean only our known entries via
            // retained descriptors, never recursively through its old path.
            for name in ["proposed", "prepared", "RECOVERY.txt"] {
                let _ = rustix::fs::unlinkat(&self.dir, name, rustix::fs::AtFlags::empty());
            }
            let _ = rustix::fs::unlinkat(&self.parent, &self.name, rustix::fs::AtFlags::REMOVEDIR);
            let _ = self.parent.sync_all();
        }
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
fn move_if_absent(
    from: &File,
    name: &std::ffi::OsStr,
    to: &File,
    destination: &std::ffi::OsStr,
) -> std::io::Result<()> {
    Ok(rustix::fs::renameat_with(
        from,
        name,
        to,
        destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )?)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
fn move_if_absent(
    _: &File,
    _: &std::ffi::OsStr,
    _: &File,
    _: &std::ffi::OsStr,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "safe no-replace rename is unavailable on this platform",
    ))
}

/// An opened regular file and its parent, reached without following links.
#[derive(Debug)]
pub(crate) struct WorkingFile {
    root: File,
    root_path: std::path::PathBuf,
    parent: File,
    name: OsString,
    path: RepoPath,
    original: File,
    identity: Metadata,
}

fn refused(path: &RepoPath, reason: impl std::fmt::Display) -> CoreError {
    CoreError::invalid(format!("cannot apply suggestion to {path}: {reason}"))
}

fn directory_at(parent: &File, name: &std::ffi::OsStr) -> std::io::Result<File> {
    Ok(openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?
    .into())
}

fn regular_at(parent: &File, name: &std::ffi::OsStr) -> std::io::Result<File> {
    // NONBLOCK prevents a substituted FIFO from hanging before we inspect its
    // type. NOFOLLOW rejects a substituted symlink at the actual open.
    Ok(openat(
        parent,
        name,
        OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?
    .into())
}

fn same_file(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

fn regular_single_link(path: &RepoPath, meta: &Metadata) -> Result<(), CoreError> {
    if !meta.is_file() {
        return Err(refused(path, "only regular files support suggestions"));
    }
    if meta.nlink() != 1 {
        return Err(refused(
            path,
            "hard-linked files do not support suggestions",
        ));
    }
    Ok(())
}

impl WorkingFile {
    pub(crate) fn open(root: &Path, path: &RepoPath) -> Result<Self, CoreError> {
        if !cfg!(any(
            target_os = "linux",
            target_os = "android",
            target_os = "macos",
            target_os = "ios"
        )) {
            return Err(refused(
                path,
                "safe no-replace rename is unavailable on this platform",
            ));
        }
        let root_path = root.to_path_buf();
        let root: File = open(
            root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| refused(path, error))?
        .into();
        let (parent, name) = Self::parent_at(&root, path)?;
        let original = regular_at(&parent, &name).map_err(|error| refused(path, error))?;
        let identity = original.metadata()?;
        regular_single_link(path, &identity)?;
        Ok(Self {
            root,
            root_path,
            parent,
            name,
            path: path.clone(),
            original,
            identity,
        })
    }

    fn parent_at(root: &File, path: &RepoPath) -> Result<(File, OsString), CoreError> {
        let mut parent = root.try_clone()?;
        let mut components = Path::new(path.as_str()).components().peekable();
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(refused(path, "expected a repository-relative file path"));
            };
            if components.peek().is_none() {
                return Ok((parent, name.to_os_string()));
            }
            parent = directory_at(&parent, name).map_err(|error| refused(path, error))?;
        }
        Err(refused(path, "expected a file path"))
    }

    pub(crate) fn read(&mut self) -> Result<Vec<u8>, CoreError> {
        self.original.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        self.original.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    /// Re-resolve from the retained checkout descriptor, never from a name
    /// that may now be an outside-checkout symlink.
    fn verify_parent(&self) -> Result<(), CoreError> {
        let root: File = open(
            &self.root_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| refused(&self.path, error))?
        .into();
        if !same_file(&root.metadata()?, &self.root.metadata()?) {
            return Err(refused(
                &self.path,
                "checkout directory changed; reopen the suggestion",
            ));
        }
        let (parent, _) = Self::parent_at(&self.root, &self.path)?;
        if !same_file(&parent.metadata()?, &self.parent.metadata()?) {
            return Err(refused(
                &self.path,
                "parent directory changed; reopen the suggestion",
            ));
        }
        Ok(())
    }

    fn verify_current(&mut self, expected: &[u8]) -> Result<(), CoreError> {
        self.verify_parent()?;
        let current =
            regular_at(&self.parent, &self.name).map_err(|error| refused(&self.path, error))?;
        let meta = current.metadata()?;
        regular_single_link(&self.path, &meta)?;
        if !same_file(&meta, &self.identity)
            || meta.mode() != self.identity.mode()
            || self.read()? != expected
        {
            return Err(refused(&self.path, "file changed; reopen the suggestion"));
        }
        Ok(())
    }

    pub(crate) fn replace(
        &mut self,
        metadata: &Path,
        expected: &[u8],
        patched: &[u8],
    ) -> Result<(), CoreError> {
        self.replace_with(metadata, expected, patched, |_| {})
    }

    fn replace_with(
        &mut self,
        metadata: &Path,
        expected: &[u8],
        patched: &[u8],
        mut at: impl FnMut(ReplacePoint),
    ) -> Result<(), CoreError> {
        self.verify_current(expected)?;
        self.original.sync_all()?;
        let stage = Staging::create(
            metadata,
            &self.parent,
            &self.root_path,
            &self.path,
            patched,
            &self.identity,
        )?;
        at(ReplacePoint::BeforeClaim);
        move_if_absent(&self.parent, &self.name, &stage.dir, "original".as_ref())
            .map_err(|error| refused(&self.path, error))?;
        // Both directory entries are durable before installing anything. An
        // interrupted process leaves the original and recovery note in Git
        // metadata, not an automatically cleaned temporary file.
        let claimed = (|| {
            stage.dir.sync_all()?;
            self.parent.sync_all()?;
            at(ReplacePoint::Claimed);
            let mut original = regular_at(&stage.dir, "original".as_ref())?;
            let identity = original.metadata()?;
            regular_single_link(&self.path, &identity)?;
            let mut bytes = Vec::new();
            original.read_to_end(&mut bytes)?;
            if !same_file(&identity, &self.identity)
                || identity.mode() != self.identity.mode()
                || bytes != expected
            {
                return Err(refused(
                    &self.path,
                    "file was replaced or edited during application",
                ));
            }
            at(ReplacePoint::BeforeInstall);
            self.verify_parent()?;
            move_if_absent(&stage.dir, "proposed".as_ref(), &self.parent, &self.name)?;
            Ok::<(), CoreError>(())
        })();
        if let Err(error) = claimed {
            // Never overwrite an intervening file to hide the conflict. If
            // restoration is obstructed, preserve both copies and name them.
            if self.verify_parent().is_err()
                || move_if_absent(&stage.dir, "original".as_ref(), &self.parent, &self.name)
                    .is_err()
            {
                return Err(stage.recovery_error(&self.path, error));
            }
            self.parent.sync_all()?;
            stage.dir.sync_all()?;
            return Err(error);
        }
        at(ReplacePoint::Installed);
        self.parent
            .sync_all()
            .map_err(|error| stage.recovery_error(&self.path, error))?;
        stage
            .dir
            .sync_all()
            .map_err(|error| stage.recovery_error(&self.path, error))?;
        rustix::fs::unlinkat(&stage.dir, "original", rustix::fs::AtFlags::empty())
            .map_err(|error| stage.recovery_error(&self.path, error))?;
        stage.dir.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    struct Fixture {
        dir: tempfile::TempDir,
        root: std::path::PathBuf,
        metadata: std::path::PathBuf,
        outside: std::path::PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("checkout");
            let metadata = root.join(".git");
            let outside = dir.path().join("outside");
            std::fs::create_dir_all(root.join("dir")).unwrap();
            std::fs::create_dir(&metadata).unwrap();
            std::fs::create_dir(&outside).unwrap();
            std::fs::write(root.join("dir/file"), b"base\n").unwrap();
            std::fs::write(outside.join("file"), b"base\n").unwrap();
            Self {
                dir,
                root,
                metadata,
                outside,
            }
        }

        fn open(&self) -> WorkingFile {
            WorkingFile::open(&self.root, &RepoPath::new("dir/file").unwrap()).unwrap()
        }

        fn stages(&self) -> Vec<std::path::PathBuf> {
            std::fs::read_dir(&self.metadata)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect()
        }
    }

    #[test]
    fn replacement_preserves_mode_and_cleans_staging() {
        let fixture = Fixture::new();
        for mode in [0o640, 0o755] {
            std::fs::set_permissions(
                fixture.root.join("dir/file"),
                std::fs::Permissions::from_mode(mode),
            )
            .unwrap();
            let mut file = fixture.open();
            file.replace(&fixture.metadata, b"base\n", b"base\n")
                .unwrap();
            assert_eq!(
                std::fs::metadata(fixture.root.join("dir/file"))
                    .unwrap()
                    .mode()
                    & 0o777,
                mode
            );
            assert!(fixture.stages().is_empty());
        }
    }

    #[test]
    fn changed_leaf_between_open_and_claim_is_restored_without_overwriting_it() {
        for linked in [false, true] {
            let fixture = Fixture::new();
            let mut file = fixture.open();
            let target = fixture.root.join("dir/file");
            let result = file.replace_with(&fixture.metadata, b"base\n", b"patched\n", |point| {
                if point == ReplacePoint::BeforeClaim {
                    std::fs::rename(&target, fixture.root.join("saved-original")).unwrap();
                    if linked {
                        symlink(fixture.outside.join("file"), &target).unwrap();
                    } else {
                        std::fs::write(&target, b"newer\n").unwrap();
                    }
                }
            });
            assert!(result.is_err());
            assert_eq!(
                std::fs::read(fixture.outside.join("file")).unwrap(),
                b"base\n"
            );
            assert_eq!(
                std::fs::read(fixture.root.join("saved-original")).unwrap(),
                b"base\n"
            );
            if linked {
                assert!(target.is_symlink());
            } else {
                assert_eq!(std::fs::read(target).unwrap(), b"newer\n");
            }
            assert!(fixture.stages().is_empty());
        }
    }

    #[test]
    fn parent_replacement_keeps_the_original_recoverable_and_never_follows_the_new_link() {
        let fixture = Fixture::new();
        let mut file = fixture.open();
        let result = file.replace_with(&fixture.metadata, b"base\n", b"patched\n", |point| {
            if point == ReplacePoint::BeforeClaim {
                std::fs::rename(fixture.root.join("dir"), fixture.root.join("old-dir")).unwrap();
                symlink(&fixture.outside, fixture.root.join("dir")).unwrap();
            }
        });
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("preserved for recovery")
        );
        assert_eq!(
            std::fs::read(fixture.outside.join("file")).unwrap(),
            b"base\n"
        );
        let stages = fixture.stages();
        assert_eq!(stages.len(), 1);
        assert_eq!(
            std::fs::read(stages[0].join("original")).unwrap(),
            b"base\n"
        );
        assert_eq!(
            std::fs::read(stages[0].join("proposed")).unwrap(),
            b"patched\n"
        );
    }

    #[test]
    fn checkout_replacement_preserves_recovery_and_never_cleans_the_new_checkout() {
        for point in [
            ReplacePoint::BeforeClaim,
            ReplacePoint::BeforeInstall,
            ReplacePoint::Installed,
        ] {
            for linked in [false, true] {
                let fixture = Fixture::new();
                let mut file = fixture.open();
                let moved = fixture.dir.path().join("moved-checkout");
                let replacement = fixture.dir.path().join("replacement-checkout");
                let mut stage_name = OsString::new();
                let result = file.replace_with(&fixture.metadata, b"base\n", b"patched\n", |at| {
                    if at == point {
                        stage_name = fixture.stages()[0].file_name().unwrap().to_os_string();
                        std::fs::rename(&fixture.root, &moved).unwrap();
                        std::fs::create_dir_all(replacement.join("dir")).unwrap();
                        std::fs::write(replacement.join("dir/file"), b"newer\n").unwrap();
                        let new_stage = replacement.join(".git").join(&stage_name);
                        std::fs::create_dir_all(&new_stage).unwrap();
                        std::fs::write(new_stage.join("RECOVERY.txt"), b"unrelated\n").unwrap();
                        if linked {
                            symlink(&replacement, &fixture.root).unwrap();
                        } else {
                            std::fs::rename(&replacement, &fixture.root).unwrap();
                        }
                    }
                });
                assert_eq!(
                    std::fs::read(fixture.root.join("dir/file")).unwrap(),
                    b"newer\n"
                );
                assert_eq!(
                    std::fs::read(fixture.metadata.join(&stage_name).join("RECOVERY.txt")).unwrap(),
                    b"unrelated\n"
                );
                let stage = moved.join(".git").join(stage_name);
                if point == ReplacePoint::Installed {
                    result.unwrap();
                    assert_eq!(std::fs::read(moved.join("dir/file")).unwrap(), b"patched\n");
                    assert!(!stage.exists());
                } else {
                    assert!(
                        result
                            .unwrap_err()
                            .to_string()
                            .contains("inside the moved Git metadata")
                    );
                    assert_eq!(std::fs::read(stage.join("original")).unwrap(), b"base\n");
                    assert_eq!(std::fs::read(stage.join("proposed")).unwrap(), b"patched\n");
                    assert!(stage.join("RECOVERY.txt").exists());
                    assert!(!moved.join("dir/file").exists());
                }
            }
        }
    }

    #[test]
    fn destination_reappearing_before_installation_is_never_overwritten() {
        let fixture = Fixture::new();
        let mut file = fixture.open();
        let target = fixture.root.join("dir/file");
        let result = file.replace_with(&fixture.metadata, b"base\n", b"patched\n", |point| {
            if point == ReplacePoint::BeforeInstall {
                std::fs::write(&target, b"newer\n").unwrap();
            }
        });
        let error = result.unwrap_err().to_string();
        assert!(error.contains("preserved for recovery"), "{error}");
        assert_eq!(std::fs::read(target).unwrap(), b"newer\n");
        let stages = fixture.stages();
        assert_eq!(stages.len(), 1);
        assert_eq!(
            std::fs::read(stages[0].join("original")).unwrap(),
            b"base\n"
        );
        assert_eq!(
            std::fs::read(stages[0].join("proposed")).unwrap(),
            b"patched\n"
        );
    }

    #[test]
    fn hard_link_created_after_open_is_rejected_without_changing_either_name() {
        let fixture = Fixture::new();
        let mut file = fixture.open();
        let alias = fixture.outside.join("alias");
        let target = fixture.root.join("dir/file");
        let result = file.replace_with(&fixture.metadata, b"base\n", b"patched\n", |point| {
            if point == ReplacePoint::BeforeClaim {
                std::fs::hard_link(&target, &alias).unwrap();
            }
        });
        assert!(result.unwrap_err().to_string().contains("hard-linked"));
        assert_eq!(std::fs::read(target).unwrap(), b"base\n");
        assert_eq!(std::fs::read(alias).unwrap(), b"base\n");
        assert!(fixture.stages().is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn different_filesystem_staging_fails_before_claiming_the_target() {
        let fixture = Fixture::new();
        let Ok(metadata) = tempfile::tempdir_in("/dev/shm") else {
            return;
        };
        if metadata.path().metadata().unwrap().dev() == fixture.root.metadata().unwrap().dev() {
            return;
        }
        let mut file = fixture.open();
        let result = file.replace_with(metadata.path(), b"base\n", b"patched\n", |_| {
            panic!("target must not be claimed")
        });
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("different filesystems")
        );
        assert_eq!(
            std::fs::read(fixture.root.join("dir/file")).unwrap(),
            b"base\n"
        );
        assert_eq!(std::fs::read_dir(metadata.path()).unwrap().count(), 0);
    }

    #[test]
    fn process_interruption_preserves_original_bytes_before_and_after_install() {
        for point in ["claimed", "installed"] {
            let fixture = Fixture::new();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "suggestion_file::tests::interruption_child",
                    "--nocapture",
                ])
                .env("NITS_SUGGESTION_TEST_ROOT", &fixture.root)
                .env("NITS_SUGGESTION_TEST_POINT", point)
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(17));
            let stages = fixture.stages();
            assert_eq!(stages.len(), 1);
            assert_eq!(
                std::fs::read(stages[0].join("original")).unwrap(),
                b"base\n"
            );
            assert!(stages[0].join("RECOVERY.txt").is_file());
            if point == "installed" {
                assert_eq!(
                    std::fs::read(fixture.root.join("dir/file")).unwrap(),
                    b"patched\n"
                );
            } else {
                assert!(!fixture.root.join("dir/file").exists());
                assert_eq!(
                    std::fs::read(stages[0].join("proposed")).unwrap(),
                    b"patched\n"
                );
            }
        }
    }

    #[test]
    fn interruption_child() {
        let Some(root) = std::env::var_os("NITS_SUGGESTION_TEST_ROOT") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let selected = std::env::var("NITS_SUGGESTION_TEST_POINT").unwrap();
        let mut file = WorkingFile::open(&root, &RepoPath::new("dir/file").unwrap()).unwrap();
        file.replace_with(&root.join(".git"), b"base\n", b"patched\n", |point| {
            if (selected == "claimed" && point == ReplacePoint::Claimed)
                || (selected == "installed" && point == ReplacePoint::Installed)
            {
                std::process::exit(17);
            }
        })
        .unwrap();
    }
}
