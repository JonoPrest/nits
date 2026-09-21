//! Bounded native host → UI messages. A logical patch batch is applied only
//! after all its fragments arrive; source rows are never shortened to fit IPC.

use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};
use strum::{EnumDiscriminants, EnumIter};

use crate::ViewPatch;

/// The complete serialized UI message must stay below this budget.
pub const VIEW_MESSAGE_LIMIT: usize = 64 * 1024;
// Leave room for the native event wrapper outside our common JSON envelope.
const FRAME_LIMIT: usize = VIEW_MESSAGE_LIMIT - 1024;

/// Ordered within one host session. A replacement host starts a new sequence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ViewRevision(pub u32);

/// Zero-based continuation position; the initial fragment has implicit index 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ViewFragmentIndex(pub NonZeroU32);

/// UTF-8 bytes in the complete serialized logical patch array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ViewBatchBytes(pub NonZeroU32);

/// Snapshots replace the UI model; deltas require the preceding revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, EnumIter)]
pub enum ViewBatchKind {
    Snapshot,
    Delta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[strum_discriminants(name(ViewFragmentPositionKind), derive(EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ViewFragmentPosition {
    Start { bytes: ViewBatchBytes },
    More { index: ViewFragmentIndex },
    End { index: ViewFragmentIndex },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[strum_discriminants(name(ViewFrameBodyKind), derive(EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ViewFrameBody {
    Complete {
        patches: Vec<ViewPatch>,
    },
    Fragment {
        position: ViewFragmentPosition,
        /// A UTF-8 substring of the logical JSON array, not a partial patch.
        json: String,
    },
}

/// One wire message. The revision/kind apply to the whole logical batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewFrame {
    pub revision: ViewRevision,
    pub kind: ViewBatchKind,
    pub body: ViewFrameBody,
}

/// A queue item holds the whole group, so one large snapshot cannot overflow
/// a queue of its own fragments. Deliberately not serializable: adapters emit
/// each bounded frame separately instead of nesting them in a larger message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewDelivery {
    frames: Vec<ViewFrame>,
}

impl ViewDelivery {
    #[must_use]
    pub fn frames(&self) -> &[ViewFrame] {
        &self.frames
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ViewDeliveryError {
    #[error("view delivery revision exhausted; reconnect the UI")]
    RevisionExhausted,
    #[error("view batch exceeds the supported UTF-8 byte count")]
    BatchTooLarge,
    #[error("serialized view frame exceeded its byte budget")]
    FrameTooLarge,
    #[error("view serialization failed: {0}")]
    Json(#[from] serde_json::Error),
}

/// Pure sequencing/framing shared by all native hosts.
#[derive(Debug, Default)]
pub struct ViewEncoder {
    revision: ViewRevision,
}

impl ViewEncoder {
    pub fn encode(
        &mut self,
        kind: ViewBatchKind,
        patches: Vec<ViewPatch>,
    ) -> Result<ViewDelivery, ViewDeliveryError> {
        let revision = ViewRevision(
            self.revision
                .0
                .checked_add(1)
                .ok_or(ViewDeliveryError::RevisionExhausted)?,
        );
        let complete = ViewFrame {
            revision,
            kind,
            body: ViewFrameBody::Complete { patches },
        };
        let frames = if serde_json::to_vec(&complete)?.len() < FRAME_LIMIT {
            vec![complete]
        } else {
            let ViewFrameBody::Complete { patches } = complete.body else {
                return Err(ViewDeliveryError::FrameTooLarge);
            };
            fragment(revision, kind, &serde_json::to_string(&patches)?)?
        };
        // Check the actual envelope as well as the chunking estimate. This
        // remains authoritative when new metadata is added to the wire shape.
        for frame in &frames {
            if serde_json::to_vec(frame)?.len() >= FRAME_LIMIT {
                return Err(ViewDeliveryError::FrameTooLarge);
            }
        }
        self.revision = revision;
        Ok(ViewDelivery { frames })
    }
}

fn fragment(
    revision: ViewRevision,
    kind: ViewBatchKind,
    json: &str,
) -> Result<Vec<ViewFrame>, ViewDeliveryError> {
    let bytes = u32::try_from(json.len())
        .ok()
        .and_then(NonZeroU32::new)
        .map(ViewBatchBytes)
        .ok_or(ViewDeliveryError::BatchTooLarge)?;
    // Reserve a conservative metadata allowance, then account for JSON string
    // escaping exactly. UTF-8 characters are indivisible between frames.
    let data_limit = FRAME_LIMIT - 1024;
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut escaped = 0;
    for (offset, character) in json.char_indices() {
        let cost = match character {
            '"' | '\\' | '\u{8}' | '\u{c}' | '\n' | '\r' | '\t' => 2,
            c if c < '\u{20}' => 6,
            c => c.len_utf8(),
        };
        if escaped + cost > data_limit {
            pieces.push(&json[start..offset]);
            start = offset;
            escaped = 0;
        }
        escaped += cost;
    }
    pieces.push(&json[start..]);
    let last = pieces.len() - 1;
    pieces
        .into_iter()
        .enumerate()
        .map(|(offset, json)| {
            let position = if offset == 0 {
                ViewFragmentPosition::Start { bytes }
            } else {
                let index = u32::try_from(offset)
                    .ok()
                    .and_then(NonZeroU32::new)
                    .map(ViewFragmentIndex)
                    .ok_or(ViewDeliveryError::BatchTooLarge)?;
                if offset == last {
                    ViewFragmentPosition::End { index }
                } else {
                    ViewFragmentPosition::More { index }
                }
            };
            Ok(ViewFrame {
                revision,
                kind,
                body: ViewFrameBody::Fragment {
                    position,
                    json: json.to_owned(),
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use nits_protocol::RpcError;
    use proptest::prelude::*;

    use super::*;
    use crate::ConnectionView;

    fn batch(text: String) -> Vec<ViewPatch> {
        vec![ViewPatch::Connection {
            connection: ConnectionView::Subscribed,
            last_error: Some(RpcError::Internal { message: text }),
        }]
    }

    fn reconstruct(delivery: &ViewDelivery, revision: ViewRevision) -> Vec<ViewPatch> {
        let mut joined = String::new();
        let mut expected_bytes = 0;
        for (offset, original) in delivery.frames().iter().enumerate() {
            let bytes = serde_json::to_vec(original).unwrap();
            assert!(bytes.len() < VIEW_MESSAGE_LIMIT, "{} bytes", bytes.len());
            let frame: ViewFrame = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(frame.revision, revision);
            match frame.body {
                ViewFrameBody::Complete { patches } => {
                    assert_eq!(delivery.frames().len(), 1);
                    return patches;
                }
                ViewFrameBody::Fragment { position, json } => {
                    assert!(!json.is_empty());
                    match position {
                        ViewFragmentPosition::Start { bytes } => {
                            assert_eq!(offset, 0);
                            expected_bytes = bytes.0.get() as usize;
                        }
                        ViewFragmentPosition::More { index } => {
                            assert_eq!(index.0.get() as usize, offset);
                            assert!(offset + 1 < delivery.frames().len());
                        }
                        ViewFragmentPosition::End { index } => {
                            assert_eq!(index.0.get() as usize, offset);
                            assert_eq!(offset + 1, delivery.frames().len());
                        }
                    }
                    joined.push_str(&json);
                }
            }
        }
        assert_eq!(joined.len(), expected_bytes);
        serde_json::from_str(&joined).unwrap()
    }

    #[test]
    fn complete_and_fragmented_batches_preserve_identity_and_exact_json() {
        let mut encoder = ViewEncoder::default();
        for (offset, size) in [0, 1, 63_000, 63_500, 64_000, 200_000]
            .into_iter()
            .enumerate()
        {
            let patches = batch("\"\\λ😀\0\r\n".repeat(size));
            let delivery = encoder
                .encode(ViewBatchKind::Snapshot, patches.clone())
                .unwrap();
            let revision = ViewRevision(u32::try_from(offset + 1).unwrap());
            assert!(
                delivery
                    .frames()
                    .iter()
                    .all(|frame| frame.kind == ViewBatchKind::Snapshot)
            );
            assert_eq!(reconstruct(&delivery, revision), patches);
        }
    }

    #[test]
    fn a_single_group_can_exceed_the_native_queue_fragment_capacity() {
        let patches = batch("a".repeat(64 * 1024 * 260));
        let delivery = ViewEncoder::default()
            .encode(ViewBatchKind::Delta, patches.clone())
            .unwrap();
        assert!(delivery.frames().len() > 256);
        assert_eq!(reconstruct(&delivery, ViewRevision(1)), patches);
    }

    #[test]
    fn revisions_never_wrap_or_advance_after_failure() {
        let mut encoder = ViewEncoder {
            revision: ViewRevision(u32::MAX),
        };
        assert!(matches!(
            encoder.encode(ViewBatchKind::Delta, Vec::new()),
            Err(ViewDeliveryError::RevisionExhausted)
        ));
        assert_eq!(encoder.revision, ViewRevision(u32::MAX));
        assert!(serde_json::from_str::<ViewFragmentIndex>("0").is_err());
        assert!(serde_json::from_str::<ViewBatchBytes>("0").is_err());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]
        #[test]
        fn arbitrary_unicode_and_json_escaping_roundtrip(characters in prop::collection::vec(any::<char>(), 0..40_000)) {
            let patches = batch(characters.into_iter().collect());
            let delivery = ViewEncoder::default().encode(ViewBatchKind::Delta, patches.clone()).unwrap();
            prop_assert_eq!(reconstruct(&delivery, ViewRevision(1)), patches);
        }
    }
}
