//! Running the daemon, and the stdio proxy that reaches it.
//!
//! Both used to be `nitsd`'s `main`. They are library entry points now
//! because the daemon ships inside the `nits` binary (`nits daemon serve`
//! and `nits daemon stdio`) rather than as a second executable — see
//! [`crate::launch`] for how a client starts one.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use nits_protocol::BuildInfo;
use nits_review_core::DataDir;

use crate::Daemon;
use crate::launch::{self, DaemonSpec, proxy_stdio};
use crate::server::{UnixServer, WsServer};

/// Where the daemon keeps state and who it listens to.
#[derive(Debug, Clone)]
pub struct ServeOpts {
    /// Where state lives.
    pub data_dir: PathBuf,
    /// Unix socket to listen on.
    pub socket: PathBuf,
    /// Exit after this many seconds with no client connected.
    pub idle_exit: Option<u64>,
    /// Also listen for WebSocket clients here.
    pub ws: Option<std::net::SocketAddr>,
}

impl ServeOpts {
    /// Defaults for `data_dir`: socket at `<data_dir>/nitsd.sock`, no idle
    /// exit, no WebSocket port.
    #[must_use]
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            socket: data_dir.join("nitsd.sock"),
            data_dir,
            idle_exit: None,
            ws: None,
        }
    }
}

/// Serve until `Request::Shutdown`, ctrl-c, or the idle timeout.
pub async fn serve(opts: ServeOpts) -> anyhow::Result<()> {
    let ServeOpts {
        data_dir,
        socket,
        idle_exit,
        ws,
    } = opts;
    let daemon = Daemon::open(
        &DataDir::new(&data_dir),
        BuildInfo {
            name: "nitsd".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
    )
    .with_context(|| format!("opening data dir {}", data_dir.display()))?;

    let server =
        UnixServer::bind(&socket).with_context(|| format!("binding {}", socket.display()))?;
    daemon.set_phase(crate::ownership::Phase::Serving);
    tracing::info!(socket = %socket.display(), data_dir = %data_dir.display(), "listening");
    let watcher = crate::watcher::Watcher::start(Arc::clone(&daemon));
    let shutdown = daemon.shutdown().clone();
    let signal = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("shutting down");
        signal.cancel();
    });
    if let Some(idle) = idle_exit {
        let idle = std::time::Duration::from_secs(idle);
        let d = Arc::clone(&daemon);
        let token = shutdown.clone();
        tokio::spawn(async move {
            let mut quiet_since = tokio::time::Instant::now();
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                if d.connections() > 0 {
                    quiet_since = tokio::time::Instant::now();
                } else if quiet_since.elapsed() >= idle {
                    tracing::info!(idle_secs = idle.as_secs(), "idle; exiting");
                    token.cancel();
                    return;
                }
            }
        });
    }
    let ws = match ws {
        Some(addr) => {
            let ws = WsServer::bind(addr)
                .await
                .with_context(|| format!("binding ws {addr}"))?;
            tracing::info!(ws = %ws.addr(), "listening");
            Some(tokio::spawn(ws.run(Arc::clone(&daemon), shutdown.clone())))
        }
        None => None,
    };
    server.run(Arc::clone(&daemon), shutdown).await;
    daemon.set_phase(crate::ownership::Phase::Stopping);
    if let Some(ws) = ws {
        let _ = ws.await;
    }
    watcher.stop();
    Ok(())
}

/// What [`stdio`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioOutcome {
    /// Bytes were piped until stdin closed.
    Proxied,
    /// Nothing was listening and `autostart` was off.
    NotRunning,
    /// Core is still owned while its endpoint is unavailable or shutting down.
    Transitioning { phase: crate::ownership::Phase },
}

impl StdioOutcome {
    /// SSH process outcomes, separate from daemon protocol frames.
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Proxied => 0,
            Self::NotRunning => 3,
            Self::Transitioning {
                phase: crate::ownership::Phase::Starting,
            } => 4,
            Self::Transitioning {
                phase: crate::ownership::Phase::Serving,
            } => 5,
            Self::Transitioning {
                phase: crate::ownership::Phase::Stopping,
            } => 6,
        }
    }

    pub(crate) const fn from_exit_code(code: i32) -> Option<Self> {
        match code {
            0 => Some(Self::Proxied),
            3 => Some(Self::NotRunning),
            4 => Some(Self::Transitioning {
                phase: crate::ownership::Phase::Starting,
            }),
            5 => Some(Self::Transitioning {
                phase: crate::ownership::Phase::Serving,
            }),
            6 => Some(Self::Transitioning {
                phase: crate::ownership::Phase::Stopping,
            }),
            _ => None,
        }
    }
}

/// How long an auto-started daemon stays up with no client, in seconds.
/// A daemon woken by `ssh host nits daemon stdio` should not outlive the
/// interest that woke it.
pub const AUTOSTART_IDLE_EXIT: u64 = 1800;

/// Pipe stdin/stdout to the machine's daemon — what `ssh host nits daemon
/// stdio` runs. With `autostart`, a detached daemon is started first if
/// nothing answers.
pub async fn stdio(opts: ServeOpts, autostart: bool) -> anyhow::Result<StdioOutcome> {
    let spec = DaemonSpec {
        idle_exit: Some(opts.idle_exit.unwrap_or(AUTOSTART_IDLE_EXIT)),
        ws: opts.ws,
        socket: opts.socket,
        ..DaemonSpec::for_data_dir(opts.data_dir)
    };
    if autostart {
        launch::ensure_daemon(&spec)
            .await
            .context("starting the daemon")?;
    } else {
        match launch::availability(&spec).await? {
            launch::Availability::Listening => {}
            launch::Availability::Stopped => return Ok(StdioOutcome::NotRunning),
            launch::Availability::Transitioning { phase } => {
                return Ok(StdioOutcome::Transitioning { phase });
            }
        }
    }
    proxy_stdio(&spec.socket).await?;
    Ok(StdioOutcome::Proxied)
}
