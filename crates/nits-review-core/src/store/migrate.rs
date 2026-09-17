//! Forward-only schema migrations. One entry per version bump; the store
//! opens at any older version and is walked to `SchemaVersion::CURRENT`
//! one step per transaction.

use nits_protocol::SchemaVersion;
use redb::{Database, ReadableTable, WriteTransaction};

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
];

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
        // Later migrations may change event payloads. Stamp the old envelope
        // without decoding it as the current domain before those steps run.
        let mut events = Vec::new();
        for entry in tables.events.iter()? {
            let (seq, bytes) = entry?;
            let mut value: serde_json::Value = serde_json::from_slice(bytes.value())?;
            let schema = value.get_mut("schema").ok_or_else(|| StoreError::Corrupt {
                seq: nits_protocol::Seq::new(seq.value()),
                reason: "stored event envelope is missing its schema".into(),
            })?;
            let _: SchemaVersion = serde_json::from_value(schema.clone())?;
            *schema = serde_json::to_value(SchemaVersion::new(2))?;
            events.push((seq.value(), serde_json::to_vec(&value)?));
        }
        for (seq, bytes) in events {
            tables.events.insert(seq, bytes.as_slice())?;
        }
        tables.clear_views()?;
        tables.clear_view_seq()?;
        Ok(())
    }
    migrate(txn).map_err(|error| error.to_string())
}

/// Schema 5 admits deferred findings. Schema 4 already has current comment
/// provenance; preserve its events and rebuild every materialized view.
fn migrate_4_to_5(txn: &WriteTransaction) -> Result<(), String> {
    fn migrate(txn: &WriteTransaction) -> Result<(), StoreError> {
        let mut tables = tables::Write::open(txn)?;
        for (_, mut stored) in tables.all_events()? {
            stored.schema = SchemaVersion::new(5);
            tables.put_event(&stored)?;
        }
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

/// Preserve legacy diff provenance while distinguishing future Browse anchors.
/// Only `CommentCreated` carries `Comment.context`; other events retain their data.
fn migrate_3_to_4(txn: &WriteTransaction) -> Result<(), String> {
    let mut tables = tables::Write::open(txn).map_err(|e| e.to_string())?;
    let mut events = Vec::new();
    for entry in tables.events.iter().map_err(|e| e.to_string())? {
        let (seq, bytes) = entry.map_err(|e| e.to_string())?;
        let mut value: serde_json::Value =
            serde_json::from_slice(bytes.value()).map_err(|e| e.to_string())?;
        if let Some(context) = value.pointer_mut("/event/body/comment/context")
            && !context.is_null()
        {
            let change: nits_protocol::ChangeKind =
                serde_json::from_value(context.clone()).map_err(|e| e.to_string())?;
            *context = serde_json::to_value(nits_protocol::CommentContext::Diff { change })
                .map_err(|e| e.to_string())?;
        }
        let schema = value
            .get_mut("schema")
            .ok_or_else(|| "stored event envelope is missing its schema".to_owned())?;
        let _: SchemaVersion = serde_json::from_value(schema.clone()).map_err(|e| e.to_string())?;
        *schema = serde_json::to_value(SchemaVersion::new(4)).map_err(|e| e.to_string())?;
        events.push((
            seq.value(),
            serde_json::to_vec(&value).map_err(|e| e.to_string())?,
        ));
    }
    for (seq, bytes) in events {
        tables
            .events
            .insert(seq, bytes.as_slice())
            .map_err(|e| e.to_string())?;
    }
    tables.clear_views().map_err(|e| e.to_string())?;
    tables.clear_view_seq().map_err(|e| e.to_string())?;
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
