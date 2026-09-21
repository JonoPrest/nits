//! Real RPC clients share global checkout identity and scoped review access.
use std::path::Path;
use std::sync::Arc;

use nits_protocol::{
    Author, BuildInfo, ClientId, ClientSeq, DiffScope, EnsureDirectoryReview, Mutation, NonEmpty,
    RefSpec, RenderOpts, RepoId, RepoPath, Request, Response, ReviewId, ReviewTarget, RpcError,
    Side, StreamItem, WorkspaceId,
};
use nits_review_core::DataDir;
use nits_test_support::{RepoBuilder, files};
use nitsd::Daemon;
use nitsd::client::{Client, ClientError, Identity};
use nitsd::ops::{Ops, OpsError};
use nitsd::server::UnixServer;
use tokio_util::sync::CancellationToken;

fn workspace(n: u128) -> WorkspaceId {
    WorkspaceId::from_parts(1, n)
}
fn repo_id() -> RepoId {
    RepoId::from_parts(1, 9)
}
fn review_id(n: u128) -> ReviewId {
    ReviewId::from_parts(1, n)
}
fn build() -> BuildInfo {
    BuildInfo {
        name: "identity-test".into(),
        version: "0".into(),
    }
}

struct Harness {
    dir: tempfile::TempDir,
    daemon: Arc<Daemon>,
    shutdown: CancellationToken,
    first: Client,
    second: Client,
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}
async fn start() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(&DataDir::new(dir.path()), build()).unwrap();
    let socket = dir.path().join("nits.sock");
    let server = UnixServer::bind(&socket).unwrap();
    let shutdown = CancellationToken::new();
    tokio::spawn(server.run(Arc::clone(&daemon), shutdown.clone()));
    let first = connect(&socket, 1).await;
    let second = connect(&socket, 2).await;
    for n in 1..=2 {
        mutate(
            &first,
            Mutation::CreateWorkspace {
                workspace_id: workspace(n),
                name: format!("workspace {n}"),
            },
        )
        .await
        .unwrap();
    }
    Harness {
        dir,
        daemon,
        shutdown,
        first,
        second,
    }
}
async fn connect(path: &Path, n: u128) -> Client {
    Client::connect_unix(
        path,
        Identity {
            client_id: ClientId::from_parts(1, n),
            client: build(),
            author: Author::Human {
                name: "ada".into(),
                machine: "test".into(),
            },
        },
    )
    .await
    .unwrap()
}
async fn mutate(client: &Client, mutation: Mutation) -> Result<Response, ClientError> {
    client
        .request(Request::Mutate {
            client_seq: ClientSeq::new(1),
            mutation,
        })
        .await
}
fn attachment(n: u128, path: &Path) -> Mutation {
    Mutation::AttachRepo {
        workspace_id: workspace(n),
        repo_id: repo_id(),
        path: path.to_str().unwrap().into(),
        display_name: "repository".into(),
    }
}
async fn tree(client: &Client) -> Vec<String> {
    let Response::TreeSnapshot { snapshot } = client
        .request(Request::TreeSnapshot {
            repo_id: repo_id(),
            ref_spec: RefSpec::Head,
        })
        .await
        .unwrap()
    else {
        panic!("tree response");
    };
    snapshot
        .entries
        .into_iter()
        .map(|entry| entry.path.to_string())
        .collect()
}

#[tokio::test]
async fn competing_rpc_attachments_commit_only_one_checkout_owner() {
    let alpha = RepoBuilder::new()
        .commit("initial", files!["alpha.txt" => "alpha\n"])
        .build()
        .unwrap();
    let beta = RepoBuilder::new()
        .commit("initial", files!["beta.txt" => "beta\n"])
        .build()
        .unwrap();
    let h = start().await;
    let before = h.daemon.core().last_seq().unwrap();
    let results = tokio::join!(
        mutate(&h.first, attachment(1, alpha.path())),
        mutate(&h.second, attachment(2, beta.path()))
    );
    let results = [results.0, results.1];
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Ok(Response::Committed { .. })))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(ClientError::Rpc(RpcError::Invalid { .. }))))
            .count(),
        1
    );
    assert_eq!(h.daemon.core().events_after(before).unwrap().len(), 1);
    let expected = if results[0].is_ok() {
        "alpha.txt"
    } else {
        "beta.txt"
    };
    assert_eq!(tree(&h.first).await, [expected]);
    assert_eq!(tree(&h.second).await, [expected]);
    let before = h.daemon.core().last_seq().unwrap();
    let rejected_path = if results[0].is_ok() {
        beta.path()
    } else {
        alpha.path()
    };
    assert!(matches!(
        h.first
            .request(Request::EnsureDirectoryReview {
                client_seq: ClientSeq::new(2),
                options: EnsureDirectoryReview {
                    workspace_id: workspace(3),
                    repo_id: repo_id(),
                    review_id: review_id(3),
                    path: rejected_path.to_str().unwrap().into(),
                    base: None,
                    head: None,
                }
            })
            .await,
        Err(ClientError::Rpc(RpcError::Invalid { .. }))
    ));
    assert_eq!(h.daemon.core().last_seq().unwrap(), before);
    assert!(h.daemon.core().workspace(workspace(3)).is_err());
    assert_eq!(tree(&h.first).await, [expected]);
}

#[tokio::test]
async fn concurrent_same_checkout_memberships_survive_scoped_detach() {
    let alpha = RepoBuilder::new()
        .commit("initial", files!["alpha.txt" => "alpha\n"])
        .build()
        .unwrap();
    let h = start().await;
    let before = h.daemon.core().last_seq().unwrap();
    let (a, b) = tokio::join!(
        mutate(&h.first, attachment(1, alpha.path())),
        mutate(&h.second, attachment(2, alpha.path()))
    );
    assert!(matches!(a, Ok(Response::Committed { .. })));
    assert!(matches!(b, Ok(Response::Committed { .. })));
    assert_eq!(h.daemon.core().events_after(before).unwrap().len(), 2);
    for n in 1..=2 {
        mutate(
            &h.first,
            Mutation::CreateReview {
                workspace_id: workspace(n),
                review_id: review_id(n),
                title: "review".into(),
                targets: NonEmpty::singleton(ReviewTarget {
                    repo_id: repo_id(),
                    base: RefSpec::Head,
                    head: RefSpec::WorkingTree,
                }),
            },
        )
        .await
        .unwrap();
    }
    mutate(
        &h.first,
        Mutation::DetachRepo {
            workspace_id: workspace(2),
            repo_id: repo_id(),
        },
    )
    .await
    .unwrap();
    assert_eq!(tree(&h.first).await, ["alpha.txt"]);
    assert_detached_review_blocks_sources(&h).await;
}

async fn assert_detached_review_blocks_sources(h: &Harness) {
    assert!(matches!(
        h.first
            .request(Request::ReviewSnapshot {
                review_id: review_id(2)
            })
            .await
            .unwrap(),
        Response::ReviewSnapshot { .. }
    ));
    assert!(matches!(
        h.first
            .request(Request::ListFiles {
                review_id: review_id(2),
                scope: DiffScope::All
            })
            .await,
        Err(ClientError::Rpc(RpcError::Invalid { .. }))
    ));
    assert!(matches!(
        h.first
            .request(Request::ListFiles {
                review_id: review_id(1),
                scope: DiffScope::All
            })
            .await
            .unwrap(),
        Response::Files { .. }
    ));
    let (_, mut stream) = h
        .first
        .stream(Request::OpenReview {
            review_id: review_id(2),
            opts: RenderOpts::default(),
        })
        .await
        .unwrap();
    assert!(matches!(
        stream.recv().await,
        Some(Ok(StreamItem::ReviewSnapshot { .. }))
    ));
    assert!(matches!(
        stream.recv().await,
        Some(Err(ClientError::Rpc(RpcError::Invalid { .. })))
    ));
    assert!(
        stream.recv().await.is_none(),
        "no tree content may escape the membership guard"
    );
    let ops = Ops::new(connect(&h.dir.path().join("nits.sock"), 3).await);
    assert!(matches!(
        ops.file_at(
            review_id(2),
            Some(repo_id()),
            &RepoPath::new("alpha.txt").unwrap(),
            Side::Head
        )
        .await,
        Err(OpsError::Rpc(RpcError::Invalid { .. }))
    ));
    let (_, _, _, chunks) = ops
        .file_at(
            review_id(1),
            Some(repo_id()),
            &RepoPath::new("alpha.txt").unwrap(),
            Side::Head,
        )
        .await
        .unwrap();
    assert!(
        !chunks.is_empty(),
        "unchanged files still work in the surviving review"
    );
}

#[tokio::test]
async fn concurrent_fresh_ids_cannot_duplicate_one_workspace_checkout() {
    let alpha = RepoBuilder::new()
        .commit("initial", files!["alpha.txt" => "alpha\n"])
        .build()
        .unwrap();
    let h = start().await;
    let before = h.daemon.core().last_seq().unwrap();
    let first = RepoId::from_parts(9, 1);
    let second = RepoId::from_parts(9, 2);
    let attach = |repo_id| Mutation::AttachRepo {
        workspace_id: workspace(1),
        repo_id,
        path: alpha.path().to_str().unwrap().into(),
        display_name: "repository".into(),
    };
    let (a, b) = tokio::join!(
        mutate(&h.first, attach(first)),
        mutate(&h.second, attach(second))
    );
    let results = [a, b];
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Ok(Response::Committed { .. })))
            .count(),
        1
    );
    assert_eq!(h.daemon.core().events_after(before).unwrap().len(), 1);
    let memberships = h.daemon.core().workspace(workspace(1)).unwrap().repos;
    assert_eq!(memberships.len(), 1);
    let winner = memberships[0].id;
    let rejected = results
        .iter()
        .find_map(|result| match result {
            Err(ClientError::Rpc(RpcError::Invalid { reason })) => Some(reason),
            _ => None,
        })
        .unwrap();
    assert!(rejected.contains(&winner.to_string()) && rejected.contains(&workspace(1).to_string()));
    // The same checkout may still join another workspace under a fresh ID.
    mutate(
        &h.first,
        Mutation::AttachRepo {
            workspace_id: workspace(2),
            repo_id: RepoId::from_parts(9, 3),
            path: alpha.path().to_str().unwrap().into(),
            display_name: "shared".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        h.daemon.core().workspace(workspace(2)).unwrap().repos.len(),
        1
    );
}
