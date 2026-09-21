//! Versioned session state for replacing an MCP worker without reinitializing
//! its host. Connections, request IDs, mutation counters and pending work are
//! deliberately absent: the replacement establishes a new daemon connection.

use std::num::NonZeroU16;
use std::path::PathBuf;

use nits_config::{Context, ContextName, Selection, SelectionOrigin};
use nits_protocol::{AgentVia, Author};
use nitsd::contexts::StartPolicy;
use serde::{Deserialize, Serialize};

use crate::server::{AgentIdentity, Endpoint};

/// Version of the private worker-session format, independent of daemon RPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CheckpointVersion(NonZeroU16);

impl CheckpointVersion {
    pub const CURRENT: Self = Self(NonZeroU16::MIN);
}

impl std::fmt::Display for CheckpointVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A malformed checkpoint cannot change immutable attribution during handoff.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckpointError {
    #[error("unsupported MCP session checkpoint version {found}")]
    UnsupportedVersion { found: CheckpointVersion },
    #[error("an initialized MCP checkpoint must have Agent/Mcp provenance")]
    WrongProvenance,
    #[error("checkpoint author changes the session ID or invoking human")]
    ChangedIdentity,
}

/// Only mutable display/routing identity belongs in the initialized session.
/// Immutable session attribution is shared on the enclosing server/checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InitializedIdentity {
    pub name: String,
    pub model: String,
}

impl InitializedIdentity {
    pub(crate) fn author(&self, agent: &AgentIdentity) -> Author {
        Author::Agent {
            name: self.name.clone(),
            model: self.model.clone(),
            session_id: agent.session_id.clone(),
            invoked_by: agent.invoked_by.clone(),
            via: AgentVia::Mcp,
        }
    }
}

/// Bare cursors are meaningful only before switching the active context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum CursorPolicy {
    InitialContext,
    RequireContext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CheckpointState {
    Uninitialized,
    Initialized {
        identity: InitializedIdentity,
        cursor_policy: CursorPolicy,
    },
}

/// A validated session snapshot. Deserialization checks attribution once;
/// restoration separately checks whether this worker supports its version.
/// No live connection or outstanding request can be represented here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "CheckpointWire", into = "CheckpointWire")]
pub struct SessionCheckpoint {
    version: CheckpointVersion,
    endpoint: Endpoint,
    agent: AgentIdentity,
    state: CheckpointState,
}

impl SessionCheckpoint {
    pub(crate) fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub(crate) fn initialized(&self) -> bool {
        matches!(self.state, CheckpointState::Initialized { .. })
    }

    pub(crate) fn capture(
        endpoint: Endpoint,
        agent: AgentIdentity,
        state: CheckpointState,
    ) -> Self {
        Self {
            version: CheckpointVersion::CURRENT,
            endpoint,
            agent,
            state,
        }
    }

    pub(crate) fn into_current_parts(
        self,
    ) -> Result<(Endpoint, AgentIdentity, CheckpointState), CheckpointError> {
        if self.version != CheckpointVersion::CURRENT {
            return Err(CheckpointError::UnsupportedVersion {
                found: self.version,
            });
        }
        Ok((self.endpoint, self.agent, self.state))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointWire {
    version: CheckpointVersion,
    endpoint: EndpointWire,
    agent: AgentIdentity,
    state: CheckpointStateWire,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum CheckpointStateWire {
    Uninitialized {},
    Initialized {
        author: Author,
        cursor_policy: CursorPolicy,
    },
}

impl TryFrom<CheckpointWire> for SessionCheckpoint {
    type Error = CheckpointError;

    fn try_from(wire: CheckpointWire) -> Result<Self, Self::Error> {
        let state = match wire.state {
            CheckpointStateWire::Uninitialized {} => CheckpointState::Uninitialized,
            CheckpointStateWire::Initialized {
                author,
                cursor_policy,
            } => {
                let Author::Agent {
                    name,
                    model,
                    session_id,
                    invoked_by,
                    via: AgentVia::Mcp,
                } = author
                else {
                    return Err(CheckpointError::WrongProvenance);
                };
                if session_id != wire.agent.session_id || invoked_by != wire.agent.invoked_by {
                    return Err(CheckpointError::ChangedIdentity);
                }
                CheckpointState::Initialized {
                    identity: InitializedIdentity { name, model },
                    cursor_policy,
                }
            }
        };
        Ok(Self {
            version: wire.version,
            endpoint: wire.endpoint.into(),
            agent: wire.agent,
            state,
        })
    }
}

impl From<SessionCheckpoint> for CheckpointWire {
    fn from(checkpoint: SessionCheckpoint) -> Self {
        let state = match checkpoint.state {
            CheckpointState::Uninitialized => CheckpointStateWire::Uninitialized {},
            CheckpointState::Initialized {
                identity,
                cursor_policy,
            } => CheckpointStateWire::Initialized {
                author: identity.author(&checkpoint.agent),
                cursor_policy,
            },
        };
        Self {
            version: checkpoint.version,
            endpoint: checkpoint.endpoint.into(),
            agent: checkpoint.agent,
            state,
        }
    }
}

/// A private stable wire representation avoids making configuration selection
/// and daemon startup policy depend on the worker handoff's serde contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EndpointWire {
    selection: SelectionWire,
    config_path: PathBuf,
    start: StartPolicyWire,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionWire {
    name: ContextName,
    context: Context,
    origin: SelectionOriginWire,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum StartPolicyWire {
    StartIfNeeded,
    RequireRunning,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum SelectionOriginWire {
    Flag,
    Environment,
    Persisted,
    Implicit,
    AdHoc,
    Mcp,
}

impl From<Endpoint> for EndpointWire {
    fn from(endpoint: Endpoint) -> Self {
        let origin = match endpoint.selection.origin {
            SelectionOrigin::Flag => SelectionOriginWire::Flag,
            SelectionOrigin::Environment => SelectionOriginWire::Environment,
            SelectionOrigin::Persisted => SelectionOriginWire::Persisted,
            SelectionOrigin::Implicit => SelectionOriginWire::Implicit,
            SelectionOrigin::AdHoc => SelectionOriginWire::AdHoc,
            SelectionOrigin::Mcp => SelectionOriginWire::Mcp,
        };
        let start = match endpoint.start {
            StartPolicy::StartIfNeeded => StartPolicyWire::StartIfNeeded,
            StartPolicy::RequireRunning => StartPolicyWire::RequireRunning,
        };
        Self {
            selection: SelectionWire {
                name: endpoint.selection.name,
                context: endpoint.selection.context,
                origin,
            },
            config_path: endpoint.config_path,
            start,
        }
    }
}

impl From<EndpointWire> for Endpoint {
    fn from(wire: EndpointWire) -> Self {
        let origin = match wire.selection.origin {
            SelectionOriginWire::Flag => SelectionOrigin::Flag,
            SelectionOriginWire::Environment => SelectionOrigin::Environment,
            SelectionOriginWire::Persisted => SelectionOrigin::Persisted,
            SelectionOriginWire::Implicit => SelectionOrigin::Implicit,
            SelectionOriginWire::AdHoc => SelectionOrigin::AdHoc,
            SelectionOriginWire::Mcp => SelectionOrigin::Mcp,
        };
        let start = match wire.start {
            StartPolicyWire::StartIfNeeded => StartPolicy::StartIfNeeded,
            StartPolicyWire::RequireRunning => StartPolicy::RequireRunning,
        };
        Self {
            selection: Selection {
                name: wire.selection.name,
                context: wire.selection.context,
                origin,
            },
            config_path: wire.config_path,
            start,
        }
    }
}
