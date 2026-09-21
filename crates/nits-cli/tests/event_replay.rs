//! Real CLI replay drains every page and emits one parseable JSON event per line.
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use nits_protocol::{Author, BuildInfo, ClientId, ClientSeq, Event, EventBody, Seq, WorkspaceId};
use nits_review_core::DataDir;
use nitsd::Daemon;
use nitsd::server::UnixServer;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

struct Harness {
    dir: tempfile::TempDir,
    socket: std::path::PathBuf,
    daemon: Arc<Daemon>,
    stop: CancellationToken,
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
fn ws(n: u128) -> WorkspaceId {
    WorkspaceId::from_parts(1, n)
}
fn context() -> nits_review_core::Ctx {
    Daemon::ctx(
        Author::Human {
            name: "ada".into(),
            machine: "test".into(),
        },
        ClientId::from_parts(1, 1),
        ClientSeq::new(1),
    )
}
async fn start() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("replay.sock");
    let daemon = Daemon::open(
        &DataDir::new(dir.path()),
        BuildInfo {
            name: "cli-replay".into(),
            version: "0".into(),
        },
    )
    .unwrap();
    daemon
        .write(|core| {
            let ctx = context();
            for n in 1..=2 {
                core.create_workspace(&ctx, ws(n), format!("workspace {n}"))?;
            }
            for n in 0..1050 {
                core.rename_workspace(&ctx, ws(1), format!("selected {n}\nembedded \"quotes\""))?;
                core.rename_workspace(&ctx, ws(2), format!("other {n}"))?;
            }
            // Leave a final scanned page containing only nonmatching records.
            for n in 0..300 {
                core.rename_workspace(&ctx, ws(2), format!("unmatched tail {n}"))?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let server = UnixServer::bind(&socket).unwrap();
    let stop = CancellationToken::new();
    tokio::spawn(server.run(Arc::clone(&daemon), stop.clone()));
    Harness {
        dir,
        socket,
        daemon,
        stop,
    }
}
impl Harness {
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_nits"));
        command
            .arg("--socket")
            .arg(&self.socket)
            .arg("--config")
            .arg(self.dir.path().join("no-config.toml"))
            .args(["--start-policy", "require-running", "--json"])
            .env_remove("NITS_CONTEXT")
            .env_remove("NITS_WS_URL")
            .env_remove("NITS_AGENT")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }
    async fn output(&self, args: &[&str]) -> Vec<Event> {
        let output =
            tokio::time::timeout(Duration::from_secs(10), self.command().args(args).output())
                .await
                .unwrap()
                .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).expect("one complete JSON event per line"))
            .collect()
    }
}

#[tokio::test]
async fn historical_cli_replay_drains_over_a_thousand_matches_and_empty_final_pages() {
    let h = start().await;
    let expected = h.daemon.core().events_after(None).unwrap();
    assert_eq!(h.output(&["events", "--since", "0"]).await, expected);
    let selected = ws(1).to_string();
    let filtered: Vec<_> = expected
        .iter()
        .filter(|event| event.body.workspace_id() == Some(ws(1)))
        .cloned()
        .collect();
    assert!(filtered.len() > 1000);
    for args in [
        vec!["--workspace", &selected, "events", "--since", "0"],
        vec!["events", "--workspace", &selected, "--since", "0"],
    ] {
        assert_eq!(h.output(&args).await, filtered);
    }
    assert!(
        h.output(&["events", "--awaiting", "nobody", "--since", "0"])
            .await
            .is_empty()
    );
    assert!(h.output(&["events"]).await.is_empty());
    for cursor in ["999999", "18446744073709551615"] {
        assert!(h.output(&["events", "--since", cursor]).await.is_empty());
    }
    assert_eq!(
        h.output(&[
            "events",
            "--since",
            &expected.last().unwrap().seq.to_string()
        ])
        .await,
        vec![]
    );
}

#[tokio::test]
async fn cli_follow_handoff_delivers_concurrent_writes_once_and_disconnects_cleanly() {
    let h = start().await;
    let initial = h.daemon.core().events_after(None).unwrap();
    let mut child = h
        .command()
        .args(["events", "--since", "0", "--follow"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let first: Event = serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first, initial[0]);
    // The first emitted line proves the historical boundary was already captured.
    // Write during output/replay, before the CLI can enter Follow.
    let ((), concurrent) = h
        .daemon
        .write(|core| {
            for n in 0..30 {
                core.rename_workspace(&context(), ws(1), format!("handoff {n}"))?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let expected: Vec<_> = initial.into_iter().skip(1).chain(concurrent).collect();
    tokio::time::timeout(Duration::from_secs(10), async {
        for expected in expected {
            let line = lines.next_line().await.unwrap().unwrap();
            assert_eq!(serde_json::from_str::<Event>(&line).unwrap(), expected);
        }
    })
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), lines.next_line())
            .await
            .is_err(),
        "no duplicates after handoff"
    );
    let ((), tail) = h
        .daemon
        .write(|core| core.rename_workspace(&context(), ws(1), "after live wait".into()))
        .await
        .unwrap();
    let line = tokio::time::timeout(Duration::from_secs(3), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(serde_json::from_str::<Event>(&line).unwrap(), tail[0]);
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while h.daemon.connections() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn non_follow_cli_exits_while_live_writes_continue() {
    let h = start().await;
    let initial = h.daemon.core().last_seq().unwrap().unwrap();
    let done = CancellationToken::new();
    let writer_done = done.clone();
    let daemon = Arc::clone(&h.daemon);
    let writer = tokio::spawn(async move {
        let mut n = 0;
        loop {
            tokio::select! {
                () = writer_done.cancelled() => break,
                () = tokio::time::sleep(Duration::from_millis(1)) => {},
            }
            daemon
                .write(move |core| core.rename_workspace(&context(), ws(2), format!("ongoing {n}")))
                .await
                .unwrap();
            n += 1;
        }
        n
    });
    let events = h.output(&["events", "--since", "0"]).await;
    assert!(!writer.is_finished());
    done.cancel();
    writer.await.unwrap();
    assert!(events.last().unwrap().seq >= initial);
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        (1..=events.last().unwrap().seq.get())
            .map(Seq::new)
            .collect::<Vec<_>>()
    );
    assert!(events.iter().any(|event| matches!(&event.body, EventBody::WorkspaceUpdated { name, .. } if name.contains('\n'))));
}
