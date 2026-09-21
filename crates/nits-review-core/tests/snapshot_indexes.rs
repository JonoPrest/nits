//! Background snapshots must not refresh a checkout's real Git indexes.
#![cfg(unix)]

use std::fs::{File, Metadata};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use nits_protocol::{CommitOid, RepoId, TreeEntryKind};
use nits_review_core::git::Repo;
use nits_test_support::{RepoBuilder, TestRepo, files};

struct IndexBefore {
    path: PathBuf,
    file: File,
    metadata: Metadata,
    bytes: Vec<u8>,
}

impl IndexBefore {
    fn capture(repo: &Repo) -> Self {
        let path = repo.metadata_paths().unwrap().worktree.join("index");
        let mut file = File::open(&path).unwrap();
        let metadata = file.metadata().unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        Self {
            path,
            file,
            metadata,
            bytes,
        }
    }

    fn assert_unchanged(&self) {
        // Retaining the original descriptor prevents inode reuse from hiding
        // an atomic index refresh, even when its bytes happen to be identical.
        assert_eq!(self.file.metadata().unwrap().ino(), self.metadata.ino());
        let now = std::fs::metadata(&self.path).unwrap();
        assert_eq!(now.dev(), self.metadata.dev());
        assert_eq!(
            now.ino(),
            self.metadata.ino(),
            "replaced {}",
            self.path.display()
        );
        assert_eq!(now.modified().unwrap(), self.metadata.modified().unwrap());
        assert_eq!(std::fs::read(&self.path).unwrap(), self.bytes);
    }
}

#[derive(Clone, Copy)]
enum Layout {
    Absorbed,
    Linked,
}

fn repositories(layout: Layout) -> (TestRepo, TestRepo) {
    let dependency = RepoBuilder::new()
        .commit("first", files!["lib.txt" => "first\n"])
        .tag("first")
        .commit("second", files!["lib.txt" => "second\n"])
        .build()
        .unwrap();
    let project = RepoBuilder::new()
        .commit("base", files!["root.txt" => "base\n"])
        .build()
        .unwrap();
    let old = dependency.rev_parse("first").unwrap();
    match layout {
        Layout::Absorbed => {
            project
                .git(&[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "add",
                    dependency.path().to_str().unwrap(),
                    "dep",
                ])
                .unwrap();
            project.git(&["-C", "dep", "checkout", "-q", &old]).unwrap();
        }
        Layout::Linked => {
            dependency
                .git(&[
                    "worktree",
                    "add",
                    "--detach",
                    project.path().join("dep").to_str().unwrap(),
                    &old,
                ])
                .unwrap();
        }
    }
    project.git(&["add", "."]).unwrap();
    project.git(&["commit", "-qm", "dependency"]).unwrap();
    (dependency, project)
}

fn snapshot_preserves_indexes(layout: Layout) {
    let (dependency, project) = repositories(layout);
    let repo = Repo::open(project.path()).unwrap();
    let child = Repo::open(&project.path().join("dep")).unwrap();
    let root_index = IndexBefore::capture(&repo);
    for revision in ["first", "HEAD"] {
        let oid = dependency.rev_parse(revision).unwrap();
        project.git(&["-C", "dep", "checkout", "-q", &oid]).unwrap();
        let dependency_index = IndexBefore::capture(&child);
        // An unchanged file with stale stat information makes the optional
        // dependency refresh observable without relying on wall-clock timing.
        File::options()
            .write(true)
            .open(project.path().join("dep/lib.txt"))
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000))
            .unwrap();
        let working = format!("worktree {revision}\n");
        project.write_file("root.txt", working.as_bytes()).unwrap();
        let current = repo.working_tree().unwrap();
        let snapshot = repo.tree_snapshot(RepoId::nil(), current.tree).unwrap();
        let pointer = snapshot
            .entries
            .iter()
            .find(|e| e.path.as_str() == "dep")
            .unwrap();
        assert_eq!(
            pointer.kind,
            TreeEntryKind::Submodule {
                commit: oid.parse::<CommitOid>().unwrap()
            }
        );
        let root = snapshot
            .entries
            .iter()
            .find(|e| e.path.as_str() == "root.txt")
            .unwrap();
        let TreeEntryKind::File { oid, .. } = root.kind else {
            panic!("root blob");
        };
        assert_eq!(repo.blob(oid).unwrap(), working.as_bytes());
        root_index.assert_unchanged();
        dependency_index.assert_unchanged();
        assert!(!dependency_index.path.with_extension("lock").exists());
        assert!(
            std::fs::read_dir(repo.metadata_paths().unwrap().worktree)
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("nits-index-"))
        );
    }
}

#[test]
fn snapshot_preserves_absorbed_submodule_index_while_capturing_edits_and_pointers() {
    snapshot_preserves_indexes(Layout::Absorbed);
}

#[test]
fn snapshot_preserves_linked_dependency_index_while_capturing_edits_and_pointers() {
    snapshot_preserves_indexes(Layout::Linked);
}
