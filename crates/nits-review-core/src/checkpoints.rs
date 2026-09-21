//! Revision checkpoints are independent of finding and human viewed lifecycles.
use crate::{Core, CoreError, Ctx};
use nits_protocol::{
    EventBody, NonEmpty, RefSpec, ResolvedSource, ResolvedTarget, ReviewCheckpointId, ReviewId,
    ReviewRound, ReviewerIdentity,
};
impl Core {
    pub(crate) fn retain_targets(
        &self,
        review: ReviewId,
        targets: &NonEmpty<ResolvedTarget>,
    ) -> Result<(), CoreError> {
        for target in targets {
            let repo = self.review_repo(review, target.repo_id)?;
            repo.retain_revision(review, &target.base)?;
            repo.retain_revision(review, &target.head)?;
        }
        Ok(())
    }

    /// Record exactly the supplied immutable revision identities, even after the
    /// current review moved. A checkpoint never changes the review's base or threads.
    pub fn record_checkpoint(
        &self,
        ctx: &Ctx,
        review_id: ReviewId,
        targets: NonEmpty<ResolvedTarget>,
        in_reply_to: Option<ReviewRound>,
    ) -> Result<ReviewCheckpointId, CoreError> {
        let snapshot = self.review_snapshot(review_id)?;
        let reviewer = ReviewerIdentity::from_author(&ctx.author)
            .ok_or_else(|| CoreError::forbidden("daemon cannot act as a reviewer"))?;
        let repos = targets
            .iter()
            .map(|target| target.repo_id)
            .collect::<std::collections::BTreeSet<_>>();
        let expected = snapshot
            .review
            .targets
            .iter()
            .map(|target| target.repo_id)
            .collect::<std::collections::BTreeSet<_>>();
        if repos != expected || repos.len() != targets.len() {
            return Err(CoreError::invalid(
                "check must cover each review repository exactly once",
            ));
        }
        validate_round(&snapshot, in_reply_to)?;
        for target in &targets {
            let repo = self.review_repo(review_id, target.repo_id)?;
            for revision in [&target.base, &target.head] {
                match &revision.source {
                    ResolvedSource::Commit { oid } => {
                        if repo.resolve(&RefSpec::Commit { oid: *oid })?.tree != revision.tree {
                            return Err(CoreError::invalid("commit and tree identities disagree"));
                        }
                    }
                    ResolvedSource::WorkingTree { .. } => {
                        // Only daemon-captured working trees are valid provenance. A
                        // caller cannot relabel an arbitrary git tree as a past checkout.
                        let known = snapshot
                            .resolved
                            .iter()
                            .flat_map(|ts| ts.iter())
                            .chain(
                                snapshot
                                    .requests
                                    .iter()
                                    .filter_map(|request| match &request.targets {
                                        nits_protocol::RequestedTargets::Unknown => None,
                                        nits_protocol::RequestedTargets::Captured { targets } => {
                                            Some(targets)
                                        }
                                    })
                                    .flat_map(|ts| ts.iter()),
                            )
                            .chain(
                                snapshot
                                    .checkpoints
                                    .iter()
                                    .flat_map(|checkpoint| checkpoint.targets.iter()),
                            )
                            .any(|known| {
                                known.repo_id == target.repo_id
                                    && (&known.base == revision || &known.head == revision)
                            });
                        let historical = if known {
                            false
                        } else {
                            self.store.events_after(None)?.iter().any(|event| {
                                if let EventBody::ReviewTargetsResolved {
                                    review_id: captured_review,
                                    targets,
                                } = &event.body
                                {
                                    *captured_review == review_id
                                        && targets.iter().any(|known| {
                                            known.repo_id == target.repo_id
                                                && (&known.base == revision
                                                    || &known.head == revision)
                                        })
                                } else {
                                    false
                                }
                            })
                        };
                        if !known && !historical {
                            return Err(CoreError::invalid(
                                "working-tree revision was not captured by this review",
                            ));
                        }
                    }
                }
            }
        }
        self.retain_targets(review_id, &targets)?;
        let event = self.append(
            ctx,
            EventBody::ReviewChecked {
                review_id,
                reviewer,
                targets,
                in_reply_to,
            },
        )?;
        Ok(ReviewCheckpointId::from_event_seq(event.seq))
    }
}

fn validate_round(
    snapshot: &nits_protocol::ReviewSnapshot,
    round: Option<ReviewRound>,
) -> Result<(), CoreError> {
    let belongs = match round {
        Some(ReviewRound::Request { request_id }) => snapshot
            .requests
            .iter()
            .any(|request| request.id == request_id),
        Some(ReviewRound::Checkpoint { checkpoint_id }) => snapshot
            .checkpoints
            .iter()
            .any(|checkpoint| checkpoint.id == checkpoint_id),
        None => true,
    };
    if belongs {
        Ok(())
    } else {
        Err(CoreError::invalid(
            "request/checkpoint does not belong to this review",
        ))
    }
}
