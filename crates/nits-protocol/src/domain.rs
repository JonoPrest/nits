//! Domain model: workspaces, repos, reviews, comments, anchors, authors.
//!
//! See `docs/ARCHITECTURE.md` §4.4.

use serde::{Deserialize, Serialize};
use strum::{EnumDiscriminants, EnumIter};

use crate::ids::{
    BlobOid, CommentId, CommitOid, RepoId, ReviewId, ThreadId, Timestamp, TreeOid, WorkspaceId,
};
use crate::invariants::{LineRange, NonEmpty, RepoPath};
use crate::render::ExpandDir;

/// A named group of repositories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    pub repos: Vec<Repo>,
}

/// A git repository attached to a workspace.
///
/// `path` is the absolute path on the daemon's machine. It is a `String`
/// rather than `PathBuf` because clients (possibly on another machine, or in a
/// browser) only display it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Repo {
    pub id: RepoId,
    pub path: String,
    pub display_name: String,
}

/// What the user asked to review — unresolved. See [`ResolvedRef`] for the
/// resolved form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(RefSpecKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum RefSpec {
    Branch {
        name: String,
    },
    Commit {
        oid: CommitOid,
    },
    Tag {
        name: String,
    },
    WorkingTree,
    /// The upstream of the current branch (`@{upstream}`).
    Upstream,
    /// `HEAD`.
    Head,
}

/// A revision valid on the base side of a review target. A working tree is
/// deliberately unrepresentable here: it has no stable parent-side content.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(BaseRefSpecKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum BaseRefSpec {
    Branch { name: String },
    Commit { oid: CommitOid },
    Tag { name: String },
    Upstream,
    Head,
}

impl From<BaseRefSpec> for RefSpec {
    fn from(spec: BaseRefSpec) -> Self {
        match spec {
            BaseRefSpec::Branch { name } => Self::Branch { name },
            BaseRefSpec::Commit { oid } => Self::Commit { oid },
            BaseRefSpec::Tag { name } => Self::Tag { name },
            BaseRefSpec::Upstream => Self::Upstream,
            BaseRefSpec::Head => Self::Head,
        }
    }
}

/// Which half of one repository's review target is being replaced.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(TargetRevisionKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum TargetRevision {
    Base { ref_spec: BaseRefSpec },
    Head { ref_spec: RefSpec },
}

/// A typed edit of one repo within a possibly multi-repo review.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReviewTargetUpdate {
    pub repo_id: RepoId,
    pub revision: TargetRevision,
}

/// One revision offered by the daemon's git-backed selector catalog.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RefCandidate {
    pub ref_spec: RefSpec,
    /// The commit subject for recent-commit candidates; absent for named
    /// refs and the working tree.
    pub subject: Option<String>,
}

/// A resolved ref: the tree it points at, plus what kind of thing it was
/// resolved from. Every resolved ref has a tree, so that lives outside the
/// enum; only the parts that differ are variants.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ResolvedRef {
    /// Real tree OID for commits; a synthetic id over hashed working files
    /// for the working tree.
    pub tree: TreeOid,
    pub source: ResolvedSource,
}

/// What a [`ResolvedRef`] was resolved from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(ResolvedSourceKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ResolvedSource {
    Commit {
        oid: CommitOid,
    },
    /// The working tree at the moment of resolution; `dirty` lists paths
    /// that differ from `HEAD`, `branch` is the checked-out branch (None
    /// on a detached HEAD).
    WorkingTree {
        dirty: Vec<RepoPath>,
        branch: Option<String>,
    },
}

/// One repo's base/head pair within a review, as requested.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReviewTarget {
    pub repo_id: RepoId,
    pub base: RefSpec,
    pub head: RefSpec,
}

/// One repo's base/head pair within a review, resolved to content.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ResolvedTarget {
    pub repo_id: RepoId,
    pub base: ResolvedRef,
    pub head: ResolvedRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumIter)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ReviewStatus {
    Open,
    Archived,
}

/// A review: a set of targets across a workspace's repos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub id: ReviewId,
    pub workspace_id: WorkspaceId,
    pub title: String,
    pub targets: NonEmpty<ReviewTarget>,
    pub created: Timestamp,
    pub status: ReviewStatus,
}

/// A git signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Sig {
    pub name: String,
    pub email: String,
    pub time: Timestamp,
    /// Timezone offset from UTC in minutes, as recorded in the commit.
    pub offset_minutes: i32,
}

/// A commit, as shown in the commit stepper panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CommitInfo {
    pub oid: CommitOid,
    pub parents: Vec<CommitOid>,
    pub tree: TreeOid,
    pub author: Sig,
    pub committer: Sig,
    /// First paragraph of the message.
    pub subject: String,
    /// Everything after the subject, trimmed. Empty if none.
    pub body: String,
}

/// A human, as recorded on events and marks.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Human {
    pub name: String,
    /// Hostname of the machine the action was taken on.
    pub machine: String,
}

/// How an agent reached the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumIter)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum AgentVia {
    Mcp,
    Cli,
}

/// Who did something. Provenance is structured, never a free-text tag.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(AuthorKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Author {
    Human {
        name: String,
        machine: String,
    },
    Agent {
        name: String,
        model: String,
        session_id: String,
        invoked_by: Option<Human>,
        via: AgentVia,
    },
    /// The daemon itself, e.g. re-resolving targets after a file change.
    Daemon {
        machine: String,
    },
}

impl Author {
    #[must_use]
    pub fn human(h: Human) -> Self {
        Author::Human {
            name: h.name,
            machine: h.machine,
        }
    }

    #[must_use]
    pub fn as_human(&self) -> Option<Human> {
        match self {
            Author::Human { name, machine } => Some(Human {
                name: name.clone(),
                machine: machine.clone(),
            }),
            Author::Agent { .. } | Author::Daemon { .. } => None,
        }
    }
}

/// Which side of a diff a line anchor refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumIter)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Side {
    Base,
    Head,
}

impl Side {
    /// The other half of the same row: base ↔ head.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Side::Base => Side::Head,
            Side::Head => Side::Base,
        }
    }
}

/// Hash of the ±3 lines surrounding an anchored range, used to detect that a
/// mapped anchor still points at the same content. Serialised as 16 hex chars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(try_from = "String", into = "String")]
pub struct ContextHash(u64);

impl ContextHash {
    #[must_use]
    pub const fn new(h: u64) -> Self {
        Self(h)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl TryFrom<String> for ContextHash {
    type Error = String;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        if s.len() != 16 {
            return Err(format!("context hash must be 16 hex chars, got {s:?}"));
        }
        u64::from_str_radix(&s, 16)
            .map(Self)
            .map_err(|e| format!("invalid context hash {s:?}: {e}"))
    }
}

impl From<ContextHash> for String {
    fn from(h: ContextHash) -> Self {
        format!("{:016x}", h.0)
    }
}

/// Where a comment is anchored. Anchors reference blobs, never diffs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(AnchorKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Anchor {
    /// Review-level; no file.
    Review,
    /// A whole file at a specific blob. Need not be in the diff.
    File {
        repo_id: RepoId,
        path: RepoPath,
        blob_oid: BlobOid,
    },
    /// A line range within a specific blob.
    Lines {
        repo_id: RepoId,
        path: RepoPath,
        side: Side,
        blob_oid: BlobOid,
        lines: LineRange,
        context_hash: ContextHash,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(CommentKindKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum CommentKind {
    /// Plain text. A root opens an actionable finding, including review-wide
    /// findings; replies retain the existing thread's lifecycle.
    Note,
    /// An informational review root: no approval, resolution, or readiness implication.
    Informational,
    /// A unified diff against the anchored blob that a client can apply.
    Suggestion { patch: String },
    /// Asks an agent (or human) to act.
    Request,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(CommentStateKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum CommentState {
    Live,
    /// The content moved or changed such that the anchor could not be mapped.
    /// The comment is still shown (collapsed) at its last good anchor.
    Outdated {
        last_good_anchor: Anchor,
    },
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Comment {
    pub id: CommentId,
    pub review_id: ReviewId,
    pub thread_id: ThreadId,
    pub author: Author,
    pub kind: CommentKind,
    pub anchor: Anchor,
    pub body: String,
    pub created: Timestamp,
    pub edited: Option<Timestamp>,
    pub state: CommentState,
    /// The file diff (blob pair) on screen when the comment was made, so
    /// a reader can jump back to that exact rendering even after the
    /// review's head moved. `None` for review-level comments and clients
    /// that don't track it.
    pub context: Option<ChangeKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(ThreadResolutionKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ThreadResolution {
    /// Conversation without an actionable lifecycle; cannot be resolved or reopened.
    Informational,
    Open,
    Resolved {
        by: Author,
        at: Timestamp,
    },
}

impl ThreadResolution {
    /// A root's kind determines whether its thread has an actionable lifecycle.
    #[must_use]
    pub fn for_root(kind: &CommentKind) -> Self {
        match kind {
            CommentKind::Informational => Self::Informational,
            CommentKind::Note | CommentKind::Suggestion { .. } | CommentKind::Request => Self::Open,
        }
    }
}

/// Intent for new plain-text threads. Existing callers default to findings.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, strum::EnumIter,
)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CommentIntent {
    #[default]
    Finding,
    Informational,
}

impl From<CommentIntent> for CommentKind {
    fn from(intent: CommentIntent) -> Self {
        match intent {
            CommentIntent::Finding => Self::Note,
            CommentIntent::Informational => Self::Informational,
        }
    }
}

/// A comment thread. Its id equals the root comment's id; replies share the
/// root's anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Thread {
    pub id: ThreadId,
    pub review_id: ReviewId,
    pub root: CommentId,
    pub replies: Vec<CommentId>,
    pub resolution: ThreadResolution,
}

/// A human marked a file as viewed at a specific head blob. Agents cannot set
/// these; the type says so by carrying a [`Human`], not an [`Author`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ViewedMark {
    pub review_id: ReviewId,
    pub repo_id: RepoId,
    pub path: RepoPath,
    pub viewer: Human,
    /// Head blob at the time of marking; `None` for a file deleted in head.
    pub blob_oid: Option<BlobOid>,
}

/// Which hidden run: gap `i` is the run before visible group `i`, and
/// the last one is the run after the final group. Groups are decided by
/// `context_lines` alone, so a gap keeps its number however far it is
/// opened — but not across a change of `context_lines`, which can merge
/// groups and renumber everything after them.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct Gap(u32);

impl Gap {
    #[must_use]
    pub const fn new(i: u32) -> Self {
        Self(i)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// How far one hidden run has been pushed apart. `up` is revealed going
/// up from the hunk below the gap, `down` going down from the hunk above
/// it, in lines; the render clamps both to what the gap actually holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GapExpansion {
    pub gap: Gap,
    pub up: u32,
    pub down: u32,
}

/// Wire input that is not a canonical expansion set.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExpansionsError {
    #[error("gap {0:?} appears more than once")]
    Duplicate(Gap),
    #[error("expansions are not ordered by gap")]
    Unordered,
}

/// The gaps a render has opened: at most one entry per gap, ordered by
/// gap. Parsed, not validated — two requests that mean the same thing
/// must not become two cache keys, so a non-canonical set is rejected at
/// the boundary rather than silently accepted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(try_from = "Vec<GapExpansion>", into = "Vec<GapExpansion>")]
pub struct Expansions(Vec<GapExpansion>);

impl TryFrom<Vec<GapExpansion>> for Expansions {
    type Error = ExpansionsError;

    fn try_from(v: Vec<GapExpansion>) -> Result<Self, Self::Error> {
        for w in v.windows(2) {
            if w[0].gap == w[1].gap {
                return Err(ExpansionsError::Duplicate(w[0].gap));
            }
            if w[0].gap > w[1].gap {
                return Err(ExpansionsError::Unordered);
            }
        }
        Ok(Self(v))
    }
}

impl From<Expansions> for Vec<GapExpansion> {
    fn from(e: Expansions) -> Self {
        e.0
    }
}

impl Expansions {
    #[must_use]
    pub fn as_slice(&self) -> &[GapExpansion] {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// What `gap` has been opened to, `(up, down)`, zero when untouched.
    #[must_use]
    pub fn of(&self, gap: Gap) -> (u32, u32) {
        self.0
            .iter()
            .find(|e| e.gap == gap)
            .map_or((0, 0), |e| (e.up, e.down))
    }

    /// Open `gap` by `step` more lines in `dir`, keeping the entries
    /// ordered and unique.
    #[must_use]
    pub fn opened(&self, gap: Gap, dir: ExpandDir, step: u32) -> Self {
        let (up, down) = self.of(gap);
        let (up, down) = match dir {
            ExpandDir::Up => (up.saturating_add(step), down),
            ExpandDir::Down => (up, down.saturating_add(step)),
            ExpandDir::Both => (up.saturating_add(step), down.saturating_add(step)),
        };
        let mut out: Vec<GapExpansion> = self.0.iter().filter(|e| e.gap != gap).copied().collect();
        out.push(GapExpansion { gap, up, down });
        out.sort_by_key(|e| e.gap);
        Self(out)
    }
}

/// Options that change the render model; part of every render cache key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RenderOpts {
    pub ignore_whitespace: bool,
    pub context_lines: u32,
    /// Hidden runs the reader has opened, one gap at a time (`enter` on an
    /// expander, `z u`/`z d`). Widening `context_lines` instead would move
    /// every other hunk in the file.
    #[serde(default)]
    pub expanded: Expansions,
}

impl Default for RenderOpts {
    fn default() -> Self {
        Self {
            ignore_whitespace: false,
            context_lines: 3,
            expanded: Expansions::default(),
        }
    }
}

/// Which diff of a review is being asked about (UI-DESIGN §Diff scope).
/// `All` is the review's targets as resolved; the others narrow the pair
/// per repo. Requests carry it with `#[serde(default)]`, so an absent
/// scope means `All`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default, EnumDiscriminants,
)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(DiffScopeKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum DiffScope {
    /// `base → head` as the review resolves them.
    #[default]
    All,
    /// Like `All`, but a working-tree head stops at its checked-out commit
    /// (the `+ working tree` toggle off). Targets without a working-tree
    /// head are unchanged.
    Committed,
    /// One commit against its first parent, in one repo; the review's
    /// other targets drop out of the file list.
    Commit { repo_id: RepoId, oid: CommitOid },
    /// The working tree against the checked-out commit — the final
    /// by-commit step — in one repo.
    Worktree { repo_id: RepoId },
}

/// How a file differs between base and head. Carries exactly the blobs that
/// exist, so there is no `Option<old> + Option<new>` pair to keep consistent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
// The discriminant is a view type too (explorer "change" badge), hence serde
// on it; unit-only, so it is a bare string on the wire.
#[strum_discriminants(name(ChangeKindKind), derive(EnumIter, Hash, Serialize, Deserialize))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ChangeKind {
    Added {
        new: BlobOid,
    },
    Deleted {
        old: BlobOid,
    },
    Modified {
        old: BlobOid,
        new: BlobOid,
    },
    Renamed {
        from: RepoPath,
        old: BlobOid,
        new: BlobOid,
    },
}

impl ChangeKind {
    #[must_use]
    pub fn old_blob(&self) -> Option<BlobOid> {
        match self {
            ChangeKind::Added { .. } => None,
            ChangeKind::Deleted { old }
            | ChangeKind::Modified { old, .. }
            | ChangeKind::Renamed { old, .. } => Some(*old),
        }
    }

    #[must_use]
    pub fn new_blob(&self) -> Option<BlobOid> {
        match self {
            ChangeKind::Deleted { .. } => None,
            ChangeKind::Added { new }
            | ChangeKind::Modified { new, .. }
            | ChangeKind::Renamed { new, .. } => Some(*new),
        }
    }
}

/// One content-search match (UI-DESIGN §Search).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ContentHit {
    pub repo_id: RepoId,
    pub path: RepoPath,
    /// 1-based line of the match, on the searched side.
    pub line: crate::invariants::LineNo,
    /// The matching line's text, trimmed of the trailing newline.
    pub text: String,
}

/// A changed file in one repo. `path` is the head-side path (for renames,
/// the destination).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FileChange {
    pub repo_id: RepoId,
    pub path: RepoPath,
    pub kind: ChangeKind,
}

/// What a tree entry points at.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(TreeEntryKindKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum TreeEntryKind {
    File {
        oid: BlobOid,
        size: u64,
        executable: bool,
    },
    Dir {
        oid: TreeOid,
    },
    Symlink {
        oid: BlobOid,
    },
    Submodule {
        commit: CommitOid,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct TreeEntry {
    pub path: RepoPath,
    pub kind: TreeEntryKind,
}

/// Full recursive listing of a tree; flat, sorted by path, one pass to nest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct TreeSnapshot {
    pub repo_id: RepoId,
    pub root_oid: TreeOid,
    pub entries: Vec<TreeEntry>,
}

/// Difference between two tree snapshots (used for working-tree refs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct TreeDelta {
    pub repo_id: RepoId,
    pub from_root: TreeOid,
    pub to_root: TreeOid,
    pub added: Vec<TreeEntry>,
    pub removed: Vec<RepoPath>,
    pub changed: Vec<TreeEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gap(i: u32, up: u32, down: u32) -> GapExpansion {
        GapExpansion {
            gap: Gap::new(i),
            up,
            down,
        }
    }

    #[test]
    fn an_expansion_set_is_parsed_into_its_canonical_shape_or_rejected() {
        // Two requests that mean the same thing must not become two cache
        // keys, so the boundary refuses anything but the canonical order.
        let ok = Expansions::try_from(vec![gap(0, 20, 0), gap(2, 0, 20)]).unwrap();
        assert_eq!(ok.of(Gap::new(0)), (20, 0));
        assert_eq!(ok.of(Gap::new(2)), (0, 20));
        assert_eq!(ok.of(Gap::new(1)), (0, 0), "an untouched gap is closed");

        assert_eq!(
            Expansions::try_from(vec![gap(1, 20, 0), gap(1, 0, 20)]),
            Err(ExpansionsError::Duplicate(Gap::new(1)))
        );
        assert_eq!(
            Expansions::try_from(vec![gap(2, 20, 0), gap(1, 0, 20)]),
            Err(ExpansionsError::Unordered)
        );

        // The same refusal off the wire, where it matters.
        let wire = r#"[{"gap":1,"up":20,"down":0},{"gap":1,"up":0,"down":20}]"#;
        assert!(serde_json::from_str::<Expansions>(wire).is_err());
    }

    #[test]
    fn opening_a_gap_keeps_the_set_canonical() {
        let e = Expansions::default()
            .opened(Gap::new(2), ExpandDir::Up, 20)
            .opened(Gap::new(0), ExpandDir::Both, 20)
            .opened(Gap::new(2), ExpandDir::Up, 20);
        assert_eq!(
            e.as_slice().iter().map(|g| g.gap).collect::<Vec<_>>(),
            vec![Gap::new(0), Gap::new(2)],
            "ordered, one entry per gap"
        );
        assert_eq!(e.of(Gap::new(2)), (40, 0), "repeat opens further");
        assert_eq!(e.of(Gap::new(0)), (20, 20));
        // Round-trips through the wire unchanged.
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<Expansions>(&json).unwrap(), e);
    }
}
