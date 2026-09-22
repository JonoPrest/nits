use super::start;
use nits_test_support::{RepoBuilder, files};
use serde_json::Value;

#[test]
fn explicit_fetch_revision_selection_and_checkpoint_warnings_are_visible_in_cli() {
    let h = start();
    let remote = RepoBuilder::new()
        .commit("remote base", files!["remote.txt" => "one\n"])
        .commit("remote fix", files!["remote.txt" => "two\n"])
        .tag("v2")
        .build()
        .unwrap();
    let next = remote.rev_parse("HEAD").unwrap();
    h.repo
        .git(&["remote", "add", "origin", remote.path().to_str().unwrap()])
        .unwrap();
    let workspace = h.out(&["workspace", "add", "remote loop"]);
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
        "HEAD",
    ]);
    let checked: Value =
        serde_json::from_str(&h.out(&["--json", "review", "check", &review, "--current"])).unwrap();
    let checkpoint = checked["seq"].as_u64().unwrap();
    let request: Value =
        serde_json::from_str(&h.out(&["--json", "review", "request", &review, "reviewer"]))
            .unwrap();
    assert_eq!(
        request["body"]["checkpoint_comparison"]["checkpoint_id"],
        checkpoint
    );
    assert_eq!(
        request["body"]["checkpoint_comparison"]["outcome"],
        "SameTargets"
    );
    let warning = h.out(&["review", "request", &review, "reviewer"]);
    assert!(
        warning.contains(&format!("unchanged since checkpoint {checkpoint}")),
        "{warning}"
    );
    let history = h.out(&["--json", "events", "--since", "0"]);
    h.nits()
        .args(["review", "set-head", &review, &next[..8], "--repo", &repo])
        .assert()
        .failure()
        .stderr(predicates::str::contains("Fetch explicitly"));
    assert_eq!(h.out(&["--json", "events", "--since", "0"]), history);
    let before_head = h.repo.rev_parse("HEAD").unwrap();
    let before_index = std::fs::read(h.repo.path().join(".git/index")).unwrap();
    let fetched: Value =
        serde_json::from_str(&h.out(&["--json", "review", "fetch", &review])).unwrap();
    assert_eq!(fetched["repo_id"], repo);
    assert_eq!(fetched["remote"], "origin");
    assert_eq!(fetched["resolution"]["changed"], false);
    assert_eq!(h.repo.rev_parse("HEAD").unwrap(), before_head);
    assert_eq!(
        std::fs::read(h.repo.path().join(".git/index")).unwrap(),
        before_index
    );
    for revision in [
        &next[..8],
        "origin/main",
        "refs/remotes/origin/main",
        "origin/main~1",
    ] {
        h.out(&["review", "set-head", &review, revision, "--repo", &repo]);
    }
    h.out(&["review", "set-base", &review, "HEAD~1", "--repo", &repo]);
    let request: Value =
        serde_json::from_str(&h.out(&["--json", "review", "request", &review, "reviewer"]))
            .unwrap();
    assert_eq!(
        request["body"]["checkpoint_comparison"]["checkpoint_id"],
        checkpoint
    );
    assert_eq!(
        request["body"]["checkpoint_comparison"]["outcome"],
        "ChangedTargets"
    );
    let text = h.out(&["review", "request", &review, "reviewer"]);
    assert!(!text.contains("Warning:"));
    assert!(text.contains(&format!("differ from checkpoint {checkpoint}")));
    let help = h.out(&["review", "set-head", "--help"]);
    assert!(
        help.contains("short/full OID")
            && help.contains("origin/branch")
            && help.contains("review fetch")
    );
}
