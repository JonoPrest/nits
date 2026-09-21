//! File watcher (plan 2.3): debounced bursts, no-op edits, tree deltas.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use nits_protocol::{
    Author, BuildInfo, ClientId, ClientSeq, EventBody, Mutation, NonEmpty, RefSpec, RepoId,
    Request, Response, ReviewId, ReviewStatus, ReviewTarget, Since, SubscribeScope, TreeEntryKind,
    WorkspaceId,
};
use nits_review_core::DataDir;
use nits_test_support::{RepoBuilder, TestRepo, files};
use nitsd::Daemon;
use nitsd::client::{Client, Identity, Unsolicited};
use nitsd::server::UnixServer;
use nitsd::watcher::Watcher;
use tokio_util::sync::CancellationToken;

struct Harness {
    _dir: tempfile::TempDir,
    _watcher: Watcher,
    shutdown: CancellationToken,
    repo: TestRepo,
    checkout: PathBuf,
    client: Client,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

fn ws() -> WorkspaceId {
    WorkspaceId::from_parts(1, 1)
}
fn rid() -> RepoId {
    RepoId::from_parts(1, 1)
}
fn review_id() -> ReviewId {
    ReviewId::from_parts(1, 1)
}

async fn mutate(c: &Client, seq: u64, m: Mutation) {
    let r = c
        .request(Request::Mutate {
            client_seq: ClientSeq::new(seq),
            mutation: m,
        })
        .await
        .unwrap();
    assert!(matches!(r, Response::Committed { .. }));
}

/// Distinct socket names per test in this process; the clock alone collides.
static SOCKET_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn build() -> BuildInfo {
    BuildInfo {
        name: "test".into(),
        version: "0".into(),
    }
}

/// Daemon + watcher + one client subscribed to a HEAD..working-tree review.
async fn start() -> Harness {
    let repo = RepoBuilder::new()
        .commit("base", files!["a.txt" => "a\n", "b.txt" => "b\n"])
        .build()
        .unwrap();
    start_repo(repo, RefSpec::Head, Checkout::Main).await
}

enum Checkout {
    Main,
    Linked,
}

async fn start_repo(repo: TestRepo, base: RefSpec, checkout: Checkout) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let checkout = match checkout {
        Checkout::Main => repo.path().to_path_buf(),
        Checkout::Linked => {
            let path = dir.path().join("checkout");
            repo.git(&["worktree", "add", "-b", "linked", path.to_str().unwrap()])
                .unwrap();
            path
        }
    };
    let socket = std::env::temp_dir().join(format!(
        "nitsw-{}-{}.sock",
        std::process::id(),
        SOCKET_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let daemon = Daemon::open(&DataDir::new(dir.path()), build()).unwrap();
    let server = UnixServer::bind(&socket).unwrap();
    let shutdown = CancellationToken::new();
    tokio::spawn(server.run(Arc::clone(&daemon), shutdown.clone()));
    let watcher = Watcher::start(Arc::clone(&daemon));

    let client = Client::connect_unix(
        &socket,
        Identity {
            client_id: ClientId::from_parts(1, 1),
            client: build(),
            author: Author::Human {
                name: "ada".into(),
                machine: "box".into(),
            },
        },
    )
    .await
    .unwrap();
    mutate(
        &client,
        1,
        Mutation::CreateWorkspace {
            workspace_id: ws(),
            name: "w".into(),
        },
    )
    .await;
    mutate(
        &client,
        2,
        Mutation::AttachRepo {
            workspace_id: ws(),
            repo_id: rid(),
            path: checkout.to_str().unwrap().into(),
            display_name: "r".into(),
        },
    )
    .await;
    mutate(
        &client,
        3,
        Mutation::CreateReview {
            review_id: review_id(),
            workspace_id: ws(),
            title: "wt".into(),
            targets: NonEmpty::new(vec![ReviewTarget {
                repo_id: rid(),
                base,
                head: RefSpec::WorkingTree,
            }])
            .unwrap(),
        },
    )
    .await;
    client
        .request(Request::ResolveTargets {
            review_id: review_id(),
        })
        .await
        .unwrap();
    client
        .request(Request::Subscribe {
            scope: SubscribeScope::Review {
                review_id: review_id(),
            },
            since: Since::Now,
        })
        .await
        .unwrap();
    let h = Harness {
        _dir: dir,
        _watcher: watcher,
        shutdown,
        repo,
        checkout,
        client,
    };
    // Observe an actual round trip rather than guessing when notify has
    // registered under load. Startup reconciliation also catches this write
    // if it lands before the watcher registers.
    std::fs::write(h.checkout.join("watcher-ready"), b"ready\n").unwrap();
    next_resolution(&h.client).await;
    std::fs::remove_file(h.checkout.join("watcher-ready")).unwrap();
    next_resolution(&h.client).await;
    h
}

async fn next_resolution(
    client: &Client,
) -> nits_protocol::NonEmpty<nits_protocol::ResolvedTarget> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(Unsolicited::Event(event)) = client.next_unsolicited().await
                && let EventBody::ReviewTargetsResolved { targets, .. } = event.body
            {
                return targets;
            }
        }
    })
    .await
    .expect("watcher did not publish resolved provenance")
}

/// Everything unsolicited that arrives within `window`.
async fn collect(c: &Client, window: Duration) -> Vec<Unsolicited> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    loop {
        match tokio::time::timeout_at(deadline, c.next_unsolicited()).await {
            Ok(Some(u)) => out.push(u),
            Ok(None) | Err(_) => return out,
        }
    }
}

fn resolved_count(msgs: &[Unsolicited]) -> usize {
    msgs.iter()
        .filter(|u| {
            matches!(
                u,
                Unsolicited::Event(e) if matches!(e.body, EventBody::ReviewTargetsResolved { .. })
            )
        })
        .count()
}

fn resolved_reviews(msgs: &[Unsolicited]) -> Vec<ReviewId> {
    msgs.iter()
        .filter_map(|message| {
            if let Unsolicited::Event(event) = message
                && let EventBody::ReviewTargetsResolved { review_id, .. } = event.body
            {
                Some(review_id)
            } else {
                None
            }
        })
        .collect()
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn archived_review_stays_quiet_while_open_review_updates_and_reopening_catches_up() {
    let h = start().await;
    let archived_id = ReviewId::from_parts(1, 2);
    mutate(
        &h.client,
        4,
        Mutation::CreateReview {
            review_id: archived_id,
            workspace_id: ws(),
            title: "superseded".into(),
            targets: NonEmpty::singleton(ReviewTarget {
                repo_id: rid(),
                base: RefSpec::Head,
                head: RefSpec::WorkingTree,
            }),
        },
    )
    .await;
    mutate(
        &h.client,
        5,
        Mutation::UpdateReview {
            review_id: archived_id,
            title: "superseded".into(),
            status: ReviewStatus::Archived,
        },
    )
    .await;
    let Response::ReviewSnapshot { snapshot: archived } = h
        .client
        .request(Request::ReviewSnapshot {
            review_id: archived_id,
        })
        .await
        .unwrap()
    else {
        panic!("expected snapshot");
    };
    h.client
        .request(Request::Subscribe {
            scope: SubscribeScope::Workspace { workspace_id: ws() },
            since: Since::Now,
        })
        .await
        .unwrap();
    collect(&h.client, Duration::from_millis(100)).await;

    h.repo
        .write_file("a.txt", b"changed while archived\n")
        .unwrap();
    let msgs = collect(&h.client, Duration::from_secs(2)).await;
    assert_eq!(resolved_reviews(&msgs), vec![review_id()], "{msgs:#?}");
    let Response::ReviewSnapshot {
        snapshot: still_archived,
    } = h
        .client
        .request(Request::ReviewSnapshot {
            review_id: archived_id,
        })
        .await
        .unwrap()
    else {
        panic!("expected snapshot");
    };
    assert_eq!(still_archived.review, archived.review);
    assert_eq!(still_archived.resolved, archived.resolved);

    // No additional filesystem event is needed to refresh on reopening.
    let response = h
        .client
        .request(Request::Mutate {
            client_seq: ClientSeq::new(6),
            mutation: Mutation::UpdateReview {
                review_id: archived_id,
                title: "reopened".into(),
                status: ReviewStatus::Open,
            },
        })
        .await
        .unwrap();
    assert!(
        matches!(response, Response::Committed { event } if matches!(event.body, EventBody::ReviewUpdated { status: ReviewStatus::Open, .. }))
    );
    let msgs = collect(&h.client, Duration::from_millis(200)).await;
    assert_eq!(resolved_reviews(&msgs), vec![archived_id], "{msgs:#?}");
    let Response::ReviewSnapshot { snapshot: reopened } = h
        .client
        .request(Request::ReviewSnapshot {
            review_id: archived_id,
        })
        .await
        .unwrap()
    else {
        panic!("expected snapshot");
    };
    assert_eq!(reopened.review.status, ReviewStatus::Open);
    assert_ne!(reopened.resolved, archived.resolved);

    h.repo
        .write_file("b.txt", b"both reviews are now open\n")
        .unwrap();
    let msgs = collect(&h.client, Duration::from_secs(2)).await;
    let mut reviews = resolved_reviews(&msgs);
    reviews.sort();
    assert_eq!(reviews, vec![review_id(), archived_id], "{msgs:#?}");
}

#[tokio::test]
async fn archived_review_subscription_receives_neither_refresh_events_nor_tree_deltas() {
    let h = start().await;
    mutate(
        &h.client,
        4,
        Mutation::UpdateReview {
            review_id: review_id(),
            title: "archived".into(),
            status: ReviewStatus::Archived,
        },
    )
    .await;
    collect(&h.client, Duration::from_millis(100)).await;
    h.repo.write_file("a.txt", b"changed\n").unwrap();
    let msgs = collect(&h.client, Duration::from_secs(2)).await;
    assert!(msgs.is_empty(), "{msgs:#?}");
}

#[tokio::test]
async fn a_burst_of_writes_resolves_once() {
    let h = start().await;
    for i in 0..10 {
        h.repo
            .write_file("a.txt", format!("a{i}\n").as_bytes())
            .unwrap();
        // No yield: this is one burst even if other test processes are busy.
    }
    let msgs = collect(&h.client, Duration::from_secs(2)).await;
    assert_eq!(resolved_count(&msgs), 1, "{msgs:#?}");
    let resolved = msgs
        .iter()
        .find_map(|u| match u {
            Unsolicited::Event(e) => Some(e),
            Unsolicited::TreeDelta(_) | Unsolicited::Error(_) => None,
        })
        .unwrap();
    assert!(matches!(resolved.author, Author::Daemon { .. }));
    let deltas = msgs
        .iter()
        .filter(|u| matches!(u, Unsolicited::TreeDelta(_)))
        .count();
    assert_eq!(deltas, 1, "one delta for the burst");
}

#[tokio::test]
async fn rewriting_identical_content_emits_nothing() {
    let h = start().await;
    h.repo.write_file("a.txt", b"a\n").unwrap();
    h.repo.write_file("b.txt", b"b\n").unwrap();
    let msgs = collect(&h.client, Duration::from_millis(800)).await;
    assert!(msgs.is_empty(), "{msgs:#?}");
}

#[tokio::test]
async fn create_and_delete_produce_single_deltas_with_the_right_entries() {
    let h = start().await;
    h.repo.write_file("new/c.txt", b"c\n").unwrap();
    let msgs = collect(&h.client, Duration::from_secs(2)).await;
    let deltas: Vec<_> = msgs
        .iter()
        .filter_map(|u| match u {
            Unsolicited::TreeDelta(d) => Some(d),
            Unsolicited::Event(_) | Unsolicited::Error(_) => None,
        })
        .collect();
    assert_eq!(deltas.len(), 1, "{msgs:#?}");
    let d = deltas[0];
    assert_eq!(d.repo_id, rid());
    assert_eq!(d.added.len(), 1);
    assert_eq!(d.added[0].path.as_str(), "new/c.txt");
    assert!(matches!(d.added[0].kind, TreeEntryKind::File { .. }));
    assert!(d.removed.is_empty() && d.changed.is_empty());
    assert_eq!(resolved_count(&msgs), 1);

    std::fs::remove_file(h.repo.path().join("b.txt")).unwrap();
    let msgs = collect(&h.client, Duration::from_secs(2)).await;
    let deltas: Vec<_> = msgs
        .iter()
        .filter_map(|u| match u {
            Unsolicited::TreeDelta(d) => Some(d),
            Unsolicited::Event(_) | Unsolicited::Error(_) => None,
        })
        .collect();
    assert_eq!(deltas.len(), 1, "{msgs:#?}");
    let d = deltas[0];
    assert_eq!(d.removed.len(), 1);
    assert_eq!(d.removed[0].as_str(), "b.txt");
    assert!(d.added.is_empty() && d.changed.is_empty());
    assert_eq!(d.from_root, deltas[0].from_root);
}

impl Harness {
    fn git(&self, args: &[&str]) -> String {
        let mut command = vec!["-C", self.checkout.to_str().unwrap()];
        command.extend_from_slice(args);
        self.repo.git(&command).unwrap()
    }

    fn write(&self, path: &Path, content: &[u8]) {
        std::fs::write(self.checkout.join(path), content).unwrap();
    }

    async fn snapshot(&self) -> nits_protocol::ReviewSnapshot {
        let Response::ReviewSnapshot { snapshot } = self
            .client
            .request(Request::ReviewSnapshot {
                review_id: review_id(),
            })
            .await
            .unwrap()
        else {
            panic!("expected review snapshot")
        };
        snapshot
    }

    async fn changed_paths(&self) -> Vec<String> {
        let Response::Files { files, .. } = self
            .client
            .request(Request::ListFiles {
                review_id: review_id(),
                scope: nits_protocol::DiffScope::All,
            })
            .await
            .unwrap()
        else {
            panic!("expected files")
        };
        files
            .into_iter()
            .map(|file| file.path.to_string())
            .collect()
    }

    async fn commits(&self) -> Vec<nits_protocol::CommitInfo> {
        let Response::Commits { commits } = self
            .client
            .request(Request::ListCommits {
                review_id: review_id(),
                repo_id: rid(),
            })
            .await
            .unwrap()
        else {
            panic!("expected commits")
        };
        commits
    }

    async fn refreshed(&self) -> nits_protocol::ResolvedTarget {
        let targets = next_resolution(&self.client).await;
        let snapshot = self.snapshot().await;
        assert_eq!(
            snapshot.resolved.as_ref(),
            Some(&targets),
            "subscribers and new readers converge"
        );
        targets.first().clone()
    }
}

fn source_head(
    target: &nits_protocol::ResolvedTarget,
) -> (
    Vec<String>,
    Option<String>,
    Option<nits_protocol::CommitOid>,
) {
    let nits_protocol::ResolvedSource::WorkingTree {
        dirty,
        branch,
        head,
    } = &target.head.source
    else {
        panic!("expected working-tree provenance")
    };
    (
        dirty.iter().map(ToString::to_string).collect(),
        branch.clone(),
        *head,
    )
}

#[tokio::test]
async fn committing_dirty_content_refreshes_identical_tree_and_moving_head_base() {
    let h = start().await;
    h.write(Path::new("a.txt"), b"changed\n");
    let dirty = h.refreshed().await;
    assert_eq!(source_head(&dirty).0, ["a.txt"]);
    assert_eq!(h.changed_paths().await, ["a.txt"]);
    h.git(&["add", "a.txt"]);
    h.git(&["commit", "-qm", "accept change"]);
    let committed = h.refreshed().await;
    assert_eq!(committed.head.tree, dirty.head.tree);
    assert_ne!(committed.base, dirty.base);
    assert!(source_head(&committed).0.is_empty());
    assert!(h.changed_paths().await.is_empty());
    assert!(h.commits().await.is_empty());
    assert!(
        collect(&h.client, Duration::from_millis(500))
            .await
            .is_empty(),
        "Git/Nits metadata must settle without feedback"
    );
}

#[tokio::test]
async fn same_tree_amend_publishes_commit_provenance_and_archived_commits_stay_pinned() {
    let repo = RepoBuilder::new()
        .commit("base", files!["a.txt" => "a\n"])
        .branch("feature")
        .commit("first", files!["a.txt" => "changed\n"])
        .build()
        .unwrap();
    let h = start_repo(
        repo,
        RefSpec::Branch {
            name: "main".into(),
        },
        Checkout::Main,
    )
    .await;
    let old = h.snapshot().await.resolved.unwrap().first().clone();
    let original_commits = h.commits().await;
    assert_eq!(original_commits.len(), 1);
    h.git(&["commit", "--amend", "-qm", "amended"]);
    let new = h.refreshed().await;
    assert_eq!(old.head.tree, new.head.tree);
    assert_ne!(source_head(&old).2, source_head(&new).2);
    assert_eq!(source_head(&old).0, source_head(&new).0);
    let commits = h.commits().await;
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].subject, "amended");
    assert_ne!(commits[0].oid, original_commits[0].oid);
    mutate(
        &h.client,
        4,
        Mutation::UpdateReview {
            review_id: review_id(),
            title: "captured".into(),
            status: ReviewStatus::Archived,
        },
    )
    .await;
    collect(&h.client, Duration::from_millis(100)).await;
    h.git(&["commit", "--amend", "-qm", "later checkout"]);
    assert!(
        collect(&h.client, Duration::from_millis(500))
            .await
            .is_empty()
    );
    assert_eq!(h.commits().await, commits);
    assert_eq!(h.snapshot().await.resolved.unwrap().first(), &new);
}

#[tokio::test]
async fn base_ref_movement_and_identical_tree_branch_switches_refresh_without_file_events() {
    let repo = RepoBuilder::new()
        .commit("base", files!["a.txt" => "a\n"])
        .branch("feature")
        .commit("second", files!["a.txt" => "b\n"])
        .build()
        .unwrap();
    repo.git(&["pack-refs", "--all", "--prune"]).unwrap();
    let h = start_repo(
        repo,
        RefSpec::Branch {
            name: "main".into(),
        },
        Checkout::Main,
    )
    .await;
    let old = h.snapshot().await.resolved.unwrap().first().clone();
    assert_eq!(h.changed_paths().await, ["a.txt"]);
    assert_eq!(h.commits().await.len(), 1);
    h.git(&["update-ref", "refs/heads/main", "HEAD"]);
    let moved = h.refreshed().await;
    assert_eq!(moved.head, old.head);
    assert_ne!(moved.base, old.base);
    assert!(h.changed_paths().await.is_empty());
    assert!(h.commits().await.is_empty());
    h.git(&["checkout", "-q", "main"]);
    let switched = h.refreshed().await;
    assert_eq!(switched.head.tree, moved.head.tree);
    assert_eq!(source_head(&switched).1.as_deref(), Some("main"));
    h.git(&["checkout", "-q", "--detach"]);
    let detached = h.refreshed().await;
    assert_eq!(detached.head.tree, switched.head.tree);
    assert_eq!(source_head(&detached).1, None);
    assert!(h.changed_paths().await.is_empty());
}

#[tokio::test]
async fn linked_worktree_private_head_and_shared_refs_are_watched_outside_checkout() {
    let repo = RepoBuilder::new()
        .commit("base", files!["a.txt" => "a\n"])
        .build()
        .unwrap();
    let h = start_repo(
        repo,
        RefSpec::Branch {
            name: "main".into(),
        },
        Checkout::Linked,
    )
    .await;
    assert!(h.checkout.join(".git").is_file());
    h.write(Path::new("a.txt"), b"linked edit\n");
    let dirty = h.refreshed().await;
    h.git(&["add", "a.txt"]);
    h.git(&["commit", "-qm", "linked commit"]);
    let committed = h.refreshed().await;
    assert_eq!(committed.head.tree, dirty.head.tree);
    assert!(source_head(&committed).0.is_empty());
    assert_eq!(h.commits().await.len(), 1);
    // This mutation is issued from the other checkout, outside every path
    // under the linked working directory, and changes only the shared refs.
    h.repo
        .git(&["update-ref", "refs/heads/main", "linked"])
        .unwrap();
    let moved = h.refreshed().await;
    assert_eq!(moved.head, committed.head);
    assert!(h.changed_paths().await.is_empty());
    assert!(h.commits().await.is_empty());
    h.git(&["checkout", "-qb", "same-tree"]);
    let switched = h.refreshed().await;
    assert_eq!(source_head(&switched).1.as_deref(), Some("same-tree"));
    assert_eq!(switched.head.tree, moved.head.tree);
    assert!(
        collect(&h.client, Duration::from_millis(500))
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn index_only_change_reconciles_previously_skipped_worktree_content() {
    let h = start().await;
    h.git(&["update-index", "--skip-worktree", "a.txt"]);
    h.write(Path::new("a.txt"), b"outside sparse index\n");
    // Nits snapshots respect Git's skip-worktree bit. The later index change,
    // without another source-file edit, must make the same bytes visible.
    assert!(
        collect(&h.client, Duration::from_millis(500))
            .await
            .is_empty()
    );
    h.git(&["update-index", "--no-skip-worktree", "a.txt"]);
    let target = h.refreshed().await;
    assert_eq!(source_head(&target).0, ["a.txt"]);
    assert_eq!(h.changed_paths().await, ["a.txt"]);
}
