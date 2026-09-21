// An inline comment thread under its anchored diff row (UI-DESIGN
// §Comments: "inline threads are primary"). The reply composer renders
// inside the card while a reply to this thread is being written.

open View

@react.component
let make = (
  ~repositories: RepositoryIdentity.context=Unavailable,
  ~thread: ThreadView.t,
  ~chrome: array<Hint.t>=[],
  ~focused: bool,
  ~index: int,
  ~composer: React.element,
  ~dispatch: Action.t => unit,
) => {
  let (focusRef, onKeyDown) = ThreadFocus.use(~focused)
  let flags =
    [
      Threads.isResolved(thread.status) ? "resolved" : "",
      thread.outdated ? "outdated" : "",
      thread.pending ? "pending" : "",
    ]->Array.filter(s => s != "")
  Attrs.focused(
    <div
      className={["inline-thread", ...flags]->Array.join(" ")}
      role="note"
      ariaLabel={Threads.placeText(~repositories, thread.place)}
      ref={ReactDOM.Ref.domRef(focusRef)}
      tabIndex={focused ? 0 : -1}
      onKeyDown
      onClick={ev => {
        ReactEvent.Mouse.stopPropagation(ev)
        dispatch(SetFocus({focus: Focus.Thread({index: index})}))
      }}
    >
      <span
        className="thread-place" title={Threads.placeText(~repositories, ~full=true, thread.place)}
      >
        {React.string(Threads.placeText(~repositories, thread.place))}
      </span>
      <Threads.Context context=thread.context />
      {thread.comments
      ->Array.map(c =>
        <div key=c.id className={"inline-comment" ++ (c.pending ? " pending" : "")}>
          <div className="thread-meta">
            <span onClick={ev => ReactEvent.Mouse.stopPropagation(ev)}>
              <UI.CopyReference reference=c.reference chrome dispatch />
            </span>
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
          {switch c.suggestion {
          | Some(suggestion) => <SuggestionCard suggestion repositories chrome dispatch />
          | None => React.null
          }}
        </div>
      )
      ->React.array}
      <Threads.Disposition status=thread.status />
      {switch composer {
      | c if c != React.null => c
      | _ =>
        <div className="inline-thread-actions" onClick={ev => ReactEvent.Mouse.stopPropagation(ev)}>
          <UI.CopyReference reference=thread.reference chrome dispatch />
          <UI.Button
            label="Reply"
            title=?{Chrome.tip(chrome, Reply)}
            kind=Primary
            onClick={() => dispatch(ReplyOpened({threadId: thread.id}))}
          />
          <Threads.FindingActions thread chrome dispatch />
        </div>
      }}
    </div>,
    focused,
  )
}
