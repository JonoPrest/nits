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
const MIGRATIONS: &[Migration] = &[migrate_0_to_1, migrate_1_to_2, migrate_2_to_3];

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
        for (_, mut stored) in tables.all_events()? {
            stored.schema = SchemaVersion::new(2);
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
