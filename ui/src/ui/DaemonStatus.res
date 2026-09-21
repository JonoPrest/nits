let buildLabel = (build: Lifecycle.BuildDescriptor.t) =>
  build.release.version ++ " (" ++ String.slice(build.digest, ~start=0, ~end=12) ++ ")"

let stageLabel = (stage: Lifecycle.UpgradeStage.t) =>
  switch stage {
  | PreparingRestart => "preparing restart"
  | Draining => "finishing accepted work"
  | StartingReplacement => "starting installed build"
  | ConfirmingReady => "checking readiness"
  }

let operationLabel = (operation: Lifecycle.UpgradeOperation.t) => {
  let phase = switch operation.progress {
  | Active({stage}) => stageLabel(stage)
  | Ready(_) => "ready"
  | Failed({failure}) => failure.message
  }
  "Upgrade " ++ operation.id ++ ": " ++ phase
}

let statusLabel = (status: Lifecycle.ManagedDaemonStatus.t) => {
  let running = switch status.running {
  | Running({build}) => "Running " ++ buildLabel(build)
  | Stopped(_) => "Daemon is stopped"
  | Legacy({reason}) | Unavailable({reason}) => reason
  | NotManaged(_) => "This daemon is managed elsewhere"
  }
  let installed = switch status.installed {
  | Available({build}) => "Installed " ++ buildLabel(build)
  | Unavailable({reason}) => reason
  }
  running ++
  ". " ++
  installed ++
  switch status.operation {
  | Some(operation) => ". " ++ operationLabel(operation)
  | None => ""
  }
}

@react.component
let make = (
  ~management: View.DaemonManagement.t,
  ~chrome: array<View.Hint.t>,
  ~dispatch: Action.t => unit,
) => {
  let message = switch management {
  | Idle(_) => ""
  | Inspecting(_) => "Inspecting daemon and installed version…"
  | Status({status}) => statusLabel(status)
  | Upgrading(_) => "Activating the installed version…"
  | Outcome({result}) =>
    switch result {
    | AlreadyCurrent({build}) => "Already running " ++ buildLabel(build)
    | Accepted({operation}) => operationLabel(operation) ++ ". Check status for readiness."
    | Restarted({operation}) => "Now running " ++ buildLabel(operation.target)
    | Failed({failure}) => failure.message
    }
  | Unavailable({message}) => message
  }
  let busy = switch management {
  | Inspecting(_) | Upgrading(_) => true
  | _ => false
  }
  <UI.Box direction=Column gap=Sm>
    <UI.Box direction=WrappingRow gap=Sm>
      <UI.Button
        label="Daemon status"
        kind=Ghost
        title=?{Chrome.tip(chrome, InspectDaemon)}
        disabled=busy
        onClick={() => dispatch(Action.RunCommand({command: InspectDaemon}))}
      />
      <UI.Button
        label="Activate installed version"
        kind=Ghost
        title=?{Chrome.tip(chrome, UpgradeDaemon)}
        disabled=busy
        onClick={() => dispatch(Action.RunCommand({command: UpgradeDaemon}))}
      />
    </UI.Box>
    {message == "" ? React.null : <UI.Message text=message />}
  </UI.Box>
}
