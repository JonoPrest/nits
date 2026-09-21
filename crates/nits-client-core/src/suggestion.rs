//! Per-comment inspection and application. Filesystem mutations are never replayed.

use nits_protocol::{
    CommentId, CommentState, Mutation, Request, RequestId, ReviewId, RpcError,
    SuggestionInspection, SuggestionOutcome, SuggestionPreview, SuggestionRecord,
    SuggestionWorktree,
};
use serde::{Deserialize, Serialize};

use crate::{ClientCore, CoreError, Effect, InFlight, MutationError, ViewSection, render};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuggestionView {
    pub record: SuggestionRecord,
    pub inspection: Option<SuggestionInspection>,
    pub status: SuggestionStatus,
    /// A previous Apply failure, including any filesystem recovery location.
    /// A subsequent read must not hide this evidence.
    pub notice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, strum::EnumDiscriminants)]
#[strum_discriminants(name(SuggestionStatusKind), derive(Hash, strum::EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum SuggestionStatus {
    Unloaded,
    Loading,
    Ready,
    Stale,
    Rejected { message: String },
    Applying,
    Uncertain { message: String },
    Applied,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SuggestionState {
    inspection: Option<SuggestionInspection>,
    notice: Option<String>,
    stage: Stage,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum Stage {
    #[default]
    Idle,
    Checking {
        request_id: RequestId,
    },
    Applying {
        request_id: RequestId,
    },
    Uncertain {
        message: String,
    },
    Rejected {
        message: String,
    },
}

impl SuggestionState {
    pub(crate) fn view(&self, record: &SuggestionRecord) -> SuggestionView {
        let status = if matches!(record.outcome, SuggestionOutcome::Applied { .. }) {
            SuggestionStatus::Applied
        } else {
            match &self.stage {
                Stage::Checking { .. } => SuggestionStatus::Loading,
                Stage::Applying { .. } => SuggestionStatus::Applying,
                Stage::Uncertain { message } => SuggestionStatus::Uncertain { message: message.clone() },
                Stage::Rejected { message } => SuggestionStatus::Rejected { message: message.clone() },
                Stage::Idle => match &self.inspection {
                    None => SuggestionStatus::Unloaded,
                    Some(SuggestionInspection::Rejected { reason }) => SuggestionStatus::Rejected { message: reason.clone() },
                    Some(SuggestionInspection::Checked { worktree, .. }) => match worktree {
                        SuggestionWorktree::Original => SuggestionStatus::Ready,
                        SuggestionWorktree::Changed => SuggestionStatus::Stale,
                        SuggestionWorktree::Unavailable { reason } => SuggestionStatus::Rejected { message: reason.clone() },
                        SuggestionWorktree::Proposed => SuggestionStatus::Uncertain {
                            message: "The proposed bytes are already present, but no applied receipt was recorded. Inspect the file before taking further action.".into(),
                        },
                    },
                },
            }
        };
        SuggestionView {
            record: record.clone(),
            inspection: self.inspection.clone(),
            status,
            notice: self.notice.clone(),
        }
    }
}

impl ClientCore {
    pub(crate) fn suggestion_applied(
        &mut self,
        request_id: RequestId,
        review_id: ReviewId,
        comment_id: CommentId,
        event: nits_protocol::Event,
    ) -> Result<Vec<Effect>, CoreError> {
        if !matches!(event.body, nits_protocol::EventBody::SuggestionApplied { review_id: review, comment_id: comment, .. } if review == review_id && comment == comment_id)
        {
            return Err(CoreError::UnexpectedResponse {
                id: request_id,
                expected: "Committed application of the requested suggestion",
                got: "Another committed event",
            });
        }
        if self.view.open_review == Some(review_id)
            && let Some(state) = self.suggestions.get_mut(&comment_id)
            && state.stage == (Stage::Applying { request_id })
        {
            state.stage = Stage::Idle;
        }
        let crate::Connection::Subscribed { last_seq } = self.connection else {
            return Err(self.wrong_state(crate::InputKind::Server));
        };
        self.connection = crate::Connection::Subscribed {
            last_seq: last_seq.max(event.seq),
        };
        Ok(self.apply_event(event))
    }

    fn suggestion_record(&self, comment_id: CommentId) -> Result<&SuggestionRecord, CoreError> {
        let open = self.view.review.as_ref().ok_or(CoreError::NoOpenReview)?;
        if !open.snapshot.comments.iter().any(|comment| {
            comment.id == comment_id && !matches!(comment.state, CommentState::Deleted)
        }) {
            return Err(CoreError::Mutation(MutationError::UnknownComment(
                comment_id,
            )));
        }
        open.snapshot
            .suggestions
            .iter()
            .find(|record| record.comment_id == comment_id)
            .ok_or(CoreError::Mutation(MutationError::UnknownComment(
                comment_id,
            )))
    }

    pub(crate) fn suggestion_view(&self, record: &SuggestionRecord) -> SuggestionView {
        self.suggestions
            .get(&record.comment_id)
            .cloned()
            .unwrap_or_default()
            .view(record)
    }

    fn focus_suggestion(&mut self, comment_id: CommentId) {
        if let Some(index) = self.view.threads.iter().position(|thread| {
            thread
                .comments
                .iter()
                .any(|comment| comment.id == comment_id)
        }) {
            self.view.focus = crate::Focus::Thread { index };
            self.view.focused_comment = Some(comment_id);
        }
    }

    pub(crate) fn preview_suggestion(
        &mut self,
        comment_id: CommentId,
    ) -> Result<Vec<Effect>, CoreError> {
        self.require_subscribed()?;
        let review_id = self.suggestion_record(comment_id)?.review_id;
        self.focus_suggestion(comment_id);
        let state = self.suggestions.entry(comment_id).or_default();
        if matches!(state.stage, Stage::Checking { .. } | Stage::Applying { .. }) {
            return Ok(Vec::new());
        }
        let request_id = RequestId::new(self.next_request);
        state.stage = Stage::Checking { request_id };
        Ok(vec![
            self.request(
                Request::PreviewSuggestion {
                    review_id,
                    comment_id,
                },
                InFlight::PreviewSuggestion {
                    review_id,
                    comment_id,
                },
            ),
            render(&[
                ViewSection::Threads,
                ViewSection::Conversation,
                ViewSection::Focus,
            ]),
        ])
    }

    pub(crate) fn apply_suggestion(
        &mut self,
        comment_id: CommentId,
    ) -> Result<Vec<Effect>, CoreError> {
        self.require_subscribed()?;
        let record = self.suggestion_record(comment_id)?;
        let review_id = record.review_id;
        let view = self.suggestion_view(record);
        match view.status {
            SuggestionStatus::Applying | SuggestionStatus::Applied => return Ok(Vec::new()),
            SuggestionStatus::Ready => {}
            SuggestionStatus::Unloaded
            | SuggestionStatus::Loading
            | SuggestionStatus::Stale
            | SuggestionStatus::Rejected { .. }
            | SuggestionStatus::Uncertain { .. } => {
                return Err(CoreError::SuggestionNotReady(comment_id));
            }
        }
        self.focus_suggestion(comment_id);
        let request_id = RequestId::new(self.next_request);
        self.suggestions.entry(comment_id).or_default().stage = Stage::Applying { request_id };
        let client_seq = self.next_client_seq;
        self.next_client_seq = client_seq.next();
        Ok(vec![
            self.request(
                Request::Mutate {
                    client_seq,
                    mutation: Mutation::ApplySuggestion {
                        review_id,
                        comment_id,
                    },
                },
                InFlight::ApplySuggestion {
                    review_id,
                    comment_id,
                },
            ),
            render(&[
                ViewSection::Threads,
                ViewSection::Conversation,
                ViewSection::Focus,
            ]),
        ])
    }

    pub(crate) fn suggestion_previewed(
        &mut self,
        request_id: RequestId,
        review_id: ReviewId,
        comment_id: CommentId,
        preview: SuggestionPreview,
    ) -> Result<Vec<Effect>, CoreError> {
        if self.view.open_review != Some(review_id) {
            return Ok(Vec::new());
        }
        let record = self.suggestion_record(comment_id)?;
        if preview.suggestion.review_id != review_id
            || preview.suggestion.comment_id != comment_id
            || preview.suggestion.anchor != record.anchor
            || preview.suggestion.patch != record.patch
        {
            return Err(CoreError::UnexpectedResponse {
                id: request_id,
                expected: "Preview of the immutable suggestion",
                got: "Preview for another suggestion",
            });
        }
        let Some(state) = self.suggestions.get_mut(&comment_id) else {
            return Ok(Vec::new());
        };
        if state.stage != (Stage::Checking { request_id }) {
            return Ok(Vec::new());
        }
        state.inspection = Some(preview.inspection);
        state.stage = Stage::Idle;
        // A read can discover a durable receipt whose event was lost. Never
        // replace an observed receipt with an older unapplied read response.
        if let SuggestionOutcome::Applied { receipt } = preview.suggestion.outcome
            && let Some(committed) = &mut self.committed
            && let Some(record) = committed
                .suggestions
                .iter_mut()
                .find(|record| record.comment_id == comment_id)
        {
            let replace = match &record.outcome {
                SuggestionOutcome::Unapplied => true,
                SuggestionOutcome::Applied { receipt: existing } => receipt.seq > existing.seq,
            };
            if replace {
                record.outcome = SuggestionOutcome::Applied { receipt };
            }
            self.rebase();
        }
        Ok(vec![render(&[
            ViewSection::Threads,
            ViewSection::Conversation,
        ])])
    }

    pub(crate) fn suggestion_failed(
        &mut self,
        request_id: RequestId,
        review_id: ReviewId,
        comment_id: CommentId,
        error: &RpcError,
    ) -> Vec<Effect> {
        if self.view.open_review != Some(review_id) {
            return Vec::new();
        }
        let applied = self
            .suggestion_record(comment_id)
            .is_ok_and(|record| matches!(record.outcome, SuggestionOutcome::Applied { .. }));
        let Some(state) = self.suggestions.get_mut(&comment_id) else {
            return Vec::new();
        };
        if applied {
            state.stage = Stage::Idle;
            return vec![render(&[ViewSection::Threads, ViewSection::Conversation])];
        }
        let message = crate::rpc_error_message(error);
        match state.stage {
            Stage::Applying {
                request_id: expected,
            } if expected == request_id => {
                // Even an error can follow filesystem replacement. Keep its
                // recovery location and reconcile by reading, never replaying.
                state.notice = Some(message.clone());
                state.stage = Stage::Uncertain { message };
            }
            Stage::Checking {
                request_id: expected,
            } if expected == request_id => {
                state.stage = Stage::Rejected { message };
            }
            Stage::Idle
            | Stage::Uncertain { .. }
            | Stage::Rejected { .. }
            | Stage::Applying { .. }
            | Stage::Checking { .. } => return Vec::new(),
        }
        vec![render(&[ViewSection::Threads, ViewSection::Conversation])]
    }

    pub(crate) fn suggestions_disconnected(&mut self) -> bool {
        let mut changed = false;
        for state in self.suggestions.values_mut() {
            match state.stage {
                Stage::Applying { .. } | Stage::Checking { .. } => {
                    changed = true;
                    state.stage = Stage::Uncertain { message: "Connection lost. The result will be checked before another application is offered.".into() };
                }
                Stage::Idle | Stage::Uncertain { .. } | Stage::Rejected { .. } => {}
            }
        }
        changed
    }

    pub(crate) fn suggestions_reconnected(&mut self) -> Vec<Effect> {
        let comments: Vec<_> = self
            .suggestions
            .iter()
            .filter_map(|(comment, state)| {
                matches!(state.stage, Stage::Uncertain { .. }).then_some(*comment)
            })
            .collect();
        comments
            .into_iter()
            .flat_map(|comment| self.preview_suggestion(comment).unwrap_or_default())
            .collect()
    }
}
