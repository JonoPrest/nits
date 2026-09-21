//! Content-keyed disk cache for render results.
//!
//! Key: `(RenderTarget, RenderOpts, lang)` — including entry OIDs and modes, so an
//! entry can never be stale; the cache is only ever trimmed. Header and
//! chunks are stored separately so a header can be served before all chunks
//! exist and so a single chunk read does not deserialise the whole file.

use std::path::Path;

use nits_protocol::{ChunkIndex, RenderChunk, RenderContent, RenderOpts, RenderTarget};
use redb::{Database, ReadableDatabase, TableDefinition};

use super::Rendered;

const RENDERS: TableDefinition<'_, &str, &[u8]> = TableDefinition::new("renders");

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("encoding: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Redb(#[from] redb::Error),
}

/// Convert redb operations to the cache error while retaining their source error.
macro_rules! from_redb {
    ($($e:ty),*) => {$(
        impl From<$e> for CacheError {
            fn from(e: $e) -> Self { CacheError::Redb(e.into()) }
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

/// Identifies one render. Serialised (canonically) to form the cache key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize)]
pub struct RenderKey<'a> {
    pub target: &'a RenderTarget,
    pub opts: &'a RenderOpts,
    pub lang: Option<&'a str>,
}

impl RenderKey<'_> {
    fn prefix(&self) -> Result<String, CacheError> {
        // JSON of a struct is deterministic for a fixed field order.
        Ok(serde_json::to_string(&(
            nits_protocol::RENDER_CACHE_GENERATION,
            self,
        ))?)
    }
}

#[derive(Debug)]
pub struct RenderCache {
    db: Database,
}

impl RenderCache {
    pub fn open(path: &Path) -> Result<Self, CacheError> {
        let db = Database::create(path)?;
        let txn = db.begin_write()?;
        txn.open_table(RENDERS)?;
        txn.commit()?;
        Ok(Self { db })
    }

    pub fn put(&self, key: &RenderKey<'_>, rendered: &Rendered) -> Result<(), CacheError> {
        let prefix = key.prefix()?;
        let txn = self.db.begin_write()?;
        {
            let mut t = txn.open_table(RENDERS)?;
            t.insert(
                format!("{prefix}/h").as_str(),
                serde_json::to_vec(&rendered.content)?.as_slice(),
            )?;
            for chunk in rendered.chunks() {
                t.insert(
                    format!("{prefix}/{}", chunk.index.get()).as_str(),
                    serde_json::to_vec(&chunk)?.as_slice(),
                )?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    pub fn header(&self, key: &RenderKey<'_>) -> Result<Option<RenderContent>, CacheError> {
        let prefix = key.prefix()?;
        let txn = self.db.begin_read()?;
        let t = txn.open_table(RENDERS)?;
        t.get(format!("{prefix}/h").as_str())?
            .map(|v| Ok(serde_json::from_slice(v.value())?))
            .transpose()
    }

    pub fn chunk(
        &self,
        key: &RenderKey<'_>,
        index: ChunkIndex,
    ) -> Result<Option<RenderChunk>, CacheError> {
        let prefix = key.prefix()?;
        let txn = self.db.begin_read()?;
        let t = txn.open_table(RENDERS)?;
        t.get(format!("{prefix}/{}", index.get()).as_str())?
            .map(|v| Ok(serde_json::from_slice(v.value())?))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nits_protocol::{BlobOid, ChangeKind};

    #[test]
    fn previous_generation_headers_and_chunks_cannot_hide_terminator_changes() {
        let dir = tempfile::tempdir().unwrap();
        let cache = RenderCache::open(&dir.path().join("render.redb")).unwrap();
        let target = RenderTarget::Diff {
            change: ChangeKind::Modified {
                old: nits_protocol::BlobEntry {
                    oid: BlobOid::from_bytes([1; 20]),
                    mode: nits_protocol::BlobMode::Regular,
                },
                new: nits_protocol::BlobEntry {
                    oid: BlobOid::from_bytes([2; 20]),
                    mode: nits_protocol::BlobMode::Regular,
                },
            },
        };
        let opts = RenderOpts::default();
        let key = RenderKey {
            target: &target,
            opts: &opts,
            lang: None,
        };
        // Previously the same OIDs yielded a perfectly decodable zero-change
        // header. Both that header and its old chunk must be cache misses.
        let old = super::super::render_file(
            &super::super::Highlighter::new(),
            Some(b"same\n"),
            Some(b"same\n"),
            None,
            &opts,
        );
        let legacy = serde_json::to_string(&key).unwrap();
        let tx = cache.db.begin_write().unwrap();
        {
            let mut table = tx.open_table(RENDERS).unwrap();
            table
                .insert(
                    format!("{legacy}/h").as_str(),
                    serde_json::to_vec(&old.content).unwrap().as_slice(),
                )
                .unwrap();
            table
                .insert(
                    format!("{legacy}/0").as_str(),
                    serde_json::to_vec(&old.chunk(ChunkIndex::FIRST).unwrap())
                        .unwrap()
                        .as_slice(),
                )
                .unwrap();
        }
        tx.commit().unwrap();
        assert!(cache.header(&key).unwrap().is_none());
        assert!(cache.chunk(&key, ChunkIndex::FIRST).unwrap().is_none());
        let rendered = super::super::render_file(
            &super::super::Highlighter::new(),
            Some(b"same"),
            Some(b"same\n"),
            None,
            &opts,
        );
        cache.put(&key, &rendered).unwrap();
        drop(cache);
        let reopened = RenderCache::open(&dir.path().join("render.redb")).unwrap();
        assert!(matches!(
            reopened.header(&key).unwrap(),
            Some(RenderContent::Text {
                additions: 1,
                deletions: 1,
                ..
            })
        ));
        assert_eq!(
            reopened.chunk(&key, ChunkIndex::FIRST).unwrap(),
            rendered.chunk(ChunkIndex::FIRST)
        );
    }

    #[test]
    fn mode_identity_separates_headers_chunks_and_legacy_cache_entries() {
        use nits_protocol::{BlobEntry, BlobMode};
        let dir = tempfile::tempdir().unwrap();
        let cache = RenderCache::open(&dir.path().join("render.redb")).unwrap();
        let entry = BlobEntry {
            oid: BlobOid::from_bytes([1; 20]),
            mode: BlobMode::Regular,
        };
        let opts = RenderOpts::default();
        let rendered =
            super::super::render_blob(&super::super::Highlighter::new(), b"source\n", None);
        let blob = RenderTarget::Blob { entry };
        let diff = RenderTarget::Diff {
            change: ChangeKind::Modified {
                old: entry,
                new: entry,
            },
        };
        for target in [blob, diff] {
            let key = RenderKey {
                target: &target,
                opts: &opts,
                lang: None,
            };
            let legacy_target = match &target {
                RenderTarget::Blob { .. } => format!(r#"{{"type":"Blob","oid":"{}"}}"#, entry.oid),
                RenderTarget::Diff { .. } => format!(
                    r#"{{"type":"Diff","change":{{"type":"Modified","old":"{}","new":"{}"}}}}"#,
                    entry.oid, entry.oid
                ),
            };
            let legacy = format!(
                r#"{{"target":{legacy_target},"opts":{},"lang":null}}"#,
                serde_json::to_string(&opts).unwrap()
            );
            let txn = cache.db.begin_write().unwrap();
            {
                let mut table = txn.open_table(RENDERS).unwrap();
                for suffix in ["h", "0"] {
                    table
                        .insert(
                            format!("{legacy}/{suffix}").as_str(),
                            &b"obsolete cached data"[..],
                        )
                        .unwrap();
                }
            }
            txn.commit().unwrap();
            assert!(cache.header(&key).unwrap().is_none());
            assert!(cache.chunk(&key, ChunkIndex::FIRST).unwrap().is_none());
            cache.put(&key, &rendered).unwrap();
            for mode in [BlobMode::Executable, BlobMode::Symlink, BlobMode::Unknown] {
                let other = BlobEntry { mode, ..entry };
                let changed = match &target {
                    RenderTarget::Blob { .. } => RenderTarget::Blob { entry: other },
                    RenderTarget::Diff { .. } => RenderTarget::Diff {
                        change: ChangeKind::Modified {
                            old: entry,
                            new: other,
                        },
                    },
                };
                let changed_key = RenderKey {
                    target: &changed,
                    ..key.clone()
                };
                assert!(cache.header(&changed_key).unwrap().is_none());
                assert!(
                    cache
                        .chunk(&changed_key, ChunkIndex::FIRST)
                        .unwrap()
                        .is_none()
                );
            }
            assert_eq!(cache.header(&key).unwrap(), Some(rendered.content.clone()));
        }
    }

    #[test]
    fn header_and_chunks_roundtrip_and_miss_on_other_opts() {
        let dir = tempfile::tempdir().unwrap();
        let cache = RenderCache::open(&dir.path().join("render.redb")).unwrap();
        let hl = super::super::Highlighter::new();
        let rendered = super::super::render_file(
            &hl,
            Some(b"a\nb\n"),
            Some(b"a\nc\n"),
            None,
            &RenderOpts::default(),
        );
        let target = RenderTarget::Diff {
            change: ChangeKind::Modified {
                old: nits_protocol::BlobEntry {
                    oid: BlobOid::from_bytes([1; 20]),
                    mode: nits_protocol::BlobMode::Regular,
                },
                new: nits_protocol::BlobEntry {
                    oid: BlobOid::from_bytes([2; 20]),
                    mode: nits_protocol::BlobMode::Regular,
                },
            },
        };
        let default_opts = RenderOpts::default();
        let key = RenderKey {
            target: &target,
            opts: &default_opts,
            lang: None,
        };
        assert!(cache.header(&key).unwrap().is_none());
        cache.put(&key, &rendered).unwrap();
        assert_eq!(cache.header(&key).unwrap(), Some(rendered.content.clone()));
        assert_eq!(
            cache.chunk(&key, ChunkIndex::FIRST).unwrap(),
            rendered.chunk(ChunkIndex::FIRST)
        );
        assert!(cache.chunk(&key, ChunkIndex::new(9)).unwrap().is_none());
        let ws_opts = RenderOpts {
            ignore_whitespace: true,
            context_lines: 3,
            ..RenderOpts::default()
        };
        let other = RenderKey {
            opts: &ws_opts,
            ..key
        };
        assert!(cache.header(&other).unwrap().is_none());
    }
}
