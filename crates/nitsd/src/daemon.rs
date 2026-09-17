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

pub struct Daemon {
    core: Arc<Core>,
    writer: std::sync::mpsc::Sender<WriteJob>,
    events: broadcast::Sender<Arc<Event>>,
    deltas: broadcast::Sender<Arc<TreeDelta>>,
    review_workspaces: Arc<Mutex<HashMap<ReviewId, WorkspaceId>>>,
    /// Cancelled by `Request::Shutdown`, ctrl-c, or the idle timer.
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
    /// The writer thread or a blocking task went away; only on shutdown.
    #[error("daemon is shutting down")]
    Shutdown,
}

impl From<DaemonError> for RpcError {
    fn from(e: DaemonError) -> Self {
        match e {
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
        let core = Arc::new(Core::open(data_dir)?);
        let (writer, jobs) = std::sync::mpsc::channel::<WriteJob>();
        let writer_core = Arc::clone(&core);
        std::thread::Builder::new()
            .name("nitsd-writer".into())
            .spawn(move || {
                for job in jobs {
                    job(&writer_core);
                }
            })
            .map_err(CoreError::Io)?;
        let (events, _) = broadcast::channel(EVENT_BACKLOG);
        let (deltas, _) = broadcast::channel(EVENT_BACKLOG);
        Ok(Arc::new(Self {
            core,
            writer,
            events,
            deltas,
            review_workspaces: Arc::new(Mutex::new(HashMap::new())),
            shutdown: tokio_util::sync::CancellationToken::new(),
            connections: std::sync::atomic::AtomicUsize::new(0),
            build,
        }))
    }

    /// Token every accept loop and background task watches.
    #[must_use]
    pub fn shutdown(&self) -> &tokio_util::sync::CancellationToken {
        &self.shutdown
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
        let core = Arc::clone(&self.core);
        tokio::task::spawn_blocking(move || f(&core))
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
        let (tx, rx) = oneshot::channel();
        let broadcast = self.events.clone();
        let review_workspaces = Arc::clone(&self.review_workspaces);
        let job: WriteJob = Box::new(move |core| {
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
        match scope {
            SubscribeScope::All => true,
            SubscribeScope::Workspace { workspace_id } => {
                event_workspace(&event.body)
                    .or_else(|| event_review(&event.body).and_then(|r| self.workspace_of(r)))
                    == Some(*workspace_id)
            }
            SubscribeScope::Review { review_id } => event_review(&event.body) == Some(*review_id),
            SubscribeScope::AwaitingAgent { agent } => {
                matches!(&event.body, EventBody::ReviewRequested { agent: a, .. } if a == agent)
            }
        }
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

/// The workspace an event names directly, if any.
fn event_workspace(body: &EventBody) -> Option<WorkspaceId> {
    match body {
        EventBody::WorkspaceCreated { workspace } => Some(workspace.id),
        EventBody::WorkspaceUpdated { workspace_id, .. }
        | EventBody::RepoAttached { workspace_id, .. }
        | EventBody::RepoDetached { workspace_id, .. } => Some(*workspace_id),
        EventBody::ReviewCreated { review } => Some(review.workspace_id),
        EventBody::ReviewUpdated { .. }
        | EventBody::ReviewTargetUpdated { .. }
        | EventBody::ReviewDeleted { .. }
        | EventBody::ReviewTargetsResolved { .. }
        | EventBody::CommentCreated { .. }
        | EventBody::CommentEdited { .. }
        | EventBody::CommentDeleted { .. }
        | EventBody::CommentReanchored { .. }
        | EventBody::ThreadDeferred { .. }
        | EventBody::ThreadResolved { .. }
        | EventBody::ThreadUnresolved { .. }
        | EventBody::FileViewed { .. }
        | EventBody::FileUnviewed { .. }
        | EventBody::ReviewRequested { .. }
        | EventBody::ReviewChecked { .. }
        | EventBody::SuggestionApplied { .. } => None,
    }
}

/// The review an event belongs to, if any.
fn event_review(body: &EventBody) -> Option<ReviewId> {
    match body {
        EventBody::ReviewCreated { review } => Some(review.id),
        EventBody::CommentCreated { comment } => Some(comment.review_id),
        EventBody::ReviewUpdated { review_id, .. }
        | EventBody::ReviewTargetUpdated { review_id, .. }
        | EventBody::ReviewDeleted { review_id }
        | EventBody::ReviewTargetsResolved { review_id, .. }
        | EventBody::CommentEdited { review_id, .. }
        | EventBody::CommentDeleted { review_id, .. }
        | EventBody::CommentReanchored { review_id, .. }
        | EventBody::ThreadDeferred { review_id, .. }
        | EventBody::ThreadResolved { review_id, .. }
        | EventBody::ThreadUnresolved { review_id, .. }
        | EventBody::FileViewed { review_id, .. }
        | EventBody::FileUnviewed { review_id, .. }
        | EventBody::ReviewRequested { review_id, .. }
        | EventBody::ReviewChecked { review_id, .. }
        | EventBody::SuggestionApplied { review_id, .. } => Some(*review_id),
        EventBody::WorkspaceCreated { .. }
        | EventBody::WorkspaceUpdated { .. }
        | EventBody::RepoAttached { .. }
        | EventBody::RepoDetached { .. } => None,
    }
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
