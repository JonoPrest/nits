use super::*;
use nits_protocol::{CommentListing, CommentQuery, CommentThreadStatus, Seq};

fn list(w: &World, query: &CommentQuery) -> CommentListing {
    w.core.list_comments(review_id(1), query).unwrap()
}

fn setup() -> World {
    let w = world();
    w.core
        .create_review(
            &human(),
            review_id(1),
            ws(),
            "conversation".into(),
            targets(),
        )
        .unwrap();
    w
}

fn add(w: &World, n: u128, kind: CommentKind) -> nits_protocol::Comment {
    w.core
        .add_comment(
            &human(),
            review_id(1),
            cid(n),
            kind,
            Anchor::Review,
            format!("root {n}\n\nUnicode café"),
            None,
        )
        .unwrap()
}

#[test]
#[allow(clippy::too_many_lines)] // One conversation compares every disposition and composed filter.
fn comment_query_joins_complete_threads_and_reports_disjoint_filtered_counts() {
    let w = setup();
    let open = add(&w, 1, CommentKind::Note);
    let resolved = add(&w, 2, CommentKind::Note);
    let deferred = add(&w, 3, CommentKind::Note);
    let deleted = add(&w, 4, CommentKind::Note);
    let informational = add(&w, 5, CommentKind::Informational);
    w.core
        .reply(
            &agent(),
            review_id(1),
            open.thread_id,
            cid(6),
            CommentKind::Note,
            "reply\nsecond line".into(),
        )
        .unwrap();
    w.core
        .resolve_thread(&human(), review_id(1), resolved.thread_id)
        .unwrap();
    w.core
        .defer_thread(
            &human(),
            review_id(1),
            deferred.thread_id,
            "Tracked elsewhere".parse().unwrap(),
            None,
        )
        .unwrap();
    w.core
        .delete_comment(&human(), review_id(1), deleted.id)
        .unwrap();
    let before = w.core.last_seq().unwrap();
    let all = list(&w, &CommentQuery::default());
    assert_eq!(Some(all.seq), before);
    assert_eq!(
        all.summary,
        nits_protocol::CommentSummary {
            threads: 5,
            open: 1,
            resolved: 1,
            deferred: 1,
            informational: 1,
            deleted: 1,
            comments: 6,
            deleted_comments: 1,
        }
    );
    assert_eq!(
        all.threads[0]
            .comments
            .iter()
            .map(|c| c.id)
            .collect::<Vec<_>>(),
        [cid(1), cid(6)]
    );
    for (status, id) in [
        (CommentThreadStatus::Open, open.thread_id),
        (CommentThreadStatus::Resolved, resolved.thread_id),
        (CommentThreadStatus::Deferred, deferred.thread_id),
        (CommentThreadStatus::Informational, informational.thread_id),
        (CommentThreadStatus::Deleted, deleted.thread_id),
    ] {
        let selected = list(
            &w,
            &CommentQuery {
                status: Some(status),
                ..CommentQuery::default()
            },
        );
        assert_eq!(selected.threads.len(), 1);
        assert_eq!(selected.threads[0].id, id);
        assert_eq!(selected.summary.threads, 1);
    }
    let by_reply = list(
        &w,
        &CommentQuery {
            author: Some("claude-code".into()),
            ..CommentQuery::default()
        },
    );
    assert_eq!(by_reply.threads.len(), 1);
    assert_eq!(
        by_reply.threads[0].comments.len(),
        2,
        "matching reply retains root context"
    );
    assert!(
        list(
            &w,
            &CommentQuery {
                author: Some("Claude-code".into()),
                ..CommentQuery::default()
            }
        )
        .threads
        .is_empty()
    );
    assert!(
        list(
            &w,
            &CommentQuery {
                thread_id: Some(open.thread_id),
                status: Some(CommentThreadStatus::Resolved),
                ..CommentQuery::default()
            }
        )
        .threads
        .is_empty()
    );
    assert_eq!(
        w.core
            .discover_reviews(&nits_protocol::ReviewQuery::default())
            .unwrap()
            .reviews[0]
            .open_findings,
        all.summary.open
    );
    assert_eq!(w.core.last_seq().unwrap(), before);
}

#[test]
fn comment_query_since_is_exclusive_thread_activity_not_review_activity() {
    let w = setup();
    let a = add(&w, 1, CommentKind::Note);
    let b = add(&w, 2, CommentKind::Note);
    let cursor = list(&w, &CommentQuery::default()).seq;
    w.core
        .update_review(&human(), review_id(1), "renamed".into(), ReviewStatus::Open)
        .unwrap();
    w.core
        .request_review(&human(), review_id(1), "other".into(), "request".into())
        .unwrap();
    let since = |cursor| {
        list(
            &w,
            &CommentQuery {
                since: Some(cursor),
                ..CommentQuery::default()
            },
        )
    };
    assert!(since(cursor).threads.is_empty());
    assert!(
        since(cursor).seq > cursor,
        "empty reads still advance coherent cursor"
    );
    let reply_cursor = since(cursor).seq;
    w.core
        .reply(
            &agent(),
            review_id(1),
            a.thread_id,
            cid(3),
            CommentKind::Note,
            "answer".into(),
        )
        .unwrap();
    let changed = since(reply_cursor);
    assert_eq!(changed.threads[0].id, a.thread_id);
    assert_eq!(changed.threads[0].comments.len(), 2);
    assert!(since(changed.seq).threads.is_empty());
    w.core
        .edit_comment(&agent(), review_id(1), cid(3), "edited".into())
        .unwrap();
    let edited = since(changed.seq);
    assert_eq!(
        edited.threads[0].comments.iter().last().unwrap().body,
        "edited"
    );
    w.core
        .delete_comment(&agent(), review_id(1), cid(3))
        .unwrap();
    let deleted = since(edited.seq);
    assert_eq!(deleted.summary.deleted_comments, 1);
    w.core
        .resolve_thread(&human(), review_id(1), b.thread_id)
        .unwrap();
    let resolved = since(deleted.seq);
    assert_eq!(resolved.threads[0].id, b.thread_id);
    assert_eq!(resolved.summary.resolved, 1);
    w.core
        .unresolve_thread(&human(), review_id(1), b.thread_id)
        .unwrap();
    let reopened = since(resolved.seq);
    assert_eq!(reopened.summary.open, 1);
    w.core
        .defer_thread(
            &human(),
            review_id(1),
            b.thread_id,
            "Later".parse().unwrap(),
            None,
        )
        .unwrap();
    assert_eq!(since(reopened.seq).summary.deferred, 1);
    assert!(since(Seq::new(u64::MAX)).threads.is_empty());
    assert!(
        since(Seq::new(list(&w, &CommentQuery::default()).seq.get() + 100))
            .threads
            .is_empty()
    );
}

#[test]
fn comment_query_path_repository_and_reply_author_select_the_same_conversation() {
    let w = world();
    w.b.write_file("src/main.rs", b"same path, different repository\n")
        .unwrap();
    w.b.git(&["add", "."]).unwrap();
    w.b.git(&["commit", "-m", "shared path"]).unwrap();
    w.core
        .create_review(&human(), review_id(1), ws(), "paths".into(), targets())
        .unwrap();
    for n in [1, 2] {
        let blob = head_blob(&w.core, review_id(1), rid(n), "src/main.rs");
        w.core
            .add_comment(
                &human(),
                review_id(1),
                cid(n),
                CommentKind::Note,
                Anchor::File {
                    repo_id: rid(n),
                    path: p("src/main.rs"),
                    blob_oid: blob,
                },
                "file".into(),
                None,
            )
            .unwrap();
    }
    add(&w, 3, CommentKind::Note);
    let query = CommentQuery {
        path: Some(p("src/main.rs")),
        ..CommentQuery::default()
    };
    assert_eq!(list(&w, &query).threads.len(), 2);
    let selected = list(
        &w,
        &CommentQuery {
            repo_id: Some(rid(2)),
            ..query.clone()
        },
    );
    assert_eq!(selected.threads[0].root, cid(2));
    assert_eq!(selected.summary.threads, 1);
    assert!(
        list(
            &w,
            &CommentQuery {
                path: Some(p("src/main")),
                ..query.clone()
            }
        )
        .threads
        .is_empty()
    );
    assert!(
        list(
            &w,
            &CommentQuery {
                repo_id: Some(rid(999)),
                ..query
            }
        )
        .threads
        .is_empty()
    );
}

#[test]
fn comment_query_survives_missing_git_reopen_and_rebuild_with_tombstones() {
    let w = setup();
    add(&w, 1, CommentKind::Note);
    w.core
        .delete_comment(&human(), review_id(1), cid(1))
        .unwrap();
    let before = list(&w, &CommentQuery::default());
    std::fs::rename(w.a.path(), w.data.root.join("missing-checkout")).unwrap();
    assert_eq!(list(&w, &CommentQuery::default()), before);
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
    let core = Core::open(&data).unwrap();
    assert_eq!(
        core.list_comments(review_id(1), &CommentQuery::default())
            .unwrap(),
        before
    );
    assert!(matches!(
        core.list_comments(review_id(999), &CommentQuery::default()),
        Err(CoreError::NotFound { .. })
    ));
    core.delete_review(&human(), review_id(1)).unwrap();
    assert!(matches!(
        core.list_comments(review_id(1), &CommentQuery::default()),
        Err(CoreError::NotFound { .. })
    ));
}

#[test]
fn comment_query_reports_reanchors_outdated_state_and_suggestion_receipts_as_activity() {
    let w = setup();
    let blob = head_blob(&w.core, review_id(1), rid(1), "src/main.rs");
    w.core
        .add_comment(
            &agent(),
            review_id(1),
            cid(1),
            CommentKind::Suggestion {
                patch: "@@ -2,1 +2,1 @@\n-    let a = 1;\n+    let a = 10;\n".into(),
            },
            Anchor::File {
                repo_id: rid(1),
                path: p("src/main.rs"),
                blob_oid: blob,
            },
            "ten".into(),
            None,
        )
        .unwrap();
    let before = list(&w, &CommentQuery::default()).seq;
    w.core
        .apply_suggestion(&human(), review_id(1), cid(1))
        .unwrap();
    let applied = list(
        &w,
        &CommentQuery {
            since: Some(before),
            ..CommentQuery::default()
        },
    );
    assert_eq!(applied.threads.len(), 1);
    assert!(matches!(
        applied.suggestions[0].outcome,
        nits_protocol::SuggestionOutcome::Applied { .. }
    ));
    w.a.git(&["add", "."]).unwrap();
    w.a.git(&["commit", "-qm", "applied"]).unwrap();
    w.core.resolve_targets(&human(), review_id(1)).unwrap();
    let reanchored = list(
        &w,
        &CommentQuery {
            since: Some(applied.seq),
            ..CommentQuery::default()
        },
    );
    assert_eq!(reanchored.threads.len(), 1);
    w.a.git(&["rm", "-q", "src/main.rs"]).unwrap();
    w.a.git(&["commit", "-qm", "remove"]).unwrap();
    w.core.resolve_targets(&human(), review_id(1)).unwrap();
    let outdated = list(
        &w,
        &CommentQuery {
            since: Some(reanchored.seq),
            status: Some(CommentThreadStatus::Open),
            ..CommentQuery::default()
        },
    );
    assert_eq!(outdated.summary.open, 1);
    assert!(matches!(
        outdated.threads[0].comments.first().state,
        CommentState::Outdated { .. }
    ));
    assert_eq!(
        outdated.suggestions, applied.suggestions,
        "receipts keep original identity after reanchoring"
    );
    assert!(
        list(
            &w,
            &CommentQuery {
                author: Some("absent".into()),
                ..CommentQuery::default()
            }
        )
        .suggestions
        .is_empty()
    );
}
