//! A peer drops an accepted mutation without acknowledging it. MCP must answer
//! with uncertainty, keep ping responsive, and reconnect only for the next call.

use std::future::Future;
use std::time::Duration;

use nits_mcp::jsonrpc::Incoming;
use nits_mcp::server::AgentIdentity;
use nits_mcp::{Endpoint, Server};
use nits_protocol::{
    BuildInfo, ClientMsg, Envelope, ProtocolVersion, Request, Response, ReviewId, RpcError,
    SchemaVersion, ServerMsg,
};
use nitsd::transport::{self, ByteRead, ByteWrite};
use serde_json::{Value, json};
use tokio::net::{UnixListener, UnixStream};

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .expect("MCP call hung")
}

fn req(method: &str, params: Value) -> Incoming {
    Incoming {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: method.into(),
        params,
    }
}

async fn tool_result(server: &mut Server, name: &str, arguments: Value) -> Value {
    bounded(server.handle(req(
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )))
    .await
    .unwrap()
    .result
    .unwrap()
}

async fn accept_hello(
    listener: &UnixListener,
) -> (ByteRead<UnixStream>, ByteWrite<UnixStream>, ClientMsg) {
    loop {
        let (socket, _) = listener.accept().await.unwrap();
        let (mut rd, wr) = transport::byte_stream(socket);
        // RequireRunning probes the socket before doing a real handshake.
        let Some(hello) = transport::recv_msg::<_, ClientMsg>(&mut rd).await.unwrap() else {
            continue;
        };
        assert!(matches!(hello.msg, ClientMsg::Hello { .. }));
        return (rd, wr, hello.msg);
    }
}

async fn accept_client(
    listener: &UnixListener,
) -> (ByteRead<UnixStream>, ByteWrite<UnixStream>, ClientMsg) {
    let (rd, mut wr, hello) = accept_hello(listener).await;
    transport::send_msg(
        &mut wr,
        &Envelope::current(ServerMsg::Welcome {
            protocol: ProtocolVersion::CURRENT,
            daemon: BuildInfo {
                name: "test-daemon".into(),
                version: "0".into(),
            },
            schema: SchemaVersion::CURRENT,
            upgrade: None,
        }),
    )
    .await
    .unwrap();
    (rd, wr, hello)
}

fn server(socket: &std::path::Path) -> Server {
    Server::new(
        Endpoint {
            selection: nits_config::Selection {
                name: "test".parse().unwrap(),
                origin: nits_config::SelectionOrigin::AdHoc,
                context: nits_config::Context::Local {
                    data_dir: Some(socket.parent().unwrap().to_owned()),
                    socket: Some(socket.to_owned()),
                },
            },
            config_path: socket.with_extension("toml"),
            start: nitsd::contexts::StartPolicy::RequireRunning,
        },
        AgentIdentity {
            model: "test-model".into(),
            session_id: "session".into(),
            invoked_by: None,
        },
        BuildInfo {
            name: "test-mcp".into(),
            version: "0".into(),
        },
    )
}

#[tokio::test]
async fn lost_mutation_reconnects_without_replay() {
    lost_mutation_after_replacement(false).await;
}

#[tokio::test]
async fn lost_mutation_is_not_replayed_after_worker_checkpoint_restore() {
    lost_mutation_after_replacement(true).await;
}

fn restore_worker(server: Server) -> Server {
    let checkpoint =
        serde_json::from_slice(&serde_json::to_vec(&server.checkpoint()).unwrap()).unwrap();
    drop(server);
    let restored = Server::from_checkpoint(
        checkpoint,
        BuildInfo {
            name: "replacement-mcp".into(),
            version: "next".into(),
        },
    )
    .unwrap();
    assert!(restored.client().is_none());
    restored
}

/// Accept one mutation without a receipt, then insist on a read after reconnect.
async fn lost_reply_peer(listener: UnixListener) {
    let (mut rd, wr, before) = accept_client(&listener).await;
    let mutation = transport::recv_msg::<_, ClientMsg>(&mut rd)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        mutation.msg,
        ClientMsg::Request {
            request: Request::Mutate { .. },
            ..
        }
    ));
    // The peer accepted the mutation. Its outcome is now unknown.
    drop((rd, wr));
    let (mut rd, mut wr, after) = accept_client(&listener).await;
    match (before, after) {
        (
            ClientMsg::Hello {
                client_id: old,
                author: before,
                ..
            },
            ClientMsg::Hello {
                client_id: new,
                author: after,
                ..
            },
        ) => {
            assert_ne!(old, new, "new Ops resets its mutation sequence");
            assert_eq!(before, after, "MCP provenance must survive reconnect");
        }
        other => panic!("expected two handshakes: {other:?}"),
    }
    let msg = transport::recv_msg::<_, ClientMsg>(&mut rd)
        .await
        .unwrap()
        .unwrap();
    let ClientMsg::Request {
        id,
        request: Request::ListWorkspaces,
    } = msg.msg
    else {
        panic!("the uncertain mutation was replayed: {:?}", msg.msg);
    };
    transport::send_msg(
        &mut wr,
        &Envelope::current(ServerMsg::Response {
            id,
            response: Response::Workspaces { workspaces: vec![] },
        }),
    )
    .await
    .unwrap();
}

async fn lost_mutation_after_replacement(replace_worker: bool) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let peer = tokio::spawn(lost_reply_peer(listener));
    let mut server = server(&socket);
    let init = bounded(server.handle(req(
        "initialize",
        json!({ "clientInfo": { "name": "test-agent" } }),
    )))
    .await
    .unwrap();
    assert!(init.error.is_none());
    let lost = bounded(server.handle(req(
        "tools/call",
        json!({
            "name": "update_review",
            "arguments": {"review_id": ReviewId::from_parts(1, 1), "title": "renamed", "status": "Open"}
        }),
    )))
    .await
    .unwrap()
    .result
    .unwrap();
    assert_eq!(lost["isError"], true);
    let message = lost["content"][0]["text"].as_str().unwrap();
    assert!(message.contains("may have committed"), "{message}");
    assert!(
        message.contains("next tool call will reconnect"),
        "{message}"
    );
    let ping = bounded(server.handle(req("ping", json!({}))))
        .await
        .unwrap();
    assert_eq!(ping.result, Some(json!({})));
    if replace_worker {
        server = restore_worker(server);
    }
    let recovered = bounded(server.handle(req(
        "tools/call",
        json!({ "name": "list_workspaces", "arguments": {} }),
    )))
    .await
    .unwrap()
    .result
    .unwrap();
    assert_eq!(recovered["structuredContent"]["workspaces"], json!([]));
    bounded(peer).await.unwrap();
}

#[tokio::test]
async fn rejected_identity_handshake_preserves_the_live_connection() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let peer = tokio::spawn(async move {
        let (mut original_rd, mut original_wr, original) = accept_client(&listener).await;
        let (candidate_rd, mut candidate_wr, candidate) = accept_hello(&listener).await;
        let ClientMsg::Hello {
            author: original_author,
            client_id: old_id,
            ..
        } = original
        else {
            panic!("expected original Hello");
        };
        let ClientMsg::Hello {
            author: candidate_author,
            client_id: new_id,
            ..
        } = candidate
        else {
            panic!("expected candidate Hello");
        };
        assert_ne!(old_id, new_id);
        let mut expected = json!(original_author);
        expected["name"] = json!("reviewer-a");
        expected["model"] = json!("next-model");
        assert_eq!(json!(candidate_author), expected);
        transport::send_msg(
            &mut candidate_wr,
            &Envelope::current(ServerMsg::Rejected {
                error: RpcError::UnsupportedProtocol {
                    requested: ProtocolVersion::CURRENT,
                    supported: vec![],
                },
            }),
        )
        .await
        .unwrap();
        drop((candidate_rd, candidate_wr));
        let message = transport::recv_msg::<_, ClientMsg>(&mut original_rd)
            .await
            .unwrap()
            .unwrap();
        let ClientMsg::Request {
            id,
            request: Request::ListWorkspaces,
        } = message.msg
        else {
            panic!("expected query on the original connection");
        };
        transport::send_msg(
            &mut original_wr,
            &Envelope::current(ServerMsg::Response {
                id,
                response: Response::Workspaces { workspaces: vec![] },
            }),
        )
        .await
        .unwrap();
    });
    let mut server = server(&socket);
    bounded(server.handle(req(
        "initialize",
        json!({ "clientInfo": { "name": "original-agent" } }),
    )))
    .await
    .unwrap();
    let before = tool_result(&mut server, "get_session_identity", json!({})).await;
    let failed = tool_result(
        &mut server,
        "set_session_identity",
        json!({ "name": "reviewer-a", "model": "next-model" }),
    )
    .await;
    assert_eq!(failed["isError"], true);
    assert!(
        failed["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cannot connect")
    );
    let after = tool_result(&mut server, "get_session_identity", json!({})).await;
    assert_eq!(before, after);
    let query = tool_result(&mut server, "list_workspaces", json!({})).await;
    assert_eq!(query["structuredContent"]["workspaces"], json!([]));
    bounded(peer).await.unwrap();
}

#[tokio::test]
async fn stalled_reconnect_times_out_and_leaves_ping_responsive() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        drop(accept_client(&listener).await);
        let connection = accept_hello(&listener).await;
        seen_tx.send(()).unwrap();
        // A listening but unresponsive replacement must not wedge stdio.
        std::future::pending::<()>().await;
        drop(connection);
    });
    let mut server = server(&socket);
    let initialized = bounded(server.handle(req("initialize", json!({}))))
        .await
        .unwrap();
    assert!(initialized.error.is_none());
    assert!(
        bounded(server.client().unwrap().next_unsolicited())
            .await
            .is_none()
    );
    {
        let query = server.handle(req(
            "tools/call",
            json!({ "name": "list_workspaces", "arguments": {} }),
        ));
        tokio::pin!(query);
        tokio::select! {
            reply = &mut query => panic!("unexpected reply before handshake: {reply:?}"),
            () = bounded(async { seen_rx.await.unwrap() }) => {}
        }
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(21)).await;
        let reply = bounded(query).await.unwrap().result.unwrap();
        assert_eq!(reply["isError"], true);
        let message = reply["content"][0]["text"].as_str().unwrap();
        assert!(message.contains("timed out"), "{message}");
        assert!(message.contains("call the tool again"), "{message}");
        tokio::time::resume();
    }
    let ping = bounded(server.handle(req("ping", json!({}))))
        .await
        .unwrap();
    assert_eq!(ping.result, Some(json!({})));
    peer.abort();
}

#[tokio::test]
async fn incompatible_daemon_keeps_mcp_initialized_for_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let peer = tokio::spawn(async move {
        let (rd, mut wr, _) = accept_hello(&listener).await;
        transport::send_msg(
            &mut wr,
            &Envelope::current(ServerMsg::Rejected {
                error: RpcError::UnsupportedProtocol {
                    requested: ProtocolVersion::CURRENT,
                    supported: vec![],
                },
            }),
        )
        .await
        .unwrap();
        drop((rd, wr));
    });
    let mut server = server(&socket);
    let reply = bounded(server.handle(req(
        "initialize",
        json!({"clientInfo":{"name":"recovery-agent"}}),
    )))
    .await
    .unwrap();
    assert!(reply.error.is_none());
    let result = reply.result.unwrap();
    assert!(
        result["instructions"]
            .as_str()
            .unwrap()
            .contains("Daemon unavailable")
    );
    assert_eq!(result["protocolVersion"], nits_mcp::server::MCP_VERSION);
    assert!(server.client().is_none());
    let identity = tool_result(&mut server, "get_session_identity", json!({})).await;
    assert_eq!(
        identity["structuredContent"]["author"]["name"],
        "recovery-agent"
    );
    let ping = bounded(server.handle(req("ping", json!({}))))
        .await
        .unwrap();
    assert_eq!(ping.result, Some(json!({})));
    let listed = bounded(server.handle(req("tools/list", json!({}))))
        .await
        .unwrap();
    assert!(
        !listed.result.unwrap()["tools"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    bounded(peer).await.unwrap();
}
