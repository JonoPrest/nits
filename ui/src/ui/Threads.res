// Thread list and the review conversation (§6.5).

open View

let authorName = (a: Domain.Author.t) =>
  switch a {
  | Human({name}) => name
  | Agent({name}) => name ++ " (agent)"
  | Daemon(_) => "nitsd"
  }

let placeText = (p: ThreadPlace.t) =>
  switch p {
  | Review(_) => "review"
  | File({file}) => file.path
  | Lines({file, start, end_}) =>
    file.path ++ ":" ++ Int.toString(start) ++ (end_ > start ? "-" ++ Int.toString(end_) : "")
  }

module Item = {
  @react.component
  let make = (
    ~thread: ThreadView.t,
    ~focused: bool,
    ~onSelect: unit => unit,
    ~onApply: unit => unit,
    ~onOriginal: unit => unit,
  ) => {
    let (focusRef, onKeyDown) = ThreadFocus.use(~focused)
    let flags =
      [
        thread.resolved ? "resolved" : "",
        thread.outdated ? "outdated" : "",
        thread.pending ? "pending" : "",
      ]->Array.filter(s => s != "")
    Attrs.focused(
      <li
        className={"thread-item " ++ flags->Array.join(" ")}
        ref={ReactDOM.Ref.domRef(focusRef)}
        tabIndex={focused ? 0 : -1}
        onKeyDown
        onClick={_ => onSelect()}
      >
        <div className="thread-meta">
          <span className="thread-author"> {React.string(authorName(thread.author))} </span>
          <span className="thread-place"> {React.string(placeText(thread.place))} </span>
          {thread.replies > 0
            ? <UI.Badge text={Int.toString(thread.replies) ++ " replies"} />
            : React.null}
          {thread.pending
            ? <span className="thread-pending"> {React.string("…")} </span>
            : React.null}
        </div>
        {focused
          ? <ul className="thread-comments">
              {thread.comments
              ->Array.map(c =>
                <li key=c.id className={"thread-comment" ++ (c.pending ? " pending" : "")}>
                  <div className="thread-meta">
                    <span className="thread-author"> {React.string(authorName(c.author))} </span>
                    <span title={Stepper.absolute(c.created)}>
                      {React.string(Stepper.relative(c.created))}
                    </span>
                    {c.pending
                      ? <span className="thread-pending"> {React.string("…")} </span>
                      : React.null}
                  </div>
                  <div className="thread-body">
                    <UI.Markdown source=c.body />
                  </div>
                </li>
              )
              ->React.array}
            </ul>
          : <div className="thread-summary"> {React.string(thread.summary)} </div>}
        {thread.suggestion
          ? <UI.Button label="Apply suggestion (a)" kind=Primary onClick=onApply />
          : React.null}
        {switch (thread.outdated, thread.context) {
        | (true, Some(_)) =>
          <UI.Button label="Open original diff (enter)" kind=Ghost onClick=onOriginal />
        | (true, None) | (false, _) => React.null
        }}
      </li>,
      focused,
    )
  }
}

@react.component
let make = (
  ~title: string,
  ~threads: array<ThreadView.t>,
  ~focus: Focus.t,
  ~indexOffset: int,
  ~dispatch: Action.t => unit,
) => {
  let focusedIndex = switch focus {
  | Thread({index}) => Some(index)
  | _ => None
  }
  <UI.Panel title>
    {Array.length(threads) == 0
      ? <UI.Empty text="No threads." />
      : <ul role="list">
          {threads
          ->Array.mapWithIndex((t, i) =>
            <Item
              key=t.id
              thread=t
              focused={focusedIndex == Some(indexOffset + i)}
              onSelect={() => {
                dispatch(SetFocus({focus: Focus.Thread({index: indexOffset + i})}))
                switch (t.outdated, t.context, t.place) {
                | (true, Some(_), _) => dispatch(OpenOriginalDiff({threadId: t.id}))
                | (_, _, Lines({file, start})) =>
                  dispatch(
                    Viewport({
                      file,
                      firstRow: Int.toFloat(start - 30)->Math.max(0.)->Float.toInt,
                      lastRow: start + 30,
                    }),
                  )
                | (_, _, File({file})) => dispatch(Viewport({file, firstRow: 0, lastRow: 59}))
                | (_, _, Review(_)) => ()
                }
              }}
              onApply={() => dispatch(ApplySuggestion({commentId: t.root}))}
              onOriginal={() => dispatch(OpenOriginalDiff({threadId: t.id}))}
            />
          )
          ->React.array}
        </ul>}
  </UI.Panel>
}
