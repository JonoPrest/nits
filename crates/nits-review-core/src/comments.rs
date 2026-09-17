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
                self.repo(repo_id)?.blob(blob_oid)?;
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
                let bytes = self.repo(repo_id)?.blob(blob_oid)?;
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
        context: Option<ChangeKind>,
    ) -> Result<Comment, CoreError> {
        if self.store.comment(review, id)?.is_some() {
            return Err(CoreError::invalid(format!("comment {id} already exists")));
        }
        if matches!(kind, CommentKind::Informational) && !matches!(anchor, Anchor::Review) {
            return Err(CoreError::invalid(
                "informational notes require a review-level anchor",
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
        self.review(review)?;
        if agent.trim().is_empty() {
            return Err(CoreError::invalid("agent name must not be empty"));
        }
        let event = self.append(
            ctx,
            EventBody::ReviewRequested {
                review_id: review,
                agent,
                note,
            },
        )?;
        Ok(nits_protocol::ReviewRequestId::from_event_seq(event.seq))
    }

    /// Apply a suggestion's patch to the working tree. The file must still
    /// be at the blob the suggestion was made against.
    pub fn apply_suggestion(
        &self,
        ctx: &Ctx,
        review: ReviewId,
        id: CommentId,
    ) -> Result<BlobOid, CoreError> {
        let c = self.comment(review, id)?;
        let CommentKind::Suggestion { patch } = &c.kind else {
            return Err(CoreError::invalid(format!(
                "comment {id} is not a suggestion"
            )));
        };
        let (repo_id, path, blob_oid) = match effective_anchor(&c.anchor, &c.state) {
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
        let repo = self.repo(repo_id)?;
        let file = repo.workdir().join(path.as_str());
        let current = std::fs::read(&file)?;
        let expected = repo.blob(blob_oid)?;
        if current != expected {
            return Err(CoreError::invalid(format!(
                "{path} has changed since the suggestion was made"
            )));
        }
        let patched = crate::patch::apply(&current, patch)
            .map_err(|e| CoreError::invalid(format!("patch does not apply: {e}")))?;
        std::fs::write(&file, &patched)?;
        let result_blob = repo.hash_blob(&patched)?;
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
            let repo = self.repo(repo_id)?;
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
                    e.insert(self.path_fates(repo_id, old_tree, new_tree)?)
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
        &self,
        repo_id: RepoId,
        old_tree: Option<nits_protocol::TreeOid>,
        new_tree: nits_protocol::TreeOid,
    ) -> Result<HashMap<RepoPath, PathFate>, CoreError> {
        let repo = self.repo(repo_id)?;
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
                                blob: new,
                            },
                        );
                    }
                    ChangeKind::Deleted { .. } => {
                        fates.insert(change.path, PathFate::Gone);
                    }
                    ChangeKind::Added { .. } | ChangeKind::Modified { .. } => {}
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
