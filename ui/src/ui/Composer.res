// The comment editor (§5.3): its text never enters the core; only
// comment submission, deferral details and discard cross the boundary.
// Ctrl/Cmd+Enter submits, Esc discards.

@react.component
let make = (
  ~draft: View.Draft.t,
  ~pendingRefresh: bool,
  ~dispatch: Action.t => unit,
  ~chrome: array<View.Hint.t>=[],
) => {
  let (body, setBody) = React.useState(() => "")
  let (trackingUrl, setTrackingUrl) = React.useState(() => "")
  let submit = () =>
    if String.trim(body) != "" {
      switch draft.purpose {
      | Defer({threadId}) =>
        dispatch(
          DeferThread({
            threadId,
            reason: body,
            trackingUrl: trackingUrl == "" ? None : Some(trackingUrl),
          }),
        )
      | Comment(_) | Reply(_) => dispatch(DraftSubmitted({body: body}))
      }
    }
  let onKeyDown = (ev: ReactEvent.Keyboard.t) => {
    let key = ReactEvent.Keyboard.key(ev)
    let submitChord =
      key == "Enter" && (ReactEvent.Keyboard.ctrlKey(ev) || ReactEvent.Keyboard.metaKey(ev))
    if submitChord {
      ReactEvent.Keyboard.preventDefault(ev)
      submit()
    } else if key == "Escape" {
      ReactEvent.Keyboard.preventDefault(ev)
      dispatch(DraftDiscarded({}))
    }
    // Everything else is text; never forwarded to the keymap.
    ReactEvent.Keyboard.stopPropagation(ev)
  }
  let placeholder = switch draft.purpose {
  | Reply(_) => "Reply…"
  | Comment({intent: Informational}) => "Summary or status note…"
  | Comment({intent: Finding}) => "Finding…"
  | Defer(_) => "Reason for deferring this unfixed finding (required)…"
  }
  <div className="composer panel">
    {switch draft.purpose {
    | Defer(_) =>
      <p>
        {React.string(
          "Record external follow-up. The finding remains unfixed; this does not approve the review or indicate deployment safety.",
        )}
      </p>
    | Comment({intent: Informational}) =>
      <p>
        {React.string("Informational note — does not approve the review or resolve findings.")}
      </p>
    | Comment({intent: Finding}) | Reply(_) => React.null
    }}
    {pendingRefresh
      ? <div className="composer-pending"> {React.string("changes pending")} </div>
      : React.null}
    {switch draft.submissionError {
    | Some(message) =>
      <UI.Panel title="Unable to submit" role="alert">
        <p> {React.string(message)} </p>
      </UI.Panel>
    | None => React.null
    }}
    <textarea
      className="composer-input"
      autoFocus=true
      placeholder
      value=body
      onChange={ev => setBody(_ => ReactEvent.Form.target(ev)["value"])}
      onKeyDown
    />
    {switch draft.purpose {
    | Defer(_) =>
      <UI.TextInput
        placeholder="External tracking URL (optional HTTP(S))"
        value=trackingUrl
        onChange={value => setTrackingUrl(_ => value)}
        onKeyEvent=onKeyDown
      />
    | Comment(_) | Reply(_) => React.null
    }}
    <div className="composer-actions">
      <UI.Button label="Submit" title=?{Chrome.tip(chrome, Submit)} kind=Primary onClick={submit} />
      <UI.Button
        label="Discard"
        title=?{Chrome.tip(chrome, Back)}
        onClick={() => dispatch(DraftDiscarded({}))}
      />
    </div>
  </div>
}
