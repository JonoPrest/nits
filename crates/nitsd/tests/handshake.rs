//! Negotiation table (plan 2.1).

use nits_protocol::{
    Author, BuildInfo, ClientId, ClientMsg, Envelope, ProtocolVersion, RequestId, RpcError,
    SchemaVersion, ServerMsg,
};
use nitsd::handshake::{AwaitingHello, SUPPORTED};

fn daemon() -> AwaitingHello {
    AwaitingHello {
        daemon: BuildInfo {
            name: "nitsd".into(),
            version: "test".into(),
        },
        schema: SchemaVersion::CURRENT,
    }
}

fn hello(v: ProtocolVersion) -> Envelope<ClientMsg> {
    Envelope {
        v,
        msg: ClientMsg::Hello {
            client_id: ClientId::from_parts(1, 1),
            protocol: v,
            client: BuildInfo {
                name: "test".into(),
                version: "0".into(),
            },
            author: Author::Human {
                name: "ada".into(),
                machine: "box".into(),
            },
        },
    }
}

#[test]
fn same_version_is_welcomed_without_upgrade_notice() {
    let (n, welcome) = daemon().negotiate(hello(ProtocolVersion::CURRENT)).unwrap();
    assert_eq!(n.protocol, ProtocolVersion::CURRENT);
    assert_eq!(welcome.v, ProtocolVersion::CURRENT);
    let ServerMsg::Welcome {
        protocol, upgrade, ..
    } = welcome.msg
    else {
        panic!("expected Welcome, got {welcome:?}");
    };
    assert_eq!(protocol, ProtocolVersion::CURRENT);
    assert!(upgrade.is_none());
}

#[test]
fn older_minor_is_rejected_before_any_incompatible_snapshot_or_event() {
    for minor in 0..ProtocolVersion::CURRENT.minor {
        let old = ProtocolVersion::new(ProtocolVersion::CURRENT.major, minor, 0);
        let rejected = daemon().negotiate(hello(old)).unwrap_err();
        assert_eq!(rejected.reply.v, old);
        assert_eq!(
            rejected.reply.msg,
            ServerMsg::Rejected {
                error: RpcError::UnsupportedProtocol {
                    requested: old,
                    supported: vec![ProtocolVersion::CURRENT],
                }
            }
        );
    }
}

#[test]
fn same_minor_patch_versions_share_the_serializer() {
    let current = ProtocolVersion::CURRENT;
    let version = ProtocolVersion::new(current.major, current.minor, current.patch + 1);
    let (negotiated, welcome) = daemon().negotiate(hello(version)).unwrap();
    assert_eq!(negotiated.protocol, version);
    assert_eq!(welcome.v, version);
}

#[test]
fn newer_minor_and_other_major_are_rejected_with_supported_list() {
    let cur = ProtocolVersion::CURRENT;
    for v in [
        ProtocolVersion::new(cur.major, cur.minor + 1, 0),
        ProtocolVersion::new(cur.major + 1, 0, 0),
    ] {
        let rejected = daemon().negotiate(hello(v)).unwrap_err();
        assert_eq!(
            rejected.reply.v, v,
            "rejection echoes the requested version"
        );
        let ServerMsg::Rejected { error } = &rejected.reply.msg else {
            panic!("expected Rejected, got {:?}", rejected.reply);
        };
        assert_eq!(
            *error,
            RpcError::UnsupportedProtocol {
                requested: v,
                supported: SUPPORTED.to_vec(),
            }
        );
    }
}

#[test]
fn non_hello_first_frame_is_rejected() {
    let rejected = daemon()
        .negotiate(Envelope::current(ClientMsg::Cancel {
            id: RequestId::new(1),
        }))
        .unwrap_err();
    assert!(matches!(
        rejected.reply.msg,
        ServerMsg::Rejected {
            error: RpcError::Invalid { .. }
        }
    ));
}

#[test]
fn post_handshake_version_mismatch_is_reported() {
    let (n, _) = daemon().negotiate(hello(ProtocolVersion::CURRENT)).unwrap();
    assert!(n.check(ProtocolVersion::CURRENT).is_ok());
    let other = ProtocolVersion::new(9, 9, 9);
    assert_eq!(
        n.check(other).unwrap_err(),
        RpcError::VersionMismatch {
            negotiated: ProtocolVersion::CURRENT,
            received: other,
        }
    );
    assert_eq!(
        n.wrap(ServerMsg::StreamEnd {
            id: RequestId::new(1)
        })
        .v,
        n.protocol
    );
}
