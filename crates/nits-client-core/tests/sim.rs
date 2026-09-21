//! Plan 3.3: optimistic mutations under races, driven through the two-client
//! simulator. Every case ends with both clients showing exactly what the
//! daemon holds and nothing pending.

use nits_client_core::{Action, Effect};
use nits_protocol::{
    Anchor, Author, ClientMsg, CommentState, NonEmpty, RefSpec, RepoId, Request, Review, ReviewId,
    ReviewSnapshot, ReviewStatus, ReviewTarget, Seq, ThreadResolution, Timestamp, WorkspaceId,
};
use nits_test_support::{Divergence, Peer, Sim};
use proptest::prelude::*;

const A: Peer = Peer(0);
const B: Peer = Peer(1);

fn human(name: &str) -> Author {
    Author::Human {
        name: name.into(),
        machine: "host".into(),
    }
}

fn snapshot() -> ReviewSnapshot {
    ReviewSnapshot {
        review: Review {
            id: ReviewId::from_parts(4, 1),
            workspace_id: WorkspaceId::from_parts(3, 3),
            title: "a review".into(),
            targets: NonEmpty::singleton(ReviewTarget {
                repo_id: RepoId::from_parts(2, 2),
                base: RefSpec::Branch {
                    name: "main".into(),
                },
                head: RefSpec::Head,
            }),
            created: Timestamp::from_millis(0),
            status: ReviewStatus::Open,
        },
        resolved: None,
        threads: Vec::new(),
        comments: Vec::new(),
        viewed: Vec::new(),
        requests: Vec::new(),
        seq: Seq::new(1),
        checkpoints: Vec::new(),
    }
}

/// Two connected clients with one thread by A, fully delivered.
fn two_clients_one_thread() -> Sim {
    let mut sim = Sim::new(snapshot(), vec![human("ada"), human("bob")]);
    sim.connect_and_open(A).unwrap();
    sim.connect_and_open(B).unwrap();
    sim.tick(1);
    sim.act(
        A,
        Action::DraftOpened {
            anchor: Anchor::Review,
        },
    )
    .unwrap();
    sim.act(
        A,
        Action::DraftSubmitted {
            body: "root".into(),
        },
    )
    .unwrap();
    sim.settle();
    sim.converged().unwrap();
    assert_eq!(sim.daemon_snapshot().threads.len(), 1);
    sim
}

fn thread_id(sim: &Sim) -> nits_protocol::ThreadId {
    sim.daemon_snapshot().threads[0].id
}

fn mutations(effects: &[Effect]) -> usize {
    effects
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::Send(ClientMsg::Request {
                    request: Request::Mutate { .. },
                    ..
                })
            )
        })
        .count()
}

#[test]
fn concurrent_replies_to_one_thread_keep_both_in_daemon_order() {
    let mut sim = two_clients_one_thread();
    let thread = thread_id(&sim);
    sim.tick(1);
    // Both reply before either reply reaches the daemon.
    sim.act(
        A,
        Action::Reply {
            thread_id: thread,
            body: "from A".into(),
        },
    )
    .unwrap();
    sim.act(
        B,
        Action::Reply {
            thread_id: thread,
            body: "from B".into(),
        },
    )
    .unwrap();
    // Each shows its own reply at once, marked pending.
    for p in [A, B] {
        let open = sim.client(p).view().review.as_ref().unwrap();
        assert_eq!(open.pending.len(), 1);
        assert_eq!(open.snapshot.comments.len(), 2);
    }
    // B's reaches the daemon first.
    assert!(sim.deliver_up(B));
    assert!(sim.deliver_up(A));
    sim.settle();
    sim.converged().unwrap();
    let bodies: Vec<&str> = sim
        .daemon_snapshot()
        .comments
        .iter()
        .map(|c| c.body.as_str())
        .collect();
    assert_eq!(bodies, vec!["root", "from B", "from A"]);
    assert_eq!(sim.daemon_snapshot().threads[0].replies.len(), 2);
}

#[test]
fn concurrent_edits_of_one_comment_last_writer_by_seq_wins() {
    // Both clients are the same author, so both may edit.
    let mut sim = Sim::new(snapshot(), vec![human("ada"), human("ada")]);
    sim.connect_and_open(A).unwrap();
    sim.connect_and_open(B).unwrap();
    sim.tick(1);
    sim.act(
        A,
        Action::DraftOpened {
            anchor: Anchor::Review,
        },
    )
    .unwrap();
    sim.act(A, Action::DraftSubmitted { body: "v0".into() })
        .unwrap();
    sim.settle();
    let id = sim.daemon_snapshot().comments[0].id;
    sim.tick(1);
    sim.act(
        A,
        Action::EditComment {
            comment_id: id,
            body: "from A".into(),
        },
    )
    .unwrap();
    sim.act(
        B,
        Action::EditComment {
            comment_id: id,
            body: "from B".into(),
        },
    )
    .unwrap();
    // Each sees its own edit while pending.
    let shown = |sim: &Sim, p: Peer| {
        sim.client(p)
            .view()
            .review
            .as_ref()
            .unwrap()
            .snapshot
            .comments[0]
            .body
            .clone()
    };
    assert_eq!(shown(&sim, A), "from A");
    assert_eq!(shown(&sim, B), "from B");
    // A's edit lands first; B's foreign event arrives at A while A's own is
    // still pending, so A keeps showing its own until the echo.
    assert!(sim.deliver_up(A));
    assert!(sim.deliver_up(B));
    // Deliver B's committed event to A before A's own echo.
    let (_, down_a) = sim.queued(A);
    assert!(down_a >= 3, "Committed, own Event, B's Event");
    sim.settle();
    sim.converged().unwrap();
    assert_eq!(sim.daemon_snapshot().comments[0].body, "from B");
    assert_eq!(shown(&sim, A), "from B");
    // Reverse order in a fresh run: A's wins when it is sequenced last.
    let mut sim = Sim::new(snapshot(), vec![human("ada"), human("ada")]);
    sim.connect_and_open(A).unwrap();
    sim.connect_and_open(B).unwrap();
    sim.tick(1);
    sim.act(
        A,
        Action::DraftOpened {
            anchor: Anchor::Review,
        },
    )
    .unwrap();
    sim.act(A, Action::DraftSubmitted { body: "v0".into() })
        .unwrap();
    sim.settle();
    let id = sim.daemon_snapshot().comments[0].id;
    sim.act(
        A,
        Action::EditComment {
            comment_id: id,
            body: "from A".into(),
        },
    )
    .unwrap();
    sim.act(
        B,
        Action::EditComment {
            comment_id: id,
            body: "from B".into(),
        },
    )
    .unwrap();
    assert!(sim.deliver_up(B));
    assert!(sim.deliver_up(A));
    sim.settle();
    sim.converged().unwrap();
    assert_eq!(sim.daemon_snapshot().comments[0].body, "from A");
    let last = sim.log().last().unwrap();
    assert_eq!(last.client_id, sim.client(A).client_id());
}

#[test]
fn resolve_unresolve_race_converges_and_the_loser_is_undone() {
    let mut sim = two_clients_one_thread();
    let thread = thread_id(&sim);
    // Both resolve concurrently: the second is rejected by the daemon
    // (already resolved) and its optimistic state is simply confirmed by
    // the first's event.
    sim.act(A, Action::ResolveThread { thread_id: thread })
        .unwrap();
    sim.act(B, Action::ResolveThread { thread_id: thread })
        .unwrap();
    assert!(sim.deliver_up(A));
    assert!(sim.deliver_up(B));
    sim.settle();
    sim.converged().unwrap();
    assert!(matches!(
        sim.daemon_snapshot().threads[0].resolution,
        ThreadResolution::Resolved { .. }
    ));
    // B's request was rejected: an error is shown, nothing pending.
    assert!(sim.client(B).view().last_error.is_some());
    assert_eq!(sim.client(B).pending_count(), 0);

    // Now A unresolves while B resolves-again (B still sees resolved, so
    // its resolve is rejected locally — nothing is sent).
    sim.act(A, Action::UnresolveThread { thread_id: thread })
        .unwrap();
    let err = sim
        .act(B, Action::ResolveThread { thread_id: thread })
        .unwrap_err();
    assert!(matches!(
        err,
        nits_client_core::CoreError::Mutation(nits_client_core::MutationError::AlreadyResolved(_))
    ));
    // A's unresolve lands; B sees it; B can now resolve.
    sim.settle();
    sim.converged().unwrap();
    assert_eq!(
        sim.daemon_snapshot().threads[0].resolution,
        ThreadResolution::Open
    );
    // Race the other way: B resolves, A unresolves before seeing B's
    // resolve → A's unresolve is rejected (not resolved) and undone.
    sim.act(B, Action::ResolveThread { thread_id: thread })
        .unwrap();
    let err = sim
        .act(A, Action::UnresolveThread { thread_id: thread })
        .unwrap_err();
    assert!(matches!(
        err,
        nits_client_core::CoreError::Mutation(nits_client_core::MutationError::NotResolved(_))
    ));
    sim.settle();
    sim.converged().unwrap();
}

#[test]
fn disconnect_mid_pending_then_reconnect_resends_exactly_once() {
    let mut sim = two_clients_one_thread();
    let thread = thread_id(&sim);
    sim.tick(1);
    // A replies; the request is queued but the connection drops before it
    // reaches the daemon.
    let effects = sim
        .act(
            A,
            Action::Reply {
                thread_id: thread,
                body: "lost?".into(),
            },
        )
        .unwrap();
    assert_eq!(mutations(&effects), 1);
    assert_eq!(sim.client(A).pending_count(), 1);
    sim.disconnect(A);
    // Still shown, still pending, review still open.
    let open = sim.client(A).view().review.as_ref().unwrap();
    assert_eq!(open.pending.len(), 1);
    assert_eq!(open.snapshot.comments.len(), 2);

    // Reconnect: after the resubscribe the pending mutation goes out again,
    // once, with its original client_seq.
    sim.reconnect(A).unwrap();
    sim.settle();
    sim.converged().unwrap();
    let resent: Vec<_> = sim
        .log()
        .iter()
        .filter(|e| e.client_id == sim.client(A).client_id())
        .collect();
    assert_eq!(resent.len(), 2, "root comment + the one reply");
    assert_eq!(sim.daemon_snapshot().comments.len(), 2);
    assert_eq!(sim.daemon_snapshot().comments[1].body, "lost?");

    // The other case: the daemon committed it but the reply was lost.
    sim.tick(1);
    sim.act(
        A,
        Action::Reply {
            thread_id: thread,
            body: "committed but unheard".into(),
        },
    )
    .unwrap();
    assert!(sim.deliver_up(A)); // daemon has it
    assert_eq!(sim.log().len(), 3);
    sim.disconnect(A); // Committed + Event never arrive
    assert_eq!(sim.client(A).pending_count(), 1);
    sim.reconnect(A).unwrap();
    sim.settle();
    // The replay after last_seq carries A's own event, which retires the
    // pending entry before the resubscribe is even answered: nothing is
    // re-sent, nothing is rejected, no second comment.
    sim.converged().unwrap();
    assert_eq!(sim.log().len(), 3);
    assert_eq!(sim.daemon_snapshot().comments.len(), 3);
    assert!(sim.client(A).view().last_error.is_none());
}

#[test]
fn delete_keeps_the_slot_with_state_deleted() {
    let mut sim = two_clients_one_thread();
    let id = sim.daemon_snapshot().comments[0].id;
    // B is not the author: rejected locally, nothing sent.
    assert!(
        sim.act(B, Action::DeleteComment { comment_id: id })
            .is_err()
    );
    sim.act(A, Action::DeleteComment { comment_id: id })
        .unwrap();
    sim.settle();
    sim.converged().unwrap();
    assert_eq!(
        sim.daemon_snapshot().comments[0].state,
        CommentState::Deleted
    );
}

// ---- proptest: any interleaving converges ---------------------------------

#[derive(Debug, Clone)]
enum Step {
    Reply(Peer),
    Edit(Peer, usize),
    Resolve(Peer),
    Unresolve(Peer),
    Up(Peer),
    Down(Peer),
    Drop(Peer),
    Reconnect(Peer),
    Tick,
}

fn peer() -> impl Strategy<Value = Peer> {
    prop_oneof![Just(A), Just(B)]
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        3 => peer().prop_map(Step::Reply),
        2 => (peer(), 0usize..4).prop_map(|(p, i)| Step::Edit(p, i)),
        2 => peer().prop_map(Step::Resolve),
        2 => peer().prop_map(Step::Unresolve),
        6 => peer().prop_map(Step::Up),
        6 => peer().prop_map(Step::Down),
        1 => peer().prop_map(Step::Drop),
        2 => peer().prop_map(Step::Reconnect),
        2 => Just(Step::Tick),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    #[test]
    fn any_interleaving_converges(steps in prop::collection::vec(step(), 0..40)) {
        let mut sim = two_clients_one_thread();
        let thread = thread_id(&sim);
        for s in steps {
            match s {
                Step::Reply(p) => {
                    let _ = sim.act(p, Action::Reply { thread_id: thread, body: format!("{p:?}") });
                }
                Step::Edit(p, i) => {
                    let ids: Vec<_> = sim.client(p).view().review.as_ref()
                        .map(|r| r.snapshot.comments.iter().map(|c| c.id).collect())
                        .unwrap_or_default();
                    if let Some(id) = ids.get(i) {
                        let _ = sim.act(p, Action::EditComment { comment_id: *id, body: format!("e{i}") });
                    }
                }
                Step::Resolve(p) => {
                    let _ = sim.act(p, Action::ResolveThread { thread_id: thread });
                }
                Step::Unresolve(p) => {
                    let _ = sim.act(p, Action::UnresolveThread { thread_id: thread });
                }
                Step::Up(p) => {
                    sim.deliver_up(p);
                }
                Step::Down(p) => {
                    // Benign rejections: a reply to a request lost with the
                    // connection, or the broadcast echo of an event the
                    // `Committed` response already delivered.
                    if let Some(Err(e)) = sim.deliver_down(p) {
                        prop_assert!(
                            matches!(
                                e,
                                nits_client_core::CoreError::UnknownRequest(_)
                                    | nits_client_core::CoreError::StaleEvent { .. }
                            ),
                            "core rejected a daemon frame: {e}"
                        );
                    }
                }
                Step::Drop(p) => sim.disconnect(p),
                Step::Reconnect(p) => {
                    let _ = sim.reconnect(p);
                }
                Step::Tick => sim.tick(1),
            }
        }
        // Bring everyone back and drain.
        for p in [A, B] {
            let _ = sim.reconnect(p);
        }
        sim.settle();
        match sim.converged() {
            Ok(()) => {}
            Err(Divergence::NotOpen(_)) => prop_assert!(false, "review closed"),
            Err(e) => prop_assert!(false, "{e}: daemon={:?}", sim.daemon_snapshot().comments),
        }
    }
}

#[test]
fn informational_notes_converge_with_concurrent_replies_and_reconnect_without_opening_findings() {
    use nits_client_core::{CoreError, MutationError, ThreadStatus};
    let mut sim = two_clients_one_thread();
    let finding = thread_id(&sim);
    sim.act(A, Action::InformationalNoteOpened).unwrap();
    sim.act(
        A,
        Action::DraftSubmitted {
            body: "Summary, not approval".into(),
        },
    )
    .unwrap();
    assert_eq!(
        sim.client(A)
            .view()
            .threads
            .iter()
            .filter(|t| t.status == ThreadStatus::Open)
            .count(),
        1
    );
    assert!(
        sim.client(A)
            .view()
            .threads
            .iter()
            .any(|t| t.status == ThreadStatus::Informational && t.pending)
    );
    sim.settle();
    let note = sim
        .daemon_snapshot()
        .threads
        .iter()
        .find(|t| t.resolution == ThreadResolution::Informational)
        .unwrap()
        .id;
    for peer in [A, B] {
        sim.act(
            peer,
            Action::Reply {
                thread_id: note,
                body: format!("status from {}", peer.0),
            },
        )
        .unwrap();
    }
    assert!(sim.deliver_up(B));
    assert!(sim.deliver_up(A));
    sim.disconnect(B);
    sim.settle();
    sim.act(A, Action::ResolveThread { thread_id: finding })
        .unwrap();
    sim.settle();
    sim.reconnect(B).unwrap();
    sim.settle();
    sim.converged().unwrap();
    for peer in [A, B] {
        let view = sim.client(peer).view();
        assert_eq!(
            view.threads
                .iter()
                .filter(|t| t.status == ThreadStatus::Open)
                .count(),
            0
        );
        let thread = view.threads.iter().find(|t| t.id == note).unwrap();
        assert_eq!(thread.status, ThreadStatus::Informational);
        assert_eq!(thread.comments.len(), 3);
        assert_eq!(thread.comments[1].author, human("bob"));
        assert_eq!(thread.comments[2].author, human("ada"));
        assert_eq!(
            sim.act(peer, Action::ResolveThread { thread_id: note }),
            Err(CoreError::Mutation(MutationError::InformationalThread(
                note
            )))
        );
        assert_eq!(
            sim.act(
                peer,
                Action::DeferThread {
                    thread_id: note,
                    reason: "External follow-up".parse().unwrap(),
                    tracking_url: None
                }
            ),
            Err(CoreError::Mutation(MutationError::InformationalThread(
                note
            )))
        );
        assert_eq!(
            sim.act(peer, Action::UnresolveThread { thread_id: note }),
            Err(CoreError::Mutation(MutationError::InformationalThread(
                note
            )))
        );
    }
    sim.converged().unwrap();
}

#[test]
fn requests_reach_live_clients_and_fresh_disconnected_recipients() {
    let mut sim = Sim::new(snapshot(), vec![human("ada"), human("bob")]);
    sim.connect_and_open(A).unwrap();
    let first = sim.request_review(
        human("ada"),
        "review-agent".into(),
        "Please review the parser".into(),
    );
    sim.settle();
    sim.connect_and_open(B).unwrap();
    sim.converged().unwrap();
    assert_eq!(sim.client(B).view().requests[0].id.event_seq(), first.seq);
    assert!(sim.client(B).view().threads.is_empty());
    sim.disconnect(B);
    let second = sim.request_review(human("ada"), "review-agent".into(), "Follow-up".into());
    sim.settle();
    sim.reconnect(B).unwrap();
    sim.settle();
    sim.converged().unwrap();
    assert_eq!(sim.client(B).view().requests.len(), 2);
    assert_eq!(sim.client(B).view().requests[1].id.event_seq(), second.seq);
    // Request navigation is independent of findings, even in an empty review.
    sim.act(
        B,
        Action::RunCommand {
            command: nits_client_core::Command::FocusRequests,
        },
    )
    .unwrap();
    assert_eq!(
        sim.client(B).view().tab,
        nits_client_core::Tab::Conversation
    );
    assert_eq!(
        sim.client(B).view().focus,
        nits_client_core::Focus::ReviewRequest { index: 0 }
    );
    sim.act(
        B,
        Action::RunCommand {
            command: nits_client_core::Command::MoveDown,
        },
    )
    .unwrap();
    assert_eq!(
        sim.client(B).view().focus,
        nits_client_core::Focus::ReviewRequest { index: 1 }
    );
    sim.act(
        B,
        Action::RunCommand {
            command: nits_client_core::Command::Open,
        },
    )
    .unwrap();
    assert_eq!(
        sim.client(B).view().tab,
        nits_client_core::Tab::FilesChanged
    );
    assert_eq!(sim.client(B).view().requests.len(), 2);
}

#[test]
#[allow(clippy::too_many_lines)] // Keep content setup and the two-client delivery scenario together.
fn browse_comment_arrives_remotely_inline_and_converges() {
    use nits_client_core::{FileRef, Tab};
    use nits_protocol::{
        BlobOid, Cell, ChunkIndex, CommentContext, FileRender, FileRenderHeader, GapTable, LineNo,
        RenderChunk, RenderContent, RenderOpts, RenderTarget, RepoPath, Row, Side, TreeEntry,
        TreeEntryKind, TreeOid, TreeSnapshot,
    };
    let mut snapshot = snapshot();
    let revision = nits_protocol::ResolvedRef {
        tree: TreeOid::from_bytes([1; 20]),
        source: nits_protocol::ResolvedSource::WorkingTree {
            head: None,
            dirty: Vec::new(),
            branch: None,
        },
    };
    let h1 = NonEmpty::singleton(nits_protocol::ResolvedTarget {
        repo_id: snapshot.review.targets.first().repo_id,
        base: revision.clone(),
        head: revision,
    });
    snapshot.resolved = Some(h1.clone());
    let repo_id = snapshot.review.targets.iter().next().unwrap().repo_id;
    let path = RepoPath::new("unchanged.rs").unwrap();
    let oid = BlobOid::from_bytes([11; 20]);
    let reference = RefSpec::Tag { name: "v1".into() };
    let mut sim = Sim::new(snapshot, vec![human("ada"), human("bob")]);
    let cell = Cell {
        line_no: LineNo::new(1).unwrap(),
        text: "source".into(),
        spans: vec![],
        changed: vec![],
    };
    sim.browse_content(
        reference.clone(),
        TreeSnapshot {
            repo_id,
            root_oid: TreeOid::from_bytes([12; 20]),
            entries: vec![TreeEntry {
                path: path.clone(),
                kind: TreeEntryKind::File {
                    oid,
                    size: 7,
                    executable: false,
                },
            }],
        },
        FileRender {
            header: FileRenderHeader {
                repo_id,
                path: path.clone(),
                target: RenderTarget::Blob {
                    entry: nits_protocol::BlobEntry {
                        oid,
                        mode: nits_protocol::BlobMode::Regular,
                    },
                },
                opts: RenderOpts::default(),
                lang: None,
                content: RenderContent::Text {
                    total_rows: 1,
                    chunk_rows: 100,
                    chunk_count: 1,
                    highlighted: false,
                    additions: 0,
                    deletions: 0,
                    gaps: GapTable::default(),
                },
            },
            chunks: vec![RenderChunk {
                index: ChunkIndex::FIRST,
                rows: vec![Row::Context {
                    left: cell.clone(),
                    right: cell,
                }],
            }],
        },
    );
    for peer in [A, B] {
        sim.connect_and_open(peer).unwrap();
        sim.act(
            peer,
            Action::SetBrowseRef {
                repo_id,
                ref_spec: Some(reference.clone()),
            },
        )
        .unwrap();
        sim.settle();
        sim.act(peer, Action::SetTab { tab: Tab::Browse }).unwrap();
        sim.act(
            peer,
            Action::Viewport {
                file: FileRef {
                    repo_id,
                    path: path.clone(),
                },
                first_row: 0,
                last_row: 59,
            },
        )
        .unwrap();
        sim.settle();
    }
    sim.request_review(
        human("ada"),
        "review-agent".into(),
        "Check this file".into(),
    );
    sim.settle();
    for peer in [A, B] {
        sim.act(
            peer,
            Action::RunCommand {
                command: nits_client_core::Command::FocusRequests,
            },
        )
        .unwrap();
        assert_eq!(sim.client(peer).view().tab, Tab::Conversation);
        sim.act(peer, Action::SetTab { tab: Tab::Browse }).unwrap();
        sim.settle();
        assert_eq!(
            sim.client(peer).view().focus,
            nits_client_core::Focus::Diff {
                row: 0,
                side: Side::Head
            },
        );
        assert_eq!(
            sim.client(peer).view().diff.as_ref().unwrap().target,
            RenderTarget::Blob {
                entry: nits_protocol::BlobEntry {
                    oid,
                    mode: nits_protocol::BlobMode::Regular
                }
            }
        );
    }
    sim.act(
        A,
        Action::CommentLines {
            file: FileRef { repo_id, path },
            side: Side::Head,
            start_line: 1,
            end_line: 1,
        },
    )
    .unwrap();
    sim.act(
        A,
        Action::DraftSubmitted {
            body: "from browse".into(),
        },
    )
    .unwrap();
    assert!(sim.client(A).view().threads[0].pending);
    assert!(sim.client(B).view().threads.is_empty());
    sim.settle();
    sim.converged().unwrap();
    for peer in [A, B] {
        let view = sim.client(peer).view();
        assert_eq!(
            view.threads[0].context,
            Some(CommentContext::Browse {
                reference: reference.clone()
            })
        );
        assert!(!view.threads[0].pending);
        assert_eq!(
            view.diff.as_ref().unwrap().rows[0].threads[0].thread,
            view.threads[0].id
        );
        assert_eq!(view.threads[0].summary, "from browse");
    }
    // Deferral changes disposition only: a fresh client retains both the captured
    // Browse source and the separate durable request after the live update.
    let thread_id = sim.client(A).view().threads[0].id;
    let requests = sim.daemon_snapshot().requests.clone();
    sim.act(
        A,
        Action::DeferThread {
            thread_id,
            reason: "Fix tracked in the controller repository".parse().unwrap(),
            tracking_url: Some("https://example.com/issues/288".parse().unwrap()),
        },
    )
    .unwrap();
    sim.settle();
    sim.converged().unwrap();
    for peer in [A, B] {
        let view = sim.client(peer).view();
        assert!(matches!(
            view.threads[0].status,
            ThreadResolution::Deferred { .. }
        ));
        assert_eq!(
            view.threads[0].context,
            Some(CommentContext::Browse {
                reference: reference.clone()
            })
        );
        assert_eq!(view.requests, requests);
        assert_eq!(
            view.diff.as_ref().unwrap().rows[0].threads[0].thread,
            thread_id
        );
    }
    // Revision checks coexist with the unfixed Browse thread and its durable invitation.
    for peer in [A, B] {
        sim.act(
            peer,
            Action::SetFocus {
                focus: nits_client_core::Focus::ReviewRequest { index: 0 },
            },
        )
        .unwrap();
        sim.act(peer, Action::CheckRequested).unwrap();
    }
    assert!(sim.deliver_up(B));
    assert!(sim.deliver_up(A));
    let mut h2 = h1.clone();
    h2.iter_mut().next().unwrap().head.tree = TreeOid::from_bytes([2; 20]);
    sim.advance_targets(h2);
    sim.disconnect(B);
    sim.settle();
    sim.reconnect(B).unwrap();
    sim.settle();
    sim.converged().unwrap();
    for peer in [A, B] {
        let view = sim.client(peer).view();
        assert_eq!(view.checkpoints.len(), 2);
        assert!(
            view.checkpoints
                .iter()
                .all(|check| check.freshness == nits_protocol::CheckpointFreshness::Changed)
        );
        assert!(matches!(
            view.threads[0].status,
            ThreadResolution::Deferred { .. }
        ));
        assert_eq!(
            view.threads[0].context,
            Some(CommentContext::Browse {
                reference: reference.clone()
            })
        );
        assert!(view.review.as_ref().unwrap().snapshot.viewed.is_empty());
    }
    let mut fresh = Sim::new(sim.daemon_snapshot().clone(), vec![human("later reviewer")]);
    fresh.connect_and_open(A).unwrap();
    assert_eq!(fresh.client(A).view().threads, sim.client(A).view().threads);
    assert_eq!(fresh.client(A).view().requests, requests);
    assert_eq!(
        fresh.client(A).view().checkpoints,
        sim.client(A).view().checkpoints
    );
    sim.act(B, Action::UnresolveThread { thread_id }).unwrap();
    sim.settle();
    sim.converged().unwrap();
    assert_eq!(
        sim.client(A).view().threads[0].status,
        ThreadResolution::Open
    );
    assert_eq!(
        sim.client(A).view().threads[0].context,
        Some(CommentContext::Browse { reference })
    );
}

#[test]
fn competing_deferrals_preserve_winning_metadata_and_reopen_converges() {
    let mut sim = two_clients_one_thread();
    let thread_id = thread_id(&sim);
    let defer = |reason: &str| Action::DeferThread {
        thread_id,
        reason: reason.parse().unwrap(),
        tracking_url: Some("https://example.com/issues/288".parse().unwrap()),
    };
    sim.act(A, defer("External controller fix")).unwrap();
    sim.act(B, defer("Another scope decision")).unwrap();
    assert!(sim.deliver_up(A));
    assert!(sim.deliver_up(B));
    sim.settle();
    sim.converged().unwrap();
    assert!(sim.client(B).view().last_error.is_some());
    assert_eq!(sim.client(B).pending_count(), 0);
    let disposition = &sim.daemon_snapshot().threads[0].resolution;
    assert!(
        matches!(disposition, ThreadResolution::Deferred { reason, by, .. } if reason.to_string() == "External controller fix" && by == &human("ada"))
    );
    for peer in [A, B] {
        assert_eq!(&sim.client(peer).view().threads[0].status, disposition);
        assert!(!matches!(
            sim.client(peer).view().threads[0].status,
            ThreadResolution::Open
        ));
    }
    let mut fresh = Sim::new(sim.daemon_snapshot().clone(), vec![human("later reviewer")]);
    fresh.connect_and_open(A).unwrap();
    assert_eq!(&fresh.client(A).view().threads[0].status, disposition);
    assert_eq!(fresh.client(A).view().threads[0].comments[0].body, "root");
    sim.act(
        B,
        Action::Reply {
            thread_id,
            body: "Bug remains unfixed".into(),
        },
    )
    .unwrap();
    sim.act(A, Action::UnresolveThread { thread_id }).unwrap();
    sim.settle();
    sim.converged().unwrap();
    assert_eq!(
        sim.daemon_snapshot().threads[0].resolution,
        ThreadResolution::Open
    );
    assert_eq!(sim.daemon_snapshot().comments.len(), 2);
    // A stale deferral loses to resolution; optimistic pending state rolls back.
    sim.act(A, Action::ResolveThread { thread_id }).unwrap();
    sim.act(B, defer("External again")).unwrap();
    assert!(sim.deliver_up(A));
    assert!(sim.deliver_up(B));
    sim.settle();
    sim.converged().unwrap();
    assert!(matches!(
        sim.daemon_snapshot().threads[0].resolution,
        ThreadResolution::Resolved { .. }
    ));
}

#[test]
fn two_reviewers_keep_requested_h1_when_current_h2_arrives_before_checks() {
    use nits_protocol::{
        CheckpointFreshness, Oid, ResolvedRef, ResolvedSource, ResolvedTarget, TreeOid,
    };
    let mut initial = snapshot();
    let reference = |n| ResolvedRef {
        tree: TreeOid::new(Oid::from_bytes([n; 20])),
        source: ResolvedSource::WorkingTree {
            head: None,
            dirty: Vec::new(),
            branch: Some("feature".into()),
        },
    };
    let h1 = NonEmpty::singleton(ResolvedTarget {
        repo_id: initial.review.targets.first().repo_id,
        base: reference(1),
        head: reference(2),
    });
    initial.resolved = Some(h1.clone());
    let mut sim = Sim::new(initial, vec![human("ada"), human("bob")]);
    sim.connect_and_open(A).unwrap();
    sim.connect_and_open(B).unwrap();
    sim.request_review(human("ada"), "bob".into(), "Check H1".into());
    sim.settle();
    let mut h2 = h1.clone();
    h2.iter_mut().next().unwrap().head = reference(3);
    sim.advance_targets(h2);
    sim.settle();
    for peer in [A, B] {
        sim.act(
            peer,
            Action::SetFocus {
                focus: nits_client_core::Focus::ReviewRequest { index: 0 },
            },
        )
        .unwrap();
        sim.act(peer, Action::CheckRequested).unwrap();
    }
    assert!(sim.deliver_up(B));
    assert!(sim.deliver_up(A));
    sim.disconnect(B);
    sim.settle();
    sim.reconnect(B).unwrap();
    sim.settle();
    sim.converged().unwrap();
    for peer in [A, B] {
        let view = sim.client(peer).view();
        assert_eq!(view.checkpoints.len(), 2);
        assert!(
            view.checkpoints
                .iter()
                .all(|c| c.freshness == CheckpointFreshness::Changed && c.checkpoint.targets == h1)
        );
        assert!(view.review.as_ref().unwrap().snapshot.viewed.is_empty());
        assert!(view.threads.is_empty());
    }
    sim.act(A, Action::CheckCurrent).unwrap();
    sim.settle();
    sim.converged().unwrap();
    assert_eq!(
        sim.client(B)
            .view()
            .checkpoints
            .iter()
            .filter(|c| c.freshness == CheckpointFreshness::Current)
            .count(),
        1
    );
}
