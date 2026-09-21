//! Client-side operations shared by the CLI and the MCP server: the
//! multi-request recipes (resolve a blob through the review's trees, collect
//! a streamed render, long-poll events) and id generation for mutations.
//! Both front ends are thin printers over this.

use std::path::Path;
use std::time::Duration;

use nits_protocol::{
    Anchor, BaseRefSpec, BlobOid, ChunkIndex, CommentId, CommentKind, ContextHash, DiffScope,
    Event, FileChange, FileRenderHeader, LineNo, LineRange, Mutation, NonEmpty, RefSpec,
    RenderChunk, RenderOpts, Repo, RepoId, RepoPath, Request, ResolvedSource, Response, Review,
    ReviewId, ReviewSnapshot, ReviewTarget, RpcError, Seq, Side, Since, StreamItem, SubscribeScope,
    ThreadId, TreeEntryKind, Workspace, WorkspaceId,
};

use crate::client::{Client, ClientError, Unsolicited};

#[derive(Debug, thiserror::Error)]
pub enum OpsError {
    #[error("{0}")]
    Invalid(String),
    #[error("daemon: {0}")]
    Client(Box<ClientError>),
    #[error("daemon: {0:?}")]
    Rpc(RpcError),
    #[error("unexpected response shape from daemon")]
    Shape,
}

impl From<ClientError> for OpsError {
    fn from(e: ClientError) -> Self {
        match e {
            ClientError::Rpc(r) => OpsError::Rpc(r),
            other => OpsError::Client(Box::new(other)),
        }
    }
}

impl From<nits_protocol::InvariantError> for OpsError {
    fn from(e: nits_protocol::InvariantError) -> Self {
        OpsError::Invalid(e.to_string())
    }
}

/// A connected client plus the per-connection mutation counter.
#[derive(Debug)]
pub struct Ops {
    client: Client,
    seq: u64,
}

/// Where a new thread lands: `thread_id` equals `comment_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewThread {
    pub comment_id: CommentId,
    pub thread_id: ThreadId,
}

/// Events collected by one long-poll and where to resume from.
/// The workspace and attached repo that contain a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub workspace: Workspace,
    pub repo: Repo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Polled {
    pub events: Vec<Event>,
    pub last_seq: Seq,
}

/// A live subscription with its own connection. Creating it acknowledges the
/// subscription before later calls proceed; dropping it releases the connection
/// and its daemon subscriptions, including when a wait is cancelled.
#[derive(Debug)]
pub struct EventPoll {
    client: Client,
    last_seq: Seq,
    timeout: Duration,
    max: usize,
}

impl EventPoll {
    pub async fn subscribe(
        client: Client,
        scope: SubscribeScope,
        since: Since,
        timeout: Duration,
        max: usize,
    ) -> Result<Self, OpsError> {
        let last_seq = subscribe_events(&client, scope, since).await?;
        Ok(Self {
            client,
            last_seq,
            timeout,
            max,
        })
    }

    pub async fn wait(self) -> Result<Polled, OpsError> {
        collect_events(&self.client, self.last_seq, self.timeout, self.max).await
    }
}

impl Ops {
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self { client, seq: 0 }
    }

    #[must_use]
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Bootstrap on the daemon's filesystem, including nested directories.
    pub async fn ensure_directory_review(
        &mut self,
        path: String,
        base: Option<BaseRefSpec>,
        head: Option<RefSpec>,
    ) -> Result<nits_protocol::DirectoryReview, OpsError> {
        let (ts, random) = crate::ids::fresh_parts();
        self.seq += 1;
        match self
            .client
            .request(Request::EnsureDirectoryReview {
                client_seq: nits_protocol::ClientSeq::new(self.seq),
                options: nits_protocol::EnsureDirectoryReview {
                    workspace_id: WorkspaceId::from_parts(ts, random),
                    repo_id: RepoId::from_parts(ts, random),
                    review_id: ReviewId::from_parts(ts, random),
                    path,
                    base,
                    head,
                },
            })
            .await?
        {
            Response::DirectoryReview { review } => Ok(review),
            _ => Err(OpsError::Shape),
        }
    }

    pub async fn workspaces(&self) -> Result<Vec<Workspace>, OpsError> {
        match self.client.request(Request::ListWorkspaces).await? {
            Response::Workspaces { workspaces } => Ok(workspaces),
            _ => Err(OpsError::Shape),
        }
    }

    /// The workspace whose attached repo contains `dir` (the deepest repo
    /// path that is a prefix of `dir`, after canonicalising). This is how
    /// front ends default `--workspace`/`--repo` from the working directory
    /// instead of from shared mutable "current" state, so concurrent
    /// sessions in different projects never interfere. `Invalid` when no
    /// repo contains `dir` or several workspaces attach the same one.
    pub async fn locate(&self, dir: &Path) -> Result<Located, OpsError> {
        let dir = std::fs::canonicalize(dir)
            .map_err(|e| OpsError::Invalid(format!("{}: {e}", dir.display())))?;
        let mut best: Vec<Located> = Vec::new();
        for ws in self.workspaces().await? {
            for repo in &ws.repos {
                let root = Path::new(&repo.path);
                if !dir.starts_with(root) {
                    continue;
                }
                let depth = root.components().count();
                let best_depth = best
                    .first()
                    .map_or(0, |b| Path::new(&b.repo.path).components().count());
                if depth > best_depth {
                    best.clear();
                }
                if depth >= best_depth {
                    best.push(Located {
                        workspace: ws.clone(),
                        repo: repo.clone(),
                    });
                }
            }
        }
        match best.len() {
            1 => Ok(best.remove(0)),
            0 => Err(OpsError::Invalid(format!(
                "{} is not inside any attached repo; pass --workspace or attach it first",
                dir.display()
            ))),
            _ => Err(OpsError::Invalid(format!(
                "{} is attached in several workspaces ({}); pass --workspace",
                dir.display(),
                best.iter()
                    .map(|b| format!("{} \"{}\"", b.workspace.id, b.workspace.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    pub async fn reviews(&self, workspace_id: WorkspaceId) -> Result<Vec<Review>, OpsError> {
        match self
            .client
            .request(Request::ListReviews { workspace_id })
            .await?
        {
            Response::Reviews { reviews } => Ok(reviews),
            _ => Err(OpsError::Shape),
        }
    }

    /// Ask the daemon beside the repository to choose a working-tree base.
    pub async fn default_base(&self, repo_id: RepoId) -> Result<RefSpec, OpsError> {
        match self
            .client
            .request(Request::DefaultBase { repo_id })
            .await?
        {
            Response::DefaultBase { base } => Ok(base),
            _ => Err(OpsError::Shape),
        }
    }

    pub async fn snapshot(&self, review_id: ReviewId) -> Result<ReviewSnapshot, OpsError> {
        match self
            .client
            .request(Request::ReviewSnapshot { review_id })
            .await?
        {
            Response::ReviewSnapshot { snapshot } => Ok(snapshot),
            _ => Err(OpsError::Shape),
        }
    }

    pub async fn files(&self, review_id: ReviewId) -> Result<Vec<FileChange>, OpsError> {
        self.files_scoped(review_id, DiffScope::All)
            .await
            .map(|(files, _)| files)
    }

    pub async fn files_scoped(
        &self,
        review_id: ReviewId,
        scope: DiffScope,
    ) -> Result<(Vec<FileChange>, NonEmpty<nits_protocol::ResolvedTarget>), OpsError> {
        match self
            .client
            .request(Request::ListFiles { review_id, scope })
            .await?
        {
            Response::Files { files, resolved } => {
                Ok((files, NonEmpty::new(resolved).map_err(|_| OpsError::Shape)?))
            }
            _ => Err(OpsError::Shape),
        }
    }

    /// The changed file at `path`, disambiguated by `repo_id` when needed.
    pub async fn file(
        &self,
        review_id: ReviewId,
        repo_id: Option<RepoId>,
        path: &str,
    ) -> Result<FileChange, OpsError> {
        let files = self.files(review_id).await?;
        let mut matches = files
            .into_iter()
            .filter(|f| f.path.as_str() == path && repo_id.is_none_or(|r| r == f.repo_id));
        let first = matches
            .next()
            .ok_or_else(|| OpsError::Invalid(format!("{path} is not changed in this review")))?;
        if matches.next().is_some() {
            return Err(OpsError::Invalid(format!(
                "{path} is changed in more than one repo; pass repo_id"
            )));
        }
        Ok(first)
    }

    /// The blob at `path` on `side`, looked up through the review's resolved
    /// target trees so unchanged files resolve too.
    pub async fn blob(
        &self,
        review_id: ReviewId,
        repo_id: Option<RepoId>,
        path: &RepoPath,
        side: Side,
    ) -> Result<(RepoId, BlobOid), OpsError> {
        let snap = self.snapshot(review_id).await?;
        let resolved = snap
            .resolved
            .ok_or_else(|| OpsError::Invalid("review targets are not resolved yet".into()))?;
        let mut found = None;
        for t in resolved
            .iter()
            .filter(|t| repo_id.is_none_or(|r| r == t.repo_id))
        {
            let r = match side {
                Side::Base => &t.base,
                Side::Head => &t.head,
            };
            let ref_spec = match &r.source {
                ResolvedSource::Commit { oid } => RefSpec::Commit { oid: *oid },
                ResolvedSource::WorkingTree { .. } => RefSpec::WorkingTree,
            };
            let Response::TreeSnapshot { snapshot } = self
                .client
                .request(Request::TreeSnapshot {
                    repo_id: t.repo_id,
                    ref_spec,
                })
                .await?
            else {
                return Err(OpsError::Shape);
            };
            let entry = snapshot.entries.iter().find(|entry| entry.path == *path);
            if let Some(entry) = entry {
                if found.is_some() {
                    return Err(OpsError::Invalid(format!(
                        "{path} exists in more than one repo; pass repo_id"
                    )));
                }
                found = Some((t.repo_id, entry.kind.clone()));
            }
        }
        let (repo_id, kind) = found
            .ok_or_else(|| OpsError::Invalid(format!("{path} not found on the {side:?} side")))?;
        match kind {
            TreeEntryKind::File { oid, .. } | TreeEntryKind::Symlink { oid } => Ok((repo_id, oid)),
            TreeEntryKind::Submodule { .. } => Err(OpsError::Invalid(format!(
                "{path} is a submodule, not a source blob; inspect its diff for commit identities"
            ))),
            TreeEntryKind::Dir { .. } => Err(OpsError::Invalid(format!(
                "{path} is a directory, not a source blob"
            ))),
        }
    }

    /// Collect a streamed render (`FileRender` / `BlobRender`) in chunk order.
    pub async fn render(
        &self,
        request: Request,
    ) -> Result<(FileRenderHeader, Vec<RenderChunk>), OpsError> {
        let (_, mut rx) = self.client.stream(request).await?;
        let mut header = None;
        let mut chunks = Vec::new();
        while let Some(item) = rx.recv().await {
            match item? {
                StreamItem::Header { header: h } => header = Some(h),
                StreamItem::Chunk { chunk, .. } => chunks.push(chunk),
                StreamItem::ReviewSnapshot { .. } | StreamItem::TreeSnapshot { .. } => {}
            }
        }
        chunks.sort_by_key(|c| c.index);
        Ok((header.ok_or(OpsError::Shape)?, chunks))
    }

    /// Diff of one changed file.
    pub async fn diff(
        &self,
        review_id: ReviewId,
        repo_id: Option<RepoId>,
        path: &str,
        opts: RenderOpts,
    ) -> Result<(FileChange, FileRenderHeader, Vec<RenderChunk>), OpsError> {
        self.diff_scoped(review_id, repo_id, path, opts, DiffScope::All)
            .await
    }

    pub async fn diff_scoped(
        &self,
        review_id: ReviewId,
        repo_id: Option<RepoId>,
        path: &str,
        opts: RenderOpts,
        scope: DiffScope,
    ) -> Result<(FileChange, FileRenderHeader, Vec<RenderChunk>), OpsError> {
        let (files, _) = self.files_scoped(review_id, scope).await?;
        let mut matching = files.into_iter().filter(|file| {
            file.path.as_str() == path && repo_id.is_none_or(|id| id == file.repo_id)
        });
        let file = matching
            .next()
            .ok_or_else(|| OpsError::Invalid(format!("{path} is not changed in this scope")))?;
        if matching.next().is_some() {
            return Err(OpsError::Invalid(
                "path is ambiguous; specify repo_id".into(),
            ));
        }
        let (header, chunks) = self
            .render(Request::FileRender {
                scope,
                review_id,
                repo_id: file.repo_id,
                path: file.path.clone(),
                opts,
                first_chunk: ChunkIndex::FIRST,
            })
            .await?;
        Ok((file, header, chunks))
    }

    /// A whole file at one side of the review.
    pub async fn file_at(
        &self,
        review_id: ReviewId,
        repo_id: Option<RepoId>,
        path: &RepoPath,
        side: Side,
    ) -> Result<(RepoId, BlobOid, FileRenderHeader, Vec<RenderChunk>), OpsError> {
        let (repo_id, blob_oid) = self.blob(review_id, repo_id, path, side).await?;
        let (header, chunks) = self
            .render(Request::BlobRender {
                repo_id,
                path: path.clone(),
                blob_oid,
                first_chunk: ChunkIndex::FIRST,
            })
            .await?;
        Ok((repo_id, blob_oid, header, chunks))
    }

    /// Submit one mutation with the next client sequence number.
    pub async fn mutate(&mut self, mutation: Mutation) -> Result<Event, OpsError> {
        self.seq += 1;
        let client_seq = nits_protocol::ClientSeq::new(self.seq);
        match self
            .client
            .request(Request::Mutate {
                client_seq,
                mutation,
            })
            .await?
        {
            Response::Committed { event } => Ok(event),
            _ => Err(OpsError::Shape),
        }
    }

    pub async fn create_workspace(
        &mut self,
        name: String,
    ) -> Result<(WorkspaceId, Event), OpsError> {
        let (ts, r) = crate::ids::fresh_parts();
        let workspace_id = WorkspaceId::from_parts(ts, r);
        let event = self
            .mutate(Mutation::CreateWorkspace { workspace_id, name })
            .await?;
        Ok((workspace_id, event))
    }

    pub async fn attach_repo(
        &mut self,
        workspace_id: WorkspaceId,
        path: String,
        display_name: String,
    ) -> Result<(RepoId, Event), OpsError> {
        let (ts, r) = crate::ids::fresh_parts();
        let repo_id = RepoId::from_parts(ts, r);
        let event = self
            .mutate(Mutation::AttachRepo {
                workspace_id,
                repo_id,
                path,
                display_name,
            })
            .await?;
        Ok((repo_id, event))
    }

    pub async fn create_review(
        &mut self,
        workspace_id: WorkspaceId,
        title: String,
        targets: NonEmpty<ReviewTarget>,
    ) -> Result<(ReviewId, Event), OpsError> {
        let (ts, r) = crate::ids::fresh_parts();
        let review_id = ReviewId::from_parts(ts, r);
        let event = self
            .mutate(Mutation::CreateReview {
                review_id,
                workspace_id,
                title,
                targets,
            })
            .await?;
        Ok((review_id, event))
    }

    /// Anchor for `path` on `side`; `Lines` when a validated range is given, else `File`.
    /// The context hash is left for the daemon to compute.
    pub async fn anchor(
        &self,
        review_id: ReviewId,
        repo_id: Option<RepoId>,
        path: &RepoPath,
        side: Side,
        lines: Option<LineRange>,
    ) -> Result<Anchor, OpsError> {
        let (repo_id, blob_oid) = self.blob(review_id, repo_id, path, side).await?;
        Ok(match lines {
            None => Anchor::File {
                repo_id,
                path: path.clone(),
                blob_oid,
            },
            Some(lines) => Anchor::Lines {
                repo_id,
                path: path.clone(),
                side,
                blob_oid,
                lines,
                context_hash: ContextHash::new(0),
            },
        })
    }

    pub async fn new_thread(
        &mut self,
        review_id: ReviewId,
        kind: CommentKind,
        anchor: Anchor,
        body: String,
    ) -> Result<(NewThread, Event), OpsError> {
        let (ts, r) = crate::ids::fresh_parts();
        let comment_id = CommentId::from_parts(ts, r);
        let event = self
            .mutate(Mutation::AddComment {
                review_id,
                comment_id,
                kind,
                anchor,
                body,
                context: None,
            })
            .await?;
        Ok((
            NewThread {
                comment_id,
                thread_id: ThreadId::from_parts(ts, r),
            },
            event,
        ))
    }

    pub async fn reply(
        &mut self,
        review_id: ReviewId,
        thread_id: ThreadId,
        body: String,
    ) -> Result<(CommentId, Event), OpsError> {
        let (ts, r) = crate::ids::fresh_parts();
        let comment_id = CommentId::from_parts(ts, r);
        let event = self
            .mutate(Mutation::Reply {
                review_id,
                thread_id,
                comment_id,
                kind: CommentKind::Note,
                body,
            })
            .await?;
        Ok((comment_id, event))
    }

    /// Subscribe, wait up to `timeout` for at least one event, drain what
    /// else is queued (up to `max`), unsubscribe. `since` is `After(seq)` to
    /// replay a gap or `Now` for live only.
    pub async fn poll_events(
        &self,
        scope: SubscribeScope,
        since: Since,
        timeout: Duration,
        max: usize,
    ) -> Result<Polled, OpsError> {
        let client = &self.client;
        // The previous Unsubscribed response fences its queued events. Drop
        // those copies: explicit replay below starts at the caller's cursor,
        // including events queued earlier but excluded by `max` or the timeout.
        while tokio::time::timeout(Duration::ZERO, client.next_unsolicited())
            .await
            .is_ok_and(|m| m.is_some())
        {}
        let last_seq = subscribe_events(client, scope.clone(), since).await?;
        let result = collect_events(client, last_seq, timeout, max).await;
        let _ = client.request(Request::Unsubscribe { scope }).await;
        result
    }
}

async fn subscribe_events(
    client: &Client,
    scope: SubscribeScope,
    since: Since,
) -> Result<Seq, OpsError> {
    let Response::Subscribed { seq: head } =
        client.request(Request::Subscribe { scope, since }).await?
    else {
        return Err(OpsError::Shape);
    };
    Ok(match since {
        Since::After { seq } => seq,
        Since::Now => head,
    })
}

async fn collect_events(
    client: &Client,
    mut last_seq: Seq,
    timeout: Duration,
    max: usize,
) -> Result<Polled, OpsError> {
    let mut deadline = tokio::time::Instant::now() + timeout;
    let mut events: Vec<Event> = Vec::new();
    while events.len() < max {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if !events.is_empty() && remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, client.next_unsolicited()).await {
            Ok(Some(Unsolicited::Event(e))) => {
                if events.is_empty() {
                    // Zero-timeout polls still return a historical batch.
                    // Keep the grace for draining queued and nearby live
                    // events, but bound the whole drain instead of resetting
                    // it after every event in a busy subscription.
                    deadline = tokio::time::Instant::now() + Duration::from_millis(20);
                }
                last_seq = e.seq;
                events.push(e);
            }
            Ok(Some(Unsolicited::Error(RpcError::SeqTooOld { oldest }))) => {
                return Err(OpsError::Invalid(format!(
                    "since is older than the daemon's backlog; restart from {oldest}"
                )));
            }
            Ok(Some(_)) => {}
            Ok(None) => return Err(ClientError::Closed.into()),
            Err(_) => break,
        }
    }
    Ok(Polled { events, last_seq })
}

/// `start..=end` as a [`LineRange`]; `end` defaults to `start`.
pub fn line_range(start: u32, end: Option<u32>) -> Result<LineRange, OpsError> {
    let s = LineNo::new(start).ok_or_else(|| OpsError::Invalid("lines start at 1".into()))?;
    let e = LineNo::new(end.unwrap_or(start))
        .ok_or_else(|| OpsError::Invalid("lines start at 1".into()))?;
    Ok(LineRange::new(s, e)?)
}
