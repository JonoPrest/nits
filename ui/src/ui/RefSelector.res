open Domain

let kind = spec =>
  switch spec {
  | RefSpec.Branch(_) => "branch"
  | Tag(_) => "tag"
  | Commit(_) => "commit"
  | WorkingTree(_) => "working tree"
  | Upstream(_) => "upstream"
  | Head(_) => "head"
  }

let value = spec =>
  switch spec {
  | RefSpec.Branch({name}) | Tag({name}) => name
  | Commit({oid}) => String.slice(oid, ~start=0, ~end=8)
  | WorkingTree(_) => "Working tree"
  | Upstream(_) => "@{upstream}"
  | Head(_) => "HEAD"
  }

module Picker = {
  @react.component
  let make = (~selector: View.RefSelectorView.t, ~dispatch: Action.t => unit) => {
    let side = switch selector.purpose {
    | Review({side: Base}) => "Base"
    | Review({side: Head}) => "Head"
    | Browse(_) => "Browse"
    }
    // Keep keystrokes local until Core replies; old query patches never erase
    // more recent typing. Request identity remounts this buffer on every open.
    let (text, setText) = React.useState(() => selector.query)
    let current = text == selector.query
    let busy = switch selector.status {
    | Loading(_) | Saving(_) => true
    | Ready(_) | InvalidRef(_) | DaemonError(_) => false
    }
    let close = () => dispatch(Action.CloseRefSelector({}))
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
      ~count=busy || !current ? 0 : Array.length(selector.options),
      ~selected=selector.selected,
      ~first=() => dispatch(Action.RefSelectorStep({delta: -selector.selected})),
      ~step=(delta: int) => dispatch(Action.RefSelectorStep({delta: delta})),
      ~query=text,
      ~change=(query: string) => {
        setText(_ => query)
        dispatch(Action.RefSelectorQuery({query: query}))
      },
      ~submit=() => {
        if !busy {
          dispatch(Action.SelectCurrentRef({}))
        }
      },
      ~close,
    )
    let id = React.useId()
    let optionId = index => id ++ "-" ++ Int.toString(index)
    <div
      className="palette-overlay"
      role="dialog"
      ariaLabel={side ++ " revision selector"}
      onKeyDown={ev => SearchNavigation.onDialogKey(close, ev)}
    >
      <div className="palette ref-selector">
        <div className="palette-tabs">
          <strong> {React.string(selector.repoName ++ " · " ++ side)} </strong>
          <span className="palette-hint">
            <UI.Kbd keys="↓" />
            {React.string(" results · ")}
            <UI.Kbd keys="enter" />
            {React.string(" select · ")}
            <UI.Kbd keys="esc" />
            {React.string(" cancel")}
          </span>
        </div>
        <UI.TextInput
          value=text
          autoFocus=true
          placeholder={"Find a " ++ String.toLowerCase(side) ++ " revision"}
          onChange
          inputRef={ReactDOM.Ref.domRef(inputRef)}
          onFocus=toInput
          onKeyEvent=onInputKey
        />
        {switch selector.purpose {
        | Browse(_) =>
          <p>
            {React.string(
              "Search refs or enter branch:name, tag:name, commit:<oid>, head, upstream, or worktree.",
            )}
          </p>
        | Review(_) => React.null
        }}
        {switch selector.status {
        | Loading(_) => <UI.Empty text="Loading branches, tags, and recent commits…" />
        | Saving(_) =>
          <UI.Empty text={side == "Browse" ? "Loading revision…" : "Updating review target…"} />
        | InvalidRef({message}) =>
          <div className="ref-selector-error" role="alert">
            {React.string("Invalid ref: " ++ message)}
          </div>
        | DaemonError({message}) =>
          <div className="ref-selector-error" role="alert">
            {React.string("Daemon error: " ++ message)}
          </div>
        | Ready(_) => React.null
        }}
        {busy || !current
          ? React.null
          : <UI.SearchResults
              kind=Palette
              label="revisions"
              listRef={ReactDOM.Ref.domRef(resultsRef)}
              onKey=onResultsKey
              onFocus=onResultsFocus
              activeId={selected->Option.map(optionId)}
            >
              {selector.options
              ->Array.mapWithIndex((option, index) => {
                let current = option.current ? " · current" : ""
                let subject = option.subject->Option.mapOr("", text => " · " ++ text)
                let label = value(option.refSpec) ++ subject ++ current
                <div
                  key={kind(option.refSpec) ++ ":" ++ value(option.refSpec)}
                  id={optionId(index)}
                  role="option"
                  ariaSelected={selected == Some(index)}
                  className={"ref-selector-option" ++ (selected == Some(index) ? " selected" : "")}
                  onClick={_ => dispatch(Action.SelectRef({index: index}))}
                >
                  <UI.Badge text={kind(option.refSpec)} tone=Neutral />
                  <span> {React.string(label)} </span>
                </div>
              })
              ->React.array}
              {Array.length(selector.options) == 0
                ? <UI.Empty text="No matching refs" />
                : React.null}
            </UI.SearchResults>}
      </div>
    </div>
  }
}

@react.component
let make = (~selector: View.RefSelectorView.t, ~dispatch: Action.t => unit) =>
  <Picker key={Float.toString(selector.requestId)} selector dispatch />
