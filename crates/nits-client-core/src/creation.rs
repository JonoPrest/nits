//! Review creation keeps editable input separate from an immutable submission.
//! A dropped response is reconciled by review identity before another write.

use nits_protocol::{CommitOid, NonEmpty, RefSpec, RepoId, ReviewId, ReviewTarget, WorkspaceId};
use serde::{Deserialize, Serialize};
use strum::EnumDiscriminants;

use crate::DaemonContext;

/// Identifies a target row within one creation attempt, independently of its repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CreationTargetId(u64);

impl CreationTargetId {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// Ordered acknowledgement of draft edits, including target commands and focus.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CreationRevision(u64);

impl CreationRevision {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    fn advance(&mut self) {
        self.0 += 1;
    }
}

/// A field edit cannot replace unrelated target rows that changed in flight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[strum_discriminants(name(CreationEditKind), derive(strum::EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum CreationEdit {
    Title {
        text: String,
    },
    Repository {
        target_id: CreationTargetId,
        repo_id: RepoId,
    },
    Base {
        target_id: CreationTargetId,
        text: String,
    },
    Head {
        target_id: CreationTargetId,
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[strum_discriminants(name(CreationBaseKind), derive(strum::EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum CreationBase {
    Automatic,
    Manual { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreationTarget {
    pub id: CreationTargetId,
    pub repo_id: RepoId,
    pub base: CreationBase,
    pub head: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreationDraft {
    pub title: String,
    pub targets: Vec<CreationTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[strum_discriminants(name(CreationDefaultStateKind), derive(strum::EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum CreationDefaultState {
    Loading,
    Ready { base: RefSpec },
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreationDefault {
    pub repo_id: RepoId,
    pub state: CreationDefaultState,
}

/// Parsed once before sending, then retained unchanged through uncertain replies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreationSubmission {
    pub title: String,
    pub targets: NonEmpty<ReviewTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::EnumIter)]
pub enum CreationReconcile {
    Inspect,
    Retry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::EnumIter)]
pub enum CreationResume {
    Editing,
    Submitted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[strum_discriminants(name(CreationStatusKind), derive(strum::EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum CreationStatus {
    Editing,
    Failed {
        message: String,
    },
    Pending {
        submission: CreationSubmission,
    },
    Interrupted {
        submission: CreationSubmission,
        message: String,
    },
    Reconciling {
        submission: CreationSubmission,
        next: CreationReconcile,
    },
    Succeeded,
}

/// The id is allocated when the form opens, before a browser can lose an ACK.
/// It is also the generation for defaults, form edits and recovery snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewCreation {
    pub review_id: ReviewId,
    pub workspace_id: WorkspaceId,
    pub context: Option<DaemonContext>,
    pub draft: CreationDraft,
    pub defaults: Vec<CreationDefault>,
    pub selected: Option<CreationTargetId>,
    pub revision: CreationRevision,
    pub status: CreationStatus,
}

impl crate::ViewModel {
    /// Only the visible home form owns creation commands. A retained draft must
    /// not capture the composer of an open review, or keys after success.
    pub(crate) fn active_creation(&self) -> Option<&ReviewCreation> {
        self.home.creating.as_ref().filter(|creation| {
            self.open_review.is_none() && creation.status != CreationStatus::Succeeded
        })
    }
}

impl ReviewCreation {
    #[must_use]
    pub fn editable(&self) -> bool {
        match self.status {
            CreationStatus::Editing | CreationStatus::Failed { .. } => true,
            CreationStatus::Pending { .. }
            | CreationStatus::Interrupted { .. }
            | CreationStatus::Reconciling { .. }
            | CreationStatus::Succeeded => false,
        }
    }

    pub(crate) fn submission(&self) -> Result<CreationSubmission, String> {
        let title = self.draft.title.trim().to_owned();
        if title.is_empty() {
            return Err("Enter a review title.".into());
        }
        let targets =
            self.draft
                .targets
                .iter()
                .map(|target| {
                    let base = match &target.base {
                        CreationBase::Manual { text } => parse_ref(text)?,
                        CreationBase::Automatic => match self
                            .defaults
                            .iter()
                            .find(|d| d.repo_id == target.repo_id)
                            .map(|d| &d.state)
                        {
                            Some(CreationDefaultState::Ready { base }) => base.clone(),
                            Some(CreationDefaultState::Failed { message }) => {
                                return Err(format!("Choose a base revision: {message}"));
                            }
                            Some(CreationDefaultState::Loading) | None => return Err(
                                "The default base is still loading. Wait or enter a base revision."
                                    .into(),
                            ),
                        },
                    };
                    Ok(ReviewTarget {
                        repo_id: target.repo_id,
                        base,
                        head: parse_ref(&target.head)?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
        let targets =
            NonEmpty::new(targets).map_err(|_| "Choose at least one repository.".to_owned())?;
        Ok(CreationSubmission { title, targets })
    }
}

/// The existing form's ref syntax, parsed into validated protocol OIDs.
fn parse_ref(text: &str) -> Result<RefSpec, String> {
    let text = text.trim();
    match text.to_ascii_lowercase().as_str() {
        "" => Err("Enter a revision.".into()),
        "worktree" | "working-tree" => Ok(RefSpec::WorkingTree),
        "head" => Ok(RefSpec::Head),
        "upstream" | "@{upstream}" => Ok(RefSpec::Upstream),
        _ => match text.split_once(':') {
            Some(("branch", name)) if !name.is_empty() => Ok(RefSpec::Branch { name: name.into() }),
            Some(("tag", name)) if !name.is_empty() => Ok(RefSpec::Tag { name: name.into() }),
            Some(("commit", oid)) => oid
                .parse::<CommitOid>()
                .map(|oid| RefSpec::Commit { oid })
                .map_err(|error| format!("Invalid commit: {error}")),
            Some(_) => {
                Err("Use a branch, tag:name, commit:<oid>, head, upstream or worktree.".into())
            }
            None => Ok(RefSpec::Branch { name: text.into() }),
        },
    }
}

use crate::{ClientCore, CoreError, Effect, InFlight};
use nits_protocol::{Event, EventBody, Mutation, Request, Review, RpcError, ViewSection};

fn changed() -> Effect {
    crate::render(&[ViewSection::ReviewList])
}

impl ClientCore {
    pub(crate) fn start_creation(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<Effect>, CoreError> {
        if let Some(current) = &self.view.home.creating {
            if !current.editable() && current.status != CreationStatus::Succeeded {
                return Ok(self.show_creation());
            }
            if current.workspace_id == workspace_id && current.status != CreationStatus::Succeeded {
                return Ok(self.show_creation());
            }
        }
        let workspace = self
            .view
            .workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .ok_or(CoreError::NoTarget(crate::NoTarget::Nothing(
                crate::Command::NewReview,
            )))?;
        let targets = workspace
            .repos
            .first()
            .map(|repo| CreationTarget {
                id: CreationTargetId::new(0),
                repo_id: repo.id,
                base: CreationBase::Automatic,
                head: "worktree".into(),
            })
            .into_iter()
            .collect();
        self.view.home.creating = Some(ReviewCreation {
            review_id: self.ids.review_id(self.now),
            workspace_id,
            context: self.view.daemon_context.clone(),
            draft: CreationDraft {
                title: String::new(),
                targets,
            },
            defaults: Vec::new(),
            selected: None,
            revision: CreationRevision::default(),
            status: CreationStatus::Editing,
        });
        let mut effects = self.creation_defaults();
        effects.extend(self.show_creation());
        Ok(effects)
    }

    fn show_creation(&mut self) -> Vec<Effect> {
        self.pending_reference = None;
        self.latest_open = None;
        let mut effects = Vec::new();
        if self.view.review.is_some() {
            self.close_review(&mut effects);
        }
        self.view.focus = crate::Focus::Composer;
        effects.push(crate::render(&[
            ViewSection::ReviewList,
            ViewSection::Focus,
            ViewSection::Draft,
        ]));
        effects
    }

    pub(crate) fn creation_defaults(&mut self) -> Vec<Effect> {
        if !matches!(self.connection, crate::Connection::Subscribed { .. }) {
            return Vec::new();
        }
        let Some(creation) = &mut self.view.home.creating else {
            return Vec::new();
        };
        if !creation.editable() || creation.context != self.view.daemon_context {
            return Vec::new();
        }
        let review_id = creation.review_id;
        let mut needed = Vec::new();
        for target in &creation.draft.targets {
            if !creation
                .defaults
                .iter()
                .any(|d| d.repo_id == target.repo_id)
            {
                creation.defaults.push(CreationDefault {
                    repo_id: target.repo_id,
                    state: CreationDefaultState::Loading,
                });
                needed.push(target.repo_id);
            }
        }
        needed
            .into_iter()
            .map(|repo_id| {
                self.request(
                    Request::DefaultBase { repo_id },
                    InFlight::CreationDefault { review_id, repo_id },
                )
            })
            .collect()
    }

    pub(crate) fn edit_creation_field(
        &mut self,
        review_id: ReviewId,
        edit: CreationEdit,
    ) -> Vec<Effect> {
        let Some(creation) = &mut self.view.home.creating else {
            return Vec::new();
        };
        if creation.review_id != review_id || !creation.editable() {
            return Vec::new();
        }
        creation.revision.advance();
        match edit {
            CreationEdit::Title { text } => creation.draft.title = text,
            CreationEdit::Repository { target_id, repo_id } => {
                if let Some(target) = creation
                    .draft
                    .targets
                    .iter_mut()
                    .find(|t| t.id == target_id)
                {
                    target.repo_id = repo_id;
                    target.base = CreationBase::Automatic;
                }
            }
            CreationEdit::Base { target_id, text } => {
                if let Some(target) = creation
                    .draft
                    .targets
                    .iter_mut()
                    .find(|t| t.id == target_id)
                {
                    target.base = CreationBase::Manual { text };
                }
            }
            CreationEdit::Head { target_id, text } => {
                if let Some(target) = creation
                    .draft
                    .targets
                    .iter_mut()
                    .find(|t| t.id == target_id)
                {
                    target.head = text;
                }
            }
        }
        creation.status = CreationStatus::Editing;
        let mut effects = self.creation_defaults();
        effects.push(changed());
        effects
    }

    pub(crate) fn select_creation_target(
        &mut self,
        review_id: ReviewId,
        target_id: CreationTargetId,
    ) -> Vec<Effect> {
        let Some(creation) = &mut self.view.home.creating else {
            return Vec::new();
        };
        if creation.review_id != review_id || !creation.editable() {
            return Vec::new();
        }
        creation.revision.advance();
        if creation
            .draft
            .targets
            .iter()
            .any(|target| target.id == target_id)
        {
            creation.selected = Some(target_id);
            self.view.focus = crate::Focus::Composer;
        }
        vec![changed()]
    }

    pub(crate) fn add_creation_target(&mut self, review_id: ReviewId) -> Vec<Effect> {
        let Some(creation) = &mut self.view.home.creating else {
            return Vec::new();
        };
        if creation.review_id != review_id || !creation.editable() {
            return Vec::new();
        }
        creation.revision.advance();
        let Some(workspace) = self
            .view
            .workspaces
            .iter()
            .find(|w| w.id == creation.workspace_id)
        else {
            return vec![changed()];
        };
        let Some(repo) = workspace
            .repos
            .iter()
            .find(|repo| !creation.draft.targets.iter().any(|t| t.repo_id == repo.id))
            .or_else(|| workspace.repos.first())
        else {
            return vec![changed()];
        };
        let id = CreationTargetId::new(creation.revision.0);
        creation.selected = Some(id);
        creation.draft.targets.push(CreationTarget {
            id,
            repo_id: repo.id,
            base: CreationBase::Automatic,
            head: "worktree".into(),
        });
        creation.status = CreationStatus::Editing;
        self.view.focus = crate::Focus::Composer;
        let mut effects = self.creation_defaults();
        effects.push(changed());
        effects
    }

    pub(crate) fn remove_creation_target(&mut self, review_id: ReviewId) -> Vec<Effect> {
        let Some(creation) = &mut self.view.home.creating else {
            return Vec::new();
        };
        if creation.review_id != review_id || !creation.editable() {
            return Vec::new();
        }
        creation.revision.advance();
        let Some(index) = creation.selected.and_then(|id| {
            creation
                .draft
                .targets
                .iter()
                .position(|target| target.id == id)
        }) else {
            return vec![changed()];
        };
        creation.draft.targets.remove(index);
        creation.selected = if creation.draft.targets.is_empty() {
            None
        } else {
            Some(creation.draft.targets[index.min(creation.draft.targets.len() - 1)].id)
        };
        creation.status = CreationStatus::Editing;
        vec![changed()]
    }

    pub(crate) fn creation_default_answer(
        &mut self,
        review_id: ReviewId,
        repo_id: RepoId,
        result: Result<RefSpec, String>,
    ) -> Vec<Effect> {
        let Some(creation) = &mut self.view.home.creating else {
            return Vec::new();
        };
        if creation.review_id != review_id || creation.context != self.view.daemon_context {
            return Vec::new();
        }
        // Cache replies even during a manual submission: a rejected attempt
        // can return to automatic defaults without issuing a stale second read.
        // The frozen submission never reads this cache again.
        let Some(default) = creation.defaults.iter_mut().find(|d| d.repo_id == repo_id) else {
            return Vec::new();
        };
        if !matches!(default.state, CreationDefaultState::Loading) {
            return Vec::new();
        }
        default.state = match result {
            Ok(base) => CreationDefaultState::Ready { base },
            Err(message) => CreationDefaultState::Failed { message },
        };
        vec![changed()]
    }

    pub(crate) fn submit_creation(&mut self, review_id: ReviewId) -> Vec<Effect> {
        let Some(creation) = &self.view.home.creating else {
            return Vec::new();
        };
        if creation.review_id != review_id || !creation.editable() {
            return Vec::new();
        }
        let result = if creation.context != self.view.daemon_context {
            Err("This draft belongs to a different daemon context. Return to that context before submitting.".into())
        } else if !matches!(self.connection, crate::Connection::Subscribed { .. }) {
            Err(
                "The daemon is disconnected. Your draft is retained; reconnect and try again."
                    .into(),
            )
        } else if !self.view.workspaces.iter().any(|workspace| {
            workspace.id == creation.workspace_id
                && creation
                    .draft
                    .targets
                    .iter()
                    .all(|target| workspace.repos.iter().any(|repo| repo.id == target.repo_id))
        }) {
            Err("A selected repository is no longer attached to this workspace. Refresh and choose its repositories again.".into())
        } else {
            creation.submission()
        };
        match result {
            Ok(submission) => self.send_creation(review_id, submission),
            Err(message) => self.creation_failure(review_id, message),
        }
    }

    fn send_creation(
        &mut self,
        review_id: ReviewId,
        submission: CreationSubmission,
    ) -> Vec<Effect> {
        let Some(creation) = &mut self.view.home.creating else {
            return Vec::new();
        };
        if creation.review_id != review_id || creation.context != self.view.daemon_context {
            return Vec::new();
        }
        let workspace_id = creation.workspace_id;
        creation.status = CreationStatus::Pending {
            submission: submission.clone(),
        };
        let client_seq = self.next_client_seq;
        self.next_client_seq = client_seq.next();
        vec![
            self.request(
                Request::Mutate {
                    client_seq,
                    mutation: Mutation::CreateReview {
                        review_id,
                        workspace_id,
                        title: submission.title,
                        targets: submission.targets,
                    },
                },
                InFlight::CreateReview {
                    review_id,
                    client_seq,
                },
            ),
            changed(),
        ]
    }

    pub(crate) fn creation_write_failed(
        &mut self,
        review_id: ReviewId,
        error: RpcError,
    ) -> Vec<Effect> {
        let Some(creation) = &mut self.view.home.creating else {
            return Vec::new();
        };
        if creation.review_id != review_id || creation.context != self.view.daemon_context {
            return Vec::new();
        }
        let CreationStatus::Pending { submission } = &creation.status else {
            return Vec::new();
        };
        // A prior write can win between a retry's lookup and its mutation.
        // Read the stable identity after every rejected write; never infer
        // "already exists" or a missing review from daemon error prose.
        creation.status = CreationStatus::Reconciling {
            submission: submission.clone(),
            next: CreationReconcile::Inspect,
        };
        vec![
            self.request(
                Request::GetReview { review_id },
                InFlight::CheckFailedCreation { review_id, error },
            ),
            changed(),
        ]
    }

    pub(crate) fn creation_failure_checked(
        &mut self,
        review_id: ReviewId,
        original: &RpcError,
        lookup: &RpcError,
    ) -> Vec<Effect> {
        if matches!(
            lookup,
            RpcError::NotFound {
                kind: nits_protocol::EntityKind::Review,
                ..
            }
        ) {
            let Some(creation) = &mut self.view.home.creating else {
                return Vec::new();
            };
            if creation.review_id != review_id
                || !matches!(creation.status, CreationStatus::Reconciling { .. })
            {
                return Vec::new();
            }
            creation.status = CreationStatus::Failed {
                message: crate::rpc_error_message(original),
            };
            vec![changed()]
        } else {
            self.creation_lookup_failed(review_id, lookup)
        }
    }

    pub(crate) fn creation_failure(&mut self, review_id: ReviewId, message: String) -> Vec<Effect> {
        let Some(creation) = &mut self.view.home.creating else {
            return Vec::new();
        };
        if creation.review_id != review_id || !creation.editable() {
            return Vec::new();
        }
        creation.status = CreationStatus::Failed { message };
        vec![changed()]
    }

    pub(crate) fn creation_disconnected(&mut self) -> bool {
        let Some(creation) = &mut self.view.home.creating else {
            return false;
        };
        creation
            .defaults
            .retain(|d| !matches!(d.state, CreationDefaultState::Loading));
        let submission = match &creation.status {
            CreationStatus::Pending { submission }
            | CreationStatus::Reconciling { submission, .. } => Some(submission.clone()),
            CreationStatus::Editing
            | CreationStatus::Failed { .. }
            | CreationStatus::Interrupted { .. }
            | CreationStatus::Succeeded => None,
        };
        if let Some(submission) = submission {
            creation.status = CreationStatus::Interrupted { submission, message: "Connection lost. The review may already exist; reconnect to check before retrying.".into() };
        }
        true
    }

    pub(crate) fn reconcile_creation(
        &mut self,
        review_id: ReviewId,
        next: CreationReconcile,
    ) -> Vec<Effect> {
        if !matches!(self.connection, crate::Connection::Subscribed { .. }) {
            return Vec::new();
        }
        let Some(creation) = &mut self.view.home.creating else {
            return Vec::new();
        };
        if creation.review_id != review_id || creation.context != self.view.daemon_context {
            return Vec::new();
        }
        let CreationStatus::Interrupted { submission, .. } = &creation.status else {
            return Vec::new();
        };
        creation.status = CreationStatus::Reconciling {
            submission: submission.clone(),
            next,
        };
        vec![
            self.request(
                Request::GetReview { review_id },
                InFlight::ReconcileCreation { review_id },
            ),
            changed(),
        ]
    }

    pub(crate) fn creation_reconnected(&mut self) -> Vec<Effect> {
        if self.view.home.creating.is_none() {
            return Vec::new();
        }
        let mut effects = self.creation_defaults();
        if let Some(creation) = &self.view.home.creating {
            effects.extend(self.reconcile_creation(creation.review_id, CreationReconcile::Inspect));
        }
        effects.push(changed());
        effects
    }

    pub(crate) fn creation_observe(&mut self, event: &Event) -> bool {
        if let EventBody::ReviewCreated { review } = &event.body {
            return self.creation_found(review);
        }
        false
    }

    pub(crate) fn creation_found(&mut self, review: &Review) -> bool {
        let Some(creation) = &mut self.view.home.creating else {
            return false;
        };
        if creation.review_id != review.id
            || creation.workspace_id != review.workspace_id
            || creation.context != self.view.daemon_context
        {
            return false;
        }
        let submission = match &creation.status {
            CreationStatus::Pending { submission }
            | CreationStatus::Interrupted { submission, .. }
            | CreationStatus::Reconciling { submission, .. } => submission,
            CreationStatus::Editing | CreationStatus::Failed { .. } | CreationStatus::Succeeded => {
                return false;
            }
        };
        if submission.title == review.title && submission.targets == review.targets {
            creation.status = CreationStatus::Succeeded;
        } else {
            creation.status = CreationStatus::Interrupted {
                submission: submission.clone(),
                message: "An existing review has different inputs. Inspect it before retrying."
                    .into(),
            };
        }
        true
    }

    pub(crate) fn creation_lookup_failed(
        &mut self,
        review_id: ReviewId,
        error: &RpcError,
    ) -> Vec<Effect> {
        let Some(creation) = &mut self.view.home.creating else {
            return Vec::new();
        };
        if creation.review_id != review_id {
            return Vec::new();
        }
        let CreationStatus::Reconciling { submission, next } = &creation.status else {
            return Vec::new();
        };
        let submission = submission.clone();
        let missing = matches!(
            error,
            RpcError::NotFound {
                kind: nits_protocol::EntityKind::Review,
                ..
            }
        );
        if missing && *next == CreationReconcile::Retry {
            return self.send_creation(review_id, submission);
        }
        creation.status = CreationStatus::Interrupted {
            submission,
            message: if missing {
                "The review was not found. Retry will check for an existing review before creating one."
                    .into()
            } else {
                format!(
                    "Unable to check the previous attempt: {}",
                    crate::rpc_error_message(error)
                )
            },
        };
        vec![changed()]
    }

    pub(crate) fn restore_creation(
        &mut self,
        mut creation: ReviewCreation,
        resume: CreationResume,
    ) -> Vec<Effect> {
        if creation.context != self.view.daemon_context {
            self.view.last_error = Some(RpcError::Invalid { reason: "The retained review draft belongs to a different daemon context and was not restored.".into() });
            return vec![crate::render(&[ViewSection::Connection])];
        }
        if self.view.home.creating.is_some() {
            return Vec::new();
        }
        if resume == CreationResume::Submitted && creation.editable() {
            creation.status = match creation.submission() {
                Ok(submission) => CreationStatus::Interrupted { submission, message: "Browser connection lost after submit. Checking whether the review was created.".into() },
                Err(message) => CreationStatus::Failed { message },
            };
        }
        self.view.home.creating = Some(creation);
        self.view.focus = crate::Focus::Composer;
        self.creation_disconnected();
        self.creation_reconnected()
    }
}
