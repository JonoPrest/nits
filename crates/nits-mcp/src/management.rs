//! MCP lifecycle results keep daemon state separate from the local adapter.
//! Remote daemon installation never implies that a local MCP worker is current.

use nits_protocol::{
    BuildDescriptor, InstalledCandidate, ManagedDaemonStatus, UpgradeId, UpgradeResult,
};
use schemars::JsonSchema;
use serde::Serialize;

use crate::tools::ContextIdentity;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum AdapterStatus {
    Ready {},
    Preparing {},
    Unavailable { reason: String },
    Retained { reason: String },
    Handoff { operation: UpgradeId },
    UpgradeRequired { reason: String },
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DaemonStatus {
    pub context: ContextIdentity,
    pub supervisor_build: BuildDescriptor,
    pub adapter_build: BuildDescriptor,
    pub installed_adapter: InstalledCandidate,
    pub daemon: ManagedDaemonStatus,
    pub adapter: AdapterStatus,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DaemonUpgrade {
    pub context: ContextIdentity,
    pub result: UpgradeResult,
    pub adapter: AdapterStatus,
}

pub(crate) async fn installed_adapter() -> InstalledCandidate {
    match nitsd::build::inspect(&nitsd::launch::nits_binary()).await {
        Ok(candidate) => InstalledCandidate::Available {
            build: candidate.descriptor,
        },
        Err(error) => InstalledCandidate::Unavailable {
            reason: error.to_string(),
        },
    }
}
