open Vitest
open TestingLibrary

afterEach(cleanup)

let base = () => Fixtures.parse(View.DiffView.schema, "client", "DiffView", "default")
let entry = (mode: Domain.BlobMode.t): Domain.BlobEntry.t => {
  oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  mode,
}
let target = (old, new): Render.RenderTarget.t => Diff({
  change: Modified({old: entry(old), new: entry(new)}),
})
let file = diff =>
  <FileDiff
    diff
    layout=Unified
    focus={Tree({index: 0})}
    threads=[]
    draft=None
    pendingRefresh=false
    isOpen=true
    dispatch={fn()}
  />

let cases: array<(Domain.BlobMode.t, Domain.BlobMode.t, string)> = [
  (Regular, Executable, "100644 → 100755 (executable)"),
  (Executable, Regular, "100755 (executable) → 100644"),
  (Regular, Symlink, "100644 → 120000 (symlink)"),
  (Symlink, Regular, "120000 (symlink) → 100644"),
  (UnknownMode, UnknownMode, "Git mode unknown (historical)"),
]
cases->Array.forEach(((old, new, text)) =>
  test(`metadata remains visible with no hunks: ${text}`, () => {
    let diff = {
      ...base(),
      target: target(old, new),
      rows: [],
      missing: [],
      content: Render.RenderContent.Binary({}),
      collapsed: false,
    }
    let {rerender} = render(file(diff))
    expect(Screen.getByText(text))->toBeTruthy
    expect(Screen.getByText("binary file"))->toBeTruthy
    rerender(file({...diff, collapsed: true, viewed: Viewed}))
    expect(Screen.getByText(text))->toBeTruthy
  })
)

test("Browse and original single-file rendering retain known and unknown mode labels", () => {
  let diff = {...base(), target: Render.RenderTarget.Blob({entry: entry(Symlink)})}
  let {rerender} = render(<DiffView diff layout=Split focus={Tree({index: 0})} dispatch={fn()} />)
  expect(Screen.getByText("120000 (symlink)"))->toBeTruthy
  rerender(
    <DiffView
      diff={{...diff, target: Blob({entry: entry(UnknownMode)}), original: true}}
      layout=Split
      focus={Tree({index: 0})}
      dispatch={fn()}
    />,
  )
  expect(Screen.getByText("unknown Git mode"))->toBeTruthy
})

test("mode-only identity changes discard previously cached viewport rows", () => {
  let original = {...base(), target: target(Regular, Regular), collapsed: false}
  let {container, rerender} = render(file(original))
  expect(Element.querySelector(container, ".row"))->not_->toBeNull
  rerender(file({...original, target: target(Regular, Executable), rows: []}))
  expect(Element.querySelector(container, ".row"))->toBeNull
  expect(Screen.getByText("100644 → 100755 (executable)"))->toBeTruthy
})
