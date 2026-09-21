//! Immutable suggestion identity, exact patch preview and committed application.

use serde::{Deserialize, Serialize};

use crate::{
    Anchor, Author, BlobOid, Comment, CommentId, CommentKind, CommentState, LineEnding, LineNo,
    RepoId, RepoPath, ReviewId, Seq, Timestamp,
};

/// Captured from `CommentCreated`, independent of subsequent comment reanchoring.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SuggestionRecord {
    pub review_id: ReviewId,
    pub comment_id: CommentId,
    pub anchor: Anchor,
    pub patch: String,
    pub outcome: SuggestionOutcome,
}

impl SuggestionRecord {
    /// Only call with the creation event's comment, never a reanchored snapshot.
    #[must_use]
    pub fn from_created(comment: &Comment) -> Option<Self> {
        let patch = match &comment.kind {
            CommentKind::Suggestion { patch } => patch.clone(),
            CommentKind::Note | CommentKind::Informational | CommentKind::Request => return None,
        };
        let anchor = match &comment.state {
            CommentState::Live | CommentState::Deleted => comment.anchor.clone(),
            CommentState::Outdated { last_good_anchor } => last_good_anchor.clone(),
        };
        Some(Self {
            review_id: comment.review_id,
            comment_id: comment.id,
            anchor,
            patch,
            outcome: SuggestionOutcome::Unapplied,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, strum::EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", deny_unknown_fields)]
#[strum_discriminants(name(SuggestionOutcomeTag), derive(Hash, strum::EnumIter))]
pub enum SuggestionOutcome {
    Unapplied,
    Applied { receipt: SuggestionReceipt },
}

/// Provenance comes from the committed event, not from a mutation ACK or bytes.
/// Historical applications may name a different path from the original anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SuggestionReceipt {
    pub seq: Seq,
    pub at: Timestamp,
    pub author: Author,
    pub repo_id: RepoId,
    pub path: RepoPath,
    pub result_blob: BlobOid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SuggestionPreview {
    pub suggestion: SuggestionRecord,
    pub inspection: SuggestionInspection,
}

/// Raw patch evidence remains in the record even when inspection fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, strum::EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", deny_unknown_fields)]
#[strum_discriminants(name(SuggestionInspectionTag), derive(Hash, strum::EnumIter))]
pub enum SuggestionInspection {
    Checked {
        hunks: Vec<SuggestionHunk>,
        worktree: SuggestionWorktree,
    },
    Rejected {
        reason: String,
    },
}

/// A read-time observation, never authorization to skip the apply-time check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, strum::EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", deny_unknown_fields)]
#[strum_discriminants(name(SuggestionWorktreeTag), derive(Hash, strum::EnumIter))]
pub enum SuggestionWorktree {
    Original,
    /// Proposed bytes are present, but this alone cannot establish who wrote them.
    Proposed,
    Changed,
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SuggestionHunk {
    pub header: String,
    pub lines: Vec<SuggestionLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SuggestionLine {
    pub kind: SuggestionLineKind,
    pub text: String,
    pub ending: LineEnding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, strum::EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", deny_unknown_fields)]
#[strum_discriminants(name(SuggestionLineKindTag), derive(Hash, strum::EnumIter))]
pub enum SuggestionLineKind {
    Context { old: LineNo, new: LineNo },
    Remove { old: LineNo },
    Add { new: LineNo },
}
