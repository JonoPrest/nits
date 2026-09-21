//! Store tests: append/read, views vs rebuild, persistence, tombstones,
//! schema versioning, concurrent appenders.

use std::sync::Arc;

use nits_protocol::{
    Anchor, Author, BlobOid, ClientId, ClientSeq, Comment, CommentId, CommentKind, CommentState,
    EventBody, Human, NonEmpty, RefSpec, Repo, RepoId, RepoPath, Review, ReviewId, ReviewStatus,
    ReviewTarget, SchemaVersion, Seq, ThreadId, Timestamp, Workspace, WorkspaceId,
};
use nits_review_core::store::{NewEvent, ReviewLifecycle, Store, StoreError};
use proptest::prelude::*;

// ---- fixtures ---------------------------------------------------------------

fn ws_id() -> WorkspaceId {
    WorkspaceId::from_parts(1, 1)
}
fn repo_id() -> RepoId {
    RepoId::from_parts(1, 2)
}
fn review_id(n: u128) -> ReviewId {
    ReviewId::from_parts(2, n)
}
fn comment_id(n: u128) -> CommentId {
    CommentId::from_parts(3, n)
}
fn thread_of(c: CommentId) -> ThreadId {
    c.to_string().parse().unwrap()
}
fn human() -> Author {
    Author::Human {
        name: "ada".into(),
        machine: "box".into(),
    }
}
fn new_event(body: EventBody) -> NewEvent {
    NewEvent {
        ts: Timestamp::from_millis(1_700_000_000_000),
        author: human(),
        client_id: ClientId::from_parts(1, 9),
        client_seq: ClientSeq::new(0),
        body,
    }
}
fn workspace() -> Workspace {
    Workspace {
        id: ws_id(),
        name: "w".into(),
        repos: vec![Repo {
            id: repo_id(),
            path: "/tmp/r".into(),
            display_name: "r".into(),
        }],
    }
}
fn review(n: u128) -> Review {
    Review {
        id: review_id(n),
        workspace_id: ws_id(),
        title: format!("review {n}"),
        targets: NonEmpty::singleton(ReviewTarget {
            repo_id: repo_id(),
            base: RefSpec::Branch {
                name: "main".into(),
            },
            head: RefSpec::WorkingTree,
        }),
        created: Timestamp::from_millis(0),
        status: ReviewStatus::Open,
    }
}
fn blob(n: u8) -> BlobOid {
    BlobOid::from_bytes([n; 20])
}
fn comment(review: u128, id: u128, thread: u128, blob_n: u8) -> Comment {
    Comment {
        id: comment_id(id),
        review_id: review_id(review),
        thread_id: thread_of(comment_id(thread)),
        author: human(),
        kind: CommentKind::Note,
        anchor: Anchor::File {
            repo_id: repo_id(),
            path: RepoPath::new("a.txt").unwrap(),
            blob_oid: blob(blob_n),
        },
        body: "hi".into(),
        created: Timestamp::from_millis(0),
        edited: None,
        state: CommentState::Live,
        context: None,
    }
}

fn open_temp() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state.redb")).unwrap();
    (dir, store)
}

fn stamp_schema(path: &std::path::Path, v: u32) {
    let db = redb::Database::open(path).unwrap();
    let txn = db.begin_write().unwrap();
    {
        let mut meta = txn
            .open_table(redb::TableDefinition::<&str, u64>::new("meta"))
            .unwrap();
        meta.insert("schema_version", u64::from(v)).unwrap();
    }
    txn.commit().unwrap();
}

// ---- tests ------------------------------------------------------------------

#[test]
fn append_assigns_increasing_seq_and_reads_back() {
    let (_d, s) = open_temp();
    assert!(s.is_empty().unwrap());
    let e1 = s
        .append(new_event(EventBody::WorkspaceCreated {
            workspace: workspace(),
        }))
        .unwrap();
    let e2 = s
        .append(new_event(EventBody::ReviewCreated { review: review(1) }))
        .unwrap();
    assert_eq!(e1.seq, Seq::FIRST);
    assert_eq!(e2.seq, Seq::new(2));
    assert_eq!(s.last_seq().unwrap(), Some(Seq::new(2)));
    assert_eq!(s.events_after(None).unwrap(), vec![e1.clone(), e2.clone()]);
    assert_eq!(s.events_after(Some(Seq::FIRST)).unwrap(), vec![e2]);
    assert_eq!(s.workspaces().unwrap(), vec![workspace()]);
    assert_eq!(s.reviews(ws_id()).unwrap()[0].review, review(1));
}

#[test]
fn comments_threads_and_anchor_index() {
    let (_d, s) = open_temp();
    s.append(new_event(EventBody::WorkspaceCreated {
        workspace: workspace(),
    }))
    .unwrap();
    s.append(new_event(EventBody::ReviewCreated { review: review(1) }))
        .unwrap();
    s.append(new_event(EventBody::CommentCreated {
        comment: comment(1, 1, 1, 7),
    }))
    .unwrap();
    s.append(new_event(EventBody::CommentCreated {
        comment: comment(1, 2, 1, 7),
    }))
    .unwrap();

    let threads = s.threads(review_id(1)).unwrap();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].root, comment_id(1));
    assert_eq!(threads[0].replies, vec![comment_id(2)]);

    let mut on_blob = s.comments_on_blob(repo_id(), blob(7)).unwrap();
    on_blob.sort();
    assert_eq!(
        on_blob,
        vec![(review_id(1), comment_id(1)), (review_id(1), comment_id(2))]
    );

    s.append(new_event(EventBody::CommentDeleted {
        review_id: review_id(1),
        comment_id: comment_id(2),
    }))
    .unwrap();
    assert_eq!(
        s.comments_on_blob(repo_id(), blob(7)).unwrap(),
        vec![(review_id(1), comment_id(1))]
    );
    let c2 = s.comment(review_id(1), comment_id(2)).unwrap().unwrap();
    assert_eq!(c2.state, CommentState::Deleted);

    // Reply to an unknown thread is inconsistent, not silently accepted.
    let err = s
        .append(new_event(EventBody::CommentCreated {
            comment: comment(1, 3, 99, 7),
        }))
        .unwrap_err();
    assert!(matches!(err, StoreError::Inconsistent { .. }), "{err}");
    // and the failed append did not consume a seq
    assert_eq!(s.last_seq().unwrap(), Some(Seq::new(5)));
}

#[test]
fn tombstoned_review_is_excluded_from_listing_but_fetchable() {
    let (_d, s) = open_temp();
    s.append(new_event(EventBody::WorkspaceCreated {
        workspace: workspace(),
    }))
    .unwrap();
    s.append(new_event(EventBody::ReviewCreated { review: review(1) }))
        .unwrap();
    s.append(new_event(EventBody::ReviewCreated { review: review(2) }))
        .unwrap();
    s.append(new_event(EventBody::ReviewDeleted {
        review_id: review_id(1),
    }))
    .unwrap();
    let listed: Vec<_> = s
        .reviews(ws_id())
        .unwrap()
        .into_iter()
        .map(|r| r.review.id)
        .collect();
    assert_eq!(listed, vec![review_id(2)]);
    let rec = s.review(review_id(1)).unwrap().unwrap();
    assert_eq!(rec.lifecycle, ReviewLifecycle::Deleted { at: Seq::new(4) });
}

#[test]
fn reopen_preserves_log_and_views() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.redb");
    let before = {
        let s = Store::open(&path).unwrap();
        s.append(new_event(EventBody::WorkspaceCreated {
            workspace: workspace(),
        }))
        .unwrap();
        s.append(new_event(EventBody::ReviewCreated { review: review(1) }))
            .unwrap();
        s.append(new_event(EventBody::CommentCreated {
            comment: comment(1, 1, 1, 3),
        }))
        .unwrap();
        (s.events_after(None).unwrap(), s.dump_views().unwrap())
    };
    let s = Store::open(&path).unwrap();
    assert_eq!(s.events_after(None).unwrap(), before.0);
    assert_eq!(s.dump_views().unwrap(), before.1);
    assert_eq!(s.schema_version().unwrap(), SchemaVersion::CURRENT);
}

#[test]
fn rebuild_matches_incremental() {
    let (_d, s) = open_temp();
    s.append(new_event(EventBody::WorkspaceCreated {
        workspace: workspace(),
    }))
    .unwrap();
    s.append(new_event(EventBody::ReviewCreated { review: review(1) }))
        .unwrap();
    s.append(new_event(EventBody::CommentCreated {
        comment: comment(1, 1, 1, 3),
    }))
    .unwrap();
    s.append(new_event(EventBody::ThreadResolved {
        review_id: review_id(1),
        thread_id: thread_of(comment_id(1)),
    }))
    .unwrap();
    let incremental = s.dump_views().unwrap();
    s.rebuild_views().unwrap();
    assert_eq!(s.dump_views().unwrap(), incremental);
}

#[test]
fn schema_too_new_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.redb");
    {
        let s = Store::open(&path).unwrap();
        s.append(new_event(EventBody::WorkspaceCreated {
            workspace: workspace(),
        }))
        .unwrap();
    }
    stamp_schema(&path, SchemaVersion::CURRENT.get() + 1);
    let err = Store::open(&path).unwrap_err();
    assert!(matches!(err, StoreError::SchemaTooNew { .. }), "{err}");
}

#[test]
fn schema_zero_migrates_forward_and_replays() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.redb");
    let events = {
        let s = Store::open(&path).unwrap();
        s.append(new_event(EventBody::WorkspaceCreated {
            workspace: workspace(),
        }))
        .unwrap();
        s.append(new_event(EventBody::ReviewCreated { review: review(1) }))
            .unwrap();
        s.events_after(None).unwrap()
    };
    stamp_schema(&path, 0);
    let s = Store::open(&path).unwrap();
    assert_eq!(s.schema_version().unwrap(), SchemaVersion::CURRENT);
    assert_eq!(s.events_after(None).unwrap(), events);
    assert_eq!(s.reviews(ws_id()).unwrap().len(), 1);
}

#[test]
fn stale_views_are_rebuilt_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.redb");
    {
        let s = Store::open(&path).unwrap();
        s.append(new_event(EventBody::WorkspaceCreated {
            workspace: workspace(),
        }))
        .unwrap();
    }
    // Simulate a crash between log append and view update: wipe the views
    // and the view_seq marker.
    {
        let db = redb::Database::open(&path).unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut meta = txn
                .open_table(redb::TableDefinition::<&str, u64>::new("meta"))
                .unwrap();
            meta.remove("view_seq").unwrap();
            let mut ws = txn
                .open_table(redb::TableDefinition::<&str, &[u8]>::new("workspaces"))
                .unwrap();
            ws.retain(|_, _| false).unwrap();
        }
        txn.commit().unwrap();
    }
    let s = Store::open(&path).unwrap();
    assert_eq!(s.workspaces().unwrap(), vec![workspace()]);
}

#[test]
fn concurrent_appenders_get_strictly_increasing_seq() {
    let (_d, s) = open_temp();
    s.append(new_event(EventBody::WorkspaceCreated {
        workspace: workspace(),
    }))
    .unwrap();
    let s = Arc::new(s);
    let handles: Vec<_> = (0..8u128)
        .map(|i| {
            let s = Arc::clone(&s);
            std::thread::spawn(move || {
                (0..25u128)
                    .map(|j| {
                        s.append(new_event(EventBody::ReviewCreated {
                            review: review(i * 100 + j),
                        }))
                        .unwrap()
                        .seq
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut seqs: Vec<Seq> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    seqs.sort();
    let expected: Vec<Seq> = (2..=201).map(Seq::new).collect();
    assert_eq!(seqs, expected);
    assert_eq!(s.reviews(ws_id()).unwrap().len(), 200);
}

// ---- proptest: any valid event sequence folds identically ------------------

#[derive(Debug, Clone)]
enum Op {
    CreateReview(u8),
    DeleteReview(u8),
    Comment {
        review: u8,
        id: u8,
        blob: u8,
    },
    Reply {
        review: u8,
        thread: u8,
        id: u8,
        blob: u8,
    },
    Edit {
        review: u8,
        id: u8,
    },
    Delete {
        review: u8,
        id: u8,
    },
    Resolve {
        review: u8,
        thread: u8,
    },
    Unresolve {
        review: u8,
        thread: u8,
    },
    Viewed {
        review: u8,
        blob: u8,
    },
    Unviewed {
        review: u8,
    },
}

fn op_strategy() -> impl Strategy<Value = Op> {
    let r = 0u8..4;
    let c = 0u8..6;
    let b = 0u8..3;
    prop_oneof![
        r.clone().prop_map(Op::CreateReview),
        r.clone().prop_map(Op::DeleteReview),
        (r.clone(), c.clone(), b.clone()).prop_map(|(review, id, blob)| Op::Comment {
            review,
            id,
            blob
        }),
        (r.clone(), c.clone(), c.clone(), b.clone()).prop_map(|(review, thread, id, blob)| {
            Op::Reply {
                review,
                thread,
                id,
                blob,
            }
        }),
        (r.clone(), c.clone()).prop_map(|(review, id)| Op::Edit { review, id }),
        (r.clone(), c.clone()).prop_map(|(review, id)| Op::Delete { review, id }),
        (r.clone(), c.clone()).prop_map(|(review, thread)| Op::Resolve { review, thread }),
        (r.clone(), c.clone()).prop_map(|(review, thread)| Op::Unresolve { review, thread }),
        (r.clone(), b).prop_map(|(review, blob)| Op::Viewed { review, blob }),
        r.prop_map(|review| Op::Unviewed { review }),
    ]
}

/// Translate ops into events, skipping ones that would be inconsistent
/// (the store rejects those; validation is upstream of append).
#[allow(clippy::too_many_lines)]
fn apply_ops(s: &Store, ops: &[Op]) -> usize {
    let mut reviews = std::collections::BTreeSet::new();
    let mut comments = std::collections::BTreeSet::<(u8, u8)>::new();
    // Only root comments start threads; replies do not.
    let mut threads = std::collections::BTreeSet::<(u8, u8)>::new();
    let mut applied = 0;
    let viewer = Human {
        name: "ada".into(),
        machine: "box".into(),
    };
    for op in ops {
        let body = match *op {
            Op::CreateReview(r) => {
                if !reviews.insert(r) {
                    continue;
                }
                EventBody::ReviewCreated {
                    review: review(r.into()),
                }
            }
            Op::DeleteReview(r) => {
                if !reviews.contains(&r) {
                    continue;
                }
                EventBody::ReviewDeleted {
                    review_id: review_id(r.into()),
                }
            }
            Op::Comment { review, id, blob } => {
                if !reviews.contains(&review) || !comments.insert((review, id)) {
                    continue;
                }
                threads.insert((review, id));
                EventBody::CommentCreated {
                    comment: comment(review.into(), id.into(), id.into(), blob),
                }
            }
            Op::Reply {
                review,
                thread,
                id,
                blob,
            } => {
                if !threads.contains(&(review, thread)) || !comments.insert((review, id)) {
                    continue;
                }
                EventBody::CommentCreated {
                    comment: comment(review.into(), id.into(), thread.into(), blob),
                }
            }
            Op::Edit { review, id } => {
                if !comments.contains(&(review, id)) {
                    continue;
                }
                EventBody::CommentEdited {
                    review_id: review_id(review.into()),
                    comment_id: comment_id(id.into()),
                    body: "edited".into(),
                }
            }
            Op::Delete { review, id } => {
                if !comments.contains(&(review, id)) {
                    continue;
                }
                EventBody::CommentDeleted {
                    review_id: review_id(review.into()),
                    comment_id: comment_id(id.into()),
                }
            }
            Op::Resolve { review, thread } => {
                if !threads.contains(&(review, thread)) {
                    continue;
                }
                EventBody::ThreadResolved {
                    review_id: review_id(review.into()),
                    thread_id: thread_of(comment_id(thread.into())),
                }
            }
            Op::Unresolve { review, thread } => {
                if !threads.contains(&(review, thread)) {
                    continue;
                }
                EventBody::ThreadUnresolved {
                    review_id: review_id(review.into()),
                    thread_id: thread_of(comment_id(thread.into())),
                }
            }
            Op::Viewed { review, blob } => {
                if !reviews.contains(&review) {
                    continue;
                }
                EventBody::FileViewed {
                    review_id: review_id(review.into()),
                    repo_id: repo_id(),
                    path: RepoPath::new("a.txt").unwrap(),
                    viewer: viewer.clone(),
                    content: nits_protocol::ViewedContent::Blob {
                        entry: nits_protocol::BlobEntry {
                            oid: self::blob(blob),
                            mode: nits_protocol::BlobMode::Regular,
                        },
                    },
                }
            }
            Op::Unviewed { review } => {
                if !reviews.contains(&review) {
                    continue;
                }
                EventBody::FileUnviewed {
                    review_id: review_id(review.into()),
                    repo_id: repo_id(),
                    path: RepoPath::new("a.txt").unwrap(),
                    viewer: viewer.clone(),
                }
            }
        };
        s.append(new_event(body)).unwrap();
        applied += 1;
    }
    applied
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn views_equal_rebuilt_views(ops in prop::collection::vec(op_strategy(), 0..40)) {
        let (_d, s) = open_temp();
        s.append(new_event(EventBody::WorkspaceCreated { workspace: workspace() })).unwrap();
        let n = apply_ops(&s, &ops);
        prop_assert_eq!(s.len().unwrap(), n as u64 + 1);
        let incremental = s.dump_views().unwrap();
        s.rebuild_views().unwrap();
        prop_assert_eq!(s.dump_views().unwrap(), incremental);

        // and a second store fed the same log independently agrees
        let (_d2, s2) = open_temp();
        for e in s.events_after(None).unwrap() {
            s2.append(NewEvent { ts: e.ts, author: e.author, client_id: e.client_id, client_seq: e.client_seq, body: e.body }).unwrap();
        }
        prop_assert_eq!(s2.dump_views().unwrap(), s.dump_views().unwrap());
    }
}

#[test]
#[allow(clippy::too_many_lines)] // one lifecycle scenario through persistence or transport
fn schema_one_history_and_informational_threads_survive_migration_rebuild_and_reopen() {
    use nits_protocol::ThreadResolution;
    use redb::{ReadableDatabase, ReadableTable};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.redb");
    let (events, views) = {
        let store = Store::open(&path).unwrap();
        store
            .append(new_event(EventBody::WorkspaceCreated {
                workspace: workspace(),
            }))
            .unwrap();
        store
            .append(new_event(EventBody::ReviewCreated { review: review(1) }))
            .unwrap();
        // Historical review-level Note roots must remain actionable, even if
        // their body looks like a summary. Include a reply and a resolved root.
        for (id, thread) in [(1, 1), (2, 1), (3, 3)] {
            let mut c = comment(1, id, thread, 1);
            c.anchor = Anchor::Review;
            c.body = "Review summary from before informational notes existed".into();
            store
                .append(new_event(EventBody::CommentCreated { comment: c }))
                .unwrap();
        }
        store
            .append(new_event(EventBody::ThreadResolved {
                review_id: review_id(1),
                thread_id: thread_of(comment_id(3)),
            }))
            .unwrap();
        (
            store.events_after(None).unwrap(),
            store.dump_views().unwrap(),
        )
    };
    // Write actual schema-1 envelopes, not just a lowered meta stamp.
    {
        let db = redb::Database::open(&path).unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut log = txn
                .open_table(redb::TableDefinition::<u64, &[u8]>::new("events"))
                .unwrap();
            let old: Vec<_> = log
                .iter()
                .unwrap()
                .map(|entry| {
                    let (seq, bytes) = entry.unwrap();
                    let mut json: serde_json::Value =
                        serde_json::from_slice(bytes.value()).unwrap();
                    json["schema"] = serde_json::json!(1);
                    (seq.value(), serde_json::to_vec(&json).unwrap())
                })
                .collect();
            for (seq, bytes) in old {
                log.insert(seq, bytes.as_slice()).unwrap();
            }
        }
        txn.commit().unwrap();
    }
    stamp_schema(&path, 1);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.events_after(None).unwrap(), events);
    assert_eq!(store.dump_views().unwrap(), views);
    assert_eq!(store.schema_version().unwrap(), SchemaVersion::CURRENT);
    let mut note = comment(1, 4, 4, 1);
    note.kind = CommentKind::Informational;
    note.anchor = Anchor::Review;
    store
        .append(new_event(EventBody::CommentCreated { comment: note }))
        .unwrap();
    let mut reply = comment(1, 5, 4, 1);
    reply.anchor = Anchor::Review;
    store
        .append(new_event(EventBody::CommentCreated { comment: reply }))
        .unwrap();
    let expected = store.dump_views().unwrap();
    store.rebuild_views().unwrap();
    assert_eq!(store.dump_views().unwrap(), expected);
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.dump_views().unwrap(), expected);
    let threads = store.threads(review_id(1)).unwrap();
    assert_eq!(
        threads
            .iter()
            .filter(|t| t.resolution == ThreadResolution::Open)
            .count(),
        1
    );
    assert_eq!(
        threads
            .iter()
            .filter(|t| matches!(t.resolution, ThreadResolution::Resolved { .. }))
            .count(),
        1
    );
    let note = threads
        .iter()
        .find(|t| t.resolution == ThreadResolution::Informational)
        .unwrap();
    assert_eq!(note.replies, vec![comment_id(5)]);
    drop(store);
    let db = redb::Database::open(&path).unwrap();
    let txn = db.begin_read().unwrap();
    let log = txn
        .open_table(redb::TableDefinition::<u64, &[u8]>::new("events"))
        .unwrap();
    for entry in log.iter().unwrap() {
        let (seq, bytes) = entry.unwrap();
        let event: serde_json::Value = serde_json::from_slice(bytes.value()).unwrap();
        // 8→9 rewrites old envelopes; 9→10 only rebuilds derived views.
        let expected = if seq.value() <= events.len() as u64 {
            serde_json::json!(9)
        } else {
            serde_json::json!(SchemaVersion::CURRENT)
        };
        assert_eq!(event["schema"], expected);
    }
}

#[test]
fn schema_two_requests_survive_upgrade_reopen_and_rebuild() {
    use redb::ReadableTable;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.redb");
    let (events, expected) = {
        let store = Store::open(&path).unwrap();
        store
            .append(new_event(EventBody::WorkspaceCreated {
                workspace: workspace(),
            }))
            .unwrap();
        store
            .append(new_event(EventBody::ReviewCreated { review: review(1) }))
            .unwrap();
        for note in ["Review the parser", "Review the follow-up"] {
            store
                .append(new_event(EventBody::ReviewRequested {
                    review_id: review_id(1),
                    agent: "review-agent".into(),
                    note: note.into(),
                    targets: nits_protocol::RequestedTargets::Unknown,
                }))
                .unwrap();
        }
        (
            store.events_after(None).unwrap(),
            store.review_snapshot(review_id(1)).unwrap().unwrap(),
        )
    };
    // Reproduce schema 2: identical historical event JSON, current view_seq,
    // and no request view table. Simply checking a stale view cursor misses this upgrade.
    {
        let db = redb::Database::open(&path).unwrap();
        let txn = db.begin_write().unwrap();
        txn.delete_table(redb::TableDefinition::<(&str, u64), &[u8]>::new(
            "review_requests",
        ))
        .unwrap();
        {
            let mut log = txn
                .open_table(redb::TableDefinition::<u64, &[u8]>::new("events"))
                .unwrap();
            let rows: Vec<_> = log
                .iter()
                .unwrap()
                .map(|entry| {
                    let (seq, bytes) = entry.unwrap();
                    let mut json: serde_json::Value =
                        serde_json::from_slice(bytes.value()).unwrap();
                    json["schema"] = serde_json::json!(2);
                    strip_legacy_request_targets(&mut json);
                    (seq.value(), serde_json::to_vec(&json).unwrap())
                })
                .collect();
            for (seq, bytes) in rows {
                log.insert(seq, bytes.as_slice()).unwrap();
            }
        }
        txn.commit().unwrap();
    }
    stamp_schema(&path, 2);
    for _ in 0..2 {
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SchemaVersion::CURRENT);
        assert_eq!(store.events_after(None).unwrap(), events);
        assert_eq!(
            store.review_snapshot(review_id(1)).unwrap().unwrap(),
            expected
        );
        store.rebuild_views().unwrap();
        assert_eq!(
            store.review_snapshot(review_id(1)).unwrap().unwrap(),
            expected
        );
    }
    assert_eq!(expected.requests.len(), 2);
    assert_eq!(expected.requests[0].id.event_seq(), Seq::new(3));
    assert_eq!(expected.requests[0].requester, human());
    assert_eq!(expected.requests[0].recipient, "review-agent");
    assert_eq!(expected.requests[0].note, "Review the parser");
    assert_eq!(expected.requests[0].created, events[2].ts);
    assert!(expected.threads.is_empty());
    assert!(expected.comments.is_empty());
}

#[test]
fn snapshot_requests_checkpoints_and_cursor_share_one_read_transaction() {
    let (_dir, store) = open_temp();
    let store = Arc::new(store);
    store
        .append(new_event(EventBody::WorkspaceCreated {
            workspace: workspace(),
        }))
        .unwrap();
    store
        .append(new_event(EventBody::ReviewCreated { review: review(1) }))
        .unwrap();
    let writer = Arc::clone(&store);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let writer_barrier = Arc::clone(&barrier);
    let handle = std::thread::spawn(move || {
        writer_barrier.wait();
        let revision = nits_protocol::ResolvedRef {
            tree: nits_protocol::TreeOid::from_bytes([1; 20]),
            source: nits_protocol::ResolvedSource::Commit {
                oid: nits_protocol::CommitOid::from_bytes([2; 20]),
            },
        };
        let targets = NonEmpty::singleton(nits_protocol::ResolvedTarget {
            repo_id: repo_id(),
            base: revision.clone(),
            head: revision,
        });
        for n in 0..100 {
            let requested = writer
                .append(new_event(EventBody::ReviewRequested {
                    review_id: review_id(1),
                    agent: "review-agent".into(),
                    note: format!("Request {n}"),
                    targets: nits_protocol::RequestedTargets::Captured {
                        targets: targets.clone(),
                    },
                }))
                .unwrap();
            writer
                .append(new_event(EventBody::ReviewChecked {
                    review_id: review_id(1),
                    reviewer: nits_protocol::ReviewerIdentity::from_author(&human()).unwrap(),
                    targets: targets.clone(),
                    in_reply_to: Some(nits_protocol::ReviewRound::Request {
                        request_id: nits_protocol::ReviewRequestId::from_event_seq(requested.seq),
                    }),
                }))
                .unwrap();
        }
    });
    barrier.wait();
    for _ in 0..150 {
        let snapshot = store.review_snapshot(review_id(1)).unwrap().unwrap();
        // Requests and checks follow the first two events. The cursor must
        // cover exactly the materialized records despite concurrent appends.
        assert_eq!(
            u64::try_from(snapshot.requests.len() + snapshot.checkpoints.len()).unwrap(),
            snapshot.seq.get() - 2
        );
        for (offset, request) in snapshot.requests.iter().enumerate() {
            assert_eq!(
                request.id.event_seq().get(),
                u64::try_from(offset).unwrap() * 2 + 3
            );
        }
        for (offset, checkpoint) in snapshot.checkpoints.iter().enumerate() {
            assert_eq!(
                checkpoint.id.event_seq().get(),
                u64::try_from(offset).unwrap() * 2 + 4
            );
            assert_eq!(
                checkpoint.in_reply_to,
                Some(nits_protocol::ReviewRound::Request {
                    request_id: snapshot.requests[offset].id,
                })
            );
        }
    }
    handle.join().unwrap();
    assert_eq!(
        store
            .review_snapshot(review_id(1))
            .unwrap()
            .unwrap()
            .requests
            .len(),
        100
    );
}

#[test]
#[allow(clippy::too_many_lines)] // Keep actual old log construction and upgrade assertions together.
fn legacy_diff_context_migrates_without_reinterpreting_null_contexts() {
    use nits_protocol::ThreadResolution;
    use redb::{ReadableTable, TableDefinition};
    for old_schema in 0..=3 {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.redb");
        let (expected, views, snapshot) = {
            let store = Store::open(&path).unwrap();
            store
                .append(new_event(EventBody::WorkspaceCreated {
                    workspace: workspace(),
                }))
                .unwrap();
            store
                .append(new_event(EventBody::ReviewCreated { review: review(1) }))
                .unwrap();
            let context = Some(nits_protocol::CommentContext::Diff {
                change: nits_protocol::ChangeKind::Modified {
                    old: nits_protocol::BlobEntry {
                        oid: blob(2),
                        mode: nits_protocol::BlobMode::Unknown,
                    },
                    new: nits_protocol::BlobEntry {
                        oid: blob(3),
                        mode: nits_protocol::BlobMode::Unknown,
                    },
                },
            });
            let mut first = comment(1, 1, 1, 3);
            first.context = context.clone();
            let mut reply = comment(1, 3, 1, 3);
            reply.context = context;
            for comment in [first, comment(1, 2, 2, 3), reply] {
                store
                    .append(new_event(EventBody::CommentCreated { comment }))
                    .unwrap();
            }
            store
                .append(new_event(EventBody::ThreadResolved {
                    review_id: review_id(1),
                    thread_id: thread_of(comment_id(1)),
                }))
                .unwrap();
            if old_schema >= 2 {
                let mut note = comment(1, 4, 4, 1);
                note.anchor = Anchor::Review;
                note.kind = CommentKind::Informational;
                store
                    .append(new_event(EventBody::CommentCreated { comment: note }))
                    .unwrap();
            }
            store
                .append(new_event(EventBody::ReviewRequested {
                    review_id: review_id(1),
                    agent: "review-agent".into(),
                    note: "Please check the retained discussion".into(),
                    targets: nits_protocol::RequestedTargets::Unknown,
                }))
                .unwrap();
            (
                store.events_after(None).unwrap(),
                store.dump_views().unwrap(),
                store.review_snapshot(review_id(1)).unwrap().unwrap(),
            )
        };
        // Recreate old envelopes with untagged ChangeKind contexts. Starting at
        // schema 1 must not decode those payloads before the 3→4 transform.
        {
            let db = redb::Database::open(&path).unwrap();
            let txn = db.begin_write().unwrap();
            if old_schema < 3 {
                txn.delete_table(TableDefinition::<(&str, u64), &[u8]>::new(
                    "review_requests",
                ))
                .unwrap();
            }
            {
                let mut events = txn
                    .open_table(TableDefinition::<u64, &[u8]>::new("events"))
                    .unwrap();
                let old: Vec<_> = events
                    .iter()
                    .unwrap()
                    .map(|row| {
                        let (key, value) = row.unwrap();
                        let mut value: serde_json::Value =
                            serde_json::from_slice(value.value()).unwrap();
                        value["schema"] = old_schema.into();
                        strip_legacy_request_targets(&mut value);
                        if let Some(context) = value.pointer_mut("/event/body/comment/context")
                            && !context.is_null()
                        {
                            *context = context["change"].take();
                            for side in ["old", "new"] {
                                context[side] = context[side]["oid"].take();
                            }
                        }
                        (key.value(), serde_json::to_vec(&value).unwrap())
                    })
                    .collect();
                for (key, value) in old {
                    events.insert(key, value.as_slice()).unwrap();
                }
            }
            txn.commit().unwrap();
        }
        stamp_schema(&path, old_schema);
        for _ in 0..2 {
            let store = Store::open(&path).unwrap();
            assert_eq!(store.schema_version().unwrap(), SchemaVersion::CURRENT);
            assert_eq!(store.events_after(None).unwrap(), expected);
            assert_eq!(store.dump_views().unwrap(), views);
            assert_eq!(
                store.review_snapshot(review_id(1)).unwrap().unwrap(),
                snapshot
            );
            store.rebuild_views().unwrap();
            assert_eq!(store.dump_views().unwrap(), views);
            assert_eq!(
                store.review_snapshot(review_id(1)).unwrap().unwrap(),
                snapshot
            );
        }
        assert_eq!(snapshot.requests.len(), 1);
        assert_eq!(snapshot.requests[0].id.event_seq(), snapshot.seq);
        assert_eq!(snapshot.threads[0].replies, vec![comment_id(3)]);
        assert!(matches!(
            snapshot.threads[0].resolution,
            ThreadResolution::Resolved { .. }
        ));
        assert!(snapshot.comments[1].context.is_none());
    }
}

#[test]
fn malformed_legacy_envelopes_return_migration_errors_without_panicking() {
    use redb::TableDefinition;
    for old_schema in [0, 1, 2, 3, 4, 5] {
        for invalid in [
            serde_json::json!([]),
            serde_json::json!({}),
            serde_json::json!({"schema": "invalid"}),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("state.redb");
            {
                let store = Store::open(&path).unwrap();
                store
                    .append(new_event(EventBody::WorkspaceCreated {
                        workspace: workspace(),
                    }))
                    .unwrap();
            }
            {
                let db = redb::Database::open(&path).unwrap();
                let txn = db.begin_write().unwrap();
                txn.open_table(TableDefinition::<u64, &[u8]>::new("events"))
                    .unwrap()
                    .insert(1, serde_json::to_vec(&invalid).unwrap().as_slice())
                    .unwrap();
                txn.commit().unwrap();
            }
            stamp_schema(&path, old_schema);
            assert!(matches!(Store::open(&path), Err(StoreError::Migration(_))));
        }
    }
}

#[test]
#[allow(clippy::too_many_lines)] // migration, rebuild and restart are one persistence scenario
fn schema_four_history_migrates_then_deferrals_survive_rebuild_and_restart() {
    use redb::{ReadableDatabase, ReadableTable};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deferrals.redb");
    let historical = {
        let store = Store::open(&path).unwrap();
        store
            .append(new_event(EventBody::WorkspaceCreated {
                workspace: workspace(),
            }))
            .unwrap();
        store
            .append(new_event(EventBody::ReviewCreated { review: review(1) }))
            .unwrap();
        for (id, thread) in [(1, 1), (2, 1), (3, 3)] {
            let mut c = comment(1, id, thread, 1);
            if id == 3 {
                c.anchor = Anchor::Review;
                c.kind = CommentKind::Informational;
            } else {
                c.context = Some(nits_protocol::CommentContext::Browse {
                    reference: nits_protocol::RefSpec::Tag { name: "v1".into() },
                });
            }
            store
                .append(new_event(EventBody::CommentCreated { comment: c }))
                .unwrap();
        }
        store
            .append(new_event(EventBody::ReviewRequested {
                review_id: review_id(1),
                agent: "review-agent".into(),
                note: "Inspect the retained Browse finding".into(),
                targets: nits_protocol::RequestedTargets::Unknown,
            }))
            .unwrap();
        (
            store.events_after(None).unwrap(),
            store.review_snapshot(review_id(1)).unwrap().unwrap(),
        )
    };
    // A real previous-schema log, with informational history and a reply.
    {
        let db = redb::Database::open(&path).unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut log = txn
                .open_table(redb::TableDefinition::<u64, &[u8]>::new("events"))
                .unwrap();
            let rows: Vec<_> = log
                .iter()
                .unwrap()
                .map(|entry| {
                    let (seq, bytes) = entry.unwrap();
                    let mut json: serde_json::Value =
                        serde_json::from_slice(bytes.value()).unwrap();
                    json["schema"] = serde_json::json!(4);
                    strip_legacy_request_targets(&mut json);
                    (seq.value(), serde_json::to_vec(&json).unwrap())
                })
                .collect();
            for (seq, bytes) in rows {
                log.insert(seq, bytes.as_slice()).unwrap();
            }
        }
        txn.commit().unwrap();
    }
    stamp_schema(&path, 4);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.events_after(None).unwrap(), historical.0);
    assert_eq!(
        store.review_snapshot(review_id(1)).unwrap().unwrap(),
        historical.1
    );
    assert_eq!(store.schema_version().unwrap(), SchemaVersion::CURRENT);
    let event = new_event(EventBody::ThreadDeferred {
        review_id: review_id(1),
        thread_id: thread_of(comment_id(1)),
        reason: "Controller issue #288 per scope decision".parse().unwrap(),
        tracking_url: Some("https://example.com/issues/288".parse().unwrap()),
    });
    store.append(event).unwrap();
    let expected = store.dump_views().unwrap();
    let events = store.events_after(None).unwrap();
    store.rebuild_views().unwrap();
    assert_eq!(store.dump_views().unwrap(), expected);
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.dump_views().unwrap(), expected);
    assert_eq!(store.events_after(None).unwrap(), events);
    let restored = store.review_snapshot(review_id(1)).unwrap().unwrap();
    assert_eq!(restored.requests, historical.1.requests);
    assert_eq!(restored.comments, historical.1.comments);
    let threads = store.threads(review_id(1)).unwrap();
    let finding = threads
        .iter()
        .find(|t| t.id == thread_of(comment_id(1)))
        .unwrap();
    assert!(
        matches!(&finding.resolution, nits_protocol::ThreadResolution::Deferred { reason, tracking_url: Some(url), by, at }
        if reason.to_string() == "Controller issue #288 per scope decision" && url.to_string() == "https://example.com/issues/288" && by == &human() && *at == Timestamp::from_millis(1_700_000_000_000))
    );
    assert_eq!(finding.replies, vec![comment_id(2)]);
    assert!(
        threads
            .iter()
            .any(|t| t.resolution == nits_protocol::ThreadResolution::Informational)
    );
    store
        .append(new_event(EventBody::ThreadUnresolved {
            review_id: review_id(1),
            thread_id: thread_of(comment_id(1)),
        }))
        .unwrap();
    store.rebuild_views().unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .threads(review_id(1))
            .unwrap()
            .iter()
            .find(|t| t.id == thread_of(comment_id(1)))
            .unwrap()
            .resolution,
        nits_protocol::ThreadResolution::Open
    );
    let reopened_history = store.events_after(None).unwrap();
    assert_eq!(
        &reopened_history[..events.len()],
        events.as_slice(),
        "reopening preserves the original deferral and its attribution in history"
    );
    drop(store);
    let db = redb::Database::open(&path).unwrap();
    let read = db.begin_read().unwrap();
    let log = read
        .open_table(redb::TableDefinition::<u64, &[u8]>::new("events"))
        .unwrap();
    for entry in log.iter().unwrap() {
        let (seq, bytes) = entry.unwrap();
        let json: serde_json::Value = serde_json::from_slice(bytes.value()).unwrap();
        // Historical envelopes stop at schema 9; subsequent appends use 10.
        let expected = if seq.value() <= historical.0.len() as u64 {
            serde_json::json!(9)
        } else {
            serde_json::json!(SchemaVersion::CURRENT)
        };
        assert_eq!(json["schema"], expected);
    }
}

#[test]
#[allow(clippy::too_many_lines)] // Actual legacy histories, upgrade and new checkpoints share one lifecycle.
fn every_old_schema_migrates_raw_requests_with_unknown_targets_without_invention() {
    use redb::ReadableTable;
    for (schema, unstamped) in [
        (0, false),
        (0, true),
        (1, false),
        (2, false),
        (3, false),
        (4, false),
        (5, false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.redb");
        let expected = {
            let store = Store::open(&path).unwrap();
            store
                .append(new_event(EventBody::WorkspaceCreated {
                    workspace: workspace(),
                }))
                .unwrap();
            store
                .append(new_event(EventBody::ReviewCreated { review: review(1) }))
                .unwrap();
            // Browse and deferred dispositions only exist in their genuine schemas.
            if schema >= 4 {
                let mut finding = comment(1, 1, 1, 1);
                finding.context = Some(nits_protocol::CommentContext::Browse {
                    reference: RefSpec::Tag {
                        name: "retained".into(),
                    },
                });
                store
                    .append(new_event(EventBody::CommentCreated { comment: finding }))
                    .unwrap();
            }
            if schema >= 5 {
                store
                    .append(new_event(EventBody::ThreadDeferred {
                        review_id: review_id(1),
                        thread_id: thread_of(comment_id(1)),
                        reason: "Existing external follow-up".parse().unwrap(),
                        tracking_url: Some("https://example.com/issues/66".parse().unwrap()),
                    }))
                    .unwrap();
            }
            store
                .append(new_event(EventBody::ReviewRequested {
                    review_id: review_id(1),
                    agent: "review-agent".into(),
                    note: "Legacy revision in prose".into(),
                    targets: nits_protocol::RequestedTargets::Unknown,
                }))
                .unwrap();
            store.review_snapshot(review_id(1)).unwrap().unwrap()
        };
        {
            let db = redb::Database::open(&path).unwrap();
            let txn = db.begin_write().unwrap();
            {
                let mut log = txn
                    .open_table(redb::TableDefinition::<u64, &[u8]>::new("events"))
                    .unwrap();
                let rows = log
                    .iter()
                    .unwrap()
                    .map(|row| {
                        let (key, bytes) = row.unwrap();
                        let mut raw: serde_json::Value =
                            serde_json::from_slice(bytes.value()).unwrap();
                        raw["schema"] = serde_json::json!(schema);
                        if raw["event"]["body"]["type"] == "ReviewRequested" {
                            raw["event"]["body"]
                                .as_object_mut()
                                .unwrap()
                                .remove("targets");
                        }
                        (key.value(), serde_json::to_vec(&raw).unwrap())
                    })
                    .collect::<Vec<_>>();
                for (key, bytes) in rows {
                    log.insert(key, bytes.as_slice()).unwrap();
                }
            }
            txn.commit().unwrap();
        }
        stamp_schema(&path, schema);
        if unstamped {
            let db = redb::Database::open(&path).unwrap();
            let txn = db.begin_write().unwrap();
            txn.open_table(redb::TableDefinition::<&str, u64>::new("meta"))
                .unwrap()
                .remove("schema_version")
                .unwrap();
            txn.commit().unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.review_snapshot(review_id(1)).unwrap().unwrap(),
            expected,
            "starting schema {schema}"
        );
        let before_events = store.events_after(None).unwrap();
        let target = nits_protocol::ResolvedRef {
            tree: nits_protocol::TreeOid::from_bytes([1; 20]),
            source: nits_protocol::ResolvedSource::WorkingTree {
                head: None,
                dirty: Vec::new(),
                branch: None,
            },
        };
        store
            .append(new_event(EventBody::ReviewChecked {
                review_id: review_id(1),
                reviewer: nits_protocol::ReviewerIdentity::Human {
                    name: "ada".into(),
                    machine: "laptop".into(),
                },
                targets: NonEmpty::singleton(nits_protocol::ResolvedTarget {
                    repo_id: review(1).targets.first().repo_id,
                    base: target.clone(),
                    head: target,
                }),
                in_reply_to: Some(nits_protocol::ReviewRound::Request {
                    request_id: expected.requests[0].id,
                }),
            }))
            .unwrap();
        let checked = store.review_snapshot(review_id(1)).unwrap().unwrap();
        assert_eq!(checked.requests, expected.requests);
        assert_eq!(checked.comments, expected.comments);
        assert_eq!(checked.threads, expected.threads);
        store.rebuild_views().unwrap();
        assert_eq!(
            store.review_snapshot(review_id(1)).unwrap().unwrap(),
            checked
        );
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(
            reopened.review_snapshot(review_id(1)).unwrap().unwrap(),
            checked
        );
        assert_eq!(
            &reopened.events_after(None).unwrap()[..before_events.len()],
            before_events
        );
        if schema >= 4 {
            let link = nits_protocol::ReviewReference {
                context: nits_protocol::ReferenceContext::named("review-box").unwrap(),
                review_id: review_id(1),
                target: nits_protocol::ReferenceTarget::Thread {
                    thread_id: thread_of(comment_id(1)),
                },
            };
            assert_eq!(link.resolve(&checked).unwrap(), Some(comment_id(1)));
        }
    }
}

fn strip_legacy_request_targets(value: &mut serde_json::Value) {
    if value["event"]["body"]["type"] == "ReviewRequested" {
        value["event"]["body"]
            .as_object_mut()
            .unwrap()
            .remove("targets");
    }
}

#[test]
fn schema_six_worktree_snapshots_preserve_unknown_head_through_upgrade_and_rebuild() {
    use nits_protocol::{ResolvedRef, ResolvedSource, ResolvedTarget, TreeOid};
    use redb::ReadableTable;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.redb");
    let targets = NonEmpty::singleton(ResolvedTarget {
        repo_id: repo_id(),
        base: ResolvedRef {
            tree: TreeOid::from_bytes([1; 20]),
            source: ResolvedSource::Commit {
                oid: nits_protocol::CommitOid::from_bytes([2; 20]),
            },
        },
        head: ResolvedRef {
            tree: TreeOid::from_bytes([3; 20]),
            source: ResolvedSource::WorkingTree {
                dirty: vec![],
                branch: Some("feature".into()),
                head: None,
            },
        },
    });
    let expected = {
        let store = Store::open(&path).unwrap();
        store
            .append(new_event(EventBody::WorkspaceCreated {
                workspace: workspace(),
            }))
            .unwrap();
        store
            .append(new_event(EventBody::ReviewCreated { review: review(1) }))
            .unwrap();
        store
            .append(new_event(EventBody::ReviewTargetsResolved {
                review_id: review_id(1),
                targets: targets.clone(),
            }))
            .unwrap();
        store
            .append(new_event(EventBody::ReviewRequested {
                review_id: review_id(1),
                agent: "review-agent".into(),
                note: "captured".into(),
                targets: nits_protocol::RequestedTargets::Captured { targets },
            }))
            .unwrap();
        store.review_snapshot(review_id(1)).unwrap().unwrap()
    };
    {
        let db = redb::Database::open(&path).unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut log = txn
                .open_table(redb::TableDefinition::<u64, &[u8]>::new("events"))
                .unwrap();
            let entries: Vec<_> = log
                .iter()
                .unwrap()
                .map(|row| {
                    let (key, bytes) = row.unwrap();
                    let mut raw: serde_json::Value = serde_json::from_slice(bytes.value()).unwrap();
                    raw["schema"] = serde_json::json!(6);
                    for pointer in [
                        "/event/body/targets/0/head/source",
                        "/event/body/targets/targets/0/head/source",
                    ] {
                        if let Some(source) = raw.pointer_mut(pointer) {
                            source.as_object_mut().unwrap().remove("head");
                        }
                    }
                    (key.value(), serde_json::to_vec(&raw).unwrap())
                })
                .collect();
            for (key, bytes) in entries {
                log.insert(key, bytes.as_slice()).unwrap();
            }
        }
        txn.commit().unwrap();
    }
    stamp_schema(&path, 6);
    for _ in 0..2 {
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SchemaVersion::CURRENT);
        assert_eq!(
            store.review_snapshot(review_id(1)).unwrap().unwrap(),
            expected
        );
        store.rebuild_views().unwrap();
        assert_eq!(
            store.review_snapshot(review_id(1)).unwrap().unwrap(),
            expected
        );
    }
}

#[test]
fn schema_seven_blob_and_missing_viewed_marks_keep_provenance_through_upgrade() {
    use nits_protocol::ViewedContent;
    use redb::ReadableTable;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("schema-seven.redb");
    let expected = {
        let store = Store::open(&path).unwrap();
        store
            .append(new_event(EventBody::WorkspaceCreated {
                workspace: workspace(),
            }))
            .unwrap();
        store
            .append(new_event(EventBody::ReviewCreated { review: review(1) }))
            .unwrap();
        for (name, content) in [
            (
                "source",
                ViewedContent::Blob {
                    entry: nits_protocol::BlobEntry {
                        oid: blob(3),
                        mode: nits_protocol::BlobMode::Unknown,
                    },
                },
            ),
            ("deleted", ViewedContent::Missing),
        ] {
            store
                .append(new_event(EventBody::FileViewed {
                    review_id: review_id(1),
                    repo_id: repo_id(),
                    path: RepoPath::new(name).unwrap(),
                    viewer: Human {
                        name: "ada".into(),
                        machine: "box".into(),
                    },
                    content,
                }))
                .unwrap();
        }
        (
            store.events_after(None).unwrap(),
            store.review_snapshot(review_id(1)).unwrap().unwrap(),
        )
    };
    {
        let db = redb::Database::open(&path).unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut log = txn
                .open_table(redb::TableDefinition::<u64, &[u8]>::new("events"))
                .unwrap();
            let rows = log
                .iter()
                .unwrap()
                .map(|row| {
                    let (key, bytes) = row.unwrap();
                    let mut raw: serde_json::Value = serde_json::from_slice(bytes.value()).unwrap();
                    raw["schema"] = serde_json::json!(7);
                    if raw["event"]["body"]["type"] == "FileViewed" {
                        let body = raw["event"]["body"].as_object_mut().unwrap();
                        let content = body.remove("content").unwrap();
                        body.insert(
                            "blob_oid".into(),
                            content
                                .pointer("/entry/oid")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null),
                        );
                    }
                    (key.value(), serde_json::to_vec(&raw).unwrap())
                })
                .collect::<Vec<_>>();
            for (key, bytes) in rows {
                log.insert(key, bytes.as_slice()).unwrap();
            }
        }
        txn.commit().unwrap();
    }
    stamp_schema(&path, 7);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.events_after(None).unwrap(), expected.0);
    assert_eq!(
        store.review_snapshot(review_id(1)).unwrap().unwrap(),
        expected.1
    );
    store.rebuild_views().unwrap();
    assert_eq!(
        store.review_snapshot(review_id(1)).unwrap().unwrap(),
        expected.1
    );
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.events_after(None).unwrap(), expected.0);
    assert_eq!(
        store.review_snapshot(review_id(1)).unwrap().unwrap(),
        expected.1
    );
}

#[test]
#[allow(clippy::too_many_lines)] // Full legacy event log and rebuild assertions form one migration scenario.
fn schema_eight_preserves_historical_modes_as_unknown_and_keeps_gitlink_commits() {
    use nits_protocol::{
        BlobEntry, BlobMode, ChangeKind, CommentContext, SubmoduleChange, ViewedContent,
    };
    use redb::{ReadableTable, TableDefinition};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("schema-eight.redb");
    let old = BlobEntry {
        oid: blob(2),
        mode: BlobMode::Unknown,
    };
    let new = BlobEntry {
        oid: blob(3),
        mode: BlobMode::Unknown,
    };
    let commit = nits_protocol::CommitOid::from_bytes([7; 20]);
    let expected = {
        let store = Store::open(&path).unwrap();
        store
            .append(new_event(EventBody::WorkspaceCreated {
                workspace: workspace(),
            }))
            .unwrap();
        store
            .append(new_event(EventBody::ReviewCreated { review: review(1) }))
            .unwrap();
        for (index, change) in [
            ChangeKind::Added { new },
            ChangeKind::Deleted { old },
            ChangeKind::Modified { old, new },
            ChangeKind::Renamed {
                from: RepoPath::new("old.txt").unwrap(),
                old,
                new,
            },
            ChangeKind::Submodule {
                change: SubmoduleChange::Added { new: commit },
            },
            ChangeKind::Submodule {
                change: SubmoduleChange::BlobToSubmodule { old, new: commit },
            },
            ChangeKind::Submodule {
                change: SubmoduleChange::SubmoduleToBlob { old: commit, new },
            },
        ]
        .into_iter()
        .enumerate()
        {
            let id = u128::try_from(index).unwrap() + 1;
            let mut comment = comment(1, id, id, 3);
            comment.context = Some(CommentContext::Diff { change });
            store
                .append(new_event(EventBody::CommentCreated { comment }))
                .unwrap();
        }
        for (name, content) in [
            ("file", ViewedContent::Blob { entry: new }),
            ("removed", ViewedContent::Missing),
            ("dependency", ViewedContent::Submodule { commit }),
        ] {
            store
                .append(new_event(EventBody::FileViewed {
                    review_id: review_id(1),
                    repo_id: repo_id(),
                    path: RepoPath::new(name).unwrap(),
                    viewer: Human {
                        name: "ada".into(),
                        machine: "box".into(),
                    },
                    content,
                }))
                .unwrap();
        }
        (
            store.events_after(None).unwrap(),
            store.review_snapshot(review_id(1)).unwrap().unwrap(),
        )
    };
    {
        let db = redb::Database::open(&path).unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut events = txn
                .open_table(TableDefinition::<u64, &[u8]>::new("events"))
                .unwrap();
            let rows: Vec<_> = events
                .iter()
                .unwrap()
                .map(|row| {
                    let (key, bytes) = row.unwrap();
                    let mut raw: serde_json::Value = serde_json::from_slice(bytes.value()).unwrap();
                    raw["schema"] = 8.into();
                    if let Some(content) = raw.pointer_mut("/event/body/content")
                        && content["type"] == "Blob"
                    {
                        let oid = content["entry"]["oid"].take();
                        *content = serde_json::json!({"type": "Blob", "oid": oid});
                    }
                    if let Some(change) = raw.pointer_mut("/event/body/comment/context/change") {
                        let change = if change["type"] == "Submodule" {
                            &mut change["change"]
                        } else {
                            change
                        };
                        for side in ["old", "new"] {
                            if change[side].is_object() {
                                change[side] = change[side]["oid"].take();
                            }
                        }
                    }
                    (key.value(), serde_json::to_vec(&raw).unwrap())
                })
                .collect();
            for (key, bytes) in rows {
                events.insert(key, bytes.as_slice()).unwrap();
            }
        }
        txn.commit().unwrap();
    }
    stamp_schema(&path, 8);
    for _ in 0..2 {
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SchemaVersion::CURRENT);
        assert_eq!(store.events_after(None).unwrap(), expected.0);
        assert_eq!(
            store.review_snapshot(review_id(1)).unwrap().unwrap(),
            expected.1
        );
        store.rebuild_views().unwrap();
        assert_eq!(
            store.review_snapshot(review_id(1)).unwrap().unwrap(),
            expected.1
        );
    }
    assert_ne!(
        ViewedContent::Blob { entry: new },
        ViewedContent::Blob {
            entry: BlobEntry {
                mode: BlobMode::Regular,
                ..new
            }
        },
        "historical marks cannot assert that today's mode was reviewed"
    );
}

#[test]
fn schema_nine_rebuilds_original_suggestion_identity_and_historical_receipts() {
    use nits_protocol::{SuggestionOutcome, SuggestionRecord};
    let (dir, store) = open_temp();
    store
        .append(new_event(EventBody::WorkspaceCreated {
            workspace: workspace(),
        }))
        .unwrap();
    store
        .append(new_event(EventBody::ReviewCreated { review: review(1) }))
        .unwrap();
    let root = comment(1, 1, 1, 7);
    store
        .append(new_event(EventBody::CommentCreated {
            comment: root.clone(),
        }))
        .unwrap();
    let mut reply = comment(1, 2, 1, 8);
    reply.kind = CommentKind::Suggestion {
        patch: "@@ -1 +1 @@\n-old\n+new\n".into(),
    };
    reply.state = CommentState::Outdated {
        last_good_anchor: root.anchor.clone(),
    };
    let original = SuggestionRecord::from_created(&reply).unwrap();
    store
        .append(new_event(EventBody::CommentCreated { comment: reply }))
        .unwrap();
    let later_anchor = Anchor::File {
        repo_id: repo_id(),
        path: RepoPath::new("renamed.txt").unwrap(),
        blob_oid: blob(9),
    };
    store
        .append(new_event(EventBody::CommentReanchored {
            review_id: review_id(1),
            comment_id: comment_id(2),
            anchor: later_anchor,
            state: CommentState::Live,
        }))
        .unwrap();
    let applied = store
        .append(new_event(EventBody::SuggestionApplied {
            review_id: review_id(1),
            comment_id: comment_id(2),
            repo_id: repo_id(),
            path: RepoPath::new("renamed.txt").unwrap(),
            result_blob: blob(10),
        }))
        .unwrap();
    let history = store.events_after(None).unwrap();
    let path = dir.path().join("state.redb");
    drop(store);
    // Recreate the real pre-migration layout: no materialized suggestion table.
    let db = redb::Database::open(&path).unwrap();
    let txn = db.begin_write().unwrap();
    txn.delete_table(redb::TableDefinition::<(&str, &str), &[u8]>::new(
        "suggestions",
    ))
    .unwrap();
    txn.open_table(redb::TableDefinition::<&str, u64>::new("meta"))
        .unwrap()
        .insert("schema_version", 9_u64)
        .unwrap();
    txn.commit().unwrap();
    drop(db);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.events_after(None).unwrap(), history);
    let snapshot = store.review_snapshot(review_id(1)).unwrap().unwrap();
    assert_eq!(snapshot.suggestions.len(), 1);
    let record = &snapshot.suggestions[0];
    assert_eq!(record.anchor, original.anchor);
    assert_eq!(record.patch, original.patch);
    assert_eq!(record.comment_id, comment_id(2));
    let SuggestionOutcome::Applied { receipt } = &record.outcome else {
        panic!("missing historical receipt")
    };
    assert_eq!(receipt.path.as_str(), "renamed.txt");
    assert_eq!(receipt.result_blob, blob(10));
    assert_eq!(receipt.seq, applied.seq);
    assert_eq!(receipt.author, applied.author);
    assert_eq!(receipt.at, applied.ts);
    let before = store.dump_views().unwrap();
    store.rebuild_views().unwrap();
    assert_eq!(store.dump_views().unwrap(), before);
    drop(store);
    let reopened = Store::open(&path).unwrap();
    assert_eq!(
        reopened.review_snapshot(review_id(1)).unwrap().unwrap(),
        snapshot
    );
}
