//! Bounded event replay, with a fixed historical end and explicit scan progress.

use serde::{Deserialize, Serialize};
use strum::{EnumDiscriminants, EnumIter};

use crate::{Event, Seq, Since};

/// A nonempty remainder of a captured log window: `after < through`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(try_from = "RawReplayCursor", into = "RawReplayCursor")]
pub struct ReplayCursor {
    after: Seq,
    through: Seq,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct RawReplayCursor {
    after: Seq,
    through: Seq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("replay cursor {after} must be before its fixed end {through}")]
pub struct ReplayCursorError {
    after: Seq,
    through: Seq,
}

impl ReplayCursor {
    pub fn new(after: Seq, through: Seq) -> Result<Self, ReplayCursorError> {
        if after < through {
            Ok(Self { after, through })
        } else {
            Err(ReplayCursorError { after, through })
        }
    }

    #[must_use]
    pub const fn after(self) -> Seq {
        self.after
    }

    #[must_use]
    pub const fn through(self) -> Seq {
        self.through
    }
}

impl TryFrom<RawReplayCursor> for ReplayCursor {
    type Error = ReplayCursorError;
    fn try_from(raw: RawReplayCursor) -> Result<Self, Self::Error> {
        Self::new(raw.after, raw.through)
    }
}

impl From<ReplayCursor> for RawReplayCursor {
    fn from(cursor: ReplayCursor) -> Self {
        Self {
            after: cursor.after,
            through: cursor.through,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(ReplayPositionKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ReplayPosition {
    /// Capture the current end once. `Now` returns that end without history.
    Start { since: Since },
    /// Resume scanning the same fixed window, including unmatched records.
    Continue { cursor: ReplayCursor },
    /// Wait for the log to advance beyond `after`, then capture a new window.
    /// The Core read is immediate; the daemon owns the cancellable wait.
    Follow { after: Seq },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(ReplayProgressKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ReplayProgress {
    /// Last scanned sequence, even when the page contains no matching events.
    More {
        after: Seq,
    },
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReplayPage {
    /// Fixed inclusive end, retained on continuation. For a future starting
    /// cursor this equals that cursor, so a completed page never moves backwards.
    pub through: Seq,
    pub events: Vec<Event>,
    pub progress: ReplayProgress,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_is_a_nonempty_ordered_window_on_the_wire() {
        for (after, through) in [(0, 1), (1, 9), (u64::MAX - 1, u64::MAX)] {
            let cursor = ReplayCursor::new(Seq::new(after), Seq::new(through)).unwrap();
            assert_eq!(
                serde_json::from_value::<ReplayCursor>(serde_json::to_value(cursor).unwrap())
                    .unwrap(),
                cursor
            );
        }
        for (after, through) in [(0, 0), (9, 1), (u64::MAX, u64::MAX)] {
            assert!(
                serde_json::from_value::<ReplayPosition>(serde_json::json!({
                    "type": "Continue", "cursor": { "after": after, "through": through }
                }))
                .is_err()
            );
        }
    }
}
