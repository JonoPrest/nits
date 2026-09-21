//! Maintenance remains bound to the configured remote executable and arguments.
#![cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Remote {
    dir: tempfile::TempDir,
    config: PathBuf,
    data: PathBuf,
}

impl Remote {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("installed 'Nits'");
        std::fs::create_dir(&install).unwrap();
        let binary = install.join("nits");
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_nits"), &binary).unwrap();
        let ssh = dir.path().join("shell-ssh");
        // OpenSSH executes a joined command through the remote shell. An argv
        // passthrough shim would hide quoting bugs for spaces and apostrophes.
        std::fs::write(&ssh, "#!/bin/sh\nshift\nexec sh -c \"$*\"\n").unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
        let data = dir.path().join("remote data 'store'");
        let mut config = nits_config::Config::default();
        config.contexts.insert(
            "remote".parse().unwrap(),
            nits_config::Context::Ssh {
                host: "test-host".into(),
                bin: nits_config::RemoteBin::Nits(binary.to_string_lossy().into()),
                args: vec!["--data-dir".into(), data.to_string_lossy().into()],
                ssh: Some(ssh.to_string_lossy().into()),
            },
        );
        let path = dir.path().join("config.toml");
        config.save(&path).unwrap();
        Self {
            dir,
            config: path,
            data,
        }
    }

    fn cli(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_nits"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("NITS_") {
                command.env_remove(key);
            }
        }
        command
            .env(
                "NITS_BIN",
                self.dir.path().join("unrelated-missing-program"),
            )
            .env("XDG_DATA_HOME", self.dir.path().join("unrelated-default"))
            .env("XDG_CONFIG_HOME", self.dir.path().join("unrelated-config"))
            .args([
                "--config",
                self.config.to_str().unwrap(),
                "--context",
                "remote",
                "--json",
            ])
            .args(args)
            .output()
            .unwrap()
    }

    fn value(&self, args: &[&str]) -> serde_json::Value {
        let result = self.cli(args);
        assert!(
            result.status.success(),
            "{:?}: {}",
            args,
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_slice(&result.stdout).unwrap()
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        let _ = self.cli(&["daemon", "stop"]);
    }
}

#[test]
fn ssh_management_preserves_literal_install_selection_and_rejects_stale_preflight() {
    let fixture = Remote::new();
    let before = std::fs::read(&fixture.config).unwrap();
    let status = fixture.value(&["daemon", "upgrade-status"]);
    assert_eq!(status["running"]["type"], "Stopped");
    assert_eq!(status["installed"]["type"], "Available");
    assert!(!fixture.data.exists(), "status opened a store");
    assert!(
        !fixture
            .cli(&["--start-policy", "require-running", "workspace", "list"])
            .status
            .success()
    );
    assert!(!fixture.data.exists(), "require-running started a store");
    fixture.value(&["workspace", "add", "remote workspace"]);
    let workspaces = fixture.value(&["workspace", "list"]);
    let status = fixture.value(&["daemon", "upgrade-status"]);
    assert_eq!(status["running"]["build"], status["installed"]["build"]);
    let wrong = "01".repeat(32);
    let refused = fixture.cli(&["daemon", "upgrade", "--expected-build", &wrong]);
    assert!(!refused.status.success());
    let rejected: serde_json::Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(rejected["type"], "Failed");
    assert_eq!(rejected["failure"]["kind"], "ContextMismatch");
    assert_eq!(fixture.value(&["workspace", "list"]), workspaces);
    let digest = status["installed"]["build"]["digest"].as_str().unwrap();
    let current = fixture.value(&["daemon", "upgrade", "--expected-build", digest]);
    assert_eq!(current["type"], "AlreadyCurrent");
    assert_eq!(std::fs::read(&fixture.config).unwrap(), before);
    assert!(!fixture.dir.path().join("unrelated-default").exists());
    let stopped = fixture.cli(&["daemon", "stop"]);
    assert!(stopped.status.success());
    assert_eq!(
        fixture.value(&["daemon", "upgrade-status"])["running"]["type"],
        "Stopped"
    );
}

#[test]
fn unavailable_ssh_is_an_error_without_touching_local_store() {
    let fixture = Remote::new();
    let failed_ssh = fixture.dir.path().join("unreachable-ssh");
    std::fs::write(&failed_ssh, "#!/bin/sh\nexit 17\n").unwrap();
    std::fs::set_permissions(&failed_ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut config = nits_config::Config::load(Path::new(&fixture.config)).unwrap();
    let nits_config::Context::Ssh { ssh, .. } = config.contexts.get_mut("remote").unwrap() else {
        panic!("SSH")
    };
    *ssh = Some(failed_ssh.to_string_lossy().into());
    config.save(&fixture.config).unwrap();
    for args in [["daemon", "upgrade-status"], ["daemon", "upgrade"]] {
        let result = fixture.cli(&args);
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("incumbent was not assumed stopped")
        );
    }
    assert!(!fixture.data.exists());
    assert!(!fixture.dir.path().join("unrelated-default").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn following_events_resume_the_flushed_cursor_after_transport_replacement() {
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, BufReader};
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("stream.sock");
    let daemon = nitsd::Daemon::open(
        &nits_review_core::DataDir::new(directory.path()),
        nits_protocol::BuildInfo {
            name: "stream".into(),
            version: "1".into(),
        },
    )
    .unwrap();
    let server = nitsd::server::UnixServer::bind(&socket).unwrap();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let task = tokio::spawn(server.run(Arc::clone(&daemon), shutdown.clone()));
    let identity = stream_identity();
    let mut ops = nitsd::ops::Ops::new(
        nitsd::client::Client::connect_unix(&socket, identity.clone())
            .await
            .unwrap(),
    );
    ops.create_workspace("first".into()).await.unwrap();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_nits"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("NITS_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .env("XDG_CONFIG_HOME", directory.path().join("config"))
        .env("NITS_BIN", directory.path().join("must-not-autostart"))
        .args([
            "--data-dir",
            directory.path().to_str().unwrap(),
            "--socket",
            socket.to_str().unwrap(),
            "--start-policy",
            "require-running",
            "--json",
            "events",
            "--follow",
            "--since",
            "0",
        ])
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let first = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let first: nits_protocol::Event = serde_json::from_str(&first).unwrap();
    assert_eq!(first.seq.get(), 1);
    shutdown.cancel();
    task.await.unwrap();
    drop(ops);
    let server = nitsd::server::UnixServer::bind(&socket).unwrap();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let task = tokio::spawn(server.run(Arc::clone(&daemon), shutdown.clone()));
    let mut ops = nitsd::ops::Ops::new(
        nitsd::client::Client::connect_unix(&socket, identity)
            .await
            .unwrap(),
    );
    ops.create_workspace("during reconnect".into())
        .await
        .unwrap();
    ops.create_workspace("after reconnect".into())
        .await
        .unwrap();
    for expected in [2, 3] {
        let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let event: nits_protocol::Event = serde_json::from_str(&line).unwrap();
        assert_eq!(
            event.seq.get(),
            expected,
            "gap or duplicate after transport loss"
        );
    }
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    drop(ops);
    shutdown.cancel();
    task.await.unwrap();
    daemon.shutdown().cancel();
}

#[test]
fn replacement_reports_typed_migration_failure_without_removing_the_newer_store() {
    use redb::ReadableDatabase as _;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.redb");
    drop(nits_review_core::store::Store::open(&path).unwrap());
    let future = nits_protocol::SchemaVersion::CURRENT.get() + 1;
    {
        let db = redb::Database::open(&path).unwrap();
        let transaction = db.begin_write().unwrap();
        transaction
            .open_table(redb::TableDefinition::<&str, u64>::new("meta"))
            .unwrap()
            .insert("schema_version", u64::from(future))
            .unwrap();
        transaction.commit().unwrap();
    }
    let id = nits_protocol::UpgradeId::from_parts(1, 5);
    let result = replacement_start(dir.path(), &dir.path().join("daemon.sock"), id);
    assert!(!result.status.success());
    let failure: nits_protocol::UpgradeFailure = serde_json::from_slice(
        &std::fs::read(dir.path().join(format!("upgrade-start-{id}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(
        failure.stage,
        nits_protocol::UpgradeStage::StartingReplacement
    );
    assert_eq!(
        failure.kind,
        nits_protocol::UpgradeFailureKind::MigrationFailed
    );
    assert!(!dir.path().join("daemon.sock").exists());
    let db = redb::Database::open(&path).unwrap();
    let transaction = db.begin_read().unwrap();
    assert_eq!(
        transaction
            .open_table(redb::TableDefinition::<&str, u64>::new("meta"))
            .unwrap()
            .get("schema_version")
            .unwrap()
            .unwrap()
            .value(),
        u64::from(future)
    );
}

#[test]
fn replacement_reports_startup_failure_and_preserves_an_occupied_regular_path() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    std::fs::write(&socket, "unrelated file").unwrap();
    let id = nits_protocol::UpgradeId::from_parts(1, 6);
    let result = replacement_start(dir.path(), &socket, id);
    assert!(!result.status.success());
    let failure: nits_protocol::UpgradeFailure = serde_json::from_slice(
        &std::fs::read(dir.path().join(format!("upgrade-start-{id}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(
        failure.stage,
        nits_protocol::UpgradeStage::StartingReplacement
    );
    assert_eq!(failure.kind, nits_protocol::UpgradeFailureKind::StartFailed);
    assert_eq!(std::fs::read_to_string(socket).unwrap(), "unrelated file");
    nits_review_core::Core::open(&nits_review_core::DataDir::new(dir.path())).unwrap();
}

fn replacement_start(data: &Path, socket: &Path, operation: nits_protocol::UpgradeId) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nits"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("NITS_") {
            command.env_remove(key);
        }
    }
    command
        .env("NITS_UPGRADE_OPERATION", operation.to_string())
        .env("XDG_CONFIG_HOME", data.join("config"))
        .args([
            "daemon",
            "serve",
            "--data-dir",
            data.to_str().unwrap(),
            "--socket",
            socket.to_str().unwrap(),
        ])
        .output()
        .unwrap()
}

fn stream_identity() -> nitsd::client::Identity {
    nitsd::client::Identity {
        client_id: nits_protocol::ClientId::from_parts(1, 1),
        client: nits_protocol::BuildInfo {
            name: "producer".into(),
            version: "1".into(),
        },
        author: nits_protocol::Author::Human {
            name: "ada".into(),
            machine: "test".into(),
        },
    }
}
