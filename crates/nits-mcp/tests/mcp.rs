//! MCP server against a real daemon (plan 2.5): JSON-RPC conformance, each
//! tool round-trips through core, and agent provenance is structural.

use std::sync::Arc;

use nits_mcp::jsonrpc::{Incoming, Outgoing};
use nits_mcp::server::AgentIdentity;
use nits_mcp::{Endpoint, Server};
use nits_protocol::{
    Author, BuildInfo, ClientId, ClientSeq, Human, Mutation, RepoId, Request, Response, ReviewId,
    Seq, ThreadId, WorkspaceId,
};
use nits_review_core::DataDir;
use nits_test_support::{RepoBuilder, TestRepo, files};
use nitsd::Daemon;
use nitsd::client::{Client, Identity};
use nitsd::server::UnixServer;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct Harness {
    dir: tempfile::TempDir,
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
    start_with_repo(small_repo())
}

fn start_with_repo(repo: TestRepo) -> Harness {
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
        dir,
        socket,
        shutdown,
        repo,
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
    server_with_session(h, "sess-1")
}

fn server_with_session(h: &Harness, session_id: &str) -> Server {
    server_at(&h.socket, session_id)
}

fn server_at(socket: &std::path::Path, session_id: &str) -> Server {
    server_in_context(
        nits_config::Context::Local {
            data_dir: None,
            socket: Some(socket.to_owned()),
        },
        session_id,
    )
}

fn server_in_context(context: nits_config::Context, session_id: &str) -> Server {
    Server::new(
        Endpoint {
            context,
            start: nitsd::contexts::StartPolicy::RequireRunning,
        },
        AgentIdentity {
            model: "test-model".into(),
            session_id: session_id.into(),
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

/// Cut the transport deterministically while the real daemon keeps its store.
async fn forward_connections(
    listener: tokio::net::UnixListener,
    target: std::path::PathBuf,
    shutdown: CancellationToken,
) {
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            accepted = listener.accept() => {
                let (mut incoming, _) = accepted.unwrap();
                let destination = target.clone();
                tasks.spawn(async move {
                    let mut outgoing = tokio::net::UnixStream::connect(destination).await.unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await;
                });
            }
        }
    }
    tasks.shutdown().await;
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
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, result["structuredContent"]);
    result["structuredContent"].clone()
}

/// Match a compact receipt to the authoritative event persisted by the daemon.
fn committed(h: &Harness, receipt: &Value) -> Value {
    let seq: Seq = serde_json::from_value(receipt["seq"].clone()).unwrap();
    let event = h
        .daemon
        .core()
        .events_after(None)
        .unwrap()
        .into_iter()
        .find(|event| event.seq == seq)
        .expect("receipt sequence must name a committed event");
    serde_json::to_value(event).unwrap()
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
    let ws = WorkspaceId::from_parts(1, 1);
    let rid = RepoId::from_parts(1, 1);
    seed_workspace(c, &h.repo, ws, rid, ClientSeq::new(1)).await;
    (ws.to_string(), rid.to_string())
}

async fn seed_workspace(
    c: &Client,
    repo: &TestRepo,
    workspace_id: WorkspaceId,
    repo_id: RepoId,
    mut client_seq: ClientSeq,
) {
    for mutation in [
        Mutation::CreateWorkspace {
            workspace_id,
            name: "w".into(),
        },
        Mutation::AttachRepo {
            workspace_id,
            repo_id,
            path: repo.path().to_str().unwrap().into(),
            display_name: "r".into(),
        },
    ] {
        let response = c
            .request(Request::Mutate {
                client_seq,
                mutation,
            })
            .await
            .unwrap();
        assert!(
            matches!(response, Response::Committed { .. }),
            "{response:?}"
        );
        client_seq = client_seq.next();
    }
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
    let instructions = r["instructions"].as_str().unwrap();
    for term in [
        "get_session_identity",
        "set_session_identity",
        "author.name",
        "awaiting_agent",
    ] {
        assert!(
            instructions.contains(term),
            "missing {term}: {instructions}"
        );
    }

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
            "ensure_directory_review",
            "update_review_target",
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
            "get_session_identity",
            "set_session_identity",
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
async fn session_identity_changes_future_authorship_and_preserves_other_sessions() {
    let h = start();
    let c = human(&h).await;
    let (ws, rid) = seed(&h, &c).await;
    let mut s = server(&h);
    let mut other = server_with_session(&h, "sess-2");
    init(&mut s).await;
    init(&mut other).await;
    let original = call(&mut s, "get_session_identity", json!({})).await;
    let other_identity = call(&mut other, "get_session_identity", json!({})).await;
    assert_eq!(original["author"]["name"], "claude-code");
    assert_eq!(original["author"]["model"], "test-model");
    let created = call(
        &mut s,
        "create_review",
        json!({ "workspace_id": ws, "title": "r", "targets": main_feature(&rid) }),
    )
    .await;
    let review_id = &created["review_id"];
    let before = call(
        &mut s,
        "add_comment",
        json!({ "review_id": review_id, "body": "before" }),
    )
    .await;
    let previous_events = h.daemon.core().events_after(None).unwrap();
    let updated = call(
        &mut s,
        "set_session_identity",
        json!({ "name": "reviewer-a", "model": "example-model" }),
    )
    .await;
    let mut expected = original.clone();
    expected["author"]["name"] = json!("reviewer-a");
    expected["author"]["model"] = json!("example-model");
    assert_eq!(updated, expected);
    assert_eq!(
        call(&mut s, "get_session_identity", json!({})).await,
        updated
    );
    assert_eq!(
        call(&mut other, "get_session_identity", json!({})).await,
        other_identity
    );
    assert_eq!(
        h.daemon.core().events_after(None).unwrap(),
        previous_events,
        "identity writes no event"
    );

    let after = call(
        &mut s,
        "add_comment",
        json!({ "review_id": review_id, "body": "after" }),
    )
    .await;
    let reply = call(
        &mut s,
        "reply",
        json!({ "review_id": review_id, "thread_id": before["thread_id"], "body": "reply" }),
    )
    .await;
    let other_comment = call(
        &mut other,
        "add_comment",
        json!({ "review_id": review_id, "body": "other" }),
    )
    .await;
    let after_event = committed(&h, &after);
    assert_ne!(
        committed(&h, &before)["client_id"],
        after_event["client_id"]
    );
    assert_eq!(
        after_event["client_seq"], 1,
        "new connection has its own mutation sequence"
    );

    let persisted = h.daemon.core().events_after(None).unwrap();
    assert_eq!(&persisted[..previous_events.len()], previous_events);
    let comments = call(&mut s, "list_comments", json!({ "review_id": review_id })).await;
    for (result, identity) in [
        (&before, &original),
        (&after, &updated),
        (&reply, &updated),
        (&other_comment, &other_identity),
    ] {
        let comment = comments["comments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|comment| comment["id"] == result["comment_id"])
            .unwrap();
        assert_eq!(comment["author"], identity["author"]);
        let event = persisted
            .iter()
            .find(|event| json!(event.seq) == result["seq"])
            .unwrap();
        assert_eq!(json!(event.author), identity["author"]);
    }
}

#[tokio::test]
async fn session_name_routes_requests_across_model_updates() {
    let h = start();
    let c = human(&h).await;
    let (ws, rid) = seed(&h, &c).await;
    let mut reviewer = server(&h);
    let mut requester = server_with_session(&h, "sess-2");
    init(&mut reviewer).await;
    init(&mut requester).await;
    let created = call(
        &mut requester,
        "create_review",
        json!({ "workspace_id": ws, "title": "r", "targets": main_feature(&rid) }),
    )
    .await;
    let identity = call(
        &mut reviewer,
        "set_session_identity",
        json!({ "name": "reviewer-a", "model": "example-model" }),
    )
    .await;
    let since = h.daemon.core().last_seq().unwrap().unwrap();
    let invitation = call(&mut requester, "request_review", json!({ "review_id": created["review_id"], "agent": identity["author"]["name"], "note": "please review" })).await;
    call(&mut requester, "request_review", json!({ "review_id": created["review_id"], "agent": "another-agent", "note": "other work" })).await;
    let model_update = call(
        &mut reviewer,
        "set_session_identity",
        json!({ "name": identity["author"]["name"], "model": "next-model" }),
    )
    .await;
    assert_eq!(model_update["author"]["name"], identity["author"]["name"]);
    let polled = call(&mut reviewer, "subscribe_events", json!({ "awaiting_agent": model_update["author"]["name"], "since_seq": since, "timeout_ms": 1000 })).await;
    assert_eq!(polled["events"], json!([committed(&h, &invitation)]));
    assert_eq!(polled["last_seq"], invitation["seq"]);
    let resumed = call(&mut reviewer, "subscribe_events", json!({ "awaiting_agent": model_update["author"]["name"], "since_seq": polled["last_seq"], "timeout_ms": 10 })).await;
    assert_eq!(resumed["events"], json!([]));
}

#[tokio::test]
async fn identity_survives_reconnect_and_failed_update_leaves_it_unchanged() {
    let h = start();
    let c = human(&h).await;
    let (ws, rid) = seed(&h, &c).await;
    let proxy_socket = h.socket.with_extension("proxy.sock");
    let listener = tokio::net::UnixListener::bind(&proxy_socket).unwrap();
    let shutdown = CancellationToken::new();
    let proxy = tokio::spawn(forward_connections(
        listener,
        h.socket.clone(),
        shutdown.clone(),
    ));
    let mut s = server_at(&proxy_socket, "sess-1");
    for (name, arguments) in [
        ("get_session_identity", json!({})),
        (
            "set_session_identity",
            json!({ "name": "reviewer-a", "model": "example-model" }),
        ),
    ] {
        assert!(
            call_err(&mut s, name, arguments)
                .await
                .contains("initialize first")
        );
    }
    init(&mut s).await;
    let identity = call(
        &mut s,
        "set_session_identity",
        json!({ "name": "reviewer-a", "model": "example-model" }),
    )
    .await;
    let created = call(
        &mut s,
        "create_review",
        json!({ "workspace_id": ws, "title": "r", "targets": main_feature(&rid) }),
    )
    .await;
    let previous_events = h.daemon.core().events_after(None).unwrap();
    shutdown.cancel();
    proxy.await.unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            s.client().unwrap().next_unsolicited()
        )
        .await
        .unwrap()
        .is_none()
    );
    assert_eq!(
        call(&mut s, "get_session_identity", json!({})).await,
        identity,
        "read needs no daemon connection"
    );
    let error = call_err(
        &mut s,
        "set_session_identity",
        json!({ "name": "rejected-name", "model": "rejected-model" }),
    )
    .await;
    assert!(error.contains("cannot connect"), "{error}");
    assert_eq!(
        call(&mut s, "get_session_identity", json!({})).await,
        identity
    );

    std::fs::remove_file(&proxy_socket).unwrap();
    let listener = tokio::net::UnixListener::bind(&proxy_socket).unwrap();
    let shutdown = CancellationToken::new();
    let proxy = tokio::spawn(forward_connections(
        listener,
        h.socket.clone(),
        shutdown.clone(),
    ));
    let comment = call(
        &mut s,
        "add_comment",
        json!({ "review_id": created["review_id"], "body": "after reconnect" }),
    )
    .await;
    let event = committed(&h, &comment);
    assert_eq!(event["author"], identity["author"]);
    assert_ne!(committed(&h, &created)["client_id"], event["client_id"]);
    assert_eq!(event["client_seq"], 1);
    let persisted = h.daemon.core().events_after(None).unwrap();
    assert_eq!(&persisted[..previous_events.len()], previous_events);
    assert_eq!(json!(persisted.last().unwrap().author), identity["author"]);
    assert_eq!(
        persisted.len(),
        previous_events.len() + 1,
        "failed update appends nothing"
    );
    shutdown.cancel();
    proxy.await.unwrap();
    std::fs::remove_file(&proxy_socket).unwrap();
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
    let review_id = created["review_id"].as_str().unwrap().to_string();
    assert_eq!(
        created,
        json!({ "review_id": review_id, "seq": created["seq"] })
    );
    assert_eq!(committed(&h, &created)["body"]["type"], "ReviewCreated");

    let reviews = call(&mut s, "list_reviews", json!({ "workspace_id": ws })).await;
    assert_eq!(reviews["reviews"][0]["title"], json!("agent review"));

    let review = call(&mut s, "get_review", json!({ "review_id": review_id })).await;
    assert!(review["resolved"].is_array(), "targets resolved on create");
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
    assert_eq!(
        updated,
        json!({
            "review_id": review_id, "status": "Archived", "seq": updated["seq"]
        })
    );
    assert_eq!(committed(&h, &updated)["body"]["type"], "ReviewUpdated");
    let review = call(&mut s, "get_review", json!({ "review_id": review_id })).await;
    assert_eq!(review["review"]["status"], "Archived");
    assert_eq!(review["review"]["title"], "renamed");
}

#[tokio::test]
async fn bounded_files_keep_review_blobs_and_absolute_lines_across_chunks() {
    let base = (1..=1030)
        .map(|n| format!("base {n}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let head = base.replace("base", "head");
    let h = start_with_repo(
        RepoBuilder::new()
            .commit(
                "base",
                files!["changed.txt" => &base, "unchanged.txt" => &base],
            )
            .branch("feature")
            .commit("head", files!["changed.txt" => &head])
            .build()
            .unwrap(),
    );
    let c = human(&h).await;
    let (ws, rid) = seed(&h, &c).await;
    let mut s = server(&h);
    init(&mut s).await;
    let created = call(
        &mut s,
        "create_review",
        json!({
            "workspace_id": ws, "title": "ranges", "targets": main_feature(&rid)
        }),
    )
    .await;
    let cases = [
        ("changed.txt", "Head", &head, "feature:changed.txt"),
        ("changed.txt", "Base", &base, "main:changed.txt"),
        ("unchanged.txt", "Head", &base, "feature:unchanged.txt"),
        ("unchanged.txt", "Base", &base, "main:unchanged.txt"),
    ];
    let oids: Vec<_> = cases
        .iter()
        .map(|(_, _, _, rev)| h.repo.git(&["rev-parse", rev]).unwrap())
        .collect();
    // Both mutable refs and the checkout now disagree with the review snapshot.
    h.repo
        .write_file("changed.txt", b"later revision\n")
        .unwrap();
    h.repo.write_file("unchanged.txt", b"also later\n").unwrap();
    h.repo.git(&["add", "."]).unwrap();
    h.repo.git(&["commit", "-qm", "later"]).unwrap();
    h.repo.git(&["branch", "-f", "main", "feature"]).unwrap();
    h.repo
        .write_file("changed.txt", b"uncommitted checkout\n")
        .unwrap();

    for ((path, side, source, _), oid) in cases.into_iter().zip(oids) {
        let args = json!({ "review_id": created["review_id"], "repo_id": rid, "path": path, "side": side });
        let full = call(&mut s, "get_file", args.clone()).await;
        let expected: Vec<_> = source
            .lines()
            .enumerate()
            .map(|(i, line)| format!("{:>5}│{line}\n", i + 1))
            .collect();
        assert_eq!(full["text"], expected.concat());
        assert_eq!(full["blob_oid"], oid);
        assert_eq!(
            full["lines"],
            json!({ "total_lines": 1030, "returned_range": { "start": 1, "end": 1030 } })
        );
        let mut args = args;
        args["start_line"] = json!(995);
        args["end_line"] = json!(1015);
        let bounded = call(&mut s, "get_file", args).await;
        assert_eq!(bounded["text"], expected[994..1015].concat());
        assert_eq!(
            bounded["lines"],
            json!({ "total_lines": 1030, "returned_range": { "start": 995, "end": 1015 } })
        );
        for field in ["repo_id", "path", "side", "blob_oid", "lang", "content"] {
            assert_eq!(bounded[field], full[field], "{path} {side} {field}");
        }
        assert_eq!(bounded["text"].as_str().unwrap().lines().count(), 21);
    }
}

#[tokio::test]
async fn file_ranges_define_eof_empty_and_binary_behavior() {
    let h = start_with_repo(RepoBuilder::new()
        .commit("base", files![
            "empty.txt" => "", "binary.dat" => b"binary\0content",
            "terminated.txt" => "one\ntwo\n", "unterminated.txt" => "one\ntwo",
            "crlf.txt" => "one\r\ntwo\r\n", "blank.txt" => "\n", "last-blank.txt" => "one\n\n"
        ])
        .branch("feature").build().unwrap());
    let c = human(&h).await;
    let (ws, rid) = seed(&h, &c).await;
    let mut s = server(&h);
    init(&mut s).await;
    let created = call(
        &mut s,
        "create_review",
        json!({
            "workspace_id": ws, "title": "edges", "targets": main_feature(&rid)
        }),
    )
    .await;
    for (path, source) in [
        ("empty.txt", ""),
        ("terminated.txt", "one\ntwo\n"),
        ("unterminated.txt", "one\ntwo"),
        ("crlf.txt", "one\ntwo\n"),
        ("blank.txt", "\n"),
        ("last-blank.txt", "one\n\n"),
    ] {
        let source: Vec<_> = source
            .lines()
            .enumerate()
            .map(|(i, line)| format!("{:>5}│{line}\n", i + 1))
            .collect();
        let total = source.len();
        let args = json!({ "review_id": created["review_id"], "path": path });
        let full = call(&mut s, "get_file", args.clone()).await;
        assert_eq!(full["text"], source.concat());
        let range = if total == 0 {
            Value::Null
        } else {
            json!({ "start": 1, "end": total })
        };
        assert_eq!(
            full["lines"],
            json!({ "total_lines": total, "returned_range": range })
        );
        for (start, end) in [
            (1_u32, 1_u32),
            (1, 2),
            (1, u32::MAX),
            (2, 20),
            (3, 3),
            (u32::MAX, u32::MAX),
        ] {
            let mut args = args.clone();
            args["start_line"] = json!(start);
            args["end_line"] = json!(end);
            let bounded = call(&mut s, "get_file", args).await;
            let selected = source
                .iter()
                .skip(start as usize - 1)
                .take((end - start + 1) as usize)
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(bounded["text"], selected.concat(), "{path} {start}..={end}");
            let range = if selected.is_empty() {
                Value::Null
            } else {
                json!({ "start": start, "end": (end as usize).min(total) })
            };
            assert_eq!(
                bounded["lines"],
                json!({ "total_lines": total, "returned_range": range })
            );
            assert_eq!(bounded["blob_oid"], full["blob_oid"]);
        }
    }
    let args = json!({ "review_id": created["review_id"], "path": "binary.dat" });
    let binary = call(&mut s, "get_file", args.clone()).await;
    assert_eq!(binary["text"], "(binary file)\n");
    assert_eq!(binary["content"], json!({ "type": "Binary" }));
    assert!(binary["lines"].is_null());
    let mut args = args;
    args["start_line"] = json!(1);
    args["end_line"] = json!(1);
    assert!(
        call_err(&mut s, "get_file", args)
            .await
            .contains("binary files have no source lines")
    );
}

#[tokio::test]
async fn get_file_rejects_invalid_line_bounds_before_reading_a_review() {
    let h = start();
    let mut s = server(&h);
    init(&mut s).await;
    for bounds in [
        json!({"start_line": 0, "end_line": 2}),
        json!({"start_line": 1, "end_line": 0}),
        json!({"start_line": 2, "end_line": 1}),
        json!({"start_line": 1}),
        json!({"end_line": 2}),
        json!({"start_line": null, "end_line": 2}),
        json!({"start_line": 1, "end_line": null}),
        json!({"start_line": -1, "end_line": 2}),
        json!({"start_line": 1.5, "end_line": 2}),
        json!({"start_line": 1, "end_line": 4_294_967_296_u64}),
        json!({"start_line": "1", "end_line": 2}),
    ] {
        let mut args = bounds;
        args["review_id"] = json!(ReviewId::from_parts(1, 1));
        args["path"] = json!("a.rs");
        let error = call_err(&mut s, "get_file", args.clone()).await;
        assert!(error.contains("invalid params"), "{args}: {error}");
    }
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
    let review_id = created["review_id"].as_str().unwrap().to_string();

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
        committed(&h, &whole)["body"]["comment"]["anchor"]["type"],
        json!("Review")
    );
    let suggestion = call(
        &mut s,
        "suggest",
        json!({
            "review_id": review_id, "path": "a.rs", "start_line": 1,
            "patch": "@@ -1 +1 @@\n-fn a() { 1; }\n+fn a() { 2; }\n", "body": "use 2"
        }),
    )
    .await;
    assert_eq!(suggestion["comment_id"], suggestion["thread_id"]);
    assert_eq!(
        committed(&h, &suggestion)["body"]["comment"]["kind"]["type"],
        "Suggestion"
    );
    let reply = call(
        &mut s,
        "reply",
        json!({ "review_id": review_id, "thread_id": thread_id, "body": "ack" }),
    )
    .await;
    assert_eq!(reply["thread_id"], thread_id);
    assert_ne!(reply["comment_id"], reply["thread_id"]);
    for result in [&line, &whole, &suggestion, &reply] {
        let event = committed(&h, result);
        assert_eq!(
            result,
            &json!({
                "comment_id": event["body"]["comment"]["id"],
                "thread_id": event["body"]["comment"]["thread_id"],
                "seq": event["seq"]
            })
        );
    }
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
    assert_thread_resolution(
        &mut s,
        &h,
        review_id.parse().unwrap(),
        thread_id.parse().unwrap(),
    )
    .await;

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

async fn assert_thread_resolution(
    s: &mut Server,
    h: &Harness,
    review_id: ReviewId,
    thread_id: ThreadId,
) {
    let resolved = call(
        s,
        "resolve",
        json!({ "review_id": review_id, "thread_id": thread_id }),
    )
    .await;

    assert_eq!(
        resolved,
        json!({
            "review_id": review_id, "thread_id": thread_id,
            "resolution": "Resolved", "seq": resolved["seq"]
        })
    );
    assert_eq!(committed(h, &resolved)["body"]["type"], "ThreadResolved");

    let comments = call(s, "list_comments", json!({ "review_id": review_id })).await;
    let resolved = comments["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == json!(thread_id))
        .unwrap();
    assert_eq!(resolved["resolution"]["type"], json!("Resolved"));
    assert_eq!(resolved["replies"].as_array().unwrap().len(), 1);

    let reopened = call(
        s,
        "resolve",
        json!({ "review_id": review_id, "thread_id": thread_id, "resolved": false }),
    )
    .await;
    assert_eq!(
        reopened,
        json!({
            "review_id": review_id, "thread_id": thread_id,
            "resolution": "Open", "seq": reopened["seq"]
        })
    );
    assert_eq!(committed(h, &reopened)["body"]["type"], "ThreadUnresolved");
    let comments = call(s, "list_comments", json!({ "review_id": review_id })).await;
    let thread = comments["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|thread| thread["id"] == json!(thread_id))
        .unwrap();
    assert_eq!(thread["resolution"]["type"], "Open");

    // Same log the human sees, with the same authorship.
    let last = h.daemon.core().last_seq().unwrap().unwrap();
    let events = h.daemon.core().events_after(None).unwrap();
    assert_eq!(events.last().unwrap().seq, last);
    assert!(matches!(
        events.last().unwrap().author,
        Author::Agent { .. }
    ));
}

#[tokio::test]
async fn mutation_receipts_resume_full_events_including_follow_up_events() {
    let h = start();
    let c = human(&h).await;
    let (ws, rid) = seed(&h, &c).await;
    let mut s = server(&h);
    init(&mut s).await;
    let created = call(
        &mut s,
        "create_review",
        json!({ "workspace_id": ws, "title": "r", "targets": [{
            "repo_id": rid, "base": { "type": "Head" }, "head": { "type": "WorkingTree" }
        }] }),
    )
    .await;
    let review_id = &created["review_id"];
    let created_event = committed(&h, &created);
    assert_eq!(created_event["body"]["review"]["id"], *review_id);
    let targets = call(
        &mut s,
        "subscribe_events",
        json!({ "review_id": review_id, "since_seq": created["seq"], "timeout_ms": 1000 }),
    )
    .await;
    assert_eq!(targets["events"].as_array().unwrap().len(), 1);
    assert_eq!(
        targets["events"][0]["body"]["type"],
        "ReviewTargetsResolved"
    );
    assert_eq!(targets["last_seq"], targets["events"][0]["seq"]);

    // A large comment must not be echoed by either MCP result representation.
    let body = "A detailed review note. ".repeat(1000);
    let comment = call(
        &mut s,
        "add_comment",
        json!({ "review_id": review_id, "path": "a.rs", "start_line": 1, "body": body }),
    )
    .await;
    assert!(serde_json::to_vec(&comment).unwrap().len() < 200);
    let replay = call(
        &mut s,
        "subscribe_events",
        json!({ "review_id": review_id, "since_seq": targets["last_seq"], "timeout_ms": 1000 }),
    )
    .await;
    assert_eq!(replay["events"], json!([committed(&h, &comment)]));
    assert_eq!(replay["events"][0]["body"]["comment"]["body"], body);
    assert_eq!(replay["events"][0]["author"]["type"], "Agent");
    assert_eq!(replay["last_seq"], comment["seq"]);

    let archived = call(
        &mut s,
        "update_review",
        json!({ "review_id": review_id, "title": "r", "status": "Archived" }),
    )
    .await;
    let after_comment = call(
        &mut s,
        "subscribe_events",
        json!({ "review_id": review_id, "since_seq": comment["seq"], "timeout_ms": 1000 }),
    )
    .await;
    assert_eq!(after_comment["events"], json!([committed(&h, &archived)]));

    // Reopening commits a status event followed by target resolution and
    // reanchoring. Returning a snapshot's later watermark would skip them.
    h.repo.write_file("a.rs", b"fn changed() {}\n").unwrap();
    let reopened = call(
        &mut s,
        "update_review",
        json!({ "review_id": review_id, "title": "r", "status": "Open" }),
    )
    .await;
    assert_eq!(reopened["status"], "Open");
    let follow_up = call(
        &mut s,
        "subscribe_events",
        json!({ "review_id": review_id, "since_seq": reopened["seq"], "timeout_ms": 1000 }),
    )
    .await;
    let events = follow_up["events"].as_array().unwrap();
    let kinds: Vec<_> = events.iter().map(|event| &event["body"]["type"]).collect();
    assert_eq!(
        kinds,
        [&json!("ReviewTargetsResolved"), &json!("CommentReanchored")]
    );
    let seq: Seq = serde_json::from_value(reopened["seq"].clone()).unwrap();
    assert_eq!(
        follow_up["events"],
        json!(h.daemon.core().events_after(Some(seq)).unwrap())
    );
    assert_eq!(follow_up["last_seq"], events.last().unwrap()["seq"]);
}

#[tokio::test]
async fn subscribe_events_rejects_conflicting_scopes() {
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
    let review_id = &created["review_id"];
    for arguments in [
        json!({ "review_id": review_id, "workspace_id": ws, "timeout_ms": 1 }),
        json!({ "review_id": review_id, "awaiting_agent": "reviewer-a", "timeout_ms": 1 }),
        json!({ "workspace_id": ws, "awaiting_agent": "reviewer-a", "timeout_ms": 1 }),
        json!({ "review_id": review_id, "workspace_id": ws, "awaiting_agent": "reviewer-a", "timeout_ms": 1 }),
    ] {
        let error = call_err(&mut s, "subscribe_events", arguments).await;
        assert!(
            error.contains("review_id, workspace_id, and awaiting_agent are mutually exclusive"),
            "{error}"
        );
        assert!(
            error.contains("provide at most one non-null scope filter"),
            "{error}"
        );
    }
}

#[tokio::test]
async fn subscribe_events_preserves_each_single_scope_and_all_events() {
    let h = start();
    let c = human(&h).await;
    let (ws, rid) = seed(&h, &c).await;
    let other_repo = small_repo();
    let other_workspace = WorkspaceId::from_parts(1, 2);
    let other_repo_id = RepoId::from_parts(1, 2);
    seed_workspace(
        &c,
        &other_repo,
        other_workspace,
        other_repo_id,
        ClientSeq::new(3),
    )
    .await;
    let mut s = server(&h);
    init(&mut s).await;
    let mut reviews = Vec::new();
    for (workspace, repo, title) in [
        (ws.clone(), rid.clone(), "first"),
        (ws.clone(), rid, "second"),
        (
            other_workspace.to_string(),
            other_repo_id.to_string(),
            "other workspace review",
        ),
    ] {
        let created = call(
            &mut s,
            "create_review",
            json!({ "workspace_id": workspace, "title": title, "targets": main_feature(&repo) }),
        )
        .await;
        reviews.push(created["review_id"].clone());
    }
    let since = h.daemon.core().last_seq().unwrap().unwrap();
    let mut events = Vec::new();
    for (review, agent) in [
        (&reviews[0], "reviewer-a"),
        (&reviews[1], "reviewer-a"),
        (&reviews[2], "reviewer-a"),
        (&reviews[0], "reviewer-b"),
    ] {
        let requested = call(
            &mut s,
            "request_review",
            json!({ "review_id": review, "agent": agent, "note": "please review" }),
        )
        .await;
        assert_eq!(
            requested,
            json!({
                "review_id": review, "agent": agent, "seq": requested["seq"]
            })
        );
        events.push(committed(&h, &requested));
    }
    let updated = call(
        &mut s,
        "update_review",
        json!({ "review_id": reviews[0], "title": "renamed", "status": "Open" }),
    )
    .await;
    events.push(committed(&h, &updated));

    for (mut arguments, expected) in [
        (json!({}), events.clone()),
        (
            json!({ "review_id": reviews[0] }),
            vec![events[0].clone(), events[3].clone(), events[4].clone()],
        ),
        (
            json!({ "workspace_id": ws }),
            vec![
                events[0].clone(),
                events[1].clone(),
                events[3].clone(),
                events[4].clone(),
            ],
        ),
        (
            json!({ "awaiting_agent": "reviewer-a" }),
            vec![events[0].clone(), events[1].clone(), events[2].clone()],
        ),
    ] {
        arguments["since_seq"] = json!(since);
        arguments["timeout_ms"] = json!(1000);
        let polled = call(&mut s, "subscribe_events", arguments.clone()).await;
        assert_eq!(polled["events"], json!(expected), "scope: {arguments}");
        assert_eq!(polled["last_seq"], expected.last().unwrap()["seq"]);
    }
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
    let review_id = created["review_id"].as_str().unwrap().to_string();
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

#[tokio::test]
async fn directory_bootstrap_is_mcp_only_idempotent_and_honors_explicit_refs() {
    let h = start();
    let mut s = server(&h);
    init(&mut s).await;
    let empty = call(&mut s, "list_workspaces", json!({})).await;
    assert_eq!(empty["workspaces"], json!([]));
    let args = json!({"path": h.repo.path(), "base": {"type": "Head"}});
    let first = call(&mut s, "ensure_directory_review", args.clone()).await;
    assert_eq!(first["outcome"], "Created");
    assert_eq!(first["base"], args["base"]);
    assert_eq!(first["head"], json!({"type": "WorkingTree"}));
    let seq = h.daemon.core().last_seq().unwrap();
    std::fs::create_dir(h.repo.path().join("nested")).unwrap();
    let again = call(
        &mut s,
        "ensure_directory_review",
        json!({
            "path": h.repo.path().join("nested/.."), "base": args["base"]
        }),
    )
    .await;
    for key in ["workspace_id", "repo_id", "review_id", "base", "head"] {
        assert_eq!(first[key], again[key], "{key}");
    }
    assert_eq!(again["outcome"], "Reused");
    assert_eq!(h.daemon.core().last_seq().unwrap(), seq);
    let different = call(
        &mut s,
        "ensure_directory_review",
        json!({
            "path": h.repo.path(), "base": {"type": "Branch", "name": "main"}
        }),
    )
    .await;
    assert_ne!(different["review_id"], first["review_id"]);
    assert_eq!(different["workspace_id"], first["workspace_id"]);
    assert_eq!(different["base"], json!({"type": "Branch", "name": "main"}));
    let pinned_args = json!({"path": h.repo.path(), "base": {"type": "Head"},
        "head": {"type": "Commit", "oid": h.repo.rev_parse("HEAD").unwrap()}});
    let pinned = call(&mut s, "ensure_directory_review", pinned_args.clone()).await;
    let repeated = call(&mut s, "ensure_directory_review", pinned_args).await;
    assert_eq!(pinned["review_id"], repeated["review_id"]);
    assert_eq!(pinned["head"], repeated["head"]);
    let events = h.daemon.core().events_after(None).unwrap();
    assert!(
        events
            .iter()
            .all(|event| matches!(event.author, Author::Agent { .. }))
    );
}

#[tokio::test]
async fn failed_bootstrap_paths_and_refs_leave_no_partial_state() {
    let h = start();
    let mut s = server(&h);
    init(&mut s).await;
    for args in [
        json!({"path": h.repo.path(), "base": {"type": "WorkingTree"}}),
        json!({"path": h.repo.path(), "base": {"type": "Branch", "name": "missing"}}),
        json!({"path": h.repo.path(), "head": {"type": "Branch", "name": "missing"}}),
        json!({"path": h.dir.path()}),
    ] {
        assert!(
            !call_err(&mut s, "ensure_directory_review", args)
                .await
                .is_empty()
        );
        assert!(h.daemon.core().workspaces().unwrap().is_empty());
        assert_eq!(h.daemon.core().last_seq().unwrap(), None);
    }
}

#[tokio::test]
async fn concurrent_directory_bootstrap_allocates_only_one_review() {
    let h = start();
    let mut one = server(&h);
    let mut two = server(&h);
    init(&mut one).await;
    init(&mut two).await;
    let args = json!({"path": h.repo.path()});
    let (first, second) = tokio::join!(
        call(&mut one, "ensure_directory_review", args.clone()),
        call(&mut two, "ensure_directory_review", args),
    );
    for key in ["workspace_id", "repo_id", "review_id"] {
        assert_eq!(first[key], second[key]);
    }
    let workspaces = h.daemon.core().workspaces().unwrap();
    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0].repos.len(), 1);
    assert_eq!(h.daemon.core().reviews(workspaces[0].id).unwrap().len(), 1);
}

#[tokio::test]
async fn target_updates_pin_working_tree_and_keep_threads_history_and_anchors() {
    let h = start();
    let mut s = server(&h);
    init(&mut s).await;
    let boot = call(
        &mut s,
        "ensure_directory_review",
        json!({"path": h.repo.path()}),
    )
    .await;
    let review = &boot["review_id"];
    let repo = &boot["repo_id"];
    let note = call(
        &mut s,
        "add_comment",
        json!({
            "review_id": review, "repo_id": repo, "path": "a.rs", "body": "Keep this file"
        }),
    )
    .await;
    call(
        &mut s,
        "reply",
        json!({"review_id": review, "thread_id": note["thread_id"], "body": "Agreed"}),
    )
    .await;
    let history = h.daemon.core().events_after(None).unwrap();
    h.repo
        .write_file("a.rs", b"// prefix\nfn a() { 1; }\nfn z() {}\n")
        .unwrap();
    h.repo.git(&["add", "a.rs"]).unwrap();
    h.repo.git(&["commit", "-q", "-m", "prefix"]).unwrap();
    let pinned = json!({"type": "Commit", "oid": h.repo.rev_parse("HEAD").unwrap()});
    let receipt = call(
        &mut s,
        "update_review_target",
        json!({
            "review_id": review, "repo_id": repo, "revision": {"type": "Head", "ref_spec": pinned}
        }),
    )
    .await;
    assert_eq!(
        committed(&h, &receipt)["body"]["type"],
        "ReviewTargetUpdated"
    );
    let detail = call(&mut s, "get_review", json!({"review_id": review})).await;
    assert_eq!(detail["review"]["id"], *review);
    assert_eq!(detail["review"]["targets"][0]["head"], pinned);
    assert_eq!(detail["threads"].as_array().unwrap().len(), 1);
    assert_eq!(detail["comments"].as_array().unwrap().len(), 2);
    let root = detail["comments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == note["comment_id"])
        .unwrap();
    assert_eq!(
        root["anchor"]["blob_oid"],
        h.repo.git(&["rev-parse", "HEAD:a.rs"]).unwrap()
    );
    assert_eq!(root["state"]["type"], "Live");
    let events = h.daemon.core().events_after(None).unwrap();
    assert_eq!(&events[..history.len()], history.as_slice());
    assert!(events[history.len()..].iter().any(|event| matches!(
        event.body,
        nits_protocol::EventBody::CommentReanchored { .. }
    )));
    h.repo
        .write_file("a.rs", b"uncommitted movement\n")
        .unwrap();
    let pinned_again = call(
        &mut s,
        "ensure_directory_review",
        json!({"path": h.repo.path(), "head": pinned}),
    )
    .await;
    assert_eq!(pinned_again["review_id"], *review);
}

#[tokio::test]
async fn target_updates_reject_missing_refs_and_can_change_base() {
    let h = start();
    let mut s = server(&h);
    init(&mut s).await;
    let boot = call(
        &mut s,
        "ensure_directory_review",
        json!({"path": h.repo.path()}),
    )
    .await;
    let review = &boot["review_id"];
    let repo = &boot["repo_id"];
    let before = call(&mut s, "get_review", json!({"review_id": review})).await;
    for revision in [
        json!({"type": "Base", "ref_spec": {"type": "Branch", "name": "missing"}}),
        json!({"type": "Head", "ref_spec": {"type": "Branch", "name": "missing"}}),
    ] {
        assert!(
            !call_err(
                &mut s,
                "update_review_target",
                json!({"review_id": review, "repo_id": repo, "revision": revision})
            )
            .await
            .is_empty()
        );
        assert_eq!(
            call(&mut s, "get_review", json!({"review_id": review})).await,
            before
        );
    }
    call(
        &mut s,
        "update_review_target",
        json!({"review_id": review, "repo_id": repo,
        "revision": {"type": "Base", "ref_spec": {"type": "Head"}}}),
    )
    .await;
    let changed = call(&mut s, "get_review", json!({"review_id": review})).await;
    assert_eq!(
        changed["review"]["targets"][0]["base"],
        json!({"type": "Head"})
    );
    assert_eq!(changed["threads"], before["threads"]);
}

#[tokio::test]
async fn remote_context_bootstrap_discovers_and_canonicalizes_paths_at_daemon() {
    let h = start();
    let ws = nitsd::server::WsServer::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let url = format!("ws://{}", ws.addr());
    tokio::spawn(ws.run(Arc::clone(&h.daemon), h.shutdown.clone()));
    let mut s = server_in_context(nits_config::Context::Ws { url }, "sess-1");
    init(&mut s).await;
    std::fs::create_dir(h.repo.path().join("nested")).unwrap();
    let alias = h.dir.path().join("checkout");
    std::os::unix::fs::symlink(h.repo.path(), &alias).unwrap();
    let first = call(
        &mut s,
        "ensure_directory_review",
        json!({"path": alias.join("nested")}),
    )
    .await;
    let again = call(
        &mut s,
        "ensure_directory_review",
        json!({"path": h.repo.path()}),
    )
    .await;
    assert_eq!(first["review_id"], again["review_id"]);
    let workspaces = call(&mut s, "list_workspaces", json!({})).await;
    assert_eq!(workspaces["workspaces"].as_array().unwrap().len(), 1);
    assert_eq!(
        workspaces["workspaces"][0]["repos"][0]["path"],
        json!(h.repo.path())
    );
}
