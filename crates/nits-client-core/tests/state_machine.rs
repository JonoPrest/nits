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
        suggestions: Vec::new(),
        review: review(id),
        resolved: None,
        threads: Vec::new(),
        comments: Vec::new(),
        viewed: Vec::new(),
        requests: Vec::new(),
        seq,
        checkpoints: Vec::new(),
    }
}

fn resolved(fill: u8) -> NonEmpty<ResolvedTarget> {
    let tree = TreeOid::new(Oid::from_bytes([fill; 20]));
    NonEmpty::singleton(ResolvedTarget {
        repo_id: repo_id(),
        base: ResolvedRef {
            tree,
            source: ResolvedSource::WorkingTree {
                head: None,
                dirty: Vec::new(),
                branch: None,
            },
        },
        head: ResolvedRef {
            tree,
            source: ResolvedSource::WorkingTree {
                head: None,
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
    subscribed_with(seq, config())
}

fn subscribed_with(seq: u64, config: Config) -> ClientCore {
    let mut core = ClientCore::new(config);
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
                new: nits_protocol::BlobEntry {
                    oid: nits_protocol::BlobOid::new(Oid::from_bytes([1; 20])),
                    mode: nits_protocol::BlobMode::Regular,
                },
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
            targets: nits_protocol::RequestedTargets::Unknown,
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
                targets: nits_protocol::RequestedTargets::Unknown,
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
                targets: nits_protocol::RequestedTargets::Unknown,
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

fn referenced_snapshot(id: ReviewId, anchor: Anchor) -> (ReviewSnapshot, nits_protocol::CommentId) {
    let mut snap = snapshot(id, Seq::new(1));
    let mut root = comment(id, 20);
    root.anchor = anchor;
    let mut reply = comment(id, 21);
    reply.thread_id = root.thread_id;
    reply.anchor = root.anchor.clone();
    reply.body = "Verified the fix".into();
    let reply_id = reply.id;
    snap.threads.push(nits_protocol::Thread {
        id: root.thread_id,
        review_id: id,
        root: root.id,
        replies: vec![reply.id],
        resolution: nits_protocol::ThreadResolution::Open,
    });
    snap.comments = vec![root, reply];
    (snap, reply_id)
}

fn reference_header() -> nits_protocol::FileRenderHeader {
    nits_protocol::FileRenderHeader {
        repo_id: repo_id(),
        path: nits_protocol::RepoPath::new("src/main.rs").unwrap(),
        target: nits_protocol::RenderTarget::Diff {
            change: nits_protocol::ChangeKind::Added {
                new: nits_protocol::BlobEntry {
                    oid: nits_protocol::BlobOid::new(Oid::from_bytes([7; 20])),
                    mode: nits_protocol::BlobMode::Regular,
                },
            },
        },
        opts: RenderOpts::default(),
        lang: None,
        content: nits_protocol::RenderContent::Text {
            total_rows: 1,
            chunk_rows: 100,
            chunk_count: 1,
            highlighted: false,
            additions: 1,
            deletions: 0,
            gaps: nits_protocol::GapTable::default(),
        },
    }
}

fn reference_for(
    review_id: ReviewId,
    target: nits_protocol::ReferenceTarget,
) -> nits_protocol::ReviewReference {
    nits_protocol::ReviewReference {
        context: nits_protocol::ReferenceContext::named("review-box").unwrap(),
        review_id,
        target,
    }
}

fn request_reference(
    core: &mut ClientCore,
    reference: &nits_protocol::ReviewReference,
) -> RequestId {
    core.handle(Input::User(Action::SetReferenceContext {
        context: reference.context.clone(),
    }))
    .unwrap();
    let effects = core
        .handle(Input::User(Action::OpenReference {
            reference: reference.to_string(),
        }))
        .unwrap();
    let (id, request) = sent_request(&effects).unwrap();
    assert!(
        matches!(request, Request::OpenReview { review_id, .. } | Request::ReviewSnapshot { review_id } if review_id == reference.review_id)
    );
    id
}

#[test]
fn portable_references_land_on_exact_reply_across_review_file_line_and_resolved_threads() {
    use nits_protocol::{BlobOid, LineNo, LineRange, ReferenceTarget, Side, ThreadResolution};
    let id = ReviewId::from_parts(41, 1);
    let blob = BlobOid::new(Oid::from_bytes([7; 20]));
    let anchors = [
        Anchor::Review,
        Anchor::File {
            repo_id: repo_id(),
            path: nits_protocol::RepoPath::try_from("src/main.rs".to_owned()).unwrap(),
            blob_oid: blob,
        },
        Anchor::Lines {
            repo_id: repo_id(),
            path: nits_protocol::RepoPath::try_from("src/main.rs".to_owned()).unwrap(),
            blob_oid: blob,
            side: Side::Head,
            lines: LineRange::new(LineNo::new(2).unwrap(), LineNo::new(3).unwrap()).unwrap(),
            context_hash: nits_protocol::ContextHash::new(0),
        },
    ];
    for anchor in anchors {
        for resolved in [false, true] {
            let mut core = subscribed(1);
            let (mut snap, reply_id) = referenced_snapshot(id, anchor.clone());
            if resolved {
                snap.threads[0].resolution = ThreadResolution::Resolved {
                    by: config().author,
                    at: Timestamp::from_millis(1),
                };
            }
            if !matches!(anchor, Anchor::Review) {
                snap.comments[0].state = CommentState::Outdated {
                    last_good_anchor: anchor.clone(),
                };
            }
            let reference = reference_for(
                id,
                ReferenceTarget::Comment {
                    comment_id: reply_id,
                },
            );
            let request = request_reference(&mut core, &reference);
            core.handle(Input::Server(ServerMsg::StreamItem {
                id: request,
                item: StreamItem::ReviewSnapshot { snapshot: snap },
            }))
            .unwrap();
            core.handle(Input::Server(ServerMsg::StreamItem {
                id: request,
                item: StreamItem::Header {
                    header: reference_header(),
                },
            }))
            .unwrap();
            core.handle(Input::Server(ServerMsg::StreamEnd { id: request }))
                .unwrap();
            assert!(core.view().diff.is_some());
            assert_eq!(core.view().tab, nits_client_core::Tab::Conversation);
            assert_eq!(
                core.view().focus,
                nits_client_core::Focus::Thread { index: 0 }
            );
            assert_eq!(core.view().focused_comment, Some(reply_id));
            assert_eq!(core.view().copy_reference, Some(reference.clone()));
            assert_eq!(
                core.handle(Input::User(Action::CopyReference { reference }))
                    .unwrap(),
                Vec::new()
            );
            let effects = core.handle(Input::Key("y".parse().unwrap())).unwrap();
            assert_eq!(
                core.view().last_key.unwrap().command,
                Some(nits_client_core::Command::CopyReference)
            );
            assert!(effects.iter().all(|e| matches!(e, Effect::Render(_))));
            core.handle(Input::Key("[".parse().unwrap())).unwrap();
            assert_eq!(
                core.view().focused_comment,
                Some(core.view().threads[0].root)
            );
            core.handle(Input::Key("]".parse().unwrap())).unwrap();
            assert_eq!(core.view().focused_comment, Some(reply_id));
        }
    }
}

#[test]
fn reference_open_ignores_superseded_snapshots_streams_and_errors() {
    use nits_protocol::ReferenceTarget;
    let mut core = subscribed(1);
    let a = ReviewId::from_parts(42, 1);
    let b = ReviewId::from_parts(42, 2);
    let (snap, reply_id) = referenced_snapshot(b, Anchor::Review);
    let old = request_reference(&mut core, &reference_for(a, ReferenceTarget::Review));
    let current = request_reference(
        &mut core,
        &reference_for(
            b,
            ReferenceTarget::Comment {
                comment_id: reply_id,
            },
        ),
    );
    core.handle(Input::Server(ServerMsg::StreamItem {
        id: current,
        item: StreamItem::ReviewSnapshot { snapshot: snap },
    }))
    .unwrap();
    assert!(
        core.handle(Input::Server(ServerMsg::StreamItem {
            id: old,
            item: StreamItem::ReviewSnapshot {
                snapshot: snapshot(a, Seq::new(1))
            }
        }))
        .unwrap()
        .is_empty()
    );
    assert!(
        core.handle(Input::Server(ServerMsg::Error {
            id: old,
            error: RpcError::Cancelled
        }))
        .unwrap()
        .is_empty()
    );
    assert_eq!(core.view().open_review, Some(b));
    assert_eq!(core.view().focused_comment, Some(reply_id));
    assert_eq!(core.view().last_error, None);
}

#[test]
fn reference_reply_event_ahead_of_snapshot_is_not_lost() {
    let mut core = subscribed(1);
    let id = ReviewId::from_parts(43, 1);
    let (mut snap, reply_id) = referenced_snapshot(id, Anchor::Review);
    let reply = snap.comments.pop().unwrap();
    snap.threads[0].replies.clear();
    let req = request_reference(
        &mut core,
        &reference_for(
            id,
            nits_protocol::ReferenceTarget::Comment {
                comment_id: reply_id,
            },
        ),
    );
    core.handle(Input::Server(ServerMsg::Event {
        event: event(2, EventBody::CommentCreated { comment: reply }),
    }))
    .unwrap();
    core.handle(Input::Server(ServerMsg::StreamItem {
        id: req,
        item: StreamItem::ReviewSnapshot { snapshot: snap },
    }))
    .unwrap();
    assert_eq!(core.view().focused_comment, Some(reply_id));
    assert_eq!(core.view().last_error, None);
}

#[test]
fn reference_missing_deleted_wrong_review_and_context_are_explicit() {
    use nits_protocol::{ReferenceError, ReferenceTarget};
    let id = ReviewId::from_parts(44, 1);
    let (mut snap, reply_id) = referenced_snapshot(id, Anchor::Review);
    let reference = reference_for(
        id,
        ReferenceTarget::Comment {
            comment_id: reply_id,
        },
    );
    let mut wrong = reference.clone();
    wrong.review_id = ReviewId::from_parts(44, 2);
    assert_eq!(wrong.resolve(&snap), Err(ReferenceError::WrongReview));
    snap.comments[1].state = CommentState::Deleted;
    assert_eq!(reference.resolve(&snap), Err(ReferenceError::Deleted));
    snap.comments.pop();
    assert_eq!(reference.resolve(&snap), Err(ReferenceError::Missing));
    let mut core = subscribed(1);
    core.handle(Input::User(Action::OpenReference {
        reference: "<script>".into(),
    }))
    .unwrap();
    assert!(matches!(
        core.view().last_error,
        Some(RpcError::Invalid { .. })
    ));
    core.handle(Input::User(Action::OpenReference {
        reference: reference.to_string(),
    }))
    .unwrap();
    assert!(
        matches!(&core.view().last_error, Some(RpcError::Invalid { reason }) if reason.contains("another daemon context"))
    );
    let req = request_reference(&mut core, &reference);
    core.handle(Input::Server(ServerMsg::StreamItem {
        id: req,
        item: StreamItem::ReviewSnapshot { snapshot: snap },
    }))
    .unwrap();
    assert!(
        matches!(&core.view().last_error, Some(RpcError::Invalid { reason }) if reason.contains("missing"))
    );
    assert_eq!(core.view().focused_comment, None);
}

#[test]
fn reference_piecewise_snapshot_and_informational_thread_land_on_root() {
    let mut cfg = config();
    cfg.cache.disk = nits_client_core::DiskTier::Enabled {
        budget: nits_client_core::Bytes::mib(1),
    };
    let mut core = subscribed_with(1, cfg);
    let id = ReviewId::from_parts(45, 1);
    let (mut snap, _) = referenced_snapshot(id, Anchor::Review);
    snap.comments[0].kind = CommentKind::Informational;
    snap.threads[0].resolution = nits_protocol::ThreadResolution::Informational;
    let root = snap.threads[0].root;
    let reference = reference_for(
        id,
        nits_protocol::ReferenceTarget::Thread {
            thread_id: snap.threads[0].id,
        },
    );
    let old = request_reference(
        &mut core,
        &reference_for(id, nits_protocol::ReferenceTarget::Review),
    );
    let current = request_reference(&mut core, &reference);
    let effects = core
        .handle(Input::Server(ServerMsg::Response {
            id: current,
            response: Response::ReviewSnapshot {
                snapshot: snap.clone(),
            },
        }))
        .unwrap();
    let files_request = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Send(ClientMsg::Request {
                id,
                request: Request::ListFiles { .. },
            }) => Some(*id),
            _ => None,
        })
        .unwrap();
    let header = reference_header();
    let nits_protocol::RenderTarget::Diff { change } = header.target else {
        panic!("diff header")
    };
    core.handle(Input::Server(ServerMsg::Response {
        id: files_request,
        response: Response::Files {
            resolved: Vec::new(),
            files: vec![nits_protocol::FileChange {
                repo_id: header.repo_id,
                path: header.path,
                kind: change,
            }],
        },
    }))
    .unwrap();
    assert!(
        core.handle(Input::Server(ServerMsg::Response {
            id: old,
            response: Response::ReviewSnapshot {
                snapshot: snap.clone()
            }
        }))
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        core.handle(Input::Server(ServerMsg::Response {
            id: old,
            response: Response::ReviewSnapshot { snapshot: snap }
        })),
        Err(CoreError::UnknownRequest(old))
    );
    assert_eq!(core.view().focused_comment, Some(root));
    assert_eq!(
        core.view().focus,
        nits_client_core::Focus::Thread { index: 0 }
    );
    assert_eq!(
        core.view().threads[0].status,
        nits_client_core::ThreadStatus::Informational
    );
    assert_eq!(core.view().tab, nits_client_core::Tab::Conversation);
}

#[test]
#[allow(clippy::too_many_lines)] // Snapshot delivery, refresh deferral and checkpoint recording share one race.
fn requested_checkpoint_and_new_target_delivered_ahead_of_snapshot_are_preserved() {
    use nits_protocol::{
        CheckpointFreshness, NonEmpty, Oid, RequestedTargets, ResolvedRef, ResolvedSource,
        ResolvedTarget, ReviewRound, ReviewerIdentity, TreeOid,
    };
    let mut core = subscribed(1);
    let review_id = ReviewId::from_parts(4, 1);
    let mut old = snapshot(review_id, Seq::new(1));
    let reference = |n| ResolvedRef {
        tree: TreeOid::new(Oid::from_bytes([n; 20])),
        source: ResolvedSource::WorkingTree {
            head: None,
            dirty: Vec::new(),
            branch: None,
        },
    };
    let h1 = NonEmpty::singleton(ResolvedTarget {
        repo_id: old.review.targets.first().repo_id,
        base: reference(1),
        head: reference(2),
    });
    let mut h2 = h1.clone();
    h2.iter_mut().next().unwrap().head = reference(3);
    old.resolved = Some(h1.clone());
    let effects = core
        .handle(Input::User(Action::OpenReview { review_id }))
        .unwrap();
    let (id, _) = sent_request(&effects).unwrap();
    let request = event(
        2,
        EventBody::ReviewRequested {
            review_id,
            agent: "review-agent".into(),
            note: "H1".into(),
            targets: RequestedTargets::Captured {
                targets: h1.clone(),
            },
        },
    );
    let request_id = nits_protocol::ReviewRequestId::from_event_seq(request.seq);
    let checked = event(
        4,
        EventBody::ReviewChecked {
            review_id,
            reviewer: ReviewerIdentity::Human {
                name: "ada".into(),
                machine: "box".into(),
            },
            targets: h1.clone(),
            in_reply_to: Some(ReviewRound::Request { request_id }),
        },
    );
    for e in [
        request,
        event(
            3,
            EventBody::ReviewTargetsResolved {
                review_id,
                targets: h2,
            },
        ),
        checked,
    ] {
        core.handle(Input::Server(ServerMsg::Event { event: e }))
            .unwrap();
    }
    core.handle(Input::Server(ServerMsg::StreamItem {
        id,
        item: StreamItem::ReviewSnapshot { snapshot: old },
    }))
    .unwrap();
    assert_eq!(core.view().requests.len(), 1);
    assert_eq!(core.view().checkpoints.len(), 1);
    assert_eq!(core.view().checkpoints[0].checkpoint.targets, h1);
    assert_eq!(
        core.view().checkpoints[0].freshness,
        CheckpointFreshness::Changed
    );
    // Holding rendered targets for a draft must not hide authoritative freshness.
    core.handle(Input::User(Action::DraftOpened {
        anchor: Anchor::Review,
    }))
    .unwrap();
    core.handle(Input::Server(ServerMsg::Event {
        event: event(
            5,
            EventBody::ReviewTargetsResolved {
                review_id,
                targets: h1.clone(),
            },
        ),
    }))
    .unwrap();
    assert!(core.view().pending_refresh);
    assert_eq!(
        core.view().checkpoints[0].freshness,
        CheckpointFreshness::Current
    );
    core.handle(Input::User(Action::DraftDiscarded)).unwrap();
    core.handle(Input::User(Action::SetFocus {
        focus: nits_client_core::Focus::ReviewRequest { index: 0 },
    }))
    .unwrap();
    let effects = core.handle(Input::User(Action::CheckRequested)).unwrap();
    let (_, request) = sent_request(&effects).unwrap();
    assert!(
        matches!(request, Request::Mutate { mutation: Mutation::RecordCheckpoint { targets, in_reply_to: Some(ReviewRound::Request { request_id: checked_request }), .. }, .. } if targets == h1 && checked_request == request_id)
    );
}

#[test]
#[allow(clippy::too_many_lines)] // Both snapshot transports exercise the complete linked-reply/revision race.
fn linked_browse_reply_survives_newer_request_target_and_checkpoint_before_snapshot() {
    use nits_protocol::{
        CommentContext, ReferenceTarget, RequestedTargets, ReviewRound, ReviewerIdentity,
    };
    for piecewise in [false, true] {
        let mut cfg = config();
        if piecewise {
            cfg.cache.disk = nits_client_core::DiskTier::Enabled {
                budget: nits_client_core::Bytes::mib(1),
            };
        }
        let mut core = subscribed_with(1, cfg);
        let id = ReviewId::from_parts(66, 64);
        let (mut snap, reply_id) = referenced_snapshot(
            id,
            Anchor::File {
                repo_id: repo_id(),
                path: nits_protocol::RepoPath::new("src/main.rs").unwrap(),
                blob_oid: nits_protocol::BlobOid::from_bytes([7; 20]),
            },
        );
        for comment in &mut snap.comments {
            comment.context = Some(CommentContext::Browse {
                reference: RefSpec::Tag {
                    name: "retained".into(),
                },
            });
        }
        let reply = snap.comments.pop().unwrap();
        snap.threads[0].replies.clear();
        let thread_id = snap.threads[0].id;
        let h1 = resolved(1);
        let h2 = resolved(2);
        snap.resolved = Some(h1.clone());
        let reference = reference_for(
            id,
            ReferenceTarget::Comment {
                comment_id: reply_id,
            },
        );
        let req = request_reference(&mut core, &reference);
        let request_id = nits_protocol::ReviewRequestId::from_event_seq(Seq::new(4));
        let events = [
            event(2, EventBody::CommentCreated { comment: reply }),
            event(
                3,
                EventBody::ThreadDeferred {
                    review_id: id,
                    thread_id,
                    reason: "Agreed external follow-up".parse().unwrap(),
                    tracking_url: None,
                },
            ),
            event(
                4,
                EventBody::ReviewRequested {
                    review_id: id,
                    agent: "review-agent".into(),
                    note: "Check H1".into(),
                    targets: RequestedTargets::Captured {
                        targets: h1.clone(),
                    },
                },
            ),
            event(
                5,
                EventBody::ReviewTargetsResolved {
                    review_id: id,
                    targets: h2.clone(),
                },
            ),
            event(
                6,
                EventBody::ReviewChecked {
                    review_id: id,
                    reviewer: ReviewerIdentity::Agent {
                        name: "review-agent".into(),
                    },
                    targets: h1,
                    in_reply_to: Some(ReviewRound::Request { request_id }),
                },
            ),
        ];
        for event in events {
            core.handle(Input::Server(ServerMsg::Event { event }))
                .unwrap();
        }
        let effects = core
            .handle(Input::Server(if piecewise {
                ServerMsg::Response {
                    id: req,
                    response: Response::ReviewSnapshot { snapshot: snap },
                }
            } else {
                ServerMsg::StreamItem {
                    id: req,
                    item: StreamItem::ReviewSnapshot { snapshot: snap },
                }
            }))
            .unwrap();
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::Send(ClientMsg::Request {
                request: Request::ListFiles { .. },
                ..
            })
        )));
        if !piecewise {
            core.handle(Input::Server(ServerMsg::StreamItem {
                id: req,
                item: StreamItem::Header {
                    header: reference_header(),
                },
            }))
            .unwrap();
            core.handle(Input::Server(ServerMsg::StreamEnd { id: req }))
                .unwrap();
        }
        let view = core.view();
        assert_eq!(view.tab, nits_client_core::Tab::Conversation);
        assert_eq!(view.focused_comment, Some(reply_id));
        assert_eq!(view.copy_reference, Some(reference));
        assert_eq!(view.threads[0].comments[1].body, "Verified the fix");
        assert!(matches!(
            view.threads[0].status,
            nits_protocol::ThreadResolution::Deferred { .. }
        ));
        assert!(matches!(
            view.threads[0].context,
            Some(CommentContext::Browse { .. })
        ));
        assert_eq!(view.requests[0].id, request_id);
        assert_eq!(
            view.checkpoints[0].freshness,
            nits_protocol::CheckpointFreshness::Changed
        );
        assert_eq!(view.review.as_ref().unwrap().snapshot.resolved, Some(h2));
        assert!(view.review.as_ref().unwrap().files.is_empty());
        assert!(!view.check_current_ready);
        assert!(matches!(
            core.handle(Input::User(Action::CheckCurrent)),
            Err(CoreError::CurrentChangesRefreshing)
        ));
    }
}

#[test]
fn provenance_refresh_requests_new_commits_and_ignores_out_of_order_old_list() {
    let mut core = subscribed(1);
    let review_id = ReviewId::from_parts(4, 1);
    open(&mut core, review_id);
    let mut requests = Vec::new();
    for n in [2u8, 3] {
        let mut target = resolved(7).first().clone();
        target.head.source = ResolvedSource::WorkingTree {
            dirty: vec![],
            branch: Some("feature".into()),
            head: Some(nits_protocol::CommitOid::from_bytes([n; 20])),
        };
        let effects = core
            .handle(Input::Server(ServerMsg::Event {
                event: event(
                    u64::from(n),
                    EventBody::ReviewTargetsResolved {
                        review_id,
                        targets: NonEmpty::singleton(target),
                    },
                ),
            }))
            .unwrap();
        let commits: Vec<_> = effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Send(ClientMsg::Request {
                    id,
                    request:
                        Request::ListCommits {
                            review_id: got_review,
                            repo_id: got_repo,
                        },
                }) => {
                    assert_eq!(*got_review, review_id);
                    assert_eq!(*got_repo, repo_id());
                    Some(*id)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            commits.len(),
            1,
            "one commit refresh for each provenance event"
        );
        requests.push(commits[0]);
    }
    let sig = nits_protocol::Sig {
        name: "ada".into(),
        email: "ada@example.com".into(),
        time: Timestamp::from_millis(0),
        offset_minutes: 0,
    };
    let commit = nits_protocol::CommitInfo {
        oid: nits_protocol::CommitOid::from_bytes([3; 20]),
        parents: vec![],
        tree: TreeOid::from_bytes([7; 20]),
        author: sig.clone(),
        committer: sig,
        subject: "amended".into(),
        body: String::new(),
    };
    let effects = core
        .handle(Input::Server(ServerMsg::Response {
            id: requests[1],
            response: Response::Commits {
                commits: vec![commit.clone()],
            },
        }))
        .unwrap();
    assert_eq!(rendered(&effects), vec![ViewSection::CommitStepper]);
    assert_eq!(
        core.view().stepper.as_ref().unwrap().commits[0].oid,
        commit.oid
    );
    let before = core.view().clone();
    let effects = core
        .handle(Input::Server(ServerMsg::Response {
            id: requests[0],
            response: Response::Commits { commits: vec![] },
        }))
        .unwrap();
    assert!(effects.is_empty());
    assert_eq!(core.view(), &before);
}

#[test]
fn pending_repository_selection_survives_provenance_refresh() {
    let mut core = subscribed(1);
    let rid = ReviewId::from_parts(4, 1);
    let second = RepoId::from_parts(2, 3);
    let effects = core
        .handle(Input::User(Action::OpenReview { review_id: rid }))
        .unwrap();
    let (id, _) = sent_request(&effects).unwrap();
    let mut snap = snapshot(rid, Seq::new(1));
    let mut target_b = snap.review.targets.first().clone();
    target_b.repo_id = second;
    snap.review.targets =
        NonEmpty::new(vec![snap.review.targets.first().clone(), target_b]).unwrap();
    let effects = core
        .handle(Input::Server(ServerMsg::StreamItem {
            id,
            item: StreamItem::ReviewSnapshot { snapshot: snap },
        }))
        .unwrap();
    let initial = effects
        .iter()
        .find_map(|e| match e {
            Effect::Send(ClientMsg::Request {
                id,
                request: Request::ListCommits { .. },
            }) => Some(*id),
            _ => None,
        })
        .unwrap();
    core.handle(Input::Server(ServerMsg::Response {
        id: initial,
        response: Response::Commits { commits: vec![] },
    }))
    .unwrap();
    assert_eq!(core.view().stepper.as_ref().unwrap().repo_id, repo_id());
    let effects = core
        .handle(Input::User(Action::ListCommits { repo_id: second }))
        .unwrap();
    let (pending, _) = sent_request(&effects).unwrap();
    let a = resolved(7).first().clone();
    let mut b = a.clone();
    b.repo_id = second;
    let effects = core
        .handle(Input::Server(ServerMsg::Event {
            event: event(
                2,
                EventBody::ReviewTargetsResolved {
                    review_id: rid,
                    targets: NonEmpty::new(vec![a, b]).unwrap(),
                },
            ),
        }))
        .unwrap();
    let (refresh, refreshed_repo) = effects
        .iter()
        .find_map(|e| match e {
            Effect::Send(ClientMsg::Request {
                id,
                request: Request::ListCommits { repo_id, .. },
            }) => Some((*id, *repo_id)),
            _ => None,
        })
        .unwrap();
    core.handle(Input::Server(ServerMsg::Response {
        id: pending,
        response: Response::Commits { commits: vec![] },
    }))
    .unwrap();
    core.handle(Input::Server(ServerMsg::Response {
        id: refresh,
        response: Response::Commits { commits: vec![] },
    }))
    .unwrap();
    assert_eq!(refreshed_repo, second);
    assert_eq!(
        core.view().stepper.as_ref().unwrap().repo_id,
        second,
        "automatic refresh must preserve the user's pending repository selection"
    );
}

/// Home rows are tested through the public sans-I/O boundary, including
/// workspaces that have no review and two repositories with the same name.
fn home_workspaces(core: &mut ClientCore, workspaces: Vec<nits_protocol::Workspace>) {
    let effects = core.handle(Input::User(Action::ListWorkspaces)).unwrap();
    let (id, _) = sent_request(&effects).unwrap();
    core.handle(Input::Server(ServerMsg::Response {
        id,
        response: Response::Workspaces { workspaces },
    }))
    .unwrap();
}

fn home_key(core: &mut ClientCore, sequence: &str) -> Vec<Effect> {
    let sequence: nits_client_core::KeySeq = sequence.parse().unwrap();
    sequence
        .chords()
        .iter()
        .flat_map(|chord| core.handle(Input::Key(*chord)).unwrap())
        .collect()
}

#[test]
#[allow(clippy::too_many_lines)] // One home session covers navigation through membership changes.
fn home_inventory_navigation_copies_daemon_checkout_and_preserves_row_identity() {
    use nits_client_core::{Command, Focus, HomeRowKind};
    let mut core = subscribed(0);
    let product = nits_protocol::Workspace {
        id: WorkspaceId::from_parts(10, 1),
        name: "Product".into(),
        repos: vec![
            nits_protocol::Repo {
                id: RepoId::from_parts(11, 1),
                display_name: "Atlas".into(),
                path: "/srv/one/atlas".into(),
            },
            nits_protocol::Repo {
                id: RepoId::from_parts(11, 2),
                display_name: "Atlas".into(),
                path: "/srv/two/atlas".into(),
            },
        ],
    };
    let empty = nits_protocol::Workspace {
        id: WorkspaceId::from_parts(10, 2),
        name: "Empty".into(),
        repos: vec![],
    };
    home_workspaces(&mut core, vec![product.clone(), empty.clone()]);
    assert_eq!(core.view().home.rows.len(), 2);
    assert_eq!(core.view().home.selected_workspace, Some(product.id));
    assert_eq!(core.view().copy_checkout, None);
    let effects = home_key(&mut core, "enter");
    assert_eq!(
        effects,
        vec![Effect::Render(ViewDelta::new(&[
            ViewSection::ReviewList,
            ViewSection::Focus,
            ViewSection::Hints,
        ]))]
    );
    assert_eq!(core.view().home.rows.len(), 4);
    home_key(&mut core, "j");
    assert_eq!(core.view().copy_checkout.as_deref(), Some("/srv/one/atlas"));
    assert_eq!(
        core.view().copy_target,
        None,
        "absolute paths never become RepoPath"
    );
    assert_eq!(
        nits_client_core::resolve_command(&core, Command::CopyCheckout),
        Ok(Action::CopyCheckout {
            repo_id: product.repos[0].id
        })
    );
    assert!(
        home_key(&mut core, "y")
            .iter()
            .all(|effect| matches!(effect, Effect::Render(_)))
    );
    home_key(&mut core, "j");
    assert_eq!(core.view().copy_checkout.as_deref(), Some("/srv/two/atlas"));
    let config = nits_client_core::KeysConfig {
        leader: None,
        bindings: [(
            nits_client_core::Mode::Normal,
            [("copy_checkout".into(), vec!["g y".into()])].into(),
        )]
        .into(),
        groups: std::collections::BTreeMap::default(),
    };
    core.handle(Input::Stored {
        key: nits_client_core::Keymap::KEY.into(),
        value: Some(serde_json::to_vec(&config).unwrap()),
    })
    .unwrap();
    assert!(
        core.view()
            .chrome
            .iter()
            .any(|hint| hint.command == Command::CopyCheckout && hint.keys == "g y")
    );
    assert!(
        core.handle(Input::Key(nits_client_core::KeyChord::char('y')))
            .is_err()
    );
    home_key(&mut core, "g y");
    assert_eq!(
        core.view().last_key.and_then(|key| key.command),
        Some(Command::CopyCheckout)
    );
    let row = core.view().home.rows[2];
    home_workspaces(&mut core, vec![empty.clone(), product.clone()]);
    let Focus::ReviewList { index } = core.view().focus else {
        panic!("home focus")
    };
    assert_eq!(
        core.view().home.rows[index],
        row,
        "refresh reorders without changing the selected checkout"
    );
    assert_eq!(
        row.kind,
        HomeRowKind::Repository {
            repo_id: product.repos[1].id
        }
    );
    core.handle(Input::User(Action::ToggleWorkspace {
        workspace_id: product.id,
    }))
    .unwrap();
    assert_eq!(core.view().copy_checkout, None);
    assert_eq!(core.view().home.selected_workspace, Some(product.id));
    home_key(&mut core, "g g");
    assert_eq!(core.view().home.selected_workspace, Some(empty.id));
    home_key(&mut core, "enter");
    assert_eq!(
        core.view().home.rows.len(),
        2,
        "empty inventory has no phantom repository"
    );
    home_key(&mut core, "N");
    assert_eq!(
        core.view().home.creating.as_ref().map(|c| c.workspace_id),
        Some(empty.id)
    );
    home_key(&mut core, "esc");
    assert_eq!(core.view().home.creating, None);
}

#[test]
#[allow(clippy::too_many_lines)] // One session crosses review, sidebar and asynchronous home navigation.
fn review_subset_and_daemon_identity_survive_sidebar_toggle_and_return_home() {
    use nits_client_core::{DaemonContext, HomeRowKind};
    let mut core = subscribed(0);
    let workspace_id = review(ReviewId::from_parts(4, 1)).workspace_id;
    let other = nits_protocol::Workspace {
        id: WorkspaceId::from_parts(8, 1),
        name: "Other".into(),
        repos: vec![],
    };
    let product = nits_protocol::Workspace {
        id: workspace_id,
        name: "Product".into(),
        repos: vec![
            nits_protocol::Repo {
                id: repo_id(),
                display_name: "Atlas".into(),
                path: "/srv/atlas".into(),
            },
            nits_protocol::Repo {
                id: RepoId::from_parts(2, 3),
                display_name: "Beacon".into(),
                path: "/srv/beacon".into(),
            },
        ],
    };
    home_workspaces(&mut core, vec![other, product.clone()]);
    core.handle(Input::User(Action::SetReferenceContext {
        context: nits_protocol::ReferenceContext::named("remote-build").unwrap(),
    }))
    .unwrap();
    let id = ReviewId::from_parts(4, 1);
    core.handle(Input::Server(ServerMsg::Event {
        event: event(1, EventBody::ReviewCreated { review: review(id) }),
    }))
    .unwrap();
    let index = core
        .view()
        .home
        .rows
        .iter()
        .position(|row| row.kind == HomeRowKind::Review { review_id: id })
        .unwrap();
    core.handle(Input::User(Action::SetFocus {
        focus: nits_client_core::Focus::ReviewList { index },
    }))
    .unwrap();
    assert_eq!(core.view().home.selected_workspace, Some(workspace_id));
    open(&mut core, id);
    assert_eq!(
        core.view().active_repo,
        Some(repo_id()),
        "single repo remains identifiable with no changed files"
    );
    core.handle(Input::User(Action::ToggleSidebar)).unwrap();
    assert!(core.view().prefs.sidebar_hidden);
    assert_eq!(core.view().home.selected_workspace, Some(workspace_id));
    assert_eq!(
        core.view().daemon_context,
        Some(DaemonContext::Named {
            name: "remote-build".into()
        })
    );
    home_key(&mut core, "g W");
    assert_eq!(core.view().open_review, None);
    assert_eq!(core.view().home.selected_workspace, Some(workspace_id));
    assert_eq!(core.view().workspaces[1].repos.len(), 2);
    assert_eq!(core.view().reviews[0].targets.len(), 1);
    let effects = core
        .handle(Input::User(Action::OpenReview { review_id: id }))
        .unwrap();
    let (request, _) = sent_request(&effects).unwrap();
    core.handle(Input::User(Action::GoHome)).unwrap();
    core.handle(Input::Server(ServerMsg::StreamItem {
        id: request,
        item: StreamItem::ReviewSnapshot {
            snapshot: snapshot(id, Seq::new(2)),
        },
    }))
    .unwrap();
    assert_eq!(
        core.view().open_review,
        None,
        "returning home cancels an unfinished review navigation"
    );
}

#[test]
#[allow(clippy::too_many_lines)] // One stream tests typed identity, progress, and metadata-only actions.
fn submodule_metadata_counts_as_a_file_and_tracks_typed_viewed_identity() {
    use nits_client_core::ViewedState;
    use nits_protocol::{
        BlobOid, ChangeKind, CommitOid, RenderContent, RenderTarget, SubmoduleChange,
        ViewedContent, ViewedMark,
    };
    let review_id = ReviewId::from_parts(4, 4);
    let pointer = CommitOid::from_bytes([8; 20]);
    for (marked, expected) in [
        (
            ViewedContent::Submodule { commit: pointer },
            ViewedState::Viewed,
        ),
        (
            ViewedContent::Submodule {
                commit: CommitOid::from_bytes([7; 20]),
            },
            ViewedState::ChangedSinceViewed,
        ),
        (
            ViewedContent::Blob {
                entry: nits_protocol::BlobEntry {
                    oid: BlobOid::from_bytes([8; 20]),
                    mode: nits_protocol::BlobMode::Regular,
                },
            },
            ViewedState::ChangedSinceViewed,
        ),
    ] {
        let mut core = subscribed(1);
        let effects = core
            .handle(Input::User(Action::OpenReview { review_id }))
            .unwrap();
        let (request, _) = sent_request(&effects).unwrap();
        let header = nits_protocol::FileRenderHeader {
            target: RenderTarget::Diff {
                change: ChangeKind::Submodule {
                    change: SubmoduleChange::Updated {
                        old: CommitOid::from_bytes([6; 20]),
                        new: pointer,
                    },
                },
            },
            content: RenderContent::Submodule,
            ..reference_header()
        };
        let file = FileRef {
            repo_id: header.repo_id,
            path: header.path.clone(),
        };
        let mut snap = snapshot(review_id, Seq::new(1));
        snap.resolved = Some(resolved(1));
        snap.viewed.push(ViewedMark {
            review_id,
            repo_id: header.repo_id,
            path: header.path.clone(),
            viewer: nits_protocol::Human {
                name: "someone".into(),
                machine: "host".into(),
            },
            content: marked,
        });
        for item in [
            StreamItem::ReviewSnapshot { snapshot: snap },
            StreamItem::Header { header },
        ] {
            core.handle(Input::Server(ServerMsg::StreamItem { id: request, item }))
                .unwrap();
        }
        core.handle(Input::Server(ServerMsg::StreamEnd { id: request }))
            .unwrap();
        assert_eq!(core.view().progress.total, 1);
        assert_eq!(core.view().diffs.len(), 1);
        assert_eq!(core.view().diffs[0].viewed, expected);
        assert!(core.view().diffs[0].rows.is_empty());
        for action in [
            Action::CommentFile { file: file.clone() },
            Action::CommentLines {
                file: file.clone(),
                side: nits_protocol::Side::Head,
                start_line: 1,
                end_line: 1,
            },
        ] {
            assert!(core.handle(Input::User(action)).is_err());
            assert!(core.view().draft.is_none());
        }
        core.handle(Input::User(Action::Viewport {
            file: file.clone(),
            first_row: 0,
            last_row: 10,
        }))
        .unwrap();
        assert!(core.handle(Input::User(Action::EnterVisual)).is_err());
        assert_eq!(
            core.handle(Input::User(Action::ExpandContext {
                file: file.clone(),
                full: true
            }))
            .unwrap(),
            Vec::new()
        );
        core.handle(Input::User(Action::MarkViewed { file }))
            .unwrap();
        assert_eq!(core.view().diffs[0].viewed, ViewedState::Viewed);
        assert_eq!(core.view().progress.viewed, 1);
    }
}

#[test]
#[allow(clippy::too_many_lines)] // One typed-identity matrix follows the actual diff and Browse request flows.
fn blob_mode_identity_controls_viewed_progress_for_diffs_and_browse() {
    use nits_client_core::ViewedState;
    use nits_protocol::{BlobEntry, BlobMode, RenderTarget, ViewedContent, ViewedMark};
    let review_id = ReviewId::from_parts(4, 4);
    let oid = nits_protocol::BlobOid::from_bytes([8; 20]);
    for mode in [
        BlobMode::Regular,
        BlobMode::Executable,
        BlobMode::Symlink,
        BlobMode::Unknown,
    ] {
        let current = BlobEntry {
            oid,
            mode: BlobMode::Executable,
        };
        let marked = BlobEntry { oid, mode };
        let mut core = subscribed(1);
        let effects = core
            .handle(Input::User(Action::OpenReview { review_id }))
            .unwrap();
        let (request, _) = sent_request(&effects).unwrap();
        let mut header = reference_header();
        header.target = RenderTarget::Diff {
            change: nits_protocol::ChangeKind::Modified {
                old: BlobEntry {
                    mode: BlobMode::Regular,
                    ..current
                },
                new: current,
            },
        };
        let file = FileRef {
            repo_id: header.repo_id,
            path: header.path.clone(),
        };
        let mut snap = snapshot(review_id, Seq::new(1));
        let targets = resolved(1);
        let tree = nits_protocol::TreeSnapshot {
            repo_id: file.repo_id,
            root_oid: targets.first().head.tree,
            entries: vec![nits_protocol::TreeEntry {
                path: file.path.clone(),
                kind: nits_protocol::TreeEntryKind::File {
                    oid,
                    size: 0,
                    executable: true,
                },
            }],
        };
        snap.resolved = Some(targets);
        snap.viewed.push(ViewedMark {
            review_id,
            repo_id: file.repo_id,
            path: file.path.clone(),
            viewer: nits_protocol::Human {
                name: "someone".into(),
                machine: "host".into(),
            },
            content: ViewedContent::Blob { entry: marked },
        });
        for item in [
            StreamItem::ReviewSnapshot { snapshot: snap },
            StreamItem::TreeSnapshot { snapshot: tree },
            StreamItem::Header {
                header: header.clone(),
            },
        ] {
            core.handle(Input::Server(ServerMsg::StreamItem { id: request, item }))
                .unwrap();
        }
        core.handle(Input::Server(ServerMsg::StreamEnd { id: request }))
            .unwrap();
        let expected = if mode == BlobMode::Executable {
            ViewedState::Viewed
        } else {
            ViewedState::ChangedSinceViewed
        };
        assert_eq!(core.view().diffs[0].viewed, expected);
        assert_eq!(
            core.view().progress.viewed,
            u32::from(mode == BlobMode::Executable)
        );
        let mut effects = core
            .handle(Input::User(Action::SetTab {
                tab: nits_client_core::Tab::Browse,
            }))
            .unwrap();
        effects.extend(
            core.handle(Input::User(Action::Viewport {
                file,
                first_row: 0,
                last_row: 10,
            }))
            .unwrap(),
        );
        let (request, message) = sent_request(&effects).unwrap();
        assert!(matches!(message, Request::BlobRender { entry, .. } if entry == current));
        header.target = RenderTarget::Blob { entry: current };
        core.handle(Input::Server(ServerMsg::StreamItem {
            id: request,
            item: StreamItem::Header { header },
        }))
        .unwrap();
        core.handle(Input::Server(ServerMsg::StreamEnd { id: request }))
            .unwrap();
        assert_eq!(core.view().diff.as_ref().unwrap().viewed, expected);
    }
}
#[path = "creation/mod.rs"]
mod creation;

#[path = "suggestion/mod.rs"]
mod suggestion_flow;
