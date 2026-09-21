//! Request recipients never replace the principal that invokes the CLI.

use super::start;
use assert_cmd::Command;
use nits_config::{Config, Context};
use nits_protocol::{AgentVia, Author, Event, Human, ReviewRequest};

#[test]
fn request_recipient_and_complete_requester_survive_every_global_flag_position() {
    let h = start();
    let review = h.out(&[h.repo.path().to_str().unwrap(), "--headless"]);
    let human = Human {
        name: "invoking-human".into(),
        machine: gethostname::gethostname().to_string_lossy().into_owned(),
    };
    let mut expected = Vec::new();
    // Exercise every nesting boundary, before/after the recipient, and env-only.
    for position in 0..=5 {
        let mut args = vec!["review", "request", &review, "recipient"];
        let name = if position < 5 {
            args.splice(position..position, ["--agent", "requester"]);
            "requester"
        } else {
            "environment-requester"
        };
        args.extend([
            "--json",
            "--user",
            "invoking-human",
            "--note",
            "Please review",
        ]);
        let output = h
            .nits()
            .env("NITS_AGENT", "environment-requester")
            .env("NITS_AGENT_MODEL", "fixture-model")
            .env("NITS_SESSION_ID", "fixture-session")
            .args(args)
            .assert()
            .success();
        let event: Event = serde_json::from_slice(&output.get_output().stdout).unwrap();
        let author = Author::Agent {
            name: name.into(),
            model: "fixture-model".into(),
            session_id: "fixture-session".into(),
            invoked_by: Some(human.clone()),
            via: AgentVia::Cli,
        };
        assert_eq!(event.author, author, "flag position {position}");
        let request = ReviewRequest::from_event(&event).unwrap();
        assert_eq!(request.recipient, "recipient");
        assert_eq!(request.note, "Please review");
        expected.push(request);
    }
    // A model/session alone must not turn a human into an agent.
    let output = h
        .nits()
        .env("NITS_AGENT_MODEL", "unused-model")
        .env("NITS_SESSION_ID", "unused-session")
        .args([
            "--json",
            "--user",
            "invoking-human",
            "review",
            "request",
            &review,
            "recipient",
        ])
        .assert()
        .success();
    let event: Event = serde_json::from_slice(&output.get_output().stdout).unwrap();
    assert_eq!(event.author, Author::human(human));
    expected.push(ReviewRequest::from_event(&event).unwrap());

    // A fresh connection reads the durable records, not just command receipts.
    let snapshot: serde_json::Value =
        serde_json::from_str(&h.out(&["--json", "review", "show", &review])).unwrap();
    let requests: Vec<ReviewRequest> =
        serde_json::from_value(snapshot["requests"].clone()).unwrap();
    assert_eq!(requests, expected);
}

#[test]
fn add_local_uses_global_endpoints_at_every_position_without_connecting() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    let data = dir.path().join("data");
    let socket = dir.path().join("daemon.sock");
    for position in 0..=4 {
        let name = format!("context-{position}");
        let mut args = vec!["context", "add-local", &name];
        let (expected_data, expected_socket) = if position < 4 {
            args.splice(
                position..position,
                [
                    "--data-dir",
                    data.to_str().unwrap(),
                    "--socket",
                    socket.to_str().unwrap(),
                ],
            );
            (data.clone(), socket.clone())
        } else {
            (
                dir.path().join("environment-data"),
                dir.path().join("environment.sock"),
            )
        };
        Command::cargo_bin("nits")
            .unwrap()
            .env("NITS_CONFIG", &config)
            .env("NITS_DATA_DIR", dir.path().join("environment-data"))
            .env("NITS_SOCKET", dir.path().join("environment.sock"))
            .args(args)
            .assert()
            .success();
        let cfg = Config::load(&config).unwrap();
        assert_eq!(
            cfg.contexts.get(name.as_str()),
            Some(&Context::Local {
                data_dir: Some(expected_data),
                socket: Some(expected_socket),
            })
        );
    }
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "only the offline config is created"
    );
}
