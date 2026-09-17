//! File watcher (plan 2.3): debounced bursts, no-op edits, tree deltas.

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
    let dir = tempfile::tempdir().unwrap();
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
            path: repo.path().to_str().unwrap().into(),
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
                base: RefSpec::Head,
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
    // Let the watcher take its baseline snapshot of the attached repo.
    tokio::time::sleep(Duration::from_millis(300)).await;
    Harness {
        _dir: dir,
        _watcher: watcher,
        shutdown,
        repo,
        client,
    }
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
        tokio::time::sleep(Duration::from_millis(10)).await;
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
