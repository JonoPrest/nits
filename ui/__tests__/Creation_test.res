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
let lastDraft = dispatch =>
  mock(dispatch).calls
  ->Array.filterMap(args =>
    switch args->Array.getUnsafe(0) {
    | Action.UpdateCreationDraft({draft}) => Some(draft)
    | _ => None
    }
  )
  ->Array.at(-1)
  ->Option.getExn

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
  rerender(view({...original, draft: earlier}, dispatch))
  expect(Element.value(Screen.getByLabelText("Base revision")))->toBe("typed-later")
  rerender(view({...original, draft: latest}, dispatch))
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
      UpdateCreationDraft({reviewId: creation.reviewId, draft}),
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
    UpdateCreationDraft({
      reviewId: creation.reviewId,
      draft: {...creation.draft, title: "old workspace"},
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
