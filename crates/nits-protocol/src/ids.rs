//! Identifier newtypes: ULID-backed entity ids, git object ids, sequence numbers.
//!
//! Every id is its own type so a `ReviewId` can never be passed where a
//! `CommentId` is expected. All of them serialise as strings (ULID / hex) except
//! the sequence counters, which are plain integers.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};

/// Error parsing an identifier from its string form.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseIdError {
    #[error("invalid ULID: {0}")]
    Ulid(String),
    #[error("invalid object id: expected 40 hex characters, got {0:?}")]
    Oid(String),
}

/// `ulid_id!(Name)` defines a `Copy` newtype over `ulid::Ulid` with
/// `from_parts`/`nil`/`timestamp_ms`, `Display`/`FromStr` in canonical ULID
/// form, and serde as that string. One macro so every id type is identical
/// except in name, which is the whole point of the newtypes.
macro_rules! ulid_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(ulid::Ulid);

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
                s.parse().map_err(serde::de::Error::custom)
            }
        }

        impl $name {
            /// Build an id from a millisecond timestamp and 80 bits of entropy.
            ///
            /// Hosts supply both; this crate never reads a clock or an RNG so it
            /// stays wasm-safe.
            #[must_use]
            pub fn from_parts(timestamp_ms: u64, random: u128) -> Self {
                Self(ulid::Ulid::from_parts(timestamp_ms, random))
            }

            /// The nil id (all zeros). Useful as a sentinel in tests only.
            #[must_use]
            pub const fn nil() -> Self {
                Self(ulid::Ulid::nil())
            }

            /// Millisecond timestamp component.
            #[must_use]
            pub fn timestamp_ms(self) -> u64 {
                self.0.timestamp_ms()
            }

            /// Random component (80 bits).
            #[must_use]
            pub fn random(self) -> u128 {
                self.0.random()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $name {
            type Err = ParseIdError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                ulid::Ulid::from_string(s)
                    .map(Self)
                    .map_err(|e| ParseIdError::Ulid(e.to_string()))
            }
        }
    };
}

ulid_id!(
    /// A workspace: a named group of repositories.
    WorkspaceId
);
ulid_id!(
    /// A repository attached to a workspace.
    RepoId
);
ulid_id!(
    /// A review.
    ReviewId
);
ulid_id!(
    /// A comment. Client-generated so creation can be optimistic.
    CommentId
);
ulid_id!(
    /// A comment thread. Equal to the root comment's id.
    ThreadId
);
ulid_id!(
    /// A connected client instance, for attributing `client_seq`.
    ClientId
);

/// Stable identity of a review request, assigned by its committed log event.
/// The store's global event sequence is unique and survives rebuilds and upgrades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct ReviewRequestId(Seq);

impl ReviewRequestId {
    #[must_use]
    pub const fn from_event_seq(seq: Seq) -> Self {
        Self(seq)
    }

    #[must_use]
    pub const fn event_seq(self) -> Seq {
        self.0
    }
}

impl fmt::Display for ReviewRequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Raw 20-byte git object id (SHA-1). Wrapped by the typed OIDs below.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Oid([u8; 20]);

impl Oid {
    /// Number of bytes in an object id.
    pub const LEN: usize = 20;

    #[must_use]
    pub const fn from_bytes(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    /// The all-zero id.
    #[must_use]
    pub const fn zero() -> Self {
        Self([0; 20])
    }
}

impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Oid({self})")
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for Oid {
    type Err = ParseIdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParseIdError::Oid(s.to_owned());
        if s.len() != 40 {
            return Err(err());
        }
        let mut out = [0u8; 20];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hi = hex_val(chunk[0]).ok_or_else(err)?;
            let lo = hex_val(chunk[1]).ok_or_else(err)?;
            out[i] = (hi << 4) | lo;
        }
        Ok(Self(out))
    }
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

impl Serialize for Oid {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Oid {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// `oid_newtype!(Name)` defines a `Copy` newtype over [`Oid`] with
/// `new`/`from_bytes`/`oid`, hex `Display`/`FromStr`, and transparent serde.
/// Blob/commit/tree ids share a representation but must not be mixed up.
macro_rules! oid_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
        #[serde(transparent)]
        pub struct $name(Oid);

        impl $name {
            #[must_use]
            pub const fn new(oid: Oid) -> Self {
                Self(oid)
            }

            #[must_use]
            pub const fn from_bytes(bytes: [u8; 20]) -> Self {
                Self(Oid::from_bytes(bytes))
            }

            #[must_use]
            pub const fn oid(self) -> Oid {
                self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $name {
            type Err = ParseIdError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                s.parse().map(Self)
            }
        }
    };
}

oid_newtype!(
    /// Id of a blob (file content).
    BlobOid
);
oid_newtype!(
    /// Id of a commit.
    CommitOid
);
oid_newtype!(
    /// Id of a tree. Working-tree snapshots carry a synthetic tree id.
    TreeOid
);

/// Daemon-assigned global event sequence number. Strictly increasing from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct Seq(u64);

impl Seq {
    /// The first sequence number a committed event can have.
    pub const FIRST: Seq = Seq(1);

    #[must_use]
    pub const fn new(n: u64) -> Self {
        Self(n)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for Seq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Client-assigned per-connection counter, for matching optimistic events to
/// their committed counterpart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct ClientSeq(u64);

impl ClientSeq {
    #[must_use]
    pub const fn new(n: u64) -> Self {
        Self(n)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// Per-connection request id for multiplexing requests, responses and streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct RequestId(u64);

impl RequestId {
    #[must_use]
    pub const fn new(n: u64) -> Self {
        Self(n)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Milliseconds since the Unix epoch, UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    #[must_use]
    pub const fn from_millis(ms: i64) -> Self {
        Self(ms)
    }

    #[must_use]
    pub const fn millis(self) -> i64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn oid_parses_and_displays() {
        let s = "0123456789abcdef0123456789abcdef01234567";
        let oid: Oid = s.parse().unwrap();
        assert_eq!(oid.to_string(), s);
        assert_eq!(
            "ABCDEF".parse::<Oid>(),
            Err(ParseIdError::Oid("ABCDEF".into()))
        );
        assert!(
            "zz23456789abcdef0123456789abcdef01234567"
                .parse::<Oid>()
                .is_err()
        );
    }

    #[test]
    fn ulid_ids_are_distinct_types_with_string_serde() {
        let id = ReviewId::from_parts(1_700_000_000_000, 42);
        let json = serde_json::to_string(&id).unwrap();
        assert!(json.starts_with('"'));
        let back: ReviewId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
        assert_eq!(id.to_string().parse::<ReviewId>().unwrap(), id);
        assert!("not a ulid".parse::<ReviewId>().is_err());
    }

    proptest! {
        #[test]
        fn oid_roundtrip(bytes in any::<[u8; 20]>()) {
            let oid = BlobOid::from_bytes(bytes);
            prop_assert_eq!(oid.to_string().parse::<BlobOid>().unwrap(), oid);
            let json = serde_json::to_string(&oid).unwrap();
            prop_assert_eq!(serde_json::from_str::<BlobOid>(&json).unwrap(), oid);
        }

        #[test]
        fn ulid_roundtrip(ts in 0u64..(1u64 << 48), rand in any::<u128>()) {
            let id = CommentId::from_parts(ts, rand);
            prop_assert_eq!(id.to_string().parse::<CommentId>().unwrap(), id);
            let json = serde_json::to_string(&id).unwrap();
            prop_assert_eq!(serde_json::from_str::<CommentId>(&json).unwrap(), id);
            prop_assert_eq!(id.timestamp_ms(), ts);
        }

        #[test]
        fn seq_is_ordered(a in any::<u64>(), b in any::<u64>()) {
            prop_assert_eq!(Seq::new(a) < Seq::new(b), a < b);
        }
    }
}
