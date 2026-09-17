// RPC frames (nits-protocol `rpc.rs`).

open Ids
open Domain

module BuildInfo = {
  @schema
  type t = {name: string, version: string}
}

module UpgradeNotice = {
  @schema
  type t = {latest: protocolVersion, message: string}
}

module EntityKind = {
  @schema
  type t = Workspace | Repo | Review | Comment | Thread | Ref | Path | Blob | Chunk
}

module RpcError = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("NotFound") NotFound({kind: EntityKind.t, id: string})
    | @as("Invalid") Invalid({reason: string})
    | @as("Forbidden") Forbidden({reason: string})
    | @as("SeqTooOld") SeqTooOld({oldest: seq})
    | @as("Cancelled") Cancelled({})
    | @as("UnsupportedProtocol")
    UnsupportedProtocol({
        requested: protocolVersion,
        supported: array<protocolVersion>,
      })
    | @as("VersionMismatch")
    VersionMismatch({
        negotiated: protocolVersion,
        received: protocolVersion,
      })
    | @as("Internal") Internal({message: string})
  @@warning("+27")
}

module Since = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("Now") Now({})
    | @as("After") After({seq: seq})
  @@warning("+27")
}

module SubscribeScope = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("All") All({})
    | @as("Workspace") Workspace({@as("workspace_id") workspaceId: workspaceId})
    | @as("Review") Review({@as("review_id") reviewId: reviewId})
    | @as("AwaitingAgent") AwaitingAgent({agent: string})
  @@warning("+27")
}

module ViewSection = {
  @schema
  type t =
    | Connection
    | Search
    | ReviewList
    | Tree
    | Diff
    | Threads
    | Conversation
    | CommitStepper
    | RefSelector
    | Progress
    | Focus
    | Hints
    | Help
    | Draft
}

module Mutation = {
  @schema @tag("type")
  type t =
    | @as("CreateWorkspace")
    CreateWorkspace({
        @as("workspace_id") workspaceId: workspaceId,
        name: string,
      })
    | @as("RenameWorkspace")
    RenameWorkspace({
        @as("workspace_id") workspaceId: workspaceId,
        name: string,
      })
    | @as("AttachRepo")
    AttachRepo({
        @as("workspace_id") workspaceId: workspaceId,
        @as("repo_id") repoId: repoId,
        path: string,
        @as("display_name") displayName: string,
      })
    | @as("DetachRepo")
    DetachRepo({
        @as("workspace_id") workspaceId: workspaceId,
        @as("repo_id") repoId: repoId,
      })
    | @as("CreateReview")
    CreateReview({
        @as("review_id") reviewId: reviewId,
        @as("workspace_id") workspaceId: workspaceId,
        title: string,
        targets: array<ReviewTarget.t>,
      })
    | @as("UpdateReview")
    UpdateReview({
        @as("review_id") reviewId: reviewId,
        title: string,
        status: ReviewStatus.t,
      })
    | @as("UpdateReviewTarget")
    UpdateReviewTarget({
        @as("review_id") reviewId: reviewId,
        update: ReviewTargetUpdate.t,
      })
    | @as("DeleteReview") DeleteReview({@as("review_id") reviewId: reviewId})
    | @as("AddComment")
    AddComment({
        @as("review_id") reviewId: reviewId,
        @as("comment_id") commentId: commentId,
        kind: CommentKind.t,
        anchor: Anchor.t,
        body: string,
        context: @s.null option<Domain.CommentContext.t>,
      })
    | @as("Reply")
    Reply({
        @as("review_id") reviewId: reviewId,
        @as("thread_id") threadId: threadId,
        @as("comment_id") commentId: commentId,
        kind: CommentKind.t,
        body: string,
      })
    | @as("EditComment")
    EditComment({
        @as("review_id") reviewId: reviewId,
        @as("comment_id") commentId: commentId,
        body: string,
      })
    | @as("DeleteComment")
    DeleteComment({
        @as("review_id") reviewId: reviewId,
        @as("comment_id") commentId: commentId,
      })
    | @as("DeferThread")
    DeferThread({
        @as("review_id") reviewId: reviewId,
        @as("thread_id") threadId: threadId,
        reason: string,
        @as("tracking_url") trackingUrl: @s.null option<string>,
      })
    | @as("ResolveThread")
    ResolveThread({
        @as("review_id") reviewId: reviewId,
        @as("thread_id") threadId: threadId,
      })
    | @as("UnresolveThread")
    UnresolveThread({
        @as("review_id") reviewId: reviewId,
        @as("thread_id") threadId: threadId,
      })
    | @as("MarkViewed")
    MarkViewed({
        @as("review_id") reviewId: reviewId,
        @as("repo_id") repoId: repoId,
        path: string,
      })
    | @as("UnmarkViewed")
    UnmarkViewed({
        @as("review_id") reviewId: reviewId,
        @as("repo_id") repoId: repoId,
        path: string,
      })
    | @as("RequestReview")
    RequestReview({
        @as("review_id") reviewId: reviewId,
        agent: string,
        note: string,
      })
    | @as("RecordCheckpoint")
    RecordCheckpoint({
        @as("review_id") reviewId: reviewId,
        targets: array<ResolvedTarget.t>,
        @as("in_reply_to") inReplyTo: @s.null option<ReviewRound.t>,
      })
    | @as("ApplySuggestion")
    ApplySuggestion({
        @as("review_id") reviewId: reviewId,
        @as("comment_id") commentId: commentId,
      })
}

module EnsureDirectoryReview = {
  @schema
  type t = {
    @as("workspace_id") workspaceId: workspaceId,
    @as("repo_id") repoId: repoId,
    @as("review_id") reviewId: reviewId,
    path: string,
    base: @s.null option<BaseRefSpec.t>,
    head: @s.null option<RefSpec.t>,
  }
}

module DirectoryReviewOutcome = {
  @schema
  type t = Created | Reused
}

module DirectoryReview = {
  @schema
  type t = {
    @as("workspace_id") workspaceId: workspaceId,
    @as("repo_id") repoId: repoId,
    @as("review_id") reviewId: reviewId,
    base: RefSpec.t,
    head: RefSpec.t,
    outcome: DirectoryReviewOutcome.t,
    seq: seq,
  }
}

module Request = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("EnsureDirectoryReview")
    EnsureDirectoryReview({
        @as("client_seq") clientSeq: clientSeq,
        options: EnsureDirectoryReview.t,
      })
    | @as("ListWorkspaces") ListWorkspaces({})
    | @as("ListReviews") ListReviews({@as("workspace_id") workspaceId: workspaceId})
    | @as("ListRefs") ListRefs({@as("repo_id") repoId: repoId})
    | @as("DefaultBase") DefaultBase({@as("repo_id") repoId: repoId})
    | @as("GetReview") GetReview({@as("review_id") reviewId: reviewId})
    | @as("ReviewSnapshot") ReviewSnapshot({@as("review_id") reviewId: reviewId})
    | @as("ListFiles") ListFiles({@as("review_id") reviewId: reviewId, scope: DiffScope.t})
    | @as("Search")
    Search({
        @as("review_id") reviewId: reviewId,
        query: string,
        @as("all_files") allFiles: bool,
        scope: DiffScope.t,
      })
    | @as("OpenReview") OpenReview({@as("review_id") reviewId: reviewId, opts: RenderOpts.t})
    | @as("ResolveTargets") ResolveTargets({@as("review_id") reviewId: reviewId})
    | @as("ListCommits")
    ListCommits({
        @as("review_id") reviewId: reviewId,
        @as("repo_id") repoId: repoId,
      })
    | @as("TreeSnapshot") TreeSnapshot({@as("repo_id") repoId: repoId, @as("ref") ref_: RefSpec.t})
    | @as("FileRender")
    FileRender({
        @as("review_id") reviewId: reviewId,
        @as("repo_id") repoId: repoId,
        path: string,
        opts: RenderOpts.t,
        @as("first_chunk") firstChunk: Render.chunkIndex,
        scope: DiffScope.t,
      })
    | @as("ChangeRender")
    ChangeRender({
        @as("repo_id") repoId: repoId,
        path: string,
        change: Domain.ChangeKind.t,
        opts: RenderOpts.t,
        @as("first_chunk") firstChunk: Render.chunkIndex,
      })
    | @as("BlobRender")
    BlobRender({
        @as("repo_id") repoId: repoId,
        path: string,
        @as("blob_oid") blobOid: blobOid,
        @as("first_chunk") firstChunk: Render.chunkIndex,
      })
    | @as("RenderChunk")
    RenderChunk({
        @as("repo_id") repoId: repoId,
        path: string,
        target: Render.RenderTarget.t,
        opts: RenderOpts.t,
        index: Render.chunkIndex,
      })
    | @as("Subscribe") Subscribe({scope: SubscribeScope.t, since: Since.t})
    | @as("Unsubscribe") Unsubscribe({scope: SubscribeScope.t})
    | @as("Mutate") Mutate({@as("client_seq") clientSeq: clientSeq, mutation: Mutation.t})
    | @as("Shutdown") Shutdown({})
  @@warning("+27")
}

module Response = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("DirectoryReview") DirectoryReview({review: DirectoryReview.t})
    | @as("Workspaces") Workspaces({workspaces: array<Workspace.t>})
    | @as("Reviews") Reviews({reviews: array<Review.t>})
    | @as("DefaultBase") DefaultBase({base: RefSpec.t})
    | @as("Review") Review({review: Review.t})
    | @as("ReviewSnapshot") ReviewSnapshot({snapshot: ReviewSnapshot.t})
    | @as("Files") Files({files: array<FileChange.t>, resolved: array<ResolvedTarget.t>})
    | @as("Search") Search({hits: array<Domain.ContentHit.t>, truncated: bool})
    | @as("Resolved") Resolved({targets: array<ResolvedTarget.t>, changed: bool})
    | @as("Commits") Commits({commits: array<CommitInfo.t>})
    | @as("Refs") Refs({@as("repo_id") repoId: repoId, refs: array<RefCandidate.t>})
    | @as("TreeSnapshot") TreeSnapshot({snapshot: TreeSnapshot.t})
    | @as("RenderChunk") RenderChunk({chunk: Render.RenderChunk.t})
    | @as("Subscribed") Subscribed({seq: seq})
    | @as("Unsubscribed") Unsubscribed({})
    | @as("Committed") Committed({event: Events.Event.t})
    | @as("ShuttingDown") ShuttingDown({})
  @@warning("+27")
}

module StreamItem = {
  @schema @tag("type")
  type t =
    | @as("ReviewSnapshot") ReviewSnapshot({snapshot: ReviewSnapshot.t})
    | @as("TreeSnapshot") TreeSnapshot({snapshot: TreeSnapshot.t})
    | @as("Header") Header({header: Render.FileRenderHeader.t})
    | @as("Chunk") Chunk({@as("repo_id") repoId: repoId, path: string, chunk: Render.RenderChunk.t})
}

module ClientMsg = {
  @schema @tag("type")
  type t =
    | @as("Hello")
    Hello({
        @as("client_id") clientId: clientId,
        protocol: protocolVersion,
        client: BuildInfo.t,
        author: Author.t,
      })
    | @as("Request") Request({id: requestId, request: Request.t})
    | @as("Cancel") Cancel({id: requestId})
}

module ServerMsg = {
  @schema @tag("type")
  type t =
    | @as("Welcome")
    Welcome({
        protocol: protocolVersion,
        daemon: BuildInfo.t,
        schema: schemaVersion,
        upgrade: @s.null option<UpgradeNotice.t>,
      })
    | @as("Rejected") Rejected({error: RpcError.t})
    | @as("Response") Response({id: requestId, response: Response.t})
    | @as("StreamItem") StreamItem({id: requestId, item: StreamItem.t})
    | @as("StreamEnd") StreamEnd({id: requestId})
    | @as("Error") ServerError({id: requestId, error: RpcError.t})
    | @as("Event") Event({event: Events.Event.t})
    | @as("TreeDelta") TreeDelta({delta: TreeDelta.t})
}

module Envelope = {
  /// A frame on the wire: `{ "v": protocol version, "msg": ... }`. Generic,
  /// so hand-written (the ppx derives monomorphic schemas only).
  type t<'msg> = {v: protocolVersion, msg: 'msg}
  let schema = (msg: S.t<'msg>): S.t<t<'msg>> =>
    S.object(s => {
      v: s.field("v", protocolVersionSchema),
      msg: s.field("msg", msg),
    })
}
