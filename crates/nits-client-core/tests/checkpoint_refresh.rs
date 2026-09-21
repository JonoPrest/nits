//! Revision checks must never attribute an obsolete content stream to a newer head.
use nits_client_core::{
    Action, CacheConfig, ClientCore, Command, Config, CoreError, Effect, Focus, IdSeed, Input,
    ScopeChoice, TransportEvent,
};
use nits_protocol::{
    Anchor, Author, BlobOid, BuildInfo, Cell, ChangeKind, ChunkIndex, ClientId, ClientMsg,
    ClientSeq, DiffScope, Event, EventBody, FileChange, FileRenderHeader, GapTable, LineNo,
    Mutation, NonEmpty, ProtocolVersion, RefSpec, RenderChunk, RenderContent, RenderOpts,
    RenderTarget, RepoId, RepoPath, Request, RequestId, RequestedTargets, ResolvedRef,
    ResolvedSource, ResolvedTarget, Response, Review, ReviewCheckpointId, ReviewId, ReviewRequest,
    ReviewRequestId, ReviewSnapshot, ReviewStatus, ReviewTarget, Row, SchemaVersion, Seq,
    ServerMsg, StreamItem, Timestamp, TreeOid, WorkspaceId,
};

fn repo() -> RepoId {
    RepoId::from_parts(2, 2)
}
fn review() -> ReviewId {
    ReviewId::from_parts(4, 1)
}
fn author() -> Author {
    Author::Human {
        name: "ada".into(),
        machine: "box".into(),
    }
}
fn targets(n: u8) -> NonEmpty<ResolvedTarget> {
    let revision = |n| ResolvedRef {
        tree: TreeOid::from_bytes([n; 20]),
        source: ResolvedSource::WorkingTree {
            head: None,
            dirty: vec![],
            branch: None,
        },
    };
    NonEmpty::singleton(ResolvedTarget {
        repo_id: repo(),
        base: revision(1),
        head: revision(n),
    })
}
fn request(effects: &[Effect], predicate: impl Fn(&Request) -> bool) -> RequestId {
    effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Send(ClientMsg::Request { id, request }) if predicate(request) => Some(*id),
            _ => None,
        })
        .unwrap()
}
fn subscribed() -> ClientCore {
    let mut core = ClientCore::new(Config {
        client_id: ClientId::from_parts(1, 1),
        client: BuildInfo {
            name: "test".into(),
            version: "0".into(),
        },
        author: author(),
        id_seed: IdSeed(7),
        cache: CacheConfig {
            max_in_flight: 16,
            ..CacheConfig::default()
        },
    });
    core.handle(Input::User(Action::Connect)).unwrap();
    core.handle(Input::Transport(TransportEvent::Connected))
        .unwrap();
    let effects = core
        .handle(Input::Server(ServerMsg::Welcome {
            protocol: ProtocolVersion::CURRENT,
            daemon: BuildInfo {
                name: "test".into(),
                version: "0".into(),
            },
            schema: SchemaVersion::CURRENT,
            upgrade: None,
        }))
        .unwrap();
    core.handle(Input::Server(ServerMsg::Response {
        id: request(&effects, |r| matches!(r, Request::Subscribe { .. })),
        response: Response::Subscribed { seq: Seq::new(1) },
    }))
    .unwrap();
    core
}
fn snapshot() -> ReviewSnapshot {
    ReviewSnapshot {
        suggestions: Vec::new(),
        review: Review {
            id: review(),
            workspace_id: WorkspaceId::from_parts(3, 3),
            title: "rounds".into(),
            targets: NonEmpty::singleton(ReviewTarget {
                repo_id: repo(),
                base: RefSpec::Head,
                head: RefSpec::WorkingTree,
            }),
            created: Timestamp::from_millis(0),
            status: ReviewStatus::Open,
        },
        resolved: Some(targets(2)),
        threads: vec![],
        comments: vec![],
        viewed: vec![],
        requests: vec![],
        checkpoints: vec![],
        seq: Seq::new(1),
    }
}
fn header(n: u8) -> FileRenderHeader {
    FileRenderHeader {
        repo_id: repo(),
        path: RepoPath::new("round.txt").unwrap(),
        target: RenderTarget::Diff {
            change: ChangeKind::Modified {
                old: nits_protocol::BlobEntry {
                    oid: BlobOid::from_bytes([1; 20]),
                    mode: nits_protocol::BlobMode::Regular,
                },
                new: nits_protocol::BlobEntry {
                    oid: BlobOid::from_bytes([n; 20]),
                    mode: nits_protocol::BlobMode::Regular,
                },
            },
        },
        opts: RenderOpts::default(),
        lang: None,
        content: RenderContent::Text {
            total_rows: 1,
            chunk_rows: 500,
            chunk_count: 1,
            highlighted: false,
            additions: 1,
            deletions: 1,
            gaps: GapTable::default(),
        },
    }
}
fn stream_content(core: &mut ClientCore, id: RequestId, n: u8) {
    let cell = |text: String| Cell {
        ending: nits_protocol::LineEnding::Lf,
        line_no: LineNo::new(1).unwrap(),
        text,
        spans: vec![],
        changed: vec![],
    };
    core.handle(Input::Server(ServerMsg::StreamItem {
        id,
        item: StreamItem::Header { header: header(n) },
    }))
    .unwrap();
    core.handle(Input::Server(ServerMsg::StreamItem {
        id,
        item: StreamItem::Chunk {
            repo_id: repo(),
            path: RepoPath::new("round.txt").unwrap(),
            chunk: RenderChunk {
                index: ChunkIndex::FIRST,
                rows: vec![Row::Modified {
                    left: cell("base".into()),
                    right: cell(format!("head {n}")),
                }],
            },
        },
    }))
    .unwrap();
    core.handle(Input::Server(ServerMsg::StreamEnd { id }))
        .unwrap();
}
fn start_open(core: &mut ClientCore) -> RequestId {
    request(
        &core
            .handle(Input::User(Action::OpenReview {
                review_id: review(),
            }))
            .unwrap(),
        |r| matches!(r, Request::OpenReview { .. }),
    )
}
fn install(core: &mut ClientCore, id: RequestId, snapshot: ReviewSnapshot) -> Vec<Effect> {
    core.handle(Input::Server(ServerMsg::StreamItem {
        id,
        item: StreamItem::ReviewSnapshot { snapshot },
    }))
    .unwrap()
}
fn advance(core: &mut ClientCore, n: u8) -> Vec<Effect> {
    core.handle(Input::Server(ServerMsg::Event {
        event: Event {
            seq: Seq::new(u64::from(n)),
            ts: Timestamp::from_millis(0),
            author: author(),
            client_id: ClientId::from_parts(9, 9),
            client_seq: ClientSeq::new(u64::from(n)),
            body: EventBody::ReviewTargetsResolved {
                review_id: review(),
                targets: targets(n),
            },
        },
    }))
    .unwrap()
}
fn files(core: &mut ClientCore, id: RequestId, n: u8) -> Vec<Effect> {
    let RenderTarget::Diff { change } = header(n).target else {
        panic!("diff")
    };
    core.handle(Input::Server(ServerMsg::Response {
        id,
        response: Response::Files {
            files: vec![FileChange {
                repo_id: repo(),
                path: RepoPath::new("round.txt").unwrap(),
                kind: change,
            }],
            resolved: targets(n).into_iter().collect(),
        },
    }))
    .unwrap()
}
fn finish_refresh(core: &mut ClientCore, id: RequestId, n: u8) {
    let effects = files(core, id, n);
    assert!(
        !core.view().check_current_ready,
        "new headers/chunks still pending"
    );
    let render = request(&effects, |r| {
        matches!(r, Request::ChangeRender { .. } | Request::FileRender { .. })
    });
    stream_content(core, render, n);
    assert!(core.view().check_current_ready);
    let effects = core.handle(Input::User(Action::CheckCurrent)).unwrap();
    assert!(effects.iter().any(|e| matches!(e, Effect::Send(ClientMsg::Request { request: Request::Mutate { mutation: Mutation::RecordCheckpoint { targets: checked, .. }, .. }, .. }) if *checked == targets(n))));
}
fn assert_refreshing(core: &mut ClientCore) {
    assert!(
        core.view().diff.is_none(),
        "old content must not appear under the new identity"
    );
    assert!(!core.view().check_current_ready);
    assert_eq!(
        core.handle(Input::User(Action::CheckCurrent)),
        Err(CoreError::CurrentChangesRefreshing)
    );
}

#[test]
fn newer_target_before_or_during_open_stream_discards_obsolete_content_and_refetches() {
    for before_snapshot in [true, false] {
        let mut core = subscribed();
        let open_id = start_open(&mut core);
        let effects = if before_snapshot {
            advance(&mut core, 3);
            install(&mut core, open_id, snapshot())
        } else {
            install(&mut core, open_id, snapshot());
            core.handle(Input::Server(ServerMsg::StreamItem {
                id: open_id,
                item: StreamItem::Header { header: header(2) },
            }))
            .unwrap();
            advance(&mut core, 3)
        };
        let files_id = request(&effects, |r| matches!(r, Request::ListFiles { .. }));
        stream_content(&mut core, open_id, 2);
        assert_eq!(
            core.view().resolved_targets,
            targets(3).into_iter().collect::<Vec<_>>()
        );
        assert_refreshing(&mut core);
        assert!(core.view().review.as_ref().unwrap().files.is_empty());
        finish_refresh(&mut core, files_id, 3);
    }
}

#[test]
fn ordinary_refresh_rejects_older_file_lists_after_new_content_is_installed() {
    let mut core = subscribed();
    let open_id = start_open(&mut core);
    install(&mut core, open_id, snapshot());
    stream_content(&mut core, open_id, 2);
    assert!(core.view().check_current_ready);
    let old = request(&advance(&mut core, 3), |r| {
        matches!(r, Request::ListFiles { .. })
    });
    assert_refreshing(&mut core);
    let new = request(&advance(&mut core, 4), |r| {
        matches!(r, Request::ListFiles { .. })
    });
    finish_refresh(&mut core, new, 4);
    files(&mut core, old, 3);
    assert_eq!(
        core.view().review.as_ref().unwrap().files[0].target,
        header(4).target
    );
    assert!(
        core.view()
            .diff
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .any(|row| matches!(&row.row, Row::Modified { right, .. } if right.text == "head 4"))
    );
}

#[test]
fn late_render_headers_cannot_reinsert_files_from_an_obsolete_target() {
    let mut core = subscribed();
    let open_id = start_open(&mut core);
    install(&mut core, open_id, snapshot());
    stream_content(&mut core, open_id, 2);
    let old_files = request(&advance(&mut core, 3), |r| {
        matches!(r, Request::ListFiles { .. })
    });
    let old_render = request(&files(&mut core, old_files, 3), |r| {
        matches!(r, Request::ChangeRender { .. } | Request::FileRender { .. })
    });
    let new_files = request(&advance(&mut core, 4), |r| {
        matches!(r, Request::ListFiles { .. })
    });
    finish_refresh(&mut core, new_files, 4);
    stream_content(&mut core, old_render, 3);
    let open = core.view().review.as_ref().unwrap();
    assert_eq!(open.files.len(), 1);
    assert_eq!(open.files[0].target, header(4).target);
    assert!(core.view().check_current_ready);
}

#[test]
fn inspecting_a_requested_scope_keeps_explicit_current_and_requested_checks_distinct() {
    let mut core = subscribed();
    let open_id = start_open(&mut core);
    let mut snapshot = snapshot();
    snapshot.resolved = Some(targets(3));
    let request_id = ReviewRequestId::from_event_seq(Seq::new(7));
    snapshot.requests.push(ReviewRequest {
        id: request_id,
        review_id: review(),
        requester: author(),
        recipient: "peer".into(),
        targets: RequestedTargets::Captured {
            targets: targets(2),
        },
        note: "H1".into(),
        created: Timestamp::from_millis(0),
    });
    install(&mut core, open_id, snapshot);
    stream_content(&mut core, open_id, 3);
    let effects = core
        .handle(Input::User(Action::SetScope {
            scope: ScopeChoice::Requested { request_id },
        }))
        .unwrap();
    let files_id = request(&effects, |r| matches!(r, Request::ListFiles { .. }));
    let effects = files(&mut core, files_id, 2);
    let render_id = request(&effects, |r| {
        matches!(r, Request::ChangeRender { .. } | Request::FileRender { .. })
    });
    stream_content(&mut core, render_id, 2);
    assert_eq!(
        core.view().resolved_targets,
        targets(2).into_iter().collect::<Vec<_>>()
    );
    let effects = core.handle(Input::User(Action::CheckCurrent)).unwrap();
    assert!(effects.iter().any(|e| matches!(e, Effect::Send(ClientMsg::Request { request: Request::Mutate { mutation: Mutation::RecordCheckpoint { targets: checked, in_reply_to: Some(nits_protocol::ReviewRound::Request { request_id: answer }), .. }, .. }, .. }) if *checked == targets(3) && *answer == request_id)));
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::ReviewRequest { index: 0 },
    }))
    .unwrap();
    let effects = core.handle(Input::User(Action::CheckRequested)).unwrap();
    assert!(effects.iter().any(|e| matches!(e, Effect::Send(ClientMsg::Request { request: Request::Mutate { mutation: Mutation::RecordCheckpoint { targets: checked, .. }, .. }, .. }) if *checked == targets(2))));
}

#[test]
fn draft_keeps_old_labels_and_content_but_cannot_check_current_until_refresh_finishes() {
    let mut core = subscribed();
    let open_id = start_open(&mut core);
    install(&mut core, open_id, snapshot());
    stream_content(&mut core, open_id, 2);
    core.handle(Input::User(Action::DraftOpened {
        anchor: Anchor::Review,
    }))
    .unwrap();
    advance(&mut core, 3);
    assert!(core.view().pending_refresh);
    assert_eq!(
        core.view().resolved_targets,
        targets(2).into_iter().collect::<Vec<_>>()
    );
    assert!(core.view().diff.is_some());
    assert_eq!(
        core.handle(Input::User(Action::CheckCurrent)),
        Err(CoreError::CurrentChangesRefreshing)
    );
    let effects = core.handle(Input::User(Action::DraftDiscarded)).unwrap();
    assert_refreshing(&mut core);
    finish_refresh(
        &mut core,
        request(&effects, |r| matches!(r, Request::ListFiles { .. })),
        3,
    );
}

#[test]
fn opening_legacy_request_resets_both_historical_scopes_to_all() {
    let mut core = subscribed();
    let open_id = start_open(&mut core);
    let mut snapshot = snapshot();
    snapshot.requests.push(ReviewRequest {
        id: ReviewRequestId::from_event_seq(Seq::new(7)),
        review_id: review(),
        requester: author(),
        recipient: "peer".into(),
        targets: RequestedTargets::Unknown,
        note: "legacy".into(),
        created: Timestamp::from_millis(0),
    });
    install(&mut core, open_id, snapshot);
    stream_content(&mut core, open_id, 2);
    for scope in [
        ScopeChoice::Requested {
            request_id: ReviewRequestId::from_event_seq(Seq::new(8)),
        },
        ScopeChoice::SinceCheckpoint {
            checkpoint_id: ReviewCheckpointId::from_event_seq(Seq::new(9)),
        },
    ] {
        core.handle(Input::User(Action::SetScope { scope }))
            .unwrap();
        core.handle(Input::User(Action::SetFocus {
            focus: Focus::ReviewRequest { index: 0 },
        }))
        .unwrap();
        let effects = core
            .handle(Input::User(Action::RunCommand {
                command: Command::Open,
            }))
            .unwrap();
        assert_eq!(core.view().scope, DiffScope::All);
        assert_eq!(core.view().tab, nits_client_core::Tab::FilesChanged);
        request(&effects, |r| {
            matches!(
                r,
                Request::ListFiles {
                    scope: DiffScope::All,
                    ..
                }
            )
        });
    }
}
