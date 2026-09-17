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
