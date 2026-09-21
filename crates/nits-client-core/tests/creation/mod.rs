//! Creation races exercise the same public effects boundary as every host.
use super::*;
use nits_client_core::{
    CreationBase, CreationDraft, CreationResume, CreationStatus, ReviewCreation,
};

fn workspace(n: u128) -> nits_protocol::Workspace {
    nits_protocol::Workspace {
        id: WorkspaceId::from_parts(10, n),
        name: format!("workspace {n}"),
        repos: vec![nits_protocol::Repo {
            id: RepoId::from_parts(11, n),
            display_name: "repo".into(),
            path: format!("/srv/repo-{n}"),
        }],
    }
}
fn begin() -> (ClientCore, ReviewId, RequestId) {
    let mut core = subscribed(0);
    home_workspaces(&mut core, vec![workspace(1), workspace(2)]);
    let effects = core
        .handle(Input::User(Action::StartReview {
            workspace_id: workspace(1).id,
        }))
        .unwrap();
    let (request, payload) = sent_request(&effects).unwrap();
    assert_eq!(
        payload,
        Request::DefaultBase {
            repo_id: workspace(1).repos[0].id
        }
    );
    let id = creation(&core).review_id;
    (core, id, request)
}
fn creation(core: &ClientCore) -> &ReviewCreation {
    core.view().home.creating.as_ref().unwrap()
}
fn answer(core: &mut ClientCore, id: RequestId, response: Response) -> Vec<Effect> {
    core.handle(Input::Server(ServerMsg::Response { id, response }))
        .unwrap()
}
fn error(core: &mut ClientCore, id: RequestId, error: RpcError) -> Vec<Effect> {
    core.handle(Input::Server(ServerMsg::Error { id, error }))
        .unwrap()
}
fn missing(id: ReviewId) -> RpcError {
    RpcError::NotFound {
        kind: nits_protocol::EntityKind::Review,
        id: id.to_string(),
    }
}
fn edit(core: &mut ClientCore, change: impl FnOnce(&mut CreationDraft)) {
    let current = creation(core);
    let id = current.review_id;
    let mut draft = current.draft.clone();
    change(&mut draft);
    let mut edits = vec![nits_client_core::CreationEdit::Title { text: draft.title }];
    for target in draft.targets {
        edits.push(nits_client_core::CreationEdit::Repository {
            target_id: target.id,
            repo_id: target.repo_id,
        });
        match target.base {
            CreationBase::Automatic => {}
            CreationBase::Manual { text } => edits.push(nits_client_core::CreationEdit::Base {
                target_id: target.id,
                text,
            }),
        }
        edits.push(nits_client_core::CreationEdit::Head {
            target_id: target.id,
            text: target.head,
        });
    }
    for edit in edits {
        core.handle(Input::User(Action::EditCreationDraft {
            review_id: id,
            edit,
        }))
        .unwrap();
    }
}
fn ready() -> (ClientCore, ReviewId) {
    let (mut core, id, request) = begin();
    answer(
        &mut core,
        request,
        Response::DefaultBase {
            base: RefSpec::Branch {
                name: "develop".into(),
            },
        },
    );
    edit(&mut core, |draft| draft.title = "retained title".into());
    (core, id)
}
fn ready_two() -> (ClientCore, ReviewId) {
    let (mut core, id) = ready();
    let mut attached = workspace(1);
    attached.repos.push(workspace(2).repos[0].clone());
    home_workspaces(&mut core, vec![attached, workspace(2)]);
    (core, id)
}
fn submit(core: &mut ClientCore, id: ReviewId) -> (RequestId, Review) {
    let effects = core
        .handle(Input::User(Action::SubmitReviewCreation { review_id: id }))
        .unwrap();
    let (request, payload) = sent_request(&effects).unwrap();
    let Request::Mutate {
        mutation:
            Mutation::CreateReview {
                review_id,
                workspace_id,
                title,
                targets,
            },
        ..
    } = payload
    else {
        panic!("create mutation")
    };
    assert_eq!(review_id, id);
    (
        request,
        Review {
            id: review_id,
            workspace_id,
            title,
            targets,
            created: Timestamp::from_millis(0),
            status: ReviewStatus::Open,
        },
    )
}
fn disconnect(core: &mut ClientCore) {
    core.handle(Input::Transport(TransportEvent::Disconnected))
        .unwrap();
}

#[test]
fn defaults_are_correlated_and_never_replace_manual_input_or_new_workspace() {
    let (mut core, id, old) = begin();
    edit(&mut core, |d| {
        d.targets[0].base = CreationBase::Manual {
            text: "typed-before-reply".into(),
        }
    });
    answer(
        &mut core,
        old,
        Response::DefaultBase {
            base: RefSpec::Branch {
                name: "develop".into(),
            },
        },
    );
    assert_eq!(
        creation(&core).draft.targets[0].base,
        CreationBase::Manual {
            text: "typed-before-reply".into()
        }
    );
    let effects = core
        .handle(Input::User(Action::StartReview {
            workspace_id: workspace(2).id,
        }))
        .unwrap();
    let (b, _) = sent_request(&effects).unwrap();
    assert_ne!(creation(&core).review_id, id);
    let effects = core
        .handle(Input::User(Action::StartReview {
            workspace_id: workspace(1).id,
        }))
        .unwrap();
    let (a, _) = sent_request(&effects).unwrap();
    answer(
        &mut core,
        b,
        Response::DefaultBase {
            base: RefSpec::Branch {
                name: "other-base".into(),
            },
        },
    );
    assert!(matches!(
        creation(&core).defaults[0].state,
        nits_client_core::CreationDefaultState::Loading
    ));
    answer(
        &mut core,
        a,
        Response::DefaultBase {
            base: RefSpec::Branch {
                name: "trunk".into(),
            },
        },
    );
    edit(&mut core, |d| d.title = "new workspace draft".into());
    let current = creation(&core).review_id;
    let (_, review) = submit(&mut core, current);
    assert_eq!(
        review.targets.first().base,
        RefSpec::Branch {
            name: "trunk".into()
        }
    );
}

#[test]
fn default_failure_is_visible_and_manual_base_can_be_submitted() {
    let (mut core, id, request) = begin();
    error(
        &mut core,
        request,
        RpcError::Internal {
            message: "no default branch".into(),
        },
    );
    edit(&mut core, |d| d.title = "keep me".into());
    let effects = core
        .handle(Input::User(Action::SubmitReviewCreation { review_id: id }))
        .unwrap();
    assert!(sent_request(&effects).is_none());
    assert!(
        matches!(&creation(&core).status,CreationStatus::Failed {message} if message.contains("no default branch"))
    );
    edit(&mut core, |d| {
        d.targets[0].base = CreationBase::Manual {
            text: "tag:v1".into(),
        }
    });
    let (_, review) = submit(&mut core, id);
    assert_eq!(
        review.targets.first().base,
        RefSpec::Tag { name: "v1".into() }
    );
}

#[test]
fn rejected_creation_retains_all_inputs_then_success_completes_once() {
    let (mut core, id) = ready();
    edit(&mut core, |d| {
        d.targets[0].base = CreationBase::Manual {
            text: "missing-ref".into(),
        }
    });
    let before = creation(&core).draft.clone();
    let (request, _) = submit(&mut core, id);
    assert!(matches!(
        creation(&core).status,
        CreationStatus::Pending { .. }
    ));
    for action in [
        Action::SubmitReviewCreation { review_id: id },
        Action::CancelNewReview,
        Action::StartReview {
            workspace_id: workspace(2).id,
        },
    ] {
        assert!(sent_request(&core.handle(Input::User(action)).unwrap()).is_none());
        assert!(matches!(
            creation(&core).status,
            CreationStatus::Pending { .. }
        ));
    }
    let effects = error(
        &mut core,
        request,
        RpcError::Internal {
            message: "revision missing-ref does not exist".into(),
        },
    );
    let (lookup, payload) = sent_request(&effects).unwrap();
    assert_eq!(payload, Request::GetReview { review_id: id });
    error(&mut core, lookup, missing(id));
    assert_eq!(creation(&core).draft, before);
    assert!(
        matches!(&creation(&core).status,CreationStatus::Failed {message} if message.contains("missing-ref"))
    );
    edit(&mut core, |d| d.targets[0].base = CreationBase::Automatic);
    let (request, review) = submit(&mut core, id);
    core.handle(Input::Server(ServerMsg::Event {
        event: event(
            1,
            EventBody::ReviewCreated {
                review: review.clone(),
            },
        ),
    }))
    .unwrap();
    assert_eq!(creation(&core).status, CreationStatus::Succeeded);
    answer(
        &mut core,
        request,
        Response::Committed {
            event: event(1, EventBody::ReviewCreated { review }),
        },
    );
    assert_eq!(core.view().reviews.iter().filter(|r| r.id == id).count(), 1);
}

#[test]
fn browser_lost_ack_restore_checks_the_stable_id_without_replaying_creation() {
    let (mut original, id) = ready();
    let retained_before_pending_patch = creation(&original).clone();
    let (_, created) = submit(&mut original, id);
    let mut restored = subscribed(0);
    home_workspaces(&mut restored, vec![workspace(1)]);
    let effects = restored
        .handle(Input::User(Action::RestoreReviewCreation {
            creation: retained_before_pending_patch,
            resume: CreationResume::Submitted,
        }))
        .unwrap();
    let (lookup, payload) = sent_request(&effects).unwrap();
    assert_eq!(payload, Request::GetReview { review_id: id });
    assert_eq!(
        effects
            .iter()
            .filter(|e| matches!(e, Effect::Send(_)))
            .count(),
        1
    );
    answer(&mut restored, lookup, Response::Review { review: created });
    assert_eq!(creation(&restored).status, CreationStatus::Succeeded);
    assert_eq!(creation(&restored).draft.title, "retained title");
}

#[test]
fn uncertain_retry_reuses_frozen_payload_and_recovers_if_original_write_wins() {
    let (mut original, id) = ready();
    let (_, created) = submit(&mut original, id);
    disconnect(&mut original);
    let retained = creation(&original).clone();
    assert!(matches!(
        retained.status,
        CreationStatus::Interrupted { .. }
    ));
    let mut restored = subscribed(0);
    let effects = restored
        .handle(Input::User(Action::RestoreReviewCreation {
            creation: retained,
            resume: CreationResume::Submitted,
        }))
        .unwrap();
    let (lookup, _) = sent_request(&effects).unwrap();
    error(&mut restored, lookup, missing(id));
    assert!(matches!(
        creation(&restored).status,
        CreationStatus::Interrupted { .. }
    ));
    let effects = restored
        .handle(Input::User(Action::RetryReviewCreation { review_id: id }))
        .unwrap();
    let (lookup, payload) = sent_request(&effects).unwrap();
    assert_eq!(payload, Request::GetReview { review_id: id });
    let effects = error(&mut restored, lookup, missing(id));
    let (retry, payload) = sent_request(&effects).unwrap();
    assert!(
        matches!(payload,Request::Mutate {mutation:Mutation::CreateReview {review_id,title,targets,..},..} if review_id==id && title==created.title && targets==created.targets)
    );
    let effects = error(
        &mut restored,
        retry,
        RpcError::Invalid {
            reason: "review id already exists".into(),
        },
    );
    let (lookup, payload) = sent_request(&effects).unwrap();
    assert_eq!(payload, Request::GetReview { review_id: id });
    answer(&mut restored, lookup, Response::Review { review: created });
    assert_eq!(creation(&restored).status, CreationStatus::Succeeded);
}

#[test]
fn lookup_failure_never_resends_and_context_change_never_restores_another_daemon_draft() {
    let (original, id) = ready();
    let retained = creation(&original).clone();
    let mut restored = subscribed(0);
    let effects = restored
        .handle(Input::User(Action::RestoreReviewCreation {
            creation: retained.clone(),
            resume: CreationResume::Submitted,
        }))
        .unwrap();
    let (lookup, _) = sent_request(&effects).unwrap();
    let effects = error(
        &mut restored,
        lookup,
        RpcError::Internal {
            message: "offline storage".into(),
        },
    );
    assert!(sent_request(&effects).is_none());
    assert!(
        matches!(&creation(&restored).status,CreationStatus::Interrupted {message,..} if message.contains("offline storage"))
    );
    let effects = restored
        .handle(Input::User(Action::EditCreationDraft {
            review_id: id,
            edit: nits_client_core::CreationEdit::Title {
                text: "must be ignored".into(),
            },
        }))
        .unwrap();
    assert!(effects.is_empty());
    assert_eq!(creation(&restored).draft, retained.draft);
    let mut other = subscribed(0);
    other
        .handle(Input::User(Action::SetReferenceContext {
            context: nits_protocol::ReferenceContext::named("other-daemon").unwrap(),
        }))
        .unwrap();
    let effects = other
        .handle(Input::User(Action::RestoreReviewCreation {
            creation: retained,
            resume: CreationResume::Submitted,
        }))
        .unwrap();
    assert!(sent_request(&effects).is_none());
    assert!(other.view().home.creating.is_none());
    assert!(matches!(
        other.view().last_error,
        Some(RpcError::Invalid { .. })
    ));
}

#[test]
fn form_commands_use_the_keymap_and_disconnected_submit_is_visible() {
    let (mut core, id) = ready_two();
    home_key(&mut core, "alt+a");
    assert_eq!(creation(&core).draft.targets.len(), 2);
    home_key(&mut core, "alt+d");
    assert_eq!(creation(&core).draft.targets.len(), 1);
    disconnect(&mut core);
    let effects = home_key(&mut core, "ctrl+enter");
    assert!(sent_request(&effects).is_none());
    assert!(
        matches!(&creation(&core).status,CreationStatus::Failed {message} if message.contains("disconnected"))
    );
    assert_eq!(creation(&core).review_id, id);
    assert!(home_key(&mut core, "alt+r").contains(&Effect::Connect));
    assert_eq!(creation(&core).draft.title, "retained title");
    home_key(&mut core, "esc");
    assert!(core.view().home.creating.is_none());
}

#[test]
fn default_reply_during_pending_manual_submission_remains_available_after_rejection() {
    let (mut core, id, default_request) = begin();
    edit(&mut core, |draft| {
        draft.title = "retained manual attempt".into();
        draft.targets[0].base = CreationBase::Manual {
            text: "missing-ref".into(),
        };
    });
    let (write_request, attempted) = submit(&mut core, id);
    answer(
        &mut core,
        default_request,
        Response::DefaultBase {
            base: RefSpec::Branch {
                name: "develop".into(),
            },
        },
    );
    assert!(matches!(
        &creation(&core).status,
        CreationStatus::Pending { submission } if submission.targets == attempted.targets
    ));
    let effects = error(
        &mut core,
        write_request,
        RpcError::Internal {
            message: "missing revision".into(),
        },
    );
    let (lookup, request) = sent_request(&effects).unwrap();
    assert_eq!(request, Request::GetReview { review_id: id });
    error(&mut core, lookup, missing(id));
    assert!(matches!(
        creation(&core).status,
        CreationStatus::Failed { .. }
    ));
    // Selecting this repository again uses its cached automatic default. The
    // reply must not have been discarded while the manual write was pending.
    edit(&mut core, |draft| {
        draft.targets[0].base = CreationBase::Automatic;
    });
    let (_, retried) = submit(&mut core, id);
    assert_eq!(
        retried.targets.first().base,
        RefSpec::Branch {
            name: "develop".into()
        }
    );
}

#[test]
fn retained_or_completed_creation_does_not_capture_review_composer_actions() {
    for completed in [false, true] {
        let (mut core, id) = ready();
        if completed {
            let (request, created) = submit(&mut core, id);
            answer(
                &mut core,
                request,
                Response::Committed {
                    event: event(1, EventBody::ReviewCreated { review: created }),
                },
            );
        }
        let retained = creation(&core).clone();
        open(&mut core, ReviewId::from_parts(40, 1));
        core.handle(Input::User(Action::DraftOpened {
            anchor: Anchor::Review,
        }))
        .unwrap();
        core.handle(Input::InvalidAction {
            reason: "Correct this comment".into(),
        })
        .unwrap();
        assert_eq!(
            core.view()
                .draft
                .as_ref()
                .unwrap()
                .submission_error
                .as_deref(),
            Some("Correct this comment")
        );
        core.handle(Input::User(Action::RunCommand {
            command: nits_client_core::Command::Back,
        }))
        .unwrap();
        assert_eq!(
            core.view().draft,
            None,
            "Escape dismisses the visible comment composer"
        );
        assert_eq!(
            creation(&core),
            &retained,
            "the hidden creation remains unchanged"
        );
        core.handle(Input::User(Action::StartReview {
            workspace_id: workspace(1).id,
        }))
        .unwrap();
        assert_eq!(
            core.view().open_review,
            None,
            "New review brings its form into view"
        );
        assert_eq!(core.view().focus, nits_client_core::Focus::Composer);
        if !completed {
            assert_eq!(creation(&core), &retained);
        }
    }
}

#[test]
fn target_structure_and_field_edits_stay_ordered_without_waiting_for_view_patches() {
    use nits_client_core::{Command, CreationEdit, CreationRevision};
    let (mut core, id) = ready_two();
    let first = creation(&core).draft.targets[0].id;
    let before = creation(&core).revision;
    core.handle(Input::User(Action::RunCommand {
        command: Command::AddReviewTarget,
    }))
    .unwrap();
    let second = creation(&core).draft.targets[1].id;
    assert_ne!(first, second);
    for edit in [
        CreationEdit::Title {
            text: "title after add before ACK".into(),
        },
        CreationEdit::Base {
            target_id: second,
            text: "second-base".into(),
        },
    ] {
        core.handle(Input::User(Action::EditCreationDraft {
            review_id: id,
            edit,
        }))
        .unwrap();
    }
    assert_eq!(creation(&core).draft.targets.len(), 2);
    core.handle(Input::User(Action::SelectCreationTarget {
        review_id: id,
        target_id: first,
    }))
    .unwrap();
    core.handle(Input::User(Action::RunCommand {
        command: Command::RemoveReviewTarget,
    }))
    .unwrap();
    for edit in [
        CreationEdit::Title {
            text: "title after remove before ACK".into(),
        },
        CreationEdit::Base {
            target_id: first,
            text: "stale removed row".into(),
        },
        CreationEdit::Head {
            target_id: second,
            text: "head".into(),
        },
    ] {
        core.handle(Input::User(Action::EditCreationDraft {
            review_id: id,
            edit,
        }))
        .unwrap();
    }
    assert_eq!(creation(&core).draft.targets.len(), 1);
    let target = &creation(&core).draft.targets[0];
    assert_eq!(target.id, second);
    assert_eq!(
        target.base,
        CreationBase::Manual {
            text: "second-base".into()
        }
    );
    assert_eq!(target.head, "head");
    assert!(creation(&core).revision > before);
    assert!(creation(&core).revision > CreationRevision::new(0));
    let retained = creation(&core).clone();
    let (_, created) = submit(&mut core, id);
    assert_eq!(created.targets.len(), 1);
    assert_eq!(created.title, retained.draft.title);
    assert_eq!(
        created.targets.first().base,
        RefSpec::Branch {
            name: "second-base".into()
        }
    );
    // A browser that loses the ACK restores precisely these rows and checks
    // the same attempt, without recreating removed rows or writing again.
    let mut restored = subscribed(0);
    let effects = restored
        .handle(Input::User(Action::RestoreReviewCreation {
            creation: retained.clone(),
            resume: CreationResume::Submitted,
        }))
        .unwrap();
    let (lookup, request) = sent_request(&effects).unwrap();
    assert_eq!(request, Request::GetReview { review_id: id });
    answer(&mut restored, lookup, Response::Review { review: created });
    assert_eq!(creation(&restored).draft, retained.draft);
    assert_eq!(creation(&restored).revision, retained.revision);
    assert_eq!(creation(&restored).status, CreationStatus::Succeeded);
}

#[test]
fn unchanged_retry_has_a_new_acknowledgement_and_recovers_without_duplicate_writes() {
    for committed in [false, true] {
        let (mut core, id) = ready();
        let (first, _) = submit(&mut core, id);
        let effects = error(
            &mut core,
            first,
            RpcError::Internal {
                message: "missing branch".into(),
            },
        );
        let (lookup, _) = sent_request(&effects).unwrap();
        error(&mut core, lookup, missing(id));
        let failed = creation(&core).clone();
        assert!(matches!(failed.status, CreationStatus::Failed { .. }));
        // The repository can be repaired externally: no draft edit is needed.
        let (_, created) = submit(&mut core, id);
        let retry_revision = creation(&core).revision;
        assert!(retry_revision > failed.revision);
        assert_eq!(creation(&core).draft, failed.draft);
        assert!(
            core.handle(Input::User(Action::SubmitReviewCreation { review_id: id }))
                .unwrap()
                .is_empty()
        );
        assert_eq!(creation(&core).revision, retry_revision);
        // The browser only saw the previous failure; its local submit intent
        // advances the revision before any host acknowledgement can be lost.
        let mut frozen = failed.clone();
        frozen.revision = retry_revision;
        let mut restored = subscribed(0);
        let effects = restored
            .handle(Input::User(Action::RestoreReviewCreation {
                creation: frozen,
                resume: CreationResume::Submitted,
            }))
            .unwrap();
        let (lookup, request) = sent_request(&effects).unwrap();
        assert_eq!(request, Request::GetReview { review_id: id });
        if committed {
            answer(&mut restored, lookup, Response::Review { review: created });
            assert_eq!(creation(&restored).status, CreationStatus::Succeeded);
        } else {
            error(&mut restored, lookup, missing(id));
            assert!(matches!(
                creation(&restored).status,
                CreationStatus::Interrupted { .. }
            ));
            let effects = restored
                .handle(Input::User(Action::RetryReviewCreation { review_id: id }))
                .unwrap();
            assert!(creation(&restored).revision > retry_revision);
            let (lookup, request) = sent_request(&effects).unwrap();
            assert_eq!(request, Request::GetReview { review_id: id });
            assert!(
                restored
                    .handle(Input::User(Action::RetryReviewCreation { review_id: id }))
                    .unwrap()
                    .is_empty()
            );
            let effects = error(&mut restored, lookup, missing(id));
            let (_, request) = sent_request(&effects).unwrap();
            assert!(matches!(request, Request::Mutate {
                mutation: Mutation::CreateReview { review_id, title, targets, .. }, ..
            } if review_id == id && title == created.title && targets == created.targets));
        }
        assert_eq!(creation(&restored).draft, failed.draft);
    }
}

#[test]
fn rejected_submission_and_disconnected_retry_acknowledge_the_current_intent() {
    let (mut core, id, _) = begin();
    let revision = creation(&core).revision;
    let effects = core
        .handle(Input::User(Action::SubmitReviewCreation { review_id: id }))
        .unwrap();
    assert!(sent_request(&effects).is_none());
    assert!(matches!(
        creation(&core).status,
        CreationStatus::Failed { .. }
    ));
    assert!(creation(&core).revision > revision);
    let (mut core, id) = ready();
    submit(&mut core, id);
    disconnect(&mut core);
    let interrupted = creation(&core).clone();
    let effects = core
        .handle(Input::User(Action::RetryReviewCreation { review_id: id }))
        .unwrap();
    assert!(sent_request(&effects).is_none());
    assert!(!effects.is_empty());
    assert!(creation(&core).revision > interrupted.revision);
    assert_eq!(creation(&core).draft, interrupted.draft);
    assert!(matches!(&creation(&core).status,
        CreationStatus::Interrupted {message, ..} if message.contains("Reconnect")));
}

#[test]
fn creation_add_exhaustion_is_acknowledged_and_removed_repositories_become_available() {
    use nits_client_core::{CreationEdit, CreationRevision};
    let (mut core, id) = ready_two();
    let first = creation(&core).draft.targets[0].clone();
    core.handle(Input::User(Action::AddCreationTarget { review_id: id }))
        .unwrap();
    let second = creation(&core).draft.targets[1].clone();
    assert_ne!(first.repo_id, second.repo_id);
    let before = creation(&core).clone();
    for _ in 0..4 {
        core.handle(Input::User(Action::AddCreationTarget { review_id: id }))
            .unwrap();
    }
    assert_eq!(creation(&core).draft, before.draft);
    assert!(creation(&core).revision > before.revision);
    core.handle(Input::User(Action::RemoveCreationTarget { review_id: id }))
        .unwrap();
    core.handle(Input::User(Action::AddCreationTarget { review_id: id }))
        .unwrap();
    let added = creation(&core).draft.targets[1].clone();
    assert_eq!(added.repo_id, second.repo_id);
    assert_ne!(added.id, second.id);
    core.handle(Input::User(Action::EditCreationDraft {
        review_id: id,
        edit: CreationEdit::Repository {
            target_id: second.id,
            repo_id: first.repo_id,
        },
    }))
    .unwrap();
    assert_eq!(
        creation(&core).draft.targets[1],
        added,
        "removed row edit cannot alter its replacement"
    );
    assert!(creation(&core).revision > CreationRevision::new(0));
}

#[test]
fn duplicate_repository_edits_remain_visible_and_cannot_submit_until_corrected() {
    use nits_client_core::CreationEdit;
    let (mut core, id) = ready_two();
    core.handle(Input::User(Action::AddCreationTarget { review_id: id }))
        .unwrap();
    let first = creation(&core).draft.targets[0].clone();
    let second = creation(&core).draft.targets[1].clone();
    core.handle(Input::User(Action::EditCreationDraft {
        review_id: id,
        edit: CreationEdit::Repository {
            target_id: second.id,
            repo_id: first.repo_id,
        },
    }))
    .unwrap();
    assert!(
        matches!(&creation(&core).status, CreationStatus::Failed { message } if message.contains("one base/head pair"))
    );
    edit(&mut core, |draft| {
        draft.title = "keep all invalid inputs".into();
        draft.targets[1].head = "branch:other".into();
    });
    let draft = creation(&core).draft.clone();
    let effects = core
        .handle(Input::User(Action::SubmitReviewCreation { review_id: id }))
        .unwrap();
    assert!(sent_request(&effects).is_none());
    assert_eq!(creation(&core).draft, draft);
    assert_eq!(creation(&core).review_id, id);
    assert!(
        matches!(&creation(&core).status, CreationStatus::Failed { message } if message.contains(&first.repo_id.to_string()))
    );
    for edit in [
        CreationEdit::Repository {
            target_id: second.id,
            repo_id: second.repo_id,
        },
        CreationEdit::Base {
            target_id: second.id,
            text: "main".into(),
        },
    ] {
        core.handle(Input::User(Action::EditCreationDraft {
            review_id: id,
            edit,
        }))
        .unwrap();
    }
    let (_, review) = submit(&mut core, id);
    assert_eq!(review.title, draft.title);
    assert_eq!(review.targets.len(), 2);
    assert_ne!(
        review.targets.first().repo_id,
        review.targets.as_slice()[1].repo_id
    );
}

#[test]
fn restored_legacy_duplicate_attempt_is_inspected_but_never_replayed_as_a_write() {
    use nits_client_core::{CreationSubmission, CreationTargetId};
    let (core, id) = ready();
    let mut retained = creation(&core).clone();
    let mut duplicate = retained.draft.targets[0].clone();
    duplicate.id = CreationTargetId::new(99);
    duplicate.head = "head".into();
    retained.draft.targets.push(duplicate);
    let targets = NonEmpty::new(vec![
        ReviewTarget {
            repo_id: workspace(1).repos[0].id,
            base: RefSpec::Head,
            head: RefSpec::WorkingTree,
        },
        ReviewTarget {
            repo_id: workspace(1).repos[0].id,
            base: RefSpec::Head,
            head: RefSpec::Head,
        },
    ])
    .unwrap();
    retained.status = CreationStatus::Pending {
        submission: CreationSubmission {
            title: retained.draft.title.clone(),
            targets: targets.clone(),
        },
    };
    for committed in [true, false] {
        let mut restored = subscribed(0);
        let effects = restored
            .handle(Input::User(Action::RestoreReviewCreation {
                creation: retained.clone(),
                resume: CreationResume::Submitted,
            }))
            .unwrap();
        let (lookup, request) = sent_request(&effects).unwrap();
        assert_eq!(request, Request::GetReview { review_id: id });
        if committed {
            let review = Review {
                id,
                workspace_id: retained.workspace_id,
                title: retained.draft.title.clone(),
                targets: targets.clone(),
                created: Timestamp::from_millis(0),
                status: ReviewStatus::Open,
            };
            answer(&mut restored, lookup, Response::Review { review });
            assert_eq!(creation(&restored).status, CreationStatus::Succeeded);
        } else {
            error(&mut restored, lookup, missing(id));
            let effects = restored
                .handle(Input::User(Action::RetryReviewCreation { review_id: id }))
                .unwrap();
            let (lookup, request) = sent_request(&effects).unwrap();
            assert_eq!(request, Request::GetReview { review_id: id });
            let effects = error(&mut restored, lookup, missing(id));
            assert!(sent_request(&effects).is_none());
            assert!(
                matches!(&creation(&restored).status, CreationStatus::Failed { message } if message.contains("one base/head pair"))
            );
        }
        assert_eq!(creation(&restored).review_id, id);
        assert_eq!(creation(&restored).draft, retained.draft);
    }
    let mut restored = subscribed(0);
    retained.status = CreationStatus::Editing;
    restored
        .handle(Input::User(Action::RestoreReviewCreation {
            creation: retained.clone(),
            resume: CreationResume::Editing,
        }))
        .unwrap();
    assert_eq!(creation(&restored).draft, retained.draft);
    assert!(
        matches!(&creation(&restored).status, CreationStatus::Failed { message } if message.contains("one base/head pair"))
    );
    let effects = restored
        .handle(Input::User(Action::CreateReview {
            workspace_id: retained.workspace_id,
            title: retained.draft.title,
            targets,
        }))
        .unwrap();
    assert!(sent_request(&effects).is_none());
    assert!(
        matches!(&restored.view().last_error, Some(RpcError::Invalid { reason }) if reason.contains("one base/head pair"))
    );
}

#[test]
fn help_back_preserves_creation_until_the_next_back() {
    let (mut core, _, _) = begin();
    edit(&mut core, |draft| draft.title = "Keep this draft".into());
    let before = creation(&core).clone();
    core.handle(Input::User(Action::ToggleHelp)).unwrap();
    core.handle(Input::Key(nits_client_core::KeyChord::named(
        nits_client_core::NamedKey::Esc,
    )))
    .unwrap();
    assert!(core.view().help.is_none());
    assert_eq!(creation(&core), &before);
    core.handle(Input::Key(nits_client_core::KeyChord::named(
        nits_client_core::NamedKey::Esc,
    )))
    .unwrap();
    assert!(core.view().home.creating.is_none());
}
