let searchBindings: array<View.Hint.t> = [{keys: "esc", command: Back, label: "back"}]

open Vitest
open TestingLibrary

afterEach(cleanup)

let ws = () => {
  let workspace = Fixtures.parse(Domain.Workspace.schema, "protocol", "Workspace", "default")
  let repo = workspace.repos->Array.getUnsafe(0)
  {
    ...workspace,
    repos: [
      {...repo, id: "alpha", displayName: "Atlas"},
      {...repo, id: "beta", displayName: "Beacon"},
    ],
  }
}
let targets: array<Domain.ReviewTarget.t> = [
  {repoId: "alpha", base: Head({}), head: Branch({name: "main"})},
  {repoId: "beta", base: Head({}), head: Branch({name: "develop"})},
]
let chrome: array<View.Hint.t> = [
  {command: BrowseRevision, keys: "g b", label: "browse revision"},
  {command: ResetBrowse, keys: "g B", label: "return to review heads"},
  {command: NextBrowseRepo, keys: "g ]", label: "next Browse repository"},
]

test(
  "Browse displays visible revision separately from repository choice and failed candidate",
  () => {
    let dispatch = fn()
    let browse: View.BrowseView.t = {
      repoId: "beta",
      selection: Some({repoId: "alpha", refSpec: Branch({name: "main"})}),
      attempt: None,
    }
    let view = browse => <BrowseBar browse targets repositories={Workspace(ws())} chrome dispatch />
    let {rerender} = render(view(browse))
    let _ = Screen.getByText("Viewing: Atlas · branch:main")
    expect(Element.value(Screen.getByLabelText("Browse repository")))->toBe("beta")
    FireEvent.click(Screen.getByText("Choose revision…"))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.RunCommand({command: BrowseRevision}))
    expect(Element.getAttribute(Screen.getByText("Choose revision…"), "title"))->toEqual(
      Nullable.make("browse revision (g b)"),
    )
    FireEvent.change(Screen.getByLabelText("Browse repository"), {"target": {"value": "alpha"}})
    expect(dispatch)->toHaveBeenLastCalledWith(Action.SelectBrowseRepo({repoId: "alpha"}))
    rerender(
      view({
        ...browse,
        attempt: Some({
          target: {repoId: "beta", refSpec: Branch({name: "main"})},
          status: Loading({}),
        }),
      }),
    )
    let _ = Screen.getByText("Loading Beacon · branch:main…")
    let _ = Screen.getByText("Viewing: Atlas · branch:main")
    rerender(
      view({
        ...browse,
        attempt: Some({
          target: {repoId: "beta", refSpec: Branch({name: "main"})},
          status: Failed({message: "unknown branch"}),
        }),
      }),
    )
    let _ = Screen.getByText("Could not open Beacon · branch:main: unknown branch")
    let _ = Screen.getByText("Viewing: Atlas · branch:main")
    FireEvent.click(Screen.getByText("Review heads"))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.ResetBrowse({}))
    rerender(view({...browse, selection: None}))
    let _ = Screen.getByText("Viewing: Atlas · branch:main, Beacon · branch:develop")
  },
)

test("Browse search types j/k normally and results navigate without review mutation", () => {
  let dispatch = fn()
  let fixture = Fixtures.parse(View.RefSelectorView.schema, "client", "RefSelectorView", "default")
  let selector: View.RefSelectorView.t = {
    ...fixture,
    repoId: "beta",
    repoName: "Beacon",
    purpose: Browse({}),
    query: "",
    selected: 0,
    options: [
      {refSpec: Branch({name: "jack"}), subject: None, current: false},
      {refSpec: Branch({name: "develop"}), subject: None, current: true},
    ],
  }
  let {rerender} = render(<RefSelector bindings=searchBindings selector dispatch />)
  let input = Screen.getByPlaceholderText("Find a browse revision")
  let _ = Screen.getByText("Beacon · Browse")
  FireEvent.keyDown(input, {"key": "j", "ctrlKey": false})
  FireEvent.keyDown(input, {"key": "k", "ctrlKey": false})
  expect(dispatch)->not_->toHaveBeenCalled
  FireEvent.change(input, {"target": {"value": "jack"}})
  expect(dispatch)->toHaveBeenLastCalledWith(Action.RefSelectorQuery({query: "jack"}))
  rerender(<RefSelector bindings=searchBindings selector={...selector, query: "jack"} dispatch />)
  FireEvent.keyDown(input, {"key": "ArrowDown", "ctrlKey": false})
  let results = Screen.getByLabelText("revisions")
  expect(Document.activeElement)->toEqual(Nullable.make(results))
  FireEvent.keyDown(results, {"key": "j", "ctrlKey": false})
  expect(dispatch)->toHaveBeenLastCalledWith(Action.RefSelectorStep({delta: 1}))
  rerender(
    <RefSelector
      bindings=searchBindings selector={...selector, query: "jack", selected: 1} dispatch
    />,
  )
  FireEvent.keyDown(results, {"key": "Enter", "ctrlKey": false})
  expect(dispatch)->toHaveBeenLastCalledWith(Action.SelectCurrentRef({}))
  rerender(
    <RefSelector
      bindings=searchBindings
      selector={...selector, query: "jack", status: InvalidRef({message: "branch disappeared"})}
      dispatch
    />,
  )
  let _ = Screen.getByText("Invalid ref: branch disappeared")
  FireEvent.click(Screen.getByText("jack"))
  expect(dispatch)->toHaveBeenLastCalledWith(Action.SelectRef({index: 0}))
  FireEvent.keyDown(input, {"key": "Escape", "ctrlKey": false})
  expect(dispatch)->toHaveBeenLastCalledWith(Action.CloseRefSelector({}))
})

test(
  "the shell builds Browse repository choices from host patches without a core-only review snapshot",
  () => {
    let workspace = ws()
    let review = Fixtures.parse(Domain.Review.schema, "protocol", "Review", "default")
    let review = {...review, workspaceId: workspace.id, targets}
    let model: View.ViewModel.t = {
      ...View.ViewModel.empty,
      workspaces: [workspace],
      reviews: [review],
      openReview: Some(review.id),
      review: None,
      tab: Browse,
      browse: Some({repoId: "beta", selection: None, attempt: None}),
    }
    let dispatch = fn()
    let core: Core.t = {
      dispatch,
      key: _ => (),
      subscribe: listener => {
        listener(model)
        () => ()
      },
      attach: () => (),
    }
    let _ = render(<App.Shell core />)
    expect(Element.value(Screen.getByLabelText("Browse repository")))->toBe("beta")
    let _ = Screen.getByText("Viewing: Atlas · branch:main, Beacon · branch:develop")
    FireEvent.click(Screen.getByText("Choose revision…"))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.RunCommand({command: BrowseRevision}))
  },
)

test(
  "delayed query patches preserve newer typing and a reopened selector resets its buffer",
  () => {
    let dispatch = fn()
    let fixture = Fixtures.parse(
      View.RefSelectorView.schema,
      "client",
      "RefSelectorView",
      "default",
    )
    let selector: View.RefSelectorView.t = {...fixture, query: "", purpose: Browse({})}
    let {rerender} = render(<RefSelector bindings=searchBindings selector dispatch />)
    let input = Screen.getByPlaceholderText("Find a browse revision")
    FireEvent.change(input, {"target": {"value": "j"}})
    FireEvent.change(input, {"target": {"value": "jack"}})
    rerender(<RefSelector bindings=searchBindings selector={...selector, query: "j"} dispatch />)
    expect(Element.value(input))->toBe("jack")
    expect(Screen.queryAllByText("main · current")->Array.length)->toBe(0)
    FireEvent.keyDown(input, {"key": "Enter", "ctrlKey": false})
    expect(dispatch)->toHaveBeenLastCalledWith(Action.SelectCurrentRef({}))
    rerender(
      <RefSelector
        bindings=searchBindings selector={...selector, requestId: selector.requestId +. 1.} dispatch
      />,
    )
    expect(Element.value(Screen.getByPlaceholderText("Find a browse revision")))->toBe("")
  },
)
