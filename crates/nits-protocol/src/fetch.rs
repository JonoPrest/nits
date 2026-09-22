//! Explicit remote fetch receipts. Fetch never selects or checks out a new ref.

use serde::{Deserialize, Serialize};
use strum::{EnumDiscriminants, EnumIter};

/// Ref refresh after a successful fetch. A resolution error does not undo the
/// fetched objects/remote-tracking refs and must not imply that fetch was skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(FetchResolutionKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum FetchResolution {
    Resolved {
        targets: crate::NonEmpty<crate::ResolvedTarget>,
        changed: bool,
    },
    Unavailable {
        reason: String,
    },
}

/// Git leaves symbolic remote-tracking destinations as aliases instead of
/// updating them directly. A selected alias may therefore still resolve to old
/// local content even after fetch succeeds. Ref names are output from Git.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SymbolicTrackingRef {
    pub name: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReviewFetch {
    pub review_id: crate::ReviewId,
    pub repo_id: crate::RepoId,
    pub remote: crate::RemoteName,
    pub symbolic_tracking_refs: Vec<SymbolicTrackingRef>,
    pub resolution: FetchResolution,
}
