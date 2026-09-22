//! Independent candidates do not change legacy bootstrap identities or history.

use super::start_seeded;
use nits_protocol::{
    Anchor, Author, BaseRefSpec, ClientId, ClientSeq, CommentId, CommentKind,
    EnsureDirectoryReview, RepoId, ReviewId, Timestamp, WorkspaceId,
};
use nits_review_core::{Core, Ctx};

#[test]
fn legacy_equal_bootstrap_ids_survive_restart_reuse_and_metadata_edits() {
    let workspace = WorkspaceId::from_parts(5, 1);
    let repo_id = RepoId::from_parts(5, 1);
    let review = ReviewId::from_parts(5, 1);
    let h = start_seeded(|data, repo| {
        let core = Core::open(data).unwrap();
        let ctx = Ctx {
            author: Author::Human {
                name: "ada".into(),
                machine: "test".into(),
            },
            client_id: ClientId::from_parts(5, 1),
            client_seq: ClientSeq::new(1),
            now: Timestamp::from_millis(1_700_000_000_000),
        };
        core.ensure_directory_review(
            &ctx,
            EnsureDirectoryReview {
                workspace_id: workspace,
                repo_id,
                review_id: review,
                path: repo.path().to_string_lossy().into_owned(),
                base: Some(BaseRefSpec::Head),
                head: None,
            },
        )
        .unwrap();
        core.add_comment(
            &ctx,
            review,
            CommentId::from_parts(5, 2),
            CommentKind::Note,
            Anchor::Review,
            "Preserved legacy discussion".into(),
            None,
        )
        .unwrap();
    }); // Drop the old Core/store before the daemon opens the same persisted state.
    let history = h.out(&["--json", "events", "--since", "0"]);
    let snapshot = h.out(&["--json", "review", "show", &review.to_string()]);
    assert!(snapshot.contains("Preserved legacy discussion"));
    for _ in 0..2 {
        let opened: serde_json::Value = serde_json::from_str(&h.out(&[
            "--json",
            h.repo.path().to_str().unwrap(),
            "--headless",
        ]))
        .unwrap();
        assert_eq!(opened["workspace_id"], workspace.to_string());
        assert_eq!(opened["repo_id"], repo_id.to_string());
        assert_eq!(opened["review_id"], review.to_string());
        assert_eq!(opened["outcome"], "Reused");
        assert_eq!(h.out(&["--json", "events", "--since", "0"]), history);
        assert_eq!(
            h.out(&["--json", "review", "show", &review.to_string()]),
            snapshot
        );
    }
    h.out(&[
        "workspace",
        "rename",
        &workspace.to_string(),
        "renamed legacy workspace",
    ]);
    assert_eq!(
        h.out(&[h.repo.path().to_str().unwrap(), "--headless"]),
        review.to_string()
    );
    let workspaces: Vec<nits_protocol::Workspace> =
        serde_json::from_str(&h.out(&["--json", "workspace", "list"])).unwrap();
    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0].id, workspace);
    assert_eq!(workspaces[0].repos[0].id, repo_id);
    assert_eq!(workspaces[0].name, "renamed legacy workspace");
    let after: serde_json::Value =
        serde_json::from_str(&h.out(&["--json", "review", "show", &review.to_string()])).unwrap();
    let original: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
    assert_eq!(after["comments"], original["comments"]);
    assert_eq!(after["review"], original["review"]);
    let events = h.out(&["--json", "events", "--since", "0"]);
    assert!(events.starts_with(&history));
    assert_eq!(events.lines().count(), history.lines().count() + 1);
}
