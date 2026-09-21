// Client (view/action) schemas by Rust type name, for the boundary test
// over `fixtures/client/`. Same contract as `Registry` for the protocol.

external erase: S.t<'a> => S.t<unknown> = "%identity"

let schemas: dict<S.t<unknown>> = Dict.fromArray([
  ("CreationBase", erase(View.CreationBase.schema)),
  ("CreationTarget", erase(View.CreationTarget.schema)),
  ("CreationDraft", erase(View.CreationDraft.schema)),
  ("CreationDefaultState", erase(View.CreationDefaultState.schema)),
  ("CreationDefault", erase(View.CreationDefault.schema)),
  ("CreationSubmission", erase(View.CreationSubmission.schema)),
  ("CreationReconcile", erase(View.CreationReconcile.schema)),
  ("CreationResume", erase(View.CreationResume.schema)),
  ("CreationStatus", erase(View.CreationStatus.schema)),
  ("ReviewCreation", erase(View.ReviewCreation.schema)),
  ("HomeView", erase(View.HomeView.schema)),
  ("HomeRow", erase(View.HomeRow.schema)),
  ("HomeRowKind", erase(View.HomeRowKind.schema)),
  ("DaemonContext", erase(View.DaemonContext.schema)),
  ("ViewModel", erase(View.ViewModel.schema)),
  ("ViewPrefs", erase(View.ViewPrefs.schema)),
  ("Layout", erase(View.Layout.schema)),
  ("Tab", erase(View.Tab.schema)),
  ("Mode", erase(View.Mode.schema)),
  ("VisualView", erase(View.VisualView.schema)),
  ("ScrollIntent", erase(View.ScrollIntent.schema)),
  ("ScrollAlign", erase(View.ScrollAlign.schema)),
  ("Landing", erase(View.Landing.schema)),
  ("ConnectionView", erase(View.ConnectionView.schema)),
  ("Draft", erase(View.Draft.schema)),
  ("DraftPurpose", erase(View.DraftPurpose.schema)),
  ("PendingEvent", erase(View.PendingEvent.schema)),
  ("OpenReview", erase(View.OpenReview.schema)),
  ("OpenFile", erase(View.OpenFile.schema)),
  ("RenderKey", erase(View.RenderKey.schema)),
  ("FileRef", erase(View.FileRef.schema)),
  ("TreeView", erase(View.TreeView.schema)),
  ("TreeNode", erase(View.TreeNode.schema)),
  ("SearchView", erase(View.SearchView.schema)),
  ("SearchHit", erase(View.SearchHit.schema)),
  ("Progress", erase(View.Progress.schema)),
  ("ViewedState", erase(View.ViewedState.schema)),
  ("ChangeKindKind", erase(View.ChangeKindKind.schema)),
  ("DiffView", erase(View.DiffView.schema)),
  ("DiffRow", erase(View.DiffRow.schema)),
  ("RowThread", erase(View.RowThread.schema)),
  ("RowPlace", erase(View.RowPlace.schema)),
  ("CommentView", erase(View.CommentView.schema)),
  ("ThreadView", erase(View.ThreadView.schema)),
  ("ThreadPlace", erase(View.ThreadPlace.schema)),
  ("CommitStepper", erase(View.CommitStepper.schema)),
  ("StepperCommit", erase(View.StepperCommit.schema)),
  ("Focus", erase(View.Focus.schema)),
  ("Hint", erase(View.Hint.schema)),
  ("HelpView", erase(View.HelpView.schema)),
  ("HelpGroup", erase(View.HelpGroup.schema)),
  ("HelpEntry", erase(View.HelpEntry.schema)),
  ("Conflict", erase(View.Conflict.schema)),
  ("Context", erase(View.Context.schema)),
  ("Command", erase(View.Command.schema)),
  ("Action", erase(Action.schema)),
  ("ScopeChoice", erase(Action.ScopeChoice.schema)),
  ("ContentSearchView", erase(View.ContentSearchView.schema)),
  ("RefSelectorSide", erase(View.RefSelectorSide.schema)),
  ("RefSelectorStatus", erase(View.RefSelectorStatus.schema)),
  ("RefOption", erase(View.RefOption.schema)),
  ("RefSelectorView", erase(View.RefSelectorView.schema)),
  ("Override", erase(View.Override.schema)),
  ("Overrides", erase(View.Overrides.schema)),
  ("ViewDelta", erase(View.ViewDelta.schema)),
  ("ViewPatch", erase(View.ViewPatch.schema)),
  ("KeyChord", erase(Keys.KeyChord.schema)),
  ("KeyCode", erase(Keys.KeyCode.schema)),
  ("NamedKey", erase(Keys.NamedKey.schema)),
  ("Modifiers", erase(Keys.Modifiers.schema)),
])

let names: array<string> = Dict.keysToArray(schemas)

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
