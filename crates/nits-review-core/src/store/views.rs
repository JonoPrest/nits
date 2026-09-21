//! The fold from events to view tables. `apply` is the only writer of the
//! view tables, used both incrementally by `Store::append` and in bulk by
//! `Store::rebuild_views`, so the two can never disagree.

use nits_protocol::{
    Anchor, CommentState, Event, EventBody, NonEmpty, ResolvedTarget, Review, ReviewId, Seq,
    Thread, ThreadResolution, ViewedMark,
};
use redb::ReadableTable;
use serde::{Deserialize, Serialize};

use super::StoreError;
use super::tables::Write;

/// Whether a review is live or has been tombstoned by `ReviewDeleted`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ReviewLifecycle {
    Live,
    Deleted { at: Seq },
}

/// A review as the views hold it: the review itself plus its last resolved
/// targets, which are not part of `Review` because they are derived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRecord {
    pub review: Review,
    pub resolved: Option<NonEmpty<ResolvedTarget>>,
    pub lifecycle: ReviewLifecycle,
}

fn inconsistent(seq: Seq, reason: impl Into<String>) -> StoreError {
    StoreError::Inconsistent {
        seq,
        reason: reason.into(),
    }
}

fn get<T: serde::de::DeserializeOwned>(
    bytes: Option<redb::AccessGuard<'_, &[u8]>>,
) -> Result<Option<T>, StoreError> {
    bytes
        .map(|g| Ok(serde_json::from_slice(g.value())?))
        .transpose()
}

/// Index a comment's anchor under its blob (if it has one).
fn index_anchor(
    t: &mut Write<'_>,
    review: ReviewId,
    comment: &nits_protocol::Comment,
) -> Result<(), StoreError> {
    match &comment.anchor {
        Anchor::Review => {}
        Anchor::File {
            repo_id, blob_oid, ..
        }
        | Anchor::Lines {
            repo_id, blob_oid, ..
        } => {
            t.anchors_by_blob.insert(
                (
                    repo_id.to_string().as_str(),
                    blob_oid.to_string().as_str(),
                    comment.id.to_string().as_str(),
                ),
                review.to_string().as_str(),
            )?;
        }
    }
    Ok(())
}

fn unindex_anchor(t: &mut Write<'_>, comment: &nits_protocol::Comment) -> Result<(), StoreError> {
    match &comment.anchor {
        Anchor::Review => {}
        Anchor::File {
            repo_id, blob_oid, ..
        }
        | Anchor::Lines {
            repo_id, blob_oid, ..
        } => {
            t.anchors_by_blob.remove((
                repo_id.to_string().as_str(),
                blob_oid.to_string().as_str(),
                comment.id.to_string().as_str(),
            ))?;
        }
    }
    Ok(())
}

/// Apply one event to the view tables.
///
/// Events that reference state the views do not hold (a comment on an
/// unknown review, a reply to an unknown thread) are an `Inconsistent`
/// error: validation happens before `append`, so this only fires on a
/// corrupt or hand-edited log.
#[allow(clippy::too_many_lines)]
pub(super) fn apply(t: &mut Write<'_>, event: &Event) -> Result<(), StoreError> {
    let seq = event.seq;
    match &event.body {
        EventBody::WorkspaceCreated { workspace } => {
            t.workspaces.insert(
                workspace.id.to_string().as_str(),
                serde_json::to_vec(workspace)?.as_slice(),
            )?;
        }
        EventBody::WorkspaceUpdated { workspace_id, name } => {
            let key = workspace_id.to_string();
            let mut ws: nits_protocol::Workspace = get(t.workspaces.get(key.as_str())?)?
                .ok_or_else(|| inconsistent(seq, format!("unknown workspace {workspace_id}")))?;
            ws.name.clone_from(name);
            t.workspaces
                .insert(key.as_str(), serde_json::to_vec(&ws)?.as_slice())?;
        }
        EventBody::RepoAttached { workspace_id, repo } => {
            let key = workspace_id.to_string();
            let mut ws: nits_protocol::Workspace = get(t.workspaces.get(key.as_str())?)?
                .ok_or_else(|| inconsistent(seq, format!("unknown workspace {workspace_id}")))?;
            ws.repos.retain(|r| r.id != repo.id);
            ws.repos.push(repo.clone());
            t.workspaces
                .insert(key.as_str(), serde_json::to_vec(&ws)?.as_slice())?;
        }
        EventBody::RepoDetached {
            workspace_id,
            repo_id,
        } => {
            let key = workspace_id.to_string();
            let mut ws: nits_protocol::Workspace = get(t.workspaces.get(key.as_str())?)?
                .ok_or_else(|| inconsistent(seq, format!("unknown workspace {workspace_id}")))?;
            ws.repos.retain(|r| r.id != *repo_id);
            t.workspaces
                .insert(key.as_str(), serde_json::to_vec(&ws)?.as_slice())?;
        }
        EventBody::ReviewCreated { review } => {
            let rec = ReviewRecord {
                review: review.clone(),
                resolved: None,
                lifecycle: ReviewLifecycle::Live,
            };
            t.reviews.insert(
                review.id.to_string().as_str(),
                serde_json::to_vec(&rec)?.as_slice(),
            )?;
        }
        EventBody::ReviewUpdated {
            review_id,
            title,
            status,
        } => {
            let mut rec = load_review(t, seq, *review_id)?;
            rec.review.title.clone_from(title);
            rec.review.status = *status;
            save_review(t, &rec)?;
        }
        EventBody::ReviewTargetUpdated { review_id, target } => {
            let mut rec = load_review(t, seq, *review_id)?;
            let stored = rec
                .review
                .targets
                .iter_mut()
                .find(|stored| stored.repo_id == target.repo_id)
                .ok_or_else(|| {
                    inconsistent(seq, format!("review has no repo {}", target.repo_id))
                })?;
            stored.clone_from(target);
            save_review(t, &rec)?;
        }
        EventBody::ReviewDeleted { review_id } => {
            let mut rec = load_review(t, seq, *review_id)?;
            rec.lifecycle = ReviewLifecycle::Deleted { at: seq };
            save_review(t, &rec)?;
        }
        EventBody::ReviewTargetsResolved { review_id, targets } => {
            let mut rec = load_review(t, seq, *review_id)?;
            rec.resolved = Some(targets.clone());
            save_review(t, &rec)?;
        }
        EventBody::CommentCreated { comment } => {
            load_review(t, seq, comment.review_id)?;
            let rkey = comment.review_id.to_string();
            let tkey = comment.thread_id.to_string();
            let existing: Option<Thread> = get(t.threads.get((rkey.as_str(), tkey.as_str()))?)?;
            let thread = if let Some(mut th) = existing {
                th.replies.push(comment.id);
                th
            } else {
                if comment.thread_id.to_string() != comment.id.to_string() {
                    return Err(inconsistent(
                        seq,
                        format!(
                            "reply {} to unknown thread {}",
                            comment.id, comment.thread_id
                        ),
                    ));
                }
                Thread {
                    id: comment.thread_id,
                    review_id: comment.review_id,
                    root: comment.id,
                    replies: vec![],
                    resolution: ThreadResolution::for_root(&comment.kind),
                }
            };
            t.threads.insert(
                (rkey.as_str(), tkey.as_str()),
                serde_json::to_vec(&thread)?.as_slice(),
            )?;
            t.comments.insert(
                (rkey.as_str(), comment.id.to_string().as_str()),
                serde_json::to_vec(comment)?.as_slice(),
            )?;
            index_anchor(t, comment.review_id, comment)?;
        }
        EventBody::CommentEdited {
            review_id,
            comment_id,
            body,
        } => {
            let mut c = load_comment(t, seq, *review_id, *comment_id)?;
            c.body.clone_from(body);
            c.edited = Some(event.ts);
            save_comment(t, &c)?;
        }
        EventBody::CommentDeleted {
            review_id,
            comment_id,
        } => {
            let mut c = load_comment(t, seq, *review_id, *comment_id)?;
            unindex_anchor(t, &c)?;
            c.state = CommentState::Deleted;
            save_comment(t, &c)?;
        }
        EventBody::CommentReanchored {
            review_id,
            comment_id,
            anchor,
            state,
        } => {
            let mut c = load_comment(t, seq, *review_id, *comment_id)?;
            unindex_anchor(t, &c)?;
            c.anchor = anchor.clone();
            c.state = state.clone();
            save_comment(t, &c)?;
            if !matches!(c.state, CommentState::Deleted) {
                index_anchor(t, *review_id, &c)?;
            }
        }
        EventBody::ThreadDeferred {
            review_id,
            thread_id,
            reason,
            tracking_url,
        } => {
            let mut th = load_thread(t, seq, *review_id, *thread_id)?;
            th.resolution = ThreadResolution::Deferred {
                reason: reason.clone(),
                tracking_url: tracking_url.clone(),
                by: event.author.clone(),
                at: event.ts,
            };
            save_thread(t, &th)?;
        }
        EventBody::ThreadResolved {
            review_id,
            thread_id,
        } => {
            let mut th = load_thread(t, seq, *review_id, *thread_id)?;
            th.resolution = ThreadResolution::Resolved {
                by: event.author.clone(),
                at: event.ts,
            };
            save_thread(t, &th)?;
        }
        EventBody::ThreadUnresolved {
            review_id,
            thread_id,
        } => {
            let mut th = load_thread(t, seq, *review_id, *thread_id)?;
            th.resolution = ThreadResolution::Open;
            save_thread(t, &th)?;
        }
        EventBody::FileViewed {
            review_id,
            repo_id,
            path,
            viewer,
            content,
        } => {
            load_review(t, seq, *review_id)?;
            let mark = ViewedMark {
                review_id: *review_id,
                repo_id: *repo_id,
                path: path.clone(),
                viewer: viewer.clone(),
                content: *content,
            };
            t.viewed.insert(
                (
                    review_id.to_string().as_str(),
                    repo_id.to_string().as_str(),
                    path.as_str(),
                ),
                serde_json::to_vec(&mark)?.as_slice(),
            )?;
        }
        EventBody::FileUnviewed {
            review_id,
            repo_id,
            path,
            viewer: _,
        } => {
            t.viewed.remove((
                review_id.to_string().as_str(),
                repo_id.to_string().as_str(),
                path.as_str(),
            ))?;
        }
        // Invitations are durable snapshot state as well as subscription events.
        EventBody::ReviewRequested { .. } => {
            if let Some(request) = nits_protocol::ReviewRequest::from_event(event) {
                t.requests.insert(
                    (
                        request.review_id.to_string().as_str(),
                        request.id.event_seq().get(),
                    ),
                    serde_json::to_vec(&request)?.as_slice(),
                )?;
            }
        }
        EventBody::ReviewChecked { .. } => {
            if let Some(checkpoint) = nits_protocol::ReviewCheckpoint::from_event(event) {
                t.checkpoints.insert(
                    (
                        checkpoint.review_id.to_string().as_str(),
                        checkpoint.id.event_seq().get(),
                    ),
                    serde_json::to_vec(&checkpoint)?.as_slice(),
                )?;
            }
        }
        EventBody::SuggestionApplied { .. } => {}
    }
    Ok(())
}

fn load_review(t: &Write<'_>, seq: Seq, id: ReviewId) -> Result<ReviewRecord, StoreError> {
    get(t.reviews.get(id.to_string().as_str())?)?
        .ok_or_else(|| inconsistent(seq, format!("unknown review {id}")))
}

fn save_review(t: &mut Write<'_>, rec: &ReviewRecord) -> Result<(), StoreError> {
    t.reviews.insert(
        rec.review.id.to_string().as_str(),
        serde_json::to_vec(rec)?.as_slice(),
    )?;
    Ok(())
}

fn load_comment(
    t: &Write<'_>,
    seq: Seq,
    review: ReviewId,
    id: nits_protocol::CommentId,
) -> Result<nits_protocol::Comment, StoreError> {
    get(t
        .comments
        .get((review.to_string().as_str(), id.to_string().as_str()))?)?
    .ok_or_else(|| inconsistent(seq, format!("unknown comment {id} in review {review}")))
}

fn save_comment(t: &mut Write<'_>, c: &nits_protocol::Comment) -> Result<(), StoreError> {
    t.comments.insert(
        (c.review_id.to_string().as_str(), c.id.to_string().as_str()),
        serde_json::to_vec(c)?.as_slice(),
    )?;
    Ok(())
}

fn load_thread(
    t: &Write<'_>,
    seq: Seq,
    review: ReviewId,
    id: nits_protocol::ThreadId,
) -> Result<Thread, StoreError> {
    get(t
        .threads
        .get((review.to_string().as_str(), id.to_string().as_str()))?)?
    .ok_or_else(|| inconsistent(seq, format!("unknown thread {id} in review {review}")))
}

fn save_thread(t: &mut Write<'_>, th: &Thread) -> Result<(), StoreError> {
    t.threads.insert(
        (
            th.review_id.to_string().as_str(),
            th.id.to_string().as_str(),
        ),
        serde_json::to_vec(th)?.as_slice(),
    )?;
    Ok(())
}
