//! `JsonSchema` for types whose serde form is hand-written (ids, OIDs,
//! validated strings and ranges). Everything else derives it behind the
//! `schema` feature. The schemas describe the wire form exactly as serde
//! produces it, so `schemars` output matches the fixtures.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};

use crate::ids::{ClientId, CommentId, Oid, RepoId, ReviewId, ThreadId, UpgradeId, WorkspaceId};
use crate::invariants::{ColRange, LineRange, NonEmpty, RepoPath};
use crate::lifecycle::{BuildDigest, ReleaseChannel, ReleaseVersion};
use crate::version::ProtocolVersion;

/// `string_schema!(Type, "description", "regex")` implements `JsonSchema`
/// for a type that serialises as a string matching `regex`, inlined at each
/// use. Exists because every id and validated string needs the same shape.
macro_rules! string_schema {
    ($ty:ty, $desc:literal, $pattern:expr) => {
        impl JsonSchema for $ty {
            fn schema_name() -> Cow<'static, str> {
                Cow::Borrowed(stringify!($ty))
            }
            fn inline_schema() -> bool {
                true
            }
            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({ "type": "string", "description": $desc, "pattern": $pattern })
            }
        }
    };
}

const ULID: &str = "^[0-7][0-9A-HJKMNP-TV-Z]{25}$";
const HEX40: &str = "^[0-9a-f]{40}$";

string_schema!(UpgradeId, "Managed replacement operation id (ULID)", ULID);
string_schema!(BuildDigest, "Executable SHA-256 digest", "^[0-9a-f]{64}$");
string_schema!(
    ReleaseChannel,
    "Installed release channel",
    "^[A-Za-z0-9._-]{1,64}$"
);
string_schema!(
    ReleaseVersion,
    "Semantic release version",
    "^[0-9]+\\.[0-9]+\\.[0-9]+([+-].*)?$"
);
string_schema!(WorkspaceId, "Workspace id (ULID)", ULID);
string_schema!(RepoId, "Repo id (ULID)", ULID);
string_schema!(ReviewId, "Review id (ULID)", ULID);
string_schema!(CommentId, "Comment id (ULID)", ULID);
string_schema!(
    ThreadId,
    "Thread id (ULID); equals its root comment id",
    ULID
);
string_schema!(ClientId, "Client instance id (ULID)", ULID);
string_schema!(Oid, "Git object id, 40 hex chars", HEX40);
string_schema!(
    RepoPath,
    "Slash-separated path relative to the repo root; no leading slash, no `.`/`..` segments",
    "^[^/]"
);
string_schema!(
    ProtocolVersion,
    "Protocol version `major.minor.patch`",
    "^[0-9]+\\.[0-9]+\\.[0-9]+$"
);

impl<T: JsonSchema> JsonSchema for NonEmpty<T> {
    fn schema_name() -> Cow<'static, str> {
        Cow::Owned(format!("NonEmpty_{}", T::schema_name()))
    }
    fn inline_schema() -> bool {
        true
    }
    fn json_schema(g: &mut SchemaGenerator) -> Schema {
        let items = g.subschema_for::<T>();
        json_schema!({ "type": "array", "items": items, "minItems": 1 })
    }
}

impl JsonSchema for LineRange {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("LineRange")
    }
    fn json_schema(g: &mut SchemaGenerator) -> Schema {
        let mut s = crate::invariants::RawLineRange::json_schema(g);
        s.insert(
            "description".into(),
            "Inclusive 1-based line range, start <= end".into(),
        );
        s
    }
}

impl JsonSchema for ColRange {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("ColRange")
    }
    fn json_schema(g: &mut SchemaGenerator) -> Schema {
        let mut s = crate::invariants::RawColRange::json_schema(g);
        s.insert(
            "description".into(),
            "Half-open byte column range [start, end), start <= end".into(),
        );
        s
    }
}
