//! Broadcasts follow durable commit order even when request futures stop polling.

use std::sync::Arc;
use std::time::Duration;

use nits_protocol::{Author, BuildInfo, ClientId, ClientSeq, WorkspaceId};
use nits_review_core::{CoreError, Ctx, DataDir};
use nitsd::Daemon;
use tokio::sync::oneshot;

fn daemon() -> (tempfile::TempDir, Arc<Daemon>) {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(
        &DataDir::new(dir.path()),
        BuildInfo {
            name: "test".into(),
            version: "0".into(),
        },
    )
    .unwrap();
    (dir, daemon)
}

fn ctx(seq: ClientSeq) -> Ctx {
    Daemon::ctx(
        Author::Human {
            name: "ada".into(),
            machine: "box".into(),
        },
        ClientId::from_parts(1, 1),
        seq,
    )
}

#[tokio::test]
async fn broadcasts_follow_commits_even_when_callers_receive_results_out_of_order() {
    let (_dir, daemon) = daemon();
    let mut events = daemon.subscribe();
    let (started_tx, started) = oneshot::channel();
    let (resume, resume_rx) = std::sync::mpsc::channel();
    let mut first = Box::pin(daemon.write(move |core| {
        started_tx.send(()).unwrap();
        resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        core.create_workspace(
            &ctx(ClientSeq::new(1)),
            WorkspaceId::from_parts(1, 1),
            "first".into(),
        )
    }));
    tokio::select! {
        result = &mut first => panic!("write completed before release: {result:?}"),
        result = started => result.unwrap(),
    }
    // Keep the first request's result unpolled while the second write finishes.
    // Publishing from the request futures would reverse the broadcast order.
    resume.send(()).unwrap();
    let (_, second_events) = daemon
        .write(|core| {
            core.create_workspace(
                &ctx(ClientSeq::new(2)),
                WorkspaceId::from_parts(1, 2),
                "second".into(),
            )
        })
        .await
        .unwrap();
    let (_, first_events) = first.await.unwrap();
    for expected in first_events.into_iter().chain(second_events) {
        let actual = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*actual, expected);
    }
}

#[tokio::test]
async fn cancelling_a_write_request_does_not_cancel_its_committed_broadcast() {
    let (_dir, daemon) = daemon();
    let mut events = daemon.subscribe();
    let (started_tx, started) = oneshot::channel();
    let (resume, resume_rx) = std::sync::mpsc::channel();
    let mut cancelled = Box::pin(daemon.write(move |core| {
        started_tx.send(()).unwrap();
        resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        core.create_workspace(
            &ctx(ClientSeq::new(1)),
            WorkspaceId::from_parts(1, 1),
            "first".into(),
        )
    }));
    tokio::select! {
        result = &mut cancelled => panic!("write completed before release: {result:?}"),
        result = started => result.unwrap(),
    }
    drop(cancelled);
    resume.send(()).unwrap();
    // A second job fences the cancelled request's durable commit.
    daemon
        .write(|core| {
            core.create_workspace(
                &ctx(ClientSeq::new(2)),
                WorkspaceId::from_parts(1, 2),
                "second".into(),
            )
        })
        .await
        .unwrap();
    for expected in daemon.core().events_after(None).unwrap() {
        let actual = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*actual, expected);
    }
}

#[tokio::test]
async fn an_error_after_a_commit_still_broadcasts_the_durable_event() {
    let (_dir, daemon) = daemon();
    let mut events = daemon.subscribe();
    let result = daemon
        .write(|core| {
            core.create_workspace(
                &ctx(ClientSeq::new(1)),
                WorkspaceId::from_parts(1, 1),
                "committed".into(),
            )?;
            Err::<(), _>(CoreError::Invalid {
                reason: "follow-up operation failed".into(),
            })
        })
        .await;
    assert!(result.is_err());
    let expected = daemon.core().events_after(None).unwrap();
    assert_eq!(expected.len(), 1);
    let actual = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*actual, expected[0]);
}

#[tokio::test]
async fn unexpected_writer_panic_stops_listeners_and_status_cannot_report_running() {
    use nits_config::Context;
    use nitsd::contexts::{Status, status};
    use nitsd::daemon::DaemonError;
    use nitsd::server::{UnixServer, WsServer};

    let (dir, daemon) = daemon();
    let socket = dir.path().join("nitsd.sock");
    let unix = UnixServer::bind(&socket).unwrap();
    let ws = WsServer::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let contexts = [
        Context::Local {
            data_dir: Some(dir.path().to_path_buf()),
            socket: Some(socket.clone()),
        },
        Context::Ws {
            url: format!("ws://{}", ws.addr()),
        },
    ];
    // Use the same cancellation token wiring as the daemon's serve entrypoint.
    let unix = tokio::spawn(unix.run(Arc::clone(&daemon), daemon.shutdown().clone()));
    let ws = tokio::spawn(ws.run(Arc::clone(&daemon), daemon.shutdown().clone()));
    for context in &contexts {
        assert!(matches!(status(context).await, Status::Running { .. }));
    }

    let result = daemon
        .write(|_| -> Result<(), CoreError> { panic!("injected unexpected writer failure") })
        .await;
    assert!(matches!(result, Err(DaemonError::Shutdown)));
    tokio::time::timeout(Duration::from_secs(5), async {
        unix.await.unwrap();
        ws.await.unwrap();
    })
    .await
    .expect("writer failure must stop both daemon listeners");
    assert!(daemon.shutdown().is_cancelled());
    assert!(!socket.exists());
    assert!(matches!(
        status(&contexts[0]).await,
        Status::Transitioning { .. }
    ));
    assert!(matches!(status(&contexts[1]).await, Status::Stopped));
    assert!(matches!(
        daemon
            .write(|core| core.create_workspace(
                &ctx(ClientSeq::new(1)),
                WorkspaceId::from_parts(1, 1),
                "cannot commit after writer failure".into(),
            ))
            .await,
        Err(DaemonError::Shutdown)
    ));
    assert!(daemon.core().events_after(None).unwrap().is_empty());
    drop(daemon);
    for context in &contexts {
        assert!(matches!(status(context).await, Status::Stopped));
    }
}
