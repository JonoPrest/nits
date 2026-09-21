//! Worker replacement preserves session decisions without serializing live I/O.

use nits_mcp::checkpoint::{CheckpointError, SessionCheckpoint};

use super::*;

fn replacement_build() -> BuildInfo {
    BuildInfo {
        name: "nits-mcp".into(),
        version: "replacement".into(),
    }
}

fn serialized_checkpoint(server: &Server) -> SessionCheckpoint {
    serde_json::from_slice(&serde_json::to_vec(&server.checkpoint()).unwrap()).unwrap()
}

#[test]
fn uninitialized_checkpoint_round_trips_every_endpoint_policy_without_a_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let contexts: Vec<nits_config::Context> = [
        json!({"type":"Local", "data_dir":dir.path().join("data"), "socket":dir.path().join("daemon.sock")}),
        json!({"type":"Ssh", "host":"review-box", "bin":"/opt/nits/bin/nits", "args":["--data-dir","/srv/reviews"], "ssh":"/usr/bin/ssh"}),
        json!({"type":"Ws", "url":"ws://127.0.0.1:9876"}),
    ].into_iter().map(|value| serde_json::from_value(value).unwrap()).collect();
    for context in contexts {
        for origin in [
            nits_config::SelectionOrigin::Flag,
            nits_config::SelectionOrigin::Environment,
            nits_config::SelectionOrigin::Persisted,
            nits_config::SelectionOrigin::Implicit,
            nits_config::SelectionOrigin::AdHoc,
            nits_config::SelectionOrigin::Mcp,
        ] {
            for start in [
                nitsd::contexts::StartPolicy::StartIfNeeded,
                nitsd::contexts::StartPolicy::RequireRunning,
            ] {
                let server = server_with_endpoint(
                    Endpoint {
                        selection: nits_config::Selection {
                            name: "review".parse().unwrap(),
                            context: context.clone(),
                            origin,
                        },
                        config_path: dir.path().join("absent.toml"),
                        start,
                    },
                    "stable-session",
                );
                let checkpoint = serialized_checkpoint(&server);
                let restored =
                    Server::from_checkpoint(checkpoint.clone(), replacement_build()).unwrap();
                assert_eq!(restored.checkpoint(), checkpoint);
                assert!(restored.client().is_none());
                assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
            }
        }
    }
}

#[test]
fn initialized_checkpoint_validates_provenance_and_restores_without_io() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_at(&dir.path().join("absent.sock"), "stable-session");
    let mut wire = serde_json::to_value(server.checkpoint()).unwrap();
    wire["state"] = json!({
        "type":"Initialized", "cursor_policy":"RequireContext",
        "author": {"type":"Agent", "name":"reviewer", "model":"updated-model", "session_id":"stable-session",
            "invoked_by":{"name":"ada","machine":"box"}, "via":"Mcp"}
    });
    let checkpoint: SessionCheckpoint = serde_json::from_value(wire.clone()).unwrap();
    let restored = Server::from_checkpoint(checkpoint.clone(), replacement_build()).unwrap();
    assert_eq!(restored.checkpoint(), checkpoint);
    assert!(restored.client().is_none());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    for (field, invalid) in [
        ("via", json!("Cli")),
        ("session_id", json!("another-session")),
        ("invoked_by", Value::Null),
    ] {
        let mut invalid_wire = wire.clone();
        invalid_wire["state"]["author"][field] = invalid;
        assert!(
            serde_json::from_value::<SessionCheckpoint>(invalid_wire).is_err(),
            "{field}"
        );
    }
    wire["state"]["author"] = json!({"type":"Human", "name":"ada", "machine":"box"});
    assert!(serde_json::from_value::<SessionCheckpoint>(wire).is_err());
}

#[test]
fn checkpoint_rejects_unknown_fields_and_unsupported_versions() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_at(&dir.path().join("absent.sock"), "stable-session");
    let wire = serde_json::to_value(server.checkpoint()).unwrap();
    for pointer in ["", "/agent", "/endpoint", "/endpoint/selection", "/state"] {
        let mut invalid = wire.clone();
        invalid.pointer_mut(pointer).unwrap()["pending_call"] = json!({"name":"add_comment"});
        assert!(
            serde_json::from_value::<SessionCheckpoint>(invalid).is_err(),
            "{pointer}"
        );
    }
    let mut invalid = wire.clone();
    invalid["version"] = json!(0);
    assert!(serde_json::from_value::<SessionCheckpoint>(invalid).is_err());
    let mut future = wire;
    future["version"] = json!(2);
    let checkpoint = serde_json::from_value(future).unwrap();
    assert!(matches!(
        Server::from_checkpoint(checkpoint, replacement_build()),
        Err(CheckpointError::UnsupportedVersion { .. })
    ));
}

#[tokio::test]
async fn unavailable_daemon_keeps_initialized_identity_and_allows_context_recovery() {
    let h = start();
    let path = h.dir.path().join("contexts.toml");
    let mut config = nits_config::Config::default();
    config.contexts.insert(
        "offline".parse().unwrap(),
        local_context(&h.socket.with_extension("absent")),
    );
    config
        .contexts
        .insert("online".parse().unwrap(), local_context(&h.socket));
    config.save(&path).unwrap();
    let unchanged = std::fs::read(&path).unwrap();
    let mut server = configured_server(&path, "offline");
    let initialized = init(&mut server).await;
    assert!(initialized.error.is_none());
    assert!(
        initialized.result.unwrap()["instructions"]
            .as_str()
            .unwrap()
            .contains("Daemon unavailable")
    );
    assert!(server.client().is_none());
    let identity = call(&mut server, "get_session_identity", json!({})).await;
    assert_eq!(identity["author"]["session_id"], "context-session");
    assert_eq!(
        call(&mut server, "list_contexts", json!({})).await["active"]["name"],
        "offline"
    );
    assert!(
        call_err(&mut server, "list_workspaces", json!({}))
            .await
            .contains("cannot connect")
    );
    let checkpoint = serialized_checkpoint(&server);
    let mut restored = Server::from_checkpoint(checkpoint, replacement_build()).unwrap();
    assert_eq!(
        call(&mut restored, "get_session_identity", json!({})).await,
        identity
    );
    call(&mut restored, "use_context", json!({"name":"online"})).await;
    assert_eq!(
        call(&mut restored, "list_workspaces", json!({})).await["workspaces"],
        json!([])
    );
    assert_eq!(std::fs::read(path).unwrap(), unchanged);
}

#[tokio::test]
async fn restored_session_preserves_updated_author_and_uses_fresh_mutation_identity() {
    let h = start();
    let review = shared_review(&h, "worker handoff").await;
    let mut server = server(&h);
    init(&mut server).await;
    let identity = call(
        &mut server,
        "set_session_identity",
        json!({"name":"reviewer", "model":"updated-model"}),
    )
    .await;
    let before = call(
        &mut server,
        "add_comment",
        json!({"review_id":review, "body":"before replacement"}),
    )
    .await;
    let old_event = committed(&h, &before);
    let checkpoint = serialized_checkpoint(&server);
    drop(server);
    let count = h.daemon.core().events_after(None).unwrap().len();
    let mut restored = Server::from_checkpoint(checkpoint, replacement_build()).unwrap();
    assert!(restored.client().is_none());
    assert_eq!(
        call(&mut restored, "get_session_identity", json!({})).await,
        identity
    );
    assert!(
        restored.client().is_none(),
        "identity reads must not reconnect"
    );
    assert_eq!(h.daemon.core().events_after(None).unwrap().len(), count);
    let after = call(
        &mut restored,
        "add_comment",
        json!({"review_id":review, "body":"after replacement"}),
    )
    .await;
    let new_event = committed(&h, &after);
    assert_eq!(new_event["author"], identity["author"]);
    assert_ne!(new_event["client_id"], old_event["client_id"]);
    assert_eq!(new_event["client_seq"], 1);
    assert_eq!(h.daemon.core().events_after(None).unwrap().len(), count + 1);
}

#[tokio::test]
async fn restore_keeps_selected_context_and_requires_its_cursor_without_changing_defaults() {
    let a = start();
    let b = start();
    let review = shared_review(&b, "selected context").await;
    let path = a.dir.path().join("contexts.toml");
    let mut config = nits_config::Config::default();
    config
        .contexts
        .insert("a".parse().unwrap(), local_context(&a.socket));
    config
        .contexts
        .insert("b".parse().unwrap(), local_context(&b.socket));
    config.current_context = Some("a".parse().unwrap());
    config.save(&path).unwrap();
    let unchanged = std::fs::read(&path).unwrap();
    let mut server = configured_server(&path, "a");
    init(&mut server).await;
    call(&mut server, "use_context", json!({"name":"b"})).await;
    let checkpoint = serialized_checkpoint(&server);
    drop(server);
    let mut restored = Server::from_checkpoint(checkpoint, replacement_build()).unwrap();
    let contexts = call(&mut restored, "list_contexts", json!({})).await;
    assert_eq!(contexts["active"]["name"], "b");
    assert_eq!(contexts["persisted"], "a");
    assert!(
        call_err(
            &mut restored,
            "subscribe_events",
            json!({"since_seq":0, "timeout_ms":10})
        )
        .await
        .contains("since_context is required")
    );
    assert!(
        call_err(
            &mut restored,
            "subscribe_events",
            json!({"since_seq":0, "since_context":"a", "timeout_ms":10})
        )
        .await
        .contains("does not match")
    );
    let events = call(
        &mut restored,
        "subscribe_events",
        json!({"since_seq":0, "since_context":"b", "timeout_ms":10}),
    )
    .await;
    assert_eq!(events["context"]["name"], "b");
    assert!(!events["events"].as_array().unwrap().is_empty());
    assert_eq!(
        call(&mut restored, "get_review", json!({"review_id":review})).await["review"]["title"],
        "selected context"
    );
    assert_eq!(std::fs::read(path).unwrap(), unchanged);
}

#[tokio::test]
async fn restoring_an_uninitialized_checkpoint_still_requires_initialize() {
    let h = start();
    let checkpoint = serialized_checkpoint(&server(&h));
    let mut restored = Server::from_checkpoint(checkpoint, replacement_build()).unwrap();
    for name in ["get_session_identity", "list_contexts", "list_workspaces"] {
        assert!(
            call_err(&mut restored, name, json!({}))
                .await
                .contains("initialize first")
        );
    }
    assert!(restored.client().is_none());
    assert!(init(&mut restored).await.error.is_none());
    assert_eq!(
        call(&mut restored, "list_workspaces", json!({})).await["workspaces"],
        json!([])
    );
}
