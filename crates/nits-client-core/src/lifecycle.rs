//! Planned connection replacement, driven solely by host clock inputs.

use crate::{ClientCore, Connection, ConnectionView, Effect, Millis, ViewSection, render};
use nits_protocol::{ProtocolVersion, RpcError, UpgradeOperation};

#[derive(Debug)]
pub(crate) enum Recovery {
    Idle,
    Restarting {
        operation: UpgradeOperation,
        deadline: Millis,
        next_attempt: Millis,
        attempts: u8,
    },
}

impl ClientCore {
    pub(crate) fn restart_notice(&mut self, operation: UpgradeOperation) -> Vec<Effect> {
        self.recovery = Recovery::Restarting {
            operation: operation.clone(),
            deadline: self.now.saturating_add(60_000),
            next_attempt: self.now.saturating_add(100),
            attempts: 0,
        };
        self.view.connection = ConnectionView::Restarting { operation };
        vec![render(&[ViewSection::Connection])]
    }

    pub(crate) fn restart_disconnected_view(&mut self) -> Option<ConnectionView> {
        let Recovery::Restarting { operation, .. } = &self.recovery else {
            return None;
        };
        if operation.target.protocol == ProtocolVersion::CURRENT {
            Some(ConnectionView::Restarting {
                operation: operation.clone(),
            })
        } else {
            let view = ConnectionView::UpgradeRequired {
                client: ProtocolVersion::CURRENT,
                supported: vec![operation.target.protocol],
            };
            self.recovery = Recovery::Idle;
            Some(view)
        }
    }

    pub(crate) fn restart_tick(&mut self) -> Vec<Effect> {
        let Recovery::Restarting {
            operation,
            deadline,
            next_attempt,
            attempts,
        } = &mut self.recovery
        else {
            return Vec::new();
        };
        if self.now >= *deadline {
            self.view.connection = ConnectionView::Rejected {
                error: RpcError::Internal {
                    message: format!(
                        "Restart {} did not become ready within the reconnect budget. Drafts are retained. Inspect daemon upgrade-status and reconnect when ready.",
                        operation.id
                    ),
                },
            };
            self.connection = Connection::Disconnected {
                last_seq: self.connection.last_seq(),
            };
            self.clear_in_flight();
            self.recovery = Recovery::Idle;
            return vec![Effect::Disconnect, render(&[ViewSection::Connection])];
        }
        if let Connection::Disconnected { last_seq } = self.connection
            && self.now >= *next_attempt
        {
            *next_attempt = self.now.saturating_add(250_u64 << (*attempts).min(4));
            *attempts = attempts.saturating_add(1);
            self.connection = Connection::Connecting {
                hello_sent: false,
                last_seq,
            };
            return vec![Effect::Connect];
        }
        Vec::new()
    }
}

/// Correlates host maintenance I/O independently of daemon request IDs and
/// connection generations: the requesting connection may retire during it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagementRequestId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagementRequest {
    Inspect,
    Upgrade,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagementReply {
    Status(Result<nits_protocol::ManagedDaemonStatus, String>),
    Upgrade(Result<nits_protocol::UpgradeResult, String>),
}

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    strum::EnumDiscriminants,
)]
#[strum_discriminants(name(DaemonManagementKind), derive(Hash, strum::EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum DaemonManagement {
    #[default]
    Idle,
    Inspecting,
    Status {
        status: nits_protocol::ManagedDaemonStatus,
    },
    Upgrading,
    Outcome {
        result: nits_protocol::UpgradeResult,
    },
    Unavailable {
        message: String,
    },
}

impl ClientCore {
    pub(crate) fn manage_daemon(&mut self, request: ManagementRequest) -> Vec<Effect> {
        // The request ID supersedes slow status responses without changing the
        // chosen endpoint, persisted context, or existing daemon connection.
        self.next_management = self.next_management.wrapping_add(1);
        let id = ManagementRequestId(self.next_management);
        self.management_request = Some(id);
        self.view.daemon_management = match request {
            ManagementRequest::Inspect => DaemonManagement::Inspecting,
            ManagementRequest::Upgrade => DaemonManagement::Upgrading,
        };
        vec![
            Effect::ManageDaemon { id, request },
            render(&[ViewSection::Connection]),
        ]
    }

    pub(crate) fn managed_daemon(
        &mut self,
        id: ManagementRequestId,
        reply: ManagementReply,
    ) -> Vec<Effect> {
        if self.management_request != Some(id) {
            return Vec::new();
        }
        self.management_request = None;
        self.view.daemon_management = match reply {
            ManagementReply::Status(Ok(status)) => DaemonManagement::Status { status },
            ManagementReply::Upgrade(Ok(result)) => DaemonManagement::Outcome { result },
            ManagementReply::Status(Err(message)) | ManagementReply::Upgrade(Err(message)) => {
                DaemonManagement::Unavailable { message }
            }
        };
        vec![render(&[ViewSection::Connection])]
    }
}
