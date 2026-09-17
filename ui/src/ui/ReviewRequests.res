// Durable invitations have their own navigation and no finding lifecycle.
module Item = {
  @react.component
  let make = (
    ~request: Domain.ReviewRequest.t,
    ~index: int,
    ~focused: bool,
    ~chrome: array<View.Hint.t>,
    ~dispatch: Action.t => unit,
  ) => {
    let (focusRef, onKeyDown) = ThreadFocus.use(~focused)
    let select = () => dispatch(SetFocus({focus: ReviewRequest({index: index})}))
    Attrs.focused(
      <li
        className="thread-item review-request"
        ref={ReactDOM.Ref.domRef(focusRef)}
        tabIndex={focused ? 0 : -1}
        onKeyDown
        onClick={_ => select()}
      >
        <div className="thread-meta">
          <span className="thread-author">
            {React.string(Threads.authorName(request.requester))}
          </span>
          <UI.Badge text="review request" />
          <span> {React.string("to " ++ request.recipient)} </span>
          <span title={Stepper.absolute(request.created)}>
            {React.string(Stepper.relative(request.created))}
          </span>
        </div>
        <div className="thread-body">
          <UI.Markdown source=request.note />
        </div>
        <div onClick={ev => ReactEvent.Mouse.stopPropagation(ev)}>
          <UI.Button
            label="Open changes"
            title=?{Chrome.tip(chrome, Open)}
            onClick={() => {
              select()
              dispatch(RunCommand({command: Open}))
            }}
          />
        </div>
      </li>,
      focused,
    )
  }
}

@react.component
let make = (
  ~requests: array<Domain.ReviewRequest.t>,
  ~focus: View.Focus.t,
  ~chrome: array<View.Hint.t>,
  ~dispatch: Action.t => unit,
) =>
  if Array.length(requests) == 0 {
    React.null
  } else {
    <section ariaLabel="Review requests" className="threads panel">
      <header className="panel-header">
        <UI.Button
          label={"Review requests (" ++ Int.toString(Array.length(requests)) ++ ")"}
          title=?{Chrome.tip(chrome, FocusRequests)}
          onClick={() => dispatch(RunCommand({command: FocusRequests}))}
        />
      </header>
      <ul className="thread-list">
        {requests
        ->Array.mapWithIndex((request, index) =>
          <Item
            key={Float.toString(request.id)}
            request
            index
            focused={focus == ReviewRequest({index: index})}
            chrome
            dispatch
          />
        )
        ->React.array}
      </ul>
    </section>
  }
