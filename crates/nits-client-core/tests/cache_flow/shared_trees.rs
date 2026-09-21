//! Repository identity must survive shared content, every cache tier and ref changes.
use super::*;
use nits_client_core::{SearchKind, Tab, TreeNode};
use nits_protocol::{Anchor, Mutation};

fn repos() -> [RepoId; 2] {
    [repo_id(), RepoId::from_parts(2, 3)]
}

fn shared_snapshot(base: u8, head: u8) -> ReviewSnapshot {
    let mut snapshot = snapshot(base, head);
    snapshot.review.targets = NonEmpty::new(
        repos()
            .map(|repo_id| ReviewTarget {
                repo_id,
                base: RefSpec::Head,
                head: RefSpec::WorkingTree,
            })
            .to_vec(),
    )
    .unwrap();
    snapshot.resolved = Some(
        NonEmpty::new(
            repos()
                .map(|repo_id| ResolvedTarget {
                    repo_id,
                    ..resolved(base, head).iter().next().unwrap().clone()
                })
                .to_vec(),
        )
        .unwrap(),
    );
    snapshot
}

fn scoped_tree(repo_id: RepoId, root: u8, files: &[&str]) -> TreeSnapshot {
    TreeSnapshot {
        repo_id,
        ..tree(root, files)
    }
}

fn scoped_key(repo_id: RepoId, root: u8) -> CacheKey {
    CacheKey::Tree {
        tree: TreeKey {
            repo_id,
            root: tree_oid(root),
        },
    }
}

fn open_shared(core: &mut ClientCore, order: [RepoId; 2], changed: bool) {
    let effects = core
        .handle(Input::User(Action::OpenReview {
            review_id: review_id(),
        }))
        .unwrap();
    let id = requests(&effects)[0].0;
    item(
        core,
        id,
        StreamItem::ReviewSnapshot {
            snapshot: shared_snapshot(if changed { 1 } else { 2 }, 2),
        },
    );
    for repo_id in order {
        if changed {
            item(
                core,
                id,
                StreamItem::TreeSnapshot {
                    snapshot: scoped_tree(repo_id, 1, &[]),
                },
            );
        }
        item(
            core,
            id,
            StreamItem::TreeSnapshot {
                snapshot: scoped_tree(repo_id, 2, &["same.txt"]),
            },
        );
        if changed {
            item(
                core,
                id,
                StreamItem::Header {
                    header: FileRenderHeader {
                        repo_id,
                        target: RenderTarget::Diff {
                            change: ChangeKind::Added {
                                new: nits_protocol::BlobEntry {
                                    oid: blob_oid(1),
                                    mode: nits_protocol::BlobMode::Regular,
                                },
                            },
                        },
                        content: RenderContent::Binary,
                        ..header("same.txt", 1, 1)
                    },
                },
            );
        }
    }
    core.handle(Input::Server(ServerMsg::StreamEnd { id }))
        .unwrap();
    let effects = core
        .handle(Input::User(Action::SetTab { tab: Tab::Browse }))
        .unwrap();
    finish_blob_requests(core, &effects);
}

fn finish_blob_requests(core: &mut ClientCore, effects: &[Effect]) {
    for (id, request) in requests(effects) {
        if let Request::BlobRender {
            repo_id,
            path: file_path,
            entry,
            first_chunk,
        } = request
        {
            assert_eq!(file_path, path("same.txt"));
            assert_eq!(
                entry,
                nits_protocol::BlobEntry {
                    oid: blob_oid(1),
                    mode: nits_protocol::BlobMode::Regular
                }
            );
            assert_eq!(first_chunk, ChunkIndex::FIRST);
            item(
                core,
                id,
                StreamItem::Header {
                    header: FileRenderHeader {
                        repo_id,
                        target: RenderTarget::Blob { entry },
                        content: RenderContent::Binary,
                        ..header("same.txt", 1, 1)
                    },
                },
            );
            core.handle(Input::Server(ServerMsg::StreamEnd { id }))
                .unwrap();
        }
    }
}

fn tree_files(core: &ClientCore) -> Vec<FileRef> {
    core.view()
        .tree
        .roots
        .iter()
        .flat_map(|root| {
            let TreeNode::Dir {
                repo_id, children, ..
            } = root
            else {
                panic!("repository root")
            };
            children.iter().map(move |child| {
                let TreeNode::File {
                    repo_id: child_repo,
                    path,
                    ..
                } = child
                else {
                    panic!("flat fixture")
                };
                assert_eq!(repo_id, child_repo);
                FileRef {
                    repo_id: *repo_id,
                    path: path.clone(),
                }
            })
        })
        .collect()
}

fn both_files(name: &str) -> Vec<FileRef> {
    repos()
        .map(|repo_id| FileRef {
            repo_id,
            path: path(name),
        })
        .to_vec()
}

#[test]
fn shared_roots_keep_both_repositories_in_browse_search_and_actions() {
    for order in [repos(), [repos()[1], repos()[0]]] {
        let mut core = subscribed(local());
        open_shared(&mut core, order, true);
        assert_eq!(tree_files(&core), both_files("same.txt"));
        assert_eq!(core.view().review.as_ref().unwrap().trees.len(), 4);
        core.handle(Input::User(Action::FileSearch {
            query: Some("same".into()),
        }))
        .unwrap();
        assert_eq!(
            core.view()
                .tree
                .search
                .as_ref()
                .unwrap()
                .hits
                .iter()
                .map(|hit| hit.file.clone())
                .collect::<Vec<_>>(),
            order.map(|repo_id| FileRef {
                repo_id,
                path: path("same.txt")
            })
        );
        for (index, file) in order
            .map(|repo_id| FileRef {
                repo_id,
                path: path("same.txt"),
            })
            .into_iter()
            .enumerate()
        {
            core.handle(Input::User(Action::SearchFirst {
                search: SearchKind::Files,
            }))
            .unwrap();
            if index == 1 {
                core.handle(Input::User(Action::SearchStep {
                    search: SearchKind::Files,
                    delta: 1,
                }))
                .unwrap();
            }
            let effects = core
                .handle(Input::User(Action::OpenSearchResult {
                    search: SearchKind::Files,
                    query: "same".into(),
                }))
                .unwrap();
            assert!(
                requests(&effects)
                    .iter()
                    .all(|(_, request)| matches!(request,
                Request::BlobRender { repo_id, .. } if *repo_id == file.repo_id))
            );
            finish_blob_requests(&mut core, &effects);
            assert!(core.cache().contains(&CacheKey::Header {
                render: open_render(&core)
            }));
            assert_eq!(open_render(&core).repo_id, file.repo_id);
            core.handle(Input::User(Action::CommentFile { file: file.clone() }))
                .unwrap();
            assert!(
                matches!(&core.view().draft.as_ref().unwrap().anchor, Anchor::File { repo_id, path: p, .. } if *repo_id == file.repo_id && *p == file.path)
            );
            let effects = core
                .handle(Input::User(Action::DraftSubmitted {
                    body: "repository-scoped note".into(),
                }))
                .unwrap();
            assert!(requests(&effects).iter().any(|(_, r)| matches!(r, Request::Mutate { mutation: Mutation::AddComment { anchor: Anchor::File { repo_id, .. }, .. }, .. } if *repo_id == file.repo_id)));
            let effects = core
                .handle(Input::User(Action::MarkViewed { file: file.clone() }))
                .unwrap();
            assert!(requests(&effects).iter().any(|(_, r)| matches!(r, Request::Mutate { mutation: Mutation::MarkViewed { repo_id, path: p, .. }, .. } if *repo_id == file.repo_id && *p == file.path)));
            // Opening a result closes search; repeat the same query for the next repository.
            core.handle(Input::User(Action::FileSearch {
                query: Some("same".into()),
            }))
            .unwrap();
        }
        let marks = &core.view().review.as_ref().unwrap().snapshot.viewed;
        assert_eq!(marks.iter().map(|m| m.repo_id).collect::<Vec<_>>(), order);
    }
}

fn open_piecewise_shared(core: &mut ClientCore) -> Vec<Effect> {
    let effects = core
        .handle(Input::User(Action::OpenReview {
            review_id: review_id(),
        }))
        .unwrap();
    core.handle(Input::Server(ServerMsg::Response {
        id: requests(&effects)[0].0,
        response: Response::ReviewSnapshot {
            snapshot: shared_snapshot(2, 2),
        },
    }))
    .unwrap()
}

#[test]
fn shared_root_piecewise_fetches_deduplicate_only_within_the_same_repository() {
    let mut core = subscribed(remote(Bytes::mib(1), Bytes::mib(10)));
    let mut kv = Kv::default();
    let effects = open_piecewise_shared(&mut core);
    assert_eq!(
        loads(&effects),
        repos().map(|r| scoped_key(r, 2).storage_key())
    );
    let effects = kv.drive(&mut core, effects);
    let fetches: Vec<_> = requests(&effects)
        .into_iter()
        .filter(|(_, r)| matches!(r, Request::TreeSnapshot { .. }))
        .collect();
    assert_eq!(fetches.len(), 2, "base=head deduplicates per repository");
    assert_eq!(core.content_in_flight(), 2);
    for (id, request) in fetches.into_iter().rev() {
        let Request::TreeSnapshot { repo_id, .. } = request else {
            unreachable!()
        };
        let effects = core
            .handle(Input::Server(ServerMsg::Response {
                id,
                response: Response::TreeSnapshot {
                    snapshot: scoped_tree(repo_id, 2, &["same.txt"]),
                },
            }))
            .unwrap();
        kv.drive(&mut core, effects);
    }
    assert_eq!(core.content_in_flight(), 0);
    core.handle(Input::User(Action::SetTab { tab: Tab::Browse }))
        .unwrap();
    assert_eq!(tree_files(&core), both_files("same.txt"));
    assert_eq!(kv.map.len(), 2);
    // A fresh client restores both identities with no tree request to the daemon.
    let mut restarted = subscribed(remote(Bytes::mib(1), Bytes::mib(10)));
    let effects = open_piecewise_shared(&mut restarted);
    let restored = kv.drive(&mut restarted, effects);
    assert!(
        !requests(&restored)
            .iter()
            .any(|(_, r)| matches!(r, Request::TreeSnapshot { .. }))
    );
    restarted
        .handle(Input::User(Action::SetTab { tab: Tab::Browse }))
        .unwrap();
    assert_eq!(tree_files(&restarted), both_files("same.txt"));
    // Reconnect replaces transport requests, but neither cached identity is lost.
    restarted
        .handle(Input::Transport(TransportEvent::Disconnected))
        .unwrap();
    restarted.handle(Input::User(Action::Connect)).unwrap();
    restarted
        .handle(Input::Transport(TransportEvent::Connected))
        .unwrap();
    let effects = restarted.handle(Input::Server(welcome())).unwrap();
    let id = requests(&effects)[0].0;
    restarted
        .handle(Input::Server(ServerMsg::Response {
            id,
            response: Response::Subscribed { seq: Seq::new(1) },
        }))
        .unwrap();
    let effects = open_piecewise_shared(&mut restarted);
    assert!(loads(&effects).is_empty());
    assert!(
        !requests(&effects)
            .iter()
            .any(|(_, r)| matches!(r, Request::TreeSnapshot { .. }))
    );
    assert_eq!(tree_files(&restarted), both_files("same.txt"));
}

#[test]
fn legacy_keys_and_wrong_repository_disk_values_cannot_fill_shared_roots() {
    let mut core = subscribed(remote(Bytes::mib(1), Bytes::mib(10)));
    let mut kv = Kv::default();
    let legacy = serde_json::json!({ "type": "Tree", "root": tree_oid(2) }).to_string();
    assert!(serde_json::from_str::<CacheKey>(&legacy).is_err());
    let value = CacheValue::Tree {
        snapshot: scoped_tree(repos()[1], 2, &["same.txt"]),
    };
    kv.map.insert(legacy.clone(), value.encode());
    kv.map
        .insert(scoped_key(repos()[0], 2).storage_key(), value.encode());
    kv.map
        .insert(scoped_key(repos()[1], 2).storage_key(), value.encode());
    let effects = open_piecewise_shared(&mut core);
    let effects = kv.drive(&mut core, effects);
    let tree_requests: Vec<_> = requests(&effects)
        .into_iter()
        .filter_map(|(_, r)| match r {
            Request::TreeSnapshot { repo_id, .. } => Some(repo_id),
            _ => None,
        })
        .collect();
    assert_eq!(tree_requests, vec![repos()[0]]);
    assert!(!core.cache().contains(&scoped_key(repos()[0], 2)));
    assert!(core.cache().contains(&scoped_key(repos()[1], 2)));
    assert!(removes(&effects).contains(&scoped_key(repos()[0], 2).storage_key()));
    assert!(!loads(&effects).contains(&legacy));
}

#[test]
fn tree_response_must_match_repository_as_well_as_root() {
    let mut core = subscribed(remote(Bytes::mib(1), Bytes::mib(10)));
    let effects = open_piecewise_shared(&mut core);
    let effects = Kv::default().drive(&mut core, effects);
    let (id, _) = requests(&effects)
        .into_iter()
        .find(|(_, r)| matches!(r, Request::TreeSnapshot { repo_id, .. } if *repo_id == repos()[0]))
        .unwrap();
    assert!(matches!(
        core.handle(Input::Server(ServerMsg::Response {
            id,
            response: Response::TreeSnapshot {
                snapshot: scoped_tree(repos()[1], 2, &["same.txt"])
            },
        })),
        Err(CoreError::UnexpectedResponse { .. })
    ));
    assert!(!core.cache().contains(&scoped_key(repos()[0], 2)));
}

#[test]
fn shared_root_delta_preserves_other_repositories_and_pinned_historical_refs() {
    let mut core = subscribed(local());
    open_shared(&mut core, repos(), false);
    let delta = TreeDelta {
        repo_id: repos()[0],
        from_root: tree_oid(2),
        to_root: tree_oid(9),
        added: scoped_tree(repos()[0], 9, &["alpha.txt"]).entries,
        removed: vec![path("same.txt")],
        changed: vec![],
    };
    // The root exists, but not for this unrelated repository.
    core.handle(Input::Server(ServerMsg::TreeDelta {
        delta: TreeDelta {
            repo_id: RepoId::from_parts(8, 8),
            ..delta.clone()
        },
    }))
    .unwrap();
    assert!(
        !core
            .cache()
            .contains(&scoped_key(RepoId::from_parts(8, 8), 9))
    );
    core.handle(Input::Server(ServerMsg::TreeDelta { delta }))
        .unwrap();
    assert_eq!(
        tree_files(&core),
        both_files("same.txt"),
        "targets still reference the immutable old root"
    );
    for repo_id in repos() {
        assert!(core.cache().is_pinned(&scoped_key(repo_id, 2)));
    }
    let mut targets = shared_snapshot(2, 2)
        .resolved
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>();
    targets[0].head = ResolvedRef {
        tree: tree_oid(9),
        source: ResolvedSource::Commit {
            oid: CommitOid::from_bytes([9; 20]),
        },
    };
    let effects = core
        .handle(Input::Server(ServerMsg::Event {
            event: Event {
                seq: Seq::new(2),
                ts: Timestamp::from_millis(0),
                author: config(local()).author,
                client_id: ClientId::from_parts(9, 9),
                client_seq: ClientSeq::new(1),
                body: EventBody::ReviewTargetsResolved {
                    review_id: review_id(),
                    targets: NonEmpty::new(targets).unwrap(),
                },
            },
        }))
        .unwrap();
    assert!(
        !requests(&effects)
            .iter()
            .any(|(_, r)| matches!(r, Request::TreeSnapshot { .. })),
        "delta supplied the new snapshot"
    );
    assert_eq!(
        tree_files(&core),
        vec![
            FileRef {
                repo_id: repos()[0],
                path: path("alpha.txt")
            },
            FileRef {
                repo_id: repos()[1],
                path: path("same.txt")
            }
        ]
    );
    for key in [
        scoped_key(repos()[0], 2),
        scoped_key(repos()[0], 9),
        scoped_key(repos()[1], 2),
    ] {
        assert!(core.cache().is_pinned(&key));
    }
    // Browse the historical shared root after divergence; Alpha must keep its identity.
    let effects = core
        .handle(Input::User(Action::SetBrowseRef {
            repo_id: repos()[0],
            ref_spec: Some(RefSpec::Tag {
                name: "before".into(),
            }),
        }))
        .unwrap();
    core.handle(Input::Server(ServerMsg::Response {
        id: requests(&effects)[0].0,
        response: Response::TreeSnapshot {
            snapshot: scoped_tree(repos()[0], 2, &["same.txt"]),
        },
    }))
    .unwrap();
    assert_eq!(tree_files(&core), vec![both_files("same.txt")[0].clone()]);
}

#[test]
fn custom_browse_shared_root_retains_selected_repository_and_ignores_old_response() {
    let mut core = subscribed(local());
    open_shared(&mut core, repos(), false);
    let mut ids = Vec::new();
    for repo_id in repos() {
        let effects = core
            .handle(Input::User(Action::SetBrowseRef {
                repo_id,
                ref_spec: Some(RefSpec::Tag {
                    name: "shared".into(),
                }),
            }))
            .unwrap();
        ids.push(requests(&effects)[0].0);
    }
    core.handle(Input::Server(ServerMsg::Response {
        id: ids[1],
        response: Response::TreeSnapshot {
            snapshot: scoped_tree(repos()[1], 2, &["same.txt"]),
        },
    }))
    .unwrap();
    core.handle(Input::Server(ServerMsg::Response {
        id: ids[0],
        response: Response::TreeSnapshot {
            snapshot: scoped_tree(repos()[0], 2, &["same.txt"]),
        },
    }))
    .unwrap();
    assert_eq!(tree_files(&core), vec![both_files("same.txt")[1].clone()]);
    core.handle(Input::User(Action::SetBrowseRef {
        repo_id: repos()[1],
        ref_spec: None,
    }))
    .unwrap();
    assert_eq!(tree_files(&core), both_files("same.txt"));
}

#[test]
fn target_refresh_keeps_custom_shared_root_pinned_after_both_heads_diverge() {
    let mut core = subscribed(local());
    open_shared(&mut core, repos(), true);
    let effects = core
        .handle(Input::User(Action::SetBrowseRef {
            repo_id: repos()[0],
            ref_spec: Some(RefSpec::Tag {
                name: "pinned".into(),
            }),
        }))
        .unwrap();
    core.handle(Input::Server(ServerMsg::Response {
        id: requests(&effects)[0].0,
        response: Response::TreeSnapshot {
            snapshot: scoped_tree(repos()[0], 2, &["same.txt"]),
        },
    }))
    .unwrap();
    core.handle(Input::Server(ServerMsg::Event {
        event: Event {
            seq: Seq::new(2),
            ts: Timestamp::from_millis(0),
            author: config(local()).author,
            client_id: ClientId::from_parts(9, 9),
            client_seq: ClientSeq::new(1),
            body: EventBody::ReviewTargetsResolved {
                review_id: review_id(),
                targets: shared_snapshot(1, 9).resolved.unwrap(),
            },
        },
    }))
    .unwrap();
    assert!(
        core.cache().is_pinned(&scoped_key(repos()[0], 2)),
        "the selected ref still needs Alpha's old root"
    );
    assert!(
        !core.cache().is_pinned(&scoped_key(repos()[1], 2)),
        "Beta no longer uses this root"
    );
    assert_eq!(tree_files(&core), vec![both_files("same.txt")[0].clone()]);
}

#[test]
fn content_search_hits_on_shared_trees_open_the_selected_repository() {
    let mut core = subscribed(local());
    open_shared(&mut core, repos(), false);
    let effects = core
        .handle(Input::User(Action::ContentSearch {
            query: Some("identical".into()),
            all_files: true,
        }))
        .unwrap();
    let (id, request) = requests(&effects)[0].clone();
    assert_eq!(
        request,
        Request::Search {
            review_id: review_id(),
            query: "identical".into(),
            all_files: true,
            scope: DiffScope::All
        }
    );
    core.handle(Input::Server(ServerMsg::Response {
        id,
        response: Response::Search {
            hits: repos()
                .map(|repo_id| nits_protocol::ContentHit {
                    repo_id,
                    path: path("same.txt"),
                    line: nits_protocol::LineNo::new(1).unwrap(),
                    text: "identical content".into(),
                })
                .to_vec(),
            truncated: false,
        },
    }))
    .unwrap();
    assert_eq!(core.view().content_search.as_ref().unwrap().hits.len(), 2);
    core.handle(Input::User(Action::SearchFirst {
        search: SearchKind::Content,
    }))
    .unwrap();
    core.handle(Input::User(Action::SearchStep {
        search: SearchKind::Content,
        delta: 1,
    }))
    .unwrap();
    let effects = core
        .handle(Input::User(Action::OpenSearchResult {
            search: SearchKind::Content,
            query: "identical".into(),
        }))
        .unwrap();
    assert_eq!(
        requests(&effects)
            .iter()
            .filter_map(|(_, r)| match r {
                Request::BlobRender { repo_id, .. } => Some(*repo_id),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![repos()[1]]
    );
    finish_blob_requests(&mut core, &effects);
    assert_eq!(open_render(&core).repo_id, repos()[1]);
}
