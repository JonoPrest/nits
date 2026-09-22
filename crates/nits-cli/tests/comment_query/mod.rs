use super::*;
use nits_protocol::CommentListing;

#[test]
fn comment_list_filters_joins_indents_tombstones_and_keeps_a_coherent_cursor() {
    let h = start();
    let workspace = h.out(&["workspace", "add", "review"]);
    h.out(&[
        "workspace",
        "attach",
        &workspace,
        h.repo.path().to_str().unwrap(),
    ]);
    let review = h.out(&[
        "review",
        "create",
        "--workspace",
        &workspace,
        "--base",
        "main",
        "--head",
        "feature",
        "--title",
        "discussion",
    ]);
    let root = h.out(&[
        "comment",
        "add",
        &review,
        "--body",
        "First café\n\nSecond paragraph",
    ]);
    h.out(&[
        "comment",
        "reply",
        &review,
        &root,
        "--agent",
        "responder",
        "--body",
        "Reply\nMore context",
    ]);
    let tombstone = h.out(&["comment", "add", &review, "--body", "Old prose"]);
    h.out(&["comment", "delete", &review, &tombstone]);
    let list = |args: &[&str]| {
        let output = h
            .nits()
            .args(["--json", "comment", "list", &review])
            .args(args)
            .assert()
            .success();
        serde_json::from_slice::<CommentListing>(&output.get_output().stdout).unwrap()
    };
    let all = list(&[]);
    assert_eq!(all.summary.threads, 2);
    assert_eq!(all.summary.open, 1);
    assert_eq!(all.summary.deleted, 1);
    assert_eq!(all.summary.comments, 3);
    assert_eq!(list(&["--open"]), list(&["--status", "open"]));
    assert_eq!(
        list(&["--author", "responder"]).threads[0].comments.len(),
        2
    );
    assert_eq!(list(&["--thread", &root]).summary.comments, 2);
    assert_eq!(list(&["--since", &u64::MAX.to_string()]).summary.threads, 0);
    assert_eq!(list(&["--since", &all.seq.to_string()]).seq, all.seq);
    let text = h.out(&["comment", "list", &review]);
    assert!(
        text.contains("    First café\n    \n    Second paragraph\n\n"),
        "{text}"
    );
    assert!(text.contains("    Reply\n    More context\n\n"), "{text}");
    assert!(text.contains("[deleted comment]"));
    assert!(!text.contains("Old prose"));
    assert!(text.contains("2 threads: 1 open, 0 resolved, 0 deferred, 0 informational, 1 deleted; 3 comments (1 deleted); seq"));
    let one = h.out(&["comment", "list", &review, "--oneline"]);
    assert_eq!(one.lines().count(), 4);
    assert!(one.contains("First café"));
    assert!(!one.contains("Second paragraph"));
    assert!(one.contains(&format!("thread {root}")));
}

#[test]
fn comment_list_invalid_filters_fail_before_configuration_or_connection() {
    let dir = tempfile::tempdir().unwrap();
    let bad_config = dir.path().join("invalid.toml");
    std::fs::write(&bad_config, "invalid = [").unwrap();
    let review = nits_protocol::ReviewId::from_parts(1, 1).to_string();
    let data = dir.path().join("never-created");
    for args in [
        vec!["--open", "--status", "resolved"],
        vec!["--status", "wrong"],
        vec!["--path", "../outside"],
        vec!["--thread", "wrong"],
        vec!["--repo", "wrong"],
        vec!["--since=-1"],
        vec!["--since", "18446744073709551616"],
    ] {
        Command::cargo_bin("nits")
            .unwrap()
            .env("NITS_CONFIG", &bad_config)
            .env("NITS_DATA_DIR", &data)
            .args(["comment", "list", &review])
            .args(args)
            .assert()
            .code(2);
        assert!(!data.exists());
    }
}
