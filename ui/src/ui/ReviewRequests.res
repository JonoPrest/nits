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
        {switch request.targets {
        | UnknownTargets(_) =>
          <p> {React.string("Requested revision unknown (historical request)")} </p>
        | Captured({targets}) => <ReviewCheckpoints.Targets targets />
        }}
        {switch request.checkpointComparison {
        | UnknownComparison(_) =>
          <p> {React.string("Checkpoint comparison unknown (historical request)")} </p>
        | NoCheckpoint(_) => React.null
        | Compared({checkpointId, outcome: SameTargets}) =>
          <p>
            {React.string(
              "Unchanged since checkpoint " ++
              Float.toString(
                checkpointId,
              ) ++ ". New work pushed elsewhere may need fetching and selecting before another round.",
            )}
          </p>
        | Compared({checkpointId, outcome: UnknownRevision}) =>
          <p>
            {React.string(
              "Cannot compare exact revisions with checkpoint " ++
              Float.toString(checkpointId) ++ ": captured HEAD identity is unavailable.",
            )}
          </p>
        | Compared({checkpointId, outcome: ChangedTargets}) =>
          <p> {React.string("Targets differ from checkpoint " ++ Float.toString(checkpointId))} </p>
        }}
        <div onClick={ev => ReactEvent.Mouse.stopPropagation(ev)}>
          {switch request.targets {
          | UnknownTargets(_) => React.null
          | Captured(_) =>
            <UI.Button
              label="Record requested revision checked"
              title=?{Chrome.tip(chrome, CheckRequested)}
              onClick={() => {
                select()
                dispatch(RunCommand({command: CheckRequested}))
              }}
            />
          }}
          <UI.Button
            label={switch request.targets {
            | UnknownTargets(_) => "Open current changes"
            | Captured(_) => "Open requested changes"
            }}
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
