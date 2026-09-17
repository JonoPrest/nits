//! Reference parsing and request-correlated navigation stay in the sans-I/O core.
use crate::{Action, ClientCore, CoreError, Effect, Focus, Tab, render};
use nits_protocol::{CommentId, ReferenceTarget, ReviewReference, RpcError, ViewSection};

/// Events received while a reference's snapshot is in flight must survive a
/// snapshot captured before them, including the exact reply being opened.
#[derive(Debug)]
pub(crate) struct PendingReference {
    pub reference: ReviewReference,
    pub events: Vec<nits_protocol::Event>,
}

impl ClientCore {
    fn reference_error(&mut self, message: impl Into<String>) -> Vec<Effect> {
        self.view.last_error = Some(RpcError::Invalid {
            reason: message.into(),
        });
        vec![render(&[ViewSection::Connection])]
    }

    pub(crate) fn open_reference(&mut self, text: &str) -> Result<Vec<Effect>, CoreError> {
        self.require_subscribed()?;
        let reference: ReviewReference = match text.parse() {
            Ok(reference) => reference,
            Err(error) => return Ok(self.reference_error(error.to_string())),
        };
        if self.reference_context.as_ref() != Some(&reference.context) {
            return Ok(self.reference_error("This reference belongs to another daemon context. Open it with `nits open <reference>` or select that context explicitly."));
        }
        let effects = self.user(Action::OpenReview {
            review_id: reference.review_id,
        })?;
        self.view.last_error = None;
        self.pending_reference = Some(PendingReference {
            reference,
            events: Vec::new(),
        });
        Ok(effects)
    }

    pub(crate) fn land_reference(&mut self, effects: &mut Vec<Effect>) {
        let Some(PendingReference { reference, events }) = self.pending_reference.take() else {
            return;
        };
        if let Some(committed) = &mut self.committed {
            for event in events {
                if event.seq > committed.seq {
                    if matches!(event.body, nits_protocol::EventBody::ReviewDeleted { .. }) {
                        self.close_review(effects);
                        effects.extend(self.reference_error("Referenced review has been deleted"));
                        return;
                    }
                    // Requests and checkpoints derive their durable identities from
                    // the committed envelope, not the body alone.
                    crate::events::apply_event(committed, &event);
                    committed.seq = event.seq;
                }
            }
        }
        self.rebase();
        let Some(open) = &self.view.review else {
            return;
        };
        match reference.resolve(&open.snapshot) {
            Ok(Some(comment_id)) => effects.extend(
                self.focus_comment(comment_id)
                    .unwrap_or_else(|error| self.reference_error(error.to_string())),
            ),
            Ok(None) => {}
            Err(error) => effects.extend(self.reference_error(error.to_string())),
        }
    }

    pub(crate) fn focus_comment(
        &mut self,
        comment_id: CommentId,
    ) -> Result<Vec<Effect>, CoreError> {
        let open = self.view.review.as_ref().ok_or(CoreError::NoOpenReview)?;
        let comment = open.snapshot.comments.iter().find(|c| c.id == comment_id);
        let Some(comment) = comment else {
            return Ok(self.reference_error("Referenced comment is missing from this review"));
        };
        if comment.state == nits_protocol::CommentState::Deleted {
            return Ok(self.reference_error("Referenced comment has been deleted"));
        }
        // Use the same chronological ordering/filtering as the displayed model,
        // which may not have been rebuilt yet when a snapshot arrives.
        let threads = crate::diff::threads(&open.snapshot, &self.pending_ids());
        let Some(index) = threads.iter().position(|t| t.id == comment.thread_id) else {
            return Ok(self.reference_error("Referenced thread is missing from this review"));
        };
        self.view.tab = Tab::Conversation;
        self.view.focus = Focus::Thread { index };
        self.view.focused_comment = Some(comment_id);
        Ok(vec![render(&[ViewSection::Focus, ViewSection::Threads])])
    }

    pub(crate) fn decorate_references(&self, threads: &mut [crate::diff::ThreadView]) {
        let Some(context) = &self.reference_context else {
            return;
        };
        let Some(review_id) = self.view.open_review else {
            return;
        };
        for thread in threads {
            thread.reference = Some(ReviewReference {
                context: context.clone(),
                review_id,
                target: ReferenceTarget::Thread {
                    thread_id: thread.id,
                },
            });
            for comment in &mut thread.comments {
                comment.reference = Some(ReviewReference {
                    context: context.clone(),
                    review_id,
                    target: ReferenceTarget::Comment {
                        comment_id: comment.id,
                    },
                });
            }
        }
    }

    pub(crate) fn focused_reference(&self) -> Option<ReviewReference> {
        let Focus::Thread { index } = self.view.focus else {
            return None;
        };
        let thread = self.view.threads.get(index)?;
        self.view
            .focused_comment
            .and_then(|id| thread.comments.iter().find(|c| c.id == id))
            .and_then(|c| c.reference.clone())
            .or_else(|| thread.reference.clone())
    }
}
