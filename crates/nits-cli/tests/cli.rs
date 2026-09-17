//! `nits` against a daemon running in this test process (plan 2.6).

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use assert_cmd::Command;
use futures_util::{SinkExt as _, StreamExt as _};
use nits_protocol::{
    BuildInfo, ClientMsg, Envelope, ProtocolVersion, Request, Response, RpcError, SchemaVersion,
    ServerMsg,
};
use nits_review_core::DataDir;
use nits_test_support::{RepoBuilder, TestRepo, files};
use nitsd::Daemon;
use nitsd::server::{UnixServer, WsServer};
use predicates::prelude::*;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

struct Harness {
    dir: tempfile::TempDir,
    socket: PathBuf,
    ws_url: String,
    shutdown: CancellationToken,
    repo: TestRepo,
    rt: tokio::runtime::Runtime,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl Harness {
    fn nits(&self) -> Command {
        let mut c = Command::cargo_bin("nits").unwrap();
        c.env("NITS_SOCKET", &self.socket)
            .env("NITS_CONFIG", self.dir.path().join("no-config.toml"))
            .env("NITS_USER", "ada")
            .env_remove("NITS_AGENT");
        c
    }

    /// Run and return trimmed stdout.
    fn out(&self, args: &[&str]) -> String {
        let a = self.nits().args(args).assert().success();
        String::from_utf8(a.get_output().stdout.clone())
            .unwrap()
            .trim()
            .to_string()
    }
}

static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

async fn serve_older_protocol_until_shutdown(
    listener: tokio::net::UnixListener,
    socket: PathBuf,
) -> [ProtocolVersion; 2] {
    let older = ProtocolVersion::new(
        ProtocolVersion::CURRENT.major,
        ProtocolVersion::CURRENT.minor.saturating_sub(1),
        0,
    );

    let (read, mut write, hello) = loop {
        let (first, _) = listener.accept().await.unwrap();
        let (mut read, write) = nitsd::transport::byte_stream(first);
        if let Some(hello) = nitsd::transport::recv_msg::<_, ClientMsg>(&mut read)
            .await
            .unwrap()
        {
            break (read, write, hello);
        }
    };
    assert_eq!(hello.v, ProtocolVersion::CURRENT);
    nitsd::transport::send_msg(
        &mut write,
        &Envelope {
            v: hello.v,
            msg: ServerMsg::Rejected {
                error: RpcError::UnsupportedProtocol {
                    requested: hello.v,
                    supported: vec![older],
                },
            },
        },
    )
    .await
    .unwrap();
    drop((read, write));

    let (mut read, mut write, hello) = loop {
        let (second, _) = listener.accept().await.unwrap();
        let (mut read, write) = nitsd::transport::byte_stream(second);
        if let Some(hello) = nitsd::transport::recv_msg::<_, ClientMsg>(&mut read)
            .await
            .unwrap()
        {
            break (read, write, hello);
        }
    };
    assert_eq!(hello.v, older);
    nitsd::transport::send_msg(
        &mut write,
        &Envelope {
            v: older,
            msg: ServerMsg::Welcome {
                protocol: older,
                daemon: BuildInfo {
                    name: "older-nitsd".into(),
                    version: "previous".into(),
                },
                schema: SchemaVersion::CURRENT,
                upgrade: None,
            },
        },
    )
    .await
    .unwrap();
    let request = nitsd::transport::recv_msg::<_, ClientMsg>(&mut read)
        .await
        .unwrap()
        .unwrap();
    let id = match request.msg {
        ClientMsg::Request {
            id,
            request: Request::Shutdown,
        } => id,
        other => panic!("expected shutdown request, got {other:?}"),
    };
    nitsd::transport::send_msg(
        &mut write,
        &Envelope {
            v: older,
            msg: ServerMsg::Response {
                id,
                response: Response::ShuttingDown,
            },
        },
    )
    .await
    .unwrap();
    drop((read, write));
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    drop(listener);
    std::fs::remove_file(socket).unwrap();
    [ProtocolVersion::CURRENT, older]
}

fn start() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let socket = std::env::temp_dir().join(format!(
        "nits-cli-{}-{}.sock",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let daemon = Daemon::open(
        &DataDir::new(dir.path()),
        BuildInfo {
            name: "nitsd".into(),
            version: "test".into(),
        },
    )
    .unwrap();
    let shutdown = CancellationToken::new();
    let server = {
        let _g = rt.enter();
        UnixServer::bind(&socket).unwrap()
    };
    rt.spawn(server.run(Arc::clone(&daemon), shutdown.clone()));
    let ws_server = rt
        .block_on(WsServer::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))))
        .unwrap();
    let ws_url = format!("ws://{}", ws_server.addr());
    rt.spawn(ws_server.run(Arc::clone(&daemon), shutdown.clone()));
    let repo = RepoBuilder::new()
        .commit("base", files!["a.rs" => "fn a() {}\nfn z() {}\n"])
        .branch("feature")
        .commit("feat", files!["a.rs" => "fn a() { 1; }\nfn z() {}\n"])
        .build()
        .unwrap();
    Harness {
        dir,
        socket,
        ws_url,
        shutdown,
        repo,
        rt,
    }
}

#[test]
#[allow(clippy::too_many_lines)] // one scenario end to end
fn workspace_review_comment_round_trip() {
    let h = start();
    let ws = h.out(&["workspace", "add", "w"]);
    assert_eq!(ws.len(), 26, "ULID: {ws}");
    let repo_path = h.repo.path().to_str().unwrap();
    let rid = h.out(&["workspace", "attach", &ws, repo_path]);
    h.nits()
        .args(["workspace", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains(&ws).and(predicate::str::contains(&rid)));

    let review = h.out(&[
        "review",
        "create",
        "--workspace",
        &ws,
        "--base",
        "main",
        "--head",
        "feature",
    ]);
    h.nits()
        .args(["review", "list", "--workspace", &ws])
        .assert()
        .success()
        .stdout(predicate::str::contains("main..feature"));
    // No `--workspace`: inferred from the working directory (any depth),
    // never from shared state. Outside every repo it says what to do.
    let sub = h.repo.path().join("sub/dir");
    std::fs::create_dir_all(&sub).unwrap();
    h.nits()
        .current_dir(&sub)
        .args(["review", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("main..feature"));
    h.nits()
        .current_dir(h.dir.path())
        .args(["review", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("pass --workspace"));
    let again = h
        .nits()
        .current_dir(h.repo.path())
        .args(["review", "create", "--base", "main", "--head", "HEAD"])
        .assert()
        .success();
    assert_eq!(
        String::from_utf8_lossy(&again.get_output().stdout)
            .trim()
            .len(),
        26
    );
    h.nits()
        .args(["files", &review])
        .assert()
        .success()
        .stdout("Modified a.rs\n");
    h.nits()
        .args(["diff", &review, "a.rs"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("-fn a() {}").and(predicate::str::contains("+fn a() { 1; }")),
        );
    h.nits()
        .args(["show", &review, "a.rs", "--side", "base"])
        .assert()
        .success()
        .stdout("    1│fn a() {}\n    2│fn z() {}\n");

    let thread = h.out(&[
        "comment", "add", &review, "--path", "a.rs", "--line", "1", "--body", "hmm",
    ]);
    h.out(&["comment", "reply", &review, &thread, "--body", "ok"]);
    h.out(&["comment", "add", &review, "--body", "lgtm"]);
    h.nits()
        .args(["comment", "list", &review])
        .assert()
        .success()
        .stdout(
            predicate::str::contains(format!("thread {thread} [open]"))
                .and(predicate::str::contains("ada @ a.rs:1-1 (Head): hmm"))
                .and(predicate::str::contains("ada @ review: lgtm")),
        );
    h.out(&["comment", "resolve", &review, &thread]);
    h.nits()
        .args(["comment", "list", &review])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "thread {thread} [resolved]"
        )));

    // JSON output is the protocol value.
    let json = h.out(&["--json", "review", "show", &review]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["review"]["id"], serde_json::json!(review));
    assert_eq!(v["files"][0]["path"], serde_json::json!("a.rs"));
    assert_eq!(v["threads"].as_array().unwrap().len(), 2);

    // Events replay from the start, attributed to the human.
    let events = h.out(&["events", "--since", "0"]);
    assert!(
        events.starts_with("#1 ada: workspace created w"),
        "{events}"
    );
    assert!(events.contains("comment "), "{events}");
    assert!(events.contains("thread resolved"), "{events}");
}

#[test]
fn agent_flag_attributes_to_an_agent() {
    let h = start();
    let ws = h.out(&["workspace", "add", "w"]);
    let json = h.out(&["--json", "--agent", "bot", "workspace", "list"]);
    assert!(json.contains(&ws));
    let ev = h.out(&["--json", "--agent", "bot", "workspace", "add", "w2"]);
    let v: serde_json::Value = serde_json::from_str(&ev).unwrap();
    assert_eq!(v["author"]["type"], serde_json::json!("Agent"));
    assert_eq!(v["author"]["name"], serde_json::json!("bot"));
    assert_eq!(v["author"]["via"], serde_json::json!("Cli"));
    assert_eq!(v["author"]["invoked_by"]["name"], serde_json::json!("ada"));
}

#[test]
fn directory_review_uses_and_reports_the_daemons_detected_base() {
    let h = start();
    h.repo.git(&["checkout", "-q", "main"]).unwrap();
    h.repo.git(&["branch", "-D", "feature"]).unwrap();
    h.repo.git(&["branch", "-m", "master"]).unwrap();
    h.repo.git(&["checkout", "-q", "-b", "topic"]).unwrap();
    h.repo
        .git(&["commit", "-q", "--allow-empty", "-m", "topic"])
        .unwrap();

    let created = h
        .nits()
        .args(["--headless", h.repo.path().to_str().unwrap()])
        .assert()
        .success()
        .stderr(predicate::str::contains("(created, base: master)"));
    let review = String::from_utf8_lossy(&created.get_output().stdout)
        .trim()
        .to_owned();
    h.nits()
        .args(["review", "show", &review])
        .assert()
        .success()
        .stdout(predicate::str::contains("name: \"master\""));
}

#[test]
fn headless_json_describes_created_and_reused_directory_reviews() {
    for flags in [&["--headless"][..], &["--ui", "headless"][..]] {
        let h = start();
        let mut previous = None;
        for outcome in ["Created", "Reused"] {
            let result = h
                .nits()
                .current_dir(h.repo.path())
                .args(["--json", "."])
                .args(flags)
                .assert()
                .success()
                .stderr(predicate::str::contains("review: "));
            // Parse all of stdout: status text or a bare ID would fail here.
            let actual: serde_json::Value =
                serde_json::from_slice(&result.get_output().stdout).unwrap();
            let review_id: nits_protocol::ReviewId =
                serde_json::from_value(actual["review_id"].clone()).unwrap();
            let workspaces: Vec<nits_protocol::Workspace> =
                serde_json::from_str(&h.out(&["--json", "workspace", "list"])).unwrap();
            assert_eq!(workspaces.len(), 1);
            let workspace = &workspaces[0];
            assert_eq!(workspace.repos.len(), 1);
            let base_tree = h.repo.git(&["rev-parse", "main^{tree}"]).unwrap();
            let base_commit = h.repo.git(&["rev-parse", "main"]).unwrap();
            let head_tree = h.repo.git(&["rev-parse", "feature^{tree}"]).unwrap();
            assert_eq!(
                actual,
                serde_json::json!({
                    "review_id": review_id,
                    "workspace_id": workspace.id,
                    "repo_id": workspace.repos[0].id,
                    "outcome": outcome,
                    "base": {
                        "tree": base_tree.trim(),
                        "source": {"type": "Commit", "oid": base_commit.trim()},
                    },
                    "head": {
                        "tree": head_tree.trim(),
                        "source": {"type": "WorkingTree", "dirty": [], "branch": "feature"},
                    },
                })
            );
            if let Some(id) = previous {
                assert_eq!(review_id, id, "reuse must return the same review");
            }
            previous = Some(review_id);
            let reviews: Vec<nits_protocol::Review> = serde_json::from_str(&h.out(&[
                "--json",
                "review",
                "list",
                "--workspace",
                &workspace.id.to_string(),
            ]))
            .unwrap();
            assert_eq!(reviews.len(), 1);
            assert_eq!(reviews[0].id, review_id);
        }
    }
}

#[test]
fn headless_text_keeps_bare_ids_for_created_and_reused_reviews() {
    for flags in [&["--headless"][..], &["--ui", "headless"][..]] {
        let h = start();
        let mut previous = None;
        for _ in 0..2 {
            let result = h
                .nits()
                .current_dir(h.repo.path())
                .arg(".")
                .args(flags)
                .assert()
                .success()
                .stderr(predicate::str::contains("review: "));
            let stdout = std::str::from_utf8(&result.get_output().stdout).unwrap();
            let id: nits_protocol::ReviewId = stdout.trim().parse().unwrap();
            assert_eq!(stdout, format!("{id}\n"));
            if let Some(previous) = previous {
                assert_eq!(id, previous);
            }
            previous = Some(id);
        }
    }
}

#[test]
fn errors_are_reported_not_panicked() {
    let h = start();
    h.nits()
        .args(["review", "show", "01ARZ3NDEKTSV4RRFFQ69G5FAV"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("NotFound"));
    let mut c = Command::cargo_bin("nits").unwrap();
    c.env("NITS_SOCKET", "/tmp/definitely-not-a-nitsd.sock")
        .env("NITS_CONFIG", h.dir.path().join("no-config.toml"))
        .args(["--start-policy", "require-running", "workspace", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not running"));
}

/// Lifecycle control can deliberately speak the newest compatible protocol
/// advertised by an older daemon. That lets an upgraded binary stop it, then
/// start its own daemon on the released socket.
#[test]
fn upgraded_cli_stops_an_older_protocol_before_starting_its_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let socket = std::env::temp_dir().join(format!(
        "nits-old-protocol-{}-{}.sock",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let listener = {
        let _guard = rt.enter();
        tokio::net::UnixListener::bind(&socket).unwrap()
    };
    let older = rt.spawn(serve_older_protocol_until_shutdown(
        listener,
        socket.clone(),
    ));
    let nits = || {
        let mut command = Command::cargo_bin("nits").unwrap();
        command
            .env("NITS_DATA_DIR", &data)
            .env("NITS_SOCKET", &socket)
            .env("NITS_CONFIG", dir.path().join("no-config.toml"))
            .env("NITS_USER", "ada")
            .env_remove("NITS_AGENT");
        command
    };

    let stopping = std::time::Instant::now();
    nits()
        .args(["daemon", "stop"])
        .assert()
        .success()
        .stdout("stopping\n");
    assert!(
        stopping.elapsed() >= std::time::Duration::from_millis(100),
        "stop returned before the older daemon released its listener"
    );
    let attempts = rt.block_on(older).unwrap();
    assert_eq!(attempts[0], ProtocolVersion::CURRENT);
    assert!(ProtocolVersion::CURRENT.can_serve(attempts[1]));

    nits()
        .args(["daemon", "start"])
        .assert()
        .success()
        .stdout("started\n");
    nits()
        .args(["daemon", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("running (nitsd"));
    nits()
        .args(["daemon", "stop"])
        .assert()
        .success()
        .stdout("stopping\n");
    let start = std::time::Instant::now();
    while std::os::unix::net::UnixStream::connect(&socket).is_ok() {
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Contexts live in the config file and are always selected per process
/// (`-c` / `NITS_CONTEXT`), never by a persisted "current"; `daemon` manages
/// the selected one, including auto-start on first use and an ssh context
/// whose remote side is exercised through a stand-in `ssh`.
#[test]
#[allow(clippy::too_many_lines)] // one scenario end to end
fn contexts_and_daemon_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    let data = dir.path().join("data");
    let socket = std::env::temp_dir().join(format!(
        "nits-ctx-{}-{}.sock",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let nits = || {
        let mut c = Command::cargo_bin("nits").unwrap();
        c.env("NITS_CONFIG", &cfg)
            .env("NITS_USER", "ada")
            .env_remove("NITS_SOCKET")
            .env_remove("NITS_AGENT");
        c
    };
    let out = |args: &[&str]| -> String {
        let a = nits().args(args).assert().success();
        String::from_utf8(a.get_output().stdout.clone())
            .unwrap()
            .trim()
            .to_string()
    };

    // Empty config: only the implicit local context.
    assert_eq!(out(&["context", "list"]), "local\tlocal (implicit)");
    out(&[
        "context",
        "add-local",
        "box",
        "--data-dir",
        data.to_str().unwrap(),
        "--socket",
        socket.to_str().unwrap(),
    ]);
    out(&["context", "add-ws", "shared", "ws://127.0.0.1:1/"]);
    let list = out(&["context", "list"]);
    assert!(list.contains("box\tlocal data_dir="), "{list}");
    assert!(list.contains("shared\tws ws://127.0.0.1:1/"), "{list}");
    // Adding never changes what an unflagged command targets.
    assert!(out(&["context", "show"]).starts_with("local\t"));
    assert!(out(&["-c", "box", "context", "show"]).starts_with("box\t"));
    assert!(!std::fs::read_to_string(&cfg).unwrap().contains("current"));
    // The rest of the scenario selects `box` via the environment.
    let nits = || {
        let mut c = nits();
        c.env("NITS_CONTEXT", "box");
        c
    };
    let out = |args: &[&str]| -> String {
        let a = nits().args(args).assert().success();
        String::from_utf8(a.get_output().stdout.clone())
            .unwrap()
            .trim()
            .to_string()
    };

    // Nothing running yet; a plain command auto-starts the daemon.
    assert_eq!(
        out(&["daemon", "status"]),
        format!("box\t{}\tstopped", ctx_desc(&data, &socket))
    );
    nits()
        .args(["--start-policy", "require-running", "workspace", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not running"));
    let ws = out(&["workspace", "add", "w"]);
    assert!(out(&["daemon", "status"]).ends_with("running (nitsd 0.1.0)"));
    assert_eq!(out(&["daemon", "start"]), "already running");
    assert!(out(&["daemon", "status", "--all"]).contains("shared\tws ws://127.0.0.1:1/\tstopped"));

    // Selecting another context by flag.
    assert!(out(&["-c", "shared", "context", "show"]).starts_with("shared\t"));
    nits()
        .args(["-c", "shared", "daemon", "stop"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("managed elsewhere"));

    // An ssh context: `ssh` is replaced by a script that ignores the host
    // and the remote binary name, and runs this build's proxy locally —
    // `nits daemon stdio`, pointed at the same data dir. The flags go after
    // the subcommand because they are global.
    let fake_ssh = dir.path().join("fake-ssh.sh");
    std::fs::write(
        &fake_ssh,
        format!(
            "#!/bin/sh\nshift 2\nexec {} \"$@\" --data-dir {} --socket {}\n",
            env!("CARGO_BIN_EXE_nits"),
            data.display(),
            socket.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        &fake_ssh,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let text = std::fs::read_to_string(&cfg).unwrap()
        + &format!(
            "\n[contexts.remote]\ntype = \"Ssh\"\nhost = \"ignored\"\nssh = \"{}\"\n",
            fake_ssh.display()
        );
    std::fs::write(&cfg, text).unwrap();
    assert!(out(&["-c", "remote", "daemon", "status"]).ends_with("running (nitsd 0.1.0)"));
    assert!(out(&["-c", "remote", "workspace", "list"]).contains(&ws));

    // Directory-review base detection happens on the daemon side. The SSH
    // client sends only the remote path and gets the same result as local.
    let remote_repo = RepoBuilder::new()
        .commit("base", files!["remote.txt" => "base\n"])
        .build()
        .unwrap();
    remote_repo.git(&["branch", "-m", "release"]).unwrap();
    remote_repo
        .git(&["config", "init.defaultBranch", "release"])
        .unwrap();
    remote_repo.git(&["checkout", "-q", "-b", "topic"]).unwrap();
    remote_repo
        .git(&["commit", "-q", "--allow-empty", "-m", "topic"])
        .unwrap();
    nits()
        .args(["-c", "remote", "--headless"])
        .arg(remote_repo.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("(created, base: release)"));

    // Stop through the ssh context; the local context sees it stopped;
    // start through ssh brings it back (remote side auto-starts).
    assert_eq!(out(&["-c", "remote", "daemon", "stop"]), "stopping");
    let start = std::time::Instant::now();
    while !out(&["daemon", "status"]).ends_with("stopped") {
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(out(&["-c", "remote", "daemon", "stop"]), "not running");
    assert_eq!(out(&["-c", "remote", "daemon", "start"]), "started");
    assert!(out(&["daemon", "status"]).ends_with("running (nitsd 0.1.0)"));
    assert_eq!(out(&["daemon", "stop"]), "stopping");
    let start = std::time::Instant::now();
    while !out(&["daemon", "status"]).ends_with("stopped") {
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    out(&["context", "remove", "shared"]);
    assert!(!out(&["context", "list"]).contains("shared"));
}

fn ctx_desc(data: &Path, socket: &Path) -> String {
    format!(
        "local data_dir={} socket={}",
        data.display(),
        socket.display()
    )
}

struct RunningUi(std::process::Child);

impl Drop for RunningUi {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn browser_reaches_subscribed(port: u16) {
    let url = format!("ws://127.0.0.1:{port}/ws");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut socket = loop {
        match tokio_tungstenite::connect_async(&url).await {
            Ok((socket, _)) => break socket,
            Err(err) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "UI bridge did not listen at {url}: {err}"
                );
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    };
    socket
        .send(Message::Text(r#"{"cmd":"attach"}"#.into()))
        .await
        .unwrap();
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("timed out waiting for subscribed view")
            .expect("UI bridge closed")
            .unwrap();
        let Message::Text(text) = message else {
            continue;
        };
        let patches: Vec<nits_client_core::ViewPatch> = serde_json::from_str(&text).unwrap();
        if patches.iter().any(|patch| {
            matches!(
                patch,
                nits_client_core::ViewPatch::Connection {
                    connection: nits_client_core::ConnectionView::Subscribed,
                    ..
                }
            )
        }) {
            return;
        }
    }
}

/// Both remote context kinds can back the locally served browser UI. The SSH
/// stand-in runs `nits daemon stdio` against this harness's daemon, exactly as
/// the lifecycle test above does for command clients.
#[test]
fn browser_ui_connects_to_named_ssh_and_websocket_contexts() {
    let h = start();
    let cfg_path = h.dir.path().join("remote-ui.toml");
    let ssh_starts = h.dir.path().join("ssh-starts");
    let fake_ssh = h.dir.path().join("fake-ui-ssh.sh");
    std::fs::write(
        &fake_ssh,
        format!(
            "#!/bin/sh\necho x >> {}\nshift 2\nexec {} \"$@\" --socket {}\n",
            ssh_starts.display(),
            env!("CARGO_BIN_EXE_nits"),
            h.socket.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        &fake_ssh,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let mut cfg = nits_config::Config::default();
    cfg.contexts.insert(
        "remote".into(),
        nits_config::Context::Ssh {
            host: "test-host".into(),
            bin: nits_config::RemoteBin::Default,
            args: Vec::new(),
            ssh: Some(fake_ssh.to_string_lossy().into_owned()),
        },
    );
    cfg.contexts.insert(
        "remote-ws".into(),
        nits_config::Context::Ws {
            url: h.ws_url.clone(),
        },
    );
    cfg.save(&cfg_path).unwrap();

    for context in ["remote", "remote-ws"] {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let child = std::process::Command::new(env!("CARGO_BIN_EXE_nits"))
            .env("NITS_CONFIG", &cfg_path)
            .env("NITS_USER", "ada")
            .env_remove("NITS_SOCKET")
            .args(["--context", context, "--port", &port.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let ui = RunningUi(child);
        h.rt.block_on(browser_reaches_subscribed(port));
        drop(ui);
    }
    let starts = std::fs::read_to_string(ssh_starts).unwrap();
    assert_eq!(
        starts.lines().count(),
        2,
        "the short-lived command client and long-lived UI host each dial once"
    );
}

/// The daemon subcommands parse their own flags.
///
/// clap keys arguments by *field name*, and only notices a clash when the
/// parsed value is read: a global `--ws` reaching a `ServeArgs` whose field
/// was also called `ws` panics with "Mismatch between definition and access"
/// at run time, in front of a user. Help output does not build far enough to
/// catch it, so this parses for real. `--start-policy require-running` makes
/// `daemon stdio` exit 3 against a socket nothing is listening on.
#[test]
fn the_daemon_subcommands_parse_their_flags() {
    let dir = tempfile::tempdir().unwrap();
    Command::cargo_bin("nits")
        .unwrap()
        .env("NITS_CONFIG", dir.path().join("no-config.toml"))
        .env_remove("NITS_SOCKET")
        .env_remove("NITS_WS")
        .args(["daemon", "stdio", "--start-policy", "require-running"])
        .args(["--ws-listen", "127.0.0.1:7699"])
        .args(["--idle-exit", "60"])
        .args(["--data-dir", dir.path().to_str().unwrap()])
        .arg("--socket")
        .arg(dir.path().join("nothing.sock"))
        .assert()
        .code(3);
}

/// Every subcommand's help renders — a cheap guard on the flag definitions
/// the test above cannot reach (`daemon serve` blocks; `mcp` reads stdin).
#[test]
fn every_subcommand_definition_builds() {
    for args in [
        &["--help"][..],
        &["daemon", "--help"],
        &["daemon", "serve", "--help"],
        &["daemon", "stdio", "--help"],
        &["mcp", "--help"],
        &["context", "add-ssh", "--help"],
    ] {
        Command::cargo_bin("nits")
            .unwrap()
            .args(args)
            .assert()
            .success();
    }
}

/// An ssh context written before the daemon became `nits daemon serve` names
/// a `nitsd` binary that cannot serve `daemon stdio`. Running it anyway would
/// report the host unreachable for a reason the user cannot see, so the
/// context is refused with the edit to make.
#[test]
fn a_legacy_ssh_context_says_how_to_migrate_it() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(
        &cfg,
        "[contexts.box]\ntype = \"Ssh\"\nhost = \"build-box\"\nnitsd = \"/opt/bin/nitsd\"\n",
    )
    .unwrap();
    let nits = || {
        let mut c = Command::cargo_bin("nits").unwrap();
        c.env("NITS_CONFIG", &cfg)
            .env_remove("NITS_SOCKET")
            .env_remove("NITS_CONTEXT");
        c
    };
    // `status` never fails the process — it reports per context — so the
    // guidance has to reach the user through the row itself.
    nits()
        .args(["-c", "box", "daemon", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("/opt/bin/nitsd"))
        .stdout(predicate::str::contains("bin = "));
    nits()
        .args(["-c", "box", "workspace", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nits daemon serve"));
}
