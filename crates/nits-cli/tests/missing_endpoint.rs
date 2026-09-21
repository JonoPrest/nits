//! A selected missing endpoint is independent of another daemon's occupied store.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

struct Fixture {
    dir: tempfile::TempDir,
    daemon: Child,
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("xdg-data/nits");
        let missing = dir.path().join("never-used.sock");
        let mut config = nits_config::Config::default();
        for (name, data_dir) in [("implicit", None), ("explicit", Some(data.clone()))] {
            config.contexts.insert(
                format!("missing-{name}").parse().unwrap(),
                nits_config::Context::Local {
                    data_dir: data_dir.clone(),
                    socket: Some(missing.clone()),
                },
            );
            let ssh = dir.path().join(format!("ssh-{name}"));
            let data_arg = data_dir
                .map(|path| format!(" --data-dir {}", quote(&path)))
                .unwrap_or_default();
            std::fs::write(
                &ssh,
                format!(
                    "#!/bin/sh\nshift 2\nexec {} \"$@\" --socket {}{data_arg}\n",
                    quote(Path::new(env!("CARGO_BIN_EXE_nits"))),
                    quote(&missing),
                ),
            )
            .unwrap();
            std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
            config.contexts.insert(
                format!("remote-{name}").parse().unwrap(),
                nits_config::Context::Ssh {
                    host: "test-host".into(),
                    bin: nits_config::RemoteBin::Default,
                    args: Vec::new(),
                    ssh: Some(ssh.to_str().unwrap().into()),
                },
            );
        }
        config.save(&dir.path().join("config.toml")).unwrap();
        let log = std::fs::File::create(dir.path().join("daemon.log")).unwrap();
        let daemon = Self::command(dir.path())
            .args(["daemon", "serve"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        let mut fixture = Self { dir, daemon };
        tokio::time::timeout(Duration::from_secs(5), async {
            while !data.join("nitsd.sock").exists() {
                assert!(fixture.daemon.try_wait().unwrap().is_none());
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("isolated default daemon starts");
        fixture.assert_default_healthy().await;
        fixture
    }

    fn command(dir: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_nits"));
        command
            .current_dir(dir)
            .env_clear()
            .envs(
                std::env::vars_os().filter(|(key, _)| !key.to_string_lossy().starts_with("NITS_")),
            )
            .env("NITS_CONFIG", dir.join("config.toml"))
            .env("NITS_USER", "Ada")
            .env("XDG_DATA_HOME", dir.join("xdg-data"))
            .env("XDG_CONFIG_HOME", dir.join("xdg-config"));
        command
    }

    async fn run(&self, selection: &[&str], args: &[&str]) -> Output {
        let mut command = tokio::process::Command::from(Self::command(self.dir.path()));
        tokio::time::timeout(
            Duration::from_secs(5),
            command
                .args(selection)
                .args(args)
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap_or_else(|_| panic!("missing endpoint command waited: {selection:?} {args:?}"))
        .unwrap()
    }

    async fn assert_default_healthy(&mut self) {
        assert!(self.daemon.try_wait().unwrap().is_none());
        let output = self
            .run(
                &[],
                &["--start-policy", "require-running", "workspace", "list"],
            )
            .await;
        assert!(output.status.success(), "{output:?}");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

#[tokio::test]
async fn missing_local_and_ssh_endpoints_do_not_inherit_the_default_daemons_store() {
    let mut fixture = Fixture::new().await;
    let missing = fixture.dir.path().join("never-used.sock");
    for selection in [
        vec!["--socket", missing.to_str().unwrap()],
        vec!["-c", "missing-implicit"],
        vec!["-c", "missing-explicit"],
        vec!["-c", "remote-implicit"],
        vec!["-c", "remote-explicit"],
    ] {
        let status = fixture
            .run(&selection, &["daemon", "status", "--json"])
            .await;
        assert!(status.status.success(), "{status:?}");
        let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
        assert_eq!(status[0]["status"], "stopped", "{selection:?}: {status}");
        let read = fixture
            .run(
                &selection,
                &["--start-policy", "require-running", "workspace", "list"],
            )
            .await;
        assert!(!read.status.success(), "{read:?}");
        assert!(
            String::from_utf8_lossy(&read.stderr).contains("not running"),
            "{read:?}"
        );
        let stop = fixture.run(&selection, &["daemon", "stop"]).await;
        assert!(stop.status.success(), "{stop:?}");
        assert_eq!(String::from_utf8_lossy(&stop.stdout).trim(), "not running");
        fixture.assert_default_healthy().await;
    }
    assert!(!missing.exists());
    assert!(
        !fixture.dir.path().join("never-used.sock.owner").exists(),
        "status/read/stop must not start or claim the missing endpoint"
    );
}
