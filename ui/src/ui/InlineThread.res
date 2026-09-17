// An inline comment thread under its anchored diff row (UI-DESIGN
// §Comments: "inline threads are primary"). The reply composer renders
// inside the card while a reply to this thread is being written.

open View

@react.component
let make = (
  ~thread: ThreadView.t,
  ~focused: bool,
  ~index: int,
  ~composer: React.element,
  ~dispatch: Action.t => unit,
) => {
  let (focusRef, onKeyDown) = ThreadFocus.use(~focused)
  let flags =
    [
      thread.status == Resolved ? "resolved" : "",
      thread.outdated ? "outdated" : "",
      thread.pending ? "pending" : "",
    ]->Array.filter(s => s != "")
  Attrs.focused(
    <div
      className={["inline-thread", ...flags]->Array.join(" ")}
      role="note"
      ref={ReactDOM.Ref.domRef(focusRef)}
      tabIndex={focused ? 0 : -1}
      onKeyDown
      onClick={ev => {
        ReactEvent.Mouse.stopPropagation(ev)
        dispatch(SetFocus({focus: Focus.Thread({index: index})}))
      }}
    >
      <Threads.Context context=thread.context />
      {thread.comments
      ->Array.map(c =>
        <div key=c.id className={"inline-comment" ++ (c.pending ? " pending" : "")}>
          <div className="thread-meta">
            <span className="thread-author"> {React.string(Threads.authorName(c.author))} </span>
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
        </div>
      )
      ->React.array}
      {switch composer {
      | c if c != React.null => c
      | _ =>
        <div className="inline-thread-actions" onClick={ev => ReactEvent.Mouse.stopPropagation(ev)}>
          <UI.Button
            label="Reply (r)"
            kind=Primary
            onClick={() => dispatch(ReplyOpened({threadId: thread.id}))}
          />
          {thread.suggestion
            ? <UI.Button
                label="Apply suggestion (a)"
                onClick={() => dispatch(ApplySuggestion({commentId: thread.root}))}
              />
            : React.null}
          {thread.status == Informational
            ? React.null
            : <UI.Button
                label={thread.status == Resolved ? "Unresolve (x)" : "Resolve (x)"}
                kind=Ghost
                onClick={() =>
                  dispatch(
                    thread.status == Resolved
                      ? UnresolveThread({threadId: thread.id})
                      : ResolveThread({threadId: thread.id}),
                  )}
              />}
        </div>
      }}
    </div>,
    focused,
  )
}
