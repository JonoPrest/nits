open Vitest
open TestingLibrary

afterEach(cleanup)

let texts = elements => {
  let out = []
  elements->Array.forEach(element => out->Array.push(Element.textContent(element)))
  out
}

let cell = (ending: Render.LineEnding.t, ~text="same", ~lineNo=7): Render.Cell.t => {
  lineNo,
  text,
  ending,
  spans: [],
  changed: [],
}
let view = (row, layout, ~onComment=fn()) =>
  <Row row layout index=9 focused=false threads=[] onComment />

[View.Layout.Unified, Split]->Array.forEach(layout => {
  test("terminator-only change labels stay on the two source cells", () => {
    let onComment = fn()
    let {container} = render(
      view(Modified({left: cell(Missing), right: cell(Lf)}), layout, ~onComment),
    )
    expect(Screen.getByText("No final newline"))->toBeTruthy
    expect(Screen.getByText("LF"))->toBeTruthy
    expect(Element.querySelectorAll(container, ".row")->Array.length)->toBe(1)
    expect(Element.querySelectorAll(container, ".cell-line-no")->texts)->toEqual(["7", "7"])
    let buttons = Element.querySelectorAll(container, ".cell-comment")
    buttons->Array.forEach(FireEvent.click)
    expect(onComment)->toHaveBeenCalledWith(Domain.Side.Base)
    expect(onComment)->toHaveBeenCalledWith(Domain.Side.Head)
  })

  test("expanded context and Browse retain CRLF and missing-final-newline labels", () => {
    let {rerender} = render(view(Context({left: cell(CrLf), right: cell(CrLf)}), layout))
    expect(Screen.queryAllByText("CRLF")->Array.length)->toBe(layout == Split ? 2 : 1)
    rerender(view(Context({left: cell(Missing), right: cell(Missing)}), layout))
    expect(Screen.queryAllByText("No final newline")->Array.length)->toBe(layout == Split ? 2 : 1)
  })
})

test("whitespace-ignored unified context does not hide differing endings", () => {
  let _ = render(view(Context({left: cell(CrLf), right: cell(Lf)}), Unified))
  expect(Screen.getByText("Line ending: CRLF → LF"))->toBeTruthy
})

test("bare CR stays source text while its display glyph and ending marker are separate", () => {
  let {container} = render(view(Added({right: cell(Missing, ~text="value\r")}), Unified))
  let controls = Element.querySelectorAll(container, ".cell-carriage-return")
  expect(controls->texts)->toEqual(["\r"])
  expect(Screen.getByText("No final newline"))->toBeTruthy
  expect(Element.querySelectorAll(container, ".cell-ending")->Array.length)->toBe(1)
  expect(Element.querySelectorAll(container, ".cell-line-no")->Array.length)->toBe(1)
})
