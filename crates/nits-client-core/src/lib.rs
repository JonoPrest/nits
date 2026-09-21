//! Sans-I/O client core (plan milestone 3, `docs/ARCHITECTURE.md` §5).
//!
//! [`ClientCore`] is a pure state machine: the host feeds it every [`Input`]
//! (user actions, server frames, transport changes, stored values, clock
//! ticks) and acts on the returned [`Effect`]s. It never touches a socket, a
//! disk or a clock, so it runs unchanged in Tauri, a browser (wasm) and the
//! TUI, and tests drive it without mocks.
//!
//! Rules that hold for every input:
//! - An input is either applied (state may change, effects returned) or
//!   rejected with a typed [`CoreError`]; a rejected input leaves the core
//!   exactly as it was and produces no effects.
//! - Draft text never enters the core. `Action::DraftOpened` /
//!   `DraftSubmitted { body }` / `DraftDiscarded` are the only crossings.
//! - `Effect::Render` names only the [`ViewSection`]s that changed, and
//!   there is at most one per input, after every other effect.
//! - Mutations are optimistic (§5.2): the view shows `committed + pending`;
//!   a foreign event re-applies the pending list on top; the daemon's own
//!   echo (matched by `client_id`/`client_seq`) retires a pending entry.
//! - Content (trees, render headers, chunks) is fetched through one path in
//!   `content.rs`: memory → disk (`Load`) → daemon (`Send`).

#![deny(clippy::wildcard_enum_match_arm)]

mod browse;
mod cache;
mod connection;
mod content;
mod creation;
mod delivery;
mod diff;
mod events;
mod explorer;
mod focus;
mod home;
mod ids;
mod keymap;
mod lifecycle;
mod patch;
mod ref_selector;
mod reference;
mod suggestion;
mod view;

use std::collections::BTreeMap;

use nits_protocol::{
    Anchor, Author, BuildInfo, ClientId, ClientMsg, ClientSeq, CommentId, CommentKind, CommitOid,
    DiffScope, Event, EventBody, Mutation, NonEmpty, ProtocolVersion, RenderTarget, RepoId,
    RepoPath, Request, RequestId, ResolvedSource, Response, ReviewId, ReviewSnapshot, ReviewTarget,
    ReviewTargetUpdate, RpcError, Seq, ServerMsg, Since, StreamItem, SubscribeScope, ThreadId,
    Timestamp, ViewSection, WorkspaceId,
};
use serde::{Deserialize, Serialize};
use strum::EnumDiscriminants;

use browse::{Browse, BrowseAttempt};
pub use browse::{BrowseAttemptView, BrowseStatus, BrowseStatusKind, BrowseTarget, BrowseView};

pub use cache::{Bytes, CacheKey, CacheValue, ContentCache, Evicted, RenderKey, TreeKey};
pub use connection::{Connection, ConnectionKind};
pub use content::{CacheConfig, DiskTier, DiskTierKind, FileRef, PREFETCH_RADIUS};
pub use creation::{
    CreationBase, CreationBaseKind, CreationDefault, CreationDefaultState,
    CreationDefaultStateKind, CreationDraft, CreationEdit, CreationEditKind, CreationReconcile,
    CreationResume, CreationRevision, CreationStatus, CreationStatusKind, CreationSubmission,
    CreationTarget, CreationTargetId, ReviewCreation,
};
pub use delivery::{
    VIEW_MESSAGE_LIMIT, ViewBatchBytes, ViewBatchKind, ViewDelivery, ViewDeliveryError,
    ViewEncoder, ViewFragmentIndex, ViewFragmentPosition, ViewFragmentPositionKind, ViewFrame,
    ViewFrameBody, ViewFrameBodyKind, ViewRevision,
};
pub use diff::{
    CommentView, CommitStepper, DiffRow, DiffView, PendingIds, RowPlace, RowThread, StepperCommit,
    ThreadPlace, ThreadPlaceKind, ThreadStatus, ThreadView, conversation, threads,
};
pub use events::{
    EventMeta, MutationError, MutationErrorKind, apply_body, apply_event, local_event, thread_id_of,
};
pub use explorer::{
    MAX_HITS, Progress, SearchHit, SearchView, TreeNode, TreeNodeKind, TreeView, ViewedState,
    viewed_state,
};
pub use focus::{
    Focus, FocusKind, NoTarget, PAGE_ROWS, Realign, VisualAnchor, clamp as clamp_focus,
    target_file_of, visible_nodes,
};
pub use home::{DaemonContext, DaemonContextKind, HomeRow, HomeRowKind, HomeRowKindKind, HomeView};
pub use ids::IdSeed;
pub use keymap::{
    Binding, Command, Conflict, Context, HelpEntry, HelpGroup, HelpView, Hint, KeyChord, KeyCode,
    KeyCodeKind, KeyParseError, KeySeq, Keymap, KeysConfig, KeysError, Lookup, Mode, Modifiers,
    NamedKey, Override, Overrides, command_named, config_name, label, modes_of,
};
pub use lifecycle::{
    DaemonManagement, DaemonManagementKind, ManagementReply, ManagementRequest, ManagementRequestId,
};
pub use patch::{ViewPatch, ViewPatchKind};
pub use ref_selector::{
    RefOption, RefSelectorPurpose, RefSelectorPurposeKind, RefSelectorSide, RefSelectorStatus,
    RefSelectorStatusKind, RefSelectorView,
};
pub use suggestion::{SuggestionStatus, SuggestionStatusKind, SuggestionView};
pub use view::{
    ConnectionView, ConnectionViewKind, ContentSearchView, Draft, DraftPurpose, DraftPurposeKind,
    Landing, LastKey, Layout, OpenFile, OpenReview, PendingEvent, ScrollAlign, ScrollIntent, Tab,
    ViewDelta, ViewModel, ViewPrefs, VisualView,
};

pub use nits_protocol as protocol;

/// Unix time in milliseconds, delivered via `Input::Tick`. It stamps ids
/// and pending events, so it must be wall-clock; the core never lets it go
/// backwards.
pub type Millis = u64;

/// Key in the host's key-value store (`Effect::Persist` / `Effect::Load`).
pub type Key = String;

/// Something the host tells the core.
#[derive(Debug, Clone, PartialEq, Eq, EnumDiscriminants)]
#[strum_discriminants(name(InputKind), derive(Hash))]
pub enum Input {
    User(Action),
    /// An adapter rejected action deserialization before a domain action existed.
    InvalidAction {
        reason: String,
    },
    Server(ServerMsg),
    Transport(TransportEvent),
    DaemonManaged {
        id: ManagementRequestId,
        reply: ManagementReply,
    },
    /// Answer to an `Effect::Load`; `None` when the key is absent.
    Stored {
        key: Key,
        value: Option<Vec<u8>>,
    },
    /// The host's clock advanced. Drives timeouts and id generation.
    Tick(Millis),
    /// A key press, resolved against the keymap and the focus (§6.4).
    Key(KeyChord),
}

/// After this long without a further chord, a pending sequence (`g` of
/// `g g`) is dropped.
pub const SEQ_TIMEOUT_MS: Millis = 800;

/// Rows a non-open file contributes to the stacked diff view; keeps every
/// `Diff` patch bounded (§6.3). The UI asks for more via `Viewport`.
pub const STACK_ROWS_CAP: u32 = 600;

/// What the transport layer observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportEvent {
    /// The connection the core asked for in `Effect::Connect` is up.
    Connected,
    /// The connection dropped (or the dial failed).
    Disconnected,
}

/// Which core-owned search supplies the result being activated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchKind {
    Files,
    Content,
}

/// A user intent, already resolved from keys or clicks by the host. Crosses
/// the host ↔ UI boundary (Tauri `dispatch`), hence serde.
#[derive(Debug, Clone, PartialEq, Eq, EnumDiscriminants, Serialize, Deserialize)]
#[strum_discriminants(name(ActionKind), derive(Hash, PartialOrd, Ord, strum::EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Action {
    InspectDaemon,
    UpgradeDaemon,
    Connect,
    Disconnect,
    /// Refresh the workspace list (and, on its answer, every review list).
    /// Done automatically on subscribe.
    ListWorkspaces,
    ToggleWorkspace {
        workspace_id: WorkspaceId,
    },
    SelectWorkspace {
        workspace_id: WorkspaceId,
    },
    StartReview {
        workspace_id: WorkspaceId,
    },
    CancelNewReview,
    SelectCreationTarget {
        review_id: ReviewId,
        target_id: CreationTargetId,
    },
    EditCreationDraft {
        review_id: ReviewId,
        edit: CreationEdit,
    },
    AddCreationTarget {
        review_id: ReviewId,
    },
    RemoveCreationTarget {
        review_id: ReviewId,
    },
    SubmitReviewCreation {
        review_id: ReviewId,
    },
    RetryReviewCreation {
        review_id: ReviewId,
    },
    RestoreReviewCreation {
        creation: ReviewCreation,
        resume: CreationResume,
    },
    /// Copy a daemon-side checkout path; repo-relative `RepoPath` is a different type.
    CopyCheckout {
        repo_id: RepoId,
    },
    GoHome,
    ListReviews {
        workspace_id: WorkspaceId,
    },
    /// Create a review; the id is minted by the core. Not optimistic: the
    /// `ReviewCreated` event adds it to the list.
    CreateReview {
        workspace_id: WorkspaceId,
        title: String,
        targets: NonEmpty<ReviewTarget>,
    },
    OpenReview {
        review_id: ReviewId,
    },
    /// Host-provided identity for references; never a browser bridge URL.
    SetReferenceContext {
        context: nits_protocol::ReferenceContext,
    },
    /// Raw route input is parsed once by the core's reference boundary.
    OpenReference {
        reference: String,
    },
    CopyReference {
        reference: nits_protocol::ReviewReference,
    },
    FocusComment {
        comment_id: nits_protocol::CommentId,
    },
    CloseReview,
    CheckCurrent,
    CheckRequested,
    CheckpointDelta,
    /// Start a review-level informational note, without an actionable lifecycle.
    InformationalNoteOpened,
    /// The user started writing a finding at `anchor`. The editor is the
    /// host's; the core only records that one is open.
    DraftOpened {
        anchor: Anchor,
    },
    DraftSubmitted {
        body: String,
    },
    DraftDiscarded,
    /// The user started writing a reply in `thread_id`.
    ReplyOpened {
        thread_id: ThreadId,
    },
    SetFocus {
        focus: Focus,
    },
    ToggleHelp,
    /// Reply in an existing thread of the open review.
    Reply {
        thread_id: ThreadId,
        body: String,
    },
    EditComment {
        comment_id: CommentId,
        body: String,
    },
    DeleteComment {
        comment_id: CommentId,
    },
    DeferOpened {
        thread_id: ThreadId,
    },
    DeferThread {
        thread_id: ThreadId,
        reason: nits_protocol::DeferralReason,
        tracking_url: Option<nits_protocol::TrackingUrl>,
    },
    ResolveThread {
        thread_id: ThreadId,
    },
    UnresolveThread {
        thread_id: ThreadId,
    },
    /// Write a suggestion comment's patch to the working tree. Not
    /// optimistic: the daemon reports the result as `SuggestionApplied`.
    ApplySuggestion {
        comment_id: CommentId,
    },
    PreviewSuggestion {
        comment_id: CommentId,
    },
    /// The host shows rows `first_row..=last_row` of `file`. Opens the file
    /// if it was not; drives chunk (pre)fetching.
    Viewport {
        file: FileRef,
        first_row: u32,
        last_row: u32,
    },
    /// Open `file` around `row` and focus that half of it — the
    /// jump-to-a-location transition (a thread in another file, the next
    /// file, a file picked in the tree). The target is known before the
    /// file's rows are, so the focus cannot be set by the host afterwards.
    /// `landing` says whether this was a deliberate jump (pin the file to
    /// the top) or a motion that walked over the boundary (just follow).
    OpenFileAt {
        file: FileRef,
        row: u32,
        side: nits_protocol::Side,
        landing: crate::view::Landing,
    },
    CloseFile,
    /// Expand or collapse a directory of the explorer (`None` = repo root).
    ToggleDir {
        repo_id: RepoId,
        path: Option<RepoPath>,
    },
    /// Open (`Some`) or close (`None`) the fuzzy file search.
    FileSearch {
        query: Option<String>,
    },
    SetLayout {
        layout: Layout,
    },
    /// Open a comment draft on a line range of `file` (mouse drag across
    /// lines, GitHub-style). The core resolves the anchor: it knows the
    /// file's render target; the UI only knows rows.
    CommentLines {
        file: FileRef,
        side: nits_protocol::Side,
        start_line: u32,
        end_line: u32,
    },
    /// Open a comment draft on the whole file (the file header's comment
    /// button, GitHub-style).
    CommentFile {
        file: FileRef,
    },
    /// Show a center tab (`1`/`2`/`3`).
    SetTab {
        tab: Tab,
    },
    /// The focused file's path was copied (the shell owns the clipboard;
    /// the core treats this as a no-op so it stays binding-reachable).
    CopyPath {
        path: RepoPath,
    },
    /// Reposition the view around the focused row (`z z`/`z t`/`z b`).
    /// Only the host knows the viewport's height, so the core records the
    /// intent and the host performs the scroll.
    ScrollView {
        align: crate::view::ScrollAlign,
    },
    /// Hide/show the left sidebar (persisted).
    ToggleSidebar,
    /// Fold/unfold one file's section in the stacked diff.
    ToggleFileCollapse {
        file: FileRef,
    },
    /// Collapse the focused tree node's parent dir (neo-tree style);
    /// focus moves to it.
    CollapseParent,
    /// Collapse every dir of the tree.
    CollapseAll,
    /// Change the render options; re-keys every render, so the open
    /// review's headers are fetched again.
    SetRenderOpts {
        ignore_whitespace: bool,
        context_lines: u32,
    },
    MarkViewed {
        file: FileRef,
    },
    UnmarkViewed {
        file: FileRef,
    },
    /// Fetch the commits of one repo of the open review for the stepper.
    ListCommits {
        repo_id: RepoId,
    },
    /// Step the by-commit diff; `None` is the working-tree step.
    StepCommit {
        selected: Option<usize>,
    },
    /// Change the diff scope (UI-DESIGN §Diff scope).
    SetScope {
        scope: ScopeChoice,
    },
    /// Open the diff a comment was made on, read-only (UI-DESIGN
    /// §Comments: jump-to-context). `Esc`/`Back` closes it.
    OpenOriginalDiff {
        thread_id: ThreadId,
    },
    /// Re-render one file with more context (UI-DESIGN §Diff rendering:
    /// expanders): +20 lines per step, or the whole file when `full`.
    /// Reveal a step more of one hidden run, in one direction — `enter`
    /// on an expander row, `z u`/`z d`, or clicking an expander. Only that
    /// gap moves.
    ExpandGap {
        file: FileRef,
        gap: nits_protocol::Gap,
        dir: nits_protocol::ExpandDir,
    },
    ExpandContext {
        file: FileRef,
        full: bool,
    },
    /// Browse `repo_id` at `ref_spec` (UI-DESIGN §Browse); `None` returns
    /// to the review's head trees.
    SetBrowseRef {
        repo_id: RepoId,
        ref_spec: Option<nits_protocol::RefSpec>,
    },
    /// Choose the repository for the next Browse ref lookup.
    SelectBrowseRepo {
        repo_id: RepoId,
    },
    /// Open the searchable ref catalog without modifying review targets.
    OpenBrowseRefSelector {
        repo_id: RepoId,
    },
    /// Restore the open review's head trees and cancel a candidate lookup.
    ResetBrowse,
    /// Open, re-query or (`None`) close the content-search palette
    /// (UI-DESIGN §Search). A non-empty query asks the daemon.
    ContentSearch {
        query: Option<String>,
        all_files: bool,
    },
    /// Open or close the actions palette (`:`): every command by name.
    ActionPalette {
        open: bool,
    },
    /// Run a command by name (the actions palette's Enter): resolved
    /// against the current focus exactly like its key binding.
    RunCommand {
        command: Command,
    },
    /// Enter Visual line selection at the focused diff row (`V`); motions
    /// extend it, `Comment` opens a draft on it, `Back` leaves.
    EnterVisual,
    /// Leave Visual mode without commenting (`esc`, `V` again).
    LeaveVisual,
    /// Enter a search's result list at its first result. This is absolute:
    /// the host may not yet have received preceding selection changes.
    SearchFirst {
        search: SearchKind,
    },
    /// Step the highlighted result while the search result list owns focus.
    SearchStep {
        search: SearchKind,
        delta: i32,
    },
    /// Activate the core's current selection after preceding navigation,
    /// only if the visible query still matches the current results.
    OpenSearchResult {
        search: SearchKind,
        query: String,
    },
    /// Open the git-backed selector for one side of one repo target.
    OpenRefSelector {
        repo_id: RepoId,
        side: RefSelectorSide,
    },
    RefSelectorQuery {
        query: String,
    },
    RefSelectorStep {
        delta: i32,
    },
    SelectRef {
        index: usize,
    },
    /// Select the option currently highlighted by the core. Keyboard hosts
    /// use this so motion and activation do not depend on an intervening
    /// render reaching the UI.
    SelectCurrentRef,
    CloseRefSelector,
}

/// How much one `ExpandContext` step adds to a file's context lines.
pub const EXPAND_STEP: u32 = 20;
/// `context_lines` treated as "show the whole file".
pub const FULL_CONTEXT: u32 = 1_000_000;

/// What the user asked the scope to become. `ByCommit` enters commit
/// stepping (fetching the commit list first when needed); the rest map to
/// wire scopes directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumDiscriminants, Serialize, Deserialize)]
#[strum_discriminants(name(ScopeChoiceKind), derive(Hash, PartialOrd, Ord, strum::EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ScopeChoice {
    All,
    Committed,
    ByCommit,
    Commit {
        repo_id: RepoId,
        oid: CommitOid,
    },
    Worktree {
        repo_id: RepoId,
    },
    Requested {
        request_id: nits_protocol::ReviewRequestId,
    },
    SinceCheckpoint {
        checkpoint_id: nits_protocol::ReviewCheckpointId,
    },
}

/// Something the host must do for the core.
#[derive(Debug, Clone, PartialEq, Eq, EnumDiscriminants)]
#[strum_discriminants(name(EffectKind), derive(Hash))]
pub enum Effect {
    ManageDaemon {
        id: ManagementRequestId,
        request: ManagementRequest,
    },
    /// Dial the daemon; report the outcome as `TransportEvent`.
    Connect,
    /// Close the connection. The host still reports `Disconnected`.
    Disconnect,
    Send(ClientMsg),
    Persist {
        key: Key,
        value: Vec<u8>,
    },
    Load {
        key: Key,
    },
    /// Delete a key from the host store (disk-tier trimming).
    Remove {
        key: Key,
    },
    Render(ViewDelta),
}

/// Why an input was rejected. The core is unchanged after any of these.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    #[error("{input:?} is not valid while the connection is {state:?}")]
    WrongConnectionState {
        input: InputKind,
        state: ConnectionKind,
    },
    #[error("server frame for unknown request id {0:?}")]
    UnknownRequest(RequestId),
    #[error("server answered request {id:?} with a {got} instead of a {expected}")]
    UnexpectedResponse {
        id: RequestId,
        expected: &'static str,
        got: &'static str,
    },
    #[error("no review is open")]
    NoOpenReview,
    #[error("current changes are refreshing; wait for matching content before recording a check")]
    CurrentChangesRefreshing,
    #[error("{0:?} is not a file of the open review")]
    UnknownFile(FileRef),
    #[error("no file is open")]
    NoOpenFile,
    #[error("a draft is already open")]
    DraftAlreadyOpen,
    #[error("no draft is open")]
    NoDraft,
    /// The mutation would be rejected by the daemon; nothing was sent.
    #[error(transparent)]
    Mutation(#[from] MutationError),
    /// Only humans mark files viewed (`Mutation::MarkViewed` is
    /// `Forbidden` for agents).
    #[error("only a human viewer can mark files viewed")]
    NotHuman,
    #[error("no commit list to step through")]
    NoStepper,
    #[error("review has no repo {0}")]
    UnknownRepo(RepoId),
    #[error("no ref selector is open")]
    NoRefSelector,
    #[error("the ref selector has no selected option")]
    NoSelectedRef,
    #[error("no thread {0}")]
    UnknownThread(ThreadId),
    #[error("suggestion {0} must have a current, ready preview before applying")]
    SuggestionNotReady(CommentId),
    #[error("thread {0} has no recorded original diff")]
    NoOriginalDiff(ThreadId),
    #[error("{0:?} indexes past the end of its list")]
    FocusOutOfRange(Focus),
    #[error("{0} is not bound in this context")]
    UnboundKey(String),
    #[error(transparent)]
    NoTarget(#[from] NoTarget),
    #[error("commit index {0} is out of range")]
    CommitOutOfRange(usize),
    #[error("nothing was loaded under key {0:?}")]
    UnknownKey(Key),
    #[error("the daemon rejected the handshake: {0:?}")]
    Rejected(RpcError),
    /// An event at or before `last_seq`; the daemon only ever sends newer.
    #[error("event {seq} is not after the last seen {last_seq}")]
    StaleEvent { seq: Seq, last_seq: Seq },
}

/// Opening content belongs to the snapshot generation that began its stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpeningContent {
    AwaitingSnapshot,
    Streaming,
    Superseded,
}

/// What a `RequestId` is waiting for, so the reply can be routed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InFlight {
    CreationDefault {
        review_id: ReviewId,
        repo_id: RepoId,
    },
    CreateReview {
        review_id: ReviewId,
        client_seq: ClientSeq,
    },
    ReconcileCreation {
        review_id: ReviewId,
    },
    CheckFailedCreation {
        review_id: ReviewId,
        error: RpcError,
    },
    Subscribe,
    ListWorkspaces,
    ListReviews {
        workspace_id: WorkspaceId,
    },
    /// Streamed open (local daemon): snapshot, trees, headers, first chunks.
    OpenReview {
        review_id: ReviewId,
        content: OpeningContent,
    },
    /// Piecewise open (disk tier on): snapshot only, the rest by key.
    ReviewSnapshot {
        review_id: ReviewId,
    },
    ListFiles {
        review_id: ReviewId,
        scope: DiffScope,
    },
    ListCommits {
        repo_id: RepoId,
    },
    ListRefs {
        repo_id: RepoId,
    },
    Search,
    TreeSnapshot {
        tree: TreeKey,
    },
    /// A Browse-tab tree at an arbitrary ref; the root is unknown until
    /// the answer names it.
    BrowseTree {
        repo_id: RepoId,
    },
    FileRender {
        render: RenderKey,
        stop_after: nits_protocol::ChunkIndex,
    },
    RenderChunk {
        key: CacheKey,
    },
    Mutate {
        client_seq: ClientSeq,
    },
    PreviewSuggestion {
        review_id: ReviewId,
        comment_id: CommentId,
    },
    ApplySuggestion {
        review_id: ReviewId,
        comment_id: CommentId,
    },
}

impl InFlight {
    /// Whether this request counts against `CacheConfig::max_in_flight`.
    fn is_content(&self) -> bool {
        match self {
            InFlight::TreeSnapshot { .. }
            | InFlight::FileRender { .. }
            | InFlight::RenderChunk { .. } => true,
            InFlight::Subscribe
            | InFlight::BrowseTree { .. }
            | InFlight::CreationDefault { .. }
            | InFlight::CreateReview { .. }
            | InFlight::ReconcileCreation { .. }
            | InFlight::CheckFailedCreation { .. }
            | InFlight::ListWorkspaces
            | InFlight::ListReviews { .. }
            | InFlight::OpenReview { .. }
            | InFlight::ReviewSnapshot { .. }
            | InFlight::ListFiles { .. }
            | InFlight::ListCommits { .. }
            | InFlight::ListRefs { .. }
            | InFlight::Search
            | InFlight::PreviewSuggestion { .. }
            | InFlight::ApplySuggestion { .. }
            | InFlight::Mutate { .. } => false,
        }
    }

    /// The cache key this request fills, if it is a single-key fetch.
    fn key(&self) -> Option<CacheKey> {
        match self {
            InFlight::TreeSnapshot { tree } => Some(CacheKey::Tree { tree: *tree }),
            InFlight::FileRender { render, .. } => Some(CacheKey::Header {
                render: render.clone(),
            }),
            InFlight::RenderChunk { key } => Some(key.clone()),
            InFlight::Subscribe
            | InFlight::CreationDefault { .. }
            | InFlight::CreateReview { .. }
            | InFlight::ReconcileCreation { .. }
            | InFlight::CheckFailedCreation { .. }
            | InFlight::ListWorkspaces
            | InFlight::ListReviews { .. }
            | InFlight::OpenReview { .. }
            | InFlight::ReviewSnapshot { .. }
            | InFlight::ListFiles { .. }
            | InFlight::ListCommits { .. }
            | InFlight::ListRefs { .. }
            | InFlight::Search
            | InFlight::BrowseTree { .. }
            | InFlight::PreviewSuggestion { .. }
            | InFlight::ApplySuggestion { .. }
            | InFlight::Mutate { .. } => None,
        }
    }
}

/// Configuration fixed for the life of a `ClientCore`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub client_id: ClientId,
    pub client: BuildInfo,
    pub author: Author,
    /// Entropy for ids the core mints (comment ids). Hosts pass real random
    /// bits; tests pass a constant for reproducibility.
    pub id_seed: IdSeed,
    pub cache: CacheConfig,
}

/// The client state machine. See the crate docs.
#[derive(Debug)]
pub struct ClientCore {
    config: Config,
    connection: Connection,
    recovery: lifecycle::Recovery,
    next_management: u64,
    management_request: Option<ManagementRequestId>,
    view: ViewModel,
    now: Millis,
    next_request: u64,
    next_client_seq: ClientSeq,
    in_flight: BTreeMap<RequestId, InFlight>,
    ids: ids::IdGen,
    /// `ReviewTargetsResolved` events held back while a draft is open (§5.4).
    deferred: Vec<Event>,
    content: content::Content,
    /// The open review as the daemon last confirmed it; `view.review.snapshot`
    /// is this plus `pending`.
    committed: Option<ReviewSnapshot>,
    /// Live requests, checks and target updates arriving while a snapshot is in flight. A response
    /// can carry an older cursor than an event delivered ahead of it.
    snapshot_events: BTreeMap<RequestId, (ReviewId, Vec<Event>)>,
    /// Only the latest file listing may install content after a target/scope refresh.
    latest_files: Option<RequestId>,
    /// Only the newest commit list may replace the stepper after ref movement.
    latest_commits: Option<RequestId>,
    /// Mutations sent and not yet echoed by the daemon, in send order.
    pending: Vec<Pending>,
    suggestions: BTreeMap<CommentId, suggestion::SuggestionState>,
    reference_context: Option<nits_protocol::ReferenceContext>,
    pending_reference: Option<reference::PendingReference>,
    latest_open: Option<RequestId>,
    explorer: explorer::ExplorerState,
    stepper: Option<CommitStepper>,
    /// `Effect::Load` for the prefs and keymap issued; answered or not.
    prefs_loaded: bool,
    keymap: Keymap,
    /// Chords of a sequence in progress, and when it started.
    chords: Vec<KeyChord>,
    chord_started: Millis,
    /// Focus whose context the open help overlay describes. `None`
    /// means help is closed; retaining the origin keeps moving focus into
    /// `Help` from replacing the commands the reader asked to inspect.
    help_return: Option<Focus>,
    /// Focus to return to when the composer closes.
    focus_return: Option<Focus>,
    /// `SetScope { ByCommit }` is waiting for its `ListCommits` answer.
    by_commit_pending: bool,
    /// Per-file fold overrides for the stacked view; unset files default
    /// to folded-when-viewed.
    file_collapse: std::collections::BTreeMap<(RepoId, RepoPath), bool>,
    /// The Browse tab's custom ref, when one is picked (UI-DESIGN §Browse).
    browse: Option<Browse>,
    browse_attempt: Option<BrowseAttempt>,
    browse_repo: Option<RepoId>,
    /// Visual mode (UI-DESIGN: modal keys): the diff row and side `V` was
    /// pressed on; the other end of the selection is the focused row.
    visual_anchor: Option<VisualAnchor>,
    /// Keys acted on, in order. Paired with what each resolved to in
    /// `last_key`, this is how a host tells whether the view it holds
    /// accounts for the keys it has sent, and what they meant (§6.4).
    keys_handled: u64,
    last_key: Option<crate::view::LastKey>,
    /// A re-render is in flight that will renumber the rows (opening a
    /// gap inserts lines above the ones below it). The line the cursor
    /// was on is remembered so the focus and the viewport can follow the
    /// content rather than the row index (§6.5).
    realign: Option<Realign>,
    ref_selector: Option<ref_selector::RefSelector>,
    /// Only this request may replace content-search results. Closing or
    /// replacing the query invalidates earlier responses, even for equal text.
    latest_search: Option<RequestId>,
}

/// A mutation applied locally and awaiting the daemon's echo.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    client_seq: ClientSeq,
    /// When it was applied locally; the optimistic event's timestamp.
    ts: Timestamp,
    mutation: Mutation,
    body: EventBody,
    delivery: PendingDelivery,
}

/// A lost reply is not proof that the mutation was rejected. Only an explicit
/// admission rejection permits an automatic retry with the original identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingDelivery {
    AwaitingReceipt,
    OutcomeUnknown,
    NotAdmitted,
}

impl ClientCore {
    #[must_use]
    pub fn new(config: Config) -> Self {
        let ids = ids::IdGen::new(config.id_seed);
        let content = content::Content::new(config.cache.clone());
        Self {
            config,
            connection: Connection::Disconnected { last_seq: None },
            recovery: lifecycle::Recovery::Idle,
            next_management: 0,
            management_request: None,
            view: ViewModel::default(),
            now: 0,
            next_request: 1,
            next_client_seq: ClientSeq::new(1),
            in_flight: BTreeMap::new(),
            ids,
            deferred: Vec::new(),
            content,
            committed: None,
            snapshot_events: BTreeMap::new(),
            latest_files: None,
            latest_commits: None,
            pending: Vec::new(),
            suggestions: BTreeMap::new(),
            reference_context: None,
            pending_reference: None,
            latest_open: None,
            explorer: explorer::ExplorerState::default(),
            stepper: None,
            prefs_loaded: false,
            keymap: Keymap::default_table(),
            chords: Vec::new(),
            chord_started: 0,
            help_return: None,
            focus_return: None,
            by_commit_pending: false,
            file_collapse: std::collections::BTreeMap::new(),
            browse: None,
            browse_attempt: None,
            browse_repo: None,
            visual_anchor: None,
            realign: None,
            ref_selector: None,
            latest_search: None,
            keys_handled: 0,
            last_key: None,
        }
    }

    #[must_use]
    pub fn client_id(&self) -> ClientId {
        self.config.client_id
    }

    #[must_use]
    pub fn author(&self) -> &Author {
        &self.config.author
    }

    #[must_use]
    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    /// Chords of the sequence in progress (`g` while waiting for `g g`).
    #[must_use]
    pub fn pending_chords(&self) -> &[KeyChord] {
        &self.chords
    }

    /// Where `V` was pressed, while Visual mode is on.
    #[must_use]
    pub fn visual_anchor(&self) -> Option<VisualAnchor> {
        self.visual_anchor
    }

    /// Mutations awaiting the daemon, oldest first.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    #[must_use]
    pub fn view(&self) -> &ViewModel {
        &self.view
    }

    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Apply one input. `Err` means nothing changed and nothing is to be done.
    pub fn handle(&mut self, input: Input) -> Result<Vec<Effect>, CoreError> {
        let mut effects = match input {
            Input::User(action) => self.user(action)?,
            Input::InvalidAction { reason } => {
                if let Some(draft) = &mut self.view.draft {
                    draft.submission_error = Some(reason);
                    vec![render(&[ViewSection::Draft])]
                } else if let Some(creation) = self.view.active_creation() {
                    self.creation_failure(creation.review_id, reason)
                } else {
                    Vec::new()
                }
            }
            Input::Server(msg) => self.server(msg)?,
            Input::Transport(ev) => self.transport(ev),
            Input::DaemonManaged { id, reply } => self.managed_daemon(id, reply),
            Input::Stored { key, value } if key == ViewPrefs::KEY => self.prefs_stored(value),
            Input::Stored { key, value } if key == Keymap::KEY => self.keymap_stored(value),
            Input::Stored { key, value } => self.stored(key, value)?,
            Input::Tick(ms) => {
                self.now = self.now.max(ms);
                // A pending sequence never expires on the clock (vim-like):
                // it waits until the user continues or cancels.
                self.restart_tick()
            }
            // Every key the core acts on is counted, including one that
            // resolves to a command changing nothing: a host that must
            // act on a key before the core answers (the clipboard needs
            // the gesture that asked for it) can only tell that its view
            // accounts for that key by counting. A key the context does
            // not bind stays a typed error and is not counted — the host
            // resolves against the same bindings, so it does not count
            // that one either.
            Input::Key(chord) => {
                // Counted before it is resolved, and whatever resolution
                // makes of it: a host counts the keys it sends, and a key
                // that turns out to be unbound (or to have no target) is
                // one it sent. Counting only what the core acted on would
                // put the two out of step at exactly the moments a host
                // needs them aligned.
                self.keys_handled = self.keys_handled.wrapping_add(1);
                let seq = self.keys_handled;
                let resolved = self.key(chord);
                let command = resolved.as_ref().ok().and_then(|(_, c)| *c);
                self.last_key = Some(crate::view::LastKey { seq, command });
                if resolved.is_err() {
                    // A rejection returns before the view is derived, so
                    // there is no patch to carry the verdict. The view
                    // records it anyway: which key the core has acted on
                    // is a fact about the view a host attaches to, and a
                    // host reading the last sequence that produced a
                    // patch would count from behind the core.
                    self.view.last_key = self.last_key;
                }
                resolved?.0
            }
        };
        // One `Render` per input: the union of every section touched, in
        // first-touched order, after every other effect.
        let mut sections: Vec<ViewSection> = Vec::new();
        effects.retain(|e| match e {
            Effect::Render(delta) => {
                sections.extend(delta.sections.iter().copied());
                false
            }
            Effect::ManageDaemon { .. }
            | Effect::Connect
            | Effect::Disconnect
            | Effect::Send(_)
            | Effect::Persist { .. }
            | Effect::Load { .. }
            | Effect::Remove { .. } => true,
        });
        sections.extend(self.derive());
        if !sections.is_empty() {
            effects.push(render(&sections));
        }
        Ok(effects)
    }

    /// Recompute the parts of the view that are functions of core state
    /// (explorer, progress) and report which changed.
    // One block per derived panel; splitting would hide what is derived.
    #[allow(clippy::too_many_lines)]
    fn derive(&mut self) -> Vec<ViewSection> {
        let uncertain: Vec<_> = self
            .pending
            .iter()
            .filter(|pending| pending.delivery == PendingDelivery::OutcomeUnknown)
            .map(|pending| pending.client_seq)
            .collect();
        let uncertainty_changed = self.view.uncertain_mutations != uncertain;
        self.view.uncertain_mutations = uncertain;
        let mut sections = if uncertainty_changed {
            vec![ViewSection::Connection]
        } else {
            Vec::new()
        };
        let previous_row = home::focused(&self.view);
        let rows = home::rows(&self.view);
        if rows != self.view.home.rows {
            self.view.home.rows = rows;
            if let Some(row) = previous_row {
                let index = self
                    .view
                    .home
                    .rows
                    .iter()
                    .position(|r| *r == row)
                    .or_else(|| {
                        self.view
                            .home
                            .rows
                            .iter()
                            .position(|r| r.workspace_id == row.workspace_id)
                    })
                    .unwrap_or(0);
                self.view.focus = Focus::ReviewList { index };
                sections.push(ViewSection::Focus);
            }
            sections.push(ViewSection::ReviewList);
        }
        let selected = self
            .view
            .review
            .as_ref()
            .map(|r| r.snapshot.review.workspace_id)
            .or_else(|| home::focused(&self.view).map(|row| row.workspace_id))
            .or(self.view.home.selected_workspace)
            .filter(|id| self.view.workspaces.iter().any(|w| w.id == *id))
            .or_else(|| self.view.workspaces.first().map(|w| w.id));
        if selected != self.view.home.selected_workspace {
            self.view.home.selected_workspace = selected;
            sections.push(ViewSection::ReviewList);
        }
        let ref_selector = self
            .ref_selector
            .as_ref()
            .map(|selector| selector.view.clone());
        if ref_selector != self.view.ref_selector {
            self.view.ref_selector = ref_selector;
            sections.push(ViewSection::RefSelector);
        }
        let (tree, progress) = match (&self.view.review, &self.committed) {
            (Some(open), Some(_)) => {
                let browse_root = match (self.view.tab, &self.browse) {
                    (Tab::Browse, Some(b)) => Some(TreeKey {
                        repo_id: b.target.repo_id,
                        root: b.root,
                    }),
                    (Tab::Browse | Tab::FilesChanged | Tab::Conversation, _) => None,
                };
                let heads: Vec<TreeKey> = match browse_root {
                    Some(root) => vec![root],
                    None => open
                        .current_targets()
                        .iter()
                        .map(|t| TreeKey {
                            repo_id: t.repo_id,
                            root: t.head.tree,
                        })
                        .collect(),
                };
                let trees: Vec<&nits_protocol::TreeSnapshot> = heads
                    .iter()
                    .filter_map(|tree| {
                        match self.content.cache.peek(&CacheKey::Tree { tree: *tree }) {
                            Some(CacheValue::Tree { snapshot }) => Some(snapshot),
                            Some(CacheValue::Header { .. } | CacheValue::Chunk { .. }) | None => {
                                None
                            }
                        }
                    })
                    .collect();
                let open_file = open.open_file.as_ref().map(|f| FileRef {
                    repo_id: f.render.repo_id,
                    path: f.render.path.clone(),
                });
                let repo_names: Vec<(RepoId, String)> = self
                    .view
                    .workspaces
                    .iter()
                    .flat_map(|w| w.repos.iter().map(|r| (r.id, r.display_name.clone())))
                    .collect();
                // A custom Browse ref shows every file plain: no change
                // badges (they belong to the review's diff).
                let files_for_tree: &[RenderKey] = if browse_root.is_some() {
                    &[]
                } else {
                    &open.files
                };
                // Per-file line stats from the cached render headers, and
                // thread counts per file, for tree rows and header totals.
                let mut stats: std::collections::BTreeMap<(RepoId, String), (u32, u32)> =
                    std::collections::BTreeMap::new();
                for k in &open.files {
                    if let Some(CacheValue::Header { header }) = self
                        .content
                        .cache
                        .peek(&CacheKey::Header { render: k.clone() })
                        && let nits_protocol::RenderContent::Text {
                            additions,
                            deletions,
                            ..
                        } = header.content
                    {
                        stats.insert(
                            (k.repo_id, k.path.as_str().to_owned()),
                            (additions, deletions),
                        );
                    }
                }
                let mut thread_counts: std::collections::BTreeMap<(RepoId, String), u32> =
                    std::collections::BTreeMap::new();
                for th in &open.snapshot.threads {
                    if let Some(root) = open.snapshot.comments.iter().find(|c| c.id == th.root) {
                        match &root.anchor {
                            Anchor::File { repo_id, path, .. }
                            | Anchor::Lines { repo_id, path, .. } => {
                                *thread_counts
                                    .entry((*repo_id, path.as_str().to_owned()))
                                    .or_insert(0) += 1;
                            }
                            Anchor::Review => {}
                        }
                    }
                }
                let inputs = explorer::ExplorerInputs {
                    snapshot: &open.snapshot,
                    repo_names: &repo_names,
                    trees,
                    files: files_for_tree,
                    open_file: open_file.as_ref(),
                    viewer: &self.config.author,
                    state: &self.explorer,
                    stats: &stats,
                    thread_counts: &thread_counts,
                    // Diffing tabs list only the changed files; the full
                    // tree belongs to Browse (UI-DESIGN §Layout).
                    changed_only: self.view.tab != Tab::Browse,
                };
                (
                    explorer::build(&inputs),
                    explorer::progress(&open.snapshot, &self.config.author, &open.files, &stats),
                )
            }
            (None, _) | (Some(_), None) => (TreeView::default(), Progress::default()),
        };
        if tree != self.view.tree {
            self.view.tree = tree;
            sections.push(ViewSection::Tree);
        }
        if progress != self.view.progress {
            self.view.progress = progress;
            sections.push(ViewSection::Progress);
        }
        // Before the diff is built, so the window this pass serves is
        // the shifted one.
        if self.realign.is_some() {
            self.apply_realign(&mut sections);
        }
        let (diff, mut threads) = match &self.view.review {
            Some(open) => {
                let pending = self.pending_ids();
                let threads = diff::threads(&open.snapshot, &pending);
                let diff = open.open_file.as_ref().and_then(|f| {
                    diff::diff_view(
                        &self.content.cache,
                        &open.snapshot,
                        &self.config.author,
                        &f.render,
                        f.first_row,
                        f.last_row,
                        self.view.draft.as_ref(),
                    )
                });
                (diff, threads)
            }
            None => (None, Vec::new()),
        };
        for thread in &mut threads {
            for comment in &mut thread.comments {
                if let Some(suggestion) = &mut comment.suggestion {
                    *suggestion = self.suggestion_view(&suggestion.record);
                }
            }
        }
        let diff = match (&self.view.review, diff) {
            (Some(open), Some(mut d))
                if open.original.is_some()
                    && open.open_file.as_ref().map(|f| &f.render) == open.original.as_ref() =>
            {
                d.original = true;
                Some(d)
            }
            (_, d) => d,
        };
        // The active Browse file and stacked sections share one fold state.
        // Pinned originals remain readable regardless of the current file's fold.
        let diff = diff.map(|mut d| {
            d.collapsed = !d.original
                && self
                    .file_collapse
                    .get(&(d.file.repo_id, d.file.path.clone()))
                    .copied()
                    .unwrap_or(d.viewed == explorer::ViewedState::Viewed);
            d
        });
        // The stacked view (UI-DESIGN §Layout, GitHub-style): one DiffView
        // per changed file. Patches stay viewport-bounded (§6.3): the open
        // file carries its viewport; every other file is capped at
        // `STACK_ROWS_CAP` rows — the UI accumulates rows it has seen and
        // asks for more via `Viewport`.
        let diffs: Vec<DiffView> = match &self.view.review {
            // The daemon serves files already in tree display order.
            Some(open) => open
                .files
                .iter()
                .filter_map(|render| {
                    let (first, last) = match open.open_file.as_ref() {
                        Some(f) if &f.render == render => (f.first_row, f.last_row),
                        Some(_) | None => (0, STACK_ROWS_CAP - 1),
                    };
                    diff::diff_view(
                        &self.content.cache,
                        &open.snapshot,
                        &self.config.author,
                        render,
                        first,
                        last,
                        self.view.draft.as_ref(),
                    )
                })
                .collect(),
            None => Vec::new(),
        };
        let diffs: Vec<DiffView> = diffs
            .into_iter()
            .map(|mut d| {
                d.collapsed = self
                    .file_collapse
                    .get(&(d.file.repo_id, d.file.path.clone()))
                    .copied()
                    .unwrap_or(d.viewed == explorer::ViewedState::Viewed);
                d
            })
            .collect();
        if diff != self.view.diff || diffs != self.view.diffs {
            self.view.diff = diff;
            self.view.diffs = diffs;
            sections.push(ViewSection::Diff);
        }
        let requests = self
            .view
            .review
            .as_ref()
            .map_or_else(Vec::new, |r| r.snapshot.requests.clone());
        let check_current_ready = self.check_current_ready();
        if check_current_ready != self.view.check_current_ready {
            self.view.check_current_ready = check_current_ready;
            sections.push(ViewSection::Conversation);
        }
        let checkpoints = self.view.review.as_ref().map_or_else(Vec::new, |r| {
            let current = self
                .deferred
                .iter()
                .rev()
                .find_map(|event| {
                    if let EventBody::ReviewTargetsResolved { review_id, targets } = &event.body {
                        (*review_id == r.snapshot.review.id).then_some(targets)
                    } else {
                        None
                    }
                })
                .or(r.snapshot.resolved.as_ref());
            nits_protocol::latest_checkpoints(&r.snapshot.checkpoints, current)
        });
        if checkpoints != self.view.checkpoints {
            self.view.checkpoints = checkpoints;
            sections.push(ViewSection::Conversation);
        }
        if requests != self.view.requests {
            self.view.requests = requests;
            sections.push(ViewSection::Conversation);
        }
        self.decorate_references(&mut threads);
        if threads != self.view.threads {
            let conversation = diff::conversation(&threads);
            if conversation != self.view.conversation {
                self.view.conversation = conversation;
                sections.push(ViewSection::Conversation);
            }
            self.view.threads = threads;
            sections.push(ViewSection::Threads);
        }
        if self.stepper != self.view.stepper {
            self.view.stepper.clone_from(&self.stepper);
            sections.push(ViewSection::CommitStepper);
        }
        let scope = self
            .view
            .review
            .as_ref()
            .map(|r| r.scope)
            .unwrap_or_default();
        if scope != self.view.scope {
            self.view.scope = scope;
            sections.push(ViewSection::ReviewList);
        }
        let browse = self.browse_view();
        if browse != self.view.browse {
            self.view.browse = browse;
            sections.push(ViewSection::ReviewList);
        }
        let focus = focus::clamp(&self.view, self.view.focus);
        if focus != self.view.focus {
            self.view.focus = focus;
            sections.push(ViewSection::Focus);
        }
        // What `y` would copy from here. Derived, not remembered: the
        // shell reads it during the gesture, so it must always be current
        // for the focus rather than for the last copy.
        let copy_target = focus::target_file_of(&self.view, focus).map(|f| f.path);
        if copy_target != self.view.copy_target {
            self.view.copy_target = copy_target;
            sections.push(ViewSection::Focus);
        }
        let copy_checkout = home::focused(&self.view).and_then(|row| match row.kind {
            HomeRowKind::Repository { repo_id } => self
                .view
                .workspaces
                .iter()
                .find(|w| w.id == row.workspace_id)
                .and_then(|w| w.repos.iter().find(|r| r.id == repo_id))
                .map(|repo| repo.path.clone()),
            HomeRowKind::Workspace | HomeRowKind::Review { .. } => None,
        });
        if copy_checkout != self.view.copy_checkout {
            self.view.copy_checkout = copy_checkout;
            sections.push(ViewSection::Focus);
        }
        let active_repo = focus::target_repo_of(&self.view, focus);
        if active_repo != self.view.active_repo {
            self.view.active_repo = active_repo;
            sections.push(ViewSection::ReviewList);
        }
        let copy_reference = self.focused_reference();
        if copy_reference != self.view.copy_reference {
            self.view.copy_reference = copy_reference;
            sections.push(ViewSection::Focus);
        }
        // A pointer refresh may replace the selected source file with a gitlink.
        // Metadata has no rows to preserve a line selection against.
        if self
            .view
            .diff
            .as_ref()
            .is_some_and(|diff| matches!(diff.content, nits_protocol::RenderContent::Submodule))
        {
            self.visual_anchor = None;
        }
        // The Visual selection spans the anchor and the focused row.
        let visual = match (self.visual_anchor, focus) {
            (Some(anchor), Focus::Diff { row, .. }) => Some(crate::view::VisualView {
                start: anchor.row.min(row),
                end: anchor.row.max(row),
                side: anchor.side,
            }),
            _ => None,
        };
        if visual != self.view.visual {
            self.view.visual = visual;
            sections.push(ViewSection::Diff);
        }
        // The hint bar is the mode indicator (UI-DESIGN): the focused
        // context's primary keys, or the pending group's keys while a
        // leader sequence is in progress.
        let pending_keys =
            KeySeq::new(self.chords.clone()).map_or_else(|_| String::new(), |s| s.to_string());
        if pending_keys != self.view.pending_keys {
            self.view.pending_keys = pending_keys;
            sections.push(ViewSection::Hints);
        }
        let pending_label = if self.chords.is_empty() {
            None
        } else {
            self.keymap.pending_label(&self.chords)
        };
        if pending_label != self.view.pending_label {
            self.view.pending_label = pending_label;
            sections.push(ViewSection::Hints);
        }
        // Vim-style mode indicator: Insert while a text editor owns keys,
        // Visual while a line selection is on.
        let mode = if focus.context() == keymap::Context::Composer {
            keymap::Mode::Insert
        } else if self.visual_anchor.is_some() {
            keymap::Mode::Visual
        } else {
            keymap::Mode::Normal
        };
        if mode != self.view.mode {
            self.view.mode = mode;
            sections.push(ViewSection::Hints);
        }
        let mut hints = if self.chords.is_empty() {
            self.keymap.hints(focus.context())
        } else {
            self.keymap.pending_hints(focus.context(), &self.chords)
        };
        // Informational conversation has no resolution lifecycle, and original
        // content cannot start file/line comments. The focused hint bar
        // (including custom prefix continuations) reflects these restrictions.
        hints.retain(|hint| {
            (!matches!(
                hint.command,
                Command::ToggleResolved | Command::DeferFinding
            ) || focus::resolve(self, hint.command).is_ok())
                && !(matches!(hint.command, Command::Comment | Command::CommentOnFile)
                    && self
                        .view
                        .review
                        .as_ref()
                        .is_some_and(|open| open.original_render().is_some())
                    && focus::resolve(self, hint.command).is_err())
        });
        if hints != self.view.hints {
            self.view.hints = hints;
            sections.push(ViewSection::Hints);
        }
        let chrome = self.keymap.chrome();
        if chrome != self.view.chrome {
            self.view.chrome = chrome;
            sections.push(ViewSection::Hints);
        }
        let bindings = self.keymap.applicable_bindings(focus.context());
        if bindings != self.view.bindings {
            self.view.bindings = bindings;
            sections.push(ViewSection::Hints);
        }
        if self.last_key != self.view.last_key {
            self.view.last_key = self.last_key;
            sections.push(ViewSection::Hints);
        }
        let leader = self.keymap.leader().to_string();
        if leader != self.view.leader {
            self.view.leader = leader;
            sections.push(ViewSection::Hints);
        }
        let help = self
            .help_return
            .map(|focus| self.keymap.help(focus.context()));
        if help != self.view.help {
            self.view.help = help;
            sections.push(ViewSection::Help);
        }
        sections
    }

    /// Comment and thread ids the pending mutations touch.
    fn pending_ids(&self) -> PendingIds {
        let mut ids = PendingIds::default();
        for p in &self.pending {
            match &p.body {
                EventBody::CommentCreated { comment } => ids.comments.push(comment.id),
                EventBody::CommentEdited { comment_id, .. }
                | EventBody::CommentDeleted { comment_id, .. }
                | EventBody::CommentReanchored { comment_id, .. } => ids.comments.push(*comment_id),
                EventBody::ThreadDeferred { thread_id, .. }
                | EventBody::ThreadResolved { thread_id, .. }
                | EventBody::ThreadUnresolved { thread_id, .. } => ids.threads.push(*thread_id),
                EventBody::ReviewCreated { .. }
                | EventBody::ReviewUpdated { .. }
                | EventBody::ReviewTargetUpdated { .. }
                | EventBody::ReviewDeleted { .. }
                | EventBody::ReviewTargetsResolved { .. }
                | EventBody::FileViewed { .. }
                | EventBody::FileUnviewed { .. }
                | EventBody::ReviewRequested { .. }
                | EventBody::ReviewChecked { .. }
                | EventBody::SuggestionApplied { .. }
                | EventBody::WorkspaceCreated { .. }
                | EventBody::WorkspaceUpdated { .. }
                | EventBody::RepoAttached { .. }
                | EventBody::RepoDetached { .. } => {}
            }
        }
        ids
    }

    /// Resolve a key press, reporting what it turned out to mean so the
    /// view can carry the verdict (§6.4): a host that must act on a key
    /// inside the gesture that produced it reads this rather than
    /// predicting the context the key would land in. The chord buffer is
    /// the one piece of state a rejected input may change: an unbound or
    /// unresolvable sequence clears it, so the next key starts fresh.
    fn key(&mut self, chord: KeyChord) -> Result<(Vec<Effect>, Option<Command>), CoreError> {
        let context = self.view.focus.context();
        let mut pressed = self.chords.clone();
        pressed.push(chord);
        match self.keymap.lookup(context, &pressed) {
            Lookup::Prefix => {
                if self.chords.is_empty() {
                    self.chord_started = self.now;
                }
                self.chords = pressed;
                Ok((Vec::new(), None))
            }
            Lookup::Command(command) => {
                self.chords.clear();
                let action = focus::resolve(self, command)?;
                Ok((self.user(action)?, Some(command)))
            }
            Lookup::None => {
                let was_pending = !self.chords.is_empty();
                self.chords.clear();
                if context == Context::Composer {
                    // Text for the host's editor, not a command.
                    Ok((Vec::new(), None))
                } else if was_pending {
                    // A key outside the pending group (esc included) just
                    // cancels the sequence, vim-like; the which-key popup
                    // closes on the derive.
                    Ok((Vec::new(), None))
                } else {
                    let seq =
                        KeySeq::new(pressed).map_or_else(|_| chord.to_string(), |s| s.to_string());
                    Err(CoreError::UnboundKey(seq))
                }
            }
        }
    }

    /// The stored keymap overrides arrived (or were absent / unreadable:
    /// the defaults stay).
    fn keymap_stored(&mut self, value: Option<Vec<u8>>) -> Vec<Effect> {
        // The host stores the typed keys config (keys.toml, validated
        // there). Unreadable or absent: the defaults stay.
        let Some(config) = value.and_then(|b| serde_json::from_slice::<KeysConfig>(&b).ok()) else {
            return Vec::new();
        };
        let Ok(map) = Keymap::with_config(&config) else {
            return Vec::new();
        };
        self.keymap = map;
        // Hints and help are derived after this returns.
        Vec::new()
    }

    /// The stored preferences arrived (or were absent).
    fn prefs_stored(&mut self, value: Option<Vec<u8>>) -> Vec<Effect> {
        self.prefs_loaded = true;
        let Some(prefs) = value.and_then(|b| serde_json::from_slice::<ViewPrefs>(&b).ok()) else {
            return Vec::new();
        };
        if prefs == self.view.prefs {
            return Vec::new();
        }
        self.apply_prefs(prefs, false)
    }

    /// Install `prefs`; persist when `save`. A render-option change re-keys
    /// every render, so the open review's file list is fetched again.
    fn apply_prefs(&mut self, prefs: ViewPrefs, save: bool) -> Vec<Effect> {
        let before = self.view.prefs;
        self.view.prefs = prefs;
        let mut effects = Vec::new();
        if save {
            effects.push(Effect::Persist {
                key: ViewPrefs::KEY.to_owned(),
                value: serde_json::to_vec(&prefs).unwrap_or_default(),
            });
        }
        let sections = vec![ViewSection::Diff];
        if prefs.render_opts() != before.render_opts() {
            self.content.config.render_opts = prefs.render_opts();
            if let Some(open) = &mut self.view.review {
                let review_id = open.snapshot.review.id;
                let scope = open.scope;
                open.files.clear();
                open.open_file = None;
                open.original = None;
                if let Connection::Subscribed { .. } = self.connection {
                    effects.push(self.request(
                        Request::ListFiles { review_id, scope },
                        InFlight::ListFiles { review_id, scope },
                    ));
                }
            }
        }
        effects.push(render(&sections));
        effects
    }

    /// Install `scope` on the open review and refetch its file list; the
    /// old files and open file are dropped (their renders stay cached).
    fn apply_scope(&mut self, scope: DiffScope) -> Vec<Effect> {
        let Some(open) = &mut self.view.review else {
            return Vec::new();
        };
        if open.scope == scope {
            return Vec::new();
        }
        let review_id = open.snapshot.review.id;
        open.scope = scope;
        open.scoped_targets.clear();
        open.files.clear();
        open.open_file = None;
        open.original = None;
        let mut effects = Vec::new();
        self.supersede_opening_content(review_id);
        self.rebase();
        if let Connection::Subscribed { .. } = self.connection {
            effects.push(self.request(
                Request::ListFiles { review_id, scope },
                InFlight::ListFiles { review_id, scope },
            ));
        }
        effects.push(render(&[ViewSection::ReviewList, ViewSection::Diff]));
        effects
    }

    /// The step by-commit mode enters at: the worktree step when the
    /// repo's head is a working tree, else the newest commit. `None` when
    /// a commit-headed review has no commits between base and head.
    fn default_by_commit_scope(&self, repo_id: RepoId) -> Option<DiffScope> {
        let open = self.view.review.as_ref()?;
        let head_worktree = open.snapshot.resolved.as_ref().is_some_and(|r| {
            r.iter().any(|t| {
                t.repo_id == repo_id && matches!(t.head.source, ResolvedSource::WorkingTree { .. })
            })
        });
        if head_worktree {
            return Some(DiffScope::Worktree { repo_id });
        }
        let stepper = self.stepper.as_ref().filter(|st| st.repo_id == repo_id)?;
        stepper.commits.first().map(|c| DiffScope::Commit {
            repo_id,
            oid: c.oid,
        })
    }

    /// Capture the visible content when composing, before any picker moves.
    fn comment_context(&self, anchor: &Anchor) -> Option<nits_protocol::CommentContext> {
        let (repo_id, path) = match anchor {
            Anchor::Review => return None,
            Anchor::File { repo_id, path, .. } | Anchor::Lines { repo_id, path, .. } => {
                (*repo_id, path)
            }
        };
        let render = self.comment_render(&FileRef {
            repo_id,
            path: path.clone(),
        })?;
        self.context_for_render(&render)
    }

    /// Prefer the content already displayed for this file; an unopened tree
    /// selection resolves against the current Browse ref or review scope.
    fn comment_render(&self, file: &FileRef) -> Option<RenderKey> {
        self.view
            .review
            .as_ref()?
            .open_file
            .as_ref()
            .map(|f| &f.render)
            .filter(|r| r.repo_id == file.repo_id && r.path == file.path)
            .cloned()
            .or_else(|| self.render_of_file(file))
    }

    fn context_for_render(&self, render: &RenderKey) -> Option<nits_protocol::CommentContext> {
        use nits_protocol::CommentContext;
        let open = self.view.review.as_ref()?;
        let (repo_id, path) = (render.repo_id, &render.path);
        if open.original.as_ref() == Some(render) {
            let original = open
                .snapshot
                .comments
                .iter()
                .filter(|comment| {
                    matches!(
                        comment.context,
                        Some(nits_protocol::CommentContext::Browse { .. })
                    )
                })
                .find(|comment| match &comment.anchor {
                    Anchor::File {
                        repo_id: id,
                        path: p,
                        blob_oid,
                    }
                    | Anchor::Lines {
                        repo_id: id,
                        path: p,
                        blob_oid,
                        ..
                    } => {
                        *id == repo_id
                            && *p == *path
                            && matches!(&render.target, RenderTarget::Blob { entry } if entry.oid == *blob_oid)
                    }
                    Anchor::Review => false,
                });
            if let Some(comment) = original {
                return comment.context.clone();
            }
        }
        Some(match &render.target {
            RenderTarget::Diff { change } => CommentContext::Diff {
                change: change.clone(),
            },
            RenderTarget::Blob { .. } => CommentContext::Browse {
                reference: self
                    .browse
                    .as_ref()
                    .filter(|b| b.target.repo_id == repo_id)
                    .map(|b| b.target.ref_spec.clone())
                    .or_else(|| {
                        open.current_targets()
                            .into_iter()
                            .find(|t| t.repo_id == repo_id)
                            .map(|t| match t.head.source {
                                ResolvedSource::Commit { oid } => {
                                    nits_protocol::RefSpec::Commit { oid }
                                }
                                ResolvedSource::WorkingTree { .. } => {
                                    nits_protocol::RefSpec::WorkingTree
                                }
                            })
                    })?,
            },
        })
    }

    /// Open the source a thread's root comment recorded as its context.
    /// Historical diffs are read-only; pinned Browse blobs retain their
    /// captured comment source. Neither joins the current review's file list.
    fn open_original(&mut self, thread_id: ThreadId) -> Result<Vec<Effect>, CoreError> {
        self.require_subscribed()?;
        let Some(open) = &self.view.review else {
            return Err(CoreError::NoOpenReview);
        };
        let review_id = open.snapshot.review.id;
        let root = open
            .snapshot
            .threads
            .iter()
            .find(|t| t.id == thread_id)
            .and_then(|t| open.snapshot.comments.iter().find(|c| c.id == t.root))
            .ok_or(CoreError::UnknownThread(thread_id))?;
        let context = root
            .context
            .clone()
            .ok_or(CoreError::NoOriginalDiff(thread_id))?;
        let (repo_id, path, blob_oid) = match &root.anchor {
            Anchor::File {
                repo_id,
                path,
                blob_oid,
            }
            | Anchor::Lines {
                repo_id,
                path,
                blob_oid,
                ..
            } => (*repo_id, path.clone(), *blob_oid),
            Anchor::Review => return Err(CoreError::NoOriginalDiff(thread_id)),
        };
        // A comment's original diff opens on the side it was made on.
        let side = match &root.anchor {
            Anchor::Lines { side, .. } => *side,
            Anchor::File { .. } | Anchor::Review => nits_protocol::Side::Head,
        };
        let key = RenderKey {
            repo_id,
            path,
            target: match &context {
                nits_protocol::CommentContext::Diff { change } => RenderTarget::Diff {
                    change: change.clone(),
                },
                nits_protocol::CommentContext::Browse { .. } => RenderTarget::Blob {
                    entry: nits_protocol::BlobEntry {
                        oid: blob_oid,
                        mode: nits_protocol::BlobMode::Unknown,
                    },
                },
            },
            opts: match context {
                nits_protocol::CommentContext::Browse { .. } => {
                    nits_protocol::RenderOpts::default()
                }
                nits_protocol::CommentContext::Diff { .. } => {
                    self.content.config.render_opts.clone()
                }
            },
        };
        let row = match (&context, &root.anchor) {
            (nits_protocol::CommentContext::Browse { .. }, Anchor::Lines { lines, .. }) => {
                lines.end().index()
            }
            (
                nits_protocol::CommentContext::Browse { .. },
                Anchor::File { .. } | Anchor::Review,
            )
            | (
                nits_protocol::CommentContext::Diff { .. },
                Anchor::Review | Anchor::File { .. } | Anchor::Lines { .. },
            ) => 0,
        };
        self.view.tab = match &context {
            nits_protocol::CommentContext::Browse { .. } => Tab::Browse,
            nits_protocol::CommentContext::Diff { .. } => Tab::FilesChanged,
        };
        if let Some(open) = &mut self.view.review {
            open.original = Some(key.clone());
            open.open_file = Some(crate::view::OpenFile {
                render: key.clone(),
                first_row: row.saturating_sub(PAGE_ROWS / 2),
                last_row: row.saturating_add(PAGE_ROWS / 2),
            });
        }
        self.view.focus = Focus::Diff { row, side };
        let mut effects = Vec::new();
        self.want_open_render(review_id, &key, &mut effects);
        effects.push(render(&[ViewSection::Focus, ViewSection::Diff]));
        Ok(effects)
    }

    /// Re-key one file's render under new options and refetch it. The
    /// expansion is per-file and transient: a scope or render-option
    /// change rebuilds the file list at the default context.
    fn re_render(
        &mut self,
        file: &FileRef,
        opts: impl FnOnce(&nits_protocol::RenderOpts) -> nits_protocol::RenderOpts,
    ) -> Result<Vec<Effect>, CoreError> {
        self.require_subscribed()?;
        let Some(open) = &self.view.review else {
            return Err(CoreError::NoOpenReview);
        };
        let review_id = open.snapshot.review.id;
        let Some(old) = open
            .original_render()
            .filter(|k| k.repo_id == file.repo_id && k.path == file.path)
            .or_else(|| {
                open.files
                    .iter()
                    .find(|k| k.repo_id == file.repo_id && k.path == file.path)
            })
            .cloned()
        else {
            return Err(CoreError::UnknownFile(file.clone()));
        };
        if matches!(
            old.target,
            RenderTarget::Diff {
                change: nits_protocol::ChangeKind::Submodule { .. }
            }
        ) {
            return Ok(Vec::new());
        }
        let key = RenderKey {
            opts: opts(&old.opts),
            ..old.clone()
        };
        if key == old {
            return Ok(Vec::new());
        }
        // Remember the cursor's line, not its row index: the row numbers
        // below an opened gap all shift by however much it revealed.
        self.realign = match self.focused_line(&old, &key) {
            Some(realign) => Some(realign),
            None => match self.realign.take() {
                // A second explicit expansion can arrive while the first
                // render is still missing. Carry the original logical line
                // forward to the newest render instead of losing it.
                Some(mut pending) if pending.render == old => {
                    pending.render = key.clone();
                    Some(pending)
                }
                Some(_) | None => None,
            },
        };
        let Some(open) = &mut self.view.review else {
            return Err(CoreError::NoOpenReview);
        };
        if open.original_render() == Some(&old) {
            // The historical pane owns its render options. Re-keying it
            // must not replace the current review's change at this path.
            open.original = Some(key.clone());
        } else if let Some(render) = open.files.iter_mut().find(|render| **render == old) {
            *render = key.clone();
        }
        // Keep viewing the file over the same rows; the render's row count
        // grows, so the window is re-evaluated when the header lands.
        if let Some(f) = &mut open.open_file
            && f.render == old
        {
            f.render = key.clone();
        }
        let mut effects = Vec::new();
        self.want_open_render(review_id, &key, &mut effects);
        effects.push(render(&[ViewSection::Diff]));
        Ok(effects)
    }

    /// Widen the whole file's context (`x`, or the header's expand-file
    /// button). Opening one hidden run is [`Self::expand_gap`].
    fn expand_context(&mut self, file: &FileRef, full: bool) -> Result<Vec<Effect>, CoreError> {
        self.re_render(file, |opts| nits_protocol::RenderOpts {
            context_lines: if full {
                FULL_CONTEXT
            } else {
                opts.context_lines.saturating_add(EXPAND_STEP)
            },
            // Gap numbers come from the grouping that `context_lines`
            // produces, so widening it can merge two groups and renumber
            // every gap after them: the recorded expansions would then
            // open runs nobody asked for. Widening subsumes them anyway.
            expanded: nits_protocol::Expansions::default(),
            ..opts.clone()
        })
    }

    /// Open one hidden run by a step, in one direction: `enter` on an
    /// expander row, `z u`/`z d`, or clicking an expander. Only that gap
    /// moves — widening `context_lines` would push every other hunk of the
    /// file around under the reader's cursor.
    fn expand_gap(
        &mut self,
        file: &FileRef,
        gap: nits_protocol::Gap,
        dir: nits_protocol::ExpandDir,
    ) -> Result<Vec<Effect>, CoreError> {
        self.re_render(file, |opts| nits_protocol::RenderOpts {
            expanded: opts.expanded.opened(gap, dir, EXPAND_STEP),
            ..opts.clone()
        })
    }

    /// Historical diff panes cannot open line/file composers. Pinned Browse
    /// blobs keep their captured source; direct mouse/host actions follow the
    /// same rule as keyboard commands.
    fn require_commentable_context(&self) -> Result<(), CoreError> {
        let open = self.view.review.as_ref().ok_or(CoreError::NoOpenReview)?;
        if open.original_is_read_only() {
            return Err(NoTarget::ReadOnlyOriginal.into());
        }
        Ok(())
    }

    /// The line the cursor is on in `render`, if that is the open file.
    fn focused_line(&self, render: &RenderKey, waiting_for: &RenderKey) -> Option<Realign> {
        let Focus::Diff { row, side } = self.view.focus else {
            return None;
        };
        let open = self.view.review.as_ref()?;
        if open.open_file.as_ref().map(|f| &f.render) != Some(render) {
            return None;
        }
        let diff = self.view.diff.as_ref()?;
        let r = diff.rows.iter().find(|r| r.index == row)?;
        // A split row can name different lines on its two sides. Preserve
        // the half the reader actually focused rather than silently
        // preferring whichever cell happens to be found first.
        let line = diff::line_on(&r.row, side)?;
        Some(Realign {
            render: waiting_for.clone(),
            side,
            line,
            row,
        })
    }

    /// After a re-render lands: put the cursor back on the line it was on
    /// and shift the viewport by the same amount, so the reader keeps
    /// looking at what they were looking at.
    fn apply_realign(&mut self, sections: &mut Vec<ViewSection>) {
        let Some(want) = self.realign.clone() else {
            return;
        };
        let Some(open) = &self.view.review else {
            return;
        };
        let Some(render) = open.open_file.as_ref().map(|f| f.render.clone()) else {
            return;
        };
        // Only the render this was recorded for: opening another file
        // before the response lands must not move that file's cursor.
        if render != want.render {
            return;
        }
        let rows = diff::all_rows(&self.content.cache, &open.snapshot, &render);
        let Some(now) = rows
            .iter()
            .find(|r| diff::line_on(&r.row, want.side) == Some(want.line))
        else {
            // The rows for that line have not arrived yet; keep waiting.
            return;
        };
        self.realign = None;
        let delta = i64::from(now.index) - i64::from(want.row);
        if delta == 0 {
            return;
        }
        // The focus is settled before this pass compares it, so the patch
        // has to be asked for here.
        sections.push(ViewSection::Focus);
        let shift = |n: u32| u32::try_from((i64::from(n) + delta).max(0)).unwrap_or(0);
        if let Focus::Diff { row, side } = self.view.focus {
            self.view.focus = Focus::Diff {
                row: shift(row),
                side,
            };
        }
        if let Some(open) = &mut self.view.review
            && let Some(f) = &mut open.open_file
        {
            f.first_row = shift(f.first_row);
            f.last_row = shift(f.last_row);
        }
    }

    fn wrong_state(&self, input: InputKind) -> CoreError {
        CoreError::WrongConnectionState {
            input,
            state: self.connection.kind(),
        }
    }

    /// Record where the view should sit around `row`. Counting the
    /// instructions is what makes the same reposition twice two of them.
    fn scroll_to(&mut self, row: u32, align: crate::view::ScrollAlign) {
        let seq = self.view.scroll.map_or(0, |s| s.seq).wrapping_add(1);
        self.view.scroll = Some(crate::view::ScrollIntent { row, align, seq });
    }

    // One arm per variant; splitting would hide the exhaustive match.
    #[allow(clippy::too_many_lines)]
    fn user(&mut self, action: Action) -> Result<Vec<Effect>, CoreError> {
        match action {
            Action::InspectDaemon => Ok(self.manage_daemon(ManagementRequest::Inspect)),
            Action::UpgradeDaemon => Ok(self.manage_daemon(ManagementRequest::Upgrade)),
            Action::Connect => match self.connection {
                Connection::Disconnected { last_seq } => {
                    self.connection = Connection::Connecting {
                        hello_sent: false,
                        last_seq,
                    };
                    self.recovery = lifecycle::Recovery::Idle;
                    self.view.connection = ConnectionView::Connecting;
                    let mut effects = vec![Effect::Connect, render(&[ViewSection::Connection])];
                    if !self.prefs_loaded {
                        // Once per core: the host answers with `Input::Stored`.
                        self.prefs_loaded = true;
                        effects.push(Effect::Load {
                            key: ViewPrefs::KEY.to_owned(),
                        });
                        effects.push(Effect::Load {
                            key: Keymap::KEY.to_owned(),
                        });
                    }
                    Ok(effects)
                }
                Connection::Connecting { .. } | Connection::Subscribed { .. } => {
                    Err(self.wrong_state(InputKind::User))
                }
            },
            Action::Disconnect => match self.connection {
                Connection::Disconnected { .. } => Err(self.wrong_state(InputKind::User)),
                Connection::Connecting { .. } | Connection::Subscribed { .. } => {
                    Ok(vec![Effect::Disconnect])
                }
            },
            Action::ListWorkspaces => {
                self.require_subscribed()?;
                Ok(vec![self.request(
                    Request::ListWorkspaces,
                    InFlight::ListWorkspaces,
                )])
            }
            Action::SelectWorkspace { workspace_id } => {
                let index = self
                    .view
                    .home
                    .rows
                    .iter()
                    .position(|row| row.workspace_id == workspace_id)
                    .ok_or(CoreError::NoTarget(NoTarget::Nothing(Command::Open)))?;
                self.view.focus = Focus::ReviewList { index };
                Ok(vec![render(&[ViewSection::Focus])])
            }
            Action::ToggleWorkspace { workspace_id } => {
                if !self.view.workspaces.iter().any(|w| w.id == workspace_id) {
                    return Err(CoreError::NoTarget(NoTarget::Nothing(Command::Open)));
                }
                if self.view.home.expanded.contains(&workspace_id) {
                    self.view.home.expanded.retain(|id| *id != workspace_id);
                } else {
                    self.view.home.expanded.push(workspace_id);
                }
                Ok(vec![render(&[ViewSection::ReviewList])])
            }
            Action::StartReview { workspace_id } => self.start_creation(workspace_id),
            Action::CancelNewReview => {
                if self
                    .view
                    .home
                    .creating
                    .as_ref()
                    .is_some_and(|c| !c.editable() && c.status != CreationStatus::Succeeded)
                {
                    return Ok(Vec::new());
                }
                self.view.home.creating = None;
                Ok(vec![render(&[ViewSection::ReviewList])])
            }
            Action::SelectCreationTarget {
                review_id,
                target_id,
            } => Ok(self.select_creation_target(review_id, target_id)),
            Action::EditCreationDraft { review_id, edit } => {
                Ok(self.edit_creation_field(review_id, edit))
            }
            Action::AddCreationTarget { review_id } => Ok(self.add_creation_target(review_id)),
            Action::RemoveCreationTarget { review_id } => {
                Ok(self.remove_creation_target(review_id))
            }
            Action::SubmitReviewCreation { review_id } => Ok(self.submit_creation(review_id)),
            Action::RetryReviewCreation { review_id } => {
                Ok(self.reconcile_creation(review_id, CreationReconcile::Retry))
            }
            Action::RestoreReviewCreation { creation, resume } => {
                Ok(self.restore_creation(creation, resume))
            }
            Action::GoHome => {
                self.pending_reference = None;
                self.latest_open = None;
                let workspace_id = self.view.home.selected_workspace;
                let mut effects = Vec::new();
                if self.view.review.is_some() {
                    self.close_review(&mut effects);
                }
                let index = self
                    .view
                    .home
                    .rows
                    .iter()
                    .position(|row| Some(row.workspace_id) == workspace_id)
                    .unwrap_or(0);
                self.view.focus = Focus::ReviewList { index };
                effects.push(render(&[
                    ViewSection::ReviewList,
                    ViewSection::Focus,
                    ViewSection::Draft,
                ]));
                Ok(effects)
            }
            Action::ListReviews { workspace_id } => {
                self.require_subscribed()?;
                Ok(vec![self.request(
                    Request::ListReviews { workspace_id },
                    InFlight::ListReviews { workspace_id },
                )])
            }
            Action::CreateReview {
                workspace_id,
                title,
                targets,
            } => {
                self.require_subscribed()?;
                let targets = match nits_protocol::CreateReviewTargets::try_from(targets) {
                    Ok(targets) => targets,
                    Err(error) => {
                        self.view.last_error = Some(RpcError::Invalid {
                            reason: error.to_string(),
                        });
                        return Ok(vec![render(&[ViewSection::Connection])]);
                    }
                };
                let review_id = self.ids.review_id(self.now);
                let client_seq = self.next_client_seq;
                self.next_client_seq = client_seq.next();
                Ok(vec![self.request(
                    Request::Mutate {
                        client_seq,
                        mutation: Mutation::CreateReview {
                            review_id,
                            workspace_id,
                            title,
                            targets: targets.into(),
                        },
                    },
                    InFlight::Mutate { client_seq },
                )])
            }
            Action::SetReferenceContext { context } => {
                self.view.daemon_context = context.locator().ok().map(|locator| match locator {
                    nits_protocol::ReferenceLocator::Named(name) => DaemonContext::Named { name },
                    nits_protocol::ReferenceLocator::Socket(path) => DaemonContext::Socket { path },
                    nits_protocol::ReferenceLocator::WebSocket(url) => {
                        DaemonContext::WebSocket { url }
                    }
                });
                self.reference_context = Some(context);
                Ok(vec![render(&[
                    ViewSection::Focus,
                    ViewSection::Threads,
                    ViewSection::ReviewList,
                ])])
            }
            Action::OpenReference { reference } => self.open_reference(&reference),
            Action::FocusComment { comment_id } => self.focus_comment(comment_id),
            Action::OpenReview { review_id } => {
                self.require_subscribed()?;
                let opts = self.content.config.render_opts.clone();
                let (request, waiting) = match self.content.config.disk {
                    DiskTier::Disabled => (
                        Request::OpenReview { review_id, opts },
                        InFlight::OpenReview {
                            review_id,
                            content: OpeningContent::AwaitingSnapshot,
                        },
                    ),
                    DiskTier::Enabled { .. } => (
                        Request::ReviewSnapshot { review_id },
                        InFlight::ReviewSnapshot { review_id },
                    ),
                };
                self.pending_reference = None;
                let effect = self.request(request, waiting);
                Ok(vec![effect])
            }
            Action::CloseReview => {
                if self.view.review.is_none() {
                    return Err(CoreError::NoOpenReview);
                }
                self.pending_reference = None;
                self.latest_open = None;
                let mut effects = Vec::new();
                self.close_review(&mut effects);
                // Diff, threads, tree, progress: derived after this returns.
                effects.push(render(&[ViewSection::ReviewList, ViewSection::Draft]));
                Ok(effects)
            }
            Action::Viewport {
                file,
                first_row,
                last_row,
            } => self.viewport(file, first_row, last_row),
            Action::OpenFileAt {
                file,
                row,
                side,
                landing,
            } => {
                let first_row = row.saturating_sub(PAGE_ROWS / 2);
                let mut effects = self.viewport(file, first_row, first_row + PAGE_ROWS - 1)?;
                // Opening set the focus to the top of the window; the
                // caller knows the row and side it opened the file for.
                self.view.focus = Focus::Diff { row, side };
                if landing == crate::view::Landing::Pin {
                    self.scroll_to(row, crate::view::ScrollAlign::Top);
                }
                effects.push(render(&[ViewSection::Focus]));
                Ok(effects)
            }
            Action::CloseFile => {
                self.visual_anchor = None;
                self.close_file()
            }
            Action::ToggleDir { repo_id, path } => {
                if self.view.review.is_none() {
                    return Err(CoreError::NoOpenReview);
                }
                self.browse_repo = Some(repo_id);
                let key = (repo_id, path);
                // Diffing tabs default dirs open (the set records the
                // collapsed ones); Browse defaults closed. Separate sets,
                // so toggling in one tab never leaks into the other.
                let set = if self.view.tab == Tab::Browse {
                    &mut self.explorer.expanded
                } else {
                    &mut self.explorer.collapsed
                };
                if !set.remove(&key) {
                    set.insert(key);
                }
                // The tree itself is derived after this returns.
                Ok(Vec::new())
            }
            Action::FileSearch { query } => {
                if self.view.review.is_none() {
                    return Err(CoreError::NoOpenReview);
                }
                self.explorer.search = query;
                self.explorer.search_selected = 0;
                Ok(Vec::new())
            }
            Action::SetLayout { layout } => {
                let prefs = ViewPrefs {
                    layout,
                    ..self.view.prefs
                };
                Ok(self.apply_prefs(prefs, true))
            }
            Action::SetRenderOpts {
                ignore_whitespace,
                context_lines,
            } => {
                let prefs = ViewPrefs {
                    ignore_whitespace,
                    context_lines,
                    ..self.view.prefs
                };
                Ok(self.apply_prefs(prefs, true))
            }
            Action::CommentLines {
                file,
                side,
                start_line,
                end_line,
            } => {
                self.require_commentable_context()?;
                let Some(open) = &self.view.review else {
                    return Err(CoreError::NoOpenReview);
                };
                if self.view.draft.is_some() {
                    return Err(CoreError::DraftAlreadyOpen);
                }
                let render_key = open
                    .open_file
                    .as_ref()
                    .map(|f| &f.render)
                    .filter(|r| r.repo_id == file.repo_id && r.path == file.path)
                    .or_else(|| {
                        if self.view.tab == Tab::Browse {
                            return None;
                        }
                        open.files
                            .iter()
                            .find(|r| r.repo_id == file.repo_id && r.path == file.path)
                    })
                    .ok_or_else(|| CoreError::UnknownFile(file.clone()))?;
                let blob = match (&render_key.target, side) {
                    (RenderTarget::Diff { change }, nits_protocol::Side::Head) => change.new_blob(),
                    (RenderTarget::Diff { change }, nits_protocol::Side::Base) => change.old_blob(),
                    (RenderTarget::Blob { entry }, nits_protocol::Side::Head) => Some(entry.oid),
                    (RenderTarget::Blob { .. }, nits_protocol::Side::Base) => None,
                }
                .ok_or_else(|| CoreError::UnknownFile(file.clone()))?;
                let rows = diff::all_rows(self.cache(), &open.snapshot, render_key);
                let (lo, hi) = (start_line.min(end_line), start_line.max(end_line));
                if ![lo, hi].iter().all(|line| {
                    rows.iter()
                        .any(|row| diff::line_on(&row.row, side) == Some(*line))
                }) {
                    return Err(CoreError::UnknownFile(file));
                }
                let (Some(lo), Some(hi)) = (
                    nits_protocol::LineNo::new(lo),
                    nits_protocol::LineNo::new(hi),
                ) else {
                    return Err(CoreError::UnknownFile(file));
                };
                let Ok(lines) = nits_protocol::LineRange::new(lo, hi) else {
                    return Err(CoreError::UnknownFile(file));
                };
                let anchor = Anchor::Lines {
                    repo_id: file.repo_id,
                    path: file.path,
                    side,
                    blob_oid: blob,
                    lines,
                    // Placeholder; the daemon hashes the context itself.
                    context_hash: nits_protocol::ContextHash::new(0),
                };
                self.visual_anchor = None;
                self.view.draft = Some(Draft {
                    submission_error: None,
                    purpose: DraftPurpose::Comment {
                        intent: nits_protocol::CommentIntent::Finding,
                        context: self.comment_context(&anchor),
                    },
                    anchor,
                });
                self.enter(Focus::Composer);
                Ok(vec![render(&[ViewSection::Draft, ViewSection::Focus])])
            }
            Action::CommentFile { file } => {
                self.require_commentable_context()?;
                if self.view.review.is_none() {
                    return Err(CoreError::NoOpenReview);
                }
                if self.view.draft.is_some() {
                    return Err(CoreError::DraftAlreadyOpen);
                }
                let source = self
                    .comment_render(&file)
                    .ok_or_else(|| CoreError::UnknownFile(file.clone()))?;
                let blob = match &source.target {
                    RenderTarget::Diff { change } => change.new_blob(),
                    RenderTarget::Blob { entry } => Some(entry.oid),
                }
                .ok_or_else(|| CoreError::UnknownFile(file.clone()))?;
                let anchor = Anchor::File {
                    repo_id: file.repo_id,
                    path: file.path,
                    blob_oid: blob,
                };
                self.visual_anchor = None;
                self.view.draft = Some(Draft {
                    submission_error: None,
                    purpose: DraftPurpose::Comment {
                        intent: nits_protocol::CommentIntent::Finding,
                        context: self.context_for_render(&source),
                    },
                    anchor,
                });
                self.enter(Focus::Composer);
                Ok(vec![render(&[ViewSection::Draft, ViewSection::Focus])])
            }
            Action::SetTab { tab } => {
                if self.view.tab == tab {
                    return Ok(Vec::new());
                }
                self.view.tab = tab;
                if matches!(self.view.focus, Focus::ReviewRequest { .. })
                    && tab != Tab::Conversation
                {
                    self.view.focus = focus::clamp(
                        &self.view,
                        Focus::Diff {
                            row: 0,
                            side: nits_protocol::Side::Head,
                        },
                    );
                }
                if tab == Tab::Conversation {
                    return Ok(vec![render(&[ViewSection::Focus])]);
                }
                if let Some(file) = self
                    .view
                    .review
                    .as_ref()
                    .and_then(|open| open.open_file.as_ref())
                    .map(|f| FileRef {
                        repo_id: f.render.repo_id,
                        path: f.render.path.clone(),
                    })
                {
                    self.visual_anchor = None;
                    if let Some(open) = &mut self.view.review {
                        open.original = None;
                    }
                    return match self.viewport(file, 0, PAGE_ROWS - 1) {
                        Ok(mut effects) => {
                            effects.push(render(&[ViewSection::Focus]));
                            Ok(effects)
                        }
                        Err(CoreError::UnknownFile(_)) => self.close_file(),
                        Err(error) => Err(error),
                    };
                }
                Ok(vec![render(&[ViewSection::Focus])])
            }
            Action::ToggleFileCollapse { file } => {
                if self.view.review.is_none() {
                    return Err(CoreError::NoOpenReview);
                }
                let key = (file.repo_id, file.path.clone());
                let effective = self.file_collapse.get(&key).copied().unwrap_or_else(|| {
                    self.view
                        .diff
                        .iter()
                        .chain(&self.view.diffs)
                        .find(|d| d.file == file)
                        .is_some_and(|d| d.viewed == explorer::ViewedState::Viewed)
                });
                self.file_collapse.insert(key, !effective);
                // The stacked view is derived after this returns.
                Ok(Vec::new())
            }
            Action::CollapseParent => {
                if self.view.review.is_none() {
                    return Err(CoreError::NoOpenReview);
                }
                let Focus::Tree { index } = self.view.focus else {
                    return Ok(Vec::new());
                };
                // The dir to collapse: the focused open dir itself, else
                // the node's parent (repo root when at the top level).
                let target: Option<(RepoId, Option<RepoPath>)> = {
                    let visible = focus::visible_nodes(&self.view);
                    visible.get(index).map(|node| match node {
                        explorer::TreeNode::Dir {
                            repo_id,
                            path,
                            expanded: true,
                            ..
                        } => (*repo_id, path.clone()),
                        explorer::TreeNode::Dir { repo_id, path, .. } => {
                            let parent = path
                                .as_ref()
                                .and_then(|p| p.as_str().rsplit_once('/'))
                                .and_then(|(dir, _)| RepoPath::new(dir).ok());
                            (*repo_id, parent)
                        }
                        explorer::TreeNode::File { repo_id, path, .. } => {
                            let parent = path
                                .as_str()
                                .rsplit_once('/')
                                .and_then(|(dir, _)| RepoPath::new(dir).ok());
                            (*repo_id, parent)
                        }
                    })
                };
                let Some((repo_id, path)) = target else {
                    return Ok(Vec::new());
                };
                // Focus follows the collapsed dir: its visible index is
                // unchanged (only its descendants, all after it, vanish).
                let new_index = focus::visible_nodes(&self.view).iter().position(|n| {
                    matches!(n, explorer::TreeNode::Dir { repo_id: r, path: p, .. }
                        if *r == repo_id && *p == path)
                });
                let key = (repo_id, path);
                if self.view.tab == Tab::Browse {
                    self.explorer.expanded.remove(&key);
                } else {
                    self.explorer.collapsed.insert(key);
                }
                if let Some(i) = new_index {
                    self.view.focus = Focus::Tree { index: i };
                }
                // Tree and focus are derived after this returns.
                Ok(Vec::new())
            }
            Action::CollapseAll => {
                if self.view.review.is_none() {
                    return Err(CoreError::NoOpenReview);
                }
                if self.view.tab == Tab::Browse {
                    self.explorer.expanded.clear();
                } else {
                    fn dirs(
                        nodes: &[explorer::TreeNode],
                        out: &mut Vec<(RepoId, Option<RepoPath>)>,
                    ) {
                        for n in nodes {
                            if let explorer::TreeNode::Dir {
                                repo_id,
                                path,
                                children,
                                ..
                            } = n
                            {
                                out.push((*repo_id, path.clone()));
                                dirs(children, out);
                            }
                        }
                    }
                    let mut keys = Vec::new();
                    dirs(&self.view.tree.roots, &mut keys);
                    self.explorer.collapsed.extend(keys);
                }
                if matches!(self.view.focus, Focus::Tree { .. }) {
                    self.view.focus = Focus::Tree { index: 0 };
                }
                Ok(Vec::new())
            }
            Action::ToggleSidebar => {
                let prefs = ViewPrefs {
                    sidebar_hidden: !self.view.prefs.sidebar_hidden,
                    ..self.view.prefs
                };
                Ok(self.apply_prefs(prefs, true))
            }
            // The clipboard is the shell's, and a write only works inside
            // the gesture that asked for it, so the shell copies during
            // the click or key press — reading `ViewModel::copy_target`,
            // which is this same decision made here. The action stays so
            // the command is binding-reachable and other hosts can send
            // it; there is nothing left for the core to do with it.
            Action::CopyCheckout { repo_id: _ }
            | Action::CopyPath { path: _ }
            | Action::CopyReference { reference: _ } => Ok(Vec::new()),
            Action::ScrollView { align } => {
                let Focus::Diff { row, .. } = self.view.focus else {
                    return Err(CoreError::NoTarget(focus::NoTarget::Nothing(
                        Command::CenterView,
                    )));
                };
                self.scroll_to(row, align);
                Ok(vec![render(&[ViewSection::Focus])])
            }
            Action::MarkViewed { file } => self.mark_viewed(file, true),
            Action::UnmarkViewed { file } => self.mark_viewed(file, false),
            Action::ListCommits { repo_id } => {
                let review_id = self.open_review_id()?;
                self.require_subscribed()?;
                Ok(vec![self.request(
                    Request::ListCommits { review_id, repo_id },
                    InFlight::ListCommits { repo_id },
                )])
            }
            Action::StepCommit { selected } => {
                let Some(stepper) = &self.stepper else {
                    return Err(CoreError::NoStepper);
                };
                if let Some(i) = selected
                    && i >= stepper.commits.len()
                {
                    return Err(CoreError::CommitOutOfRange(i));
                }
                // In by-commit scope the cursor *is* the diff: stepping
                // re-targets the file list (worktree = the final step).
                let by_commit = self.view.review.as_ref().is_some_and(|open| {
                    matches!(
                        open.scope,
                        DiffScope::Commit { .. } | DiffScope::Worktree { .. }
                    )
                });
                if by_commit {
                    let repo_id = stepper.repo_id;
                    let scope = match selected {
                        Some(i) => DiffScope::Commit {
                            repo_id,
                            oid: stepper.commits[i].oid,
                        },
                        None => DiffScope::Worktree { repo_id },
                    };
                    return Ok(self.apply_scope(scope));
                }
                Ok(Vec::new())
            }
            Action::SetScope { scope } => {
                self.require_subscribed()?;
                let Some(open) = &self.view.review else {
                    return Err(CoreError::NoOpenReview);
                };
                let review_id = open.snapshot.review.id;
                let wire = match scope {
                    ScopeChoice::Requested { request_id } => DiffScope::Requested { request_id },
                    ScopeChoice::SinceCheckpoint { checkpoint_id } => {
                        DiffScope::SinceCheckpoint { checkpoint_id }
                    }
                    ScopeChoice::All => DiffScope::All,
                    ScopeChoice::Committed => DiffScope::Committed,
                    ScopeChoice::Commit { repo_id, oid } => DiffScope::Commit { repo_id, oid },
                    ScopeChoice::Worktree { repo_id } => DiffScope::Worktree { repo_id },
                    ScopeChoice::ByCommit => {
                        let repo_id = open.snapshot.review.targets.first().repo_id;
                        let have_stepper = self
                            .stepper
                            .as_ref()
                            .is_some_and(|st| st.repo_id == repo_id);
                        if !have_stepper {
                            // No step list yet: fetch it and enter
                            // by-commit when it answers.
                            self.by_commit_pending = true;
                            return Ok(vec![self.request(
                                Request::ListCommits { review_id, repo_id },
                                InFlight::ListCommits { repo_id },
                            )]);
                        }
                        self.default_by_commit_scope(repo_id)
                            .ok_or(CoreError::NoStepper)?
                    }
                };
                let mut effects = self.user(Action::SetTab {
                    tab: Tab::FilesChanged,
                })?;
                effects.extend(self.apply_scope(wire));
                Ok(effects)
            }
            Action::OpenOriginalDiff { thread_id } => self.open_original(thread_id),
            Action::ExpandContext { file, full } => self.expand_context(&file, full),
            Action::ExpandGap { file, gap, dir } => self.expand_gap(&file, gap, dir),
            Action::ContentSearch { query, all_files } => {
                let Some(open) = &self.view.review else {
                    return Err(CoreError::NoOpenReview);
                };
                let review_id = open.snapshot.review.id;
                let scope = open.scope;
                match query {
                    None => {
                        self.latest_search = None;
                        self.view.content_search = None;
                        Ok(vec![render(&[ViewSection::Search])])
                    }
                    Some(query) => {
                        let ask = !query.trim().is_empty();
                        if ask {
                            self.require_subscribed()?;
                        }
                        self.latest_search = None;
                        self.view.content_search = Some(ContentSearchView {
                            query: query.clone(),
                            all_files,
                            hits: Vec::new(),
                            truncated: false,
                            pending: ask,
                            selected: 0,
                        });
                        let mut effects = Vec::new();
                        if ask {
                            effects.push(self.request(
                                Request::Search {
                                    review_id,
                                    query,
                                    all_files,
                                    scope,
                                },
                                InFlight::Search,
                            ));
                        }
                        effects.push(render(&[ViewSection::Search]));
                        Ok(effects)
                    }
                }
            }
            Action::ActionPalette { open } => {
                if self.view.action_palette == open {
                    return Ok(Vec::new());
                }
                self.view.action_palette = open;
                Ok(vec![render(&[ViewSection::Search])])
            }
            Action::RunCommand { command } => {
                let action = focus::resolve(self, command)?;
                self.user(action)
            }
            Action::EnterVisual => {
                if self.view.review.is_none() {
                    return Err(CoreError::NoOpenReview);
                }
                let Focus::Diff { row, side } = self.view.focus else {
                    return Err(CoreError::NoTarget(focus::NoTarget::Nothing(
                        Command::VisualMode,
                    )));
                };
                if self.view.diff.as_ref().is_some_and(|diff| {
                    matches!(diff.content, nits_protocol::RenderContent::Submodule)
                }) {
                    return Err(CoreError::NoTarget(focus::NoTarget::Nothing(
                        Command::VisualMode,
                    )));
                }
                self.visual_anchor = Some(VisualAnchor { row, side });
                // The selection and mode are derived after this returns.
                Ok(Vec::new())
            }
            Action::LeaveVisual => {
                if self.visual_anchor.take().is_none() {
                    return Err(CoreError::NoTarget(focus::NoTarget::Nothing(
                        Command::VisualMode,
                    )));
                }
                Ok(Vec::new())
            }
            Action::SearchFirst { search } => match search {
                SearchKind::Files => {
                    if self.explorer.search.is_none() {
                        return Err(CoreError::NoTarget(focus::NoTarget::Nothing(
                            Command::MoveDown,
                        )));
                    }
                    self.explorer.search_selected = 0;
                    Ok(Vec::new())
                }
                SearchKind::Content => {
                    let Some(search) = &mut self.view.content_search else {
                        return Err(CoreError::NoTarget(focus::NoTarget::Nothing(
                            Command::MoveDown,
                        )));
                    };
                    search.selected = 0;
                    Ok(vec![render(&[ViewSection::Search])])
                }
            },
            Action::SearchStep { search, delta } => {
                let step = |sel: usize, len: usize| -> usize {
                    if len == 0 {
                        return 0;
                    }
                    let max = i64::try_from(len - 1).unwrap_or(i64::MAX);
                    let cur = i64::try_from(sel).unwrap_or(i64::MAX).min(max);
                    usize::try_from((cur + i64::from(delta)).clamp(0, max)).unwrap_or(0)
                };
                match search {
                    SearchKind::Files => {
                        let Some(search) = &self.view.tree.search else {
                            return Err(CoreError::NoTarget(focus::NoTarget::Nothing(
                                Command::MoveDown,
                            )));
                        };
                        self.explorer.search_selected = step(search.selected, search.hits.len());
                        Ok(Vec::new())
                    }
                    SearchKind::Content => {
                        let Some(search) = &mut self.view.content_search else {
                            return Err(CoreError::NoTarget(focus::NoTarget::Nothing(
                                Command::MoveDown,
                            )));
                        };
                        search.selected = step(search.selected, search.hits.len());
                        Ok(vec![render(&[ViewSection::Search])])
                    }
                }
            }
            Action::OpenSearchResult { search, query } => {
                let target = match search {
                    SearchKind::Files => self
                        .view
                        .tree
                        .search
                        .as_ref()
                        .filter(|s| s.query == query)
                        .and_then(|s| s.hits.get(s.selected))
                        .map(|hit| (hit.file.clone(), 0, 59)),
                    SearchKind::Content => self
                        .view
                        .content_search
                        .as_ref()
                        .filter(|s| s.query == query && !s.pending)
                        .and_then(|s| s.hits.get(s.selected))
                        .map(|hit| {
                            (
                                FileRef {
                                    repo_id: hit.repo_id,
                                    path: hit.path.clone(),
                                },
                                hit.line.get().saturating_sub(30),
                                hit.line.get().saturating_add(30),
                            )
                        }),
                };
                let Some((file, first_row, last_row)) = target else {
                    return Ok(Vec::new());
                };
                let mut effects = self.user(Action::Viewport {
                    file,
                    first_row,
                    last_row,
                })?;
                if search == SearchKind::Content {
                    self.view.content_search = None;
                    self.latest_search = None;
                    effects.push(render(&[ViewSection::Search]));
                }
                Ok(effects)
            }
            Action::OpenRefSelector { repo_id, side } => {
                self.open_ref_selector(repo_id, RefSelectorPurpose::Review { side })
            }
            Action::OpenBrowseRefSelector { repo_id } => {
                self.open_ref_selector(repo_id, RefSelectorPurpose::Browse)
            }
            Action::SelectBrowseRepo { repo_id } => {
                self.require_browse_repo(repo_id)?;
                self.close_browse_selector();
                self.browse_repo = Some(repo_id);
                self.browse_attempt = None;
                Ok(Vec::new())
            }
            Action::ResetBrowse => {
                let repo_id = self.browse_repo().ok_or(CoreError::NoOpenReview)?;
                self.set_browse_ref(repo_id, None)
            }
            Action::RefSelectorQuery { query } => {
                let Some(selector) = &mut self.ref_selector else {
                    return Err(CoreError::NoRefSelector);
                };
                selector.query(query);
                Ok(vec![render(&[ViewSection::RefSelector])])
            }
            Action::RefSelectorStep { delta } => {
                let Some(selector) = &mut self.ref_selector else {
                    return Err(CoreError::NoRefSelector);
                };
                selector.step(delta);
                Ok(vec![render(&[ViewSection::RefSelector])])
            }
            Action::SelectRef { index } => self.select_ref(index),
            Action::SelectCurrentRef => {
                let index = self
                    .ref_selector
                    .as_ref()
                    .ok_or(CoreError::NoRefSelector)?
                    .view
                    .selected;
                self.select_ref(index)
            }
            Action::CloseRefSelector => {
                let selector = self.ref_selector.take().ok_or(CoreError::NoRefSelector)?;
                if selector.view.purpose == RefSelectorPurpose::Browse {
                    self.browse_attempt = None;
                }
                Ok(vec![render(&[ViewSection::RefSelector])])
            }
            Action::SetBrowseRef { repo_id, ref_spec } => self.set_browse_ref(repo_id, ref_spec),
            Action::InformationalNoteOpened => {
                let effects = self.user(Action::DraftOpened {
                    anchor: Anchor::Review,
                })?;
                if let Some(draft) = &mut self.view.draft {
                    draft.purpose = DraftPurpose::Comment {
                        intent: nits_protocol::CommentIntent::Informational,
                        context: None,
                    };
                }
                Ok(effects)
            }
            Action::DraftOpened { anchor } => {
                if self.view.review.is_none() {
                    return Err(CoreError::NoOpenReview);
                }
                match &anchor {
                    Anchor::File { .. } | Anchor::Lines { .. } => {
                        self.require_commentable_context()?;
                    }
                    Anchor::Review => {}
                }
                if self.view.draft.is_some() {
                    return Err(CoreError::DraftAlreadyOpen);
                }
                self.visual_anchor = None;
                self.view.draft = Some(Draft {
                    submission_error: None,
                    purpose: DraftPurpose::Comment {
                        intent: nits_protocol::CommentIntent::Finding,
                        context: self.comment_context(&anchor),
                    },
                    anchor,
                });
                self.enter(Focus::Composer);
                Ok(vec![render(&[ViewSection::Draft, ViewSection::Focus])])
            }
            Action::ReplyOpened { thread_id } => {
                let Some(open) = &self.view.review else {
                    return Err(CoreError::NoOpenReview);
                };
                if self.view.draft.is_some() {
                    return Err(CoreError::DraftAlreadyOpen);
                }
                let root = open
                    .snapshot
                    .threads
                    .iter()
                    .find(|t| t.id == thread_id)
                    .and_then(|t| open.snapshot.comments.iter().find(|c| c.id == t.root))
                    .ok_or(CoreError::UnknownThread(thread_id))?;
                self.view.draft = Some(Draft {
                    submission_error: None,
                    purpose: DraftPurpose::Reply { thread_id },
                    anchor: root.anchor.clone(),
                });
                self.enter(Focus::Composer);
                Ok(vec![render(&[ViewSection::Draft, ViewSection::Focus])])
            }
            Action::SetFocus { focus } => {
                if focus::clamp(&self.view, focus) != focus {
                    return Err(CoreError::FocusOutOfRange(focus));
                }
                self.view.focus = focus;
                if matches!(focus, Focus::Tree { .. } | Focus::Diff { .. }) {
                    self.browse_repo = focus::target_repo_of(&self.view, focus);
                }
                if matches!(focus, Focus::ReviewRequest { .. }) {
                    self.view.tab = Tab::Conversation;
                }
                self.view.focused_comment = None;
                let mut effects = Vec::new();
                // A focused row outside the viewport scrolls the viewport.
                if let Focus::Diff { row, .. } = focus
                    && let Some(open) = &self.view.review
                    && let Some(f) = &open.open_file
                    && (row < f.first_row || row > f.last_row)
                {
                    let file = FileRef {
                        repo_id: f.render.repo_id,
                        path: f.render.path.clone(),
                    };
                    let first_row = row.saturating_sub(PAGE_ROWS / 2);
                    effects.extend(self.viewport(file, first_row, first_row + PAGE_ROWS - 1)?);
                }
                effects.push(render(&[ViewSection::Focus]));
                Ok(effects)
            }
            Action::ToggleHelp => {
                // Help may cover a composer, which already has a return focus.
                // Preserve that modal return while remembering Help's own origin.
                if let Some(focus) = self.help_return.take() {
                    self.view.focus = focus;
                } else {
                    self.help_return = Some(self.view.focus);
                    self.view.focus = Focus::Help;
                }
                Ok(vec![render(&[ViewSection::Help, ViewSection::Focus])])
            }
            Action::DraftSubmitted { body } => {
                let Some(review) = &self.view.review else {
                    return Err(CoreError::NoOpenReview);
                };
                let Some(draft) = &self.view.draft else {
                    return Err(CoreError::NoDraft);
                };
                self.require_subscribed()?;
                let review_id = review.snapshot.review.id;
                let anchor = draft.anchor.clone();
                let purpose = draft.purpose.clone();
                let comment_id = self.ids.comment_id(self.now);
                let mutation = match purpose {
                    DraftPurpose::Defer { .. } => return Err(CoreError::NoDraft),
                    DraftPurpose::Reply { thread_id } => Mutation::Reply {
                        review_id,
                        thread_id,
                        comment_id,
                        kind: CommentKind::Note,
                        body,
                    },
                    DraftPurpose::Comment { intent, context } => Mutation::AddComment {
                        review_id,
                        comment_id,
                        kind: intent.into(),
                        anchor,
                        body,
                        context,
                    },
                };
                let mut effects = self.mutate(mutation)?;
                self.view.draft = None;
                self.visual_anchor = None;
                self.leave();
                effects.extend(self.drain_deferred());
                Ok(effects)
            }
            Action::Reply { thread_id, body } => {
                let review_id = self.open_review_id()?;
                let comment_id = self.ids.comment_id(self.now);
                self.mutate(Mutation::Reply {
                    review_id,
                    thread_id,
                    comment_id,
                    kind: CommentKind::Note,
                    body,
                })
            }
            Action::EditComment { comment_id, body } => {
                let review_id = self.open_review_id()?;
                self.mutate(Mutation::EditComment {
                    review_id,
                    comment_id,
                    body,
                })
            }
            Action::DeleteComment { comment_id } => {
                let review_id = self.open_review_id()?;
                self.mutate(Mutation::DeleteComment {
                    review_id,
                    comment_id,
                })
            }
            Action::DeferOpened { thread_id } => {
                let Some(open) = &self.view.review else {
                    return Err(CoreError::NoOpenReview);
                };
                if self.view.draft.is_some() {
                    return Err(CoreError::DraftAlreadyOpen);
                }
                let thread = open
                    .snapshot
                    .threads
                    .iter()
                    .find(|t| t.id == thread_id)
                    .ok_or(CoreError::UnknownThread(thread_id))?;
                match thread.resolution {
                    nits_protocol::ThreadResolution::Open => {}
                    nits_protocol::ThreadResolution::Informational => {
                        return Err(CoreError::Mutation(MutationError::InformationalThread(
                            thread_id,
                        )));
                    }
                    nits_protocol::ThreadResolution::Resolved { .. }
                    | nits_protocol::ThreadResolution::Deferred { .. } => {
                        return Err(CoreError::Mutation(MutationError::NotOpen(thread_id)));
                    }
                }
                let root = open
                    .snapshot
                    .comments
                    .iter()
                    .find(|c| c.id == thread.root)
                    .ok_or(CoreError::UnknownThread(thread_id))?;
                self.view.draft = Some(Draft {
                    submission_error: None,
                    anchor: root.anchor.clone(),
                    purpose: DraftPurpose::Defer { thread_id },
                });
                self.enter(Focus::Composer);
                Ok(vec![render(&[ViewSection::Draft, ViewSection::Focus])])
            }
            Action::DeferThread {
                thread_id,
                reason,
                tracking_url,
            } => {
                let review_id = self.open_review_id()?;
                let mut effects = self.mutate(Mutation::DeferThread {
                    review_id,
                    thread_id,
                    reason,
                    tracking_url,
                })?;
                if self
                    .view
                    .draft
                    .as_ref()
                    .is_some_and(|d| d.purpose == (DraftPurpose::Defer { thread_id }))
                {
                    self.view.draft = None;
                    self.leave();
                    effects.extend(self.drain_deferred());
                }
                Ok(effects)
            }
            Action::ResolveThread { thread_id } => {
                let review_id = self.open_review_id()?;
                self.mutate(Mutation::ResolveThread {
                    review_id,
                    thread_id,
                })
            }
            Action::UnresolveThread { thread_id } => {
                let review_id = self.open_review_id()?;
                self.mutate(Mutation::UnresolveThread {
                    review_id,
                    thread_id,
                })
            }
            Action::CheckCurrent | Action::CheckRequested => {
                self.require_subscribed()?;
                if matches!(action, Action::CheckCurrent) && !self.check_current_ready() {
                    return Err(CoreError::CurrentChangesRefreshing);
                }
                let snapshot = self.committed.as_ref().ok_or(CoreError::NoOpenReview)?;
                let review_id = snapshot.review.id;
                let (targets, in_reply_to) = if matches!(action, Action::CheckRequested) {
                    let Focus::ReviewRequest { index } = self.view.focus else {
                        return Err(CoreError::NoOpenReview);
                    };
                    let request = snapshot
                        .requests
                        .get(index)
                        .ok_or(CoreError::NoOpenReview)?;
                    let nits_protocol::RequestedTargets::Captured { targets } = &request.targets
                    else {
                        return Err(CoreError::NoOpenReview);
                    };
                    (
                        targets.clone(),
                        Some(nits_protocol::ReviewRound::Request {
                            request_id: request.id,
                        }),
                    )
                } else {
                    (
                        snapshot.resolved.clone().ok_or(CoreError::NoOpenReview)?,
                        match self.view.scope {
                            DiffScope::Requested { request_id } => {
                                Some(nits_protocol::ReviewRound::Request { request_id })
                            }
                            DiffScope::SinceCheckpoint { checkpoint_id } => {
                                Some(nits_protocol::ReviewRound::Checkpoint { checkpoint_id })
                            }
                            DiffScope::All
                            | DiffScope::Committed
                            | DiffScope::Commit { .. }
                            | DiffScope::Worktree { .. } => None,
                        },
                    )
                };
                let client_seq = self.next_client_seq;
                self.next_client_seq = client_seq.next();
                Ok(vec![self.request(
                    Request::Mutate {
                        client_seq,
                        mutation: Mutation::RecordCheckpoint {
                            review_id,
                            targets,
                            in_reply_to,
                        },
                    },
                    InFlight::Mutate { client_seq },
                )])
            }
            Action::CheckpointDelta => {
                self.require_subscribed()?;
                let checkpoints = &self.view.checkpoints;
                let next = match self.view.scope {
                    DiffScope::SinceCheckpoint { checkpoint_id } => checkpoints
                        .iter()
                        .position(|c| c.checkpoint.id == checkpoint_id)
                        .map_or(0, |i| (i + 1) % checkpoints.len().max(1)),
                    DiffScope::All
                    | DiffScope::Committed
                    | DiffScope::Commit { .. }
                    | DiffScope::Worktree { .. }
                    | DiffScope::Requested { .. } => 0,
                };
                let checkpoint_id = checkpoints
                    .get(next)
                    .ok_or(CoreError::NoOpenReview)?
                    .checkpoint
                    .id;
                self.view.tab = Tab::FilesChanged;
                Ok(self.apply_scope(DiffScope::SinceCheckpoint { checkpoint_id }))
            }
            Action::ApplySuggestion { comment_id } => self.apply_suggestion(comment_id),
            Action::PreviewSuggestion { comment_id } => self.preview_suggestion(comment_id),
            Action::DraftDiscarded => {
                if self.view.draft.is_none() {
                    return Err(CoreError::NoDraft);
                }
                self.view.draft = None;
                self.visual_anchor = None;
                self.leave();
                Ok(self.drain_deferred())
            }
        }
    }

    /// Move focus into a modal panel (composer, help), remembering where
    /// to come back to.
    fn enter(&mut self, focus: Focus) {
        if self.focus_return.is_none() {
            self.focus_return = Some(self.view.focus);
        }
        self.view.focus = focus;
    }

    /// Leave the modal panel; `derive` clamps the restored focus.
    fn leave(&mut self) {
        if let Some(f) = self.focus_return.take() {
            self.view.focus = f;
        }
    }

    fn open_review_id(&self) -> Result<ReviewId, CoreError> {
        self.view
            .review
            .as_ref()
            .map(|r| r.snapshot.review.id)
            .ok_or(CoreError::NoOpenReview)
    }

    fn select_ref(&mut self, index: usize) -> Result<Vec<Effect>, CoreError> {
        let Some(selector) = &self.ref_selector else {
            return Err(CoreError::NoRefSelector);
        };
        let repo_id = selector.view.repo_id;
        let selection = selector
            .selection_at(index)
            .ok_or(CoreError::NoSelectedRef)?;
        let revision = match selection {
            ref_selector::RefSelection::Review { revision } => revision,
            ref_selector::RefSelection::Browse { ref_spec } => {
                let mut effects = self.set_browse_ref(repo_id, Some(ref_spec))?;
                if let Some(selector) = &mut self.ref_selector {
                    selector.view.selected = index;
                    selector.view.status = RefSelectorStatus::Saving;
                }
                effects.push(render(&[ViewSection::RefSelector]));
                return Ok(effects);
            }
        };
        self.require_subscribed()?;
        let Some(open) = &self.view.review else {
            return Err(CoreError::NoOpenReview);
        };
        let mutation = Mutation::UpdateReviewTarget {
            review_id: open.snapshot.review.id,
            update: ReviewTargetUpdate { repo_id, revision },
        };
        let meta = self.meta_now();
        let body = local_event(&open.snapshot, &meta, &mutation)?;
        let Some(selector) = &mut self.ref_selector else {
            return Err(CoreError::NoRefSelector);
        };
        selector.view.selected = index;
        selector.view.status = RefSelectorStatus::Saving;
        let mut effects = self.mutate_with(mutation, body);
        effects.push(render(&[ViewSection::RefSelector]));
        Ok(effects)
    }

    /// What this client's own events carry, at the core's current time.
    fn meta_now(&self) -> EventMeta {
        EventMeta {
            author: self.config.author.clone(),
            ts: Timestamp::from_millis(i64::try_from(self.now).unwrap_or(i64::MAX)),
        }
    }

    /// Mark or unmark `file` viewed at its current head blob. The event body
    /// needs the blob, which only the file list knows, so it is built here
    /// rather than by `local_event`.
    fn mark_viewed(&mut self, file: FileRef, viewed: bool) -> Result<Vec<Effect>, CoreError> {
        self.require_subscribed()?;
        let Some(open) = &self.view.review else {
            return Err(CoreError::NoOpenReview);
        };
        let review_id = open.snapshot.review.id;
        let Some(human) = self.config.author.as_human() else {
            return Err(CoreError::NotHuman);
        };
        let Some(render) = open
            .files
            .iter()
            .find(|k| k.repo_id == file.repo_id && k.path == file.path)
        else {
            return Err(CoreError::UnknownFile(file));
        };
        let content = match &render.target {
            RenderTarget::Diff { change } => change.viewed_content(),
            RenderTarget::Blob { entry } => nits_protocol::ViewedContent::Blob { entry: *entry },
        };
        let (mutation, body) = if viewed {
            (
                Mutation::MarkViewed {
                    review_id,
                    repo_id: file.repo_id,
                    path: file.path.clone(),
                },
                EventBody::FileViewed {
                    review_id,
                    repo_id: file.repo_id,
                    path: file.path,
                    viewer: human,
                    content,
                },
            )
        } else {
            (
                Mutation::UnmarkViewed {
                    review_id,
                    repo_id: file.repo_id,
                    path: file.path.clone(),
                },
                EventBody::FileUnviewed {
                    review_id,
                    repo_id: file.repo_id,
                    path: file.path,
                    viewer: human,
                },
            )
        };
        Ok(self.mutate_with(mutation, body))
    }

    /// Apply `mutation` optimistically and send it. Rejected (nothing sent,
    /// nothing shown) when the daemon would reject it against the current
    /// view, pending mutations included.
    fn mutate(&mut self, mutation: Mutation) -> Result<Vec<Effect>, CoreError> {
        self.require_subscribed()?;
        let Some(open) = &self.view.review else {
            return Err(CoreError::NoOpenReview);
        };
        let meta = self.meta_now();
        let body = local_event(&open.snapshot, &meta, &mutation)?;
        Ok(self.mutate_with(mutation, body))
    }

    /// `mutate` with the optimistic event already built and validated.
    fn mutate_with(&mut self, mutation: Mutation, body: EventBody) -> Vec<Effect> {
        let meta = self.meta_now();
        let client_seq = self.next_client_seq;
        self.next_client_seq = client_seq.next();
        self.pending.push(Pending {
            client_seq,
            ts: meta.ts,
            mutation: mutation.clone(),
            body,
            delivery: PendingDelivery::AwaitingReceipt,
        });
        let send = self.request(
            Request::Mutate {
                client_seq,
                mutation,
            },
            InFlight::Mutate { client_seq },
        );
        let sections = self.rebase();
        vec![send, render(&sections)]
    }

    /// Rebuild the shown snapshot as `committed` plus every pending event,
    /// and mirror the pending list into the view. Returns the sections the
    /// pending events touch (`Threads` when the list just emptied, so the
    /// pending marks clear).
    fn rebase(&mut self) -> Vec<ViewSection> {
        let (Some(committed), Some(open)) = (&self.committed, &mut self.view.review) else {
            return Vec::new();
        };
        let mut shown = committed.clone();
        // The header's resolved refs follow the committed snapshot, or the
        // scoped targets when a narrower scope is on screen.
        self.view.resolved_targets = match open.scope {
            DiffScope::All => committed
                .resolved
                .as_ref()
                .map(|r| r.iter().cloned().collect())
                .unwrap_or_default(),
            DiffScope::Committed
            | DiffScope::Commit { .. }
            | DiffScope::Worktree { .. }
            | DiffScope::Requested { .. }
            | DiffScope::SinceCheckpoint { .. } => open.scoped_targets.clone(),
        };
        let mut sections = Vec::new();
        let author = self.config.author.clone();
        for p in &self.pending {
            let meta = EventMeta {
                author: author.clone(),
                ts: p.ts,
            };
            sections.extend(apply_body(&mut shown, &meta, &p.body));
        }
        let was_pending = !open.pending.is_empty();
        open.snapshot = shown;
        open.pending = self
            .pending
            .iter()
            .map(|p| PendingEvent {
                client_seq: p.client_seq,
                body: p.body.clone(),
            })
            .collect();
        if was_pending && open.pending.is_empty() {
            sections.push(ViewSection::Threads);
        }
        sections
    }

    /// Retire the pending entry the daemon has answered (by echo or error).
    fn retire_pending(&mut self, client_seq: ClientSeq) -> bool {
        let before = self.pending.len();
        self.pending.retain(|p| p.client_seq != client_seq);
        self.pending.len() != before
    }

    /// Drop the open review and everything content-side it pinned.
    fn close_review(&mut self, effects: &mut Vec<Effect>) {
        self.view.review = None;
        self.view.focused_comment = None;
        self.view.open_review = None;
        self.view.resolved_targets.clear();
        self.view.draft = None;
        self.view.pending_refresh = false;
        self.latest_files = None;
        self.latest_commits = None;
        self.deferred.clear();
        self.committed = None;
        self.pending.clear();
        self.suggestions.clear();
        self.stepper = None;
        self.focus_return = None;
        self.help_return = None;
        self.by_commit_pending = false;
        self.browse = None;
        self.browse_attempt = None;
        self.browse_repo = None;
        self.visual_anchor = None;
        self.view.content_search = None;
        self.latest_search = None;
        self.view.action_palette = false;
        self.ref_selector = None;
        self.review_closed(effects);
    }

    /// A review snapshot arrived (streamed or single): it becomes the open
    /// review, replacing any other. Pending mutations for the same review
    /// survive (they are still on their way to the daemon).
    fn install_snapshot(
        &mut self,
        id: RequestId,
        mut snapshot: ReviewSnapshot,
        effects: &mut Vec<Effect>,
    ) {
        // Reference navigation owns complete replay in land_reference, where
        // linked discussion and revision records advance together. Ordinary
        // opens retain their in-flight revision events here.
        if let Some((_, mut events)) = self.snapshot_events.remove(&id)
            && self.pending_reference.is_none()
        {
            events.sort_by_key(|event| event.seq);
            for event in events {
                if event.seq > snapshot.seq {
                    events::apply_event(&mut snapshot, &event);
                }
            }
        }
        let same = self
            .committed
            .as_ref()
            .is_some_and(|c| c.review.id == snapshot.review.id);
        let pending = if same {
            std::mem::take(&mut self.pending)
        } else {
            Vec::new()
        };
        let suggestions = if same {
            std::mem::take(&mut self.suggestions)
        } else {
            BTreeMap::new()
        };
        // A preview may discover a committed receipt before its event or a
        // concurrently requested older snapshot arrives.
        if same && let Some(committed) = &self.committed {
            for existing in &committed.suggestions {
                if let nits_protocol::SuggestionOutcome::Applied { receipt } = &existing.outcome
                    && receipt.seq > snapshot.seq
                    && let Some(record) = snapshot
                        .suggestions
                        .iter_mut()
                        .find(|record| record.comment_id == existing.comment_id)
                {
                    record.outcome = existing.outcome.clone();
                }
            }
        }
        self.close_review(effects);
        self.suggestions = suggestions;
        self.committed = Some(snapshot.clone());
        self.view.open_review = Some(snapshot.review.id);
        self.view.review = Some(OpenReview::new(snapshot));
        self.pending = pending;
        self.rebase();
        // Keys go to the explorer of the review that just opened.
        self.view.focus = Focus::Tree { index: 0 };
        // The commits list sits under the file tree from the start
        // (UI-DESIGN §Layout), so fetch it with the review rather than
        // waiting for by-commit mode.
        if let Some(open) = &self.view.review {
            let review_id = open.snapshot.review.id;
            if let Some(repo_id) = open
                .snapshot
                .review
                .targets
                .iter()
                .next()
                .map(|t| t.repo_id)
            {
                let req = self.request(
                    Request::ListCommits { review_id, repo_id },
                    InFlight::ListCommits { repo_id },
                );
                effects.push(req);
            }
        }
    }

    fn supersede_opening_content(&mut self, review: ReviewId) {
        for waiting in self.in_flight.values_mut() {
            if let InFlight::OpenReview { review_id, content } = waiting
                && *review_id == review
                && *content == OpeningContent::Streaming
            {
                *content = OpeningContent::Superseded;
            }
        }
    }

    /// The explicit current check is unavailable while the shown content is
    /// being replaced, including a refresh held back for a draft.
    fn check_current_ready(&self) -> bool {
        self.view.review.as_ref().is_some_and(|open| {
            open.snapshot.resolved.is_some() && self.latest_files.is_none()
                && (open.open_file.is_none() || self.view.diff.as_ref().is_some_and(|diff| diff.missing.is_empty()))
                && !self.view.pending_refresh
                && !self.in_flight.values().any(|waiting| matches!(waiting,
                    InFlight::OpenReview { review_id, content: OpeningContent::AwaitingSnapshot | OpeningContent::Streaming }
                    if *review_id == open.snapshot.review.id))
        })
    }

    fn require_subscribed(&self) -> Result<(), CoreError> {
        match self.connection {
            Connection::Subscribed { .. } => Ok(()),
            Connection::Disconnected { .. } | Connection::Connecting { .. } => {
                Err(self.wrong_state(InputKind::User))
            }
        }
    }

    pub(crate) fn request(&mut self, request: Request, waiting: InFlight) -> Effect {
        let id = RequestId::new(self.next_request);
        self.next_request += 1;
        if matches!(
            waiting,
            InFlight::OpenReview { .. } | InFlight::ReviewSnapshot { .. }
        ) {
            self.latest_open = Some(id);
        }
        if matches!(waiting, InFlight::Search) {
            self.latest_search = Some(id);
        }
        if let Request::OpenReview { review_id, .. } | Request::ReviewSnapshot { review_id } =
            &request
        {
            self.snapshot_events.insert(id, (*review_id, Vec::new()));
        }
        if matches!(request, Request::ListFiles { .. }) {
            self.latest_files = Some(id);
        }
        if matches!(request, Request::ListCommits { .. }) {
            self.latest_commits = Some(id);
        }
        self.in_flight.insert(id, waiting);
        Effect::Send(ClientMsg::Request { id, request })
    }

    fn transport(&mut self, ev: TransportEvent) -> Vec<Effect> {
        match ev {
            TransportEvent::Connected => match self.connection {
                Connection::Connecting {
                    hello_sent: false,
                    last_seq,
                } => {
                    self.connection = Connection::Connecting {
                        hello_sent: true,
                        last_seq,
                    };
                    vec![Effect::Send(ClientMsg::Hello {
                        client_id: self.config.client_id,
                        protocol: ProtocolVersion::CURRENT,
                        client: self.config.client.clone(),
                        author: self.config.author.clone(),
                    })]
                }
                // A duplicate or unsolicited "connected" changes nothing.
                Connection::Connecting {
                    hello_sent: true, ..
                }
                | Connection::Disconnected { .. }
                | Connection::Subscribed { .. } => Vec::new(),
            },
            TransportEvent::Disconnected => {
                let last_seq = self.connection.last_seq();
                let was_down = matches!(self.connection, Connection::Disconnected { .. });
                self.connection = Connection::Disconnected { last_seq };
                let suggestions_changed = self.suggestions_disconnected();
                self.clear_in_flight();
                self.browse_disconnected();
                if was_down {
                    return Vec::new();
                }
                self.invalidate_daemon_status();
                self.view.connection = self
                    .restart_disconnected_view()
                    .unwrap_or(ConnectionView::Disconnected);
                let mut sections = vec![ViewSection::Connection];
                if suggestions_changed {
                    sections.extend([ViewSection::Threads, ViewSection::Conversation]);
                }
                if self.creation_disconnected() {
                    sections.push(ViewSection::ReviewList);
                }
                if let Some(selector) = &mut self.ref_selector
                    && matches!(selector.view.status, RefSelectorStatus::Loading)
                {
                    selector.view.status = RefSelectorStatus::DaemonError {
                        message: "daemon disconnected".into(),
                    };
                    sections.push(ViewSection::RefSelector);
                }
                vec![render(&sections)]
            }
        }
    }

    // One arm per variant; splitting would hide the exhaustive match.
    #[allow(clippy::too_many_lines)]
    fn server(&mut self, msg: ServerMsg) -> Result<Vec<Effect>, CoreError> {
        match msg {
            ServerMsg::Welcome { .. } => match self.connection {
                Connection::Connecting {
                    hello_sent: true,
                    last_seq,
                } => {
                    let since = match last_seq {
                        Some(seq) => Since::After { seq },
                        None => Since::Now,
                    };
                    Ok(vec![self.request(
                        Request::Subscribe {
                            scope: SubscribeScope::All,
                            since,
                        },
                        InFlight::Subscribe,
                    )])
                }
                Connection::Connecting {
                    hello_sent: false, ..
                }
                | Connection::Disconnected { .. }
                | Connection::Subscribed { .. } => Err(self.wrong_state(InputKind::Server)),
            },
            ServerMsg::Rejected { error } => match self.connection {
                Connection::Connecting { .. } => {
                    self.connection = Connection::Disconnected {
                        last_seq: self.connection.last_seq(),
                    };
                    self.clear_in_flight();
                    if let RpcError::UnsupportedProtocol { supported, .. } = &error {
                        self.recovery = lifecycle::Recovery::Idle;
                        self.view.connection = ConnectionView::UpgradeRequired {
                            client: ProtocolVersion::CURRENT,
                            supported: supported.clone(),
                        };
                        Ok(vec![render(&[ViewSection::Connection])])
                    } else if matches!(error, RpcError::Restarting { .. })
                        && !matches!(self.recovery, lifecycle::Recovery::Idle)
                    {
                        self.view.connection = self
                            .restart_disconnected_view()
                            .unwrap_or(ConnectionView::Rejected { error });
                        Ok(vec![render(&[ViewSection::Connection])])
                    } else {
                        self.recovery = lifecycle::Recovery::Idle;
                        self.view.connection = ConnectionView::Rejected {
                            error: error.clone(),
                        };
                        Err(CoreError::Rejected(error))
                    }
                }
                Connection::Disconnected { .. } | Connection::Subscribed { .. } => {
                    Err(self.wrong_state(InputKind::Server))
                }
            },
            ServerMsg::Response { id, response } => self.response(id, response),
            ServerMsg::StreamItem { id, item } => self.stream_item(id, item),
            ServerMsg::StreamEnd { id } => {
                let Some(waiting) = self.in_flight.remove(&id) else {
                    return Err(CoreError::UnknownRequest(id));
                };
                let mut effects = Vec::new();
                match waiting {
                    InFlight::OpenReview { review_id, content } => {
                        self.snapshot_events.remove(&id);
                        if self.latest_open == Some(id)
                            && content == OpeningContent::Streaming
                            && self.open_mut(review_id).is_some()
                        {
                            // The streamed open carries the file list; land
                            // on the first diff (UI-DESIGN §Layout).
                            self.auto_open_first(&mut effects);
                            effects.push(render(&[ViewSection::Diff]));
                        }
                    }
                    InFlight::FileRender { render, .. } => {
                        // The header may never have come (cancelled early or
                        // errored); clear the pending mark so it can be retried.
                        self.content_failed(&CacheKey::Header { render });
                        self.content_done(&mut effects);
                    }
                    InFlight::Subscribe
                    | InFlight::CreationDefault { .. }
                    | InFlight::CreateReview { .. }
                    | InFlight::ReconcileCreation { .. }
                    | InFlight::CheckFailedCreation { .. }
                    | InFlight::ListWorkspaces
                    | InFlight::ListReviews { .. }
                    | InFlight::ReviewSnapshot { .. }
                    | InFlight::ListFiles { .. }
                    | InFlight::ListCommits { .. }
                    | InFlight::ListRefs { .. }
                    | InFlight::Search
                    | InFlight::TreeSnapshot { .. }
                    | InFlight::BrowseTree { .. }
                    | InFlight::RenderChunk { .. }
                    | InFlight::PreviewSuggestion { .. }
                    | InFlight::ApplySuggestion { .. }
                    | InFlight::Mutate { .. } => {
                        self.in_flight.insert(id, waiting);
                        return Err(CoreError::UnexpectedResponse {
                            id,
                            expected: "Response",
                            got: "StreamEnd",
                        });
                    }
                }
                Ok(effects)
            }
            ServerMsg::Error { id, error } => {
                self.snapshot_events.remove(&id);
                let Some(waiting) = self.in_flight.remove(&id) else {
                    return Err(CoreError::UnknownRequest(id));
                };
                if matches!(
                    waiting,
                    InFlight::OpenReview { .. } | InFlight::ReviewSnapshot { .. }
                ) {
                    if self.latest_open != Some(id) {
                        return Ok(Vec::new());
                    }
                    self.pending_reference = None;
                }
                match &waiting {
                    InFlight::PreviewSuggestion {
                        review_id,
                        comment_id,
                    }
                    | InFlight::ApplySuggestion {
                        review_id,
                        comment_id,
                    } => {
                        return Ok(self.suggestion_failed(id, *review_id, *comment_id, &error));
                    }
                    InFlight::CreationDefault { review_id, repo_id } => {
                        return Ok(self.creation_default_answer(
                            *review_id,
                            *repo_id,
                            Err(rpc_error_message(&error)),
                        ));
                    }
                    InFlight::CreateReview { review_id, .. } => {
                        return Ok(self.creation_write_failed(*review_id, error));
                    }
                    InFlight::CheckFailedCreation {
                        review_id,
                        error: original,
                    } => {
                        return Ok(self.creation_failure_checked(*review_id, original, &error));
                    }
                    InFlight::ReconcileCreation { review_id } => {
                        return Ok(self.creation_lookup_failed(*review_id, &error));
                    }
                    InFlight::BrowseTree { .. } => return Ok(self.browse_failed(id, &error)),
                    InFlight::ListRefs { repo_id }
                        if !self
                            .ref_selector
                            .as_ref()
                            .is_some_and(|selector| selector.matches_request(*repo_id, id)) =>
                    {
                        return Ok(Vec::new());
                    }
                    InFlight::Subscribe
                    | InFlight::ListWorkspaces
                    | InFlight::ListReviews { .. }
                    | InFlight::OpenReview { .. }
                    | InFlight::ReviewSnapshot { .. }
                    | InFlight::ListFiles { .. }
                    | InFlight::ListCommits { .. }
                    | InFlight::ListRefs { .. }
                    | InFlight::Search
                    | InFlight::TreeSnapshot { .. }
                    | InFlight::FileRender { .. }
                    | InFlight::RenderChunk { .. }
                    | InFlight::Mutate { .. } => {}
                }
                self.view.last_error = Some(error.clone());
                let mut effects = Vec::new();
                let mut sections = vec![ViewSection::Connection];
                if let InFlight::Subscribe = waiting {
                    // Subscription failed: stay connected but not subscribed;
                    // the host may retry with `Connect` after disconnecting.
                    self.view.connection = ConnectionView::Connecting;
                }
                if let InFlight::Mutate { client_seq } = waiting {
                    let target_update = self.pending.iter().any(|pending| {
                        pending.client_seq == client_seq
                            && matches!(pending.mutation, Mutation::UpdateReviewTarget { .. })
                    });
                    match &error {
                        RpcError::RestartInterrupted { .. } => {
                            if let Some(pending) = self
                                .pending
                                .iter_mut()
                                .find(|pending| pending.client_seq == client_seq)
                            {
                                pending.delivery = PendingDelivery::OutcomeUnknown;
                            }
                        }
                        RpcError::Restarting { .. } => {
                            if let Some(pending) = self
                                .pending
                                .iter_mut()
                                .find(|pending| pending.client_seq == client_seq)
                            {
                                pending.delivery = PendingDelivery::NotAdmitted;
                            }
                        }
                        RpcError::UnsupportedProtocol { .. }
                        | RpcError::VersionMismatch { .. }
                        | RpcError::Invalid { .. }
                        | RpcError::NotFound { .. }
                        | RpcError::Forbidden { .. }
                        | RpcError::Internal { .. }
                        | RpcError::Cancelled
                        | RpcError::SeqTooOld { .. } => {
                            self.retire_pending(client_seq);
                        }
                    }
                    sections.extend(self.rebase());
                    sections.push(ViewSection::Threads);
                    if target_update
                        && let Some(selector) = &mut self.ref_selector
                        && matches!(selector.view.purpose, RefSelectorPurpose::Review { .. })
                    {
                        selector.view.status = selector_error(&error);
                        sections.push(ViewSection::RefSelector);
                    }
                }
                if let InFlight::ListRefs { repo_id } = waiting
                    && let Some(selector) = &mut self.ref_selector
                    && selector.matches_request(repo_id, id)
                {
                    selector.view.status = RefSelectorStatus::DaemonError {
                        message: rpc_error_message(&error),
                    };
                    sections.push(ViewSection::RefSelector);
                }
                if let Some(key) = waiting.key() {
                    self.content_failed(&key);
                }
                if waiting.is_content() {
                    self.content_done(&mut effects);
                }
                effects.push(render(&sections));
                Ok(effects)
            }
            ServerMsg::Event { event } => match self.connection {
                // A mutation response and the subscription tail carry the
                // same event.  If the response wins the race, its broadcast
                // echo is an expected no-op rather than a stale event.
                Connection::Subscribed { last_seq }
                    if event.seq == last_seq && event.client_id == self.config.client_id =>
                {
                    Ok(Vec::new())
                }
                Connection::Subscribed { last_seq } if event.seq <= last_seq => {
                    Err(CoreError::StaleEvent {
                        seq: event.seq,
                        last_seq,
                    })
                }
                Connection::Subscribed { .. } => {
                    self.connection = Connection::Subscribed {
                        last_seq: event.seq,
                    };
                    Ok(self.apply_event(event))
                }
                // The daemon replays the gap after `Since::After` *before*
                // answering the subscribe, so events are valid once the
                // `Subscribe` request is out.
                Connection::Connecting {
                    hello_sent: true,
                    last_seq: Some(last_seq),
                } if event.seq <= last_seq => Err(CoreError::StaleEvent {
                    seq: event.seq,
                    last_seq,
                }),
                Connection::Connecting {
                    hello_sent: true,
                    last_seq,
                } if last_seq.is_some() => {
                    self.connection = Connection::Connecting {
                        hello_sent: true,
                        last_seq: Some(event.seq),
                    };
                    Ok(self.apply_event(event))
                }
                Connection::Disconnected { .. } | Connection::Connecting { .. } => {
                    Err(self.wrong_state(InputKind::Server))
                }
            },
            ServerMsg::TreeDelta { delta } => match self.connection {
                Connection::Subscribed { .. } => Ok(self.tree_delta(&delta)),
                Connection::Disconnected { .. } | Connection::Connecting { .. } => {
                    Err(self.wrong_state(InputKind::Server))
                }
            },
            ServerMsg::Lifecycle {
                notice: nits_protocol::LifecycleNotice::Restarting { operation },
            } => Ok(self.restart_notice(operation)),
        }
    }

    fn clear_in_flight(&mut self) {
        self.snapshot_events.clear();
        self.pending_reference = None;
        self.latest_open = None;
        let keys: Vec<CacheKey> = self.in_flight.values().filter_map(InFlight::key).collect();
        for k in &keys {
            self.content_failed(k);
        }
        self.in_flight.clear();
        self.content_reset_in_flight();
        for p in &mut self.pending {
            if p.delivery == PendingDelivery::AwaitingReceipt {
                p.delivery = PendingDelivery::OutcomeUnknown;
            }
        }
    }

    // One arm per variant; splitting would hide the exhaustive match.
    #[allow(clippy::too_many_lines)]
    fn stream_item(&mut self, id: RequestId, item: StreamItem) -> Result<Vec<Effect>, CoreError> {
        let Some(waiting) = self.in_flight.get(&id).cloned() else {
            return Err(CoreError::UnknownRequest(id));
        };
        if matches!(
            waiting,
            InFlight::OpenReview {
                content: OpeningContent::Superseded,
                ..
            }
        ) || (matches!(waiting, InFlight::OpenReview { .. }) && self.latest_open != Some(id))
        {
            return Ok(Vec::new());
        }
        let got = stream_item_name(&item);
        let unexpected = |expected| CoreError::UnexpectedResponse { id, expected, got };
        let mut effects = Vec::new();
        match (waiting, item) {
            (InFlight::OpenReview { review_id, .. }, StreamItem::ReviewSnapshot { snapshot }) => {
                if snapshot.review.id != review_id {
                    return Err(unexpected("ReviewSnapshot for the requested review"));
                }
                let streamed_targets = snapshot.resolved.clone();
                self.install_snapshot(id, snapshot, &mut effects);
                self.land_reference(&mut effects);
                let current_targets = self.committed.as_ref().and_then(|s| s.resolved.as_ref());
                let content = if self.view.review.is_none() {
                    OpeningContent::Superseded
                } else if current_targets == streamed_targets.as_ref() {
                    self.expect_streamed_trees(&mut effects);
                    OpeningContent::Streaming
                } else {
                    self.review_opened_piecewise(review_id, &mut effects);
                    OpeningContent::Superseded
                };
                self.in_flight
                    .insert(id, InFlight::OpenReview { review_id, content });
                effects.push(render(&[
                    ViewSection::ReviewList,
                    ViewSection::Diff,
                    ViewSection::Threads,
                    ViewSection::Conversation,
                    ViewSection::Draft,
                    ViewSection::Focus,
                ]));
            }
            (InFlight::OpenReview { .. }, StreamItem::TreeSnapshot { snapshot }) => {
                let key = CacheKey::Tree {
                    tree: TreeKey::of_snapshot(&snapshot),
                };
                self.arrived(
                    key,
                    CacheValue::Tree { snapshot },
                    content::Arrival::Stream,
                    &mut effects,
                );
            }
            (InFlight::OpenReview { review_id, .. }, StreamItem::Header { header }) => {
                let render = RenderKey::of_header(&header);
                // Only the review stream discovers changed files. Standalone
                // renders may outlive their Browse ref or original thread view.
                if let Some(open) = &mut self.view.review
                    && open.snapshot.review.id == review_id
                    && matches!(render.target, RenderTarget::Diff { .. })
                    && !open.files.contains(&render)
                {
                    open.files.push(render.clone());
                }
                let key = CacheKey::Header { render };
                self.arrived(
                    key,
                    CacheValue::Header { header },
                    content::Arrival::Stream,
                    &mut effects,
                );
            }
            (
                InFlight::OpenReview { .. },
                StreamItem::Chunk {
                    repo_id,
                    path,
                    chunk,
                },
            ) => {
                let Some(render) = self.view.review.as_ref().and_then(|r| {
                    r.files
                        .iter()
                        .find(|k| k.repo_id == repo_id && k.path == path)
                        .cloned()
                }) else {
                    return Err(unexpected("Chunk of a file whose header was streamed"));
                };
                let key = CacheKey::Chunk {
                    render,
                    index: chunk.index,
                };
                self.arrived(
                    key,
                    CacheValue::Chunk { chunk },
                    content::Arrival::Stream,
                    &mut effects,
                );
            }
            (InFlight::FileRender { render, .. }, StreamItem::Header { header }) => {
                if RenderKey::of_header(&header) != render {
                    return Err(unexpected("Header of the requested file"));
                }
                let key = CacheKey::Header { render };
                self.arrived(
                    key,
                    CacheValue::Header { header },
                    content::Arrival::Stream,
                    &mut effects,
                );
            }
            (
                InFlight::FileRender { render, stop_after },
                StreamItem::Chunk {
                    repo_id,
                    path,
                    chunk,
                },
            ) => {
                if repo_id != render.repo_id || path != render.path {
                    return Err(unexpected("Chunk of the requested file"));
                }
                self.stream_chunk(id, &render, stop_after, chunk, &mut effects);
            }
            (
                InFlight::FileRender { .. },
                StreamItem::ReviewSnapshot { .. } | StreamItem::TreeSnapshot { .. },
            ) => return Err(unexpected("Header or Chunk")),
            (
                InFlight::Subscribe
                | InFlight::CreationDefault { .. }
                | InFlight::CreateReview { .. }
                | InFlight::ReconcileCreation { .. }
                | InFlight::CheckFailedCreation { .. }
                | InFlight::ListWorkspaces
                | InFlight::ListReviews { .. }
                | InFlight::ReviewSnapshot { .. }
                | InFlight::ListFiles { .. }
                | InFlight::ListCommits { .. }
                | InFlight::ListRefs { .. }
                | InFlight::Search
                | InFlight::TreeSnapshot { .. }
                | InFlight::BrowseTree { .. }
                | InFlight::RenderChunk { .. }
                | InFlight::PreviewSuggestion { .. }
                | InFlight::ApplySuggestion { .. }
                | InFlight::Mutate { .. },
                StreamItem::ReviewSnapshot { .. }
                | StreamItem::TreeSnapshot { .. }
                | StreamItem::Header { .. }
                | StreamItem::Chunk { .. },
            ) => return Err(unexpected("Response")),
        }
        Ok(effects)
    }

    // One arm per variant; splitting would hide the exhaustive match.
    #[allow(clippy::too_many_lines)]
    fn response(&mut self, id: RequestId, response: Response) -> Result<Vec<Effect>, CoreError> {
        let Some(waiting) = self.in_flight.get(&id).cloned() else {
            return Err(CoreError::UnknownRequest(id));
        };
        if matches!(waiting, InFlight::ReviewSnapshot { .. }) && self.latest_open != Some(id) {
            self.in_flight.remove(&id);
            self.snapshot_events.remove(&id);
            return Ok(Vec::new());
        }
        let got = response_name(&response);
        let effects = match (waiting, response) {
            (InFlight::Subscribe, Response::Subscribed { seq }) => {
                let Connection::Connecting { .. } = self.connection else {
                    return Err(self.wrong_state(InputKind::Server));
                };
                let last_seq = self.connection.last_seq().map_or(seq, |s| s.max(seq));
                self.connection = Connection::Subscribed { last_seq };
                self.view.connection = ConnectionView::Subscribed;
                self.invalidate_daemon_status();
                self.recovery = lifecycle::Recovery::Idle;
                self.view.last_error = None;
                // Only a typed rejection before admission proves a retry safe.
                // Interrupted or lost acknowledgements never replay a write.
                let resend: Vec<(ClientSeq, Mutation)> = self
                    .pending
                    .iter_mut()
                    .filter(|p| p.delivery == PendingDelivery::NotAdmitted)
                    .map(|p| {
                        p.delivery = PendingDelivery::AwaitingReceipt;
                        (p.client_seq, p.mutation.clone())
                    })
                    .collect();
                let mut effects: Vec<Effect> = resend
                    .into_iter()
                    .map(|(client_seq, mutation)| {
                        self.request(
                            Request::Mutate {
                                client_seq,
                                mutation,
                            },
                            InFlight::Mutate { client_seq },
                        )
                    })
                    .collect();
                // The review list is the union of every workspace's reviews;
                // start with the workspaces.
                effects.push(self.request(Request::ListWorkspaces, InFlight::ListWorkspaces));
                effects.extend(self.creation_reconnected());
                effects.extend(self.suggestions_reconnected());
                effects.push(render(&[ViewSection::Connection]));
                effects
            }
            (InFlight::CreationDefault { review_id, repo_id }, Response::DefaultBase { base }) => {
                self.creation_default_answer(review_id, repo_id, Ok(base))
            }
            (
                InFlight::ReconcileCreation { review_id }
                | InFlight::CheckFailedCreation { review_id, .. },
                Response::Review { review },
            ) => {
                if review.id != review_id {
                    return Err(CoreError::UnexpectedResponse {
                        id,
                        expected: "Review for the attempted ID",
                        got: "Review for another ID",
                    });
                }
                self.creation_found(&review);
                self.view.reviews.retain(|r| r.id != review.id);
                self.view.reviews.push(review);
                vec![render(&[ViewSection::ReviewList])]
            }
            (InFlight::CreateReview { review_id, .. }, Response::Committed { event }) => {
                if event.body.review_id() != Some(review_id) {
                    return Err(CoreError::UnexpectedResponse {
                        id,
                        expected: "Committed creation for the attempted ID",
                        got: "Committed event for another review",
                    });
                }
                let mut effects = Vec::new();
                if self.creation_observe(&event) {
                    effects.push(render(&[ViewSection::ReviewList]));
                }
                if let Connection::Subscribed { last_seq } = self.connection
                    && event.seq > last_seq
                {
                    self.connection = Connection::Subscribed {
                        last_seq: event.seq,
                    };
                    effects.extend(self.apply_event(event));
                }
                effects
            }
            (InFlight::ListWorkspaces, Response::Workspaces { workspaces }) => {
                let ids: Vec<WorkspaceId> = workspaces.iter().map(|w| w.id).collect();
                self.view.workspaces = workspaces;
                let mut effects: Vec<Effect> = ids
                    .into_iter()
                    .map(|workspace_id| {
                        self.request(
                            Request::ListReviews { workspace_id },
                            InFlight::ListReviews { workspace_id },
                        )
                    })
                    .collect();
                effects.push(render(&[ViewSection::ReviewList]));
                effects
            }
            (InFlight::ListReviews { workspace_id }, Response::Reviews { reviews }) => {
                // Replace this workspace's reviews, keep the others'.
                self.view.reviews.retain(|r| r.workspace_id != workspace_id);
                self.view.reviews.extend(reviews);
                self.view.reviews.sort_by_key(|r| r.created);
                vec![render(&[ViewSection::ReviewList])]
            }
            (InFlight::ReviewSnapshot { review_id }, Response::ReviewSnapshot { snapshot }) => {
                if snapshot.review.id != review_id {
                    return Err(CoreError::UnexpectedResponse {
                        id,
                        expected: "ReviewSnapshot for the requested review",
                        got: "ReviewSnapshot for another review",
                    });
                }
                let mut effects = Vec::new();
                self.install_snapshot(id, snapshot, &mut effects);
                self.land_reference(&mut effects);
                self.review_opened_piecewise(review_id, &mut effects);
                effects.push(render(&[
                    ViewSection::ReviewList,
                    ViewSection::Diff,
                    ViewSection::Threads,
                    ViewSection::Conversation,
                    ViewSection::Draft,
                    ViewSection::Focus,
                ]));
                effects
            }
            (InFlight::ListFiles { review_id, scope }, Response::Files { files, resolved }) => {
                let mut effects = Vec::new();
                let matches_current = self.view.review.as_ref().is_some_and(|open| {
                    open.snapshot.review.id == review_id
                        && open.scope == scope
                        && (scope != DiffScope::All
                            || open
                                .snapshot
                                .resolved
                                .as_ref()
                                .map_or(resolved.is_empty(), |targets| {
                                    targets.iter().eq(resolved.iter())
                                }))
                });
                if self.latest_files == Some(id) && matches_current {
                    self.latest_files = None;
                    if let Some(open) = &mut self.view.review
                        && !matches!(open.scope, DiffScope::All)
                    {
                        open.scoped_targets = resolved;
                        // The header chips and the explorer trees follow
                        // the scoped targets.
                        let sections = self.rebase();
                        self.want_review_trees(&mut effects);
                        effects.push(render(&sections));
                        effects.push(render(&[ViewSection::ReviewList]));
                    }
                    self.review_files(review_id, files, &mut effects);
                }
                effects
            }
            (InFlight::ListCommits { repo_id }, Response::Commits { commits }) => {
                if self.latest_commits != Some(id) {
                    return Ok(Vec::new());
                }
                self.latest_commits = None;
                let mut effects = Vec::new();
                if let Some(open) = &self.view.review {
                    let has_worktree = open.snapshot.review.targets.iter().any(|target| {
                        target.repo_id == repo_id
                            && matches!(target.head, nits_protocol::RefSpec::WorkingTree)
                    });
                    self.stepper =
                        Some(CommitStepper::from_commits(repo_id, &commits, has_worktree));
                    if self.by_commit_pending {
                        self.by_commit_pending = false;
                        if let Some(scope) = self.default_by_commit_scope(repo_id) {
                            effects = self.apply_scope(scope);
                        }
                    }
                }
                effects
            }
            (
                InFlight::ListRefs { repo_id },
                Response::Refs {
                    repo_id: got_repo,
                    refs,
                },
            ) => {
                if repo_id != got_repo {
                    return Err(CoreError::UnexpectedResponse {
                        id,
                        expected: "Refs for the requested repo",
                        got: "Refs for another repo",
                    });
                }
                if let Some(selector) = &mut self.ref_selector
                    && selector.matches_request(repo_id, id)
                {
                    selector.install(refs);
                    vec![render(&[ViewSection::RefSelector])]
                } else {
                    Vec::new()
                }
            }
            (InFlight::Search, Response::Search { hits, truncated }) => {
                if self.latest_search == Some(id)
                    && let Some(cs) = &mut self.view.content_search
                {
                    self.latest_search = None;
                    cs.hits = hits;
                    cs.selected = cs.selected.min(cs.hits.len().saturating_sub(1));
                    cs.truncated = truncated;
                    cs.pending = false;
                    vec![render(&[ViewSection::Search])]
                } else {
                    Vec::new()
                }
            }
            (InFlight::BrowseTree { repo_id }, Response::TreeSnapshot { snapshot }) => {
                self.browse_answer(id, repo_id, snapshot)?
            }
            (InFlight::TreeSnapshot { tree }, Response::TreeSnapshot { snapshot }) => {
                if TreeKey::of_snapshot(&snapshot) != tree {
                    return Err(CoreError::UnexpectedResponse {
                        id,
                        expected: "TreeSnapshot of the requested repository and root",
                        got: "TreeSnapshot of another repository or root",
                    });
                }
                let mut effects = Vec::new();
                self.arrived(
                    CacheKey::Tree { tree },
                    CacheValue::Tree { snapshot },
                    content::Arrival::Response,
                    &mut effects,
                );
                self.content_done(&mut effects);
                effects
            }
            (InFlight::RenderChunk { key }, Response::RenderChunk { chunk }) => {
                let CacheKey::Chunk { index, .. } = &key else {
                    return Err(CoreError::UnexpectedResponse {
                        id,
                        expected: "RenderChunk",
                        got: "RenderChunk for a non-chunk key",
                    });
                };
                if chunk.index != *index {
                    return Err(CoreError::UnexpectedResponse {
                        id,
                        expected: "RenderChunk with the requested index",
                        got: "RenderChunk with another index",
                    });
                }
                let mut effects = Vec::new();
                self.arrived(
                    key,
                    CacheValue::Chunk { chunk },
                    content::Arrival::Response,
                    &mut effects,
                );
                self.content_done(&mut effects);
                effects
            }
            (
                InFlight::PreviewSuggestion {
                    review_id,
                    comment_id,
                },
                Response::SuggestionPreview { preview },
            ) => self.suggestion_previewed(id, review_id, comment_id, preview)?,
            (
                InFlight::ApplySuggestion {
                    review_id,
                    comment_id,
                },
                Response::Committed { event },
            ) => self.suggestion_applied(id, review_id, comment_id, event)?,
            (InFlight::Mutate { client_seq }, Response::Committed { event }) => {
                // The same event is also broadcast; whichever arrives first
                // applies it, the other only retires the pending entry.
                match self.connection {
                    Connection::Subscribed { last_seq } if last_seq >= event.seq => {
                        if self.retire_pending(client_seq) {
                            let sections = self.rebase();
                            vec![render(&sections)]
                        } else {
                            Vec::new()
                        }
                    }
                    Connection::Subscribed { .. } => {
                        self.connection = Connection::Subscribed {
                            last_seq: event.seq,
                        };
                        self.apply_event(event)
                    }
                    Connection::Disconnected { .. } | Connection::Connecting { .. } => {
                        return Err(self.wrong_state(InputKind::Server));
                    }
                }
            }
            (waiting, _) => {
                let expected = match waiting {
                    InFlight::PreviewSuggestion { .. } => "SuggestionPreview",
                    InFlight::Subscribe => "Subscribed",
                    InFlight::CreationDefault { .. } => "DefaultBase",
                    InFlight::CreateReview { .. }
                    | InFlight::Mutate { .. }
                    | InFlight::ApplySuggestion { .. } => "Committed",
                    InFlight::ReconcileCreation { .. } | InFlight::CheckFailedCreation { .. } => {
                        "Review"
                    }
                    InFlight::ListWorkspaces => "Workspaces",
                    InFlight::ListReviews { .. } => "Reviews",
                    InFlight::OpenReview { .. } | InFlight::FileRender { .. } => "StreamItem",
                    InFlight::ReviewSnapshot { .. } => "ReviewSnapshot",
                    InFlight::ListFiles { .. } => "Files",
                    InFlight::ListCommits { .. } => "Commits",
                    InFlight::ListRefs { .. } => "Refs",
                    InFlight::Search => "Search",
                    InFlight::TreeSnapshot { .. } | InFlight::BrowseTree { .. } => "TreeSnapshot",
                    InFlight::RenderChunk { .. } => "RenderChunk",
                };
                return Err(CoreError::UnexpectedResponse { id, expected, got });
            }
        };
        self.in_flight.remove(&id);
        self.snapshot_events.remove(&id);
        Ok(effects)
    }

    /// Fold a committed event into the view: the review list first, then the
    /// open review's committed snapshot, then the pending list on top.
    // One arm per event; splitting would hide the exhaustive match.
    #[allow(clippy::too_many_lines)]
    fn apply_event(&mut self, event: Event) -> Vec<Effect> {
        if matches!(
            &event.body,
            EventBody::ReviewRequested { .. }
                | EventBody::ReviewChecked { .. }
                | EventBody::SuggestionApplied { .. }
                | EventBody::ReviewTargetsResolved { .. }
        ) {
            for (review_id, events) in self.snapshot_events.values_mut() {
                if event.body.review_id() == Some(*review_id)
                    && !events.iter().any(|e| e.seq == event.seq)
                {
                    events.push(event.clone());
                }
            }
        }
        if let Some(pending) = &mut self.pending_reference
            && event.body.review_id() == Some(pending.reference.review_id)
        {
            pending.events.push(event.clone());
        }
        let mut sections = Vec::new();
        let mut effects = Vec::new();
        if self.creation_observe(&event) {
            sections.push(ViewSection::ReviewList);
        }
        if event.client_id == self.config.client_id {
            // Our own mutation came back: it is committed now.
            self.retire_pending(event.client_seq);
        }
        match &event.body {
            EventBody::ReviewCreated { review } => {
                self.view.reviews.retain(|r| r.id != review.id);
                self.view.reviews.push(review.clone());
                sections.push(ViewSection::ReviewList);
            }
            EventBody::ReviewUpdated {
                review_id,
                title,
                status,
            } => {
                if let Some(r) = self.view.reviews.iter_mut().find(|r| r.id == *review_id) {
                    r.title.clone_from(title);
                    r.status = *status;
                    sections.push(ViewSection::ReviewList);
                }
            }
            EventBody::ReviewTargetUpdated { review_id, target } => {
                if let Some(review) = self.view.reviews.iter_mut().find(|r| r.id == *review_id)
                    && let Some(existing) = review
                        .targets
                        .iter_mut()
                        .find(|existing| existing.repo_id == target.repo_id)
                {
                    existing.clone_from(target);
                    sections.push(ViewSection::ReviewList);
                }
                let close = self.ref_selector.as_ref().is_some_and(|selector| {
                    selector.view.repo_id == target.repo_id
                        && matches!(selector.view.purpose, RefSelectorPurpose::Review { .. })
                        && self
                            .committed
                            .as_ref()
                            .is_some_and(|snapshot| snapshot.review.id == *review_id)
                });
                if close {
                    self.ref_selector = None;
                    sections.push(ViewSection::RefSelector);
                }
            }
            EventBody::ReviewDeleted { review_id } => {
                let before = self.view.reviews.len();
                self.view.reviews.retain(|r| r.id != *review_id);
                if self.view.reviews.len() != before {
                    sections.push(ViewSection::ReviewList);
                }
                if self.open_mut(*review_id).is_some() {
                    self.close_review(&mut effects);
                    sections.extend([
                        ViewSection::ReviewList,
                        ViewSection::Diff,
                        ViewSection::Threads,
                        ViewSection::Draft,
                    ]);
                }
            }
            EventBody::ReviewTargetsResolved { review_id, .. }
                if self.open_mut(*review_id).is_some() && self.view.draft.is_some() =>
            {
                // Held back until the draft closes (§5.4).
                self.deferred.push(event);
                if !self.view.pending_refresh {
                    self.view.pending_refresh = true;
                    sections.push(ViewSection::Draft);
                }
                return if sections.is_empty() {
                    Vec::new()
                } else {
                    vec![render(&sections)]
                };
            }
            EventBody::WorkspaceCreated { workspace } => {
                self.view.workspaces.retain(|w| w.id != workspace.id);
                self.view.workspaces.push(workspace.clone());
                sections.push(ViewSection::ReviewList);
            }
            EventBody::WorkspaceUpdated { workspace_id, name } => {
                if let Some(w) = self
                    .view
                    .workspaces
                    .iter_mut()
                    .find(|w| w.id == *workspace_id)
                {
                    w.name.clone_from(name);
                    sections.push(ViewSection::ReviewList);
                }
            }
            EventBody::RepoAttached { workspace_id, repo } => {
                if let Some(w) = self
                    .view
                    .workspaces
                    .iter_mut()
                    .find(|w| w.id == *workspace_id)
                {
                    w.repos.retain(|r| r.id != repo.id);
                    w.repos.push(repo.clone());
                    sections.push(ViewSection::ReviewList);
                }
            }
            EventBody::RepoDetached {
                workspace_id,
                repo_id,
            } => {
                if let Some(w) = self
                    .view
                    .workspaces
                    .iter_mut()
                    .find(|w| w.id == *workspace_id)
                {
                    w.repos.retain(|r| r.id != *repo_id);
                    sections.push(ViewSection::ReviewList);
                }
            }
            EventBody::ReviewTargetsResolved { .. }
            | EventBody::CommentCreated { .. }
            | EventBody::CommentEdited { .. }
            | EventBody::CommentDeleted { .. }
            | EventBody::CommentReanchored { .. }
            | EventBody::ThreadDeferred { .. }
            | EventBody::ThreadResolved { .. }
            | EventBody::ThreadUnresolved { .. }
            | EventBody::FileViewed { .. }
            | EventBody::FileUnviewed { .. }
            | EventBody::ReviewRequested { .. }
            | EventBody::ReviewChecked { .. }
            | EventBody::SuggestionApplied { .. } => {}
        }
        let concerns_open = event
            .body
            .review_id()
            .is_some_and(|id| self.committed.as_ref().is_some_and(|c| c.review.id == id));
        if concerns_open {
            if let EventBody::ReviewTargetsResolved { review_id, .. } = &event.body {
                self.supersede_opening_content(*review_id);
                if let Some(open) = &mut self.view.review {
                    open.files.clear();
                    open.open_file = None;
                    open.original = None;
                    open.scoped_targets.clear();
                }
                sections.push(ViewSection::Diff);
            }
            if let Some(committed) = &mut self.committed {
                sections.extend(apply_event(committed, &event));
            }
            sections.extend(self.rebase());
            if let EventBody::ReviewTargetsResolved { review_id, .. } = event.body {
                // The header shows the resolved refs.
                sections.push(ViewSection::ReviewList);
                // New heads mean new trees and renders: refetch the trees
                // and the file list; headers re-key by blob.
                self.want_review_trees(&mut effects);
                let scope = self
                    .view
                    .review
                    .as_ref()
                    .map(|r| r.scope)
                    .unwrap_or_default();
                effects.push(self.request(
                    Request::ListFiles { review_id, scope },
                    InFlight::ListFiles { review_id, scope },
                ));
                // Commit identity can move while the content tree is unchanged
                // (for example an amend). Preserve a repository selection whose
                // list is still loading, then fall back to the displayed list.
                // Supersede answers from an earlier resolution in either case.
                let repo_id = self
                    .latest_commits
                    .and_then(|id| self.in_flight.get(&id))
                    .and_then(|request| {
                        if let InFlight::ListCommits { repo_id } = request {
                            Some(*repo_id)
                        } else {
                            None
                        }
                    })
                    .or_else(|| self.stepper.as_ref().map(|stepper| stepper.repo_id))
                    .or_else(|| {
                        self.view
                            .review
                            .as_ref()
                            .map(|open| open.snapshot.review.targets.first().repo_id)
                    });
                if let Some(repo_id) = repo_id {
                    effects.push(self.request(
                        Request::ListCommits { review_id, repo_id },
                        InFlight::ListCommits { repo_id },
                    ));
                }
            }
        }
        if !sections.is_empty() {
            effects.push(render(&sections));
        }
        effects
    }

    fn open_mut(&mut self, review_id: ReviewId) -> Option<&mut OpenReview> {
        self.view
            .review
            .as_mut()
            .filter(|r| r.snapshot.review.id == review_id)
    }

    /// Apply refreshes held back during a draft; always renders `Draft`
    /// (the draft just closed) plus whatever the refreshes touched, after
    /// any fetches the refreshes issued.
    fn drain_deferred(&mut self) -> Vec<Effect> {
        let deferred = std::mem::take(&mut self.deferred);
        self.view.pending_refresh = false;
        let mut sections = vec![ViewSection::Draft, ViewSection::Focus];
        let mut effects = Vec::new();
        for event in deferred {
            for effect in self.apply_event(event) {
                match effect {
                    Effect::Render(delta) => sections.extend(delta.sections),
                    Effect::ManageDaemon { .. }
                    | Effect::Connect
                    | Effect::Disconnect
                    | Effect::Send(_)
                    | Effect::Persist { .. }
                    | Effect::Load { .. }
                    | Effect::Remove { .. } => effects.push(effect),
                }
            }
        }
        effects.push(render(&sections));
        effects
    }
}

/// What `command` would do right now, without doing it. Hosts use this
/// for menus and tests use it to prove every action is reachable.
pub fn resolve_command(core: &ClientCore, command: Command) -> Result<Action, NoTarget> {
    focus::resolve(core, command)
}

pub(crate) fn render(sections: &[ViewSection]) -> Effect {
    Effect::Render(ViewDelta::new(sections))
}

fn stream_item_name(item: &StreamItem) -> &'static str {
    match item {
        StreamItem::ReviewSnapshot { .. } => "ReviewSnapshot",
        StreamItem::TreeSnapshot { .. } => "TreeSnapshot",
        StreamItem::Header { .. } => "Header",
        StreamItem::Chunk { .. } => "Chunk",
    }
}

fn response_name(r: &Response) -> &'static str {
    match r {
        Response::Workspaces { .. } => "Workspaces",
        Response::Reviews { .. } => "Reviews",
        Response::ReviewDiscovery { .. } => "ReviewDiscovery",
        Response::DefaultBase { .. } => "DefaultBase",
        Response::DirectoryReview { .. } => "DirectoryReview",
        Response::Review { .. } => "Review",
        Response::ReviewSnapshot { .. } => "ReviewSnapshot",
        Response::SuggestionPreview { .. } => "SuggestionPreview",
        Response::Files { .. } => "Files",
        Response::Resolved { .. } => "Resolved",
        Response::Search { .. } => "Search",
        Response::Commits { .. } => "Commits",
        Response::Refs { .. } => "Refs",
        Response::TreeSnapshot { .. } => "TreeSnapshot",
        Response::RenderChunk { .. } => "RenderChunk",
        Response::Subscribed { .. } => "Subscribed",
        Response::ReplayEvents { .. } => "ReplayEvents",
        Response::Unsubscribed => "Unsubscribed",
        Response::Committed { .. } => "Committed",
        Response::ShuttingDown => "ShuttingDown",
    }
}

fn selector_error(error: &RpcError) -> RefSelectorStatus {
    match error {
        RpcError::Invalid { reason } => RefSelectorStatus::InvalidRef {
            message: reason.clone(),
        },
        RpcError::NotFound { kind, id } => RefSelectorStatus::InvalidRef {
            message: format!("{kind:?} {id} was not found"),
        },
        RpcError::Forbidden { .. }
        | RpcError::SeqTooOld { .. }
        | RpcError::Cancelled
        | RpcError::Restarting { .. }
        | RpcError::RestartInterrupted { .. }
        | RpcError::UnsupportedProtocol { .. }
        | RpcError::VersionMismatch { .. }
        | RpcError::Internal { .. } => RefSelectorStatus::DaemonError {
            message: rpc_error_message(error),
        },
    }
}

fn rpc_error_message(error: &RpcError) -> String {
    match error {
        RpcError::NotFound { kind, id } => format!("{kind:?} {id} was not found"),
        RpcError::Invalid { reason } | RpcError::Forbidden { reason } => reason.clone(),
        RpcError::SeqTooOld { oldest } => format!("event history starts at {oldest}"),
        RpcError::Cancelled => "request cancelled".to_owned(),
        RpcError::Restarting { operation_id } => {
            format!("daemon restarting ({operation_id}); request was not admitted")
        }
        RpcError::RestartInterrupted { operation_id } => format!(
            "daemon restarted ({operation_id}); interrupted mutation outcome may be unknown"
        ),
        RpcError::UnsupportedProtocol { requested, .. } => {
            format!("protocol {requested} is unsupported")
        }
        RpcError::VersionMismatch {
            negotiated,
            received,
        } => format!("expected protocol {negotiated}, received {received}"),
        RpcError::Internal { message } => message.clone(),
    }
}
