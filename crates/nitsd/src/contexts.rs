//! Reaching a daemon through a [`Context`]: connect (starting the daemon on
//! demand where the context allows), probe status, start, stop. The CLI,
//! the MCP server and the desktop app all go through here so a machine's
//! daemon is managed one way.

use std::future::Future;
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
use crate::ownership::Phase;
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
    #[error("daemon retains store ownership while starting or stopping ({phase:?})")]
    Transitioning { phase: Phase },
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
        if let ClientError::Codec(crate::codec::CodecError::Io(error)) = &e
            && let Some(exit) = error
                .get_ref()
                .and_then(|error| error.downcast_ref::<ProxyExit>())
        {
            return match exit.outcome {
                crate::serve::StdioOutcome::NotRunning => Self::NotRunning,
                crate::serve::StdioOutcome::Transitioning { phase } => {
                    Self::Transitioning { phase }
                }
                crate::serve::StdioOutcome::Proxied => Self::Client(Box::new(e)),
            };
        }
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
    Transitioning {
        phase: Phase,
    },
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
struct SshStream {
    stdout: ChildStdout,
    stdin: ChildStdin,
    child_done: CancellationToken,
    exit: SshExit,
}

enum SshExit {
    Waiting(Pin<Box<dyn Future<Output = std::io::Result<std::process::ExitStatus>> + Send>>),
    Finished(std::process::ExitStatus),
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
#[error("remote daemon proxy exited with {outcome:?}")]
struct ProxyExit {
    outcome: crate::serve::StdioOutcome,
}

impl SshStream {
    fn poll_exit(&mut self, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        if let SshExit::Waiting(wait) = &mut self.exit {
            match wait.as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(status)) => self.exit = SshExit::Finished(status),
                Poll::Ready(Err(error)) => self.exit = SshExit::Failed(error.to_string()),
            }
        }
        Poll::Ready(match &self.exit {
            SshExit::Finished(status) if status.success() => Ok(()),
            SshExit::Finished(status) => match status
                .code()
                .and_then(crate::serve::StdioOutcome::from_exit_code)
            {
                Some(outcome) => Err(std::io::Error::other(ProxyExit { outcome })),
                None => Err(std::io::Error::other(format!(
                    "SSH process exited with {status}"
                ))),
            },
            SshExit::Failed(reason) => Err(std::io::Error::other(reason.clone())),
            SshExit::Waiting(_) => return Poll::Pending,
        })
    }
}

impl AsyncRead for SshStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let before = buf.filled().len();
        match Pin::new(&mut self.stdout).poll_read(cx, buf) {
            Poll::Ready(Ok(())) if buf.filled().len() == before => self.poll_exit(cx),
            result => result,
        }
    }
}

impl AsyncWrite for SshStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        match Pin::new(&mut self.stdin).poll_write(cx, buf) {
            Poll::Ready(Err(error)) if error.kind() == std::io::ErrorKind::BrokenPipe => {
                self.poll_exit(cx).map(|result| result.and(Err(error)))
            }
            result => result,
        }
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        match Pin::new(&mut self.stdin).poll_flush(cx) {
            Poll::Ready(Err(error)) if error.kind() == std::io::ErrorKind::BrokenPipe => {
                self.poll_exit(cx).map(|result| result.and(Err(error)))
            }
            result => result,
        }
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
    let (exit, exited) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        tokio::select! {
            result = child.wait() => {
                let _ = exit.send(result);
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
        exit: SshExit::Waiting(Box::pin(async move {
            // Starts only when EOF/BrokenPipe polls this future, not when SSH
            // is spawned. A broken SSH process cannot hide EOF indefinitely.
            tokio::time::timeout(Duration::from_secs(2), exited)
                .await
                .map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "SSH process did not exit after EOF",
                    )
                })?
                .map_err(|_| std::io::Error::other("SSH process wait was cancelled"))?
        })),
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
                    match launch::availability(spec)
                        .await
                        .map_err(io("probing daemon ownership"))?
                    {
                        launch::Availability::Listening => {}
                        launch::Availability::Stopped => return Err(ContextError::NotRunning),
                        launch::Availability::Transitioning { phase } => {
                            return Err(ContextError::Transitioning { phase });
                        }
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
    let (read, write) = dial(endpoint).await?.into_parts();
    Client::handshake_framed(read, write, identity, protocol)
        .await
        .map_err(Into::into)
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
            match launch::availability(&spec).await {
                Ok(launch::Availability::Listening) => {}
                Ok(launch::Availability::Stopped) => return Status::Stopped,
                Ok(launch::Availability::Transitioning { phase }) => {
                    return Status::Transitioning { phase };
                }
                Err(error) => {
                    return Status::Unreachable {
                        reason: error.to_string(),
                    };
                }
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
                Err(ContextError::Transitioning { phase }) => Status::Transitioning { phase },
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
        Err(ContextError::Transitioning { .. }) => {
            wait_until_stopped(&endpoint).await?;
            return Ok(true);
        }
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

/// Wait until a managed endpoint and every background store owner have exited.
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
                    if launch::availability(spec)
                        .await
                        .map_err(io("probing daemon ownership"))?
                        == launch::Availability::Stopped
                    {
                        return Ok(());
                    }
                }
                DaemonEndpoint::Ssh { .. } => {
                    match connect_endpoint(endpoint, probe_identity(), ProtocolVersion::CURRENT)
                        .await
                    {
                        Err(ContextError::NotRunning) => return Ok(()),
                        Err(ContextError::Transitioning { .. }) => {}
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
    #[cfg(unix)]
    fn scripted_ssh(dir: &Path, script: &str) -> Context {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("ssh");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Context::Ssh {
            host: "test-host".into(),
            bin: nits_config::RemoteBin::Default,
            args: Vec::new(),
            ssh: Some(path.to_str().unwrap().into()),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ssh_exit_outcomes_distinguish_ownership_from_absence_and_transport_failure() {
        let dir = tempfile::tempdir().unwrap();
        for (code, expected) in [
            (3, Status::Stopped),
            (
                4,
                Status::Transitioning {
                    phase: Phase::Starting,
                },
            ),
            (
                5,
                Status::Transitioning {
                    phase: Phase::Serving,
                },
            ),
            (
                6,
                Status::Transitioning {
                    phase: Phase::Stopping,
                },
            ),
        ] {
            let context = scripted_ssh(dir.path(), &format!("exit {code}"));
            assert_eq!(status(&context).await, expected, "exit {code}");
        }
        let context = scripted_ssh(dir.path(), "exit 7");
        assert_eq!(
            status(&context).await,
            Status::Transitioning {
                phase: Phase::Unknown
            }
        );
        // A clean proxy EOF can follow a successful connection just before
        // shutdown; only exit 3 proves the remote store is no longer owned.
        for code in [0, 1, 2, 8, 255] {
            let context = scripted_ssh(dir.path(), &format!("exit {code}"));
            assert!(
                matches!(status(&context).await, Status::Unreachable { .. }),
                "exit {code}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ssh_eof_without_process_exit_is_bounded_and_not_stopped() {
        let dir = tempfile::tempdir().unwrap();
        // exec keeps the sleeper as the owned child, so stream cancellation
        // kills/reaps it without leaving a shell's grandchild behind.
        let context = scripted_ssh(dir.path(), "exec 1>&-\nexec sleep 60");
        let status = tokio::time::timeout(Duration::from_secs(5), status(&context))
            .await
            .unwrap();
        assert!(
            matches!(status, Status::Unreachable { reason } if reason.contains("did not exit after EOF"))
        );
    }
}
