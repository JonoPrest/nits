//! `Core` shared by every connection.
//!
//! Reads run on tokio's blocking pool straight against the shared `Core`
//! (redb read transactions and git object reads are concurrent-safe).
//! Mutations go through one writer thread so appends — and the re-anchoring
//! `resolve_targets` triggers — are strictly serialised without a lock that
//! readers would contend on. Every committed event is broadcast to the
//! connections, which filter by their subscriptions.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nits_protocol::{
    BuildInfo, Event, EventBody, ReviewId, RpcError, SchemaVersion, SubscribeScope, TreeDelta,
    WorkspaceId,
};
use nits_review_core::{Core, CoreError, Ctx, DataDir};
use tokio::sync::{broadcast, oneshot};

/// Capacity of the event broadcast. A subscriber further behind than this is
/// told to resubscribe from its last `Seq` (see `connection`).
pub const EVENT_BACKLOG: usize = 4096;

type WriteJob = Box<dyn FnOnce(&Core) + Send>;

/// Field order matters: close both stores before releasing process ownership.
/// Every writer/blocking read retains this same owner even if its caller exits.
#[derive(Debug)]
struct OwnedCore {
    core: Core,
    lease: crate::ownership::Lease,
    // Retained with Core even after the listener and socket file disappear.
    _socket_lease: Option<crate::ownership::Lease>,
    // Last: waiters observe closure only after Core and both guards drop.
    released: tokio::sync::watch::Sender<()>,
}

impl std::ops::Deref for OwnedCore {
    type Target = Core;

    fn deref(&self) -> &Self::Target {
        &self.core
    }
}

pub struct Daemon {
    admission: Arc<crate::admission::Admission>,
    core: Arc<OwnedCore>,
    writer: std::sync::mpsc::Sender<WriteJob>,
    events: broadcast::Sender<Arc<Event>>,
    deltas: broadcast::Sender<Arc<TreeDelta>>,
    review_workspaces: Arc<Mutex<HashMap<ReviewId, WorkspaceId>>>,
    /// Cancelled by `Request::Shutdown`, ctrl-c, the idle timer, or writer exit.
    shutdown: tokio_util::sync::CancellationToken,
    /// Open connections, for the idle timer.
    connections: std::sync::atomic::AtomicUsize,
    pub build: BuildInfo,
}

impl std::fmt::Debug for Daemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Daemon")
            .field("build", &self.build)
            .finish_non_exhaustive()
    }
}

/// Decrements the connection count on drop.
#[derive(Debug)]
pub struct ConnectionGuard {
    daemon: Arc<Daemon>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.daemon
            .connections
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error(transparent)]
    Core(#[from] CoreError),
    /// The writer thread or a blocking task went away.
    #[error("daemon is shutting down")]
    Shutdown,
    #[error("daemon is restarting (operation {0}); request was not admitted")]
    Restarting(nits_protocol::UpgradeId),
}

impl From<DaemonError> for RpcError {
    fn from(e: DaemonError) -> Self {
        match e {
            DaemonError::Restarting(operation_id) => RpcError::Restarting { operation_id },
            DaemonError::Core(c) => c.into_rpc(),
            DaemonError::Shutdown => RpcError::Internal {
                message: e.to_string(),
            },
        }
    }
}

/// `CoreError` → wire error. Internal failures keep their message; the
/// daemon is a local process, so this leaks nothing the user can't read.
trait IntoRpc {
    fn into_rpc(self) -> RpcError;
}

impl IntoRpc for CoreError {
    fn into_rpc(self) -> RpcError {
        match self {
            CoreError::NotFound { kind, id } => RpcError::NotFound { kind, id },
            CoreError::Invalid { reason } => RpcError::Invalid { reason },
            CoreError::Forbidden { reason } => RpcError::Forbidden { reason },
            CoreError::Store(_) | CoreError::Git(_) | CoreError::Cache(_) | CoreError::Io(_) => {
                RpcError::Internal {
                    message: self.to_string(),
                }
            }
        }
    }
}

impl Daemon {
    /// Open the data dir and start the writer thread.
    pub fn open(data_dir: &DataDir, build: BuildInfo) -> Result<Arc<Self>, DaemonError> {
        Self::open_owned(data_dir, build, None)
    }

    pub(crate) fn open_at_socket(
        data_dir: &DataDir,
        build: BuildInfo,
        socket: &std::path::Path,
    ) -> Result<Arc<Self>, DaemonError> {
        let socket_lease = crate::ownership::Lease::for_socket(socket).map_err(CoreError::Io)?;
        Self::open_owned(data_dir, build, Some(socket_lease))
    }

    fn open_owned(
        data_dir: &DataDir,
        build: BuildInfo,
        socket_lease: Option<crate::ownership::Lease>,
    ) -> Result<Arc<Self>, DaemonError> {
        let lease = crate::ownership::Lease::acquire(&data_dir.root).map_err(CoreError::Io)?;
        let core = Core::open(data_dir)?;
        lease
            .set_phase(crate::ownership::Phase::Starting)
            .map_err(CoreError::Io)?;
        let core = Arc::new(OwnedCore {
            core,
            lease,
            _socket_lease: socket_lease,
            released: tokio::sync::watch::channel(()).0,
        });
        let (writer, jobs) = std::sync::mpsc::channel::<WriteJob>();
        let writer_core = Arc::clone(&core);
        let shutdown = tokio_util::sync::CancellationToken::new();
        let shutdown_on_writer_exit = shutdown.clone().drop_guard();
        std::thread::Builder::new()
            .name("nitsd-writer".into())
            .spawn(move || {
                // An unexpected unwind may leave a mutation partly committed.
                // Stop the daemon instead of serving reads as if it were healthy
                // or trying further writes against potentially inconsistent state.
                let _shutdown_on_writer_exit = shutdown_on_writer_exit;
                for job in jobs {
                    job(&writer_core);
                }
            })
            .map_err(CoreError::Io)?;
        let (events, _) = broadcast::channel(EVENT_BACKLOG);
        let (deltas, _) = broadcast::channel(EVENT_BACKLOG);
        Ok(Arc::new(Self {
            admission: crate::admission::Admission::new(),
            core,
            writer,
            events,
            deltas,
            review_workspaces: Arc::new(Mutex::new(HashMap::new())),
            shutdown,
            connections: std::sync::atomic::AtomicUsize::new(0),
            build,
        }))
    }

    /// Independent of review subscription filters; only current-protocol peers
    /// receive these messages. Legacy peers use the documented EOF fallback.
    pub fn lifecycle(
        &self,
    ) -> tokio::sync::watch::Receiver<Option<nits_protocol::UpgradeOperation>> {
        self.admission.subscribe()
    }

    /// Close admission before announcing the operation. The owned draining task
    /// keeps accepted Core jobs alive even if the requester disappears.
    pub fn prepare_restart(
        self: &Arc<Self>,
        operation: nits_protocol::UpgradeOperation,
    ) -> nits_protocol::UpgradeOperation {
        let (operation, first) = self.admission.close(operation);
        if !first {
            return operation;
        }
        let daemon = Arc::clone(self);
        tokio::spawn(async move {
            tokio::select! {
                () = daemon.shutdown.cancelled() => return,
                () = daemon.admission.drained() => {},
            }
            // Connections drain their accepted responses independently. Bound
            // the final output phase so an unread socket cannot retain shutdown.
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(4);
            while daemon.connections() > 0 && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            daemon.shutdown.cancel();
        });
        operation
    }

    /// Token every accept loop and background task watches.
    #[must_use]
    pub fn shutdown(&self) -> &tokio_util::sync::CancellationToken {
        &self.shutdown
    }

    /// Closed after every Core user, its stores and ownership guards are gone.
    /// In particular the dedicated writer is outside Tokio's blocking pool, so
    /// runtime shutdown alone does not wait for its started work to finish.
    pub(crate) fn core_released(&self) -> tokio::sync::watch::Receiver<()> {
        self.core.released.subscribe()
    }

    pub(crate) fn set_phase(&self, phase: crate::ownership::Phase) {
        if let Err(error) = self.core.lease.set_phase(phase) {
            tracing::warn!(%error, "recording daemon lifecycle phase");
        }
    }

    /// Currently open client connections.
    #[must_use]
    pub fn connections(&self) -> usize {
        self.connections.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Count a connection for its lifetime.
    #[must_use]
    pub fn track_connection(self: &Arc<Self>) -> ConnectionGuard {
        self.connections
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        ConnectionGuard {
            daemon: Arc::clone(self),
        }
    }

    /// Direct access to the core, for tests and diagnostics. Mutations must
    /// still go through [`Self::write`].
    #[must_use]
    pub fn core(&self) -> &Core {
        &self.core
    }

    pub fn schema(&self) -> SchemaVersion {
        SchemaVersion::CURRENT
    }

    /// Run a read on the blocking pool.
    pub async fn read<T, F>(&self, f: F) -> Result<T, DaemonError>
    where
        T: Send + 'static,
        F: FnOnce(&Core) -> Result<T, CoreError> + Send + 'static,
    {
        let permit = self.admission.accept()?;
        let core = Arc::clone(&self.core);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            f(&core)
        })
        .await
        .map_err(|_| DaemonError::Shutdown)?
        .map_err(DaemonError::Core)
    }

    /// Run a mutation on the writer thread. Returns the events it appended,
    /// in order, after broadcasting them.
    pub async fn write<T, F>(&self, f: F) -> Result<(T, Vec<Event>), DaemonError>
    where
        T: Send + 'static,
        F: FnOnce(&Core) -> Result<T, CoreError> + Send + 'static,
    {
        let permit = self.admission.accept()?;
        let (tx, rx) = oneshot::channel();
        let broadcast = self.events.clone();
        let review_workspaces = Arc::clone(&self.review_workspaces);
        let job: WriteJob = Box::new(move |core| {
            let _permit = permit;
            let result = (|| {
                let before = core.last_seq()?;
                let out = f(core);
                let events = core.events_after(before)?;
                // Publish on the writer thread, in commit order, even if the
                // requesting task was cancelled before receiving its result.
                for event in &events {
                    // Keep scope information even if the review is deleted
                    // before a subscriber first asks for its workspace.
                    if let EventBody::ReviewCreated { review } = &event.body {
                        review_workspaces
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .insert(review.id, review.workspace_id);
                    }
                    // No subscribers is not an error.
                    let _ = broadcast.send(Arc::new(event.clone()));
                }
                Ok::<_, CoreError>((out?, events))
            })();
            let _ = tx.send(result);
        });
        self.writer.send(job).map_err(|_| DaemonError::Shutdown)?;
        Ok(rx.await.map_err(|_| DaemonError::Shutdown)??)
    }

    /// Subscribe to the live event tail.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Event>> {
        self.events.subscribe()
    }

    /// A cancellable log-head wait followed by one bounded Core page. Register
    /// before checking the head: a write in between is either in the page or
    /// wakes this receiver. Broadcast lag only requires another head check;
    /// history itself is always read from the store, never from this queue.
    pub async fn replay_events(
        &self,
        scope: SubscribeScope,
        position: nits_protocol::ReplayPosition,
    ) -> Result<nits_protocol::ReplayPage, DaemonError> {
        if let nits_protocol::ReplayPosition::Follow { after } = position {
            let mut wake = self.subscribe();
            loop {
                let head = self
                    .read(Core::last_seq)
                    .await?
                    .unwrap_or(nits_protocol::Seq::new(0));
                if head > after {
                    break;
                }
                tokio::select! {
                    () = self.shutdown.cancelled() => return Err(DaemonError::Shutdown),
                    event = wake.recv() => match event {
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {},
                        Err(broadcast::error::RecvError::Closed) => return Err(DaemonError::Shutdown),
                    }
                }
            }
        }
        self.read(move |core| core.replay_events(&scope, position))
            .await
    }

    /// Subscribe to working-tree deltas.
    #[must_use]
    pub fn subscribe_deltas(&self) -> broadcast::Receiver<Arc<TreeDelta>> {
        self.deltas.subscribe()
    }

    /// Publish a working-tree delta (from the file watcher).
    pub fn broadcast_delta(&self, delta: TreeDelta) {
        let _ = self.deltas.send(Arc::new(delta));
    }

    /// Does a working-tree delta for `delta.repo_id` concern `scope`?
    /// `All` and the repo's workspace always; a review only if it targets
    /// that repo's working tree and is open.
    #[must_use]
    pub fn delta_matches(&self, scope: &SubscribeScope, delta: &TreeDelta) -> bool {
        match scope {
            SubscribeScope::All => true,
            SubscribeScope::Workspace { workspace_id } => self
                .core
                .workspace(*workspace_id)
                .is_ok_and(|w| w.repos.iter().any(|r| r.id == delta.repo_id)),
            SubscribeScope::Review { review_id } => self
                .core
                .working_tree_reviews(delta.repo_id)
                .is_ok_and(|rs| rs.contains(review_id)),
            SubscribeScope::AwaitingAgent { .. } => false,
        }
    }

    /// A `Ctx` for a connection's author at the current wall-clock time.
    #[must_use]
    pub fn ctx(
        author: nits_protocol::Author,
        client_id: nits_protocol::ClientId,
        client_seq: nits_protocol::ClientSeq,
    ) -> Ctx {
        Ctx {
            author,
            client_id,
            client_seq,
            now: now(),
        }
    }

    /// Workspace of a review, cached; falls back to persisted records including
    /// tombstones so replay still matches reviews deleted before this process.
    fn workspace_of(&self, review: ReviewId) -> Option<WorkspaceId> {
        if let Some(w) = self
            .review_workspaces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&review)
        {
            return Some(*w);
        }
        let w = self.core.stored_review_workspace(review).ok()?;
        self.review_workspaces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(review, w);
        Some(w)
    }

    /// Does `event` fall inside `scope`?
    #[must_use]
    pub fn matches(&self, scope: &SubscribeScope, event: &Event) -> bool {
        scope.matches(event, |review| self.workspace_of(review))
    }
}

/// Milliseconds since the epoch.
#[must_use]
pub fn now() -> nits_protocol::Timestamp {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    nits_protocol::Timestamp::from_millis(ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use nits_protocol::{
        Anchor, Author, ClientId, ClientSeq, CommentId, CommentKind, NonEmpty, ProtocolVersion,
        RefSpec, RepoId, Request, ReviewTarget, Seq, Since,
    };
    use nits_test_support::{RepoBuilder, TestRepo, files};

    use crate::client::{Client, ClientError, Identity};
    use crate::ops::Ops;

    fn identity() -> Identity {
        Identity {
            client_id: ClientId::from_parts(1, 1),
            client: BuildInfo {
                name: "test".into(),
                version: "0".into(),
            },
            author: Author::Human {
                name: "ada".into(),
                machine: "box".into(),
            },
        }
    }

    #[tokio::test]
    async fn unexpected_writer_panic_stops_listeners_and_status_cannot_report_running() {
        use crate::contexts::{Status, status};
        use crate::server::{UnixServer, WsServer};
        use nits_config::Context;

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("nitsd.sock");
        // Keep the selected endpoint associated with Core exactly as serve does.
        // A bare listener with no guard deliberately has legacy status semantics.
        let daemon =
            Daemon::open_at_socket(&DataDir::new(dir.path()), identity().client, &socket).unwrap();
        let unix = UnixServer::bind(&socket).unwrap();
        crate::ownership::associate_socket(&socket, dir.path()).unwrap();
        daemon.set_phase(crate::ownership::Phase::Serving);
        let ws = WsServer::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let contexts = [
            Context::Local {
                data_dir: Some(dir.path().to_path_buf()),
                socket: Some(socket.clone()),
            },
            Context::Ws {
                url: format!("ws://{}", ws.addr()),
            },
        ];
        // Use the same cancellation token wiring as the daemon's serve entrypoint.
        let unix = tokio::spawn(unix.run(Arc::clone(&daemon), daemon.shutdown().clone()));
        let ws = tokio::spawn(ws.run(Arc::clone(&daemon), daemon.shutdown().clone()));
        for context in &contexts {
            assert!(matches!(status(context).await, Status::Running { .. }));
        }

        let result = daemon
            .write(|_| -> Result<(), CoreError> { panic!("injected unexpected writer failure") })
            .await;
        assert!(matches!(result, Err(DaemonError::Shutdown)));
        tokio::time::timeout(Duration::from_secs(5), async {
            unix.await.unwrap();
            ws.await.unwrap();
        })
        .await
        .expect("writer failure must stop both daemon listeners");
        assert!(daemon.shutdown().is_cancelled());
        assert!(!socket.exists());
        assert!(matches!(
            status(&contexts[0]).await,
            Status::Transitioning { .. }
        ));
        assert!(matches!(status(&contexts[1]).await, Status::Stopped));
        assert!(matches!(
            daemon
                .write(|core| core.create_workspace(
                    &Daemon::ctx(identity().author, identity().client_id, ClientSeq::new(1)),
                    WorkspaceId::from_parts(1, 1),
                    "cannot commit after writer failure".into(),
                ))
                .await,
            Err(DaemonError::Shutdown)
        ));
        assert!(daemon.core().events_after(None).unwrap().is_empty());
        drop(daemon);
        for context in &contexts {
            assert!(matches!(status(context).await, Status::Stopped));
        }
    }

    async fn deleted_review_gap(daemon: &Daemon, repo: &TestRepo) -> (Seq, Vec<Event>) {
        let who = identity();
        let ctx = Daemon::ctx(who.author, who.client_id, ClientSeq::new(1));
        let path = repo.path().to_str().unwrap().to_owned();
        daemon
            .write(move |core| {
                let workspace = WorkspaceId::from_parts(1, 1);
                let repository = RepoId::from_parts(1, 1);
                core.create_workspace(&ctx, workspace, "review workspace".into())?;
                core.create_workspace(
                    &ctx,
                    WorkspaceId::from_parts(1, 2),
                    "other workspace".into(),
                )?;
                core.attach_repo(&ctx, workspace, repository, &path, "repo".into())?;
                core.create_review(
                    &ctx,
                    ReviewId::from_parts(1, 1),
                    workspace,
                    "review".into(),
                    NonEmpty::singleton(ReviewTarget {
                        repo_id: repository,
                        base: RefSpec::Head,
                        head: RefSpec::Head,
                    }),
                )
            })
            .await
            .unwrap();
        // Reconnect after creation: ReviewCreated cannot warm the new cache.
        let cursor = daemon.core().last_seq().unwrap().unwrap();
        let who = identity();
        let ctx = Daemon::ctx(who.author, who.client_id, ClientSeq::new(2));
        let ((), gap) = daemon
            .write(move |core| {
                let review = ReviewId::from_parts(1, 1);
                core.add_comment(
                    &ctx,
                    review,
                    CommentId::from_parts(1, 1),
                    CommentKind::Note,
                    Anchor::Review,
                    "persisted comment".into(),
                    None,
                )?;
                core.delete_review(&ctx, review)?;
                core.rename_workspace(&ctx, WorkspaceId::from_parts(1, 2), "unrelated".into())
            })
            .await
            .unwrap();
        assert_eq!(gap.len(), 3);
        assert!(matches!(gap[0].body, EventBody::CommentCreated { .. }));
        assert!(matches!(gap[1].body, EventBody::ReviewDeleted { .. }));
        assert!(matches!(gap[2].body, EventBody::WorkspaceUpdated { .. }));
        (cursor, gap)
    }

    #[tokio::test]
    async fn workspace_replay_after_restart_includes_deleted_reviews() {
        let dir = tempfile::tempdir().unwrap();
        let data = DataDir::new(dir.path());
        let repo = RepoBuilder::new()
            .commit("initial", files!["file.txt" => "text\n"])
            .build()
            .unwrap();
        let daemon = Daemon::open(&data, identity().client).unwrap();
        let (cursor, gap) = deleted_review_gap(&daemon, &repo).await;
        // The writer owns another Core reference. Wait for its release rather
        // than racing the database lock with the restarted daemon.
        let Daemon {
            mut core, writer, ..
        } = Arc::try_unwrap(daemon).unwrap();
        drop(writer);
        let old_core = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match Arc::try_unwrap(core) {
                    Ok(core) => break core,
                    Err(shared) => core = shared,
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(old_core);

        let daemon = Daemon::open(&data, identity().client).unwrap();
        let (client_io, server_io) = tokio::io::duplex(65536);
        let server = tokio::spawn(crate::connection::serve(Arc::clone(&daemon), server_io));
        let client = Client::handshake(client_io, identity(), ProtocolVersion::CURRENT)
            .await
            .unwrap();
        let review = ReviewId::from_parts(1, 1);
        assert!(matches!(
            client
                .request(Request::GetReview { review_id: review })
                .await,
            Err(ClientError::Rpc(RpcError::NotFound { .. }))
        ));
        let ops = Ops::new(client);
        // Ask for workspace replay first, with an empty process-local cache.
        for (scope, expected) in [
            (
                SubscribeScope::Workspace {
                    workspace_id: WorkspaceId::from_parts(1, 1),
                },
                &gap[..2],
            ),
            (SubscribeScope::Review { review_id: review }, &gap[..2]),
            (
                SubscribeScope::Workspace {
                    workspace_id: WorkspaceId::from_parts(1, 2),
                },
                &gap[2..],
            ),
            (SubscribeScope::All, gap.as_slice()),
        ] {
            let polled = ops
                .poll_events(
                    scope,
                    Since::After { seq: cursor },
                    Duration::from_millis(50),
                    10,
                )
                .await
                .unwrap();
            assert_eq!(polled.events, expected);
            assert_eq!(polled.last_seq, expected.last().unwrap().seq);
        }
        drop(ops);
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
