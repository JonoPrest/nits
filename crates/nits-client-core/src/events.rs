//! Pure event semantics over a [`ReviewSnapshot`] (plan 3.3, ARCHITECTURE
//! §5.2).
//!
//! Two functions, no state:
//! - [`apply_body`] folds one committed or pending event into a snapshot.
//!   The client applies committed events to its committed snapshot and
//!   pending events on top of a copy; the two-client simulator's daemon
//!   model applies the same function, so client and daemon cannot drift.
//! - [`local_event`] is what a mutation *will* become once committed, built
//!   from the current snapshot: the optimistic event the client shows
//!   immediately, and the event the simulated daemon appends.
//!
//! Neither touches the review list or workspace state; those events are
//! handled by `ClientCore::apply_event` before this is called.

use nits_protocol::{
    Author, Comment, CommentId, CommentState, EventBody, Mutation, MutationKind, RepoId,
    ReviewSnapshot, TargetRevision, Thread, ThreadId, ThreadResolution, Timestamp, ViewSection,
    ViewedMark,
};
use strum::EnumDiscriminants;

/// Who did it and when, for events that record an author or a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventMeta {
    pub author: Author,
    pub ts: Timestamp,
}

/// Why a mutation cannot be turned into an event against this snapshot.
/// Mirrors the checks `nits-review-core` makes, so the client rejects what
/// the daemon would reject before sending it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, EnumDiscriminants)]
#[strum_discriminants(name(MutationErrorKind), derive(Hash))]
pub enum MutationError {
    #[error("comment {0} already exists")]
    DuplicateComment(CommentId),
    #[error("informational notes require a review-level anchor")]
    InformationalAnchor,
    #[error("informational thread {0} cannot be resolved or reopened")]
    InformationalThread(ThreadId),
    #[error("no thread {0}")]
    UnknownThread(ThreadId),
    #[error("no comment {0}")]
    UnknownComment(CommentId),
    #[error("only the author may change comment {0}")]
    NotAuthor(CommentId),
    #[error("thread {0} is already resolved")]
    AlreadyResolved(ThreadId),
    #[error("thread {0} is not resolved")]
    NotResolved(ThreadId),
    #[error("review has no repo {0}")]
    UnknownRepo(RepoId),
    /// The mutation is not one the client core issues optimistically.
    #[error("{0:?} is not applied locally")]
    Unsupported(MutationKind),
}

/// A root comment's id is its thread's id (same rule as `nits-review-core`).
#[must_use]
pub fn thread_id_of(id: CommentId) -> ThreadId {
    ThreadId::from_parts(id.timestamp_ms(), id.random())
}

/// The event `mutation` produces against `snapshot`, or why it cannot.
// One arm per mutation; splitting would hide the exhaustive match.
#[allow(clippy::too_many_lines)]
pub fn local_event(
    snapshot: &ReviewSnapshot,
    meta: &EventMeta,
    mutation: &Mutation,
) -> Result<EventBody, MutationError> {
    let comment = |id: CommentId| {
        snapshot
            .comments
            .iter()
            .find(|c| c.id == id)
            .ok_or(MutationError::UnknownComment(id))
    };
    let thread = |id: ThreadId| {
        snapshot
            .threads
            .iter()
            .find(|t| t.id == id)
            .ok_or(MutationError::UnknownThread(id))
    };
    match mutation {
        Mutation::AddComment {
            review_id,
            comment_id,
            kind,
            anchor,
            body,
            context,
        } => {
            if comment(*comment_id).is_ok() {
                return Err(MutationError::DuplicateComment(*comment_id));
            }
            if matches!(kind, nits_protocol::CommentKind::Informational)
                && !matches!(anchor, nits_protocol::Anchor::Review)
            {
                return Err(MutationError::InformationalAnchor);
            }
            Ok(EventBody::CommentCreated {
                comment: Comment {
                    id: *comment_id,
                    review_id: *review_id,
                    thread_id: thread_id_of(*comment_id),
                    author: meta.author.clone(),
                    kind: kind.clone(),
                    anchor: anchor.clone(),
                    body: body.clone(),
                    created: meta.ts,
                    edited: None,
                    state: CommentState::Live,
                    context: context.clone(),
                },
            })
        }
        Mutation::Reply {
            review_id,
            thread_id,
            comment_id,
            kind,
            body,
        } => {
            if comment(*comment_id).is_ok() {
                return Err(MutationError::DuplicateComment(*comment_id));
            }
            let th = thread(*thread_id)?;
            let root = comment(th.root)?;
            Ok(EventBody::CommentCreated {
                comment: Comment {
                    id: *comment_id,
                    review_id: *review_id,
                    thread_id: *thread_id,
                    author: meta.author.clone(),
                    kind: kind.clone(),
                    anchor: root.anchor.clone(),
                    body: body.clone(),
                    created: meta.ts,
                    edited: None,
                    state: root.state.clone(),
                    context: root.context.clone(),
                },
            })
        }
        Mutation::EditComment {
            review_id,
            comment_id,
            body,
        } => {
            let c = comment(*comment_id)?;
            if c.author != meta.author {
                return Err(MutationError::NotAuthor(*comment_id));
            }
            Ok(EventBody::CommentEdited {
                review_id: *review_id,
                comment_id: *comment_id,
                body: body.clone(),
            })
        }
        Mutation::DeleteComment {
            review_id,
            comment_id,
        } => {
            let c = comment(*comment_id)?;
            if c.author != meta.author {
                return Err(MutationError::NotAuthor(*comment_id));
            }
            Ok(EventBody::CommentDeleted {
                review_id: *review_id,
                comment_id: *comment_id,
            })
        }
        Mutation::ResolveThread {
            review_id,
            thread_id,
        } => {
            let th = thread(*thread_id)?;
            match th.resolution {
                ThreadResolution::Informational => {
                    Err(MutationError::InformationalThread(*thread_id))
                }
                ThreadResolution::Resolved { .. } => {
                    Err(MutationError::AlreadyResolved(*thread_id))
                }
                ThreadResolution::Open => Ok(EventBody::ThreadResolved {
                    review_id: *review_id,
                    thread_id: *thread_id,
                }),
            }
        }
        Mutation::UnresolveThread {
            review_id,
            thread_id,
        } => {
            let th = thread(*thread_id)?;
            match th.resolution {
                ThreadResolution::Informational => {
                    Err(MutationError::InformationalThread(*thread_id))
                }
                ThreadResolution::Open => Err(MutationError::NotResolved(*thread_id)),
                ThreadResolution::Resolved { .. } => Ok(EventBody::ThreadUnresolved {
                    review_id: *review_id,
                    thread_id: *thread_id,
                }),
            }
        }
        Mutation::UpdateReview {
            review_id,
            title,
            status,
        } => Ok(EventBody::ReviewUpdated {
            review_id: *review_id,
            title: title.clone(),
            status: *status,
        }),
        Mutation::UpdateReviewTarget { review_id, update } => {
            let Some(existing) = snapshot
                .review
                .targets
                .iter()
                .find(|target| target.repo_id == update.repo_id)
            else {
                return Err(MutationError::UnknownRepo(update.repo_id));
            };
            let mut target = existing.clone();
            match &update.revision {
                TargetRevision::Base { ref_spec } => target.base = ref_spec.clone().into(),
                TargetRevision::Head { ref_spec } => target.head.clone_from(ref_spec),
            }
            Ok(EventBody::ReviewTargetUpdated {
                review_id: *review_id,
                target,
            })
        }
        Mutation::CreateWorkspace { .. }
        | Mutation::RenameWorkspace { .. }
        | Mutation::AttachRepo { .. }
        | Mutation::DetachRepo { .. }
        | Mutation::CreateReview { .. }
        | Mutation::DeleteReview { .. }
        | Mutation::MarkViewed { .. }
        | Mutation::UnmarkViewed { .. }
        | Mutation::RequestReview { .. }
        | Mutation::ApplySuggestion { .. } => {
            Err(MutationError::Unsupported(MutationKind::from(mutation)))
        }
    }
}

/// Fold `body` into `snapshot` and say which view sections it touched.
/// Events for other reviews, and review-list / workspace events, touch
/// nothing here. Idempotent for `CommentCreated` (same id twice is one
/// comment) so replay and optimistic re-application are safe.
// One arm per event; splitting would hide the exhaustive match.
#[allow(clippy::too_many_lines)]
pub fn apply_body(
    snapshot: &mut ReviewSnapshot,
    meta: &EventMeta,
    body: &EventBody,
) -> Vec<ViewSection> {
    let mine = |id| snapshot.review.id == id;
    match body {
        EventBody::ReviewUpdated {
            review_id,
            title,
            status,
        } if mine(*review_id) => {
            snapshot.review.title.clone_from(title);
            snapshot.review.status = *status;
            vec![ViewSection::Conversation]
        }
        EventBody::ReviewTargetsResolved { review_id, targets } if mine(*review_id) => {
            snapshot.resolved = Some(targets.clone());
            vec![ViewSection::Diff]
        }
        EventBody::ReviewTargetUpdated { review_id, target } if mine(*review_id) => {
            let Some(existing) = snapshot
                .review
                .targets
                .iter_mut()
                .find(|existing| existing.repo_id == target.repo_id)
            else {
                return Vec::new();
            };
            existing.clone_from(target);
            vec![ViewSection::ReviewList]
        }
        EventBody::CommentCreated { comment } if mine(comment.review_id) => {
            if snapshot.comments.iter().any(|c| c.id == comment.id) {
                return Vec::new();
            }
            snapshot.comments.push(comment.clone());
            match snapshot
                .threads
                .iter_mut()
                .find(|t| t.id == comment.thread_id)
            {
                Some(t) => {
                    if !t.replies.contains(&comment.id) && t.root != comment.id {
                        t.replies.push(comment.id);
                    }
                }
                None => snapshot.threads.push(Thread {
                    id: comment.thread_id,
                    review_id: comment.review_id,
                    root: comment.id,
                    replies: Vec::new(),
                    resolution: ThreadResolution::for_root(&comment.kind),
                }),
            }
            vec![ViewSection::Threads]
        }
        EventBody::CommentEdited {
            review_id,
            comment_id,
            body,
        } if mine(*review_id) => {
            let Some(c) = snapshot.comments.iter_mut().find(|c| c.id == *comment_id) else {
                return Vec::new();
            };
            c.body.clone_from(body);
            c.edited = Some(meta.ts);
            vec![ViewSection::Threads]
        }
        EventBody::CommentDeleted {
            review_id,
            comment_id,
        } if mine(*review_id) => {
            let Some(c) = snapshot.comments.iter_mut().find(|c| c.id == *comment_id) else {
                return Vec::new();
            };
            c.state = CommentState::Deleted;
            vec![ViewSection::Threads]
        }
        EventBody::CommentReanchored {
            review_id,
            comment_id,
            anchor,
            state,
        } if mine(*review_id) => {
            let Some(c) = snapshot.comments.iter_mut().find(|c| c.id == *comment_id) else {
                return Vec::new();
            };
            c.anchor = anchor.clone();
            c.state = state.clone();
            vec![ViewSection::Threads]
        }
        EventBody::ThreadResolved {
            review_id,
            thread_id,
        } if mine(*review_id) => {
            let Some(t) = snapshot.threads.iter_mut().find(|t| t.id == *thread_id) else {
                return Vec::new();
            };
            t.resolution = ThreadResolution::Resolved {
                by: meta.author.clone(),
                at: meta.ts,
            };
            vec![ViewSection::Threads]
        }
        EventBody::ThreadUnresolved {
            review_id,
            thread_id,
        } if mine(*review_id) => {
            let Some(t) = snapshot.threads.iter_mut().find(|t| t.id == *thread_id) else {
                return Vec::new();
            };
            t.resolution = ThreadResolution::Open;
            vec![ViewSection::Threads]
        }
        EventBody::FileViewed {
            review_id,
            repo_id,
            path,
            viewer,
            blob_oid,
        } if mine(*review_id) => {
            snapshot
                .viewed
                .retain(|v| !(v.repo_id == *repo_id && v.path == *path && v.viewer == *viewer));
            snapshot.viewed.push(ViewedMark {
                review_id: *review_id,
                repo_id: *repo_id,
                path: path.clone(),
                viewer: viewer.clone(),
                blob_oid: *blob_oid,
            });
            vec![ViewSection::Progress]
        }
        EventBody::FileUnviewed {
            review_id,
            repo_id,
            path,
            viewer,
        } if mine(*review_id) => {
            snapshot
                .viewed
                .retain(|v| !(v.repo_id == *repo_id && v.path == *path && v.viewer == *viewer));
            vec![ViewSection::Progress]
        }
        EventBody::ReviewRequested { review_id, .. }
        | EventBody::SuggestionApplied { review_id, .. }
            if mine(*review_id) =>
        {
            vec![ViewSection::Conversation]
        }
        EventBody::ReviewUpdated { .. }
        | EventBody::ReviewTargetUpdated { .. }
        | EventBody::ReviewTargetsResolved { .. }
        | EventBody::CommentCreated { .. }
        | EventBody::CommentEdited { .. }
        | EventBody::CommentDeleted { .. }
        | EventBody::CommentReanchored { .. }
        | EventBody::ThreadResolved { .. }
        | EventBody::ThreadUnresolved { .. }
        | EventBody::FileViewed { .. }
        | EventBody::FileUnviewed { .. }
        | EventBody::ReviewRequested { .. }
        | EventBody::SuggestionApplied { .. }
        | EventBody::ReviewCreated { .. }
        | EventBody::ReviewDeleted { .. }
        | EventBody::WorkspaceCreated { .. }
        | EventBody::WorkspaceUpdated { .. }
        | EventBody::RepoAttached { .. }
        | EventBody::RepoDetached { .. } => Vec::new(),
    }
}
