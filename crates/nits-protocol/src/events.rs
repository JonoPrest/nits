//! The append-only event log's wire form. See `docs/ARCHITECTURE.md` §4.2.

use serde::{Deserialize, Serialize};
use strum::{EnumDiscriminants, EnumIter};

use crate::domain::{
    Anchor, Author, Comment, CommentState, Human, Repo, ResolvedTarget, Review, ReviewStatus,
    ReviewTarget, Workspace,
};
use crate::ids::{
    BlobOid, ClientId, ClientSeq, CommentId, RepoId, ReviewId, Seq, ThreadId, Timestamp,
    WorkspaceId,
};
use crate::invariants::{NonEmpty, RepoPath};

/// A committed event: has a daemon-assigned `seq`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub seq: Seq,
    pub ts: Timestamp,
    pub author: Author,
    pub client_id: ClientId,
    pub client_seq: ClientSeq,
    pub body: EventBody,
}

/// Every kind of thing that can happen. Each variant carries enough to update
/// the materialised views without consulting anything but earlier events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(EventKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum EventBody {
    WorkspaceCreated {
        workspace: Workspace,
    },
    WorkspaceUpdated {
        workspace_id: WorkspaceId,
        name: String,
    },
    RepoAttached {
        workspace_id: WorkspaceId,
        repo: Repo,
    },
    RepoDetached {
        workspace_id: WorkspaceId,
        repo_id: RepoId,
    },
    ReviewCreated {
        review: Review,
    },
    ReviewUpdated {
        review_id: ReviewId,
        title: String,
        status: ReviewStatus,
    },
    /// One repository's requested base/head pair changed.
    ReviewTargetUpdated {
        review_id: ReviewId,
        target: ReviewTarget,
    },
    /// Tombstone. The review is excluded from listings; comments remain in the
    /// log until an offline compaction.
    ReviewDeleted {
        review_id: ReviewId,
    },
    /// Snapshot of the resolved OIDs. Emitted only when they change.
    ReviewTargetsResolved {
        review_id: ReviewId,
        targets: NonEmpty<ResolvedTarget>,
    },
    CommentCreated {
        comment: Comment,
    },
    CommentEdited {
        review_id: ReviewId,
        comment_id: CommentId,
        body: String,
    },
    CommentDeleted {
        review_id: ReviewId,
        comment_id: CommentId,
    },
    /// Result of re-anchoring after the review's targets moved.
    CommentReanchored {
        review_id: ReviewId,
        comment_id: CommentId,
        anchor: Anchor,
        state: CommentState,
    },
    ThreadDeferred {
        review_id: ReviewId,
        thread_id: ThreadId,
        reason: crate::DeferralReason,
        tracking_url: Option<crate::TrackingUrl>,
    },
    ThreadResolved {
        review_id: ReviewId,
        thread_id: ThreadId,
    },
    ThreadUnresolved {
        review_id: ReviewId,
        thread_id: ThreadId,
    },
    /// Human-only; the store rejects it from agents before it is appended.
    FileViewed {
        review_id: ReviewId,
        repo_id: RepoId,
        path: RepoPath,
        viewer: Human,
        content: crate::ViewedContent,
    },
    FileUnviewed {
        review_id: ReviewId,
        repo_id: RepoId,
        path: RepoPath,
        viewer: Human,
    },
    /// Asks a named agent to review. Shown as a card in human clients.
    ReviewRequested {
        review_id: ReviewId,
        agent: String,
        note: String,
        targets: crate::RequestedTargets,
    },
    ReviewChecked {
        review_id: ReviewId,
        reviewer: crate::ReviewerIdentity,
        targets: NonEmpty<ResolvedTarget>,
        in_reply_to: Option<crate::ReviewRound>,
    },
    /// A suggestion comment's patch was written to the working tree.
    SuggestionApplied {
        review_id: ReviewId,
        comment_id: CommentId,
        repo_id: RepoId,
        path: RepoPath,
        result_blob: BlobOid,
    },
}

impl EventBody {
    /// The review this event belongs to, if any. Used for subscription
    /// scoping and view updates.
    #[must_use]
    pub fn review_id(&self) -> Option<ReviewId> {
        match self {
            EventBody::WorkspaceCreated { .. }
            | EventBody::WorkspaceUpdated { .. }
            | EventBody::RepoAttached { .. }
            | EventBody::RepoDetached { .. } => None,
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
        }
    }

    /// The workspace this event belongs to, if it is a workspace-level event.
    #[must_use]
    pub fn workspace_id(&self) -> Option<WorkspaceId> {
        match self {
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
}
