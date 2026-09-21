// File search owns the query and selection in the core; the shared
// navigation model owns the input/result focus zones.
open View

@react.component
let make = (
  ~search: SearchView.t,
  ~repositories: RepositoryIdentity.context=Unavailable,
  ~bindings: array<View.Hint.t>=[],
  ~dispatch: Action.t => unit,
) => {
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
    _,
  ) = SearchNavigation.useNavigation(
    ~bindings,
    ~count=Array.length(search.hits),
    ~selected=search.selected,
    ~first=() => dispatch(SearchFirst({search: Files})),
    ~step=delta => dispatch(SearchStep({search: Files, delta})),
    ~query=search.query,
    ~change=query => dispatch(FileSearch({query: Some(query)})),
    ~submit=() => dispatch(OpenSearchResult({search: Files, query: search.query})),
    ~close=() => dispatch(FileSearch({query: None})),
  )
  let id = React.useId()
  let hitId = i => id ++ "-" ++ Int.toString(i)
  <div className="search-box panel" role="search">
    <UI.TextInput
      autoFocus=true
      placeholder="file…"
      value=search.query
      onChange
      inputRef={ReactDOM.Ref.domRef(inputRef)}
      onFocus=toInput
      onKeyEvent=onInputKey
    />
    <UI.SearchResults
      kind=Files
      label="files"
      listRef={ReactDOM.Ref.domRef(resultsRef)}
      onKey=onResultsKey
      onFocus=onResultsFocus
      onPointer=onResultsPointer
      onBlur=onResultsBlur
      activeId={selected->Option.map(hitId)}
    >
      {search.hits
      ->Array.mapWithIndex((h, i) =>
        <div
          key={h.file.repoId ++ h.file.path ++ Int.toString(i)}
          id={hitId(i)}
          role="option"
          ariaLabel={RepositoryIdentity.fileText(repositories, h.file)}
          ariaSelected={selected == Some(i)}
          className={"search-hit" ++ (selected == Some(i) ? " selected" : "")}
          onClick={_ => dispatch(Viewport({file: h.file, firstRow: 0, lastRow: 59}))}
        >
          <RepositoryIdentity.File repositories file=h.file />
        </div>
      )
      ->React.array}
      {Array.length(search.hits) == 0 ? <UI.Empty text="no files match" /> : React.null}
    </UI.SearchResults>
  </div>
}
