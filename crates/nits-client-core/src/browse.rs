//! Browse selection is committed only after its repository's tree resolves.
//! Failed or superseded candidates never replace the file currently being read.

use nits_protocol::{
    RefSpec, RepoId, Request, RequestId, RpcError, TreeOid, TreeSnapshot, ViewSection,
};
use serde::{Deserialize, Serialize};
use strum::EnumDiscriminants;

use crate::{CacheKey, CacheValue, ClientCore, CoreError, Effect, InFlight, TreeKey};

/// One repository and the revision requested within it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowseTarget {
    pub repo_id: RepoId,
    pub ref_spec: RefSpec,
}

/// The candidate's state; the committed selection remains usable throughout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[strum_discriminants(name(BrowseStatusKind), derive(Hash, strum::EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum BrowseStatus {
    Loading,
    Failed { message: String },
}

/// A candidate shown separately from the currently visible revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowseAttemptView {
    pub target: BrowseTarget,
    pub status: BrowseStatus,
}

/// Browse controls for an open review. No selection means its review head trees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowseView {
    pub repo_id: RepoId,
    pub selection: Option<BrowseTarget>,
    pub attempt: Option<BrowseAttemptView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Browse {
    pub(crate) target: BrowseTarget,
    pub(crate) root: TreeOid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrowseAttempt {
    pub(crate) target: BrowseTarget,
    pub(crate) state: BrowseAttemptState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BrowseAttemptState {
    Loading { request_id: RequestId },
    Failed { message: String },
}

impl ClientCore {
    pub(crate) fn browse_repo(&self) -> Option<RepoId> {
        let review = self.view.review.as_ref()?;
        self.browse_repo
            .or_else(|| crate::focus::target_repo_of(&self.view, self.view.focus))
            .filter(|repo_id| {
                review
                    .snapshot
                    .review
                    .targets
                    .iter()
                    .any(|target| target.repo_id == *repo_id)
            })
            .or_else(|| Some(review.snapshot.review.targets.first().repo_id))
    }

    pub(crate) fn browse_view(&self) -> Option<BrowseView> {
        Some(BrowseView {
            repo_id: self.browse_repo()?,
            selection: self.browse.as_ref().map(|browse| browse.target.clone()),
            attempt: self
                .browse_attempt
                .as_ref()
                .map(|attempt| BrowseAttemptView {
                    target: attempt.target.clone(),
                    status: match &attempt.state {
                        BrowseAttemptState::Loading { .. } => BrowseStatus::Loading,
                        BrowseAttemptState::Failed { message } => BrowseStatus::Failed {
                            message: message.clone(),
                        },
                    },
                }),
        })
    }

    pub(crate) fn close_browse_selector(&mut self) {
        if self
            .ref_selector
            .as_ref()
            .is_some_and(|selector| selector.view.purpose == crate::RefSelectorPurpose::Browse)
        {
            self.ref_selector = None;
        }
    }

    pub(crate) fn require_browse_repo(&self, repo_id: RepoId) -> Result<(), CoreError> {
        let review = self.view.review.as_ref().ok_or(CoreError::NoOpenReview)?;
        if review
            .snapshot
            .review
            .targets
            .iter()
            .any(|target| target.repo_id == repo_id)
        {
            Ok(())
        } else {
            Err(CoreError::UnknownRepo(repo_id))
        }
    }

    pub(crate) fn set_browse_ref(
        &mut self,
        repo_id: RepoId,
        ref_spec: Option<RefSpec>,
    ) -> Result<Vec<Effect>, CoreError> {
        self.require_browse_repo(repo_id)?;
        if self.view.draft.is_some() {
            return Err(CoreError::DraftAlreadyOpen);
        }
        if let Some(ref_spec) = ref_spec {
            self.require_subscribed()?;
            self.browse_repo = Some(repo_id);
            self.browse_attempt = Some(BrowseAttempt {
                target: BrowseTarget {
                    repo_id,
                    ref_spec: ref_spec.clone(),
                },
                state: BrowseAttemptState::Loading {
                    request_id: RequestId::new(self.next_request),
                },
            });
            Ok(vec![self.request(
                Request::TreeSnapshot { repo_id, ref_spec },
                InFlight::BrowseTree { repo_id },
            )])
        } else {
            self.close_browse_selector();
            self.browse = None;
            self.browse_attempt = None;
            self.browse_repo = Some(repo_id);
            self.visual_anchor = None;
            if let Some(open) = &mut self.view.review {
                open.open_file = None;
                open.original = None;
            }
            let mut effects = Vec::new();
            self.want_review_trees(&mut effects);
            Ok(effects)
        }
    }

    pub(crate) fn browse_answer(
        &mut self,
        id: RequestId,
        repo_id: RepoId,
        snapshot: TreeSnapshot,
    ) -> Result<Vec<Effect>, CoreError> {
        let Some(attempt) = &self.browse_attempt else {
            return Ok(Vec::new());
        };
        if attempt.state != (BrowseAttemptState::Loading { request_id: id })
            || attempt.target.repo_id != repo_id
        {
            return Ok(Vec::new());
        }
        if snapshot.repo_id != repo_id {
            return Err(CoreError::UnexpectedResponse {
                id,
                expected: "Browse tree of the requested repository",
                got: "TreeSnapshot of another repository",
            });
        }
        if self.view.draft.is_some() {
            let message = "Finish or discard the open comment before changing revision.".to_owned();
            if let Some(attempt) = &mut self.browse_attempt {
                attempt.state = BrowseAttemptState::Failed {
                    message: message.clone(),
                };
            }
            if let Some(selector) = &mut self.ref_selector
                && selector.view.purpose == crate::RefSelectorPurpose::Browse
            {
                selector.view.status = crate::RefSelectorStatus::DaemonError { message };
            }
            return Ok(Vec::new());
        }
        let tree = TreeKey::of_snapshot(&snapshot);
        self.browse = Some(Browse {
            target: attempt.target.clone(),
            root: tree.root,
        });
        self.browse_attempt = None;
        if self.ref_selector.as_ref().is_some_and(|selector| {
            selector.view.purpose == crate::RefSelectorPurpose::Browse
                && selector.view.repo_id == repo_id
        }) {
            self.ref_selector = None;
        }
        self.visual_anchor = None;
        if let Some(open) = &mut self.view.review {
            open.open_file = None;
            open.original = None;
        }
        let mut effects = Vec::new();
        self.content.cache.pin(CacheKey::Tree { tree });
        self.arrived(
            CacheKey::Tree { tree },
            CacheValue::Tree { snapshot },
            crate::content::Arrival::Response,
            &mut effects,
        );
        self.want_review_trees(&mut effects);
        Ok(effects)
    }

    pub(crate) fn browse_failed(&mut self, id: RequestId, error: &RpcError) -> Vec<Effect> {
        if let Some(attempt) = &mut self.browse_attempt
            && attempt.state == (BrowseAttemptState::Loading { request_id: id })
        {
            attempt.state = BrowseAttemptState::Failed {
                message: crate::rpc_error_message(error),
            };
            if let Some(selector) = &mut self.ref_selector
                && selector.view.purpose == crate::RefSelectorPurpose::Browse
                && selector.view.repo_id == attempt.target.repo_id
            {
                selector.view.status = crate::selector_error(error);
            }
            return vec![crate::render(&[ViewSection::ReviewList])];
        }
        Vec::new()
    }
}

impl ClientCore {
    pub(crate) fn open_ref_selector(
        &mut self,
        repo_id: RepoId,
        purpose: crate::RefSelectorPurpose,
    ) -> Result<Vec<Effect>, CoreError> {
        self.require_subscribed()?;
        self.require_browse_repo(repo_id)?;
        if purpose == crate::RefSelectorPurpose::Browse && self.view.draft.is_some() {
            return Err(CoreError::DraftAlreadyOpen);
        }
        let review = self.view.review.as_ref().ok_or(CoreError::NoOpenReview)?;
        let target = review
            .snapshot
            .review
            .targets
            .iter()
            .find(|target| target.repo_id == repo_id)
            .ok_or(CoreError::UnknownRepo(repo_id))?;
        let current = match purpose {
            crate::RefSelectorPurpose::Review {
                side: crate::RefSelectorSide::Base,
            } => target.base.clone(),
            crate::RefSelectorPurpose::Review {
                side: crate::RefSelectorSide::Head,
            } => target.head.clone(),
            crate::RefSelectorPurpose::Browse => self
                .browse
                .as_ref()
                .filter(|browse| browse.target.repo_id == repo_id)
                .map_or_else(
                    || target.head.clone(),
                    |browse| browse.target.ref_spec.clone(),
                ),
        };
        let repo_name = self
            .view
            .workspaces
            .iter()
            .find(|workspace| workspace.id == review.snapshot.review.workspace_id)
            .and_then(|workspace| workspace.repos.iter().find(|repo| repo.id == repo_id))
            .map_or_else(|| repo_id.to_string(), |repo| repo.display_name.clone());
        if purpose == crate::RefSelectorPurpose::Browse {
            self.browse_repo = Some(repo_id);
            self.browse_attempt = None;
        }
        self.ref_selector = Some(crate::ref_selector::RefSelector::loading(
            repo_id,
            repo_name,
            purpose,
            current,
            RequestId::new(self.next_request),
        ));
        Ok(vec![self.request(
            Request::ListRefs { repo_id },
            InFlight::ListRefs { repo_id },
        )])
    }

    pub(crate) fn browse_disconnected(&mut self) {
        if let Some(attempt) = &mut self.browse_attempt
            && matches!(attempt.state, BrowseAttemptState::Loading { .. })
        {
            let message = "Daemon disconnected; the previous revision is still visible.".to_owned();
            attempt.state = BrowseAttemptState::Failed {
                message: message.clone(),
            };
            if let Some(selector) = &mut self.ref_selector
                && selector.view.purpose == crate::RefSelectorPurpose::Browse
            {
                selector.view.status = crate::RefSelectorStatus::DaemonError { message };
            }
        }
    }
}
