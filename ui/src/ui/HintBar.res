// The hint bar (§6.4, UI-DESIGN): the bar is the mode indicator. It shows
// the focused context's primary bindings straight from `ViewModel.hints`,
// and switches to the pending group's keys while a leader is held
// (zellij-style) — never hand-written.

@react.component
let make = (
  ~hints: array<View.Hint.t>,
  ~pendingKeys: string="",
  ~pendingLabel: option<string>=?,
  ~mode: View.Mode.t=View.Mode.Normal,
  ~leader: string="",
  ~focusName: string="",
  ~connection: View.ConnectionView.t,
  ~progress: View.Progress.t,
) => {
  let conn = switch connection {
  | Disconnected(_) => "disconnected"
  | Connecting(_) => "connecting…"
  | Subscribed(_) => "connected"
  | Rejected(_) => "rejected"
  | Restarting({operation}) => "Restarting to " ++ operation.target.release.version
  | UpgradeRequired(_) => "client upgrade required"
  }
  let pending = pendingKeys != ""
  <footer className={"hint-bar" ++ (pending ? " hint-bar-pending" : "")} role="contentinfo">
    {switch mode {
    | Insert => <span className="mode-badge mode-insert"> {React.string("INSERT")} </span>
    | Visual => <span className="mode-badge mode-visual"> {React.string("VISUAL")} </span>
    | Normal =>
      focusName == ""
        ? React.null
        : <span className="mode-badge mode-focus"> {React.string(focusName)} </span>
    }}
    <span className={"conn conn-" ++ conn}> {React.string(conn)} </span>
    <span className="progress">
      {React.string(
        Int.toString(progress.viewed) ++ "/" ++ Int.toString(progress.total) ++ " viewed",
      )}
    </span>
    {leader != "" && !pending
      ? <span className="leader-chip" title={"leader: " ++ leader}>
          <UI.Kbd keys=leader />
          {React.string(" leader")}
        </span>
      : React.null}
    {pending
      ? <span className="pending-keys">
          <UI.Kbd keys=pendingKeys />
          {switch pendingLabel {
          | Some(label) => <span className="pending-label"> {React.string(" " ++ label)} </span>
          | None => React.null
          }}
        </span>
      : React.null}
    {hints
    ->Array.map(h =>
      <span key={h.keys ++ h.label} className="hint">
        <UI.Kbd keys=h.keys />
        {React.string(" " ++ h.label)}
      </span>
    )
    ->React.array}
  </footer>
}
