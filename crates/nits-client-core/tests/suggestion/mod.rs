//! Read/apply ordering and lost responses must never replay a filesystem write.
use super::*;
use nits_client_core::{Command, SuggestionStatus, SuggestionView};
use nits_protocol::{
    BlobOid, CommentId, LineEnding, LineNo, RepoPath, SuggestionHunk, SuggestionInspection,
    SuggestionLine, SuggestionLineKind, SuggestionOutcome, SuggestionPreview, SuggestionReceipt,
    SuggestionRecord, SuggestionWorktree,
};

fn review_id() -> ReviewId {
    ReviewId::from_parts(40, 1)
}
fn id(n: u128) -> CommentId {
    CommentId::from_parts(41, n)
}
fn ready() -> ClientCore {
    let mut core = subscribed(1);
    open(&mut core, review_id());
    for n in [1, 2] {
        let mut c = comment(review_id(), n);
        c.id = id(n);
        c.thread_id = nits_client_core::thread_id_of(id(1));
        c.kind = CommentKind::Suggestion {
            patch: "@@ -1 +1 @@\n-old\n+new\n".into(),
        };
        c.anchor = Anchor::File {
            repo_id: repo_id(),
            path: RepoPath::new("same.txt").unwrap(),
            blob_oid: BlobOid::from_bytes([1; 20]),
        };
        core.handle(Input::Server(ServerMsg::Event {
            event: event(
                u64::try_from(n).unwrap() + 1,
                EventBody::CommentCreated { comment: c },
            ),
        }))
        .unwrap();
    }
    core
}
fn shown(core: &ClientCore, comment_id: CommentId) -> &SuggestionView {
    core.view()
        .threads
        .iter()
        .flat_map(|t| &t.comments)
        .find(|c| c.id == comment_id)
        .unwrap()
        .suggestion
        .as_ref()
        .unwrap()
}
fn record(core: &ClientCore, comment_id: CommentId) -> SuggestionRecord {
    shown(core, comment_id).record.clone()
}
fn checked(record: SuggestionRecord, worktree: SuggestionWorktree) -> SuggestionPreview {
    SuggestionPreview {
        suggestion: record,
        inspection: SuggestionInspection::Checked {
            hunks: vec![SuggestionHunk {
                header: "@@ -1 +1 @@".into(),
                lines: vec![
                    SuggestionLine {
                        kind: SuggestionLineKind::Remove { old: LineNo::FIRST },
                        text: "old".into(),
                        ending: LineEnding::Lf,
                    },
                    SuggestionLine {
                        kind: SuggestionLineKind::Add { new: LineNo::FIRST },
                        text: "new".into(),
                        ending: LineEnding::Lf,
                    },
                ],
            }],
            worktree,
        },
    }
}
fn query(core: &mut ClientCore, comment_id: CommentId) -> RequestId {
    let effects = core
        .handle(Input::User(Action::PreviewSuggestion { comment_id }))
        .unwrap();
    let (request_id, request) = sent_request(&effects).unwrap();
    assert_eq!(
        request,
        Request::PreviewSuggestion {
            review_id: review_id(),
            comment_id
        }
    );
    request_id
}
fn answer(core: &mut ClientCore, request_id: RequestId, preview: SuggestionPreview) {
    core.handle(Input::Server(ServerMsg::Response {
        id: request_id,
        response: Response::SuggestionPreview { preview },
    }))
    .unwrap();
}
fn inspect(core: &mut ClientCore, comment_id: CommentId, worktree: SuggestionWorktree) {
    let record = record(core, comment_id);
    let request_id = query(core, comment_id);
    answer(core, request_id, checked(record, worktree));
}
fn apply(core: &mut ClientCore, comment_id: CommentId) -> RequestId {
    let effects = core
        .handle(Input::User(Action::ApplySuggestion { comment_id }))
        .unwrap();
    let (request_id, request) = sent_request(&effects).unwrap();
    assert!(
        matches!(request, Request::Mutate { mutation: Mutation::ApplySuggestion { review_id: review, comment_id: comment }, .. } if review == review_id() && comment == comment_id)
    );
    assert_eq!(
        core.pending_count(),
        0,
        "filesystem writes must not enter optimistic resend"
    );
    request_id
}
fn applied(comment_id: CommentId) -> Event {
    let mut event = event(
        4,
        EventBody::SuggestionApplied {
            review_id: review_id(),
            comment_id,
            repo_id: repo_id(),
            path: RepoPath::new("same.txt").unwrap(),
            result_blob: BlobOid::from_bytes([2; 20]),
        },
    );
    event.client_id = config().client_id;
    event.author = config().author;
    event
}
fn receipt() -> SuggestionOutcome {
    let event = applied(id(1));
    SuggestionOutcome::Applied {
        receipt: SuggestionReceipt {
            seq: event.seq,
            at: event.ts,
            author: event.author,
            repo_id: repo_id(),
            path: RepoPath::new("same.txt").unwrap(),
            result_blob: BlobOid::from_bytes([2; 20]),
        },
    }
}
fn reconnect(core: &mut ClientCore) -> Vec<Effect> {
    core.handle(Input::Transport(TransportEvent::Disconnected))
        .unwrap();
    core.handle(Input::User(Action::Connect)).unwrap();
    core.handle(Input::Transport(TransportEvent::Connected))
        .unwrap();
    let effects = core.handle(Input::Server(welcome())).unwrap();
    let (request_id, _) = sent_request(&effects).unwrap();
    core.handle(Input::Server(ServerMsg::Response {
        id: request_id,
        response: Response::Subscribed { seq: Seq::new(3) },
    }))
    .unwrap()
}
fn preview_request(effects: &[Effect]) -> RequestId {
    let mut found = None;
    for effect in effects {
        if let Effect::Send(ClientMsg::Request { id, request }) = effect {
            assert!(
                !matches!(request, Request::Mutate { .. }),
                "reconnect replayed a filesystem mutation"
            );
            if matches!(request, Request::PreviewSuggestion { .. }) {
                found = Some(*id);
            }
        }
    }
    found.unwrap()
}

#[test]
fn exact_comment_preview_gates_application_and_guards_duplicate_commands() {
    let mut core = ready();
    assert_eq!(
        core.handle(Input::User(Action::ApplySuggestion { comment_id: id(1) })),
        Err(CoreError::SuggestionNotReady(id(1)))
    );
    let tab = core.view().tab;
    inspect(&mut core, id(2), SuggestionWorktree::Original);
    assert_eq!(
        core.view().tab,
        tab,
        "inline preview must not jump to Conversation"
    );
    assert_eq!(core.view().focused_comment, Some(id(2)));
    let effects = core
        .handle(Input::User(Action::RunCommand {
            command: Command::ApplySuggestion,
        }))
        .unwrap();
    assert!(
        matches!(sent_request(&effects).unwrap().1, Request::Mutate { mutation: Mutation::ApplySuggestion { comment_id, .. }, .. } if comment_id == id(2))
    );
    assert_eq!(shown(&core, id(2)).status, SuggestionStatus::Applying);
    assert_eq!(shown(&core, id(1)).status, SuggestionStatus::Unloaded);
    assert!(
        sent_request(
            &core
                .handle(Input::User(Action::ApplySuggestion { comment_id: id(2) }))
                .unwrap()
        )
        .is_none()
    );
    assert!(
        sent_request(
            &core
                .handle(Input::User(Action::PreviewSuggestion { comment_id: id(2) }))
                .unwrap()
        )
        .is_none()
    );
    assert!(shown(&core, id(2)).inspection.is_some());
}

#[test]
fn committed_event_and_reply_can_arrive_in_either_order_without_losing_receipt() {
    for response_first in [true, false] {
        let mut core = ready();
        inspect(&mut core, id(1), SuggestionWorktree::Original);
        let request_id = apply(&mut core, id(1));
        let messages = [
            ServerMsg::Response {
                id: request_id,
                response: Response::Committed {
                    event: applied(id(1)),
                },
            },
            ServerMsg::Event {
                event: applied(id(1)),
            },
        ];
        for index in if response_first { [0, 1] } else { [1, 0] } {
            core.handle(Input::Server(messages[index].clone())).unwrap();
            assert_eq!(shown(&core, id(1)).status, SuggestionStatus::Applied);
            assert_eq!(shown(&core, id(1)).record.outcome, receipt());
        }
        assert!(
            sent_request(
                &core
                    .handle(Input::User(Action::ApplySuggestion { comment_id: id(1) }))
                    .unwrap()
            )
            .is_none()
        );
    }
}

#[test]
fn lost_apply_response_reconciles_by_read_and_requires_explicit_retry_if_uncommitted() {
    for committed in [false, true] {
        let mut core = ready();
        inspect(&mut core, id(1), SuggestionWorktree::Original);
        apply(&mut core, id(1));
        let mut record = record(&core, id(1));
        let effects = reconnect(&mut core);
        let request_id = preview_request(&effects);
        assert_eq!(shown(&core, id(1)).status, SuggestionStatus::Loading);
        if committed {
            record.outcome = receipt();
        }
        answer(
            &mut core,
            request_id,
            checked(
                record,
                if committed {
                    SuggestionWorktree::Proposed
                } else {
                    SuggestionWorktree::Original
                },
            ),
        );
        if committed {
            assert_eq!(shown(&core, id(1)).status, SuggestionStatus::Applied);
            assert!(
                sent_request(
                    &core
                        .handle(Input::User(Action::ApplySuggestion { comment_id: id(1) }))
                        .unwrap()
                )
                .is_none()
            );
        } else {
            assert_eq!(shown(&core, id(1)).status, SuggestionStatus::Ready);
            apply(&mut core, id(1));
        }
    }
}

#[test]
fn post_write_error_preserves_recovery_location_and_does_not_invent_success_from_bytes() {
    let mut core = ready();
    inspect(&mut core, id(1), SuggestionWorktree::Original);
    let inspection = shown(&core, id(1)).inspection.clone();
    let request_id = apply(&mut core, id(1));
    core.handle(Input::Server(ServerMsg::Error {
        id: request_id,
        error: RpcError::Internal {
            message: "write interrupted; recover original at /tmp/recovery/original".into(),
        },
    }))
    .unwrap();
    assert!(matches!(
        shown(&core, id(1)).status,
        SuggestionStatus::Uncertain { .. }
    ));
    assert_eq!(shown(&core, id(1)).inspection, inspection);
    assert_eq!(
        core.handle(Input::User(Action::ApplySuggestion { comment_id: id(1) })),
        Err(CoreError::SuggestionNotReady(id(1)))
    );
    inspect(&mut core, id(1), SuggestionWorktree::Proposed);
    assert!(matches!(
        shown(&core, id(1)).status,
        SuggestionStatus::Uncertain { .. }
    ));
    assert_eq!(
        shown(&core, id(1)).record.outcome,
        SuggestionOutcome::Unapplied
    );
    assert!(
        shown(&core, id(1))
            .notice
            .as_ref()
            .unwrap()
            .contains("/tmp/recovery/original")
    );
}

#[test]
fn stale_and_malformed_previews_never_offer_apply_and_keep_patch_evidence() {
    let mut core = ready();
    inspect(&mut core, id(1), SuggestionWorktree::Changed);
    assert_eq!(shown(&core, id(1)).status, SuggestionStatus::Stale);
    assert_eq!(
        core.handle(Input::User(Action::ApplySuggestion { comment_id: id(1) })),
        Err(CoreError::SuggestionNotReady(id(1)))
    );
    let record = record(&core, id(1));
    let request_id = query(&mut core, id(1));
    answer(
        &mut core,
        request_id,
        SuggestionPreview {
            suggestion: record.clone(),
            inspection: SuggestionInspection::Rejected {
                reason: "invalid patch header".into(),
            },
        },
    );
    assert!(matches!(
        shown(&core, id(1)).status,
        SuggestionStatus::Rejected { .. }
    ));
    assert_eq!(shown(&core, id(1)).record.patch, record.patch);
    assert_eq!(
        core.handle(Input::User(Action::ApplySuggestion { comment_id: id(1) })),
        Err(CoreError::SuggestionNotReady(id(1)))
    );
}

#[test]
fn late_unapplied_preview_and_apply_error_cannot_override_a_committed_receipt() {
    let mut core = ready();
    let original = record(&core, id(1));
    let request_id = query(&mut core, id(1));
    core.handle(Input::Server(ServerMsg::Event {
        event: applied(id(1)),
    }))
    .unwrap();
    answer(
        &mut core,
        request_id,
        checked(original, SuggestionWorktree::Original),
    );
    assert_eq!(shown(&core, id(1)).status, SuggestionStatus::Applied);
    let mut core = ready();
    inspect(&mut core, id(1), SuggestionWorktree::Original);
    let request_id = apply(&mut core, id(1));
    core.handle(Input::Server(ServerMsg::Event {
        event: applied(id(1)),
    }))
    .unwrap();
    core.handle(Input::Server(ServerMsg::Error {
        id: request_id,
        error: RpcError::Internal {
            message: "late transport failure".into(),
        },
    }))
    .unwrap();
    assert_eq!(shown(&core, id(1)).status, SuggestionStatus::Applied);
    assert_eq!(shown(&core, id(1)).record.outcome, receipt());
    assert_eq!(shown(&core, id(1)).notice, None);
}

#[test]
fn applied_event_delivered_before_an_older_snapshot_survives_installation() {
    let mut core = ready();
    let snapshot = core.view().review.as_ref().unwrap().snapshot.clone();
    let effects = core
        .handle(Input::User(Action::OpenReview {
            review_id: review_id(),
        }))
        .unwrap();
    let (request_id, _) = sent_request(&effects).unwrap();
    core.handle(Input::Server(ServerMsg::Event {
        event: applied(id(1)),
    }))
    .unwrap();
    core.handle(Input::Server(ServerMsg::StreamItem {
        id: request_id,
        item: StreamItem::ReviewSnapshot { snapshot },
    }))
    .unwrap();
    assert_eq!(shown(&core, id(1)).status, SuggestionStatus::Applied);
    assert_eq!(shown(&core, id(1)).record.outcome, receipt());
}

#[test]
fn wrong_identity_and_old_review_previews_cannot_replace_current_suggestion() {
    let mut core = ready();
    let original = record(&core, id(1));
    let request_id = query(&mut core, id(1));
    let mut wrong = original.clone();
    wrong.anchor = Anchor::File {
        repo_id: RepoId::from_parts(99, 1),
        path: RepoPath::new("same.txt").unwrap(),
        blob_oid: BlobOid::from_bytes([1; 20]),
    };
    assert!(matches!(
        core.handle(Input::Server(ServerMsg::Response {
            id: request_id,
            response: Response::SuggestionPreview {
                preview: checked(wrong, SuggestionWorktree::Original)
            }
        })),
        Err(CoreError::UnexpectedResponse { .. })
    ));
    assert_eq!(shown(&core, id(1)).record, original);
    open(&mut core, ReviewId::from_parts(40, 2));
    answer(
        &mut core,
        request_id,
        checked(original, SuggestionWorktree::Original),
    );
    assert_eq!(core.view().open_review, Some(ReviewId::from_parts(40, 2)));
    assert!(core.view().threads.is_empty());
}
