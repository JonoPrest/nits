//! Append-only event log with materialised views, on redb.
//!
//! See `docs/ARCHITECTURE.md` §4.2 and §4.9. The log (`events`) is the only
//! source of truth; every other table is a view derived by [`views::apply`]
//! and can be rebuilt from scratch with [`Store::rebuild_views`].
//!
//! Tables:
//!
//! | table            | key                                  | value          |
//! |------------------|--------------------------------------|----------------|
//! | `meta`           | `"schema_version"` / `"view_seq"`    | u64            |
//! | `events`         | `seq: u64`                           | `StoredEvent`  |
//! | `workspaces`     | `workspace_id`                       | `Workspace`    |
//! | `reviews`        | `review_id`                          | `ReviewRecord` |
//! | `comments`       | `(review_id, comment_id)`            | `Comment`      |
//! | `review_requests`| `(review_id, event_seq)`             | `ReviewRequest` |
//! | `threads`        | `(review_id, thread_id)`             | `Thread`       |
//! | `viewed`         | `(review_id, repo_id, path)`         | `ViewedMark`   |
//! | `anchors_by_blob`| `(repo_id, blob_oid, comment_id)`    | `review_id`    |
//!
//! All values are JSON. Ids are stored as their canonical strings so keys are
//! human-readable in a hex dump and sort by ULID time.

mod migrate;
mod tables;
mod views;

use std::path::Path;

use nits_protocol::{
    Author, BlobOid, ClientId, ClientSeq, Comment, Event, EventBody, RepoId, ReviewId,
    SchemaVersion, Seq, Thread, Timestamp, ViewedMark, Workspace, WorkspaceId,
};
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata};
use serde::{Deserialize, Serialize};

pub use migrate::MigrationError;
pub use views::{ReviewLifecycle, ReviewRecord};

/// An event the store has not yet assigned a `Seq` to. Constructed by
/// review/comment logic once a mutation has been validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEvent {
    pub ts: Timestamp,
    pub author: Author,
    pub client_id: ClientId,
    pub client_seq: ClientSeq,
    pub body: EventBody,
}

/// What actually sits in the `events` table: the event plus the schema it
/// was written under, so a future migration can re-serialise per event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredEvent {
    pub schema: SchemaVersion,
    pub event: Event,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(
        "store was written by a newer nitsd (schema {found}, this build supports up to {supported})"
    )]
    SchemaTooNew {
        found: SchemaVersion,
        supported: SchemaVersion,
    },
    #[error("migration failed: {0}")]
    Migration(#[from] MigrationError),
    #[error("corrupt store at seq {seq}: {reason}")]
    Corrupt { seq: Seq, reason: String },
    #[error("event at seq {seq} does not apply to current views: {reason}")]
    Inconsistent { seq: Seq, reason: String },
    #[error("encoding: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Redb(#[from] redb::Error),
}

macro_rules! from_redb {
    ($($e:ty),*) => {$(
        impl From<$e> for StoreError {
            fn from(e: $e) -> Self {
                StoreError::Redb(e.into())
            }
        }
    )*};
}
from_redb!(
    redb::DatabaseError,
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError
);

/// The event store. Cheap to share behind an `Arc`; redb serialises writers.
#[derive(Debug)]
pub struct Store {
    db: Database,
}

impl Store {
    /// Open or create the store at `path`, migrating an older schema forward
    /// and rebuilding views if they are behind the log.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let db = Database::create(path)?;
        let store = Self { db };
        store.init()?;
        Ok(store)
    }

    fn init(&self) -> Result<(), StoreError> {
        let stored = {
            let txn = self.db.begin_write()?;
            let version = tables::ensure(&txn)?;
            txn.commit()?;
            version
        };
        migrate::run(&self.db, stored)?;
        if self.view_seq()? != self.last_seq()? {
            self.rebuild_views()?;
        }
        Ok(())
    }

    /// Append one event, assigning the next `Seq` and updating every view in
    /// the same transaction.
    pub fn append(&self, new: NewEvent) -> Result<Event, StoreError> {
        let txn = self.db.begin_write()?;
        let event = {
            let mut t = tables::Write::open(&txn)?;
            let seq = t.last_seq()?.map_or(Seq::FIRST, Seq::next);
            let event = Event {
                seq,
                ts: new.ts,
                author: new.author,
                client_id: new.client_id,
                client_seq: new.client_seq,
                body: new.body,
            };
            views::apply(&mut t, &event)?;
            t.put_event(&StoredEvent {
                schema: SchemaVersion::CURRENT,
                event: event.clone(),
            })?;
            t.set_view_seq(seq)?;
            event
        };
        txn.commit()?;
        Ok(event)
    }

    /// The highest `Seq` in the log, or `None` if empty.
    pub fn last_seq(&self) -> Result<Option<Seq>, StoreError> {
        let txn = self.db.begin_read()?;
        let events = txn.open_table(tables::EVENTS)?;
        Ok(events.last()?.map(|(k, _)| Seq::new(k.value())))
    }

    fn view_seq(&self) -> Result<Option<Seq>, StoreError> {
        let txn = self.db.begin_read()?;
        let meta = txn.open_table(tables::META)?;
        Ok(meta
            .get(tables::META_VIEW_SEQ)?
            .map(|v| Seq::new(v.value())))
    }

    /// Every event with `seq > after` (all events if `None`), in order.
    pub fn events_after(&self, after: Option<Seq>) -> Result<Vec<Event>, StoreError> {
        let txn = self.db.begin_read()?;
        let events = txn.open_table(tables::EVENTS)?;
        let start = after.map_or(0, |s| s.get() + 1);
        let mut out = Vec::new();
        for entry in events.range(start..)? {
            let (k, v) = entry?;
            let stored: StoredEvent =
                serde_json::from_slice(v.value()).map_err(|e| StoreError::Corrupt {
                    seq: Seq::new(k.value()),
                    reason: e.to_string(),
                })?;
            out.push(stored.event);
        }
        Ok(out)
    }

    /// Drop every view table and fold the whole log again.
    pub fn rebuild_views(&self) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut t = tables::Write::open(&txn)?;
            t.clear_views()?;
            let mut last = None;
            for (seq, stored) in t.all_events()? {
                views::apply(&mut t, &stored.event)?;
                last = Some(seq);
            }
            match last {
                Some(seq) => t.set_view_seq(seq)?,
                None => t.clear_view_seq()?,
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Schema version currently stamped in the store.
    pub fn schema_version(&self) -> Result<SchemaVersion, StoreError> {
        let txn = self.db.begin_read()?;
        let meta = txn.open_table(tables::META)?;
        let v = meta.get(tables::META_SCHEMA)?.map_or(0, |v| v.value());
        #[allow(clippy::cast_possible_truncation)]
        Ok(SchemaVersion::new(v as u32))
    }

    // ---- queries over views -------------------------------------------------

    pub fn workspaces(&self) -> Result<Vec<Workspace>, StoreError> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(tables::WORKSPACES)?;
        t.iter()?
            .map(|e| Ok(serde_json::from_slice(e?.1.value())?))
            .collect()
    }

    pub fn workspace(&self, id: WorkspaceId) -> Result<Option<Workspace>, StoreError> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(tables::WORKSPACES)?;
        t.get(id.to_string().as_str())?
            .map(|v| Ok(serde_json::from_slice(v.value())?))
            .transpose()
    }

    /// Live reviews in a workspace (tombstoned reviews are excluded).
    pub fn reviews(&self, workspace: WorkspaceId) -> Result<Vec<ReviewRecord>, StoreError> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(tables::REVIEWS)?;
        let mut out = Vec::new();
        for e in t.iter()? {
            let rec: ReviewRecord = serde_json::from_slice(e?.1.value())?;
            if rec.review.workspace_id == workspace
                && matches!(rec.lifecycle, ReviewLifecycle::Live)
            {
                out.push(rec);
            }
        }
        Ok(out)
    }

    /// A review by id, including tombstoned ones (callers decide).
    pub fn review(&self, id: ReviewId) -> Result<Option<ReviewRecord>, StoreError> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(tables::REVIEWS)?;
        t.get(id.to_string().as_str())?
            .map(|v| Ok(serde_json::from_slice(v.value())?))
            .transpose()
    }

    /// One coherent read of every materialized field and its subscription cursor.
    /// A concurrent writer is either visible in both the data and seq, or neither.
    pub fn review_snapshot(
        &self,
        id: ReviewId,
    ) -> Result<Option<nits_protocol::ReviewSnapshot>, StoreError> {
        let txn = self.db.begin_read()?;
        let rid = id.to_string();
        let reviews = txn.open_table(tables::REVIEWS)?;
        let Some(record) = reviews.get(rid.as_str())? else {
            return Ok(None);
        };
        let record: ReviewRecord = serde_json::from_slice(record.value())?;
        if matches!(record.lifecycle, ReviewLifecycle::Deleted { .. }) {
            return Ok(None);
        }
        let threads = txn.open_table(tables::THREADS)?;
        let comments = txn.open_table(tables::COMMENTS)?;
        let viewed = txn.open_table(tables::VIEWED)?;
        let requests = txn.open_table(tables::REVIEW_REQUESTS)?;
        let checkpoints = txn.open_table(tables::CHECKPOINTS)?;
        Ok(Some(nits_protocol::ReviewSnapshot {
            review: record.review,
            resolved: record.resolved,
            threads: threads
                .range((rid.as_str(), "")..(rid.as_str(), "\u{10FFFF}"))?
                .map(|e| Ok(serde_json::from_slice(e?.1.value())?))
                .collect::<Result<_, StoreError>>()?,
            comments: comments
                .range((rid.as_str(), "")..(rid.as_str(), "\u{10FFFF}"))?
                .map(|e| Ok(serde_json::from_slice(e?.1.value())?))
                .collect::<Result<_, StoreError>>()?,
            viewed: viewed
                .range((rid.as_str(), "", "")..(rid.as_str(), "\u{10FFFF}", ""))?
                .map(|e| Ok(serde_json::from_slice(e?.1.value())?))
                .collect::<Result<_, StoreError>>()?,
            requests: requests
                .range((rid.as_str(), 0)..=(rid.as_str(), u64::MAX))?
                .map(|e| Ok(serde_json::from_slice(e?.1.value())?))
                .collect::<Result<_, StoreError>>()?,
            checkpoints: checkpoints
                .range((rid.as_str(), 0)..=(rid.as_str(), u64::MAX))?
                .map(|e| Ok(serde_json::from_slice(e?.1.value())?))
                .collect::<Result<_, StoreError>>()?,
            seq: txn
                .open_table(tables::META)?
                .get(tables::META_VIEW_SEQ)?
                .map_or(Seq::new(0), |v| Seq::new(v.value())),
        }))
    }

    pub fn comments(&self, review: ReviewId) -> Result<Vec<Comment>, StoreError> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(tables::COMMENTS)?;
        let rid = review.to_string();
        t.range((rid.as_str(), "")..(rid.as_str(), "\u{10FFFF}"))?
            .map(|e| Ok(serde_json::from_slice(e?.1.value())?))
            .collect()
    }

    pub fn comment(
        &self,
        review: ReviewId,
        id: nits_protocol::CommentId,
    ) -> Result<Option<Comment>, StoreError> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(tables::COMMENTS)?;
        t.get((review.to_string().as_str(), id.to_string().as_str()))?
            .map(|v| Ok(serde_json::from_slice(v.value())?))
            .transpose()
    }

    pub fn threads(&self, review: ReviewId) -> Result<Vec<Thread>, StoreError> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(tables::THREADS)?;
        let rid = review.to_string();
        t.range((rid.as_str(), "")..(rid.as_str(), "\u{10FFFF}"))?
            .map(|e| Ok(serde_json::from_slice(e?.1.value())?))
            .collect()
    }

    pub fn viewed(&self, review: ReviewId) -> Result<Vec<ViewedMark>, StoreError> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(tables::VIEWED)?;
        let rid = review.to_string();
        t.range((rid.as_str(), "", "")..(rid.as_str(), "\u{10FFFF}", ""))?
            .map(|e| Ok(serde_json::from_slice(e?.1.value())?))
            .collect()
    }

    /// Comments anchored (by `File` or `Lines`) to a specific blob, as
    /// `(review_id, comment_id)`. Used by re-anchoring.
    pub fn comments_on_blob(
        &self,
        repo: RepoId,
        blob: BlobOid,
    ) -> Result<Vec<(ReviewId, nits_protocol::CommentId)>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(tables::ANCHORS_BY_BLOB)?;
        let (repo_key, blob_key) = (repo.to_string(), blob.to_string());
        let lo = (repo_key.as_str(), blob_key.as_str(), "");
        let hi = (repo_key.as_str(), blob_key.as_str(), "\u{10FFFF}");
        let mut out = Vec::new();
        for entry in table.range(lo..hi)? {
            let (key, value) = entry?;
            let (_, _, comment_key) = key.value();
            let corrupt = |err: nits_protocol::ParseIdError| StoreError::Corrupt {
                seq: Seq::new(0),
                reason: format!("anchors_by_blob key: {err}"),
            };
            let review: ReviewId = value.value().parse().map_err(corrupt)?;
            let comment = comment_key.parse().map_err(corrupt)?;
            out.push((review, comment));
        }
        Ok(out)
    }

    /// Number of events in the log.
    pub fn len(&self) -> Result<u64, StoreError> {
        let txn = self.db.begin_read()?;
        Ok(txn.open_table(tables::EVENTS)?.len()?)
    }

    pub fn is_empty(&self) -> Result<bool, StoreError> {
        Ok(self.len()? == 0)
    }

    /// Everything the views hold, for equality checks in tests and for
    /// diagnostics. Deterministically ordered.
    pub fn dump_views(&self) -> Result<ViewDump, StoreError> {
        fn rows<T: serde::de::DeserializeOwned>(
            table: &redb::ReadOnlyTable<impl redb::Key + 'static, &'static [u8]>,
        ) -> Result<Vec<T>, StoreError> {
            table
                .iter()?
                .map(|entry| Ok(serde_json::from_slice(entry?.1.value())?))
                .collect()
        }
        let txn = self.db.begin_read()?;
        let mut dump = ViewDump {
            workspaces: rows(&txn.open_table(tables::WORKSPACES)?)?,
            reviews: rows(&txn.open_table(tables::REVIEWS)?)?,
            comments: rows(&txn.open_table(tables::COMMENTS)?)?,
            threads: rows(&txn.open_table(tables::THREADS)?)?,
            viewed: rows(&txn.open_table(tables::VIEWED)?)?,
            requests: rows(&txn.open_table(tables::REVIEW_REQUESTS)?)?,
            checkpoints: rows(&txn.open_table(tables::CHECKPOINTS)?)?,
            anchors: Vec::new(),
        };
        for entry in txn.open_table(tables::ANCHORS_BY_BLOB)?.iter()? {
            let (key, value) = entry?;
            let (repo, blob, comment) = key.value();
            dump.anchors.push((
                repo.to_owned(),
                blob.to_owned(),
                comment.to_owned(),
                value.value().to_owned(),
            ));
        }
        Ok(dump)
    }
}

/// Snapshot of all view tables. See [`Store::dump_views`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ViewDump {
    pub workspaces: Vec<Workspace>,
    pub reviews: Vec<ReviewRecord>,
    pub comments: Vec<Comment>,
    pub threads: Vec<Thread>,
    pub viewed: Vec<ViewedMark>,
    pub requests: Vec<nits_protocol::ReviewRequest>,
    pub checkpoints: Vec<nits_protocol::ReviewCheckpoint>,
    /// `(repo_id, blob_oid, comment_id, review_id)`
    pub anchors: Vec<(String, String, String, String)>,
}
