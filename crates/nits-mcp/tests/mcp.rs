//! MCP server against a real daemon (plan 2.5): JSON-RPC conformance, each
//! tool round-trips through core, and agent provenance is structural.

use std::sync::Arc;

use nits_mcp::jsonrpc::{Incoming, Outgoing};
use nits_mcp::server::AgentIdentity;
use nits_mcp::{Endpoint, Server};
use nits_protocol::{Author, BuildInfo, ClientId, ClientSeq, Human, Mutation, Request, Response};
use nits_review_core::DataDir;
use nits_test_support::{RepoBuilder, TestRepo, files};
use nitsd::Daemon;
use nitsd::client::{Client, Identity};
use nitsd::server::UnixServer;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct Harness {
    _dir: tempfile::TempDir,
    socket: std::path::PathBuf,
    shutdown: CancellationToken,
    repo: TestRepo,
    daemon: Arc<Daemon>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

fn small_repo() -> TestRepo {
    RepoBuilder::new()
        .commit(
            "base",
            files!["a.rs" => "fn a() {}\nfn z() {}\n", "b.rs" => "fn b() {}\n"],
        )
        .branch("feature")
        .commit(
            "feat",
            files!["a.rs" => "fn a() { 1; }\nfn z() {}\n", "c.rs" => "fn c() {}\n"],
        )
        .build()
        .unwrap()
}

fn start() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let socket = std::env::temp_dir().join(format!(
        "nits-mcp-{}-{}.sock",
        std::process::id(),
        SOCKET_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let daemon = Daemon::open(
        &DataDir::new(dir.path()),
        BuildInfo {
            name: "nitsd".into(),
            version: "test".into(),
        },
    )
    .unwrap();
    let server = UnixServer::bind(&socket).unwrap();
    let shutdown = CancellationToken::new();
    tokio::spawn(server.run(Arc::clone(&daemon), shutdown.clone()));
    Harness {
        _dir: dir,
        socket,
        shutdown,
        repo: small_repo(),
        daemon,
    }
}

/// A human client, for seeding and for observing what the agent did.
async fn human(h: &Harness) -> Client {
    Client::connect_unix(
        &h.socket,
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
    )
    .await
    .unwrap()
}

fn server(h: &Harness) -> Server {
    Server::new(
        Endpoint {
            context: nits_config::Context::Local {
                data_dir: None,
                socket: Some(h.socket.clone()),
            },
            start: nitsd::contexts::StartPolicy::RequireRunning,
        },
        AgentIdentity {
            model: "test-model".into(),
            session_id: "sess-1".into(),
            invoked_by: Some(Human {
                name: "ada".into(),
                machine: "box".into(),
            }),
        },
        BuildInfo {
            name: "nits-mcp".into(),
            version: "test".into(),
        },
    )
}

fn req(id: u64, method: &str, params: Value) -> Incoming {
    Incoming {
        jsonrpc: "2.0".into(),
        id: Some(json!(id)),
        method: method.into(),
        params,
    }
}

async fn init(s: &mut Server) -> Outgoing {
    s.handle(req(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "claude-code", "version": "1.0" }
        }),
    ))
    .await
    .unwrap()
}

/// Call a tool and return its structured content, panicking on `isError`.
async fn call(s: &mut Server, name: &str, args: Value) -> Value {
    let out = s
        .handle(req(
            7,
            "tools/call",
            json!({ "name": name, "arguments": args }),
        ))
        .await
        .unwrap();
    assert!(out.error.is_none(), "{out:?}");
    let result = out.result.unwrap();
    assert!(
        result.get("isError").is_none(),
        "tool error: {}",
        result["content"][0]["text"]
    );
    result["structuredContent"].clone()
}

async fn call_err(s: &mut Server, name: &str, args: Value) -> String {
    let out = s
        .handle(req(
            7,
            "tools/call",
            json!({ "name": name, "arguments": args }),
        ))
        .await
        .unwrap();
    let result = out.result.expect("tool errors are results, not rpc errors");
    assert_eq!(result["isError"], json!(true), "{result}");
    result["content"][0]["text"].as_str().unwrap().to_string()
}

/// Distinct socket names per test in this process; the clock alone collides.
static SOCKET_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Workspace + repo via the human client; returns `(workspace_id, repo_id)`.
async fn seed(h: &Harness, c: &Client) -> (String, String) {
    let ws = nits_protocol::WorkspaceId::from_parts(1, 1);
    let rid = nits_protocol::RepoId::from_parts(1, 1);
    for (n, m) in [
        Mutation::CreateWorkspace {
            workspace_id: ws,
            name: "w".into(),
        },
        Mutation::AttachRepo {
            workspace_id: ws,
            repo_id: rid,
            path: h.repo.path().to_str().unwrap().into(),
            display_name: "r".into(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        c.request(Request::Mutate {
            client_seq: ClientSeq::new(n as u64 + 1),
            mutation: m,
        })
        .await
        .unwrap();
    }
    (ws.to_string(), rid.to_string())
}

fn main_feature(rid: &str) -> Value {
    json!([{
        "repo_id": rid,
        "base": { "type": "Branch", "name": "main" },
        "head": { "type": "Branch", "name": "feature" }
    }])
}

#[tokio::test]
async fn tools_list_is_json_rpc_conformant() {
    let h = start();
    let mut s = server(&h);
    let welcome = init(&mut s).await;
    let r = welcome.result.unwrap();
    assert_eq!(r["protocolVersion"], json!("2025-06-18"));
    assert_eq!(r["serverInfo"]["name"], json!("nits-mcp"));

    let out = s.handle(req(2, "tools/list", json!({}))).await.unwrap();
    assert_eq!(out.jsonrpc, "2.0");
    assert_eq!(out.id, json!(2));
    let tools = out.result.unwrap()["tools"].as_array().unwrap().clone();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "list_workspaces",
            "list_reviews",
            "get_review",
            "create_review",
            "update_review",
            "get_diff",
            "get_file",
            "list_comments",
            "add_comment",
            "suggest",
            "reply",
            "resolve",
            "request_review",
            "subscribe_events",
        ]
    );
    for t in &tools {
        assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()));
        assert_eq!(t["inputSchema"]["type"], json!("object"));
    }

    // Notifications get no reply; unknown methods get -32601; bad JSON -32700.
    let none = s
        .handle(Incoming {
            jsonrpc: "2.0".into(),
            id: None,
            method: "notifications/initialized".into(),
            params: Value::Null,
        })
        .await;
    assert!(none.is_none());
    let unknown = s.handle(req(3, "resources/list", json!({}))).await.unwrap();
    assert_eq!(unknown.error.unwrap().code, -32601);
    let bad = s.handle_line("{not json").await.unwrap();
    assert_eq!(bad.error.unwrap().code, -32700);
    let unknown_tool = s
        .handle(req(
            4,
            "tools/call",
            json!({ "name": "nope", "arguments": {} }),
        ))
        .await
        .unwrap();
    assert_eq!(unknown_tool.error.unwrap().code, -32602);
}

#[tokio::test]
async fn tools_round_trip_through_core() {
    let h = start();
    let c = human(&h).await;
    let (ws, rid) = seed(&h, &c).await;
    let mut s = server(&h);
    init(&mut s).await;

    let w = call(&mut s, "list_workspaces", json!({})).await;
    assert_eq!(w["workspaces"][0]["id"], json!(ws));
    assert_eq!(w["workspaces"][0]["repos"][0]["id"], json!(rid));

    let created = call(
        &mut s,
        "create_review",
        json!({ "workspace_id": ws, "title": "agent review", "targets": main_feature(&rid) }),
    )
    .await;
    let review_id = created["review"]["id"].as_str().unwrap().to_string();
    assert!(created["resolved"].is_array(), "targets resolved on create");

    let reviews = call(&mut s, "list_reviews", json!({ "workspace_id": ws })).await;
    assert_eq!(reviews["reviews"][0]["title"], json!("agent review"));

    let review = call(&mut s, "get_review", json!({ "review_id": review_id })).await;
    let paths: Vec<&str> = review["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["a.rs", "c.rs"]);

    let diff = call(
        &mut s,
        "get_diff",
        json!({ "review_id": review_id, "path": "a.rs" }),
    )
    .await;
    let text = diff["text"].as_str().unwrap();
    assert!(text.contains("-fn a() {}"), "{text}");
    assert!(text.contains("+fn a() { 1; }"), "{text}");
    assert_eq!(diff["change"]["type"], json!("Modified"));

    // Unchanged file on the base side still resolves through the tree.
    let file = call(
        &mut s,
        "get_file",
        json!({ "review_id": review_id, "path": "b.rs", "side": "Base" }),
    )
    .await;
    assert_eq!(file["text"], json!("    1│fn b() {}\n"));

    let missing = call_err(
        &mut s,
        "get_diff",
        json!({ "review_id": review_id, "path": "b.rs" }),
    )
    .await;
    assert!(missing.contains("not changed"), "{missing}");

    let updated = call(
        &mut s,
        "update_review",
        json!({ "review_id": review_id, "title": "renamed", "status": "Archived" }),
    )
    .await;
    assert_eq!(updated["event"]["body"]["type"], json!("ReviewUpdated"));
}

#[tokio::test]
async fn agent_comments_carry_provenance_and_thread_ops_work() {
    let h = start();
    let c = human(&h).await;
    let (ws, rid) = seed(&h, &c).await;
    let mut s = server(&h);
    init(&mut s).await;
    let created = call(
        &mut s,
        "create_review",
        json!({ "workspace_id": ws, "title": "r", "targets": main_feature(&rid) }),
    )
    .await;
    let review_id = created["review"]["id"].as_str().unwrap().to_string();

    let line = call(
        &mut s,
        "add_comment",
        json!({ "review_id": review_id, "path": "a.rs", "start_line": 1, "body": "nit" }),
    )
    .await;
    let thread_id = line["thread_id"].as_str().unwrap().to_string();
    assert_eq!(line["comment_id"], line["thread_id"]);
    let whole = call(
        &mut s,
        "add_comment",
        json!({ "review_id": review_id, "body": "overall fine" }),
    )
    .await;
    assert_eq!(
        whole["event"]["body"]["comment"]["anchor"]["type"],
        json!("Review")
    );
    call(
        &mut s,
        "suggest",
        json!({
            "review_id": review_id, "path": "a.rs", "start_line": 1,
            "patch": "@@ -1 +1 @@\n-fn a() { 1; }\n+fn a() { 2; }\n", "body": "use 2"
        }),
    )
    .await;
    let reply = call(
        &mut s,
        "reply",
        json!({ "review_id": review_id, "thread_id": thread_id, "body": "ack" }),
    )
    .await;
    assert_eq!(reply["event"]["body"]["type"], json!("CommentCreated"));
    assert_eq!(
        reply["event"]["body"]["comment"]["thread_id"],
        json!(thread_id)
    );
    call(
        &mut s,
        "resolve",
        json!({ "review_id": review_id, "thread_id": thread_id }),
    )
    .await;

    let comments = call(&mut s, "list_comments", json!({ "review_id": review_id })).await;
    let all = comments["comments"].as_array().unwrap();
    assert_eq!(all.len(), 4);
    for cm in all {
        assert_eq!(cm["author"]["type"], json!("Agent"), "{cm}");
        assert_eq!(cm["author"]["name"], json!("claude-code"));
        assert_eq!(cm["author"]["model"], json!("test-model"));
        assert_eq!(cm["author"]["session_id"], json!("sess-1"));
        assert_eq!(cm["author"]["via"], json!("Mcp"));
        assert_eq!(cm["author"]["invoked_by"]["name"], json!("ada"));
    }
    let lines_anchor = all.iter().find(|cm| cm["body"] == json!("nit")).unwrap()["anchor"].clone();
    assert_eq!(lines_anchor["type"], json!("Lines"));
    assert_eq!(lines_anchor["side"], json!("Head"));
    assert_ne!(
        lines_anchor["context_hash"],
        json!("0000000000000000"),
        "daemon replaced the placeholder hash"
    );
    let resolved = comments["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == json!(thread_id))
        .unwrap();
    assert_eq!(resolved["resolution"]["type"], json!("Resolved"));
    assert_eq!(resolved["replies"].as_array().unwrap().len(), 1);

    // Same log the human sees, with the same authorship.
    let last = h.daemon.core().last_seq().unwrap().unwrap();
    let events = h.daemon.core().events_after(None).unwrap();
    assert_eq!(events.last().unwrap().seq, last);
    assert!(matches!(
        events.last().unwrap().author,
        Author::Agent { .. }
    ));

    // Agents cannot mark viewed: the tool is not even offered.
    let out = s
        .handle(req(
            8,
            "tools/call",
            json!({ "name": "mark_viewed", "arguments": {} }),
        ))
        .await
        .unwrap();
    assert_eq!(out.error.unwrap().code, -32602);
}

#[tokio::test]
async fn subscribe_events_long_polls_and_resumes() {
    let h = start();
    let c = human(&h).await;
    let (ws, rid) = seed(&h, &c).await;
    let mut s = server(&h);
    init(&mut s).await;
    let created = call(
        &mut s,
        "create_review",
        json!({ "workspace_id": ws, "title": "r", "targets": main_feature(&rid) }),
    )
    .await;
    let review_id = created["review"]["id"].as_str().unwrap().to_string();
    let rid_typed: nits_protocol::ReviewId = review_id.parse().unwrap();

    // Nothing yet: returns empty at the deadline.
    let empty = call(
        &mut s,
        "subscribe_events",
        json!({ "review_id": review_id, "timeout_ms": 50 }),
    )
    .await;
    assert_eq!(empty["events"].as_array().unwrap().len(), 0);
    let since = empty["last_seq"].as_u64().unwrap();

    // A human requests a review while the agent is waiting.
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        c.request(Request::Mutate {
            client_seq: ClientSeq::new(9),
            mutation: Mutation::RequestReview {
                review_id: rid_typed,
                agent: "claude-code".into(),
                note: "please".into(),
            },
        })
        .await
        .unwrap();
    });
    let got = call(
        &mut s,
        "subscribe_events",
        json!({ "awaiting_agent": "claude-code", "since_seq": since, "timeout_ms": 5000 }),
    )
    .await;
    let events = got["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["body"]["type"], json!("ReviewRequested"));
    assert_eq!(events[0]["body"]["note"], json!("please"));
    assert_eq!(got["last_seq"], events[0]["seq"]);

    // Resuming after it yields nothing new.
    let again = call(
        &mut s,
        "subscribe_events",
        json!({ "review_id": review_id, "since_seq": got["last_seq"], "timeout_ms": 50 }),
    )
    .await;
    assert_eq!(again["events"].as_array().unwrap().len(), 0);

    // Rewinding on the same MCP session must replay persisted events even
    // though a previous scope already delivered them to this connection.
    let replay = call(
        &mut s,
        "subscribe_events",
        json!({ "review_id": review_id, "since_seq": since, "timeout_ms": 50 }),
    )
    .await;
    assert_eq!(replay["events"], got["events"]);
    assert_eq!(replay["last_seq"], got["last_seq"]);

    let _ = Response::Unsubscribed;
}
