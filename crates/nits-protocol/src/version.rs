//! Wire-protocol and store-schema versioning. See `docs/ARCHITECTURE.md` §4.9.
//!
//! Two independent versions:
//!
//! - [`ProtocolVersion`]: semver of the JSON wire format. A client states the
//!   version it speaks in `ClientMsg::Hello`; the daemon answers with
//!   `ServerMsg::Welcome` carrying the version it will *serialise responses
//!   in* (always one the client asked for or is compatible with), or rejects
//!   with `RpcError::UnsupportedProtocol`. Every frame is wrapped in an
//!   [`Envelope`] so the version is visible on each message, not just at
//!   handshake.
//! - [`SchemaVersion`]: monotonic integer stamped into the redb `meta` table.
//!   The daemon migrates older stores forward and refuses to open newer ones.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};

/// Semantic version of the wire protocol.
///
/// Compatibility rule: equal `major` and daemon `minor >= client minor`. Minor
/// bumps may add variants/fields; the daemon serialises responses at the
/// client's requested minor so `deny_unknown_fields` on the client still
/// holds. Serving an older minor additionally requires an explicitly supported
/// serializer; retired minors are rejected. Major bumps are never bridged silently.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl ProtocolVersion {
    /// The version this crate serialises.
    pub const CURRENT: ProtocolVersion = ProtocolVersion {
        major: 0,
        minor: 10,
        patch: 0,
    };

    #[must_use]
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Is `requested` in the same-major compatibility family at or before `self`?
    /// This does not promise an older-minor serializer exists: daemon negotiation
    /// must also require an explicitly supported minor. The stable shutdown-only
    /// fallback uses this predicate to reach older daemons during upgrades.
    #[must_use]
    pub fn can_serve(self, requested: ProtocolVersion) -> bool {
        self.major == requested.major && self.minor >= requested.minor
    }
}

impl fmt::Debug for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ProtocolVersion({self})")
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Error parsing a `ProtocolVersion`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid protocol version {0:?}: expected MAJOR.MINOR.PATCH")]
pub struct ParseVersionError(String);

impl FromStr for ProtocolVersion {
    type Err = ParseVersionError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParseVersionError(s.to_owned());
        let mut parts = s.split('.');
        let mut next = || {
            parts
                .next()
                .and_then(|p| p.parse::<u16>().ok())
                .ok_or_else(err)
        };
        let v = Self::new(next()?, next()?, next()?);
        if parts.next().is_some() {
            return Err(err());
        }
        Ok(v)
    }
}

impl TryFrom<String> for ProtocolVersion {
    type Error = ParseVersionError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<ProtocolVersion> for String {
    fn from(v: ProtocolVersion) -> Self {
        v.to_string()
    }
}

/// Version of the on-disk store layout. Bumped on every incompatible change
/// to a redb table; each bump ships a migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct SchemaVersion(u32);

impl SchemaVersion {
    /// The layout this build writes.
    pub const CURRENT: SchemaVersion = SchemaVersion(5);

    #[must_use]
    pub const fn new(n: u32) -> Self {
        Self(n)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What a client or daemon is, for logs and upgrade notices.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BuildInfo {
    /// e.g. `nits-client-tauri`, `nits-mcp`, `nitsd`.
    pub name: String,
    /// Crate version of the binary.
    pub version: String,
}

/// Advice attached to a successful handshake when the peer is behind.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UpgradeNotice {
    /// The version the daemon would prefer the client to speak.
    pub latest: ProtocolVersion,
    pub message: String,
}

/// Every frame on the wire: the protocol version plus the message.
///
/// Clients send `Envelope { v: <requested>, msg }`; the daemon replies with
/// `v` set to the negotiated version. A mismatch after handshake is a
/// protocol error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Envelope<T> {
    pub v: ProtocolVersion,
    pub msg: T,
}

impl<T> Envelope<T> {
    #[must_use]
    pub fn current(msg: T) -> Self {
        Self {
            v: ProtocolVersion::CURRENT,
            msg,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parses_and_rejects() {
        assert_eq!(
            "1.2.3".parse::<ProtocolVersion>().unwrap(),
            ProtocolVersion::new(1, 2, 3)
        );
        for bad in ["1.2", "1.2.3.4", "a.b.c", "", "1.2.-1"] {
            assert!(bad.parse::<ProtocolVersion>().is_err(), "{bad:?}");
        }
        assert_eq!(
            serde_json::to_string(&ProtocolVersion::CURRENT).unwrap(),
            "\"0.10.0\""
        );
    }

    #[test]
    fn compatibility_rule() {
        let d = ProtocolVersion::new(1, 3, 0);
        assert!(d.can_serve(ProtocolVersion::new(1, 3, 9)));
        assert!(d.can_serve(ProtocolVersion::new(1, 0, 0)));
        assert!(!d.can_serve(ProtocolVersion::new(1, 4, 0)));
        assert!(!d.can_serve(ProtocolVersion::new(2, 0, 0)));
        assert!(!d.can_serve(ProtocolVersion::new(0, 3, 0)));
    }

    proptest! {
        #[test]
        fn roundtrip(a in any::<u16>(), b in any::<u16>(), c in any::<u16>()) {
            let v = ProtocolVersion::new(a, b, c);
            prop_assert_eq!(v.to_string().parse::<ProtocolVersion>().unwrap(), v);
            let json = serde_json::to_string(&v).unwrap();
            prop_assert_eq!(serde_json::from_str::<ProtocolVersion>(&json).unwrap(), v);
        }

        #[test]
        fn ordering_matches_tuple(a in 0u16..4, b in 0u16..4, c in 0u16..4, d in 0u16..4, e in 0u16..4, f in 0u16..4) {
            prop_assert_eq!(
                ProtocolVersion::new(a, b, c).cmp(&ProtocolVersion::new(d, e, f)),
                (a, b, c).cmp(&(d, e, f))
            );
        }
    }
}
