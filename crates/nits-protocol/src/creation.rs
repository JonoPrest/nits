//! Validated inputs for creating new reviews. Historical reviews intentionally
//! retain their original target arrays, including duplicates from older clients.

use std::collections::BTreeSet;

use crate::{NonEmpty, RepoId, ReviewTarget};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CreateReviewTargetsError {
    #[error("Choose at least one repository.")]
    Empty,
    #[error(
        "Repository {repo_id} is included more than once. Choose one base/head pair for each repository."
    )]
    Duplicate { repo_id: RepoId },
}

/// Ordered, nonempty targets with exactly one entry for each repository.
/// This is a creation boundary type, not the decoder for persisted reviews or
/// recovery snapshots: those must remain inspectable when they contain old data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateReviewTargets(NonEmpty<ReviewTarget>);

impl CreateReviewTargets {
    #[must_use]
    pub fn singleton(target: ReviewTarget) -> Self {
        Self(NonEmpty::singleton(target))
    }

    /// Check editable repository choices before their ref text can be parsed.
    /// Empty editable drafts are allowed; construction enforces nonemptiness.
    pub fn check_repositories(
        repositories: impl IntoIterator<Item = RepoId>,
    ) -> Result<(), CreateReviewTargetsError> {
        let mut seen = BTreeSet::new();
        for repo_id in repositories {
            if !seen.insert(repo_id) {
                return Err(CreateReviewTargetsError::Duplicate { repo_id });
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn as_targets(&self) -> &NonEmpty<ReviewTarget> {
        &self.0
    }
}

impl TryFrom<NonEmpty<ReviewTarget>> for CreateReviewTargets {
    type Error = CreateReviewTargetsError;

    fn try_from(targets: NonEmpty<ReviewTarget>) -> Result<Self, Self::Error> {
        Self::check_repositories(targets.iter().map(|target| target.repo_id))?;
        Ok(Self(targets))
    }
}

impl TryFrom<Vec<ReviewTarget>> for CreateReviewTargets {
    type Error = CreateReviewTargetsError;

    fn try_from(targets: Vec<ReviewTarget>) -> Result<Self, Self::Error> {
        NonEmpty::new(targets)
            .map_err(|_| CreateReviewTargetsError::Empty)?
            .try_into()
    }
}

impl From<CreateReviewTargets> for NonEmpty<ReviewTarget> {
    fn from(targets: CreateReviewTargets) -> Self {
        targets.0
    }
}
