//! A fixed historical boundary and scanned cursors survive sparse filtering.
use nits_protocol::{
    Author, ClientId, ClientSeq, Event, EventBody, NonEmpty, RefSpec, ReplayCursor, ReplayPage,
    ReplayPosition, ReplayProgress, RepoId, ReviewId, ReviewTarget, Seq, Since, SubscribeScope,
    Timestamp, WorkspaceId,
};
use nits_review_core::store::replay::{REPLAY_PAGE_BYTES, REPLAY_SCAN_LIMIT};
use nits_review_core::{Core, Ctx, DataDir};
use nits_test_support::{RepoBuilder, files};

fn ctx() -> Ctx {
    Ctx {
        author: Author::Human {
            name: "ada".into(),
            machine: "test".into(),
        },
        client_id: ClientId::from_parts(1, 1),
        client_seq: ClientSeq::new(1),
        now: Timestamp::from_millis(0),
    }
}
fn ws(n: u128) -> WorkspaceId {
    WorkspaceId::from_parts(1, n)
}
fn start(after: u64) -> ReplayPosition {
    ReplayPosition::Start {
        since: Since::After {
            seq: Seq::new(after),
        },
    }
}
fn next(page: &ReplayPage) -> Option<ReplayPosition> {
    match page.progress {
        ReplayProgress::More { after } => Some(ReplayPosition::Continue {
            cursor: ReplayCursor::new(after, page.through).unwrap(),
        }),
        ReplayProgress::Complete => None,
    }
}
fn core() -> (tempfile::TempDir, Core) {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(&DataDir::new(dir.path())).unwrap();
    (dir, core)
}
fn drain(
    core: &Core,
    scope: &SubscribeScope,
    mut position: ReplayPosition,
) -> (Vec<Event>, Vec<ReplayPage>) {
    let mut events = vec![];
    let mut pages = vec![];
    loop {
        let page = core.replay_events(scope, position).unwrap();
        events.extend(page.events.clone());
        let more = next(&page);
        pages.push(page);
        match more {
            Some(p) => position = p,
            None => return (events, pages),
        }
    }
}

#[test]
fn replay_pages_bound_scans_and_drain_empty_filtered_pages_at_a_fixed_head() {
    let (_dir, core) = core();
    core.create_workspace(&ctx(), ws(1), "selected".into())
        .unwrap();
    core.create_workspace(&ctx(), ws(2), "other".into())
        .unwrap();
    for n in 0..1300 {
        core.rename_workspace(
            &ctx(),
            if n == 1100 { ws(1) } else { ws(2) },
            format!("update {n}"),
        )
        .unwrap();
    }
    let scope = SubscribeScope::Workspace {
        workspace_id: ws(1),
    };
    let first = core.replay_events(&scope, start(2)).unwrap();
    assert!(first.events.is_empty());
    assert_eq!(
        first.progress,
        ReplayProgress::More {
            after: Seq::new(2 + REPLAY_SCAN_LIMIT as u64)
        }
    );
    let end = first.through;
    // Writes after capture must not extend any continuation or escape its window.
    core.rename_workspace(&ctx(), ws(1), "past captured boundary".into())
        .unwrap();
    let (events, pages) = drain(&core, &scope, next(&first).unwrap());
    assert!(pages.iter().all(|page| page.through == end));
    assert!(
        pages.last().unwrap().events.is_empty(),
        "empty final matching page still completes"
    );
    assert_eq!(events.len(), 1);
    assert!(events[0].seq < end);
    let following = core
        .replay_events(&scope, ReplayPosition::Follow { after: end })
        .unwrap();
    assert_eq!(following.events.len(), 1);
    assert!(following.events[0].seq > end);
    let (all, all_pages) = drain(&core, &SubscribeScope::All, start(0));
    assert_eq!(all.len(), 1303);
    assert!(all.windows(2).all(|pair| pair[0].seq < pair[1].seq));
    assert!(
        all_pages
            .iter()
            .all(|page| page.events.len() <= REPLAY_SCAN_LIMIT)
    );
    assert!(
        drain(
            &core,
            &SubscribeScope::Workspace {
                workspace_id: ws(9)
            },
            start(0)
        )
        .0
        .is_empty()
    );
}

#[test]
fn replay_exact_pages_now_future_and_maximum_cursors_never_regress_or_overflow() {
    let (_dir, core) = core();
    for seq in [0, 999_999, u64::MAX] {
        let page = core
            .replay_events(&SubscribeScope::All, start(seq))
            .unwrap();
        assert_eq!(page.through, Seq::new(seq));
        assert_eq!(page.progress, ReplayProgress::Complete);
        assert!(page.events.is_empty());
        assert!(core.events_after(Some(Seq::new(seq))).unwrap().is_empty());
    }
    core.create_workspace(&ctx(), ws(1), "selected".into())
        .unwrap();
    for n in 1..REPLAY_SCAN_LIMIT * 2 {
        core.rename_workspace(&ctx(), ws(1), format!("update {n}"))
            .unwrap();
    }
    let (events, pages) = drain(&core, &SubscribeScope::All, start(0));
    assert_eq!(events.len(), REPLAY_SCAN_LIMIT * 2);
    assert_eq!(pages.len(), 2);
    assert!(
        pages
            .iter()
            .all(|page| page.events.len() == REPLAY_SCAN_LIMIT)
    );
    let now = core
        .replay_events(
            &SubscribeScope::All,
            ReplayPosition::Start { since: Since::Now },
        )
        .unwrap();
    assert!(now.events.is_empty());
    assert_eq!(now.through, core.last_seq().unwrap().unwrap());
    assert!(
        core.replay_events(
            &SubscribeScope::All,
            ReplayPosition::Continue {
                cursor: ReplayCursor::new(Seq::new(0), Seq::new(u64::MAX)).unwrap()
            }
        )
        .is_err()
    );
    let max = core
        .replay_events(&SubscribeScope::All, start(u64::MAX))
        .unwrap();
    assert_eq!(max.through, Seq::new(u64::MAX));
    assert!(
        core.events_after(Some(Seq::new(u64::MAX)))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn replay_byte_budget_splits_large_pages_and_preserves_one_large_event() {
    let (_dir, core) = core();
    core.create_workspace(&ctx(), ws(1), "selected".into())
        .unwrap();
    for _ in 0..5 {
        core.rename_workspace(&ctx(), ws(1), "x".repeat(REPLAY_PAGE_BYTES / 3))
            .unwrap();
    }
    let big = "y".repeat(REPLAY_PAGE_BYTES + 100);
    core.rename_workspace(&ctx(), ws(1), big.clone()).unwrap();
    core.rename_workspace(&ctx(), ws(1), "tail".into()).unwrap();
    let (events, pages) = drain(&core, &SubscribeScope::All, start(1));
    assert_eq!(events.len(), 7);
    assert!(pages.iter().all(|page| page.events.len() == 1
        || serde_json::to_vec(&page.events).unwrap().len() <= REPLAY_PAGE_BYTES));
    assert!(pages.iter().any(|page| matches!(page.events.as_slice(), [event] if matches!(&event.body, EventBody::WorkspaceUpdated { name, .. } if name == &big))));
    // Sparse output still obeys the byte budget of scanned records.
    let empty = core
        .replay_events(
            &SubscribeScope::Workspace {
                workspace_id: ws(2),
            },
            start(1),
        )
        .unwrap();
    assert!(empty.events.is_empty());
    assert!(matches!(empty.progress, ReplayProgress::More { after } if after.get() < 6));
}

#[test]
fn replay_workspace_and_recipient_filters_keep_deleted_review_history() {
    let (_dir, core) = core();
    let repo = RepoBuilder::new()
        .commit("base", files!["file.txt" => "base\n"])
        .build()
        .unwrap();
    let rid = RepoId::from_parts(1, 1);
    let review = ReviewId::from_parts(1, 1);
    core.create_workspace(&ctx(), ws(1), "selected".into())
        .unwrap();
    core.attach_repo(
        &ctx(),
        ws(1),
        rid,
        repo.path().to_str().unwrap(),
        "repo".into(),
    )
    .unwrap();
    core.create_review(
        &ctx(),
        review,
        ws(1),
        "review".into(),
        NonEmpty::singleton(ReviewTarget {
            repo_id: rid,
            base: RefSpec::Head,
            head: RefSpec::WorkingTree,
        }),
    )
    .unwrap();
    core.request_review(&ctx(), review, "recipient".into(), "please check".into())
        .unwrap();
    core.delete_review(&ctx(), review).unwrap();
    let (workspace, _) = drain(
        &core,
        &SubscribeScope::Workspace {
            workspace_id: ws(1),
        },
        start(0),
    );
    assert_eq!(workspace, core.events_after(None).unwrap());
    let (recipient, _) = drain(
        &core,
        &SubscribeScope::AwaitingAgent {
            agent: "recipient".into(),
        },
        start(0),
    );
    assert_eq!(recipient.len(), 1);
    assert!(
        matches!(&recipient[0].body, EventBody::ReviewRequested { agent, .. } if agent == "recipient")
    );
    let (selected, _) = drain(
        &core,
        &SubscribeScope::Review { review_id: review },
        start(0),
    );
    assert!(
        selected
            .iter()
            .all(|event| event.body.review_id() == Some(review))
    );
    assert!(matches!(
        selected.last().unwrap().body,
        EventBody::ReviewDeleted { .. }
    ));
}
