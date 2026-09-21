use super::*;
use nits_protocol::{ReviewQuery, ReviewRound, ReviewScope};

fn all(core: &Core) -> nits_protocol::ReviewDiscovery {
    core.discover_reviews(&ReviewQuery::default()).unwrap()
}

#[test]
#[allow(clippy::too_many_lines)] // One lifecycle proves ordering survives unavailable Git and rebuild.
fn discovery_spans_workspaces_filters_and_orders_durable_activity_without_git() {
    let w = world();
    let second = WorkspaceId::from_parts(1, 11);
    let empty = WorkspaceId::from_parts(1, 12);
    for (id, name) in [(second, "hacks-ci"), (empty, "hacks-empty")] {
        w.core.create_workspace(&human(), id, name.into()).unwrap();
    }
    for repo in w.core.workspace(ws()).unwrap().repos {
        w.core
            .attach_repo(&human(), second, repo.id, &repo.path, repo.display_name)
            .unwrap();
    }
    for (id, workspace, title) in [
        (review_id(1), ws(), "PR #42 café"),
        (review_id(2), second, "PR #420"),
    ] {
        w.core
            .create_review(&human(), id, workspace, title.into(), targets())
            .unwrap();
    }
    assert_eq!(
        all(&w.core)
            .reviews
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        [review_id(2), review_id(1)]
    );
    // Commit order remains meaningful even if a caller's wall clock moves back.
    let ctx = Ctx {
        now: Timestamp::from_millis(1),
        ..human()
    };
    w.core
        .update_review(
            &ctx,
            review_id(1),
            "PR #42 CAFÉ".into(),
            ReviewStatus::Archived,
        )
        .unwrap();
    let result = all(&w.core);
    assert_eq!(result.reviews[0].id, review_id(1));
    assert_eq!(result.reviews[0].last_activity.at, ctx.now);
    assert_eq!(result.reviews[0].status, ReviewStatus::Archived);
    assert_eq!(result.reviews[1].workspace_name, "hacks-ci");
    let query = ReviewQuery {
        title: Some("café".into()),
        ..ReviewQuery::default()
    };
    assert_eq!(w.core.discover_reviews(&query).unwrap().reviews.len(), 1);
    let scoped = ReviewQuery {
        scope: ReviewScope::Workspace {
            workspace_id: second,
        },
        ..ReviewQuery::default()
    };
    assert_eq!(
        w.core.discover_reviews(&scoped).unwrap().reviews[0].id,
        review_id(2)
    );
    assert!(
        w.core
            .discover_reviews(&ReviewQuery {
                title: Some("absent".into()),
                ..scoped
            })
            .unwrap()
            .reviews
            .is_empty()
    );
    assert!(
        w.core
            .discover_reviews(&ReviewQuery {
                scope: ReviewScope::Workspace {
                    workspace_id: empty
                },
                ..ReviewQuery::default()
            })
            .unwrap()
            .reviews
            .is_empty()
    );
    assert!(matches!(
        w.core.discover_reviews(&ReviewQuery {
            scope: ReviewScope::Workspace {
                workspace_id: WorkspaceId::nil()
            },
            ..ReviewQuery::default()
        }),
        Err(CoreError::NotFound { .. })
    ));
    w.core.delete_review(&human(), review_id(2)).unwrap();
    let before = w.core.last_seq().unwrap();
    std::fs::rename(w.a.path(), w.data.root.join("missing-a")).unwrap();
    std::fs::rename(w.b.path(), w.data.root.join("missing-b")).unwrap();
    let result = all(&w.core);
    assert_eq!(result.reviews.len(), 1);
    assert_eq!(Some(result.seq), before);
    assert_eq!(w.core.last_seq().unwrap(), before);
    let World {
        _dir,
        data,
        core,
        a: _,
        b: _,
    } = w;
    drop(core);
    let store = nits_review_core::store::Store::open(&data.state()).unwrap();
    store.rebuild_views().unwrap();
    drop(store);
    let reopened = Core::open(&data).unwrap();
    assert_eq!(all(&reopened), result);
}

#[test]
#[allow(clippy::too_many_lines)] // The linked and unlinked checkpoints share one real conversation.
fn discovery_counts_threads_not_replies_and_requires_the_named_request_recipient() {
    let w = world();
    let id = review_id(1);
    w.core
        .create_review(&human(), id, ws(), "PR #42".into(), targets())
        .unwrap();
    let add = |n, kind| {
        w.core
            .add_comment(
                &agent(),
                id,
                cid(n),
                kind,
                Anchor::Review,
                format!("body {n}"),
                None,
            )
            .unwrap()
    };
    let open = add(1, CommentKind::Note);
    let resolved = add(2, CommentKind::Note);
    let deferred = add(3, CommentKind::Note);
    let deleted = add(4, CommentKind::Note);
    add(5, CommentKind::Informational);
    w.core
        .reply(
            &agent(),
            id,
            open.thread_id,
            cid(6),
            CommentKind::Note,
            "reply".into(),
        )
        .unwrap();
    w.core
        .resolve_thread(&human(), id, resolved.thread_id)
        .unwrap();
    w.core
        .defer_thread(
            &human(),
            id,
            deferred.thread_id,
            "External tracking".parse().unwrap(),
            None,
        )
        .unwrap();
    w.core.delete_comment(&agent(), id, deleted.id).unwrap();
    let request = w
        .core
        .request_review(&human(), id, "claude-code".into(), "inspect".into())
        .unwrap();
    let other = w
        .core
        .request_review(
            &human(),
            id,
            "other-agent".into(),
            "inspect separately".into(),
        )
        .unwrap();
    let summary = &all(&w.core).reviews[0];
    assert_eq!(summary.open_findings, 1);
    assert_eq!(
        summary
            .pending_requests
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        [request, other]
    );
    let revisions = w.core.review_snapshot(id).unwrap().resolved.unwrap();
    // An unlinked check and an answer by somebody else do not discharge this invitation.
    w.core
        .record_checkpoint(&agent(), id, revisions.clone(), None)
        .unwrap();
    w.core
        .record_checkpoint(
            &human(),
            id,
            revisions.clone(),
            Some(ReviewRound::Request {
                request_id: request,
            }),
        )
        .unwrap();
    assert_eq!(all(&w.core).reviews[0].pending_requests.len(), 2);
    let query = ReviewQuery {
        title: Some("#42".into()),
        awaiting: Some("claude-code".into()),
        ..ReviewQuery::default()
    };
    assert_eq!(w.core.discover_reviews(&query).unwrap().reviews.len(), 1);
    w.core
        .record_checkpoint(
            &agent(),
            id,
            revisions,
            Some(ReviewRound::Request {
                request_id: request,
            }),
        )
        .unwrap();
    assert!(w.core.discover_reviews(&query).unwrap().reviews.is_empty());
    assert_eq!(
        all(&w.core).reviews[0]
            .pending_requests
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        [other]
    );
    w.core
        .unresolve_thread(&human(), id, resolved.thread_id)
        .unwrap();
    assert_eq!(all(&w.core).reviews[0].open_findings, 2);
}

#[test]
fn discovery_joins_current_membership_without_inventing_review_activity() {
    let dir = tempfile::tempdir().unwrap();
    let empty = Core::open(&DataDir::new(dir.path().join("empty"))).unwrap();
    let initial = all(&empty);
    assert!(initial.reviews.is_empty());
    assert_eq!(initial.seq, nits_protocol::Seq::new(0));
    assert_eq!(empty.last_seq().unwrap(), None);

    let w = world();
    let id = review_id(1);
    w.core
        .create_review(&human(), id, ws(), "retained".into(), targets())
        .unwrap();
    let original = all(&w.core).reviews.remove(0);
    assert_eq!(original.repositories.len(), 2);
    w.core
        .rename_workspace(&human(), ws(), "renamed".into())
        .unwrap();
    w.core.detach_repo(&human(), ws(), rid(1)).unwrap();
    let changed = all(&w.core);
    let row = &changed.reviews[0];
    assert_eq!(row.workspace_name, "renamed");
    assert_eq!(
        row.repositories
            .iter()
            .map(|repo| repo.id)
            .collect::<Vec<_>>(),
        [rid(2)]
    );
    assert_eq!(row.targets, original.targets);
    assert_eq!(row.last_activity, original.last_activity);
    assert!(changed.seq > row.last_activity.seq);
    assert_eq!(Some(changed.seq), w.core.last_seq().unwrap());
    w.core.delete_review(&human(), id).unwrap();
    let deleted = all(&w.core);
    assert!(deleted.reviews.is_empty());
    assert_eq!(Some(deleted.seq), w.core.last_seq().unwrap());
}
