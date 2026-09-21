//! Explorer model (plan 3.5, ARCHITECTURE §5.5, §5.6): the merged file tree,
//! breadcrumbs, fuzzy file search, viewed-state derivation and progress
//! counts. Everything here is a pure function of the open review, the
//! cached head trees and the client-local expand/search state — it never
//! produces a request.

use std::collections::{BTreeMap, BTreeSet};

use nits_protocol::{
    Author, ViewedContent, ChangeKind, ChangeKindKind, RenderTarget, RepoId, RepoPath, ReviewSnapshot,
    TreeEntryKind, TreeSnapshot,
};
use serde::{Deserialize, Serialize};
use strum::EnumDiscriminants;

use crate::cache::RenderKey;
use crate::content::FileRef;

/// Whether the current viewer has seen a file at its current head blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, strum::EnumIter)]
pub enum ViewedState {
    Viewed,
    /// Marked viewed at an earlier blob; the file changed since.
    ChangedSinceViewed,
    Unviewed,
}

/// A node of the merged tree. Directories carry their children; the UI
/// renders expanded ones recursively.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[strum_discriminants(name(TreeNodeKind), derive(Hash, strum::EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum TreeNode {
    Dir {
        name: String,
        repo_id: RepoId,
        /// Repo-relative path; `None` for the repo root itself.
        path: Option<RepoPath>,
        expanded: bool,
        /// Changed files anywhere below, so collapsed dirs still show it.
        changed_below: u32,
        children: Vec<TreeNode>,
    },
    File {
        name: String,
        repo_id: RepoId,
        path: RepoPath,
        /// How the file differs in the review; `None` for unchanged files.
        change: Option<ChangeKindKind>,
        viewed: ViewedState,
        /// True for the file the viewport is on.
        open: bool,
        /// Lines added/removed (UI-DESIGN §Layout), once the file's render
        /// header is cached; `None` until then (or for binary files).
        additions: Option<u32>,
        deletions: Option<u32>,
        /// Threads anchored to this file.
        threads: u32,
    },
}

/// One hit of the fuzzy file search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchHit {
    pub file: FileRef,
    /// Byte offsets into the path that matched, for highlighting.
    pub matched: Vec<usize>,
    pub change: Option<ChangeKindKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchView {
    pub query: String,
    pub hits: Vec<SearchHit>,
    /// The highlighted hit (Down/Up step it; Enter opens it).
    #[serde(default)]
    pub selected: usize,
}

/// The explorer as rendered: one root per repo (§5.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TreeView {
    pub roots: Vec<TreeNode>,
    /// Repo root then each path component of the open file.
    pub breadcrumbs: Vec<String>,
    pub search: Option<SearchView>,
}

/// Review-wide counts over the changed files (§5.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub viewed: u32,
    pub changed_since_viewed: u32,
    pub total: u32,
    /// Lines added/removed across the changed files whose render headers
    /// are cached (the header totals, UI-DESIGN §Layout).
    pub additions: u32,
    pub deletions: u32,
}

/// Client-local explorer state that is not derived: what is expanded and
/// what is being searched for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ExplorerState {
    /// Browse-mode dirs the user opened (Browse defaults closed).
    pub(crate) expanded: BTreeSet<(RepoId, Option<RepoPath>)>,
    /// Diffing-mode dirs the user closed (diffing defaults open).
    pub(crate) collapsed: BTreeSet<(RepoId, Option<RepoPath>)>,
    pub(crate) search: Option<String>,
    /// The highlighted search hit (clamped to the hit list on derive).
    pub(crate) search_selected: usize,
}

/// Inputs the tree is derived from.
pub(crate) struct ExplorerInputs<'a> {
    pub(crate) snapshot: &'a ReviewSnapshot,
    /// Repo display names by id, from the workspaces; roots fall back to
    /// the id when a repo is unknown.
    pub(crate) repo_names: &'a [(RepoId, String)],
    /// Head tree per repo, when cached.
    pub(crate) trees: Vec<&'a TreeSnapshot>,
    pub(crate) files: &'a [RenderKey],
    pub(crate) open_file: Option<&'a FileRef>,
    pub(crate) viewer: &'a Author,
    pub(crate) state: &'a ExplorerState,
    /// `(repo, path)` → lines added/removed, from the cached render headers.
    pub(crate) stats: &'a BTreeMap<(RepoId, String), (u32, u32)>,
    /// `(repo, path)` → threads anchored to the file.
    pub(crate) thread_counts: &'a BTreeMap<(RepoId, String), u32>,
    /// Diffing mode (UI-DESIGN §Layout): the tree lists only the changed
    /// files, every directory expanded; the full tree belongs to Browse.
    /// Search still spans the head trees either way.
    pub(crate) changed_only: bool,
}

/// The change for `file`, if it is in the review's file list.
fn change_of<'a>(
    files: &'a [RenderKey],
    repo_id: RepoId,
    path: &RepoPath,
) -> Option<&'a ChangeKind> {
    files
        .iter()
        .find(|k| k.repo_id == repo_id && k.path == *path)
        .and_then(|k| match &k.target {
            RenderTarget::Diff { change } => Some(change),
            RenderTarget::Blob { .. } => None,
        })
}

/// Viewed state of `file` at `head_content` for `viewer`. Agents never mark
/// files, so for an agent everything is `Unviewed`.
#[must_use]
pub fn viewed_state(
    snapshot: &ReviewSnapshot,
    viewer: &Author,
    repo_id: RepoId,
    path: &RepoPath,
    head_content: ViewedContent,
) -> ViewedState {
    let Some(human) = viewer.as_human() else {
        return ViewedState::Unviewed;
    };
    match snapshot
        .viewed
        .iter()
        .find(|v| v.repo_id == repo_id && v.path == *path && v.viewer == human)
    {
        None => ViewedState::Unviewed,
        Some(mark) if mark.content == head_content => ViewedState::Viewed,
        Some(_) => ViewedState::ChangedSinceViewed,
    }
}

/// Progress over the changed files.
#[must_use]
pub(crate) fn progress(
    snapshot: &ReviewSnapshot,
    viewer: &Author,
    files: &[RenderKey],
    stats: &BTreeMap<(RepoId, String), (u32, u32)>,
) -> Progress {
    let mut p = Progress::default();
    for f in files {
        if let Some((a, d)) = stats.get(&(f.repo_id, f.path.as_str().to_owned())) {
            p.additions += a;
            p.deletions += d;
        }
        let head = match &f.target {
            RenderTarget::Diff { change } => change.viewed_content(),
            RenderTarget::Blob { oid } => ViewedContent::Blob { oid: *oid },
        };
        p.total += 1;
        match viewed_state(snapshot, viewer, f.repo_id, &f.path, head) {
            ViewedState::Viewed => p.viewed += 1,
            ViewedState::ChangedSinceViewed => p.changed_since_viewed += 1,
            ViewedState::Unviewed => {}
        }
    }
    p
}

/// A file the tree shows: from the head tree, or from the file list when
/// it only exists in base (deleted).
struct Leaf {
    path: RepoPath,
    head_content: ViewedContent,
}

/// Build the whole explorer view.
pub(crate) fn build(inputs: &ExplorerInputs<'_>) -> TreeView {
    let mut repos: BTreeMap<RepoId, Vec<Leaf>> = BTreeMap::new();
    if inputs.changed_only {
        // Seed the repo roots so a review with files still shows its repos.
        for f in inputs.files {
            repos.entry(f.repo_id).or_default();
        }
    }
    for t in if inputs.changed_only {
        &[][..]
    } else {
        &inputs.trees
    } {
        let leaves = repos.entry(t.repo_id).or_default();
        for e in &t.entries {
            match &e.kind {
                TreeEntryKind::File { oid, .. } | TreeEntryKind::Symlink { oid } => {
                    leaves.push(Leaf {
                        path: e.path.clone(),
                        head_content: ViewedContent::Blob { oid: *oid },
                    });
                }
                TreeEntryKind::Dir { .. } | TreeEntryKind::Submodule { .. } => {}
            }
        }
    }
    for f in inputs.files {
        let leaves = repos.entry(f.repo_id).or_default();
        if leaves.iter().all(|l| l.path != f.path) {
            let head_content = match &f.target {
                RenderTarget::Diff { change } => change.viewed_content(),
                RenderTarget::Blob { oid } => ViewedContent::Blob { oid: *oid },
            };
            leaves.push(Leaf {
                path: f.path.clone(),
                head_content,
            });
        }
    }
    let roots = repos
        .into_iter()
        .map(|(repo_id, mut leaves)| {
            leaves.sort_by(|a, b| a.path.cmp(&b.path));
            let children = nest(inputs, repo_id, &leaves, None);
            let changed_below = count_changed(&children);
            TreeNode::Dir {
                name: inputs
                    .repo_names
                    .iter()
                    .find(|(id, _)| *id == repo_id)
                    .map_or_else(|| repo_id.to_string(), |(_, n)| n.clone()),
                repo_id,
                path: None,
                expanded: if inputs.changed_only {
                    !inputs.state.collapsed.contains(&(repo_id, None))
                } else {
                    inputs.state.expanded.contains(&(repo_id, None))
                },
                changed_below,
                children,
            }
        })
        .collect();
    let breadcrumbs = inputs.open_file.map_or_else(Vec::new, |f| {
        let root = inputs
            .repo_names
            .iter()
            .find(|(id, _)| *id == f.repo_id)
            .map_or_else(|| f.repo_id.to_string(), |(_, n)| n.clone());
        std::iter::once(root)
            .chain(f.path.components().map(str::to_owned))
            .collect()
    });
    let search = inputs.state.search.as_ref().map(|q| {
        let hits = search(inputs, q);
        SearchView {
            query: q.clone(),
            selected: inputs
                .state
                .search_selected
                .min(hits.len().saturating_sub(1)),
            hits,
        }
    });
    TreeView {
        roots,
        breadcrumbs,
        search,
    }
}

/// Children of `dir` (None = repo root), from the sorted leaves below it.
fn nest(
    inputs: &ExplorerInputs<'_>,
    repo_id: RepoId,
    leaves: &[Leaf],
    dir: Option<&RepoPath>,
) -> Vec<TreeNode> {
    let prefix = dir.map_or(String::new(), |d| format!("{d}/"));
    let mut out = Vec::new();
    let mut seen_dirs = BTreeSet::new();
    for leaf in leaves {
        let Some(rest) = leaf.path.as_str().strip_prefix(&prefix) else {
            continue;
        };
        if let Some((child_dir, _)) = rest.split_once('/') {
            if !seen_dirs.insert(child_dir.to_owned()) {
                continue;
            }
            let Ok(child_path) = RepoPath::new(format!("{prefix}{child_dir}")) else {
                continue;
            };
            let children = nest(inputs, repo_id, leaves, Some(&child_path));
            let changed_below = count_changed(&children);
            out.push(TreeNode::Dir {
                name: child_dir.to_owned(),
                repo_id,
                expanded: if inputs.changed_only {
                    !inputs
                        .state
                        .collapsed
                        .contains(&(repo_id, Some(child_path.clone())))
                } else {
                    inputs
                        .state
                        .expanded
                        .contains(&(repo_id, Some(child_path.clone())))
                },
                path: Some(child_path),
                changed_below,
                children,
            });
        } else {
            let change = change_of(inputs.files, repo_id, &leaf.path);
            let key = (repo_id, leaf.path.as_str().to_owned());
            let stats = inputs.stats.get(&key).copied();
            out.push(TreeNode::File {
                name: rest.to_owned(),
                repo_id,
                change: change.map(ChangeKindKind::from),
                viewed: viewed_state(
                    inputs.snapshot,
                    inputs.viewer,
                    repo_id,
                    &leaf.path,
                    leaf.head_content,
                ),
                open: inputs
                    .open_file
                    .is_some_and(|f| f.repo_id == repo_id && f.path == leaf.path),
                additions: stats.map(|(a, _)| a),
                deletions: stats.map(|(_, d)| d),
                threads: inputs.thread_counts.get(&key).copied().unwrap_or(0),
                path: leaf.path.clone(),
            });
        }
    }
    // Directories first, then files, each alphabetical (git order otherwise
    // interleaves them).
    out.sort_by(|a, b| {
        let rank = |n: &TreeNode| match n {
            TreeNode::Dir { name, .. } => (0, name.clone()),
            TreeNode::File { name, .. } => (1, name.clone()),
        };
        rank(a).cmp(&rank(b))
    });
    out
}

fn count_changed(nodes: &[TreeNode]) -> u32 {
    nodes
        .iter()
        .map(|n| match n {
            TreeNode::Dir { changed_below, .. } => *changed_below,
            TreeNode::File { change, .. } => u32::from(change.is_some()),
        })
        .sum()
}

/// Fuzzy path search over the changed files first, then everything in the
/// head trees: case-insensitive subsequence match, scored by contiguity
/// and closeness to the file name; at most [`MAX_HITS`] hits.
pub const MAX_HITS: usize = 50;

fn search(inputs: &ExplorerInputs<'_>, query: &str) -> Vec<SearchHit> {
    let query: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    if query.is_empty() {
        return Vec::new();
    }
    let mut candidates: Vec<(RepoId, RepoPath, Option<ChangeKindKind>)> = inputs
        .files
        .iter()
        .map(|f| {
            (
                f.repo_id,
                f.path.clone(),
                match &f.target {
                    RenderTarget::Diff { change } => Some(ChangeKindKind::from(change)),
                    RenderTarget::Blob { .. } => None,
                },
            )
        })
        .collect();
    for t in &inputs.trees {
        for e in &t.entries {
            if matches!(
                e.kind,
                TreeEntryKind::File { .. } | TreeEntryKind::Symlink { .. }
            ) && !candidates
                .iter()
                .any(|(r, p, _)| *r == t.repo_id && *p == e.path)
            {
                candidates.push((t.repo_id, e.path.clone(), None));
            }
        }
    }
    let mut scored: Vec<(i64, SearchHit)> = candidates
        .into_iter()
        .filter_map(|(repo_id, path, change)| {
            let (score, matched) = fuzzy(path.as_str(), &query)?;
            // Changed files rank above unchanged ones at equal score.
            let score = score + i64::from(change.is_some()) * 1_000;
            Some((
                score,
                SearchHit {
                    file: FileRef { repo_id, path },
                    matched,
                    change,
                },
            ))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.file.path.cmp(&b.1.file.path))
    });
    scored.into_iter().take(MAX_HITS).map(|(_, h)| h).collect()
}

/// Subsequence match of `query` (lowercase chars) in `text`; returns a
/// score and the byte offsets matched. Higher is better.
fn fuzzy(text: &str, query: &[char]) -> Option<(i64, Vec<usize>)> {
    let mut matched = Vec::with_capacity(query.len());
    let mut score: i64 = 0;
    let mut qi = 0;
    let mut prev_hit = false;
    let file_name_start = text.rfind('/').map_or(0, |i| i + 1);
    for (offset, ch) in text.char_indices() {
        if qi == query.len() {
            break;
        }
        let lower: Vec<char> = ch.to_lowercase().collect();
        if lower.first() == Some(&query[qi]) {
            matched.push(offset);
            qi += 1;
            score += 10;
            if prev_hit {
                score += 15; // contiguous run
            }
            if offset >= file_name_start {
                score += 5; // in the file name
            }
            if offset == 0 || text.as_bytes()[offset - 1] == b'/' {
                score += 8; // component start
            }
            prev_hit = true;
        } else {
            prev_hit = false;
        }
    }
    if qi < query.len() {
        return None;
    }
    // Shorter paths win ties.
    score -= i64::try_from(text.len()).unwrap_or(i64::MAX) / 4;
    Some((score, matched))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_is_greedy_and_scores_component_starts_and_contiguity() {
        let q: Vec<char> = "srl".chars().collect();
        let (a, m) = fuzzy("src/render/lib.rs", &q).unwrap();
        assert_eq!(m, vec![0, 1, 11]);
        let (b, _) = fuzzy("assets/really/long/name.rs", &q).unwrap();
        assert!(a > b, "{a} vs {b}");
        assert!(fuzzy("nope", &q).is_none());
        // `search` lowercases the query; the text side is folded here.
        let q: Vec<char> = "lib".chars().collect();
        assert!(fuzzy("src/LIB.rs", &q).is_some(), "case-insensitive");
    }
}
