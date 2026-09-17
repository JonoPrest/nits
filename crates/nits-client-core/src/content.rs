//! Content flow (plan 3.2): how the core gets trees, render headers and
//! chunks into [`ContentCache`] and onto the screen.
//!
//! Every want goes through one path, [`ClientCore::want`]:
//!
//! 1. memory hit → done, no effects;
//! 2. memory miss with the disk tier on → `Effect::Load`; the host answers
//!    with `Input::Stored`; a hit is inserted, a miss falls through;
//! 3. otherwise → queued, then sent to the daemon, at most
//!    `CacheConfig::max_in_flight` content requests at a time.
//!
//! Each key has at most one outstanding fetch, so concurrent wants for the
//! same key collapse. When the disk tier is on, everything the daemon sends
//! is written through as it arrives (not only on memory eviction: the open
//! review's entries are pinned and would otherwise never reach disk, and a
//! restart must be able to serve the last review from disk alone). The disk
//! tier keeps its own LRU budget, session-local: entries written by earlier
//! sessions are counted once they are loaded again.

use std::collections::{BTreeMap, HashMap, VecDeque};

use nits_protocol::{
    ChunkIndex, ClientMsg, FileChange, FileRenderHeader, RefSpec, RenderContent, RenderOpts,
    RenderTarget, RepoId, RepoPath, Request, ResolvedRef, ResolvedSource, ResolvedTarget, ReviewId,
    TreeDelta, TreeEntry, TreeOid, TreeSnapshot, ViewSection,
};
use serde::{Deserialize, Serialize};
use strum::EnumDiscriminants;

use crate::cache::{Bytes, CacheKey, CacheValue, ContentCache, Evicted, RenderKey};
use crate::view::OpenFile;
use crate::{ClientCore, CoreError, Effect, InFlight, Key};

/// How many chunks either side of the viewport chunk are prefetched.
pub const PREFETCH_RADIUS: u32 = 2;

/// Whether the host has a disk tier for this connection (§5.1: only for
/// remote daemons; a local daemon's own cache is a socket hop away).
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumDiscriminants)]
#[strum_discriminants(name(DiskTierKind), derive(Hash))]
pub enum DiskTier {
    Disabled,
    Enabled { budget: Bytes },
}

/// Cache sizing and fetch policy, fixed for the life of a `ClientCore`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheConfig {
    pub memory_budget: Bytes,
    pub disk: DiskTier,
    /// Content requests (tree, render, chunk) outstanding to the daemon at
    /// once; further wants queue.
    pub max_in_flight: usize,
    /// Options every render this client asks for uses.
    pub render_opts: RenderOpts,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            memory_budget: Bytes::mib(256),
            disk: DiskTier::Disabled,
            max_in_flight: 4,
            render_opts: RenderOpts::default(),
        }
    }
}

/// A file within the open review, as the host names it in `Action::Viewport`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRef {
    pub repo_id: RepoId,
    pub path: RepoPath,
}

/// Session-local index of the disk tier: what this core has written or
/// loaded, with an LRU order so the disk budget holds.
#[derive(Debug)]
pub(crate) struct DiskIndex {
    budget: Bytes,
    used: Bytes,
    sizes: HashMap<CacheKey, (Bytes, u64)>,
    order: BTreeMap<u64, CacheKey>,
    next_tick: u64,
}

impl DiskIndex {
    fn new(budget: Bytes) -> Self {
        Self {
            budget,
            used: Bytes::ZERO,
            sizes: HashMap::new(),
            order: BTreeMap::new(),
            next_tick: 0,
        }
    }

    /// Record that `key` is on disk at `size`, most recently used; returns
    /// the keys that must be removed to stay in budget.
    fn touch(&mut self, key: &CacheKey, size: Bytes) -> Vec<CacheKey> {
        if let Some((old, tick)) = self.sizes.remove(key) {
            self.order.remove(&tick);
            self.used.0 -= old.0;
        }
        let tick = self.next_tick;
        self.next_tick += 1;
        self.used.0 += size.0;
        self.order.insert(tick, key.clone());
        self.sizes.insert(key.clone(), (size, tick));
        let mut out = Vec::new();
        while self.used > self.budget {
            let Some((&t, k)) = self.order.iter().next() else {
                break;
            };
            if k == key {
                break;
            }
            let k = k.clone();
            self.order.remove(&t);
            if let Some((s, _)) = self.sizes.remove(&k) {
                self.used.0 -= s.0;
            }
            out.push(k);
        }
        out
    }

    fn forget(&mut self, key: &CacheKey) {
        if let Some((size, tick)) = self.sizes.remove(key) {
            self.order.remove(&tick);
            self.used.0 -= size.0;
        }
    }
}

/// A content request the core knows how to issue. The cache key it fills
/// is [`Fetch::key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fetch {
    Tree {
        repo_id: RepoId,
        ref_spec: RefSpec,
        root: TreeOid,
    },
    /// `FileRender` stream: header then chunks from `first_chunk`; the core
    /// cancels once a chunk past `stop_after` arrives.
    Render {
        review_id: ReviewId,
        render: RenderKey,
        first_chunk: ChunkIndex,
        stop_after: ChunkIndex,
    },
    Chunk {
        render: RenderKey,
        index: ChunkIndex,
    },
}

impl Fetch {
    fn key(&self) -> CacheKey {
        match self {
            Fetch::Tree { root, .. } => CacheKey::Tree { root: *root },
            Fetch::Render { render, .. } => CacheKey::Header {
                render: render.clone(),
            },
            Fetch::Chunk { render, index } => CacheKey::Chunk {
                render: render.clone(),
                index: *index,
            },
        }
    }
}

/// Where a wanted key currently is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    /// `Effect::Load` issued; waiting for `Input::Stored`.
    Loading(Fetch),
    /// Waiting for an in-flight slot.
    Queued,
    /// Sent to the daemon.
    Requested,
}

/// Content-side state of the core.
#[derive(Debug)]
pub(crate) struct Content {
    pub(crate) config: CacheConfig,
    pub(crate) cache: ContentCache,
    disk: Option<DiskIndex>,
    pending: HashMap<CacheKey, Pending>,
    /// Storage key → cache key for outstanding `Load`s.
    loads: HashMap<Key, CacheKey>,
    queue: VecDeque<Fetch>,
    in_flight: usize,
}

impl Content {
    pub(crate) fn new(config: CacheConfig) -> Self {
        let disk = match config.disk {
            DiskTier::Disabled => None,
            DiskTier::Enabled { budget } => Some(DiskIndex::new(budget)),
        };
        let memory_budget = config.memory_budget;
        Self {
            config,
            cache: ContentCache::new(memory_budget),
            disk,
            pending: HashMap::new(),
            loads: HashMap::new(),
            queue: VecDeque::new(),
            in_flight: 0,
        }
    }

    /// Write an entry to disk (when the tier is on) and trim the disk tier
    /// to its budget.
    fn persist(&mut self, key: &CacheKey, value: &CacheValue, effects: &mut Vec<Effect>) {
        let Some(disk) = &mut self.disk else {
            return;
        };
        let bytes = value.encode();
        let size = Bytes(bytes.len() as u64);
        for gone in disk.touch(key, size) {
            effects.push(Effect::Remove {
                key: gone.storage_key(),
            });
        }
        effects.push(Effect::Persist {
            key: key.storage_key(),
            value: bytes,
        });
    }

    /// Memory evictions are already on disk (written on arrival), so they
    /// only need re-touching in the disk LRU. Entries the disk index does
    /// not know (loaded before this session) are written again.
    fn write_through(&mut self, evicted: Vec<Evicted>, effects: &mut Vec<Effect>) {
        if self.disk.is_none() {
            return;
        }
        for e in evicted {
            let known = self
                .disk
                .as_ref()
                .is_some_and(|d| d.sizes.contains_key(&e.key));
            if known {
                let Some(disk) = &mut self.disk else {
                    return;
                };
                for gone in disk.touch(&e.key, e.size) {
                    effects.push(Effect::Remove {
                        key: gone.storage_key(),
                    });
                }
            } else {
                self.persist(&e.key, &e.value, effects);
            }
        }
    }
}

/// Whether the review's trees are coming on a stream or must be fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeSource {
    Fetch,
    Stream,
}

/// What to do once a header is known: fetch the chunks a viewport needs, or
/// just the first for prefetch. Streams bring their own chunks, so nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterHeader {
    WantChunks,
    StreamFollows,
}

/// How a value reached the cache; decides pinning, rendering and follow-ups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arrival {
    Stream,
    Disk,
    Response,
}

impl ClientCore {
    #[must_use]
    pub fn cache(&self) -> &ContentCache {
        &self.content.cache
    }

    #[must_use]
    pub fn cache_config(&self) -> &CacheConfig {
        &self.content.config
    }

    /// Content requests currently sent to the daemon and not yet answered.
    #[must_use]
    pub fn content_in_flight(&self) -> usize {
        self.content.in_flight
    }

    /// Content requests waiting for a slot.
    #[must_use]
    pub fn content_queued(&self) -> usize {
        self.content.queue.len()
    }

    // ---- wanting --------------------------------------------------------

    /// Ensure the key `fetch` fills is or will be cached. Returns whether it
    /// was already resident.
    pub(crate) fn want(&mut self, fetch: Fetch, effects: &mut Vec<Effect>) -> bool {
        let key = fetch.key();
        if self.content.cache.contains(&key) {
            return true;
        }
        if self.content.pending.contains_key(&key) {
            return false;
        }
        if self.content.disk.is_some() {
            let storage = key.storage_key();
            self.content.loads.insert(storage.clone(), key.clone());
            self.content.pending.insert(key, Pending::Loading(fetch));
            effects.push(Effect::Load { key: storage });
        } else {
            self.enqueue(fetch, effects);
        }
        false
    }

    fn enqueue(&mut self, fetch: Fetch, effects: &mut Vec<Effect>) {
        self.content.pending.insert(fetch.key(), Pending::Queued);
        self.content.queue.push_back(fetch);
        self.drain(effects);
    }

    /// Send queued fetches while slots are free.
    fn drain(&mut self, effects: &mut Vec<Effect>) {
        while self.content.in_flight < self.content.config.max_in_flight {
            let Some(fetch) = self.content.queue.pop_front() else {
                break;
            };
            let key = fetch.key();
            self.content.pending.insert(key.clone(), Pending::Requested);
            self.content.in_flight += 1;
            let (request, waiting) = match fetch {
                Fetch::Tree {
                    repo_id,
                    ref_spec,
                    root,
                } => (
                    Request::TreeSnapshot { repo_id, ref_spec },
                    InFlight::TreeSnapshot { root },
                ),
                Fetch::Render {
                    review_id,
                    render,
                    first_chunk,
                    stop_after,
                } => {
                    let open = self
                        .view
                        .review
                        .as_ref()
                        .filter(|r| r.snapshot.review.id == review_id);
                    // A comment's recorded original diff is not in the
                    // review's file list; the daemon renders the change
                    // directly.
                    let original = open.is_some_and(|r| r.original.as_ref() == Some(&render));
                    let request = match (&render.target, original) {
                        (RenderTarget::Diff { change }, true) => Request::ChangeRender {
                            repo_id: render.repo_id,
                            path: render.path.clone(),
                            change: change.clone(),
                            opts: render.opts.clone(),
                            first_chunk,
                        },
                        (RenderTarget::Blob { oid }, _) => Request::BlobRender {
                            repo_id: render.repo_id,
                            path: render.path.clone(),
                            blob_oid: *oid,
                            first_chunk,
                        },
                        (RenderTarget::Diff { .. }, false) => Request::FileRender {
                            review_id,
                            repo_id: render.repo_id,
                            path: render.path.clone(),
                            opts: render.opts.clone(),
                            first_chunk,
                            scope: open.map(|r| r.scope).unwrap_or_default(),
                        },
                    };
                    (request, InFlight::FileRender { render, stop_after })
                }
                Fetch::Chunk { render, index } => (
                    Request::RenderChunk {
                        repo_id: render.repo_id,
                        path: render.path.clone(),
                        target: render.target.clone(),
                        opts: render.opts,
                        index,
                    },
                    InFlight::RenderChunk { key },
                ),
            };
            effects.push(self.request(request, waiting));
        }
    }

    /// A content request finished (response, stream end or error).
    pub(crate) fn content_done(&mut self, effects: &mut Vec<Effect>) {
        self.content.in_flight = self.content.in_flight.saturating_sub(1);
        self.drain(effects);
    }

    /// Drop the pending mark for a key whose fetch failed.
    pub(crate) fn content_failed(&mut self, key: &CacheKey) {
        self.content.pending.remove(key);
    }

    /// The connection dropped: nothing is in flight any more. Queued fetches
    /// stay queued and go out after the next subscribe.
    pub(crate) fn content_reset_in_flight(&mut self) {
        self.content.in_flight = 0;
    }

    /// Queued chunk fetches outside `keep` are dropped before being sent.
    fn prune_queue(&mut self, keep: impl Fn(&Fetch) -> bool) {
        let mut kept = VecDeque::new();
        for f in std::mem::take(&mut self.content.queue) {
            if keep(&f) {
                kept.push_back(f);
            } else {
                self.content.pending.remove(&f.key());
            }
        }
        self.content.queue = kept;
    }

    // ---- stored answers -------------------------------------------------

    pub(crate) fn stored(
        &mut self,
        key: Key,
        value: Option<Vec<u8>>,
    ) -> Result<Vec<Effect>, CoreError> {
        let Some(cache_key) = self.content.loads.remove(&key) else {
            return Err(CoreError::UnknownKey(key));
        };
        let Some(Pending::Loading(fetch)) = self.content.pending.remove(&cache_key) else {
            return Ok(Vec::new());
        };
        let mut effects = Vec::new();
        let decoded = value
            .as_deref()
            .and_then(|b| CacheValue::decode(b).ok())
            .filter(|v| v.matches(&cache_key));
        match (decoded, value) {
            (Some(v), Some(bytes)) => {
                if let Some(disk) = &mut self.content.disk {
                    for gone in disk.touch(&cache_key, Bytes(bytes.len() as u64)) {
                        effects.push(Effect::Remove {
                            key: gone.storage_key(),
                        });
                    }
                }
                self.arrived(cache_key, v, Arrival::Disk, &mut effects);
            }
            (None, Some(_)) => {
                // Unreadable or the wrong shape: drop it and go to the daemon.
                if let Some(disk) = &mut self.content.disk {
                    disk.forget(&cache_key);
                }
                effects.push(Effect::Remove { key });
                self.enqueue(fetch, &mut effects);
            }
            (_, None) => self.enqueue(fetch, &mut effects),
        }
        Ok(effects)
    }

    // ---- arrivals -------------------------------------------------------

    /// A value reached the core. Inserts, pins if the open review or file
    /// needs it, writes evictions through, renders what changed and issues
    /// follow-up wants.
    pub(crate) fn arrived(
        &mut self,
        key: CacheKey,
        value: CacheValue,
        via: Arrival,
        effects: &mut Vec<Effect>,
    ) {
        self.content.pending.remove(&key);
        let size = Bytes(value.encode().len() as u64);
        if self.needs_pin(&key) {
            self.content.cache.pin(key.clone());
        }
        match via {
            Arrival::Stream | Arrival::Response => self.content.persist(&key, &value, effects),
            Arrival::Disk => {}
        }
        let evicted = self.content.cache.insert(key.clone(), value.clone(), size);
        self.content.write_through(evicted, effects);
        let after = match via {
            Arrival::Stream => AfterHeader::StreamFollows,
            Arrival::Disk | Arrival::Response => AfterHeader::WantChunks,
        };
        let mut sections = Vec::new();
        match (key, value) {
            (CacheKey::Header { render }, CacheValue::Header { header }) => {
                if let Some(open) = &mut self.view.review
                    && !open.files.contains(&render)
                    && open.original.as_ref() != Some(&render)
                    && open.open_file.as_ref().map(|f| &f.render) != Some(&render)
                {
                    open.files.push(render.clone());
                }
                if after == AfterHeader::WantChunks {
                    self.want_chunks_for(&render, &header, effects);
                }
            }
            (CacheKey::Chunk { render, index }, CacheValue::Chunk { .. }) => {
                if let Some(file) = self.open_file()
                    && file.render == render
                    && self
                        .viewport_window(&render)
                        .is_some_and(|w| w.contains(index))
                {
                    sections.push(ViewSection::Diff);
                }
            }
            // Trees: the explorer is derived from the cache after every input.
            // Mismatched shapes are checked before insertion.
            (CacheKey::Tree { .. }, CacheValue::Tree { .. })
            | (CacheKey::Tree { .. }, CacheValue::Header { .. } | CacheValue::Chunk { .. })
            | (CacheKey::Header { .. }, CacheValue::Tree { .. } | CacheValue::Chunk { .. })
            | (CacheKey::Chunk { .. }, CacheValue::Tree { .. } | CacheValue::Header { .. }) => {
                // Shapes are checked before insertion; nothing to show.
            }
        }
        if !sections.is_empty() {
            effects.push(crate::render(&sections));
        }
    }

    fn needs_pin(&self, key: &CacheKey) -> bool {
        let Some(open) = &self.view.review else {
            return false;
        };
        match key {
            CacheKey::Tree { root } => open.trees.contains(root),
            // A header only ever arrives for the open review (streamed with
            // it, or requested from its file list), so it is always pinned.
            CacheKey::Header { .. } => true,
            CacheKey::Chunk { render, .. } => {
                open.open_file.as_ref().is_some_and(|f| f.render == *render)
            }
        }
    }

    fn open_file(&self) -> Option<&OpenFile> {
        self.view.review.as_ref()?.open_file.as_ref()
    }

    /// The chunks the viewport on `render` covers, ±[`PREFETCH_RADIUS`],
    /// clipped to the file; `None` if the file is not open or has no rows.
    fn viewport_window(&self, render: &RenderKey) -> Option<ChunkWindow> {
        let file = self.open_file().filter(|f| f.render == *render)?;
        let header = header_of(&self.content.cache, render)?;
        ChunkWindow::around(header, file.first_row, file.last_row)
    }

    /// After a header is known: the open file's viewport window, or the
    /// first chunk of a review file for prefetch.
    fn want_chunks_for(
        &mut self,
        render: &RenderKey,
        header: &FileRenderHeader,
        effects: &mut Vec<Effect>,
    ) {
        let RenderContent::Text { chunk_count, .. } = header.content else {
            return;
        };
        if chunk_count == 0 {
            return;
        }
        let is_open_file = self.open_file().is_some_and(|f| f.render == *render);
        if is_open_file {
            self.want_viewport_chunks(render, effects);
        } else if self
            .view
            .review
            .as_ref()
            .is_some_and(|r| r.files.contains(render))
        {
            self.want(
                Fetch::Chunk {
                    render: render.clone(),
                    index: ChunkIndex::FIRST,
                },
                effects,
            );
        }
    }

    fn want_viewport_chunks(&mut self, render: &RenderKey, effects: &mut Vec<Effect>) {
        let Some(window) = self.viewport_window(render) else {
            return;
        };
        for index in window.by_distance() {
            self.want(
                Fetch::Chunk {
                    render: render.clone(),
                    index,
                },
                effects,
            );
        }
    }

    // ---- review open / close --------------------------------------------

    /// The review snapshot is in the view; want its trees and file list.
    /// In the streamed (local) flow the `OpenReview` stream brings them;
    /// here is the piecewise flow used when a disk tier may already hold them.
    pub(crate) fn review_opened_piecewise(
        &mut self,
        review_id: ReviewId,
        effects: &mut Vec<Effect>,
    ) {
        self.want_review_trees(effects);
        let scope = self
            .view
            .review
            .as_ref()
            .map(|r| r.scope)
            .unwrap_or_default();
        effects.push(self.request(
            Request::ListFiles { review_id, scope },
            InFlight::ListFiles { review_id },
        ));
    }

    /// Resolved targets changed (or were first seen): pin and want the trees.
    pub(crate) fn want_review_trees(&mut self, effects: &mut Vec<Effect>) {
        self.review_trees(TreeSource::Fetch, effects);
    }

    /// The `OpenReview` stream brings the trees itself: pin, do not fetch.
    pub(crate) fn expect_streamed_trees(&mut self, effects: &mut Vec<Effect>) {
        self.review_trees(TreeSource::Stream, effects);
    }

    fn review_trees(&mut self, source: TreeSource, effects: &mut Vec<Effect>) {
        let Some(open) = &mut self.view.review else {
            return;
        };
        let targets: Vec<ResolvedTarget> = open.current_targets();
        let mut fetches = Vec::new();
        let mut trees = Vec::new();
        for t in &targets {
            for r in [&t.base, &t.head] {
                if !trees.contains(&r.tree) {
                    trees.push(r.tree);
                    fetches.push(Fetch::Tree {
                        repo_id: t.repo_id,
                        ref_spec: ref_spec_of(r),
                        root: r.tree,
                    });
                }
            }
        }
        open.trees = trees;
        let evicted = self.retain_review_pins();
        self.content.write_through(evicted, effects);
        for f in fetches {
            self.content.cache.pin(f.key());
            match source {
                TreeSource::Fetch => {
                    self.want(f, effects);
                }
                TreeSource::Stream => {}
            }
        }
    }

    /// `ListFiles` answered: replace the file list and prefetch each header
    /// (and its first chunk).
    pub(crate) fn review_files(
        &mut self,
        review_id: ReviewId,
        files: Vec<FileChange>,
        effects: &mut Vec<Effect>,
    ) {
        let opts = self.content.config.render_opts.clone();
        let renders: Vec<RenderKey> = files
            .into_iter()
            .map(|f| RenderKey {
                repo_id: f.repo_id,
                path: f.path,
                target: RenderTarget::Diff { change: f.kind },
                opts: opts.clone(),
            })
            .collect();
        let Some(open) = &mut self.view.review else {
            return;
        };
        open.files.clone_from(&renders);
        if let Some(f) = &open.open_file
            && !renders.contains(&f.render)
            && open.original.as_ref() != Some(&f.render)
        {
            open.open_file = None;
        }
        let evicted = self.retain_review_pins();
        self.content.write_through(evicted, effects);
        for render in renders {
            let header_key = CacheKey::Header {
                render: render.clone(),
            };
            self.content.cache.pin(header_key.clone());
            let fetch = Fetch::Render {
                review_id,
                render: render.clone(),
                first_chunk: ChunkIndex::FIRST,
                stop_after: ChunkIndex::FIRST,
            };
            if self.want(fetch, effects)
                && let Some(header) = header_of(&self.content.cache, &render).cloned()
            {
                self.want_chunks_for(&render, &header, effects);
            }
        }
        self.auto_open_first(effects);
    }

    /// Diffing mode lands on the first diff right away instead of an empty
    /// "open a file" pane (UI-DESIGN §Layout). No-op when a file is open.
    pub(crate) fn auto_open_first(&mut self, effects: &mut Vec<Effect>) {
        if self
            .view
            .review
            .as_ref()
            .is_some_and(|o| o.open_file.is_none())
            && let Some(first) = self
                .view
                .review
                .as_ref()
                .and_then(|o| o.files.first())
                .cloned()
        {
            let file = FileRef {
                repo_id: first.repo_id,
                path: first.path.clone(),
            };
            if let Ok(more) = self.viewport(file, 0, crate::focus::PAGE_ROWS - 1) {
                effects.extend(more);
            }
        }
    }

    /// Keep only pins the open review (and its open file) still needs.
    fn retain_review_pins(&mut self) -> Vec<Evicted> {
        let (trees, files, open_file, original) = match &self.view.review {
            Some(r) => (
                r.trees.clone(),
                r.files.clone(),
                r.open_file.as_ref().map(|f| f.render.clone()),
                r.original.clone(),
            ),
            None => (Vec::new(), Vec::new(), None, None),
        };
        self.content.cache.retain_pins(|k| match k {
            CacheKey::Tree { root } => trees.contains(root),
            CacheKey::Header { render } => {
                files.contains(render)
                    || original.as_ref() == Some(render)
                    || open_file.as_ref() == Some(render)
            }
            CacheKey::Chunk { render, .. } => open_file.as_ref() == Some(render),
        })
    }

    /// Review closed (or replaced): release pins, drop queued content.
    pub(crate) fn review_closed(&mut self, effects: &mut Vec<Effect>) {
        self.prune_queue(|_| false);
        let evicted = self.content.cache.clear_pins();
        self.content.write_through(evicted, effects);
    }

    /// Pin and fetch a render that just became the open file (a comment's
    /// original diff, or a context expansion's re-key).
    pub(crate) fn want_open_render(
        &mut self,
        review_id: ReviewId,
        render: &RenderKey,
        effects: &mut Vec<Effect>,
    ) {
        let evicted = self.retain_review_pins();
        self.content.write_through(evicted, effects);
        self.content.cache.pin(CacheKey::Header {
            render: render.clone(),
        });
        if header_of(&self.content.cache, render).is_some() {
            self.want_viewport_chunks(render, effects);
        } else {
            self.want(
                Fetch::Render {
                    review_id,
                    render: render.clone(),
                    first_chunk: ChunkIndex::FIRST,
                    stop_after: ChunkIndex::new(PREFETCH_RADIUS),
                },
                effects,
            );
        }
    }

    /// A blob render key for a file outside the review's diff: from the
    /// Browse ref's tree when one is picked for the repo, else the repo's
    /// scoped head tree (UI-DESIGN §Browse).
    fn blob_render_of(&self, file: &FileRef) -> Option<RenderKey> {
        let open = self.view.review.as_ref()?;
        let root = match (self.view.tab, self.browse_root(file.repo_id)) {
            (crate::Tab::Browse, Some(root)) => root,
            (crate::Tab::Browse | crate::Tab::FilesChanged | crate::Tab::Conversation, _) => {
                open.current_targets()
                    .iter()
                    .find(|t| t.repo_id == file.repo_id)?
                    .head
                    .tree
            }
        };
        let CacheValue::Tree { snapshot } = self.content.cache.peek(&CacheKey::Tree { root })?
        else {
            return None;
        };
        let oid = snapshot
            .entries
            .iter()
            .find(|e| e.path == file.path)
            .and_then(|e| match e.kind {
                nits_protocol::TreeEntryKind::File { oid, .. }
                | nits_protocol::TreeEntryKind::Symlink { oid } => Some(oid),
                nits_protocol::TreeEntryKind::Dir { .. }
                | nits_protocol::TreeEntryKind::Submodule { .. } => None,
            })?;
        Some(RenderKey {
            repo_id: file.repo_id,
            path: file.path.clone(),
            target: RenderTarget::Blob { oid },
            opts: RenderOpts::default(),
        })
    }

    // ---- viewport -------------------------------------------------------

    pub(crate) fn viewport(
        &mut self,
        file: FileRef,
        first_row: u32,
        last_row: u32,
    ) -> Result<Vec<Effect>, CoreError> {
        let Some(open) = &self.view.review else {
            return Err(CoreError::NoOpenReview);
        };
        let review_id = open.snapshot.review.id;
        let original = open
            .original
            .as_ref()
            .filter(|r| {
                open.open_file.as_ref().is_some_and(|f| f.render == **r)
                    && r.repo_id == file.repo_id
                    && r.path == file.path
            })
            .cloned();
        // In Browse at a picked ref, every file opens as that ref's blob —
        // even ones the review's diff also touches.
        let browsing = self.view.tab == crate::Tab::Browse;
        if original.is_none()
            && browsing
            && self
                .browse
                .as_ref()
                .is_some_and(|b| b.repo_id == file.repo_id && b.root.is_none())
        {
            return Err(CoreError::UnknownFile(file));
        }
        let Some(render) = original.or_else(|| {
            if browsing {
                self.blob_render_of(&file)
            } else {
                open.files
                    .iter()
                    .find(|r| r.repo_id == file.repo_id && r.path == file.path)
                    .cloned()
                    .or_else(|| self.blob_render_of(&file))
            }
        }) else {
            return Err(CoreError::UnknownFile(file));
        };
        let (first_row, last_row) = (first_row.min(last_row), first_row.max(last_row));
        let mut effects = Vec::new();
        let changed_file = self.open_file().is_none_or(|f| f.render != render);
        if let Some(open) = &mut self.view.review {
            open.open_file = Some(OpenFile {
                render: render.clone(),
                first_row,
                last_row,
            });
        }
        if changed_file {
            // A newly opened file takes the keys, unless a modal panel has
            // them (a host scroll of the same file never moves focus).
            if !matches!(self.view.focus, crate::Focus::Composer | crate::Focus::Help) {
                self.view.focus = crate::Focus::Diff {
                    row: first_row,
                    side: nits_protocol::Side::Head,
                };
            }
            let evicted = self.retain_review_pins();
            self.content.write_through(evicted, &mut effects);
            let mine: Vec<CacheKey> = self
                .content
                .cache
                .keys()
                .filter(|k| matches!(k, CacheKey::Chunk { render: r, .. } if *r == render))
                .cloned()
                .collect();
            for k in mine {
                self.content.cache.pin(k);
            }
        }
        // Chunk fetches that drifted away from every open viewport are
        // dropped before they are sent.
        let window = self.viewport_window(&render);
        self.prune_queue(|f| match f {
            Fetch::Chunk { render: r, index } => {
                *r == render && window.is_some_and(|w| w.contains(*index))
            }
            Fetch::Tree { .. } | Fetch::Render { .. } => true,
        });
        if header_of(&self.content.cache, &render).is_some() {
            self.want_viewport_chunks(&render, &mut effects);
        } else {
            // No header yet, so no chunk geometry: stream from the start and
            // stop after the prefetch radius; the viewport is re-evaluated
            // when the header lands.
            self.want(
                Fetch::Render {
                    review_id,
                    render,
                    first_chunk: ChunkIndex::FIRST,
                    stop_after: ChunkIndex::new(PREFETCH_RADIUS),
                },
                &mut effects,
            );
        }
        // The diff rows themselves are derived after this returns.
        effects.push(crate::render(&[ViewSection::Focus]));
        Ok(effects)
    }

    pub(crate) fn close_file(&mut self) -> Result<Vec<Effect>, CoreError> {
        let Some(open) = &mut self.view.review else {
            return Err(CoreError::NoOpenReview);
        };
        if open.open_file.take().is_none() {
            return Err(CoreError::NoOpenFile);
        }
        open.original = None;
        let mut effects = Vec::new();
        self.prune_queue(|f| !matches!(f, Fetch::Chunk { .. }));
        let evicted = self.retain_review_pins();
        self.content.write_through(evicted, &mut effects);
        effects.push(crate::render(&[ViewSection::Focus]));
        Ok(effects)
    }

    // ---- streams --------------------------------------------------------

    /// A `Chunk` item on a `FileRender` stream: cache it and cancel the
    /// stream once it has passed what was wanted.
    pub(crate) fn stream_chunk(
        &mut self,
        id: nits_protocol::RequestId,
        render: &RenderKey,
        stop_after: ChunkIndex,
        chunk: nits_protocol::RenderChunk,
        effects: &mut Vec<Effect>,
    ) {
        let index = chunk.index;
        self.arrived(
            CacheKey::Chunk {
                render: render.clone(),
                index,
            },
            CacheValue::Chunk { chunk },
            Arrival::Stream,
            effects,
        );
        if index >= stop_after {
            effects.push(Effect::Send(ClientMsg::Cancel { id }));
        }
    }

    // ---- tree deltas ----------------------------------------------------

    /// Apply a working-tree delta in place: the snapshot under `from_root`
    /// becomes the one under `to_root`. Unknown `from_root` is ignored.
    pub(crate) fn tree_delta(&mut self, delta: &TreeDelta) -> Vec<Effect> {
        let from = CacheKey::Tree {
            root: delta.from_root,
        };
        let Some(CacheValue::Tree { snapshot }) = self.content.cache.remove(&from) else {
            return Vec::new();
        };
        let snapshot = apply_delta(snapshot, delta);
        let mut effects = Vec::new();
        if let Some(open) = &mut self.view.review
            && let Some(slot) = open.trees.iter_mut().find(|t| **t == delta.from_root)
        {
            *slot = delta.to_root;
        }
        self.arrived(
            CacheKey::Tree {
                root: delta.to_root,
            },
            CacheValue::Tree { snapshot },
            Arrival::Response,
            &mut effects,
        );
        effects
    }
}

/// A contiguous range of chunk indices, with the one the viewport starts in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChunkWindow {
    first: u32,
    last: u32,
    centre: u32,
}

impl ChunkWindow {
    fn around(header: &FileRenderHeader, first_row: u32, last_row: u32) -> Option<Self> {
        let RenderContent::Text {
            chunk_rows,
            chunk_count,
            ..
        } = header.content
        else {
            return None;
        };
        if chunk_rows == 0 || chunk_count == 0 {
            return None;
        }
        let max = chunk_count - 1;
        let centre = (first_row / chunk_rows).min(max);
        let end = (last_row / chunk_rows).min(max);
        Some(Self {
            first: centre.saturating_sub(PREFETCH_RADIUS),
            last: end.saturating_add(PREFETCH_RADIUS).min(max),
            centre,
        })
    }

    fn contains(self, index: ChunkIndex) -> bool {
        (self.first..=self.last).contains(&index.get())
    }

    /// Indices nearest the viewport first, so the visible chunk is
    /// requested before its neighbours.
    fn by_distance(self) -> Vec<ChunkIndex> {
        let mut v: Vec<u32> = (self.first..=self.last).collect();
        v.sort_by_key(|i| (i.abs_diff(self.centre), *i));
        v.into_iter().map(ChunkIndex::new).collect()
    }
}

fn header_of<'a>(cache: &'a ContentCache, render: &RenderKey) -> Option<&'a FileRenderHeader> {
    match cache.peek(&CacheKey::Header {
        render: render.clone(),
    })? {
        CacheValue::Header { header } => Some(header),
        CacheValue::Tree { .. } | CacheValue::Chunk { .. } => None,
    }
}

/// The `RefSpec` that re-resolves to a resolved ref, for `TreeSnapshot`.
fn ref_spec_of(r: &ResolvedRef) -> RefSpec {
    match &r.source {
        ResolvedSource::Commit { oid } => RefSpec::Commit { oid: *oid },
        ResolvedSource::WorkingTree { .. } => RefSpec::WorkingTree,
    }
}

/// Apply a delta: removed and changed paths go, then added and changed
/// entries come in; the result stays sorted by path.
fn apply_delta(mut snapshot: TreeSnapshot, delta: &TreeDelta) -> TreeSnapshot {
    let gone: Vec<&RepoPath> = delta
        .removed
        .iter()
        .chain(delta.changed.iter().map(|e| &e.path))
        .chain(delta.added.iter().map(|e| &e.path))
        .collect();
    snapshot.entries.retain(|e| !gone.contains(&&e.path));
    let incoming: Vec<TreeEntry> = delta
        .added
        .iter()
        .chain(delta.changed.iter())
        .cloned()
        .collect();
    snapshot.entries.extend(incoming);
    snapshot.entries.sort_by(|a, b| a.path.cmp(&b.path));
    snapshot.root_oid = delta.to_root;
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use nits_protocol::{Oid, TreeEntryKind};

    fn header(chunk_rows: u32, chunk_count: u32) -> FileRenderHeader {
        FileRenderHeader {
            repo_id: RepoId::from_parts(1, 1),
            path: RepoPath::new("a.rs").unwrap(),
            target: RenderTarget::Blob {
                oid: nits_protocol::BlobOid::from_bytes([1; 20]),
            },
            opts: RenderOpts::default(),
            lang: None,
            content: RenderContent::Text {
                total_rows: chunk_rows * chunk_count,
                chunk_rows,
                chunk_count,
                highlighted: true,
                additions: 0,
                deletions: 0,
                gaps: nits_protocol::GapTable::default(),
            },
        }
    }

    #[test]
    fn window_is_viewport_plus_radius_clipped_to_file() {
        let h = header(100, 1000);
        let w = ChunkWindow::around(&h, 0, 50).unwrap();
        assert_eq!((w.first, w.last), (0, 2));
        let w = ChunkWindow::around(&h, 5_050, 5_150).unwrap();
        assert_eq!((w.first, w.last), (48, 53));
        assert_eq!(
            w.by_distance().iter().map(|c| c.get()).collect::<Vec<_>>(),
            vec![50, 49, 51, 48, 52, 53]
        );
        let w = ChunkWindow::around(&h, 99_990, 200_000).unwrap();
        assert_eq!((w.first, w.last), (997, 999));
        assert!(ChunkWindow::around(&header(100, 0), 0, 0).is_none());
    }

    #[test]
    fn delta_replaces_changed_and_keeps_sorted() {
        let repo_id = RepoId::from_parts(1, 1);
        let entry = |p: &str, fill: u8| TreeEntry {
            path: RepoPath::new(p).unwrap(),
            kind: TreeEntryKind::File {
                oid: nits_protocol::BlobOid::new(Oid::from_bytes([fill; 20])),
                size: 1,
                executable: false,
            },
        };
        let snapshot = TreeSnapshot {
            repo_id,
            root_oid: TreeOid::from_bytes([1; 20]),
            entries: vec![entry("a", 1), entry("b", 1), entry("c", 1)],
        };
        let delta = TreeDelta {
            repo_id,
            from_root: TreeOid::from_bytes([1; 20]),
            to_root: TreeOid::from_bytes([2; 20]),
            added: vec![entry("aa", 2)],
            removed: vec![RepoPath::new("c").unwrap()],
            changed: vec![entry("b", 3)],
        };
        let out = apply_delta(snapshot, &delta);
        assert_eq!(out.root_oid, delta.to_root);
        assert_eq!(
            out.entries
                .iter()
                .map(|e| e.path.to_string())
                .collect::<Vec<_>>(),
            vec!["a", "aa", "b"]
        );
        assert_eq!(out.entries[2].kind, entry("b", 3).kind);
    }

    #[test]
    fn disk_index_trims_oldest_first() {
        let mut d = DiskIndex::new(Bytes(20));
        let k = |f: u8| CacheKey::Tree {
            root: TreeOid::from_bytes([f; 20]),
        };
        assert!(d.touch(&k(1), Bytes(10)).is_empty());
        assert!(d.touch(&k(2), Bytes(10)).is_empty());
        assert_eq!(d.touch(&k(3), Bytes(10)), vec![k(1)]);
        // Touching refreshes; a single oversize entry is kept.
        assert!(d.touch(&k(2), Bytes(10)).is_empty());
        assert_eq!(d.touch(&k(4), Bytes(30)), vec![k(3), k(2)]);
        assert_eq!(d.used, Bytes(30));
        d.forget(&k(4));
        assert_eq!(d.used, Bytes::ZERO);
    }
}
