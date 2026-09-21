open Vitest
open TestingLibrary

@module("react") external act: (unit => unit) => unit = "act"
@set external setScrollIntoView: (element, unit => unit) => unit = "scrollIntoView"
let select = (container, selector) => container->Element.querySelector(selector)->Nullable.getExn
let nativeKey: (
  element,
  string,
  bool,
  bool,
  bool,
) => bool = %raw(`(target, key, ctrlKey, altKey, isComposing) =>
  target.dispatchEvent(new KeyboardEvent('keydown', {key, ctrlKey, altKey, isComposing, bubbles: true, cancelable: true}))`)

module Shell = {
  @react.component
  let make = (~bindings, ~sent, ~children) => {
    React.useEffect0(() => {
      let pending = App.Pending.make()
      let core: Core.t = {key: sent, dispatch: _ => (), subscribe: _ => () => (), attach: () => ()}
      let listener = ev =>
        App.onKeyDown(core, ~onChord=chord => App.Pending.step(pending, bindings, chord), ev)
      App.KeyEvent.listen("keydown", listener)
      Some(() => App.KeyEvent.unlisten("keydown", listener))
    })
    children
  }
}

afterEach(cleanup)
let custom: array<View.Hint.t> = [
  {keys: "alt+s", command: Submit, label: "submit"},
  {keys: "alt+q", command: Back, label: "discard"},
]

test(
  "non-text controls route configured chords while editing, IME and native activation stay native",
  () => {
    let sent = fn()
    let bindings: array<View.Hint.t> = [
      {keys: "g W", command: GoHome, label: "home"},
      {keys: "ctrl+k", command: FileSearch, label: "files"},
    ]
    let {container} = render(
      <Shell bindings sent>
        <div>
          <input type_="checkbox" ariaLabel="Viewed" />
          <input ariaLabel="Title" />
          <textarea ariaLabel="Body" />
          <select ariaLabel="Repository">
            <option> {React.string("one")} </option>
          </select>
          <div contentEditable=true suppressContentEditableWarning=true>
            {React.string("editable")}
          </div>
          <button> {React.string("Native action")} </button>
        </div>
      </Shell>,
    )
    let box = Screen.getByLabelText("Viewed")
    expect(nativeKey(box, " ", false, false, false))->toBe(true)
    expect(nativeKey(box, "g", false, false, false))->toBe(false)
    expect(nativeKey(box, "W", false, false, false))->toBe(false)
    expect(nativeKey(box, "p", true, false, false))->toBe(true)
    expect(nativeKey(box, "k", true, false, false))->toBe(false)
    expect(mock(sent).calls->Array.map(args => Keys.text(args->Array.getUnsafe(0))))->toEqual([
      "g",
      "W",
      "ctrl+p",
      "ctrl+k",
    ])
    let before = Array.length(mock(sent).calls)
    [
      Screen.getByLabelText("Title"),
      Screen.getByLabelText("Body"),
      Screen.getByLabelText("Repository"),
      select(container, "[contenteditable]"),
    ]->Array.forEach(target => expect(nativeKey(target, "g", false, false, false))->toBe(true))
    expect(nativeKey(box, "g", false, false, true))->toBe(true)
    expect(nativeKey(Screen.getByText("Native action"), "Enter", false, false, false))->toBe(true)
    expect(Array.length(mock(sent).calls))->toBe(before)
  },
)

test(
  "composer custom bindings match their hints and leave removed defaults and composing keys alone",
  () => {
    let dispatch = fn()
    let draft = Fixtures.parse(View.Draft.schema, "client", "Draft", "default")
    let {rerender} = render(
      <Composer draft pendingRefresh=false bindings=custom chrome=custom dispatch />,
    )
    let input = Screen.getByPlaceholderText("Reply…")
    FireEvent.change(input, {"target": {"value": "kept exact\ntext"}})
    expect(Screen.getByText("Submit")->Element.getAttribute("title")->Nullable.getExn)->toBe(
      "submit (alt+s)",
    )
    act(() => {
      expect(nativeKey(input, "Enter", true, false, false))->toBe(true)
      expect(nativeKey(input, "Escape", false, false, false))->toBe(true)
      expect(nativeKey(input, "s", false, true, true))->toBe(true)
    })
    expect(dispatch)->not_->toHaveBeenCalled
    act(() => {
      let _ = nativeKey(input, "s", false, true, false)
    })
    expect(dispatch)->toHaveBeenLastCalledWith(Action.DraftSubmitted({body: "kept exact\ntext"}))
    act(() => {
      let _ = nativeKey(input, "q", false, true, false)
    })
    expect(dispatch)->toHaveBeenLastCalledWith(Action.DraftDiscarded({}))
    let sequence: array<View.Hint.t> = [{keys: "ctrl+k ctrl+s", command: Submit, label: "submit"}]
    rerender(<Composer draft pendingRefresh=false bindings=sequence chrome=sequence dispatch />)
    let before = Array.length(mock(dispatch).calls)
    act(() => {
      expect(nativeKey(input, "k", true, false, false))->toBe(false)
      expect(nativeKey(input, "s", true, false, false))->toBe(false)
    })
    expect(Array.length(mock(dispatch).calls))->toBe(before + 1)
    expect(dispatch)->toHaveBeenLastCalledWith(Action.DraftSubmitted({body: "kept exact\ntext"}))
  },
)

test("search close binding follows the keymap in both input and result zones", () => {
  let dispatch = fn()
  let bindings: array<View.Hint.t> = [{keys: "alt+q", command: Back, label: "back"}]
  let {container} = render(
    <Palette contentSearch=None actionPalette=true chrome=custom bindings dispatch />,
  )
  expect(Screen.getByText("Close")->Element.getAttribute("title")->Nullable.getExn)->toBe(
    "back (alt+q)",
  )
  let input = Screen.getByPlaceholderText("run a command")
  act(() => {
    let _ = nativeKey(input, "Escape", false, false, false)
  })
  expect(dispatch)->not_->toHaveBeenCalled
  act(() => {
    let _ = nativeKey(input, "q", false, true, false)
  })
  expect(dispatch)->toHaveBeenLastCalledWith(Action.ActionPalette({open_: false}))
  let list = select(container, "[role=listbox]")
  act(() => {Element.focus(list)})
  let before = Array.length(mock(dispatch).calls)
  act(() => {
    let _ = nativeKey(list, "q", false, true, true)
  })
  expect(Array.length(mock(dispatch).calls))->toBe(before)
  act(() => {
    let _ = nativeKey(list, "q", false, true, false)
  })
  expect(Array.length(mock(dispatch).calls))->toBe(before + 1)
})

test("tree rows retain repository/path identity when a preceding directory expands", () => {
  let dispatch = fn()
  let dir = (name, expanded, children): View.TreeNode.t => Dir({
    name,
    repoId: "repo",
    path: Some(name),
    expanded,
    changedBelow: 1,
    children,
  })
  let child: View.TreeNode.t = File({
    name: "child.txt",
    repoId: "repo",
    path: "alpha/child.txt",
    change: None,
    viewed: Unviewed,
    open_: false,
    additions: None,
    deletions: None,
    threads: 0,
  })
  let tree = (expanded): View.TreeView.t => {
    breadcrumbs: [],
    search: None,
    roots: [dir("alpha", expanded, [child]), dir("beta", false, [])],
  }
  let element = expanded => <Tree tree={tree(expanded)} focus={Tree({index: 0})} dispatch />
  let {rerender} = render(element(false))
  let beta = Screen.getByText("beta")->Element.parentElement
  FireEvent.mouseDown(beta)
  rerender(element(true))
  expect(Screen.getByText("beta")->Element.parentElement)->toEqual(beta)
  FireEvent.mouseUp(beta)
  FireEvent.click(beta)
  expect(dispatch)->toHaveBeenLastCalledWith(Action.ToggleDir({repoId: "repo", path: Some("beta")}))
  expect(
    mock(dispatch).calls->Array.some(args =>
      args->Array.getUnsafe(0) == Action.RunCommand({command: Open})
    ),
  )->toBe(false)
})
