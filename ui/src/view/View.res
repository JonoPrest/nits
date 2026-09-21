// The client `ViewModel` and everything it contains (nits-client-core
// `view.rs`, `explorer.rs`, `diff.rs`, `focus.rs`, `keymap.rs`), one module
// per Rust type, schemas derived by `@schema`.

open Ids

module Layout = {
  @schema
  type t = Unified | Split
}

module Tab = {
  @schema
  type t = FilesChanged | Conversation | Browse
}

module Mode = {
  @schema
  type t = Normal | Insert | Visual
}

module ViewPrefs = {
  @schema
  type t = {
    layout: Layout.t,
    @as("ignore_whitespace") ignoreWhitespace: bool,
    @as("context_lines") contextLines: int,
    @as("sidebar_hidden") sidebarHidden: bool,
  }
}

module ConnectionView = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("Disconnected") Disconnected({})
    | @as("Connecting") Connecting({})
    | @as("Subscribed") Subscribed({})
    | @as("Rejected") Rejected({error: Rpc.RpcError.t})
  @@warning("+27")
}

module DraftPurpose = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("Comment")
    Comment({
        intent: Domain.CommentIntent.t,
        context: @s.null option<Domain.CommentContext.t>,
      })
    | @as("Reply") Reply({@as("thread_id") threadId: threadId})
    | @as("Defer") Defer({@as("thread_id") threadId: threadId})
  @@warning("+27")
}

module Draft = {
  @schema
  type t = {
    anchor: Domain.Anchor.t,
    purpose: DraftPurpose.t,
    @as("submission_error") submissionError: @s.null option<string>,
  }
  let thread = (draft: t) =>
    switch draft.purpose {
    | Reply({threadId}) | Defer({threadId}) => Some(threadId)
    | Comment(_) => None
    }
  let isDocked = (draft: t) =>
    switch (draft.purpose, draft.anchor) {
    | (Comment(_), Review(_) | File(_)) => true
    | (Comment(_), Lines(_)) | (Reply(_) | Defer(_), _) => false
    }
}

module PendingEvent = {
  @schema
  type t = {@as("client_seq") clientSeq: clientSeq, body: Events.EventBody.t}
}

module FileRef = {
  @schema
  type t = {@as("repo_id") repoId: repoId, path: string}
}

module RenderKey = {
  @schema
  type t = {
    @as("repo_id") repoId: repoId,
    path: string,
    target: Render.RenderTarget.t,
    opts: Domain.RenderOpts.t,
  }
}

module OpenFile = {
  @schema
  type t = {render: RenderKey.t, @as("first_row") firstRow: int, @as("last_row") lastRow: int}
}

module TreeKey = {
  @schema
  type t = {@as("repo_id") repoId: repoId, root: treeOid}
}

module OpenReview = {
  @schema
  type t = {
    snapshot: Domain.ReviewSnapshot.t,
    pending: array<PendingEvent.t>,
    trees: array<TreeKey.t>,
    files: array<RenderKey.t>,
    @as("open_file") openFile: @s.null option<OpenFile.t>,
    scope: Domain.DiffScope.t,
    @as("scoped_targets") scopedTargets: array<Domain.ResolvedTarget.t>,
    original: @s.null option<RenderKey.t>,
  }
}

module ViewedState = {
  @schema
  type t = Viewed | ChangedSinceViewed | Unviewed
}

module ChangeKindKind = {
  @schema
  type t = Added | Deleted | Modified | Renamed | Submodule
}

module TreeNode = {
  // Recursive, so hand-written (the ppx derives non-recursive schemas).
  type rec t =
    | Dir({
        name: string,
        repoId: repoId,
        path: option<string>,
        expanded: bool,
        changedBelow: int,
        children: array<t>,
      })
    | File({
        name: string,
        repoId: repoId,
        path: string,
        change: option<ChangeKindKind.t>,
        viewed: ViewedState.t,
        open_: bool,
        additions: option<int>,
        deletions: option<int>,
        threads: int,
      })
  let schema: S.t<t> = S.recursive(self =>
    S.union([
      S.object(s => {
        s.tag("type", "Dir")
        Dir({
          name: s.field("name", S.string),
          repoId: s.field("repo_id", repoIdSchema),
          path: s.field("path", S.null(S.string)),
          expanded: s.field("expanded", S.bool),
          changedBelow: s.field("changed_below", S.int),
          children: s.field("children", S.array(self)),
        })
      }),
      S.object(s => {
        s.tag("type", "File")
        File({
          name: s.field("name", S.string),
          repoId: s.field("repo_id", repoIdSchema),
          path: s.field("path", S.string),
          change: s.field("change", S.null(ChangeKindKind.schema)),
          viewed: s.field("viewed", ViewedState.schema),
          open_: s.field("open", S.bool),
          additions: s.field("additions", S.null(S.int)),
          deletions: s.field("deletions", S.null(S.int)),
          threads: s.field("threads", S.int),
        })
      }),
    ])
  )
}

module SearchHit = {
  @schema
  type t = {file: FileRef.t, matched: array<int>, change: @s.null option<ChangeKindKind.t>}
}

module SearchView = {
  @schema
  type t = {query: string, hits: array<SearchHit.t>, selected: int}
}

module TreeView = {
  @schema
  type t = {
    roots: array<@s.matches(TreeNode.schema) TreeNode.t>,
    breadcrumbs: array<string>,
    search: @s.null option<SearchView.t>,
  }
}

/// The Visual-mode line selection: row indices of the open file, ordered,
/// both ends inclusive.
/// Where the host should put a row of the diff (`z z`/`z t`/`z b`).
module ScrollAlign = {
  @schema
  type t = Center | Top | Bottom
}

/// How the view should land on a file being opened: a motion that walked
/// over the boundary follows, a deliberate jump pins.
module Landing = {
  @schema
  type t = Follow | Pin
}

/// A reposition the host has not performed yet; `seq` counts the
/// instructions so the same chord twice is two of them.
module ScrollIntent = {
  @schema
  type t = {row: int, align: ScrollAlign.t, seq: int}
}

module VisualView = {
  @schema
  type t = {start: int, @as("end") end_: int, side: Domain.Side.t}
}

module ContentSearchView = {
  @schema
  type t = {
    query: string,
    @as("all_files") allFiles: bool,
    hits: array<Domain.ContentHit.t>,
    truncated: bool,
    pending: bool,
    selected: int,
  }
}

module RefSelectorSide = {
  @schema
  type t = Base | Head
}

module BrowseTarget = {
  @schema type t = {@as("repo_id") repoId: repoId, @as("ref_spec") refSpec: Domain.RefSpec.t}
}
module BrowseStatus = {
  @@warning("-27")
  @schema @tag("type")
  type t = | @as("Loading") Loading({}) | @as("Failed") Failed({message: string})
  @@warning("+27")
}
module BrowseAttemptView = {
  @schema type t = {target: BrowseTarget.t, status: BrowseStatus.t}
}
module BrowseView = {
  @schema
  type t = {
    @as("repo_id") repoId: repoId,
    selection: @s.null option<BrowseTarget.t>,
    attempt: @s.null option<BrowseAttemptView.t>,
  }
}
module RefSelectorPurpose = {
  @@warning("-27")
  @schema @tag("type")
  type t = | @as("Review") Review({side: RefSelectorSide.t}) | @as("Browse") Browse({})
  @@warning("+27")
}
module RefSelectorStatus = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("Loading") Loading({})
    | @as("Ready") Ready({})
    | @as("Saving") Saving({})
    | @as("InvalidRef") InvalidRef({message: string})
    | @as("DaemonError") DaemonError({message: string})
  @@warning("+27")
}

module RefOption = {
  @schema
  type t = {
    @as("ref_spec") refSpec: Domain.RefSpec.t,
    subject: @s.null option<string>,
    current: bool,
  }
}

module RefSelectorView = {
  @schema
  type t = {
    @as("request_id") requestId: requestId,
    @as("repo_id") repoId: repoId,
    @as("repo_name") repoName: string,
    purpose: RefSelectorPurpose.t,
    current: Domain.RefSpec.t,
    query: string,
    options: array<RefOption.t>,
    selected: int,
    status: RefSelectorStatus.t,
  }
}

module Progress = {
  @schema
  type t = {
    viewed: int,
    @as("changed_since_viewed") changedSinceViewed: int,
    total: int,
    additions: int,
    deletions: int,
  }
}

/// What a row is to an anchored range: its last line, under which the
/// card or the composer renders, or a line inside it.
module RowPlace = {
  @schema
  type t = Anchor | Inside
}

/// A thread placed on a row: the half of the row it is anchored to, and
/// whether this row is the range's last line or one inside it.
module RowThread = {
  @schema
  type t = {thread: threadId, side: Domain.Side.t, place: RowPlace.t}
}

module DiffRow = {
  @schema
  type t = {
    index: int,
    row: Render.Row.t,
    threads: array<RowThread.t>,
    /// The open draft covers this row; the composer renders under the
    /// `Anchor` one.
    drafted: @s.null option<(RowPlace.t, Domain.Side.t)>,
  }
}

module DiffView = {
  @schema
  type t = {
    target: Render.RenderTarget.t,
    file: FileRef.t,
    lang: @s.null option<string>,
    content: Render.RenderContent.t,
    viewed: ViewedState.t,
    @as("first_row") firstRow: int,
    @as("last_row") lastRow: int,
    rows: array<DiffRow.t>,
    missing: array<Render.chunkIndex>,
    @as("file_threads") fileThreads: array<threadId>,
    original: bool,
    collapsed: bool,
  }
}

module ThreadPlace = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("Review") Review({})
    | @as("File") File({file: FileRef.t})
    | @as("Lines") Lines({file: FileRef.t, side: Domain.Side.t, start: int, @as("end") end_: int})
  @@warning("+27")
}

module SuggestionStatus = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | Unloaded({})
    | Loading({})
    | Ready({})
    | Stale({})
    | Rejected({message: string})
    | Applying({})
    | Uncertain({message: string})
    | Applied({})
  @@warning("+27")
}
module SuggestionView = {
  @schema
  type t = {
    record: Domain.SuggestionRecord.t,
    inspection: @s.null option<Suggestion.Inspection.t>,
    status: SuggestionStatus.t,
    notice: @s.null option<string>,
  }
}

module CommentView = {
  @schema
  type t = {
    reference: @s.null option<string>,
    id: commentId,
    author: Domain.Author.t,
    created: timestamp,
    body: string,
    suggestion: @s.null option<SuggestionView.t>,
    pending: bool,
  }
}

module ThreadStatus = Domain.ThreadResolution

module ThreadView = {
  @schema
  type t = {
    reference: @s.null option<string>,
    id: threadId,
    root: commentId,
    author: Domain.Author.t,
    created: timestamp,
    summary: string,
    replies: int,
    status: ThreadStatus.t,
    place: ThreadPlace.t,
    outdated: bool,
    pending: bool,
    comments: array<CommentView.t>,
    context: @s.null option<Domain.CommentContext.t>,
  }
}

module StepperCommit = {
  @schema
  type t = {
    oid: commitOid,
    parents: array<commitOid>,
    subject: string,
    body: string,
    author: string,
    time: timestamp,
    committer: string,
    @as("committer_time") committerTime: timestamp,
  }
}

module CommitStepper = {
  @schema
  type t = {
    @as("repo_id") repoId: repoId,
    commits: array<StepperCommit.t>,
    @as("has_worktree") hasWorktree: bool,
  }
}

module Focus = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("ReviewList") ReviewList({index: int})
    | @as("Tree") Tree({index: int})
    | @as("Diff") Diff({row: int, side: Domain.Side.t})
    | @as("Thread") Thread({index: int})
    | @as("ReviewRequest") ReviewRequest({index: int})
    | @as("Composer") Composer({})
    | @as("CommitStepper") CommitStepper({index: int})
    | @as("Help") Help({})
  @@warning("+27")
}

module Context = {
  @schema
  type t = Global | ReviewList | Tree | Diff | Thread | Requests | Composer | CommitStepper | Help
}

module Command = {
  @schema
  type t =
    | MoveDown
    | MoveUp
    | PageDown
    | PageUp
    | GoTop
    | GoBottom
    | NextHunk
    | PrevHunk
    | NextFile
    | PrevFile
    | NextComment
    | PrevComment
    | Open
    | Back
    | NextPanel
    | ToggleViewed
    | Comment
    | ReviewFinding
    | InformationalNote
    | Reply
    | Delete
    | ApplySuggestion
    | PreviewSuggestion
    | ToggleResolved
    | DeferFinding
    | FileSearch
    | ToggleLayout
    | ToggleWhitespace
    | ToggleHelp
    | TabFiles
    | TabConversation
    | TabBrowse
    | BrowseRevision
    | ResetBrowse
    | NextBrowseRepo
    | PrevBrowseRepo
    | ToggleSidebar
    | Submit
    | CopyPath
    | CopyCheckout
    | NewReview
    | AddReviewTarget
    | RemoveReviewTarget
    | ReconnectReviewCreation
    | GoHome
    | CopyReference
    | NextReply
    | PrevReply
    | CollapseParent
    | CollapseAll
    | ToggleFileCollapse
    | ExpandFile
    | FocusTree
    | FocusDiff
    | FocusThreads
    | FocusRequests
    | CheckCurrent
    | CheckRequested
    | CheckpointDelta
    | FocusCommits
    | Connect
    | Disconnect
    | Commits
    | Refresh
    | ScopeAll
    | ScopeByCommit
    | ScopeWorktree
    | ExpandContext
    | ContentSearch
    | ActionPalette
    | VisualMode
    | ExpandUp
    | ExpandDown
    | CommentOnFile
    | SideBase
    | SideHead
    | CenterView
    | ViewTop
    | ViewBottom
}

/// The core's verdict on one key: which key (counted in order from 1)
/// and what it resolved to. `None` means the key meant nothing where it
/// landed, so a shell waiting on it must not act.
module LastKey = {
  @schema
  type t = {seq: int, command: @s.null option<Command.t>}
}

/// A key sequence in its text form (`"g g"`, `"ctrl+p"`).
@schema type keySeq = string

module Hint = {
  @schema
  type t = {keys: keySeq, command: Command.t, label: string}
}

module HelpEntry = {
  @schema
  type t = {keys: keySeq, command: Command.t, label: string, primary: bool, overridden: bool}
}

module HelpGroup = {
  @schema
  type t = {context: Context.t, entries: array<HelpEntry.t>}
}

module Conflict = {
  @schema
  type t = {context: Context.t, keys: keySeq, commands: array<Command.t>}
}

module HelpView = {
  @schema
  type t = {groups: array<HelpGroup.t>, conflicts: array<Conflict.t>}
}

module Override = {
  @schema
  type t = {context: Context.t, command: Command.t, keys: @s.null option<keySeq>, primary: bool}
}

module Overrides = {
  @schema
  type t = {bindings: array<Override.t>}
}

module ViewDelta = {
  @schema
  type t = {sections: array<Rpc.ViewSection.t>}
}

module HomeRowKind = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("Workspace") Workspace({})
    | @as("Repository") Repository({@as("repo_id") repoId: repoId})
    | @as("Review") Review({@as("review_id") reviewId: reviewId})
  @@warning("+27")
}
module HomeRow = {
  @schema
  type t = {@as("workspace_id") workspaceId: workspaceId, kind: HomeRowKind.t}
}
module DaemonContext = {
  @schema @tag("type")
  type t =
    | @as("Named") Named({name: string})
    | @as("Socket") Socket({path: string})
    | @as("WebSocket") WebSocket({url: string})
}

module CreationTargetId = {
  @schema type t = int
}
module CreationRevision = {
  @schema type t = int
}
module CreationEdit = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("Title") Title({text: string})
    | @as("Repository")
    Repository({
        @as("target_id") targetId: CreationTargetId.t,
        @as("repo_id") repoId: repoId,
      })
    | @as("Base") Base({@as("target_id") targetId: CreationTargetId.t, text: string})
    | @as("Head") Head({@as("target_id") targetId: CreationTargetId.t, text: string})
  @@warning("+27")
}
module CreationBase = {
  @@warning("-27")
  @schema @tag("type")
  type t = | @as("Automatic") Automatic({}) | @as("Manual") Manual({text: string})
  @@warning("+27")
}
module CreationTarget = {
  @schema
  type t = {
    id: CreationTargetId.t,
    @as("repo_id") repoId: repoId,
    base: CreationBase.t,
    head: string,
  }
}
module CreationDraft = {
  @schema
  type t = {title: string, targets: array<CreationTarget.t>}
}
module CreationDefaultState = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("Loading") Loading({})
    | @as("Ready") Ready({base: Domain.RefSpec.t})
    | @as("Failed") Failed({message: string})
  @@warning("+27")
}
module CreationDefault = {
  @schema
  type t = {@as("repo_id") repoId: repoId, state: CreationDefaultState.t}
}
module CreationSubmission = {
  @schema
  type t = {title: string, targets: array<Domain.ReviewTarget.t>}
}
module CreationReconcile = {
  @schema
  type t = Inspect | Retry
}
module CreationResume = {
  @schema
  type t = Editing | Submitted
}
module CreationStatus = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("Editing") Editing({})
    | @as("Failed") Failed({message: string})
    | @as("Pending") Pending({submission: CreationSubmission.t})
    | @as("Interrupted") Interrupted({submission: CreationSubmission.t, message: string})
    | @as("Reconciling") Reconciling({submission: CreationSubmission.t, next: CreationReconcile.t})
    | @as("Succeeded") Succeeded({})
  @@warning("+27")
}
module ReviewCreation = {
  @schema
  type t = {
    @as("review_id") reviewId: reviewId,
    @as("workspace_id") workspaceId: workspaceId,
    context: @s.null option<DaemonContext.t>,
    draft: CreationDraft.t,
    defaults: array<CreationDefault.t>,
    selected: @s.null option<CreationTargetId.t>,
    revision: CreationRevision.t,
    status: CreationStatus.t,
  }
  let editable = (creation: t) =>
    switch creation.status {
    | Editing(_) | Failed(_) => true
    | Pending(_) | Interrupted(_) | Reconciling(_) | Succeeded(_) => false
    }
}
module HomeView = {
  @schema
  type t = {
    rows: array<HomeRow.t>,
    @as("selected_workspace") selectedWorkspace: @s.null option<workspaceId>,
    expanded: array<workspaceId>,
    creating: @s.null option<ReviewCreation.t>,
  }
  let empty: t = {rows: [], selectedWorkspace: None, expanded: [], creating: None}
}

module ViewModel = {
  @@warning("-27")
  @schema
  type t = {
    home: HomeView.t,
    @as("daemon_context") daemonContext: @s.null option<DaemonContext.t>,
    @as("active_repo") activeRepo: @s.null option<repoId>,
    @as("copy_checkout") copyCheckout: @s.null option<string>,
    prefs: ViewPrefs.t,
    tree: TreeView.t,
    progress: Progress.t,
    diff: @s.null option<DiffView.t>,
    diffs: array<DiffView.t>,
    threads: array<ThreadView.t>,
    conversation: array<ThreadView.t>,
    requests: array<Domain.ReviewRequest.t>,
    checkpoints: array<Domain.ReviewerCheckpoint.t>,
    @as("check_current_ready") checkCurrentReady: bool,
    stepper: @s.null option<CommitStepper.t>,
    focus: Focus.t,
    tab: Tab.t,
    scroll: @s.null option<ScrollIntent.t>,
    mode: Mode.t,
    hints: array<Hint.t>,
    @as("pending_keys") pendingKeys: string,
    @as("pending_label") pendingLabel: @s.null option<string>,
    leader: string,
    chrome: array<Hint.t>,
    /// Every binding that applies where the focus is, aliases included.
    bindings: array<Hint.t>,
    /// What the core made of the last key it acted on. A shell that must
    /// act on a key before the core answers compares `seq` against what
    /// it has sent, and reads `command` rather than guessing which one
    /// the core will have run.
    @as("last_key") lastKey: @s.null option<LastKey.t>,
    help: @s.null option<HelpView.t>,
    /// What `y` would copy from where the focus is; the shell copies it
    /// during the gesture that asks for it.
    @as("copy_target") copyTarget: @s.null option<string>,
    @as("copy_reference") copyReference: @s.null option<string>,
    @as("focused_comment") focusedComment: @s.null option<string>,
    connection: ConnectionView.t,
    @as("last_error") lastError: @s.null option<Rpc.RpcError.t>,
    workspaces: array<Domain.Workspace.t>,
    reviews: array<Domain.Review.t>,
    @as("open_review") openReview: @s.null option<reviewId>,
    @as("resolved_targets") resolvedTargets: array<Domain.ResolvedTarget.t>,
    scope: Domain.DiffScope.t,
    browse: @s.null option<BrowseView.t>,
    @as("content_search") contentSearch: @s.null option<ContentSearchView.t>,
    @as("action_palette") actionPalette: bool,
    @as("ref_selector") refSelector: @s.null option<RefSelectorView.t>,
    visual: @s.null option<VisualView.t>,
    review: @s.null option<OpenReview.t>,
    draft: @s.null option<Draft.t>,
    @as("pending_refresh") pendingRefresh: bool,
  }

  /// The model before any patch arrives.
  let empty: t = {
    home: HomeView.empty,
    daemonContext: None,
    activeRepo: None,
    copyCheckout: None,
    prefs: {
      layout: Unified,
      ignoreWhitespace: false,
      contextLines: 3,
      sidebarHidden: false,
    },
    tree: {roots: [], breadcrumbs: [], search: None},
    progress: {viewed: 0, changedSinceViewed: 0, total: 0, additions: 0, deletions: 0},
    diff: None,
    diffs: [],
    threads: [],
    conversation: [],
    requests: [],
    checkpoints: [],
    checkCurrentReady: false,
    stepper: None,
    focus: ReviewList({index: 0}),
    tab: FilesChanged,
    scroll: None,
    mode: Normal,
    hints: [],
    pendingKeys: "",
    pendingLabel: None,
    leader: "space",
    chrome: [],
    bindings: [],
    lastKey: None,
    help: None,
    copyTarget: None,
    copyReference: None,
    focusedComment: None,
    connection: Disconnected({}),
    lastError: None,
    workspaces: [],
    reviews: [],
    openReview: None,
    resolvedTargets: [],
    scope: All({}),
    browse: None,
    contentSearch: None,
    actionPalette: false,
    refSelector: None,
    visual: None,
    review: None,
    draft: None,
    pendingRefresh: false,
  }
  @@warning("+27")
}

module ViewPatch = {
  // One section of the model, as the host pushes it (client-core `patch.rs`).
  @schema @tag("type")
  type t =
    | @as("Connection")
    Connection({
        connection: ConnectionView.t,
        @as("last_error") lastError: @s.null option<Rpc.RpcError.t>,
      })
    | @as("ReviewList")
    ReviewList({
        home: HomeView.t,
        @as("daemon_context") daemonContext: @s.null option<DaemonContext.t>,
        @as("active_repo") activeRepo: @s.null option<repoId>,
        workspaces: array<Domain.Workspace.t>,
        reviews: array<Domain.Review.t>,
        @as("open_review") openReview: @s.null option<reviewId>,
        @as("resolved_targets") resolvedTargets: array<Domain.ResolvedTarget.t>,
        scope: Domain.DiffScope.t,
        browse: @s.null option<BrowseView.t>,
      })
    | @as("Tree") Tree({tree: TreeView.t})
    | @as("Diff")
    Diff({
        diff: @s.null option<DiffView.t>,
        diffs: array<DiffView.t>,
        prefs: ViewPrefs.t,
        visual: @s.null option<VisualView.t>,
      })
    | @as("Threads") Threads({threads: array<ThreadView.t>})
    | @as("Conversation")
    Conversation({
        conversation: array<ThreadView.t>,
        requests: array<Domain.ReviewRequest.t>,
        checkpoints: array<Domain.ReviewerCheckpoint.t>,
        @as("check_current_ready") checkCurrentReady: bool,
      })
    | @as("CommitStepper") CommitStepper({stepper: @s.null option<CommitStepper.t>})
    | @as("RefSelector")
    RefSelector({
        @as("ref_selector") refSelector: @s.null option<RefSelectorView.t>,
      })
    | @as("Progress") Progress({progress: Progress.t})
    | @as("Focus")
    Focus({
        focus: Focus.t,
        tab: Tab.t,
        scroll: @s.null option<ScrollIntent.t>,
        @as("copy_target") copyTarget: @s.null option<string>,
        @as("copy_checkout") copyCheckout: @s.null option<string>,
        @as("copy_reference") copyReference: @s.null option<string>,
        @as("focused_comment") focusedComment: @s.null option<string>,
      })
    | @as("Hints")
    Hints({
        hints: array<Hint.t>,
        pending: string,
        @as("pending_label") pendingLabel: @s.null option<string>,
        mode: Mode.t,
        leader: string,
        chrome: array<Hint.t>,
        bindings: array<Hint.t>,
        @as("last_key") lastKey: @s.null option<LastKey.t>,
      })
    | @as("Help") Help({help: @s.null option<HelpView.t>})
    | @as("Draft")
    Draft({
        draft: @s.null option<Draft.t>,
        @as("pending_refresh") pendingRefresh: bool,
      })
    | @as("Search")
    Search({
        @as("content_search") contentSearch: @s.null option<ContentSearchView.t>,
        @as("action_palette") actionPalette: bool,
      })

  /// Install a patch into the UI's copy of the model.
  let apply = (model: ViewModel.t, patch: t): ViewModel.t =>
    switch patch {
    | Connection({connection, lastError}) => {...model, connection, lastError}
    | ReviewList({
        home,
        daemonContext,
        activeRepo,
        workspaces,
        reviews,
        openReview,
        resolvedTargets,
        scope,
        browse,
      }) => {
        ...model,
        home,
        daemonContext,
        activeRepo,
        workspaces,
        reviews,
        openReview,
        resolvedTargets,
        scope,
        browse,
      }
    | Tree({tree}) => {...model, tree}
    | Diff({diff, diffs, prefs, visual}) => {...model, diff, diffs, prefs, visual}
    | Threads({threads}) => {...model, threads}
    | Conversation({conversation, requests, checkpoints, checkCurrentReady}) => {
        ...model,
        conversation,
        requests,
        checkpoints,
        checkCurrentReady,
      }
    | CommitStepper({stepper}) => {...model, stepper}
    | RefSelector({refSelector}) => {...model, refSelector}
    | Progress({progress}) => {...model, progress}
    | Focus({focus, tab, scroll, copyTarget, copyCheckout, copyReference, focusedComment}) => {
        ...model,
        focus,
        tab,
        scroll,
        copyTarget,
        copyCheckout,
        copyReference,
        focusedComment,
      }
    | Hints({hints, pending, pendingLabel, mode, leader, chrome, bindings, lastKey}) => {
        ...model,
        hints,
        pendingKeys: pending,
        pendingLabel,
        mode,
        leader,
        chrome,
        bindings,
        lastKey,
      }
    | Help({help}) => {...model, help}
    | Draft({draft, pendingRefresh}) => {...model, draft, pendingRefresh}
    | Search({contentSearch, actionPalette}) => {...model, contentSearch, actionPalette}
    }
}
