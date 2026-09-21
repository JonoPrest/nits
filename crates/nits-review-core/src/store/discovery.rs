//! One store snapshot for cross-workspace discovery; no Git access or N+1 RPCs.

use std::collections::{BTreeMap, BTreeSet};

use nits_protocol::{
    Comment, CommentState, PendingReviewRequest, Review, ReviewActivity, ReviewCheckpoint,
    ReviewDiscovery, ReviewId, ReviewQuery, ReviewRequest, ReviewRound, ReviewScope, ReviewSummary,
    ReviewerIdentity, Seq, Thread, ThreadResolution, Workspace,
};
use redb::{ReadableDatabase, ReadableTable};

use super::{ReviewLifecycle, ReviewRecord, Store, StoreError, StoredEvent, tables};

impl Store {
    /// `None` means the explicitly selected workspace does not exist.
    pub fn discover_reviews(
        &self,
        query: &ReviewQuery,
    ) -> Result<Option<ReviewDiscovery>, StoreError> {
        let txn = self.db.begin_read()?;
        let workspace_table = txn.open_table(tables::WORKSPACES)?;
        let workspaces = workspace_table
            .iter()?
            .map(|row| {
                let workspace: Workspace = serde_json::from_slice(row?.1.value())?;
                Ok((workspace.id, workspace))
            })
            .collect::<Result<BTreeMap<_, _>, StoreError>>()?;
        if let ReviewScope::Workspace { workspace_id } = query.scope
            && !workspaces.contains_key(&workspace_id)
        {
            return Ok(None);
        }
        let selected = select_reviews(&txn, query)?;
        let seq = txn
            .open_table(tables::META)?
            .get(tables::META_VIEW_SEQ)?
            .map_or(Seq::new(0), |value| Seq::new(value.value()));
        let mut activity = latest_activity(&txn, &selected)?;
        let mut reviews = Vec::new();
        for (id, review) in selected {
            let (open_findings, pending_requests) = discussion_summary(&txn, id, seq)?;
            if query.awaiting.as_ref().is_some_and(|name| {
                !pending_requests
                    .iter()
                    .any(|request| &request.recipient == name)
            }) {
                continue;
            }
            let workspace =
                workspaces
                    .get(&review.workspace_id)
                    .ok_or_else(|| StoreError::Corrupt {
                        seq,
                        reason: format!("review {id} has no workspace {}", review.workspace_id),
                    })?;
            let last_activity = activity.remove(&id).ok_or_else(|| StoreError::Corrupt {
                seq,
                reason: format!("review {id} has no committed activity"),
            })?;
            let repositories = workspace
                .repos
                .iter()
                .filter(|repo| {
                    review
                        .targets
                        .iter()
                        .any(|target| target.repo_id == repo.id)
                })
                .cloned()
                .collect();
            reviews.push(ReviewSummary {
                id,
                workspace_id: review.workspace_id,
                title: review.title,
                targets: review.targets,
                created: review.created,
                status: review.status,
                workspace_name: workspace.name.clone(),
                repositories,
                open_findings,
                pending_requests,
                last_activity,
            });
        }
        reviews.sort_by(|a, b| {
            b.last_activity
                .seq
                .cmp(&a.last_activity.seq)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(Some(ReviewDiscovery { reviews, seq }))
    }
}

fn select_reviews(
    txn: &redb::ReadTransaction,
    query: &ReviewQuery,
) -> Result<BTreeMap<ReviewId, Review>, StoreError> {
    let title = query.title.as_ref().map(|title| title.to_lowercase());
    let mut selected = BTreeMap::new();
    for row in txn.open_table(tables::REVIEWS)?.iter()? {
        let record: ReviewRecord = serde_json::from_slice(row?.1.value())?;
        let in_scope = match query.scope {
            ReviewScope::All {} => true,
            ReviewScope::Workspace { workspace_id } => record.review.workspace_id == workspace_id,
        };
        if matches!(record.lifecycle, ReviewLifecycle::Live)
            && in_scope
            && title
                .as_ref()
                .is_none_or(|needle| record.review.title.to_lowercase().contains(needle))
        {
            selected.insert(record.review.id, record.review);
        }
    }
    Ok(selected)
}

fn latest_activity(
    txn: &redb::ReadTransaction,
    selected: &BTreeMap<ReviewId, Review>,
) -> Result<BTreeMap<ReviewId, ReviewActivity>, StoreError> {
    let mut activity = BTreeMap::new();
    // Stop once each selected review's latest event is found. Old histories
    // need not be copied into memory or scanned once for every review.
    if !selected.is_empty() {
        for row in txn.open_table(tables::EVENTS)?.iter()?.rev() {
            let stored: StoredEvent = serde_json::from_slice(row?.1.value())?;
            if let Some(id) = stored.event.body.review_id()
                && selected.contains_key(&id)
            {
                activity.entry(id).or_insert(ReviewActivity {
                    seq: stored.event.seq,
                    at: stored.event.ts,
                });
                if activity.len() == selected.len() {
                    break;
                }
            }
        }
    }
    Ok(activity)
}

fn discussion_summary(
    txn: &redb::ReadTransaction,
    id: ReviewId,
    seq: Seq,
) -> Result<(u32, Vec<PendingReviewRequest>), StoreError> {
    let threads = txn.open_table(tables::THREADS)?;
    let comments = txn.open_table(tables::COMMENTS)?;
    let requests = txn.open_table(tables::REVIEW_REQUESTS)?;
    let checkpoints = txn.open_table(tables::CHECKPOINTS)?;
    let key = id.to_string();
    let mut open_findings: u32 = 0;
    for row in threads.range((key.as_str(), "")..(key.as_str(), "\u{10FFFF}"))? {
        let thread: Thread = serde_json::from_slice(row?.1.value())?;
        if matches!(thread.resolution, ThreadResolution::Open)
            && let Some(root) = comments.get((key.as_str(), thread.root.to_string().as_str()))?
        {
            let root: Comment = serde_json::from_slice(root.value())?;
            if !matches!(root.state, CommentState::Deleted) {
                open_findings =
                    open_findings
                        .checked_add(1)
                        .ok_or_else(|| StoreError::Corrupt {
                            seq,
                            reason: format!("review {id} has too many open findings"),
                        })?;
            }
        }
    }
    let mut answered = BTreeSet::new();
    for row in checkpoints.range((key.as_str(), 0)..=(key.as_str(), u64::MAX))? {
        let checkpoint: ReviewCheckpoint = serde_json::from_slice(row?.1.value())?;
        if let (ReviewerIdentity::Agent { name }, Some(ReviewRound::Request { request_id })) =
            (checkpoint.reviewer, checkpoint.in_reply_to)
        {
            answered.insert((request_id, name));
        }
    }
    let mut pending_requests = Vec::new();
    for row in requests.range((key.as_str(), 0)..=(key.as_str(), u64::MAX))? {
        let request: ReviewRequest = serde_json::from_slice(row?.1.value())?;
        if !answered.contains(&(request.id, request.recipient.clone())) {
            pending_requests.push(PendingReviewRequest {
                id: request.id,
                recipient: request.recipient,
                created: request.created,
            });
        }
    }
    Ok((open_findings, pending_requests))
}
