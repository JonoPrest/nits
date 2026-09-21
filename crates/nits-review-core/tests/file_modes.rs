//! Git's finite tree-entry modes are part of review and viewed identity.
use nits_protocol::{
    Author, BlobEntry, BlobMode, BlobOid, ChangeKind, ClientId, ClientSeq, CommitOid, DiffScope,
    NonEmpty, RefSpec, RenderContent, RenderOpts, RenderTarget, RepoId, RepoPath, ReviewId,
    ReviewTarget, Timestamp, ViewedContent, WorkspaceId,
};
use nits_review_core::git::Repo;
use nits_review_core::review::ViewedState;
use nits_review_core::{Core, Ctx, DataDir};
use nits_test_support::{RepoBuilder, TestRepo, files};

fn ctx() -> Ctx {
    Ctx {
        author: Author::Human {
            name: "ada".into(),
            machine: "box".into(),
        },
        client_id: ClientId::from_parts(1, 1),
        client_seq: ClientSeq::new(0),
        now: Timestamp::from_millis(1),
    }
}
fn repo_id() -> RepoId {
    RepoId::from_parts(1, 2)
}
fn review_id() -> ReviewId {
    ReviewId::from_parts(1, 3)
}
fn path() -> RepoPath {
    RepoPath::new("entry").unwrap()
}
fn head(repo: &TestRepo) -> CommitOid {
    repo.rev_parse("HEAD").unwrap().parse().unwrap()
}
fn tree(repo: &Repo, commit: CommitOid) -> nits_protocol::TreeOid {
    repo.resolve(&RefSpec::Commit { oid: commit }).unwrap().tree
}

#[test]
fn committed_mode_and_file_type_transitions_keep_identical_blob_ids() {
    let test = RepoBuilder::new()
        .commit("initial", files!["entry" => "destination"])
        .build()
        .unwrap();
    let oid: BlobOid = test
        .git(&["rev-parse", "HEAD:entry"])
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let repo = Repo::open(test.path()).unwrap();
    let mut revisions = Vec::new();
    for (mode, octal) in [
        (BlobMode::Regular, "100644"),
        (BlobMode::Executable, "100755"),
        (BlobMode::Symlink, "120000"),
    ] {
        test.git(&[
            "update-index",
            "--cacheinfo",
            &format!("{octal},{oid},entry"),
        ])
        .unwrap();
        test.git(&["commit", "--allow-empty", "-qm", octal])
            .unwrap();
        revisions.push((mode, head(&test)));
    }
    for &(old_mode, old_commit) in &revisions {
        for &(new_mode, new_commit) in &revisions {
            let changes = repo
                .changed_files(tree(&repo, old_commit), tree(&repo, new_commit))
                .unwrap();
            if old_mode == new_mode {
                assert!(changes.is_empty());
            } else {
                assert_eq!(changes.len(), 1);
                assert_eq!(
                    changes[0].kind,
                    ChangeKind::Modified {
                        old: BlobEntry {
                            oid,
                            mode: old_mode
                        },
                        new: BlobEntry {
                            oid,
                            mode: new_mode
                        }
                    }
                );
            }
        }
    }
}

#[test]
fn executable_add_rename_and_symlink_delete_retain_modes() {
    let test = RepoBuilder::new()
        .commit("base", files!["README" => "project\n"])
        .build()
        .unwrap();
    let base = head(&test);
    test.write_file("entry", b"destination").unwrap();
    test.git(&["add", "entry"]).unwrap();
    let oid: BlobOid = test
        .git(&["rev-parse", ":entry"])
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    test.git(&["update-index", "--chmod=+x", "entry"]).unwrap();
    test.git(&["commit", "-qm", "executable"]).unwrap();
    let added = head(&test);
    test.git(&["mv", "entry", "renamed"]).unwrap();
    test.git(&["commit", "-qm", "rename"]).unwrap();
    let renamed = head(&test);
    test.git(&[
        "update-index",
        "--cacheinfo",
        &format!("120000,{oid},renamed"),
    ])
    .unwrap();
    test.git(&["commit", "-qm", "symlink"]).unwrap();
    let symlink = head(&test);
    test.git(&["update-index", "--force-remove", "renamed"])
        .unwrap();
    test.git(&["commit", "-qm", "delete"]).unwrap();
    let deleted = head(&test);
    let repo = Repo::open(test.path()).unwrap();
    let executable = BlobEntry {
        oid,
        mode: BlobMode::Executable,
    };
    for (old, new, expected) in [
        (base, added, ChangeKind::Added { new: executable }),
        (
            added,
            renamed,
            ChangeKind::Renamed {
                from: path(),
                old: executable,
                new: executable,
            },
        ),
        (
            symlink,
            deleted,
            ChangeKind::Deleted {
                old: BlobEntry {
                    oid,
                    mode: BlobMode::Symlink,
                },
            },
        ),
    ] {
        let changes = repo
            .changed_files(tree(&repo, old), tree(&repo, new))
            .unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, expected);
    }
}

#[cfg(unix)]
#[test]
#[allow(clippy::too_many_lines)] // One real Git review follows mode changes, viewed marks, rendering and restart.
fn working_tree_metadata_changes_clear_viewed_and_render_through_restart() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let test = RepoBuilder::new()
        .commit("initial", files!["entry" => "destination"])
        .build()
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let data = DataDir::new(temp.path().join("data"));
    let core = Core::open(&data).unwrap();
    let workspace = WorkspaceId::from_parts(1, 1);
    core.create_workspace(&ctx(), workspace, "Modes".into())
        .unwrap();
    core.attach_repo(
        &ctx(),
        workspace,
        repo_id(),
        test.path().to_str().unwrap(),
        "Repository".into(),
    )
    .unwrap();
    core.create_review(
        &ctx(),
        review_id(),
        workspace,
        "Metadata".into(),
        NonEmpty::singleton(ReviewTarget {
            repo_id: repo_id(),
            base: RefSpec::Head,
            head: RefSpec::WorkingTree,
        }),
    )
    .unwrap();
    let regular = core
        .mark_viewed(&ctx(), review_id(), repo_id(), path())
        .unwrap();
    let ViewedContent::Blob { entry: old } = regular.content else {
        panic!("blob")
    };
    assert_eq!(old.mode, BlobMode::Regular);
    std::fs::set_permissions(
        test.path().join("entry"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    core.resolve_targets(&ctx(), review_id()).unwrap();
    assert_eq!(
        core.viewed_state(review_id(), repo_id(), &path()).unwrap(),
        ViewedState::ChangedSinceViewed {
            marked: regular.content
        }
    );
    let (header, _) = core
        .file_render(
            review_id(),
            repo_id(),
            &path(),
            RenderOpts::default(),
            &DiffScope::All,
        )
        .unwrap();
    assert_eq!(
        header.target,
        RenderTarget::Diff {
            change: ChangeKind::Modified {
                old,
                new: BlobEntry {
                    mode: BlobMode::Executable,
                    ..old
                },
            }
        }
    );
    assert!(matches!(
        header.content,
        RenderContent::Text {
            additions: 0,
            deletions: 0,
            ..
        }
    ));
    core.mark_viewed(&ctx(), review_id(), repo_id(), path())
        .unwrap();
    drop(core);
    let core = Core::open(&data).unwrap();
    assert_eq!(
        core.viewed_state(review_id(), repo_id(), &path()).unwrap(),
        ViewedState::Viewed
    );
    std::fs::set_permissions(
        test.path().join("entry"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    core.resolve_targets(&ctx(), review_id()).unwrap();
    assert!(matches!(
        core.viewed_state(review_id(), repo_id(), &path()).unwrap(),
        ViewedState::ChangedSinceViewed { .. }
    ));
    assert!(core.files(review_id()).unwrap().is_empty());
    core.mark_viewed(&ctx(), review_id(), repo_id(), path())
        .unwrap();
    std::fs::remove_file(test.path().join("entry")).unwrap();
    symlink("destination", test.path().join("entry")).unwrap();
    core.resolve_targets(&ctx(), review_id()).unwrap();
    let change = core.files(review_id()).unwrap().remove(0).kind;
    assert_eq!(
        change,
        ChangeKind::Modified {
            old,
            new: BlobEntry {
                mode: BlobMode::Symlink,
                ..old
            }
        }
    );
    assert!(matches!(
        core.viewed_state(review_id(), repo_id(), &path()).unwrap(),
        ViewedState::ChangedSinceViewed { .. }
    ));
    let (header, _) = core
        .file_render(
            review_id(),
            repo_id(),
            &path(),
            RenderOpts::default(),
            &DiffScope::All,
        )
        .unwrap();
    assert_eq!(header.target, RenderTarget::Diff { change });
    assert!(matches!(
        header.content,
        RenderContent::Text {
            additions: 0,
            deletions: 0,
            ..
        }
    ));
    let (blob, _) = core
        .blob_render(
            repo_id(),
            &path(),
            BlobEntry {
                mode: BlobMode::Symlink,
                ..old
            },
        )
        .unwrap();
    assert_eq!(
        blob.target,
        RenderTarget::Blob {
            entry: BlobEntry {
                mode: BlobMode::Symlink,
                ..old
            }
        }
    );
}

#[cfg(unix)]
#[test]
fn working_tree_mode_changes_respect_git_core_filemode_policy() {
    use std::os::unix::fs::PermissionsExt;
    let test = RepoBuilder::new()
        .commit("initial", files!["entry" => "content\n"])
        .build()
        .unwrap();
    test.git(&["config", "core.filemode", "false"]).unwrap();
    std::fs::set_permissions(
        test.path().join("entry"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let repo = Repo::open(test.path()).unwrap();
    let base = repo.resolve(&RefSpec::Head).unwrap();
    let current = repo.resolve(&RefSpec::WorkingTree).unwrap();
    assert_eq!(base.tree, current.tree);
    assert!(
        repo.changed_files(base.tree, current.tree)
            .unwrap()
            .is_empty()
    );
}
