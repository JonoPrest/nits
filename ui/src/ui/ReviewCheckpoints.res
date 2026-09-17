// Checked revisions have no approval, resolution or human viewed semantics.
module Targets = {
  @react.component
  let make = (~targets: array<Domain.ResolvedTarget.t>) =>
    <ul>
      {targets
      ->Array.map(target => {
        let describe = (revision: Domain.ResolvedRef.t) =>
          switch revision.source {
          | Commit({oid}) => "commit " ++ oid ++ " · tree " ++ revision.tree
          | WorkingTree(_) => "working tree " ++ revision.tree
          }
        <li key=target.repoId>
          {React.string(
            target.repoId ++ ": " ++ describe(target.base) ++ " → " ++ describe(target.head),
          )}
        </li>
      })
      ->React.array}
    </ul>
}

@react.component
let make = (
  ~checkpoints: array<Domain.ReviewerCheckpoint.t>,
  ~chrome: array<View.Hint.t>,
  ~checkCurrentReady: bool=true,
  ~dispatch: Action.t => unit,
) =>
  <section ariaLabel="Revision checkpoints" className="threads panel">
    <header className="panel-header"> {React.string("Revision checkpoints")} </header>
    <UI.Box>
      <UI.Button
        label="Record current revision checked"
        disabled={!checkCurrentReady}
        title=?{Chrome.tip(chrome, CheckCurrent)}
        onClick={() => dispatch(RunCommand({command: CheckCurrent}))}
      />
      {if Array.length(checkpoints) > 0 {
        <UI.Button
          label="Inspect next checkpoint delta"
          title=?{Chrome.tip(chrome, CheckpointDelta)}
          onClick={() => dispatch(RunCommand({command: CheckpointDelta}))}
        />
      } else {
        React.null
      }}
    </UI.Box>
    {checkCurrentReady ? React.null : <p> {React.string("Current changes are refreshing.")} </p>}
    {checkpoints
    ->Array.map(status => {
      let checkpoint = status.checkpoint
      let freshness = switch status.freshness {
      | Current => "Current target checked"
      | Changed => "Current target changed since check"
      | UnknownCurrent => "Current target unknown"
      }
      <div key={Float.toString(checkpoint.id)} className="thread-item">
        <div className="thread-meta">
          <span> {React.string(Threads.authorName(checkpoint.author))} </span>
          <UI.Badge text=freshness />
          <span> {React.string("checkpoint " ++ Float.toString(checkpoint.id))} </span>
          <span title={Stepper.absolute(checkpoint.created)}>
            {React.string(Stepper.relative(checkpoint.created))}
          </span>
        </div>
        {switch checkpoint.inReplyTo {
        | Some(Request({requestId})) =>
          <p> {React.string("Answers request " ++ Float.toString(requestId))} </p>
        | Some(Checkpoint({checkpointId})) =>
          <p> {React.string("Answers checkpoint " ++ Float.toString(checkpointId))} </p>
        | None => React.null
        }}
        <Targets targets=checkpoint.targets />
      </div>
    })
    ->React.array}
  </section>
