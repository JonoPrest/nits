// The palette (UI-DESIGN §Search): one overlay, `F` opens content
// search, `:` opens actions; Tab enters results. Content results
// come from the daemon (`ViewModel.contentSearch`); actions are the
// keymap chrome — never hand-written.

open View

type mode = Content | Actions

@react.component
let make = (
  ~repositories: RepositoryIdentity.context=Unavailable,
  ~contentSearch: option<ContentSearchView.t>,
  ~actionPalette: bool,
  ~chrome: array<Hint.t>,
  ~dispatch: Action.t => unit,
) => {
  let mode = actionPalette ? Actions : Content
  let (text, setText) = React.useState(() => contentSearch->Option.mapOr("", c => c.query))
  // Actions-mode selection is UI state: the filtered list lives here.
  let (actionSel, setActionSel) = React.useState(() => 0)
  let close = () =>
    switch mode {
    | Content => dispatch(ContentSearch({query: None, allFiles: false}))
    | Actions => dispatch(ActionPalette({open_: false}))
    }
  let switchMode = () => {
    setText(_ => "")
    setActionSel(_ => 0)
    switch mode {
    | Content => {
        dispatch(ContentSearch({query: None, allFiles: false}))
        dispatch(ActionPalette({open_: true}))
      }
    | Actions => {
        dispatch(ActionPalette({open_: false}))
        dispatch(ContentSearch({query: Some(""), allFiles: false}))
      }
    }
  }
  let allFiles = contentSearch->Option.mapOr(false, c => c.allFiles)
  // Fuzzy subsequence match over the action's config name
  // (`toggle_layout`) and its label; lower score (earlier hits) first.
  let toSnake = (name: string) =>
    name->String.replaceRegExp(/([a-z0-9])([A-Z])/g, "$1_$2")->String.toLowerCase
  let fuzzy = (hay: string, needle: string): option<int> => {
    let score = ref(0)
    let from = ref(0)
    let ok = ref(true)
    needle
    ->String.split("")
    ->Array.forEach(ch =>
      if ok.contents {
        switch hay->String.indexOfFrom(ch, from.contents) {
        | -1 => ok := false
        | i => {
            score := score.contents + i
            from := i + 1
          }
        }
      }
    )
    ok.contents ? Some(score.contents) : None
  }
  let actions = {
    let q = String.toLowerCase(String.trim(text))
    chrome
    ->Array.filterMap(h => {
      if q == "" {
        Some((h, 0))
      } else {
        let name = toSnake((h.command :> string))
        switch (fuzzy(name, q), fuzzy(String.toLowerCase(h.label), q)) {
        | (Some(a), Some(b)) => Some((h, Math.Int.min(a, b)))
        | (Some(a), None) => Some((h, a))
        | (None, Some(b)) => Some((h, b))
        | (None, None) => None
        }
      }
    })
    ->Array.toSorted(((_, a), (_, b)) => Int.compare(a, b))
    ->Array.map(((h, _)) => h)
  }
  let openHit = (h: Domain.ContentHit.t) => {
    dispatch(ContentSearch({query: None, allFiles: false}))
    dispatch(
      Viewport({
        file: {repoId: h.repoId, path: h.path},
        firstRow: Int.toFloat(h.line - 30)->Math.max(0.)->Float.toInt,
        lastRow: h.line + 30,
      }),
    )
  }
  let submit = () =>
    switch mode {
    | Content =>
      // Results for this query on screen: Enter opens the highlighted
      // one; otherwise it (re)runs the search.
      switch contentSearch {
      | Some(c) if c.query == text && !c.pending =>
        dispatch(OpenSearchResult({search: Content, query: text}))
      | _ => dispatch(ContentSearch({query: Some(text), allFiles}))
      }
    | Actions =>
      switch actions->Array.get(Math.Int.min(actionSel, Array.length(actions) - 1)) {
      | Some(h) => {
          dispatch(ActionPalette({open_: false}))
          dispatch(RunCommand({command: h.command}))
        }
      | None => ()
      }
    }
  let step = delta =>
    switch mode {
    | Content => dispatch(SearchStep({search: Content, delta}))
    | Actions => {
        let n = Array.length(actions)
        setActionSel(sel =>
          n == 0 ? 0 : Math.Int.min(Math.Int.max(Math.Int.min(sel, n - 1) + delta, 0), n - 1)
        )
      }
    }
  let count = switch mode {
  | Content =>
    contentSearch->Option.mapOr(0, c => c.pending || c.query != text ? 0 : Array.length(c.hits))
  | Actions => Array.length(actions)
  }
  let current = switch mode {
  | Content => contentSearch->Option.mapOr(0, c => c.selected)
  | Actions => actionSel
  }
  let (
    selected,
    inputRef,
    resultsRef,
    toInput,
    onResultsFocus,
    onChange,
    onInputKey,
    onResultsKey,
  ) = SearchNavigation.useNavigation(
    ~count,
    ~selected=current,
    ~first=() =>
      switch mode {
      | Content => dispatch(SearchFirst({search: Content}))
      | Actions => setActionSel(_ => 0)
      },
    ~step,
    ~query=text,
    ~change=value => {
      setText(_ => value)
      setActionSel(_ => 0)
    },
    ~submit,
    ~close,
  )
  let id = React.useId()
  let hitId = i => id ++ "-" ++ Int.toString(i)
  <div
    className="palette-overlay"
    role="dialog"
    ariaLabel="palette"
    onKeyDown={ev => SearchNavigation.onDialogKey(close, ev)}
  >
    <div className="palette">
      <div className="palette-tabs">
        <span className={"palette-tab" ++ (mode == Content ? " active" : "")}>
          {React.string("Content")}
        </span>
        <span className={"palette-tab" ++ (mode == Actions ? " active" : "")}>
          {React.string("Actions")}
        </span>
      </div>
      <UI.TextInput
        value=text
        autoFocus=true
        placeholder={mode == Content ? "search file contents (enter)" : "run a command"}
        onChange
        inputRef={ReactDOM.Ref.domRef(inputRef)}
        onFocus=toInput
        onKeyEvent=onInputKey
      />
      {switch mode {
      | Content => {
          let cs = contentSearch
          <div className="palette-results">
            {switch cs {
            | Some(c) if c.query != text => <UI.Empty text="press Enter to search" />
            | Some(c) if c.pending => <UI.Empty text="searching…" />
            | Some(c) if c.query != "" && Array.length(c.hits) == 0 =>
              <UI.Empty text="no matches" />
            | Some(c) =>
              <UI.SearchResults
                kind=Palette
                label="content matches"
                listRef={ReactDOM.Ref.domRef(resultsRef)}
                onKey=onResultsKey
                onFocus=onResultsFocus
                activeId={selected->Option.map(hitId)}
              >
                {c.hits
                ->Array.mapWithIndex((h, i) =>
                  <div
                    key={h.repoId ++ h.path ++ Int.toString(i)}
                    id={hitId(i)}
                    role="option"
                    ariaLabel={RepositoryIdentity.fileText(
                      repositories,
                      {repoId: h.repoId, path: h.path},
                    ) ++
                    ":" ++
                    Int.toString(h.line)}
                    ariaSelected={selected == Some(i)}
                    className={"search-hit" ++ (selected == Some(i) ? " selected" : "")}
                    onClick={_ => openHit(h)}
                  >
                    <span className="hit-path">
                      <RepositoryIdentity.File
                        repositories file={{repoId: h.repoId, path: h.path}}
                      />
                      {React.string(":" ++ Int.toString(h.line))}
                    </span>
                    <span className="hit-text"> {React.string(h.text)} </span>
                  </div>
                )
                ->React.array}
                {c.truncated
                  ? <div className="palette-truncated">
                      {React.string("more matches not shown")}
                    </div>
                  : React.null}
              </UI.SearchResults>
            | None => React.null
            }}
            <label className="palette-scope">
              <input
                type_="checkbox"
                checked=allFiles
                onKeyDown={ev => {
                  SearchNavigation.onDialogKey(close, ev)
                  ReactEvent.Keyboard.stopPropagation(ev)
                }}
                onChange={_ =>
                  dispatch(
                    ContentSearch({
                      query: Some(text),
                      allFiles: !allFiles,
                    }),
                  )}
              />
              {React.string(" all files (not just changed)")}
            </label>
          </div>
        }
      | Actions =>
        <UI.SearchResults
          kind=Palette
          label="actions"
          listRef={ReactDOM.Ref.domRef(resultsRef)}
          onKey=onResultsKey
          onFocus=onResultsFocus
          activeId={selected->Option.map(hitId)}
        >
          {actions
          ->Array.mapWithIndex((h, i) =>
            <div
              key={h.keys ++ h.label}
              id={hitId(i)}
              role="option"
              ariaSelected={selected == Some(i)}
              className={"search-hit" ++ (selected == Some(i) ? " selected" : "")}
              onClick={_ => {
                dispatch(ActionPalette({open_: false}))
                dispatch(RunCommand({command: h.command}))
              }}
            >
              <span className="hit-text"> {React.string(h.label)} </span>
              <UI.Kbd keys=h.keys />
            </div>
          )
          ->React.array}
        </UI.SearchResults>
      }}
      <UI.Button
        label={mode == Content ? "Actions" : "Content"}
        kind=Ghost
        title=?{Chrome.tip(chrome, mode == Content ? ActionPalette : ContentSearch)}
        onClick={() => {
          toInput()
          switchMode()
        }}
      />
      <UI.Button label="close ⎋" kind=Ghost onClick=close />
    </div>
  </div>
}
