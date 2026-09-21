open Vitest
open TestingLibrary

afterEach(cleanup)
let view = (creation, dispatch) =>
  <NewReview
    creation
    workspaces=[CreationFixtures.workspace()]
    chrome=CreationFixtures.hints
    bindings=CreationFixtures.hints
    dispatch
  />
let lastDraft = dispatch => {
  let state = CreationRecovery.make()
  CreationRecovery.observe(state, Some(CreationFixtures.make()))
  mock(dispatch).calls->Array.forEach(args => {
    let _ = CreationRecovery.beforeAction(
      state,
      args->Array.getUnsafe(0),
      ~workspaces=[CreationFixtures.workspace()],
    )
  })
  (state.snapshot->Option.getExn).creation.draft
}

test("failed creation keeps title and refs visible until confirmed success", () => {
  let dispatch = fn()
  let creation = CreationFixtures.make()
  let {rerender} = render(view(creation, dispatch))
  expect(Element.value(Screen.getByLabelText("Base revision")))->toBe("develop")
  FireEvent.change(Screen.getByPlaceholderText("Title"), {"target": {"value": "Keep this title"}})
  FireEvent.change(Screen.getByLabelText("Base revision"), {"target": {"value": "missing-ref"}})
  FireEvent.click(Screen.getByText("Create"))
  let draft = lastDraft(dispatch)
  rerender(
    view(
      {...creation, draft, status: Failed({message: "revision missing-ref does not exist"})},
      dispatch,
    ),
  )
  expect(Element.value(Screen.getByPlaceholderText("Title")))->toBe("Keep this title")
  expect(Element.value(Screen.getByLabelText("Base revision")))->toBe("missing-ref")
  let _ = Screen.getByText("revision missing-ref does not exist")
  expect(Element.hasAttribute(Screen.getByText("Create"), "disabled"))->toBe(false)
})

test("delayed defaults and old draft echoes cannot overwrite a newer edit", () => {
  let dispatch = fn()
  let original = CreationFixtures.make()
  let creation = {
    ...original,
    defaults: original.defaults->Array.map(d => {...d, state: Loading({})}),
  }
  let {rerender} = render(view(creation, dispatch))
  FireEvent.change(Screen.getByLabelText("Base revision"), {"target": {"value": "typed"}})
  let earlier = lastDraft(dispatch)
  FireEvent.change(Screen.getByLabelText("Base revision"), {"target": {"value": "typed-later"}})
  let latest = lastDraft(dispatch)
  rerender(view({...original, draft: {...creation.draft, title: creation.draft.title}}, dispatch))
  expect(Element.value(Screen.getByLabelText("Base revision")))->toBe("typed-later")
  rerender(view({...original, draft: earlier, revision: 1}, dispatch))
  expect(Element.value(Screen.getByLabelText("Base revision")))->toBe("typed-later")
  rerender(view({...original, draft: latest, revision: 2}, dispatch))
  expect(Element.value(Screen.getByLabelText("Base revision")))->toBe("typed-later")
})

test("pending and reconciliation disable duplicate submission and editing", () => {
  let creation = CreationFixtures.make()
  let submission: View.CreationSubmission.t = {title: "pending", targets: []}
  let dispatch = fn()
  let {rerender} = render(view({...creation, status: Pending({submission: submission})}, dispatch))
  expect(Element.hasAttribute(Screen.getByText("Create"), "disabled"))->toBe(true)
  expect(Element.hasAttribute(Screen.getByPlaceholderText("Title"), "disabled"))->toBe(true)
  FireEvent.click(Screen.getByText("Create"))
  expect(dispatch)->not_->toHaveBeenCalled
  rerender(view({...creation, status: Reconciling({submission, next: Inspect})}, dispatch))
  let _ = Screen.getByText("Checking the previous creation attempt…")
  expect(Element.hasAttribute(Screen.getByText("Cancel"), "disabled"))->toBe(true)
  rerender(
    view(
      {...creation, status: Interrupted({submission, message: "check the prior attempt"})},
      dispatch,
    ),
  )
  FireEvent.click(Screen.getByText("Check and retry"))
  expect(dispatch)->toHaveBeenCalledWith(Action.RunCommand({command: Submit}))
})

test("configured form keys and tooltips follow the same command mapping", () => {
  let dispatch = fn()
  let _ = render(view(CreationFixtures.make(), dispatch))
  let title = Screen.getByPlaceholderText("Title")
  ["u", "a", "r", "b"]->Array.forEach(key =>
    FireEvent.keyDown(title, {"key": key, "ctrlKey": true})
  )
  [View.Command.Submit, AddReviewTarget, RemoveReviewTarget, Back]->Array.forEach(command =>
    expect(dispatch)->toHaveBeenCalledWith(Action.RunCommand({command: command}))
  )
  expect(Element.getAttribute(Screen.getByText("+ target"), "title")->Nullable.toOption)->toBe(
    Some("add target (ctrl+a)"),
  )
})

test(
  "a sent attempt is retained before any host acknowledgement and cleared only on success",
  () => {
    let state = CreationRecovery.make()
    let creation = CreationFixtures.make()
    CreationRecovery.observe(state, Some(creation))
    let draft = {...creation.draft, title: "latest unsaved input"}
    let _ = CreationRecovery.beforeAction(
      state,
      EditCreationDraft({reviewId: creation.reviewId, edit: Title({text: draft.title})}),
    )
    let _ = CreationRecovery.beforeAction(state, RunCommand({command: Submit}))
    CreationRecovery.observe(
      state,
      Some({...creation, draft: {...creation.draft, title: creation.draft.title}}),
    )
    let saved = state.snapshot->Option.getExn
    expect(saved.resume)->toBe(View.CreationResume.Submitted)
    expect(saved.creation.draft.title)->toBe("latest unsaved input")
    CreationRecovery.observe(state, Some({...creation, status: Succeeded({})}))
    expect(state.snapshot)->toBe(None)
  },
)

test("a new workspace draft cannot inherit another workspace's unacknowledged edits", () => {
  let state = CreationRecovery.make()
  let creation = CreationFixtures.make()
  CreationRecovery.observe(state, Some(creation))
  let _ = CreationRecovery.beforeAction(
    state,
    EditCreationDraft({
      reviewId: creation.reviewId,
      edit: Title({text: "old workspace"}),
    }),
  )
  let other = CreationFixtures.make(~id="01ARZ3NDEKTSV4RRFFQ69G5FAB")
  CreationRecovery.observe(state, Some(other))
  expect((state.snapshot->Option.getExn).creation.draft.title)->toBe("")
})

test("every daemon RPC error has visible shell text", () => {
  Fixtures.variants("protocol", "RpcError")->Array.forEach(variant => {
    let error = Fixtures.parse(Rpc.RpcError.schema, "protocol", "RpcError", variant)
    expect(String.length(RpcErrorText.message(error)) > 0)->toBe(true)
  })
})

test("disconnected creation offers a configured reconnect action without discarding input", () => {
  let dispatch = fn()
  let creation = {
    ...CreationFixtures.make(),
    draft: {title: "Retained through reconnect", targets: []},
  }
  let _ = render(
    <NewReview
      creation
      workspaces=[CreationFixtures.workspace()]
      chrome=CreationFixtures.hints
      bindings=CreationFixtures.hints
      connection={Disconnected({})}
      dispatch
    />,
  )
  expect(Element.getAttribute(Screen.getByText("Reconnect"), "title")->Nullable.toOption)->toBe(
    Some("reconnect review creation (alt+r)"),
  )
  FireEvent.click(Screen.getByText("Reconnect"))
  expect(dispatch)->toHaveBeenCalledWith(Action.RunCommand({command: ReconnectReviewCreation}))
  expect(Element.value(Screen.getByPlaceholderText("Title")))->toBe("Retained through reconnect")
})

test("add and remove remain ordered with typing before their model patches arrive", () => {
  let original = CreationFixtures.make()
  let dispatch = fn()
  let {container, rerender} = render(view(original, dispatch))
  let bases = () => Element.querySelectorAll(container, "[aria-label='Base revision']")
  FireEvent.click(Screen.getByText("+ target"))
  expect(Array.length(bases()))->toBe(2)
  FireEvent.change(Screen.getByPlaceholderText("Title"), {"target": {"value": "after add"}})
  FireEvent.change(bases()->Array.getUnsafe(1), {"target": {"value": "second-base"}})
  // The delayed pre-add model must not undo the new row or subsequent edits.
  rerender(view({...original, defaults: []}, dispatch))
  expect(Array.length(bases()))->toBe(2)
  expect(Element.value(Screen.getByPlaceholderText("Title")))->toBe("after add")
  expect(Element.value(bases()->Array.getUnsafe(1)))->toBe("second-base")
  FireEvent.click(
    Element.querySelectorAll(container, "[aria-label='Remove target']")->Array.getUnsafe(0),
  )
  FireEvent.change(Screen.getByPlaceholderText("Title"), {"target": {"value": "after remove"}})
  expect(Array.length(bases()))->toBe(1)
  expect(Element.value(bases()->Array.getUnsafe(0)))->toBe("second-base")
  let state = CreationRecovery.make()
  CreationRecovery.observe(state, Some(original))
  let actions = mock(dispatch).calls
  actions->Array.forEach(args => {
    let _ = CreationRecovery.beforeAction(
      state,
      args->Array.getUnsafe(0),
      ~workspaces=[CreationFixtures.workspace()],
    )
  })
  let acknowledged = (state.snapshot->Option.getExn).creation
  rerender(view(acknowledged, dispatch))
  expect(Array.length(bases()))->toBe(1)
  expect(Element.value(Screen.getByPlaceholderText("Title")))->toBe("after remove")
  expect(Element.value(bases()->Array.getUnsafe(0)))->toBe("second-base")
  expect(
    actions->Array.some(args =>
      switch args->Array.getUnsafe(0) {
      | Action.EditCreationDraft({edit: Base({targetId})}) => targetId != 0
      | _ => false
      }
    ),
  )->toBe(true)
})

test("revision ACKs preserve ABA edits and the unacknowledged structural recovery snapshot", () => {
  let state = CreationRecovery.make()
  let original = CreationFixtures.make()
  CreationRecovery.observe(state, Some(original))
  let send = action => {
    let _ = CreationRecovery.beforeAction(state, action, ~workspaces=[CreationFixtures.workspace()])
  }
  send(RunCommand({command: AddReviewTarget}))
  let added = (state.snapshot->Option.getExn).creation
  ["first", "second", "first"]->Array.forEach(text =>
    send(EditCreationDraft({reviewId: original.reviewId, edit: Title({text: text})}))
  )
  CreationRecovery.observe(state, Some(added))
  let retained = (state.snapshot->Option.getExn).creation
  expect(retained.draft.title)->toBe("first")
  expect(Array.length(retained.draft.targets))->toBe(2)
  expect(Array.length(state.edits))->toBe(3)
  send(SelectCreationTarget({reviewId: original.reviewId, targetId: 0}))
  send(RunCommand({command: RemoveReviewTarget}))
  send(RunCommand({command: Submit}))
  CreationRecovery.observe(state, Some(original))
  let frozen = state.snapshot->Option.getExn
  expect(frozen.resume)->toBe(View.CreationResume.Submitted)
  expect(frozen.creation.draft.title)->toBe("first")
  expect(Array.length(frozen.creation.draft.targets))->toBe(1)
  expect((frozen.creation.draft.targets->Array.getUnsafe(0)).id)->not_->toBe(0)
})

test("a delayed failure patch cannot discard its later correction before reconnect", () => {
  let state = CreationRecovery.make()
  let failed = {...CreationFixtures.make(), status: Failed({message: "Unknown base"})}
  CreationRecovery.observe(state, Some(failed))
  let _ = CreationRecovery.beforeAction(
    state,
    EditCreationDraft({
      reviewId: failed.reviewId,
      edit: Base({targetId: 0, text: "corrected-base"}),
    }),
  )
  CreationRecovery.observe(state, Some(failed))
  let corrected = (state.snapshot->Option.getExn).creation
  expect((corrected.draft.targets->Array.getUnsafe(0)).base)->toEqual(
    View.CreationBase.Manual({text: "corrected-base"}),
  )
  expect(corrected.status)->toEqual(View.CreationStatus.Editing({}))
  let _ = CreationRecovery.beforeAction(state, RunCommand({command: Submit}))
  CreationRecovery.observe(state, Some(failed))
  expect((state.snapshot->Option.getExn).resume)->toBe(View.CreationResume.Submitted)
  expect((state.snapshot->Option.getExn).creation)->toEqual(corrected)
})

test("submit waits for displayed automatic defaults but accepts an immediate manual base", () => {
  let original = CreationFixtures.make()
  let loading = {...original, defaults: []}
  let dispatch = fn()
  let {rerender} = render(view(loading, dispatch))
  expect(Element.hasAttribute(Screen.getByText("Create"), "disabled"))->toBe(true)
  FireEvent.keyDown(Screen.getByPlaceholderText("Title"), {"key": "u", "ctrlKey": true})
  FireEvent.keyDown(Screen.getByText("+ target"), {"key": "u", "ctrlKey": true})
  expect(dispatch)->not_->toHaveBeenCalled
  rerender(view(original, dispatch))
  expect(Element.hasAttribute(Screen.getByText("Create"), "disabled"))->toBe(false)
  rerender(view(loading, dispatch))
  FireEvent.change(Screen.getByLabelText("Base revision"), {"target": {"value": "manual-base"}})
  expect(Element.hasAttribute(Screen.getByText("Create"), "disabled"))->toBe(false)
  FireEvent.keyDown(Screen.getByPlaceholderText("Title"), {"key": "u", "ctrlKey": true})
  expect(dispatch)->toHaveBeenCalledWith(Action.RunCommand({command: Submit}))
})
