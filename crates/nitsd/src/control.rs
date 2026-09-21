//! Stable, bounded machine-local maintenance control, separate from app Hello.
//!
//! Only version 1 is decoded. Application protocol retirement never prevents
//! inspecting an incumbent or requesting its explicitly supported drain path.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use nits_protocol::{
    BuildDescriptor, ControlVersion, ReleaseRelation, UpgradeOperation, UpgradeProgress,
    UpgradeStage,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::Daemon;
use crate::serve::ServeOpts;

const LIMIT: u32 = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunningDaemon {
    pub build: BuildDescriptor,
    pub runtime: ServeOpts,
    pub operation: Option<UpgradeOperation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Request {
    Inspect {},
    Prepare { operation: UpgradeOperation },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Response {
    Running { daemon: RunningDaemon },
    Prepared { operation: UpgradeOperation },
    Rejected { reason: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    version: ControlVersion,
    message: T,
}

pub fn path(socket: &Path) -> io::Result<PathBuf> {
    let mut path = crate::ownership::socket_target(socket)?.into_os_string();
    path.push(".control");
    Ok(PathBuf::from(path))
}

async fn read<R: AsyncRead + Unpin, T: serde::de::DeserializeOwned>(
    stream: &mut R,
) -> io::Result<T> {
    let length = stream.read_u32().await?;
    if length > LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "maintenance frame exceeds limit",
        ));
    }
    let mut bytes = vec![0; length as usize];
    stream.read_exact(&mut bytes).await?;
    let envelope: Envelope<T> = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if envelope.version != ControlVersion::CURRENT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported maintenance protocol",
        ));
    }
    Ok(envelope.message)
}

async fn write<W: AsyncWrite + Unpin, T: Serialize>(stream: &mut W, message: T) -> io::Result<()> {
    let bytes = serde_json::to_vec(&Envelope {
        version: ControlVersion::CURRENT,
        message,
    })
    .map_err(io::Error::other)?;
    let length = u32::try_from(bytes.len())
        .ok()
        .filter(|length| *length <= LIMIT)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "maintenance frame exceeds limit",
            )
        })?;
    stream.write_u32(length).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await
}

pub async fn request(socket: &Path, request: Request) -> io::Result<Response> {
    tokio::time::timeout(TIMEOUT, async {
        let mut stream = UnixStream::connect(path(socket)?).await?;
        write(&mut stream, request).await?;
        read(&mut stream).await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "maintenance request timed out"))?
}

pub async fn inspect(socket: &Path) -> io::Result<RunningDaemon> {
    match request(socket, Request::Inspect {}).await? {
        Response::Running { daemon } => Ok(daemon),
        Response::Rejected { reason } => Err(io::Error::other(reason)),
        Response::Prepared { .. } => Err(io::Error::other("unexpected maintenance response")),
    }
}

#[derive(Debug)]
pub struct Server {
    listener: crate::server::UnixServer,
    build: BuildDescriptor,
    runtime: ServeOpts,
}

impl Server {
    pub fn bind(runtime: ServeOpts) -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let path = path(&runtime.socket)?;
        let listener = crate::server::UnixServer::bind(&path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            listener,
            build: crate::build::running()?,
            runtime,
        })
    }

    pub async fn run(self, daemon: Arc<Daemon>) {
        let mut clients = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                () = daemon.shutdown().cancelled() => break,
                _ = clients.join_next(), if !clients.is_empty() => {},
                accepted = self.listener.accept() => match accepted {
                    Ok(mut stream) => {
                        let daemon = Arc::clone(&daemon);
                        let build = self.build.clone();
                        let runtime = self.runtime.clone();
                        clients.spawn(async move {
                            let _tracked = daemon.track_connection();
                            let _ = tokio::time::timeout(TIMEOUT, async {
                                let response = match read::<_, Request>(&mut stream).await? {
                                    Request::Inspect {} => Response::Running { daemon: RunningDaemon {
                                        build, runtime, operation: daemon.lifecycle().borrow().clone(),
                                    } },
                                    Request::Prepare { mut operation } => {
                                        if operation.source != build || operation.target.control != ControlVersion::CURRENT {
                                            Response::Rejected { reason: "running build changed or replacement control is incompatible".into() }
                                        } else if matches!(operation.target.release.relative_to(&build.release), ReleaseRelation::Older | ReleaseRelation::DifferentChannel) {
                                            Response::Rejected { reason: "replacement would downgrade or change the selected release channel".into() }
                                        } else {
                                            operation.progress = UpgradeProgress::Active { stage: UpgradeStage::Draining };
                                            Response::Prepared { operation: daemon.prepare_restart(operation) }
                                        }
                                    }
                                };
                                write(&mut stream, response).await
                            }).await;
                        });
                    }
                    Err(error) => { tracing::warn!(%error, "maintenance accept failed"); break; }
                }
            }
        }
        clients.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nits_protocol::{Author, BuildInfo, ClientId, ClientSeq, UpgradeId, WorkspaceId};
    use nits_review_core::DataDir;

    fn operation(source: BuildDescriptor) -> UpgradeOperation {
        let mut target = source.clone();
        target.digest = nits_protocol::BuildDigest::from_bytes([42; 32]);
        UpgradeOperation {
            id: UpgradeId::from_parts(1, 1),
            source,
            target,
            progress: UpgradeProgress::Active {
                stage: UpgradeStage::PreparingRestart,
            },
        }
    }

    fn daemon(path: &Path) -> Arc<Daemon> {
        Daemon::open(
            &DataDir::new(path),
            BuildInfo {
                name: "maintenance-test".into(),
                version: "0".into(),
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn maintenance_rejects_unknown_versions_and_oversized_frames_before_decoding() {
        for bytes in [(LIMIT + 1).to_be_bytes().to_vec(), {
            let payload = br#"{"version":2,"message":{"type":"Inspect"}}"#;
            let mut bytes = u32::try_from(payload.len()).unwrap().to_be_bytes().to_vec();
            bytes.extend(payload);
            bytes
        }] {
            let (mut sender, mut receiver) = tokio::io::duplex(1024);
            sender.write_all(&bytes).await.unwrap();
            assert!(read::<_, Request>(&mut receiver).await.is_err());
        }
    }

    #[tokio::test]
    async fn a_prepare_receipt_flushes_even_without_an_application_connection() {
        for _ in 0..8 {
            let dir = tempfile::tempdir().unwrap();
            let runtime = ServeOpts::new(dir.path().to_path_buf());
            let daemon = daemon(dir.path());
            let server = Server::bind(runtime.clone()).unwrap();
            let task = tokio::spawn(server.run(Arc::clone(&daemon)));
            let running = inspect(&runtime.socket).await.unwrap();
            let expected = operation(running.build);
            let response = request(
                &runtime.socket,
                Request::Prepare {
                    operation: expected.clone(),
                },
            )
            .await
            .unwrap();
            assert!(
                matches!(response, Response::Prepared { operation } if operation.id == expected.id)
            );
            tokio::time::timeout(Duration::from_secs(2), daemon.shutdown().cancelled())
                .await
                .unwrap();
            task.await.unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn accepted_mutation_survives_caller_loss_and_new_work_is_not_admitted() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = ServeOpts::new(dir.path().to_path_buf());
        let daemon = daemon(dir.path());
        let task = tokio::spawn(
            Server::bind(runtime.clone())
                .unwrap()
                .run(Arc::clone(&daemon)),
        );
        let (started, entered) = tokio::sync::oneshot::channel();
        let (release, held) = std::sync::mpsc::channel();
        let writer = Arc::clone(&daemon);
        let write = tokio::spawn(async move {
            writer
                .write(move |core| {
                    started.send(()).unwrap();
                    held.recv_timeout(Duration::from_secs(5)).unwrap();
                    core.create_workspace(
                        &Daemon::ctx(
                            Author::Human {
                                name: "ada".into(),
                                machine: "test".into(),
                            },
                            ClientId::from_parts(1, 1),
                            ClientSeq::new(1),
                        ),
                        WorkspaceId::from_parts(1, 1),
                        "accepted before restart".into(),
                    )
                })
                .await
        });
        entered.await.unwrap();
        let prepared = operation(inspect(&runtime.socket).await.unwrap().build);
        assert!(matches!(
            request(
                &runtime.socket,
                Request::Prepare {
                    operation: prepared.clone()
                }
            )
            .await
            .unwrap(),
            Response::Prepared { .. }
        ));
        write.abort();
        assert!(write.await.unwrap_err().is_cancelled());
        assert!(
            matches!(daemon.read(nits_review_core::Core::last_seq).await, Err(crate::daemon::DaemonError::Restarting(id)) if id == prepared.id)
        );
        assert!(!daemon.shutdown().is_cancelled());
        assert!(matches!(
            crate::ownership::probe(dir.path()).unwrap(),
            crate::ownership::Ownership::Held { .. }
        ));
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), daemon.shutdown().cancelled())
            .await
            .unwrap();
        task.await.unwrap();
        assert_eq!(
            daemon.core().workspaces().unwrap()[0].name,
            "accepted before restart"
        );
        let mut released = daemon.core_released();
        drop(daemon);
        // Closing this watch is the release signal; no value is published.
        assert!(
            tokio::time::timeout(Duration::from_secs(2), released.changed())
                .await
                .unwrap()
                .is_err()
        );
        let reopened = nits_review_core::Core::open(&DataDir::new(dir.path())).unwrap();
        assert_eq!(
            reopened.workspaces().unwrap()[0].name,
            "accepted before restart"
        );
    }

    #[tokio::test]
    async fn preflight_rejection_keeps_incumbent_serving() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = ServeOpts::new(dir.path().to_path_buf());
        let daemon = daemon(dir.path());
        let task = tokio::spawn(
            Server::bind(runtime.clone())
                .unwrap()
                .run(Arc::clone(&daemon)),
        );
        let source = inspect(&runtime.socket).await.unwrap().build;
        let mut wrong_source = operation(source.clone());
        wrong_source.source.digest = nits_protocol::BuildDigest::from_bytes([3; 32]);
        let mut wrong_channel = operation(source);
        wrong_channel.target.release.channel = "other-channel".parse().unwrap();
        for operation in [wrong_source, wrong_channel] {
            assert!(matches!(
                request(&runtime.socket, Request::Prepare { operation })
                    .await
                    .unwrap(),
                Response::Rejected { .. }
            ));
            assert!(daemon.lifecycle().borrow().is_none());
            assert!(daemon.read(nits_review_core::Core::last_seq).await.is_ok());
        }
        daemon.shutdown().cancel();
        task.await.unwrap();
    }
}
