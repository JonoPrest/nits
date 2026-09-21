//! Exercise the transport scheduler with real daemons and pipelined JSON-RPC.

use std::time::Duration;

use tokio::io::{
    AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};

use super::*;

const DEADLINE: Duration = Duration::from_secs(3);

struct Session {
    input: WriteHalf<DuplexStream>,
    output: Lines<BufReader<ReadHalf<DuplexStream>>>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Session {
    fn new(server: Server) -> Self {
        let (caller, transport) = tokio::io::duplex(64 * 1024);
        let (input, output) = tokio::io::split(transport);
        let task = tokio::spawn(nits_mcp::serve(server, BufReader::new(input), output));
        let (output, input) = tokio::io::split(caller);
        Self {
            input,
            output: BufReader::new(output).lines(),
            task,
        }
    }

    async fn send(&mut self, value: Value) {
        let mut line = serde_json::to_vec(&value).unwrap();
        line.push(b'\n');
        self.input.write_all(&line).await.unwrap();
    }

    async fn tool(&mut self, id: Value, name: &str, args: Value) {
        self.send(json!({"jsonrpc":"2.0", "id":id, "method":"tools/call", "params":{"name":name,"arguments":args}})).await;
    }

    async fn ping(&mut self, id: Value) {
        self.send(json!({"jsonrpc":"2.0", "id":id, "method":"ping"}))
            .await;
    }

    async fn cancel(&mut self, id: Value) {
        self.send(json!({"jsonrpc":"2.0", "method":"notifications/cancelled", "params":{"requestId":id,"reason":"test complete"}})).await;
    }

    async fn next(&mut self) -> Value {
        let line = tokio::time::timeout(DEADLINE, self.output.next_line())
            .await
            .expect("ordinary requests must not wait for the 60-second event poll")
            .unwrap()
            .expect("MCP response");
        serde_json::from_str(&line).unwrap()
    }

    async fn stop(&mut self) {
        self.input.shutdown().await.unwrap();
        tokio::time::timeout(DEADLINE, &mut self.task)
            .await
            .expect("EOF must cancel and join pending waits")
            .unwrap()
            .unwrap();
        assert!(
            self.output.next_line().await.unwrap().is_none(),
            "no reply after EOF"
        );
    }
}

fn content(reply: &Value) -> &Value {
    assert!(reply.get("error").is_none(), "{reply}");
    assert!(reply["result"].get("isError").is_none(), "{reply}");
    &reply["result"]["structuredContent"]
}

async fn initialized(h: &Harness) -> Session {
    let mut server = server(h);
    init(&mut server).await;
    Session::new(server)
}

#[tokio::test]
async fn live_wait_allows_ping_read_and_ordered_mutation_without_losing_the_event() {
    let h = start();
    let review = shared_review(&h, "scheduled").await;
    let mut session = initialized(&h).await;
    session
        .tool(
            json!(10),
            "subscribe_events",
            json!({"awaiting_agent":"reviewer-a", "timeout_ms":60000}),
        )
        .await;
    session.ping(json!(11)).await;
    session.tool(json!(12), "list_workspaces", json!({})).await;
    session
        .tool(
            json!(13),
            "request_review",
            json!({"review_id":review,"agent":"reviewer-a","note":"after subscribe"}),
        )
        .await;
    let mut replies = std::collections::HashMap::new();
    for _ in 0..4 {
        let reply = session.next().await;
        replies.insert(reply["id"].as_u64().unwrap(), reply);
    }
    assert_eq!(replies[&11]["result"], json!({}));
    assert_eq!(
        content(&replies[&12])["workspaces"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let receipt = content(&replies[&13]);
    let poll = content(&replies[&10]);
    assert_eq!(poll["context"]["name"], "test");
    assert_eq!(poll["events"].as_array().unwrap().len(), 1);
    assert_eq!(poll["events"][0]["seq"], receipt["seq"]);
    assert_eq!(poll["events"][0]["author"]["name"], "claude-code");
    session.stop().await;
}

#[tokio::test]
async fn cancellation_is_prompt_typed_silent_and_request_ids_can_be_reused() {
    let h = start();
    let review = shared_review(&h, "cancellation").await;
    let mut session = initialized(&h).await;
    for id in [json!(7), json!("7")] {
        session
            .tool(
                id,
                "subscribe_events",
                json!({"awaiting_agent":"reviewer-a","timeout_ms":60000}),
            )
            .await;
    }
    session.ping(json!(8)).await;
    assert_eq!(session.next().await["id"], 8);
    session.cancel(json!(7)).await;
    session.cancel(json!("unknown")).await;
    session
        .send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":{}}}))
        .await;
    session
        .tool(
            json!(7),
            "subscribe_events",
            json!({"awaiting_agent":"different-agent","timeout_ms":0}),
        )
        .await;
    assert!(
        content(&session.next().await)["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    session
        .tool(
            json!(9),
            "request_review",
            json!({"review_id":review,"agent":"reviewer-a","note":"only string poll survives"}),
        )
        .await;
    let a = session.next().await;
    let b = session.next().await;
    let poll = if a["id"] == "7" {
        a
    } else {
        assert_eq!(b["id"], "7");
        b
    };
    assert_eq!(content(&poll)["events"].as_array().unwrap().len(), 1);
    session.cancel(json!("7")).await; // already completed is harmless
    session.ping(json!(10)).await;
    assert_eq!(session.next().await["id"], 10);
    session.stop().await;
}

#[tokio::test]
async fn simultaneous_polls_receive_their_own_events_and_eof_cleans_pending_waits() {
    let h = start();
    let review = shared_review(&h, "parallel polls").await;
    let mut session = initialized(&h).await;
    for id in [1, 2] {
        session
            .tool(
                json!(id),
                "subscribe_events",
                json!({"awaiting_agent":"reviewer-a","timeout_ms":60000}),
            )
            .await;
    }
    session
        .tool(
            json!(3),
            "request_review",
            json!({"review_id":review,"agent":"reviewer-a","note":"both see this"}),
        )
        .await;
    let mut polls = Vec::new();
    for _ in 0..3 {
        let reply = session.next().await;
        if reply["id"] != 3 {
            polls.push(content(&reply).clone());
        }
    }
    assert_eq!(polls.len(), 2);
    assert_eq!(polls[0], polls[1]);
    assert_eq!(polls[0]["events"].as_array().unwrap().len(), 1);
    session
        .tool(
            json!(4),
            "subscribe_events",
            json!({"awaiting_agent":"nobody","timeout_ms":60000}),
        )
        .await;
    session.ping(json!(5)).await;
    assert_eq!(session.next().await["id"], 5);
    session.stop().await;
}

#[tokio::test]
async fn event_wait_timeout_is_bounded_and_short_polls_expire() {
    let h = start();
    shared_review(&h, "replay").await;
    let mut session = initialized(&h).await;
    session
        .tool(json!(1), "subscribe_events", json!({"timeout_ms":u64::MAX}))
        .await;
    let rejected = session.next().await;
    assert_eq!(rejected["result"]["isError"], true);
    assert!(
        rejected["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("60000")
    );
    session
        .tool(json!(2), "subscribe_events", json!({"timeout_ms":10}))
        .await;
    assert!(
        content(&session.next().await)["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    session
        .tool(
            json!(3),
            "subscribe_events",
            json!({"since_seq":0,"timeout_ms":0}),
        )
        .await;
    let replay = session.next().await;
    assert_eq!(
        content(&replay)["events"].as_array().unwrap().len(),
        h.daemon.core().events_after(None).unwrap().len()
    );
    session.stop().await;
}

#[tokio::test]
async fn outstanding_wait_limit_recovers_after_cancellation() {
    let h = start();
    let mut session = initialized(&h).await;
    for id in 0..nits_mcp::MAX_EVENT_WAITS {
        session
            .tool(json!(id), "subscribe_events", json!({"timeout_ms":60000}))
            .await;
    }
    session
        .tool(
            json!("excess"),
            "subscribe_events",
            json!({"timeout_ms":60000}),
        )
        .await;
    let rejected = session.next().await;
    assert_eq!(rejected["id"], "excess");
    assert!(
        rejected["error"]["message"]
            .as_str()
            .unwrap()
            .contains("32")
    );
    for id in 0..nits_mcp::MAX_EVENT_WAITS {
        session.cancel(json!(id)).await;
    }
    session
        .tool(
            json!("accepted"),
            "subscribe_events",
            json!({"timeout_ms":0}),
        )
        .await;
    let reply = session.next().await;
    assert_eq!(reply["id"], "accepted");
    assert!(content(&reply)["events"].as_array().unwrap().is_empty());
    session.stop().await;
}

#[tokio::test]
async fn daemon_disconnect_finishes_wait_and_leaves_mcp_ping_responsive() {
    let h = start();
    let proxy = h.socket.with_extension("forward.sock");
    let listener = tokio::net::UnixListener::bind(&proxy).unwrap();
    let cut = CancellationToken::new();
    let forwarding = tokio::spawn(forward_connections(listener, h.socket.clone(), cut.clone()));
    let mut server = server_at(&proxy, "disconnect-session");
    init(&mut server).await;
    let mut session = Session::new(server);
    session
        .tool(json!(1), "subscribe_events", json!({"timeout_ms":60000}))
        .await;
    session.ping(json!(2)).await;
    assert_eq!(session.next().await["id"], 2);
    cut.cancel();
    forwarding.await.unwrap();
    let reply = session.next().await;
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["result"]["isError"], true);
    session.ping(json!(3)).await;
    assert_eq!(session.next().await["id"], 3);
    session.stop().await;
}

#[tokio::test]
async fn context_switch_cancels_old_wait_and_preserves_mutation_identity_and_cursor_rules() {
    let a = start();
    let b = start();
    let review = shared_review(&a, "a").await;
    assert_eq!(shared_review(&b, "b").await, review);
    let path = a.dir.path().join("contexts.toml");
    let mut config = nits_config::Config::default();
    config
        .contexts
        .insert("a".parse().unwrap(), local_context(&a.socket));
    config
        .contexts
        .insert("b".parse().unwrap(), local_context(&b.socket));
    config.save(&path).unwrap();
    let mut server = configured_server(&path, "a");
    init(&mut server).await;
    let mut session = Session::new(server);
    session
        .tool(
            json!(1),
            "subscribe_events",
            json!({"awaiting_agent":"nobody","timeout_ms":60000}),
        )
        .await;
    session
        .tool(
            json!(2),
            "request_review",
            json!({"review_id":review,"agent":"old-agent","note":"on a"}),
        )
        .await;
    session
        .tool(json!(3), "use_context", json!({"name":"b"}))
        .await;
    session
        .tool(
            json!(4),
            "set_session_identity",
            json!({"name":"new-agent","model":"new-model"}),
        )
        .await;
    session
        .tool(
            json!(5),
            "request_review",
            json!({"review_id":review,"agent":"new-agent","note":"on b"}),
        )
        .await;
    session
        .tool(json!(6), "get_review", json!({"review_id":review}))
        .await;
    let mut replies = std::collections::HashMap::new();
    for _ in 0..6 {
        let reply = session.next().await;
        replies.insert(reply["id"].as_u64().unwrap(), reply);
    }
    assert_eq!(replies[&1]["result"]["isError"], true);
    assert!(
        replies[&1]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("context a cancelled")
    );
    let old = committed(&a, content(&replies[&2]));
    let new = committed(&b, content(&replies[&5]));
    assert_eq!(old["author"]["name"], "claude-code");
    assert_eq!(new["author"]["name"], "new-agent");
    assert_eq!(new["author"]["session_id"], old["author"]["session_id"]);
    assert_eq!(content(&replies[&6])["context"]["name"], "b");
    assert_eq!(content(&replies[&6])["review"]["title"], "b");
    session
        .tool(
            json!(7),
            "subscribe_events",
            json!({"since_seq":0,"timeout_ms":0}),
        )
        .await;
    assert_eq!(session.next().await["result"]["isError"], true);
    session
        .tool(
            json!(8),
            "subscribe_events",
            json!({"since_seq":0,"since_context":"a","timeout_ms":0}),
        )
        .await;
    assert_eq!(session.next().await["result"]["isError"], true);
    session
        .tool(
            json!(9),
            "subscribe_events",
            json!({"since_seq":0,"since_context":"b","timeout_ms":0}),
        )
        .await;
    assert_eq!(content(&session.next().await)["context"]["name"], "b");
    session.stop().await;
}

#[tokio::test]
async fn failed_context_switch_keeps_original_wait_and_identity_change_cancels_it() {
    let h = start();
    let path = h.dir.path().join("contexts.toml");
    let mut config = nits_config::Config::default();
    config
        .contexts
        .insert("a".parse().unwrap(), local_context(&h.socket));
    config.contexts.insert(
        "offline".parse().unwrap(),
        local_context(&h.socket.with_extension("missing")),
    );
    config.save(&path).unwrap();
    let mut server = configured_server(&path, "a");
    init(&mut server).await;
    let mut session = Session::new(server);
    session
        .tool(
            json!(1),
            "subscribe_events",
            json!({"awaiting_agent":"nobody","timeout_ms":60000}),
        )
        .await;
    session
        .tool(json!(2), "use_context", json!({"name":"offline"}))
        .await;
    let failed = session.next().await;
    assert_eq!(failed["id"], 2);
    assert_eq!(failed["result"]["isError"], true);
    session.ping(json!(3)).await;
    assert_eq!(session.next().await["id"], 3);
    session
        .tool(
            json!(4),
            "set_session_identity",
            json!({"name":"new-agent","model":"new-model"}),
        )
        .await;
    let a = session.next().await;
    let b = session.next().await;
    let cancelled = if a["id"] == 1 {
        a
    } else {
        assert_eq!(b["id"], 1);
        b
    };
    assert_eq!(cancelled["result"]["isError"], true);
    session.stop().await;
}

async fn connections(h: &Harness, expected: usize) {
    tokio::time::timeout(DEADLINE, async {
        while h.daemon.connections() != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "expected {expected} connections, got {}",
            h.daemon.connections()
        )
    });
}

#[tokio::test]
async fn cancel_and_eof_release_owned_daemon_connections() {
    let h = start();
    let mut session = initialized(&h).await;
    session
        .tool(json!(1), "subscribe_events", json!({"timeout_ms":60000}))
        .await;
    session.ping(json!(2)).await;
    assert_eq!(session.next().await["id"], 2);
    connections(&h, 2).await;
    session.cancel(json!(1)).await;
    session.ping(json!(3)).await;
    assert_eq!(session.next().await["id"], 3);
    connections(&h, 1).await;
    session
        .tool(json!(4), "subscribe_events", json!({"timeout_ms":60000}))
        .await;
    session.ping(json!(5)).await;
    assert_eq!(session.next().await["id"], 5);
    connections(&h, 2).await;
    session.stop().await;
    connections(&h, 0).await;
}

#[tokio::test]
async fn filtered_zero_timeout_replay_resumes_every_undelivered_matching_event() {
    let h = start();
    let review = shared_review(&h, "filtered replay").await;
    let mut server = server(&h);
    init(&mut server).await;
    let mut expected = Vec::new();
    for agent in [
        "other", "wanted", "other", "wanted", "other", "wanted", "other",
    ] {
        let receipt = call(
            &mut server,
            "request_review",
            json!({"review_id":review,"agent":agent,"note":"replay cursor"}),
        )
        .await;
        if agent == "wanted" {
            expected.push(receipt["seq"].clone());
        }
    }
    let mut session = Session::new(server);
    let mut cursor = json!(0);
    let mut received = Vec::new();
    for id in 0..3 {
        session
            .tool(
                json!(id),
                "subscribe_events",
                json!({"awaiting_agent":"wanted","since_seq":cursor,"timeout_ms":0,"max":2}),
            )
            .await;
        let reply = session.next().await;
        let poll = content(&reply);
        let events = poll["events"].as_array().unwrap();
        if let Some(last) = events.last() {
            assert_eq!(poll["last_seq"], last["seq"]);
        } else {
            assert_eq!(
                poll["last_seq"], cursor,
                "empty replay must not skip unrelated log positions"
            );
        }
        received.extend(events.iter().map(|event| event["seq"].clone()));
        assert_eq!(
            received,
            expected[..received.len()],
            "every batch is the next matching prefix"
        );
        cursor = poll["last_seq"].clone();
    }
    assert_eq!(received, expected);
    assert_eq!(cursor, expected.last().unwrap().clone());
    session.stop().await;
}
