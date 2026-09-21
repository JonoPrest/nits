open Vitest
open TestingLibrary

afterEach(cleanup)
let ready = () => Fixtures.parse(View.SuggestionView.schema, "client", "SuggestionView", "default")
let chrome: array<View.Hint.t> = [
  {command: PreviewSuggestion, keys: "x p", label: "preview suggestion"},
  {command: ApplySuggestion, keys: "x a", label: "apply suggestion"},
]
let card = (suggestion, dispatch) => <SuggestionCard suggestion chrome dispatch />
let applyButton = () => Screen.getByText("Apply suggestion")
let previewButton = () => Screen.getByText("Preview suggestion")

describe("Suggestion preview", () => {
  test("renders exact endings and derives both control hints from keymap", () => {
    let suggestion = ready()
    let inspection: Suggestion.Inspection.t = Checked({
      worktree: Original({}),
      hunks: [
        {
          header: "@@ -1 +1 @@",
          lines: [
            {kind: Remove({old: 1}), text: "héllo", ending: CrLf},
            {kind: Add({new: 1}), text: "世界", ending: CrLf},
          ],
        },
        {
          header: "@@ -3 +3 @@",
          lines: [
            {kind: Remove({old: 3}), text: "尾\r", ending: Missing},
            {kind: Add({new: 3}), text: "終", ending: Missing},
          ],
        },
      ],
    })
    let dispatch = fn()
    let {container} = render(card({...suggestion, inspection: Some(inspection)}, dispatch))
    expect(Element.textContent(Screen.getByLabelText("Suggested patch")))->toContain("世界")
    expect(Array.length(Screen.queryAllByText("CRLF")))->toBe(2)
    expect(Array.length(Screen.queryAllByText("No final newline")))->toBe(2)
    expect(Array.length(Element.querySelectorAll(container, ".cell-carriage-return")))->toBe(1)
    expect(Element.getAttribute(previewButton(), "title")->Nullable.toOption)->toBe(
      Some("preview suggestion (x p)"),
    )
    expect(Element.getAttribute(applyButton(), "title")->Nullable.toOption)->toBe(
      Some("apply suggestion (x a)"),
    )
    FireEvent.click(previewButton())
    expect(dispatch)->toHaveBeenLastCalledWith(
      Action.PreviewSuggestion({commentId: suggestion.record.commentId}),
    )
    FireEvent.click(applyButton())
    expect(dispatch)->toHaveBeenLastCalledWith(
      Action.ApplySuggestion({commentId: suggestion.record.commentId}),
    )
  })

  test("pending, stale, rejected and uncertain states retain preview and block apply", () => {
    let suggestion = ready()
    let dispatch = fn()
    let {rerender} = render(card(suggestion, dispatch))
    let statuses: array<View.SuggestionStatus.t> = [
      Applying({}),
      Stale({}),
      Rejected({message: "target is linked"}),
      Uncertain({message: "Application unconfirmed"}),
    ]
    statuses->Array.forEach(
      status => {
        rerender(card({...suggestion, status}, dispatch))
        expect(Element.textContent(Screen.getByLabelText("Suggested patch")))->toContain(
          "let x: u32 = 1;",
        )
        expect(Element.hasAttribute(applyButton(), "disabled"))->toBe(true)
        FireEvent.click(applyButton())
      },
    )
    expect(dispatch)->not_->toHaveBeenCalled
    rerender(card({...suggestion, status: Applying({})}, dispatch))
    expect(Element.hasAttribute(previewButton(), "disabled"))->toBe(true)
  })

  test("malformed and unchecked patches remain inspectable", () => {
    let suggestion = ready()
    let malformed = {
      ...suggestion,
      record: {...suggestion.record, patch: "@@ broken\n+visible evidence\n"},
      status: Rejected({message: "malformed hunk header"}),
      inspection: Some(Rejected({reason: "malformed hunk header"})),
    }
    let {rerender} = render(card(malformed, fn()))
    expect(Element.textContent(Screen.getByLabelText("Suggested patch (raw)")))->toContain(
      "@@ broken",
    )
    expect(Element.hasAttribute(applyButton(), "disabled"))->toBe(true)
    rerender(card({...suggestion, inspection: None, status: Unloaded({})}, fn()))
    expect(Element.hasAttribute(applyButton(), "disabled"))->toBe(true)
    expect(Element.hasAttribute(previewButton(), "disabled"))->toBe(false)
  })

  test("duplicate repository paths label and dispatch the exact suggestion", () => {
    let suggestion = ready()
    let workspace = Fixtures.parse(Domain.Workspace.schema, "protocol", "Workspace", "default")
    let alpha = "00000000000000000000000001"
    let beta = "00000000000000000000000002"
    let repositories: RepositoryIdentity.context = Workspace({
      ...workspace,
      repos: [
        {id: alpha, displayName: "service", path: "/checkouts/alpha"},
        {id: beta, displayName: "service", path: "/checkouts/beta"},
      ],
    })
    let variants = [alpha, beta]->Array.mapWithIndex(
      (repoId, index) => {
        ...suggestion,
        record: {
          ...suggestion.record,
          commentId: "comment-" ++ Int.toString(index),
          anchor: File({
            repoId,
            path: "same.txt",
            blobOid: "1111111111111111111111111111111111111111",
          }),
        },
      },
    )
    let dispatch = fn()
    let _ = render(
      <div>
        {variants
        ->Array.map(
          suggestion =>
            <SuggestionCard
              key=suggestion.record.commentId suggestion repositories chrome dispatch
            />,
        )
        ->React.array}
      </div>,
    )
    variants->Array.forEachWithIndex(
      (suggestion, index) => {
        let repoId = index == 0 ? alpha : beta
        let location = RepositoryIdentity.fileDescription(repositories, {repoId, path: "same.txt"})
        FireEvent.click(Screen.getByLabelText("Apply suggestion for " ++ location))
        expect(dispatch)->toHaveBeenLastCalledWith(
          Action.ApplySuggestion({commentId: suggestion.record.commentId}),
        )
        FireEvent.click(Screen.getByLabelText("Preview suggestion for " ++ location))
        expect(dispatch)->toHaveBeenLastCalledWith(
          Action.PreviewSuggestion({commentId: suggestion.record.commentId}),
        )
      },
    )
  })

  test("reply suggestions render and target the reply in both thread views", () => {
    let thread = Fixtures.parse(View.ThreadView.schema, "client", "ThreadView", "default")
    let root = thread.comments->Array.getUnsafe(0)
    let suggestion = ready()
    let reply: View.CommentView.t = {
      ...root,
      id: suggestion.record.commentId,
      body: "A suggested reply",
      suggestion: Some(suggestion),
    }
    let thread = {...thread, comments: [root, reply], replies: 1}
    let dispatch = fn()
    let {rerender} = render(
      <Threads
        title="Discussion" threads=[thread] focus={Thread({index: 0})} indexOffset=0 chrome dispatch
      />,
    )
    FireEvent.click(applyButton())
    expect(dispatch)->toHaveBeenLastCalledWith(Action.ApplySuggestion({commentId: reply.id}))
    rerender(<InlineThread thread chrome focused=true index=0 composer={React.null} dispatch />)
    expect(Element.textContent(Screen.getByLabelText("Suggested patch")))->toContain(
      "let x: u32 = 1;",
    )
    FireEvent.click(applyButton())
    expect(dispatch)->toHaveBeenLastCalledWith(Action.ApplySuggestion({commentId: reply.id}))
  })

  test("receipt and recovery location survive a subsequent check", () => {
    let suggestion = ready()
    let applicationReceipt: Domain.SuggestionReceipt.t = Fixtures.parse(
      Domain.SuggestionReceipt.schema,
      "protocol",
      "SuggestionReceipt",
      "default",
    )
    let outcome: Domain.SuggestionOutcome.t = Applied({receipt: applicationReceipt})
    let record: Domain.SuggestionRecord.t = {...suggestion.record, outcome}
    let suggestion = {
      ...suggestion,
      record,
      status: Applied({}),
      notice: Some("Original retained at /recovery/original"),
    }
    let _ = render(card(suggestion, fn()))
    expect(Element.hasAttribute(applyButton(), "disabled"))->toBe(true)
    expect(Element.textContent(Screen.getByTextRe(/Applied by/)))->toContain("ada")
    expect(
      Element.textContent(Screen.getByText("Original retained at /recovery/original")),
    )->toContain("/recovery/original")
    expect(Element.textContent(Screen.getByTextRe(/Result blob/)))->toContain(
      String.slice(applicationReceipt.resultBlob, ~start=0, ~end=12),
    )
  })
})
