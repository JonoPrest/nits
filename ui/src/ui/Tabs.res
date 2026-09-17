// The tab row (UI-DESIGN Layout): Files changed / Conversation / Browse,
// keys 1/2/3. A click is an alias for the chord; tooltips from the chrome.

@react.component
let make = (
  ~tab: View.Tab.t,
  ~fileCount: int,
  ~threadCount: int,
  ~requestCount: int=0,
  ~chrome: array<View.Hint.t>,
  ~dispatch: Action.t => unit,
) => {
  let item = (target: View.Tab.t, command: View.Command.t, label: string, count: option<int>) => {
    let active = tab == target
    let el =
      <button
        key=label
        type_="button"
        role="tab"
        ariaSelected=active
        className="tab"
        title=?{Chrome.tip(chrome, command)}
        onClick={_ => dispatch(SetTab({tab: target}))}
      >
        {React.string(label)}
        {target == Conversation && requestCount > 0
          ? <UI.Badge text={Int.toString(requestCount) ++ " requests"} />
          : React.null}
        {switch count {
        | Some(n) => <UI.Badge text={Int.toString(n) ++ (target == Conversation ? " open" : "")} />
        | None => React.null
        }}
      </button>
    active ? Attrs.withData(el, [("data-active", "true")]) : el
  }
  <nav className="tab-row" role="tablist">
    {item(FilesChanged, TabFiles, "Files changed", Some(fileCount))}
    {item(Conversation, TabConversation, "Conversation", Some(threadCount))}
    {item(Browse, TabBrowse, "Browse", None)}
  </nav>
}
