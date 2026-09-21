let searchBindings: array<View.Hint.t> = [{keys: "esc", command: Back, label: "back"}]

// 4.4 component tests: each Row variant in both layouts, placeholder →
// chunk swap in the diff view, composer state, hint bar and tree.

open Vitest
open TestingLibrary

let editorBindings: array<View.Hint.t> = [
  {keys: "ctrl+enter", command: Submit, label: "submit"},
  {keys: "esc", command: Back, label: "discard"},
]

afterEach(cleanup)

let kebab = (name: string) =>
  name
  ->String.replaceRegExp(/([a-z0-9])([A-Z])/g, "$1-$2")
  ->String.toLowerCase

describe("Row", () => {
  Fixtures.variants("protocol", "Row")->Array.forEach(v => {
    [View.Layout.Unified, Split]->Array.forEach(
      layout => {
        let layoutName = layout == Unified ? "Unified" : "Split"
        test(
          `renders ${v} (${layoutName}) with its semantic class`,
          () => {
            let row = Fixtures.parse(Render.Row.schema, "protocol", "Row", v)
            let {container} = render(
              <Row
                row
                layout
                index=3
                focused={v == "Added"}
                threads={v == "Modified" ? [{thread: "t1", side: Head, place: Anchor}] : []}
              />,
            )
            let el =
              Element.querySelector(container, "[role=\"row\"]")->Nullable.toOption->Option.getExn
            expect(Element.className(el))->toContain("row-" ++ kebab(v))
            expect(Element.className(el))->toContain(layout == Split ? "row-split" : "row-unified")
            expect(Element.getAttribute(el, "data-row-index"))->toEqual(Nullable.make("3"))
            expect(Element.hasAttribute(el, "data-focused"))->toBe(v == "Added")
            if v == "Modified" {
              // The marker hangs on the cell the thread is anchored to,
              // so a base thread never shows against the green half.
              expect(Element.querySelector(el, ".cell-right .cell-threads"))->not_->toBeNull
              expect(Element.querySelector(el, ".cell-left .cell-threads"))->toBeNull
              expect(Element.querySelector(el, ".span-keyword"))->not_->toBeNull
              expect(Element.querySelector(el, ".cell-changed"))->not_->toBeNull
            }

            // Split layout always shows both sides for line rows.
            if ["Context", "Removed", "Added", "Modified"]->Array.includes(v) && layout == Split {
              expect(Element.querySelector(el, ".cell-left"))->not_->toBeNull
              expect(Element.querySelector(el, ".cell-right"))->not_->toBeNull
            }
          },
        )
      },
    )
  })
})

describe("DiffView", () => {
  test("shows the grid, then swaps placeholders for rows when a chunk lands", () => {
    let dispatch = fn()
    let base = Fixtures.parse(View.DiffView.schema, "client", "DiffView", "default")
    let {container, rerender} = render(
      <DiffView diff=base layout=Unified focus={Diff({row: 121, side: Head})} dispatch />,
    )
    // jsdom has no layout, so the virtualizer renders nothing until measured;
    // the grid and its scroll container must still be there.
    expect(Element.querySelector(container, "[role=\"grid\"]"))->not_->toBeNull
    expect(Element.querySelector(container, ".diff-scroll"))->not_->toBeNull
    let filled = {
      ...base,
      missing: [],
      rows: base.rows->Array.concat([{...base.rows->Array.getUnsafe(0), index: 122}]),
    }
    rerender(<DiffView diff=filled layout=Split focus={Diff({row: 122, side: Head})} dispatch />)
    expect(Element.querySelector(container, "[role=\"grid\"]"))->not_->toBeNull
    expect(Screen.getByText("1 file-level thread(s)"))->toBeTruthy
  })
})

describe("DiffSeen.mergeSeen", () => {
  test("rows survive a viewport move and clear on a file change", () => {
    let base = Fixtures.parse(View.DiffView.schema, "client", "DiffView", "default")
    let row = (index: int): View.DiffRow.t => {...base.rows->Array.getUnsafe(0), index}
    let key = DiffSeen.fileKey(base)
    // First window: rows 1 and 2.
    let seen = DiffSeen.mergeSeen(Dict.make(), "", key, [row(1), row(2)])
    // The window moves on: row 3 arrives, rows 1–2 must not regress to
    // placeholders (the flicker while scrolling).
    let seen = DiffSeen.mergeSeen(seen, key, key, [row(3)])
    expect(Array.length(Dict.keysToArray(seen)))->toBe(3)
    // A different file (or a re-render with new totals) starts fresh.
    let other = DiffSeen.fileKey({...base, file: {...base.file, path: "other.rs"}})
    let seen = DiffSeen.mergeSeen(seen, key, other, [row(9)])
    expect(Dict.keysToArray(seen))->toEqual(["9"])
  })
})

describe("InlineThread", () => {
  test("renders the comments, dispatches reply/resolve, hosts the composer", () => {
    let dispatch = fn()
    let thread = Fixtures.parse(View.ThreadView.schema, "client", "ThreadView", "default")
    let {rerender} = render(
      <InlineThread thread focused=false index=3 composer=React.null dispatch />,
    )
    // Every comment body renders inline (design: threads under their row).
    expect(Screen.getByText("This should be a newtype."))->toBeTruthy
    FireEvent.click(Screen.getByText("Reply"))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.ReplyOpened({threadId: thread.id}))
    FireEvent.click(Screen.getByText("Resolve finding"))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.ResolveThread({threadId: thread.id}))
    // Clicking the card focuses the thread by its list index.
    FireEvent.click(Screen.getByText("This should be a newtype."))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.SetFocus({focus: Thread({index: 3})}))
    // While a reply is being written the composer replaces the actions.
    rerender(
      <InlineThread
        thread focused=true index=3 composer={<div> {React.string("the composer")} </div>} dispatch
      />,
    )
    expect(Screen.getByText("the composer"))->toBeTruthy
    expect(Array.length(Screen.queryAllByText("Reply")))->toBe(0)
  })
})

describe("Composer", () => {
  test("submits with ctrl+enter and discards with esc, never leaking keys", () => {
    let dispatch = fn()
    let draft = Fixtures.parse(View.Draft.schema, "client", "Draft", "default")
    let _ = render(<Composer bindings=editorBindings draft pendingRefresh=true dispatch />)
    let box = Screen.getByPlaceholderText("Reply…")
    expect(Screen.getByText("changes pending"))->toBeTruthy
    FireEvent.change(box, {"target": {"value": "  "}})
    FireEvent.keyDown(box, {"key": "Enter", "ctrlKey": true})
    expect(dispatch)->not_->toHaveBeenCalled // blank bodies are not sent
    FireEvent.change(box, {"target": {"value": "looks good"}})
    FireEvent.keyDown(box, {"key": "j", "ctrlKey": false}) // plain text, not a command
    FireEvent.keyDown(box, {"key": "Enter", "ctrlKey": true})
    expect(dispatch)->toHaveBeenCalledWith(Action.DraftSubmitted({body: "looks good"}))
    FireEvent.keyDown(box, {"key": "Escape", "ctrlKey": false})
    expect(dispatch)->toHaveBeenLastCalledWith(Action.DraftDiscarded({}))
  })
})

describe("HintBar", () => {
  test("renders the hints the model carries, and the connection and progress", () => {
    let hint = Fixtures.parse(View.Hint.schema, "client", "Hint", "default")
    let _ = render(
      <HintBar
        hints=[hint]
        connection={Subscribed({})}
        progress={{viewed: 2, changedSinceViewed: 0, total: 5, additions: 0, deletions: 0}}
      />,
    )
    expect(Screen.getByText("] f"))->toBeTruthy
    expect(Screen.getByText("next file"))->toBeTruthy
    expect(Screen.getByText("connected"))->toBeTruthy
    expect(Screen.getByText("2/5 viewed"))->toBeTruthy
  })

  test("switches to the pending-leader style and shows the pending keys", () => {
    let hint: View.Hint.t = {keys: "s", command: ToggleLayout, label: "split layout"}
    let {container} = render(
      <HintBar
        hints=[hint]
        pendingKeys="g"
        connection={Subscribed({})}
        progress={{viewed: 0, changedSinceViewed: 0, total: 0, additions: 0, deletions: 0}}
      />,
    )
    expect(Element.querySelector(container, ".hint-bar-pending"))->not_->toBeNull
    expect(Element.querySelector(container, ".pending-keys"))->not_->toBeNull
    expect(Screen.getByText("g"))->toBeTruthy
    expect(Screen.getByText("split layout"))->toBeTruthy
  })

  test("renders every continuation while the Diff z group is pending", () => {
    let hints: array<View.Hint.t> = [
      {keys: "u", command: ExpandUp, label: "expand up"},
      {keys: "d", command: ExpandDown, label: "expand down"},
      {keys: "c", command: CommentOnFile, label: "comment on file"},
      {keys: "z", command: CenterView, label: "centre the view"},
      {keys: "t", command: ViewTop, label: "row to top"},
      {keys: "b", command: ViewBottom, label: "row to bottom"},
    ]
    let {container} = render(
      <HintBar
        hints
        pendingKeys="z"
        pendingLabel="Expand/scroll"
        focusName="DIFF"
        connection={Subscribed({})}
        progress={{viewed: 0, changedSinceViewed: 0, total: 0, additions: 0, deletions: 0}}
      />,
    )
    expect(Element.querySelector(container, ".hint-bar-pending"))->not_->toBeNull
    expect(Screen.getByText("Expand/scroll"))->toBeTruthy
    [
      "expand up",
      "expand down",
      "comment on file",
      "centre the view",
      "row to top",
      "row to bottom",
    ]->Array.forEach(label => expect(Screen.getByText(label))->toBeTruthy)
    ["u", "d", "c", "t", "b"]->Array.forEach(
      key => expect(Array.length(Screen.queryAllByText(key)))->toBe(1),
    )
  })
})

describe("HelpOverlay", () => {
  test("renders every full Diff z chord plus Global, overrides, and conflicts", () => {
    let entry = (keys, command, label, overridden): View.HelpEntry.t => {
      keys,
      command,
      label,
      primary: false,
      overridden,
    }
    let help: View.HelpView.t = {
      groups: [
        {
          context: Diff,
          entries: [
            entry("z u", ExpandUp, "expand up", true),
            entry("z d", ExpandDown, "expand down", false),
            entry("z c", CommentOnFile, "comment on file", false),
            entry("z z", CenterView, "centre the view", false),
            entry("z t", ViewTop, "row to top", false),
            entry("z b", ViewBottom, "row to bottom", false),
          ],
        },
        {
          context: Global,
          entries: [entry("?", ToggleHelp, "help", false)],
        },
      ],
      conflicts: [{context: Diff, keys: "z u", commands: [ExpandUp, ExpandDown]}],
    }
    let {container} = render(<HelpOverlay bindings=searchBindings help dispatch={_ => ()} />)
    ["z u", "z d", "z c", "z z", "z t", "z b", "?"]->Array.forEach(
      keys => expect(Array.length(Screen.queryAllByText(keys)))->toBe(keys == "z u" ? 2 : 1),
    )
    expect(Screen.getByText("Diff"))->toBeTruthy
    expect(Screen.getByText("Global"))->toBeTruthy
    expect(Element.querySelector(container, ".help-overridden"))->not_->toBeNull
    expect(Element.querySelector(container, ".help-conflicts"))->not_->toBeNull
  })
})

describe("Tree", () => {
  test("flattens expanded dirs in display order and marks the focused node", () => {
    let dispatch = fn()
    let tree = Fixtures.parse(View.TreeView.schema, "client", "TreeView", "default")
    let {container} = render(<Tree tree focus={Tree({index: 2})} dispatch />)
    let items = Element.querySelectorAll(container, "[role=\"treeitem\"]")
    // root, src (expanded), lib.rs, README.md
    expect(Array.length(items))->toBe(4)
    expect(Element.hasAttribute(items->Array.getUnsafe(2), "data-focused"))->toBe(true)
    FireEvent.click(items->Array.getUnsafe(2))
    let calls = mock(dispatch).calls
    // A tree click is the mouse alias of `enter` on a tree file: it runs
    // the same command, so the two cannot drift.
    let opened = calls->Array.some(
      args =>
        switch args->Array.getUnsafe(0) {
        | Action.RunCommand({command: Open}) => true
        | _ => false
        },
    )
    expect(opened)->toBe(true)
    FireEvent.click(items->Array.getUnsafe(1))
    let calls = mock(dispatch).calls
    let focused =
      calls->Array.some(
        args => args->Array.getUnsafe(0) == Action.SetFocus({focus: Tree({index: 1})}),
      )
    expect(focused)->toBe(true)
    switch calls->Array.getUnsafe(Array.length(calls) - 1)->Array.getUnsafe(0) {
    | Action.ToggleDir(_) => ()
    | _ => expect(false)->toBe(true)
    }
  })
})

describe("Stepper", () => {
  test("selects aggregate and commit scopes from the same list", () => {
    let dispatch = fn()
    let stepper = Fixtures.parse(View.CommitStepper.schema, "client", "CommitStepper", "default")
    let {container} = render(
      <Stepper
        stepper scope={Domain.DiffScope.All({})} focus={CommitStepper({index: 0})} dispatch
      />,
    )
    let items = Element.querySelectorAll(container, ".stepper-commit")
    expect(Array.length(items))->toBe(Array.length(stepper.commits) + 2)
    expect(Element.textContent(items->Array.getUnsafe(0)))->toContain("All changes")
    expect(Element.hasAttribute(items->Array.getUnsafe(0), "data-focused"))->toBe(true)
    FireEvent.click(items->Array.getUnsafe(0))
    expect(dispatch)->toHaveBeenCalledWith(
      Action.SetFocus({focus: View.Focus.CommitStepper({index: 0})}),
    )
    expect(dispatch)->toHaveBeenLastCalledWith(Action.SetScope({scope: Action.ScopeChoice.All({})}))
    let commit = stepper.commits->Array.getUnsafe(0)
    FireEvent.click(items->Array.getUnsafe(1))
    expect(dispatch)->toHaveBeenCalledWith(
      Action.SetFocus({focus: View.Focus.CommitStepper({index: 1})}),
    )
    expect(dispatch)->toHaveBeenLastCalledWith(
      Action.SetScope({
        scope: Action.ScopeChoice.Commit({repoId: stepper.repoId, oid: commit.oid}),
      }),
    )
    FireEvent.click(items->Array.getUnsafe(Array.length(items) - 1))
    expect(dispatch)->toHaveBeenLastCalledWith(
      Action.SetScope({scope: Action.ScopeChoice.Worktree({repoId: stepper.repoId})}),
    )
    cleanup()
    let {container} = render(
      <Stepper
        stepper
        scope={Domain.DiffScope.Commit({repoId: stepper.repoId, oid: commit.oid})}
        focus={CommitStepper({index: 1})}
        dispatch
      />,
    )
    let items = Element.querySelectorAll(container, ".stepper-commit")
    expect(Element.className(items->Array.getUnsafe(1)))->toContain("stepper-selected")
    expect(Element.querySelector(container, ".commit-panel .commit-subject"))->not_->toBeNull
    expect(Element.querySelector(container, ".commit-body"))->not_->toBeNull
    expect(Element.querySelector(container, ".commit-panel .commit-oid"))->not_->toBeNull
    cleanup()
    let {container} = render(
      <Stepper
        stepper
        scope={Domain.DiffScope.Worktree({repoId: stepper.repoId})}
        focus={CommitStepper({index: Array.length(stepper.commits) + 1})}
        dispatch
      />,
    )
    let items = Element.querySelectorAll(container, ".stepper-commit")
    expect(Element.className(items->Array.getUnsafe(Array.length(items) - 1)))->toContain(
      "stepper-selected",
    )
  })
})

describe("Threads", () => {
  test("offers to apply a suggestion thread and dispatches ApplySuggestion", () => {
    let dispatch = fn()
    let thread = Fixtures.parse(View.ThreadView.schema, "client", "ThreadView", "default")
    let suggestion = Fixtures.parse(
      View.SuggestionView.schema,
      "client",
      "SuggestionView",
      "default",
    )
    let c = thread.comments->Array.getUnsafe(0)
    let thread = {
      ...thread,
      root: suggestion.record.commentId,
      comments: [{...c, id: suggestion.record.commentId, suggestion: Some(suggestion)}],
    }
    let _ = render(
      <Threads
        title="Threads" threads=[thread] focus={Thread({index: 0})} indexOffset=0 dispatch
      />,
    )
    FireEvent.click(Screen.getByText("Apply suggestion"))
    // The click also bubbles to the row (SetFocus), so not the last call.
    expect(dispatch)->toHaveBeenCalledWith(Action.ApplySuggestion({commentId: thread.root}))
    let plain = {...thread, comments: thread.comments->Array.map(c => {...c, suggestion: None})}
    cleanup()
    let _ = render(
      <Threads title="Threads" threads=[plain] focus={Thread({index: 0})} indexOffset=0 dispatch />,
    )
    expect(Array.length(Screen.queryAllByText("Apply suggestion")))->toBe(0)
  })

  test("a focused thread shows every comment body and a click opens its file", () => {
    let dispatch = fn()
    let thread = Fixtures.parse(View.ThreadView.schema, "client", "ThreadView", "default")
    let {container} = render(
      <Threads
        title="Threads" threads=[thread] focus={Thread({index: 0})} indexOffset=0 dispatch
      />,
    )
    let bodies = Element.querySelectorAll(container, ".thread-body")
    expect(Array.length(bodies))->toBe(Array.length(thread.comments))
    FireEvent.click(Element.querySelector(container, ".thread-item")->Nullable.getExn)
    let opened = mock(dispatch).calls->Array.some(
      args =>
        switch args->Array.getUnsafe(0) {
        | Action.Viewport(_) => true
        | _ => false
        },
    )
    expect(opened)->toBe(true)
    cleanup()
    let _ = render(
      <Threads title="Threads" threads=[thread] focus={Tree({index: 0})} indexOffset=0 dispatch />,
    )
    expect(Array.length(Screen.queryAllByText(thread.summary)))->toBe(1)
  })
})

describe("Palette", () => {
  test("content hits jump to the file; actions run keymap commands", () => {
    let dispatch = fn()
    let cs = Fixtures.parse(View.ContentSearchView.schema, "client", "ContentSearchView", "default")
    let _ = render(
      <Palette
        bindings=searchBindings contentSearch=Some(cs) actionPalette=false chrome=[] dispatch
      />,
    )
    let hit = cs.hits->Array.getUnsafe(0)
    FireEvent.click(
      Screen.getByLabelText(
        RepositoryIdentity.fileText(Unavailable, {repoId: hit.repoId, path: hit.path}) ++
        ":" ++
        Int.toString(hit.line),
      ),
    )
    let calls = mock(dispatch).calls
    let jumped = calls->Array.some(
      args =>
        switch args->Array.getUnsafe(0) {
        | Action.Viewport({file}) => file.path == hit.path
        | _ => false
        },
    )
    expect(jumped)->toBe(true)
    cleanup()
    let chrome: array<View.Hint.t> = [
      {keys: "s", command: ToggleLayout, label: "split/unified"},
      {keys: "w", command: ToggleWhitespace, label: "whitespace"},
    ]
    let _ = render(
      <Palette bindings=searchBindings contentSearch=None actionPalette=true chrome dispatch />,
    )
    FireEvent.click(Screen.getByText("split/unified"))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.RunCommand({command: ToggleLayout}))
  })
})

describe("Context expanders", () => {
  test("the expand-file button dispatches ExpandContext for the whole file", () => {
    let dispatch = fn()
    let base = Fixtures.parse(View.DiffView.schema, "client", "DiffView", "default")
    let _ = render(
      <DiffView diff=base layout=Unified focus={Diff({row: 0, side: Head})} dispatch />,
    )
    FireEvent.click(Screen.getByText("expand file"))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.ExpandContext({file: base.file, full: true}))
  })

  test("expander tooltips come from the keymap, not from the code", () => {
    let chrome: array<View.Hint.t> = [
      {keys: "z u", command: ExpandUp, label: "expand up"},
      {keys: "z d", command: ExpandDown, label: "expand down"},
    ]
    let row = Fixtures.parse(Render.Row.schema, "protocol", "Row", "Expander")
    let {container} = render(
      <Row row layout=Unified index=0 focused=false threads=[] chrome onExpand={(_, _) => ()} />,
    )
    expect(Element.querySelector(container, "[title=\"expand up (z u)\"]"))->not_->toBeNull
    expect(Element.querySelector(container, "[title=\"expand down (z d)\"]"))->not_->toBeNull
    cleanup()
    // Rebound: the tooltip follows.
    let rebound: array<View.Hint.t> = [{keys: "] u", command: ExpandUp, label: "expand up"}]
    let {container} = render(
      <Row
        row layout=Unified index=0 focused=false threads=[] chrome=rebound onExpand={(_, _) => ()}
      />,
    )
    expect(Element.querySelector(container, "[title=\"expand up (] u)\"]"))->not_->toBeNull
  })

  test("an expander opens its own gap, in the direction that was clicked", () => {
    // The fixture expander is `Both` on gap 1: two arrows, each opening
    // that one hidden run rather than re-rendering the whole file.
    let row = Fixtures.parse(Render.Row.schema, "protocol", "Row", "Expander")
    let expand = fn()
    let focus = fn()
    let {container} = render(
      <Row
        row
        layout=Unified
        index=0
        focused=false
        threads=[]
        onClick={side => focus(side)}
        onExpand={(g, d) => expand((g, d))}
      />,
    )
    let arrows = Element.querySelectorAll(container, ".expander-arrow")
    expect(Array.length(arrows))->toBe(2)
    FireEvent.click(arrows->Array.getUnsafe(0))
    expect(expand)->toHaveBeenLastCalledWith((1, Render.ExpandDir.Up))
    FireEvent.click(arrows->Array.getUnsafe(1))
    expect(expand)->toHaveBeenLastCalledWith((1, Render.ExpandDir.Down))
    // Clicking the row itself opens the gap in its own direction.
    FireEvent.click(Screen.getByTextRe(/more lines/))
    expect(expand)->toHaveBeenLastCalledWith((1, Render.ExpandDir.Both))
    expect(focus)->not_->toHaveBeenCalled
  })
})

describe("Jump to original diff", () => {
  test("an outdated thread with context offers the original diff", () => {
    let dispatch = fn()
    let thread = Fixtures.parse(View.ThreadView.schema, "client", "ThreadView", "default")
    let outdated = {
      ...thread,
      outdated: true,
      context: Some(
        Diff({
          change: Fixtures.parse(Domain.ChangeKind.schema, "protocol", "ChangeKind", "Modified"),
        }),
      ),
    }
    let _ = render(
      <Threads
        title="Threads" threads=[outdated] focus={Thread({index: 0})} indexOffset=0 dispatch
      />,
    )
    FireEvent.click(Screen.getByText("Open original diff"))
    expect(dispatch)->toHaveBeenCalledWith(Action.OpenOriginalDiff({threadId: outdated.id}))
    // Clicking the row itself also jumps to the original, not the moved-on diff.
    let calls = mock(dispatch).calls
    let jumped = calls->Array.every(
      args =>
        switch args->Array.getUnsafe(0) {
        | Action.Viewport(_) => false
        | _ => true
        },
    )
    expect(jumped)->toBe(true)
  })

  test("the original diff shows the read-only banner", () => {
    let dispatch = fn()
    let base = Fixtures.parse(View.DiffView.schema, "client", "DiffView", "default")
    let {container} = render(
      <DiffView
        diff={...base, original: true} layout=Unified focus={Diff({row: 0, side: Head})} dispatch
      />,
    )
    expect(Element.querySelector(container, ".original-banner"))->not_->toBeNull
    cleanup()
    let {container} = render(
      <DiffView diff=base layout=Unified focus={Diff({row: 0, side: Head})} dispatch />,
    )
    expect(Element.querySelector(container, ".original-banner"))->toBeNull
  })
})

describe("DiffView (viewed)", () => {
  test("collapses a viewed file until the reader asks to see it", () => {
    let dispatch = fn()
    let base = Fixtures.parse(View.DiffView.schema, "client", "DiffView", "default")
    let viewed = {...base, viewed: Viewed, collapsed: true}
    let {container, rerender} = render(
      <DiffView diff=viewed layout=Unified focus={Diff({row: 121, side: Head})} dispatch />,
    )
    expect(Element.querySelector(container, ".diff-collapsed"))->not_->toBeNull
    expect(Element.querySelector(container, ".diff-scroll.hidden"))->not_->toBeNull
    FireEvent.click(Screen.getByText("show anyway"))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.ToggleFileCollapse({file: viewed.file}))
    rerender(
      <DiffView
        diff={{...viewed, collapsed: false}}
        layout=Unified
        focus={Diff({row: 121, side: Head})}
        dispatch
      />,
    )
    expect(Element.querySelector(container, ".diff-collapsed"))->toBeNull
  })
})

describe("RefSpecText", () => {
  test("parses every ref spec form and prints it back", () => {
    let cases = [
      ("main", Some(Domain.RefSpec.Branch({name: "main"}))),
      ("branch:feature/x", Some(Branch({name: "feature/x"}))),
      ("tag:v1.0", Some(Tag({name: "v1.0"}))),
      ("commit:" ++ String.repeat("a", 40), Some(Commit({oid: String.repeat("a", 40)}))),
      ("commit:abc", None),
      ("worktree", Some(WorkingTree({}))),
      ("HEAD", Some(Head({}))),
      ("upstream", Some(Upstream({}))),
      ("", None),
    ]
    cases->Array.forEach(((text, want)) => expect(RefSpecText.parse(text))->toEqual(want))
    [
      Domain.RefSpec.Branch({name: "main"}),
      Tag({name: "v1"}),
      WorkingTree({}),
      Head({}),
      Upstream({}),
    ]->Array.forEach(
      spec => expect(RefSpecText.parse(RefSpecText.print(spec)))->toEqual(Some(spec)),
    )
  })
})

describe("ReviewList", () => {
  test(
    "groups review target summaries beneath workspace inventory and dispatches the shared create action",
    () => {
      let dispatch = fn()
      let ws = Fixtures.parse(Domain.Workspace.schema, "protocol", "Workspace", "default")
      let review = Fixtures.parse(Domain.Review.schema, "protocol", "Review", "default")
      let other = {...ws, id: "01ARZ3NDEKTSV4RRFFQ69G5FAV", name: "empty-ws"}
      let _ = render(
        <ReviewList
          reviews=[{...review, workspaceId: ws.id}]
          workspaces=[ws, other]
          home={
            rows: [
              {workspaceId: ws.id, kind: Workspace({})},
              {workspaceId: ws.id, kind: Review({reviewId: review.id})},
              {workspaceId: other.id, kind: Workspace({})},
            ],
            selectedWorkspace: Some(ws.id),
            expanded: [],
            creating: None,
          }
          chrome=[{keys: "N", command: NewReview, label: "new review"}]
          focus={ReviewList({index: 0})}
          dispatch
        />,
      )
      let group = Screen.getByLabelText(ws.name)
      expect(Array.length(Element.querySelectorAll(group, ".review-item")))->toBe(1)
      let emptyGroup = Screen.getByLabelText("empty-ws")
      expect(Array.length(Element.querySelectorAll(emptyGroup, ".review-item")))->toBe(0)
      FireEvent.click(Screen.getByLabelText("New review in empty-ws"))
      expect(dispatch)->toHaveBeenLastCalledWith(Action.StartReview({workspaceId: other.id}))
    },
  )
})

describe("ReviewHeader", () => {
  test("shows base → head for the open review, nothing otherwise", () => {
    let review = Fixtures.parse(Domain.Review.schema, "protocol", "Review", "default")
    let ws = Fixtures.parse(Domain.Workspace.schema, "protocol", "Workspace", "default")
    let resolved = Fixtures.parse(
      Domain.ResolvedTarget.schema,
      "protocol",
      "ResolvedTarget",
      "default",
    )
    let prefs = View.ViewModel.empty.prefs
    let {container} = render(
      <ReviewHeader
        reviews=[review] workspaces=[ws] resolvedTargets=[resolved] openReview=Some(review.id) prefs
      />,
    )
    expect(Array.length(Screen.queryAllByText("Base")))->toBe(Array.length(review.targets))
    expect(Array.length(Screen.queryAllByText("Head")))->toBe(Array.length(review.targets))
    expect(Array.length(Element.querySelectorAll(container, ".review-header-target .btn")))->toBe(
      2 * Array.length(review.targets),
    )
    cleanup()
    let {container: closed} = render(
      <ReviewHeader reviews=[review] workspaces=[ws] resolvedTargets=[] openReview=None prefs />,
    )
    expect(Element.querySelector(closed, ".review-header"))->toBeNull
  })

  test("renders a minimal closed header and a keymap-described settings menu", () => {
    let dispatch = fn()
    let review = Fixtures.parse(Domain.Review.schema, "protocol", "Review", "default")
    let ws = Fixtures.parse(Domain.Workspace.schema, "protocol", "Workspace", "default")
    let prefs = View.ViewModel.empty.prefs
    let chrome: array<View.Hint.t> = [
      {keys: "g s", command: ToggleLayout, label: "split layout"},
      {keys: "g h", command: ToggleWhitespace, label: "hide whitespace"},
    ]
    let {container} = render(
      <ReviewHeader
        reviews=[review]
        workspaces=[ws]
        resolvedTargets=[]
        openReview=Some(review.id)
        prefs
        chrome
        dispatch
      />,
    )
    let trigger = Screen.getByLabelText("Diff settings")
    expect(Element.getAttribute(trigger, "aria-controls"))->toEqual(
      Nullable.make("diff-settings-menu"),
    )
    expect(Element.getAttribute(trigger, "aria-haspopup"))->toEqual(Nullable.make("menu"))
    expect(Element.getAttribute(trigger, "aria-expanded"))->toEqual(Nullable.make("false"))
    expect(Element.querySelector(container, "[role=\"menu\"]"))->toBeNull
    expect(Element.querySelector(container, ".segmented"))->toBeNull
    expect(Array.length(Screen.queryAllByText("Unified")))->toBe(0)
    expect(Array.length(Screen.queryAllByText("hide whitespace")))->toBe(0)
    expect(Element.innerHTML(container))->toMatchSnapshot("minimal closed review header")

    FireEvent.keyDown(trigger, {"key": "Enter", "ctrlKey": false})
    expect(Element.getAttribute(trigger, "aria-expanded"))->toEqual(Nullable.make("true"))
    let unified = Screen.getByLabelText("Unified")
    let split = Screen.getByLabelText("Split")
    let show = Screen.getByLabelText("Show")
    expect(Document.activeElement)->toEqual(Nullable.make(unified))
    expect(Element.getAttribute(unified, "aria-checked"))->toEqual(Nullable.make("true"))
    expect(Element.getAttribute(split, "aria-checked"))->toEqual(Nullable.make("false"))
    expect(Element.getAttribute(show, "aria-checked"))->toEqual(Nullable.make("true"))
    expect(Element.getAttribute(split, "title"))->toEqual(Nullable.make("split layout (g s)"))
    expect(Element.textContent(split))->toContain("g s")
    expect(Element.getAttribute(Screen.getByLabelText("Hide"), "title"))->toEqual(
      Nullable.make("hide whitespace (g h)"),
    )
    expect(Element.textContent(Screen.getByLabelText("Hide")))->toContain("g h")
    expect(Element.innerHTML(container))->toMatchSnapshot("open diff settings menu")
  })

  test("supports mouse and roving-keyboard selection, Esc, and outside click", () => {
    let dispatch = fn()
    let review = Fixtures.parse(Domain.Review.schema, "protocol", "Review", "default")
    let ws = Fixtures.parse(Domain.Workspace.schema, "protocol", "Workspace", "default")
    let prefs = View.ViewModel.empty.prefs
    let {container} = render(
      <ReviewHeader
        reviews=[review]
        workspaces=[ws]
        resolvedTargets=[]
        openReview=Some(review.id)
        prefs
        dispatch
      />,
    )
    let trigger = Screen.getByLabelText("Diff settings")
    FireEvent.click(trigger)
    let unified = Screen.getByLabelText("Unified")
    FireEvent.keyDown(unified, {"key": "ArrowDown", "ctrlKey": false})
    let split = Screen.getByLabelText("Split")
    expect(Document.activeElement)->toEqual(Nullable.make(split))
    FireEvent.keyDown(split, {"key": "Enter", "ctrlKey": false})
    expect(dispatch)->toHaveBeenLastCalledWith(Action.SetLayout({layout: Split}))
    expect(Element.querySelector(container, "[role=\"menu\"]"))->toBeNull
    expect(Document.activeElement)->toEqual(Nullable.make(trigger))

    // Space opens the trigger once; ArrowUp wraps to the last option and
    // Space selects it without leaking either key to the shell keymap.
    FireEvent.keyDown(trigger, {"key": " ", "ctrlKey": false})
    let unified = Screen.getByLabelText("Unified")
    FireEvent.keyDown(unified, {"key": "ArrowUp", "ctrlKey": false})
    let hide = Screen.getByLabelText("Hide")
    expect(Document.activeElement)->toEqual(Nullable.make(hide))
    FireEvent.keyDown(hide, {"key": " ", "ctrlKey": false})
    expect(dispatch)->toHaveBeenLastCalledWith(
      Action.SetRenderOpts({ignoreWhitespace: true, contextLines: prefs.contextLines}),
    )
    expect(Document.activeElement)->toEqual(Nullable.make(trigger))

    FireEvent.click(trigger)
    FireEvent.keyDown(Screen.getByLabelText("Unified"), {"key": "Escape", "ctrlKey": false})
    expect(Element.querySelector(container, "[role=\"menu\"]"))->toBeNull
    expect(Document.activeElement)->toEqual(Nullable.make(trigger))

    FireEvent.click(trigger)
    let target = review.targets->Array.getUnsafe(0)
    let outside = Screen.getByText(RefSpecText.print(target.base) ++ " ▾")
    Element.focus(outside)
    FireEvent.click(outside)
    expect(Element.querySelector(container, "[role=\"menu\"]"))->toBeNull
    expect(Document.activeElement)->toEqual(Nullable.make(trigger))

    FireEvent.click(trigger)
    FireEvent.click(Screen.getByLabelText("Split"))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.SetLayout({layout: Split}))
    expect(Document.activeElement)->toEqual(Nullable.make(trigger))

    FireEvent.click(trigger)
    FireEvent.keyDown(Screen.getByLabelText("Unified"), {"key": "Tab", "ctrlKey": false})
    expect(Element.querySelector(container, "[role=\"menu\"]"))->toBeNull

    FireEvent.click(trigger)
    FireEvent.keyDownWithShift(
      Screen.getByLabelText("Unified"),
      {
        "key": "Tab",
        "ctrlKey": false,
        "shiftKey": true,
      },
    )
    expect(Element.querySelector(container, "[role=\"menu\"]"))->toBeNull
  })

  test("reflects preference changes made by shortcuts while it is open", () => {
    let dispatch = fn()
    let review = Fixtures.parse(Domain.Review.schema, "protocol", "Review", "default")
    let ws = Fixtures.parse(Domain.Workspace.schema, "protocol", "Workspace", "default")
    let prefs = View.ViewModel.empty.prefs
    let view = prefs =>
      <ReviewHeader
        reviews=[review]
        workspaces=[ws]
        resolvedTargets=[]
        openReview=Some(review.id)
        prefs
        dispatch
      />
    let {rerender} = render(view(prefs))
    FireEvent.click(Screen.getByLabelText("Diff settings"))
    rerender(view({...prefs, layout: Split, ignoreWhitespace: true}))
    expect(Element.getAttribute(Screen.getByLabelText("Unified"), "aria-checked"))->toEqual(
      Nullable.make("false"),
    )
    expect(Element.getAttribute(Screen.getByLabelText("Split"), "aria-checked"))->toEqual(
      Nullable.make("true"),
    )
    expect(Element.getAttribute(Screen.getByLabelText("Show"), "aria-checked"))->toEqual(
      Nullable.make("false"),
    )
    expect(Element.getAttribute(Screen.getByLabelText("Hide"), "aria-checked"))->toEqual(
      Nullable.make("true"),
    )
  })

  test("base and head buttons open the selector for the exact repo", () => {
    let dispatch = fn()
    let review = Fixtures.parse(Domain.Review.schema, "protocol", "Review", "default")
    let ws = Fixtures.parse(Domain.Workspace.schema, "protocol", "Workspace", "default")
    let _ = render(
      <ReviewHeader
        reviews=[review]
        workspaces=[ws]
        resolvedTargets=[]
        openReview=Some(review.id)
        prefs=View.ViewModel.empty.prefs
        dispatch
      />,
    )
    let target = review.targets->Array.getUnsafe(0)
    FireEvent.click(Screen.getByText(RefSpecText.print(target.base) ++ " ▾"))
    expect(dispatch)->toHaveBeenLastCalledWith(
      Action.OpenRefSelector({repoId: target.repoId, side: Base}),
    )
  })
})

describe("RefSelector", () => {
  test("shows kinds/current state and dispatches keyboard navigation", () => {
    let dispatch = fn()
    let selector = Fixtures.parse(
      View.RefSelectorView.schema,
      "client",
      "RefSelectorView",
      "default",
    )
    let {rerender} = render(<RefSelector bindings=searchBindings selector dispatch />)
    let input = Screen.getByPlaceholderText("Find a head revision")
    expect(Document.activeElement)->toEqual(Nullable.make(input))
    let _ = Screen.getByText("branch")
    let current = Screen.getByTextRe(/current/)
    FireEvent.click(current)
    expect(dispatch)->toHaveBeenLastCalledWith(Action.SelectRef({index: 0}))
    FireEvent.change(input, {"target": {"value": "feature"}})
    expect(dispatch)->toHaveBeenLastCalledWith(Action.RefSelectorQuery({query: "feature"}))
    let callsBefore = mock(dispatch).calls->Array.length
    FireEvent.keyDown(input, {"key": "j", "ctrlKey": false})
    expect(mock(dispatch).calls->Array.length)->toBe(callsBefore)
    rerender(
      <RefSelector bindings=searchBindings selector={...selector, query: "feature"} dispatch />,
    )
    FireEvent.keyDown(input, {"key": "ArrowDown", "ctrlKey": false})
    expect(dispatch)->toHaveBeenLastCalledWith(Action.RefSelectorStep({delta: 0}))
    let results = Screen.getByLabelText("revisions")
    expect(Document.activeElement)->toEqual(Nullable.make(results))
    FireEvent.keyDown(results, {"key": "j", "ctrlKey": false})
    expect(dispatch)->toHaveBeenLastCalledWith(Action.RefSelectorStep({delta: 1}))
    FireEvent.keyDown(results, {"key": "ArrowUp", "ctrlKey": false})
    expect(Document.activeElement)->toEqual(Nullable.make(input))
    FireEvent.keyDown(input, {"key": "Enter", "ctrlKey": false})
    expect(dispatch)->toHaveBeenLastCalledWith(Action.SelectCurrentRef({}))
    FireEvent.keyDown(input, {"key": "Escape", "ctrlKey": false})
    expect(dispatch)->toHaveBeenLastCalledWith(Action.CloseRefSelector({}))
  })

  test("renders loading, empty, invalid-ref, and daemon-error states", () => {
    let selector = Fixtures.parse(
      View.RefSelectorView.schema,
      "client",
      "RefSelectorView",
      "default",
    )
    let dispatch = fn()
    let {rerender} = render(
      <RefSelector bindings=searchBindings selector={...selector, status: Loading({})} dispatch />,
    )
    let _ = Screen.getByTextRe(/Loading branches/)
    rerender(
      <RefSelector
        bindings=searchBindings selector={...selector, options: [], status: Ready({})} dispatch
      />,
    )
    let _ = Screen.getByText("No matching refs")
    rerender(
      <RefSelector
        bindings=searchBindings
        selector={...selector, status: InvalidRef({message: "missing"})}
        dispatch
      />,
    )
    let _ = Screen.getByText("Invalid ref: missing")
    rerender(
      <RefSelector
        bindings=searchBindings
        selector={...selector, status: DaemonError({message: "offline"})}
        dispatch
      />,
    )
    let _ = Screen.getByText("Daemon error: offline")
  })
})

describe("ReviewHeader scope", () => {
  test("keeps the working-tree toggle without a redundant scope selector", () => {
    let dispatch = fn()
    let review = Fixtures.parse(Domain.Review.schema, "protocol", "Review", "default")
    let ws = Fixtures.parse(Domain.Workspace.schema, "protocol", "Workspace", "default")
    let prefs = View.ViewModel.empty.prefs
    let render_ = scope =>
      render(
        <ReviewHeader
          reviews=[review]
          workspaces=[ws]
          resolvedTargets=[]
          openReview=Some(review.id)
          prefs
          scope
          dispatch
        />,
      )
    let {container} = render_(Domain.DiffScope.All({}))
    expect(Element.querySelector(container, "[aria-label=\"diff scope\"]"))->toBeNull
    expect(Array.length(Screen.queryAllByText("All changes")))->toBe(0)
    expect(Array.length(Screen.queryAllByText("By commit")))->toBe(0)
    expect(Element.hasAttribute(Screen.getByText("+ working tree"), "data-active"))->toBe(true)
    FireEvent.click(Screen.getByText("+ working tree"))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.SetScope({scope: Committed({})}))
    cleanup()
    let _ = render_(Domain.DiffScope.Committed({}))
    expect(Element.hasAttribute(Screen.getByText("+ working tree"), "data-active"))->toBe(false)
  })
})

describe("Tabs", () => {
  test("marks the active tab, shows counts, dispatches SetTab on click", () => {
    let dispatch = fn()
    let chrome: array<View.Hint.t> = [{keys: "2", command: TabConversation, label: "conversation"}]
    let {container} = render(<Tabs tab=FilesChanged fileCount=4 threadCount=2 chrome dispatch />)
    let tabs = Element.querySelectorAll(container, "[role=\"tab\"]")
    expect(Array.length(tabs))->toBe(3)
    expect(Element.hasAttribute(tabs->Array.getUnsafe(0), "data-active"))->toBe(true)
    expect(Element.hasAttribute(tabs->Array.getUnsafe(1), "data-active"))->toBe(false)
    expect(Element.getAttribute(tabs->Array.getUnsafe(1), "title"))->toEqual(
      Nullable.make("conversation (2)"),
    )
    FireEvent.click(tabs->Array.getUnsafe(1))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.SetTab({tab: Conversation}))
    FireEvent.click(tabs->Array.getUnsafe(2))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.SetTab({tab: Browse}))
  })
})

describe("HelpOverlay", () => {
  let entry = (~keys, ~command, ~label): View.HelpEntry.t => {
    keys,
    command,
    label,
    primary: false,
    overridden: false,
  }
  let help: View.HelpView.t = {
    groups: [
      {
        context: Diff,
        entries: [
          entry(~keys="u z", ~command=ExpandUp, ~label="zoom utility"),
          entry(~keys="z u", ~command=ExpandUp, ~label="expand up"),
          entry(~keys="z d", ~command=ExpandDown, ~label="expand down"),
        ],
      },
      {
        context: Tree,
        entries: [entry(~keys="s", ~command=ToggleLayout, ~label="toggle layout")],
      },
    ],
    conflicts: [],
  }

  test("autofocuses and fuzzy-ranks normalized chords without empty groups", () => {
    let dispatch = fn()
    let {container} = render(<HelpOverlay bindings=searchBindings help dispatch />)
    let input = Screen.getByPlaceholderText("filter…")
    expect(Document.activeElement->Nullable.toOption)->toEqual(Some(input))

    FireEvent.change(input, {"target": {"value": "zu"}})
    let rows = Element.querySelectorAll(container, ".help-group tbody tr")
    expect(Array.length(rows))->toBe(2)
    expect(Element.textContent(rows->Array.getUnsafe(0)))->toContain("z u")
    expect(Element.querySelectorAll(container, ".help-group")->Array.length)->toBe(1)

    // Sparse subsequences match labels too, not only literal substrings.
    FireEvent.change(input, {"target": {"value": "epu"}})
    expect(Screen.getByText("expand up"))->toBeTruthy

    FireEvent.change(input, {"target": {"value": "nothing"}})
    expect(Screen.getByText("no shortcuts match"))->toBeTruthy
    expect(Element.querySelectorAll(container, ".help-group")->Array.length)->toBe(0)

    let panel = Element.querySelector(container, ".help-panel")->Nullable.toOption->Option.getExn
    Element.setScrollTop(panel, 200)
    FireEvent.change(input, {"target": {"value": ""}})
    expect(Element.querySelectorAll(container, ".help-group tbody tr")->Array.length)->toBe(4)
    expect(Element.querySelectorAll(container, ".help-group")->Array.length)->toBe(2)
    expect(Element.scrollTop(panel))->toBe(0)
  })

  test("Escape closes help from the focused search input", () => {
    let dispatch = fn()
    render(<HelpOverlay bindings=searchBindings help dispatch />)->ignore
    let input = Screen.getByPlaceholderText("filter…")
    FireEvent.keyDown(input, {"key": "Escape", "ctrlKey": false})
    expect(dispatch)->toHaveBeenCalledWith(Action.ToggleHelp({}))
  })
})

describe("Tree (rows)", () => {
  test("rows show line stats and thread badges, never a checkbox", () => {
    let dispatch = fn()
    let tree = Fixtures.parse(View.TreeView.schema, "client", "TreeView", "default")
    let {container} = render(<Tree tree focus={Tree({index: 0})} dispatch />)
    // The design has no checkbox in the tree (`v` toggles viewed).
    expect(Element.querySelectorAll(container, "input")->Array.length)->toBe(0)
    // lib.rs carries +9 −1 and a 2-thread badge (from the fixture).
    expect(Screen.getByText("+9"))->toBeTruthy
    expect(Screen.getByText("−1"))->toBeTruthy
    let badge = Element.querySelector(container, ".tree-threads")->Nullable.toOption->Option.getExn
    expect(Element.textContent(badge))->toBe("2")
    // Clicking a file focuses it and runs `Open` — the chord's own
    // decision, including where in an already-open file to land.
    FireEvent.click(Screen.getByText("lib.rs"))
    let calls = mock(dispatch).calls
    let last = calls->Array.getUnsafe(Array.length(calls) - 1)->Array.getUnsafe(0)
    let focused = calls->Array.getUnsafe(Array.length(calls) - 2)->Array.getUnsafe(0)
    expect(last)->toEqual(Action.RunCommand({command: Open}))
    switch focused {
    | Action.SetFocus({focus: Tree({index})}) => expect(index)->toBe(2)
    | _ => expect(false)->toBe(true)
    }
  })
})

// jsdom has no layout, so the stacked view's scroll-into-view is a no-op
// here; without the stub mounting an open file throws.
%%raw(`
if (!globalThis.Element.prototype.scrollIntoView) {
  globalThis.Element.prototype.scrollIntoView = function () {}
}
`)

module SideFocusHarness = {
  @react.component
  let make = (~diff: View.DiffView.t) => {
    let (focus, setFocus) = React.useState((): View.Focus.t => Diff({row: 121, side: Head}))
    let dispatch = action =>
      switch action {
      | Action.SetFocus({focus}) => setFocus(_ => focus)
      | _ => ()
      }
    <FileDiff
      diff layout=Split focus threads=[] draft=None pendingRefresh=false isOpen=true dispatch
    />
  }
}

describe("Row sides", () => {
  // The fixture row is modified (two cells) and carries a base-anchored
  // thread; a removed row below it gives the drag a second base line.
  let atLine = (row: Render.Row.t, n: int): Render.Row.t =>
    switch row {
    | Modified({left, right}) =>
      Modified({left: {...left, lineNo: n}, right: {...right, lineNo: n}})
    | Context({left, right}) => Context({left: {...left, lineNo: n}, right: {...right, lineNo: n}})
    | Removed({left}) => Removed({left: {...left, lineNo: n}})
    | Added({right}) => Added({right: {...right, lineNo: n}})
    | HunkHeader(_) | Expander(_) | WhitespaceOnly(_) => row
    }

  let diff = (): View.DiffView.t => {
    let base = Fixtures.parse(View.DiffView.schema, "client", "DiffView", "default")
    let modified = Fixtures.parse(View.DiffRow.schema, "client", "DiffRow", "default")
    let removed = Fixtures.parse(Render.Row.schema, "protocol", "Row", "Removed")
    {
      ...base,
      firstRow: 121,
      lastRow: 122,
      missing: [],
      collapsed: false,
      rows: [
        {...modified, index: 121, row: atLine(modified.row, 9)},
        {index: 122, row: atLine(removed, 10), threads: [], drafted: None},
      ],
    }
  }

  let mount = (~diff as d, ~dispatch) =>
    render(
      <FileDiff
        diff=d
        layout=Split
        focus={Diff({row: 121, side: Head})}
        threads=[]
        draft=None
        pendingRefresh=false
        isOpen=true
        dispatch
      />,
    )

  test("a base-anchored thread hangs on the removed cell, not the added one", () => {
    let {container} = mount(~diff=diff(), ~dispatch=fn())
    let row =
      Element.querySelector(container, "[data-row-index=\"121\"]")
      ->Nullable.toOption
      ->Option.getExn
    expect(Element.querySelector(row, ".cell-left .cell-threads"))->not_->toBeNull
    expect(Element.querySelector(row, ".cell-right .cell-threads"))->toBeNull
  })

  test("clicking a cell focuses that half of the row", () => {
    let dispatch = fn()
    let {container} = mount(~diff=diff(), ~dispatch)
    let row =
      Element.querySelector(container, "[data-row-index=\"121\"]")
      ->Nullable.toOption
      ->Option.getExn
    let left = Element.querySelector(row, ".cell-left")->Nullable.toOption->Option.getExn
    FireEvent.click(left)
    expect(dispatch)->toHaveBeenLastCalledWith(
      Action.SetFocus({focus: Diff({row: 121, side: Base})}),
    )
    let right = Element.querySelector(row, ".cell-right")->Nullable.toOption->Option.getExn
    FireEvent.click(right)
    expect(dispatch)->toHaveBeenLastCalledWith(
      Action.SetFocus({focus: Diff({row: 121, side: Head})}),
    )
  })

  test("split-mode focus styling follows side-selection interactions", () => {
    let {container} = render(<SideFocusHarness diff={diff()} />)
    let row = () =>
      Element.querySelector(container, "[data-row-index=\"121\"]")
      ->Nullable.toOption
      ->Option.getExn
    let left = () => Element.querySelector(row(), ".cell-left")->Nullable.toOption->Option.getExn
    let right = () => Element.querySelector(row(), ".cell-right")->Nullable.toOption->Option.getExn

    expect(Element.className(right()))->toContain("cell-focused")
    expect(Element.className(left()))->not_->toContain("cell-focused")
    FireEvent.click(left())
    expect(Element.className(left()))->toContain("cell-focused")
    expect(Element.className(right()))->not_->toContain("cell-focused")
    expect(Element.getAttribute(row(), "data-side"))->toEqual(Nullable.make("Base"))

    FireEvent.click(right())
    expect(Element.className(right()))->toContain("cell-focused")
    expect(Element.className(left()))->not_->toContain("cell-focused")
    expect(Element.getAttribute(row(), "data-side"))->toEqual(Nullable.make("Head"))
  })

  test("dragging down the removed side comments on base lines", () => {
    let dispatch = fn()
    let {container} = mount(~diff=diff(), ~dispatch)
    let leftOf = (index: int) =>
      Element.querySelector(container, "[data-row-index=\"" ++ Int.toString(index) ++ "\"]")
      ->Nullable.toOption
      ->Option.getExn
      ->Element.querySelector(".cell-left")
      ->Nullable.toOption
      ->Option.getExn
    // Starting on the red half of a modified row and crossing into a
    // removed row: the drag keeps growing (it used to stop dead).
    FireEvent.mouseDown(leftOf(121))
    FireEvent.mouseEnter(leftOf(122))
    FireEvent.mouseUp(leftOf(122))
    let calls = mock(dispatch).calls
    let commented = calls->Array.some(
      args =>
        switch args->Array.getUnsafe(0) {
        | Action.CommentLines({side, startLine, endLine}) =>
          side == Base && startLine == 9 && endLine == 10
        | _ => false
        },
    )
    expect(commented)->toBe(true)
  })
})

describe("Scroll.delta", () => {
  // A 400px viewport of 20px rows, nothing painted over the top.
  let container: Scroll.box = {top: 0., bottom: 400., height: 400.}
  let row = (top: float): Scroll.box => {top, bottom: top +. 20., height: 20.}
  let d = (~row as r, ~mode, ~headroom=0.) =>
    Scroll.delta(~container, ~row=r, ~headroom, ~margin=Scroll.scrolloff *. 20., ~mode)

  test("a row well inside the viewport does not scroll", () => {
    expect(d(~row=row(200.), ~mode=Nearest))->toBe(0.)
  })

  test("a row inside the scrolloff margin pushes the view by the shortfall", () => {
    // 3 rows of margin = 60px: a row at 40 is 20px too high, one whose
    // bottom is at 360 is exactly at the limit.
    expect(d(~row=row(40.), ~mode=Nearest))->toBe(-20.)
    expect(d(~row=row(320.), ~mode=Nearest))->toBe(0.)
    expect(d(~row=row(340.), ~mode=Nearest))->toBe(20.)
  })

  test("a row below the fold scrolls just far enough to clear the margin", () => {
    expect(d(~row=row(1000.), ~mode=Nearest))->toBe(1020. -. 340.)
  })

  test("the sticky header is headroom the row must clear", () => {
    // With a 24px header the top limit is 24 + 60 = 84.
    expect(d(~row=row(80.), ~mode=Nearest, ~headroom=24.))->toBe(-4.)
  })

  test("zt/zz/zb put the row at the top, middle and bottom", () => {
    expect(d(~row=row(200.), ~mode=Align(Top)))->toBe(200.)
    expect(d(~row=row(200.), ~mode=Align(Center)))->toBe(200. -. 190.)
    expect(d(~row=row(200.), ~mode=Align(Bottom)))->toBe(220. -. 400.)
    // `z t` on a row inside a file section leaves its sticky header room.
    expect(d(~row=row(200.), ~mode=Align(Top), ~headroom=24.))->toBe(176.)
  })

  test("a viewport too short for the margin still shows the row", () => {
    let tiny: Scroll.box = {top: 0., bottom: 30., height: 30.}
    let delta = Scroll.delta(
      ~container=tiny,
      ~row=row(100.),
      ~headroom=0.,
      ~margin=60.,
      ~mode=Nearest,
    )
    expect(delta)->toBe(120. -. 25.)
  })
})

/// React 19's `act`, so a model pushed outside an event is flushed.
@module("react") external act: (unit => unit) => unit = "act"

module AnchorGeometry = {
  type t = {mutable rowTop: float, mutable rowHeight: float}

  // jsdom does not lay elements out. Model one real scroll container so
  // the test observes viewport-relative pixels, including fractional and
  // variable-height geometry, rather than merely testing row arithmetic.
  let install: (Dom.element, Dom.element, t) => unit = %raw(`
    (container, row, geometry) => {
      Object.defineProperties(container, {
        clientHeight: {configurable: true, value: 400},
        scrollHeight: {configurable: true, value: 2000},
        getBoundingClientRect: {
          configurable: true,
          value: () => ({top: 0, bottom: 400, height: 400})
        }
      })
      row.getBoundingClientRect = () => {
        const top = geometry.rowTop - container.scrollTop
        return {top, bottom: top + geometry.rowHeight, height: geometry.rowHeight}
      }
    }
  `)

  // Install geometry for the real Shell/FileDiff lifecycle. The matcher is
  // prototype-based so it also covers the replacement scroller and row that
  // mount after the transient loading patch removes the originals.
  let installShell: t => unit => unit = %raw(`geometry => {
    const proto = globalThis.HTMLElement.prototype
    const clientHeight = Object.getOwnPropertyDescriptor(proto, "clientHeight")
    const scrollHeight = Object.getOwnPropertyDescriptor(proto, "scrollHeight")
    const rect = proto.getBoundingClientRect
    Object.defineProperties(proto, {
      clientHeight: {
        configurable: true,
        get() {
          if (this.classList?.contains("diff-stack")) return 400
          return clientHeight?.get?.call(this) ?? 0
        },
      },
      scrollHeight: {
        configurable: true,
        get() {
          if (this.classList?.contains("diff-stack")) return 2000
          return scrollHeight?.get?.call(this) ?? 0
        },
      },
    })
    proto.getBoundingClientRect = function() {
      if (this.classList?.contains("diff-stack")) {
        return {top: 0, bottom: 400, height: 400}
      }
      if (this.hasAttribute?.("data-scroll-anchor")) {
        const container = this.closest(".diff-stack")
        const top = geometry.rowTop - (container?.scrollTop ?? 0)
        return {top, bottom: top + geometry.rowHeight, height: geometry.rowHeight}
      }
      return rect.call(this)
    }
    return () => {
      if (clientHeight) Object.defineProperty(proto, "clientHeight", clientHeight)
      else delete proto.clientHeight
      if (scrollHeight) Object.defineProperty(proto, "scrollHeight", scrollHeight)
      else delete proto.scrollHeight
      proto.getBoundingClientRect = rect
    }
  }
  `)
}

describe("Scroll stable line anchor", () => {
  let mount = () => {
    let key = "repo:src/a.rs:Head:42"
    let line = Attrs.withData(
      <div className="row"> {React.string("the focused line")} </div>,
      [("data-focused", "true"), ("data-scroll-anchor", key)],
    )
    let {container} = render(<div className="anchor-scroller"> line </div>)
    let scroller =
      Element.querySelector(container, ".anchor-scroller")
      ->Nullable.toOption
      ->Option.getExn
    let row =
      Element.querySelector(container, "[data-scroll-anchor]")
      ->Nullable.toOption
      ->Option.getExn
    (scroller, row)
  }

  test("rows inserted above preserve the focused line's visual Y", () => {
    let (scroller, row) = mount()
    let geometry: AnchorGeometry.t = {rowTop: 260., rowHeight: 20.}
    AnchorGeometry.install(scroller, row, geometry)
    Scroll.setScrollTop(scroller, 100.)
    let before = Scroll.boxOf(row).top
    let anchor = Scroll.captureAnchor()->Option.getExn
    geometry.rowTop = geometry.rowTop +. 73.5
    expect(Scroll.restoreAnchor(anchor))->toBe(true)
    expect(Scroll.boxOf(row).top)->toBe(before)
    expect(Scroll.scrollTop(scroller))->toBe(173.5)
  })

  test("rows inserted below leave the exact visual Y untouched", () => {
    let (scroller, row) = mount()
    let geometry: AnchorGeometry.t = {rowTop: 260., rowHeight: 20.}
    AnchorGeometry.install(scroller, row, geometry)
    Scroll.setScrollTop(scroller, 100.)
    let before = Scroll.boxOf(row).top
    let anchor = Scroll.captureAnchor()->Option.getExn
    // Downward expansion changes the document below the anchor only.
    expect(Scroll.restoreAnchor(anchor))->toBe(true)
    expect(Scroll.boxOf(row).top)->toBe(before)
    expect(Scroll.scrollTop(scroller))->toBe(100.)
  })

  test("repeated variable-height expansion does not accumulate drift", () => {
    let (scroller, row) = mount()
    let geometry: AnchorGeometry.t = {rowTop: 140.25, rowHeight: 20.}
    AnchorGeometry.install(scroller, row, geometry)
    Scroll.setScrollTop(scroller, 40.)
    let wanted = Scroll.boxOf(row).top

    let first = Scroll.captureAnchor()->Option.getExn
    geometry.rowTop = geometry.rowTop +. 37.25
    geometry.rowHeight = 57.5
    expect(Scroll.restoreAnchor(first))->toBe(true)
    expect(Scroll.boxOf(row).top)->toBe(wanted)

    let second = Scroll.captureAnchor()->Option.getExn
    geometry.rowTop = geometry.rowTop +. 21.75
    geometry.rowHeight = 13.25
    expect(Scroll.restoreAnchor(second))->toBe(true)
    expect(Scroll.boxOf(row).top)->toBe(wanted)
    expect(Scroll.scrollTop(scroller))->toBe(99.)
  })

  test("the Shell carries an anchor across the missing-render patch", () => {
    let base = Fixtures.parse(View.ViewModel.schema, "client", "ViewModel", "default")
    let diff = base.diff->Option.getExn
    let initial: View.ViewModel.t = {...base, diffs: [diff]}
    let loading: View.ViewModel.t = {...initial, diff: None, diffs: []}
    let moved: View.DiffView.t = {
      ...diff,
      firstRow: diff.firstRow + 20,
      lastRow: diff.lastRow + 20,
      rows: diff.rows->Array.map(row => {...row, index: row.index + 20}),
    }
    let landed: View.ViewModel.t = {
      ...initial,
      focus: Diff({row: 141, side: Head}),
      diff: Some(moved),
      diffs: [moved],
    }
    let push = ref(_ => ())
    let core: Core.t = {
      dispatch: _ => (),
      key: _ => (),
      subscribe: listener => {
        push := listener
        listener(initial)
        () => ()
      },
      attach: () => (),
    }
    let geometry: AnchorGeometry.t = {rowTop: 260., rowHeight: 20.}
    let restoreGeometry = AnchorGeometry.installShell(geometry)
    let {container} = render(<App.Shell core />)
    let row = Element.querySelector(container, "[data-scroll-anchor]")->Nullable.getExn
    let scroller = Element.querySelector(container, ".diff-stack")->Nullable.getExn
    Scroll.setScrollTop(scroller, 100.)
    let wanted = Scroll.boxOf(row).top

    act(() => push.contents(loading))
    expect(Element.querySelector(container, "[data-scroll-anchor]"))->toBeNull
    geometry.rowTop = geometry.rowTop +. 73.5
    act(() => push.contents(landed))

    let replacement = Element.querySelector(container, "[data-scroll-anchor]")->Nullable.getExn
    let replacementScroller = Element.querySelector(container, ".diff-stack")->Nullable.getExn
    expect(Scroll.boxOf(replacement).top)->toBe(wanted)
    expect(Scroll.scrollTop(replacementScroller))->toBe(173.5)
    restoreGeometry()
  })
})

describe("Scroll.plan", () => {
  let intent = (seq: int): View.ScrollIntent.t => {row: 120, align: Center, seq}

  test("a focused row that has not arrived yet keeps the intent for later", () => {
    // `G` focuses a row whose chunk is still in flight: there is nothing
    // in the DOM to scroll to, and consuming the intent here would lose
    // the reposition for good.
    let (step, seen) = Scroll.plan(
      ~focus=Diff({row: 120, side: Head}),
      ~scroll=Some(intent(4)),
      ~present=false,
      ~seen=None,
    )
    expect(step)->toEqual(Scroll.Skip)
    expect(seen)->toEqual(None)
    // The chunk lands: same intent, now performable.
    let (step, seen) = Scroll.plan(
      ~focus=Diff({row: 120, side: Head}),
      ~scroll=Some(intent(4)),
      ~present=true,
      ~seen,
    )
    expect(step)->toEqual(Scroll.Reposition(Center))
    expect(seen)->toEqual(Some(4))
  })

  test("an intent is performed once; motions after it only follow", () => {
    let (step, seen) = Scroll.plan(
      ~focus=Diff({row: 121, side: Head}),
      ~scroll=Some(intent(4)),
      ~present=true,
      ~seen=Some(4),
    )
    expect(step)->toEqual(Scroll.Follow)
    expect(seen)->toEqual(Some(4))
    // Pressing the chord again is a new instruction.
    let (step, _) = Scroll.plan(
      ~focus=Diff({row: 121, side: Head}),
      ~scroll=Some(intent(5)),
      ~present=true,
      ~seen,
    )
    expect(step)->toEqual(Scroll.Reposition(Center))
  })

  test("a view mounting at an existing intent does not replay it", () => {
    // Leaving a tab and coming back remounts the consumer; starting its
    // watermark at the current intent is what keeps the old `z z` from
    // yanking the view back to where the cursor used to be.
    let (step, _) = Scroll.plan(
      ~focus=Diff({row: 121, side: Head}),
      ~scroll=Some(intent(4)),
      ~present=true,
      ~seen=Some(4),
    )
    expect(step)->toEqual(Scroll.Follow)
  })

  test("list focus scrolls itself, and modal focus not at all", () => {
    let (step, _) = Scroll.plan(~focus=Tree({index: 2}), ~scroll=None, ~present=false, ~seen=None)
    expect(step)->toEqual(Scroll.List)
    let (step, _) = Scroll.plan(~focus=Composer({}), ~scroll=None, ~present=false, ~seen=None)
    expect(step)->toEqual(Scroll.Skip)
  })
})

// jsdom has no clipboard, and the interesting cases are the ones that do
// not look like success: an absent API and a refused write.
%%raw(`
function installSlowClipboard(delays) {
  globalThis.__copied = []
  let i = 0
  Object.defineProperty(globalThis.navigator, "clipboard", {
    configurable: true,
    value: {
      writeText: (text) => {
        const ms = delays[i++] ?? 0
        return new Promise((resolve) => setTimeout(() => {
          globalThis.__copied.push(text)
          resolve()
        }, ms))
      },
    },
  })
}
function installClipboard(mode) {
  globalThis.__copied = []
  const value =
    mode === "missing"
      ? undefined
      : {
          writeText: (text) => {
            if (mode === "refuse") return Promise.reject(new Error("denied"))
            globalThis.__copied.push(text)
            return Promise.resolve()
          },
        }
  Object.defineProperty(globalThis.navigator, "clipboard", {configurable: true, value})
}
function copiedPaths() {
  return globalThis.__copied || []
}
function flush() {
  return new Promise((resolve) => setTimeout(resolve, 0))
}
function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}
`)

@val external installClipboard: string => unit = "installClipboard"
@val external installSlowClipboard: array<int> => unit = "installSlowClipboard"
@val external copiedPaths: unit => array<string> = "copiedPaths"
@val external flush: unit => promise<unit> = "flush"
@val external sleep: int => promise<unit> = "sleep"

describe("Copy path", () => {
  let chrome: array<View.Hint.t> = [{keys: "y", command: CopyPath, label: "copy path"}]

  test("both file headers offer copy, and the tooltip comes from the keymap", () => {
    let dispatch = fn()
    let base = Fixtures.parse(View.DiffView.schema, "client", "DiffView", "default")
    // Browse (DiffView) had no copy affordance at all.
    let {container} = render(
      <DiffView diff=base layout=Unified focus={Diff({row: 0, side: Head})} chrome dispatch />,
    )
    let copy =
      Element.querySelector(container, "[title=\"copy path (y)\"]")
      ->Nullable.toOption
      ->Option.getExn
    FireEvent.click(copy)
    expect(dispatch)->toHaveBeenLastCalledWith(Action.CopyPath({path: base.file.path}))
    cleanup()
    // Rebound: the tooltip follows the keymap rather than a literal `y`.
    let rebound: array<View.Hint.t> = [{keys: "ctrl+y", command: CopyPath, label: "copy path"}]
    let {container} = render(
      <DiffView
        diff=base layout=Unified focus={Diff({row: 0, side: Head})} chrome=rebound dispatch
      />,
    )
    expect(Element.querySelector(container, "[title=\"copy path (ctrl+y)\"]"))->not_->toBeNull
  })

  testAsync("a write that lands, one that is refused, and no clipboard at all", async () => {
    installClipboard("ok")
    let outcome = await Clipboard.write("src/a.rs")
    expect(outcome)->toEqual(Ok())
    expect(copiedPaths())->toEqual(["src/a.rs"])
    expect(Clipboard.message("src/a.rs", outcome))->toBe("Copied src/a.rs")

    installClipboard("refuse")
    let outcome = await Clipboard.write("src/a.rs")
    expect(outcome)->toEqual(Error(Clipboard.Refused))
    expect(copiedPaths())->toEqual([])
    expect(Clipboard.message("src/a.rs", outcome))->toContain("refused")

    // An insecure context has no clipboard object at all: the binding
    // must not throw where a promise rejection would have been caught.
    installClipboard("missing")
    let outcome = await Clipboard.write("src/a.rs")
    expect(outcome)->toEqual(Error(Clipboard.Unavailable))
    expect(Clipboard.message("src/a.rs", outcome))->toContain("no clipboard")
  })

  testAsync("the toast shows, then dismisses itself", async () => {
    let {container, rerender} = render(<Toast message=None />)
    expect(Element.querySelector(container, ".toast"))->toBeNull
    rerender(<Toast message={Some(("Copied src/a.rs", false))} />)
    expect(Element.textContent(Screen.getByTextRe(/Copied/)))->toContain("src/a.rs")
    // A failure is styled as one, and neither sits there for the session.
    rerender(<Toast message={Some(("Could not copy — the clipboard refused", true))} />)
    let el = Element.querySelector(container, ".toast")->Nullable.toOption->Option.getExn
    expect(Element.className(el))->toContain("toast-error")
    await sleep(Toast.dismissAfterMs + 20)
    expect(Element.querySelector(container, ".toast"))->toBeNull
  })

  testAsync("overlapping copies show the newest result, not the slowest", async () => {
    // Two clicks in a row: the first write settles last, and must not
    // overwrite what the second one already told the reader.
    installSlowClipboard([40, 0])
    let shown = ref([])
    let writer = Clipboard.latest()
    writer("first.rs", (text, _) => shown := Array.concat(shown.contents, [text]))
    writer("second.rs", (text, _) => shown := Array.concat(shown.contents, [text]))
    await sleep(80)
    expect(shown.contents)->toEqual(["Copied second.rs"])
  })

  test("the shell tracks its own chord prefix, so it cannot lag the keys", () => {
    // The core's `pendingKeys` is its answer to the *previous* key and
    // arrives a round trip later — one keystroke too late to decide
    // anything about this one. The shell sends the chords, so it tracks
    // the prefix itself.
    let chord = (c: string): Keys.KeyChord.t => {
      key: Char({c: c}),
      mods: {ctrl: false, alt: false, shift: false, meta: false},
    }
    let bindings: array<View.Hint.t> = [
      {keys: "y", command: CopyPath, label: "copy path"},
      {keys: "g g", command: GoTop, label: "top"},
    ]
    let p = App.Pending.make()
    expect(App.Pending.step(p, bindings, chord("y")))->toEqual(
      App.Pending.Runs(View.Command.CopyPath),
    )
    // `j` binds nothing in this table and nothing is pending: the core
    // calls that an unbound key and does not count it, so neither does
    // the shell.
    expect(App.Pending.step(p, bindings, chord("j")))->toEqual(App.Pending.Unbound)

    // A sequence typed faster than the round trip: the second chord still
    // completes it, because the prefix never left this shell.
    let sequence: array<View.Hint.t> = [{keys: "g y", command: CopyPath, label: "copy path"}]
    let p = App.Pending.make()
    expect(App.Pending.step(p, sequence, chord("g")))->toEqual(App.Pending.Prefix)
    expect(App.Pending.step(p, sequence, chord("y")))->toEqual(
      App.Pending.Runs(View.Command.CopyPath),
    )
    // And a bare `y` under that binding runs nothing, so nothing is copied.
    let p = App.Pending.make()
    expect(App.Pending.step(p, sequence, chord("y")))->toEqual(App.Pending.Unbound)
    // A chord that matches no binding clears the prefix rather than
    // leaving it to swallow the next key.
    let p = App.Pending.make()
    expect(App.Pending.step(p, sequence, chord("g")))->toEqual(App.Pending.Prefix)
    expect(App.Pending.step(p, sequence, chord("x")))->toEqual(App.Pending.Prefix)
    expect(App.Pending.step(p, bindings, chord("y")))->toEqual(
      App.Pending.Runs(View.Command.CopyPath),
    )

    // The bindings come from the core's applicable set for the focused
    // context — not from `hints` (primary only, so `y` is absent) and not
    // from `chrome` (one per command, no context, so `y` would look bound
    // on a focused thread, where the core refuses it).
    let m = Fixtures.parse(View.ViewModel.schema, "client", "ViewModel", "default")
    let onAThread: View.ViewModel.t = {
      ...m,
      bindings: [{keys: "enter", command: Open, label: "open"}],
      hints: [{keys: "enter", command: Open, label: "open"}],
      chrome: [{keys: "y", command: CopyPath, label: "copy path"}],
    }
    let p = App.Pending.make()
    expect(App.Pending.step(p, App.bindingsFor(onAThread), chord("y")))->toEqual(
      App.Pending.Unbound,
    )
    let onATree: View.ViewModel.t = {
      ...m,
      bindings: [{keys: "y", command: CopyPath, label: "copy path"}],
      hints: [],
      chrome: [{keys: "y", command: CopyPath, label: "copy path"}],
    }
    let p = App.Pending.make()
    expect(App.Pending.step(p, App.bindingsFor(onATree), chord("y")))->toEqual(
      App.Pending.Runs(View.Command.CopyPath),
    )
  })
})

// Key events, dispatched at the window the way the shell listens for them.
%%raw(`
function pressKey(key) {
  globalThis.window.dispatchEvent(new globalThis.KeyboardEvent("keydown", {key, bubbles: true}))
}
`)
@val external pressKey: string => unit = "pressKey"
describe("The shell's file-tree toggle", () => {
  let model = (hidden: bool): View.ViewModel.t => {
    let base = Fixtures.parse(View.ViewModel.schema, "client", "ViewModel", "default")
    {
      ...base,
      prefs: {...base.prefs, sidebarHidden: hidden},
      chrome: [{keys: "x b", command: ToggleSidebar, label: "custom sidebar toggle"}],
    }
  }

  let mount = initial => {
    let push = ref(_ => ())
    let dispatch: Action.t => unit = fn()
    let key: Keys.KeyChord.t => unit = fn()
    let core: Core.t = {
      dispatch,
      key,
      subscribe: listener => {
        push := listener
        listener(initial)
        () => ()
      },
      attach: () => (),
    }
    (render(<App.Shell core />), push, dispatch, key)
  }

  test("stays put, exposes both states, and owns mouse and keyboard activation", () => {
    let ({container}, push, dispatch, key) = mount(model(false))
    let toggle = Screen.getByLabelText("Hide file tree")
    expect(Element.getAttribute(toggle, "aria-controls"))->toEqual(
      Nullable.make("file-tree-sidebar"),
    )
    expect(Element.getAttribute(toggle, "aria-expanded"))->toEqual(Nullable.make("true"))
    expect(Element.getAttribute(toggle, "title"))->toEqual(
      Nullable.make("custom sidebar toggle (x b)"),
    )
    expect(Element.querySelector(toggle, "[aria-hidden=\"true\"]"))->not_->toBeNull
    expect(Element.querySelector(container, "#file-tree-sidebar"))->not_->toBeNull
    expect(Element.querySelector(container, ".sidebar-rail"))->toBeNull
    expect(Element.querySelector(container, ".sidebar-collapse"))->toBeNull

    FireEvent.keyDown(toggle, {"key": "Enter", "ctrlKey": false})
    expect(Array.length(mock(dispatch).calls))->toBe(1)
    expect(dispatch)->toHaveBeenLastCalledWith(Action.ToggleSidebar({}))
    expect(Array.length(mock(key).calls))->toBe(0)

    FireEvent.keyDown(toggle, {"key": " ", "ctrlKey": false})
    expect(Array.length(mock(dispatch).calls))->toBe(2)
    expect(dispatch)->toHaveBeenLastCalledWith(Action.ToggleSidebar({}))
    expect(Array.length(mock(key).calls))->toBe(0)

    FireEvent.click(toggle)
    expect(Array.length(mock(dispatch).calls))->toBe(3)
    expect(dispatch)->toHaveBeenLastCalledWith(Action.ToggleSidebar({}))

    Element.focus(toggle)
    expect(Document.activeElement->Nullable.toOption)->toEqual(Some(toggle))
    act(() => push.contents(model(true)))
    let sameToggle = Screen.getByLabelText("Show file tree")
    expect(sameToggle)->toBe(toggle)
    expect(Document.activeElement->Nullable.toOption)->toEqual(Some(toggle))
    expect(Element.getAttribute(toggle, "aria-expanded"))->toEqual(Nullable.make("false"))
    expect(Element.textContent(toggle))->toContain("◨")
    expect(Element.querySelector(container, "#file-tree-sidebar"))->toBeNull
    expect(
      Element.className(Element.querySelector(container, ".app-body")->Nullable.getExn),
    )->toContain("sidebar-hidden")

    act(() => push.contents(model(false)))
    expect(Screen.getByLabelText("Hide file tree"))->toBe(toggle)
    expect(Document.activeElement->Nullable.toOption)->toEqual(Some(toggle))
    expect(Element.textContent(toggle))->toContain("◧")
    expect(Element.querySelector(container, "#file-tree-sidebar"))->not_->toBeNull
  })
})

describe("Copying from the keyboard, through the shell", () => {
  // The core applies keys in order, counts every one it is sent, and
  // says what each turned out to mean. The shell copies inside the
  // gesture when its view accounts for every earlier key; otherwise it
  // waits for the core's verdict rather than predicting one, because a
  // key typed after a focus change lands somewhere the shell has not
  // seen yet.
  let after = (seq, command): option<View.LastKey.t> => Some({seq, command})

  let model = (~target, ~lastKey): View.ViewModel.t => {
    let base = Fixtures.parse(View.ViewModel.schema, "client", "ViewModel", "default")
    {
      ...base,
      copyTarget: Some(target),
      lastKey,
      bindings: [
        {keys: "y", command: CopyPath, label: "copy path"},
        {keys: "j", command: MoveDown, label: "down"},
        {keys: "g t", command: FocusThreads, label: "threads"},
      ],
      hints: [],
      chrome: [],
    }
  }

  let mount = initial => {
    let push = ref(_ => ())
    let core: Core.t = {
      dispatch: _ => (),
      key: _ => (),
      subscribe: listener => {
        push := listener
        listener(initial)
        () => ()
      },
      attach: () => (),
    }
    (render(<App.Shell core />), push)
  }

  testAsync(
    "checkout copying follows its rebound chord and uses an absolute daemon path",
    async () => {
      installClipboard("ok")
      let initial = {
        ...model(~target="src/a.rs", ~lastKey=None),
        copyCheckout: Some("/srv/atlas"),
        bindings: [{keys: "g y", command: CopyCheckout, label: "copy checkout path"}],
      }
      let (_, push) = mount(initial)
      pressKey("y")
      await flush()
      expect(copiedPaths())->toEqual([])
      act(() => push.contents({...initial, lastKey: after(1, None)}))
      pressKey("g")
      act(() => push.contents({...initial, lastKey: after(2, None)}))
      pressKey("y")
      await flush()
      expect(copiedPaths())->toEqual(["/srv/atlas"])
      cleanup()
    },
  )

  testAsync("`y` copies the current target when the core is caught up", async () => {
    installClipboard("ok")
    let (_, _) = mount(model(~target="src/a.rs", ~lastKey=None))
    pressKey("y")
    await flush()
    expect(copiedPaths())->toEqual(["src/a.rs"])
    cleanup()
  })

  testAsync("`y` twice copies twice, though neither key changes the view", async () => {
    // `CopyPath` is a no-op in the core, so the second `y` gets no patch
    // of its own: a shell that waited for one would copy once.
    installClipboard("ok")
    let (_, push) = mount(model(~target="src/a.rs", ~lastKey=None))
    pressKey("y")
    await flush()
    act(() => push.contents(model(~target="src/a.rs", ~lastKey=after(1, Some(CopyPath)))))
    pressKey("y")
    await flush()
    expect(copiedPaths())->toEqual(["src/a.rs", "src/a.rs"])
    cleanup()
  })

  testAsync("two movements then `y` waits for the core's word on `y` itself", async () => {
    installClipboard("ok")
    let (_, push) = mount(model(~target="before.rs", ~lastKey=None))
    pressKey("j")
    pressKey("j")
    pressKey("y")
    await flush()
    expect(copiedPaths())->toEqual([])
    // The movements land one at a time; neither is the key that copies.
    act(() => push.contents(model(~target="after-one.rs", ~lastKey=after(1, Some(MoveDown)))))
    act(() => push.contents(model(~target="after-two.rs", ~lastKey=after(2, Some(MoveDown)))))
    await flush()
    expect(copiedPaths())->toEqual([])
    // The core reaches the `y`, in the context the movements left it in.
    act(() => push.contents(model(~target="after-two.rs", ~lastKey=after(3, Some(CopyPath)))))
    await flush()
    expect(copiedPaths())->toEqual(["after-two.rs"])
    cleanup()
  })

  testAsync("a shell that attaches mid-session does not read the core as caught up", async () => {
    // A Tauri shell remount can meet a core that has handled keys this
    // shell never sent, so its count says nothing about this shell's own.
    installClipboard("ok")
    let (_, push) = mount(model(~target="before.rs", ~lastKey=after(10, Some(MoveDown))))
    pressKey("j")
    pressKey("y")
    await flush()
    expect(copiedPaths())->toEqual([])
    act(() => push.contents(model(~target="after.rs", ~lastKey=after(11, Some(MoveDown)))))
    await flush()
    expect(copiedPaths())->toEqual([])
    act(() => push.contents(model(~target="after.rs", ~lastKey=after(12, Some(CopyPath)))))
    await flush()
    expect(copiedPaths())->toEqual(["after.rs"])
    cleanup()
  })

  testAsync("`y` typed into a context the core has moved to copies nothing", async () => {
    // `g t` focuses the threads, where `y` is bound to nothing. The shell
    // still holds the diff's bindings when the `y` arrives, so it would
    // resolve it as a copy — the core's verdict is what stops it.
    installClipboard("ok")
    let (_, push) = mount(model(~target="before.rs", ~lastKey=None))
    pressKey("g")
    pressKey("t")
    pressKey("y")
    pressKey("j")
    await flush()
    act(() => push.contents(model(~target="threads.rs", ~lastKey=after(2, Some(FocusThreads)))))
    await flush()
    expect(copiedPaths())->toEqual([])
    // The core rejected the `y` (seq 3), which carries no patch, so the
    // next verdict is the `j` that followed it: the shell reads that as
    // its answer — the key it was holding is behind the core now — and
    // drops the copy rather than taking the file it was looking at.
    act(() => push.contents(model(~target="threads.rs", ~lastKey=after(4, Some(MoveDown)))))
    await flush()
    expect(copiedPaths())->toEqual([])
    cleanup()
  })

  testAsync("two deferred copies both happen, in the order they were typed", async () => {
    installClipboard("ok")
    let (_, push) = mount(model(~target="before.rs", ~lastKey=None))
    pressKey("j")
    pressKey("y")
    pressKey("j")
    pressKey("y")
    await flush()
    act(() => push.contents(model(~target="after-one.rs", ~lastKey=after(2, Some(CopyPath)))))
    act(() => push.contents(model(~target="after-two.rs", ~lastKey=after(4, Some(CopyPath)))))
    await flush()
    expect(copiedPaths())->toEqual(["after-one.rs", "after-two.rs"])
    cleanup()
  })

  testAsync("a reconnect drops copy verdicts belonging to the old session", async () => {
    installClipboard("ok")
    let (_, push) = mount(model(~target="old.rs", ~lastKey=None))
    pressKey("j")
    pressKey("y")
    await flush()
    expect(copiedPaths())->toEqual([])

    // CoreWs publishes the empty, disconnected model before the fresh
    // host attaches. Its sequence starts over and must not release the
    // copy that was waiting for old-session sequence 2.
    act(() => push.contents(View.ViewModel.empty))
    act(() => push.contents(model(~target="fresh.rs", ~lastKey=after(2, Some(CopyPath)))))
    await flush()
    expect(copiedPaths())->toEqual([])

    pressKey("y")
    await flush()
    expect(copiedPaths())->toEqual(["fresh.rs"])
    cleanup()
  })

  testAsync("an unbound key still counts, on both sides", async () => {
    // The core counts every key it is handed, rejected ones included, so
    // this shell counts every chord it sends. `q` is bound to nothing
    // here; if it went uncounted, the `j` verdict would be mistaken for
    // the `y` one and the copy would be dropped.
    installClipboard("ok")
    let (_, push) = mount(model(~target="before.rs", ~lastKey=None))
    pressKey("q")
    pressKey("j")
    pressKey("y")
    await flush()
    // `q` was seq 1 and carried no patch of its own; `j` is seq 2.
    act(() => push.contents(model(~target="after.rs", ~lastKey=after(2, Some(MoveDown)))))
    await flush()
    expect(copiedPaths())->toEqual([])
    act(() => push.contents(model(~target="after.rs", ~lastKey=after(3, Some(CopyPath)))))
    await flush()
    expect(copiedPaths())->toEqual(["after.rs"])
    cleanup()
  })

  testAsync("attaching after a rejected key adopts the sequence the core is at", async () => {
    // A rejection derives no patch, but the view it attaches to records
    // it, so this shell starts from 11 rather than the 10 that was last
    // broadcast.
    installClipboard("ok")
    let (_, push) = mount(model(~target="before.rs", ~lastKey=after(11, None)))
    pressKey("j")
    pressKey("y")
    await flush()
    act(() => push.contents(model(~target="after.rs", ~lastKey=after(12, Some(MoveDown)))))
    await flush()
    expect(copiedPaths())->toEqual([])
    act(() => push.contents(model(~target="after.rs", ~lastKey=after(13, Some(CopyPath)))))
    await flush()
    expect(copiedPaths())->toEqual(["after.rs"])
    cleanup()
  })

  testAsync("a key the focused context does not bind copies nothing", async () => {
    installClipboard("ok")
    let base = Fixtures.parse(View.ViewModel.schema, "client", "ViewModel", "default")
    // A thread is focused: the keymap binds no copy there, and the core
    // would refuse the key.
    let onAThread: View.ViewModel.t = {
      ...base,
      copyTarget: Some("src/a.rs"),
      lastKey: None,
      bindings: [{keys: "enter", command: Open, label: "open"}],
      hints: [],
      chrome: [{keys: "y", command: CopyPath, label: "copy path"}],
    }
    let (_, _) = mount(onAThread)
    pressKey("y")
    await flush()
    expect(copiedPaths())->toEqual([])
    cleanup()
  })
})

describe("Commented lines and the inline composer", () => {
  // A two-line thread on the head side, its card under the last line,
  // plus a draft over the same rows.
  let diff = (~drafted): View.DiffView.t => {
    let base = Fixtures.parse(View.DiffView.schema, "client", "DiffView", "default")
    let row = Fixtures.parse(View.DiffRow.schema, "client", "DiffRow", "default")
    let thread = (place: View.RowPlace.t): View.RowThread.t => {
      thread: "t1",
      side: Head,
      place,
    }
    {
      ...base,
      firstRow: 121,
      lastRow: 122,
      missing: [],
      collapsed: false,
      rows: [
        {
          ...row,
          index: 121,
          threads: [thread(Inside)],
          drafted: drafted ? Some((Inside, Head)) : None,
        },
        {
          ...row,
          index: 122,
          threads: [thread(Anchor)],
          drafted: drafted ? Some((Anchor, Head)) : None,
        },
      ],
    }
  }

  let mount = (~diff as d, ~draft, ~dispatch) =>
    render(
      <FileDiff
        diff=d
        layout=Split
        focus={Diff({row: 121, side: Head})}
        threads=[]
        draft
        pendingRefresh=false
        isOpen=true
        dispatch
      />,
    )

  test("a unified context row shows a base anchor's marker on its one cell", () => {
    // The single rendered cell stands for both sides, so a thread
    // anchored to the base line of an unchanged row still has to show
    // its marker there.
    let row = Fixtures.parse(Render.Row.schema, "protocol", "Row", "Context")
    let {container} = render(
      <Row
        row layout=Unified index=0 focused=false threads=[{thread: "t1", side: Base, place: Anchor}]
      />,
    )
    expect(Element.querySelector(container, ".cell-right .cell-threads"))->not_->toBeNull
    cleanup()
    // In split layout the two cells are distinct, so it belongs to the
    // left one only.
    let {container} = render(
      <Row
        row layout=Split index=0 focused=false threads=[{thread: "t1", side: Base, place: Anchor}]
      />,
    )
    expect(Element.querySelector(container, ".cell-left .cell-threads"))->not_->toBeNull
    expect(Element.querySelector(container, ".cell-right .cell-threads"))->toBeNull
  })

  test("the three cell states compose rather than replacing each other", () => {
    // Commented, selected and being-drafted-about can all be true at
    // once; each contributes a layer of one shadow (see app.css §cells),
    // so the classes have to survive together.
    let base = Fixtures.parse(View.DiffView.schema, "client", "DiffView", "default")
    let row = Fixtures.parse(View.DiffRow.schema, "client", "DiffRow", "default")
    let d: View.DiffView.t = {
      ...base,
      firstRow: 121,
      lastRow: 121,
      missing: [],
      collapsed: false,
      rows: [
        {
          ...row,
          index: 121,
          threads: [{thread: "t1", side: Head, place: Anchor}],
          drafted: Some((Anchor, Head)),
        },
      ],
    }
    let {container} = render(
      <FileDiff
        diff=d
        layout=Split
        focus={Diff({row: 121, side: Head})}
        threads=[]
        draft=None
        pendingRefresh=false
        isOpen=true
        visual={{start: 121, end_: 121, side: Head}}
        dispatch={_ => ()}
      />,
    )
    let cell =
      Element.querySelector(container, "[data-row-index=\"121\"] .cell-right")
      ->Nullable.toOption
      ->Option.getExn
    let className = Element.className(cell)
    expect(className)->toContain("cell-commented")
    expect(className)->toContain("cell-selected")
    expect(className)->toContain("cell-drafting")
    expect(className)->toContain("cell-focused")
  })

  test("every line of a range is marked, and the card's row is the last", () => {
    let {container} = mount(~diff=diff(~drafted=false), ~draft=None, ~dispatch=fn())
    let commented = Element.querySelectorAll(container, ".cell-commented")
    expect(Array.length(commented))->toBe(2)
    // The 💬 marker belongs to the row the card hangs under, not to
    // every line of the range.
    let markers = Element.querySelectorAll(container, ".cell-threads")
    expect(Array.length(markers))->toBe(1)
    let anchorRow =
      Element.querySelector(container, "[data-row-index=\"122\"]")
      ->Nullable.toOption
      ->Option.getExn
    expect(Element.querySelector(anchorRow, ".cell-threads"))->not_->toBeNull
  })

  test("a line draft composes under its own last line, not at the bottom", () => {
    let draft: View.Draft.t = {
      anchor: Lines({
        repoId: "r",
        path: "src/a.rs",
        side: Head,
        blobOid: "b",
        lines: {start: 9, end_: 9},
        contextHash: "0",
      }),
      submissionError: None,
      purpose: Comment({intent: Finding, context: None}),
    }
    expect(View.Draft.isDocked(draft))->toBe(false)
    let {container} = mount(~diff=diff(~drafted=true), ~draft=Some(draft), ~dispatch=fn())
    // Drafted rows are marked while typing…
    expect(Array.length(Element.querySelectorAll(container, ".cell-drafting")))->toBe(2)
    // …and the composer sits in the slot the thread will occupy.
    let anchorRow =
      Element.querySelector(container, "[data-row-index=\"122\"]")
      ->Nullable.toOption
      ->Option.getExn
      ->Element.parentElement
    expect(Element.querySelector(anchorRow, ".composer"))->not_->toBeNull
  })

  test("a review-level draft still docks", () => {
    let draft: View.Draft.t = {
      anchor: Review({}),
      submissionError: None,
      purpose: Comment({intent: Finding, context: None}),
    }
    expect(View.Draft.isDocked(draft))->toBe(true)
  })
})

describe("The shell's composer placement", () => {
  // Through the real shell, not by mounting a component with props its
  // caller does not pass: that is exactly how Browse ended up with the
  // docked composer while the diff view had an inline one.
  let shell = (model: View.ViewModel.t) => {
    let core: Core.t = {
      dispatch: _ => (),
      key: _ => (),
      subscribe: listener => {
        listener(model)
        () => ()
      },
      attach: () => (),
    }
    render(<App.Shell core />)
  }

  let lineDraft: View.Draft.t = {
    anchor: Lines({
      repoId: "r",
      path: "src/a.rs",
      side: Head,
      blobOid: "b",
      lines: {start: 9, end_: 9},
      contextHash: "0",
    }),
    submissionError: None,
    purpose: Comment({intent: Finding, context: None}),
  }

  let withDraft = (~tab: View.Tab.t, ~draft): View.ViewModel.t => {
    let base = Fixtures.parse(View.ViewModel.schema, "client", "ViewModel", "default")
    let diff = Fixtures.parse(View.DiffView.schema, "client", "DiffView", "default")
    let row = Fixtures.parse(View.DiffRow.schema, "client", "DiffRow", "default")
    {
      ...base,
      tab,
      draft,
      diff: Some({
        ...diff,
        collapsed: false,
        missing: [],
        firstRow: 121,
        lastRow: 121,
        rows: [{...row, index: 121, drafted: Some((Anchor, Head))}],
      }),
      diffs: [],
    }
  }

  test("a line draft does not dock, in either tab", () => {
    // Browse used to mount `DiffView` without the draft and then dock
    // every draft below it, so a line comment there was composed at the
    // bottom of the page whatever the diff view did.
    [View.Tab.FilesChanged, Browse]->Array.forEach(
      tab => {
        let {container} = shell(withDraft(~tab, ~draft=Some(lineDraft)))
        expect(Array.length(Element.querySelectorAll(container, ".app-center > .composer")))->toBe(
          0,
        )
        cleanup()
      },
    )
    // (The inline composer itself is asserted in the row tests above;
    // jsdom has no layout, so Browse's virtualizer renders no rows here.)
  })

  test("a review-level draft still docks, in both tabs", () => {
    let review: View.Draft.t = {
      anchor: Review({}),
      submissionError: None,
      purpose: Comment({intent: Finding, context: None}),
    }
    [View.Tab.FilesChanged, Browse]->Array.forEach(
      tab => {
        let {container} = shell(withDraft(~tab, ~draft=Some(review)))
        let composers = Element.querySelectorAll(container, ".composer")
        expect(Array.length(composers))->toBe(1)
        let row = Element.querySelector(container, "[data-row-index=\"121\"]")->Nullable.toOption
        switch row {
        | Some(el) =>
          expect(Element.querySelector(Element.parentElement(el), ".composer"))->toBeNull
        | None => ()
        }
        cleanup()
      },
    )
  })
})

describe("Informational conversation", () => {
  test("three findings and a summary count as three open items, with attributed replies", () => {
    let base = Fixtures.parse(View.ThreadView.schema, "client", "ThreadView", "default")
    let note = {
      ...base,
      id: "summary",
      status: Informational({}),
      place: Review({}),
      comments: [
        ...base.comments,
        {
          ...base.comments->Array.getUnsafe(0),
          id: "reply",
          body: "Progress update",
          author: Human({name: "Bea", machine: "host"}),
        },
      ],
    }
    let findings = [base, {...base, id: "second"}, {...base, id: "third"}]
    expect(Threads.openFindings([...findings, note]))->toBe(3)
    expect(
      Threads.openFindings([
        {...base, status: Resolved({by: base.author, at: base.created})},
        note,
      ]),
    )->toBe(0)
    let dispatch = fn()
    let chrome: array<View.Hint.t> = [{command: Reply, keys: "R", label: "reply"}]
    let {container} = render(
      <Threads
        title="Conversation" threads=[note] focus={Thread({index: 0})} indexOffset=0 dispatch chrome
      />,
    )
    expect(Screen.getByText("informational"))->toBeTruthy
    expect(Screen.getByText("Progress update"))->toBeTruthy
    expect(Screen.getByText("Bea"))->toBeTruthy
    expect(Array.length(Screen.queryAllByText("Resolve finding")))->toBe(0)
    expect(Array.length(Screen.queryAllByText("Reopen finding")))->toBe(0)
    let reply = Screen.getByText("Reply")
    expect(Element.getAttribute(reply, "title"))->toEqual(Nullable.make("reply (R)"))
    FireEvent.click(reply)
    expect(dispatch)->toHaveBeenLastCalledWith(Action.ReplyOpened({threadId: "summary"}))
    expect(Element.textContent(container))->toContain("Progress update")
    cleanup()
    let _ = render(
      <Threads
        title="Conversation"
        threads=[note]
        focus={Composer({})}
        indexOffset=0
        dispatch
        chrome
        draft={{submissionError: None, purpose: Reply({threadId: note.id}), anchor: Review({})}}
      />,
    )
    expect(Screen.getByPlaceholderText("Reply…"))->toBeTruthy
    expect(Screen.getByText("Progress update"))->toBeTruthy
    expect(Array.length(Screen.queryAllByText("Reply")))->toBe(0)
  })

  test("conversation exposes both intents and renders its composer", () => {
    let model = {
      ...Fixtures.parse(View.ViewModel.schema, "client", "ViewModel", "default"),
      tab: Conversation,
      draft: Some({
        submissionError: None,
        purpose: Comment({intent: Informational, context: None}),
        anchor: Review({}),
      }),
    }
    let dispatch = fn()
    let core: Core.t = {
      dispatch,
      key: _ => (),
      subscribe: f => {
        f(model)
        () => ()
      },
      attach: () => (),
    }
    let _ = render(<App.Shell core />)
    FireEvent.click(Screen.getByText("Add informational note"))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.RunCommand({command: InformationalNote}))
    FireEvent.click(Screen.getByText("Add review-wide finding"))
    expect(dispatch)->toHaveBeenLastCalledWith(Action.RunCommand({command: ReviewFinding}))
    expect(Screen.getByPlaceholderText("Summary or status note…"))->toBeTruthy
  })
})

describe("Review requests", () => {
  test("shows durable attribution and note without finding controls", () => {
    let request = Fixtures.parse(
      Domain.ReviewRequest.schema,
      "protocol",
      "ReviewRequest",
      "default",
    )
    let dispatch = fn()
    let chrome: array<View.Hint.t> = [
      {keys: "g r", command: FocusRequests, label: "review requests"},
      {keys: "enter", command: Open, label: "open"},
    ]
    let {container, rerender} = render(
      <ReviewRequests requests=[request] focus={ReviewRequest({index: 0})} chrome dispatch />,
    )
    expect(Screen.getByText(Threads.authorName(request.requester)))->toBeTruthy
    expect(Screen.getByText("to " ++ request.recipient))->toBeTruthy
    expect(Screen.getByText(request.note))->toBeTruthy
    expect(
      Element.querySelector(container, "[title='" ++ Stepper.absolute(request.created) ++ "']"),
    )
    ->not_
    ->toBeNull
    expect(Element.querySelector(container, "[data-focused]"))->not_->toBeNull
    expect(Screen.queryAllByText("Resolve finding")->Array.length)->toBe(0)
    expect(Screen.queryAllByText("Reply")->Array.length)->toBe(0)
    FireEvent.click(Screen.getByText("Open current changes"))
    expect(dispatch)->toHaveBeenCalledWith(Action.SetFocus({focus: ReviewRequest({index: 0})}))
    expect(dispatch)->toHaveBeenCalledWith(Action.RunCommand({command: Open}))
    expect(dispatch)->toHaveBeenCalledTimes(2)
    FireEvent.click(Screen.getByText("Review requests (1)"))
    expect(dispatch)->toHaveBeenCalledWith(Action.RunCommand({command: FocusRequests}))
    rerender(<ReviewRequests requests=[] focus={Tree({index: 0})} chrome dispatch />)
    expect(Element.querySelector(container, ".review-request"))->toBeNull
  })

  test("conversation counts requests and deferred findings separately from open findings", () => {
    let {container} = render(
      <Tabs
        tab=Conversation
        fileCount=0
        threadCount=0
        requestCount=2
        deferredCount=3
        chrome=[]
        dispatch={fn()}
      />,
    )
    expect(Element.textContent(container))->toContain("0 open")
    expect(Element.textContent(container))->toContain("2 requests")
    expect(Element.textContent(container))->toContain("3 deferred")
  })
})

describe("Deferred findings", () => {
  test(
    "keeps an unfixed Browse finding visible with provenance, attribution, follow-up and reopen",
    () => {
      let base = Fixtures.parse(View.ThreadView.schema, "client", "ThreadView", "default")
      let deferred = {
        ...base,
        context: Some(Browse({reference: Tag({name: "v1"})})),
        status: Deferred({
          reason: "Controller issue #288 per scope decision",
          trackingUrl: Some("https://example.com/issues/288"),
          by: base.author,
          at: base.created,
        }),
      }
      let note = {...base, id: "note", status: Informational({})}
      expect(Threads.openFindings([base, deferred, note]))->toBe(1)
      expect(Threads.deferredFindings([base, deferred, note]))->toBe(1)
      let dispatch = fn()
      let chrome: array<View.Hint.t> = [
        {keys: "q", command: ToggleResolved, label: "reopen"},
        {keys: "D", command: DeferFinding, label: "defer"},
      ]
      let {container, rerender} = render(
        <Threads
          title="Conversation"
          threads=[deferred]
          focus={Thread({index: 0})}
          indexOffset=0
          chrome
          dispatch
        />,
      )
      expect(Screen.getByText("Controller issue #288 per scope decision"))->toBeTruthy
      expect(Screen.getByText("This should be a newtype."))->toBeTruthy
      expect(Screen.getByText("Deferred · unfixed"))->toBeTruthy
      expect(Screen.getByText("browse @tag:v1"))->toBeTruthy
      let link = Element.querySelector(container, "a[href]")->Nullable.getExn
      expect(Element.getAttribute(link, "href"))->toEqual(
        Nullable.make("https://example.com/issues/288"),
      )
      expect(Element.getAttribute(link, "rel"))->toEqual(Nullable.make("noopener noreferrer"))
      let reopen = Screen.getByText("Reopen finding")
      expect(Element.getAttribute(reopen, "title"))->toEqual(Nullable.make("reopen (q)"))
      FireEvent.click(reopen)
      expect(dispatch)->toHaveBeenLastCalledWith(Action.UnresolveThread({threadId: base.id}))
      rerender(
        <Threads
          title="Conversation"
          threads=[base]
          focus={Thread({index: 0})}
          indexOffset=0
          chrome
          dispatch
        />,
      )
      let defer = Screen.getByText("Defer finding")
      expect(Element.getAttribute(defer, "title"))->toEqual(Nullable.make("defer (D)"))
      FireEvent.click(defer)
      expect(dispatch)->toHaveBeenLastCalledWith(Action.DeferOpened({threadId: base.id}))
      rerender(
        <Threads
          title="Conversation"
          threads=[note]
          focus={Thread({index: 0})}
          indexOffset=0
          chrome
          dispatch
        />,
      )
      expect(Array.length(Screen.queryAllByText("Defer finding")))->toBe(0)
      expect(Array.length(Screen.queryAllByText("Reopen finding")))->toBe(0)
    },
  )

  test(
    "deferral composer requires a reason, includes optional URL and supports keyboard submission",
    () => {
      let draft: View.Draft.t = {
        anchor: Review({}),
        submissionError: None,
        purpose: Defer({threadId: "finding"}),
      }
      let dispatch = fn()
      let _ = render(<Composer bindings=editorBindings draft pendingRefresh=false dispatch />)
      let reason = Screen.getByPlaceholderText(
        "Reason for deferring this unfixed finding (required)…",
      )
      FireEvent.keyDown(reason, {"key": "Enter", "ctrlKey": true})
      expect(dispatch)->not_->toHaveBeenCalled
      FireEvent.change(reason, {"target": {"value": "Controller issue #288 per scope decision"}})
      let url = Screen.getByPlaceholderText("External tracking URL (optional HTTP(S))")
      FireEvent.change(url, {"target": {"value": "https://example.com/issues/288"}})
      FireEvent.keyDown(url, {"key": "Enter", "ctrlKey": true})
      expect(dispatch)->toHaveBeenLastCalledWith(
        Action.DeferThread({
          threadId: "finding",
          reason: "Controller issue #288 per scope decision",
          trackingUrl: Some("https://example.com/issues/288"),
        }),
      )
      FireEvent.keyDown(reason, {"key": "Escape", "ctrlKey": false})
      expect(dispatch)->toHaveBeenLastCalledWith(Action.DraftDiscarded({}))
    },
  )
})

test("deferral rejection is visible and keeps both inputs available for correction", () => {
  let draft: View.Draft.t = {
    anchor: Review({}),
    purpose: Defer({threadId: "finding"}),
    submissionError: None,
  }
  let dispatch = fn()
  let {container, rerender} = render(
    <Composer bindings=editorBindings draft pendingRefresh=false dispatch />,
  )
  let reason = Screen.getByPlaceholderText(
    "Reason for deferring this unfixed finding (required)…",
  )
  let url = Screen.getByPlaceholderText("External tracking URL (optional HTTP(S))")
  FireEvent.change(reason, {"target": {"value": "External follow-up"}})
  FireEvent.change(url, {"target": {"value": "example.com/issues/288"}})
  FireEvent.click(Screen.getByText("Submit"))
  // A rejected action returns this Draft patch; the real Rust bridge test
  // verifies the parse failure, patch delivery, and absence of a committed event.
  let message = "Invalid tracking URL. Enter a complete http:// or https:// URL."
  rerender(
    <Composer
      bindings=editorBindings
      draft={{...draft, submissionError: Some(message)}}
      pendingRefresh=false
      dispatch
    />,
  )
  let alert = Element.querySelector(container, "[role='alert']")->Nullable.getExn
  expect(Element.textContent(alert))->toContain(message)
  expect(Element.value(reason))->toBe("External follow-up")
  expect(Element.value(url))->toBe("example.com/issues/288")
  FireEvent.change(url, {"target": {"value": "https://example.com/issues/288"}})
  FireEvent.keyDown(url, {"key": "Enter", "ctrlKey": true})
  expect(dispatch)->toHaveBeenLastCalledWith(
    Action.DeferThread({
      threadId: "finding",
      reason: "External follow-up",
      trackingUrl: Some("https://example.com/issues/288"),
    }),
  )
  rerender(<Composer bindings=editorBindings draft pendingRefresh=false dispatch />)
  expect(Element.querySelector(container, "[role='alert']"))->toBeNull
  expect(Element.value(reason))->toBe("External follow-up")
})

describe("Portable thread and reply references", () => {
  let thread = () => Fixtures.parse(View.ThreadView.schema, "client", "ThreadView", "default")
  let chrome: array<View.Hint.t> = [{keys: "g y", command: CopyReference, label: "copy reference"}]

  test("both surfaces copy the exact comment and thread with configured tooltips", () => {
    let thread = thread()
    let dispatch = fn()
    let {container, rerender} = render(
      <InlineThread thread focused=true index=0 composer=React.null dispatch chrome />,
    )
    let buttons = Screen.queryAllByText("Copy reference")
    expect(Array.length(buttons))->toBe(2)
    FireEvent.click(buttons->Array.getUnsafe(0))
    expect(dispatch)->toHaveBeenLastCalledWith(
      Action.CopyReference({
        reference: (thread.comments->Array.getUnsafe(0)).reference->Option.getExn,
      }),
    )
    expect(
      Element.getAttribute(buttons->Array.getUnsafe(0), "title")->Nullable.toOption->Option.getExn,
    )->toContain("g y")
    FireEvent.click(buttons->Array.getUnsafe(1))
    expect(dispatch)->toHaveBeenLastCalledWith(
      Action.CopyReference({reference: thread.reference->Option.getExn}),
    )
    let commentId = (thread.comments->Array.getUnsafe(0)).id
    rerender(
      <Threads
        title="Conversation"
        threads=[{...thread, status: Resolved({by: thread.author, at: thread.created})}]
        focus={Thread({index: 0})}
        indexOffset=0
        dispatch
        chrome
        focusedComment=commentId
      />,
    )
    let comments = Element.querySelectorAll(container, ".thread-comments .thread-comment")
    expect(Array.length(comments))->toBe(1)
    expect(Element.getAttribute(comments->Array.getUnsafe(0), "id"))->toEqual(
      Nullable.make("comment-" ++ commentId),
    )
    expect(Element.className(comments->Array.getUnsafe(0)))->toContain("reference-target")
    FireEvent.click(Screen.queryAllByText("Copy reference")->Array.getUnsafe(0))
    expect(dispatch)->toHaveBeenLastCalledWith(
      Action.CopyReference({
        reference: (thread.comments->Array.getUnsafe(0)).reference->Option.getExn,
      }),
    )
  })

  testAsync("the shell copies a configured reference chord during the gesture", async () => {
    installClipboard("ok")
    let reference = thread().reference->Option.getExn
    let base = Fixtures.parse(View.ViewModel.schema, "client", "ViewModel", "default")
    let model = {
      ...base,
      copyReference: Some(reference),
      focusedComment: None,
      lastKey: None,
      bindings: [{keys: "y", command: CopyReference, label: "copy reference"}],
      tab: Conversation,
      focus: Thread({index: 0}),
    }
    let core: Core.t = {
      dispatch: _ => (),
      key: _ => (),
      subscribe: f => {
        f(model)
        () => ()
      },
      attach: () => (),
    }
    let _ = render(<App.Shell core />)
    pressKey("y")
    await flush()
    expect(copiedPaths())->toEqual([reference])
  })
})

%%raw(`
function referenceQuery(query) {
  window.history.replaceState(null, "", query || "/")
}
`)
@val external referenceQuery: string => unit = "referenceQuery"
@val external encodeURIComponent: string => string = "encodeURIComponent"

describe("Reference routes", () => {
  afterEach(() => referenceQuery("/"))
  test("opens the portable route once subscribed, ahead of a legacy review parameter", () => {
    let reference = "nits://context/review-box/review/00000000010000000000000002/comment/00000000030000000000000004"
    referenceQuery("?review=legacy&reference=" ++ encodeURIComponent(reference))
    let dispatch = fn()
    let core: Core.t = {
      dispatch,
      key: _ => (),
      subscribe: listener => {
        listener({...View.ViewModel.empty, connection: Subscribed({})})
        () => ()
      },
      attach: () => (),
    }
    let _ = render(<App.Shell core />)
    expect(dispatch)->toHaveBeenCalledWith(Action.OpenReference({reference: reference}))
  })
  test("preserves review-only links and delegates malformed references to the core", () => {
    let dispatch = fn()
    let core: Core.t = {
      dispatch,
      key: _ => (),
      subscribe: listener => {
        listener({...View.ViewModel.empty, connection: Subscribed({})})
        () => ()
      },
      attach: () => (),
    }
    referenceQuery("?review=00000000010000000000000002")
    let _ = render(<App.Shell core />)
    expect(dispatch)->toHaveBeenCalledWith(
      Action.OpenReview({reviewId: "00000000010000000000000002"}),
    )
    cleanup()
    referenceQuery("?reference=" ++ encodeURIComponent("<script>invalid</script>"))
    let _ = render(<App.Shell core />)
    expect(dispatch)->toHaveBeenLastCalledWith(
      Action.OpenReference({reference: "<script>invalid</script>"}),
    )
    expect(Element.querySelector(Document.body, "script"))->toBeNull
  })
})

test("a linked outdated finding opens a visible original pane instead of the current stack", () => {
  let base = Fixtures.parse(View.ViewModel.schema, "client", "ViewModel", "default")
  let diff = Fixtures.parse(View.DiffView.schema, "client", "DiffView", "default")
  let thread = {...base.threads->Array.getUnsafe(0), outdated: true}
  let selected = {
    ...base,
    tab: View.Tab.Conversation,
    threads: [thread],
    focus: View.Focus.Thread({index: 0}),
    focusedComment: Some(thread.root),
    draft: None,
  }
  let dispatch = fn()
  let push = ref(_ => ())
  let core: Core.t = {
    dispatch,
    key: _ => (),
    subscribe: listener => {
      push := listener
      listener(selected)
      () => ()
    },
    attach: () => (),
  }
  let {container} = render(<App.Shell core />)
  FireEvent.click(Screen.getByText("Open original diff"))
  expect(dispatch)->toHaveBeenCalledWith(Action.OpenOriginalDiff({threadId: thread.id}))
  act(() =>
    push.contents({
      ...selected,
      tab: FilesChanged,
      focus: Diff({row: 0, side: Head}),
      diff: Some({...diff, original: true, viewed: Viewed}),
    })
  )
  expect(Element.querySelector(container, ".original-banner"))->not_->toBeNull
  expect(Element.querySelector(container, ".diff-scroll"))->not_->toBeNull
  expect(Element.querySelector(container, ".diff-stack"))->toBeNull
  expect(Element.querySelector(container, ".diff-collapsed"))->toBeNull
  expect(Element.querySelector(container, ".diff-scroll.hidden"))->toBeNull
})
describe("Revision checkpoints", () => {
  test("current checking is disabled while content refreshes", () => {
    let dispatch = fn()
    render(<ReviewCheckpoints checkpoints=[] chrome=[] checkCurrentReady=false dispatch />)->ignore
    let button = Screen.getByText("Record current revision checked")
    expect(Element.getAttribute(button, "disabled")->Nullable.toOption)->toBe(Some(""))
    expect(Screen.getByText("Current changes are refreshing."))->toBeTruthy
    FireEvent.click(button)
    FireEvent.keyDown(button, {"key": "Enter", "ctrlKey": false})
    expect(dispatch)->toHaveBeenCalledTimes(0)
  })
  test(
    "shows checked provenance, changed status and keymap-backed actions without finding controls",
    () => {
      let status = Fixtures.parse(
        Domain.ReviewerCheckpoint.schema,
        "protocol",
        "ReviewerCheckpoint",
        "default",
      )
      let dispatch = fn()
      let chrome: array<View.Hint.t> = [
        {keys: "space k", command: CheckCurrent, label: "record current revision checked"},
        {keys: "space d", command: CheckpointDelta, label: "next checkpoint delta"},
      ]
      let {container} = render(<ReviewCheckpoints checkpoints=[status] chrome dispatch />)
      expect(Screen.getByText("Current target changed since check"))->toBeTruthy
      expect(Screen.getByText("Answers request 40"))->toBeTruthy
      expect(Element.textContent(container))->toContain(
        (status.checkpoint.targets->Array.getUnsafe(0)).head.tree,
      )
      expect(Screen.queryAllByText("Resolve finding")->Array.length)->toBe(0)
      FireEvent.click(Screen.getByText("Inspect next checkpoint delta"))
      expect(dispatch)->toHaveBeenCalledWith(Action.RunCommand({command: CheckpointDelta}))
      FireEvent.click(Screen.getByText("Record current revision checked"))
      expect(dispatch)->toHaveBeenCalledWith(Action.RunCommand({command: CheckCurrent}))
    },
  )
  test("request cards expose exact captured revisions and check the selected request", () => {
    let request = Fixtures.parse(
      Domain.ReviewRequest.schema,
      "protocol",
      "ReviewRequest",
      "default",
    )
    let captured = Fixtures.parse(
      Domain.RequestedTargets.schema,
      "protocol",
      "RequestedTargets",
      "Captured",
    )
    let dispatch = fn()
    let chrome: array<View.Hint.t> = [
      {keys: "v", command: CheckRequested, label: "record requested revision checked"},
    ]
    render(
      <ReviewRequests
        requests=[{...request, targets: captured}] focus={ReviewRequest({index: 0})} chrome dispatch
      />,
    )->ignore
    FireEvent.click(Screen.getByText("Record requested revision checked"))
    expect(dispatch)->toHaveBeenCalledWith(Action.SetFocus({focus: ReviewRequest({index: 0})}))
    expect(dispatch)->toHaveBeenCalledWith(Action.RunCommand({command: CheckRequested}))
    expect(Screen.getByText("Open requested changes"))->toBeTruthy
  })
})
