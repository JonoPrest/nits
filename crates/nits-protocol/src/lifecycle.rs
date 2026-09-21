//! Installed-build identity and lifecycle control types.
//!
//! Exact executable identity, release ordering, application protocol and store
//! schema are independent. A digest never establishes which release is newer.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};

/// Invalid installed-build metadata. Parse once before admitting an upgrade.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildMetadataError {
    #[error("build digest must contain exactly 64 hexadecimal characters")]
    Digest,
    #[error("invalid release version: {0}")]
    Release(String),
    #[error("release channel must be 1–64 ASCII letters, digits, '.', '_' or '-'")]
    Channel,
}

/// SHA-256 of an executable's complete bytes, not its path or modification time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BuildDigest([u8; 32]);

impl BuildDigest {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl fmt::Display for BuildDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for BuildDigest {
    type Err = BuildMetadataError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(BuildMetadataError::Digest);
        }
        let mut bytes = [0; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .map_err(|_| BuildMetadataError::Digest)?;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for BuildDigest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for BuildDigest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Semantic package release. Build metadata is retained but never orders releases.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ReleaseVersion(semver::Version);

impl FromStr for ReleaseVersion {
    type Err = BuildMetadataError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        semver::Version::parse(value)
            .map(Self)
            .map_err(|error| BuildMetadataError::Release(error.to_string()))
    }
}

impl TryFrom<String> for ReleaseVersion {
    type Error = BuildMetadataError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<ReleaseVersion> for String {
    fn from(value: ReleaseVersion) -> Self {
        value.0.to_string()
    }
}

impl fmt::Display for ReleaseVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Independently installed release line. Different channels are not ordered.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ReleaseChannel(String);

impl FromStr for ReleaseChannel {
    type Err = BuildMetadataError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty()
            || value.len() > 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(BuildMetadataError::Channel);
        }
        Ok(Self(value.into()))
    }
}

impl TryFrom<String> for ReleaseChannel {
    type Error = BuildMetadataError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<ReleaseChannel> for String {
    fn from(value: ReleaseChannel) -> Self {
        value.0
    }
}

impl fmt::Display for ReleaseChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Ordering evidence used separately from executable equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::EnumIter)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ReleaseRelation {
    Older,
    Equal,
    Newer,
    DifferentChannel,
}

/// Package/channel provenance published by the installed executable itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReleaseIdentity {
    pub channel: ReleaseChannel,
    pub version: ReleaseVersion,
}

impl ReleaseIdentity {
    #[must_use]
    pub fn relative_to(&self, running: &Self) -> ReleaseRelation {
        if self.channel != running.channel {
            return ReleaseRelation::DifferentChannel;
        }
        match self.version.0.cmp_precedence(&running.version.0) {
            core::cmp::Ordering::Less => ReleaseRelation::Older,
            core::cmp::Ordering::Equal => ReleaseRelation::Equal,
            core::cmp::Ordering::Greater => ReleaseRelation::Newer,
        }
    }
}

/// Stable maintenance wire version; independent of the application serializer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct ControlVersion(u16);

impl ControlVersion {
    pub const CURRENT: Self = Self(1);
}

/// Stable supervisor/worker IPC version. An incompatible installed worker is
/// rejected before retiring the current worker or its initialized host session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct WorkerVersion(u16);

impl WorkerVersion {
    pub const CURRENT: Self = Self(1);
}

/// What an executable actually contains. None of these independent identities
/// is inferred from a filename, modification time, or another version field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BuildDescriptor {
    pub digest: BuildDigest,
    pub release: ReleaseIdentity,
    pub protocol: crate::ProtocolVersion,
    pub schema: crate::SchemaVersion,
    pub control: ControlVersion,
    pub worker: WorkerVersion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::EnumIter)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum UpgradeStage {
    PreparingRestart,
    Draining,
    StartingReplacement,
    ConfirmingReady,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::EnumIter)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum UpgradeFailureKind {
    NotManaged,
    CandidateUnavailable,
    IncompatibleControl,
    OlderRelease,
    DifferentChannel,
    UnorderedRelease,
    ContextMismatch,
    DrainTimeout,
    StartFailed,
    MigrationFailed,
    ReadinessTimeout,
    OwnerInterrupted,
    Io,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UpgradeFailure {
    pub stage: UpgradeStage,
    pub kind: UpgradeFailureKind,
    pub message: String,
}

/// A phase is either active, durably successful, or failed with its last phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, strum::EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", deny_unknown_fields)]
#[strum_discriminants(name(UpgradeProgressKind), derive(Hash, strum::EnumIter))]
pub enum UpgradeProgress {
    Active { stage: UpgradeStage },
    Ready {},
    Failed { failure: UpgradeFailure },
}

/// Ephemeral lifecycle provenance, persisted only in the coordinator journal.
/// It is deliberately not an event in the review log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UpgradeOperation {
    pub id: crate::UpgradeId,
    pub source: BuildDescriptor,
    pub target: BuildDescriptor,
    pub progress: UpgradeProgress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, strum::EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", deny_unknown_fields)]
#[strum_discriminants(name(UpgradeResultKind), derive(Hash, strum::EnumIter))]
pub enum UpgradeResult {
    AlreadyCurrent { build: BuildDescriptor },
    Accepted { operation: UpgradeOperation },
    Restarted { operation: UpgradeOperation },
    Failed { failure: UpgradeFailure },
}

/// Explicit activation may replace an unordered development build on the same
/// release/channel. Automatic repair requires positive newer-release evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::EnumIter)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum UpgradeIntent {
    Automatic,
    Explicit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, strum::EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", deny_unknown_fields)]
#[strum_discriminants(name(InstalledCandidateKind), derive(Hash, strum::EnumIter))]
pub enum InstalledCandidate {
    Available { build: BuildDescriptor },
    Unavailable { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, strum::EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", deny_unknown_fields)]
#[strum_discriminants(name(ManagedDaemonStateKind), derive(Hash, strum::EnumIter))]
pub enum ManagedDaemonState {
    Running { build: BuildDescriptor },
    Stopped {},
    Legacy { reason: String },
    Unavailable { reason: String },
    NotManaged {},
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ManagedDaemonStatus {
    pub running: ManagedDaemonState,
    pub installed: InstalledCandidate,
    pub operation: Option<UpgradeOperation>,
}

/// A request rejected before admission is safe to retry after readiness. An
/// interrupted accepted request has an unknown outcome unless its receipt was
/// delivered; clients must never infer non-commit from connection loss.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, strum::EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", deny_unknown_fields)]
#[strum_discriminants(name(LifecycleNoticeKind), derive(Hash, strum::EnumIter))]
pub enum LifecycleNotice {
    Restarting { operation: UpgradeOperation },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(channel: &str, version: &str) -> ReleaseIdentity {
        ReleaseIdentity {
            channel: channel.parse().unwrap(),
            version: version.parse().unwrap(),
        }
    }

    #[test]
    fn release_precedence_is_not_digest_metadata_or_protocol_order() {
        let running = release("stable", "1.2.3+build.old");
        for (candidate, expected) in [
            (release("stable", "1.2.3+build.new"), ReleaseRelation::Equal),
            (release("stable", "1.2.4"), ReleaseRelation::Newer),
            (release("stable", "1.2.3-rc.1"), ReleaseRelation::Older),
            (release("stable", "1.10.0"), ReleaseRelation::Newer),
            (
                release("preview", "99.0.0"),
                ReleaseRelation::DifferentChannel,
            ),
        ] {
            assert_eq!(candidate.relative_to(&running), expected);
        }
    }

    #[test]
    fn installed_build_metadata_rejects_invalid_wire_values() {
        for value in ["", "../stable", " stable", "stable\n", "é"] {
            assert!(value.parse::<ReleaseChannel>().is_err());
        }
        for value in ["", "1.2", "01.2.3", "1.2.3 ", "latest"] {
            assert!(value.parse::<ReleaseVersion>().is_err());
        }
        let digest = BuildDigest::from_bytes([0xab; 32]);
        let json = serde_json::to_string(&digest).unwrap();
        assert_eq!(serde_json::from_str::<BuildDigest>(&json).unwrap(), digest);
        for value in ["ab".repeat(31), "é".repeat(32), "zz".repeat(32)] {
            assert!(value.parse::<BuildDigest>().is_err());
        }
    }
}
