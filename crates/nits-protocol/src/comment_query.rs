//! Complete, filtered conversations from one coherent review snapshot.

use serde::{Deserialize, Serialize};
use strum::EnumIter;

use crate::{
    Comment, CommentId, NonEmpty, RepoId, RepoPath, ReviewCheckpoint, ReviewId, ReviewRequest,
    ReviewerCheckpoint, Seq, SuggestionRecord, ThreadId, ThreadResolution,
};

/// A deleted root is a tombstoned conversation, not an actionable open finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, EnumIter)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CommentThreadStatus {
    Open,
    Resolved,
    Deferred,
    Informational,
    Deleted,
}

/// All filters select whole threads, retaining the root and every reply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CommentQuery {
    #[serde(default)]
    pub status: Option<CommentThreadStatus>,
    #[serde(default)]
    pub thread_id: Option<ThreadId>,
    /// Exact repository-relative path; optionally disambiguated by `repo_id`.
    #[serde(default)]
    pub path: Option<RepoPath>,
    #[serde(default)]
    pub repo_id: Option<RepoId>,
    /// Exact human or agent name on any comment; "daemon" selects daemon authors.
    #[serde(default)]
    pub author: Option<String>,
    /// Exclusive committed sequence of thread activity, including replies,
    /// edits, deletion, reanchoring, disposition changes and suggestion applies.
    /// Unrelated review metadata, requests and checkpoints do not match.
    #[serde(default)]
    pub since: Option<Seq>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CommentThread {
    pub id: ThreadId,
    pub review_id: ReviewId,
    pub root: CommentId,
    pub replies: Vec<CommentId>,
    /// The persisted disposition, retained even when the root is deleted.
    pub resolution: ThreadResolution,
    pub status: CommentThreadStatus,
    /// Root first, then replies in their committed order. Deleted entries remain
    /// explicit tombstones; their retained historical body is not live prose.
    pub comments: NonEmpty<Comment>,
}

/// Counts describe only the returned threads. Status counts are disjoint:
/// deleted roots are counted solely in deleted, regardless of old disposition.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CommentSummary {
    pub threads: u32,
    pub open: u32,
    pub resolved: u32,
    pub deferred: u32,
    pub informational: u32,
    pub deleted: u32,
    pub comments: u32,
    pub deleted_comments: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CommentListing {
    pub threads: Vec<CommentThread>,
    pub summary: CommentSummary,
    /// Suggestion identities and durable receipts for returned comments.
    pub suggestions: Vec<SuggestionRecord>,
    /// Coordination history is review-wide, independent of the thread filter.
    pub requests: Vec<ReviewRequest>,
    pub checkpoints: Vec<ReviewCheckpoint>,
    pub latest_checkpoints: Vec<ReviewerCheckpoint>,
    /// Global store position, including when the filtered result is empty.
    pub seq: Seq,
}
