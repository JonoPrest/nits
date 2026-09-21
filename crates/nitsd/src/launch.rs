//! Starting a daemon on demand and proxying stdio to it.
//!
//! One daemon per machine (the store is single-process); every other way in
//! goes through its socket. `nits daemon stdio` — what `ssh host nits daemon
//! stdio` runs — therefore does not serve inline: it connects to the local
//! socket, starting a detached daemon first if nothing answers, and pipes
//! bytes. The CLI uses the same [`ensure_daemon`] for local contexts.
//!
//! There is no separate daemon executable: the daemon *is* `nits`, run as
//! `nits daemon serve`. [`nits_binary`] finds that binary from whichever
//! process is doing the starting.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tokio::net::UnixStream;

use crate::ownership::{self, Ownership, Phase, SocketOwnership};

pub mod stdio;

/// How long to wait for a freshly started daemon to listen.
pub const START_TIMEOUT: Duration = Duration::from_secs(15);

/// The subcommand that makes `nits` be the daemon.
pub const SERVE_ARGV: [&str; 2] = ["daemon", "serve"];

/// The flag `daemon serve` takes for its WebSocket listen address. Not
/// `--ws`: that is the global *client* flag, a URL to connect to, and a
/// daemon spawned with it would try to dial itself.
pub const WS_LISTEN_FLAG: &str = "--ws-listen";

/// What to run to start a daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonSpec {
    /// The binary to run. Default: [`nits_binary`].
    pub program: PathBuf,
    /// Arguments before the daemon's own flags — the subcommand that turns
    /// `program` into a daemon. Default: [`SERVE_ARGV`].
    pub argv_prefix: Vec<String>,
    pub data_dir: PathBuf,
    pub socket: PathBuf,
    /// Exit after this many seconds without connections; `None` runs forever.
    pub idle_exit: Option<u64>,
    /// Also listen for WebSocket clients here.
    pub ws: Option<std::net::SocketAddr>,
}

impl DaemonSpec {
    /// Sensible spec for `data_dir`, socket at `<data_dir>/nitsd.sock`.
    #[must_use]
    pub fn for_data_dir(data_dir: PathBuf) -> Self {
        Self {
            program: nits_binary(),
            argv_prefix: SERVE_ARGV.iter().map(|&s| s.to_string()).collect(),
            socket: data_dir.join("nitsd.sock"),
            data_dir,
            idle_exit: None,
            ws: None,
        }
    }

    fn args(&self) -> Vec<String> {
        let mut a = self.argv_prefix.clone();
        a.extend([
            "--data-dir".to_string(),
            self.data_dir.to_string_lossy().into_owned(),
            "--socket".to_string(),
            self.socket.to_string_lossy().into_owned(),
        ]);
        if let Some(s) = self.idle_exit {
            a.push("--idle-exit".into());
            a.push(s.to_string());
        }
        if let Some(ws) = self.ws {
            a.push(WS_LISTEN_FLAG.into());
            a.push(ws.to_string());
        }
        a
    }
}

/// Environment variable naming the `nits` binary to start daemons with.
/// Tests and unusual installs set it; nothing else needs to.
pub const NITS_BIN_ENV: &str = "NITS_BIN";

/// The `nits` executable to start a daemon with.
///
/// `$NITS_BIN` wins. Otherwise a process that *is* `nits` re-executes
/// itself, so the daemon it starts is guaranteed to speak the same protocol
/// version as the client starting it — the point of shipping one binary.
/// Other embedders (the desktop app) look next to themselves, then `PATH`.
#[must_use]
pub fn nits_binary() -> PathBuf {
    if let Some(p) = std::env::var_os(NITS_BIN_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    match std::env::current_exe() {
        Ok(exe) if exe.file_stem().is_some_and(|s| s == "nits") => exe,
        _ => sibling_binary("nits"),
    }
}

/// `name` next to the running executable if present, else bare `name` (PATH).
#[must_use]
pub fn sibling_binary(name: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(name)))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Whether something accepts connections on `socket` right now.
pub async fn is_listening(socket: &Path) -> bool {
    UnixStream::connect(socket).await.is_ok()
}

/// A closed endpoint is not stopped while background Core users own its store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Listening,
    Transitioning { phase: Phase },
    Stopped,
}

/// Probe the selected socket only. The configured store is an independent
/// startup constraint, not evidence that this endpoint has an owner.
pub async fn availability(spec: &DaemonSpec) -> std::io::Result<Availability> {
    let owner = match ownership::probe_socket(&spec.socket)? {
        SocketOwnership::Tracked(owner) => owner,
        // Legacy daemons have no endpoint guard. Their listener below is the
        // only evidence available; an unrelated store cannot establish it.
        SocketOwnership::Untracked => Ownership::Free,
    };
    if let Ownership::Held {
        phase: phase @ (Phase::Starting | Phase::Stopping),
    } = owner
    {
        return Ok(Availability::Transitioning { phase });
    }
    if is_listening(&spec.socket).await {
        return Ok(Availability::Listening);
    }
    Ok(match owner {
        Ownership::Free => Availability::Stopped,
        Ownership::Held { phase } => Availability::Transitioning { phase },
    })
}

/// Start a daemon detached from this process: no inherited stdio, and
/// immune to the `SIGHUP` an ending ssh session sends (via `nohup` when
/// available). Its log goes to `<data_dir>/nitsd.log`.
pub fn spawn_detached(spec: &DaemonSpec) -> std::io::Result<()> {
    std::fs::create_dir_all(&spec.data_dir)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(spec.data_dir.join("nitsd.log"))?;
    let use_nohup = Command::new("nohup")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok();
    let mut cmd = if use_nohup {
        let mut c = Command::new("nohup");
        c.arg(&spec.program);
        c
    } else {
        Command::new(&spec.program)
    };
    cmd.args(spec.args())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()?;
    Ok(())
}

/// Make sure a daemon answers on `spec.socket`, starting one if needed.
/// Returns whether one was started.
pub async fn ensure_daemon(spec: &DaemonSpec) -> std::io::Result<bool> {
    let start = Instant::now();
    let mut spawned = false;
    loop {
        match availability(spec).await? {
            Availability::Listening => return Ok(spawned),
            Availability::Stopped if !spawned => {
                // Status/stop concern the selected endpoint. Starting also
                // requires the explicitly configured store to be unowned; do
                // not silently substitute an endpoint's previous data path.
                if ownership::probe(&spec.data_dir)? == Ownership::Free {
                    spawn_detached(spec)?;
                    spawned = true;
                }
            }
            Availability::Stopped | Availability::Transitioning { .. } => {}
        }
        if start.elapsed() > START_TIMEOUT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "daemon did not become ready on {}; an existing owner may still be stopping (see {})",
                    spec.socket.display(),
                    spec.data_dir.join("nitsd.log").display()
                ),
            ));
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

/// Pipe this process's stdin/stdout to the daemon socket. Stdin EOF closes
/// the request half while remaining responses drain; daemon EOF ends the
/// proxy even if stdin is still open. A failed connection is never replayed.
///
/// This exclusively consumes stdin while active and restores its descriptor
/// flags before returning, including when the future is cancelled.
pub async fn proxy_stdio(socket: &Path) -> std::io::Result<()> {
    stdio::proxy(UnixStream::connect(socket).await?).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_untracked_endpoint_does_not_inherit_an_unrelated_stores_phase() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = DaemonSpec::for_data_dir(dir.path().join("store"));
        spec.socket = dir.path().join("never-used.sock");
        let store = ownership::Lease::acquire(&spec.data_dir).unwrap();
        for phase in [Phase::Starting, Phase::Serving, Phase::Stopping] {
            store.set_phase(phase).unwrap();
            assert_eq!(availability(&spec).await.unwrap(), Availability::Stopped);
            assert_eq!(
                ownership::probe(&spec.data_dir).unwrap(),
                Ownership::Held { phase }
            );
        }
        assert_eq!(
            ownership::probe_socket(&spec.socket).unwrap(),
            SocketOwnership::Untracked,
            "status must not create an endpoint guard"
        );
    }

    #[tokio::test]
    async fn legacy_listeners_and_starting_endpoint_guards_retain_their_own_status() {
        let dir = tempfile::tempdir().unwrap();
        let spec = DaemonSpec::for_data_dir(dir.path().to_path_buf());
        let store = ownership::Lease::acquire(&spec.data_dir).unwrap();
        store.set_phase(Phase::Stopping).unwrap();
        let listener = tokio::net::UnixListener::bind(&spec.socket).unwrap();
        assert_eq!(availability(&spec).await.unwrap(), Availability::Listening);
        drop(listener);
        std::fs::remove_file(&spec.socket).unwrap();
        let endpoint = ownership::Lease::for_socket(&spec.socket).unwrap();
        assert_eq!(
            availability(&spec).await.unwrap(),
            Availability::Transitioning {
                phase: Phase::Unknown
            }
        );
        drop(endpoint);
        assert_eq!(availability(&spec).await.unwrap(), Availability::Stopped);
    }

    #[tokio::test]
    async fn starting_an_untracked_endpoint_still_waits_for_the_configured_store() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let mut spec = DaemonSpec::for_data_dir(dir.path().join("store"));
        spec.socket = dir.path().join("never-used.sock");
        spec.program = dir.path().join("launcher");
        // The marker is the socket path passed to the launcher, so this test
        // catches an accidental spawn without opening a real daemon or store.
        std::fs::write(&spec.program, "#!/bin/sh\n: > \"$6\"\n").unwrap();
        std::fs::set_permissions(&spec.program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = ownership::Lease::acquire(&spec.data_dir).unwrap();
        store.set_phase(Phase::Serving).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), ensure_daemon(&spec))
                .await
                .is_err()
        );
        assert!(
            !spec.socket.exists(),
            "start must not spawn for an owned store"
        );
        assert!(!spec.data_dir.join("nitsd.log").exists());
    }

    /// The argv a spawned daemon gets. A default spec that did not start
    /// with the serve subcommand would run the *client* — which would try
    /// to start a daemon of its own, forever.
    #[test]
    fn a_default_spec_runs_the_serve_subcommand_first() {
        let spec = DaemonSpec::for_data_dir(PathBuf::from("/tmp/d"));
        let args = spec.args();
        assert_eq!(&args[..2], ["daemon", "serve"]);
        assert!(args.contains(&"--data-dir".to_string()));
        assert!(args.contains(&"/tmp/d/nitsd.sock".to_string()));
    }

    #[test]
    fn optional_flags_are_passed_through_after_the_subcommand() {
        let spec = DaemonSpec {
            idle_exit: Some(30),
            ws: Some("127.0.0.1:7677".parse().expect("addr")),
            ..DaemonSpec::for_data_dir(PathBuf::from("/tmp/d"))
        };
        let args = spec.args();
        assert_eq!(&args[..2], ["daemon", "serve"]);
        assert!(args.windows(2).any(|w| w == ["--idle-exit", "30"]));
        assert!(
            args.windows(2)
                .any(|w| w == [WS_LISTEN_FLAG, "127.0.0.1:7677"]),
            "the listen flag, not the client's --ws URL: {args:?}"
        );
    }
}
