//! Duplicate attachment rejection and explicit membership repair through the CLI.
use super::*;
use nits_protocol::{
    Anchor, Author, ClientId, ClientSeq, CommentId, CommentKind, Event, EventBody, NonEmpty,
    RefSpec, Repo, RepoId, ReviewId, ReviewTarget, Timestamp, WorkspaceId,
};
use nits_review_core::store::{NewEvent, Store};
use nits_review_core::{Core, Ctx};

fn workspace() -> WorkspaceId {
    WorkspaceId::from_parts(5, 1)
}
fn repo_id(n: u128) -> RepoId {
    RepoId::from_parts(5, n)
}
fn review_id() -> ReviewId {
    ReviewId::from_parts(5, 1)
}
fn context() -> Ctx {
    Ctx {
        author: Author::Human {
            name: "ada".into(),
            machine: "test".into(),
        },
        client_id: ClientId::from_parts(5, 1),
        client_seq: ClientSeq::new(1),
        now: Timestamp::from_millis(1_700_000_000_000),
    }
}

fn seed_duplicates(data: &DataDir, repo: &TestRepo) {
    let ctx = context();
    let core = Core::open(data).unwrap();
    core.create_workspace(&ctx, workspace(), "legacy workspace".into())
        .unwrap();
    core.attach_repo(
        &ctx,
        workspace(),
        repo_id(1),
        repo.path().to_str().unwrap(),
        "original".into(),
    )
    .unwrap();
    drop(core);
    {
        let store = Store::open(&data.state()).unwrap();
        for (id, path) in [
            (repo_id(2), repo.path().to_path_buf()),
            (repo_id(3), repo.path().join("missing-checkout")),
        ] {
            store
                .append(NewEvent {
                    ts: ctx.now,
                    author: ctx.author.clone(),
                    client_id: ctx.client_id,
                    client_seq: ctx.client_seq,
                    body: EventBody::RepoAttached {
                        workspace_id: workspace(),
                        repo: Repo {
                            id,
                            path: path.to_str().unwrap().into(),
                            display_name: "legacy".into(),
                        },
                    },
                })
                .unwrap();
        }
    }
    let core = Core::open(data).unwrap();
    core.create_review(
        &ctx,
        review_id(),
        workspace(),
        "preserved legacy review".into(),
        NonEmpty::singleton(ReviewTarget {
            repo_id: repo_id(2),
            base: RefSpec::Head,
            head: RefSpec::WorkingTree,
        }),
    )
    .unwrap();
    core.add_comment(
        &ctx,
        review_id(),
        CommentId::from_parts(5, 1),
        CommentKind::Note,
        Anchor::Review,
        "preserved history".into(),
        None,
    )
    .unwrap();
}
fn events(h: &Harness) -> String {
    h.out(&["--json", "events", "--since", "0"])
}
fn reject_alias(h: &Harness, workspace: &str, existing: &str, path: &str) {
    let before = events(h);
    let workspaces = h.out(&["--json", "workspace", "list"]);
    h.nits()
        .current_dir(h.repo.path())
        .args(["workspace", "attach", workspace, path])
        .assert()
        .failure()
        .stderr(predicate::str::contains(existing))
        .stderr(predicate::str::contains(workspace));
    assert_eq!(events(h), before);
    assert_eq!(h.out(&["--json", "workspace", "list"]), workspaces);
}

#[test]
fn repeated_cli_attach_rejects_aliases_and_detach_allows_a_fresh_attachment() {
    let h = start();
    let ws = h.out(&["workspace", "add", "one"]);
    let path = h.repo.path().to_str().unwrap();
    let first = h.out(&["workspace", "attach", &ws, path]);
    for alias in [".", ".git", path] {
        reject_alias(&h, &ws, &first, alias);
    }
    #[cfg(unix)]
    {
        let alias = h.dir.path().join("checkout-alias");
        std::os::unix::fs::symlink(h.repo.path(), &alias).unwrap();
        reject_alias(&h, &ws, &first, alias.to_str().unwrap());
    }
    let second_workspace = h.out(&["workspace", "add", "shared"]);
    let other = h.out(&["workspace", "attach", &second_workspace, path]);
    assert_ne!(first, other);
    let linked = h.dir.path().join("linked");
    h.repo
        .git(&["worktree", "add", "-b", "linked", linked.to_str().unwrap()])
        .unwrap();
    let linked_id = h.out(&["workspace", "attach", &ws, linked.to_str().unwrap()]);
    let detached: Event =
        serde_json::from_str(&h.out(&["--json", "workspace", "detach", &ws, &first])).unwrap();
    assert!(
        matches!(detached.body, EventBody::RepoDetached { workspace_id, repo_id } if workspace_id.to_string() == ws && repo_id.to_string() == first)
    );
    let list = h.out(&["workspace", "list"]);
    assert!(list.contains(&other) && list.contains(&linked_id));
    assert!(!list.contains(&first));
    assert!(h.repo.path().join("a.rs").exists());
    let fresh = h.out(&["workspace", "attach", &ws, path]);
    assert_ne!(fresh, first);
}

#[test]
fn cli_detach_repairs_legacy_duplicates_and_missing_checkouts_without_losing_history() {
    let h = start_seeded(seed_duplicates);
    let ws = workspace().to_string();
    let duplicate = repo_id(2).to_string();
    let missing = repo_id(3).to_string();
    let list = h.out(&["workspace", "list"]);
    assert!(list.contains(&duplicate) && list.contains(&missing));
    h.nits()
        .current_dir(h.repo.path())
        .args(["review", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("multiple repository attachments"))
        .stderr(predicate::str::contains("workspace detach"));
    let before = std::fs::read(h.repo.path().join("a.rs")).unwrap();
    let before_events = events(&h);
    h.nits()
        .current_dir(h.repo.path())
        .args([".", "--headless"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("multiple repository attachments"));
    assert_eq!(events(&h), before_events);
    assert_eq!(
        h.out(&["workspace", "detach", &ws, &duplicate]),
        format!("detached {duplicate}")
    );
    assert_eq!(
        h.out(&["workspace", "detach", &ws, &missing]),
        format!("detached {missing}")
    );
    h.nits()
        .current_dir(h.repo.path())
        .args(["review", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("preserved legacy review"));
    assert!(
        h.out(&["comment", "list", &review_id().to_string()])
            .contains("preserved history")
    );
    h.nits()
        .args(["show", &review_id().to_string(), "a.rs"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not attached"));
    assert_eq!(std::fs::read(h.repo.path().join("a.rs")).unwrap(), before);
    assert!(
        h.out(&["workspace", "list"])
            .contains(&repo_id(1).to_string())
    );
}

#[test]
fn cli_detach_rejects_unknown_membership_and_invalid_ids_without_events() {
    let h = start();
    let ws = h.out(&["workspace", "add", "one"]);
    let before = events(&h);
    h.nits()
        .args(["workspace", "detach", &ws, &repo_id(99).to_string()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("NotFound"));
    h.nits()
        .args(["workspace", "detach", &ws, "invalid-id"])
        .assert()
        .code(2);
    assert_eq!(events(&h), before);
    h.nits()
        .args(["workspace", "detach", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("review history"));
}
