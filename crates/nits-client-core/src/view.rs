//! What the UI renders. Hosts read it after each `Effect::Render`, which
//! names the sections that changed.

use nits_protocol::{
    Anchor, ClientSeq, DiffScope, EventBody, RenderOpts, RepoPath, ResolvedTarget, Review,
    ReviewId, ReviewSnapshot, RpcError, Side, ThreadId, TreeOid, ViewSection, Workspace,
};

use crate::cache::RenderKey;
use crate::diff::{CommitStepper, DiffView, ThreadView};
use crate::explorer::{Progress, TreeView};
use crate::focus::Focus;
use crate::keymap::{HelpView, Hint};
use crate::ref_selector::RefSelectorView;
use serde::{Deserialize, Serialize};
use strum::EnumDiscriminants;

/// The sections of the [`ViewModel`] that changed, in first-touched order,
/// without duplicates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewDelta {
    pub sections: Vec<ViewSection>,
}

impl ViewDelta {
    #[must_use]
    pub fn new(sections: &[ViewSection]) -> Self {
        let mut out: Vec<ViewSection> = Vec::with_capacity(sections.len());
        for s in sections {
            if !out.contains(s) {
                out.push(*s);
            }
        }
        Self { sections: out }
    }
}

/// Connection state as the UI shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants, Default)]
#[strum_discriminants(name(ConnectionViewKind), derive(Hash, strum::EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ConnectionView {
    #[default]
    Disconnected,
    Connecting,
    Subscribed,
    /// The daemon refused the handshake; shown until the next `Connect`.
    Rejected {
        error: RpcError,
    },
}

/// An editor is open at `anchor`. Its purpose distinguishes new comments,
/// replies and finding deferrals; text stays in the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    /// Rejected wire input leaves the editor open for correction. Text stays in the host.
    pub submission_error: Option<String>,
    pub anchor: Anchor,
    pub purpose: DraftPurpose,
}

/// The editor's operation; thread actions always carry a target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, strum::EnumDiscriminants)]
#[strum_discriminants(name(DraftPurposeKind), derive(Hash, strum::EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum DraftPurpose {
    Comment {
        intent: nits_protocol::CommentIntent,
        /// Capture provenance when opening the editor, independent of later navigation.
        context: Option<nits_protocol::CommentContext>,
    },
    Reply {
        thread_id: ThreadId,
    },
    Defer {
        thread_id: ThreadId,
    },
}

/// The file the user is looking at and where. Rows are indices into the
/// render model; the core turns them into chunk requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenFile {
    pub render: RenderKey,
    pub first_row: u32,
    pub last_row: u32,
}

/// A mutation this client applied locally that the daemon has not echoed
/// yet. Hosts mark what `body` touches (a comment, a thread) as pending.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingEvent {
    pub client_seq: ClientSeq,
    pub body: EventBody,
}

/// The review currently on screen. Content (trees, headers, chunks) lives in
/// the cache; this names what of it belongs to the review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenReview {
    /// Committed state plus every pending mutation applied on top (§5.2).
    pub snapshot: ReviewSnapshot,
    pub pending: Vec<PendingEvent>,
    /// Tree roots of the resolved targets (base and head per repo).
    pub trees: Vec<TreeOid>,
    /// Changed files, in daemon order; one render key each.
    pub files: Vec<RenderKey>,
    pub open_file: Option<OpenFile>,
    /// Which diff of the review is on screen (UI-DESIGN §Diff scope).
    #[serde(default)]
    pub scope: DiffScope,
    /// A comment's recorded original diff is open read-only (UI-DESIGN
    /// §Comments: jump-to-context); `open_file.render` equals this while
    /// the mode is on.
    #[serde(default)]
    pub original: Option<RenderKey>,
    /// What `scope` resolved to, from the last `Files` answer; empty until
    /// answered (and always empty for `All`, whose targets are the
    /// snapshot's).
    #[serde(default)]
    pub scoped_targets: Vec<ResolvedTarget>,
}

impl OpenReview {
    /// The retained historical source, only while it is the open viewport.
    /// Current files can share its path without sharing its navigation state.
    #[must_use]
    pub fn original_render(&self) -> Option<&RenderKey> {
        self.original.as_ref().filter(|original| {
            self.open_file
                .as_ref()
                .is_some_and(|file| file.render == **original)
        })
    }

    /// Historical diffs are read-only. A pinned Browse blob retains its
    /// captured source and supports composition against that exact blob.
    #[must_use]
    pub fn original_is_read_only(&self) -> bool {
        self.original_render()
            .is_some_and(|render| match render.target {
                nits_protocol::RenderTarget::Diff { .. } => true,
                nits_protocol::RenderTarget::Blob { .. } => false,
            })
    }

    #[must_use]
    pub fn new(snapshot: ReviewSnapshot) -> Self {
        Self {
            snapshot,
            pending: Vec::new(),
            trees: Vec::new(),
            files: Vec::new(),
            open_file: None,
            scope: DiffScope::All,
            scoped_targets: Vec::new(),
            original: None,
        }
    }

    /// The targets of the current scope: the snapshot's resolved targets
    /// for `All`, else what the last scoped `Files` answer reported.
    #[must_use]
    pub fn current_targets(&self) -> Vec<ResolvedTarget> {
        match self.scope {
            DiffScope::All => self
                .snapshot
                .resolved
                .as_ref()
                .map(|r| r.iter().cloned().collect())
                .unwrap_or_default(),
            DiffScope::Committed
            | DiffScope::Commit { .. }
            | DiffScope::Worktree { .. }
            | DiffScope::Requested { .. }
            | DiffScope::SinceCheckpoint { .. } => self.scoped_targets.clone(),
        }
    }
}

/// Which center tab is shown (UI-DESIGN §layout): keys `1`/`2`/`3`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default, strum::EnumIter,
)]
pub enum Tab {
    #[default]
    FilesChanged,
    Conversation,
    Browse,
}

/// How diff rows are laid out.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default, strum::EnumIter,
)]
pub enum Layout {
    #[default]
    Unified,
    Split,
}

/// User preferences, persisted in the host KV under [`ViewPrefs::KEY`].
/// `ignore_whitespace` / `context_lines` are the render options every
/// request uses, so changing them re-keys the render cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewPrefs {
    pub layout: Layout,
    pub ignore_whitespace: bool,
    pub context_lines: u32,
    /// Whole sidebar hidden (`<leader>b`).
    #[serde(default)]
    pub sidebar_hidden: bool,
}

impl ViewPrefs {
    /// Host KV key the preferences live under.
    pub const KEY: &'static str = "nits/prefs";

    #[must_use]
    pub fn render_opts(self) -> RenderOpts {
        RenderOpts {
            ignore_whitespace: self.ignore_whitespace,
            context_lines: self.context_lines,
            // Expansions belong to one file, not to the preferences.
            expanded: nits_protocol::Expansions::default(),
        }
    }
}

impl Default for ViewPrefs {
    fn default() -> Self {
        let opts = RenderOpts::default();
        Self {
            layout: Layout::Unified,
            ignore_whitespace: opts.ignore_whitespace,
            context_lines: opts.context_lines,
            sidebar_hidden: false,
        }
    }
}

/// Where the host should put a row of the diff. Vertical motion keeps the
/// focused row on screen by itself; these are the explicit repositions
/// (`z z`/`z t`/`z b`), which move the view and leave the cursor put.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::EnumIter)]
pub enum ScrollAlign {
    /// The row to the middle of the viewport (`z z`).
    Center,
    /// The row to the top (`z t`).
    Top,
    /// The row to the bottom (`z b`).
    Bottom,
}

/// How the view should land on a file being opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::EnumIter)]
pub enum Landing {
    /// A continuous motion walked over the file boundary (`j` at the last
    /// row): the view scrolls by as little as it can, like any other
    /// motion. Re-anchoring here would throw away the lines the reader
    /// was looking at.
    Follow,
    /// A deliberate jump to a distant file (the tree, `] f`, file find):
    /// the file's header goes to the top of the viewport.
    Pin,
}

/// A reposition the host has not performed yet. `seq` counts the
/// instructions, so pressing `z z` again after scrolling away by mouse is
/// a new one even though the row and alignment repeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScrollIntent {
    pub row: u32,
    pub align: ScrollAlign,
    pub seq: u32,
}

/// The core's verdict on one key: which key (keys it acts on are counted
/// in order from 1) and what it resolved to. `None` means the key meant
/// nothing where it landed — a cancelled sequence, or a command whose
/// context had moved on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LastKey {
    pub seq: u64,
    pub command: Option<crate::keymap::Command>,
}

/// The Visual-mode line selection (UI-DESIGN: modal keys): row indices of
/// the open file, ordered, both ends inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualView {
    pub start: u32,
    pub end: u32,
    /// The half of the rows the selection is on: hosts highlight that
    /// cell, and `c` anchors the comment there.
    pub side: Side,
}

/// The content-search palette (UI-DESIGN §Search): query, scope toggle
/// and the daemon's hits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentSearchView {
    pub query: String,
    /// Search every file of the head trees, not just the changed ones.
    pub all_files: bool,
    pub hits: Vec<nits_protocol::ContentHit>,
    /// The hit cap was reached; more matches exist.
    pub truncated: bool,
    /// A query is on its way to the daemon.
    pub pending: bool,
    /// The highlighted hit (Down/Up step it; Enter opens it).
    #[serde(default)]
    pub selected: usize,
}

/// Everything the UI needs, kept identical across hosts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ViewModel {
    pub home: crate::HomeView,
    pub daemon_context: Option<crate::DaemonContext>,
    pub active_repo: Option<nits_protocol::RepoId>,
    /// Absolute path on the selected daemon, never a repository-relative `RepoPath`.
    pub copy_checkout: Option<String>,
    pub prefs: ViewPrefs,
    /// Explorer over the open review's head trees (§5.5); empty otherwise.
    pub tree: TreeView,
    pub progress: Progress,
    /// The open file over its viewport, with comment overlays (§6.5).
    pub diff: Option<DiffView>,
    /// The stacked diffs (UI-DESIGN §Layout): one per changed file, rows
    /// from whatever chunks are cached; empty without an open review.
    pub diffs: Vec<DiffView>,
    /// Every thread of the open review, oldest first.
    pub threads: Vec<ThreadView>,
    /// Review-level threads (the conversation panel).
    pub conversation: Vec<ThreadView>,
    /// Durable review invitations, separate from finding and note threads.
    pub requests: Vec<nits_protocol::ReviewRequest>,
    pub checkpoints: Vec<nits_protocol::ReviewerCheckpoint>,
    /// Current target and displayed content have finished their refresh.
    pub check_current_ready: bool,
    pub stepper: Option<CommitStepper>,
    /// Where keys go (§6.4). Always valid for the current lists.
    pub focus: Focus,
    /// Which center tab is shown (`1`/`2`/`3`).
    pub tab: Tab,
    /// The last `z z`/`z t`/`z b`, for the host to act on (§6.4: the
    /// viewport follows core state, the DOM never decides where to be).
    pub scroll: Option<ScrollIntent>,
    /// Vim-style mode (UI-DESIGN: modal keys): Insert while a text
    /// editor owns the keys, Normal otherwise. The hint bar shows it.
    pub mode: crate::keymap::Mode,
    /// Which-key label for the pending group (`Leader`, `Go`), when the
    /// pending prefix has one configured.
    pub pending_label: Option<String>,
    /// Primary bindings for the focused context, for the hint bar — or,
    /// while a leader is pending, the pending group's keys.
    pub hints: Vec<Hint>,
    /// The pending leader sequence in text form (`"g"`), empty when none.
    /// The hint bar shows it as the mode indicator.
    pub pending_keys: String,
    /// The leader chord in text form (`"space"`), for the footer chip.
    pub leader: String,
    /// One hint per bound command, for button tooltips (derived from the
    /// keymap, never hand-written).
    pub chrome: Vec<Hint>,
    /// Every binding that applies where the focus is, aliases included.
    /// A host that must decide what a key means before the core answers
    /// resolves against this rather than guessing from `hints` (primary
    /// only) or `chrome` (one per command, no context).
    pub bindings: Vec<Hint>,
    /// What the core made of the last key it acted on. A host that must
    /// act on a key before the core answers (the clipboard needs the
    /// gesture that asked for it) compares `seq` against what it has
    /// sent — equal means this view accounts for every key — and reads
    /// `command` rather than predicting one: the context a key lands in
    /// is the one the core moved to, which the host cannot know until it
    /// is told.
    pub last_key: Option<LastKey>,
    /// The `?` overlay while open.
    pub help: Option<HelpView>,
    /// What `y` would copy from where the focus is (UI-DESIGN §Chrome).
    /// The clipboard is the shell's and a write only works inside the
    /// gesture that asked for it, so the shell reads this and copies
    /// during the key or click rather than after a round trip — the core
    /// still owns *which file* that is.
    pub copy_target: Option<RepoPath>,
    /// Portable reference selected by the core for the clipboard gesture.
    pub copy_reference: Option<nits_protocol::ReviewReference>,
    /// Exact reply to highlight and scroll into view in the expanded thread.
    pub focused_comment: Option<nits_protocol::CommentId>,
    pub connection: ConnectionView,
    /// Last request error the daemon returned; cleared on (re)subscribe.
    pub last_error: Option<RpcError>,
    /// Workspaces and their repos, listed on subscribe; the review list is
    /// the union of every workspace's reviews.
    pub workspaces: Vec<Workspace>,
    pub reviews: Vec<Review>,
    /// Which of `reviews` is open, for headers (the review itself is in
    /// `review`, which is never pushed to the UI).
    pub open_review: Option<ReviewId>,
    /// The open review's resolved base/head per repo (branch names,
    /// dirty paths) under the current scope; empty until the daemon has
    /// resolved.
    pub resolved_targets: Vec<ResolvedTarget>,
    /// Which diff of the open review is on screen (UI-DESIGN §Diff scope);
    /// `All` when no review is open.
    #[serde(default)]
    pub scope: DiffScope,
    /// The Browse tab's picked ref (UI-DESIGN §Browse); `None` browses the
    /// review's head trees.
    #[serde(default)]
    pub browse_ref: Option<nits_protocol::RefSpec>,
    /// The content-search palette while open (UI-DESIGN §Search).
    #[serde(default)]
    pub content_search: Option<ContentSearchView>,
    /// The actions palette (`:`) while open; its entries are `chrome`.
    #[serde(default)]
    pub action_palette: bool,
    /// Base/head selector while open.
    #[serde(default)]
    pub ref_selector: Option<RefSelectorView>,
    /// Rows of the open file selected in Visual mode (`V`), when it is on.
    #[serde(default)]
    pub visual: Option<VisualView>,
    pub review: Option<OpenReview>,
    pub draft: Option<Draft>,
    /// A working-tree refresh arrived while `draft` was open and is being
    /// held back (§5.4).
    pub pending_refresh: bool,
}
