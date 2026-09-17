//! Reaching a daemon through a [`Context`]: connect (starting the daemon on
//! demand where the context allows), probe status, start, stop. The CLI,
//! the MCP server and the desktop app all go through here so a machine's
//! daemon is managed one way.

use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use nits_config::Context;
use nits_protocol::{Author, BuildInfo, ProtocolVersion, Request, Response, RpcError};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::process::{ChildStdin, ChildStdout};
use tokio_util::sync::CancellationToken;

use crate::client::{Client, ClientError, Identity};
use crate::launch::{self, DaemonSpec};
use crate::transport::FramedConnection;

const STOP_TIMEOUT: Duration = Duration::from_secs(15);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(30);

#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error("{0}")]
    Client(Box<ClientError>),
    #[error("{what}: {source}")]
    Io {
        what: String,
        #[source]
        source: std::io::Error,
    },
    #[error("a WebSocket context's daemon is managed elsewhere")]
    NotManaged,
    #[error("daemon is not running")]
    NotRunning,
    #[error("daemon did not stop within {after:?}")]
    StopTimedOut { after: Duration },
    #[error("this context names a `nitsd` binary on {host} ({nitsd}), which cannot serve: {help}")]
    LegacyNitsd {
        host: String,
        nitsd: String,
        help: &'static str,
    },
    #[error("unexpected response shape")]
    Shape,
}

impl From<ClientError> for ContextError {
    fn from(e: ClientError) -> Self {
        ContextError::Client(Box::new(e))
    }
}

fn io(what: impl Into<String>) -> impl FnOnce(std::io::Error) -> ContextError {
    move |source| ContextError::Io {
        what: what.into(),
        source,
    }
}

/// Whether a context's daemon is up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Running {
        daemon: BuildInfo,
    },
    Stopped,
    /// Could not even ask, e.g. ssh failed. Carries the reason.
    Unreachable {
        reason: String,
    },
}

/// Whether a managed local or SSH endpoint may start its daemon while
/// connecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartPolicy {
    /// Start the daemon when nothing is listening yet.
    StartIfNeeded,
    /// Connect only when the daemon is already running.
    RequireRunning,
}

/// A runnable SSH target after the legacy config spelling has been rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    host: String,
    nits: String,
    args: Vec<String>,
    ssh: Option<String>,
}

/// A context resolved into exactly the information needed for every dial.
///
/// Local defaults and the runnable SSH binary are resolved once. WebSocket
/// endpoints carry no start policy because their daemon is managed elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonEndpoint {
    Local {
        spec: DaemonSpec,
        start: StartPolicy,
    },
    Ssh {
        target: SshTarget,
        start: StartPolicy,
    },
    WebSocket {
        url: String,
    },
}

impl DaemonEndpoint {
    /// Fallback identity for standalone hosts. CLI hosts replace SSH's host
    /// name with the selected saved context, preserving custom SSH settings.
    pub fn reference_context(
        &self,
    ) -> Result<nits_protocol::ReferenceContext, nits_protocol::ReferenceError> {
        match self {
            Self::Local { spec, .. } => nits_protocol::ReferenceContext::socket(
                &std::path::absolute(&spec.socket)
                    .map_err(|_| nits_protocol::ReferenceError::Malformed)?
                    .to_string_lossy(),
            ),
            Self::Ssh { target, .. } => nits_protocol::ReferenceContext::named(&target.host),
            Self::WebSocket { url } => nits_protocol::ReferenceContext::websocket(url),
        }
    }

    /// Resolve a configured context for repeated connection attempts.
    pub fn resolve(ctx: &Context, start: StartPolicy) -> Result<Self, ContextError> {
        match ctx {
            Context::Local { data_dir, socket } => {
                let spec =
                    local_spec(data_dir.as_ref(), socket.as_ref()).map_err(io("data dir"))?;
                Ok(Self::Local { spec, start })
            }
            Context::Ssh {
                host,
                bin,
                args,
                ssh,
            } => {
                let nits = match bin {
                    nits_config::RemoteBin::Legacy(nitsd) => {
                        return Err(ContextError::LegacyNitsd {
                            host: host.clone(),
                            nitsd: nitsd.clone(),
                            help: Context::LEGACY_NITSD_HELP,
                        });
                    }
                    nits_config::RemoteBin::Default => "nits".to_owned(),
                    nits_config::RemoteBin::Nits(bin) => bin.clone(),
                };
                Ok(Self::Ssh {
                    target: SshTarget {
                        host: host.clone(),
                        nits,
                        args: args.clone(),
                        ssh: ssh.clone(),
                    },
                    start,
                })
            }
            Context::Ws { url } => Ok(Self::WebSocket { url: url.clone() }),
        }
    }
}

/// `$XDG_DATA_HOME/nits` or `~/.local/share/nits`.
pub fn default_data_dir() -> std::io::Result<PathBuf> {
    if let Ok(x) = std::env::var("XDG_DATA_HOME") {
        return Ok(PathBuf::from(x).join("nits"));
    }
    let home = std::env::var("HOME")
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME is not set"))?;
    Ok(PathBuf::from(home).join(".local/share/nits"))
}

/// Resolve a `Local` context's paths.
pub fn local_spec(
    data_dir: Option<&PathBuf>,
    socket: Option<&PathBuf>,
) -> std::io::Result<DaemonSpec> {
    let data_dir = match data_dir {
        Some(d) => d.clone(),
        None => default_data_dir()?,
    };
    let mut spec = DaemonSpec::for_data_dir(data_dir);
    if let Some(s) = socket {
        spec.socket.clone_from(s);
    }
    Ok(spec)
}

/// `ssh <host> <bin> daemon stdio <args...>`: the remote `nits` proxies to
/// (and starts) the daemon on its own machine.
fn ssh_command(target: &SshTarget) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(target.ssh.as_deref().unwrap_or("ssh"));
    cmd.arg(&target.host)
        .arg(&target.nits)
        .args(["daemon", "stdio"])
        .args(&target.args);
    cmd
}

/// The two SSH pipes and a signal to the task which owns and reaps the child.
#[derive(Debug)]
struct SshStream {
    stdout: ChildStdout,
    stdin: ChildStdin,
    child_done: CancellationToken,
}

impl AsyncRead for SshStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdout).poll_read(cx, buf)
    }
}

impl AsyncWrite for SshStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.stdin).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.stdin).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let result = Pin::new(&mut self.stdin).poll_shutdown(cx);
        if matches!(result, Poll::Ready(Ok(()))) {
            self.child_done.cancel();
        }
        result
    }
}

impl Drop for SshStream {
    fn drop(&mut self) {
        self.child_done.cancel();
    }
}

fn dial_ssh(target: &SshTarget, start: StartPolicy) -> Result<FramedConnection, ContextError> {
    let mut cmd = ssh_command(target);
    if start == StartPolicy::RequireRunning {
        // Exits 3 rather than waking a daemon, so a client can probe or stop
        // a remote without starting one.
        cmd.args(["--start-policy", "require-running"]);
    }
    cmd.kill_on_drop(true);
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(io(format!("running ssh {}", target.host)))?;
    let stdin = child.stdin.take().ok_or_else(|| ContextError::Io {
        what: "ssh stdin".into(),
        source: std::io::Error::other("not piped"),
    })?;
    let stdout = child.stdout.take().ok_or_else(|| ContextError::Io {
        what: "ssh stdout".into(),
        source: std::io::Error::other("not piped"),
    })?;
    let child_done = CancellationToken::new();
    let reap = child_done.clone();
    tokio::spawn(async move {
        tokio::select! {
            result = child.wait() => {
                if let Err(err) = result {
                    tracing::debug!(%err, "waiting for ssh child failed");
                }
            }
            () = reap.cancelled() => {
                if let Err(err) = child.kill().await {
                    tracing::debug!(%err, "stopping ssh child failed");
                }
            }
        }
    });
    Ok(FramedConnection::byte(SshStream {
        stdout,
        stdin,
        child_done,
    }))
}

/// Dial an endpoint without performing the Nits protocol handshake.
///
/// `nits-client-core` owns that handshake in UI hosts, while the CLI/MCP
/// client feeds these same framed halves to [`Client::handshake_framed`].
pub async fn dial(endpoint: &DaemonEndpoint) -> Result<FramedConnection, ContextError> {
    match endpoint {
        DaemonEndpoint::Local { spec, start } => {
            match start {
                StartPolicy::StartIfNeeded => {
                    launch::ensure_daemon(spec)
                        .await
                        .map_err(io("starting the daemon"))?;
                }
                StartPolicy::RequireRunning => {
                    if !launch::is_listening(&spec.socket).await {
                        return Err(ContextError::NotRunning);
                    }
                }
            }
            let stream = tokio::net::UnixStream::connect(&spec.socket)
                .await
                .map_err(io(format!("connecting to {}", spec.socket.display())))?;
            Ok(FramedConnection::byte(stream))
        }
        DaemonEndpoint::Ssh { target, start } => dial_ssh(target, *start),
        DaemonEndpoint::WebSocket { url } => {
            let (socket, _) = tokio_tungstenite::connect_async(url)
                .await
                .map_err(|source| ContextError::Io {
                    what: format!("connecting to {url}"),
                    source: std::io::Error::other(source),
                })?;
            Ok(FramedConnection::web_socket(socket))
        }
    }
}

/// Connect through `ctx`. [`StartPolicy::StartIfNeeded`] starts the daemon if
/// nothing answers (local and SSH contexts only); otherwise a stopped daemon is
/// [`ContextError::NotRunning`].
pub async fn connect(
    ctx: &Context,
    identity: Identity,
    start: StartPolicy,
) -> Result<Client, ContextError> {
    let endpoint = DaemonEndpoint::resolve(ctx, start)?;
    connect_endpoint(&endpoint, identity, ProtocolVersion::CURRENT).await
}

async fn connect_endpoint(
    endpoint: &DaemonEndpoint,
    identity: Identity,
    protocol: ProtocolVersion,
) -> Result<Client, ContextError> {
    let ssh_requires_running = matches!(
        endpoint,
        DaemonEndpoint::Ssh {
            start: StartPolicy::RequireRunning,
            ..
        }
    );
    let (read, write) = dial(endpoint).await?.into_parts();
    match Client::handshake_framed(read, write, identity, protocol).await {
        Ok(client) => Ok(client),
        Err(ClientError::Closed) if ssh_requires_running => Err(ContextError::NotRunning),
        Err(err) => Err(err.into()),
    }
}

/// Probe without starting anything.
pub async fn status(ctx: &Context) -> Status {
    match ctx {
        Context::Local { data_dir, socket } => {
            let spec = match local_spec(data_dir.as_ref(), socket.as_ref()) {
                Ok(s) => s,
                Err(e) => {
                    return Status::Unreachable {
                        reason: e.to_string(),
                    };
                }
            };
            if !launch::is_listening(&spec.socket).await {
                return Status::Stopped;
            }
            match Client::connect_unix(&spec.socket, probe_identity()).await {
                Ok(c) => Status::Running {
                    daemon: c.welcome.daemon.clone(),
                },
                Err(e) => Status::Unreachable {
                    reason: e.to_string(),
                },
            }
        }
        Context::Ssh { .. } => {
            match connect(ctx, probe_identity(), StartPolicy::RequireRunning).await {
                Ok(c) => Status::Running {
                    daemon: c.welcome.daemon.clone(),
                },
                Err(ContextError::NotRunning) => Status::Stopped,
                Err(e) => Status::Unreachable {
                    reason: e.to_string(),
                },
            }
        }
        Context::Ws { url } => match Client::connect_ws(url, probe_identity()).await {
            Ok(c) => Status::Running {
                daemon: c.welcome.daemon.clone(),
            },
            Err(ClientError::Codec(_)) => Status::Stopped,
            Err(e) => Status::Unreachable {
                reason: e.to_string(),
            },
        },
    }
}

/// Start the daemon if it is not running. Returns whether it was started.
pub async fn start(ctx: &Context) -> Result<bool, ContextError> {
    match ctx {
        Context::Local { data_dir, socket } => {
            let spec = local_spec(data_dir.as_ref(), socket.as_ref()).map_err(io("data dir"))?;
            launch::ensure_daemon(&spec)
                .await
                .map_err(io("starting the daemon"))
        }
        Context::Ssh { .. } => {
            let was_running = matches!(status(ctx).await, Status::Running { .. });
            drop(connect(ctx, probe_identity(), StartPolicy::StartIfNeeded).await?);
            Ok(!was_running)
        }
        Context::Ws { .. } => Err(ContextError::NotManaged),
    }
}

/// Ask the daemon to exit. Ok(false) if it was not running.
pub async fn stop(ctx: &Context) -> Result<bool, ContextError> {
    if matches!(ctx, Context::Ws { .. }) {
        return Err(ContextError::NotManaged);
    }
    let endpoint = DaemonEndpoint::resolve(ctx, StartPolicy::RequireRunning)?;
    let client = match connect_endpoint(&endpoint, probe_identity(), ProtocolVersion::CURRENT).await
    {
        Ok(c) => c,
        Err(ContextError::NotRunning) => return Ok(false),
        Err(ContextError::Client(error)) => {
            let (requested, supported) = match *error {
                ClientError::Rejected(RpcError::UnsupportedProtocol {
                    requested,
                    supported,
                }) => (requested, supported),
                other => return Err(ContextError::Client(Box::new(other))),
            };
            let Some(protocol) = shutdown_protocol(&supported) else {
                return Err(ContextError::Client(Box::new(ClientError::Rejected(
                    RpcError::UnsupportedProtocol {
                        requested,
                        supported,
                    },
                ))));
            };
            connect_endpoint(&endpoint, probe_identity(), protocol).await?
        }
        Err(e) => return Err(e),
    };
    match client.request(Request::Shutdown).await? {
        Response::ShuttingDown => {
            drop(client);
            wait_until_stopped(&endpoint).await?;
            Ok(true)
        }
        _ => Err(ContextError::Shape),
    }
}

/// Wait until a managed endpoint no longer accepts connections.
///
/// The shutdown response is sent before the daemon cancels its accept loop,
/// so returning immediately would let a following `daemon start` reconnect to
/// the process that is still exiting. The timeout keeps a broken remote
/// lifecycle from hanging its caller indefinitely.
async fn wait_until_stopped(endpoint: &DaemonEndpoint) -> Result<(), ContextError> {
    let wait = async {
        loop {
            match endpoint {
                DaemonEndpoint::Local { spec, .. } => {
                    if !launch::is_listening(&spec.socket).await {
                        return Ok(());
                    }
                }
                DaemonEndpoint::Ssh { .. } => {
                    match connect_endpoint(endpoint, probe_identity(), ProtocolVersion::CURRENT)
                        .await
                    {
                        Err(ContextError::NotRunning) => return Ok(()),
                        Ok(client) => drop(client),
                        Err(ContextError::Client(error))
                            if matches!(
                                &*error,
                                ClientError::Rejected(RpcError::UnsupportedProtocol { .. })
                            ) => {}
                        Err(error) => return Err(error),
                    }
                }
                DaemonEndpoint::WebSocket { .. } => return Err(ContextError::NotManaged),
            }
            tokio::time::sleep(STOP_POLL_INTERVAL).await;
        }
    };
    match tokio::time::timeout(STOP_TIMEOUT, wait).await {
        Ok(result) => result,
        Err(_) => Err(ContextError::StopTimedOut {
            after: STOP_TIMEOUT,
        }),
    }
}

/// Pick the newest protocol whose wire shape this build can still emit.
///
/// Shutdown is deliberately the only RPC allowed to retry after negotiation
/// rejects the current version: its request and response are lifecycle
/// control messages, not application data. An incompatible major is never
/// guessed at.
fn shutdown_protocol(supported: &[ProtocolVersion]) -> Option<ProtocolVersion> {
    supported
        .iter()
        .copied()
        .filter(|candidate| ProtocolVersion::CURRENT.can_serve(*candidate))
        .max()
}

fn probe_identity() -> Identity {
    let (ts, r) = crate::ids::fresh_parts();
    Identity {
        client_id: nits_protocol::ClientId::from_parts(ts, r),
        client: BuildInfo {
            name: "nits-probe".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        author: Author::Daemon {
            machine: gethostname::gethostname().to_string_lossy().into_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn endpoint_resolution_makes_lifecycle_states_explicit() {
        let local = Context::Local {
            data_dir: Some(PathBuf::from("/tmp/nits-data")),
            socket: Some(PathBuf::from("/tmp/nits.sock")),
        };
        let endpoint = DaemonEndpoint::resolve(&local, StartPolicy::RequireRunning).unwrap();
        assert!(matches!(
            endpoint,
            DaemonEndpoint::Local {
                spec,
                start: StartPolicy::RequireRunning,
            } if spec.socket == Path::new("/tmp/nits.sock")
                && spec.data_dir == Path::new("/tmp/nits-data")
        ));

        let ws = Context::Ws {
            url: "ws://review.example:7677".into(),
        };
        let starts = [StartPolicy::StartIfNeeded, StartPolicy::RequireRunning]
            .map(|start| DaemonEndpoint::resolve(&ws, start).unwrap());
        assert_eq!(starts[0], starts[1], "WebSocket contexts are unmanaged");
        assert!(matches!(starts[0], DaemonEndpoint::WebSocket { .. }));
    }

    #[test]
    fn ssh_endpoint_contains_one_runnable_command_or_is_rejected() {
        let ssh = Context::Ssh {
            host: "review-box".into(),
            bin: nits_config::RemoteBin::Nits("/opt/nits".into()),
            args: vec!["--data-dir".into(), "/srv/nits".into()],
            ssh: Some("test-ssh".into()),
        };
        let endpoint = DaemonEndpoint::resolve(&ssh, StartPolicy::StartIfNeeded).unwrap();
        assert!(matches!(
            endpoint,
            DaemonEndpoint::Ssh {
                target: SshTarget {
                    host,
                    nits,
                    args,
                    ssh: Some(client),
                },
                start: StartPolicy::StartIfNeeded,
            } if host == "review-box"
                && nits == "/opt/nits"
                && args == ["--data-dir", "/srv/nits"]
                && client == "test-ssh"
        ));

        let legacy = Context::Ssh {
            host: "review-box".into(),
            bin: nits_config::RemoteBin::Legacy("/opt/nitsd".into()),
            args: Vec::new(),
            ssh: None,
        };
        assert!(matches!(
            DaemonEndpoint::resolve(&legacy, StartPolicy::StartIfNeeded),
            Err(ContextError::LegacyNitsd { .. })
        ));
    }

    #[test]
    fn shutdown_uses_the_newest_compatible_advertised_protocol() {
        let older = ProtocolVersion::new(
            ProtocolVersion::CURRENT.major,
            ProtocolVersion::CURRENT.minor.saturating_sub(1),
            0,
        );
        let oldest = ProtocolVersion::new(ProtocolVersion::CURRENT.major, 0, 0);
        let other_major = ProtocolVersion::new(ProtocolVersion::CURRENT.major + 1, 0, 0);
        assert_eq!(
            shutdown_protocol(&[oldest, other_major, older]),
            Some(older)
        );
        assert_eq!(shutdown_protocol(&[other_major]), None);
        assert_eq!(shutdown_protocol(&[]), None);
    }
    #[test]
    fn references_identify_daemon_endpoints_and_make_relative_sockets_absolute() {
        let endpoint = DaemonEndpoint::resolve(
            &Context::Local {
                data_dir: Some("state".into()),
                socket: None,
            },
            StartPolicy::RequireRunning,
        )
        .unwrap();
        let identity = endpoint.reference_context().unwrap();
        assert_eq!(
            identity.locator().unwrap(),
            nits_protocol::ReferenceLocator::Socket(
                std::path::absolute("state/nitsd.sock")
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            )
        );
        let endpoint = DaemonEndpoint::resolve(
            &Context::Ws {
                url: "wss://reviews.example/daemon".into(),
            },
            StartPolicy::RequireRunning,
        )
        .unwrap();
        assert_eq!(
            endpoint.reference_context().unwrap().locator().unwrap(),
            nits_protocol::ReferenceLocator::WebSocket("wss://reviews.example/daemon".into())
        );
    }
}
