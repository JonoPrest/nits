//! Short, bounded read transactions for historical event pages.

use std::ops::Bound::{Excluded, Included};

use nits_protocol::{ReplayPage, ReplayPosition, ReplayProgress, Seq, Since};
use redb::{ReadableDatabase, ReadableTable};

use super::{Store, StoreError, StoredEvent, tables};

/// Bound work even when a filter matches no records.
pub const REPLAY_SCAN_LIMIT: usize = 256;
/// Normal page budget, counting both stored and re-encoded event bytes.
/// A single larger event gets its own page, up to `REPLAY_EVENT_BYTES`.
pub const REPLAY_PAGE_BYTES: usize = 1024 * 1024;
/// The transport permits 64 MiB frames. Reserve 4 KiB for the envelope, page
/// metadata and framing punctuation; never construct an oversized response.
pub const REPLAY_EVENT_BYTES: usize = 64 * 1024 * 1024 - 4096;

impl Store {
    /// Scan at most one page in `(after, through]`, capturing the high-water
    /// mark in this read transaction on the first call. Filtering happens in
    /// Core, after progress is recorded, so empty result pages remain resumable.
    pub fn replay_page(&self, position: ReplayPosition) -> Result<ReplayPage, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(tables::EVENTS)?;
        let head = table
            .last()?
            .map_or(Seq::new(0), |(key, _)| Seq::new(key.value()));
        let (mut after, through) = match position {
            ReplayPosition::Start { since: Since::Now } => (head, head),
            ReplayPosition::Start {
                since: Since::After { seq },
            }
            | ReplayPosition::Follow { after: seq } => (seq, head.max(seq)),
            ReplayPosition::Continue { cursor } => {
                if cursor.through() > head {
                    return Err(StoreError::Replay {
                        reason: format!(
                            "replay end {} exceeds the current log head {head}",
                            cursor.through()
                        ),
                    });
                }
                (cursor.after(), cursor.through())
            }
        };
        let mut events = Vec::new();
        if after >= through {
            return Ok(ReplayPage {
                through,
                events,
                progress: ReplayProgress::Complete,
            });
        }
        let mut stored_bytes = 0;
        let mut wire_bytes = 0;
        for entry in table
            .range((Excluded(after.get()), Included(through.get())))?
            .take(REPLAY_SCAN_LIMIT)
        {
            let (key, value) = entry?;
            let seq = Seq::new(key.value());
            let bytes = value.value();
            if !events.is_empty() && stored_bytes + bytes.len() > REPLAY_PAGE_BYTES {
                break;
            }
            if bytes.len() > REPLAY_EVENT_BYTES {
                return Err(StoreError::Replay {
                    reason: format!(
                        "event {seq} exceeds the {REPLAY_EVENT_BYTES}-byte replay event limit"
                    ),
                });
            }
            let stored: StoredEvent =
                serde_json::from_slice(bytes).map_err(|error| StoreError::Corrupt {
                    seq,
                    reason: error.to_string(),
                })?;
            // Bound the actual wire representation too, including migrated data
            // whose original JSON spelling might differ from today's encoder.
            let encoded = serde_json::to_vec(&stored.event)?.len();
            if encoded > REPLAY_EVENT_BYTES {
                return Err(StoreError::Replay {
                    reason: format!(
                        "event {seq} exceeds the {REPLAY_EVENT_BYTES}-byte replay event limit"
                    ),
                });
            }
            if !events.is_empty() && wire_bytes + encoded + 1 > REPLAY_PAGE_BYTES {
                break;
            }
            stored_bytes += bytes.len();
            wire_bytes += encoded + 1;
            after = seq;
            events.push(stored.event);
        }
        let progress = if after >= through {
            ReplayProgress::Complete
        } else {
            ReplayProgress::More { after }
        };
        Ok(ReplayPage {
            through,
            events,
            progress,
        })
    }
}
