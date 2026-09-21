//! Content cache, memory tier (plan 3.2, `docs/ARCHITECTURE.md` §5.1).
//!
//! Every entry is content-addressed — trees by repository and root oid,
//! render headers and chunks by `(repo, path, target, opts[, chunk])` where `target` names the
//! blob oids — so nothing here is ever stale; entries only leave by LRU
//! eviction under the byte budget. Values are headers and chunks, never
//! whole files.
//!
//! The disk tier is not here: it is the host's KV store, driven by
//! `Effect::Persist` / `Effect::Load` from `ClientCore` (see `content.rs`).
//! [`ContentCache::insert`] reports what it evicted so the core can write
//! those entries through.

use std::collections::{BTreeMap, HashMap, HashSet};

use nits_protocol::{
    ChunkIndex, FileRenderHeader, RenderChunk, RenderOpts, RenderTarget, RepoId, RepoPath, TreeOid,
    TreeSnapshot,
};
use serde::{Deserialize, Serialize};

/// A byte count for cache budgets and entry sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Bytes(pub u64);

impl Bytes {
    pub const ZERO: Bytes = Bytes(0);

    #[must_use]
    pub const fn mib(n: u64) -> Self {
        Bytes(n * 1024 * 1024)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A rendered file within the render cache: which file, of what, with which
/// options. Shared by the header key and every chunk key of that render.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderKey {
    pub repo_id: RepoId,
    pub path: RepoPath,
    pub target: RenderTarget,
    pub opts: RenderOpts,
}

impl RenderKey {
    #[must_use]
    pub fn of_header(header: &FileRenderHeader) -> Self {
        Self {
            repo_id: header.repo_id,
            path: header.path.clone(),
            target: header.target.clone(),
            opts: header.opts.clone(),
        }
    }
}

/// A repository's immutable tree. Git root OIDs identify contents, but a
/// snapshot also carries the repository used by navigation and mutations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeKey {
    pub repo_id: RepoId,
    pub root: TreeOid,
}

impl TreeKey {
    #[must_use]
    pub fn of_snapshot(snapshot: &TreeSnapshot) -> Self {
        Self {
            repo_id: snapshot.repo_id,
            root: snapshot.root_oid,
        }
    }
}

/// What a cache entry is. The serialised form (`serde_json`) is the host KV
/// key, so the wire spelling here is what lands on disk. Legacy root-only
/// tree keys are intentionally not requested or decoded: their snapshots
/// may have been overwritten by a different repository with that root.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum CacheKey {
    Tree {
        tree: TreeKey,
    },
    Header {
        render: RenderKey,
    },
    Chunk {
        render: RenderKey,
        index: ChunkIndex,
    },
}

impl CacheKey {
    /// The host KV key for this entry.
    #[must_use]
    pub fn storage_key(&self) -> String {
        // A key is a closed set of plain fields; serialising it cannot fail.
        match self {
            Self::Tree { .. } => serde_json::to_string(self).unwrap_or_default(),
            Self::Header { .. } | Self::Chunk { .. } => {
                serde_json::to_string(&(nits_protocol::RENDER_CACHE_GENERATION, self))
                    .unwrap_or_default()
            }
        }
    }

    /// The render this key belongs to, for header and chunk keys.
    #[must_use]
    pub fn render(&self) -> Option<&RenderKey> {
        match self {
            CacheKey::Tree { .. } => None,
            CacheKey::Header { render } | CacheKey::Chunk { render, .. } => Some(render),
        }
    }
}

/// A cache entry's value. Matches [`CacheKey`] variant-for-variant; the
/// pairing is checked when a stored value is loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum CacheValue {
    Tree { snapshot: TreeSnapshot },
    Header { header: FileRenderHeader },
    Chunk { chunk: RenderChunk },
}

impl CacheValue {
    /// Bytes on the wire / on disk, which is what the budget counts.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// Whether this value is the right shape for `key`.
    #[must_use]
    pub fn matches(&self, key: &CacheKey) -> bool {
        match (self, key) {
            (CacheValue::Tree { snapshot }, CacheKey::Tree { tree }) => {
                TreeKey::of_snapshot(snapshot) == *tree
            }
            (CacheValue::Header { header }, CacheKey::Header { render }) => {
                RenderKey::of_header(header) == *render
            }
            (CacheValue::Chunk { chunk }, CacheKey::Chunk { index, .. }) => chunk.index == *index,
            (CacheValue::Tree { .. }, CacheKey::Header { .. } | CacheKey::Chunk { .. })
            | (CacheValue::Header { .. }, CacheKey::Tree { .. } | CacheKey::Chunk { .. })
            | (CacheValue::Chunk { .. }, CacheKey::Tree { .. } | CacheKey::Header { .. }) => false,
        }
    }
}

/// An entry the memory tier dropped under budget pressure, handed back so
/// the caller can write it through to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evicted {
    pub key: CacheKey,
    pub value: CacheValue,
    pub size: Bytes,
}

#[derive(Debug)]
struct Entry {
    value: CacheValue,
    size: Bytes,
    /// Position in `order`; larger is more recently used.
    tick: u64,
}

/// LRU memory tier with a byte budget and a pin set. Pinned entries never
/// leave, even if that keeps the cache over budget; eviction only ever
/// considers unpinned entries.
#[derive(Debug)]
pub struct ContentCache {
    budget: Bytes,
    used: Bytes,
    entries: HashMap<CacheKey, Entry>,
    /// LRU order: tick → key. `BTreeMap` so the oldest is `first_key_value`.
    order: BTreeMap<u64, CacheKey>,
    next_tick: u64,
    pinned: HashSet<CacheKey>,
}

impl ContentCache {
    #[must_use]
    pub fn new(budget: Bytes) -> Self {
        Self {
            budget,
            used: Bytes::ZERO,
            entries: HashMap::new(),
            order: BTreeMap::new(),
            next_tick: 0,
            pinned: HashSet::new(),
        }
    }

    #[must_use]
    pub fn budget(&self) -> Bytes {
        self.budget
    }

    #[must_use]
    pub fn used(&self) -> Bytes {
        self.used
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn contains(&self, key: &CacheKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Look up and mark as most recently used.
    pub fn get(&mut self, key: &CacheKey) -> Option<&CacheValue> {
        let tick = self.bump();
        let entry = self.entries.get_mut(key)?;
        self.order.remove(&entry.tick);
        entry.tick = tick;
        self.order.insert(tick, key.clone());
        Some(&entry.value)
    }

    /// Look up without touching recency (for renders that must not perturb
    /// eviction order, and for tests).
    #[must_use]
    pub fn peek(&self, key: &CacheKey) -> Option<&CacheValue> {
        self.entries.get(key).map(|e| &e.value)
    }

    pub fn tree(&mut self, tree: TreeKey) -> Option<&TreeSnapshot> {
        match self.get(&CacheKey::Tree { tree })? {
            CacheValue::Tree { snapshot } => Some(snapshot),
            CacheValue::Header { .. } | CacheValue::Chunk { .. } => None,
        }
    }

    pub fn header(&mut self, render: &RenderKey) -> Option<&FileRenderHeader> {
        match self.get(&CacheKey::Header {
            render: render.clone(),
        })? {
            CacheValue::Header { header } => Some(header),
            CacheValue::Tree { .. } | CacheValue::Chunk { .. } => None,
        }
    }

    pub fn chunk(&mut self, render: &RenderKey, index: ChunkIndex) -> Option<&RenderChunk> {
        match self.get(&CacheKey::Chunk {
            render: render.clone(),
            index,
        })? {
            CacheValue::Chunk { chunk } => Some(chunk),
            CacheValue::Tree { .. } | CacheValue::Header { .. } => None,
        }
    }

    /// Insert (or replace) and evict the least recently used unpinned
    /// entries until the budget holds. Returns what was evicted, oldest
    /// first. `size` is the value's encoded length.
    pub fn insert(&mut self, key: CacheKey, value: CacheValue, size: Bytes) -> Vec<Evicted> {
        if let Some(old) = self.entries.remove(&key) {
            self.order.remove(&old.tick);
            self.used.0 -= old.size.0;
        }
        let tick = self.bump();
        self.used.0 += size.0;
        self.order.insert(tick, key.clone());
        self.entries.insert(key, Entry { value, size, tick });
        self.evict()
    }

    /// Drop an entry without writing it anywhere.
    pub fn remove(&mut self, key: &CacheKey) -> Option<CacheValue> {
        let entry = self.entries.remove(key)?;
        self.order.remove(&entry.tick);
        self.used.0 -= entry.size.0;
        self.pinned.remove(key);
        Some(entry.value)
    }

    /// Keep `key` resident regardless of pressure. Pinning a missing key is
    /// allowed: the pin applies once the entry arrives.
    pub fn pin(&mut self, key: CacheKey) {
        self.pinned.insert(key);
    }

    pub fn unpin(&mut self, key: &CacheKey) {
        self.pinned.remove(key);
    }

    /// Release every pin `keep` rejects, then evict down to budget.
    pub fn retain_pins(&mut self, keep: impl Fn(&CacheKey) -> bool) -> Vec<Evicted> {
        self.pinned.retain(|k| keep(k));
        self.evict()
    }

    pub fn clear_pins(&mut self) -> Vec<Evicted> {
        self.pinned.clear();
        self.evict()
    }

    #[must_use]
    pub fn is_pinned(&self, key: &CacheKey) -> bool {
        self.pinned.contains(key)
    }

    pub fn keys(&self) -> impl Iterator<Item = &CacheKey> {
        self.entries.keys()
    }

    fn bump(&mut self) -> u64 {
        let t = self.next_tick;
        self.next_tick += 1;
        t
    }

    fn evict(&mut self) -> Vec<Evicted> {
        let mut out = Vec::new();
        let mut cursor = 0u64;
        while self.used > self.budget {
            let Some((&tick, key)) = self.order.range(cursor..).next() else {
                break;
            };
            cursor = tick + 1;
            if self.pinned.contains(key) {
                continue;
            }
            let key = key.clone();
            self.order.remove(&tick);
            let Some(entry) = self.entries.remove(&key) else {
                continue;
            };
            self.used.0 -= entry.size.0;
            out.push(Evicted {
                key,
                value: entry.value,
                size: entry.size,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nits_protocol::Oid;

    fn tree_key(fill: u8) -> CacheKey {
        CacheKey::Tree {
            tree: TreeKey {
                repo_id: RepoId::from_parts(1, 1),
                root: TreeOid::from_bytes([fill; 20]),
            },
        }
    }

    fn tree_value(fill: u8) -> CacheValue {
        CacheValue::Tree {
            snapshot: TreeSnapshot {
                repo_id: RepoId::from_parts(1, 1),
                root_oid: TreeOid::new(Oid::from_bytes([fill; 20])),
                entries: Vec::new(),
            },
        }
    }

    #[test]
    fn hit_and_miss() {
        let mut c = ContentCache::new(Bytes(100));
        assert!(c.get(&tree_key(1)).is_none());
        assert!(c.insert(tree_key(1), tree_value(1), Bytes(10)).is_empty());
        assert_eq!(c.get(&tree_key(1)), Some(&tree_value(1)));
        assert_eq!(c.used(), Bytes(10));
        assert!(
            c.tree(TreeKey {
                repo_id: RepoId::from_parts(1, 1),
                root: TreeOid::from_bytes([1; 20])
            })
            .is_some()
        );
        assert!(
            c.tree(TreeKey {
                repo_id: RepoId::from_parts(1, 1),
                root: TreeOid::from_bytes([2; 20])
            })
            .is_none()
        );
    }

    #[test]
    fn eviction_is_lru_and_respects_budget() {
        let mut c = ContentCache::new(Bytes(30));
        c.insert(tree_key(1), tree_value(1), Bytes(10));
        c.insert(tree_key(2), tree_value(2), Bytes(10));
        c.insert(tree_key(3), tree_value(3), Bytes(10));
        // Touch 1 so 2 becomes the oldest.
        c.get(&tree_key(1));
        let evicted = c.insert(tree_key(4), tree_value(4), Bytes(10));
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].key, tree_key(2));
        assert_eq!(evicted[0].size, Bytes(10));
        assert!(c.used() <= c.budget());
        assert!(c.contains(&tree_key(1)));
        assert!(!c.contains(&tree_key(2)));
        // A large insert evicts several, oldest first.
        let evicted = c.insert(tree_key(5), tree_value(5), Bytes(25));
        let keys: Vec<_> = evicted.into_iter().map(|e| e.key).collect();
        assert_eq!(keys, vec![tree_key(3), tree_key(1), tree_key(4)]);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn pinned_entries_survive_pressure() {
        let mut c = ContentCache::new(Bytes(20));
        c.insert(tree_key(1), tree_value(1), Bytes(10));
        c.pin(tree_key(1));
        c.insert(tree_key(2), tree_value(2), Bytes(10));
        let evicted = c.insert(tree_key(3), tree_value(3), Bytes(10));
        assert_eq!(evicted[0].key, tree_key(2));
        assert!(c.contains(&tree_key(1)));
        // Pins can hold the cache over budget; nothing unpinned remains.
        c.pin(tree_key(3));
        let evicted = c.insert(tree_key(4), tree_value(4), Bytes(15));
        assert_eq!(evicted[0].key, tree_key(4));
        assert_eq!(c.used(), Bytes(20));
        assert_eq!(c.len(), 2);
        // Releasing a pin makes the entry evictable again, oldest first.
        assert!(c.retain_pins(|k| *k == tree_key(3)).is_empty());
        let evicted = c.insert(tree_key(5), tree_value(5), Bytes(5));
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].key, tree_key(1));
        assert!(c.used() <= c.budget());
        assert!(c.clear_pins().is_empty());
    }

    #[test]
    fn replace_accounts_for_old_size() {
        let mut c = ContentCache::new(Bytes(100));
        c.insert(tree_key(1), tree_value(1), Bytes(10));
        c.insert(tree_key(1), tree_value(1), Bytes(30));
        assert_eq!(c.used(), Bytes(30));
        assert_eq!(c.len(), 1);
        assert!(c.remove(&tree_key(1)).is_some());
        assert_eq!(c.used(), Bytes::ZERO);
    }

    #[test]
    fn value_and_key_round_trip_and_match() {
        let v = tree_value(7);
        let bytes = v.encode();
        assert_eq!(CacheValue::decode(&bytes).unwrap(), v);
        assert!(v.matches(&tree_key(7)));
        assert!(!v.matches(&tree_key(8)));
        let k = tree_key(7);
        let parsed: CacheKey = serde_json::from_str(&k.storage_key()).unwrap();
        assert_eq!(parsed, k);
    }
}
