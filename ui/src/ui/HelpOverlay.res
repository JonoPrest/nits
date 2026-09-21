// `?` overlay (§6.4): every binding of the focused context plus Global,
// searchable, showing overrides and conflicts.

open View

let resetPanel: Dom.element => unit = %raw(`el => { el.scrollTop = 0 }`)

let compact = (text: string) => text->String.toLowerCase->String.replaceRegExp(/\s+/g, "")

// Fuzzy subsequence match. Exact and prefix matches sort before sparse
// subsequences; otherwise tighter, earlier matches receive the lower score.
let fuzzyScore = (haystack: string, needle: string): option<int> => {
  let haystack = compact(haystack)
  let needle = compact(needle)
  if needle == "" {
    Some(0)
  } else if haystack == needle {
    Some(-10000)
  } else if haystack->String.startsWith(needle) {
    Some(-5000 + String.length(haystack) - String.length(needle))
  } else {
    let from = ref(0)
    let first = ref(-1)
    let last = ref(-1)
    let found = ref(true)
    needle
    ->String.split("")
    ->Array.forEach(ch =>
      if found.contents {
        switch haystack->String.indexOfFrom(ch, from.contents) {
        | -1 => found := false
        | index => {
            if first.contents == -1 {
              first := index
            }
            last := index
            from := index + 1
          }
        }
      }
    )
    found.contents
      ? Some(
          first.contents * 100 +
          (last.contents - first.contents - String.length(needle) + 1) * 10 +
          String.length(haystack),
        )
      : None
  }
}

let entryScore = (entry: HelpEntry.t, query: string): option<int> =>
  switch (fuzzyScore(entry.keys, query), fuzzyScore(entry.label, query)) {
  | (Some(keys), Some(label)) => Some(Math.Int.min(keys, label + 1))
  | (Some(keys), None) => Some(keys)
  | (None, Some(label)) => Some(label + 1)
  | (None, None) => None
  }

let rankedEntries = (entries: array<HelpEntry.t>, query: string): array<HelpEntry.t> =>
  if compact(query) == "" {
    entries
  } else {
    entries
    ->Array.mapWithIndex((entry, index) => (entry, index))
    ->Array.filterMap(((entry, index)) =>
      entryScore(entry, query)->Option.map(score => (entry, score, index))
    )
    ->Array.toSorted(((_, a, ai), (_, b, bi)) => {
      let order = Int.compare(a, b)
      order == 0. ? Int.compare(ai, bi) : order
    })
    ->Array.map(((entry, _, _)) => entry)
  }

let filteredGroups = (groups: array<HelpGroup.t>, query: string): array<HelpGroup.t> =>
  groups->Array.filterMap(group => {
    let entries = rankedEntries(group.entries, query)
    Array.length(entries) == 0 ? None : Some({...group, entries})
  })

let conflictMatches = (conflict: Conflict.t, query: string) =>
  compact(query) == "" ||
  fuzzyScore(conflict.keys, query)->Option.isSome ||
  conflict.commands->Array.some(command => fuzzyScore((command :> string), query)->Option.isSome)

@react.component
let make = (~help: HelpView.t, ~bindings: array<View.Hint.t>=[], ~dispatch: Action.t => unit) => {
  let (query, setQuery) = React.useState(() => "")
  let groups = filteredGroups(help.groups, query)
  let conflicts = help.conflicts->Array.filter(conflict => conflictMatches(conflict, query))
  let entries = groups->Array.flatMap(g => g.entries->Array.map(e => (g.context, e)))
  let (selection, setSelection) = React.useState(() => 0)
  let count = Array.length(entries)
  let close = () => dispatch(ToggleHelp({}))
  let submit = () =>
    switch entries[Math.Int.min(selection, count - 1)] {
    | Some((_, entry)) => {
        close()
        dispatch(RunCommand({command: entry.command}))
      }
    | None => ()
    }
  let panelRef = React.useRef(Nullable.null)
  let (
    selected,
    inputRef,
    resultsRef,
    toInput,
    onResultsFocus,
    onResultsPointer,
    onResultsBlur,
    onChange,
    onInputKey,
    onResultsKey,
    onDialogKey,
  ) = SearchNavigation.useNavigation(
    ~bindings,
    ~count,
    ~selected=selection,
    ~first=() => setSelection(_ => 0),
    ~step=delta =>
      setSelection(current =>
        Math.Int.min(Math.Int.max(Math.Int.min(current, count - 1) + delta, 0), count - 1)
      ),
    ~query,
    ~change=value => {
      setQuery(_ => value)
      setSelection(_ => 0)
    },
    ~submit,
    ~close,
  )
  let id = React.useId()
  let hitId = index => id ++ "-" ++ Int.toString(index)
  let selectedEntry = selected->Option.flatMap(i => entries[i])
  React.useEffect1(() => {
    panelRef.current->Nullable.toOption->Option.forEach(resetPanel)
    None
  }, [query])
  <div
    className="help-overlay"
    role="dialog"
    ariaLabel="keyboard help"
    onKeyDown={ev => onDialogKey(ev)}
  >
    <div className="help-panel panel" ref={ReactDOM.Ref.domRef(panelRef)}>
      <header className="panel-header"> {React.string("Keyboard")} </header>
      <UI.TextInput
        value=query
        autoFocus=true
        placeholder="filter…"
        onChange
        inputRef={ReactDOM.Ref.domRef(inputRef)}
        onFocus=toInput
        onKeyEvent=onInputKey
      />
      <UI.SearchResults
        kind=Help
        label="keyboard shortcuts"
        listRef={ReactDOM.Ref.domRef(resultsRef)}
        onKey=onResultsKey
        onFocus=onResultsFocus
        onPointer=onResultsPointer
        onBlur=onResultsBlur
        activeId={selected->Option.map(hitId)}
      >
        {Array.length(groups) == 0 && Array.length(conflicts) == 0
          ? <UI.Empty text="no shortcuts match" />
          : React.null}
        {groups
        ->Array.map(g =>
          <section key={(g.context :> string)} className="help-group">
            <h3> {React.string((g.context :> string))} </h3>
            <table>
              <tbody>
                {g.entries
                ->Array.map(e =>
                  <tr
                    key={e.keys ++ e.label}
                    role="option"
                    id={hitId(entries->Array.findIndex(entry => entry == (g.context, e)))}
                    ariaSelected={selectedEntry == Some((g.context, e))}
                    className={(e.overridden ? "help-overridden" : "") ++ (
                      selectedEntry == Some((g.context, e)) ? " search-hit selected" : ""
                    )}
                    onClick={_ => {
                      close()
                      dispatch(RunCommand({command: e.command}))
                    }}
                  >
                    <td>
                      <UI.Kbd keys=e.keys />
                    </td>
                    <td> {React.string(e.label)} </td>
                    <td> {React.string(e.primary ? "★" : "")} </td>
                  </tr>
                )
                ->React.array}
              </tbody>
            </table>
          </section>
        )
        ->React.array}
        {Array.length(conflicts) > 0
          ? <section className="help-conflicts">
              <h3> {React.string("Conflicts")} </h3>
              {conflicts
              ->Array.map(c =>
                <div key={(c.context :> string) ++ c.keys}>
                  <UI.Kbd keys=c.keys />
                  {React.string(" in " ++ (c.context :> string) ++ ": ")}
                  {React.string(c.commands->Array.map(cmd => (cmd :> string))->Array.join(", "))}
                </div>
              )
              ->React.array}
            </section>
          : React.null}
      </UI.SearchResults>
      <UI.Button label="Close" title=?{Chrome.tip(bindings, Back)} kind=Ghost onClick=close />
    </div>
  </div>
}
