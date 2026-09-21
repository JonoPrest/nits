//! Accept loops. Each accepted stream is served by `connection::serve` in
//! its own task.

use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use std::net::SocketAddr;

use tokio::net::{TcpListener, UnixListener};
use tokio_util::sync::CancellationToken;

use crate::connection;
use crate::daemon::Daemon;
use crate::transport;

/// A bound unix socket. Removes the socket file on drop.
#[derive(Debug)]
pub struct UnixServer {
    listener: UnixListener,
    path: PathBuf,
}

impl UnixServer {
    /// Bind `path`, replacing a stale socket file left by a crashed daemon.
    /// A live daemon on the same path is detected by connecting first.
    pub fn bind(path: &Path) -> std::io::Result<Self> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => {
                if !metadata.file_type().is_socket() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!("refusing to replace a non-socket path: {}", path.display()),
                    ));
                }
                if std::os::unix::net::UnixStream::connect(path).is_ok() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AddrInUse,
                        format!("another nitsd is listening on {}", path.display()),
                    ));
                }
                std::fs::remove_file(path)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self {
            listener: UnixListener::bind(path)?,
            path: path.to_path_buf(),
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Accept until `shutdown` fires. In-flight connections are dropped when
    /// the returned future completes.
    pub async fn run(self, daemon: Arc<Daemon>, shutdown: CancellationToken) {
        let mut tasks = tokio::task::JoinSet::<()>::new();
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = tasks.join_next(), if !tasks.is_empty() => {},
                accepted = self.listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        let d = Arc::clone(&daemon);
                        tasks.spawn(async move {
                            if let Err(e) = connection::serve(d, stream).await {
                                tracing::debug!(error = %e, "connection ended");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "accept failed");
                    }
                },
            }
        }
        // Wait until connection futures (and their owned work/I/O) are dropped.
        tasks.shutdown().await;
    }
}

impl Drop for UnixServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A bound WebSocket (plain TCP) listener. Same protocol as the unix
/// socket, one envelope per binary message; intended for browser clients
/// and remote daemons.
#[derive(Debug)]
pub struct WsServer {
    listener: TcpListener,
    addr: SocketAddr,
}

impl WsServer {
    /// Bind `addr`; pass port 0 to let the OS pick (see [`Self::addr`]).
    pub async fn bind(addr: SocketAddr) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        Ok(Self { listener, addr })
    }

    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Accept until `shutdown` fires; see [`UnixServer::run`].
    pub async fn run(self, daemon: Arc<Daemon>, shutdown: CancellationToken) {
        let mut tasks = tokio::task::JoinSet::<()>::new();
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = tasks.join_next(), if !tasks.is_empty() => {},
                accepted = self.listener.accept() => match accepted {
                    Ok((stream, peer)) => {
                        let d = Arc::clone(&daemon);
                        tasks.spawn(async move {
                            let ws = match tokio_tungstenite::accept_async(stream).await {
                                Ok(ws) => ws,
                                Err(e) => {
                                    tracing::debug!(%peer, error = %e, "websocket upgrade failed");
                                    return;
                                }
                            };
                            let (rd, wr) = transport::web_socket(ws);
                            if let Err(e) = connection::serve_framed(d, rd, wr).await {
                                tracing::debug!(%peer, error = %e, "connection ended");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "accept failed");
                    }
                },
            }
        }
        // Wait until connection futures (and their owned work/I/O) are dropped.
        tasks.shutdown().await;
    }
}

/// Serve exactly one client over stdin/stdout (`ssh host nitsd --stdio`).
pub async fn serve_stdio(daemon: Arc<Daemon>) -> Result<(), connection::ConnectionError> {
    let stream = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
    connection::serve(daemon, stream).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn binding_preserves_regular_files_and_the_stable_ownership_inode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("important-file");
        std::fs::write(&path, "keep this file").unwrap();
        assert_eq!(
            UnixServer::bind(&path).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep this file");
        let owner = crate::ownership::Lease::acquire(dir.path()).unwrap();
        owner.set_phase(crate::ownership::Phase::Stopping).unwrap();
        assert!(UnixServer::bind(&dir.path().join("daemon.lock")).is_err());
        assert_eq!(
            crate::ownership::probe(dir.path()).unwrap(),
            crate::ownership::Ownership::Held {
                phase: crate::ownership::Phase::Stopping,
            }
        );
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink("important-file", &alias).unwrap();
        assert!(UnixServer::bind(&alias).is_err());
        assert_eq!(
            std::fs::read_link(alias).unwrap(),
            Path::new("important-file")
        );
    }

    #[tokio::test]
    async fn binding_still_reclaims_stale_sockets_and_refuses_live_ones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        let live = UnixServer::bind(&path).unwrap();
        assert_eq!(
            UnixServer::bind(&path).unwrap_err().kind(),
            std::io::ErrorKind::AddrInUse
        );
        drop(live);
        assert!(!path.exists());
    }
}
