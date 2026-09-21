open Vitest
open TestingLibrary

afterEach(cleanup)
let workspace = (): Domain.Workspace.t => {
  id: "01ARZ3NDEKTSV4RRFFQ69G5FAA",
  name: "Product",
  repos: [
    {id: "01ARZ3NDEKTSV4RRFFQ69G5FAB", displayName: "Atlas", path: "/srv/one/atlas"},
    {id: "01ARZ3NDEKTSV4RRFFQ69G5FAC", displayName: "Atlas", path: "/srv/two/atlas"},
    {id: "01ARZ3NDEKTSV4RRFFQ69G5FAD", displayName: "Beacon", path: "/srv/beacon"},
  ],
}
let model = (): View.ViewModel.t => {
  let ws = workspace()
  let review = Fixtures.parse(Domain.Review.schema, "protocol", "Review", "default")
  {
    ...View.ViewModel.empty,
    workspaces: [ws],
    daemonContext: Some(Named({name: "remote-build"})),
    home: {...View.HomeView.empty, selectedWorkspace: Some(ws.id)},
    chrome: [
      {keys: "g y", command: CopyCheckout, label: "copy checkout path"},
      {keys: "g n", command: NewReview, label: "new review"},
      {keys: "g o", command: Open, label: "open"},
      {keys: "g R", command: Refresh, label: "refresh"},
      {keys: "g W", command: GoHome, label: "workspaces"},
    ],
    reviews: [
      {
        ...review,
        workspaceId: ws.id,
        targets: [
          {
            repoId: (ws.repos->Array.getUnsafe(2)).id,
            base: Branch({name: "main"}),
            head: WorkingTree({}),
          },
        ],
      },
    ],
  }
}

test(
  "home separates complete membership from review targets and disambiguates duplicate names",
  () => {
    let m = model()
    let dispatch = fn()
    let {container} = render(<WorkspaceHome model=m dispatch />)
    expect(Element.textContent(container))->toContain("3 repositories")
    expect(Element.textContent(container))->toContain("Daemon context: remote-build")
    expect(Element.textContent(container))->toContain("Atlas · Q69G5FAB")
    expect(Element.textContent(container))->toContain("Atlas · Q69G5FAC")
    let targets = Screen.getByLabelText("Repositories in this review")
    expect(Element.textContent(targets))->toContain("Beacon")
    expect(Element.textContent(targets))->not_->toContain("Atlas")
    expect(Element.textContent(targets))->toContain("main → worktree")
    let copy = Screen.getByLabelText("Copy checkout path for Beacon")
    expect(Element.getAttribute(copy, "title")->Nullable.toOption)->toEqual(
      Some("copy checkout path (g y)"),
    )
    FireEvent.click(copy)
    expect(dispatch)->toHaveBeenLastCalledWith(
      Action.CopyCheckout({repoId: "01ARZ3NDEKTSV4RRFFQ69G5FAD"}),
    )
    FireEvent.click(Screen.getByText("New review"))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.StartReview({workspaceId: workspace().id}))
  },
)

test("empty daemon and empty workspace offer correct attach and refresh guidance", () => {
  let dispatch = fn()
  let {container, rerender} = render(<WorkspaceHome model=View.ViewModel.empty dispatch />)
  expect(Element.textContent(container))->toContain("nits workspace add <name>")
  expect(Element.textContent(container))->toContain(
    "nits workspace attach <workspace-id> <checkout-path>",
  )
  FireEvent.click(Screen.getByText("Refresh workspaces"))
  expect(dispatch)->toHaveBeenLastCalledWith(Action.ListWorkspaces({}))
  let m = model()
  let empty = {...workspace(), repos: []}
  rerender(<WorkspaceHome model={...m, workspaces: [empty], reviews: []} dispatch />)
  expect(Element.textContent(container))->toContain("0 repositories")
  expect(Element.textContent(container))->toContain(
    "nits --context 'remote-build' workspace attach " ++ empty.id,
  )
  expect(Element.hasAttribute(Screen.getByText("New review"), "disabled"))->toBe(true)
})

test(
  "single-repository review keeps workspace, daemon and active repo visible with sidebar hidden",
  () => {
    let m = model()
    let review = m.reviews->Array.getUnsafe(0)
    let ws = workspace()
    let core: Core.t = {
      dispatch: fn(),
      key: fn(),
      attach: () => (),
      subscribe: listener => {
        listener({
          ...m,
          openReview: Some(review.id),
          activeRepo: Some((ws.repos->Array.getUnsafe(2)).id),
          prefs: {...m.prefs, sidebarHidden: true},
        })
        () => ()
      },
    }
    let {container} = render(<App.Shell core />)
    expect(Element.querySelector(container, ".app-left")->Nullable.toOption)->toEqual(None)
    let location = Screen.getByLabelText("Review location")
    expect(Element.textContent(location))->toContain("Product")
    expect(Element.textContent(location))->toContain("remote-build")
    expect(Element.textContent(location))->toContain("Active repository: Beacon")
    expect(Element.textContent(Screen.getByLabelText("review targets")))->toContain("Beacon:")
  },
)
