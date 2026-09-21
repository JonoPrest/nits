//! The same stdio MCP process survives a stopped and restarted real daemon.

use std::future::Future;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use nits_protocol::{Author, BuildInfo, ClientId, ClientSeq, Mutation, Request, WorkspaceId};
use nitsd::client::{Client, Identity};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("MCP/daemon operation hung")
}

async fn spawn_daemon(dir: &Path, socket: &Path) -> Child {
    let child = Command::new(env!("CARGO_BIN_EXE_nits"))
        .args(["daemon", "serve", "--data-dir"])
        .arg(dir)
        .arg("--socket")
        .arg(socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    bounded(async {
        while tokio::net::UnixStream::connect(socket).await.is_err() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    child
}

async fn connect(socket: &Path) -> Client {
    bounded(Client::connect_unix(
        socket,
        Identity {
            client_id: ClientId::from_parts(1, 1),
            client: BuildInfo {
                name: "test".into(),
                version: "0".into(),
            },
            author: Author::Human {
                name: "ada".into(),
                machine: "box".into(),
            },
        },
    ))
    .await
    .unwrap()
}

struct Mcp {
    child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
}

impl Mcp {
    fn spawn(dir: &Path, socket: &Path) -> Self {
        Self::spawn_in(dir, socket, None)
    }

    fn spawn_in(dir: &Path, socket: &Path, cwd: Option<&Path>) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_nits"));
        command
            .args(["mcp", "--data-dir"])
            .arg(dir)
            .arg("--socket")
            .arg(socket)
            .args(["--start-policy", "require-running"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn().unwrap();
        Self {
            input: child.stdin.take().unwrap(),
            output: BufReader::new(child.stdout.take().unwrap()).lines(),
            child,
        }
    }

    async fn send(&mut self, message: Value) {
        let mut bytes = serde_json::to_vec(&message).unwrap();
        bytes.push(b'\n');
        bounded(self.input.write_all(&bytes)).await.unwrap();
        bounded(self.input.flush()).await.unwrap();
    }

    async fn receive(&mut self) -> Value {
        let line = bounded(self.output.next_line()).await.unwrap().unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn list(&mut self, id: u64) -> Value {
        self.send(list(id)).await;
        let reply = self.receive().await;
        assert_eq!(reply["id"], id);
        assert!(reply["result"].get("isError").is_none(), "{reply}");
        reply["result"]["structuredContent"]["workspaces"].clone()
    }
}

fn list(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": "list_workspaces", "arguments": {} }
    })
}

#[tokio::test]
async fn stopped_daemon_does_not_wedge_stdio_and_same_mcp_recovers_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    let mut daemon = spawn_daemon(dir.path(), &socket).await;
    let client = connect(&socket).await;
    bounded(client.request(Request::Mutate {
        client_seq: ClientSeq::new(1),
        mutation: Mutation::CreateWorkspace {
            workspace_id: WorkspaceId::from_parts(1, 1),
            name: "persistent workspace".into(),
        },
    }))
    .await
    .unwrap();
    let mut mcp = Mcp::spawn(dir.path(), &socket);
    let process_id = mcp.child.id();
    mcp.send(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "test-agent", "version": "0" }
        }
    }))
    .await;
    assert!(mcp.receive().await.get("error").is_none());
    mcp.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .await;
    let before = mcp.list(2).await;
    assert_eq!(before[0]["name"], "persistent workspace");

    bounded(client.request(Request::Shutdown)).await.unwrap();
    assert!(bounded(daemon.wait()).await.unwrap().success());
    assert!(bounded(client.next_unsolicited()).await.is_none());

    // The stable host can answer ping before the worker's outage response.
    // Both requests must complete, without imposing cross-request ordering.
    mcp.send(list(3)).await;
    mcp.send(json!({ "jsonrpc": "2.0", "id": 4, "method": "ping" }))
        .await;
    let replies = [mcp.receive().await, mcp.receive().await];
    let lost = replies.iter().find(|reply| reply["id"] == 3).unwrap();
    assert_eq!(lost["result"]["isError"], true, "{lost}");
    let message = lost["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        message.contains("next tool call will reconnect")
            || message.contains("call the tool again"),
        "{message}"
    );
    let ping = replies.iter().find(|reply| reply["id"] == 4).unwrap();
    assert_eq!(ping["result"], json!({}));
    // Repeated outage calls also finish, leaving the session initialized.
    mcp.send(list(5)).await;
    assert_eq!(mcp.receive().await["result"]["isError"], true);

    daemon = spawn_daemon(dir.path(), &socket).await;
    assert_eq!(mcp.list(6).await, before);
    assert_eq!(mcp.child.id(), process_id);
    assert!(mcp.child.try_wait().unwrap().is_none());
    mcp.child.kill().await.unwrap();
    daemon.kill().await.unwrap();
}

async fn initialize(mcp: &mut Mcp) {
    mcp.send(json!({"jsonrpc":"2.0", "id":1, "method":"initialize",
        "params":{"protocolVersion":"2025-06-18","capabilities":{},
        "clientInfo":{"name":"test-agent","version":"0"}}}))
        .await;
    assert!(mcp.receive().await.get("error").is_none());
}

fn poll(id: u64) -> Value {
    json!({"jsonrpc":"2.0", "id":id, "method":"tools/call",
        "params":{"name":"subscribe_events","arguments":{"awaiting_agent":"nobody","timeout_ms":60000}}})
}

#[tokio::test]
async fn real_stdio_event_waits_allow_ping_cancel_and_eof_without_waiting_for_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    let mut daemon = spawn_daemon(dir.path(), &socket).await;
    let mut mcp = Mcp::spawn(dir.path(), &socket);
    initialize(&mut mcp).await;
    mcp.send(poll(2)).await;
    mcp.send(json!({"jsonrpc":"2.0","id":3,"method":"ping"}))
        .await;
    assert_eq!(mcp.receive().await["id"], 3);
    mcp.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}}))
        .await;
    mcp.send(list(4)).await;
    assert_eq!(mcp.receive().await["id"], 4);
    mcp.send(poll(5)).await;
    mcp.send(json!({"jsonrpc":"2.0","id":6,"method":"ping"}))
        .await;
    assert_eq!(mcp.receive().await["id"], 6);
    // The process must cancel the still-pending poll and exit on actual pipe EOF.
    drop(mcp.input);
    assert!(bounded(mcp.child.wait()).await.unwrap().success());
    assert!(bounded(mcp.output.next_line()).await.unwrap().is_none());
    daemon.kill().await.unwrap();
}

#[tokio::test]
async fn real_stdio_pending_poll_reports_daemon_exit_and_reconnects_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    let mut daemon = spawn_daemon(dir.path(), &socket).await;
    let mut mcp = Mcp::spawn(dir.path(), &socket);
    initialize(&mut mcp).await;
    mcp.send(poll(2)).await;
    mcp.send(json!({"jsonrpc":"2.0","id":3,"method":"ping"}))
        .await;
    assert_eq!(mcp.receive().await["id"], 3);
    let client = connect(&socket).await;
    bounded(client.request(Request::Shutdown)).await.unwrap();
    assert!(bounded(daemon.wait()).await.unwrap().success());
    let disconnected = mcp.receive().await;
    assert_eq!(disconnected["id"], 2);
    assert_eq!(disconnected["result"]["isError"], true);
    mcp.send(json!({"jsonrpc":"2.0","id":4,"method":"ping"}))
        .await;
    assert_eq!(mcp.receive().await["id"], 4);
    daemon = spawn_daemon(dir.path(), &socket).await;
    assert!(mcp.list(5).await.as_array().unwrap().is_empty());
    drop(mcp.input);
    assert!(bounded(mcp.child.wait()).await.unwrap().success());
    daemon.kill().await.unwrap();
}

#[tokio::test]
async fn closed_stdout_during_wait_exits_even_with_stdin_open() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    let mut daemon = spawn_daemon(dir.path(), &socket).await;
    let mut mcp = Mcp::spawn(dir.path(), &socket);
    initialize(&mut mcp).await;
    mcp.send(poll(2)).await;
    mcp.send(json!({"jsonrpc":"2.0","id":3,"method":"ping"}))
        .await;
    assert_eq!(mcp.receive().await["id"], 3);
    drop(mcp.output);
    // No new stdin bytes: the event completion, not input, triggers the write.
    let client = connect(&socket).await;
    bounded(client.request(Request::Shutdown)).await.unwrap();
    assert!(bounded(daemon.wait()).await.unwrap().success());
    assert!(!bounded(mcp.child.wait()).await.unwrap().success());
    drop(mcp.input);
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Actual process cwd is required to exercise implicit repository identity.
async fn implicit_and_explicit_duplicate_targets_cannot_commit_through_real_mcp_stdio() {
    use nits_protocol::{ReplayPosition, ReplayProgress, RepoId, Response, Since, SubscribeScope};
    use nits_test_support::{RepoBuilder, files};
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    let repo = RepoBuilder::new()
        .commit("initial", files!["sample.txt" => "initial\n"])
        .build()
        .unwrap();
    let mut daemon = spawn_daemon(dir.path(), &socket).await;
    let client = connect(&socket).await;
    let workspace = WorkspaceId::from_parts(1, 1);
    let repo_id = RepoId::from_parts(1, 2);
    let mut through = None;
    for mutation in [
        Mutation::CreateWorkspace {
            workspace_id: workspace,
            name: "unique targets".into(),
        },
        Mutation::AttachRepo {
            workspace_id: workspace,
            repo_id,
            path: repo.path().to_str().unwrap().into(),
            display_name: "repo".into(),
        },
    ] {
        let Response::Committed { event } = bounded(client.request(Request::Mutate {
            client_seq: ClientSeq::new(1),
            mutation,
        }))
        .await
        .unwrap() else {
            panic!("committed setup");
        };
        through = Some(event.seq);
    }
    let through = through.unwrap();
    let mut mcp = Mcp::spawn_in(dir.path(), &socket, Some(repo.path()));
    initialize(&mut mcp).await;
    let implicit = json!({"base":{"type":"Head"},"head":{"type":"WorkingTree"}});
    let explicit = json!({"repo_id":repo_id,"base":{"type":"Head"},"head":{"type":"WorkingTree"}});
    for (index, targets) in [json!([implicit,implicit]), json!([implicit,explicit]), json!([explicit,implicit]), json!([implicit,{"repo_id":repo_id,"base":{"type":"Branch","name":"missing"},"head":{"type":"Head"}}])].into_iter().enumerate() {
        let id = index + 2;
        mcp.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"create_review","arguments":{"title":"duplicate implicit targets","targets":targets}}})).await;
        let reply = mcp.receive().await;
        assert_eq!(reply["id"], id);
        assert_eq!(reply["result"]["isError"], true);
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("one base/head pair") && text.contains(&repo_id.to_string()), "{text}");
        let history = bounded(client.request(Request::ReplayEvents { scope: SubscribeScope::All, position: ReplayPosition::Start { since: Since::After { seq: through } } })).await.unwrap();
        assert!(matches!(history, Response::ReplayEvents { page } if page.through == through && page.events.is_empty() && page.progress == ReplayProgress::Complete));
    }
    mcp.send(json!({"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"create_review","arguments":{"title":"valid implicit singleton","targets":[implicit]}}})).await;
    let reply = mcp.receive().await;
    assert_eq!(reply["id"], 10);
    assert_ne!(reply["result"]["isError"], true, "{reply}");
    let Response::Reviews { reviews } = bounded(client.request(Request::ListReviews {
        workspace_id: workspace,
    }))
    .await
    .unwrap() else {
        panic!("reviews");
    };
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0].targets.len(), 1);
    assert_eq!(reviews[0].targets.first().repo_id, repo_id);
    mcp.child.kill().await.unwrap();
    daemon.kill().await.unwrap();
}
