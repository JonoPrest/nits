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

/// A durable invitation to review. Requests carry no approval or finding lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReviewRequest {
    pub id: crate::ReviewRequestId,
    pub review_id: ReviewId,
    pub requester: Author,
    pub recipient: String,
    pub targets: RequestedTargets,
    pub note: String,
    pub created: Timestamp,
}

impl ReviewRequest {
    /// Derive the same record for historical replay, live delivery, and view rebuilds.
    #[must_use]
    pub fn from_event(event: &crate::Event) -> Option<Self> {
        if let crate::EventBody::ReviewRequested {
            review_id,
            agent,
            note,
            targets,
        } = &event.body
        {
            Some(Self {
                id: crate::ReviewRequestId::from_event_seq(event.seq),
                review_id: *review_id,
                requester: event.author.clone(),
                recipient: agent.clone(),
                targets: targets.clone(),
                note: note.clone(),
                created: event.ts,
            })
        } else {
            None
        }
    }
}

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
    /// Real git tree OID, including immutable working-tree snapshots.
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
        /// HEAD at capture time. Absent for unborn branches and historical
        /// snapshots made before HEAD provenance was recorded; never inferred
        /// from the checkout's current HEAD when reading an old snapshot.
        #[serde(default)]
        head: Option<CommitOid>,
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

/// The content a comment was written against. Browse keeps its anchor's
/// immutable blob even if this human-readable ref or the review targets move.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(CommentContextKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum CommentContext {
    Diff { change: ChangeKind },
    Browse { reference: RefSpec },
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
    /// The diff pair or Browse revision on screen when the comment was made.
    /// Readers reopen the exact content after refs move. `None` for
    /// review-level comments and clients that do not record provenance.
    pub context: Option<CommentContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(ThreadResolutionKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ThreadResolution {
    /// Conversation without an actionable lifecycle; cannot be resolved, deferred or reopened.
    Informational,
    Open,
    /// A real, unfixed finding acknowledged outside the current review scope.
    Deferred {
        reason: crate::DeferralReason,
        tracking_url: Option<crate::TrackingUrl>,
        by: Author,
        at: Timestamp,
    },
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

/// Content identity of a reviewed tree entry. Git stores a gitlink as a commit
/// OID, not a blob; a missing entry identifies a deletion explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(ViewedContentKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ViewedContent {
    Missing,
    Blob { oid: BlobOid },
    Submodule { commit: CommitOid },
}

impl From<Option<BlobOid>> for ViewedContent {
    fn from(blob: Option<BlobOid>) -> Self {
        blob.map_or(Self::Missing, |oid| Self::Blob { oid })
    }
}

/// A human marked a file as viewed at a specific head identity. Agents cannot set
/// these; the type says so by carrying a [`Human`], not an [`Author`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ViewedMark {
    pub review_id: ReviewId,
    pub repo_id: RepoId,
    pub path: RepoPath,
    pub viewer: Human,
    pub content: ViewedContent,
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
    Commit {
        repo_id: RepoId,
        oid: CommitOid,
    },
    /// The working tree against the checked-out commit — the final
    /// by-commit step — in one repo.
    Worktree {
        repo_id: RepoId,
    },
    Requested {
        request_id: crate::ReviewRequestId,
    },
    SinceCheckpoint {
        checkpoint_id: crate::ReviewCheckpointId,
    },
}

/// A change involving a gitlink. Gitlink OIDs name commits in the submodule,
/// which need not exist in the superproject object database.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(SubmoduleChangeKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum SubmoduleChange {
    Added { new: CommitOid },
    Deleted { old: CommitOid },
    Updated { old: CommitOid, new: CommitOid },
    Renamed { from: RepoPath, old: CommitOid, new: CommitOid },
    BlobToSubmodule { old: BlobOid, new: CommitOid },
    SubmoduleToBlob { old: CommitOid, new: BlobOid },
}

impl SubmoduleChange {
    pub fn old_blob(&self) -> Option<BlobOid> {
        match self {
            Self::BlobToSubmodule { old, .. } => Some(*old),
            Self::Added { .. } | Self::Deleted { .. } | Self::Updated { .. }
            | Self::Renamed { .. } | Self::SubmoduleToBlob { .. } => None,
        }
    }

    pub fn new_blob(&self) -> Option<BlobOid> {
        match self {
            Self::SubmoduleToBlob { new, .. } => Some(*new),
            Self::Added { .. } | Self::Deleted { .. } | Self::Updated { .. }
            | Self::Renamed { .. } | Self::BlobToSubmodule { .. } => None,
        }
    }

    pub fn viewed_content(&self) -> ViewedContent {
        match self {
            Self::Deleted { .. } => ViewedContent::Missing,
            Self::SubmoduleToBlob { new, .. } => ViewedContent::Blob { oid: *new },
            Self::Added { new } | Self::Updated { new, .. } | Self::Renamed { new, .. }
            | Self::BlobToSubmodule { new, .. } => ViewedContent::Submodule { commit: *new },
        }
    }
}

/// How a file differs between base and head. Carries exactly the entries that
/// exist, so there is no `Option<old> + Option<new>` pair to keep consistent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
// The discriminant is a view type too (explorer "change" badge), hence serde
// on it; unit-only, so it is a bare string on the wire.
#[strum_discriminants(name(ChangeKindKind), derive(EnumIter, Hash, Serialize, Deserialize))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ChangeKind {
    Submodule { change: SubmoduleChange },
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
    pub fn viewed_content(&self) -> ViewedContent {
        match self {
            Self::Submodule { change } => change.viewed_content(),
            Self::Deleted { .. } => ViewedContent::Missing,
            Self::Added { new } | Self::Modified { new, .. } | Self::Renamed { new, .. } =>
                ViewedContent::Blob { oid: *new },
        }
    }

    #[must_use]
    pub fn old_blob(&self) -> Option<BlobOid> {
        match self {
            ChangeKind::Submodule { change } => change.old_blob(),
            ChangeKind::Added { .. } => None,
            ChangeKind::Deleted { old }
            | ChangeKind::Modified { old, .. }
            | ChangeKind::Renamed { old, .. } => Some(*old),
        }
    }

    #[must_use]
    pub fn new_blob(&self) -> Option<BlobOid> {
        match self {
            ChangeKind::Submodule { change } => change.new_blob(),
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

/// Historical requests before revision capture retain explicitly unknown provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", deny_unknown_fields)]
#[strum_discriminants(name(RequestedTargetsKind), derive(EnumIter, Hash))]
pub enum RequestedTargets {
    Unknown,
    Captured { targets: NonEmpty<ResolvedTarget> },
}

/// Stable reviewer identity across restarts. Full session provenance remains on the checkpoint.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, EnumDiscriminants,
)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", deny_unknown_fields)]
#[strum_discriminants(name(ReviewerIdentityKind), derive(EnumIter, Hash))]
pub enum ReviewerIdentity {
    Human { name: String, machine: String },
    Agent { name: String },
}
impl ReviewerIdentity {
    #[must_use]
    pub fn from_author(author: &Author) -> Option<Self> {
        match author {
            Author::Human { name, machine } => Some(Self::Human {
                name: name.clone(),
                machine: machine.clone(),
            }),
            Author::Agent { name, .. } => Some(Self::Agent { name: name.clone() }),
            Author::Daemon { .. } => None,
        }
    }
}

/// The durable round this check answers; both identities belong to this review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", deny_unknown_fields)]
#[strum_discriminants(name(ReviewRoundKind), derive(EnumIter, Hash))]
pub enum ReviewRound {
    Request {
        request_id: crate::ReviewRequestId,
    },
    Checkpoint {
        checkpoint_id: crate::ReviewCheckpointId,
    },
}

/// An attributed record of exactly what was checked. This conveys no approval or resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReviewCheckpoint {
    pub id: crate::ReviewCheckpointId,
    pub review_id: ReviewId,
    pub reviewer: ReviewerIdentity,
    pub author: Author,
    pub created: Timestamp,
    pub targets: NonEmpty<ResolvedTarget>,
    pub in_reply_to: Option<ReviewRound>,
}
impl ReviewCheckpoint {
    #[must_use]
    pub fn from_event(event: &crate::Event) -> Option<Self> {
        if let crate::EventBody::ReviewChecked {
            review_id,
            reviewer,
            targets,
            in_reply_to,
        } = &event.body
        {
            Some(Self {
                id: crate::ReviewCheckpointId::from_event_seq(event.seq),
                review_id: *review_id,
                reviewer: reviewer.clone(),
                author: event.author.clone(),
                created: event.ts,
                targets: targets.clone(),
                in_reply_to: *in_reply_to,
            })
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, EnumIter)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CheckpointFreshness {
    Current,
    Changed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReviewerCheckpoint {
    pub checkpoint: ReviewCheckpoint,
    pub freshness: CheckpointFreshness,
}

/// Latest committed check per stable reviewer, compared with the current resolved identities.
#[must_use]
pub fn latest_checkpoints(
    checkpoints: &[ReviewCheckpoint],
    current: Option<&NonEmpty<ResolvedTarget>>,
) -> Vec<ReviewerCheckpoint> {
    let mut latest = std::collections::BTreeMap::new();
    for checkpoint in checkpoints {
        let entry = latest
            .entry(checkpoint.reviewer.clone())
            .or_insert(checkpoint);
        if checkpoint.id > entry.id {
            *entry = checkpoint;
        }
    }
    latest
        .into_values()
        .map(|checkpoint| ReviewerCheckpoint {
            checkpoint: checkpoint.clone(),
            freshness: match current {
                None => CheckpointFreshness::Unknown,
                Some(current) if same_targets(&checkpoint.targets, current) => {
                    CheckpointFreshness::Current
                }
                Some(_) => CheckpointFreshness::Changed,
            },
        })
        .collect()
}

/// Compare repository and revision identities, ignoring incidental working-tree dirty-path metadata.
#[must_use]
pub fn same_targets(a: &NonEmpty<ResolvedTarget>, b: &NonEmpty<ResolvedTarget>) -> bool {
    fn same_ref(a: &ResolvedRef, b: &ResolvedRef) -> bool {
        a.tree == b.tree
            && match (&a.source, &b.source) {
                (ResolvedSource::Commit { oid: a }, ResolvedSource::Commit { oid: b }) => a == b,
                (ResolvedSource::WorkingTree { .. }, ResolvedSource::WorkingTree { .. }) => true,
                (ResolvedSource::Commit { .. }, ResolvedSource::WorkingTree { .. })
                | (ResolvedSource::WorkingTree { .. }, ResolvedSource::Commit { .. }) => false,
            }
    }
    a.len() == b.len()
        && a.iter().all(|a| {
            b.iter().any(|b| {
                a.repo_id == b.repo_id && same_ref(&a.base, &b.base) && same_ref(&a.head, &b.head)
            })
        })
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
