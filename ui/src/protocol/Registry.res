// Every schema by its Rust type name, for the boundary test: a fixture
// directory under `fixtures/protocol/<Type>/` must have an entry here, and
// each fixture must parse and re-serialise to the same JSON.

external erase: S.t<'a> => S.t<unknown> = "%identity"

let schemas: dict<S.t<unknown>> = Dict.fromArray([
  ("ProtocolVersion", erase(Ids.protocolVersionSchema)),
  ("SchemaVersion", erase(Ids.schemaVersionSchema)),
  ("Repo", erase(Domain.Repo.schema)),
  ("Workspace", erase(Domain.Workspace.schema)),
  ("Human", erase(Domain.Human.schema)),
  ("AgentVia", erase(Domain.AgentVia.schema)),
  ("Author", erase(Domain.Author.schema)),
  ("RefSpec", erase(Domain.RefSpec.schema)),
  ("BaseRefSpec", erase(Domain.BaseRefSpec.schema)),
  ("TargetRevision", erase(Domain.TargetRevision.schema)),
  ("ReviewTargetUpdate", erase(Domain.ReviewTargetUpdate.schema)),
  ("RefCandidate", erase(Domain.RefCandidate.schema)),
  ("ResolvedSource", erase(Domain.ResolvedSource.schema)),
  ("ResolvedRef", erase(Domain.ResolvedRef.schema)),
  ("ReviewTarget", erase(Domain.ReviewTarget.schema)),
  ("ResolvedTarget", erase(Domain.ResolvedTarget.schema)),
  ("ReviewStatus", erase(Domain.ReviewStatus.schema)),
  ("Review", erase(Domain.Review.schema)),
  ("Sig", erase(Domain.Sig.schema)),
  ("CommitInfo", erase(Domain.CommitInfo.schema)),
  ("Side", erase(Domain.Side.schema)),
  ("Anchor", erase(Domain.Anchor.schema)),
  ("CommentContext", erase(Domain.CommentContext.schema)),
  ("CommentKind", erase(Domain.CommentKind.schema)),
  ("CommentIntent", erase(Domain.CommentIntent.schema)),
  ("CommentState", erase(Domain.CommentState.schema)),
  ("Comment", erase(Domain.Comment.schema)),
  ("ThreadResolution", erase(Domain.ThreadResolution.schema)),
  ("Thread", erase(Domain.Thread.schema)),
  ("BlobMode", erase(Domain.BlobMode.schema)),
  ("BlobEntry", erase(Domain.BlobEntry.schema)),
  ("ViewedContent", erase(Domain.ViewedContent.schema)),
  ("SubmoduleChange", erase(Domain.SubmoduleChange.schema)),
  ("ViewedMark", erase(Domain.ViewedMark.schema)),
  ("RenderOpts", erase(Domain.RenderOpts.schema)),
  ("GapExpansion", erase(Domain.GapExpansion.schema)),
  ("ChangeKind", erase(Domain.ChangeKind.schema)),
  ("DiffScope", erase(Domain.DiffScope.schema)),
  ("ContentHit", erase(Domain.ContentHit.schema)),
  ("FileChange", erase(Domain.FileChange.schema)),
  ("TreeEntryKind", erase(Domain.TreeEntryKind.schema)),
  ("TreeEntry", erase(Domain.TreeEntry.schema)),
  ("TreeSnapshot", erase(Domain.TreeSnapshot.schema)),
  ("TreeDelta", erase(Domain.TreeDelta.schema)),
  ("RequestedTargets", erase(Domain.RequestedTargets.schema)),
  ("ReviewerIdentity", erase(Domain.ReviewerIdentity.schema)),
  ("ReviewRound", erase(Domain.ReviewRound.schema)),
  ("ReviewCheckpoint", erase(Domain.ReviewCheckpoint.schema)),
  ("CheckpointFreshness", erase(Domain.CheckpointFreshness.schema)),
  ("ReviewerCheckpoint", erase(Domain.ReviewerCheckpoint.schema)),
  ("ReviewCheckpointId", erase(Ids.reviewCheckpointIdSchema)),
  ("ReviewRequestId", erase(Ids.reviewRequestIdSchema)),
  ("ReviewRequest", erase(Domain.ReviewRequest.schema)),
  ("ReviewSnapshot", erase(Domain.ReviewSnapshot.schema)),
  ("SpanClass", erase(Render.SpanClass.schema)),
  ("Span", erase(Render.Span.schema)),
  ("Cell", erase(Render.Cell.schema)),
  ("ExpandDir", erase(Render.ExpandDir.schema)),
  ("Row", erase(Render.Row.schema)),
  ("RenderTarget", erase(Render.RenderTarget.schema)),
  ("RenderContent", erase(Render.RenderContent.schema)),
  ("GapRow", erase(Render.GapRow.schema)),
  ("FileRenderHeader", erase(Render.FileRenderHeader.schema)),
  ("RenderChunk", erase(Render.RenderChunk.schema)),
  ("FileRender", erase(Render.FileRender.schema)),
  ("FileSummary", erase(Render.FileSummary.schema)),
  ("DiffSummary", erase(Render.DiffSummary.schema)),
  ("EventBody", erase(Events.EventBody.schema)),
  ("Event", erase(Events.Event.schema)),
  ("BuildInfo", erase(Rpc.BuildInfo.schema)),
  ("UpgradeNotice", erase(Rpc.UpgradeNotice.schema)),
  ("EntityKind", erase(Rpc.EntityKind.schema)),
  ("RpcError", erase(Rpc.RpcError.schema)),
  ("Since", erase(Rpc.Since.schema)),
  ("ReplayCursor", erase(Rpc.ReplayCursor.schema)),
  ("ReplayPosition", erase(Rpc.ReplayPosition.schema)),
  ("ReplayProgress", erase(Rpc.ReplayProgress.schema)),
  ("ReplayPage", erase(Rpc.ReplayPage.schema)),
  ("SubscribeScope", erase(Rpc.SubscribeScope.schema)),
  ("ViewSection", erase(Rpc.ViewSection.schema)),
  ("Mutation", erase(Rpc.Mutation.schema)),
  ("Request", erase(Rpc.Request.schema)),
  ("EnsureDirectoryReview", erase(Rpc.EnsureDirectoryReview.schema)),
  ("DirectoryReview", erase(Rpc.DirectoryReview.schema)),
  ("DirectoryReviewOutcome", erase(Rpc.DirectoryReviewOutcome.schema)),
  ("Response", erase(Rpc.Response.schema)),
  ("StreamItem", erase(Rpc.StreamItem.schema)),
  ("ClientMsg", erase(Rpc.ClientMsg.schema)),
  ("ServerMsg", erase(Rpc.ServerMsg.schema)),
  ("EnvelopeClientMsg", erase(Rpc.Envelope.schema(Rpc.ClientMsg.schema))),
  ("EnvelopeServerMsg", erase(Rpc.Envelope.schema(Rpc.ServerMsg.schema))),
])

let names: array<string> = Dict.keysToArray(schemas)

/// Parse `json` as `typeName` and serialise it back. `Error` carries the
/// schema error message (or "no schema").
let roundtrip = (typeName: string, json: JSON.t): result<JSON.t, string> =>
  switch Dict.get(schemas, typeName) {
  | None => Error("no schema for " ++ typeName)
  | Some(schema) =>
    try {
      let value = S.parseJsonOrThrow(json, schema)
      Ok(S.reverseConvertToJsonOrThrow(value, schema))
    } catch {
    | exn =>
      Error(JsExn.fromException(exn)->Option.flatMap(JsExn.message)->Option.getOr("unknown error"))
    }
  }
