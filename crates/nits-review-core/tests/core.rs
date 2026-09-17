//! End-to-end scenarios over `Core` (plan 1.6–1.8).

use nits_protocol::{
    AgentVia, Anchor, Author, BaseRefSpec, ClientId, ClientSeq, CommentId, CommentKind,
    CommentState, CommitOid, DiffScope, EventBody, NonEmpty, RefSpec, RenderOpts, RepoId, RepoPath,
    ReviewId, ReviewStatus, ReviewTarget, ReviewTargetUpdate, Row, Side, TargetRevision,
    ThreadResolution, Timestamp, WorkspaceId,
};
use nits_review_core::comments::{lines_anchor, thread_id_of};
use nits_review_core::review::ViewedState;
use nits_review_core::{Core, CoreError, Ctx, DataDir};
use nits_test_support::{RepoBuilder, TestRepo, files};

fn human() -> Ctx {
    Ctx {
        author: Author::Human {
            name: "ada".into(),
            machine: "box".into(),
        },
        client_id: ClientId::from_parts(1, 1),
        client_seq: ClientSeq::new(0),
        now: Timestamp::from_millis(1_700_000_000_000),
    }
}
fn other_human() -> Ctx {
    Ctx {
        author: Author::Human {
            name: "bob".into(),
            machine: "box".into(),
        },
        ..human()
    }
}
fn agent() -> Ctx {
    Ctx {
        author: Author::Agent {
            name: "claude-code".into(),
            model: "claude-fable-5".into(),
            session_id: "s1".into(),
            invoked_by: None,
            via: AgentVia::Mcp,
        },
        ..human()
    }
}
fn ws() -> WorkspaceId {
    WorkspaceId::from_parts(1, 10)
}
fn rid(n: u128) -> RepoId {
    RepoId::from_parts(1, n)
}
fn review_id(n: u128) -> ReviewId {
    ReviewId::from_parts(2, n)
}
fn cid(n: u128) -> CommentId {
    CommentId::from_parts(3, n)
}
fn p(s: &str) -> RepoPath {
    RepoPath::new(s).unwrap()
}

struct World {
    _dir: tempfile::TempDir,
    data: DataDir,
    core: Core,
    a: TestRepo,
    b: TestRepo,
}

const SRC: &str = "fn main() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n    let d = 4;\n    let e = 5;\n    let f = 6;\n    let g = 7;\n    let h = 8;\n    println!(\"{}\", a + b);\n}\n";

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let data = DataDir::new(dir.path().join("nits"));
    let core = Core::open(&data).unwrap();
    let a = RepoBuilder::new()
        .commit("base", files!["src/main.rs" => SRC, "README.md" => "# a\n"])
        .branch("feature")
        .commit(
            "feat",
            files!["src/main.rs" => SRC.replace("let h = 8;", "let h = 80;"), "new.txt" => "n\n"],
        )
        .build()
        .unwrap();
    let b = RepoBuilder::new()
        .commit("base", files!["lib.rs" => "pub fn x() {}\n"])
        .branch("feature")
        .commit("feat", files!["lib.rs" => "pub fn x() {}\npub fn y() {}\n"])
        .build()
        .unwrap();
    let ctx = human();
    core.create_workspace(&ctx, ws(), "hacks".into()).unwrap();
    core.attach_repo(
        &ctx,
        ws(),
        rid(1),
        a.path().to_str().unwrap(),
        "zeta".into(),
    )
    .unwrap();
    core.attach_repo(
        &ctx,
        ws(),
        rid(2),
        b.path().to_str().unwrap(),
        "alpha".into(),
    )
    .unwrap();
    World {
        _dir: dir,
        data,
        core,
        a,
        b,
    }
}

fn targets() -> NonEmpty<ReviewTarget> {
    NonEmpty::new(vec![
        ReviewTarget {
            repo_id: rid(1),
            base: RefSpec::Branch {
                name: "main".into(),
            },
            head: RefSpec::Branch {
                name: "feature".into(),
            },
        },
        ReviewTarget {
            repo_id: rid(2),
            base: RefSpec::Branch {
                name: "main".into(),
            },
            head: RefSpec::Branch {
                name: "feature".into(),
            },
        },
    ])
    .unwrap()
}

fn head_blob(core: &Core, review: ReviewId, repo: RepoId, path: &str) -> nits_protocol::BlobOid {
    let f = core
        .file_change(review, repo, &p(path), &DiffScope::All)
        .unwrap();
    f.kind.new_blob().unwrap()
}

#[test]
fn stored_review_workspace_survives_deletion_and_reopening() {
    let w = world();
    assert!(matches!(
        w.core.stored_review_workspace(review_id(1)),
        Err(CoreError::NotFound { .. })
    ));
    w.core
        .create_review(&human(), review_id(1), ws(), "review".into(), targets())
        .unwrap();
    assert_eq!(w.core.stored_review_workspace(review_id(1)).unwrap(), ws());
    w.core.delete_review(&human(), review_id(1)).unwrap();
    assert_eq!(w.core.stored_review_workspace(review_id(1)).unwrap(), ws());
    drop(w.core);

    let core = Core::open(&w.data).unwrap();
    assert_eq!(core.stored_review_workspace(review_id(1)).unwrap(), ws());
    assert!(matches!(
        core.review(review_id(1)),
        Err(CoreError::NotFound { .. })
    ));
    assert!(matches!(
        core.review_snapshot(review_id(1)),
        Err(CoreError::NotFound { .. })
    ));
    assert!(core.reviews(ws()).unwrap().is_empty());
}

#[test]
fn explicit_review_base_is_preserved_when_remote_trunk_is_newer() {
    let w = world();
    w.a.git(&["checkout", "-q", "--detach", "main"]).unwrap();
    w.a.git(&["rm", "README.md"]).unwrap();
    w.a.git(&["commit", "-q", "-m", "unrelated trunk change"])
        .unwrap();
    w.a.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])
        .unwrap();
    w.a.git(&["checkout", "-q", "feature"]).unwrap();
    w.a.git(&["rebase", "refs/remotes/origin/main"]).unwrap();
    assert_eq!(
        w.core.default_base(rid(1)).unwrap(),
        RefSpec::Commit {
            oid: w
                .a
                .rev_parse("refs/remotes/origin/main")
                .unwrap()
                .parse()
                .unwrap()
        }
    );

    let explicit_base = RefSpec::Branch {
        name: "main".into(),
    };
    let rec = w
        .core
        .create_review(
            &human(),
            review_id(1),
            ws(),
            "explicit base".into(),
            NonEmpty::singleton(ReviewTarget {
                repo_id: rid(1),
                base: explicit_base.clone(),
                head: RefSpec::WorkingTree,
            }),
        )
        .unwrap();
    assert_eq!(rec.review.targets.first().base, explicit_base);
    let mut paths: Vec<_> = w
        .core
        .files(review_id(1))
        .unwrap()
        .into_iter()
        .map(|file| file.path.to_string())
        .collect();
    paths.sort();
    assert_eq!(paths, vec!["README.md", "new.txt", "src/main.rs"]);
}

#[test]
fn multi_repo_review_lists_files_ordered_by_repo_display_name() {
    let w = world();
    let rec = w
        .core
        .create_review(&human(), review_id(1), ws(), "r".into(), targets())
        .unwrap();
    assert!(rec.resolved.is_some());
    let files: Vec<(RepoId, String)> = w
        .core
        .files(review_id(1))
        .unwrap()
        .into_iter()
        .map(|f| (f.repo_id, f.path.to_string()))
        .collect();
    assert_eq!(
        files,
        vec![
            (rid(2), "lib.rs".into()),
            // Tree display order within a repo: dirs before files.
            (rid(1), "src/main.rs".into()),
            (rid(1), "new.txt".into())
        ],
        "alpha (repo 2) sorts before zeta (repo 1)"
    );
    // Unknown repo in targets is rejected up front.
    let bad = NonEmpty::singleton(ReviewTarget {
        repo_id: rid(9),
        base: RefSpec::Head,
        head: RefSpec::Head,
    });
    assert!(matches!(
        w.core
            .create_review(&human(), review_id(2), ws(), "x".into(), bad),
        Err(CoreError::Invalid { .. })
    ));
}

#[test]
fn updating_one_repo_target_is_typed_resolved_and_persisted() {
    let w = world();
    w.core
        .create_review(&human(), review_id(1), ws(), "r".into(), targets())
        .unwrap();
    let before = w.core.last_seq().unwrap();
    w.core
        .update_review_target(
            &human(),
            review_id(1),
            ReviewTargetUpdate {
                repo_id: rid(2),
                revision: TargetRevision::Base {
                    ref_spec: BaseRefSpec::Head,
                },
            },
        )
        .unwrap();

    let review = w.core.review(review_id(1)).unwrap();
    let changed = review
        .review
        .targets
        .iter()
        .find(|target| target.repo_id == rid(2))
        .unwrap();
    assert_eq!(changed.base, RefSpec::Head);
    let events = w.core.events_after(before).unwrap();
    assert!(matches!(
        events.first().map(|event| &event.body),
        Some(EventBody::ReviewTargetUpdated { target, .. }) if target.repo_id == rid(2)
    ));
    assert!(
        events
            .iter()
            .any(|event| matches!(event.body, EventBody::ReviewTargetsResolved { .. }))
    );

    let last = w.core.last_seq().unwrap();
    let error = w
        .core
        .update_review_target(
            &human(),
            review_id(1),
            ReviewTargetUpdate {
                repo_id: rid(2),
                revision: TargetRevision::Head {
                    ref_spec: RefSpec::Branch {
                        name: "missing".into(),
                    },
                },
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("missing"), "{error}");
    assert!(matches!(error, CoreError::Invalid { .. }));
    assert_eq!(
        w.core.last_seq().unwrap(),
        last,
        "invalid refs append no event"
    );
}

#[test]
fn re_resolve_is_idempotent_and_emits_on_change() {
    let w = world();
    w.core
        .create_review(&human(), review_id(1), ws(), "r".into(), targets())
        .unwrap();
    let before = w.core.last_seq().unwrap();
    let (_, changed) = w.core.resolve_targets(&human(), review_id(1)).unwrap();
    assert!(!changed);
    assert_eq!(
        w.core.last_seq().unwrap(),
        before,
        "no duplicate ReviewTargetsResolved"
    );

    w.a.write_file(
        "src/main.rs",
        SRC.replace("let h = 8;", "let h = 800;").as_bytes(),
    )
    .unwrap();
    w.a.git(&["commit", "-qam", "more"]).unwrap();
    let (_, changed) = w.core.resolve_targets(&human(), review_id(1)).unwrap();
    assert!(changed);
    let last = w.core.events_after(before).unwrap();
    assert!(matches!(
        last[0].body,
        EventBody::ReviewTargetsResolved { .. }
    ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn archived_working_tree_reviews_preserve_history_and_refresh_on_reopen() {
    let w = world();
    let review = review_id(1);
    w.core
        .create_review(
            &human(),
            review,
            ws(),
            "working tree".into(),
            NonEmpty::singleton(ReviewTarget {
                repo_id: rid(1),
                base: RefSpec::Branch {
                    name: "main".into(),
                },
                head: RefSpec::WorkingTree,
            }),
        )
        .unwrap();
    let blob = head_blob(&w.core, review, rid(1), "src/main.rs");
    w.core
        .add_comment(
            &human(),
            review,
            cid(1),
            CommentKind::Note,
            lines_anchor(rid(1), p("src/main.rs"), Side::Head, blob, 8, 8).unwrap(),
            "keep this discussion".into(),
            None,
        )
        .unwrap();
    let original = w.core.review_snapshot(review).unwrap();
    let candidates = w.core.working_tree_reviews(rid(1)).unwrap();
    assert_eq!(candidates, vec![review]);
    w.core
        .update_review(&human(), review, "archived".into(), ReviewStatus::Archived)
        .unwrap();
    let archived_at = w.core.last_seq().unwrap();
    let shifted = format!(
        "// header\n// header2\n{}",
        SRC.replace("let h = 8;", "let h = 80;")
    );
    w.a.write_file("src/main.rs", shifted.as_bytes()).unwrap();
    // A watcher that selected this review before archival must still skip it.
    for candidate in candidates {
        w.core
            .refresh_working_tree_review(&human(), candidate, rid(1))
            .unwrap();
    }
    assert_eq!(w.core.last_seq().unwrap(), archived_at);
    assert!(w.core.working_tree_reviews(rid(1)).unwrap().is_empty());

    drop(w.core);
    let core = Core::open(&w.data).unwrap();
    let archived = core.review_snapshot(review).unwrap();
    assert_eq!(archived.review.status, ReviewStatus::Archived);
    assert_eq!(archived.resolved, original.resolved);
    assert_eq!(archived.comments, original.comments);
    assert_eq!(archived.threads, original.threads);
    assert!(core.working_tree_reviews(rid(1)).unwrap().is_empty());
    core.blob_render(rid(1), &p("src/main.rs"), blob).unwrap();

    core.update_review(&human(), review, "reopened".into(), ReviewStatus::Open)
        .unwrap();
    let events = core.events_after(archived_at).unwrap();
    assert_eq!(events.len(), 3);
    assert!(matches!(
        events[0].body,
        EventBody::ReviewUpdated {
            status: ReviewStatus::Open,
            ..
        }
    ));
    assert!(matches!(
        events[1].body,
        EventBody::ReviewTargetsResolved { .. }
    ));
    assert!(matches!(
        events[2].body,
        EventBody::CommentReanchored { .. }
    ));
    let reopened = core.review_snapshot(review).unwrap();
    assert_ne!(reopened.resolved, original.resolved);
    assert_eq!(reopened.comments.len(), 1);
    let comment = &reopened.comments[0];
    assert_eq!(comment.id, original.comments[0].id);
    assert_eq!(comment.body, original.comments[0].body);
    assert_eq!(comment.state, CommentState::Live);
    let Anchor::Lines {
        lines, blob_oid, ..
    } = &comment.anchor
    else {
        panic!("expected line anchor")
    };
    assert_eq!(lines.start().get(), 10);
    assert_ne!(*blob_oid, blob);
    assert_eq!(reopened.threads, original.threads);
    assert_eq!(core.working_tree_reviews(rid(1)).unwrap(), vec![review]);
    // Old anchor blobs remain readable after refreshing as well.
    core.blob_render(rid(1), &p("src/main.rs"), blob).unwrap();
}

#[test]
fn reopening_unchanged_review_emits_only_metadata_and_missing_ref_keeps_it_archived() {
    let w = world();
    w.core
        .create_review(&human(), review_id(1), ws(), "r".into(), targets())
        .unwrap();
    w.core
        .update_review(&human(), review_id(1), "r".into(), ReviewStatus::Archived)
        .unwrap();
    let before = w.core.last_seq().unwrap();
    w.core
        .update_review(&human(), review_id(1), "r".into(), ReviewStatus::Open)
        .unwrap();
    let events = w.core.events_after(before).unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0].body, EventBody::ReviewUpdated { .. }));

    w.core
        .update_review(&human(), review_id(1), "r".into(), ReviewStatus::Archived)
        .unwrap();
    let archived = w.core.review_snapshot(review_id(1)).unwrap();
    w.a.git(&["checkout", "main"]).unwrap();
    w.a.git(&["branch", "-D", "feature"]).unwrap();
    assert!(
        w.core
            .update_review(
                &human(),
                review_id(1),
                "reopened".into(),
                ReviewStatus::Open
            )
            .is_err()
    );
    assert_eq!(w.core.review_snapshot(review_id(1)).unwrap(), archived);
}

#[test]
fn working_tree_refresh_candidates_require_open_status_and_the_matching_repo() {
    let w = world();
    for (review, base, head) in [
        (review_id(1), RefSpec::Head, RefSpec::WorkingTree),
        (review_id(2), RefSpec::WorkingTree, RefSpec::Head),
        (review_id(3), RefSpec::Head, RefSpec::Head),
        (review_id(4), RefSpec::Head, RefSpec::WorkingTree),
        (review_id(5), RefSpec::Head, RefSpec::WorkingTree),
    ] {
        w.core
            .create_review(
                &human(),
                review,
                ws(),
                "r".into(),
                NonEmpty::singleton(ReviewTarget {
                    repo_id: rid(1),
                    base,
                    head,
                }),
            )
            .unwrap();
    }
    w.core
        .update_review(&human(), review_id(4), "r".into(), ReviewStatus::Archived)
        .unwrap();
    w.core.delete_review(&human(), review_id(5)).unwrap();
    assert_eq!(
        w.core.working_tree_reviews(rid(1)).unwrap(),
        vec![review_id(1), review_id(2)]
    );
    assert!(w.core.working_tree_reviews(rid(2)).unwrap().is_empty());
    let before = w.core.last_seq().unwrap();
    w.a.write_file("new.txt", b"changed\n").unwrap();
    w.core
        .refresh_working_tree_review(&human(), review_id(1), rid(2))
        .unwrap();
    w.core
        .refresh_working_tree_review(&human(), review_id(3), rid(1))
        .unwrap();
    assert_eq!(w.core.last_seq().unwrap(), before);
    // Explicit refresh is still available for archived reviews.
    assert!(w.core.resolve_targets(&human(), review_id(4)).unwrap().1);
    assert_eq!(
        w.core.review(review_id(4)).unwrap().review.status,
        ReviewStatus::Archived
    );
}

#[test]
fn commit_stepping_yields_parent_based_targets_with_full_messages() {
    let w = world();
    w.a.write_file("src/main.rs", b"changed\n").unwrap();
    w.a.git(&[
        "commit",
        "-qam",
        "second: subject\n\nbody paragraph\n\nmore body",
    ])
    .unwrap();
    w.core
        .create_review(&human(), review_id(1), ws(), "r".into(), targets())
        .unwrap();
    let commits = w.core.commits(review_id(1), rid(1)).unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].subject, "second: subject");
    assert_eq!(commits[0].body, "body paragraph\n\nmore body");
    assert_eq!(commits[0].author.name, "Test User");
    let step = w.core.commit_step(rid(1), commits[0].oid).unwrap();
    assert_eq!(step.head.tree, commits[0].tree);
    assert_eq!(step.base.tree, commits[1].tree, "base is the first parent");
    // Working-tree targets have no commit range.
    let wt = NonEmpty::singleton(ReviewTarget {
        repo_id: rid(1),
        base: RefSpec::Head,
        head: RefSpec::WorkingTree,
    });
    w.core
        .create_review(&human(), review_id(2), ws(), "wt".into(), wt)
        .unwrap();
    assert!(w.core.commits(review_id(2), rid(1)).unwrap().is_empty());
}

#[test]
fn viewed_marks_track_the_head_blob_and_reject_agents() {
    let w = world();
    w.core
        .create_review(&human(), review_id(1), ws(), "r".into(), targets())
        .unwrap();
    let err = w
        .core
        .mark_viewed(&agent(), review_id(1), rid(1), p("src/main.rs"))
        .unwrap_err();
    assert!(matches!(err, CoreError::Forbidden { .. }), "{err}");

    w.core
        .mark_viewed(&human(), review_id(1), rid(1), p("src/main.rs"))
        .unwrap();
    assert_eq!(
        w.core
            .viewed_state(review_id(1), rid(1), &p("src/main.rs"))
            .unwrap(),
        ViewedState::Viewed
    );

    // Head moves without touching the file: still viewed.
    w.a.write_file("other.txt", b"o\n").unwrap();
    w.a.git(&["add", "."]).unwrap();
    w.a.git(&["commit", "-qm", "unrelated"]).unwrap();
    w.core.resolve_targets(&human(), review_id(1)).unwrap();
    assert_eq!(
        w.core
            .viewed_state(review_id(1), rid(1), &p("src/main.rs"))
            .unwrap(),
        ViewedState::Viewed
    );

    // Head moves and touches the file: changed since viewed.
    let marked = head_blob(&w.core, review_id(1), rid(1), "src/main.rs");
    w.a.write_file("src/main.rs", b"totally new\n").unwrap();
    w.a.git(&["commit", "-qam", "touch"]).unwrap();
    w.core.resolve_targets(&human(), review_id(1)).unwrap();
    assert_eq!(
        w.core
            .viewed_state(review_id(1), rid(1), &p("src/main.rs"))
            .unwrap(),
        ViewedState::ChangedSinceViewed {
            marked: Some(marked)
        }
    );
    w.core
        .unmark_viewed(&human(), review_id(1), rid(1), p("src/main.rs"))
        .unwrap();
    assert_eq!(
        w.core
            .viewed_state(review_id(1), rid(1), &p("src/main.rs"))
            .unwrap(),
        ViewedState::Unviewed
    );
}

#[test]
fn comments_validate_anchors_and_stamp_context_hash() {
    let w = world();
    w.core
        .create_review(&human(), review_id(1), ws(), "r".into(), targets())
        .unwrap();
    let blob = head_blob(&w.core, review_id(1), rid(1), "src/main.rs");
    let anchor = lines_anchor(rid(1), p("src/main.rs"), Side::Head, blob, 9, 9).unwrap();
    let c = w
        .core
        .add_comment(
            &human(),
            review_id(1),
            cid(1),
            CommentKind::Note,
            anchor,
            "why 80?".into(),
            None,
        )
        .unwrap();
    let Anchor::Lines { context_hash, .. } = c.anchor else {
        panic!()
    };
    assert_ne!(context_hash.get(), 0, "daemon stamps the hash");
    assert_eq!(c.thread_id, thread_id_of(cid(1)));

    let too_far = lines_anchor(rid(1), p("src/main.rs"), Side::Head, blob, 1, 999).unwrap();
    let err = w
        .core
        .add_comment(
            &human(),
            review_id(1),
            cid(2),
            CommentKind::Note,
            too_far,
            "x".into(),
            None,
        )
        .unwrap_err();
    assert!(matches!(err, CoreError::Invalid { .. }), "{err}");

    let wrong_repo = lines_anchor(rid(9), p("src/main.rs"), Side::Head, blob, 1, 1).unwrap();
    assert!(matches!(
        w.core.add_comment(
            &human(),
            review_id(1),
            cid(3),
            CommentKind::Note,
            wrong_repo,
            "x".into(),
            None,
        ),
        Err(CoreError::Invalid { .. })
    ));
}

#[test]
fn threads_replies_permissions_and_resolution() {
    let w = world();
    w.core
        .create_review(&human(), review_id(1), ws(), "r".into(), targets())
        .unwrap();
    w.core
        .add_comment(
            &human(),
            review_id(1),
            cid(1),
            CommentKind::Note,
            Anchor::Review,
            "overall".into(),
            None,
        )
        .unwrap();
    let th = thread_id_of(cid(1));
    let reply = w
        .core
        .reply(
            &agent(),
            review_id(1),
            th,
            cid(2),
            CommentKind::Note,
            "ack".into(),
        )
        .unwrap();
    assert_eq!(reply.anchor, Anchor::Review);
    assert!(matches!(reply.author, Author::Agent { .. }));

    // Only the author edits/deletes.
    assert!(matches!(
        w.core
            .edit_comment(&other_human(), review_id(1), cid(1), "x".into()),
        Err(CoreError::Forbidden { .. })
    ));
    w.core
        .edit_comment(&human(), review_id(1), cid(1), "edited".into())
        .unwrap();
    let c = w
        .core
        .comments(review_id(1))
        .unwrap()
        .into_iter()
        .find(|c| c.id == cid(1))
        .unwrap();
    assert_eq!(c.body, "edited");
    assert!(c.edited.is_some());

    w.core.resolve_thread(&human(), review_id(1), th).unwrap();
    assert!(matches!(
        w.core.resolve_thread(&human(), review_id(1), th),
        Err(CoreError::Invalid { .. })
    ));
    let t = w.core.threads(review_id(1)).unwrap().pop().unwrap();
    assert!(matches!(t.resolution, ThreadResolution::Resolved { .. }));
    assert_eq!(t.replies, vec![cid(2)]);
    w.core.unresolve_thread(&human(), review_id(1), th).unwrap();

    w.core
        .delete_comment(&agent(), review_id(1), cid(2))
        .unwrap();
    assert!(matches!(
        w.core.reply(
            &human(),
            review_id(1),
            thread_id_of(cid(2)),
            cid(3),
            CommentKind::Note,
            "?".into()
        ),
        Err(CoreError::NotFound { .. })
    ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn comments_reanchor_when_head_moves() {
    let w = world();
    w.core
        .create_review(&human(), review_id(1), ws(), "r".into(), targets())
        .unwrap();
    let blob = head_blob(&w.core, review_id(1), rid(1), "src/main.rs");
    // Anchor on "let g = 7;" (line 8).
    let anchor = lines_anchor(rid(1), p("src/main.rs"), Side::Head, blob, 8, 8).unwrap();
    w.core
        .add_comment(
            &human(),
            review_id(1),
            cid(1),
            CommentKind::Note,
            anchor,
            "g".into(),
            None,
        )
        .unwrap();
    // File-level comment on README (not in the diff).
    let readme_blob = w
        .core
        .tree_snapshot(
            rid(1),
            &RefSpec::Branch {
                name: "feature".into(),
            },
        )
        .unwrap()
        .entries
        .into_iter()
        .find(|e| e.path.as_str() == "README.md")
        .map(|e| match e.kind {
            nits_protocol::TreeEntryKind::File { oid, .. } => oid,
            _ => panic!(),
        })
        .unwrap();
    w.core
        .add_comment(
            &human(),
            review_id(1),
            cid(2),
            CommentKind::Note,
            Anchor::File {
                repo_id: rid(1),
                path: p("README.md"),
                blob_oid: readme_blob,
            },
            "readme".into(),
            None,
        )
        .unwrap();

    // 1) Insert two lines at the top of the file: comment shifts, stays live.
    let shifted = format!(
        "// header\n// header2\n{}",
        SRC.replace("let h = 8;", "let h = 80;")
    );
    w.a.write_file("src/main.rs", shifted.as_bytes()).unwrap();
    w.a.git(&["commit", "-qam", "shift"]).unwrap();
    w.core.resolve_targets(&human(), review_id(1)).unwrap();
    let c = w
        .core
        .comments(review_id(1))
        .unwrap()
        .into_iter()
        .find(|c| c.id == cid(1))
        .unwrap();
    let Anchor::Lines {
        lines, blob_oid, ..
    } = &c.anchor
    else {
        panic!()
    };
    assert_eq!(lines.start().get(), 10);
    assert_eq!(
        *blob_oid,
        head_blob(&w.core, review_id(1), rid(1), "src/main.rs")
    );
    assert_eq!(c.state, CommentState::Live);
    let readme = w
        .core
        .comments(review_id(1))
        .unwrap()
        .into_iter()
        .find(|c| c.id == cid(2))
        .unwrap();
    assert_eq!(
        readme.state,
        CommentState::Live,
        "untouched file-level comment is unchanged"
    );

    // 2) Edit the anchored line: outdated, keeps last good anchor.
    let edited = shifted.replace("let g = 7;", "let g = 70;");
    w.a.write_file("src/main.rs", edited.as_bytes()).unwrap();
    w.a.git(&["commit", "-qam", "edit"]).unwrap();
    w.core.resolve_targets(&human(), review_id(1)).unwrap();
    let c = w
        .core
        .comments(review_id(1))
        .unwrap()
        .into_iter()
        .find(|c| c.id == cid(1))
        .unwrap();
    let CommentState::Outdated {
        last_good_anchor: Anchor::Lines { lines, .. },
    } = &c.state
    else {
        panic!("{:?}", c.state)
    };
    assert_eq!(lines.start().get(), 10);

    // 3) Revert the edit: comment comes back to life.
    w.a.write_file("src/main.rs", shifted.as_bytes()).unwrap();
    w.a.git(&["commit", "-qam", "revert"]).unwrap();
    w.core.resolve_targets(&human(), review_id(1)).unwrap();
    let c = w
        .core
        .comments(review_id(1))
        .unwrap()
        .into_iter()
        .find(|c| c.id == cid(1))
        .unwrap();
    assert_eq!(c.state, CommentState::Live);

    // 4) Rename the file: both comments follow the new path.
    w.a.git(&["mv", "src/main.rs", "src/app.rs"]).unwrap();
    w.a.git(&["mv", "README.md", "README.txt"]).unwrap();
    w.a.git(&["commit", "-qm", "rename"]).unwrap();
    w.core.resolve_targets(&human(), review_id(1)).unwrap();
    let cs = w.core.comments(review_id(1)).unwrap();
    let c1 = cs.iter().find(|c| c.id == cid(1)).unwrap();
    let Anchor::Lines { path, .. } = &c1.anchor else {
        panic!()
    };
    assert_eq!(path.as_str(), "src/app.rs");
    let c2 = cs.iter().find(|c| c.id == cid(2)).unwrap();
    let Anchor::File { path, .. } = &c2.anchor else {
        panic!()
    };
    assert_eq!(path.as_str(), "README.txt");

    // 5) Delete the file: outdated.
    w.a.git(&["rm", "-q", "src/app.rs"]).unwrap();
    w.a.git(&["commit", "-qm", "rm"]).unwrap();
    w.core.resolve_targets(&human(), review_id(1)).unwrap();
    let c = w
        .core
        .comments(review_id(1))
        .unwrap()
        .into_iter()
        .find(|c| c.id == cid(1))
        .unwrap();
    assert!(matches!(c.state, CommentState::Outdated { .. }));
}

#[test]
fn base_side_anchor_survives_base_move() {
    let w = world();
    w.core
        .create_review(&human(), review_id(1), ws(), "r".into(), targets())
        .unwrap();
    let old_blob = w
        .core
        .file_change(review_id(1), rid(1), &p("src/main.rs"), &DiffScope::All)
        .unwrap()
        .kind
        .old_blob()
        .unwrap();
    let anchor = lines_anchor(rid(1), p("src/main.rs"), Side::Base, old_blob, 8, 8).unwrap();
    w.core
        .add_comment(
            &human(),
            review_id(1),
            cid(1),
            CommentKind::Note,
            anchor,
            "base side".into(),
            None,
        )
        .unwrap();
    // Move main forward with an insert above.
    w.a.git(&["checkout", "-q", "main"]).unwrap();
    w.a.write_file("src/main.rs", format!("// top\n{SRC}").as_bytes())
        .unwrap();
    w.a.git(&["commit", "-qam", "base moves"]).unwrap();
    w.a.git(&["checkout", "-q", "feature"]).unwrap();
    w.core.resolve_targets(&human(), review_id(1)).unwrap();
    let c = w.core.comments(review_id(1)).unwrap().pop().unwrap();
    let Anchor::Lines { lines, side, .. } = &c.anchor else {
        panic!()
    };
    assert_eq!(*side, Side::Base);
    assert_eq!(lines.start().get(), 9);
    assert_eq!(c.state, CommentState::Live);
}

#[test]
fn suggestion_applies_once_to_the_working_tree() {
    let w = world();
    w.core
        .create_review(&human(), review_id(1), ws(), "r".into(), targets())
        .unwrap();
    // feature is checked out in repo a; suggest on its head blob.
    let blob = head_blob(&w.core, review_id(1), rid(1), "src/main.rs");
    let anchor = lines_anchor(rid(1), p("src/main.rs"), Side::Head, blob, 2, 2).unwrap();
    let patch = "@@ -2,1 +2,1 @@\n-    let a = 1;\n+    let a = 10;\n".to_string();
    w.core
        .add_comment(
            &agent(),
            review_id(1),
            cid(1),
            CommentKind::Suggestion { patch },
            anchor,
            "ten".into(),
            None,
        )
        .unwrap();
    let result = w
        .core
        .apply_suggestion(&human(), review_id(1), cid(1))
        .unwrap();
    let on_disk = std::fs::read_to_string(w.a.path().join("src/main.rs")).unwrap();
    assert!(on_disk.contains("let a = 10;"));
    assert_eq!(
        w.core.repo_blob(rid(1), result).unwrap(),
        on_disk.as_bytes()
    );
    let err = w
        .core
        .apply_suggestion(&human(), review_id(1), cid(1))
        .unwrap_err();
    assert!(
        matches!(err, CoreError::Invalid { .. }),
        "second apply: {err}"
    );
    assert!(
        w.core
            .events_after(None)
            .unwrap()
            .iter()
            .any(|e| matches!(e.body, EventBody::SuggestionApplied { .. }))
    );
}

#[test]
fn file_render_and_snapshot_and_reopen() {
    let w = world();
    w.core
        .create_review(&human(), review_id(1), ws(), "r".into(), targets())
        .unwrap();
    let (header, rendered) = w
        .core
        .file_render(
            review_id(1),
            rid(1),
            &p("src/main.rs"),
            RenderOpts::default(),
            &DiffScope::All,
        )
        .unwrap();
    assert_eq!(header.lang.as_deref(), Some("Rust"));
    assert!(
        rendered
            .rows
            .iter()
            .any(|r| matches!(r, Row::Modified { .. }))
    );
    // Second call is served from the cache and identical.
    let again = w
        .core
        .file_render(
            review_id(1),
            rid(1),
            &p("src/main.rs"),
            RenderOpts::default(),
            &DiffScope::All,
        )
        .unwrap();
    assert_eq!(again, (header.clone(), rendered.clone()));
    // Blob render for the explorer.
    let (bh, br) = w
        .core
        .blob_render(
            rid(1),
            &p("src/main.rs"),
            match &header.target {
                nits_protocol::RenderTarget::Diff { change } => change.new_blob().unwrap(),
                nits_protocol::RenderTarget::Blob { oid } => *oid,
            },
        )
        .unwrap();
    assert!(br.rows.iter().all(|r| matches!(r, Row::Context { .. })));
    assert_eq!(bh.lang.as_deref(), Some("Rust"));
    assert!(matches!(
        w.core.file_render(
            review_id(1),
            rid(1),
            &p("nope.rs"),
            RenderOpts::default(),
            &DiffScope::All,
        ),
        Err(CoreError::NotFound { .. })
    ));

    w.core
        .add_comment(
            &human(),
            review_id(1),
            cid(1),
            CommentKind::Note,
            Anchor::Review,
            "hi".into(),
            None,
        )
        .unwrap();
    w.core
        .mark_viewed(&human(), review_id(1), rid(2), p("lib.rs"))
        .unwrap();
    let snap = w.core.review_snapshot(review_id(1)).unwrap();
    assert_eq!(snap.comments.len(), 1);
    assert_eq!(snap.threads.len(), 1);
    assert_eq!(snap.viewed.len(), 1);
    assert_eq!(Some(snap.seq), w.core.last_seq().unwrap());

    // Reopen from the same data dir: everything persists; repos reopen lazily.
    drop(w.core);
    let core = Core::open(&w.data).unwrap();
    assert_eq!(core.review_snapshot(review_id(1)).unwrap(), snap);
    assert_eq!(core.files(review_id(1)).unwrap().len(), 3);
    core.delete_review(&human(), review_id(1)).unwrap();
    assert!(core.reviews(ws()).unwrap().is_empty());
    assert!(matches!(
        core.review(review_id(1)),
        Err(CoreError::NotFound { .. })
    ));
    drop(w.b);
}

#[test]
fn diff_scopes_narrow_files_and_step_a_worktree_review() {
    let w = world();
    // A worktree-headed review: main → working tree, with one commit on
    // top of main and uncommitted changes on top of that.
    w.a.git(&["checkout", "-q", "main"]).unwrap();
    let base_oid = CommitOid::new(w.a.rev_parse("HEAD").unwrap().parse().unwrap());
    w.a.write_file("src/main.rs", b"fn main() {}\n").unwrap();
    w.a.git(&["commit", "-qam", "tip"]).unwrap();
    w.a.write_file("uncommitted.txt", b"dirty\n").unwrap();
    let wt = NonEmpty::singleton(ReviewTarget {
        repo_id: rid(1),
        base: RefSpec::Commit { oid: base_oid },
        head: RefSpec::WorkingTree,
    });
    w.core
        .create_review(&human(), review_id(3), ws(), "wt".into(), wt)
        .unwrap();
    // The commit range steps through the checked-out branch even though
    // the head is the working tree.
    let commits = w.core.commits(review_id(3), rid(1)).unwrap();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].subject, "tip");
    // All: committed + uncommitted changes.
    let (all, _) = w.core.files_scoped(review_id(3), &DiffScope::All).unwrap();
    let paths = |fs: &[nits_protocol::FileChange]| {
        fs.iter().map(|f| f.path.to_string()).collect::<Vec<_>>()
    };
    assert_eq!(paths(&all), ["src/main.rs", "uncommitted.txt"]);
    // Committed: the working-tree head stops at the checked-out commit.
    let (committed, resolved) = w
        .core
        .files_scoped(review_id(3), &DiffScope::Committed)
        .unwrap();
    assert_eq!(paths(&committed), ["src/main.rs"]);
    assert!(matches!(
        resolved.first().head.source,
        nits_protocol::ResolvedSource::Commit { .. }
    ));
    // Commit: the tip against its parent.
    let (step, _) = w
        .core
        .files_scoped(
            review_id(3),
            &DiffScope::Commit {
                repo_id: rid(1),
                oid: commits[0].oid,
            },
        )
        .unwrap();
    assert_eq!(paths(&step), ["src/main.rs"]);
    // Worktree: only the uncommitted changes (the final step).
    let (dirty, _) = w
        .core
        .files_scoped(review_id(3), &DiffScope::Worktree { repo_id: rid(1) })
        .unwrap();
    assert_eq!(paths(&dirty), ["uncommitted.txt"]);
    // A scoped render agrees with the scoped file list.
    let err = w
        .core
        .file_render(
            review_id(3),
            rid(1),
            &p("uncommitted.txt"),
            RenderOpts::default(),
            &DiffScope::Committed,
        )
        .unwrap_err();
    assert!(matches!(err, CoreError::NotFound { .. }));
    w.core
        .file_render(
            review_id(3),
            rid(1),
            &p("uncommitted.txt"),
            RenderOpts::default(),
            &DiffScope::Worktree { repo_id: rid(1) },
        )
        .unwrap();
}

#[test]
fn an_unresolvable_target_leaves_no_ghost_review() {
    let w = world();
    // No upstream is configured in the test repo: the create must fail
    // whole, committing nothing.
    let t = NonEmpty::singleton(ReviewTarget {
        repo_id: rid(1),
        base: RefSpec::Upstream,
        head: RefSpec::WorkingTree,
    });
    let err = w
        .core
        .create_review(&human(), review_id(9), ws(), "ghost".into(), t)
        .unwrap_err();
    let _ = err;
    assert!(
        w.core.reviews(ws()).unwrap().is_empty(),
        "a failed create leaves no review behind"
    );
}

#[test]
fn content_search_scans_changed_files_or_the_whole_head_tree() {
    let w = world();
    w.core
        .create_review(&human(), review_id(1), ws(), "r".into(), targets())
        .unwrap();
    // "let h" appears in the changed src/main.rs; README.md is unchanged.
    let (hits, truncated) = w
        .core
        .search(review_id(1), "LET H", false, &DiffScope::All)
        .unwrap();
    assert!(!truncated);
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|h| h.path.as_str() == "src/main.rs"));
    // All-files search also reaches unchanged files.
    let (hits, _) = w
        .core
        .search(review_id(1), "# a", true, &DiffScope::All)
        .unwrap();
    assert!(
        hits.iter().any(|h| h.path.as_str() == "README.md"),
        "{hits:?}"
    );
    // An empty query matches nothing.
    let (hits, _) = w
        .core
        .search(review_id(1), "  ", true, &DiffScope::All)
        .unwrap();
    assert!(hits.is_empty());
}

#[test]
fn directory_bootstrap_discovers_nested_paths_without_partial_invalid_refs() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(&DataDir::new(dir.path())).unwrap();
    let repo = RepoBuilder::new()
        .commit("initial", files!["nested/file.txt" => "hello\n"])
        .build()
        .unwrap();
    let mut options = nits_protocol::EnsureDirectoryReview {
        workspace_id: ws(),
        repo_id: rid(1),
        review_id: review_id(1),
        path: repo.path().join("nested").to_string_lossy().into_owned(),
        base: Some(BaseRefSpec::Branch {
            name: "missing".into(),
        }),
        head: None,
    };
    assert!(
        core.ensure_directory_review(&human(), options.clone())
            .is_err()
    );
    assert!(core.workspaces().unwrap().is_empty());
    assert_eq!(core.last_seq().unwrap(), None);
    options.base = Some(BaseRefSpec::Head);
    let first = core
        .ensure_directory_review(&human(), options.clone())
        .unwrap();
    assert_eq!(
        first.outcome,
        nits_protocol::DirectoryReviewOutcome::Created
    );
    options.path = repo.path().to_string_lossy().into_owned();
    let before = core.last_seq().unwrap();
    let again = core.ensure_directory_review(&human(), options).unwrap();
    assert_eq!(again.review_id, first.review_id);
    assert_eq!(again.outcome, nits_protocol::DirectoryReviewOutcome::Reused);
    assert_eq!(before, core.last_seq().unwrap());
}

#[test]
#[allow(clippy::too_many_lines)] // one lifecycle scenario through persistence or transport
fn informational_summary_is_conversation_not_an_open_finding() {
    let w = world();
    let review = review_id(1);
    w.core
        .create_review(&human(), review, ws(), "review".into(), targets())
        .unwrap();
    let blob = head_blob(&w.core, review, rid(1), "src/main.rs");
    for n in 1..=3 {
        w.core
            .add_comment(
                &agent(),
                review,
                cid(n),
                CommentKind::Note,
                lines_anchor(rid(1), p("src/main.rs"), Side::Head, blob, 2, 2).unwrap(),
                format!("finding {n}"),
                None,
            )
            .unwrap();
    }
    let summary = w
        .core
        .add_comment(
            &agent(),
            review,
            cid(4),
            CommentKind::Informational,
            Anchor::Review,
            "Three findings; review is in progress".into(),
            None,
        )
        .unwrap();
    assert_eq!(
        w.core
            .threads(review)
            .unwrap()
            .iter()
            .filter(|t| t.resolution == ThreadResolution::Open)
            .count(),
        3
    );
    let reply = w
        .core
        .reply(
            &other_human(),
            review,
            summary.thread_id,
            cid(5),
            CommentKind::Note,
            "Two findings fixed; one remains".into(),
        )
        .unwrap();
    assert_eq!(reply.author, other_human().author);
    assert_eq!(reply.anchor, Anchor::Review);
    for n in 1..=2 {
        w.core
            .resolve_thread(&human(), review, thread_id_of(cid(n)))
            .unwrap();
    }
    let before = w.core.events_after(None).unwrap();
    assert!(
        w.core
            .resolve_thread(&human(), review, summary.thread_id)
            .is_err()
    );
    assert!(
        w.core
            .unresolve_thread(&human(), review, summary.thread_id)
            .is_err()
    );
    assert_eq!(w.core.events_after(None).unwrap(), before);
    assert!(
        w.core
            .add_comment(
                &human(),
                review,
                cid(6),
                CommentKind::Informational,
                lines_anchor(rid(1), p("src/main.rs"), Side::Head, blob, 2, 2).unwrap(),
                "invalid".into(),
                None
            )
            .is_err()
    );
    assert_eq!(
        w.core
            .threads(review)
            .unwrap()
            .iter()
            .filter(|t| t.resolution == ThreadResolution::Open)
            .count(),
        1
    );
    w.core
        .add_comment(
            &human(),
            review,
            cid(7),
            CommentKind::Note,
            Anchor::Review,
            "Review-wide actionable concern".into(),
            None,
        )
        .unwrap();
    let threads = w.core.threads(review).unwrap();
    assert_eq!(
        threads
            .iter()
            .filter(|t| t.resolution == ThreadResolution::Open)
            .count(),
        2
    );
    let note = threads.iter().find(|t| t.id == summary.thread_id).unwrap();
    assert_eq!(note.resolution, ThreadResolution::Informational);
    assert_eq!(note.replies, vec![reply.id]);
    assert_eq!(
        w.core.review(review).unwrap().review.status,
        ReviewStatus::Open
    );
}

#[test]
fn review_requests_are_durable_state_separate_from_findings() {
    let w = world();
    w.core
        .create_review(&human(), review_id(1), ws(), "review".into(), targets())
        .unwrap();
    w.core
        .request_review(
            &human(),
            review_id(1),
            "review-agent".into(),
            "Review the parser".into(),
        )
        .unwrap();
    let snapshot = w.core.review_snapshot(review_id(1)).unwrap();
    assert_eq!(snapshot.requests.len(), 1);
    let request = &snapshot.requests[0];
    assert_eq!(request.review_id, review_id(1));
    assert_eq!(request.requester, human().author);
    assert_eq!(request.recipient, "review-agent");
    assert_eq!(request.note, "Review the parser");
    assert_eq!(request.created, human().now);
    assert_eq!(request.id.event_seq(), snapshot.seq);
    assert!(snapshot.threads.is_empty());
    assert!(snapshot.comments.is_empty());
    drop(w.core);
    let core = Core::open(&w.data).unwrap();
    assert_eq!(core.review_snapshot(review_id(1)).unwrap(), snapshot);
    core.delete_review(&human(), review_id(1)).unwrap();
    assert!(matches!(
        core.review_snapshot(review_id(1)),
        Err(CoreError::NotFound { .. })
    ));
}

#[test]
fn browse_comments_pin_arbitrary_ref_blobs_through_review_refresh_and_restart() {
    use nits_protocol::{CommentContext, TreeEntryKind};
    let w = world();
    w.core
        .create_review(&human(), review_id(1), ws(), "r".into(), targets())
        .unwrap();
    // A separate history, with a file wholly absent from the review's diff.
    w.a.git(&["checkout", "-b", "archive", "main"]).unwrap();
    w.a.write_file("unchanged.txt", b"one\ntwo\nthree\n")
        .unwrap();
    w.a.git(&["add", "."]).unwrap();
    w.a.git(&["commit", "-m", "archive content"]).unwrap();
    w.a.git(&["tag", "v1"]).unwrap();
    let oid: CommitOid =
        w.a.git(&["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .parse()
            .unwrap();
    let references = [
        RefSpec::Branch {
            name: "archive".into(),
        },
        RefSpec::Tag { name: "v1".into() },
        RefSpec::Commit { oid },
        RefSpec::WorkingTree,
    ];
    let mut originals = vec![];
    for (index, reference) in references.into_iter().enumerate() {
        if reference == RefSpec::WorkingTree {
            w.a.write_file("unchanged.txt", b"one\nworking snapshot\nthree\n")
                .unwrap();
        }
        let snapshot = w.core.tree_snapshot(rid(1), &reference).unwrap();
        let blob = snapshot
            .entries
            .iter()
            .find_map(|entry| match &entry.kind {
                TreeEntryKind::File { oid, .. } if entry.path == p("unchanged.txt") => Some(*oid),
                _ => None,
            })
            .unwrap();
        let anchor = lines_anchor(rid(1), p("unchanged.txt"), Side::Head, blob, 1, 3).unwrap();
        let comment = w
            .core
            .add_comment(
                &human(),
                review_id(1),
                cid(index as u128 + 1),
                CommentKind::Note,
                anchor,
                "browse this revision".into(),
                Some(CommentContext::Browse { reference }),
            )
            .unwrap();
        originals.push(comment);
    }
    // Move the reviewed branch AND the original branch; neither owns these anchors.
    w.a.git(&["reset", "--hard"]).unwrap();
    w.a.git(&["checkout", "feature"]).unwrap();
    w.a.write_file("unchanged.txt", b"unrelated replacement\n")
        .unwrap();
    w.a.git(&["add", "."]).unwrap();
    w.a.git(&["commit", "-m", "unrelated reviewed content"])
        .unwrap();
    w.a.git(&["branch", "-f", "archive", "feature"]).unwrap();
    w.core.resolve_targets(&human(), review_id(1)).unwrap();
    assert_eq!(w.core.comments(review_id(1)).unwrap(), originals);
    let data = w.data.clone();
    drop(w.core);
    let reopened = Core::open(&data).unwrap();
    assert_eq!(reopened.comments(review_id(1)).unwrap(), originals);
    for original in originals {
        let Anchor::Lines {
            blob_oid,
            lines,
            side,
            ..
        } = original.anchor
        else {
            panic!()
        };
        let (header, _) = reopened
            .blob_render(rid(1), &p("unchanged.txt"), blob_oid)
            .unwrap();
        assert_eq!(
            header.target,
            nits_protocol::RenderTarget::Blob { oid: blob_oid }
        );
        assert_eq!(
            (side, lines.start().get(), lines.end().get()),
            (Side::Head, 1, 3)
        );
        assert_eq!(original.state, CommentState::Live);
    }
}

#[test]
#[allow(clippy::too_many_lines)] // one complete finding lifecycle and informational control
fn deferred_finding_retains_discussion_and_is_reversible_without_affecting_notes() {
    let w = world();
    let review = review_id(1);
    w.core
        .create_review(&human(), review, ws(), "review".into(), targets())
        .unwrap();
    let finding = w
        .core
        .add_comment(
            &agent(),
            review,
            cid(1),
            CommentKind::Note,
            Anchor::Review,
            "Controller wire format is wrong".into(),
            None,
        )
        .unwrap();
    let note = w
        .core
        .add_comment(
            &agent(),
            review,
            cid(2),
            CommentKind::Informational,
            Anchor::Review,
            "Review in progress".into(),
            None,
        )
        .unwrap();
    let reason: nits_protocol::DeferralReason =
        "Deferred to controller issue #288 per scope decision"
            .parse()
            .unwrap();
    let url: nits_protocol::TrackingUrl =
        "https://example.com/controller/issues/288".parse().unwrap();
    w.core
        .defer_thread(
            &other_human(),
            review,
            finding.thread_id,
            reason.clone(),
            Some(url.clone()),
        )
        .unwrap();
    let thread = |id| {
        w.core
            .threads(review)
            .unwrap()
            .into_iter()
            .find(|t| t.id == id)
            .unwrap()
    };
    let expected = ThreadResolution::Deferred {
        reason: reason.clone(),
        tracking_url: Some(url),
        by: other_human().author,
        at: other_human().now,
    };
    assert_eq!(thread(finding.thread_id).resolution, expected);
    let before = w.core.last_seq().unwrap();
    for thread in [finding.thread_id, note.thread_id] {
        assert!(
            w.core
                .defer_thread(&human(), review, thread, reason.clone(), None)
                .is_err()
        );
    }
    assert_eq!(w.core.last_seq().unwrap(), before);
    w.core
        .reply(
            &agent(),
            review,
            finding.thread_id,
            cid(3),
            CommentKind::Note,
            "Tracked externally; bug remains".into(),
        )
        .unwrap();
    assert_eq!(thread(finding.thread_id).resolution, expected);
    assert_eq!(thread(finding.thread_id).replies, vec![cid(3)]);
    w.core
        .unresolve_thread(&human(), review, finding.thread_id)
        .unwrap();
    assert_eq!(thread(finding.thread_id).resolution, ThreadResolution::Open);
    w.core
        .defer_thread(&agent(), review, finding.thread_id, reason.clone(), None)
        .unwrap();
    w.core
        .resolve_thread(&human(), review, finding.thread_id)
        .unwrap();
    assert!(matches!(
        thread(finding.thread_id).resolution,
        ThreadResolution::Resolved { .. }
    ));
    assert!(
        w.core
            .defer_thread(&agent(), review, finding.thread_id, reason, None)
            .is_err()
    );
    assert_eq!(
        thread(note.thread_id).resolution,
        ThreadResolution::Informational
    );
    assert_eq!(w.core.comments(review).unwrap().len(), 3);
}

#[test]
#[allow(clippy::too_many_lines)] // One scenario follows immutable content through every lifecycle.
fn requested_h1_checked_after_h2_is_changed_and_delta_survives_gc_restart_rebuild() {
    use nits_protocol::{CheckpointFreshness, RequestedTargets, ReviewRound};
    let w = world();
    let id = review_id(66);
    let mut targets = targets();
    targets
        .iter_mut()
        .for_each(|t| t.head = RefSpec::WorkingTree);
    w.a.write_file("round.txt", b"H1\n").unwrap();
    w.core
        .create_review(&human(), id, ws(), "rounds".into(), targets)
        .unwrap();
    w.core
        .add_comment(
            &human(),
            id,
            cid(66),
            CommentKind::Note,
            Anchor::Review,
            "Keep this finding".into(),
            None,
        )
        .unwrap();
    let original = w.core.tree_snapshot(rid(1), &RefSpec::WorkingTree).unwrap();
    let original_blob = original
        .entries
        .iter()
        .find_map(|entry| match &entry.kind {
            nits_protocol::TreeEntryKind::File { oid, .. } if entry.path == p("round.txt") => {
                Some(*oid)
            }
            _ => None,
        })
        .unwrap();
    let browse = w
        .core
        .add_comment(
            &human(),
            id,
            cid(67),
            CommentKind::Note,
            lines_anchor(rid(1), p("round.txt"), Side::Head, original_blob, 1, 1).unwrap(),
            "Retain the unfixed original".into(),
            Some(nits_protocol::CommentContext::Browse {
                reference: RefSpec::WorkingTree,
            }),
        )
        .unwrap();
    w.core
        .defer_thread(
            &human(),
            id,
            browse.thread_id,
            "Agreed external follow-up".parse().unwrap(),
            None,
        )
        .unwrap();
    let link = nits_protocol::ReviewReference {
        context: nits_protocol::ReferenceContext::named("review-box").unwrap(),
        review_id: id,
        target: nits_protocol::ReferenceTarget::Comment {
            comment_id: browse.id,
        },
    };
    w.core
        .mark_viewed(&human(), id, rid(1), p("round.txt"))
        .unwrap();
    let request_id = w
        .core
        .request_review(
            &human(),
            id,
            "review-agent".into(),
            "Please inspect H1".into(),
        )
        .unwrap();
    let RequestedTargets::Captured { targets: h1 } = w.core.review_snapshot(id).unwrap().requests
        [0]
    .targets
    .clone() else {
        panic!("new requests capture identities")
    };
    w.a.write_file("round.txt", b"H2\n").unwrap();
    w.core.resolve_targets(&human(), id).unwrap();
    let before = w.core.review_snapshot(id).unwrap();
    let checkpoint_id = w
        .core
        .record_checkpoint(
            &agent(),
            id,
            h1.clone(),
            Some(ReviewRound::Request { request_id }),
        )
        .unwrap();
    let after = w.core.review_snapshot(id).unwrap();
    assert_eq!(after.review, before.review);
    assert_eq!(after.resolved, before.resolved);
    assert_eq!(after.comments, before.comments);
    assert_eq!(after.threads, before.threads);
    assert_eq!(after.viewed, before.viewed);
    assert_eq!(after.checkpoints[0].targets, h1);
    assert_eq!(
        nits_protocol::latest_checkpoints(&after.checkpoints, after.resolved.as_ref())[0].freshness,
        CheckpointFreshness::Changed
    );
    // Both working-tree content and the original base remain reachable after GC.
    w.a.git(&["gc", "--prune=now"]).unwrap();
    w.b.git(&["gc", "--prune=now"]).unwrap();
    let scope = DiffScope::SinceCheckpoint { checkpoint_id };
    let (files, delta) = w.core.files_scoped(id, &scope).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, p("round.txt"));
    for target in &delta {
        let checked = h1.iter().find(|t| t.repo_id == target.repo_id).unwrap();
        assert_eq!(target.base, checked.head);
    }
    let (_, rendered) = w
        .core
        .file_render(id, rid(1), &p("round.txt"), RenderOpts::default(), &scope)
        .unwrap();
    let rendered = format!("{rendered:?}");
    assert!(rendered.contains("H1"));
    assert!(rendered.contains("H2"));
    assert_eq!(
        w.core
            .scoped_targets(id, &DiffScope::Requested { request_id })
            .unwrap(),
        h1
    );
    drop(w.core);
    let store = nits_review_core::store::Store::open(&w.data.state()).unwrap();
    let events = store.events_after(None).unwrap();
    store.rebuild_views().unwrap();
    assert_eq!(store.events_after(None).unwrap(), events);
    assert_eq!(store.review_snapshot(id).unwrap().unwrap(), after);
    drop(store);
    let core = Core::open(&w.data).unwrap();
    assert_eq!(core.review_snapshot(id).unwrap(), after);
    assert_eq!(link.resolve(&after).unwrap(), Some(browse.id));
    assert_eq!(
        after
            .comments
            .iter()
            .find(|comment| comment.id == browse.id)
            .unwrap(),
        &browse
    );
    assert!(matches!(
        after
            .threads
            .iter()
            .find(|thread| thread.id == browse.thread_id)
            .unwrap()
            .resolution,
        nits_protocol::ThreadResolution::Deferred { .. }
    ));
    let (_, original_rows) = core
        .blob_render(rid(1), &p("round.txt"), original_blob)
        .unwrap();
    assert!(format!("{original_rows:?}").contains("H1"));
    assert_eq!(core.files_scoped(id, &scope).unwrap().0, files);
    // Restarted agent groups with its previous session; provenance is preserved.
    let mut restarted = agent();
    if let Author::Agent {
        session_id, model, ..
    } = &mut restarted.author
    {
        *session_id = "second-session".into();
        *model = "new-model".into();
    }
    core.record_checkpoint(
        &restarted,
        id,
        after.resolved.clone().unwrap(),
        Some(ReviewRound::Checkpoint { checkpoint_id }),
    )
    .unwrap();
    let current = core.review_snapshot(id).unwrap();
    let latest = nits_protocol::latest_checkpoints(&current.checkpoints, current.resolved.as_ref());
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].freshness, CheckpointFreshness::Current);
    assert_eq!(current.checkpoints[0].author, agent().author);
    assert_eq!(latest[0].checkpoint.author, restarted.author);
}

#[test]
fn checkpoint_rejects_partial_duplicate_forged_and_cross_review_provenance() {
    use nits_protocol::{ReviewRequestId, ReviewRound, Seq};
    let w = world();
    let id = review_id(67);
    w.core
        .create_review(&human(), id, ws(), "rounds".into(), targets())
        .unwrap();
    let targets = w.core.review_snapshot(id).unwrap().resolved.unwrap();
    let partial = NonEmpty::singleton(targets.first().clone());
    assert!(
        w.core
            .record_checkpoint(&agent(), id, partial, None)
            .is_err()
    );
    let duplicate = NonEmpty::new(vec![targets.first().clone(), targets.first().clone()]).unwrap();
    assert!(
        w.core
            .record_checkpoint(&agent(), id, duplicate, None)
            .is_err()
    );
    let mut forged = targets.clone();
    forged.iter_mut().next().unwrap().head.tree = forged.first().base.tree;
    assert!(
        w.core
            .record_checkpoint(&agent(), id, forged, None)
            .is_err()
    );
    assert!(
        w.core
            .record_checkpoint(
                &agent(),
                id,
                targets,
                Some(ReviewRound::Request {
                    request_id: ReviewRequestId::from_event_seq(Seq::new(999))
                })
            )
            .is_err()
    );
    assert!(w.core.review_snapshot(id).unwrap().checkpoints.is_empty());
}

#[test]
fn captured_commit_provenance_survives_force_push_and_garbage_collection() {
    let w = world();
    let id = review_id(68);
    w.core
        .create_review(&human(), id, ws(), "rounds".into(), targets())
        .unwrap();
    let request_id = w
        .core
        .request_review(&human(), id, "review-agent".into(), "H1".into())
        .unwrap();
    let nits_protocol::RequestedTargets::Captured { targets: checked } =
        w.core.review_snapshot(id).unwrap().requests[0]
            .targets
            .clone()
    else {
        panic!("captured")
    };
    w.a.git(&["reset", "--hard", "main"]).unwrap();
    w.a.git(&["reflog", "expire", "--expire=now", "--all"])
        .unwrap();
    w.a.git(&["gc", "--prune=now"]).unwrap();
    w.core.resolve_targets(&human(), id).unwrap();
    w.core
        .record_checkpoint(
            &agent(),
            id,
            checked.clone(),
            Some(nits_protocol::ReviewRound::Request { request_id }),
        )
        .unwrap();
    let snapshot = w.core.review_snapshot(id).unwrap();
    assert_eq!(snapshot.checkpoints[0].targets, checked);
    assert_eq!(
        nits_protocol::latest_checkpoints(&snapshot.checkpoints, snapshot.resolved.as_ref())[0]
            .freshness,
        nits_protocol::CheckpointFreshness::Changed
    );
}

#[test]
fn requesting_a_moved_branch_updates_current_targets_and_reanchors() {
    let w = world();
    let id = review_id(70);
    w.core
        .create_review(&human(), id, ws(), "rounds".into(), targets())
        .unwrap();
    let old_blob = head_blob(&w.core, id, rid(1), "src/main.rs");
    w.core
        .add_comment(
            &human(),
            id,
            cid(70),
            CommentKind::Note,
            lines_anchor(rid(1), p("src/main.rs"), Side::Head, old_blob, 8, 8).unwrap(),
            "g".into(),
            None,
        )
        .unwrap();
    let before = w.core.review_snapshot(id).unwrap();
    let shifted = format!("// shifted\n{}", SRC.replace("let h = 8;", "let h = 80;"));
    w.a.write_file("src/main.rs", shifted.as_bytes()).unwrap();
    w.a.git(&["commit", "-qam", "next round"]).unwrap();
    let request_id = w
        .core
        .request_review(&human(), id, "review-agent".into(), "next round".into())
        .unwrap();
    let snapshot = w.core.review_snapshot(id).unwrap();
    let nits_protocol::RequestedTargets::Captured { targets } = &snapshot.requests[0].targets
    else {
        panic!("captured")
    };
    assert_eq!(snapshot.resolved.as_ref(), Some(targets));
    assert_ne!(snapshot.resolved, before.resolved);
    assert_eq!(snapshot.review, before.review);
    let Anchor::Lines {
        lines, blob_oid, ..
    } = &snapshot.comments[0].anchor
    else {
        panic!("lines")
    };
    assert_eq!(lines.start().get(), 9);
    assert_ne!(*blob_oid, old_blob);
    assert_eq!(snapshot.comments[0].state, CommentState::Live);
    let checked = w
        .core
        .record_checkpoint(
            &agent(),
            id,
            targets.clone(),
            Some(nits_protocol::ReviewRound::Request { request_id }),
        )
        .unwrap();
    let checked_snapshot = w.core.review_snapshot(id).unwrap();
    assert_eq!(
        nits_protocol::latest_checkpoints(
            &checked_snapshot.checkpoints,
            checked_snapshot.resolved.as_ref()
        )[0]
        .freshness,
        nits_protocol::CheckpointFreshness::Current
    );
    assert!(
        w.core
            .files_scoped(
                id,
                &DiffScope::SinceCheckpoint {
                    checkpoint_id: checked
                }
            )
            .unwrap()
            .0
            .is_empty()
    );
}

#[test]
fn checking_a_displayed_worktree_after_refresh_uses_its_retained_target_event() {
    let w = world();
    let id = review_id(69);
    let mut targets = targets();
    targets
        .iter_mut()
        .for_each(|t| t.head = RefSpec::WorkingTree);
    w.a.write_file("round.txt", b"H1\n").unwrap();
    w.core
        .create_review(&human(), id, ws(), "rounds".into(), targets)
        .unwrap();
    let displayed = w.core.review_snapshot(id).unwrap().resolved.unwrap();
    w.a.write_file("round.txt", b"H2\n").unwrap();
    w.core.resolve_targets(&human(), id).unwrap();
    w.a.git(&["gc", "--prune=now"]).unwrap();
    w.core
        .record_checkpoint(&human(), id, displayed.clone(), None)
        .unwrap();
    let snapshot = w.core.review_snapshot(id).unwrap();
    assert!(snapshot.requests.is_empty());
    assert_eq!(snapshot.checkpoints[0].targets, displayed);
    assert_eq!(
        nits_protocol::latest_checkpoints(&snapshot.checkpoints, snapshot.resolved.as_ref())[0]
            .freshness,
        nits_protocol::CheckpointFreshness::Changed
    );
}
