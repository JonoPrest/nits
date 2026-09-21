//! Duplicate attachment rejection and explicit membership repair through the CLI.
use super::*;
use nits_protocol::{
    Anchor, Author, ClientId, ClientSeq, CommentId, CommentKind, Event, EventBody, NonEmpty,
    RefSpec, Repo, RepoId, ReviewId, ReviewTarget, Timestamp, WorkspaceId,
};
use nits_review_core::store::{NewEvent, Store};
use nits_review_core::{Core, Ctx};
use nitsd::client::{Client, Identity};

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
    seed_duplicates_at(data, repo, repo.path());
}
fn seed_duplicates_at(data: &DataDir, repo: &TestRepo, alias: &std::path::Path) {
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
            (repo_id(2), alias.to_path_buf()),
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

#[test]
fn legacy_alias_inference_rejects_duplicates_and_reuses_the_survivor_after_detach() {
    for symlink in [false, cfg!(unix)] {
        let h = start_seeded(|data, repo| {
            let mut alias = repo.path().join(".git");
            if symlink {
                alias = data.root.join("checkout-alias");
                #[cfg(unix)]
                std::os::unix::fs::symlink(repo.path(), &alias).unwrap();
            }
            seed_duplicates_at(data, repo, &alias);
        });
        let before = events(&h);
        for args in [vec!["review", "list"], vec![".", "--headless"]] {
            h.nits()
                .current_dir(h.repo.path())
                .args(args)
                .assert()
                .failure()
                .stderr(predicate::str::contains("multiple repository attachments"));
        }
        assert_eq!(events(&h), before);
        h.out(&[
            "workspace",
            "detach",
            &workspace().to_string(),
            &repo_id(1).to_string(),
        ]);
        h.nits()
            .current_dir(h.repo.path())
            .args(["review", "list"])
            .assert()
            .success()
            .stdout(predicate::str::contains("preserved legacy review"));
        let before = h.out(&["--json", "workspace", "list"]);
        let result = h
            .nits()
            .current_dir(h.repo.path())
            .args(["--json", ".", "--headless"])
            .assert()
            .success();
        let opened: serde_json::Value =
            serde_json::from_slice(&result.get_output().stdout).unwrap();
        assert_eq!(opened["workspace_id"], workspace().to_string());
        assert_eq!(opened["repo_id"], repo_id(2).to_string());
        assert_eq!(opened["review_id"], review_id().to_string());
        assert_eq!(opened["outcome"], "Reused");
        assert_eq!(h.out(&["--json", "workspace", "list"]), before);
    }
}

#[test]
fn inference_uses_checkout_depth_instead_of_legacy_alias_path_depth() {
    let h = start_seeded(|data, repo| seed_duplicates_at(data, repo, &repo.path().join(".git")));
    h.out(&[
        "workspace",
        "detach",
        &workspace().to_string(),
        &repo_id(1).to_string(),
    ]);
    let nested = h.repo.path().join("nested");
    h.repo
        .git(&["worktree", "add", "-b", "nested", nested.to_str().unwrap()])
        .unwrap();
    let nested_id = h.out(&[
        "workspace",
        "attach",
        &workspace().to_string(),
        nested.to_str().unwrap(),
    ]);
    let located = h.rt.block_on(async {
        let client = Client::connect_unix(
            &h.socket,
            Identity {
                client_id: ClientId::from_parts(5, 99),
                client: BuildInfo {
                    name: "nested-inference".into(),
                    version: "test".into(),
                },
                author: context().author,
            },
        )
        .await
        .unwrap();
        nitsd::ops::Ops::new(client).locate(&nested).await.unwrap()
    });
    assert_eq!(located.repo.id.to_string(), nested_id);
}

#[test]
fn remote_inference_does_not_require_the_advertised_checkout_to_be_local_git() {
    let h = start_seeded(|data, repo| {
        let ctx = context();
        let core = Core::open(data).unwrap();
        core.create_workspace(&ctx, workspace(), "remote workspace".into())
            .unwrap();
        // An unrelated accessible checkout must not become a fallback match.
        core.attach_repo(
            &ctx,
            workspace(),
            repo_id(2),
            repo.path().to_str().unwrap(),
            "other".into(),
        )
        .unwrap();
        drop(core);
        let path = data.root.join("remote-checkout");
        std::fs::create_dir_all(path.join("nested")).unwrap();
        // Simulate daemon metadata for a path that is not a Git repo on this client.
        Store::open(&data.state())
            .unwrap()
            .append(NewEvent {
                ts: ctx.now,
                author: ctx.author,
                client_id: ctx.client_id,
                client_seq: ctx.client_seq,
                body: EventBody::RepoAttached {
                    workspace_id: workspace(),
                    repo: Repo {
                        id: repo_id(1),
                        path: path.to_str().unwrap().into(),
                        display_name: "remote".into(),
                    },
                },
            })
            .unwrap();
    });
    let path = h.dir.path().join("remote-checkout/nested");
    let located = h.rt.block_on(async {
        let client = Client::connect_ws(
            &h.ws_url,
            Identity {
                client_id: ClientId::from_parts(5, 99),
                client: BuildInfo {
                    name: "remote-inference".into(),
                    version: "test".into(),
                },
                author: context().author,
            },
        )
        .await
        .unwrap();
        nitsd::ops::Ops::new(client).locate(&path).await.unwrap()
    });
    assert_eq!(located.workspace.id, workspace());
    assert_eq!(located.repo.id, repo_id(1));
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
    for repo in [&duplicate, &missing] {
        let receipt = h.out(&["workspace", "detach", &ws, repo]);
        assert!(receipt.contains(&format!("repo detached {repo} from workspace {ws}")));
        assert!(receipt.contains("checkout and history kept"));
    }
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
