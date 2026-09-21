// `nits_client_core::Action`: what the UI dispatches to the host.

open Ids

module ScopeChoice = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("All") All({})
    | @as("Committed") Committed({})
    | @as("ByCommit") ByCommit({})
    | @as("Commit") Commit({@as("repo_id") repoId: repoId, oid: commitOid})
    | @as("Worktree") Worktree({@as("repo_id") repoId: repoId})
    | @as("Requested") Requested({@as("request_id") requestId: reviewRequestId})
    | @as("SinceCheckpoint")
    SinceCheckpoint({
        @as("checkpoint_id") checkpointId: reviewCheckpointId,
      })
  @@warning("+27")
}

module SearchKind = {
  @schema
  type t = Files | Content
}

@@warning("-27")
@schema @tag("type")
type t =
  | @as("Connect") Connect({})
  | @as("Disconnect") Disconnect({})
  | @as("ListWorkspaces") ListWorkspaces({})
  | @as("ToggleWorkspace") ToggleWorkspace({@as("workspace_id") workspaceId: workspaceId})
  | @as("SelectWorkspace") SelectWorkspace({@as("workspace_id") workspaceId: workspaceId})
  | @as("StartReview") StartReview({@as("workspace_id") workspaceId: workspaceId})
  | @as("CancelNewReview") CancelNewReview({})
  | @as("EditCreationDraft")
  EditCreationDraft({
      @as("review_id") reviewId: reviewId,
      edit: View.CreationEdit.t,
    })
  | @as("SubmitReviewCreation") SubmitReviewCreation({@as("review_id") reviewId: reviewId})
  | @as("RetryReviewCreation") RetryReviewCreation({@as("review_id") reviewId: reviewId})
  | @as("RestoreReviewCreation")
  RestoreReviewCreation({
      creation: View.ReviewCreation.t,
      resume: View.CreationResume.t,
    })
  | @as("SelectCreationTarget")
  SelectCreationTarget({
      @as("review_id") reviewId: reviewId,
      @as("target_id") targetId: View.CreationTargetId.t,
    })
  | @as("AddCreationTarget") AddCreationTarget({@as("review_id") reviewId: reviewId})
  | @as("RemoveCreationTarget") RemoveCreationTarget({@as("review_id") reviewId: reviewId})
  | @as("CopyCheckout") CopyCheckout({@as("repo_id") repoId: repoId})
  | @as("GoHome") GoHome({})
  | @as("ListReviews") ListReviews({@as("workspace_id") workspaceId: workspaceId})
  | @as("CreateReview")
  CreateReview({
      @as("workspace_id") workspaceId: workspaceId,
      title: string,
      targets: array<Domain.ReviewTarget.t>,
    })
  | @as("OpenReview") OpenReview({@as("review_id") reviewId: reviewId})
  | @as("CloseReview") CloseReview({})
  | @as("CheckCurrent") CheckCurrent({})
  | @as("CheckRequested") CheckRequested({})
  | @as("CheckpointDelta") CheckpointDelta({})
  | @as("InformationalNoteOpened") InformationalNoteOpened({})
  | @as("DraftOpened") DraftOpened({anchor: Domain.Anchor.t})
  | @as("DraftSubmitted") DraftSubmitted({body: string})
  | @as("DraftDiscarded") DraftDiscarded({})
  | @as("ReplyOpened") ReplyOpened({@as("thread_id") threadId: threadId})
  | @as("SetFocus") SetFocus({focus: View.Focus.t})
  | @as("ToggleHelp") ToggleHelp({})
  | @as("Reply") Reply({@as("thread_id") threadId: threadId, body: string})
  | @as("EditComment") EditComment({@as("comment_id") commentId: commentId, body: string})
  | @as("DeleteComment") DeleteComment({@as("comment_id") commentId: commentId})
  | @as("DeferOpened") DeferOpened({@as("thread_id") threadId: threadId})
  | @as("DeferThread")
  DeferThread({
      @as("thread_id") threadId: threadId,
      reason: string,
      @as("tracking_url") trackingUrl: @s.null option<string>,
    })
  | @as("ResolveThread") ResolveThread({@as("thread_id") threadId: threadId})
  | @as("UnresolveThread") UnresolveThread({@as("thread_id") threadId: threadId})
  | @as("ApplySuggestion") ApplySuggestion({@as("comment_id") commentId: commentId})
  | PreviewSuggestion({@as("comment_id") commentId: commentId})
  | @as("Viewport")
  Viewport({
      file: View.FileRef.t,
      @as("first_row") firstRow: int,
      @as("last_row") lastRow: int,
    })
  | @as("OpenFileAt")
  OpenFileAt({
      file: View.FileRef.t,
      row: int,
      side: Domain.Side.t,
      landing: View.Landing.t,
    })
  | @as("CloseFile") CloseFile({})
  | @as("ToggleDir") ToggleDir({@as("repo_id") repoId: repoId, path: @s.null option<string>})
  | @as("FileSearch") FileSearch({query: @s.null option<string>})
  | @as("SetLayout") SetLayout({layout: View.Layout.t})
  | @as("CommentLines")
  CommentLines({
      file: View.FileRef.t,
      side: Domain.Side.t,
      @as("start_line") startLine: int,
      @as("end_line") endLine: int,
    })
  | @as("CommentFile") CommentFile({file: View.FileRef.t})
  | @as("SetTab") SetTab({tab: View.Tab.t})
  | @as("ToggleSidebar") ToggleSidebar({})
  | @as("SetReferenceContext") SetReferenceContext({context: string})
  | @as("OpenReference") OpenReference({reference: string})
  | @as("CopyReference") CopyReference({reference: string})
  | @as("FocusComment") FocusComment({@as("comment_id") commentId: string})
  | @as("CopyPath") CopyPath({path: string})
  | @as("ScrollView") ScrollView({align: View.ScrollAlign.t})
  | @as("ToggleFileCollapse") ToggleFileCollapse({file: View.FileRef.t})
  | @as("CollapseParent") CollapseParent({})
  | @as("CollapseAll") CollapseAll({})
  | @as("SetRenderOpts")
  SetRenderOpts({
      @as("ignore_whitespace") ignoreWhitespace: bool,
      @as("context_lines") contextLines: int,
    })
  | @as("MarkViewed") MarkViewed({file: View.FileRef.t})
  | @as("UnmarkViewed") UnmarkViewed({file: View.FileRef.t})
  | @as("ListCommits") ListCommits({@as("repo_id") repoId: repoId})
  | @as("StepCommit") StepCommit({selected: @s.null option<int>})
  | @as("SetScope") SetScope({scope: ScopeChoice.t})
  | @as("OpenOriginalDiff") OpenOriginalDiff({@as("thread_id") threadId: threadId})
  | @as("ExpandContext") ExpandContext({file: View.FileRef.t, full: bool})
  | @as("ExpandGap") ExpandGap({file: View.FileRef.t, gap: int, dir: Render.ExpandDir.t})
  | @as("SelectBrowseRepo") SelectBrowseRepo({@as("repo_id") repoId: repoId})
  | @as("OpenBrowseRefSelector") OpenBrowseRefSelector({@as("repo_id") repoId: repoId})
  | @as("ResetBrowse") ResetBrowse({})
  | @as("SetBrowseRef")
  SetBrowseRef({
      @as("repo_id") repoId: repoId,
      @as("ref_spec") refSpec: @s.null option<Domain.RefSpec.t>,
    })
  | @as("ContentSearch")
  ContentSearch({
      query: @s.null option<string>,
      @as("all_files") allFiles: bool,
    })
  | @as("ActionPalette") ActionPalette({@as("open") open_: bool})
  | @as("RunCommand") RunCommand({command: View.Command.t})
  | @as("EnterVisual") EnterVisual({})
  | @as("LeaveVisual") LeaveVisual({})
  | @as("SearchFirst") SearchFirst({search: SearchKind.t})
  | @as("SearchStep") SearchStep({search: SearchKind.t, delta: int})
  | @as("OpenSearchResult") OpenSearchResult({search: SearchKind.t, query: string})
  | @as("OpenRefSelector")
  OpenRefSelector({
      @as("repo_id") repoId: repoId,
      side: View.RefSelectorSide.t,
    })
  | @as("RefSelectorQuery") RefSelectorQuery({query: string})
  | @as("RefSelectorStep") RefSelectorStep({delta: int})
  | @as("SelectRef") SelectRef({index: int})
  | @as("SelectCurrentRef") SelectCurrentRef({})
  | @as("CloseRefSelector") CloseRefSelector({})
@@warning("+27")
