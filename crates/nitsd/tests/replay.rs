//! Bounded replay and live handoff over real transports, without subscriptions.
use std::sync::Arc;
use std::time::Duration;

use nits_protocol::{
    Author, BuildInfo, ClientId, ClientSeq, Mutation, ReplayCursor, ReplayPage, ReplayPosition,
    ReplayProgress, Request, Response, RpcError, Seq, Since, SubscribeScope, WorkspaceId,
};
use nits_review_core::DataDir;
use nitsd::Daemon;
use nitsd::client::{Client, ClientError, Identity};
use nitsd::server::{UnixServer, WsServer};
use tokio_util::sync::CancellationToken;

fn identity() -> Identity {
    Identity {
        client_id: ClientId::from_parts(1, 1),
        client: BuildInfo {
            name: "replay-test".into(),
            version: "0".into(),
        },
        author: Author::Human {
            name: "ada".into(),
            machine: "test".into(),
        },
    }
}
fn ws() -> WorkspaceId {
    WorkspaceId::from_parts(1, 1)
}
fn request(scope: SubscribeScope, position: ReplayPosition) -> Request {
    Request::ReplayEvents { scope, position }
}
async fn page(client: &Client, position: ReplayPosition) -> ReplayPage {
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        client.request(request(SubscribeScope::All, position)),
    )
    .await
    .unwrap()
    .unwrap();
    let Response::ReplayEvents { page } = response else {
        panic!("unexpected {response:?}")
    };
    page
}
async fn seed(daemon: &Daemon, count: usize) {
    daemon
        .write(move |core| {
            let ctx = Daemon::ctx(identity().author, identity().client_id, ClientSeq::new(1));
            core.create_workspace(&ctx, ws(), "workspace".into())?;
            for n in 1..count {
                core.rename_workspace(&ctx, ws(), format!("rename {n}"))?;
            }
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn bounded_rpc_pages_keep_capture_and_live_handoff_order_without_subscribing() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(&DataDir::new(dir.path()), identity().client).unwrap();
    seed(&daemon, 1100).await;
    let socket = dir.path().join("replay.sock");
    let server = UnixServer::bind(&socket).unwrap();
    let stop = CancellationToken::new();
    let serving = tokio::spawn(server.run(Arc::clone(&daemon), stop.clone()));
    let client = Client::connect_unix(&socket, identity()).await.unwrap();
    let first = page(
        &client,
        ReplayPosition::Start {
            since: Since::After { seq: Seq::new(0) },
        },
    )
    .await;
    assert_eq!(first.through, Seq::new(1100));
    let mut output = first
        .events
        .iter()
        .map(|event| event.seq)
        .collect::<Vec<_>>();
    daemon
        .write(|core| {
            let ctx = Daemon::ctx(identity().author, identity().client_id, ClientSeq::new(2));
            for n in 0..100 {
                core.rename_workspace(&ctx, ws(), format!("concurrent {n}"))?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let mut current = first;
    while let ReplayProgress::More { after } = current.progress {
        current = page(
            &client,
            ReplayPosition::Continue {
                cursor: ReplayCursor::new(after, current.through).unwrap(),
            },
        )
        .await;
        assert_eq!(current.through, Seq::new(1100));
        output.extend(current.events.iter().map(|event| event.seq));
    }
    assert_eq!(output, (1..=1100).map(Seq::new).collect::<Vec<_>>());
    let handoff = page(
        &client,
        ReplayPosition::Follow {
            after: current.through,
        },
    )
    .await;
    assert_eq!(
        handoff
            .events
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        (1101..=1200).map(Seq::new).collect::<Vec<_>>()
    );
    // Repeat simultaneous first-wait/write handoffs; either registration order
    // must return exactly the newly committed record, including the lost-wakeup gap.
    let mut after = handoff.through;
    for n in 0..25 {
        let (next, mutation) = tokio::join!(
            page(&client, ReplayPosition::Follow { after }),
            client.request(Request::Mutate {
                client_seq: ClientSeq::new(n),
                mutation: Mutation::RenameWorkspace {
                    workspace_id: ws(),
                    name: format!("live {n}")
                }
            })
        );
        let Response::Committed { event } = mutation.unwrap() else {
            panic!("mutation")
        };
        assert_eq!(next.events, [event]);
        assert!(next.through > after);
        after = next.through;
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(30), client.next_unsolicited())
            .await
            .is_err(),
        "paging must not leave an event subscription"
    );
    drop(client);
    stop.cancel();
    serving.await.unwrap();
}

async fn wait_lifecycle(websocket: bool) {
    let dir = tempfile::tempdir().unwrap();
    let data = DataDir::new(dir.path());
    let daemon = Daemon::open(&data, identity().client).unwrap();
    let weak = Arc::downgrade(&daemon);
    let stop = CancellationToken::new();
    let (serving, client) = if websocket {
        let server = WsServer::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let url = format!("ws://{}", server.addr());
        let serving = tokio::spawn(server.run(Arc::clone(&daemon), stop.clone()));
        (serving, Client::connect_ws(&url, identity()).await.unwrap())
    } else {
        let socket = dir.path().join("replay.sock");
        let server = UnixServer::bind(&socket).unwrap();
        let serving = tokio::spawn(server.run(Arc::clone(&daemon), stop.clone()));
        (
            serving,
            Client::connect_unix(&socket, identity()).await.unwrap(),
        )
    };
    let client = Arc::new(client);
    // A stream registration gives this wire-level test its request ID; the
    // single-response wait stays pending and can be explicitly cancelled.
    let (id, mut pending) = client
        .stream(request(
            SubscribeScope::All,
            ReplayPosition::Follow {
                after: Seq::new(u64::MAX),
            },
        ))
        .await
        .unwrap();
    assert!(matches!(
        client.request(Request::ListWorkspaces).await.unwrap(),
        Response::Workspaces { .. }
    ));
    seed(&daemon, 2).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), pending.recv())
            .await
            .is_err(),
        "future cursors must not move backwards to the present head"
    );
    client.cancel(id);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), pending.recv())
            .await
            .unwrap(),
        Some(Err(ClientError::Rpc(RpcError::Cancelled)))
    ));
    assert!(pending.recv().await.is_none());
    // Follow remains reusable after cancellation; ordinary reads keep responding.
    let copy = Arc::clone(&client);
    let waiting = tokio::spawn(async move {
        copy.request(request(
            SubscribeScope::All,
            ReplayPosition::Follow { after: Seq::new(2) },
        ))
        .await
    });
    assert!(matches!(
        client.request(Request::ListWorkspaces).await.unwrap(),
        Response::Workspaces { .. }
    ));
    stop.cancel();
    serving.await.unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), waiting)
            .await
            .unwrap()
            .unwrap(),
        Err(ClientError::Closed)
    ));
    drop(client);
    drop(daemon);
    tokio::time::timeout(Duration::from_secs(3), async {
        while weak.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
        loop {
            match Daemon::open(&data, identity().client) {
                Ok(reopened) => break drop(reopened),
                Err(error) if error.to_string().contains("Database already open") => {
                    tokio::task::yield_now().await
                }
                Err(error) => panic!("unexpected reopen: {error}"),
            }
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn unix_follow_wait_is_cancellable_and_shutdown_releases_the_store() {
    wait_lifecycle(false).await;
}
#[tokio::test]
async fn websocket_follow_wait_is_cancellable_and_shutdown_releases_the_store() {
    wait_lifecycle(true).await;
}

#[tokio::test]
async fn large_replay_events_are_split_into_transport_sized_pages() {
    use nits_protocol::{Envelope, RequestId, ServerMsg};
    use nits_review_core::store::replay::REPLAY_PAGE_BYTES;
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(&DataDir::new(dir.path()), identity().client).unwrap();
    seed(&daemon, 1).await;
    daemon
        .write(|core| {
            let ctx = Daemon::ctx(identity().author, identity().client_id, ClientSeq::new(2));
            for n in 0..4 {
                core.rename_workspace(
                    &ctx,
                    ws(),
                    format!("large {n}: {}", "x".repeat(REPLAY_PAGE_BYTES + 100)),
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let mut position = ReplayPosition::Start {
        since: Since::After { seq: Seq::new(1) },
    };
    let mut received = vec![];
    loop {
        let page = daemon
            .replay_events(SubscribeScope::All, position)
            .await
            .unwrap();
        assert_eq!(page.events.len(), 1, "a large event occupies its own page");
        let encoded = nitsd::codec::encode(&Envelope::current(ServerMsg::Response {
            id: RequestId::new(u64::MAX),
            response: Response::ReplayEvents { page: page.clone() },
        }))
        .unwrap();
        assert!(encoded.len() < nitsd::codec::MAX_FRAME as usize);
        received.push(page.events[0].seq);
        match page.progress {
            ReplayProgress::More { after } => {
                position = ReplayPosition::Continue {
                    cursor: ReplayCursor::new(after, page.through).unwrap(),
                }
            }
            ReplayProgress::Complete => break,
        }
    }
    assert_eq!(received, (2..=5).map(Seq::new).collect::<Vec<_>>());
}
