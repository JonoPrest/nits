open Vitest
open TestingLibrary

module User = {
  type t
  @module("@testing-library/user-event") @scope("default") external setup: unit => t = "setup"
  @send external keyboard: (t, string) => promise<unit> = "keyboard"
  @send external type_: (t, element, string) => promise<unit> = "type"
  @send external clear: (t, element) => promise<unit> = "clear"
  @send external tab: t => promise<unit> = "tab"
}

afterEach(cleanup)
let hints: array<View.Hint.t> = [
  {keys: "s", command: ToggleLayout, label: "layout"},
  {keys: "w", command: ToggleWhitespace, label: "whitespace"},
  {keys: "y", command: CopyPath, label: "copy"},
]
let help = (hints: array<View.Hint.t>): View.HelpView.t => {
  groups: [
    {
      context: Diff,
      entries: hints->Array.map((h): View.HelpEntry.t => {
        keys: h.keys,
        command: h.command,
        label: h.label,
        primary: false,
        overridden: false,
      }),
    },
  ],
  conflicts: [],
}
let fileBase = () => Fixtures.parse(View.SearchView.schema, "client", "SearchView", "default")
let contentBase = () =>
  Fixtures.parse(View.ContentSearchView.schema, "client", "ContentSearchView", "default")
let files = () => {
  let base = fileBase()
  let hit = base.hits->Array.getUnsafe(0)
  ["a.rs", "b.rs", "c.rs"]->Array.map(path => {...hit, file: {...hit.file, path}})
}
let contentHits = () => {
  let hit = contentBase().hits->Array.getUnsafe(0)
  [1, 2, 3]->Array.map(line => {...hit, line})
}

module FileHarness = {
  @react.component
  let make = (~dispatch) => {
    let (query, setQuery) = React.useState(() => "")
    let (selected, setSelected) = React.useState(() => 0)
    let hits = files()->Array.filter(hit => hit.file.path->String.includes(query))
    let send = action => {
      dispatch(action)
      switch action {
      | Action.FileSearch({query: Some(q)}) => {
          setQuery(_ => q)
          setSelected(_ => 0)
        }
      | Action.SearchStep({delta}) =>
        setSelected(s => Math.Int.min(Math.Int.max(s + delta, 0), Array.length(hits) - 1))
      | _ => ()
      }
    }
    <div>
      <SearchBox search={{query, hits, selected}} dispatch=send />
      <UI.Button label="after results" onClick={() => ()} />
    </div>
  }
}
module ContentHarness = {
  @react.component
  let make = (~dispatch) => {
    let (search, setSearch) = React.useState(() => {
      ...contentBase(),
      query: "",
      pending: false,
      hits: contentHits(),
      selected: 0,
    })
    let send = action => {
      dispatch(action)
      switch action {
      | Action.SearchStep({delta}) =>
        setSearch(s => {
          ...s,
          selected: Math.Int.min(Math.Int.max(s.selected + delta, 0), Array.length(s.hits) - 1),
        })
      | Action.ContentSearch({query: Some(q)}) => setSearch(s => {...s, query: q, selected: 0})
      | _ => ()
      }
    }
    <Palette contentSearch=Some(search) actionPalette=false chrome=hints dispatch=send />
  }
}
type surface = Files | Content | Actions | Help
let mount = (surface, dispatch) =>
  switch surface {
  | Files => render(<FileHarness dispatch />)
  | Content => render(<ContentHarness dispatch />)
  | Actions => render(<Palette contentSearch=None actionPalette=true chrome=hints dispatch />)
  | Help => render(<HelpOverlay help={help(hints)} dispatch />)
  }
let placeholder = surface =>
  switch surface {
  | Files => "file…"
  | Content => "search file contents (enter)"
  | Actions => "run a command"
  | Help => "filter…"
  }
let resultLabel = surface =>
  switch surface {
  | Files => "files"
  | Content => "content matches"
  | Actions => "actions"
  | Help => "keyboard shortcuts"
  }
let current = () => Document.activeElement->Nullable.toOption->Option.getExn
let selected = container =>
  Element.querySelector(container, "[aria-selected='true']")->Nullable.toOption->Option.getExn

[Files, Content, Actions, Help]->Array.forEach(surface => {
  testAsync("native text and both focus zones: " ++ placeholder(surface), async () => {
    let user = User.setup()
    let dispatch = fn()
    let {container} = mount(surface, dispatch)
    let input = Screen.getByPlaceholderText(placeholder(surface))
    await User.type_(user, input, "jk")
    expect(Element.value(input))->toBe("jk")
    expect(current())->toEqual(input)
    await User.clear(user, input)
    await User.keyboard(user, "{ArrowUp}")
    expect(current())->toEqual(input)
    await User.keyboard(user, "{ArrowDown}")
    let results = Screen.getByLabelText(resultLabel(surface))
    expect(current())->toEqual(results)
    let first = selected(container)
    await User.keyboard(user, "{ArrowDown}")
    expect(selected(container))->not_->toEqual(first)
    await User.keyboard(user, "k")
    expect(selected(container))->toEqual(first)
    await User.keyboard(user, "{ArrowUp}")
    expect(current())->toEqual(input)
    await User.tab(user)
    expect(current())->toEqual(results)
    await User.keyboard(user, "j")
    expect(selected(container))->not_->toEqual(first)
    await User.keyboard(user, "{Shift>}{Tab}{/Shift}")
    expect(current())->toEqual(input)
    await User.tab(user)
    expect(selected(container))->toEqual(first)
    await User.keyboard(user, "j{Enter}")
    switch surface {
    | Files =>
      expect(dispatch)->toHaveBeenLastCalledWith(
        Action.OpenSearchResult({search: Files, query: ""}),
      )
    | Content =>
      expect(dispatch)->toHaveBeenLastCalledWith(
        Action.OpenSearchResult({search: Content, query: ""}),
      )
    | Actions | Help =>
      expect(dispatch)->toHaveBeenLastCalledWith(Action.RunCommand({command: ToggleWhitespace}))
    }
    // Forward Tab uses the DOM's next accessible control, without switching mode.
    await User.tab(user)
    expect(current())->not_->toEqual(results)
    expect(current())->not_->toEqual(input)
  })

  testAsync(
    "result typing returns to input and empty results stay valid: " ++ placeholder(surface),
    async () => {
      let user = User.setup()
      let dispatch = fn()
      let {container} = mount(surface, dispatch)
      let input = Screen.getByPlaceholderText(placeholder(surface))
      await User.keyboard(user, "{ArrowDown}z")
      expect(current())->toEqual(input)
      expect(Element.value(input))->toBe("z")
      expect(Element.querySelector(container, "[aria-selected='true']"))->toBeNull
      await User.keyboard(user, "jk{ArrowDown}")
      expect(current())->toEqual(input)
      expect(Element.value(input))->toBe("zjk")
      await User.keyboard(user, "{Escape}")
      switch surface {
      | Files => expect(dispatch)->toHaveBeenLastCalledWith(Action.FileSearch({query: None}))
      | Content =>
        expect(dispatch)->toHaveBeenLastCalledWith(
          Action.ContentSearch({query: None, allFiles: false}),
        )
      | Actions => expect(dispatch)->toHaveBeenLastCalledWith(Action.ActionPalette({open_: false}))
      | Help => expect(dispatch)->toHaveBeenLastCalledWith(Action.ToggleHelp({}))
      }
    },
  )
})

testAsync("action result replacement clamps selection and activation to the same row", async () => {
  let user = User.setup()
  let dispatch = fn()
  let element = chrome => <Palette contentSearch=None actionPalette=true chrome dispatch />
  let {container, rerender} = render(element(hints))
  await User.keyboard(user, "{ArrowDown}jj")
  rerender(element(hints->Array.slice(~start=0, ~end=2)))
  expect(Element.textContent(selected(container)))->toContain("whitespace")
  await User.keyboard(user, "{Enter}")
  expect(dispatch)->toHaveBeenLastCalledWith(Action.RunCommand({command: ToggleWhitespace}))
  rerender(element([]))
  expect(current())->toEqual(Screen.getByPlaceholderText("run a command"))
  expect(Element.querySelector(container, "[aria-selected='true']"))->toBeNull
})

testAsync("help live replacement preserves valid focus and selection", async () => {
  let user = User.setup()
  let dispatch = fn()
  let element = hints => <HelpOverlay help={help(hints)} dispatch />
  let {container, rerender} = render(element(hints))
  await User.keyboard(user, "{ArrowDown}jj")
  rerender(element(hints->Array.slice(~start=0, ~end=2)))
  await User.keyboard(user, "k")
  expect(Element.textContent(selected(container)))->toContain("layout")
  rerender(element([]))
  expect(current())->toEqual(Screen.getByPlaceholderText("filter…"))
  expect(Element.querySelector(container, "[aria-selected='true']"))->toBeNull
})

testAsync("file and content live replacement never leave focus on a removed result", async () => {
  let user = User.setup()
  let dispatch = fn()
  let file = {...fileBase(), hits: files(), selected: 2}
  let {container, rerender} = render(<SearchBox search=file dispatch />)
  await User.keyboard(user, "{ArrowDown}")
  rerender(<SearchBox search={{...file, hits: files()->Array.slice(~start=0, ~end=1)}} dispatch />)
  expect(Element.textContent(selected(container)))->toContain("a.rs")
  rerender(<SearchBox search={{...file, hits: []}} dispatch />)
  expect(current())->toEqual(Screen.getByPlaceholderText("file…"))
  cleanup()
  let content = {...contentBase(), hits: contentHits(), pending: false, selected: 2}
  let element = search =>
    <Palette contentSearch=Some(search) actionPalette=false chrome=hints dispatch />
  let {container, rerender} = render(element(content))
  await User.keyboard(user, "{ArrowDown}")
  rerender(element({...content, hits: contentHits()->Array.slice(~start=0, ~end=1)}))
  expect(Element.querySelectorAll(container, "[aria-selected='true']")->Array.length)->toBe(1)
  rerender(element({...content, hits: []}))
  expect(current())->toEqual(Screen.getByPlaceholderText("search file contents (enter)"))
})

testAsync("content Enter submits changed text before it can activate results", async () => {
  let user = User.setup()
  let dispatch = fn()
  mount(Content, dispatch)->ignore
  let input = Screen.getByPlaceholderText("search file contents (enter)")
  await User.type_(user, input, "new")
  await User.keyboard(user, "{Enter}")
  expect(dispatch)->toHaveBeenLastCalledWith(
    Action.ContentSearch({query: Some("new"), allFiles: contentBase().allFiles}),
  )
  await User.keyboard(user, "{ArrowDown}{Enter}")
  expect(dispatch)->toHaveBeenLastCalledWith(
    Action.OpenSearchResult({search: Content, query: "new"}),
  )
})

testAsync("typing from results preserves the input caret for continued editing", async () => {
  let user = User.setup()
  let dispatch = fn()
  mount(Help, dispatch)->ignore
  let input = Screen.getByPlaceholderText("filter…")
  await User.type_(user, input, "la")
  SearchNavigation.setSelectionRange(input, 1, 1)
  await User.keyboard(user, "{ArrowDown}yk")
  expect(Element.value(input))->toBe("lyka")
})

testAsync(
  "live results do not steal focus from dialog controls and Escape still closes",
  async () => {
    let user = User.setup()
    let dispatch = fn()
    let element = chrome => <Palette contentSearch=None actionPalette=true chrome dispatch />
    let {rerender} = render(element(hints))
    await User.keyboard(user, "{ArrowDown}{Tab}")
    let control = current()
    expect(Element.textContent(control))->toBe("Content")
    rerender(element([]))
    expect(current())->toEqual(control)
    await User.keyboard(user, "{Escape}")
    expect(dispatch)->toHaveBeenLastCalledWith(Action.ActionPalette({open_: false}))
  },
)
