//! Global repository identity, failed attempts, restart and legacy repair.
use std::sync::{Arc, Barrier};

use nits_protocol::{
    Anchor, Author, ClientId, ClientSeq, CommentId, CommentKind, DiffScope, EnsureDirectoryReview,
    EventBody, NonEmpty, RefSpec, Repo, RepoId, RepoPath, ReviewId, ReviewTarget, Timestamp,
    WorkspaceId,
};
use nits_review_core::store::{NewEvent, Store};
use nits_review_core::{Core, CoreError, Ctx, DataDir};
use nits_test_support::{RepoBuilder, TestRepo, files};

fn ctx() -> Ctx {
    Ctx {
        author: Author::Human {
            name: "ada".into(),
            machine: "test".into(),
        },
        client_id: ClientId::from_parts(1, 1),
        client_seq: ClientSeq::new(1),
        now: Timestamp::from_millis(1_700_000_000_000),
    }
}
fn ws(n: u128) -> WorkspaceId {
    WorkspaceId::from_parts(1, n)
}
fn rid() -> RepoId {
    RepoId::from_parts(1, 9)
}
fn review(n: u128) -> ReviewId {
    ReviewId::from_parts(1, n)
}
fn repo(name: &str) -> TestRepo {
    RepoBuilder::new()
        .commit("initial", files![name => "source\n"])
        .build()
        .unwrap()
}
fn attach(core: &Core, workspace: WorkspaceId, path: &std::path::Path) -> Result<Repo, CoreError> {
    core.attach_repo(
        &ctx(),
        workspace,
        rid(),
        path.to_str().unwrap(),
        "repository".into(),
    )
}
fn paths(core: &Core) -> Vec<String> {
    core.tree_snapshot(rid(), &RefSpec::Head)
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.path.to_string())
        .collect()
}
fn make_review(core: &Core, workspace: WorkspaceId, id: ReviewId) {
    core.create_review(
        &ctx(),
        id,
        workspace,
        "review".into(),
        NonEmpty::singleton(ReviewTarget {
            repo_id: rid(),
            base: RefSpec::Head,
            head: RefSpec::WorkingTree,
        }),
    )
    .unwrap();
}
fn legacy_attach(data: &DataDir, workspace_id: WorkspaceId, path: &std::path::Path) {
    let context = ctx();
    Store::open(&data.state())
        .unwrap()
        .append(NewEvent {
            ts: context.now,
            author: context.author,
            client_id: context.client_id,
            client_seq: context.client_seq,
            body: EventBody::RepoAttached {
                workspace_id,
                repo: Repo {
                    id: rid(),
                    path: path.to_str().unwrap().into(),
                    display_name: "legacy".into(),
                },
            },
        })
        .unwrap();
}
fn setup() -> (tempfile::TempDir, DataDir, Core) {
    let dir = tempfile::tempdir().unwrap();
    let data = DataDir::new(dir.path());
    let core = Core::open(&data).unwrap();
    for n in 1..=3 {
        core.create_workspace(&ctx(), ws(n), format!("workspace {n}"))
            .unwrap();
    }
    (dir, data, core)
}

#[test]
fn different_checkout_and_invalid_paths_never_replace_a_warm_cache_or_append() {
    let (_dir, data, core) = setup();
    let alpha = repo("alpha.txt");
    let beta = repo("beta.txt");
    attach(&core, ws(1), alpha.path()).unwrap();
    assert_eq!(paths(&core), ["alpha.txt"]);
    let before = core.last_seq().unwrap();
    let workspaces = core.workspaces().unwrap();
    let error = attach(&core, ws(2), beta.path()).unwrap_err();
    assert!(error.to_string().contains("already identifies"));
    assert!(attach(&core, ws(2), &beta.path().join("missing")).is_err());
    let not_git = tempfile::tempdir().unwrap();
    assert!(attach(&core, ws(2), not_git.path()).is_err());
    assert_eq!(core.last_seq().unwrap(), before);
    assert_eq!(core.workspaces().unwrap(), workspaces);
    assert_eq!(paths(&core), ["alpha.txt"]);
    drop(core);
    let core = Core::open(&data).unwrap();
    assert_eq!(paths(&core), ["alpha.txt"]);
    assert!(core.workspace(ws(2)).unwrap().repos.is_empty());
}

#[test]
#[cfg(unix)]
fn same_checkout_memberships_canonicalize_aliases_and_survive_detach_and_restart() {
    let (dir, data, core) = setup();
    let alpha = repo("alpha.txt");
    let beta = repo("beta.txt");
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(alpha.path(), &alias).unwrap();
    let first = attach(&core, ws(1), &alias).unwrap();
    let second = attach(&core, ws(2), &alpha.path().join(".git")).unwrap();
    assert_eq!(first.path, second.path);
    assert_eq!(std::path::Path::new(&first.path), alpha.path());
    assert!(
        attach(&core, ws(1), alpha.path()).is_err(),
        "same-workspace duplicate ID remains invalid"
    );
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(beta.path(), &alias).unwrap();
    assert_eq!(
        paths(&core),
        ["alpha.txt"],
        "cached Git handle must not follow the supplied alias"
    );
    core.detach_repo(&ctx(), ws(1), rid()).unwrap();
    assert_eq!(paths(&core), ["alpha.txt"]);
    // A fresh ID for this checkout in another workspace retains existing semantics.
    core.attach_repo(
        &ctx(),
        ws(3),
        RepoId::from_parts(1, 10),
        alpha.path().to_str().unwrap(),
        "another membership".into(),
    )
    .unwrap();
    drop(core);
    let core = Core::open(&data).unwrap();
    assert_eq!(paths(&core), ["alpha.txt"]);
    assert!(core.workspace(ws(1)).unwrap().repos.is_empty());
    assert_eq!(core.workspace(ws(2)).unwrap().repos[0].path, first.path);
}

#[test]
fn linked_worktrees_cannot_share_one_repository_id() {
    let (dir, _data, core) = setup();
    let alpha = repo("alpha.txt");
    let linked = dir.path().join("linked");
    alpha
        .git(&["worktree", "add", "-b", "linked", linked.to_str().unwrap()])
        .unwrap();
    std::fs::write(linked.join("linked.txt"), "linked\n").unwrap();
    alpha
        .git(&["-C", linked.to_str().unwrap(), "add", "linked.txt"])
        .unwrap();
    alpha
        .git(&[
            "-C",
            linked.to_str().unwrap(),
            "commit",
            "-qm",
            "linked content",
        ])
        .unwrap();
    attach(&core, ws(1), alpha.path()).unwrap();
    let before = core.last_seq().unwrap();
    assert!(attach(&core, ws(2), &linked).is_err());
    assert_eq!(core.last_seq().unwrap(), before);
    assert_eq!(paths(&core), ["alpha.txt"]);
}

#[test]
fn bootstrap_preflights_ownership_before_creating_a_workspace() {
    let (_dir, _data, core) = setup();
    let alpha = repo("alpha.txt");
    let beta = repo("beta.txt");
    attach(&core, ws(1), alpha.path()).unwrap();
    let before = core.last_seq().unwrap();
    assert!(
        core.ensure_directory_review(
            &ctx(),
            EnsureDirectoryReview {
                workspace_id: ws(4),
                repo_id: rid(),
                review_id: review(4),
                path: beta.path().to_str().unwrap().into(),
                base: None,
                head: None,
            }
        )
        .is_err()
    );
    assert_eq!(core.last_seq().unwrap(), before);
    assert!(core.workspace(ws(4)).is_err());
    assert!(core.review(review(4)).is_err());
    assert_eq!(paths(&core), ["alpha.txt"]);
    let nested = alpha.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    let reused = core
        .ensure_directory_review(
            &ctx(),
            EnsureDirectoryReview {
                workspace_id: ws(4),
                repo_id: rid(),
                review_id: review(4),
                path: nested.to_str().unwrap().into(),
                base: None,
                head: None,
            },
        )
        .unwrap();
    assert_eq!(reused.workspace_id, ws(1));
    assert!(core.workspace(ws(4)).is_err());
}

#[test]
fn legacy_ambiguity_is_explicit_and_detach_repairs_without_redirecting_old_reviews() {
    // Exercise both store traversal orders: neither checkout wins by sorting.
    for owner in [ws(1), ws(2)] {
        let (_dir, data, core) = setup();
        let conflicting = if owner == ws(1) { ws(2) } else { ws(1) };
        let alpha = repo("alpha.txt");
        let beta = repo("beta.txt");
        attach(&core, owner, alpha.path()).unwrap();
        attach(&core, conflicting, alpha.path()).unwrap();
        make_review(&core, owner, review(1));
        make_review(&core, conflicting, review(2));
        core.add_comment(
            &ctx(),
            review(2),
            CommentId::from_parts(1, 1),
            CommentKind::Note,
            Anchor::Review,
            "retained history".into(),
            None,
        )
        .unwrap();
        let blob = alpha
            .git(&["rev-parse", "HEAD:alpha.txt"])
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        core.add_comment(
            &ctx(),
            review(2),
            CommentId::from_parts(1, 2),
            CommentKind::Suggestion {
                patch: "--- a/alpha.txt\n+++ b/alpha.txt\n@@ -1 +1 @@\n-source\n+patched\n".into(),
            },
            Anchor::File {
                repo_id: rid(),
                path: RepoPath::new("alpha.txt").unwrap(),
                blob_oid: blob,
            },
            "suggestion retained across repair".into(),
            None,
        )
        .unwrap();
        core.detach_repo(&ctx(), conflicting, rid()).unwrap();
        drop(core);
        legacy_attach(&data, conflicting, beta.path());
        let core = Core::open(&data).unwrap();
        let before = core.last_seq().unwrap();
        assert_eq!(core.workspaces().unwrap().len(), 3);
        assert_eq!(core.comments(review(2)).unwrap().len(), 2);
        let error = core
            .tree_snapshot(rid(), &RefSpec::Head)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("conflicting checkout memberships"),
            "{error}"
        );
        assert!(error.contains(&owner.to_string()) && error.contains(&conflicting.to_string()));
        assert!(core.resolve_targets(&ctx(), review(1)).is_err());
        assert!(core.files(review(2)).is_err());
        assert!(attach(&core, ws(3), alpha.path()).is_err());
        assert_eq!(core.last_seq().unwrap(), before);
        core.detach_repo(&ctx(), conflicting, rid()).unwrap();
        assert_eq!(paths(&core), ["alpha.txt"]);
        assert!(core.files(review(1)).unwrap().is_empty());
        let detached = core.review_snapshot(review(2)).unwrap();
        let target = detached.resolved.as_ref().unwrap().first();
        assert!(
            core.review_tree_snapshot(review(2), rid(), &target.head)
                .is_err()
        );
        assert!(core.files_scoped(review(2), &DiffScope::All).is_err());
        assert!(core.resolve_targets(&ctx(), review(2)).is_err());
        assert!(core.commits(review(2), rid()).is_err());
        assert!(
            core.mark_viewed(
                &ctx(),
                review(2),
                rid(),
                RepoPath::new("alpha.txt").unwrap()
            )
            .is_err()
        );
        assert_eq!(core.comments(review(2)).unwrap().len(), 2);
        let error = core
            .apply_suggestion(&ctx(), review(2), CommentId::from_parts(1, 2))
            .unwrap_err();
        assert!(error.to_string().contains("not attached"), "{error}");
        assert_eq!(
            std::fs::read_to_string(alpha.path().join("alpha.txt")).unwrap(),
            "source\n"
        );
        drop(core);
        let core = Core::open(&data).unwrap();
        assert_eq!(paths(&core), ["alpha.txt"]);
        assert!(core.files(review(2)).is_err());
        core.delete_review(&ctx(), review(2)).unwrap();
    }
}

#[test]
fn unavailable_membership_is_repairable_without_blocking_other_repository_ids() {
    let (dir, data, core) = setup();
    let alpha = repo("alpha.txt");
    attach(&core, ws(1), alpha.path()).unwrap();
    let other = RepoId::from_parts(1, 10);
    core.attach_repo(
        &ctx(),
        ws(3),
        other,
        alpha.path().to_str().unwrap(),
        "healthy".into(),
    )
    .unwrap();
    drop(core);
    legacy_attach(&data, ws(2), &dir.path().join("missing"));
    let core = Core::open(&data).unwrap();
    assert!(core.tree_snapshot(rid(), &RefSpec::Head).is_err());
    assert!(core.tree_snapshot(other, &RefSpec::Head).is_ok());
    core.detach_repo(&ctx(), ws(2), rid()).unwrap();
    assert_eq!(paths(&core), ["alpha.txt"]);
}

#[test]
fn concurrent_core_attachment_has_one_owner_or_two_same_checkout_memberships() {
    let alpha = repo("alpha.txt");
    let beta = repo("beta.txt");
    for paths_to_attach in [[alpha.path(), beta.path()], [alpha.path(), alpha.path()]] {
        let (_dir, _data, core) = setup();
        let core = Arc::new(core);
        let ready = Arc::new(Barrier::new(2));
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = paths_to_attach
                .into_iter()
                .enumerate()
                .map(|(index, path)| {
                    let core = Arc::clone(&core);
                    let ready = Arc::clone(&ready);
                    scope.spawn(move || {
                        ready.wait();
                        attach(&core, ws(index as u128 + 1), path)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        let accepted = results.iter().filter(|result| result.is_ok()).count();
        assert_eq!(
            accepted,
            if paths_to_attach[0] == paths_to_attach[1] {
                2
            } else {
                1
            }
        );
        let memberships = core
            .workspaces()
            .unwrap()
            .into_iter()
            .flat_map(|workspace| workspace.repos)
            .collect::<Vec<_>>();
        assert_eq!(memberships.len(), accepted);
        let actual = core.tree_snapshot(rid(), &RefSpec::Head).unwrap();
        let expected = if memberships[0].path == alpha.path().to_str().unwrap() {
            "alpha.txt"
        } else {
            "beta.txt"
        };
        assert_eq!(actual.entries[0].path.as_str(), expected);
    }
}
