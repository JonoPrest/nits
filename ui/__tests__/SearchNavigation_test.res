let searchBindings: array<View.Hint.t> = [{keys: "esc", command: Back, label: "back"}]

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
      | Action.SearchFirst(_) => setSelected(_ => 0)
      | Action.SearchStep({delta}) =>
        setSelected(s => Math.Int.min(Math.Int.max(s + delta, 0), Array.length(hits) - 1))
      | _ => ()
      }
    }
    <div>
      <SearchBox bindings=searchBindings search={{query, hits, selected}} dispatch=send />
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
      | Action.SearchFirst(_) => setSearch(s => {...s, selected: 0})
      | Action.SearchStep({delta}) =>
        setSearch(s => {
          ...s,
          selected: Math.Int.min(Math.Int.max(s.selected + delta, 0), Array.length(s.hits) - 1),
        })
      | Action.ContentSearch({query: Some(q)}) => setSearch(s => {...s, query: q, selected: 0})
      | _ => ()
      }
    }
    <Palette
      bindings=searchBindings
      contentSearch=Some(search)
      actionPalette=false
      chrome=hints
      dispatch=send
    />
  }
}
type surface = Files | Content | Actions | Help
let mount = (surface, dispatch) =>
  switch surface {
  | Files => render(<FileHarness dispatch />)
  | Content => render(<ContentHarness dispatch />)
  | Actions =>
    render(
      <Palette
        bindings=searchBindings contentSearch=None actionPalette=true chrome=hints dispatch
      />,
    )
  | Help => render(<HelpOverlay bindings=searchBindings help={help(hints)} dispatch />)
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
  let element = chrome =>
    <Palette bindings=searchBindings contentSearch=None actionPalette=true chrome dispatch />
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
  let element = hints => <HelpOverlay bindings=searchBindings help={help(hints)} dispatch />
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
  let {container, rerender} = render(<SearchBox bindings=searchBindings search=file dispatch />)
  await User.keyboard(user, "{ArrowDown}")
  rerender(
    <SearchBox
      bindings=searchBindings
      search={{...file, hits: files()->Array.slice(~start=0, ~end=1)}}
      dispatch
    />,
  )
  expect(Element.textContent(selected(container)))->toContain("a.rs")
  rerender(<SearchBox bindings=searchBindings search={{...file, hits: []}} dispatch />)
  expect(current())->toEqual(Screen.getByPlaceholderText("file…"))
  cleanup()
  let content = {...contentBase(), hits: contentHits(), pending: false, selected: 2}
  let element = search =>
    <Palette
      bindings=searchBindings contentSearch=Some(search) actionPalette=false chrome=hints dispatch
    />
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
    let element = chrome =>
      <Palette bindings=searchBindings contentSearch=None actionPalette=true chrome dispatch />
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

// Use the real window handler: an isolated dialog cannot reveal a Tab
// that escapes to the shell, where it becomes a pane-focus command.
module ShellKeys = {
  @react.component
  let make = (~onKey, ~children) => {
    React.useEffect0(() => {
      let core: Core.t = {
        dispatch: _ => (),
        key: onKey,
        subscribe: _ => () => (),
        attach: () => (),
      }
      let handler = ev => App.onKeyDown(core, ~onChord=_ => App.Pending.Unbound, ev)
      App.KeyEvent.listen("keydown", handler)
      Some(() => App.KeyEvent.unlisten("keydown", handler))
    })
    children
  }
}

[Actions, Help]->Array.forEach(surface => {
  testAsync(
    "dialog control Tab stays native under the shell keymap: " ++ placeholder(surface),
    async () => {
      let user = User.setup()
      let dispatch = fn()
      let onKey = fn()
      let dialog = switch surface {
      | Help => <HelpOverlay bindings=searchBindings help={help(hints)} dispatch />
      | Files | Content | Actions =>
        <Palette
          bindings=searchBindings contentSearch=None actionPalette=true chrome=hints dispatch
        />
      }
      render(
        <ShellKeys onKey>
          <div>
            dialog
            <UI.Button label="after dialog" onClick={() => ()} />
          </div>
        </ShellKeys>,
      )->ignore
      let results = Screen.getByLabelText(resultLabel(surface))
      await User.keyboard(user, "{ArrowDown}{Tab}")
      switch surface {
      | Actions => {
          expect(Element.textContent(current()))->toBe("Content")
          await User.tab(user)
          expect(Element.textContent(current()))->toBe("Close")
          await User.keyboard(user, "{Shift>}{Tab}{/Shift}")
          expect(Element.textContent(current()))->toBe("Content")
        }
      | Help => expect(Element.textContent(current()))->toBe("Close")
      | Files | Content => ()
      }
      await User.keyboard(user, "{Shift>}{Tab}{/Shift}")
      expect(current())->toEqual(results)
      await User.keyboard(user, "{Tab}")
      if surface == Actions {
        await User.tab(user)
      }
      await User.tab(user)
      expect(Element.textContent(current()))->toBe("after dialog")
      expect(onKey)->not_->toHaveBeenCalled
    },
  )
})

testAsync(
  "content scope keeps native traversal and handles Escape before stopping propagation",
  async () => {
    let user = User.setup()
    let dispatch = fn()
    let onKey = fn()
    render(
      <ShellKeys onKey>
        <ContentHarness dispatch />
      </ShellKeys>,
    )->ignore
    let scope = Screen.getByLabelText("all files (not just changed)")
    await User.keyboard(user, "{ArrowDown}{Tab}")
    expect(current())->toEqual(scope)
    await User.tab(user)
    expect(Element.textContent(current()))->toBe("Actions")
    await User.keyboard(user, "{Shift>}{Tab}{/Shift}")
    expect(current())->toEqual(scope)
    await User.keyboard(user, "{Escape}")
    expect(dispatch)->toHaveBeenLastCalledWith(Action.ContentSearch({query: None, allFiles: false}))
    expect(onKey)->not_->toHaveBeenCalled
  },
)

let coreSearches: array<Action.SearchKind.t> = [Files, Content]
coreSearches->Array.forEach(search => {
  ["{ArrowDown}", "{Tab}"]->Array.forEach(enterResults => {
    let label = switch search {
    | Files => "files"
    | Content => "content"
    }
    testAsync(
      "first selection survives delayed " ++ label ++ " view publication via " ++ enterResults,
      async () => {
        let user = User.setup()
        let coreSelected = ref(0)
        let opened = fn()
        let sent = fn()
        let dispatch = action => {
          sent(action)
          // Model ordered host dispatch without publishing any selection patches
          // back to React. Rust tests exercise these intents in the real core.
          switch action {
          | Action.SearchFirst(_) => coreSelected := 0
          | Action.SearchStep({delta}) => coreSelected := coreSelected.contents + delta
          | Action.OpenSearchResult(_) => opened(coreSelected.contents)
          | _ => ()
          }
        }
        let element = switch search {
        | Files =>
          <SearchBox
            bindings=searchBindings search={{query: "", hits: files(), selected: 0}} dispatch
          />
        | Content =>
          <Palette
            bindings=searchBindings
            contentSearch=Some({
              ...contentBase(),
              query: "",
              hits: contentHits(),
              pending: false,
              selected: 0,
            })
            actionPalette=false
            chrome=hints
            dispatch
          />
        }
        let {container} = render(element)
        let first = selected(container)
        await User.keyboard(user, "{ArrowDown}j")
        expect(coreSelected.contents)->toBe(1)
        expect(selected(container))->toEqual(first)
        await User.keyboard(user, "{Shift>}{Tab}{/Shift}" ++ enterResults ++ "{Enter}")
        expect(opened)->toHaveBeenLastCalledWith(0)
        expect(mock(sent).calls)->toEqual([
          [Action.SearchFirst({search: search})],
          [Action.SearchStep({search, delta: 1})],
          [Action.SearchFirst({search: search})],
          [Action.OpenSearchResult({search, query: ""})],
        ])
      },
    )
  })
})

@module("react") external act: (unit => unit) => unit = "act"
@module("@testing-library/react") @scope("fireEvent")
external pointerDown: element => unit = "pointerDown"
@set external setScrollIntoView: (element, unit => unit) => unit = "scrollIntoView"

[Files, Content, Actions, Help]->Array.forEach(surface => {
  testAsync(
    "pointer entry preserves the clicked result before keyboard scrolling resumes: " ++
    placeholder(surface),
    async () => {
      let dispatch = fn()
      let {container} = mount(surface, dispatch)
      let list = container->Element.querySelector("[role=listbox]")->Nullable.getExn
      let options = list->Element.querySelectorAll("[role=option]")
      let last = options->Array.getUnsafe(2)
      let scroll = fn()
      options->Array.forEach(option => setScrollIntoView(option, () => scroll()))
      pointerDown(last)
      act(() => Element.focus(list))
      expect(scroll)->not_->toHaveBeenCalled
      FireEvent.mouseUp(last)
      FireEvent.click(last)
      switch surface {
      | Files =>
        expect(dispatch)->toHaveBeenLastCalledWith(
          Action.Viewport({file: (files()->Array.getUnsafe(2)).file, firstRow: 0, lastRow: 59}),
        )
      | Content => {
          let hit = contentHits()->Array.getUnsafe(2)
          expect(dispatch)->toHaveBeenLastCalledWith(
            Action.Viewport({
              file: {repoId: hit.repoId, path: hit.path},
              firstRow: Math.Int.max(hit.line - 30, 0),
              lastRow: hit.line + 30,
            }),
          )
        }
      | Actions | Help =>
        expect(dispatch)->toHaveBeenLastCalledWith(Action.RunCommand({command: CopyPath}))
      }
      // Mock dispatch intentionally leaves the dialog open; entering it from
      // the keyboard must still reveal the selected result.
      act(() => Element.focus(Screen.getByPlaceholderText(placeholder(surface))))
      let user = User.setup()
      await User.keyboard(user, "{ArrowDown}")
      expect(scroll)->toHaveBeenCalled
    },
  )
})
