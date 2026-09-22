//! Comment mutations and the re-anchoring driver on `Core`.

use std::collections::HashMap;

use nits_protocol::{
    Anchor, BlobOid, ChangeKind, Comment, CommentId, CommentKind, CommentState, EntityKind,
    EventBody, LineNo, LineRange, NonEmpty, RepoId, RepoPath, ResolvedTarget, ReviewId, Side,
    Thread, ThreadId, ThreadResolution, TreeEntryKind,
};

use crate::anchor::{PathFate, Reanchor, context_hash, effective_anchor, reanchor, side_of};
use crate::core::{Core, CoreError, Ctx};
use crate::render::lines_of;

impl Core {
    pub fn list_comments(
        &self,
        review: ReviewId,
        query: &nits_protocol::CommentQuery,
    ) -> Result<nits_protocol::CommentListing, CoreError> {
        self.store
            .list_comments(review, query)?
            .ok_or_else(|| CoreError::not_found(EntityKind::Review, &review))
    }

    pub fn comments(&self, review: ReviewId) -> Result<Vec<Comment>, CoreError> {
        self.review(review)?;
        Ok(self.store.comments(review)?)
    }

    pub fn threads(&self, review: ReviewId) -> Result<Vec<Thread>, CoreError> {
        self.review(review)?;
        Ok(self.store.threads(review)?)
    }

    fn comment(&self, review: ReviewId, id: CommentId) -> Result<Comment, CoreError> {
        self.store
            .comment(review, id)?
            .filter(|c| !matches!(c.state, CommentState::Deleted))
            .ok_or_else(|| CoreError::not_found(EntityKind::Comment, &id))
    }

    fn thread(&self, review: ReviewId, id: ThreadId) -> Result<Thread, CoreError> {
        self.store
            .threads(review)?
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| CoreError::not_found(EntityKind::Thread, &id))
    }

    /// Validate an anchor against the review and stamp the context hash
    /// (the daemon is the authority for it; a client-supplied value is
    /// replaced).
    fn validate_anchor(&self, review: ReviewId, anchor: Anchor) -> Result<Anchor, CoreError> {
        let rec = self.review(review)?;
        match anchor {
            Anchor::Review => Ok(Anchor::Review),
            Anchor::File {
                repo_id,
                path,
                blob_oid,
            } => {
                if !rec.review.targets.iter().any(|t| t.repo_id == repo_id) {
                    return Err(CoreError::invalid(format!(
                        "repo {repo_id} is not in review {review}"
                    )));
                }
                self.review_repo(review, repo_id)?.blob(blob_oid)?;
                Ok(Anchor::File {
                    repo_id,
                    path,
                    blob_oid,
                })
            }
            Anchor::Lines {
                repo_id,
                path,
                side,
                blob_oid,
                lines,
                context_hash: _,
            } => {
                if !rec.review.targets.iter().any(|t| t.repo_id == repo_id) {
                    return Err(CoreError::invalid(format!(
                        "repo {repo_id} is not in review {review}"
                    )));
                }
                let bytes = self.review_repo(review, repo_id)?.blob(blob_oid)?;
                let text = lines_of(&bytes);
                if lines.end().index() as usize >= text.len() {
                    return Err(CoreError::invalid(format!(
                        "line range {}-{} exceeds blob length {}",
                        lines.start(),
                        lines.end(),
                        text.len()
                    )));
                }
                Ok(Anchor::Lines {
                    repo_id,
                    path,
                    side,
                    blob_oid,
                    lines,
                    context_hash: context_hash(&text, lines),
                })
            }
        }
    }

    /// Start a new thread. `id` doubles as the thread id.
    // A wire mutation's fields, passed through one by one.
    #[allow(clippy::too_many_arguments)]
    pub fn add_comment(
        &self,
        ctx: &Ctx,
        review: ReviewId,
        id: CommentId,
        kind: CommentKind,
        anchor: Anchor,
        body: String,
        context: Option<nits_protocol::CommentContext>,
    ) -> Result<Comment, CoreError> {
        if self.store.comment(review, id)?.is_some() {
            return Err(CoreError::invalid(format!("comment {id} already exists")));
        }
        if matches!(kind, CommentKind::Informational) && !matches!(anchor, Anchor::Review) {
            return Err(CoreError::invalid(
                "informational notes require a review-level anchor",
            ));
        }
        if matches!(context, Some(nits_protocol::CommentContext::Browse { .. }))
            && matches!(
                anchor,
                Anchor::Review
                    | Anchor::Lines {
                        side: Side::Base,
                        ..
                    }
            )
        {
            return Err(CoreError::invalid(
                "Browse comments require a file or head-side line anchor",
            ));
        }
        let anchor = self.validate_anchor(review, anchor)?;
        let comment = Comment {
            id,
            review_id: review,
            thread_id: thread_id_of(id),
            author: ctx.author.clone(),
            kind,
            anchor,
            body,
            created: ctx.now,
            edited: None,
            state: CommentState::Live,
            context,
        };
        self.append(
            ctx,
            EventBody::CommentCreated {
                comment: comment.clone(),
            },
        )?;
        Ok(comment)
    }

    /// Reply in an existing thread; inherits the root's anchor.
    pub fn reply(
        &self,
        ctx: &Ctx,
        review: ReviewId,
        thread: ThreadId,
        id: CommentId,
        kind: CommentKind,
        body: String,
    ) -> Result<Comment, CoreError> {
        if self.store.comment(review, id)?.is_some() {
            return Err(CoreError::invalid(format!("comment {id} already exists")));
        }
        let th = self.thread(review, thread)?;
        let root = self.comment(review, th.root)?;
        let comment = Comment {
            id,
            review_id: review,
            thread_id: thread,
            author: ctx.author.clone(),
            kind,
            anchor: root.anchor,
            body,
            created: ctx.now,
            edited: None,
            state: root.state,
            context: root.context,
        };
        self.append(
            ctx,
            EventBody::CommentCreated {
                comment: comment.clone(),
            },
        )?;
        Ok(comment)
    }

    /// Only the original author may edit.
    pub fn edit_comment(
        &self,
        ctx: &Ctx,
        review: ReviewId,
        id: CommentId,
        body: String,
    ) -> Result<(), CoreError> {
        let c = self.comment(review, id)?;
        if c.author != ctx.author {
            return Err(CoreError::forbidden("only the author may edit a comment"));
        }
        self.append(
            ctx,
            EventBody::CommentEdited {
                review_id: review,
                comment_id: id,
                body,
            },
        )?;
        Ok(())
    }

    /// Only the original author may delete.
    pub fn delete_comment(
        &self,
        ctx: &Ctx,
        review: ReviewId,
        id: CommentId,
    ) -> Result<(), CoreError> {
        let c = self.comment(review, id)?;
        if c.author != ctx.author {
            return Err(CoreError::forbidden("only the author may delete a comment"));
        }
        self.append(
            ctx,
            EventBody::CommentDeleted {
                review_id: review,
                comment_id: id,
            },
        )?;
        Ok(())
    }

    /// Record an unfixed finding outside the current scope. Reopen before
    /// changing a recorded deferral, so concurrent deferrals cannot overwrite it.
    pub fn defer_thread(
        &self,
        ctx: &Ctx,
        review: ReviewId,
        thread: ThreadId,
        reason: nits_protocol::DeferralReason,
        tracking_url: Option<nits_protocol::TrackingUrl>,
    ) -> Result<(), CoreError> {
        match self.thread(review, thread)?.resolution {
            ThreadResolution::Open => {}
            ThreadResolution::Informational => {
                return Err(CoreError::invalid(
                    "informational threads cannot be deferred",
                ));
            }
            ThreadResolution::Deferred { .. } | ThreadResolution::Resolved { .. } => {
                return Err(CoreError::invalid("reopen the finding before deferring it"));
            }
        }
        self.append(
            ctx,
            EventBody::ThreadDeferred {
                review_id: review,
                thread_id: thread,
                reason,
                tracking_url,
            },
        )?;
        Ok(())
    }

    pub fn resolve_thread(
        &self,
        ctx: &Ctx,
        review: ReviewId,
        thread: ThreadId,
    ) -> Result<(), CoreError> {
        let th = self.thread(review, thread)?;
        if matches!(th.resolution, ThreadResolution::Informational) {
            return Err(CoreError::invalid(
                "informational threads cannot be resolved or reopened",
            ));
        }
        if matches!(th.resolution, ThreadResolution::Resolved { .. }) {
            return Err(CoreError::invalid(format!(
                "thread {thread} is already resolved"
            )));
        }
        self.append(
            ctx,
            EventBody::ThreadResolved {
                review_id: review,
                thread_id: thread,
            },
        )?;
        Ok(())
    }

    pub fn unresolve_thread(
        &self,
        ctx: &Ctx,
        review: ReviewId,
        thread: ThreadId,
    ) -> Result<(), CoreError> {
        let th = self.thread(review, thread)?;
        if matches!(th.resolution, ThreadResolution::Informational) {
            return Err(CoreError::invalid(
                "informational threads cannot be resolved or reopened",
            ));
        }
        if matches!(th.resolution, ThreadResolution::Open) {
            return Err(CoreError::invalid(format!(
                "thread {thread} is not resolved"
            )));
        }
        self.append(
            ctx,
            EventBody::ThreadUnresolved {
                review_id: review,
                thread_id: thread,
            },
        )?;
        Ok(())
    }

    pub fn request_review(
        &self,
        ctx: &Ctx,
        review: ReviewId,
        agent: String,
        note: String,
    ) -> Result<nits_protocol::ReviewRequestId, CoreError> {
        if agent.trim().is_empty() {
            return Err(CoreError::invalid("agent name must not be empty"));
        }
        let record = self.review(review)?;
        let targets = self.resolve_review_targets(&record.review)?;
        self.retain_targets(review, &targets)?;
        let snapshot = self.review_snapshot(review)?;
        let checkpoint = snapshot
            .checkpoints
            .iter()
            .max_by_key(|checkpoint| checkpoint.id);
        let checkpoint_comparison =
            nits_protocol::RequestCheckpointComparison::against(checkpoint, &targets);
        let event = self.append(
            ctx,
            EventBody::ReviewRequested {
                review_id: review,
                agent,
                note,
                checkpoint_comparison,
                targets: nits_protocol::RequestedTargets::Captured {
                    targets: targets.clone(),
                },
            },
        )?;
        // Keep the request first for mutation acknowledgement/cursor ordering,
        // then publish this same resolution and reanchor through the normal path.
        self.record_resolved_targets(ctx, &record, &targets)?;
        Ok(nits_protocol::ReviewRequestId::from_event_seq(event.seq))
    }

    /// Apply a suggestion's patch to the working tree. The file must still
    /// be at the original blob. The immutable review/comment identity binds
    /// both this check and `PreviewSuggestion` to the same anchor and patch.
    pub fn apply_suggestion(
        &self,
        ctx: &Ctx,
        review: ReviewId,
        id: CommentId,
    ) -> Result<BlobOid, CoreError> {
        let suggestion = self.suggestion(review, id)?;
        match &suggestion.outcome {
            nits_protocol::SuggestionOutcome::Applied { receipt } => {
                return Err(CoreError::invalid(format!(
                    "suggestion {id} was already applied at event {}",
                    receipt.seq
                )));
            }
            nits_protocol::SuggestionOutcome::Unapplied => {}
        }
        let (repo_id, path, blob_oid) = match &suggestion.anchor {
            Anchor::Lines {
                repo_id,
                path,
                blob_oid,
                ..
            }
            | Anchor::File {
                repo_id,
                path,
                blob_oid,
            } => (*repo_id, path.clone(), *blob_oid),
            Anchor::Review => {
                return Err(CoreError::invalid(
                    "review-level suggestions cannot be applied",
                ));
            }
        };
        let repo = self.review_repo(review, repo_id)?;
        let mut file = crate::suggestion_file::WorkingFile::open(repo.workdir(), &path)?;
        let current = file.read()?;
        let expected = repo.blob(blob_oid)?;
        if current != expected {
            return Err(CoreError::invalid(format!(
                "{path} has changed since the suggestion was made"
            )));
        }
        let patched = crate::patch::apply(&current, &suggestion.patch)
            .map_err(|e| CoreError::invalid(format!("patch does not apply: {e}")))?;
        let result_blob = repo.hash_blob(&patched)?;
        file.replace(&repo.metadata_paths()?.worktree, &current, &patched)?;
        self.append(
            ctx,
            EventBody::SuggestionApplied {
                review_id: review,
                comment_id: id,
                repo_id,
                path,
                result_blob,
            },
        )?;
        Ok(result_blob)
    }

    fn suggestion(
        &self,
        review: ReviewId,
        id: CommentId,
    ) -> Result<nits_protocol::SuggestionRecord, CoreError> {
        self.review(review)?;
        self.comment(review, id)?;
        self.store
            .suggestion(review, id)?
            .ok_or_else(|| CoreError::invalid(format!("comment {id} is not a suggestion")))
    }

    /// Inspect the original anchored bytes and current checkout without any
    /// filesystem, object-store or event-log writes. Rejections retain the raw
    /// patch; a proposed-byte match never manufactures an application receipt.
    pub fn preview_suggestion(
        &self,
        review: ReviewId,
        id: CommentId,
    ) -> Result<nits_protocol::SuggestionPreview, CoreError> {
        use nits_protocol::{SuggestionInspection, SuggestionPreview, SuggestionWorktree};

        let suggestion = self.suggestion(review, id)?;
        let inspect = || -> Result<SuggestionInspection, CoreError> {
            let (repo_id, path, blob_oid) = match &suggestion.anchor {
                Anchor::File {
                    repo_id,
                    path,
                    blob_oid,
                }
                | Anchor::Lines {
                    repo_id,
                    path,
                    blob_oid,
                    ..
                } => (*repo_id, path, *blob_oid),
                Anchor::Review => {
                    return Err(CoreError::invalid(
                        "review-level suggestions cannot be applied",
                    ));
                }
            };
            let repo = self.review_repo(review, repo_id)?;
            let original = repo.blob(blob_oid)?;
            let preview = crate::patch::preview(&original, &suggestion.patch)
                .map_err(|error| CoreError::invalid(format!("patch does not apply: {error}")))?;
            let current = crate::suggestion_file::WorkingFile::open(repo.workdir(), path)
                .and_then(|mut file| file.read());
            let worktree = match current {
                Ok(bytes) if bytes == original => SuggestionWorktree::Original,
                Ok(bytes) if bytes == preview.result => SuggestionWorktree::Proposed,
                Ok(_) => SuggestionWorktree::Changed,
                Err(error) => SuggestionWorktree::Unavailable {
                    reason: error.to_string(),
                },
            };
            Ok(SuggestionInspection::Checked {
                hunks: preview.hunks,
                worktree,
            })
        };
        let inspection = inspect().unwrap_or_else(|error| SuggestionInspection::Rejected {
            reason: error.to_string(),
        });
        Ok(SuggestionPreview {
            suggestion,
            inspection,
        })
    }

    // ---- re-anchoring -------------------------------------------------------

    /// Re-anchor every comment of `review` after its targets moved from
    /// `old` to `new`, emitting one `CommentReanchored` per moved comment.
    pub(crate) fn reanchor_review(
        &self,
        ctx: &Ctx,
        review: ReviewId,
        old: &NonEmpty<ResolvedTarget>,
        new: &NonEmpty<ResolvedTarget>,
    ) -> Result<Vec<CommentId>, CoreError> {
        let mut fates: HashMap<(RepoId, Side), HashMap<RepoPath, PathFate>> = HashMap::new();
        let mut moved = Vec::new();
        for c in self.store.comments(review)? {
            if matches!(
                c.context,
                Some(nits_protocol::CommentContext::Browse { .. })
            ) {
                continue;
            }
            let base = effective_anchor(&c.anchor, &c.state);
            let Some(side) = side_of(base) else { continue };
            let (repo_id, path) = match base {
                Anchor::File { repo_id, path, .. } | Anchor::Lines { repo_id, path, .. } => {
                    (*repo_id, path.clone())
                }
                Anchor::Review => continue,
            };
            let Some(new_t) = new.iter().find(|t| t.repo_id == repo_id) else {
                continue;
            };
            let old_t = old.iter().find(|t| t.repo_id == repo_id);
            let repo = self.review_repo(review, repo_id)?;
            let fate_map = match fates.entry((repo_id, side)) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let new_tree = match side {
                        Side::Base => new_t.base.tree,
                        Side::Head => new_t.head.tree,
                    };
                    let old_tree = old_t.map(|t| match side {
                        Side::Base => t.base.tree,
                        Side::Head => t.head.tree,
                    });
                    e.insert(Self::path_fates(&repo, repo_id, old_tree, new_tree)?)
                }
            };
            let fate = fate_map.get(&path).cloned().unwrap_or(PathFate::Gone);
            let outcome = reanchor(
                &c.anchor,
                &c.state,
                &fate,
                |b| repo.blob(b).ok(),
                |b| repo.blob(b).ok(),
            );
            if let Reanchor::Moved { anchor, state } = outcome {
                self.append(
                    ctx,
                    EventBody::CommentReanchored {
                        review_id: review,
                        comment_id: c.id,
                        anchor,
                        state,
                    },
                )?;
                moved.push(c.id);
            }
        }
        Ok(moved)
    }

    /// For every path in `old_tree` (or all of `new_tree` when there is no
    /// old), where it is in `new_tree`: same path, renamed, or gone.
    fn path_fates(
        repo: &crate::git::Repo,
        repo_id: RepoId,
        old_tree: Option<nits_protocol::TreeOid>,
        new_tree: nits_protocol::TreeOid,
    ) -> Result<HashMap<RepoPath, PathFate>, CoreError> {
        let snap = repo.tree_snapshot(repo_id, new_tree)?;
        let mut fates: HashMap<RepoPath, PathFate> = snap
            .entries
            .into_iter()
            .filter_map(|e| match e.kind {
                TreeEntryKind::File { oid, .. } | TreeEntryKind::Symlink { oid } => Some((
                    e.path.clone(),
                    PathFate::Present {
                        path: e.path,
                        blob: oid,
                    },
                )),
                TreeEntryKind::Dir { .. } | TreeEntryKind::Submodule { .. } => None,
            })
            .collect();
        if let Some(old_tree) = old_tree {
            for change in repo.changed_files(old_tree, new_tree)? {
                match change.kind {
                    ChangeKind::Renamed { from, new, .. } => {
                        fates.insert(
                            from,
                            PathFate::Present {
                                path: change.path,
                                blob: new.oid,
                            },
                        );
                    }
                    ChangeKind::Deleted { .. } => {
                        fates.insert(change.path, PathFate::Gone);
                    }
                    ChangeKind::Added { .. } | ChangeKind::Modified { .. } => {}
                    ChangeKind::Submodule { change: submodule } => {
                        if submodule.new_blob().is_none() {
                            fates.insert(change.path, PathFate::Gone);
                        }
                    }
                }
            }
        }
        Ok(fates)
    }
}

/// A root comment's id is its thread's id.
#[must_use]
pub fn thread_id_of(id: CommentId) -> ThreadId {
    ThreadId::from_parts(id.timestamp_ms(), id.random())
}

/// Convenience for tests and clients: build a `Lines` anchor with a
/// placeholder hash (the daemon replaces it).
#[must_use]
pub fn lines_anchor(
    repo_id: RepoId,
    path: RepoPath,
    side: Side,
    blob_oid: BlobOid,
    start: u32,
    end: u32,
) -> Option<Anchor> {
    Some(Anchor::Lines {
        repo_id,
        path,
        side,
        blob_oid,
        lines: LineRange::new(LineNo::new(start)?, LineNo::new(end)?).ok()?,
        context_hash: nits_protocol::ContextHash::new(0),
    })
}
