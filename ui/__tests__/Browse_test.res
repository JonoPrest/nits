open Vitest
open TestingLibrary

// Give the real virtualizer a measurable viewport; jsdom has no layout.
// The tests below exercise the Browse component, including its actual rows.
%%raw(`
Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
  configurable: true, get() { return this.classList.contains("diff-scroll") ? 300 : 20; }
});
Object.defineProperty(HTMLElement.prototype, "offsetWidth", {
  configurable: true, get() { return 800; }
});
HTMLElement.prototype.scrollTo = function () {};
`)

afterEach(cleanup)

let base = (): View.DiffView.t => {
  let diff = Fixtures.parse(View.DiffView.schema, "client", "DiffView", "default")
  let rows = [1, 2, 3]->Array.map(line => {
    let cell: Render.Cell.t = {
      lineNo: line,
      text: "source " ++ Int.toString(line),
      spans: [],
      changed: [],
    }
    let row: View.DiffRow.t = {
      index: line - 1,
      row: Context({left: cell, right: cell}),
      threads: [],
      drafted: None,
    }
    row
  })
  {
    ...diff,
    target: Blob({oid: "original"}),
    viewed: Unviewed,
    firstRow: 0,
    lastRow: 2,
    rows,
    missing: [],
    fileThreads: [],
    content: Text({
      totalRows: 3,
      chunkRows: 100,
      chunkCount: 1,
      highlighted: false,
      additions: 0,
      deletions: 0,
      gaps: [],
    }),
  }
}

let draft = (diff: View.DiffView.t): View.Draft.t => {
  anchor: Lines({
    repoId: diff.file.repoId,
    path: diff.file.path,
    side: Head,
    blobOid: "original",
    lines: {start: 1, end_: 2},
    contextHash: "0000000000000000",
  }),
  submissionError: None,
  purpose: Comment({intent: Finding, context: Some(Browse({reference: Tag({name: "v1"})}))}),
}

let chrome: array<View.Hint.t> = [{keys: "c", command: Comment, label: "comment"}]

test("Browse cells focus Head and the comment mouse alias opens the typed line action", () => {
  let dispatch = fn()
  let diff = base()
  let {container} = render(
    <DiffView diff layout=Split focus={Diff({row: 0, side: Head})} chrome dispatch />,
  )
  FireEvent.click(Screen.getByText("source 2"))
  expect(dispatch)->toHaveBeenLastCalledWith(Action.SetFocus({focus: Diff({row: 1, side: Head})}))
  FireEvent.click(Screen.getByLabelText("Comment on line 2"))
  expect(dispatch)->toHaveBeenLastCalledWith(
    Action.CommentLines({file: diff.file, side: Head, startLine: 2, endLine: 2}),
  )
  expect(
    Screen.getByLabelText("Comment on line 2")->Element.getAttribute("title")->Nullable.getExn,
  )->toBe("comment (c)")
  expect(Element.querySelectorAll(container, ".cell-left")->Array.length)->toBe(0)
})

test("Browse drag and visual ranges mark source cells on their starting side", () => {
  let dispatch = fn()
  let diff = base()
  let {container} = render(
    <DiffView
      diff
      layout=Unified
      focus={Diff({row: 2, side: Head})}
      visual={{start: 0, end_: 2, side: Head}}
      chrome
      dispatch
    />,
  )
  expect(Element.querySelectorAll(container, ".cell-selected")->Array.length)->toBe(3)
  FireEvent.mouseDown(Screen.getByText("source 3"))
  FireEvent.mouseEnter(Screen.getByText("source 1")->Element.parentElement)
  FireEvent.mouseUp(Screen.getByText("source 1"))
  expect(dispatch)->toHaveBeenLastCalledWith(
    Action.CommentLines({file: diff.file, side: Head, startLine: 3, endLine: 1}),
  )
})

test("Browse composes below the range, focuses the editor, cancels and submits", () => {
  let dispatch = fn()
  let diff = base()
  let drafted = {
    ...diff,
    rows: diff.rows->Array.map(row => {
      ...row,
      drafted: row.index == 0
        ? Some((Inside, Domain.Side.Head))
        : row.index == 1
        ? Some((Anchor, Domain.Side.Head))
        : None,
    }),
  }
  let {container, rerender} = render(
    <DiffView diff=drafted layout=Unified focus={Composer({})} draft={draft(diff)} dispatch />,
  )
  let textarea = Screen.getByPlaceholderText("Finding…")
  expect(Document.activeElement->Nullable.getExn)->toBe(textarea)
  let anchor = Element.querySelector(container, "[data-row-index='1']")->Nullable.getExn
  expect(Element.querySelector(anchor->Element.parentElement, ".composer"))->not_->toBeNull
  expect(Element.querySelectorAll(container, ".cell-drafting")->Array.length)->toBe(2)
  FireEvent.keyDown(textarea, {"key": "Escape", "ctrlKey": false})
  expect(dispatch)->toHaveBeenLastCalledWith(Action.DraftDiscarded({}))
  rerender(<DiffView diff layout=Unified focus={Diff({row: 1, side: Head})} dispatch />)
  expect(Element.querySelector(container, ".composer"))->toBeNull
  rerender(
    <DiffView diff=drafted layout=Unified focus={Composer({})} draft={draft(diff)} dispatch />,
  )
  let textarea = Screen.getByPlaceholderText("Finding…")
  FireEvent.change(textarea, {"target": {"value": "reviewed this revision"}})
  FireEvent.keyDown(textarea, {"key": "Enter", "ctrlKey": true})
  expect(dispatch)->toHaveBeenLastCalledWith(
    Action.DraftSubmitted({body: "reviewed this revision"}),
  )
})

test(
  "Browse threads render inline with provenance and Conversation jumps to original content",
  () => {
    let dispatch = fn()
    let diff = base()
    let thread = Fixtures.parse(View.ThreadView.schema, "client", "ThreadView", "default")
    let thread = {...thread, context: Some(Browse({reference: Tag({name: "v1"})})), outdated: false}
    let placed = {
      ...diff,
      rows: diff.rows->Array.map(row => {
        ...row,
        threads: row.index == 1 ? [{thread: thread.id, side: Head, place: Anchor}] : [],
      }),
    }
    let {container} = render(
      <DiffView
        diff=placed layout=Unified focus={Diff({row: 1, side: Head})} threads=[thread] dispatch
      />,
    )
    let anchor = Element.querySelector(container, "[data-row-index='1']")->Nullable.getExn
    expect(Element.querySelector(anchor->Element.parentElement, ".inline-thread"))->not_->toBeNull
    expect(Screen.getByText("browse @tag:v1"))->toBeTruthy
    cleanup()
    let _ = render(
      <Threads
        title="Conversation" threads=[thread] focus={Thread({index: 0})} indexOffset=0 dispatch
      />,
    )
    FireEvent.click(Screen.getByText("browse @tag:v1"))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.OpenOriginalDiff({threadId: thread.id}))
  },
)

test("Browse loading and non-source content never expose comment targets", () => {
  let dispatch = fn()
  let diff = base()
  let invalid = {
    ...diff,
    rows: [{index: 0, row: HunkHeader({text: "header"}), threads: [], drafted: None}],
    missing: [0],
  }
  let {container, rerender} = render(
    <DiffView diff=invalid layout=Unified focus={Diff({row: 0, side: Head})} chrome dispatch />,
  )
  expect(Element.querySelectorAll(container, ".cell-comment")->Array.length)->toBe(0)
  expect(Element.querySelectorAll(container, ".row-placeholder")->Array.length)->toBe(2)
  let binary = Fixtures.parse(Render.RenderContent.schema, "protocol", "RenderContent", "Binary")
  rerender(
    <DiffView
      diff={{...diff, content: binary, rows: []}}
      layout=Unified
      focus={Diff({row: 0, side: Head})}
      chrome
      dispatch
    />,
  )
  expect(Element.querySelectorAll(container, ".cell-comment")->Array.length)->toBe(0)
})

test("same-length ref changes clear cached rows by blob identity", () => {
  let before = base()
  let after = {...before, target: Blob({oid: "different"})}
  expect(DiffSeen.fileKey(before))->not_->toBe(DiffSeen.fileKey(after))
  let seen = DiffSeen.mergeSeen(Dict.make(), "", DiffSeen.fileKey(before), before.rows)
  let next = DiffSeen.mergeSeen(
    seen,
    DiffSeen.fileKey(before),
    DiffSeen.fileKey(after),
    [after.rows->Array.getUnsafe(0)],
  )
  expect(next->Dict.valuesToArray->Array.length)->toBe(1)
})
