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
        let mut child = Command::new(env!("CARGO_BIN_EXE_nits"))
            .args(["mcp", "--data-dir"])
            .arg(dir)
            .arg("--socket")
            .arg(socket)
            .args(["--start-policy", "require-running"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
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

    // Queue ping behind the formerly wedged call on the actual stdio loop.
    mcp.send(list(3)).await;
    mcp.send(json!({ "jsonrpc": "2.0", "id": 4, "method": "ping" }))
        .await;
    let lost = mcp.receive().await;
    assert_eq!(lost["id"], 3);
    assert_eq!(lost["result"]["isError"], true, "{lost}");
    let message = lost["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        message.contains("next tool call will reconnect")
            || message.contains("call the tool again"),
        "{message}"
    );
    let ping = mcp.receive().await;
    assert_eq!(ping["id"], 4);
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
