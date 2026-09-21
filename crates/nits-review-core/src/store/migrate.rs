//! Forward-only schema migrations. One entry per version bump; the store
//! opens at any older version and is walked to `SchemaVersion::CURRENT`
//! one step per transaction.

use nits_protocol::SchemaVersion;
use redb::{Database, WriteTransaction};

use super::{StoreError, tables};

#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error("no migration from schema {from} to {to}")]
    Missing {
        from: SchemaVersion,
        to: SchemaVersion,
    },
    #[error("migration {from}->{to} failed: {reason}")]
    Failed {
        from: SchemaVersion,
        to: SchemaVersion,
        reason: String,
    },
}

type Migration = fn(&WriteTransaction) -> Result<(), String>;

/// `MIGRATIONS[n]` upgrades a store at schema `n` to `n + 1`.
///
/// Schema 0 is "a store created before versioning existed" (no `meta` stamp);
/// upgrading it to 1 is a no-op because the tables are identical.
const MIGRATIONS: &[Migration] = &[
    migrate_0_to_1,
    migrate_1_to_2,
    migrate_2_to_3,
    migrate_3_to_4,
    migrate_4_to_5,
    migrate_5_to_6,
    migrate_6_to_7,
    migrate_7_to_8,
    migrate_8_to_9,
    migrate_9_to_10,
];

/// Schema 10 materializes immutable suggestion anchors and applied receipts.
/// Existing creation/application events are sufficient; never rewrite history.
fn migrate_9_to_10(txn: &WriteTransaction) -> Result<(), String> {
    fn migrate(txn: &WriteTransaction) -> Result<(), StoreError> {
        let mut tables = tables::Write::open(txn)?;
        tables.clear_views()?;
        tables.clear_view_seq()?;
        Ok(())
    }
    migrate(txn).map_err(|error| error.to_string())
}

/// Schema 7 captures working-tree HEAD provenance. Missing historical HEADs
/// deserialize as None: migrating never invents provenance from today's repo.
/// Rebuild views with the current shape and prevent older writers from opening
/// a store containing the newly captured field.
fn migrate_6_to_7(txn: &WriteTransaction) -> Result<(), String> {
    fn migrate(txn: &WriteTransaction) -> Result<(), StoreError> {
        let mut tables = tables::Write::open(txn)?;
        rewrite_raw_events(&mut tables, |stored| {
            stored.schema = SchemaVersion::new(7);
            Ok(())
        })?;
        tables.clear_views()?;
        tables.clear_view_seq()?;
        Ok(())
    }
    migrate(txn).map_err(|error| error.to_string())
}

/// Schema 3 materializes every historical `ReviewRequested`. The event format is
/// unchanged; rebuilding assigns each request its original event identity.
fn migrate_2_to_3(txn: &WriteTransaction) -> Result<(), String> {
    fn migrate(txn: &WriteTransaction) -> Result<(), StoreError> {
        let mut tables = tables::Write::open(txn)?;
        tables.clear_views()?;
        tables.clear_view_seq()?;
        Ok(())
    }
    migrate(txn).map_err(|error| error.to_string())
}

/// Schema 2 admits informational comments and threads. Historical Note roots
/// retain their actionable lifecycle, including every resolution and reply.
fn migrate_1_to_2(txn: &WriteTransaction) -> Result<(), String> {
    fn migrate(txn: &WriteTransaction) -> Result<(), StoreError> {
        let mut tables = tables::Write::open(txn)?;
        rewrite_raw_events(&mut tables, |stored| {
            stored.schema = SchemaVersion::new(2);
            Ok(())
        })?;
        tables.clear_views()?;
        tables.clear_view_seq()?;
        Ok(())
    }
    migrate(txn).map_err(|error| error.to_string())
}

#[allow(clippy::unnecessary_wraps)] // must match the `Migration` fn-pointer type
fn migrate_0_to_1(_txn: &WriteTransaction) -> Result<(), String> {
    Ok(())
}

/// Bring `db` from `stored` to `SchemaVersion::CURRENT`, or refuse.
pub(super) fn run(db: &Database, stored: SchemaVersion) -> Result<(), StoreError> {
    let current = SchemaVersion::CURRENT;
    if stored > current {
        return Err(StoreError::SchemaTooNew {
            found: stored,
            supported: current,
        });
    }
    let mut at = stored;
    while at < current {
        let next = SchemaVersion::new(at.get() + 1);
        let step = MIGRATIONS
            .get(at.get() as usize)
            .ok_or(MigrationError::Missing { from: at, to: next })?;
        let txn = db.begin_write()?;
        step(&txn).map_err(|reason| MigrationError::Failed {
            from: at,
            to: next,
            reason,
        })?;
        {
            let mut meta = txn.open_table(tables::META)?;
            meta.insert(tables::META_SCHEMA, u64::from(next.get()))?;
        }
        txn.commit()?;
        at = next;
    }
    Ok(())
}

/// The envelope was stable before revision provenance existed. Its event body
/// remains raw until every schema step has introduced the required fields.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct RawStoredEvent {
    schema: SchemaVersion,
    event: serde_json::Value,
}

/// Rewrite raw envelopes before decoding current types, so older migrations never
/// attempt to deserialize fields introduced by a later schema.
fn rewrite_raw_events(
    tables: &mut tables::Write<'_>,
    rewrite: impl Fn(&mut RawStoredEvent) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    use redb::ReadableTable;
    let rows = tables
        .events
        .iter()?
        .map(|row| {
            let (key, value) = row?;
            let mut stored: RawStoredEvent = serde_json::from_slice(value.value())?;
            rewrite(&mut stored)?;
            Ok((key.value(), serde_json::to_vec(&stored)?))
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    for (key, value) in rows {
        tables.events.insert(key, value.as_slice())?;
    }
    Ok(())
}

/// No historical request can truthfully inherit today's moving targets.
fn migrate_5_to_6(txn: &WriteTransaction) -> Result<(), String> {
    fn migrate(txn: &WriteTransaction) -> Result<(), StoreError> {
        let mut tables = tables::Write::open(txn)?;
        rewrite_raw_events(&mut tables, |stored| {
            if let Some(body) = stored
                .event
                .get_mut("body")
                .and_then(serde_json::Value::as_object_mut)
                && body.get("type").and_then(serde_json::Value::as_str) == Some("ReviewRequested")
            {
                body.insert("targets".into(), serde_json::json!({"type": "Unknown"}));
            }
            stored.schema = SchemaVersion::new(6);
            Ok(())
        })?;
        tables.clear_views()?;
        tables.clear_view_seq()?;
        Ok(())
    }
    migrate(txn).map_err(|error| error.to_string())
}

/// Preserve legacy diff provenance while distinguishing future Browse anchors.
/// Keep request bodies raw until schema 6 supplies their unknown targets.
fn migrate_3_to_4(txn: &WriteTransaction) -> Result<(), String> {
    fn migrate(txn: &WriteTransaction) -> Result<(), StoreError> {
        let mut tables = tables::Write::open(txn)?;
        rewrite_raw_events(&mut tables, |stored| {
            if let Some(context) = stored.event.pointer_mut("/body/comment/context")
                && !context.is_null()
            {
                let change = context.take();
                *context = serde_json::json!({"type": "Diff", "change": change});
            }
            stored.schema = SchemaVersion::new(4);
            Ok(())
        })?;
        tables.clear_views()?;
        tables.clear_view_seq()?;
        Ok(())
    }
    migrate(txn).map_err(|error| error.to_string())
}

/// Schema 5 admits deferred findings, preserving all prior raw event bodies.
fn migrate_4_to_5(txn: &WriteTransaction) -> Result<(), String> {
    fn migrate(txn: &WriteTransaction) -> Result<(), StoreError> {
        let mut tables = tables::Write::open(txn)?;
        rewrite_raw_events(&mut tables, |stored| {
            stored.schema = SchemaVersion::new(5);
            Ok(())
        })?;
        tables.clear_views()?;
        tables.clear_view_seq()?;
        Ok(())
    }
    migrate(txn).map_err(|error| error.to_string())
}

/// Preserve historical blob/deletion marks while admitting commit-valued gitlinks.
fn migrate_7_to_8(txn: &WriteTransaction) -> Result<(), String> {
    fn migrate(txn: &WriteTransaction) -> Result<(), StoreError> {
        let mut tables = tables::Write::open(txn)?;
        rewrite_raw_events(&mut tables, |stored| {
            if let Some(body) = stored
                .event
                .get_mut("body")
                .and_then(serde_json::Value::as_object_mut)
                && body.get("type").and_then(serde_json::Value::as_str) == Some("FileViewed")
                && let Some(blob) = body.remove("blob_oid")
            {
                let oid: Option<nits_protocol::BlobOid> = serde_json::from_value(blob)?;
                body.insert(
                    "content".into(),
                    match oid {
                        Some(oid) => serde_json::json!({"type": "Blob", "oid": oid}),
                        None => serde_json::json!({"type": "Missing"}),
                    },
                );
            }
            stored.schema = SchemaVersion::new(8);
            Ok(())
        })?;
        tables.clear_views()?;
        tables.clear_view_seq()?;
        Ok(())
    }
    migrate(txn).map_err(|error| error.to_string())
}

/// Schema 9 includes Git modes in blob identities. Historical marks and comment
/// diffs never recorded modes, so preserve the OIDs with explicit Unknown modes.
/// Gitlink commits remain commits, and original event identity/discussion stays
/// untouched. Derived views are rebuilt only after every migration completes.
fn migrate_8_to_9(txn: &WriteTransaction) -> Result<(), String> {
    fn unknown_mode(blob: &mut serde_json::Value) {
        if blob.is_string() {
            let oid = blob.take();
            *blob = serde_json::json!({"oid": oid, "mode": "Unknown"});
        }
    }
    fn migrate(txn: &WriteTransaction) -> Result<(), StoreError> {
        let mut tables = tables::Write::open(txn)?;
        rewrite_raw_events(&mut tables, |stored| {
            if let Some(content) = stored.event.pointer_mut("/body/content")
                && content.get("type").and_then(serde_json::Value::as_str) == Some("Blob")
                && let Some(fields) = content.as_object_mut()
                && let Some(oid) = fields.remove("oid")
            {
                fields.insert(
                    "entry".into(),
                    serde_json::json!({"oid": oid, "mode": "Unknown"}),
                );
            }
            if let Some(change) = stored.event.pointer_mut("/body/comment/context/change") {
                let fields: &[&str] = match change.get("type").and_then(serde_json::Value::as_str) {
                    Some("Added") => &["new"],
                    Some("Deleted") => &["old"],
                    Some("Modified" | "Renamed") => &["old", "new"],
                    _ => &[],
                };
                for field in fields {
                    if let Some(blob) = change.get_mut(*field) {
                        unknown_mode(blob);
                    }
                }
                if let Some(submodule) = change.get_mut("change") {
                    let field = match submodule.get("type").and_then(serde_json::Value::as_str) {
                        Some("BlobToSubmodule") => Some("old"),
                        Some("SubmoduleToBlob") => Some("new"),
                        _ => None,
                    };
                    if let Some(blob) = field.and_then(|field| submodule.get_mut(field)) {
                        unknown_mode(blob);
                    }
                }
            }
            stored.schema = SchemaVersion::new(9);
            Ok(())
        })?;
        tables.clear_views()?;
        tables.clear_view_seq()?;
        Ok(())
    }
    migrate(txn).map_err(|error| error.to_string())
}
