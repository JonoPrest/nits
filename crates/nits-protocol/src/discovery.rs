//! Metadata-only review discovery, independent of client or daemon checkout paths.

use serde::{Deserialize, Serialize};
use strum::{EnumDiscriminants, EnumIter};

use crate::{
    NonEmpty, Repo, ReviewId, ReviewRequestId, ReviewStatus, ReviewTarget, Seq, Timestamp,
    WorkspaceId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[strum_discriminants(name(ReviewScopeKind), derive(EnumIter, Hash))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ReviewScope {
    All {},
    Workspace { workspace_id: WorkspaceId },
}

impl Default for ReviewScope {
    fn default() -> Self {
        Self::All {}
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReviewQuery {
    #[serde(default)]
    pub scope: ReviewScope,
    /// Case-insensitive substring, with no regular-expression interpretation.
    #[serde(default)]
    pub title: Option<String>,
    /// Exact agent routing name, matched only against unanswered requests.
    #[serde(default)]
    pub awaiting: Option<String>,
}

/// The most recent committed event belonging to this review. Sequence is the
/// daemon's durable order; timestamps are the event's signed Unix milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReviewActivity {
    pub seq: Seq,
    pub at: Timestamp,
}

/// A request remains pending until its named recipient records a checkpoint
/// explicitly answering that request. An unrelated check is not an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PendingReviewRequest {
    pub id: ReviewRequestId,
    pub recipient: String,
    pub created: Timestamp,
}

/// Existing review-list fields remain at the top level; discovery adds metadata
/// without making clients open each review or join workspace identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReviewSummary {
    pub id: ReviewId,
    pub workspace_id: WorkspaceId,
    pub title: String,
    pub targets: NonEmpty<ReviewTarget>,
    pub created: Timestamp,
    pub status: ReviewStatus,
    pub workspace_name: String,
    /// Attached repository identities for this review's targets. A detached
    /// target remains in `targets` but has no current membership here.
    pub repositories: Vec<Repo>,
    /// Open actionable threads whose root has not been deleted. Outdated
    /// anchors still count; informational/resolved/deferred threads do not.
    pub open_findings: u32,
    pub pending_requests: Vec<PendingReviewRequest>,
    pub last_activity: ReviewActivity,
}

/// One coherent store read, newest committed review activity first. `seq` is
/// the global log position reflected by all rows, including an empty result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReviewDiscovery {
    pub reviews: Vec<ReviewSummary>,
    pub seq: Seq,
}
