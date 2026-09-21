let searchBindings: array<View.Hint.t> = [{keys: "esc", command: Back, label: "back"}]

open Vitest
open TestingLibrary

afterEach(cleanup)
let workspace: Domain.Workspace.t = {
  id: "01ARZ3NDEKTSV4RRFFQ69G5FAA",
  name: "Product",
  repos: [
    {id: "01ARZ3NDEKTSV4RRFFQ69G5FAB", displayName: "Atlas", path: "/srv/atlas"},
    {id: "01ARZ3NDEKTSV4RRFFQ69G5FAC", displayName: "Beacon", path: "/srv/beacon"},
  ],
}
let repositories = RepositoryIdentity.Workspace(workspace)
let file = (i): View.FileRef.t => {
  repoId: (workspace.repos->Array.getUnsafe(i)).id,
  path: "src/common.txt",
}
let diff = i => {
  ...Fixtures.parse(View.DiffView.schema, "client", "DiffView", "default"),
  file: file(i),
}
let thread = (i, place) => {
  ...Fixtures.parse(View.ThreadView.schema, "client", "ThreadView", "default"),
  id: "thread-" ++ Int.toString(i),
  place,
  context: None,
  outdated: false,
}
let query = (container, selector) =>
  Element.querySelector(container, selector)->Nullable.toOption->Option.getExn
let chrome: array<View.Hint.t> = [
  {keys: "g c", command: Comment, label: "comment"},
  {keys: "g v", command: ToggleViewed, label: "mark viewed"},
  {keys: "g y", command: CopyPath, label: "copy path"},
  {keys: "g x", command: ExpandContext, label: "expand context"},
  {keys: "g z", command: ToggleFileCollapse, label: "fold file"},
]

module Card = {
  @react.component
  let make = (~diff, ~dispatch, ~repositories: RepositoryIdentity.context=Unavailable) =>
    <FileDiff
      diff
      repositories
      chrome
      layout=Unified
      focus={Tree({index: 0})}
      threads=[]
      draft=None
      pendingRefresh=false
      isOpen=false
      dispatch
    />
}

test("stacked labels and each action retain the same-path file's repository", () => {
  let dispatch = fn()
  let {container, rerender} = render(
    <>
      <Card diff={diff(0)} repositories dispatch />
      <Card diff={diff(1)} repositories dispatch />
    </>,
  )
  [0, 1]->Array.forEach(i => {
    let name = i == 0 ? "Atlas" : "Beacon"
    let label = name ++ " · src/common.txt"
    let section = Screen.getByLabelText(label)
    expect(Element.textContent(query(section, ".file-diff-header")))->toContain(name)
    FireEvent.click(Screen.getByLabelText("Comment on " ++ label))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.CommentFile({file: file(i)}))
    FireEvent.click(Screen.getByLabelText("Viewed " ++ label))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.MarkViewed({file: file(i)}))
    FireEvent.click(Screen.getByLabelText("Expand context for " ++ label))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.ExpandContext({file: file(i), full: true}))
    FireEvent.click(Screen.getByLabelText("Collapse " ++ label))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.ToggleFileCollapse({file: file(i)}))
    let copy = Screen.getByLabelText("Copy relative path for " ++ label)
    expect(Element.getAttribute(copy, "title")->Nullable.toOption)->toEqual(Some("copy path (g y)"))
    FireEvent.click(copy)
    expect(dispatch)->toHaveBeenLastCalledWith(Action.CopyPath({path: "src/common.txt"}))
  })
  expect(Element.innerHTML(query(container, ".file-diff-header")))->toMatchSnapshot(
    "qualified sticky header and rebound controls",
  )
  rerender(<Card diff={{...diff(1), viewed: Viewed}} repositories dispatch />)
  FireEvent.click(Screen.getByLabelText("Viewed Beacon · src/common.txt"))
  expect(dispatch)->toHaveBeenLastCalledWith(Action.UnmarkViewed({file: file(1)}))
})

test(
  "Browse and pinned original headers identify their own file rather than the active repository",
  () => {
    let dispatch = fn()
    let base = {...diff(1), original: true}
    let {container, rerender} = render(
      <DiffView diff=base repositories layout=Unified focus={Tree({index: 0})} chrome dispatch />,
    )
    expect(
      Element.getAttribute(query(container, "[role=grid]"), "aria-label")->Nullable.toOption,
    )->toEqual(Some("Beacon · src/common.txt"))
    expect(Element.innerHTML(query(container, ".file-header")))->toMatchSnapshot(
      "original file header",
    )
    rerender(
      <DiffView
        diff={{...base, original: false}}
        repositories
        layout=Unified
        focus={Tree({index: 0})}
        chrome
        dispatch
      />,
    )
    expect(Element.textContent(query(container, ".file-header")))->toContain("Beacon")
  },
)

test(
  "conversation and inline locations match FileRef navigation including line ranges and original context",
  () => {
    let dispatch = fn()
    let threads = [
      thread(0, File({file: file(0)})),
      thread(1, Lines({file: file(1), side: Head, start: 4, end_: 7})),
    ]
    let {container, rerender} = render(
      <Threads
        title="Conversation" repositories threads focus={Tree({index: 0})} indexOffset=0 dispatch
      />,
    )
    FireEvent.click(Screen.getByText("Atlas · src/common.txt"))
    expect(dispatch)->toHaveBeenLastCalledWith(
      Action.Viewport({file: file(0), firstRow: 0, lastRow: 59}),
    )
    FireEvent.click(Screen.getByText("Beacon · src/common.txt:4-7"))
    expect(dispatch)->toHaveBeenLastCalledWith(
      Action.Viewport({file: file(1), firstRow: 0, lastRow: 34}),
    )
    expect(Element.textContent(query(container, ".thread-place")))->toMatchSnapshot(
      "conversation location",
    )
    let original = {
      ...threads->Array.getUnsafe(1),
      context: Some(Browse({reference: Branch({name: "main"})})),
    }
    rerender(
      <Threads
        title="Conversation"
        repositories
        threads=[original]
        focus={Tree({index: 0})}
        indexOffset=0
        dispatch
      />,
    )
    FireEvent.click(Screen.getByText("Beacon · src/common.txt:4-7"))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.OpenOriginalDiff({threadId: original.id}))
    rerender(
      <InlineThread
        thread=original repositories focused=false index=1 composer=React.null dispatch
      />,
    )
    expect(
      Element.getAttribute(query(container, "[role=note]"), "aria-label")->Nullable.toOption,
    )->toEqual(Some("Beacon · src/common.txt:4-7"))
    expect(Threads.placeText(~repositories, Review({})))->toBe("review")
  },
)

test(
  "both file-search and content-search results qualify same paths and preserve clicked identity",
  () => {
    let dispatch = fn()
    let search = Fixtures.parse(View.SearchView.schema, "client", "SearchView", "default")
    let hits = [0, 1]->Array.map(i => {...search.hits->Array.getUnsafe(0), file: file(i)})
    let {container, rerender} = render(
      <SearchBox bindings=searchBindings search={{...search, hits}} repositories dispatch />,
    )
    FireEvent.click(Screen.getByLabelText("Atlas · src/common.txt"))
    expect(dispatch)->toHaveBeenLastCalledWith(
      Action.Viewport({file: file(0), firstRow: 0, lastRow: 59}),
    )
    FireEvent.click(Screen.getByLabelText("Beacon · src/common.txt"))
    expect(dispatch)->toHaveBeenLastCalledWith(
      Action.Viewport({file: file(1), firstRow: 0, lastRow: 59}),
    )
    expect(Element.innerHTML(query(container, "[role=option]")))->toMatchSnapshot(
      "file search result",
    )
    let content = Fixtures.parse(
      View.ContentSearchView.schema,
      "client",
      "ContentSearchView",
      "default",
    )
    let content = {
      ...content,
      pending: false,
      hits: [0, 1]->Array.map(i => {
        ...content.hits->Array.getUnsafe(0),
        repoId: file(i).repoId,
        path: file(i).path,
        line: 4,
      }),
    }
    rerender(
      <Palette
        bindings=searchBindings
        contentSearch=Some(content)
        repositories
        actionPalette=false
        chrome
        dispatch
      />,
    )
    [0, 1]->Array.forEach(i => {
      FireEvent.click(
        Screen.getByLabelText((i == 0 ? "Atlas" : "Beacon") ++ " · src/common.txt:4"),
      )
      expect(dispatch)->toHaveBeenLastCalledWith(
        Action.Viewport({file: file(i), firstRow: 0, lastRow: 34}),
      )
    })
    expect(Element.innerHTML(query(container, "[role=option]")))->toMatchSnapshot(
      "content search result",
    )
  },
)

test(
  "same checkout IDs use the open review's membership names even when another workspace comes first",
  () => {
    let review = {
      ...Fixtures.parse(Domain.Review.schema, "protocol", "Review", "default"),
      workspaceId: workspace.id,
    }
    let other = {
      ...workspace,
      id: "other-workspace",
      repos: workspace.repos->Array.map(r => {...r, displayName: "Wrong workspace name"}),
    }
    let model = {
      ...View.ViewModel.empty,
      workspaces: [other, workspace],
      reviews: [review],
      openReview: Some(review.id),
    }
    expect(RepositoryIdentity.fileText(RepositoryIdentity.ofView(model), file(0)))->toBe(
      "Atlas · src/common.txt",
    )
    expect(
      RepositoryIdentity.fileText(
        RepositoryIdentity.ofView({...model, workspaces: [other]}),
        file(0),
      ),
    )->toBe("Repository · Q69G5FAB · src/common.txt")
  },
)

test(
  "duplicate names and detached or delayed metadata remain distinct without changing copy payload",
  () => {
    let dispatch = fn()
    let duplicates = RepositoryIdentity.Workspace({
      ...workspace,
      repos: workspace.repos->Array.map(r => {...r, displayName: "Shared"}),
    })
    let {container, rerender} = render(
      <>
        <Card diff={diff(0)} repositories=duplicates dispatch />
        <Card diff={diff(1)} repositories=duplicates dispatch />
      </>,
    )
    expect(Element.textContent(container))->toContain("Shared · Q69G5FAB")
    expect(Element.textContent(container))->toContain("Shared · Q69G5FAC")
    expect(
      Element.getAttribute(query(container, ".repository-file"), "title")->Nullable.toOption,
    )->toEqual(Some("Shared · /srv/atlas · Q69G5FAB · src/common.txt"))
    FireEvent.click(
      Screen.getByLabelText("Copy relative path for Shared · Q69G5FAC · src/common.txt"),
    )
    expect(dispatch)->toHaveBeenLastCalledWith(Action.CopyPath({path: "src/common.txt"}))
    rerender(<Card diff={diff(1)} repositories={Workspace({...workspace, repos: []})} dispatch />)
    expect(Element.textContent(container))->toContain("Detached repository · Q69G5FAC")
    rerender(<Card diff={diff(1)} dispatch />)
    expect(Element.textContent(container))->toContain("Repository · Q69G5FAC")
  },
)
