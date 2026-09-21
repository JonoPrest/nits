//! Planned retirement reaches every transport without relying on subscriptions.
use std::sync::Arc;
use std::time::Duration;

use nits_protocol::{Author, BuildInfo, ClientId, LifecycleNotice, ProtocolVersion, ReplayPosition, Request, Response, RpcError, Seq, Since, SubscribeScope, UpgradeId, UpgradeOperation, UpgradeProgress, UpgradeStage};
use nits_review_core::DataDir;
use nitsd::client::{Client, ClientError, Identity, Unsolicited};
use nitsd::server::{UnixServer, WsServer};

fn identity(n: u128) -> Identity {
    Identity { client_id: ClientId::from_parts(1, n), client: BuildInfo { name: "retirement-test".into(), version: "1".into() }, author: Author::Human { name: "ada".into(), machine: "test".into() } }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unix_and_websocket_notices_bypass_filters_and_interrupt_waits_before_store_release() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("d.sock");
    let daemon = nitsd::Daemon::open(&DataDir::new(dir.path()), BuildInfo { name: "test".into(), version: "1".into() }).unwrap();
    let unix = UnixServer::bind(&socket).unwrap();
    let ws = WsServer::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let url = format!("ws://{}", ws.addr());
    let shutdown = daemon.shutdown().clone();
    let unix_task = tokio::spawn(unix.run(Arc::clone(&daemon), shutdown.clone()));
    let ws_task = tokio::spawn(ws.run(Arc::clone(&daemon), shutdown.clone()));
    let unix = Arc::new(Client::connect_unix(&socket, identity(1)).await.unwrap());
    let ws = Arc::new(Client::connect_ws(&url, identity(2)).await.unwrap());
    unix.request(Request::Subscribe { scope: SubscribeScope::AwaitingAgent { agent: "nobody".into() }, since: Since::Now }).await.unwrap();
    // The WS peer never subscribes. Both still need the same lifecycle notice.
    let wait = tokio::spawn({ let ws = Arc::clone(&ws); async move { ws.request(Request::ReplayEvents { scope: SubscribeScope::All, position: ReplayPosition::Follow { after: Seq::new(0) } }).await } });
    assert!(matches!(unix.request(Request::ListWorkspaces).await.unwrap(), Response::Workspaces { .. }));
    let (entered, entering) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let blocked = tokio::spawn({ let daemon = Arc::clone(&daemon); async move { daemon.read(move |_| { let _ = entered.send(()); released.recv().unwrap(); Ok(()) }).await } });
    entering.await.unwrap();
    let build = nitsd::build::running().unwrap();
    let mut target = build.clone();
    target.digest = nits_protocol::BuildDigest::from_bytes([2; 32]);
    let operation = daemon.prepare_restart(UpgradeOperation { id: UpgradeId::from_parts(1, 1), source: build, target, progress: UpgradeProgress::Active { stage: UpgradeStage::PreparingRestart } });
    for client in [&unix, &ws] {
        let notice = tokio::time::timeout(Duration::from_secs(2), client.next_unsolicited()).await.unwrap().unwrap();
        assert!(matches!(notice, Unsolicited::Lifecycle(LifecycleNotice::Restarting { operation: announced }) if announced.id == operation.id));
    }
    // Gate closure is typed for current Hello, but a strict old client receives
    // only the older protocol's ordinary rejection shape, never Lifecycle.
    let current = Client::connect_unix(&socket, identity(3)).await.unwrap_err();
    assert!(matches!(current, ClientError::Rejected(RpcError::Restarting { operation_id }) if operation_id == operation.id));
    let raw = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let old = Client::handshake(raw, identity(4), ProtocolVersion::new(0, 1, 0)).await.unwrap_err();
    assert!(matches!(old, ClientError::Rejected(RpcError::UnsupportedProtocol { .. })));
    let result = tokio::time::timeout(Duration::from_secs(5), wait).await.unwrap().unwrap();
    assert!(matches!(result, Err(ClientError::Rpc(RpcError::Restarting { .. } | RpcError::RestartInterrupted { .. }))));
    assert!(!shutdown.is_cancelled(), "blocked accepted Core work still owns retirement");
    assert!(nits_review_core::Core::open(&DataDir::new(dir.path())).is_err());
    release.send(()).unwrap();
    blocked.await.unwrap().unwrap();
    drop(unix);
    drop(ws);
    tokio::time::timeout(Duration::from_secs(6), shutdown.cancelled()).await.unwrap();
    unix_task.await.unwrap().unwrap();
    ws_task.await.unwrap().unwrap();
    drop(daemon);
    nits_review_core::Core::open(&DataDir::new(dir.path())).unwrap();
}
