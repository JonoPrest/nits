//! Repository IDs identify canonical checkouts across workspace memberships.
//! One guard covers ownership checks, membership commits and cache publication.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nits_protocol::{EntityKind, EventBody, Repo, RepoId, ReviewId, WorkspaceId};

use crate::git::Repo as GitRepo;
use crate::{Core, CoreError, Ctx};

/// A checkout root, not the common Git directory shared by linked worktrees.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckoutPath(PathBuf);

#[derive(Debug, Clone)]
struct OpenedRepo {
    checkout: CheckoutPath,
    git: Arc<GitRepo>,
}

impl OpenedRepo {
    fn open(path: &Path) -> Result<Self, CoreError> {
        let git = GitRepo::open(&std::fs::canonicalize(path)?)?;
        let checkout = CheckoutPath(std::fs::canonicalize(git.workdir())?);
        // Keep the cached handle on the same canonical checkout as its identity,
        // even if the supplied symlink or Git-directory alias later changes.
        let git = if git.workdir() == checkout.0 {
            git
        } else {
            GitRepo::open(&checkout.0)?
        };
        Ok(Self {
            checkout,
            git: Arc::new(git),
        })
    }
}

#[derive(Debug, Default)]
pub(crate) struct RepositoryRegistry {
    opened: HashMap<RepoId, OpenedRepo>,
}

impl Core {
    /// Resolve every current membership before trusting a cached repository.
    /// Legacy ambiguity does not prevent opening the store or detaching a member.
    fn bound_repo(
        &self,
        registry: &RepositoryRegistry,
        id: RepoId,
    ) -> Result<Option<OpenedRepo>, CoreError> {
        let mut bound: Option<(WorkspaceId, OpenedRepo)> = None;
        for workspace in self.store.workspaces()? {
            for member in workspace.repos.into_iter().filter(|repo| repo.id == id) {
                let path = std::fs::canonicalize(&member.path).map_err(|error| {
                    CoreError::invalid(format!(
                        "repo {id} in workspace {} is unavailable at {}: {error}; detach this membership to repair it",
                        workspace.id, member.path
                    ))
                })?;
                let opened = match registry.opened.get(&id) {
                    Some(cached) if cached.checkout.0 == path => cached.clone(),
                    Some(_) | None => OpenedRepo::open(&path).map_err(|error| {
                        CoreError::invalid(format!(
                            "repo {id} in workspace {} cannot open {}: {error}; detach this membership to repair it",
                            workspace.id, member.path
                        ))
                    })?,
                };
                if let Some((owner, previous)) = &bound {
                    if previous.checkout != opened.checkout {
                        return Err(CoreError::invalid(format!(
                            "repo {id} has conflicting checkout memberships: workspace {owner} ({}) and workspace {} ({}); detach the incorrect membership before accessing this repo",
                            previous.checkout.0.display(),
                            workspace.id,
                            opened.checkout.0.display()
                        )));
                    }
                } else {
                    bound = Some((workspace.id, opened));
                }
            }
        }
        Ok(bound.map(|(_, opened)| opened))
    }

    fn check_repo_owner(
        &self,
        registry: &RepositoryRegistry,
        id: RepoId,
        candidate: &OpenedRepo,
    ) -> Result<(), CoreError> {
        if let Some(bound) = self.bound_repo(registry, id)?
            && bound.checkout != candidate.checkout
        {
            return Err(CoreError::invalid(format!(
                "repo {id} already identifies {}; cannot attach it to {}",
                bound.checkout.0.display(),
                candidate.checkout.0.display()
            )));
        }
        Ok(())
    }

    fn publish_attachment(
        &self,
        registry: &mut RepositoryRegistry,
        ctx: &Ctx,
        workspace_id: WorkspaceId,
        repo_id: RepoId,
        opened: OpenedRepo,
        display_name: String,
    ) -> Result<Repo, CoreError> {
        let repo = Repo {
            id: repo_id,
            path: opened.checkout.0.to_string_lossy().into_owned(),
            display_name,
        };
        self.append(
            ctx,
            EventBody::RepoAttached {
                workspace_id,
                repo: repo.clone(),
            },
        )?;
        registry.opened.insert(repo_id, opened);
        Ok(repo)
    }

    /// A repository ID may join multiple workspaces only for the same checkout.
    pub fn attach_repo(
        &self,
        ctx: &Ctx,
        workspace_id: WorkspaceId,
        repo_id: RepoId,
        path: &str,
        display_name: String,
    ) -> Result<Repo, CoreError> {
        let mut registry = self
            .repositories
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let workspace = self.workspace(workspace_id)?;
        if workspace.repos.iter().any(|repo| repo.id == repo_id) {
            return Err(CoreError::invalid(format!(
                "repo {repo_id} already attached"
            )));
        }
        let opened = OpenedRepo::open(Path::new(path))?;
        self.check_repo_owner(&registry, repo_id, &opened)?;
        self.publish_attachment(
            &mut registry,
            ctx,
            workspace_id,
            repo_id,
            opened,
            display_name,
        )
    }

    /// Bootstrap preflights ownership before creating its workspace, holding the
    /// same guard through attachment so a competing attachment cannot race it.
    pub(crate) fn create_workspace_with_repo(
        &self,
        ctx: &Ctx,
        workspace_id: WorkspaceId,
        repo_id: RepoId,
        path: &Path,
        name: String,
    ) -> Result<(), CoreError> {
        let mut registry = self
            .repositories
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let opened = OpenedRepo::open(path)?;
        self.check_repo_owner(&registry, repo_id, &opened)?;
        self.create_workspace(ctx, workspace_id, name.clone())?;
        self.publish_attachment(&mut registry, ctx, workspace_id, repo_id, opened, name)?;
        Ok(())
    }

    pub fn detach_repo(
        &self,
        ctx: &Ctx,
        workspace_id: WorkspaceId,
        repo_id: RepoId,
    ) -> Result<(), CoreError> {
        let mut registry = self
            .repositories
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let workspace = self.workspace(workspace_id)?;
        if !workspace.repos.iter().any(|repo| repo.id == repo_id) {
            return Err(CoreError::not_found(EntityKind::Repo, &repo_id));
        }
        self.append(
            ctx,
            EventBody::RepoDetached {
                workspace_id,
                repo_id,
            },
        )?;
        registry.opened.remove(&repo_id);
        Ok(())
    }

    fn lookup_repo(
        &self,
        registry: &mut RepositoryRegistry,
        id: RepoId,
    ) -> Result<Arc<GitRepo>, CoreError> {
        let opened = self
            .bound_repo(registry, id)?
            .ok_or_else(|| CoreError::not_found(EntityKind::Repo, &id))?;
        let git = Arc::clone(&opened.git);
        registry.opened.insert(id, opened);
        Ok(git)
    }

    pub(crate) fn repo(&self, id: RepoId) -> Result<Arc<GitRepo>, CoreError> {
        let mut registry = self
            .repositories
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.lookup_repo(&mut registry, id)
    }

    pub(crate) fn workspace_repo(
        &self,
        workspace_id: WorkspaceId,
        id: RepoId,
    ) -> Result<Arc<GitRepo>, CoreError> {
        let mut registry = self
            .repositories
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self
            .workspace(workspace_id)?
            .repos
            .iter()
            .any(|repo| repo.id == id)
        {
            return Err(CoreError::invalid(format!(
                "repo {id} is not attached to workspace {workspace_id}"
            )));
        }
        self.lookup_repo(&mut registry, id)
    }

    pub(crate) fn review_repo(
        &self,
        review_id: ReviewId,
        id: RepoId,
    ) -> Result<Arc<GitRepo>, CoreError> {
        let review = self.review(review_id)?.review;
        if !review.targets.iter().any(|target| target.repo_id == id) {
            return Err(CoreError::invalid(format!(
                "repo {id} is not in review {review_id}"
            )));
        }
        self.workspace_repo(review.workspace_id, id)
    }
}
