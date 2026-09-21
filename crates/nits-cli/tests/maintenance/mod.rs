//! Maintenance commands exercise the real daemon's mutations and permissions.

use super::{Harness, start};
use assert_cmd::Command;
use nits_protocol::{Event, EventBody, Mutation, ReviewStatus};
use predicates::prelude::*;
use serde_json::Value;

fn setup() -> (Harness, String, String, String) {
    let h = start();
    let workspace = h.out(&["workspace", "add", "before"]);
    let repo = h.out(&[
        "workspace",
        "attach",
        &workspace,
        h.repo.path().to_str().unwrap(),
    ]);
    let review = h.out(&[
        "--workspace",
        &workspace,
        "review",
        "create",
        "--base",
        "main",
        "--head",
        "feature",
        "--title",
        "original",
    ]);
    (h, workspace, repo, review)
}

fn snapshot(h: &Harness, review: &str) -> Value {
    serde_json::from_str(&h.out(&["--json", "review", "show", review])).unwrap()
}

fn receipt(h: &Harness, args: &[&str]) -> Event {
    let mut command = vec!["--json"];
    command.extend_from_slice(args);
    serde_json::from_str(&h.out(&command)).unwrap()
}

fn events(h: &Harness) -> Vec<Event> {
    h.out(&["--json", "events", "--since", "0"])
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn review_metadata_and_base_preserve_identity_other_fields_and_json_receipts() {
    let (h, workspace, repo, review) = setup();
    let original = snapshot(&h, &review);
    let event = receipt(&h, &["review", "rename", &review, "renamed"]);
    assert!(
        matches!(event.body, EventBody::ReviewUpdated { review_id, title, status: ReviewStatus::Open }
        if review_id.to_string() == review && title == "renamed")
    );
    let event = receipt(&h, &["review", "archive", &review]);
    assert!(
        matches!(event.body, EventBody::ReviewUpdated { title, status: ReviewStatus::Archived, .. }
        if title == "renamed")
    );
    h.nits()
        .args(["review", "rename", &review, "archived title"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains(&review)
                .and(predicate::str::contains("archived title [Archived]")),
        );
    assert_eq!(snapshot(&h, &review)["review"]["status"], "Archived");
    let event = receipt(&h, &["review", "reopen", &review]);
    assert!(
        matches!(event.body, EventBody::ReviewUpdated { title, status: ReviewStatus::Open, .. }
        if title == "archived title")
    );
    let event = receipt(
        &h,
        &["review", "set-base", &review, "HEAD", "--repo", &repo],
    );
    assert!(
        matches!(event.body, EventBody::ReviewTargetUpdated { review_id, target }
        if review_id.to_string() == review && target.repo_id.to_string() == repo
            && target.base == nits_protocol::RefSpec::Head
            && target.head == nits_protocol::RefSpec::Branch { name: "feature".into() })
    );
    h.repo.git(&["tag", "base-tag", "main"]).unwrap();
    h.repo
        .git(&["branch", "--set-upstream-to", "main", "feature"])
        .unwrap();
    let commit = h.repo.git(&["rev-parse", "main"]).unwrap();
    for reference in ["main", "tag:base-tag", commit.trim(), "upstream"] {
        let event = receipt(
            &h,
            &["review", "set-base", &review, reference, "--repo", &repo],
        );
        let EventBody::ReviewTargetUpdated { target, .. } = event.body else {
            panic!("target receipt")
        };
        assert_eq!(
            target.head,
            nits_protocol::RefSpec::Branch {
                name: "feature".into()
            }
        );
        let current = snapshot(&h, &review);
        assert_eq!(
            current["resolved"][0]["base"]["source"]["oid"],
            commit.trim()
        );
    }
    let current = snapshot(&h, &review);
    for field in ["id", "workspace_id", "created"] {
        assert_eq!(current["review"][field], original["review"][field]);
    }
    assert_eq!(current["review"]["title"], "archived title");
    assert_eq!(current["review"]["status"], "Open");
    let reviews: Value =
        serde_json::from_str(&h.out(&["review", "list", "--workspace", &workspace, "--json"]))
            .unwrap();
    assert_eq!(reviews[0], current["review"]);
}

#[test]
fn rejected_base_and_reopen_leave_targets_status_and_event_history_unchanged() {
    let (h, workspace, repo, review) = setup();
    let other = nits_test_support::RepoBuilder::new()
        .commit("other", nits_test_support::files!["b.rs" => "other\n"])
        .build()
        .unwrap();
    let outsider = h.out(&[
        "workspace",
        "attach",
        &workspace,
        other.path().to_str().unwrap(),
    ]);
    let before = snapshot(&h, &review);
    let history = events(&h);
    for (reference, target) in [
        ("missing", repo.as_str()),
        ("worktree", repo.as_str()),
        ("wt", repo.as_str()),
        ("main", outsider.as_str()),
    ] {
        h.nits()
            .args(["review", "set-base", &review, reference, "--repo", target])
            .assert()
            .failure();
        assert_eq!(snapshot(&h, &review), before);
        assert_eq!(events(&h), history);
    }
    h.out(&["review", "archive", &review]);
    h.repo.git(&["checkout", "main"]).unwrap();
    h.repo.git(&["branch", "-D", "feature"]).unwrap();
    let archived = snapshot(&h, &review);
    let history = events(&h);
    h.nits()
        .args(["review", "reopen", &review])
        .assert()
        .failure();
    assert_eq!(snapshot(&h, &review), archived);
    assert_eq!(events(&h), history);
    assert_eq!(archived["review"]["status"], "Archived");
    // A correction can be made while archived, without changing status.
    h.out(&["review", "set-head", &review, "HEAD", "--repo", &repo]);
    assert_eq!(snapshot(&h, &review)["review"]["status"], "Archived");
    h.out(&["review", "reopen", &review]);
    assert_eq!(snapshot(&h, &review)["review"]["status"], "Open");
}

#[test]
fn deletion_hides_review_but_preserves_scoped_history_and_checkout() {
    let (h, workspace, _, review) = setup();
    h.out(&["comment", "add", &review, "--body", "historical discussion"]);
    let before = events(&h);
    let contents = std::fs::read(h.repo.path().join("a.rs")).unwrap();
    h.nits()
        .args(["review", "delete", &review])
        .assert()
        .success()
        .stdout(
            predicate::str::contains(&review).and(predicate::str::contains(
                "removed from listings, event history kept",
            )),
        );
    let reviews: Value =
        serde_json::from_str(&h.out(&["--json", "--workspace", &workspace, "review", "list"]))
            .unwrap();
    assert_eq!(reviews, serde_json::json!([]));
    let history = events(&h);
    assert_eq!(&history[..before.len()], &before);
    assert!(
        matches!(history.last().unwrap().body, EventBody::ReviewDeleted { review_id }
        if review_id.to_string() == review)
    );
    let scoped = h.out(&["--json", "events", "--review", &review, "--since", "0"]);
    assert!(scoped.contains("historical discussion") && scoped.contains("ReviewDeleted"));
    h.nits()
        .args(["review", "reopen", &review])
        .assert()
        .failure();
    assert_eq!(events(&h), history);
    assert_eq!(std::fs::read(h.repo.path().join("a.rs")).unwrap(), contents);
}

#[test]
fn workspace_rename_and_detach_preserve_review_and_files() {
    let (h, workspace, repo, review) = setup();
    let before = snapshot(&h, &review);
    let contents = std::fs::read(h.repo.path().join("a.rs")).unwrap();
    let event = receipt(&h, &["workspace", "rename", &workspace, "after"]);
    assert!(
        matches!(event.body, EventBody::WorkspaceUpdated { workspace_id, name }
        if workspace_id.to_string() == workspace && name == "after")
    );
    let event = receipt(&h, &["workspace", "detach", &workspace, &repo]);
    assert!(
        matches!(event.body, EventBody::RepoDetached { workspace_id, repo_id }
        if workspace_id.to_string() == workspace && repo_id.to_string() == repo)
    );
    let workspaces: Value = serde_json::from_str(&h.out(&["workspace", "list", "--json"])).unwrap();
    assert_eq!(workspaces[0]["name"], "after");
    assert_eq!(workspaces[0]["repos"], serde_json::json!([]));
    let reviews: Value =
        serde_json::from_str(&h.out(&["--workspace", &workspace, "review", "list", "--json"]))
            .unwrap();
    assert_eq!(reviews[0], before["review"]);
    assert_eq!(std::fs::read(h.repo.path().join("a.rs")).unwrap(), contents);
    assert!(
        h.out(&["events", "--since", "0"])
            .contains("checkout and history kept")
    );
}

fn agent(h: &Harness) -> Command {
    let mut command = h.nits();
    command
        .args(["--agent", "reviewer"])
        .env("NITS_AGENT_MODEL", "test-model")
        .env("NITS_SESSION_ID", "test-session");
    command
}

#[test]
fn comment_edits_and_tombstones_enforce_complete_human_and_agent_identity() {
    let (h, _, _, review) = setup();
    let human = receipt(&h, &["comment", "add", &review, "--body", "human"]);
    let EventBody::CommentCreated { comment: human } = human.body else {
        panic!("comment receipt")
    };
    let output = agent(&h)
        .args(["--json", "comment", "add", &review, "--body", "agent"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let EventBody::CommentCreated { comment: bot } =
        serde_json::from_slice::<Event>(&output).unwrap().body
    else {
        panic!("agent comment receipt")
    };
    let history = events(&h);
    for id in [human.id, bot.id] {
        for action in ["edit", "delete"] {
            let id = id.to_string();
            let mut args = vec!["--user", "another", "comment", action, &review, &id];
            if action == "edit" {
                args.extend(["--body", "forbidden"]);
            }
            h.nits()
                .args(args)
                .assert()
                .failure()
                .stderr(predicate::str::contains("only the author"));
        }
    }
    // Same agent name alone is insufficient: model, session and invoking human matter.
    for (key, value) in [
        ("NITS_AGENT_MODEL", "other-model"),
        ("NITS_SESSION_ID", "other-session"),
        ("NITS_USER", "another"),
    ] {
        for action in ["edit", "delete"] {
            let id = bot.id.to_string();
            let mut args = vec!["comment", action, &review, &id];
            if action == "edit" {
                args.extend(["--body", "forbidden"]);
            }
            agent(&h)
                .env(key, value)
                .args(args)
                .assert()
                .failure()
                .stderr(predicate::str::contains("only the author"));
        }
    }
    assert_eq!(events(&h), history);
    for (id, bot_author) in [(human.id, false), (bot.id, true)] {
        let command = || if bot_author { agent(&h) } else { h.nits() };
        let id = id.to_string();
        let output = command()
            .args([
                "--json",
                "comment",
                "edit",
                &review,
                &id,
                "--body",
                "corrected",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        assert!(
            matches!(serde_json::from_slice::<Event>(&output).unwrap().body,
            EventBody::CommentEdited { comment_id, body, .. } if comment_id.to_string() == id && body == "corrected")
        );
        let output = command()
            .args(["--json", "comment", "delete", &review, &id])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        assert!(
            matches!(serde_json::from_slice::<Event>(&output).unwrap().body,
            EventBody::CommentDeleted { comment_id, .. } if comment_id.to_string() == id)
        );
    }
    let current = snapshot(&h, &review);
    for comment in current["comments"].as_array().unwrap() {
        assert_eq!(comment["state"]["type"], "Deleted");
        assert_eq!(comment["body"], "corrected");
    }
    assert_eq!(current["threads"].as_array().unwrap().len(), 2);
    assert_eq!(&events(&h)[..history.len()], &history);
}

#[test]
fn editing_applied_suggestion_prose_preserves_immutable_patch_and_receipt() {
    let (h, _, repo, review) = setup();
    let patch = "@@ -1,2 +1,2 @@\n-fn a() { 1; }\n+fn a() { 2; }\n fn z() {}\n";
    let event = receipt(
        &h,
        &[
            "comment", "add", &review, "--path", "a.rs", "--repo", &repo, "--body", "before",
            "--patch", patch,
        ],
    );
    let EventBody::CommentCreated { comment } = event.body else {
        panic!("suggestion receipt")
    };
    let id = comment.id.to_string();
    h.rt.block_on(async {
        let client = nitsd::client::Client::connect_unix(
            &h.socket,
            nitsd::client::Identity {
                client_id: nits_protocol::ClientId::from_parts(1, 991),
                client: nits_protocol::BuildInfo {
                    name: "maintenance-test".into(),
                    version: "test".into(),
                },
                author: comment.author.clone(),
            },
        )
        .await
        .unwrap();
        nitsd::ops::Ops::new(client)
            .mutate(Mutation::ApplySuggestion {
                review_id: comment.review_id,
                comment_id: comment.id,
            })
            .await
            .unwrap();
    });
    let before = snapshot(&h, &review);
    assert_eq!(before["suggestions"][0]["outcome"]["type"], "Applied");
    receipt(
        &h,
        &[
            "comment",
            "edit",
            &review,
            &id,
            "--body",
            "corrected explanation",
        ],
    );
    let after = snapshot(&h, &review);
    assert_eq!(after["suggestions"], before["suggestions"]);
    assert_eq!(after["comments"][0]["kind"], before["comments"][0]["kind"]);
    assert_eq!(
        after["comments"][0]["anchor"],
        before["comments"][0]["anchor"]
    );
    assert_eq!(after["comments"][0]["body"], "corrected explanation");
    receipt(&h, &["comment", "delete", &review, &id]);
    assert_eq!(snapshot(&h, &review)["suggestions"], before["suggestions"]);
    assert_eq!(
        std::fs::read_to_string(h.repo.path().join("a.rs")).unwrap(),
        "fn a() { 2; }\nfn z() {}\n"
    );
}

#[test]
fn maintenance_uses_selected_websocket_daemon_and_json_receipts() {
    let (h, _, _, review) = setup();
    let output = h
        .nits()
        .args([
            "--daemon-url",
            &h.ws_url,
            "--json",
            "review",
            "rename",
            &review,
            "via websocket",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        matches!(serde_json::from_slice::<Event>(&output).unwrap().body,
        EventBody::ReviewUpdated { title, .. } if title == "via websocket")
    );
    assert_eq!(snapshot(&h, &review)["review"]["title"], "via websocket");
}

#[test]
fn maintenance_help_explains_consequences_and_bad_ids_fail_before_connect() {
    let dir = tempfile::tempdir().unwrap();
    let nits = || {
        let mut command = Command::cargo_bin("nits").unwrap();
        command
            .env("NITS_CONFIG", dir.path().join("absent.toml"))
            .env_remove("NITS_SOCKET")
            .env_remove("NITS_WS_URL")
            .args(["-c", "unconfigured-remote"]);
        command
    };
    for (args, expected) in [
        (vec!["review", "delete", "--help"], "retains event history"),
        (vec!["review", "archive", "--help"], "can be reopened"),
        (vec!["comment", "delete", "--help"], "tombstone"),
        (vec!["comment", "edit", "--help"], "suggestion patch"),
        (vec!["workspace", "detach", "--help"], "keeps its checkout"),
        (vec!["review", "set-base", "--help"], "never worktree"),
    ] {
        nits()
            .args(args)
            .assert()
            .success()
            .stdout(predicate::str::contains(expected));
    }
    for args in [
        vec!["review", "delete", "invalid"],
        vec!["review", "rename", "invalid", "title"],
        vec!["workspace", "rename", "invalid", "name"],
        vec!["comment", "delete", "01ARZ3NDEKTSV4RRFFQ69G5FAV", "invalid"],
    ] {
        nits()
            .args(args)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("invalid value"))
            .stderr(predicate::str::contains("unknown context").not());
    }
}

#[test]
fn matching_human_name_on_another_machine_cannot_edit_or_delete() {
    let (h, _, _, review) = setup();
    let event = receipt(&h, &["comment", "add", &review, "--body", "local"]);
    let EventBody::CommentCreated { comment } = event.body else {
        panic!("comment receipt")
    };
    let nits_protocol::Author::Human { name, machine } = comment.author else {
        panic!("human")
    };
    let foreign =
        h.rt.block_on(async {
            let client = nitsd::client::Client::connect_unix(
                &h.socket,
                nitsd::client::Identity {
                    client_id: nits_protocol::ClientId::from_parts(1, 992),
                    client: nits_protocol::BuildInfo {
                        name: "maintenance-test".into(),
                        version: "test".into(),
                    },
                    author: nits_protocol::Author::Human {
                        name,
                        machine: format!("other-{machine}"),
                    },
                },
            )
            .await
            .unwrap();
            nitsd::ops::Ops::new(client)
                .new_thread(
                    comment.review_id,
                    nits_protocol::CommentKind::Note,
                    nits_protocol::Anchor::Review,
                    "foreign".into(),
                )
                .await
                .unwrap()
                .0
                .comment_id
        })
        .to_string();
    let before = snapshot(&h, &review);
    let history = events(&h);
    for action in ["edit", "delete"] {
        let mut args = vec!["comment", action, &review, &foreign];
        if action == "edit" {
            args.extend(["--body", "forbidden"]);
        }
        h.nits()
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("only the author"));
    }
    assert_eq!(snapshot(&h, &review), before);
    assert_eq!(events(&h), history);
}
