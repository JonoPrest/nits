//! Workspaces, reviews, files, commits, renders and viewed marks on `Core`.

use nits_protocol::{
    BlobOid, ChangeKind, CommitInfo, CommitOid, DiffScope, EntityKind, EventBody, FileChange,
    FileRenderHeader, NonEmpty, RefCandidate, RefSpec, RenderOpts, RenderTarget, Repo, RepoId,
    RepoPath, ResolvedRef, ResolvedSource, ResolvedTarget, Review, ReviewId, ReviewSnapshot,
    ReviewStatus, ReviewTarget, ReviewTargetUpdate, TargetRevision, TreeDelta, TreeEntryKind,
    TreeSnapshot, ViewedMark, Workspace, WorkspaceId,
};

use crate::core::{Core, CoreError, Ctx};
use crate::render::cache::RenderKey;
use crate::render::{Rendered, render_blob, render_file};
use crate::store::{ReviewLifecycle, ReviewRecord};

/// Derived per-file viewed state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewedState {
    Unviewed,
    Viewed,
    /// Marked viewed at a different head blob than the current one.
    ChangedSinceViewed {
        marked: nits_protocol::ViewedContent,
    },
}

impl Core {
    // ---- workspaces -------------------------------------------------------

    pub fn workspaces(&self) -> Result<Vec<Workspace>, CoreError> {
        Ok(self.store.workspaces()?)
    }

    pub fn workspace(&self, id: WorkspaceId) -> Result<Workspace, CoreError> {
        self.store
            .workspace(id)?
            .ok_or_else(|| CoreError::not_found(EntityKind::Workspace, &id))
    }

    pub fn create_workspace(
        &self,
        ctx: &Ctx,
        id: WorkspaceId,
        name: String,
    ) -> Result<Workspace, CoreError> {
        if self.store.workspace(id)?.is_some() {
            return Err(CoreError::invalid(format!("workspace {id} already exists")));
        }
        if name.trim().is_empty() {
            return Err(CoreError::invalid("workspace name must not be empty"));
        }
        let workspace = Workspace {
            id,
            name,
            repos: vec![],
        };
        self.append(
            ctx,
            EventBody::WorkspaceCreated {
                workspace: workspace.clone(),
            },
        )?;
        Ok(workspace)
    }

    pub fn rename_workspace(
        &self,
        ctx: &Ctx,
        id: WorkspaceId,
        name: String,
    ) -> Result<(), CoreError> {
        self.workspace(id)?;
        self.append(
            ctx,
            EventBody::WorkspaceUpdated {
                workspace_id: id,
                name,
            },
        )?;
        Ok(())
    }

    /// Attach a repository; the path must be a git work tree.
    pub fn attach_repo(
        &self,
        ctx: &Ctx,
        workspace_id: WorkspaceId,
        repo_id: RepoId,
        path: &str,
        display_name: String,
    ) -> Result<Repo, CoreError> {
        let ws = self.workspace(workspace_id)?;
        if ws.repos.iter().any(|r| r.id == repo_id) {
            return Err(CoreError::invalid(format!(
                "repo {repo_id} already attached"
            )));
        }
        let canonical = std::fs::canonicalize(path)?;
        self.open_repo_at(repo_id, &canonical)?;
        let repo = Repo {
            id: repo_id,
            path: canonical.to_string_lossy().into_owned(),
            display_name,
        };
        self.append(
            ctx,
            EventBody::RepoAttached {
                workspace_id,
                repo: repo.clone(),
            },
        )?;
        Ok(repo)
    }

    pub fn detach_repo(
        &self,
        ctx: &Ctx,
        workspace_id: WorkspaceId,
        repo_id: RepoId,
    ) -> Result<(), CoreError> {
        let ws = self.workspace(workspace_id)?;
        if !ws.repos.iter().any(|r| r.id == repo_id) {
            return Err(CoreError::not_found(EntityKind::Repo, &repo_id));
        }
        self.append(
            ctx,
            EventBody::RepoDetached {
                workspace_id,
                repo_id,
            },
        )?;
        self.forget_repo(repo_id);
        Ok(())
    }

    // ---- reviews ----------------------------------------------------------

    pub fn reviews(&self, workspace_id: WorkspaceId) -> Result<Vec<Review>, CoreError> {
        Ok(self
            .store
            .reviews(workspace_id)?
            .into_iter()
            .map(|r| r.review)
            .collect())
    }

    /// Detect the base revision for a working-tree review in `repo_id`.
    pub fn default_base(&self, repo_id: RepoId) -> Result<RefSpec, CoreError> {
        Ok(self.repo(repo_id)?.default_base()?)
    }

    pub fn ref_candidates(&self, repo_id: RepoId) -> Result<Vec<RefCandidate>, CoreError> {
        Ok(self.repo(repo_id)?.ref_candidates()?)
    }

    /// A live review's record (review + resolved targets).
    pub fn review(&self, id: ReviewId) -> Result<ReviewRecord, CoreError> {
        let rec = self
            .store
            .review(id)?
            .ok_or_else(|| CoreError::not_found(EntityKind::Review, &id))?;
        match rec.lifecycle {
            ReviewLifecycle::Live => Ok(rec),
            ReviewLifecycle::Deleted { .. } => Err(CoreError::not_found(EntityKind::Review, &id)),
        }
    }

    /// The workspace retained in a stored review, including its tombstone.
    /// Event replay needs this association after deletion; live review APIs
    /// continue to reject deleted reviews through [`Self::review`].
    pub fn stored_review_workspace(&self, id: ReviewId) -> Result<WorkspaceId, CoreError> {
        self.store
            .review(id)?
            .map(|record| record.review.workspace_id)
            .ok_or_else(|| CoreError::not_found(EntityKind::Review, &id))
    }

    /// Create a review and resolve its targets immediately.
    pub fn create_review(
        &self,
        ctx: &Ctx,
        id: ReviewId,
        workspace_id: WorkspaceId,
        title: String,
        targets: NonEmpty<ReviewTarget>,
    ) -> Result<ReviewRecord, CoreError> {
        let ws = self.workspace(workspace_id)?;
        if self.store.review(id)?.is_some() {
            return Err(CoreError::invalid(format!("review {id} already exists")));
        }
        for t in &targets {
            if !ws.repos.iter().any(|r| r.id == t.repo_id) {
                return Err(CoreError::invalid(format!(
                    "repo {} is not in workspace {workspace_id}",
                    t.repo_id
                )));
            }
        }
        // Pre-flight: every target must resolve before anything is
        // committed, or an unresolvable base (say `Upstream` with no
        // upstream configured) would leave a ghost review behind.
        for t in &targets {
            let repo = self.repo(t.repo_id)?;
            repo.resolve(&t.base)?;
            repo.resolve(&t.head)?;
        }
        let review = Review {
            id,
            workspace_id,
            title,
            targets,
            created: ctx.now,
            status: ReviewStatus::Open,
        };
        self.append(
            ctx,
            EventBody::ReviewCreated {
                review: review.clone(),
            },
        )?;
        let (resolved, _) = self.resolve_targets(ctx, id)?;
        Ok(ReviewRecord {
            review,
            resolved: Some(resolved),
            lifecycle: ReviewLifecycle::Live,
        })
    }

    /// Update metadata, refreshing targets and anchors when an archived review
    /// is reopened. Unresolvable refs leave the review archived.
    pub fn update_review(
        &self,
        ctx: &Ctx,
        id: ReviewId,
        title: String,
        status: ReviewStatus,
    ) -> Result<(), CoreError> {
        let rec = self.review(id)?;
        // Resolve before writing so a missing ref cannot reopen a stale review.
        // Keep ReviewUpdated first: transports acknowledge the primary event.
        let refreshed =
            if rec.review.status == ReviewStatus::Archived && status == ReviewStatus::Open {
                Some(self.resolve_review_targets(&rec.review)?)
            } else {
                None
            };
        self.append(
            ctx,
            EventBody::ReviewUpdated {
                review_id: id,
                title,
                status,
            },
        )?;
        if let Some(resolved) = refreshed {
            self.record_resolved_targets(ctx, &rec, &resolved)?;
        }
        Ok(())
    }

    /// Replace one repo's base or head after resolving the complete new
    /// target first. A failed/stale ref cannot partially update a review,
    /// and `TargetRevision::Base` cannot contain a working tree.
    pub fn update_review_target(
        &self,
        ctx: &Ctx,
        id: ReviewId,
        update: ReviewTargetUpdate,
    ) -> Result<(), CoreError> {
        let rec = self.review(id)?;
        let mut target = rec
            .review
            .targets
            .iter()
            .find(|target| target.repo_id == update.repo_id)
            .cloned()
            .ok_or_else(|| CoreError::not_found(EntityKind::Repo, &update.repo_id))?;
        match update.revision {
            TargetRevision::Base { ref_spec } => target.base = ref_spec.into(),
            TargetRevision::Head { ref_spec } => target.head = ref_spec,
        }
        let repo = self.repo(target.repo_id)?;
        repo.resolve(&target.base)
            .and_then(|_| repo.resolve(&target.head))
            .map_err(|error| CoreError::Invalid {
                reason: error.to_string(),
            })?;
        self.append(
            ctx,
            EventBody::ReviewTargetUpdated {
                review_id: id,
                target,
            },
        )?;
        self.resolve_targets(ctx, id)?;
        Ok(())
    }

    pub fn delete_review(&self, ctx: &Ctx, id: ReviewId) -> Result<(), CoreError> {
        self.review(id)?;
        self.append(ctx, EventBody::ReviewDeleted { review_id: id })?;
        Ok(())
    }

    /// Re-resolve every target. Emits `ReviewTargetsResolved` only when the
    /// resolved content or provenance differs, then re-anchors comments.
    /// Returns `(targets, changed)`.
    pub fn resolve_targets(
        &self,
        ctx: &Ctx,
        id: ReviewId,
    ) -> Result<(NonEmpty<ResolvedTarget>, bool), CoreError> {
        let rec = self.review(id)?;
        let resolved = self.resolve_review_targets(&rec.review)?;
        let changed = self.record_resolved_targets(ctx, &rec, &resolved)?;
        Ok((resolved, changed))
    }

    pub(crate) fn resolve_review_targets(
        &self,
        review: &Review,
    ) -> Result<NonEmpty<ResolvedTarget>, CoreError> {
        let mut resolved = Vec::new();
        for t in &review.targets {
            let repo = self.repo(t.repo_id)?;
            resolved.push(ResolvedTarget {
                repo_id: t.repo_id,
                base: repo.resolve(&t.base)?,
                head: repo.resolve(&t.head)?,
            });
        }
        NonEmpty::new(resolved).map_err(|e| CoreError::invalid(e.to_string()))
    }

    pub(crate) fn record_resolved_targets(
        &self,
        ctx: &Ctx,
        rec: &ReviewRecord,
        resolved: &NonEmpty<ResolvedTarget>,
    ) -> Result<bool, CoreError> {
        let changed = rec.resolved.as_ref() != Some(resolved);
        if changed {
            self.retain_targets(rec.review.id, resolved)?;
            self.append(
                ctx,
                EventBody::ReviewTargetsResolved {
                    review_id: rec.review.id,
                    targets: resolved.clone(),
                },
            )?;
            if let Some(old) = &rec.resolved {
                self.reanchor_review(ctx, rec.review.id, old, resolved)?;
            }
        }
        Ok(changed)
    }

    fn resolved(
        &self,
        id: ReviewId,
    ) -> Result<(ReviewRecord, NonEmpty<ResolvedTarget>), CoreError> {
        let rec = self.review(id)?;
        let resolved = rec
            .resolved
            .clone()
            .ok_or_else(|| CoreError::invalid(format!("review {id} has not been resolved")))?;
        Ok((rec, resolved))
    }

    pub(crate) fn target(
        resolved: &NonEmpty<ResolvedTarget>,
        repo_id: RepoId,
    ) -> Result<ResolvedTarget, CoreError> {
        resolved
            .iter()
            .find(|t| t.repo_id == repo_id)
            .cloned()
            .ok_or_else(|| CoreError::not_found(EntityKind::Repo, &repo_id))
    }

    /// The targets `scope` selects (UI-DESIGN §Diff scope), starting from
    /// the review's resolved targets. `Commit` and `Worktree` must name a
    /// repo the review targets.
    pub fn scoped_targets(
        &self,
        id: ReviewId,
        scope: &DiffScope,
    ) -> Result<NonEmpty<ResolvedTarget>, CoreError> {
        let (_, resolved) = self.resolved(id)?;
        match scope {
            DiffScope::All => Ok(resolved),
            DiffScope::Requested { request_id } => {
                let snapshot = self.review_snapshot(id)?;
                let request = snapshot
                    .requests
                    .iter()
                    .find(|request| request.id == *request_id)
                    .ok_or_else(|| CoreError::invalid("request does not belong to this review"))?;
                match &request.targets {
                    nits_protocol::RequestedTargets::Unknown => {
                        Err(CoreError::invalid("historical request has unknown targets"))
                    }
                    nits_protocol::RequestedTargets::Captured { targets } => Ok(targets.clone()),
                }
            }
            DiffScope::SinceCheckpoint { checkpoint_id } => {
                let snapshot = self.review_snapshot(id)?;
                let checkpoint = snapshot
                    .checkpoints
                    .iter()
                    .find(|checkpoint| checkpoint.id == *checkpoint_id)
                    .ok_or_else(|| {
                        CoreError::invalid("checkpoint does not belong to this review")
                    })?;
                if checkpoint.targets.len() != resolved.len() {
                    return Err(CoreError::invalid(
                        "checkpoint repository set differs from current review",
                    ));
                }
                let targets = resolved
                    .into_iter()
                    .map(|mut target| {
                        target.base = Self::target(&checkpoint.targets, target.repo_id)?.head;
                        Ok(target)
                    })
                    .collect::<Result<Vec<_>, CoreError>>()?;
                NonEmpty::new(targets).map_err(|_| CoreError::invalid("review has no targets"))
            }
            DiffScope::Committed => {
                let mapped = resolved
                    .into_iter()
                    .map(|mut t| {
                        if let ResolvedSource::WorkingTree { head, .. } = t.head.source {
                            let oid = head.ok_or_else(|| {
                                CoreError::invalid("snapshot has no captured HEAD commit")
                            })?;
                            t.head = self.repo(t.repo_id)?.resolve(&RefSpec::Commit { oid })?;
                        }
                        Ok(t)
                    })
                    .collect::<Result<Vec<_>, CoreError>>()?;
                NonEmpty::new(mapped).map_err(|_| CoreError::invalid("review has no targets"))
            }
            DiffScope::Commit { repo_id, oid } => {
                Self::target(&resolved, *repo_id)?;
                Ok(NonEmpty::singleton(self.commit_step(*repo_id, *oid)?))
            }
            DiffScope::Worktree { repo_id } => {
                let target = Self::target(&resolved, *repo_id)?;
                let repo = self.repo(*repo_id)?;
                let (base, head) = match target.head.source {
                    ResolvedSource::WorkingTree { head, .. } => {
                        let oid = head.ok_or_else(|| {
                            CoreError::invalid("snapshot has no captured working-tree HEAD commit")
                        })?;
                        (repo.resolve(&RefSpec::Commit { oid })?, target.head)
                    }
                    // Explicitly inspecting the live checkout remains available
                    // for a review whose target itself is a fixed commit.
                    ResolvedSource::Commit { .. } => (
                        repo.resolve(&RefSpec::Head)?,
                        repo.resolve(&RefSpec::WorkingTree)?,
                    ),
                };
                Ok(NonEmpty::singleton(ResolvedTarget {
                    repo_id: *repo_id,
                    base,
                    head,
                }))
            }
        }
    }

    /// Every changed file across all targets, ordered by repo display name
    /// then path. This is the flat form of the merged tree.
    pub fn files(&self, id: ReviewId) -> Result<Vec<FileChange>, CoreError> {
        self.files_scoped(id, &DiffScope::All)
            .map(|(files, _)| files)
    }

    /// [`Self::files`] under `scope`, with the targets the files were
    /// diffed between.
    pub fn files_scoped(
        &self,
        id: ReviewId,
        scope: &DiffScope,
    ) -> Result<(Vec<FileChange>, NonEmpty<ResolvedTarget>), CoreError> {
        let rec = self.review(id)?;
        let resolved = self.scoped_targets(id, scope)?;
        let ws = self.workspace(rec.review.workspace_id)?;
        let mut targets: Vec<&ResolvedTarget> = resolved.iter().collect();
        let name = |rid: RepoId| {
            ws.repos
                .iter()
                .find(|r| r.id == rid)
                .map(|r| r.display_name.clone())
                .unwrap_or_default()
        };
        targets.sort_by_key(|t| (name(t.repo_id), t.repo_id));
        let mut out = Vec::new();
        for t in targets {
            let repo = self.repo(t.repo_id)?;
            let mut changes = repo.changed_files(t.base.tree, t.head.tree)?;
            // Tree display order (dirs first), so every client shows and
            // steps files in the same order without re-sorting.
            changes.sort_by(|a, b| a.path.tree_order(&b.path));
            out.extend(changes.into_iter().map(|c| FileChange {
                repo_id: t.repo_id,
                path: c.path,
                kind: c.kind,
            }));
        }
        Ok((out, resolved))
    }

    /// The change for one file, if it is in the review under `scope`.
    pub fn file_change(
        &self,
        id: ReviewId,
        repo_id: RepoId,
        path: &RepoPath,
        scope: &DiffScope,
    ) -> Result<FileChange, CoreError> {
        self.files_scoped(id, scope)?
            .0
            .into_iter()
            .find(|f| f.repo_id == repo_id && &f.path == path)
            .ok_or_else(|| CoreError::not_found(EntityKind::Path, &path))
    }

    /// Content search (UI-DESIGN §Search): case-insensitive substring over
    /// the scoped changed files, or over every file of the head trees when
    /// `all_files`. Binary and oversized blobs are skipped; hits are capped
    /// (the flag says the cap was hit).
    pub fn search(
        &self,
        id: ReviewId,
        query: &str,
        all_files: bool,
        scope: &DiffScope,
    ) -> Result<(Vec<nits_protocol::ContentHit>, bool), CoreError> {
        const MAX_HITS: usize = 200;
        const MAX_BLOB: usize = 1 << 20;
        if query.trim().is_empty() {
            return Ok((Vec::new(), false));
        }
        let needle = query.to_lowercase();
        let (files, targets) = self.files_scoped(id, scope)?;
        // (repo, path, blob) per searched file, head side.
        let mut candidates: Vec<(RepoId, RepoPath, BlobOid)> = Vec::new();
        if all_files {
            for t in &targets {
                let snapshot = self.tree_snapshot_of(t.repo_id, &t.head)?;
                for e in snapshot.entries {
                    if let TreeEntryKind::File { oid, size, .. } = e.kind
                        && size <= MAX_BLOB as u64
                    {
                        candidates.push((t.repo_id, e.path, oid));
                    }
                }
            }
        } else {
            for f in files {
                // Deleted files are searched on their base side.
                if let Some(oid) = f.kind.new_blob().or_else(|| f.kind.old_blob()) {
                    candidates.push((f.repo_id, f.path, oid));
                }
            }
        }
        let mut hits = Vec::new();
        let mut truncated = false;
        'files: for (repo_id, path, oid) in candidates {
            let repo = self.repo(repo_id)?;
            let bytes = repo.blob(oid)?;
            if bytes.len() > MAX_BLOB || crate::git::is_binary(&bytes) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            for (i, line) in text.lines().enumerate() {
                if line.to_lowercase().contains(&needle) {
                    if hits.len() >= MAX_HITS {
                        truncated = true;
                        break 'files;
                    }
                    let Some(line_no) =
                        nits_protocol::LineNo::new(u32::try_from(i + 1).unwrap_or(u32::MAX))
                    else {
                        continue;
                    };
                    let mut shown: String = line.trim_end().to_owned();
                    if shown.len() > 300 {
                        shown.truncate(shown.floor_char_boundary(300));
                    }
                    hits.push(nits_protocol::ContentHit {
                        repo_id,
                        path: path.clone(),
                        line: line_no,
                        text: shown,
                    });
                }
            }
        }
        Ok((hits, truncated))
    }

    /// Commits between base and head for one repo target (newest first).
    /// A working-tree head steps through its captured HEAD's commits (the
    /// worktree itself is the final step). Empty when the base is not a
    /// commit or the working-tree snapshot has no recorded HEAD.
    pub fn commits(&self, id: ReviewId, repo_id: RepoId) -> Result<Vec<CommitInfo>, CoreError> {
        let (_, resolved) = self.resolved(id)?;
        let t = Self::target(&resolved, repo_id)?;
        let ResolvedSource::Commit { oid: base } = t.base.source else {
            return Ok(vec![]);
        };
        let head = match t.head.source {
            ResolvedSource::Commit { oid }
            | ResolvedSource::WorkingTree {
                head: Some(oid), ..
            } => oid,
            ResolvedSource::WorkingTree { head: None, .. } => return Ok(vec![]),
        };
        Ok(self.repo(repo_id)?.commits_between(base, head)?)
    }

    /// A single-commit sub-target for stepping: base = first parent.
    pub fn commit_step(
        &self,
        repo_id: RepoId,
        commit: CommitOid,
    ) -> Result<ResolvedTarget, CoreError> {
        let repo = self.repo(repo_id)?;
        let info = repo.commit_info(commit)?;
        let base = match info.parents.first() {
            Some(p) => {
                let parent = repo.commit_info(*p)?;
                ResolvedRef {
                    tree: parent.tree,
                    source: ResolvedSource::Commit { oid: *p },
                }
            }
            None => ResolvedRef {
                tree: EMPTY_TREE,
                source: ResolvedSource::Commit { oid: commit },
            },
        };
        Ok(ResolvedTarget {
            repo_id,
            base,
            head: ResolvedRef {
                tree: info.tree,
                source: ResolvedSource::Commit { oid: commit },
            },
        })
    }

    pub fn tree_snapshot(
        &self,
        repo_id: RepoId,
        spec: &RefSpec,
    ) -> Result<TreeSnapshot, CoreError> {
        let repo = self.repo(repo_id)?;
        let r = repo.resolve(spec)?;
        Ok(repo.tree_snapshot(repo_id, r.tree)?)
    }

    pub fn tree_snapshot_of(
        &self,
        repo_id: RepoId,
        r: &ResolvedRef,
    ) -> Result<TreeSnapshot, CoreError> {
        Ok(self.repo(repo_id)?.tree_snapshot(repo_id, r.tree)?)
    }

    // ---- render -----------------------------------------------------------

    /// Render one changed file of a review under `scope`. Cached by content.
    pub fn file_render(
        &self,
        id: ReviewId,
        repo_id: RepoId,
        path: &RepoPath,
        opts: RenderOpts,
        scope: &DiffScope,
    ) -> Result<(FileRenderHeader, Rendered), CoreError> {
        let change = self.file_change(id, repo_id, path, scope)?;
        self.render_change(repo_id, path, change.kind, opts)
    }

    /// Render a change directly (used by commit stepping).
    pub fn render_change(
        &self,
        repo_id: RepoId,
        path: &RepoPath,
        kind: ChangeKind,
        opts: RenderOpts,
    ) -> Result<(FileRenderHeader, Rendered), CoreError> {
        if matches!(kind, ChangeKind::Submodule { .. }) {
            let rendered = Rendered { content: nits_protocol::RenderContent::Submodule, rows: vec![] };
            return Ok((FileRenderHeader {
                repo_id, path: path.clone(), target: RenderTarget::Diff { change: kind }, opts,
                lang: None, content: rendered.content.clone(),
            }, rendered));
        }
        let repo = self.repo(repo_id)?;
        let old = kind.old_blob().map(|b| repo.blob(b)).transpose()?;
        let new = kind.new_blob().map(|b| repo.blob(b)).transpose()?;
        let sample = new.as_deref().or(old.as_deref()).unwrap_or_default();
        let lang = self.hl.detect(path.as_str(), first_line(sample));
        let target = RenderTarget::Diff { change: kind };
        let rendered = self.cached_render(&target, &opts, lang.as_deref(), || {
            render_file(
                &self.hl,
                old.as_deref(),
                new.as_deref(),
                lang.as_deref(),
                &opts,
            )
        })?;
        Ok((
            FileRenderHeader {
                repo_id,
                path: path.clone(),
                target,
                opts,
                lang,
                content: rendered.content.clone(),
            },
            rendered,
        ))
    }

    /// Render a blob for the explorer.
    pub fn blob_render(
        &self,
        repo_id: RepoId,
        path: &RepoPath,
        blob: BlobOid,
    ) -> Result<(FileRenderHeader, Rendered), CoreError> {
        let repo = self.repo(repo_id)?;
        let bytes = repo.blob(blob)?;
        let lang = self.hl.detect(path.as_str(), first_line(&bytes));
        let target = RenderTarget::Blob { oid: blob };
        let opts = RenderOpts::default();
        let rendered = self.cached_render(&target, &opts, lang.as_deref(), || {
            render_blob(&self.hl, &bytes, lang.as_deref())
        })?;
        Ok((
            FileRenderHeader {
                repo_id,
                path: path.clone(),
                target,
                opts,
                lang,
                content: rendered.content.clone(),
            },
            rendered,
        ))
    }

    /// Serve from the render cache or compute and store.
    fn cached_render(
        &self,
        target: &RenderTarget,
        opts: &RenderOpts,
        lang: Option<&str>,
        render: impl FnOnce() -> Rendered,
    ) -> Result<Rendered, CoreError> {
        let key = RenderKey { target, opts, lang };
        if let Some(content) = self.cache.header(&key)? {
            let mut rows = Vec::new();
            let mut i = 0;
            while let Some(chunk) = self.cache.chunk(&key, nits_protocol::ChunkIndex::new(i))? {
                rows.extend(chunk.rows);
                i += 1;
            }
            return Ok(Rendered { content, rows });
        }
        let rendered = render();
        self.cache.put(&key, &rendered)?;
        Ok(rendered)
    }

    /// Raw blob bytes from a repo (CLI/MCP `get_file`).
    pub fn repo_blob(&self, repo_id: RepoId, blob: BlobOid) -> Result<Vec<u8>, CoreError> {
        Ok(self.repo(repo_id)?.blob(blob)?)
    }

    // ---- viewed -----------------------------------------------------------

    /// Head-side content identity, preserving gitlinks as commits.
    pub(crate) fn head_content(
        &self,
        id: ReviewId,
        repo_id: RepoId,
        path: &RepoPath,
    ) -> Result<nits_protocol::ViewedContent, CoreError> {
        use nits_protocol::ViewedContent;
        let (_, resolved) = self.resolved(id)?;
        let target = Self::target(&resolved, repo_id)?;
        let snap = self.repo(repo_id)?.tree_snapshot(repo_id, target.head.tree)?;
        Ok(snap.entries.iter().find(|entry| &entry.path == path).map_or(
            ViewedContent::Missing,
            |entry| match entry.kind {
                TreeEntryKind::File { oid, .. } | TreeEntryKind::Symlink { oid } => ViewedContent::Blob { oid },
                TreeEntryKind::Submodule { commit } => ViewedContent::Submodule { commit },
                TreeEntryKind::Dir { .. } => ViewedContent::Missing,
            },
        ))
    }

    /// Human-only: agents get `Forbidden`.
    pub fn mark_viewed(
        &self,
        ctx: &Ctx,
        id: ReviewId,
        repo_id: RepoId,
        path: RepoPath,
    ) -> Result<ViewedMark, CoreError> {
        let viewer = ctx
            .author
            .as_human()
            .ok_or_else(|| CoreError::forbidden("agents cannot mark files as viewed"))?;
        let content = self.head_content(id, repo_id, &path)?;
        let mark = ViewedMark {
            review_id: id,
            repo_id,
            path: path.clone(),
            viewer: viewer.clone(),
            content,
        };
        self.append(
            ctx,
            EventBody::FileViewed {
                review_id: id,
                repo_id,
                path,
                viewer,
                content,
            },
        )?;
        Ok(mark)
    }

    pub fn unmark_viewed(
        &self,
        ctx: &Ctx,
        id: ReviewId,
        repo_id: RepoId,
        path: RepoPath,
    ) -> Result<(), CoreError> {
        let viewer = ctx
            .author
            .as_human()
            .ok_or_else(|| CoreError::forbidden("agents cannot mark files as viewed"))?;
        self.review(id)?;
        self.append(
            ctx,
            EventBody::FileUnviewed {
                review_id: id,
                repo_id,
                path,
                viewer,
            },
        )?;
        Ok(())
    }

    pub fn viewed_marks(&self, id: ReviewId) -> Result<Vec<ViewedMark>, CoreError> {
        Ok(self.store.viewed(id)?)
    }

    pub fn viewed_state(
        &self,
        id: ReviewId,
        repo_id: RepoId,
        path: &RepoPath,
    ) -> Result<ViewedState, CoreError> {
        let Some(mark) = self
            .store
            .viewed(id)?
            .into_iter()
            .find(|m| m.repo_id == repo_id && &m.path == path)
        else {
            return Ok(ViewedState::Unviewed);
        };
        let current = self.head_content(id, repo_id, path)?;
        Ok(if current == mark.content {
            ViewedState::Viewed
        } else {
            ViewedState::ChangedSinceViewed {
                marked: mark.content,
            }
        })
    }

    // ---- snapshot ---------------------------------------------------------

    /// Materialised state for a fresh client.
    pub fn review_snapshot(&self, id: ReviewId) -> Result<ReviewSnapshot, CoreError> {
        self.store
            .review_snapshot(id)?
            .ok_or_else(|| CoreError::not_found(EntityKind::Review, &id))
    }
}

/// git's well-known empty tree.
pub const EMPTY_TREE: nits_protocol::TreeOid = nits_protocol::TreeOid::from_bytes([
    0x4b, 0x82, 0x5d, 0xc6, 0x42, 0xcb, 0x6e, 0xb9, 0xa0, 0x60, 0xe5, 0x4b, 0xf8, 0xd6, 0x92, 0x88,
    0xfb, 0xee, 0x49, 0x04,
]);

fn first_line(bytes: &[u8]) -> Option<&str> {
    let end = bytes
        .iter()
        .position(|b| *b == b'\n')
        .unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end]).ok()
}

impl Core {
    // ---- working tree (used by the file watcher) --------------------------

    /// Snapshot the working tree of `repo_id` as a real tree object.
    pub fn working_tree(&self, repo_id: RepoId) -> Result<ResolvedRef, CoreError> {
        Ok(self.repo(repo_id)?.working_tree()?)
    }

    /// Entries that differ between two trees of `repo_id`.
    pub fn tree_delta(
        &self,
        repo_id: RepoId,
        from: nits_protocol::TreeOid,
        to: nits_protocol::TreeOid,
    ) -> Result<TreeDelta, CoreError> {
        Ok(self.repo(repo_id)?.tree_delta(repo_id, from, to)?)
    }

    /// Open, non-deleted reviews with a working-tree target on `repo_id`,
    /// across all workspaces. Archived reviews retain their last resolution.
    pub fn working_tree_reviews(&self, repo_id: RepoId) -> Result<Vec<ReviewId>, CoreError> {
        let mut out = Vec::new();
        for ws in self.store.workspaces()? {
            for rec in self.store.reviews(ws.id)? {
                let live = matches!(rec.lifecycle, ReviewLifecycle::Live);
                if live && watches_working_tree(&rec.review, repo_id) {
                    out.push(rec.review.id);
                }
            }
        }
        Ok(out)
    }

    /// Refresh a watcher candidate only while it still watches `repo_id`.
    /// Call on the writer thread: a review may have been archived since the
    /// watcher obtained its candidate list.
    pub fn refresh_working_tree_review(
        &self,
        ctx: &Ctx,
        id: ReviewId,
        repo_id: RepoId,
    ) -> Result<(), CoreError> {
        if watches_working_tree(&self.review(id)?.review, repo_id) {
            self.resolve_targets(ctx, id)?;
        }
        Ok(())
    }
}

fn watches_working_tree(review: &Review, repo_id: RepoId) -> bool {
    match review.status {
        ReviewStatus::Open => review.targets.iter().any(|t| {
            t.repo_id == repo_id
                && (t.base == RefSpec::WorkingTree || t.head == RefSpec::WorkingTree)
        }),
        ReviewStatus::Archived => false,
    }
}
