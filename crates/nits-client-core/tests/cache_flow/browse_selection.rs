//! A candidate never changes the visible repository/ref until it resolves.
use super::shared_trees::{open_shared, repos, scoped_tree};
use super::*;
use nits_client_core::{
    BrowseStatus, Command, RefSelectorPurpose, RefSelectorSide, RefSelectorStatus,
};
use nits_protocol::{RefCandidate, RpcError};

fn ready() -> ClientCore {
    let mut core = subscribed(local());
    open_shared(&mut core, repos(), true);
    core
}
fn select(core: &mut ClientCore, repo_id: RepoId, name: &str) -> RequestId {
    let effects = core
        .handle(Input::User(Action::SetBrowseRef {
            repo_id,
            ref_spec: Some(RefSpec::Branch { name: name.into() }),
        }))
        .unwrap();
    let sent = requests(&effects);
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].1,
        Request::TreeSnapshot {
            repo_id,
            ref_spec: RefSpec::Branch { name: name.into() }
        }
    );
    sent[0].0
}
fn answer(core: &mut ClientCore, id: RequestId, repo_id: RepoId, root: u8) -> Vec<Effect> {
    core.handle(Input::Server(ServerMsg::Response {
        id,
        response: Response::TreeSnapshot {
            snapshot: scoped_tree(repo_id, root, &["same.txt"]),
        },
    }))
    .unwrap()
}
fn fail(core: &mut ClientCore, id: RequestId) -> Vec<Effect> {
    core.handle(Input::Server(ServerMsg::Error {
        id,
        error: RpcError::Invalid {
            reason: "branch does not exist in this repository".into(),
        },
    }))
    .unwrap()
}
fn open_file(core: &mut ClientCore, repo_id: RepoId) {
    core.handle(Input::User(Action::Viewport {
        file: FileRef {
            repo_id,
            path: path("same.txt"),
        },
        first_row: 0,
        last_row: 10,
    }))
    .unwrap();
}
fn catalog(core: &mut ClientCore, id: RequestId, repo_id: RepoId, name: &str) {
    core.handle(Input::Server(ServerMsg::Response {
        id,
        response: Response::Refs {
            repo_id,
            refs: vec![RefCandidate {
                ref_spec: RefSpec::Branch { name: name.into() },
                subject: None,
            }],
        },
    }))
    .unwrap();
}
fn picker(core: &mut ClientCore, repo_id: RepoId) -> RequestId {
    let effects = core
        .handle(Input::User(Action::OpenBrowseRefSelector { repo_id }))
        .unwrap();
    assert_eq!(requests(&effects)[0].1, Request::ListRefs { repo_id });
    requests(&effects)[0].0
}

#[test]
fn explicit_refs_never_select_fuzzy_commit_subjects_or_other_catalog_entries() {
    let [_, repo_id] = repos();
    let oid = CommitOid::from_bytes([42; 20]);
    for (query, ref_spec) in [
        (
            "branch:main".to_owned(),
            RefSpec::Branch {
                name: "main".into(),
            },
        ),
        ("tag:v1".to_owned(), RefSpec::Tag { name: "v1".into() }),
        (format!("commit:{oid}"), RefSpec::Commit { oid }),
        ("head".to_owned(), RefSpec::Head),
        ("upstream".to_owned(), RefSpec::Upstream),
        ("worktree".to_owned(), RefSpec::WorkingTree),
    ] {
        for present in [false, true] {
            let mut core = ready();
            let id = picker(&mut core, repo_id);
            let mut refs = vec![RefCandidate {
                ref_spec: RefSpec::Commit {
                    oid: CommitOid::from_bytes([43; 20]),
                },
                subject: Some(format!("Fix deployment for {query}")),
            }];
            if present {
                refs.push(RefCandidate {
                    ref_spec: ref_spec.clone(),
                    subject: None,
                });
            }
            core.handle(Input::Server(ServerMsg::Response {
                id,
                response: Response::Refs { repo_id, refs },
            }))
            .unwrap();
            core.handle(Input::User(Action::RefSelectorQuery {
                query: query.clone(),
            }))
            .unwrap();
            assert_eq!(core.view().ref_selector.as_ref().unwrap().options.len(), 1);
            let effects = core.handle(Input::User(Action::SelectCurrentRef)).unwrap();
            assert_eq!(
                requests(&effects)[0].1,
                Request::TreeSnapshot {
                    repo_id,
                    ref_spec: ref_spec.clone(),
                },
                "{query}, present={present}"
            );
        }
    }
}
#[test]
fn selected_repository_follows_open_file_and_explicit_keyboard_choice() {
    let [alpha, beta] = repos();
    let mut core = ready();
    open_file(&mut core, beta);
    assert_eq!(core.view().browse.as_ref().unwrap().repo_id, beta);
    assert_eq!(
        resolve_command(&core, Command::BrowseRevision).unwrap(),
        Action::OpenBrowseRefSelector { repo_id: beta }
    );
    for (command, repo_id) in [
        (Command::NextBrowseRepo, alpha),
        (Command::PrevBrowseRepo, beta),
    ] {
        let action = resolve_command(&core, command).unwrap();
        assert_eq!(action, Action::SelectBrowseRepo { repo_id });
        core.handle(Input::User(action)).unwrap();
    }
    core.handle(Input::User(Action::SelectBrowseRepo { repo_id: alpha }))
        .unwrap();
    // A viewport update for the same file must not undo an explicit choice.
    open_file(&mut core, beta);
    assert_eq!(core.view().browse.as_ref().unwrap().repo_id, alpha);
    core.handle(Input::User(Action::ToggleDir {
        repo_id: beta,
        path: None,
    }))
    .unwrap();
    assert_eq!(core.view().browse.as_ref().unwrap().repo_id, beta);
    let unknown = RepoId::from_parts(20, 20);
    let before = core.view().clone();
    assert_eq!(
        core.handle(Input::User(Action::SelectBrowseRepo { repo_id: unknown })),
        Err(CoreError::UnknownRepo(unknown))
    );
    assert_eq!(core.view(), &before);
}
#[test]
fn failed_candidate_preserves_visible_file_then_success_commits_exact_repo_and_reset_unpins() {
    let [alpha, beta] = repos();
    let mut core = ready();
    open_file(&mut core, beta);
    let visible = open_render(&core);
    let before = core.view().tree.clone();
    let id = select(&mut core, beta, "main");
    assert_eq!(open_render(&core), visible);
    assert!(core.view().browse.as_ref().unwrap().selection.is_none());
    fail(&mut core, id);
    assert_eq!(open_render(&core), visible);
    assert_eq!(core.view().tree, before);
    let browse = core.view().browse.as_ref().unwrap();
    assert_eq!(browse.attempt.as_ref().unwrap().target.repo_id, beta);
    assert!(matches!(
        browse.attempt.as_ref().unwrap().status,
        BrowseStatus::Failed { .. }
    ));
    let id = select(&mut core, alpha, "main");
    answer(&mut core, id, alpha, 8);
    let committed = core
        .view()
        .browse
        .as_ref()
        .unwrap()
        .selection
        .as_ref()
        .unwrap();
    assert_eq!(committed.repo_id, alpha);
    assert!(core.cache().is_pinned(&CacheKey::Tree {
        tree: TreeKey {
            repo_id: alpha,
            root: tree_oid(8)
        }
    }));
    assert!(core.view().review.as_ref().unwrap().open_file.is_none());
    open_file(&mut core, alpha);
    assert_eq!(open_render(&core).repo_id, alpha);
    let pending = select(&mut core, beta, "develop");
    core.handle(Input::User(Action::ResetBrowse)).unwrap();
    answer(&mut core, pending, beta, 9);
    assert!(core.view().browse.as_ref().unwrap().selection.is_none());
    assert!(core.view().browse.as_ref().unwrap().attempt.is_none());
    assert_eq!(core.view().tree.roots.len(), 2);
    assert!(!core.cache().is_pinned(&CacheKey::Tree {
        tree: TreeKey {
            repo_id: alpha,
            root: tree_oid(8)
        }
    }));
}
#[test]
fn picker_uses_selected_repo_and_never_mutates_review_targets_even_for_arbitrary_commit() {
    let [alpha, beta] = repos();
    let mut core = ready();
    let original = core.view().review.as_ref().unwrap().snapshot.review.clone();
    for (repo_id, name) in [(alpha, "main"), (beta, "develop")] {
        let id = picker(&mut core, repo_id);
        catalog(&mut core, id, repo_id, name);
        assert_eq!(
            core.view().ref_selector.as_ref().unwrap().purpose,
            RefSelectorPurpose::Browse
        );
        core.handle(Input::User(Action::RefSelectorQuery { query: name.into() }))
            .unwrap();
        let effects = core.handle(Input::User(Action::SelectCurrentRef)).unwrap();
        assert_eq!(
            requests(&effects),
            vec![(
                requests(&effects)[0].0,
                Request::TreeSnapshot {
                    repo_id,
                    ref_spec: RefSpec::Branch { name: name.into() }
                }
            )]
        );
        fail(&mut core, requests(&effects)[0].0);
        assert!(matches!(
            core.view().ref_selector.as_ref().unwrap().status,
            RefSelectorStatus::InvalidRef { .. }
        ));
        core.handle(Input::User(Action::RefSelectorQuery {
            query: format!("commit:{}", CommitOid::from_bytes([15; 20])),
        }))
        .unwrap();
        let effects = core.handle(Input::User(Action::SelectCurrentRef)).unwrap();
        assert!(matches!(
            requests(&effects)[0].1,
            Request::TreeSnapshot {
                ref_spec: RefSpec::Commit { .. },
                ..
            }
        ));
        answer(&mut core, requests(&effects)[0].0, repo_id, 8);
        assert!(core.view().ref_selector.is_none());
        assert_eq!(
            core.view().review.as_ref().unwrap().snapshot.review,
            original
        );
    }
}
#[test]
fn stale_catalogs_and_errors_cannot_replace_same_repo_reopened_picker() {
    let mut core = ready();
    let repo = repos()[1];
    let stale = picker(&mut core, repo);
    let current = picker(&mut core, repo);
    catalog(&mut core, stale, repo, "obsolete");
    assert_eq!(
        core.view().ref_selector.as_ref().unwrap().status,
        RefSelectorStatus::Loading
    );
    catalog(&mut core, current, repo, "develop");
    let stale = picker(&mut core, repo);
    let current = picker(&mut core, repo);
    assert!(fail(&mut core, stale).is_empty());
    assert!(core.view().last_error.is_none());
    catalog(&mut core, current, repo, "main");
    assert!(
        !core
            .view()
            .ref_selector
            .as_ref()
            .unwrap()
            .options
            .iter()
            .any(|option| option.ref_spec
                == RefSpec::Branch {
                    name: "obsolete".into()
                })
    );
    let pending = select(&mut core, repo, "main");
    core.handle(Input::User(Action::OpenRefSelector {
        repo_id: repo,
        side: RefSelectorSide::Base,
    }))
    .unwrap();
    answer(&mut core, pending, repo, 8);
    assert!(matches!(
        core.view().ref_selector.as_ref().unwrap().purpose,
        RefSelectorPurpose::Review { .. }
    ));
}
#[test]
fn stale_attempts_after_repository_change_new_picker_close_review_or_disconnect_do_not_commit() {
    let [alpha, beta] = repos();
    for next in [
        Action::SelectBrowseRepo { repo_id: alpha },
        Action::OpenBrowseRefSelector { repo_id: alpha },
        Action::CloseReview,
    ] {
        let mut core = ready();
        let pending = select(&mut core, beta, "develop");
        core.handle(Input::User(next)).unwrap();
        answer(&mut core, pending, beta, 9);
        assert!(
            core.view()
                .browse
                .as_ref()
                .is_none_or(|browse| browse.selection.is_none())
        );
    }
    let mut core = ready();
    let id = picker(&mut core, beta);
    catalog(&mut core, id, beta, "develop");
    let pending = select(&mut core, beta, "develop");
    core.handle(Input::User(Action::CloseRefSelector)).unwrap();
    assert!(fail(&mut core, pending).is_empty());
    let pending = select(&mut core, beta, "develop");
    core.handle(Input::Transport(TransportEvent::Disconnected))
        .unwrap();
    assert!(matches!(
        core.view()
            .browse
            .as_ref()
            .unwrap()
            .attempt
            .as_ref()
            .unwrap()
            .status,
        BrowseStatus::Failed { .. }
    ));
    // Transport cancellation forgets its request; it cannot be applied on reconnect.
    assert!(
        core.handle(Input::Server(ServerMsg::Response {
            id: pending,
            response: Response::TreeSnapshot {
                snapshot: scoped_tree(beta, 9, &["same.txt"])
            }
        }))
        .is_err()
    );
    assert!(core.view().browse.as_ref().unwrap().selection.is_none());
}
#[test]
fn resolving_candidate_cannot_replace_a_file_under_a_new_comment_draft() {
    let mut core = browse_ready(RefSpec::WorkingTree);
    let visible = open_render(&core);
    let pending = select(&mut core, repo_id(), "main");
    core.handle(Input::User(Action::CommentFile {
        file: file_ref("unchanged.rs"),
    }))
    .unwrap();
    let draft = core.view().draft.clone();
    answer(&mut core, pending, repo_id(), 8);
    assert_eq!(core.view().draft, draft);
    assert_eq!(open_render(&core), visible);
    assert!(matches!(
        core.view()
            .browse
            .as_ref()
            .unwrap()
            .attempt
            .as_ref()
            .unwrap()
            .status,
        BrowseStatus::Failed { .. }
    ));
}
#[test]
fn candidate_completions_do_not_release_an_unrelated_budgeted_content_slot() {
    let mut core = subscribed(CacheConfig {
        max_in_flight: 1,
        ..local()
    });
    let opened = open_streamed(&mut core);
    daemon_answers(&mut core, &opened);
    core.handle(Input::User(Action::Viewport {
        file: file_ref("a.rs"),
        first_row: 500,
        last_row: 600,
    }))
    .unwrap();
    assert_eq!(core.content_in_flight(), 1);
    assert!(core.content_queued() > 0);
    let queued = core.content_queued();
    let stale = select(&mut core, repo_id(), "old");
    let current = select(&mut core, repo_id(), "new");
    assert!(answer(&mut core, stale, repo_id(), 8).is_empty());
    let effects = fail(&mut core, current);
    assert!(requests(&effects).is_empty());
    assert_eq!(core.content_in_flight(), 1);
    assert_eq!(core.content_queued(), queued);
    let current = select(&mut core, repo_id(), "main");
    let effects = answer(&mut core, current, repo_id(), 9);
    assert!(requests(&effects).is_empty());
    assert_eq!(core.content_in_flight(), 1);
}
