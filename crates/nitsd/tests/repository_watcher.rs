//! Watch registrations follow repaired global checkout ownership.
use std::sync::Arc;
use std::time::Duration;

use nits_protocol::{
    Author, BuildInfo, ClientId, ClientSeq, EventBody, Mutation, NonEmpty, RefSpec, Repo, RepoId,
    Request, Response, ReviewId, ReviewTarget, Since, SubscribeScope, Timestamp, WorkspaceId,
};
use nits_review_core::store::{NewEvent, Store};
use nits_review_core::{Core, Ctx, DataDir};
use nits_test_support::{RepoBuilder, TestRepo, files};
use nitsd::client::{Client, Identity, Unsolicited};
use nitsd::server::UnixServer;
use nitsd::watcher::Watcher;
use tokio_util::sync::CancellationToken;

fn context() -> Ctx {
    Ctx {
        author: Author::Human {
            name: "ada".into(),
            machine: "test".into(),
        },
        client_id: ClientId::from_parts(1, 1),
        client_seq: ClientSeq::new(1),
        now: Timestamp::from_millis(1_700_000_000_000),
    }
}
fn workspace(n: u128) -> WorkspaceId {
    WorkspaceId::from_parts(1, n)
}
fn review(n: u128) -> ReviewId {
    ReviewId::from_parts(1, n)
}
fn shared() -> RepoId {
    RepoId::from_parts(1, 9)
}
fn healthy() -> RepoId {
    RepoId::from_parts(1, 20)
}
fn repository(file: &str) -> TestRepo {
    RepoBuilder::new()
        .commit("initial", files![file => "source\n"])
        .build()
        .unwrap()
}

fn seed(data: &DataDir, alpha: &TestRepo, beta: &TestRepo, gamma: &TestRepo) {
    let core = Core::open(data).unwrap();
    let ctx = context();
    for n in 1..=3 {
        core.create_workspace(&ctx, workspace(n), format!("workspace {n}"))
            .unwrap();
        let (id, path) = if n == 3 {
            (healthy(), gamma.path())
        } else {
            (shared(), alpha.path())
        };
        core.attach_repo(
            &ctx,
            workspace(n),
            id,
            path.to_str().unwrap(),
            "repo".into(),
        )
        .unwrap();
        core.create_review(
            &ctx,
            review(n),
            workspace(n),
            "review".into(),
            NonEmpty::singleton(ReviewTarget {
                repo_id: id,
                base: RefSpec::Head,
                head: RefSpec::WorkingTree,
            }),
        )
        .unwrap();
    }
    drop(core);
    // Authentic pre-fix history could overwrite one membership with a different
    // checkout while preserving the same global ID.
    Store::open(&data.state())
        .unwrap()
        .append(NewEvent {
            ts: ctx.now,
            author: ctx.author,
            client_id: ctx.client_id,
            client_seq: ctx.client_seq,
            body: EventBody::RepoAttached {
                workspace_id: workspace(2),
                repo: Repo {
                    id: shared(),
                    path: beta.path().to_str().unwrap().into(),
                    display_name: "legacy".into(),
                },
            },
        })
        .unwrap();
}

struct Harness {
    watcher: Watcher,
    shutdown: CancellationToken,
    client: Client,
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.watcher.stop();
        self.shutdown.cancel();
    }
}
async fn start(data: &DataDir, socket: &std::path::Path) -> Harness {
    let build = BuildInfo {
        name: "watcher-identity-test".into(),
        version: "0".into(),
    };
    let daemon = nitsd::Daemon::open(data, build.clone()).unwrap();
    let server = UnixServer::bind(socket).unwrap();
    let shutdown = CancellationToken::new();
    tokio::spawn(server.run(Arc::clone(&daemon), shutdown.clone()));
    let watcher = Watcher::start(daemon);
    let ctx = context();
    let client = Client::connect_unix(
        socket,
        Identity {
            client_id: ctx.client_id,
            client: build,
            author: ctx.author,
        },
    )
    .await
    .unwrap();
    client
        .request(Request::Subscribe {
            scope: SubscribeScope::All,
            since: Since::Now,
        })
        .await
        .unwrap();
    Harness {
        watcher,
        shutdown,
        client,
    }
}
async fn resolution(client: &Client, id: ReviewId) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let message = client.next_unsolicited().await.expect("live watcher connection");
            if let Unsolicited::Event(event) = message
                && matches!(event.body, EventBody::ReviewTargetsResolved { review_id, .. } if review_id == id) { return; }
        }
    }).await.expect("automatic review resolution");
}
async fn fence(client: &Client, gamma: &TestRepo, content: &str) {
    std::fs::write(gamma.path().join("ready.txt"), content).unwrap();
    resolution(client, review(3)).await;
    while tokio::time::timeout(Duration::from_millis(250), client.next_unsolicited())
        .await
        .is_ok()
    {}
}

#[tokio::test]
async fn legacy_repair_watches_the_surviving_checkout_in_either_order() {
    for detached in [1, 2] {
        let dir = tempfile::tempdir().unwrap();
        let data = DataDir::new(dir.path());
        let alpha = repository("alpha.txt");
        let beta = repository("beta.txt");
        let gamma = repository("gamma.txt");
        seed(&data, &alpha, &beta, &gamma);
        let h = start(&data, &dir.path().join("watch.sock")).await;
        // Positive independent watcher activity fences startup and repair; no
        // startup delay or event ordering assumption decides the test outcome.
        fence(&h.client, &gamma, "startup ready\n").await;
        let response = h
            .client
            .request(Request::Mutate {
                client_seq: ClientSeq::new(2),
                mutation: Mutation::DetachRepo {
                    workspace_id: workspace(detached),
                    repo_id: shared(),
                },
            })
            .await
            .unwrap();
        assert!(matches!(response, Response::Committed { .. }));
        let survivor = if detached == 1 { 2 } else { 1 };
        h.client
            .request(Request::ResolveTargets {
                review_id: review(survivor),
            })
            .await
            .unwrap();
        fence(&h.client, &gamma, "repair ready\n").await;
        let (path, file) = if survivor == 2 {
            (beta.path(), "beta.txt")
        } else {
            (alpha.path(), "alpha.txt")
        };
        std::fs::write(path.join(file), "changed surviving checkout\n").unwrap();
        resolution(&h.client, review(survivor)).await;
        let files = h
            .client
            .request(Request::ListFiles {
                review_id: review(survivor),
                scope: nits_protocol::DiffScope::All,
            })
            .await
            .unwrap();
        let Response::Files { files, .. } = files else {
            panic!("file listing")
        };
        assert!(files.iter().any(|change| change.path.as_str() == file));
        let detached_path = if detached == 1 {
            alpha.path()
        } else {
            beta.path()
        };
        std::fs::write(detached_path.join("detached.txt"), "unrelated edit\n").unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(500), h.client.next_unsolicited())
                .await
                .is_err(),
            "detached checkout must not publish review updates"
        );
        // Reattaching the same checkout under the shared ID must preserve
        // automatic refresh for both valid memberships.
        h.client
            .request(Request::Mutate {
                client_seq: ClientSeq::new(3),
                mutation: Mutation::AttachRepo {
                    workspace_id: workspace(detached),
                    repo_id: shared(),
                    path: path.to_str().unwrap().into(),
                    display_name: "repaired".into(),
                },
            })
            .await
            .unwrap();
        fence(&h.client, &gamma, "shared membership ready\n").await;
        std::fs::write(path.join(file), "shared checkout edit\n").unwrap();
        resolution(&h.client, review(survivor)).await;
    }
}
