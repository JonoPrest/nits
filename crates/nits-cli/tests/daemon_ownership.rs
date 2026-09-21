//! Lifecycle completion follows store ownership, including Git work that outlives
//! the socket. Every scenario uses the actual CLI and a real gated clean filter.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use nits_test_support::{RepoBuilder, TestRepo, files};
use tokio::process::Child as AsyncChild;

const BUDGET: Duration = Duration::from_secs(25);

fn quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
}

fn executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn configure_contexts(dir: &Path, socket: &Path, ssh: &Path) {
    let mut config = nits_config::Config::default();
    for (name, data_dir, endpoint) in [
        ("box", dir.join("data"), socket.to_path_buf()),
        (
            "socket-only",
            dir.join("unrelated-data"),
            socket.to_path_buf(),
        ),
        (
            "socket-alias",
            dir.join("unrelated-data"),
            dir.join("alias.sock"),
        ),
    ] {
        config.contexts.insert(
            name.parse().unwrap(),
            nits_config::Context::Local {
                data_dir: Some(data_dir),
                socket: Some(endpoint),
            },
        );
    }
    config.contexts.insert(
        "remote".parse().unwrap(),
        nits_config::Context::Ssh {
            host: "test-host".into(),
            bin: nits_config::RemoteBin::Default,
            args: Vec::new(),
            ssh: Some(ssh.to_str().unwrap().into()),
        },
    );
    config.save(&dir.join("config.toml")).unwrap();
}

struct Fixture {
    dir: tempfile::TempDir,
    repo: TestRepo,
    daemon: Child,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = RepoBuilder::new()
            .commit("base", files!["file.txt" => "base\n"])
            .build()
            .unwrap();
        let data = dir.path().join("data");
        let socket = dir.path().join("daemon.sock");
        let filter = dir.path().join("filter");
        executable(
            &filter,
            &format!(
                "#!/bin/sh\nif test -e {}; then\n touch {}\n while ! test -e {}; do sleep .01; done\nfi\ncat\n",
                quote(&dir.path().join("armed")),
                quote(&dir.path().join("entered")),
                quote(&dir.path().join("release")),
            ),
        );
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .current_dir(repo.path())
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        };
        git(&["config", "filter.delay.clean", &quote(&filter)]);
        std::fs::write(
            repo.path().join(".gitattributes"),
            "file.txt filter=delay\n",
        )
        .unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "clean filter"]);
        let binary = Path::new(env!("CARGO_BIN_EXE_nits"));
        let ssh = dir.path().join("ssh");
        executable(
            &ssh,
            &format!(
                "#!/bin/sh\nshift 2\nexec {} \"$@\" --data-dir {} --socket {}\n",
                quote(binary),
                quote(&data),
                quote(&socket),
            ),
        );
        let launcher = dir.path().join("launcher");
        executable(
            &launcher,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$$\" >> {}\nexec {} \"$@\"\n",
                quote(&dir.path().join("pids")),
                quote(binary),
            ),
        );
        std::os::unix::fs::symlink("daemon.sock", dir.path().join("alias.sock")).unwrap();
        configure_contexts(dir.path(), &socket, &ssh);
        let log = std::fs::File::create(dir.path().join("initial-daemon.log")).unwrap();
        let daemon = Command::new(binary)
            .args(["daemon", "serve", "--data-dir"])
            .arg(&data)
            .arg("--socket")
            .arg(&socket)
            .env_clear()
            .envs(
                std::env::vars_os().filter(|(key, _)| !key.to_string_lossy().starts_with("NITS_")),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        let fixture = Self { dir, repo, daemon };
        fixture.wait_for(|| socket.exists()).await;
        fixture.ok("box", &["--json", ".", "--headless"]).await;
        std::fs::write(fixture.path("armed"), "").unwrap();
        std::fs::write(
            fixture.repo.path().join("file.txt"),
            "pending filtered change\n",
        )
        .unwrap();
        fixture.wait_for(|| fixture.path("entered").exists()).await;
        fixture
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    async fn wait_for(&self, mut ready: impl FnMut() -> bool) {
        tokio::time::timeout(BUDGET, async {
            while !ready() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("condition timed out: {}", self.logs()));
    }

    fn spawn(&self, context: &str, args: &[&str]) -> AsyncChild {
        tokio::process::Command::new(env!("CARGO_BIN_EXE_nits"))
            .current_dir(self.repo.path())
            .env_clear()
            .envs(
                std::env::vars_os().filter(|(key, _)| !key.to_string_lossy().starts_with("NITS_")),
            )
            .env("NITS_USER", "Ada")
            .env("NITS_CONFIG", self.path("config.toml"))
            .env("NITS_BIN", self.path("launcher"))
            .args(["-c", context])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    async fn output(&self, process: AsyncChild) -> Output {
        tokio::time::timeout(BUDGET, process.wait_with_output())
            .await
            .unwrap_or_else(|_| panic!("CLI timed out: {}", self.logs()))
            .unwrap()
    }

    async fn ok(&self, context: &str, args: &[&str]) -> String {
        let output = self.output(self.spawn(context, args)).await;
        assert!(output.status.success(), "{output:?}\n{}", self.logs());
        String::from_utf8(output.stdout).unwrap()
    }

    fn release(&self) {
        std::fs::write(self.path("release"), "").unwrap();
        let _ = std::fs::remove_file(self.path("armed"));
    }

    fn logs(&self) -> String {
        [
            "initial-daemon.log",
            "data/nitsd.log",
            "unrelated-data/nitsd.log",
        ]
        .map(|name| std::fs::read_to_string(self.path(name)).unwrap_or_default())
        .join("\n")
    }

    async fn assert_stopping(&self) {
        self.wait_for(|| {
            matches!(
                nitsd::ownership::probe(&self.path("data")).unwrap(),
                nitsd::ownership::Ownership::Held {
                    phase: nitsd::ownership::Phase::Stopping
                }
            )
        })
        .await;
        // Both adapters must distinguish retained Core from a stopped daemon.
        for context in ["box", "remote"] {
            let status = self.ok(context, &["daemon", "status", "--json"]).await;
            let status: serde_json::Value = serde_json::from_str(&status).unwrap();
            assert_eq!(status[0]["status"], "transitioning", "{status}");
        }
    }

    async fn restart_and_stop(&self, context: &str) {
        let started = self.ok(context, &["daemon", "start"]).await;
        assert_eq!(started.trim(), "started");
        let status = self.ok(context, &["daemon", "status", "--json"]).await;
        let status: serde_json::Value = serde_json::from_str(&status).unwrap();
        assert_eq!(status[0]["status"], "running");
        self.ok(context, &["daemon", "stop"]).await;
        assert_eq!(
            nitsd::ownership::probe(&self.path("data")).unwrap(),
            nitsd::ownership::Ownership::Free
        );
        assert!(
            !self.logs().contains("Database already open"),
            "{}",
            self.logs()
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.release();
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        // Each recorded PID belongs to a daemon started by this isolated fixture.
        // Kill remaining detached daemons even when an assertion failed.
        if let Ok(pids) = std::fs::read_to_string(self.path("pids")) {
            for pid in pids.lines() {
                if let Ok(process) = Command::new("ps").args(["-p", pid, "-o", "args="]).output()
                    && String::from_utf8_lossy(&process.stdout)
                        .contains(self.dir.path().to_str().unwrap())
                {
                    let _ = Command::new("kill").args(["-TERM", pid]).output();
                }
            }
        }
        if std::thread::panicking() {
            eprintln!("daemon logs:\n{}", self.logs());
        }
    }
}

async fn stop_waits_for_git(context: &str) {
    let fixture = Fixture::new().await;
    let mut stop = fixture.spawn(context, &["daemon", "stop"]);
    fixture.assert_stopping().await;
    fixture
        .wait_for(|| !fixture.path("daemon.sock").exists())
        .await;
    let status = fixture.ok(context, &["daemon", "status", "--json"]).await;
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status[0]["status"], "transitioning");
    assert_eq!(
        status[0]["phase"],
        if context.starts_with("socket-") {
            "Unknown"
        } else {
            "Stopping"
        }
    );
    assert!(
        stop.try_wait().unwrap().is_none(),
        "stop completed before the gated Git read"
    );
    assert!(!fixture.path("pids").exists(), "a probe started a daemon");
    fixture.release();
    let output = fixture.output(stop).await;
    assert!(output.status.success(), "{output:?}\n{}", fixture.logs());
    let stopped = fixture.ok(context, &["daemon", "status", "--json"]).await;
    let stopped: serde_json::Value = serde_json::from_str(&stopped).unwrap();
    assert_eq!(stopped[0]["status"], "stopped");
    fixture.restart_and_stop(context).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_stop_waits_for_real_git_ownership_then_immediately_restarts() {
    stop_waits_for_git("box").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ssh_stop_waits_for_real_git_ownership_then_immediately_restarts() {
    stop_waits_for_git("remote").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stuck_git_stop_and_start_time_out_honestly_then_recover() {
    let fixture = Fixture::new().await;
    let stop = fixture.spawn("remote", &["daemon", "stop"]);
    fixture.assert_stopping().await;
    let output = fixture.output(stop).await;
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("daemon did not stop within"),
        "{output:?}"
    );
    fixture.assert_stopping().await;
    let start = fixture.spawn("box", &["daemon", "start"]);
    let output = fixture.output(start).await;
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("existing owner may still be stopping"),
        "{output:?}"
    );
    assert!(
        !fixture.path("pids").exists(),
        "start launched while old Core was held"
    );
    fixture.release();
    fixture.ok("remote", &["daemon", "stop"]).await;
    fixture.restart_and_stop("box").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_stop_preserves_ownership_and_start_waits_for_release() {
    let fixture = Fixture::new().await;
    let mut stop = fixture.spawn("box", &["daemon", "stop"]);
    fixture.assert_stopping().await;
    stop.kill().await.unwrap();
    stop.wait().await.unwrap();
    let mut start = fixture.spawn("remote", &["daemon", "start"]);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(start.try_wait().unwrap().is_none());
    assert!(!fixture.path("pids").exists());
    fixture.release();
    let output = fixture.output(start).await;
    assert!(output.status.success(), "{output:?}\n{}", fixture.logs());
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "started");
    fixture.ok("box", &["daemon", "stop"]).await;
    assert!(
        !fixture.logs().contains("Database already open"),
        "{}",
        fixture.logs()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socket_only_context_and_dangling_alias_retain_the_actual_daemons_ownership() {
    for context in ["socket-only", "socket-alias"] {
        stop_waits_for_git(context).await;
    }
}
