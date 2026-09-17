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
    review_workspaces: Mutex<HashMap<ReviewId, WorkspaceId>>,
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
            review_workspaces: Mutex::new(HashMap::new()),
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
        let job: WriteJob = Box::new(move |core| {
            let result = (|| {
                let before = core.last_seq()?;
                let out = f(core)?;
                let events = core.events_after(before)?;
                Ok::<_, CoreError>((out, events))
            })();
            // The receiver only disappears if the caller was cancelled; the
            // write already happened and is broadcast below regardless.
            let _ = tx.send(result);
        });
        self.writer.send(job).map_err(|_| DaemonError::Shutdown)?;
        let (out, events) = rx.await.map_err(|_| DaemonError::Shutdown)??;
        for e in &events {
            self.note_workspace(e);
            // No subscribers is not an error.
            let _ = self.events.send(Arc::new(e.clone()));
        }
        Ok((out, events))
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

    fn note_workspace(&self, e: &Event) {
        if let EventBody::ReviewCreated { review } = &e.body {
            self.review_workspaces
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(review.id, review.workspace_id);
        }
    }

    /// Workspace of a review, cached; falls back to the store for reviews
    /// created before this process started.
    fn workspace_of(&self, review: ReviewId) -> Option<WorkspaceId> {
        if let Some(w) = self
            .review_workspaces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&review)
        {
            return Some(*w);
        }
        let w = self.core.review(review).ok()?.review.workspace_id;
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
        | EventBody::ThreadResolved { .. }
        | EventBody::ThreadUnresolved { .. }
        | EventBody::FileViewed { .. }
        | EventBody::FileUnviewed { .. }
        | EventBody::ReviewRequested { .. }
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
        | EventBody::ThreadResolved { review_id, .. }
        | EventBody::ThreadUnresolved { review_id, .. }
        | EventBody::FileViewed { review_id, .. }
        | EventBody::FileUnviewed { review_id, .. }
        | EventBody::ReviewRequested { review_id, .. }
        | EventBody::SuggestionApplied { review_id, .. } => Some(*review_id),
        EventBody::WorkspaceCreated { .. }
        | EventBody::WorkspaceUpdated { .. }
        | EventBody::RepoAttached { .. }
        | EventBody::RepoDetached { .. } => None,
    }
}
