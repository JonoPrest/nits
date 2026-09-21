open Vitest
open TestingLibrary

afterEach(cleanup)

let base = () => Fixtures.parse(View.DiffView.schema, "client", "DiffView", "default")
let diffFor = (variant: string): View.DiffView.t => {
  let submodule: Domain.SubmoduleChange.t = Fixtures.parse(
    Domain.SubmoduleChange.schema,
    "protocol",
    "SubmoduleChange",
    variant,
  )
  {
    ...base(),
    target: Render.RenderTarget.Diff({change: Domain.ChangeKind.Submodule({change: submodule})}),
    content: Submodule({}),
    rows: [],
    missing: [],
    fileThreads: [],
    viewed: Unviewed,
    collapsed: false,
  }
}
let file = (diff, dispatch) =>
  <FileDiff
    diff
    layout=Unified
    focus={Tree({index: 0})}
    threads=[]
    draft=None
    pendingRefresh=false
    isOpen=true
    dispatch
  />

describe("Submodule changes", () => {
  Fixtures.variants("protocol", "SubmoduleChange")->Array.forEach(variant => {
    test(
      `renders ${variant} metadata and only supported controls`,
      () => {
        let diff = diffFor(variant)
        let dispatch = fn()
        let {container} = render(file(diff, dispatch))
        let metadata = Screen.getByLabelText("Submodule change")
        expect(Element.textContent(metadata))->toContain("no line diff")
        expect(Element.querySelector(container, ".row"))->toBeNull
        expect(Screen.queryAllByText("expand file")->Array.length)->toBe(0)
        let comment = Element.querySelector(container, "[aria-label^='Comment on ']")
        expect(comment->Nullable.toOption == None)->toBe(variant != "SubmoduleToBlob")
        FireEvent.click(
          Screen.getByLabelText("Viewed " ++ RepositoryIdentity.fileText(Unavailable, diff.file)),
        )
        expect(dispatch)->toHaveBeenLastCalledWith(Action.MarkViewed({file: diff.file}))
        expect(Element.textContent(metadata))->toMatchSnapshot(variant)
      },
    )
  })

  test("switching a cached text diff to gitlink metadata removes stale rows", () => {
    let dispatch = fn()
    let text = {...base(), collapsed: false}
    let {container, rerender} = render(file(text, dispatch))
    expect(Element.querySelector(container, ".row"))->not_->toBeNull
    rerender(file(diffFor("BlobToSubmodule"), dispatch))
    expect(Element.querySelector(container, ".row"))->toBeNull
    expect(Screen.getByLabelText("Submodule change"))->toBeTruthy
  })

  test("the single-file view shows metadata without a source expansion action", () => {
    let diff = diffFor("Updated")
    let {container} = render(
      <DiffView diff layout=Unified focus={Tree({index: 0})} dispatch={fn()} />,
    )
    expect(Screen.getByLabelText("Submodule change"))->toBeTruthy
    expect(Element.querySelector(container, ".row"))->toBeNull
    expect(Screen.queryAllByText("expand file")->Array.length)->toBe(0)
  })
})
