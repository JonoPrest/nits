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
