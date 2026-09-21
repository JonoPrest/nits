//! Table definitions and a thin typed wrapper over a write transaction.

use nits_protocol::{SchemaVersion, Seq};
use redb::{ReadableTable, ReadableTableMetadata, Table, TableDefinition, WriteTransaction};

use super::{StoreError, StoredEvent};

pub(super) const META: TableDefinition<'_, &str, u64> = TableDefinition::new("meta");
pub(super) const EVENTS: TableDefinition<'_, u64, &[u8]> = TableDefinition::new("events");
pub(super) const WORKSPACES: TableDefinition<'_, &str, &[u8]> = TableDefinition::new("workspaces");
pub(super) const REVIEWS: TableDefinition<'_, &str, &[u8]> = TableDefinition::new("reviews");
pub(super) const COMMENTS: TableDefinition<'_, (&str, &str), &[u8]> =
    TableDefinition::new("comments");
pub(super) const SUGGESTIONS: TableDefinition<'_, (&str, &str), &[u8]> =
    TableDefinition::new("suggestions");
pub(super) const THREADS: TableDefinition<'_, (&str, &str), &[u8]> =
    TableDefinition::new("threads");
pub(super) const REVIEW_REQUESTS: TableDefinition<'_, (&str, u64), &[u8]> =
    TableDefinition::new("review_requests");
pub(super) const CHECKPOINTS: TableDefinition<'_, (&str, u64), &[u8]> =
    TableDefinition::new("review_checkpoints");
pub(super) const VIEWED: TableDefinition<'_, (&str, &str, &str), &[u8]> =
    TableDefinition::new("viewed");
pub(super) const ANCHORS_BY_BLOB: TableDefinition<'_, (&str, &str, &str), &str> =
    TableDefinition::new("anchors_by_blob");

pub(super) const META_SCHEMA: &str = "schema_version";
pub(super) const META_VIEW_SEQ: &str = "view_seq";

/// Create every table if missing and stamp a fresh store with the current
/// schema. Returns the schema version the store carries after this call
/// (which may still be old and need migrating).
pub(super) fn ensure(txn: &WriteTransaction) -> Result<SchemaVersion, StoreError> {
    let mut meta = txn.open_table(META)?;
    txn.open_table(EVENTS)?;
    txn.open_table(WORKSPACES)?;
    txn.open_table(REVIEWS)?;
    txn.open_table(COMMENTS)?;
    txn.open_table(SUGGESTIONS)?;
    txn.open_table(THREADS)?;
    txn.open_table(VIEWED)?;
    txn.open_table(REVIEW_REQUESTS)?;
    txn.open_table(CHECKPOINTS)?;
    txn.open_table(ANCHORS_BY_BLOB)?;
    let existing = meta.get(META_SCHEMA)?.map(|v| v.value());
    let v = if let Some(v) = existing {
        v
    } else {
        // An unstamped nonempty log predates schema versioning. Do not mark
        // it current before raw-event migrations have had a chance to run.
        let version = if txn.open_table(EVENTS)?.is_empty()? {
            u64::from(SchemaVersion::CURRENT.get())
        } else {
            0
        };
        meta.insert(META_SCHEMA, version)?;
        version
    };
    #[allow(clippy::cast_possible_truncation)]
    Ok(SchemaVersion::new(v as u32))
}

/// All tables opened for writing within one transaction.
pub(super) struct Write<'txn> {
    pub meta: Table<'txn, &'static str, u64>,
    pub events: Table<'txn, u64, &'static [u8]>,
    pub workspaces: Table<'txn, &'static str, &'static [u8]>,
    pub reviews: Table<'txn, &'static str, &'static [u8]>,
    pub comments: Table<'txn, (&'static str, &'static str), &'static [u8]>,
    pub suggestions: Table<'txn, (&'static str, &'static str), &'static [u8]>,
    pub threads: Table<'txn, (&'static str, &'static str), &'static [u8]>,
    pub requests: Table<'txn, (&'static str, u64), &'static [u8]>,
    pub checkpoints: Table<'txn, (&'static str, u64), &'static [u8]>,
    pub viewed: Table<'txn, (&'static str, &'static str, &'static str), &'static [u8]>,
    pub anchors_by_blob: Table<'txn, (&'static str, &'static str, &'static str), &'static str>,
}

impl std::fmt::Debug for Write<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("tables::Write")
    }
}

impl<'txn> Write<'txn> {
    pub fn open(txn: &'txn WriteTransaction) -> Result<Self, StoreError> {
        Ok(Self {
            meta: txn.open_table(META)?,
            events: txn.open_table(EVENTS)?,
            workspaces: txn.open_table(WORKSPACES)?,
            reviews: txn.open_table(REVIEWS)?,
            comments: txn.open_table(COMMENTS)?,
            suggestions: txn.open_table(SUGGESTIONS)?,
            threads: txn.open_table(THREADS)?,
            viewed: txn.open_table(VIEWED)?,
            requests: txn.open_table(REVIEW_REQUESTS)?,
            checkpoints: txn.open_table(CHECKPOINTS)?,
            anchors_by_blob: txn.open_table(ANCHORS_BY_BLOB)?,
        })
    }

    pub fn last_seq(&self) -> Result<Option<Seq>, StoreError> {
        Ok(self.events.last()?.map(|(k, _)| Seq::new(k.value())))
    }

    pub fn put_event(&mut self, stored: &StoredEvent) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec(stored)?;
        self.events
            .insert(stored.event.seq.get(), bytes.as_slice())?;
        Ok(())
    }

    pub fn all_events(&self) -> Result<Vec<(Seq, StoredEvent)>, StoreError> {
        let mut out = Vec::with_capacity(usize::try_from(self.events.len()?).unwrap_or(0));
        for e in self.events.iter()? {
            let (k, v) = e?;
            let seq = Seq::new(k.value());
            let stored = serde_json::from_slice(v.value()).map_err(|e| StoreError::Corrupt {
                seq,
                reason: e.to_string(),
            })?;
            out.push((seq, stored));
        }
        Ok(out)
    }

    pub fn set_view_seq(&mut self, seq: Seq) -> Result<(), StoreError> {
        self.meta.insert(META_VIEW_SEQ, seq.get())?;
        Ok(())
    }

    pub fn clear_view_seq(&mut self) -> Result<(), StoreError> {
        self.meta.remove(META_VIEW_SEQ)?;
        Ok(())
    }

    pub fn clear_views(&mut self) -> Result<(), StoreError> {
        self.workspaces.retain(|_, _| false)?;
        self.reviews.retain(|_, _| false)?;
        self.comments.retain(|_, _| false)?;
        self.suggestions.retain(|_, _| false)?;
        self.threads.retain(|_, _| false)?;
        self.viewed.retain(|_, _| false)?;
        self.requests.retain(|_, _| false)?;
        self.checkpoints.retain(|_, _| false)?;
        self.anchors_by_blob.retain(|_, _| false)?;
        Ok(())
    }
}
