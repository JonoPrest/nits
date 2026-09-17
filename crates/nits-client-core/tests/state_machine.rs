//! Plan 3.1: every `Input` in every state either transitions or is rejected
//! with a typed error, never panics, and a rejection changes nothing.

use nits_client_core::{
    Action, CacheConfig, ClientCore, Config, Connection, ConnectionView, CoreError, Effect,
    FileRef, IdSeed, Input, RefSelectorSide, RefSelectorStatus, TransportEvent, ViewDelta,
};
use nits_protocol::{
    Anchor, Author, BuildInfo, ClientId, ClientMsg, ClientSeq, Comment, CommentKind, CommentState,
    Event, EventBody, Mutation, NonEmpty, Oid, ProtocolVersion, RefCandidate, RefSpec, RenderOpts,
    RepoId, Request, RequestId, ResolvedRef, ResolvedSource, ResolvedTarget, Response, Review,
    ReviewId, ReviewSnapshot, ReviewStatus, ReviewTarget, RpcError, SchemaVersion, Seq, ServerMsg,
    Since, StreamItem, SubscribeScope, ThreadId, Timestamp, TreeDelta, TreeOid, TreeSnapshot,
    ViewSection, WorkspaceId,
};
use proptest::prelude::*;

fn config() -> Config {
    Config {
        client_id: ClientId::from_parts(1, 1),
        client: BuildInfo {
            name: "test".into(),
            version: "0".into(),
        },
        author: Author::Human {
            name: "someone".into(),
            machine: "host".into(),
        },
        id_seed: IdSeed(7),
        cache: CacheConfig::default(),
    }
}

fn repo_id() -> RepoId {
    RepoId::from_parts(2, 2)
}

fn review(id: ReviewId) -> Review {
    Review {
        id,
        workspace_id: WorkspaceId::from_parts(3, 3),
        title: "a review".into(),
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

fn snapshot(id: ReviewId, seq: Seq) -> ReviewSnapshot {
    ReviewSnapshot {
        review: review(id),
        resolved: None,
        threads: Vec::new(),
        comments: Vec::new(),
        viewed: Vec::new(),
        requests: Vec::new(),
        seq,
    }
}

fn resolved(fill: u8) -> NonEmpty<ResolvedTarget> {
    let tree = TreeOid::new(Oid::from_bytes([fill; 20]));
    NonEmpty::singleton(ResolvedTarget {
        repo_id: repo_id(),
        base: ResolvedRef {
            tree,
            source: ResolvedSource::WorkingTree {
                dirty: Vec::new(),
                branch: None,
            },
        },
        head: ResolvedRef {
            tree,
            source: ResolvedSource::WorkingTree {
                dirty: Vec::new(),
                branch: None,
            },
        },
    })
}

fn event(seq: u64, body: EventBody) -> Event {
    Event {
        seq: Seq::new(seq),
        ts: Timestamp::from_millis(0),
        author: Author::Human {
            name: "other".into(),
            machine: "host".into(),
        },
        client_id: ClientId::from_parts(9, 9),
        client_seq: ClientSeq::new(1),
        body,
    }
}

fn comment(review_id: ReviewId, n: u128) -> Comment {
    let id = nits_protocol::CommentId::from_parts(5, n);
    Comment {
        id,
        review_id,
        thread_id: ThreadId::from_parts(5, n),
        author: Author::Human {
            name: "other".into(),
            machine: "host".into(),
        },
        kind: CommentKind::Note,
        anchor: Anchor::Review,
        body: "hello".into(),
        created: Timestamp::from_millis(0),
        edited: None,
        state: CommentState::Live,
        context: None,
    }
}

fn welcome() -> ServerMsg {
    ServerMsg::Welcome {
        protocol: ProtocolVersion::CURRENT,
        daemon: BuildInfo {
            name: "nitsd".into(),
            version: "0".into(),
        },
        schema: SchemaVersion::CURRENT,
        upgrade: None,
    }
}

fn rendered(effects: &[Effect]) -> Vec<ViewSection> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Render(ViewDelta { sections }) => Some(sections.clone()),
            Effect::Connect
            | Effect::Disconnect
            | Effect::Send(_)
            | Effect::Persist { .. }
            | Effect::Load { .. }
            | Effect::Remove { .. } => None,
        })
        .flatten()
        .collect()
}

fn sent_request(effects: &[Effect]) -> Option<(RequestId, Request)> {
    effects.iter().find_map(|e| match e {
        Effect::Send(ClientMsg::Request { id, request }) => Some((*id, request.clone())),
        Effect::Send(ClientMsg::Hello { .. } | ClientMsg::Cancel { .. })
        | Effect::Connect
        | Effect::Disconnect
        | Effect::Render(_)
        | Effect::Persist { .. }
        | Effect::Load { .. }
        | Effect::Remove { .. } => None,
    })
}

/// Drive a fresh core to `Subscribed { last_seq: seq }`.
fn subscribed(seq: u64) -> ClientCore {
    let mut core = ClientCore::new(config());
    let effects = core.handle(Input::User(Action::Connect)).unwrap();
    assert_eq!(effects[0], Effect::Connect);
    let effects = core
        .handle(Input::Transport(TransportEvent::Connected))
        .unwrap();
    assert!(
        matches!(&effects[0], Effect::Send(ClientMsg::Hello { protocol, .. }) if *protocol == ProtocolVersion::CURRENT)
    );
    let effects = core.handle(Input::Server(welcome())).unwrap();
    let (id, request) = sent_request(&effects).unwrap();
    assert_eq!(
        request,
        Request::Subscribe {
            scope: SubscribeScope::All,
            since: Since::Now
        }
    );
    let effects = core
        .handle(Input::Server(ServerMsg::Response {
            id,
            response: Response::Subscribed { seq: Seq::new(seq) },
        }))
        .unwrap();
    assert_eq!(rendered(&effects), vec![ViewSection::Connection]);
    assert_eq!(core.view().connection, ConnectionView::Subscribed);
    core
}

fn open(core: &mut ClientCore, id: ReviewId) {
    let effects = core
        .handle(Input::User(Action::OpenReview { review_id: id }))
        .unwrap();
    let (req, request) = sent_request(&effects).unwrap();
    assert_eq!(
        request,
        Request::OpenReview {
            review_id: id,
            opts: RenderOpts::default()
        }
    );
    core.handle(Input::Server(ServerMsg::StreamItem {
        id: req,
        item: StreamItem::ReviewSnapshot {
            snapshot: snapshot(id, Seq::new(1)),
        },
    }))
    .unwrap();
    core.handle(Input::Server(ServerMsg::StreamEnd { id: req }))
        .unwrap();
}

fn load_ref_selector(core: &mut ClientCore, side: RefSelectorSide) {
    let effects = core
        .handle(Input::User(Action::OpenRefSelector {
            repo_id: repo_id(),
            side,
        }))
        .unwrap();
    let (request_id, request) = sent_request(&effects).unwrap();
    assert_eq!(request, Request::ListRefs { repo_id: repo_id() });
    assert!(matches!(
        core.view()
            .ref_selector
            .as_ref()
            .map(|selector| &selector.status),
        Some(RefSelectorStatus::Loading)
    ));
    core.handle(Input::Server(ServerMsg::Response {
        id: request_id,
        response: Response::Refs {
            repo_id: repo_id(),
            refs: vec![
                RefCandidate {
                    ref_spec: RefSpec::Branch {
                        name: "feature/selector".into(),
                    },
                    subject: None,
                },
                RefCandidate {
                    ref_spec: RefSpec::WorkingTree,
                    subject: None,
                },
            ],
        },
    }))
    .unwrap();
}

fn filter_to_feature(core: &mut ClientCore) {
    core.handle(Input::User(Action::RefSelectorQuery {
        query: "fsel".into(),
    }))
    .unwrap();
    let selector = core.view().ref_selector.as_ref().unwrap();
    assert_eq!(selector.options.len(), 1);
    assert_eq!(
        selector.options[0].ref_spec,
        RefSpec::Branch {
            name: "feature/selector".into()
        }
    );
}

#[test]
fn ref_selector_loads_filters_and_excludes_working_tree_from_base() {
    let review_id = ReviewId::from_parts(4, 4);
    let mut core = subscribed(1);
    open(&mut core, review_id);
    load_ref_selector(&mut core, RefSelectorSide::Base);
    let selector = core.view().ref_selector.as_ref().unwrap();
    assert!(
        selector
            .options
            .iter()
            .all(|option| option.ref_spec != RefSpec::WorkingTree)
    );
    assert!(selector.options.iter().any(|option| option.current));
    insta::assert_json_snapshot!("ref_selector_open", selector);

    filter_to_feature(&mut core);
}

#[test]
fn ref_selector_updates_the_typed_target_and_rolls_back_invalid_refs() {
    let review_id = ReviewId::from_parts(4, 4);
    let mut core = subscribed(1);
    open(&mut core, review_id);
    load_ref_selector(&mut core, RefSelectorSide::Base);
    filter_to_feature(&mut core);

    let effects = core
        .handle(Input::User(Action::SelectRef { index: 0 }))
        .unwrap();
    let (mutation_id, request) = sent_request(&effects).unwrap();
    assert_eq!(
        request,
        Request::Mutate {
            client_seq: ClientSeq::new(1),
            mutation: Mutation::UpdateReviewTarget {
                review_id,
                update: nits_protocol::ReviewTargetUpdate {
                    repo_id: repo_id(),
                    revision: nits_protocol::TargetRevision::Base {
                        ref_spec: nits_protocol::BaseRefSpec::Branch {
                            name: "feature/selector".into()
                        }
                    }
                }
            }
        }
    );
    assert!(matches!(
        core.view()
            .ref_selector
            .as_ref()
            .map(|selector| &selector.status),
        Some(RefSelectorStatus::Saving)
    ));
    assert_eq!(
        core.view()
            .review
            .as_ref()
            .unwrap()
            .snapshot
            .review
            .targets
            .first()
            .base,
        RefSpec::Branch {
            name: "feature/selector".into()
        }
    );
    core.handle(Input::Server(ServerMsg::Error {
        id: mutation_id,
        error: RpcError::Invalid {
            reason: "revision disappeared".into(),
        },
    }))
    .unwrap();
    assert!(matches!(
        core.view()
            .ref_selector
            .as_ref()
            .map(|selector| &selector.status),
        Some(RefSelectorStatus::InvalidRef { message }) if message == "revision disappeared"
    ));
    assert_eq!(
        core.view()
            .review
            .as_ref()
            .unwrap()
            .snapshot
            .review
            .targets
            .first()
            .base,
        RefSpec::Branch {
            name: "main".into()
        }
    );
}

#[test]
fn ref_selector_motion_then_enter_uses_core_state_without_a_render_round_trip() {
    let review_id = ReviewId::from_parts(4, 4);
    let mut core = subscribed(1);
    open(&mut core, review_id);
    load_ref_selector(&mut core, RefSelectorSide::Head);

    let before = core.view().ref_selector.as_ref().unwrap().selected;
    core.handle(Input::User(Action::RefSelectorStep { delta: 1 }))
        .unwrap();
    let selected = core.view().ref_selector.as_ref().unwrap().selected;
    assert_ne!(selected, before);

    let effects = core.handle(Input::User(Action::SelectCurrentRef)).unwrap();
    let (_, request) = sent_request(&effects).unwrap();
    let Request::Mutate {
        mutation: Mutation::UpdateReviewTarget { update, .. },
        ..
    } = request
    else {
        panic!("Enter must update the highlighted ref");
    };
    assert_eq!(
        update.revision,
        nits_protocol::TargetRevision::Head {
            ref_spec: RefSpec::WorkingTree,
        }
    );
}

#[test]
fn disconnected_ref_selection_is_rejected_without_changing_selector_state() {
    let review_id = ReviewId::from_parts(4, 4);
    let mut core = subscribed(1);
    open(&mut core, review_id);
    load_ref_selector(&mut core, RefSelectorSide::Head);
    core.handle(Input::Transport(TransportEvent::Disconnected))
        .unwrap();
    let before = core.view().ref_selector.clone();

    let error = core
        .handle(Input::User(Action::SelectCurrentRef))
        .unwrap_err();

    assert!(matches!(error, CoreError::WrongConnectionState { .. }));
    // A rejected input does not derive the public view.  A subsequent
    // successful input proves the private selector was not corrupted either.
    core.handle(Input::Tick(1_000)).unwrap();
    assert_eq!(core.view().ref_selector, before);
    assert_eq!(core.pending_count(), 0);
}

#[test]
fn response_first_ref_selection_ignores_its_echo_then_refreshes_the_diff() {
    let review_id = ReviewId::from_parts(4, 4);
    let mut core = subscribed(1);
    open(&mut core, review_id);
    load_ref_selector(&mut core, RefSelectorSide::Base);
    filter_to_feature(&mut core);

    let effects = core
        .handle(Input::User(Action::SelectRef { index: 0 }))
        .unwrap();
    let (request_id, request) = sent_request(&effects).unwrap();
    let Request::Mutate { client_seq, .. } = request else {
        panic!("selection must send a mutation");
    };
    let target = core
        .view()
        .review
        .as_ref()
        .unwrap()
        .snapshot
        .review
        .targets
        .first()
        .clone();
    let committed = Event {
        seq: Seq::new(2),
        ts: Timestamp::from_millis(0),
        author: config().author,
        client_id: core.client_id(),
        client_seq,
        body: EventBody::ReviewTargetUpdated { review_id, target },
    };
    core.handle(Input::Server(ServerMsg::Response {
        id: request_id,
        response: Response::Committed {
            event: committed.clone(),
        },
    }))
    .unwrap();
    assert!(core.view().ref_selector.is_none());
    assert_eq!(core.pending_count(), 0);

    let effects = core
        .handle(Input::Server(ServerMsg::Event { event: committed }))
        .unwrap();
    assert!(effects.is_empty(), "the broadcast echo is a no-op");

    let effects = core
        .handle(Input::Server(ServerMsg::Event {
            event: Event {
                ..event(
                    3,
                    EventBody::ReviewTargetsResolved {
                        review_id,
                        targets: resolved(8),
                    },
                )
            },
        }))
        .unwrap();
    let requests = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Send(ClientMsg::Request { request, .. }) => Some(request),
            Effect::Connect
            | Effect::Disconnect
            | Effect::Send(ClientMsg::Hello { .. } | ClientMsg::Cancel { .. })
            | Effect::Persist { .. }
            | Effect::Load { .. }
            | Effect::Remove { .. }
            | Effect::Render(_) => None,
        })
        .collect::<Vec<_>>();
    assert!(
        requests
            .iter()
            .any(|request| matches!(request, Request::ListFiles { .. }))
    );
    assert!(
        requests
            .iter()
            .any(|request| matches!(request, Request::TreeSnapshot { .. }))
    );
}

#[test]
fn ref_selector_keeps_errors_visible_and_escape_cancels() {
    let review_id = ReviewId::from_parts(4, 4);
    let mut core = subscribed(1);
    open(&mut core, review_id);
    let effects = core
        .handle(Input::User(Action::OpenRefSelector {
            repo_id: repo_id(),
            side: RefSelectorSide::Head,
        }))
        .unwrap();
    let (request_id, _) = sent_request(&effects).unwrap();
    core.handle(Input::Server(ServerMsg::Error {
        id: request_id,
        error: RpcError::Internal {
            message: "git unavailable".into(),
        },
    }))
    .unwrap();
    assert!(matches!(
        core.view().ref_selector.as_ref().map(|selector| &selector.status),
        Some(RefSelectorStatus::DaemonError { message }) if message == "git unavailable"
    ));
    core.handle(Input::User(Action::CloseRefSelector)).unwrap();
    assert!(core.view().ref_selector.is_none());
    insta::assert_json_snapshot!("ref_selector_closed", core.view().ref_selector);
}

#[test]
fn handshake_then_subscribe_then_reconnect_resumes_after_last_seq() {
    let mut core = subscribed(10);
    let effects = core
        .handle(Input::Server(ServerMsg::Event {
            event: event(
                11,
                EventBody::ReviewCreated {
                    review: review(ReviewId::from_parts(4, 4)),
                },
            ),
        }))
        .unwrap();
    assert_eq!(rendered(&effects), vec![ViewSection::ReviewList]);
    assert_eq!(core.view().reviews.len(), 1);

    let effects = core
        .handle(Input::Transport(TransportEvent::Disconnected))
        .unwrap();
    assert_eq!(rendered(&effects), vec![ViewSection::Connection]);
    assert_eq!(
        *core.connection(),
        Connection::Disconnected {
            last_seq: Some(Seq::new(11))
        }
    );

    core.handle(Input::User(Action::Connect)).unwrap();
    core.handle(Input::Transport(TransportEvent::Connected))
        .unwrap();
    let effects = core.handle(Input::Server(welcome())).unwrap();
    let (_, request) = sent_request(&effects).unwrap();
    assert_eq!(
        request,
        Request::Subscribe {
            scope: SubscribeScope::All,
            since: Since::After { seq: Seq::new(11) }
        }
    );
}

#[test]
fn rejected_handshake_is_a_typed_error_and_shows_why() {
    let mut core = ClientCore::new(config());
    core.handle(Input::User(Action::Connect)).unwrap();
    core.handle(Input::Transport(TransportEvent::Connected))
        .unwrap();
    let error = RpcError::UnsupportedProtocol {
        requested: ProtocolVersion::CURRENT,
        supported: vec![],
    };
    let err = core
        .handle(Input::Server(ServerMsg::Rejected {
            error: error.clone(),
        }))
        .unwrap_err();
    assert_eq!(err, CoreError::Rejected(error.clone()));
    assert_eq!(core.view().connection, ConnectionView::Rejected { error });
    assert!(matches!(core.connection(), Connection::Disconnected { .. }));
}

#[test]
fn inputs_out_of_state_are_rejected_without_change() {
    let mut core = ClientCore::new(config());
    let before = core.view().clone();
    for input in [
        Input::User(Action::Disconnect),
        Input::User(Action::ListReviews {
            workspace_id: WorkspaceId::from_parts(3, 3),
        }),
        Input::User(Action::OpenReview {
            review_id: ReviewId::from_parts(4, 4),
        }),
        Input::User(Action::CloseReview),
        Input::User(Action::DraftOpened {
            anchor: Anchor::Review,
        }),
        Input::User(Action::DraftSubmitted { body: "x".into() }),
        Input::User(Action::DraftDiscarded),
        Input::Server(welcome()),
        Input::Server(ServerMsg::Response {
            id: RequestId::new(1),
            response: Response::Unsubscribed,
        }),
        Input::Server(ServerMsg::StreamEnd {
            id: RequestId::new(1),
        }),
        Input::Server(ServerMsg::Event {
            event: event(
                1,
                EventBody::ReviewDeleted {
                    review_id: ReviewId::from_parts(4, 4),
                },
            ),
        }),
        Input::Stored {
            key: "k".into(),
            value: None,
        },
    ] {
        let err = core.handle(input.clone()).unwrap_err();
        assert_eq!(*core.view(), before, "{input:?} changed the view via {err}");
        assert_eq!(
            *core.connection(),
            Connection::Disconnected { last_seq: None }
        );
    }
}

// One scenario end to end; splitting it would hide the ordering it checks.
#[allow(clippy::too_many_lines)]
#[test]
fn draft_lifecycle_defers_refresh_and_submits_one_add_comment() {
    let id = ReviewId::from_parts(4, 4);
    let mut core = subscribed(1);
    open(&mut core, id);
    core.handle(Input::Tick(1_000)).unwrap();

    assert_eq!(
        core.handle(Input::User(Action::DraftSubmitted { body: "x".into() })),
        Err(CoreError::NoDraft)
    );
    let effects = core
        .handle(Input::User(Action::DraftOpened {
            anchor: Anchor::Review,
        }))
        .unwrap();
    assert_eq!(
        rendered(&effects),
        vec![ViewSection::Draft, ViewSection::Focus, ViewSection::Hints]
    );
    assert_eq!(
        core.handle(Input::User(Action::DraftOpened {
            anchor: Anchor::Review
        })),
        Err(CoreError::DraftAlreadyOpen)
    );

    // A working-tree refresh while the editor is open is held back.
    let effects = core
        .handle(Input::Server(ServerMsg::Event {
            event: event(
                2,
                EventBody::ReviewTargetsResolved {
                    review_id: id,
                    targets: resolved(1),
                },
            ),
        }))
        .unwrap();
    assert_eq!(rendered(&effects), vec![ViewSection::Draft]);
    assert!(core.view().pending_refresh);
    assert_eq!(core.view().review.as_ref().unwrap().snapshot.resolved, None);
    // A second one only re-queues; nothing new to render.
    let effects = core
        .handle(Input::Server(ServerMsg::Event {
            event: event(
                3,
                EventBody::ReviewTargetsResolved {
                    review_id: id,
                    targets: resolved(2),
                },
            ),
        }))
        .unwrap();
    assert!(effects.is_empty());

    let effects = core
        .handle(Input::User(Action::DraftSubmitted {
            body: "looks fine".into(),
        }))
        .unwrap();
    let (_, request) = sent_request(&effects).unwrap();
    let Request::Mutate {
        client_seq,
        mutation:
            Mutation::AddComment {
                review_id,
                anchor,
                body,
                kind,
                comment_id,
                context: _,
            },
    } = request
    else {
        panic!("expected AddComment, got {request:?}");
    };
    assert_eq!(
        (client_seq, review_id, anchor, body, kind),
        (
            ClientSeq::new(1),
            id,
            Anchor::Review,
            "looks fine".to_string(),
            CommentKind::Note
        )
    );
    assert_eq!(comment_id.timestamp_ms(), 1_000);
    // The comment shows at once (pending), then the held-back refreshes
    // land as the draft closes.
    assert_eq!(
        rendered(&effects),
        vec![
            ViewSection::Threads,
            ViewSection::Draft,
            ViewSection::Focus, // composer closed
            ViewSection::Diff,
            ViewSection::ReviewList, // refreshed targets ride the review list
            ViewSection::Conversation, // the comment is review-anchored
            ViewSection::Hints,
        ]
    );
    let open = core.view().review.as_ref().unwrap();
    assert_eq!(open.pending.len(), 1);
    assert_eq!(open.snapshot.comments.len(), 1);
    assert_eq!(open.snapshot.comments[0].id, comment_id);
    assert_eq!(open.snapshot.threads.len(), 1);
    assert!(!core.view().pending_refresh);
    assert!(core.view().draft.is_none());
    assert_eq!(
        core.view().review.as_ref().unwrap().snapshot.resolved,
        Some(resolved(2))
    );

    // Second draft mints a different id and the next client_seq.
    core.handle(Input::User(Action::DraftOpened {
        anchor: Anchor::Review,
    }))
    .unwrap();
    let effects = core
        .handle(Input::User(Action::DraftSubmitted {
            body: "again".into(),
        }))
        .unwrap();
    let Some((
        _,
        Request::Mutate {
            client_seq: seq2,
            mutation: Mutation::AddComment {
                comment_id: id2, ..
            },
        },
    )) = sent_request(&effects)
    else {
        panic!("expected a second AddComment");
    };
    assert_eq!(seq2, ClientSeq::new(2));
    assert_ne!(id2, comment_id);
}

#[test]
fn events_update_only_what_they_touch() {
    let id = ReviewId::from_parts(4, 4);
    let other = ReviewId::from_parts(6, 6);
    let mut core = subscribed(1);
    open(&mut core, id);

    let effects = core
        .handle(Input::Server(ServerMsg::Event {
            event: event(
                2,
                EventBody::CommentCreated {
                    comment: comment(other, 1),
                },
            ),
        }))
        .unwrap();
    assert!(
        effects.is_empty(),
        "comment on another review renders nothing"
    );

    let effects = core
        .handle(Input::Server(ServerMsg::Event {
            event: event(
                3,
                EventBody::CommentCreated {
                    comment: comment(id, 2),
                },
            ),
        }))
        .unwrap();
    assert_eq!(
        rendered(&effects),
        vec![ViewSection::Threads, ViewSection::Conversation]
    );
    assert_eq!(
        core.view().review.as_ref().unwrap().snapshot.comments.len(),
        1
    );

    assert_eq!(
        core.handle(Input::Server(ServerMsg::Event {
            event: event(
                3,
                EventBody::CommentCreated {
                    comment: comment(id, 3)
                }
            ),
        })),
        Err(CoreError::StaleEvent {
            seq: Seq::new(3),
            last_seq: Seq::new(3)
        })
    );

    let effects = core
        .handle(Input::Server(ServerMsg::Event {
            event: event(4, EventBody::ReviewDeleted { review_id: id }),
        }))
        .unwrap();
    assert_eq!(
        rendered(&effects),
        vec![
            ViewSection::ReviewList,
            ViewSection::Diff,
            ViewSection::Threads,
            ViewSection::Draft,
            ViewSection::Conversation,
            ViewSection::Focus, // back to the review list
            ViewSection::Hints,
        ]
    );
    assert!(core.view().review.is_none());
    assert_eq!(
        core.handle(Input::User(Action::CloseReview)),
        Err(CoreError::NoOpenReview)
    );
}

#[test]
fn mismatched_and_unknown_responses_are_typed() {
    let mut core = subscribed(1);
    let effects = core
        .handle(Input::User(Action::ListReviews {
            workspace_id: WorkspaceId::from_parts(3, 3),
        }))
        .unwrap();
    let (id, _) = sent_request(&effects).unwrap();
    assert_eq!(
        core.handle(Input::Server(ServerMsg::Response {
            id,
            response: Response::Unsubscribed
        })),
        Err(CoreError::UnexpectedResponse {
            id,
            expected: "Reviews",
            got: "Unsubscribed"
        })
    );
    let bogus = RequestId::new(999);
    assert_eq!(
        core.handle(Input::Server(ServerMsg::Response {
            id: bogus,
            response: Response::Unsubscribed
        })),
        Err(CoreError::UnknownRequest(bogus))
    );
    // The real answer still lands.
    let effects = core
        .handle(Input::Server(ServerMsg::Response {
            id,
            response: Response::Reviews {
                reviews: vec![review(ReviewId::from_parts(4, 4))],
            },
        }))
        .unwrap();
    assert_eq!(rendered(&effects), vec![ViewSection::ReviewList]);
    // And answering twice is unknown.
    assert_eq!(
        core.handle(Input::Server(ServerMsg::Response {
            id,
            response: Response::Reviews { reviews: vec![] }
        })),
        Err(CoreError::UnknownRequest(id))
    );
}

#[test]
fn draft_anchors_at_the_head_seen_when_opened_and_the_daemon_reanchors() {
    use nits_protocol::{BlobOid, CommentState, ContextHash, LineNo, LineRange, RepoPath, Side};
    let id = ReviewId::from_parts(4, 1);
    let mut core = subscribed(1);
    open(&mut core, id);
    core.handle(Input::Tick(1_000)).unwrap();
    let anchor_at = |fill: u8, line: u32| Anchor::Lines {
        repo_id: repo_id(),
        path: RepoPath::new("src/lib.rs").unwrap(),
        side: Side::Head,
        blob_oid: BlobOid::new(Oid::from_bytes([fill; 20])),
        lines: LineRange::single(LineNo::new(line).unwrap()),
        context_hash: ContextHash::new(7),
    };
    // The host opens the editor against the head it is showing (blob 1).
    core.handle(Input::User(Action::DraftOpened {
        anchor: anchor_at(1, 10),
    }))
    .unwrap();
    // The working tree moves on while the user types: held back.
    let effects = core
        .handle(Input::Server(ServerMsg::Event {
            event: event(
                2,
                EventBody::ReviewTargetsResolved {
                    review_id: id,
                    targets: resolved(2),
                },
            ),
        }))
        .unwrap();
    assert_eq!(rendered(&effects), vec![ViewSection::Draft]);
    assert!(core.view().pending_refresh);
    assert_eq!(core.view().review.as_ref().unwrap().snapshot.resolved, None);

    // Submit: the mutation carries the anchor from draft-open time, the
    // refresh lands afterwards, and the optimistic comment shows.
    let effects = core
        .handle(Input::User(Action::DraftSubmitted {
            body: "on the old head".into(),
        }))
        .unwrap();
    let (req, request) = sent_request(&effects).unwrap();
    let Request::Mutate {
        client_seq,
        mutation: Mutation::AddComment {
            anchor, comment_id, ..
        },
    } = request
    else {
        panic!("expected AddComment, got {request:?}");
    };
    assert_eq!(anchor, anchor_at(1, 10));
    let open = core.view().review.as_ref().unwrap();
    assert_eq!(open.snapshot.resolved, Some(resolved(2)));
    assert_eq!(open.pending.len(), 1);
    assert_eq!(open.snapshot.comments[0].anchor, anchor_at(1, 10));
    assert_eq!(open.snapshot.comments[0].state, CommentState::Live);

    // The daemon commits it as-is (echo retires the pending entry)...
    let committed = Event {
        seq: Seq::new(3),
        ts: Timestamp::from_millis(1_000),
        author: config().author,
        client_id: core.client_id(),
        client_seq,
        body: EventBody::CommentCreated {
            comment: core.view().review.as_ref().unwrap().snapshot.comments[0].clone(),
        },
    };
    let effects = core
        .handle(Input::Server(ServerMsg::Response {
            id: req,
            response: Response::Committed { event: committed },
        }))
        .unwrap();
    assert_eq!(rendered(&effects), vec![ViewSection::Threads]);
    assert!(core.view().review.as_ref().unwrap().pending.is_empty());
    assert_eq!(core.pending_count(), 0);

    // ...then re-anchors it against the new head: mapped to blob 2, line 12,
    // or marked outdated when it cannot be mapped.
    let effects = core
        .handle(Input::Server(ServerMsg::Event {
            event: event(
                4,
                EventBody::CommentReanchored {
                    review_id: id,
                    comment_id,
                    anchor: anchor_at(2, 12),
                    state: CommentState::Outdated {
                        last_good_anchor: anchor_at(1, 10),
                    },
                },
            ),
        }))
        .unwrap();
    assert_eq!(rendered(&effects), vec![ViewSection::Threads]);
    let c = &core.view().review.as_ref().unwrap().snapshot.comments[0];
    assert_eq!(c.anchor, anchor_at(2, 12));
    assert_eq!(
        c.state,
        CommentState::Outdated {
            last_good_anchor: anchor_at(1, 10)
        }
    );
}

// ---- proptest: random input sequences never panic; rejections are no-ops ----

fn review_id_strategy() -> impl Strategy<Value = ReviewId> {
    (1u128..3).prop_map(|n| ReviewId::from_parts(4, n))
}

fn action_strategy() -> impl Strategy<Value = Action> {
    prop_oneof![
        Just(Action::Connect),
        Just(Action::Disconnect),
        Just(Action::ListReviews {
            workspace_id: WorkspaceId::from_parts(3, 3)
        }),
        review_id_strategy().prop_map(|review_id| Action::OpenReview { review_id }),
        Just(Action::CloseReview),
        Just(Action::DraftOpened {
            anchor: Anchor::Review
        }),
        Just(Action::DraftSubmitted {
            body: "body".into()
        }),
        Just(Action::DraftDiscarded),
        (0u32..3000, 0u32..3000).prop_map(|(a, b)| Action::Viewport {
            file: FileRef {
                repo_id: repo_id(),
                path: nits_protocol::RepoPath::new("a.rs").unwrap(),
            },
            first_row: a,
            last_row: b,
        }),
        Just(Action::CloseFile),
        prop_oneof![Just(None), Just(Some("src".to_string()))].prop_map(|p| Action::ToggleDir {
            repo_id: repo_id(),
            path: p.map(|p| nits_protocol::RepoPath::new(p).unwrap()),
        }),
        prop_oneof![Just(None), Just(Some("a".to_string()))]
            .prop_map(|query| Action::FileSearch { query }),
        prop_oneof![
            Just(nits_client_core::Layout::Unified),
            Just(nits_client_core::Layout::Split)
        ]
        .prop_map(|layout| Action::SetLayout { layout }),
        (any::<bool>(), 0u32..6).prop_map(|(ignore_whitespace, context_lines)| {
            Action::SetRenderOpts {
                ignore_whitespace,
                context_lines,
            }
        }),
        Just(Action::MarkViewed {
            file: FileRef {
                repo_id: repo_id(),
                path: nits_protocol::RepoPath::new("a.rs").unwrap(),
            }
        }),
        Just(Action::UnmarkViewed {
            file: FileRef {
                repo_id: repo_id(),
                path: nits_protocol::RepoPath::new("a.rs").unwrap(),
            }
        }),
        (1u128..4).prop_map(|n| Action::ReplyOpened {
            thread_id: ThreadId::from_parts(5, n)
        }),
        prop_oneof![
            (0usize..3).prop_map(|index| nits_client_core::Focus::ReviewList { index }),
            (0usize..3).prop_map(|index| nits_client_core::Focus::Tree { index }),
            (0u32..300).prop_map(|row| nits_client_core::Focus::Diff {
                row,
                side: nits_protocol::Side::Head,
            }),
            (0usize..3).prop_map(|index| nits_client_core::Focus::Thread { index }),
            Just(nits_client_core::Focus::Composer),
            (0usize..3).prop_map(|index| nits_client_core::Focus::CommitStepper { index }),
            Just(nits_client_core::Focus::Help),
        ]
        .prop_map(|focus| Action::SetFocus { focus }),
        Just(Action::ToggleHelp),
    ]
}

fn key_strategy() -> impl Strategy<Value = nits_client_core::KeyChord> {
    use nits_client_core::{KeyChord, NamedKey};
    prop_oneof![
        prop::sample::select(vec![
            'j', 'k', 'g', 'G', ']', '[', 'f', 'c', 'v', 'r', 'x', 's', 'w', '?', 'n', 'p', 'z'
        ])
        .prop_map(KeyChord::char),
        prop::sample::select(vec![
            NamedKey::Enter,
            NamedKey::Esc,
            NamedKey::Tab,
            NamedKey::Down,
            NamedKey::Up
        ])
        .prop_map(KeyChord::named),
        Just({
            let mut c = KeyChord::char('p');
            c.mods.ctrl = true;
            c
        }),
    ]
}

fn stream_item_strategy() -> impl Strategy<Value = StreamItem> {
    let header = |chunks: u32| nits_protocol::FileRenderHeader {
        repo_id: repo_id(),
        path: nits_protocol::RepoPath::new("a.rs").unwrap(),
        target: nits_protocol::RenderTarget::Diff {
            change: nits_protocol::ChangeKind::Added {
                new: nits_protocol::BlobOid::new(Oid::from_bytes([1; 20])),
            },
        },
        opts: nits_protocol::RenderOpts::default(),
        lang: None,
        content: nits_protocol::RenderContent::Text {
            total_rows: chunks * 100,
            chunk_rows: 100,
            chunk_count: chunks,
            highlighted: false,
            additions: 0,
            deletions: 0,
            gaps: nits_protocol::GapTable::default(),
        },
    };
    prop_oneof![
        review_id_strategy().prop_map(|id| StreamItem::ReviewSnapshot {
            snapshot: snapshot(id, Seq::new(1))
        }),
        (1u8..3).prop_map(|fill| StreamItem::TreeSnapshot {
            snapshot: TreeSnapshot {
                repo_id: repo_id(),
                root_oid: TreeOid::new(Oid::from_bytes([fill; 20])),
                entries: Vec::new(),
            }
        }),
        (1u32..30).prop_map(move |chunks| StreamItem::Header {
            header: header(chunks)
        }),
        (0u32..30).prop_map(|i| StreamItem::Chunk {
            repo_id: repo_id(),
            path: nits_protocol::RepoPath::new("a.rs").unwrap(),
            chunk: nits_protocol::RenderChunk {
                index: nits_protocol::ChunkIndex::new(i),
                rows: Vec::new(),
            },
        }),
    ]
}

fn body_strategy() -> impl Strategy<Value = EventBody> {
    prop_oneof![
        review_id_strategy().prop_map(|id| EventBody::ReviewCreated { review: review(id) }),
        review_id_strategy().prop_map(|review_id| EventBody::ReviewDeleted { review_id }),
        review_id_strategy().prop_map(|review_id| EventBody::ReviewTargetsResolved {
            review_id,
            targets: resolved(1)
        }),
        (review_id_strategy(), 1u128..4).prop_map(|(id, n)| EventBody::CommentCreated {
            comment: comment(id, n)
        }),
        (review_id_strategy(), 1u128..4).prop_map(|(id, n)| EventBody::CommentDeleted {
            review_id: id,
            comment_id: nits_protocol::CommentId::from_parts(5, n)
        }),
    ]
}

fn response_strategy() -> impl Strategy<Value = Response> {
    prop_oneof![
        (1u64..20).prop_map(|s| Response::Subscribed { seq: Seq::new(s) }),
        Just(Response::Reviews { reviews: vec![] }),
        review_id_strategy().prop_map(|id| Response::ReviewSnapshot {
            snapshot: snapshot(id, Seq::new(1))
        }),
        (1u64..20, body_strategy()).prop_map(|(s, b)| Response::Committed { event: event(s, b) }),
        Just(Response::Unsubscribed),
    ]
}

fn server_strategy() -> impl Strategy<Value = ServerMsg> {
    let id = (1u64..6).prop_map(RequestId::new);
    prop_oneof![
        Just(welcome()),
        Just(ServerMsg::Rejected {
            error: RpcError::Cancelled
        }),
        (id.clone(), response_strategy())
            .prop_map(|(id, response)| ServerMsg::Response { id, response }),
        (id.clone(), stream_item_strategy())
            .prop_map(|(id, item)| ServerMsg::StreamItem { id, item }),
        id.clone().prop_map(|id| ServerMsg::StreamEnd { id }),
        (1u8..3, 1u8..3).prop_map(|(from, to)| ServerMsg::TreeDelta {
            delta: TreeDelta {
                repo_id: repo_id(),
                from_root: TreeOid::new(Oid::from_bytes([from; 20])),
                to_root: TreeOid::new(Oid::from_bytes([to; 20])),
                added: Vec::new(),
                removed: Vec::new(),
                changed: Vec::new(),
            }
        }),
        id.prop_map(|id| ServerMsg::Error {
            id,
            error: RpcError::Cancelled
        }),
        (1u64..20, body_strategy()).prop_map(|(s, b)| ServerMsg::Event { event: event(s, b) }),
    ]
}

fn input_strategy() -> impl Strategy<Value = Input> {
    prop_oneof![
        4 => action_strategy().prop_map(Input::User),
        4 => server_strategy().prop_map(Input::Server),
        2 => prop_oneof![Just(TransportEvent::Connected), Just(TransportEvent::Disconnected)].prop_map(Input::Transport),
        1 => Just(Input::Stored { key: "k".into(), value: None }),
        1 => (0u64..10_000).prop_map(Input::Tick),
        3 => key_strategy().prop_map(Input::Key),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]
    #[test]
    fn any_sequence_is_handled_or_rejected(inputs in prop::collection::vec(input_strategy(), 0..60)) {
        let mut core = ClientCore::new(config());
        for input in inputs {
            let view = core.view().clone();
            let connection = *core.connection();
            let cache_len = core.cache().len();
            match core.handle(input.clone()) {
                Ok(effects) => {
                    // Focus is always valid for the current lists.
                    prop_assert_eq!(
                        nits_client_core::clamp_focus(core.view(), core.view().focus),
                        core.view().focus,
                        "focus out of range after {:?}", input
                    );
                    for e in &effects {
                        if let Effect::Render(delta) = e {
                            prop_assert!(!delta.sections.is_empty(), "empty render for {input:?}");
                            let mut dedup = delta.sections.clone();
                            dedup.dedup();
                            prop_assert_eq!(&dedup, &delta.sections);
                        }
                    }
                    if let (Connection::Subscribed { last_seq: before }, Connection::Subscribed { last_seq: after }) = (connection, *core.connection()) {
                        prop_assert!(after >= before, "seq went backwards on {input:?}");
                    }
                }
                Err(CoreError::Rejected(_)) => {
                    // The one rejection that moves state: the daemon is gone.
                    let down = matches!(core.connection(), Connection::Disconnected { .. });
                    prop_assert!(down, "rejected handshake left the connection up");
                }
                Err(_) => {
                    // A rejected key is still a key the core was handed,
                    // and the view says which key that was — a host
                    // attaching afterwards reads the sequence the core is
                    // actually at, not the last one that produced a
                    // patch. Nothing else about the view may move.
                    let mut before = view.clone();
                    if matches!(input, Input::Key(_)) {
                        let seq = before.last_key.map_or(0, |k| k.seq) + 1;
                        prop_assert_eq!(
                            core.view().last_key,
                            Some(nits_client_core::LastKey { seq, command: None }),
                            "a rejected key is counted and reported"
                        );
                        before.last_key = core.view().last_key;
                    }
                    prop_assert_eq!(core.view(), &before, "rejected {:?} changed the view", input);
                    prop_assert_eq!(*core.connection(), connection);
                    prop_assert_eq!(core.cache().len(), cache_len, "rejected {:?} changed the cache", input);
                }
            }
        }
    }
}

#[test]
fn request_delivered_ahead_of_older_open_snapshot_survives_handoff() {
    let mut core = subscribed(1);
    let review_id = ReviewId::from_parts(4, 1);
    let effects = core
        .handle(Input::User(Action::OpenReview { review_id }))
        .unwrap();
    let (id, _) = sent_request(&effects).unwrap();
    let request = event(
        2,
        EventBody::ReviewRequested {
            review_id,
            agent: "review-agent".into(),
            note: "Review the update".into(),
        },
    );
    core.handle(Input::Server(ServerMsg::Event {
        event: request.clone(),
    }))
    .unwrap();
    core.handle(Input::Server(ServerMsg::StreamItem {
        id,
        item: StreamItem::ReviewSnapshot {
            snapshot: snapshot(review_id, Seq::new(1)),
        },
    }))
    .unwrap();
    core.handle(Input::Server(ServerMsg::StreamEnd { id }))
        .unwrap();
    assert_eq!(
        core.view().requests,
        vec![nits_protocol::ReviewRequest::from_event(&request).unwrap()]
    );
    assert!(core.view().threads.is_empty());
    // A later live request is delivered normally and keeps deterministic order.
    core.handle(Input::Server(ServerMsg::Event {
        event: event(
            3,
            EventBody::ReviewRequested {
                review_id,
                agent: "other-agent".into(),
                note: "Follow-up".into(),
            },
        ),
    }))
    .unwrap();
    assert_eq!(core.view().requests.len(), 2);
}

#[test]
fn committed_request_fold_is_idempotent_scoped_and_ordered_by_identity() {
    let review_id = ReviewId::from_parts(4, 1);
    let mut snapshot = snapshot(review_id, Seq::new(1));
    let request = |seq, review_id| {
        event(
            seq,
            EventBody::ReviewRequested {
                review_id,
                agent: "review-agent".into(),
                note: "Please review".into(),
            },
        )
    };
    for event in [
        request(3, review_id),
        request(2, review_id),
        request(3, review_id),
        request(4, ReviewId::from_parts(4, 2)),
    ] {
        let sections = nits_client_core::apply_event(&mut snapshot, &event);
        assert_eq!(
            sections,
            if event.seq == Seq::new(4) {
                Vec::new()
            } else {
                vec![ViewSection::Conversation]
            }
        );
    }
    assert_eq!(
        snapshot
            .requests
            .iter()
            .map(|r| r.id.event_seq())
            .collect::<Vec<_>>(),
        vec![Seq::new(2), Seq::new(3)]
    );
    assert!(snapshot.comments.is_empty());
    assert!(snapshot.threads.is_empty());
}
