//! Protocol negotiation as a typestate: a connection starts as
//! [`AwaitingHello`] and becomes [`Negotiated`] only by way of an acceptable
//! `Hello`. Everything after the handshake goes through [`Negotiated`], so a
//! response can only be produced with the negotiated version stamped on it.

use nits_protocol::{
    Author, BuildInfo, ClientId, ClientMsg, Envelope, ProtocolVersion, RpcError, SchemaVersion,
    ServerMsg, UpgradeNotice,
};

/// Versions this daemon can serialise, newest first. Adding an entry here
/// requires a per-version serialiser (see §4.9); today only `CURRENT` exists.
pub const SUPPORTED: &[ProtocolVersion] = &[ProtocolVersion::CURRENT];

/// A connection that has not yet said `Hello`.
#[derive(Debug)]
pub struct AwaitingHello {
    pub daemon: BuildInfo,
    pub schema: SchemaVersion,
}

/// A connection with an agreed protocol version and identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Negotiated {
    pub protocol: ProtocolVersion,
    pub client_id: ClientId,
    pub client: BuildInfo,
    pub author: Author,
}

/// Why a `Hello` was refused. The daemon sends `reply` and closes.
/// Boxed: the error path carries a whole `ServerMsg`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub reply: Box<Envelope<ServerMsg>>,
}

impl AwaitingHello {
    /// Process the first frame. Anything but a servable `Hello` is rejected.
    pub fn negotiate(
        self,
        first: Envelope<ClientMsg>,
    ) -> Result<(Negotiated, Envelope<ServerMsg>), Rejected> {
        let ClientMsg::Hello {
            client_id,
            protocol,
            client,
            author,
        } = first.msg
        else {
            return Err(reject(
                first.v,
                RpcError::Invalid {
                    reason: "first frame must be Hello".into(),
                },
            ));
        };
        if first.v != protocol {
            return Err(reject(
                first.v,
                RpcError::Invalid {
                    reason: format!(
                        "Hello.protocol {protocol} disagrees with envelope version {}",
                        first.v
                    ),
                },
            ));
        }
        // Same-major compatibility alone is insufficient: each supported
        // minor must have a serializer. We currently encode only CURRENT.
        if !SUPPORTED
            .iter()
            .any(|s| s.major == protocol.major && s.minor == protocol.minor)
        {
            return Err(reject(
                protocol,
                RpcError::UnsupportedProtocol {
                    requested: protocol,
                    supported: SUPPORTED.to_vec(),
                },
            ));
        }
        // The client is served at the version it asked for; the daemon
        // never surprises a client with fields from a newer minor.
        let upgrade = (protocol.minor < ProtocolVersion::CURRENT.minor).then(|| UpgradeNotice {
            latest: ProtocolVersion::CURRENT,
            message: format!(
                "protocol {protocol} is still served; {} is current",
                ProtocolVersion::CURRENT
            ),
        });
        let negotiated = Negotiated {
            protocol,
            client_id,
            client,
            author,
        };
        let welcome = negotiated.wrap(ServerMsg::Welcome {
            protocol,
            daemon: self.daemon,
            schema: self.schema,
            upgrade,
        });
        Ok((negotiated, welcome))
    }
}

fn reject(v: ProtocolVersion, error: RpcError) -> Rejected {
    Rejected {
        reply: Box::new(Envelope {
            v,
            msg: ServerMsg::Rejected { error },
        }),
    }
}

impl Negotiated {
    /// Stamp the negotiated version on an outgoing message.
    #[must_use]
    pub fn wrap(&self, msg: ServerMsg) -> Envelope<ServerMsg> {
        Envelope {
            v: self.protocol,
            msg,
        }
    }

    /// Reject frames whose version differs from the negotiated one.
    pub fn check(&self, received: ProtocolVersion) -> Result<(), RpcError> {
        if received == self.protocol {
            Ok(())
        } else {
            Err(RpcError::VersionMismatch {
                negotiated: self.protocol,
                received,
            })
        }
    }
}
