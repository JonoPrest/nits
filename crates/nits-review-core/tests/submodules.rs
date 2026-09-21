//! Gitlinks in real Git indexes reference commits outside the superproject.
use nits_protocol::{
    Anchor, Author, BlobOid, ChangeKind, ClientId, ClientSeq, CommentId, CommentKind, CommentState,
    CommitOid, DiffScope, NonEmpty, RefSpec, RenderContent, RenderOpts, RepoId, RepoPath, ReviewId,
    ReviewTarget, SubmoduleChange, Timestamp, ViewedContent, WorkspaceId,
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
    RepoPath::new("dep").unwrap()
}
fn commit(repo: &TestRepo, message: &str) -> CommitOid {
    repo.git(&["commit", "-qm", message]).unwrap();
    repo.rev_parse("HEAD").unwrap().parse().unwrap()
}
fn gitlink(repo: &TestRepo, oid: CommitOid) -> CommitOid {
    repo.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{oid},dep"),
    ])
    .unwrap();
    commit(repo, "set gitlink")
}
struct History {
    repo: TestRepo,
    base: CommitOid,
    added: CommitOid,
    updated: CommitOid,
    deleted: CommitOid,
    blob: CommitOid,
    to_link: CommitOid,
    to_blob: CommitOid,
    old: CommitOid,
    new: CommitOid,
    blob_oid: BlobOid,
}
impl History {
    fn new() -> Self {
        let dependency = RepoBuilder::new()
            .commit("first", files!["lib.txt" => "first\n"])
            .tag("first")
            .commit("second", files!["lib.txt" => "second\n"])
            .build()
            .unwrap();
        let old = dependency.rev_parse("first").unwrap().parse().unwrap();
        let new = dependency.rev_parse("HEAD").unwrap().parse().unwrap();
        let repo = RepoBuilder::new()
            .commit("base", files!["README" => "project\n"])
            .build()
            .unwrap();
        let base = repo.rev_parse("HEAD").unwrap().parse().unwrap();
        let added = gitlink(&repo, old);
        let updated = gitlink(&repo, new);
        repo.git(&["update-index", "--force-remove", "dep"])
            .unwrap();
        let deleted = commit(&repo, "remove gitlink");
        repo.write_file("dep", b"file content\n").unwrap();
        repo.git(&["add", "dep"]).unwrap();
        let blob = commit(&repo, "add blob");
        let blob_oid = repo
            .git(&["rev-parse", "HEAD:dep"])
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let to_link = gitlink(&repo, new);
        repo.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{blob_oid},dep"),
        ])
        .unwrap();
        let to_blob = commit(&repo, "replace gitlink with blob");
        // No commit from the dependency exists in the superproject object database.
        assert!(repo.git(&["cat-file", "-e", &old.to_string()]).is_err());
        Self {
            repo,
            base,
            added,
            updated,
            deleted,
            blob,
            to_link,
            to_blob,
            old,
            new,
            blob_oid,
        }
    }
    fn core(&self, data: &DataDir, base: CommitOid, head: CommitOid) -> Core {
        self.repo
            .git(&["update-ref", "refs/heads/review-head", &head.to_string()])
            .unwrap();
        let core = Core::open(data).unwrap();
        let ws = WorkspaceId::from_parts(1, 1);
        core.create_workspace(&ctx(), ws, "Project".into()).unwrap();
        core.attach_repo(
            &ctx(),
            ws,
            repo_id(),
            self.repo.path().to_str().unwrap(),
            "Project".into(),
        )
        .unwrap();
        core.create_review(
            &ctx(),
            review_id(),
            ws,
            "Dependency".into(),
            NonEmpty::new(vec![ReviewTarget {
                repo_id: repo_id(),
                base: RefSpec::Commit { oid: base },
                head: RefSpec::Branch {
                    name: "review-head".into(),
                },
            }])
            .unwrap(),
        )
        .unwrap();
        core
    }
    fn move_head(&self, core: &Core, head: CommitOid) {
        self.repo
            .git(&["update-ref", "refs/heads/review-head", &head.to_string()])
            .unwrap();
        core.resolve_targets(&ctx(), review_id()).unwrap();
    }
}

#[test]
fn gitlink_add_update_remove_and_type_transitions_are_typed_and_render_without_blobs() {
    let h = History::new();
    for (base, head, expected) in [
        (h.base, h.added, SubmoduleChange::Added { new: h.old }),
        (
            h.added,
            h.updated,
            SubmoduleChange::Updated {
                old: h.old,
                new: h.new,
            },
        ),
        (
            h.updated,
            h.deleted,
            SubmoduleChange::Deleted { old: h.new },
        ),
        (
            h.blob,
            h.to_link,
            SubmoduleChange::BlobToSubmodule {
                old: nits_protocol::BlobEntry {
                    oid: h.blob_oid,
                    mode: nits_protocol::BlobMode::Regular,
                },
                new: h.new,
            },
        ),
        (
            h.to_link,
            h.to_blob,
            SubmoduleChange::SubmoduleToBlob {
                old: h.new,
                new: nits_protocol::BlobEntry {
                    oid: h.blob_oid,
                    mode: nits_protocol::BlobMode::Regular,
                },
            },
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let core = h.core(&DataDir::new(dir.path().join("data")), base, head);
        let files = core.files(review_id()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, path());
        assert_eq!(files[0].kind, ChangeKind::Submodule { change: expected });
        let (header, rendered) = core
            .file_render(
                review_id(),
                repo_id(),
                &path(),
                RenderOpts::default(),
                &DiffScope::All,
            )
            .unwrap();
        assert_eq!(header.content, RenderContent::Submodule);
        assert!(rendered.rows.is_empty());
        assert!(
            core.search(review_id(), "second", false, &DiffScope::All)
                .unwrap()
                .0
                .is_empty()
        );
    }
}

#[test]
fn gitlink_renames_keep_commit_identity_and_source_path() {
    let h = History::new();
    h.repo.git(&["read-tree", &h.updated.to_string()]).unwrap();
    h.repo
        .git(&["update-index", "--force-remove", "dep"])
        .unwrap();
    h.repo
        .git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},renamed-dep", h.new),
        ])
        .unwrap();
    let renamed = commit(&h.repo, "rename gitlink");
    let repo = Repo::open(h.repo.path()).unwrap();
    let base = repo.resolve(&RefSpec::Commit { oid: h.updated }).unwrap();
    let head = repo.resolve(&RefSpec::Commit { oid: renamed }).unwrap();
    let changes = repo.changed_files(base.tree, head.tree).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(
        changes[0].kind,
        ChangeKind::Submodule {
            change: SubmoduleChange::Renamed {
                from: path(),
                old: h.new,
                new: h.new,
            }
        }
    );
}

#[test]
fn viewed_gitlinks_track_pointer_changes_and_survive_reopening() {
    let h = History::new();
    let dir = tempfile::tempdir().unwrap();
    let data = DataDir::new(dir.path().join("data"));
    let core = h.core(&data, h.base, h.added);
    let mark = core
        .mark_viewed(&ctx(), review_id(), repo_id(), path())
        .unwrap();
    assert_eq!(mark.content, ViewedContent::Submodule { commit: h.old });
    assert_eq!(
        core.viewed_state(review_id(), repo_id(), &path()).unwrap(),
        ViewedState::Viewed
    );
    h.move_head(&core, h.updated);
    assert_eq!(
        core.viewed_state(review_id(), repo_id(), &path()).unwrap(),
        ViewedState::ChangedSinceViewed {
            marked: ViewedContent::Submodule { commit: h.old },
        }
    );
    core.mark_viewed(&ctx(), review_id(), repo_id(), path())
        .unwrap();
    h.move_head(&core, h.deleted);
    assert!(matches!(
        core.viewed_state(review_id(), repo_id(), &path()).unwrap(),
        ViewedState::ChangedSinceViewed { .. }
    ));
    let mark = core
        .mark_viewed(&ctx(), review_id(), repo_id(), path())
        .unwrap();
    assert_eq!(mark.content, ViewedContent::Missing);
    drop(core);
    let core = Core::open(&data).unwrap();
    assert_eq!(core.viewed_marks(review_id()).unwrap(), vec![mark]);
    h.move_head(&core, h.to_blob);
    assert!(matches!(
        core.viewed_state(review_id(), repo_id(), &path()).unwrap(),
        ViewedState::ChangedSinceViewed { .. }
    ));
}

#[test]
fn replacing_a_commented_blob_with_a_gitlink_marks_comments_outdated_and_can_restore_them() {
    let h = History::new();
    let dir = tempfile::tempdir().unwrap();
    let core = h.core(&DataDir::new(dir.path().join("data")), h.base, h.blob);
    let id = CommentId::from_parts(1, 4);
    core.add_comment(
        &ctx(),
        review_id(),
        id,
        CommentKind::Note,
        Anchor::File {
            repo_id: repo_id(),
            path: path(),
            blob_oid: h.blob_oid,
        },
        "Please explain this dependency".into(),
        None,
    )
    .unwrap();
    core.add_comment(
        &ctx(),
        review_id(),
        CommentId::from_parts(1, 5),
        CommentKind::Note,
        nits_review_core::comments::lines_anchor(
            repo_id(),
            path(),
            nits_protocol::Side::Head,
            h.blob_oid,
            1,
            1,
        )
        .unwrap(),
        "Explain this source line".into(),
        None,
    )
    .unwrap();
    h.move_head(&core, h.to_link);
    let comments = core.comments(review_id()).unwrap();
    assert_eq!(comments.len(), 2);
    assert!(
        comments
            .iter()
            .all(|c| matches!(c.state, CommentState::Outdated { .. }))
    );
    h.move_head(&core, h.to_blob);
    let comments = core.comments(review_id()).unwrap();
    assert_eq!(comments.len(), 2);
    assert!(comments.iter().all(|c| c.state == CommentState::Live));
}

#[test]
fn checked_out_submodule_pointer_is_captured_without_mutating_the_real_index() {
    let dependency = RepoBuilder::new()
        .commit("first", files!["lib.txt" => "first\n"])
        .tag("first")
        .commit("second", files!["lib.txt" => "second\n"])
        .build()
        .unwrap();
    let old: CommitOid = dependency.rev_parse("first").unwrap().parse().unwrap();
    let new: CommitOid = dependency.rev_parse("HEAD").unwrap().parse().unwrap();
    let superproject = RepoBuilder::new()
        .commit("base", files!["README" => "project\n"])
        .build()
        .unwrap();
    superproject
        .git(&[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--",
            dependency.path().to_str().unwrap(),
            "dep",
        ])
        .unwrap();
    superproject
        .git(&["-C", "dep", "checkout", "-q", &old.to_string()])
        .unwrap();
    superproject.git(&["add", "dep"]).unwrap();
    let base = commit(&superproject, "add dependency");
    let index = superproject.git(&["ls-files", "--stage"]).unwrap();
    superproject
        .git(&["-C", "dep", "checkout", "-q", &new.to_string()])
        .unwrap();
    let repo = Repo::open(superproject.path()).unwrap();
    let base_tree = repo.resolve(&RefSpec::Commit { oid: base }).unwrap().tree;
    let working = repo.resolve(&RefSpec::WorkingTree).unwrap();
    let changes = repo.changed_files(base_tree, working.tree).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path, path());
    assert_eq!(
        changes[0].kind,
        ChangeKind::Submodule {
            change: SubmoduleChange::Updated { old, new }
        }
    );
    assert_eq!(superproject.git(&["ls-files", "--stage"]).unwrap(), index);
    assert_eq!(superproject.rev_parse("HEAD").unwrap(), base.to_string());
}
