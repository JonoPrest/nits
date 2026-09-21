// One ordered local draft model is shared by the editor and tab-local recovery.
// Stable row IDs let field edits survive structural changes before their ACK.
type snapshot = {creation: View.ReviewCreation.t, resume: View.CreationResume.t}
type operation =
  | Field(View.CreationEdit.t)
  | Select(View.CreationTargetId.t)
  | Add(option<View.CreationTarget.t>)
  | Remove(option<View.CreationTargetId.t>)
type pending = {revision: View.CreationRevision.t, operation: operation}
type t = {mutable snapshot: option<snapshot>, mutable edits: array<pending>}
let make = () => {snapshot: None, edits: []}

let apply = (creation: View.ReviewCreation.t, pending: pending) => {
  let creation = {...creation, revision: pending.revision}
  let draft = creation.draft
  let field = (id, change) => {
    ...creation,
    status: Editing({}),
    draft: {
      ...draft,
      targets: draft.targets->Array.map(t => t.id == id ? change(t) : t),
    },
  }
  switch pending.operation {
  | Field(Title({text})) => {...creation, status: Editing({}), draft: {...draft, title: text}}
  | Field(Repository({targetId, repoId})) =>
    field(targetId, t => {...t, repoId, base: Automatic({})})
  | Field(Base({targetId, text})) => field(targetId, t => {...t, base: Manual({text: text})})
  | Field(Head({targetId, text})) => field(targetId, t => {...t, head: text})
  | Select(id) =>
    draft.targets->Array.some(t => t.id == id) ? {...creation, selected: Some(id)} : creation
  | Add(Some(target)) => {
      ...creation,
      status: Editing({}),
      selected: Some(target.id),
      draft: {...draft, targets: draft.targets->Array.concat([target])},
    }
  | Add(None) | Remove(None) => creation
  | Remove(Some(id)) =>
    let index = draft.targets->Array.findIndex(t => t.id == id)
    if index < 0 {
      creation
    } else {
      let targets = draft.targets->Array.filter(t => t.id != id)
      let selected =
        targets->Array.get(Math.Int.min(index, Array.length(targets) - 1))->Option.map(t => t.id)
      {...creation, status: Editing({}), selected, draft: {...draft, targets}}
    }
  }
}

let observe = (state: t, creation: option<View.ReviewCreation.t>) => {
  switch creation {
  | None =>
    state.snapshot = None
    state.edits = []
  | Some(creation)
    if state.snapshot
    ->Option.map(previous =>
      previous.creation.reviewId == creation.reviewId &&
      previous.resume == Submitted &&
      creation.revision < previous.creation.revision
    )
    ->Option.getOr(false) => ()
  | Some(creation) =>
    switch state.snapshot {
    | Some(previous) if previous.creation.reviewId != creation.reviewId => state.edits = []
    | Some(_) | None => ()
    }
    switch creation.status {
    | Succeeded(_) =>
      state.snapshot = None
      state.edits = []
    | Editing(_) | Failed(_) =>
      switch state.snapshot {
      | Some(previous)
        if previous.creation.reviewId == creation.reviewId &&
        previous.resume == Submitted &&
        creation.status == View.CreationStatus.Editing({}) => ()
      | Some(_) | None =>
        state.edits = state.edits->Array.filter(edit => edit.revision > creation.revision)
        let creation = state.edits->Array.reduce(creation, apply)
        state.snapshot = Some({creation, resume: Editing})
      }
    | Pending(_) | Interrupted(_) | Reconciling(_) =>
      state.edits = []
      state.snapshot = Some({creation, resume: Submitted})
    }
  }
}

// Capture before sending. No creation writes are queued across browser hosts.
let beforeAction = (
  state: t,
  action: Action.t,
  ~workspaces: array<Domain.Workspace.t>=[],
): bool => {
  let current = state.snapshot
  let edit = (reviewId, operation) =>
    switch current {
    | Some(previous)
      if previous.creation.reviewId == reviewId &&
      previous.resume == Editing &&
      View.ReviewCreation.editable(previous.creation) =>
      let pending = {revision: previous.creation.revision + 1, operation}
      state.edits = state.edits->Array.concat([pending])
      state.snapshot = Some({creation: apply(previous.creation, pending), resume: Editing})
    | Some(_) | None => ()
    }
  let add = reviewId => {
    let target = current->Option.flatMap(saved =>
      workspaces
      ->Array.find(w => w.id == saved.creation.workspaceId)
      ->Option.flatMap(w => {
        let repo =
          w.repos->Array.find(
            r => !(saved.creation.draft.targets->Array.some(t => t.repoId == r.id)),
          )
        repo->Option.map(
          r => {
            View.CreationTarget.id: saved.creation.revision + 1,
            repoId: r.id,
            base: Automatic({}),
            head: "worktree",
          },
        )
      })
    )
    edit(reviewId, Add(target))
  }
  let remove = reviewId => edit(reviewId, Remove(current->Option.flatMap(s => s.creation.selected)))
  let submit = reviewId =>
    switch current {
    | Some(previous)
      if previous.creation.reviewId == reviewId &&
      previous.resume == Editing &&
      View.ReviewCreation.editable(previous.creation) =>
      state.snapshot = Some({
        creation: {...previous.creation, revision: previous.creation.revision + 1},
        resume: Submitted,
      })
    | Some(_) | None => ()
    }
  let retry = reviewId =>
    switch current {
    | Some(previous) if previous.creation.reviewId == reviewId =>
      switch previous.creation.status {
      | Interrupted({submission}) =>
        state.snapshot = Some({
          creation: {
            ...previous.creation,
            revision: previous.creation.revision + 1,
            status: Reconciling({submission, next: Retry}),
          },
          resume: Submitted,
        })
      | Editing(_) | Failed(_) | Pending(_) | Reconciling(_) | Succeeded(_) => ()
      }
    | Some(_) | None => ()
    }
  switch action {
  | EditCreationDraft({reviewId, edit: value}) =>
    edit(reviewId, Field(value))
    true
  | SelectCreationTarget({reviewId, targetId}) =>
    edit(reviewId, Select(targetId))
    true
  | AddCreationTarget({reviewId}) =>
    add(reviewId)
    true
  | RemoveCreationTarget({reviewId}) =>
    remove(reviewId)
    true
  | RunCommand({command: AddReviewTarget}) =>
    current->Option.forEach(saved => add(saved.creation.reviewId))
    true
  | RunCommand({command: RemoveReviewTarget}) =>
    current->Option.forEach(saved => remove(saved.creation.reviewId))
    true
  | SubmitReviewCreation({reviewId}) =>
    submit(reviewId)
    true
  | RunCommand({command: Submit}) =>
    current->Option.forEach(saved => {
      switch saved.creation.status {
      | Interrupted(_) => retry(saved.creation.reviewId)
      | Editing(_) | Failed(_) | Pending(_) | Reconciling(_) | Succeeded(_) =>
        submit(saved.creation.reviewId)
      }
    })
    state.snapshot != None
  | RetryReviewCreation({reviewId}) =>
    retry(reviewId)
    true
  | RestoreReviewCreation(_) | StartReview(_) | CancelNewReview(_) => true
  | _ => false
  }
}
