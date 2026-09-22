//! Conversation filtering uses the snapshot and event cursor from one read transaction.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound::{Excluded, Included};

use nits_protocol::{
    Anchor, Author, Comment, CommentId, CommentListing, CommentQuery, CommentState, CommentSummary,
    CommentThread, CommentThreadStatus, EventBody, NonEmpty, ReviewId, Seq, ThreadId,
    ThreadResolution,
};
use redb::ReadableDatabase;

use super::{Store, StoreError, StoredEvent, tables};

impl Store {
    pub fn list_comments(
        &self,
        review_id: ReviewId,
        query: &CommentQuery,
    ) -> Result<Option<CommentListing>, StoreError> {
        let txn = self.db.begin_read()?;
        let Some(snapshot) = Self::snapshot_in_transaction(&txn, review_id)? else {
            return Ok(None);
        };
        let seq = snapshot.seq;
        let comments: BTreeMap<_, _> = snapshot.comments.into_iter().map(|c| (c.id, c)).collect();
        let changed = query
            .since
            .map(|since| changed_threads(&txn, review_id, since, seq, &comments))
            .transpose()?;
        let mut threads = Vec::new();
        let mut summary = CommentSummary::default();
        let mut selected_comments = BTreeSet::new();
        for thread in snapshot.threads {
            if query.thread_id.is_some_and(|id| id != thread.id)
                || changed
                    .as_ref()
                    .is_some_and(|ids| !ids.contains(&thread.id))
            {
                continue;
            }
            let Some(joined) = join_thread(thread, &comments, query, seq)? else {
                continue;
            };
            for comment in &joined.comments {
                selected_comments.insert(comment.id);
            }
            count_thread(&mut summary, &joined, seq)?;
            threads.push(joined);
        }
        Ok(Some(CommentListing {
            threads,
            summary,
            suggestions: snapshot
                .suggestions
                .into_iter()
                .filter(|s| selected_comments.contains(&s.comment_id))
                .collect(),
            latest_checkpoints: nits_protocol::latest_checkpoints(
                &snapshot.checkpoints,
                snapshot.resolved.as_ref(),
            ),
            requests: snapshot.requests,
            checkpoints: snapshot.checkpoints,
            seq,
        }))
    }
}

fn join_thread(
    thread: nits_protocol::Thread,
    comments: &BTreeMap<CommentId, Comment>,
    query: &CommentQuery,
    seq: Seq,
) -> Result<Option<CommentThread>, StoreError> {
    let root = comments
        .get(&thread.root)
        .ok_or_else(|| StoreError::Corrupt {
            seq,
            reason: format!("thread {} has no root {}", thread.id, thread.root),
        })?;
    let status = if matches!(root.state, CommentState::Deleted) {
        CommentThreadStatus::Deleted
    } else {
        match thread.resolution {
            ThreadResolution::Open => CommentThreadStatus::Open,
            ThreadResolution::Resolved { .. } => CommentThreadStatus::Resolved,
            ThreadResolution::Deferred { .. } => CommentThreadStatus::Deferred,
            ThreadResolution::Informational => CommentThreadStatus::Informational,
        }
    };
    if query.status.is_some_and(|wanted| wanted != status) {
        return Ok(None);
    }
    let joined = std::iter::once(&thread.root)
        .chain(&thread.replies)
        .map(|id| {
            comments
                .get(id)
                .cloned()
                .ok_or_else(|| StoreError::Corrupt {
                    seq,
                    reason: format!("thread {} has no comment {id}", thread.id),
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !joined
        .iter()
        .any(|comment| matches_anchor(&comment.anchor, query))
        || query.author.as_ref().is_some_and(|name| {
            !joined.iter().any(|comment| match &comment.author {
                Author::Human { name: author, .. } | Author::Agent { name: author, .. } => {
                    author == name
                }
                Author::Daemon { .. } => name == "daemon",
            })
        })
    {
        return Ok(None);
    }
    Ok(Some(CommentThread {
        id: thread.id,
        review_id: thread.review_id,
        root: thread.root,
        replies: thread.replies,
        resolution: thread.resolution,
        status,
        comments: NonEmpty::new(joined).map_err(|error| StoreError::Corrupt {
            seq,
            reason: error.to_string(),
        })?,
    }))
}

fn count_thread(
    summary: &mut CommentSummary,
    thread: &CommentThread,
    seq: Seq,
) -> Result<(), StoreError> {
    increment(&mut summary.threads, seq)?;
    increment(
        match thread.status {
            CommentThreadStatus::Open => &mut summary.open,
            CommentThreadStatus::Resolved => &mut summary.resolved,
            CommentThreadStatus::Deferred => &mut summary.deferred,
            CommentThreadStatus::Informational => &mut summary.informational,
            CommentThreadStatus::Deleted => &mut summary.deleted,
        },
        seq,
    )?;
    for comment in &thread.comments {
        increment(&mut summary.comments, seq)?;
        if matches!(comment.state, CommentState::Deleted) {
            increment(&mut summary.deleted_comments, seq)?;
        }
    }
    Ok(())
}

fn increment(count: &mut u32, seq: Seq) -> Result<(), StoreError> {
    *count = count.checked_add(1).ok_or_else(|| StoreError::Corrupt {
        seq,
        reason: "comment query count exceeds u32".into(),
    })?;
    Ok(())
}

fn matches_anchor(anchor: &Anchor, query: &CommentQuery) -> bool {
    match anchor {
        Anchor::Review => query.path.is_none() && query.repo_id.is_none(),
        Anchor::File { repo_id, path, .. } | Anchor::Lines { repo_id, path, .. } => {
            query.path.as_ref().is_none_or(|wanted| wanted == path)
                && query.repo_id.is_none_or(|wanted| wanted == *repo_id)
        }
    }
}

fn changed_threads(
    txn: &redb::ReadTransaction,
    review_id: ReviewId,
    since: Seq,
    through: Seq,
    comments: &BTreeMap<CommentId, Comment>,
) -> Result<BTreeSet<ThreadId>, StoreError> {
    let mut changed = BTreeSet::new();
    if since >= through {
        return Ok(changed);
    }
    for row in txn
        .open_table(tables::EVENTS)?
        .range((Excluded(since.get()), Included(through.get())))?
    {
        let stored: StoredEvent = serde_json::from_slice(row?.1.value())?;
        if stored.event.body.review_id() != Some(review_id) {
            continue;
        }
        let thread = match stored.event.body {
            EventBody::CommentCreated { comment } => Some(comment.thread_id),
            EventBody::CommentEdited { comment_id, .. }
            | EventBody::CommentDeleted { comment_id, .. }
            | EventBody::CommentReanchored { comment_id, .. }
            | EventBody::SuggestionApplied { comment_id, .. } => {
                comments.get(&comment_id).map(|c| c.thread_id)
            }
            EventBody::ThreadDeferred { thread_id, .. }
            | EventBody::ThreadResolved { thread_id, .. }
            | EventBody::ThreadUnresolved { thread_id, .. } => Some(thread_id),
            EventBody::WorkspaceCreated { .. }
            | EventBody::WorkspaceUpdated { .. }
            | EventBody::RepoAttached { .. }
            | EventBody::RepoDetached { .. }
            | EventBody::ReviewCreated { .. }
            | EventBody::ReviewUpdated { .. }
            | EventBody::ReviewTargetUpdated { .. }
            | EventBody::ReviewDeleted { .. }
            | EventBody::ReviewTargetsResolved { .. }
            | EventBody::FileViewed { .. }
            | EventBody::FileUnviewed { .. }
            | EventBody::ReviewRequested { .. }
            | EventBody::ReviewChecked { .. } => None,
        };
        if let Some(id) = thread {
            changed.insert(id);
        }
    }
    Ok(changed)
}
