//! Git-backed base/head selector state. The daemon supplies the catalog;
//! this module owns filtering and selection so every host behaves identically.

use nits_protocol::{BaseRefSpec, RefCandidate, RefSpec, RepoId, RequestId, TargetRevision};
use serde::{Deserialize, Serialize};
use strum::EnumDiscriminants;

/// Which side of a review target the selector edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::EnumIter)]
pub enum RefSelectorSide {
    Base,
    Head,
}

/// Selecting a review target is a mutation; Browse only resolves a tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[strum_discriminants(name(RefSelectorPurposeKind), derive(Hash, strum::EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum RefSelectorPurpose {
    Review { side: RefSelectorSide },
    Browse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RefSelection {
    Review { revision: TargetRevision },
    Browse { ref_spec: RefSpec },
}

/// The asynchronous state shown inside the selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[strum_discriminants(name(RefSelectorStatusKind), derive(strum::EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum RefSelectorStatus {
    Loading,
    Ready,
    Saving,
    InvalidRef { message: String },
    DaemonError { message: String },
}

/// One filtered selector row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefOption {
    pub ref_spec: RefSpec,
    pub subject: Option<String>,
    pub current: bool,
}

/// Everything a host needs to render the open selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefSelectorView {
    /// Catalog request identity also scopes a host's local query buffer.
    pub request_id: RequestId,
    pub repo_id: RepoId,
    pub repo_name: String,
    pub purpose: RefSelectorPurpose,
    pub current: RefSpec,
    pub query: String,
    pub options: Vec<RefOption>,
    pub selected: usize,
    pub status: RefSelectorStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RefSelector {
    catalog: Vec<RefCandidate>,
    pub(crate) view: RefSelectorView,
}

impl RefSelector {
    pub(crate) fn loading(
        repo_id: RepoId,
        repo_name: String,
        purpose: RefSelectorPurpose,
        current: RefSpec,
        request_id: RequestId,
    ) -> Self {
        Self {
            catalog: Vec::new(),
            view: RefSelectorView {
                request_id,
                repo_id,
                repo_name,
                purpose,
                current,
                query: String::new(),
                options: Vec::new(),
                selected: 0,
                status: RefSelectorStatus::Loading,
            },
        }
    }

    pub(crate) fn install(&mut self, mut catalog: Vec<RefCandidate>) {
        if !catalog
            .iter()
            .any(|candidate| candidate.ref_spec == self.view.current)
        {
            catalog.insert(
                0,
                RefCandidate {
                    ref_spec: self.view.current.clone(),
                    subject: None,
                },
            );
        }
        self.catalog = catalog;
        self.view.status = RefSelectorStatus::Ready;
        self.filter();
    }

    pub(crate) fn query(&mut self, query: String) {
        self.view.query = query;
        self.filter();
    }

    pub(crate) fn step(&mut self, delta: i32) {
        let len = self.view.options.len();
        if len == 0 {
            self.view.selected = 0;
            return;
        }
        let current = self.view.selected.min(len - 1);
        let amount = usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX);
        self.view.selected = if delta.is_negative() {
            current.saturating_sub(amount)
        } else {
            current.saturating_add(amount).min(len - 1)
        };
    }

    pub(crate) fn matches_request(&self, repo_id: RepoId, request_id: RequestId) -> bool {
        self.view.repo_id == repo_id && self.view.request_id == request_id
    }

    pub(crate) fn selection_at(&self, index: usize) -> Option<RefSelection> {
        let option = self.view.options.get(index)?;
        match self.view.purpose {
            RefSelectorPurpose::Browse => Some(RefSelection::Browse {
                ref_spec: option.ref_spec.clone(),
            }),
            RefSelectorPurpose::Review { side } => {
                let revision = match side {
                    RefSelectorSide::Head => TargetRevision::Head {
                        ref_spec: option.ref_spec.clone(),
                    },
                    RefSelectorSide::Base => TargetRevision::Base {
                        ref_spec: base_ref(&option.ref_spec)?,
                    },
                };
                Some(RefSelection::Review { revision })
            }
        }
    }

    fn filter(&mut self) {
        if self.view.purpose == RefSelectorPurpose::Browse {
            match literal_query(&self.view.query) {
                LiteralQuery::Ref(ref_spec) => {
                    let subject = self
                        .catalog
                        .iter()
                        .find(|candidate| candidate.ref_spec == ref_spec)
                        .and_then(|candidate| candidate.subject.clone());
                    self.view.options = vec![RefOption {
                        current: ref_spec == self.view.current,
                        ref_spec,
                        subject,
                    }];
                    self.view.selected = 0;
                    return;
                }
                LiteralQuery::Invalid => {
                    self.view.options.clear();
                    self.view.selected = 0;
                    return;
                }
                LiteralQuery::Search => {}
            }
        }
        let query = self.view.query.trim().to_lowercase();
        self.view.options = self
            .catalog
            .iter()
            .filter(
                |candidate| match (&self.view.purpose, &candidate.ref_spec) {
                    (
                        RefSelectorPurpose::Review {
                            side: RefSelectorSide::Base,
                        },
                        RefSpec::WorkingTree,
                    ) => false,
                    (RefSelectorPurpose::Review { .. } | RefSelectorPurpose::Browse, _) => true,
                },
            )
            .filter(|candidate| {
                query.is_empty()
                    || fuzzy_match(&search_text(candidate).to_lowercase(), query.as_str())
            })
            .map(|candidate| RefOption {
                ref_spec: candidate.ref_spec.clone(),
                subject: candidate.subject.clone(),
                current: candidate.ref_spec == self.view.current,
            })
            .collect();
        if self.view.purpose == RefSelectorPurpose::Browse
            && let Some(ref_spec) = named_query(&self.view.query)
            && !self
                .view
                .options
                .iter()
                .any(|option| option.ref_spec == ref_spec)
        {
            self.view.options.push(RefOption {
                current: ref_spec == self.view.current,
                ref_spec,
                subject: Some("Resolve this revision".into()),
            });
        }
        self.view.selected = 0;
    }
}

#[derive(Debug, PartialEq, Eq)]
enum LiteralQuery {
    Search,
    Ref(RefSpec),
    Invalid,
}

// Explicit syntax is a literal request, never a fuzzy catalog search. In
// particular, a commit subject containing "branch:main" cannot take precedence
// over that branch. Invalid explicit OIDs never fall back to subject matching.
fn literal_query(query: &str) -> LiteralQuery {
    let query = query.trim();
    let special = match query.to_lowercase().as_str() {
        "worktree" | "working-tree" => Some(RefSpec::WorkingTree),
        "head" => Some(RefSpec::Head),
        "upstream" | "@{upstream}" => Some(RefSpec::Upstream),
        _ => None,
    };
    if let Some(ref_spec) = special {
        return LiteralQuery::Ref(ref_spec);
    }
    let Some((kind, value)) = query.split_once(':') else {
        return LiteralQuery::Search;
    };
    if !matches!(kind, "branch" | "tag" | "commit") {
        return LiteralQuery::Search;
    }
    if value.is_empty() || query.chars().any(char::is_whitespace) {
        return LiteralQuery::Invalid;
    }
    match kind {
        "branch" => LiteralQuery::Ref(RefSpec::Branch { name: value.into() }),
        "tag" => LiteralQuery::Ref(RefSpec::Tag { name: value.into() }),
        "commit" => match value.parse() {
            Ok(oid) => LiteralQuery::Ref(RefSpec::Commit { oid }),
            Err(_) => LiteralQuery::Invalid,
        },
        _ => LiteralQuery::Search,
    }
}

// Bare names still search the catalog, with a resolution row for refs outside it.
fn named_query(query: &str) -> Option<RefSpec> {
    let query = query.trim();
    if query.is_empty() || query.contains(':') || query.chars().any(char::is_whitespace) {
        return None;
    }
    query.parse().ok()
}

fn base_ref(spec: &RefSpec) -> Option<BaseRefSpec> {
    match spec {
        RefSpec::Branch { name } => Some(BaseRefSpec::Branch { name: name.clone() }),
        RefSpec::Commit { oid } => Some(BaseRefSpec::Commit { oid: *oid }),
        RefSpec::Tag { name } => Some(BaseRefSpec::Tag { name: name.clone() }),
        RefSpec::Revision { expression } => Some(BaseRefSpec::Revision {
            expression: expression.clone(),
        }),
        RefSpec::Upstream => Some(BaseRefSpec::Upstream),
        RefSpec::Head => Some(BaseRefSpec::Head),
        RefSpec::WorkingTree => None,
    }
}

fn search_text(candidate: &RefCandidate) -> String {
    let (kind, value) = match &candidate.ref_spec {
        RefSpec::Branch { name } => ("branch", name.clone()),
        RefSpec::Commit { oid } => ("commit", oid.to_string()),
        RefSpec::Tag { name } => ("tag", name.clone()),
        RefSpec::Revision { expression } => ("revision", expression.to_string()),
        RefSpec::WorkingTree => ("working tree", String::new()),
        RefSpec::Upstream => ("upstream", String::new()),
        RefSpec::Head => ("head", String::new()),
    };
    format!(
        "{kind} {value} {}",
        candidate.subject.as_deref().unwrap_or("")
    )
}

fn fuzzy_match(haystack: &str, needle: &str) -> bool {
    let mut chars = needle.chars();
    let mut wanted = chars.next();
    for candidate in haystack.chars() {
        if wanted == Some(candidate) {
            wanted = chars.next();
        }
    }
    wanted.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_id() -> RepoId {
        RepoId::from_parts(1, 1)
    }

    #[test]
    fn fuzzy_match_is_an_ordered_subsequence() {
        assert!(fuzzy_match("branch feature/ref-selector", "frs"));
        assert!(!fuzzy_match("branch feature/ref-selector", "srf"));
    }

    #[test]
    fn working_tree_is_offered_only_for_head() {
        let catalog = vec![RefCandidate {
            ref_spec: RefSpec::WorkingTree,
            subject: None,
        }];
        let mut base = RefSelector::loading(
            repo_id(),
            "repo".into(),
            RefSelectorPurpose::Review {
                side: RefSelectorSide::Base,
            },
            RefSpec::Head,
            RequestId::new(1),
        );
        base.install(catalog.clone());
        assert!(
            base.view
                .options
                .iter()
                .all(|option| { !matches!(option.ref_spec, RefSpec::WorkingTree) })
        );

        let mut head = RefSelector::loading(
            repo_id(),
            "repo".into(),
            RefSelectorPurpose::Review {
                side: RefSelectorSide::Head,
            },
            RefSpec::Head,
            RequestId::new(2),
        );
        head.install(catalog);
        assert!(
            head.view
                .options
                .iter()
                .any(|option| { matches!(option.ref_spec, RefSpec::WorkingTree) })
        );
    }
}
