// Tab-local transport recovery. The stable attempt crosses a fresh browser
// host through Core actions; no draft is stored in shared KV or localStorage.
type snapshot = {creation: View.ReviewCreation.t, resume: View.CreationResume.t}
type t = {mutable snapshot: option<snapshot>, mutable edits: array<View.CreationDraft.t>}
let make = () => {snapshot: None, edits: []}

let observe = (state: t, creation: option<View.ReviewCreation.t>) => {
  switch creation {
  | None =>
    state.snapshot = None
    state.edits = []
  | Some(creation) =>
    switch state.snapshot {
    | Some(previous) if previous.creation.reviewId != creation.reviewId => state.edits = []
    | Some(_) | None => ()
    }
    switch creation.status {
    | Succeeded(_) =>
      state.snapshot = None
      state.edits = []
    | Editing(_) =>
      switch state.snapshot {
      | Some(previous)
        if previous.creation.reviewId == creation.reviewId && previous.resume == Submitted => ()
      | Some(_) | None =>
        let acknowledged = state.edits->Array.findIndex(d => d == creation.draft)
        if acknowledged >= 0 {
          state.edits = state.edits->Array.filterWithIndex((_, i) => i > acknowledged)
        }
        let draft = state.edits[Array.length(state.edits) - 1]->Option.getOr(creation.draft)
        state.snapshot = Some({creation: {...creation, draft}, resume: Editing})
      }
    | Failed(_) =>
      state.edits = []
      state.snapshot = Some({creation, resume: Editing})
    | Pending(_) | Interrupted(_) | Reconciling(_) =>
      state.edits = []
      state.snapshot = Some({creation, resume: Submitted})
    }
  }
}

// Return whether this is an intent that must never be queued across hosts.
let beforeAction = (state: t, action: Action.t): bool => {
  let submit = () =>
    switch state.snapshot {
    | Some(previous) if View.ReviewCreation.editable(previous.creation) =>
      state.snapshot = Some({...previous, resume: Submitted})
    | Some(_) | None => ()
    }
  switch action {
  | UpdateCreationDraft({reviewId, draft}) =>
    switch state.snapshot {
    | Some(previous)
      if previous.creation.reviewId == reviewId &&
      previous.resume == Editing &&
      View.ReviewCreation.editable(previous.creation) =>
      state.edits = state.edits->Array.concat([draft])
      state.snapshot = Some({
        creation: {...previous.creation, draft, status: Editing({})},
        resume: Editing,
      })
    | Some(_) | None => ()
    }
    true
  | SubmitReviewCreation(_) =>
    submit()
    true
  | RunCommand({command: Submit}) =>
    submit()
    state.snapshot != None
  | RetryReviewCreation(_)
  | RestoreReviewCreation(_)
  | StartReview(_)
  | CancelNewReview(_)
  | AddCreationTarget(_)
  | RemoveCreationTarget(_)
  | SelectCreationTarget(_) => true
  | RunCommand({command: AddReviewTarget | RemoveReviewTarget}) => true
  | _ => false
  }
}
