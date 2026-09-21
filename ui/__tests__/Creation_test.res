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
let lastCreation = dispatch => {
  let state = CreationRecovery.make()
  CreationRecovery.observe(state, Some(CreationFixtures.make()))
  mock(dispatch).calls->Array.forEach(args => {
    let _ = CreationRecovery.beforeAction(
      state,
      args->Array.getUnsafe(0),
      ~workspaces=[CreationFixtures.workspace()],
    )
  })
  (state.snapshot->Option.getExn).creation
}
let lastDraft = dispatch => lastCreation(dispatch).draft

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
      {
        ...creation,
        draft,
        revision: lastCreation(dispatch).revision,
        status: Failed({message: "revision missing-ref does not exist"}),
      },
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
    CreationRecovery.observe(
      state,
      Some({...creation, revision: saved.creation.revision, status: Succeeded({})}),
    )
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
  expect((state.snapshot->Option.getExn).creation)->toEqual({
    ...corrected,
    revision: corrected.revision + 1,
  })
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

test(
  "unchanged retry stays frozen through an old failure until this submit is acknowledged",
  () => {
    let failed = {...CreationFixtures.make(), revision: 7, status: Failed({message: "Missing ref"})}
    let dispatch = fn()
    let {rerender} = render(view(failed, dispatch))
    FireEvent.click(Screen.getByText("Create"))
    rerender(view({...failed, defaults: failed.defaults->Array.map(d => d)}, dispatch))
    expect(Element.hasAttribute(Screen.getByText("Create"), "disabled"))->toBe(true)
    expect(Element.hasAttribute(Screen.getByPlaceholderText("Title"), "disabled"))->toBe(true)
    expect(Element.hasAttribute(Screen.getByLabelText("Base revision"), "disabled"))->toBe(true)
    FireEvent.keyDown(Screen.getByText("+ target"), {"key": "u", "ctrlKey": true})
    expect(Array.length(mock(dispatch).calls))->toBe(1)
    // A failure acknowledging this actual submit releases the unchanged draft.
    rerender(
      view({...failed, revision: 8, status: Failed({message: "Still unavailable"})}, dispatch),
    )
    expect(Element.hasAttribute(Screen.getByText("Create"), "disabled"))->toBe(false)
    expect(Element.hasAttribute(Screen.getByPlaceholderText("Title"), "disabled"))->toBe(false)
    let _ = Screen.getByText("Still unavailable")
  },
)

test("interrupted retry has a new lifecycle revision and ignores an old interruption", () => {
  let submission: View.CreationSubmission.t = {title: "retained", targets: []}
  let interrupted = {
    ...CreationFixtures.make(),
    revision: 4,
    status: Interrupted({submission, message: "Reconnect to check"}),
  }
  let state = CreationRecovery.make()
  CreationRecovery.observe(state, Some(interrupted))
  let _ = CreationRecovery.beforeAction(state, RunCommand({command: Submit}))
  let retried = state.snapshot->Option.getExn
  expect(retried.creation.revision)->toBe(5)
  expect(retried.creation.status)->toEqual(
    View.CreationStatus.Reconciling({submission, next: Retry}),
  )
  CreationRecovery.observe(state, Some(interrupted))
  expect(state.snapshot)->toEqual(Some(retried))
  let _ = CreationRecovery.beforeAction(state, RunCommand({command: Submit}))
  expect(state.snapshot)->toEqual(Some(retried))
  CreationRecovery.observe(state, Some({...interrupted, revision: 5}))
  expect((state.snapshot->Option.getExn).creation.status)->toEqual(interrupted.status)
})

test("unique target choices follow local add/remove operations before acknowledgements", () => {
  let dispatch = fn()
  let creation = CreationFixtures.make()
  let workspace = CreationFixtures.workspace()
  let {container, rerender} = render(view(creation, dispatch))
  let selectors = () => Element.querySelectorAll(container, "select[aria-label='repo']")
  let options = element => {
    let nodes = Element.querySelectorAll(element, "option")
    let values = []
    nodes->Array.forEach(node => values->Array.push(Element.value(node)))
    values
  }
  expect(options(selectors()->Array.getUnsafe(0)))->toEqual(workspace.repos->Array.map(r => r.id))
  FireEvent.click(Screen.getByText("+ target"))
  expect(Array.length(selectors()))->toBe(2)
  expect(options(selectors()->Array.getUnsafe(0)))->toEqual([
    (workspace.repos->Array.getUnsafe(0)).id,
  ])
  expect(options(selectors()->Array.getUnsafe(1)))->toEqual([
    (workspace.repos->Array.getUnsafe(1)).id,
  ])
  expect(Element.hasAttribute(Screen.getByText("+ target"), "disabled"))->toBe(true)
  let _ = Screen.getByText(
    "All workspace repositories are included. Remove a target to choose it again.",
  )
  FireEvent.click(Screen.getByText("+ target"))
  FireEvent.keyDown(Screen.getByPlaceholderText("Title"), {"key": "a", "ctrlKey": true})
  FireEvent.change(
    Screen.getByPlaceholderText("Title"),
    {"target": {"value": "typed after exhaustion"}},
  )
  expect(Array.length(selectors()))->toBe(2)
  let before = lastCreation(dispatch)
  expect(Array.length(before.draft.targets))->toBe(2)
  let removed = before.draft.targets->Array.getUnsafe(1)
  FireEvent.click(
    Element.querySelectorAll(container, "[aria-label='Remove target']")->Array.getUnsafe(1),
  )
  expect(Element.hasAttribute(Screen.getByText("+ target"), "disabled"))->toBe(false)
  expect(Array.length(options(selectors()->Array.getUnsafe(0))))->toBe(2)
  FireEvent.click(Screen.getByText("+ target"))
  let current = lastCreation(dispatch)
  let added = current.draft.targets->Array.getUnsafe(1)
  expect(added.repoId)->toBe(removed.repoId)
  expect(added.id == removed.id)->toBe(false)
  rerender(view({...creation, revision: before.revision, draft: before.draft}, dispatch))
  expect(Array.length(selectors()))->toBe(2)
  expect(Element.value(Screen.getByPlaceholderText("Title")))->toBe("typed after exhaustion")
  expect(Element.hasAttribute(Screen.getByText("+ target"), "disabled"))->toBe(true)
})

test("one repo is exhausted immediately and restored duplicates remain correctable", () => {
  let workspace = CreationFixtures.workspace()
  let first = workspace.repos->Array.getUnsafe(0)
  let single = {...workspace, repos: [first]}
  let dispatch = fn()
  let creation = CreationFixtures.make(~workspace=single)
  let {container, rerender} = render(
    <NewReview
      creation
      workspaces=[single]
      chrome=CreationFixtures.hints
      bindings=CreationFixtures.hints
      dispatch
    />,
  )
  expect(Element.hasAttribute(Screen.getByText("+ target"), "disabled"))->toBe(true)
  let duplicate = {...creation.draft.targets->Array.getUnsafe(0), id: 9, head: "other-head"}
  let retained = {
    ...creation,
    draft: {
      title: "retain duplicate inputs",
      targets: creation.draft.targets->Array.concat([duplicate]),
    },
    status: Failed({message: "Choose one base/head pair for each repository."}),
  }
  rerender(view(retained, dispatch))
  let rows = Element.querySelectorAll(container, "select[aria-label='repo']")
  expect(Array.length(rows))->toBe(2)
  rows->Array.forEach(row => {
    expect(Element.value(row))->toBe(first.id)
    expect(Array.length(Element.querySelectorAll(row, "option")))->toBe(2)
  })
  expect(Element.value(Screen.getByPlaceholderText("Title")))->toBe("retain duplicate inputs")
  let _ = Screen.getByText("Choose one base/head pair for each repository.")
  let other = (workspace.repos->Array.getUnsafe(1)).id
  FireEvent.change(rows->Array.getUnsafe(1), {"target": {"value": other}})
  expect(dispatch)->toHaveBeenCalledWith(
    Action.EditCreationDraft({
      reviewId: creation.reviewId,
      edit: Repository({targetId: duplicate.id, repoId: other}),
    }),
  )
})

@send external blur: element => unit = "blur"

[false, true]->Array.forEach(movedElsewhere => {
  test(
    "failed submission restores its editor only when native focus was lost: " ++
    Bool.toString(movedElsewhere),
    () => {
      let dispatch = fn()
      let creation = CreationFixtures.make()
      let element = creation =>
        <div>
          {view(creation, dispatch)}
          <button> {React.string("elsewhere")} </button>
        </div>
      let {rerender} = render(element(creation))
      let input = Screen.getByLabelText("Base revision")
      Element.focus(input)
      FireEvent.keyDown(input, {"key": "u", "ctrlKey": true})
      expect(Element.hasAttribute(input, "disabled"))->toBe(true)
      // jsdom does not implement the browser's blur-on-disable behavior.
      blur(input)
      let other = Screen.getByText("elsewhere")
      if movedElsewhere {
        Element.focus(other)
      }
      let acknowledged = {...lastCreation(dispatch), status: Failed({message: "invalid ref"})}
      rerender(element(acknowledged))
      expect(Document.activeElement)->toEqual(Nullable.make(movedElsewhere ? other : input))
      expect(Element.value(input))->toBe("develop")
    },
  )
})

[false, true]->Array.forEach(movedElsewhere => {
  test(
    "interrupted recovery offers keyboard retry without stealing chosen focus: " ++
    Bool.toString(movedElsewhere),
    () => {
      let dispatch = fn()
      let creation = CreationFixtures.make()
      let submission: View.CreationSubmission.t = {title: "retained", targets: []}
      let element = status =>
        <div>
          {view({...creation, status}, dispatch)}
          <button> {React.string("elsewhere")} </button>
        </div>
      let {rerender} = render(element(Reconciling({submission, next: Inspect})))
      let other = Screen.getByText("elsewhere")
      if movedElsewhere {
        Element.focus(other)
      }
      rerender(element(Interrupted({submission, message: "Check before retrying"})))
      let retry = Screen.getByText("Check and retry")
      expect(Document.activeElement)->toEqual(Nullable.make(movedElsewhere ? other : retry))
      expect(dispatch)->not_->toHaveBeenCalled
      if !movedElsewhere {
        FireEvent.keyDown(retry, {"key": "Enter", "ctrlKey": false})
        expect(dispatch)->toHaveBeenCalledWith(Action.RunCommand({command: Submit}))
        expect(Array.length(mock(dispatch).calls))->toBe(1)
      }
    },
  )
})

test("empty workspace guidance names the configured refresh chord", () => {
  let workspace = {...CreationFixtures.workspace(), repos: []}
  let chrome: array<View.Hint.t> = [{keys: "alt+f", command: Refresh, label: "refresh"}]
  let _ = render(
    <NewReview
      creation={CreationFixtures.make(~workspace)}
      workspaces=[workspace]
      chrome
      bindings=chrome
      dispatch={_ => ()}
    />,
  )
  let _ = Screen.getByText(
    "No repositories to review. Attach one to this workspace, then refresh (alt+f).",
  )
})
