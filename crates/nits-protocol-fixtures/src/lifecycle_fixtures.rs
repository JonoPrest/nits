//! Stable maintenance values, independently registered from app RPC variants.
use super::*;

fn version() -> Result<ReleaseVersion, FixtureError> {
    "1.0.0"
        .parse()
        .map_err(|error: BuildMetadataError| FixtureError::Invalid(error.to_string()))
}
fn channel() -> Result<ReleaseChannel, FixtureError> {
    "stable"
        .parse()
        .map_err(|error: BuildMetadataError| FixtureError::Invalid(error.to_string()))
}
fn release() -> Result<ReleaseIdentity, FixtureError> {
    Ok(ReleaseIdentity {
        channel: channel()?,
        version: version()?,
    })
}
pub(super) fn build() -> Result<BuildDescriptor, FixtureError> {
    Ok(BuildDescriptor {
        digest: BuildDigest::from_bytes([1; 32]),
        release: release()?,
        protocol: ProtocolVersion::CURRENT,
        schema: SchemaVersion::CURRENT,
        control: ControlVersion::CURRENT,
        worker: WorkerVersion::CURRENT,
    })
}
pub(super) fn operation() -> Result<UpgradeOperation, FixtureError> {
    let source = build()?;
    let mut target = source.clone();
    target.digest = BuildDigest::from_bytes([2; 32]);
    target.release.version = "1.1.0"
        .parse()
        .map_err(|error: BuildMetadataError| FixtureError::Invalid(error.to_string()))?;
    Ok(UpgradeOperation {
        id: UpgradeId::from_parts(1, 1),
        source,
        target,
        progress: UpgradeProgress::Active {
            stage: UpgradeStage::Draining,
        },
    })
}
fn failure() -> UpgradeFailure {
    UpgradeFailure {stage:UpgradeStage::StartingReplacement,kind:UpgradeFailureKind::MigrationFailed,message:"The installed build could not migrate the store; inspect the daemon log. No rollback was attempted.".into()}
}
struct_fixture!(BuildDigest, "BuildDigest", BuildDigest::from_bytes([1; 32]));
struct_fixture!(ReleaseVersion, "ReleaseVersion", version()?);
struct_fixture!(ReleaseChannel, "ReleaseChannel", channel()?);
struct_fixture!(ControlVersion, "ControlVersion", ControlVersion::CURRENT);
struct_fixture!(WorkerVersion, "WorkerVersion", WorkerVersion::CURRENT);
struct_fixture!(UpgradeId, "UpgradeId", UpgradeId::from_parts(1, 1));
struct_fixture!(ReleaseIdentity, "ReleaseIdentity", release()?);
struct_fixture!(BuildDescriptor, "BuildDescriptor", build()?);
struct_fixture!(UpgradeOperation, "UpgradeOperation", operation()?);
struct_fixture!(UpgradeFailure, "UpgradeFailure", failure());
unit_enum_fixture!(ReleaseRelation, "ReleaseRelation");
unit_enum_fixture!(UpgradeStage, "UpgradeStage");
unit_enum_fixture!(UpgradeFailureKind, "UpgradeFailureKind");
unit_enum_fixture!(UpgradeIntent, "UpgradeIntent");
enum_fixture!(
    UpgradeProgress,
    UpgradeProgressKind,
    "UpgradeProgress",
    [
        UpgradeProgress::Active {
            stage: UpgradeStage::Draining
        },
        UpgradeProgress::Ready {},
        UpgradeProgress::Failed { failure: failure() }
    ]
);
enum_fixture!(
    UpgradeResult,
    UpgradeResultKind,
    "UpgradeResult",
    [
        UpgradeResult::AlreadyCurrent { build: build()? },
        UpgradeResult::Accepted {
            operation: operation()?
        },
        UpgradeResult::Restarted {
            operation: operation()?
        },
        UpgradeResult::Failed { failure: failure() }
    ]
);
enum_fixture!(
    InstalledCandidate,
    InstalledCandidateKind,
    "InstalledCandidate",
    [
        InstalledCandidate::Available { build: build()? },
        InstalledCandidate::Unavailable {
            reason: "selected installed executable is missing".into()
        }
    ]
);
enum_fixture!(
    ManagedDaemonState,
    ManagedDaemonStateKind,
    "ManagedDaemonState",
    [
        ManagedDaemonState::Running { build: build()? },
        ManagedDaemonState::Stopped {},
        ManagedDaemonState::Legacy {
            reason: "one-time legacy stop/start bootstrap required".into()
        },
        ManagedDaemonState::Unavailable {
            reason: "store still owned while draining".into()
        },
        ManagedDaemonState::NotManaged {}
    ]
);
enum_fixture!(
    LifecycleNotice,
    LifecycleNoticeKind,
    "LifecycleNotice",
    [LifecycleNotice::Restarting {
        operation: operation()?
    }]
);
struct_fixture!(
    ManagedDaemonStatus,
    "ManagedDaemonStatus",
    ManagedDaemonStatus {
        running: ManagedDaemonState::Running { build: build()? },
        installed: InstalledCandidate::Available { build: build()? },
        operation: Some(operation()?)
    }
);
