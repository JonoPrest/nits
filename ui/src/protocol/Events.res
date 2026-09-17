// Events (nits-protocol `events.rs`).

open Ids
open Domain

module EventBody = {
  @schema @tag("type")
  type t =
    | @as("WorkspaceCreated") WorkspaceCreated({workspace: Workspace.t})
    | @as("WorkspaceUpdated")
    WorkspaceUpdated({
        @as("workspace_id") workspaceId: workspaceId,
        name: string,
      })
    | @as("RepoAttached") RepoAttached({@as("workspace_id") workspaceId: workspaceId, repo: Repo.t})
    | @as("RepoDetached")
    RepoDetached({
        @as("workspace_id") workspaceId: workspaceId,
        @as("repo_id") repoId: repoId,
      })
    | @as("ReviewCreated") ReviewCreated({review: Review.t})
    | @as("ReviewUpdated")
    ReviewUpdated({
        @as("review_id") reviewId: reviewId,
        title: string,
        status: ReviewStatus.t,
      })
    | @as("ReviewTargetUpdated")
    ReviewTargetUpdated({
        @as("review_id") reviewId: reviewId,
        target: ReviewTarget.t,
      })
    | @as("ReviewDeleted") ReviewDeleted({@as("review_id") reviewId: reviewId})
    | @as("ReviewTargetsResolved")
    ReviewTargetsResolved({
        @as("review_id") reviewId: reviewId,
        targets: array<ResolvedTarget.t>,
      })
    | @as("CommentCreated") CommentCreated({comment: Comment.t})
    | @as("CommentEdited")
    CommentEdited({
        @as("review_id") reviewId: reviewId,
        @as("comment_id") commentId: commentId,
        body: string,
      })
    | @as("CommentDeleted")
    CommentDeleted({
        @as("review_id") reviewId: reviewId,
        @as("comment_id") commentId: commentId,
      })
    | @as("CommentReanchored")
    CommentReanchored({
        @as("review_id") reviewId: reviewId,
        @as("comment_id") commentId: commentId,
        anchor: Anchor.t,
        state: CommentState.t,
      })
    | @as("ThreadDeferred")
    ThreadDeferred({
        @as("review_id") reviewId: reviewId,
        @as("thread_id") threadId: threadId,
        reason: string,
        @as("tracking_url") trackingUrl: @s.null option<string>,
      })
    | @as("ThreadResolved")
    ThreadResolved({
        @as("review_id") reviewId: reviewId,
        @as("thread_id") threadId: threadId,
      })
    | @as("ThreadUnresolved")
    ThreadUnresolved({
        @as("review_id") reviewId: reviewId,
        @as("thread_id") threadId: threadId,
      })
    | @as("FileViewed")
    FileViewed({
        @as("review_id") reviewId: reviewId,
        @as("repo_id") repoId: repoId,
        path: string,
        viewer: Human.t,
        @as("blob_oid") blobOid: @s.null option<blobOid>,
      })
    | @as("FileUnviewed")
    FileUnviewed({
        @as("review_id") reviewId: reviewId,
        @as("repo_id") repoId: repoId,
        path: string,
        viewer: Human.t,
      })
    | @as("ReviewRequested")
    ReviewRequested({
        @as("review_id") reviewId: reviewId,
        agent: string,
        note: string,
      })
    | @as("SuggestionApplied")
    SuggestionApplied({
        @as("review_id") reviewId: reviewId,
        @as("comment_id") commentId: commentId,
        @as("repo_id") repoId: repoId,
        path: string,
        @as("result_blob") resultBlob: blobOid,
      })
}

module Event = {
  @schema
  type t = {
    seq: seq,
    ts: timestamp,
    author: Author.t,
    @as("client_id") clientId: clientId,
    @as("client_seq") clientSeq: clientSeq,
    body: EventBody.t,
  }
}
