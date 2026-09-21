// Stable installed-build and maintenance identities, independent of app Hello.
module BuildDigest = {
  @schema type t = string
}
module ReleaseVersion = {
  @schema type t = string
}
module ReleaseChannel = {
  @schema type t = string
}
module ControlVersion = {
  @schema type t = int
}
module WorkerVersion = {
  @schema type t = int
}
module ReleaseRelation = {
  @schema type t = Older | Equal | Newer | DifferentChannel
}
module ReleaseIdentity = {
  @schema type t = {channel: ReleaseChannel.t, version: ReleaseVersion.t}
}
module BuildDescriptor = {
  @schema
  type t = {
    digest: BuildDigest.t,
    release: ReleaseIdentity.t,
    protocol: Ids.protocolVersion,
    schema: Ids.schemaVersion,
    control: ControlVersion.t,
    worker: WorkerVersion.t,
  }
}
module UpgradeStage = {
  @schema type t = PreparingRestart | Draining | StartingReplacement | ConfirmingReady
}
module UpgradeFailureKind = {
  @schema
  type t =
    | NotManaged
    | CandidateUnavailable
    | IncompatibleControl
    | OlderRelease
    | DifferentChannel
    | UnorderedRelease
    | ContextMismatch
    | DrainTimeout
    | StartFailed
    | MigrationFailed
    | ReadinessTimeout
    | OwnerInterrupted
    | Io
}
module UpgradeFailure = {
  @schema type t = {stage: UpgradeStage.t, kind: UpgradeFailureKind.t, message: string}
}
module UpgradeProgress = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("Active") Active({stage: UpgradeStage.t})
    | @as("Ready") Ready({})
    | @as("Failed") Failed({failure: UpgradeFailure.t})
  @@warning("+27")
}
module UpgradeOperation = {
  @schema
  type t = {
    id: Ids.upgradeId,
    source: BuildDescriptor.t,
    target: BuildDescriptor.t,
    progress: UpgradeProgress.t,
  }
}
module UpgradeResult = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("AlreadyCurrent") AlreadyCurrent({build: BuildDescriptor.t})
    | @as("Accepted") Accepted({operation: UpgradeOperation.t})
    | @as("Restarted") Restarted({operation: UpgradeOperation.t})
    | @as("Failed") Failed({failure: UpgradeFailure.t})
  @@warning("+27")
}
module UpgradeIntent = {
  @schema type t = Automatic | Explicit
}
module InstalledCandidate = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("Available") Available({build: BuildDescriptor.t})
    | @as("Unavailable") Unavailable({reason: string})
  @@warning("+27")
}
module ManagedDaemonState = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("Running") Running({build: BuildDescriptor.t})
    | @as("Stopped") Stopped({})
    | @as("Legacy") Legacy({reason: string})
    | @as("Unavailable") Unavailable({reason: string})
    | @as("NotManaged") NotManaged({})
  @@warning("+27")
}
module ManagedDaemonStatus = {
  @schema
  type t = {
    running: ManagedDaemonState.t,
    installed: InstalledCandidate.t,
    operation: @s.null option<UpgradeOperation.t>,
  }
}
module LifecycleNotice = {
  @@warning("-27")
  @schema @tag("type") type t = | @as("Restarting") Restarting({operation: UpgradeOperation.t})
  @@warning("+27")
}
