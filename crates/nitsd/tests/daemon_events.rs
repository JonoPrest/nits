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
