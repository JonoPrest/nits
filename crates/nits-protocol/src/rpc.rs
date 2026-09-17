//! Client ⇄ daemon messages. See `docs/ARCHITECTURE.md` §4.8.
//!
//! One `ClientMsg::Request` gets either a single `ServerMsg::Response`, or a
//! sequence of `ServerMsg::StreamItem` terminated by `ServerMsg::StreamEnd`,
//! or a `ServerMsg::Error`. Which one is determined by [`Request::shape`].
//! Events arrive unsolicited on subscribed connections.

use serde::{Deserialize, Serialize};
use strum::{EnumDiscriminants, EnumIter};

use crate::domain::{
    Anchor, Author, BaseRefSpec, ChangeKind, Comment, CommentKind, CommitInfo, ContentHit,
    DiffScope, FileChange, RefCandidate, RefSpec, RenderOpts, ResolvedTarget, Review, ReviewStatus,
    ReviewTarget, ReviewTargetUpdate, Thread, TreeDelta, TreeSnapshot, ViewedMark, Workspace,
};
use crate::events::Event;
use crate::ids::{
    BlobOid, ClientId, ClientSeq, CommentId, RepoId, RequestId, ReviewId, Seq, ThreadId,
    WorkspaceId,
};
use crate::invariants::{NonEmpty, RepoPath};
use crate::render::{ChunkIndex, FileRenderHeader, RenderChunk};
use crate::version::{BuildInfo, ProtocolVersion, SchemaVersion, UpgradeNotice};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(ClientMsgKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ClientMsg {
    /// First message on a connection. States the protocol version the client
    /// speaks; the daemon answers `Welcome` or `Rejected`.
    Hello {
        client_id: ClientId,
        protocol: ProtocolVersion,
        client: BuildInfo,
        /// Who every mutation on this connection is attributed to.
        author: Author,
    },
    Request {
        id: RequestId,
        request: Request,
    },
    /// Stop a streaming request early. Best effort; a `StreamEnd` still follows.
    Cancel {
        id: RequestId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(ServerMsgKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ServerMsg {
    /// Handshake accepted. `protocol` is the version all following frames
    /// use; `upgrade` is set when the client is behind but still served.
    Welcome {
        protocol: ProtocolVersion,
        daemon: BuildInfo,
        schema: SchemaVersion,
        upgrade: Option<UpgradeNotice>,
    },
    /// Handshake refused; the daemon closes the connection after this.
    Rejected {
        error: RpcError,
    },
    Response {
        id: RequestId,
        response: Response,
    },
    StreamItem {
        id: RequestId,
        item: StreamItem,
    },
    StreamEnd {
        id: RequestId,
    },
    Error {
        id: RequestId,
        error: RpcError,
    },
    /// Broadcast to every subscriber whose scope matches.
    Event {
        event: Event,
    },
    /// Working-tree change for a subscribed ref.
    TreeDelta {
        delta: TreeDelta,
    },
}

/// Whether a request answers with one `Response` or a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResponseShape {
    Single,
    Stream,
}

/// Where a subscription starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(SinceKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Since {
    /// Fresh client: no replay; the response carries the current `Seq`.
    Now,
    /// Reconnect: replay everything after this `Seq`, then tail.
    After { seq: Seq },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(SubscribeScopeKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum SubscribeScope {
    All,
    Workspace {
        workspace_id: WorkspaceId,
    },
    Review {
        review_id: ReviewId,
    },
    /// Only `ReviewRequested` events addressed to this agent name.
    AwaitingAgent {
        agent: String,
    },
}

/// A mutation submitted by a client. Author is taken from the connection,
/// `client_seq` lets the client match its optimistic copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(MutationKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Mutation {
    CreateWorkspace {
        workspace_id: WorkspaceId,
        name: String,
    },
    RenameWorkspace {
        workspace_id: WorkspaceId,
        name: String,
    },
    AttachRepo {
        workspace_id: WorkspaceId,
        repo_id: RepoId,
        path: String,
        display_name: String,
    },
    DetachRepo {
        workspace_id: WorkspaceId,
        repo_id: RepoId,
    },
    CreateReview {
        review_id: ReviewId,
        workspace_id: WorkspaceId,
        title: String,
        targets: NonEmpty<ReviewTarget>,
    },
    UpdateReview {
        review_id: ReviewId,
        title: String,
        status: ReviewStatus,
    },
    UpdateReviewTarget {
        review_id: ReviewId,
        update: ReviewTargetUpdate,
    },
    DeleteReview {
        review_id: ReviewId,
    },
    /// Starts a new thread. `comment_id` doubles as the thread id.
    AddComment {
        review_id: ReviewId,
        comment_id: CommentId,
        kind: CommentKind,
        anchor: Anchor,
        body: String,
        /// The content being viewed (see [`Comment::context`]).
        context: Option<crate::CommentContext>,
    },
    Reply {
        review_id: ReviewId,
        thread_id: ThreadId,
        comment_id: CommentId,
        kind: CommentKind,
        body: String,
    },
    EditComment {
        review_id: ReviewId,
        comment_id: CommentId,
        body: String,
    },
    DeleteComment {
        review_id: ReviewId,
        comment_id: CommentId,
    },
    DeferThread {
        review_id: ReviewId,
        thread_id: ThreadId,
        reason: crate::DeferralReason,
        tracking_url: Option<crate::TrackingUrl>,
    },
    ResolveThread {
        review_id: ReviewId,
        thread_id: ThreadId,
    },
    UnresolveThread {
        review_id: ReviewId,
        thread_id: ThreadId,
    },
    /// Human-only; rejected with `RpcError::Forbidden` for agents.
    MarkViewed {
        review_id: ReviewId,
        repo_id: RepoId,
        path: RepoPath,
    },
    UnmarkViewed {
        review_id: ReviewId,
        repo_id: RepoId,
        path: RepoPath,
    },
    RequestReview {
        review_id: ReviewId,
        agent: String,
        note: String,
    },
    ApplySuggestion {
        review_id: ReviewId,
        comment_id: CommentId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(RequestKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Request {
    /// Ensure an open directory review beside the daemon. The supplied IDs
    /// are allocated only when the checkout or matching review is unknown.
    EnsureDirectoryReview {
        client_seq: ClientSeq,
        options: EnsureDirectoryReview,
    },
    ListWorkspaces,
    ListReviews {
        workspace_id: WorkspaceId,
    },
    /// Detect the branch a working-tree review should use as its base.
    /// Detection runs beside the repository, in the daemon, so local and
    /// remote clients behave identically.
    DefaultBase {
        repo_id: RepoId,
    },
    /// Branches, tags, recent commits and working tree read from git.
    ListRefs {
        repo_id: RepoId,
    },
    GetReview {
        review_id: ReviewId,
    },
    /// Review, resolved targets, threads, comments and viewed marks, without
    /// rendering anything. What `OpenReview` streams first.
    ReviewSnapshot {
        review_id: ReviewId,
    },
    /// Changed files across the review's targets, with rename detection.
    /// `scope` narrows which diff (UI-DESIGN §Diff scope); absent = `All`.
    ListFiles {
        review_id: ReviewId,
        #[serde(default)]
        scope: DiffScope,
    },
    /// Streamed: `ReviewSnapshot` → `TreeSnapshot` per target ref →
    /// `FileRenderHeader` per changed file → first `RenderChunk` per file.
    OpenReview {
        review_id: ReviewId,
        opts: RenderOpts,
    },
    /// Re-resolve targets now (e.g. after a commit). Emits
    /// `ReviewTargetsResolved` only if something changed.
    ResolveTargets {
        review_id: ReviewId,
    },
    ListCommits {
        review_id: ReviewId,
        repo_id: RepoId,
    },
    TreeSnapshot {
        repo_id: RepoId,
        #[serde(rename = "ref")]
        ref_spec: RefSpec,
    },
    /// Streamed: header, then chunks starting at `first_chunk`.
    FileRender {
        review_id: ReviewId,
        repo_id: RepoId,
        path: RepoPath,
        opts: RenderOpts,
        first_chunk: ChunkIndex,
        #[serde(default)]
        scope: DiffScope,
    },
    /// Search file contents across the review's targets (UI-DESIGN
    /// §Search): case-insensitive substring over the scoped changed files,
    /// or over every file of the head trees when `all_files`.
    Search {
        review_id: ReviewId,
        query: String,
        #[serde(default)]
        all_files: bool,
        #[serde(default)]
        scope: DiffScope,
    },
    /// Streamed: header, then chunks starting at `first_chunk` — for an
    /// arbitrary change (a comment's recorded context), not the review's
    /// current diff of the path.
    ChangeRender {
        repo_id: RepoId,
        path: RepoPath,
        change: ChangeKind,
        opts: RenderOpts,
        first_chunk: ChunkIndex,
    },
    /// Streamed: header, then chunks starting at `first_chunk`.
    BlobRender {
        repo_id: RepoId,
        path: RepoPath,
        blob_oid: BlobOid,
        first_chunk: ChunkIndex,
    },
    /// A single chunk of a render already known from a header.
    RenderChunk {
        repo_id: RepoId,
        path: RepoPath,
        target: crate::render::RenderTarget,
        opts: RenderOpts,
        index: ChunkIndex,
    },
    Subscribe {
        scope: SubscribeScope,
        since: Since,
    },
    Unsubscribe {
        scope: SubscribeScope,
    },
    Mutate {
        client_seq: ClientSeq,
        mutation: Mutation,
    },
    /// Ask the daemon to exit once this reply is sent. Used by
    /// `nits daemon stop`; the daemon is restarted on demand.
    Shutdown,
}

impl Request {
    #[must_use]
    pub fn shape(&self) -> ResponseShape {
        match self {
            Request::OpenReview { .. }
            | Request::FileRender { .. }
            | Request::ChangeRender { .. }
            | Request::BlobRender { .. } => ResponseShape::Stream,
            Request::ListWorkspaces
            | Request::ListReviews { .. }
            | Request::ListRefs { .. }
            | Request::DefaultBase { .. }
            | Request::EnsureDirectoryReview { .. }
            | Request::GetReview { .. }
            | Request::ReviewSnapshot { .. }
            | Request::ListFiles { .. }
            | Request::Search { .. }
            | Request::ResolveTargets { .. }
            | Request::ListCommits { .. }
            | Request::TreeSnapshot { .. }
            | Request::RenderChunk { .. }
            | Request::Subscribe { .. }
            | Request::Unsubscribe { .. }
            | Request::Mutate { .. }
            | Request::Shutdown => ResponseShape::Single,
        }
    }
}

/// Materialised state of a review for a fresh client. Never a log replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReviewSnapshot {
    pub review: Review,
    pub resolved: Option<NonEmpty<ResolvedTarget>>,
    pub threads: Vec<Thread>,
    pub comments: Vec<Comment>,
    pub viewed: Vec<ViewedMark>,
    /// Durable requests, in creation event order, independent of finding threads.
    pub requests: Vec<crate::ReviewRequest>,
    /// The log position this snapshot reflects; subscribe `After` it.
    pub seq: Seq,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(ResponseKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Response {
    DirectoryReview {
        review: DirectoryReview,
    },
    Workspaces {
        workspaces: Vec<Workspace>,
    },
    Reviews {
        reviews: Vec<Review>,
    },
    DefaultBase {
        base: RefSpec,
    },
    Refs {
        repo_id: RepoId,
        refs: Vec<RefCandidate>,
    },
    Review {
        review: Review,
    },
    ReviewSnapshot {
        snapshot: ReviewSnapshot,
    },
    Files {
        files: Vec<FileChange>,
        /// The targets the files were diffed between under the request's
        /// scope (for `All`: the review's resolved targets).
        #[serde(default)]
        resolved: Vec<ResolvedTarget>,
    },
    /// Content search results, capped; `truncated` says the cap was hit.
    Search {
        hits: Vec<ContentHit>,
        truncated: bool,
    },
    /// The targets after resolution (whether or not they changed).
    Resolved {
        targets: NonEmpty<ResolvedTarget>,
        changed: bool,
    },
    Commits {
        commits: Vec<CommitInfo>,
    },
    TreeSnapshot {
        snapshot: TreeSnapshot,
    },
    RenderChunk {
        chunk: RenderChunk,
    },
    /// Position from which live events will flow.
    Subscribed {
        seq: Seq,
    },
    Unsubscribed,
    /// The committed form of a mutation. The same event is also broadcast.
    Committed {
        event: Event,
    },
    ShuttingDown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(StreamItemKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum StreamItem {
    ReviewSnapshot {
        snapshot: ReviewSnapshot,
    },
    TreeSnapshot {
        snapshot: TreeSnapshot,
    },
    Header {
        header: FileRenderHeader,
    },
    Chunk {
        repo_id: RepoId,
        path: RepoPath,
        chunk: RenderChunk,
    },
}

/// What kind of entity was not found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumIter)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum EntityKind {
    Workspace,
    Repo,
    Review,
    Comment,
    Thread,
    Ref,
    Path,
    Blob,
    Chunk,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(RpcErrorKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum RpcError {
    NotFound {
        kind: EntityKind,
        id: String,
    },
    Invalid {
        reason: String,
    },
    /// The author is not allowed to do this (e.g. an agent marking viewed).
    Forbidden {
        reason: String,
    },
    /// A `Since::After` older than the retained log.
    SeqTooOld {
        oldest: Seq,
    },
    Cancelled,
    /// The client's `Hello.protocol` cannot be served by this daemon.
    UnsupportedProtocol {
        requested: ProtocolVersion,
        supported: Vec<ProtocolVersion>,
    },
    /// A frame arrived with a different `v` than was negotiated.
    VersionMismatch {
        negotiated: ProtocolVersion,
        received: ProtocolVersion,
    },
    Internal {
        message: String,
    },
}

/// Sections of the client `ViewModel` that a render delta may touch. The
/// payload lives in `nits-client-core`; the section names are protocol so the
/// UI's schema and the core agree on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumIter)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ViewSection {
    Connection,
    /// The content-search palette (UI-DESIGN §Search).
    Search,
    ReviewList,
    Tree,
    Diff,
    Threads,
    Conversation,
    CommitStepper,
    RefSelector,
    Progress,
    Focus,
    Hints,
    Help,
    Draft,
}

/// Bootstrap parameters. Paths are interpreted on the daemon's machine;
/// omitted refs reuse an existing open working-tree review or select its
/// detected base and a working-tree head. Explicit refs must match on reuse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct EnsureDirectoryReview {
    pub workspace_id: WorkspaceId,
    pub repo_id: RepoId,
    pub review_id: ReviewId,
    pub path: String,
    pub base: Option<BaseRefSpec>,
    pub head: Option<RefSpec>,
}

/// Whether bootstrap found a matching open review or allocated one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, EnumIter)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum DirectoryReviewOutcome {
    Created,
    Reused,
}

/// The selected directory review and its requested (unresolved) refs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DirectoryReview {
    pub workspace_id: WorkspaceId,
    pub repo_id: RepoId,
    pub review_id: ReviewId,
    pub base: RefSpec,
    pub head: RefSpec,
    pub outcome: DirectoryReviewOutcome,
    /// First committed bootstrap event, or current log position on reuse.
    /// Pass to `subscribe_events` to receive subsequent events.
    pub seq: Seq,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_bootstrap_accepts_working_tree_only_for_head() {
        let options = EnsureDirectoryReview {
            workspace_id: WorkspaceId::from_parts(1, 1),
            repo_id: RepoId::from_parts(1, 2),
            review_id: ReviewId::from_parts(1, 3),
            path: "/repos/example".into(),
            base: Some(BaseRefSpec::Head),
            head: Some(RefSpec::WorkingTree),
        };
        let mut wire = serde_json::to_value(&options).unwrap();
        assert_eq!(
            serde_json::from_value::<EnsureDirectoryReview>(wire.clone()).unwrap(),
            options
        );
        wire["base"] = serde_json::json!({"type": "WorkingTree"});
        assert!(serde_json::from_value::<EnsureDirectoryReview>(wire.clone()).is_err());
        let request =
            serde_json::json!({"type": "EnsureDirectoryReview", "client_seq": 1, "options": wire});
        assert!(serde_json::from_value::<Request>(request).is_err());
    }
}
