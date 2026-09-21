//! Directory bootstrap runs beside git and under the daemon's writer queue.

use crate::git::Repo;
use crate::{Core, CoreError, Ctx};
use nits_protocol::{
    DirectoryReview, DirectoryReviewOutcome, EnsureDirectoryReview, NonEmpty, RefSpec,
    ReviewStatus, ReviewTarget,
};
use std::path::Path;

impl Core {
    /// Find or create an open review of a checkout. Reuse requires every
    /// explicit ref to match; an omitted base preserves an existing base.
    /// Paths and refs are resolved before creating any persistent state.
    /// Callers serialize this operation with other writes.
    pub fn ensure_directory_review(
        &self,
        ctx: &Ctx,
        options: EnsureDirectoryReview,
    ) -> Result<DirectoryReview, CoreError> {
        let root = std::fs::canonicalize(&options.path)
            .map_err(|error| CoreError::invalid(format!("{}: {error}", options.path)))?;
        let root = root
            .ancestors()
            .find(|path| path.join(".git").exists())
            .ok_or_else(|| CoreError::invalid("path is not inside a git working tree"))?;
        let git = Repo::open(root).map_err(|error| CoreError::invalid(error.to_string()))?;
        let located = self.directory_attachment(root)?;
        let head = options.head.unwrap_or(RefSpec::WorkingTree);
        let requested_base = options.base.map(RefSpec::from);
        // An explicit missing ref must fail even if a stale review still names it.
        git.resolve(&head)
            .map_err(|error| CoreError::invalid(error.to_string()))?;
        if let Some(base) = &requested_base {
            git.resolve(base)
                .map_err(|error| CoreError::invalid(error.to_string()))?;
        }
        if let Some((workspace_id, repo_id)) = located
            && let Some(mut review) = self.matching_directory_review(
                workspace_id,
                repo_id,
                requested_base.as_ref(),
                &head,
            )?
        {
            self.resolve_targets(ctx, review.review_id)?;
            review.seq = self
                .last_seq()?
                .ok_or_else(|| CoreError::invalid("review has no log position"))?;
            return Ok(review);
        }
        let base = match requested_base {
            Some(base) => base,
            None => git.default_base()?,
        };
        git.resolve(&base)
            .map_err(|error| CoreError::invalid(error.to_string()))?;
        if self.store.review(options.review_id)?.is_some() {
            return Err(CoreError::invalid(format!(
                "review {} already exists",
                options.review_id
            )));
        }
        let before = self.last_seq()?;
        let name = root
            .file_name()
            .map_or_else(|| "repo".into(), |name| name.to_string_lossy().into_owned());
        let (workspace_id, repo_id) = if let Some(ids) = located {
            ids
        } else {
            self.create_workspace_with_repo(
                ctx,
                options.workspace_id,
                options.repo_id,
                root,
                name.clone(),
            )?;
            (options.workspace_id, options.repo_id)
        };
        self.create_review(
            ctx,
            options.review_id,
            workspace_id,
            name,
            NonEmpty::singleton(ReviewTarget {
                repo_id,
                base: base.clone(),
                head: head.clone(),
            }),
        )?;
        let seq = self
            .events_after(before)?
            .first()
            .map(|event| event.seq)
            .ok_or_else(|| CoreError::invalid("bootstrap committed no event"))?;
        Ok(DirectoryReview {
            workspace_id,
            repo_id,
            review_id: options.review_id,
            base,
            head,
            outcome: DirectoryReviewOutcome::Created,
            seq,
        })
    }

    /// Distinguish legacy duplicates in one workspace from intentional sharing.
    fn directory_attachment(
        &self,
        root: &Path,
    ) -> Result<Option<(nits_protocol::WorkspaceId, nits_protocol::RepoId)>, CoreError> {
        let workspaces = self.workspaces()?;
        let matches: Vec<_> = workspaces
            .iter()
            .flat_map(|workspace| {
                workspace
                    .repos
                    .iter()
                    .filter(|repo| Path::new(&repo.path) == root)
                    .map(move |repo| (workspace.id, repo.id))
            })
            .collect();
        if let Some((workspace, _)) = matches.first()
            && matches.len() > 1
            && matches.iter().all(|(id, _)| id == workspace)
        {
            return Err(CoreError::invalid(format!(
                "checkout has multiple repository attachments in workspace {workspace}; detach duplicate memberships before opening this directory"
            )));
        }
        let mut matches = matches.into_iter();
        let located = matches.next();
        if matches.next().is_some() {
            return Err(CoreError::invalid(
                "checkout is attached in several workspaces; use create_review with explicit workspace_id and repo_id",
            ));
        }
        Ok(located)
    }

    fn matching_directory_review(
        &self,
        workspace_id: nits_protocol::WorkspaceId,
        repo_id: nits_protocol::RepoId,
        base: Option<&RefSpec>,
        head: &RefSpec,
    ) -> Result<Option<DirectoryReview>, CoreError> {
        for review in self.reviews(workspace_id)? {
            if review.status != ReviewStatus::Open {
                continue;
            }
            if let Some(target) = review.targets.iter().find(|target| {
                target.repo_id == repo_id
                    && target.head == *head
                    && base.is_none_or(|base| *base == target.base)
            }) {
                return Ok(Some(DirectoryReview {
                    workspace_id,
                    repo_id,
                    review_id: review.id,
                    base: target.base.clone(),
                    head: head.clone(),
                    outcome: DirectoryReviewOutcome::Reused,
                    seq: self
                        .last_seq()?
                        .ok_or_else(|| CoreError::invalid("review has no log position"))?,
                }));
            }
        }
        Ok(None)
    }
}
