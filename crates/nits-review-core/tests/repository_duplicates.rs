//! Workspace-local canonical checkout uniqueness and repair of legacy duplicates.
use std::path::Path;
use std::sync::{Arc, Barrier};

use nits_protocol::{
    Anchor, Author, ClientId, ClientSeq, CommentId, CommentKind, EnsureDirectoryReview, EventBody,
    NonEmpty, RefSpec, Repo, RepoId, ReviewId, ReviewTarget, Timestamp, WorkspaceId,
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
fn rid(n: u128) -> RepoId {
    RepoId::from_parts(1, n)
}
fn repo(file: &str) -> TestRepo {
    RepoBuilder::new()
        .commit("initial", files![file => "source\n"])
        .build()
        .unwrap()
}
fn attach(core: &Core, workspace: WorkspaceId, id: RepoId, path: &Path) -> Result<Repo, CoreError> {
    core.attach_repo(&ctx(), workspace, id, path.to_str().unwrap(), "repo".into())
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
fn legacy_attach(data: &DataDir, id: RepoId, path: &Path) {
    let context = ctx();
    Store::open(&data.state())
        .unwrap()
        .append(NewEvent {
            ts: context.now,
            author: context.author,
            client_id: context.client_id,
            client_seq: context.client_seq,
            body: EventBody::RepoAttached {
                workspace_id: ws(1),
                repo: Repo {
                    id,
                    path: path.to_str().unwrap().into(),
                    display_name: "legacy".into(),
                },
            },
        })
        .unwrap();
}

#[test]
fn repeated_checkout_aliases_reject_without_events_or_cache_changes_across_restart() {
    let (dir, data, core) = setup();
    let alpha = repo("alpha.txt");
    attach(&core, ws(1), rid(1), alpha.path()).unwrap();
    let tree = core.tree_snapshot(rid(1), &RefSpec::Head).unwrap();
    let before = core.last_seq().unwrap();
    let mut paths = vec![alpha.path().to_path_buf(), alpha.path().join(".git")];
    #[cfg(unix)]
    {
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(alpha.path(), &alias).unwrap();
        paths.push(alias);
    }
    for (index, path) in paths.iter().enumerate() {
        let id = rid(index as u128 + 2);
        let error = attach(&core, ws(1), id, path).unwrap_err().to_string();
        assert!(
            error.contains(&rid(1).to_string()) && error.contains(&ws(1).to_string()),
            "{error}"
        );
        assert!(error.contains("another workspace is allowed"), "{error}");
        assert_eq!(core.last_seq().unwrap(), before);
        assert!(core.tree_snapshot(id, &RefSpec::Head).is_err());
        assert_eq!(core.tree_snapshot(rid(1), &RefSpec::Head).unwrap(), tree);
    }
    assert_eq!(core.workspace(ws(1)).unwrap().repos.len(), 1);
    drop(core);
    let core = Core::open(&data).unwrap();
    assert!(attach(&core, ws(1), rid(8), alpha.path()).is_err());
    assert_eq!(core.last_seq().unwrap(), before);
    assert_eq!(core.tree_snapshot(rid(1), &RefSpec::Head).unwrap(), tree);
}

#[test]
fn shared_workspaces_and_distinct_linked_checkouts_remain_supported() {
    let (dir, _data, core) = setup();
    let alpha = repo("alpha.txt");
    attach(&core, ws(1), rid(1), alpha.path()).unwrap();
    attach(&core, ws(2), rid(1), alpha.path()).unwrap();
    attach(&core, ws(3), rid(2), alpha.path()).unwrap();
    let linked = dir.path().join("linked");
    alpha
        .git(&["worktree", "add", "-b", "linked", linked.to_str().unwrap()])
        .unwrap();
    std::fs::write(linked.join("linked.txt"), "linked\n").unwrap();
    attach(&core, ws(1), rid(3), &linked).unwrap();
    assert_eq!(core.workspace(ws(1)).unwrap().repos.len(), 2);
    assert_ne!(
        core.working_tree(rid(1)).unwrap().tree,
        core.working_tree(rid(3)).unwrap().tree
    );
    core.detach_repo(&ctx(), ws(2), rid(1)).unwrap();
    assert!(core.tree_snapshot(rid(1), &RefSpec::Head).is_ok());
}

#[test]
fn concurrent_fresh_ids_commit_one_attachment_per_workspace_checkout() {
    let alpha = repo("alpha.txt");
    let beta = repo("beta.txt");
    for paths in [[alpha.path(), alpha.path()], [alpha.path(), beta.path()]] {
        let (_dir, _data, core) = setup();
        let before = core.last_seq().unwrap();
        let core = Arc::new(core);
        let ready = Arc::new(Barrier::new(2));
        let results = std::thread::scope(|scope| {
            paths
                .into_iter()
                .enumerate()
                .map(|(index, path)| {
                    let core = Arc::clone(&core);
                    let ready = Arc::clone(&ready);
                    scope.spawn(move || {
                        ready.wait();
                        attach(&core, ws(1), rid(index as u128 + 1), path)
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        let expected = if paths[0] == paths[1] { 1 } else { 2 };
        assert_eq!(
            results.iter().filter(|result| result.is_ok()).count(),
            expected
        );
        assert_eq!(core.workspace(ws(1)).unwrap().repos.len(), expected);
        assert_eq!(core.events_after(before).unwrap().len(), expected);
    }
}

#[test]
fn legacy_duplicate_aliases_can_be_detached_without_losing_review_history() {
    let (_dir, data, core) = setup();
    let alpha = repo("alpha.txt");
    attach(&core, ws(1), rid(1), alpha.path()).unwrap();
    drop(core);
    legacy_attach(&data, rid(2), &alpha.path().join(".git"));
    let core = Core::open(&data).unwrap();
    let review = ReviewId::from_parts(1, 1);
    core.create_review(
        &ctx(),
        review,
        ws(1),
        "legacy review".into(),
        NonEmpty::singleton(ReviewTarget {
            repo_id: rid(2),
            base: RefSpec::Head,
            head: RefSpec::WorkingTree,
        }),
    )
    .unwrap();
    core.add_comment(
        &ctx(),
        review,
        CommentId::from_parts(1, 1),
        CommentKind::Note,
        Anchor::Review,
        "preserved".into(),
        None,
    )
    .unwrap();
    assert_eq!(core.workspace(ws(1)).unwrap().repos.len(), 2);
    let before = core.last_seq().unwrap();
    assert!(attach(&core, ws(1), rid(3), alpha.path()).is_err());
    assert_eq!(core.last_seq().unwrap(), before);
    core.detach_repo(&ctx(), ws(1), rid(2)).unwrap();
    assert_eq!(core.workspace(ws(1)).unwrap().repos[0].id, rid(1));
    assert!(core.tree_snapshot(rid(1), &RefSpec::Head).is_ok());
    assert_eq!(core.comments(review).unwrap().len(), 1);
    assert!(core.review(review).is_ok());
    assert!(core.files(review).is_err());
    assert_eq!(
        std::fs::read_to_string(alpha.path().join("alpha.txt")).unwrap(),
        "source\n"
    );
    core.detach_repo(&ctx(), ws(1), rid(1)).unwrap();
    attach(&core, ws(1), rid(3), alpha.path()).unwrap();
}

#[test]
fn unavailable_legacy_attachment_does_not_block_a_different_checkout_or_repair() {
    let (dir, data, core) = setup();
    let alpha = repo("alpha.txt");
    drop(core);
    legacy_attach(&data, rid(1), &dir.path().join("missing"));
    let core = Core::open(&data).unwrap();
    attach(&core, ws(1), rid(2), alpha.path()).unwrap();
    core.detach_repo(&ctx(), ws(1), rid(1)).unwrap();
    assert_eq!(core.workspace(ws(1)).unwrap().repos[0].id, rid(2));
    assert!(core.tree_snapshot(rid(2), &RefSpec::Head).is_ok());
}

#[test]
fn directory_lookup_detects_legacy_aliases_and_reuses_either_survivor_after_restart() {
    let alpha = repo("alpha.txt");
    let aliases = tempfile::tempdir().unwrap();
    let mut paths = vec![alpha.path().join(".git")];
    #[cfg(unix)]
    {
        let alias = aliases.path().join("alias");
        std::os::unix::fs::symlink(alpha.path(), &alias).unwrap();
        paths.push(alias);
    }
    for path in paths {
        for removed in [rid(1), rid(2)] {
            let (_dir, data, core) = setup();
            attach(&core, ws(1), rid(1), alpha.path()).unwrap();
            drop(core);
            legacy_attach(&data, rid(2), &path);
            let core = Core::open(&data).unwrap();
            let options = EnsureDirectoryReview {
                workspace_id: ws(9),
                repo_id: rid(9),
                review_id: ReviewId::from_parts(1, 9),
                path: alpha.path().to_str().unwrap().into(),
                base: None,
                head: Some(RefSpec::WorkingTree),
            };
            let before = core.last_seq().unwrap();
            let error = core
                .ensure_directory_review(&ctx(), options.clone())
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("multiple repository attachments")
            );
            assert_eq!(core.last_seq().unwrap(), before);
            core.detach_repo(&ctx(), ws(1), removed).unwrap();
            drop(core);
            let core = Core::open(&data).unwrap();
            let opened = core
                .ensure_directory_review(&ctx(), options.clone())
                .unwrap();
            let survivor = if removed == rid(1) { rid(2) } else { rid(1) };
            assert_eq!((opened.workspace_id, opened.repo_id), (ws(1), survivor));
            assert_eq!(core.workspaces().unwrap().len(), 3);
            assert_eq!(
                core.ensure_directory_review(&ctx(), options.clone())
                    .unwrap()
                    .review_id,
                opened.review_id
            );
            attach(&core, ws(2), rid(3), alpha.path()).unwrap();
            let before = core.last_seq().unwrap();
            let error = core.ensure_directory_review(&ctx(), options).unwrap_err();
            assert!(error.to_string().contains("several workspaces"));
            assert_eq!(core.last_seq().unwrap(), before);
        }
    }
}

#[cfg(unix)]
#[test]
fn configured_symlink_workdir_keeps_canonical_ownership_and_directory_reuse() {
    let (dir, _data, core) = setup();
    let alpha = repo("alpha.txt");
    let alias = dir.path().join("configured-workdir");
    std::os::unix::fs::symlink(alpha.path(), &alias).unwrap();
    alpha
        .git(&["config", "core.worktree", alias.to_str().unwrap()])
        .unwrap();
    let attached = attach(&core, ws(1), rid(1), alpha.path()).unwrap();
    let canonical = std::fs::canonicalize(alpha.path()).unwrap();
    assert_eq!(Path::new(&attached.path), canonical);
    assert_eq!(core.repo_checkout_path(rid(1)).unwrap(), canonical);
    let before = core.last_seq().unwrap();
    assert!(attach(&core, ws(1), rid(2), &alias).is_err());
    assert_eq!(core.last_seq().unwrap(), before);
    let options = EnsureDirectoryReview {
        workspace_id: ws(9),
        repo_id: rid(9),
        review_id: ReviewId::from_parts(1, 9),
        path: alpha.path().to_str().unwrap().into(),
        base: None,
        head: Some(RefSpec::WorkingTree),
    };
    let opened = core.ensure_directory_review(&ctx(), options).unwrap();
    assert_eq!((opened.workspace_id, opened.repo_id), (ws(1), rid(1)));
    assert_eq!(core.workspaces().unwrap().len(), 3);
}
