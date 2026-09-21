//! Repository IDs identify canonical checkouts across workspace memberships.
//! One guard covers ownership checks, membership commits and cache publication.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nits_protocol::{EntityKind, EventBody, Repo, RepoId, ReviewId, WorkspaceId};

use crate::git::{CheckoutPath, Repo as GitRepo};
use crate::{Core, CoreError, Ctx};

#[derive(Debug, Clone)]
struct OpenedRepo {
    checkout: CheckoutPath,
    git: Arc<GitRepo>,
}

impl OpenedRepo {
    fn open(path: &Path) -> Result<Self, CoreError> {
        let checkout = CheckoutPath::resolve(path)?;
        let git = GitRepo::open(checkout.as_path())?;
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
                    Some(cached) if cached.checkout.as_path() == path => cached.clone(),
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
                            previous.checkout.as_path().display(),
                            workspace.id,
                            opened.checkout.as_path().display()
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
                bound.checkout.as_path().display(),
                candidate.checkout.as_path().display()
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
            path: opened.checkout.as_path().to_string_lossy().into_owned(),
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
                "repo {repo_id} already attached to workspace {workspace_id}; reuse this repository ID or detach this membership before attaching again"
            )));
        }
        let opened = OpenedRepo::open(Path::new(path))?;
        self.check_repo_owner(&registry, repo_id, &opened)?;
        for member in workspace.repos {
            // Compare opened workdirs so legacy Git-directory and symlink
            // aliases are equivalent. An unavailable unrelated attachment must
            // remain repairable without blocking another valid checkout.
            if let Ok(existing) = OpenedRepo::open(Path::new(&member.path))
                && existing.checkout == opened.checkout
            {
                return Err(CoreError::invalid(format!(
                    "checkout {} is already attached to workspace {workspace_id} as repo {}; reuse this repository ID or detach that membership before attaching again. Attaching it to another workspace is allowed",
                    opened.checkout.as_path().display(),
                    member.id
                )));
            }
        }
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

    /// The canonical checkout currently owned by this ID. Filesystem adapters
    /// must use the same ownership validation as Git-backed reads, including
    /// legacy ambiguity and unavailable memberships.
    pub fn repo_checkout_path(&self, id: RepoId) -> Result<PathBuf, CoreError> {
        let registry = self
            .repositories
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let opened = self
            .bound_repo(&registry, id)?
            .ok_or_else(|| CoreError::not_found(EntityKind::Repo, &id))?;
        Ok(opened.checkout.as_path().to_path_buf())
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
