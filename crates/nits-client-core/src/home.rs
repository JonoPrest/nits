//! Workspace inventory and home navigation, shared by every client shell.
use nits_protocol::{RepoId, ReviewId, WorkspaceId};
use serde::{Deserialize, Serialize};
use strum::EnumDiscriminants;

use crate::{Focus, ViewModel};

/// Identity of one navigable home row. Repository membership is independent
/// of review targets; each review remains visible when its inventory is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[strum_discriminants(name(HomeRowKindKind), derive(strum::EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum HomeRowKind {
    Workspace,
    Repository { repo_id: RepoId },
    Review { review_id: ReviewId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeRow {
    pub workspace_id: WorkspaceId,
    pub kind: HomeRowKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeView {
    pub rows: Vec<HomeRow>,
    pub selected_workspace: Option<WorkspaceId>,
    pub expanded: Vec<WorkspaceId>,
    /// Editable inputs and the correlated creation attempt survive daemon errors.
    pub creating: Option<crate::ReviewCreation>,
}

/// The actual selected daemon, provided by the host. A browser bridge's HTTP
/// address is never a substitute for the daemon owning these checkout paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[strum_discriminants(name(DaemonContextKind), derive(strum::EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum DaemonContext {
    Named { name: String },
    Socket { path: String },
    WebSocket { url: String },
}

pub(crate) fn rows(view: &ViewModel) -> Vec<HomeRow> {
    let mut rows = Vec::new();
    for workspace in &view.workspaces {
        let workspace_id = workspace.id;
        rows.push(HomeRow {
            workspace_id,
            kind: HomeRowKind::Workspace,
        });
        if view.home.expanded.contains(&workspace_id) {
            rows.extend(workspace.repos.iter().map(|repo| HomeRow {
                workspace_id,
                kind: HomeRowKind::Repository { repo_id: repo.id },
            }));
        }
        rows.extend(
            view.reviews
                .iter()
                .filter(|review| review.workspace_id == workspace_id)
                .map(|review| HomeRow {
                    workspace_id,
                    kind: HomeRowKind::Review {
                        review_id: review.id,
                    },
                }),
        );
    }
    rows
}

pub(crate) fn focused(view: &ViewModel) -> Option<HomeRow> {
    match view.focus {
        Focus::ReviewList { index } => view.home.rows.get(index).copied(),
        Focus::Tree { .. }
        | Focus::Diff { .. }
        | Focus::Thread { .. }
        | Focus::ReviewRequest { .. }
        | Focus::Composer
        | Focus::CommitStepper { .. }
        | Focus::Help => None,
    }
}
