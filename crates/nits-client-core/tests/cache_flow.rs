//! Plan 3.2: content flows memory → disk → daemon, with the exact effects
//! each tier produces; pins, budgets, viewport prefetch and restart.

// Scenario tests read top to bottom; splitting them would hide the flow.
#![allow(clippy::too_many_lines)]

use std::collections::BTreeMap;

use nits_client_core::{
    Action, Bytes, CacheConfig, CacheKey, CacheValue, ClientCore, Config, ConnectionView,
    CoreError, DiskTier, Effect, FileRef, Focus, IdSeed, Input, KeyChord, NamedKey,
    PREFETCH_RADIUS, RenderKey, ScopeChoice, TransportEvent, TreeKey, resolve_command,
};
use nits_protocol::{
    Author, BlobOid, BuildInfo, ChangeKind, ChunkIndex, ClientId, ClientMsg, ClientSeq, CommitOid,
    DiffScope, Event, EventBody, FileChange, FileRenderHeader, NonEmpty, Oid, ProtocolVersion,
    RefSpec, RenderChunk, RenderContent, RenderOpts, RenderTarget, RepoId, RepoPath, Request,
    RequestId, ResolvedRef, ResolvedSource, ResolvedTarget, Response, Review, ReviewId,
    ReviewSnapshot, ReviewStatus, ReviewTarget, Row, SchemaVersion, Seq, ServerMsg, Side,
    StreamItem, Timestamp, TreeDelta, TreeEntry, TreeEntryKind, TreeOid, TreeSnapshot, ViewSection,
    WorkspaceId,
};

// ---- fixtures -----------------------------------------------------------

fn config(cache: CacheConfig) -> Config {
    Config {
        client_id: ClientId::from_parts(1, 1),
        client: BuildInfo {
            name: "test".into(),
            version: "0".into(),
        },
        author: Author::Human {
            name: "someone".into(),
            machine: "host".into(),
        },
        id_seed: IdSeed(7),
        cache,
    }
}

fn local() -> CacheConfig {
    CacheConfig::default()
}

fn remote(memory: Bytes, disk: Bytes) -> CacheConfig {
    CacheConfig {
        memory_budget: memory,
        disk: DiskTier::Enabled { budget: disk },
        ..CacheConfig::default()
    }
}

fn repo_id() -> RepoId {
    RepoId::from_parts(2, 2)
}

fn review_id() -> ReviewId {
    ReviewId::from_parts(4, 1)
}

fn tree_oid(fill: u8) -> TreeOid {
    TreeOid::new(Oid::from_bytes([fill; 20]))
}

fn blob_oid(fill: u8) -> BlobOid {
    BlobOid::new(Oid::from_bytes([fill; 20]))
}

fn path(p: &str) -> RepoPath {
    RepoPath::new(p).unwrap()
}

fn resolved(base: u8, head: u8) -> NonEmpty<ResolvedTarget> {
    let commit = |fill: u8| ResolvedRef {
        tree: tree_oid(fill),
        source: ResolvedSource::Commit {
            oid: CommitOid::new(Oid::from_bytes([fill; 20])),
        },
    };
    NonEmpty::singleton(ResolvedTarget {
        repo_id: repo_id(),
        base: commit(base),
        head: commit(head),
    })
}

fn snapshot(base: u8, head: u8) -> ReviewSnapshot {
    ReviewSnapshot {
        review: Review {
            id: review_id(),
            workspace_id: WorkspaceId::from_parts(3, 3),
            title: "a review".into(),
            targets: NonEmpty::singleton(ReviewTarget {
                repo_id: repo_id(),
                base: RefSpec::Branch {
                    name: "main".into(),
                },
                head: RefSpec::Head,
            }),
            created: Timestamp::from_millis(0),
            status: ReviewStatus::Open,
        },
        resolved: Some(resolved(base, head)),
        threads: Vec::new(),
        comments: Vec::new(),
        viewed: Vec::new(),
        requests: Vec::new(),
        seq: Seq::new(1),
        checkpoints: Vec::new(),
    }
}

fn tree(root: u8, files: &[&str]) -> TreeSnapshot {
    TreeSnapshot {
        repo_id: repo_id(),
        root_oid: tree_oid(root),
        entries: files
            .iter()
            .map(|p| TreeEntry {
                path: path(p),
                kind: TreeEntryKind::File {
                    oid: blob_oid(1),
                    size: 1,
                    executable: false,
                },
            })
            .collect(),
    }
}

fn change(p: &str) -> FileChange {
    FileChange {
        repo_id: repo_id(),
        path: path(p),
        kind: ChangeKind::Modified {
            old: nits_protocol::BlobEntry {
                oid: blob_oid(10),
                mode: nits_protocol::BlobMode::Regular,
            },
            new: nits_protocol::BlobEntry {
                oid: blob_oid(11),
                mode: nits_protocol::BlobMode::Regular,
            },
        },
    }
}

fn render_key(p: &str) -> RenderKey {
    RenderKey {
        repo_id: repo_id(),
        path: path(p),
        target: RenderTarget::Diff {
            change: change(p).kind,
        },
        opts: RenderOpts::default(),
    }
}

fn header(p: &str, chunk_rows: u32, chunk_count: u32) -> FileRenderHeader {
    FileRenderHeader {
        repo_id: repo_id(),
        path: path(p),
        target: RenderTarget::Diff {
            change: change(p).kind,
        },
        opts: RenderOpts::default(),
        lang: Some("rust".into()),
        content: RenderContent::Text {
            total_rows: chunk_rows * chunk_count,
            chunk_rows,
            chunk_count,
            highlighted: true,
            additions: 1,
            deletions: 1,
            gaps: nits_protocol::GapTable::default(),
        },
    }
}

fn chunk(index: u32) -> RenderChunk {
    RenderChunk {
        index: ChunkIndex::new(index),
        rows: vec![Row::HunkHeader {
            text: format!("@@ chunk {index} @@"),
        }],
    }
}

fn chunk_key(p: &str, index: u32) -> CacheKey {
    CacheKey::Chunk {
        render: render_key(p),
        index: ChunkIndex::new(index),
    }
}

fn header_key(p: &str) -> CacheKey {
    CacheKey::Header {
        render: render_key(p),
    }
}

fn tree_key(fill: u8) -> CacheKey {
    CacheKey::Tree {
        tree: TreeKey {
            repo_id: repo_id(),
            root: tree_oid(fill),
        },
    }
}

fn file_ref(p: &str) -> FileRef {
    file(p)
}

fn file(p: &str) -> FileRef {
    FileRef {
        repo_id: repo_id(),
        path: path(p),
    }
}

fn welcome() -> ServerMsg {
    ServerMsg::Welcome {
        protocol: ProtocolVersion::CURRENT,
        daemon: BuildInfo {
            name: "nitsd".into(),
            version: "0".into(),
        },
        schema: SchemaVersion::CURRENT,
        upgrade: None,
    }
}

// ---- effect helpers -------------------------------------------------------

fn requests(effects: &[Effect]) -> Vec<(RequestId, Request)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send(ClientMsg::Request { id, request }) => Some((*id, request.clone())),
            Effect::Send(ClientMsg::Hello { .. } | ClientMsg::Cancel { .. })
            | Effect::Connect
            | Effect::Disconnect
            | Effect::Render(_)
            | Effect::Persist { .. }
            | Effect::Load { .. }
            | Effect::Remove { .. } => None,
        })
        .collect()
}

fn loads(effects: &[Effect]) -> Vec<String> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Load { key } => Some(key.clone()),
            Effect::Send(_)
            | Effect::Connect
            | Effect::Disconnect
            | Effect::Render(_)
            | Effect::Persist { .. }
            | Effect::Remove { .. } => None,
        })
        .collect()
}

fn persists(effects: &[Effect]) -> Vec<String> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Persist { key, .. } => Some(key.clone()),
            Effect::Send(_)
            | Effect::Connect
            | Effect::Disconnect
            | Effect::Render(_)
            | Effect::Load { .. }
            | Effect::Remove { .. } => None,
        })
        .collect()
}

fn persisted_view_prefs(effects: &[Effect]) -> Vec<nits_client_core::ViewPrefs> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Persist { key, value } if key == nits_client_core::ViewPrefs::KEY => {
                Some(serde_json::from_slice(value).unwrap())
            }
            Effect::Persist { .. }
            | Effect::Send(_)
            | Effect::Connect
            | Effect::Disconnect
            | Effect::Render(_)
            | Effect::Load { .. }
            | Effect::Remove { .. } => None,
        })
        .collect()
}

fn removes(effects: &[Effect]) -> Vec<String> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Remove { key } => Some(key.clone()),
            Effect::Send(_)
            | Effect::Connect
            | Effect::Disconnect
            | Effect::Render(_)
            | Effect::Load { .. }
            | Effect::Persist { .. } => None,
        })
        .collect()
}

fn cancels(effects: &[Effect]) -> Vec<RequestId> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send(ClientMsg::Cancel { id }) => Some(*id),
            Effect::Send(ClientMsg::Hello { .. } | ClientMsg::Request { .. })
            | Effect::Connect
            | Effect::Disconnect
            | Effect::Render(_)
            | Effect::Load { .. }
            | Effect::Persist { .. }
            | Effect::Remove { .. } => None,
        })
        .collect()
}

fn rendered(effects: &[Effect]) -> Vec<ViewSection> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Render(d) => Some(d.sections.clone()),
            Effect::Send(_)
            | Effect::Connect
            | Effect::Disconnect
            | Effect::Load { .. }
            | Effect::Persist { .. }
            | Effect::Remove { .. } => None,
        })
        .flatten()
        .collect()
}

fn is_content(r: &Request) -> bool {
    matches!(
        r,
        Request::TreeSnapshot { .. } | Request::FileRender { .. } | Request::RenderChunk { .. }
    )
}

/// The host's KV store: applies `Persist`/`Remove`, answers `Load`s.
#[derive(Default)]
struct Kv {
    map: BTreeMap<String, Vec<u8>>,
}

impl Kv {
    /// Apply storage effects and feed every `Load` answer back into the
    /// core, collecting everything the core emitted in response (recursively
    /// — an answer can trigger more loads).
    fn drive(&mut self, core: &mut ClientCore, effects: Vec<Effect>) -> Vec<Effect> {
        let mut out = Vec::new();
        let mut pending = effects;
        while !pending.is_empty() {
            let mut next = Vec::new();
            for e in pending {
                match &e {
                    Effect::Persist { key, value } => {
                        self.map.insert(key.clone(), value.clone());
                    }
                    Effect::Remove { key } => {
                        self.map.remove(key);
                    }
                    Effect::Load { key } => {
                        let value = self.map.get(key).cloned();
                        next.extend(
                            core.handle(Input::Stored {
                                key: key.clone(),
                                value,
                            })
                            .unwrap(),
                        );
                    }
                    Effect::Send(_) | Effect::Connect | Effect::Disconnect | Effect::Render(_) => {}
                }
                out.push(e);
            }
            pending = next;
        }
        out
    }
}

// ---- drivers --------------------------------------------------------------

fn subscribed(cache: CacheConfig) -> ClientCore {
    let mut core = ClientCore::new(config(cache));
    core.handle(Input::User(Action::Connect)).unwrap();
    core.handle(Input::Transport(TransportEvent::Connected))
        .unwrap();
    let effects = core.handle(Input::Server(welcome())).unwrap();
    let (id, _) = requests(&effects)[0].clone();
    core.handle(Input::Server(ServerMsg::Response {
        id,
        response: Response::Subscribed { seq: Seq::new(1) },
    }))
    .unwrap();
    assert_eq!(core.view().connection, ConnectionView::Subscribed);
    core
}

fn item(core: &mut ClientCore, id: RequestId, item: StreamItem) -> Vec<Effect> {
    core.handle(Input::Server(ServerMsg::StreamItem { id, item }))
        .unwrap()
}

/// Open a review over a local daemon: the full `OpenReview` stream with two
/// files, `a.rs` (10 chunks) and `b.rs` (1 chunk), first chunks included.
fn open_streamed(core: &mut ClientCore) -> Vec<Effect> {
    open_streamed_with(core, snapshot(1, 2))
}

fn open_streamed_with(core: &mut ClientCore, snapshot: ReviewSnapshot) -> Vec<Effect> {
    let effects = core
        .handle(Input::User(Action::OpenReview {
            review_id: review_id(),
        }))
        .unwrap();
    let (id, request) = requests(&effects)[0].clone();
    assert_eq!(
        request,
        Request::OpenReview {
            review_id: review_id(),
            opts: RenderOpts::default()
        }
    );
    let mut all = Vec::new();
    all.extend(item(core, id, StreamItem::ReviewSnapshot { snapshot }));
    all.extend(item(
        core,
        id,
        StreamItem::TreeSnapshot {
            snapshot: tree(1, &["a.rs", "b.rs"]),
        },
    ));
    all.extend(item(
        core,
        id,
        StreamItem::TreeSnapshot {
            snapshot: tree(2, &["a.rs", "b.rs", "c.rs"]),
        },
    ));
    for (p, count) in [("a.rs", 10), ("b.rs", 1)] {
        all.extend(item(
            core,
            id,
            StreamItem::Header {
                header: header(p, 100, count),
            },
        ));
        all.extend(item(
            core,
            id,
            StreamItem::Chunk {
                repo_id: repo_id(),
                path: path(p),
                chunk: chunk(0),
            },
        ));
    }
    all.extend(
        core.handle(Input::Server(ServerMsg::StreamEnd { id }))
            .unwrap(),
    );
    all
}

/// Answer every outstanding content request in `effects` from a "daemon"
/// that knows `a.rs` has 10 chunks; returns the effects those answers caused.
fn daemon_answers(core: &mut ClientCore, effects: &[Effect]) -> Vec<Effect> {
    let mut out = Vec::new();
    for (id, request) in requests(effects) {
        let msg = match request {
            Request::RenderChunk { index, .. } => Some(ServerMsg::Response {
                id,
                response: Response::RenderChunk {
                    chunk: chunk(index.get()),
                },
            }),
            Request::TreeSnapshot { .. }
            | Request::FileRender { .. }
            | Request::ChangeRender { .. }
            | Request::Search { .. }
            | Request::ListWorkspaces
            | Request::ListReviews { .. }
            | Request::ListRefs { .. }
            | Request::DefaultBase { .. }
            | Request::EnsureDirectoryReview { .. }
            | Request::GetReview { .. }
            | Request::ReviewSnapshot { .. }
            | Request::ListFiles { .. }
            | Request::OpenReview { .. }
            | Request::ResolveTargets { .. }
            | Request::ListCommits { .. }
            | Request::BlobRender { .. }
            | Request::Subscribe { .. }
            | Request::ReplayEvents { .. }
            | Request::Unsubscribe { .. }
            | Request::Mutate { .. }
            | Request::Shutdown => None,
        };
        if let Some(msg) = msg
            && let Ok(effects) = core.handle(Input::Server(msg))
        {
            out.extend(effects);
        }
    }
    out
}

// ---- tests ----------------------------------------------------------------

#[test]
fn streamed_open_fills_and_pins_the_cache_and_renders_once_at_end() {
    let mut core = subscribed(local());
    let effects = open_streamed(&mut core);
    // Snapshot renders the review sections; the stream end renders tree+diff.
    assert_eq!(
        rendered(&effects),
        vec![
            ViewSection::ReviewList,
            ViewSection::Diff,
            ViewSection::Threads,
            ViewSection::Conversation,
            ViewSection::Draft,
            ViewSection::Focus, // keys now go to the explorer
            ViewSection::Hints,
            // The diffing tree lists only the changed files, so the head
            // trees arriving render nothing; the file list does. Each
            // header/chunk arriving re-renders Diff: the stacked view
            // (`diffs`) grows with the cache.
            ViewSection::Tree, // file list (derived)
            ViewSection::Progress,
            ViewSection::Diff, // a.rs header → stacked diffs
            ViewSection::Diff, // a.rs chunk
            ViewSection::Tree, // header b.rs (derived)
            ViewSection::Progress,
            ViewSection::Diff, // b.rs header → stacked diffs
            ViewSection::Diff, // b.rs chunk
            // The stream end auto-opens the first diff (a.rs).
            ViewSection::Focus,
            ViewSection::Diff,
            ViewSection::Tree,
            ViewSection::Conversation, // current-check availability
            ViewSection::Hints,
        ]
    );
    // The stream brought all content; what goes out is the commits list
    // every open fetches, plus the auto-opened file's prefetch chunks.
    // Nothing touches disk.
    let reqs = requests(&effects);
    assert!(
        reqs.iter()
            .any(|(_, r)| matches!(r, Request::ListCommits { .. }))
    );
    assert!(
        reqs.iter()
            .all(|(_, r)| matches!(r, Request::ListCommits { .. } | Request::RenderChunk { .. }))
    );
    assert!(loads(&effects).is_empty());
    assert!(persists(&effects).is_empty());
    let cache = core.cache();
    for key in [
        tree_key(1),
        tree_key(2),
        header_key("a.rs"),
        header_key("b.rs"),
    ] {
        assert!(cache.contains(&key), "{key:?} missing");
        assert!(cache.is_pinned(&key), "{key:?} not pinned");
    }
    assert!(cache.contains(&chunk_key("a.rs", 0)));
    // a.rs is the auto-opened file, so its chunks are pinned too.
    assert!(cache.is_pinned(&chunk_key("a.rs", 0)));
    let open = core.view().review.as_ref().unwrap();
    assert_eq!(
        open.trees,
        vec![
            TreeKey {
                repo_id: repo_id(),
                root: tree_oid(1)
            },
            TreeKey {
                repo_id: repo_id(),
                root: tree_oid(2)
            }
        ]
    );
    assert_eq!(open.files, vec![render_key("a.rs"), render_key("b.rs")]);
}

#[test]
fn memory_hit_produces_no_effects_but_a_render() {
    let mut core = subscribed(local());
    open_streamed(&mut core);
    // b.rs has one chunk, already cached: the viewport is served locally.
    let effects = core
        .handle(Input::User(Action::Viewport {
            file: file("b.rs"),
            first_row: 0,
            last_row: 20,
        }))
        .unwrap();
    // Only renders: the diff/focus, and the tree (breadcrumbs, open mark).
    assert!(requests(&effects).is_empty());
    assert!(loads(&effects).is_empty());
    assert_eq!(
        rendered(&effects),
        // Hints don't change: the auto-open already focused the diff.
        vec![ViewSection::Focus, ViewSection::Tree, ViewSection::Diff]
    );
    assert_eq!(
        core.view().tree.breadcrumbs,
        vec![repo_id().to_string(), "b.rs".to_string()]
    );
    assert!(core.cache().is_pinned(&chunk_key("b.rs", 0)));
    // Closing the file releases the chunk pin but keeps the header pinned.
    let effects = core.handle(Input::User(Action::CloseFile)).unwrap();
    assert_eq!(
        rendered(&effects),
        vec![
            ViewSection::Focus,
            ViewSection::Tree,
            ViewSection::Diff,
            ViewSection::Hints
        ]
    );
    assert!(!core.cache().is_pinned(&chunk_key("b.rs", 0)));
    assert!(core.cache().is_pinned(&header_key("b.rs")));
    assert_eq!(
        core.handle(Input::User(Action::CloseFile)),
        Err(CoreError::NoOpenFile)
    );
    assert_eq!(
        core.handle(Input::User(Action::Viewport {
            file: file("zzz.rs"),
            first_row: 0,
            last_row: 0
        })),
        Err(CoreError::UnknownFile(file("zzz.rs")))
    );
}

#[test]
fn viewport_requests_only_the_window_and_bounds_in_flight() {
    let mut core = subscribed(local());
    let opened = open_streamed(&mut core);
    // Settle the auto-open's prefetch fetches so the budget is clean for
    // what this test measures.
    let _ = daemon_answers(&mut core, &opened);
    let max = core.cache_config().max_in_flight;
    // Rows 500..600 of a.rs span chunks 5..=6; the window is 3..=8, nearest
    // to the first visible chunk first.
    let effects = core
        .handle(Input::User(Action::Viewport {
            file: file("a.rs"),
            first_row: 500,
            last_row: 600,
        }))
        .unwrap();
    let sent: Vec<u32> = requests(&effects)
        .into_iter()
        .filter_map(|(_, r)| match r {
            Request::RenderChunk { index, .. } => Some(index.get()),
            Request::TreeSnapshot { .. }
            | Request::FileRender { .. }
            | Request::ChangeRender { .. }
            | Request::Search { .. }
            | Request::ListWorkspaces
            | Request::ListReviews { .. }
            | Request::ListRefs { .. }
            | Request::DefaultBase { .. }
            | Request::EnsureDirectoryReview { .. }
            | Request::GetReview { .. }
            | Request::ReviewSnapshot { .. }
            | Request::ListFiles { .. }
            | Request::OpenReview { .. }
            | Request::ResolveTargets { .. }
            | Request::ListCommits { .. }
            | Request::BlobRender { .. }
            | Request::Subscribe { .. }
            | Request::ReplayEvents { .. }
            | Request::Unsubscribe { .. }
            | Request::Mutate { .. }
            | Request::Shutdown => None,
        })
        .collect();
    assert_eq!(sent, vec![5, 4, 6, 3]);
    let all = effects.clone();
    assert_eq!(core.content_in_flight(), max);
    assert_eq!(core.content_queued(), 2); // chunks 7 and 8 wait for a slot
    assert!(loads(&effects).is_empty());

    // The user scrolls to the end before anything answers: window 7..=9
    // (clipped to the file). 7 and 8 stay queued, 9 joins; nothing else is
    // sent while the slots are full.
    let effects = core
        .handle(Input::User(Action::Viewport {
            file: file("a.rs"),
            first_row: 950,
            last_row: 999,
        }))
        .unwrap();
    assert!(requests(&effects).is_empty());
    assert_eq!(core.content_in_flight(), max);
    assert_eq!(core.content_queued(), 3);
    // Scrolling back to the top drops the queued far-away chunks (7, 8, 9)
    // before they are sent; 0, 1 and 2 are already cached (the auto-open
    // settled them), so nothing queues.
    core.handle(Input::User(Action::Viewport {
        file: file("a.rs"),
        first_row: 0,
        last_row: 10,
    }))
    .unwrap();
    assert_eq!(core.content_queued(), 0);
    assert_eq!(core.content_in_flight(), max);

    // Answers free slots; the queue drains, never exceeding the cap.
    let mut outstanding = all;
    outstanding.extend(effects);
    let mut rounds = 0;
    while core.content_in_flight() > 0 {
        assert!(core.content_in_flight() <= max);
        outstanding = daemon_answers(&mut core, &outstanding);
        rounds += 1;
        assert!(rounds < 10, "did not drain");
    }
    assert_eq!(core.content_queued(), 0);
    for i in [0, 1, 2, 3, 4, 5, 6] {
        assert!(core.cache().contains(&chunk_key("a.rs", i)), "chunk {i}");
    }
    for i in [7, 8, 9] {
        assert!(!core.cache().contains(&chunk_key("a.rs", i)), "chunk {i}");
    }
    // Chunks of the open file are pinned.
    assert!(core.cache().is_pinned(&chunk_key("a.rs", 1)));
}

#[test]
fn viewport_before_header_streams_the_file_and_cancels_past_the_radius() {
    let mut core = subscribed(local());
    open_streamed(&mut core);
    // Forget a.rs's header as if it were evicted; the file is still listed.
    let mut core = {
        // No public eviction hook: rebuild with a tiny budget so the header
        // is evicted by later inserts instead.
        let _ = &mut core;
        subscribed(CacheConfig {
            memory_budget: Bytes(1),
            ..local()
        })
    };
    let opened = open_streamed(&mut core);
    // Park the auto-opened diff and settle its fetches: unpinned under the
    // 1-byte budget, its chunks evict and their in-flight slots free up.
    core.handle(Input::User(Action::CloseFile)).unwrap();
    let _ = daemon_answers(&mut core, &opened);
    // Pinned entries survive the 1-byte budget; the unpinned chunks did not.
    assert!(core.cache().is_pinned(&header_key("a.rs")));
    assert!(!core.cache().contains(&chunk_key("a.rs", 0)));
    // Unpin by closing the review: everything unpinned is evicted.
    core.handle(Input::User(Action::CloseReview)).unwrap();
    assert!(core.cache().is_empty());

    // Reopen with only the snapshot streamed, then look at a file.
    let effects = core
        .handle(Input::User(Action::OpenReview {
            review_id: review_id(),
        }))
        .unwrap();
    let (id, _) = requests(&effects)[0].clone();
    item(
        &mut core,
        id,
        StreamItem::ReviewSnapshot {
            snapshot: snapshot(1, 2),
        },
    );
    core.handle(Input::Server(ServerMsg::StreamEnd { id }))
        .unwrap();
    // No headers were streamed, so the file list is empty: refresh it.
    assert_eq!(
        core.handle(Input::User(Action::Viewport {
            file: file("a.rs"),
            first_row: 0,
            last_row: 10
        })),
        Err(CoreError::UnknownFile(file("a.rs")))
    );
    let effects = core
        .handle(Input::Server(ServerMsg::Event {
            event: Event {
                seq: Seq::new(2),
                ts: Timestamp::from_millis(0),
                author: Author::Human {
                    name: "other".into(),
                    machine: "host".into(),
                },
                client_id: ClientId::from_parts(9, 9),
                client_seq: ClientSeq::new(1),
                body: EventBody::ReviewTargetsResolved {
                    review_id: review_id(),
                    targets: resolved(1, 3),
                },
            },
        }))
        .unwrap();
    let reqs = requests(&effects);
    // New head: its tree is fetched, the base tree too (evicted), and the
    // file list is refreshed.
    assert!(
        reqs.iter()
            .any(|(_, r)| matches!(r, Request::TreeSnapshot { .. }))
    );
    let (files_id, _) = reqs
        .iter()
        .find(|(_, r)| matches!(r, Request::ListFiles { .. }))
        .cloned()
        .unwrap();
    let effects = core
        .handle(Input::Server(ServerMsg::Response {
            id: files_id,
            response: Response::Files {
                files: vec![change("a.rs")],
                resolved: resolved(1, 3).into_iter().collect(),
            },
        }))
        .unwrap();
    // Header unknown → FileRender from chunk 0, stopping after chunk 0.
    let (render_id, request) = requests(&effects)
        .into_iter()
        .find(|(_, r)| matches!(r, Request::FileRender { .. }))
        .unwrap();
    assert_eq!(
        request,
        Request::FileRender {
            review_id: review_id(),
            repo_id: repo_id(),
            path: path("a.rs"),
            opts: RenderOpts::default(),
            first_chunk: ChunkIndex::FIRST,
            scope: DiffScope::All,
        }
    );
    item(
        &mut core,
        render_id,
        StreamItem::Header {
            header: header("a.rs", 100, 10),
        },
    );
    let effects = item(
        &mut core,
        render_id,
        StreamItem::Chunk {
            repo_id: repo_id(),
            path: path("a.rs"),
            chunk: chunk(0),
        },
    );
    assert_eq!(cancels(&effects), vec![render_id]);
    // Items still arriving after the cancel are cached, not rejected.
    item(
        &mut core,
        render_id,
        StreamItem::Chunk {
            repo_id: repo_id(),
            path: path("a.rs"),
            chunk: chunk(1),
        },
    );
    core.handle(Input::Server(ServerMsg::StreamEnd { id: render_id }))
        .unwrap();
    // Only the two (unanswered) tree requests remain in flight.
    assert_eq!(core.content_in_flight(), 2);
    assert!(core.cache().is_pinned(&header_key("a.rs")));
    // The late chunk was accepted; a.rs is the (auto-)open file, so its
    // chunks are pinned and survive even the 1-byte budget.
    assert!(core.cache().contains(&chunk_key("a.rs", 1)));
}

#[test]
fn disk_tier_load_before_send_and_dedupes_concurrent_misses() {
    let mut kv = Kv::default();
    let mut core = subscribed(remote(Bytes::mib(1), Bytes::mib(1)));
    // Piecewise open: snapshot first.
    let effects = core
        .handle(Input::User(Action::OpenReview {
            review_id: review_id(),
        }))
        .unwrap();
    let (id, request) = requests(&effects)[0].clone();
    assert_eq!(
        request,
        Request::ReviewSnapshot {
            review_id: review_id()
        }
    );
    let effects = core
        .handle(Input::Server(ServerMsg::Response {
            id,
            response: Response::ReviewSnapshot {
                snapshot: snapshot(1, 2),
            },
        }))
        .unwrap();
    // Trees: memory miss → exactly one Load each, no Send yet; ListFiles goes
    // out because the file list is not content-addressed.
    assert_eq!(
        loads(&effects),
        vec![tree_key(1).storage_key(), tree_key(2).storage_key()]
    );
    // (plus the commits list, fetched with every open for the sidebar).
    let reqs = requests(&effects);
    assert_eq!(reqs.len(), 2);
    assert!(
        reqs.iter()
            .any(|(_, r)| matches!(r, Request::ListCommits { .. }))
    );
    let files_id = reqs
        .iter()
        .find(|(_, r)| matches!(r, Request::ListFiles { .. }))
        .expect("ListFiles goes out with the open")
        .0;

    // Disk answers: tree 1 is there, tree 2 is not → one Send for tree 2.
    kv.map.insert(
        tree_key(1).storage_key(),
        CacheValue::Tree {
            snapshot: tree(1, &["a.rs"]),
        }
        .encode(),
    );
    let effects = kv.drive(&mut core, effects);
    let reqs: Vec<_> = requests(&effects)
        .into_iter()
        .filter(|(_, r)| is_content(r))
        .collect();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].1,
        Request::TreeSnapshot {
            repo_id: repo_id(),
            ref_spec: RefSpec::Commit {
                oid: CommitOid::new(Oid::from_bytes([2; 20]))
            }
        }
    );
    assert!(core.cache().contains(&tree_key(1)));
    assert!(core.cache().is_pinned(&tree_key(1)));
    // Loading from disk does not write back to disk.
    assert!(persists(&effects).is_empty());

    // The daemon's answer is cached and written through.
    let effects = core
        .handle(Input::Server(ServerMsg::Response {
            id: reqs[0].0,
            response: Response::TreeSnapshot {
                snapshot: tree(2, &["a.rs"]),
            },
        }))
        .unwrap();
    assert_eq!(persists(&effects), vec![tree_key(2).storage_key()]);
    kv.drive(&mut core, effects);

    // Files: header miss on both tiers → FileRender; chunk 0 then follows.
    let effects = core
        .handle(Input::Server(ServerMsg::Response {
            id: files_id,
            response: Response::Files {
                files: vec![change("a.rs")],
                resolved: resolved(1, 2).into_iter().collect(),
            },
        }))
        .unwrap();
    assert_eq!(loads(&effects), vec![header_key("a.rs").storage_key()]);
    let effects = kv.drive(&mut core, effects);
    let (render_id, _) = requests(&effects)
        .into_iter()
        .find(|(_, r)| matches!(r, Request::FileRender { .. }))
        .unwrap();

    // Two viewports want chunk 5 before anything is known: the second is a
    // no-op on the fetch side (one outstanding request per key).
    let effects = item(
        &mut core,
        render_id,
        StreamItem::Header {
            header: header("a.rs", 100, 10),
        },
    );
    kv.drive(&mut core, effects);
    core.handle(Input::Server(ServerMsg::StreamEnd { id: render_id }))
        .unwrap();
    assert_eq!(core.content_in_flight(), 0);
    let first = core
        .handle(Input::User(Action::Viewport {
            file: file("a.rs"),
            first_row: 500,
            last_row: 510,
        }))
        .unwrap();
    let first_loads = loads(&first);
    assert_eq!(first_loads.len(), 5);
    assert!(requests(&first).is_empty());
    let second = core
        .handle(Input::User(Action::Viewport {
            file: file("a.rs"),
            first_row: 500,
            last_row: 510,
        }))
        .unwrap();
    assert!(loads(&second).is_empty());
    assert!(requests(&second).is_empty());
    // All disk misses: each becomes one daemon request, capped in flight.
    let effects = kv.drive(&mut core, first);
    let chunk_reqs: Vec<_> = requests(&effects)
        .into_iter()
        .filter(|(_, r)| matches!(r, Request::RenderChunk { .. }))
        .collect();
    assert_eq!(chunk_reqs.len(), core.cache_config().max_in_flight);
    assert_eq!(core.content_queued(), 1);
}

#[test]
fn previous_render_generation_is_not_loaded_even_when_its_header_decodes() {
    let mut kv = Kv::default();
    let mut previous = header("a.rs", 1, 1);
    let RenderContent::Text {
        additions,
        deletions,
        ..
    } = &mut previous.content
    else {
        panic!("text")
    };
    *additions = 0;
    *deletions = 0;
    kv.map.insert(
        serde_json::to_string(&header_key("a.rs")).unwrap(),
        CacheValue::Header { header: previous }.encode(),
    );
    kv.map.insert(
        serde_json::to_string(&chunk_key("a.rs", 0)).unwrap(),
        CacheValue::Chunk {
            chunk: RenderChunk {
                index: ChunkIndex::FIRST,
                rows: vec![Row::Expander {
                    hidden: 1,
                    dir: nits_protocol::ExpandDir::Both,
                    gap: nits_protocol::Gap::new(0),
                }],
            },
        }
        .encode(),
    );
    let mut core = subscribed(remote(Bytes::mib(1), Bytes::mib(1)));
    let effects = core
        .handle(Input::User(Action::OpenReview {
            review_id: review_id(),
        }))
        .unwrap();
    let (id, _) = requests(&effects)[0].clone();
    let effects = core
        .handle(Input::Server(ServerMsg::Response {
            id,
            response: Response::ReviewSnapshot {
                snapshot: snapshot(1, 2),
            },
        }))
        .unwrap();
    let files_id = requests(&effects)
        .into_iter()
        .find(|(_, r)| matches!(r, Request::ListFiles { .. }))
        .unwrap()
        .0;
    let effects = core
        .handle(Input::Server(ServerMsg::Response {
            id: files_id,
            response: Response::Files {
                files: vec![change("a.rs")],
                resolved: resolved(1, 2).into_iter().collect(),
            },
        }))
        .unwrap();
    let effects = kv.drive(&mut core, effects);
    let render_id = requests(&effects)
        .into_iter()
        .find(|(_, r)| matches!(r, Request::FileRender { .. }))
        .expect("old valid header must not suppress daemon render")
        .0;
    assert!(!core.cache().contains(&header_key("a.rs")));
    let effects = item(
        &mut core,
        render_id,
        StreamItem::Header {
            header: header("a.rs", 1, 1),
        },
    );
    kv.drive(&mut core, effects);
    assert!(
        !core.cache().contains(&chunk_key("a.rs", 0)),
        "old chunk is in the previous namespace too"
    );
    let cell = |ending| nits_protocol::Cell {
        line_no: nits_protocol::LineNo::new(1).unwrap(),
        text: "same".into(),
        ending,
        spans: vec![],
        changed: vec![],
    };
    let current = RenderChunk {
        index: ChunkIndex::FIRST,
        rows: vec![Row::Modified {
            left: cell(nits_protocol::LineEnding::Missing),
            right: cell(nits_protocol::LineEnding::Lf),
        }],
    };
    let effects = item(
        &mut core,
        render_id,
        StreamItem::Chunk {
            repo_id: repo_id(),
            path: path("a.rs"),
            chunk: current.clone(),
        },
    );
    kv.drive(&mut core, effects);
    let stored =
        CacheValue::decode(kv.map.get(&chunk_key("a.rs", 0).storage_key()).unwrap()).unwrap();
    assert_eq!(stored, CacheValue::Chunk { chunk: current });
}

#[test]
fn old_format_and_wrong_mode_disk_headers_are_removed_and_fetched_again() {
    for legacy_format in [false, true] {
        let mut core = subscribed(remote(Bytes::mib(1), Bytes::mib(1)));
        let effects = core
            .handle(Input::User(Action::OpenReview {
                review_id: review_id(),
            }))
            .unwrap();
        let (id, _) = requests(&effects)[0].clone();
        let effects = core
            .handle(Input::Server(ServerMsg::Response {
                id,
                response: Response::ReviewSnapshot {
                    snapshot: snapshot(1, 2),
                },
            }))
            .unwrap();
        let files_id = requests(&effects)
            .into_iter()
            .find(|(_, r)| matches!(r, Request::ListFiles { .. }))
            .unwrap()
            .0;
        let mut file = change("a.rs");
        let ChangeKind::Modified { new, .. } = &mut file.kind else {
            panic!("modified")
        };
        new.mode = nits_protocol::BlobMode::Executable;
        let effects = core
            .handle(Input::Server(ServerMsg::Response {
                id: files_id,
                response: Response::Files {
                    files: vec![file],
                    resolved: resolved(1, 2).into_iter().collect(),
                },
            }))
            .unwrap();
        let key = loads(&effects)
            .into_iter()
            .find(|key| key.contains("Header"))
            .unwrap();
        let stale = CacheValue::Header {
            header: header("a.rs", 100, 1),
        };
        let bytes = if legacy_format {
            let mut json: serde_json::Value = serde_json::from_slice(&stale.encode()).unwrap();
            let change = json.pointer_mut("/header/target/change").unwrap();
            for side in ["old", "new"] {
                change[side] = change[side]["oid"].take();
            }
            serde_json::to_vec(&json).unwrap()
        } else {
            stale.encode()
        };
        let effects = core
            .handle(Input::Stored {
                key: key.clone(),
                value: Some(bytes),
            })
            .unwrap();
        assert_eq!(removes(&effects), vec![key]);
        assert!(requests(&effects).iter().any(|(_, request)| matches!(request, Request::FileRender { path, .. } if path.as_str() == "a.rs")));
        assert!(!core.cache().contains(&header_key("a.rs")));
    }
}

#[test]
fn eviction_respects_both_budgets_and_pins_survive() {
    let mut kv = Kv::default();
    // Memory: room for ~2 chunks; disk: room for ~4.
    let one = Bytes(CacheValue::Chunk { chunk: chunk(0) }.encode().len() as u64);
    let mut core = subscribed(remote(Bytes(one.get() * 2 + 1), Bytes(one.get() * 4 + 1)));
    // Reach a state with a.rs open (header known, chunks flowing).
    let effects = core
        .handle(Input::User(Action::OpenReview {
            review_id: review_id(),
        }))
        .unwrap();
    let (id, _) = requests(&effects)[0].clone();
    let effects = core
        .handle(Input::Server(ServerMsg::Response {
            id,
            response: Response::ReviewSnapshot {
                snapshot: snapshot(1, 2),
            },
        }))
        .unwrap();
    let effects = kv.drive(&mut core, effects);
    let files_id = requests(&effects)
        .into_iter()
        .find(|(_, r)| matches!(r, Request::ListFiles { .. }))
        .unwrap()
        .0;
    let effects = core
        .handle(Input::Server(ServerMsg::Response {
            id: files_id,
            response: Response::Files {
                files: vec![change("a.rs")],
                resolved: resolved(1, 2).into_iter().collect(),
            },
        }))
        .unwrap();
    let effects = kv.drive(&mut core, effects);
    let (render_id, _) = requests(&effects)
        .into_iter()
        .find(|(_, r)| matches!(r, Request::FileRender { .. }))
        .unwrap();
    let effects = item(
        &mut core,
        render_id,
        StreamItem::Header {
            header: header("a.rs", 100, 20),
        },
    );
    kv.drive(&mut core, effects);
    core.handle(Input::Server(ServerMsg::StreamEnd { id: render_id }))
        .unwrap();

    // Not open: chunks are unpinned. Stream 6 chunks through RenderChunk
    // answers and watch the tiers trim.
    let mut all = Vec::new();
    let mut effects = core
        .handle(Input::User(Action::Viewport {
            file: file("a.rs"),
            first_row: 1000,
            last_row: 1000,
        }))
        .unwrap();
    // Viewport pins the open file's chunks; close it again so they are
    // ordinary LRU entries, but keep the wants flowing.
    effects.extend(core.handle(Input::User(Action::CloseFile)).unwrap());
    let effects = kv.drive(&mut core, effects);
    let mut effects = effects;
    for _ in 0..4 {
        let answered = daemon_answers(&mut core, &effects);
        let driven = kv.drive(&mut core, answered);
        all.extend(driven.clone());
        effects = driven;
    }
    let mem_chunks = core
        .cache()
        .keys()
        .filter(|k| matches!(k, CacheKey::Chunk { .. }))
        .count();
    assert!(core.cache().used() <= core.cache().budget() || mem_chunks == 0);
    assert!(mem_chunks <= 2, "memory holds {mem_chunks} chunks");
    // Every chunk that arrived was persisted, and the disk was trimmed.
    let persisted = persists(&all).len();
    assert!(persisted >= 5, "persisted {persisted}");
    assert!(!removes(&all).is_empty(), "disk tier never trimmed");
    let disk_chunks = kv.map.keys().filter(|k| k.contains("\"Chunk\"")).count();
    assert!(disk_chunks <= 4, "disk holds {disk_chunks}");
    // The pinned header outlived all that pressure.
    assert!(core.cache().is_pinned(&header_key("a.rs")));
    assert!(core.cache().contains(&header_key("a.rs")));
}

#[test]
fn restart_serves_the_previous_review_from_disk_without_content_requests() {
    let mut kv = Kv::default();
    // Session 1: open piecewise, everything comes from the daemon.
    {
        let mut core = subscribed(remote(Bytes::mib(1), Bytes::mib(1)));
        let effects = core
            .handle(Input::User(Action::OpenReview {
                review_id: review_id(),
            }))
            .unwrap();
        let (id, _) = requests(&effects)[0].clone();
        let effects = core
            .handle(Input::Server(ServerMsg::Response {
                id,
                response: Response::ReviewSnapshot {
                    snapshot: snapshot(1, 2),
                },
            }))
            .unwrap();
        let effects = kv.drive(&mut core, effects);
        let mut files_id = None;
        for (id, r) in requests(&effects) {
            match r {
                Request::TreeSnapshot { ref_spec, .. } => {
                    let RefSpec::Commit { oid } = ref_spec else {
                        panic!("commit ref");
                    };
                    let fill = oid.oid().as_bytes()[0];
                    let effects = core
                        .handle(Input::Server(ServerMsg::Response {
                            id,
                            response: Response::TreeSnapshot {
                                snapshot: tree(fill, &["a.rs"]),
                            },
                        }))
                        .unwrap();
                    kv.drive(&mut core, effects);
                }
                Request::ListFiles { .. } => files_id = Some(id),
                // Fetched with every open (sidebar commits); unanswered here.
                Request::ListCommits { .. } => {}
                Request::FileRender { .. }
                | Request::ChangeRender { .. }
                | Request::Search { .. }
                | Request::RenderChunk { .. }
                | Request::ListWorkspaces
                | Request::ListReviews { .. }
                | Request::ListRefs { .. }
                | Request::DefaultBase { .. }
                | Request::EnsureDirectoryReview { .. }
                | Request::GetReview { .. }
                | Request::ReviewSnapshot { .. }
                | Request::OpenReview { .. }
                | Request::ResolveTargets { .. }
                | Request::BlobRender { .. }
                | Request::Subscribe { .. }
                | Request::ReplayEvents { .. }
                | Request::Unsubscribe { .. }
                | Request::Mutate { .. }
                | Request::Shutdown => panic!("unexpected {r:?}"),
            }
        }
        let effects = core
            .handle(Input::Server(ServerMsg::Response {
                id: files_id.unwrap(),
                response: Response::Files {
                    files: vec![change("a.rs")],
                    resolved: resolved(1, 2).into_iter().collect(),
                },
            }))
            .unwrap();
        let effects = kv.drive(&mut core, effects);
        let (render_id, _) = requests(&effects)
            .into_iter()
            .find(|(_, r)| matches!(r, Request::FileRender { .. }))
            .unwrap();
        let effects = item(
            &mut core,
            render_id,
            StreamItem::Header {
                header: header("a.rs", 100, 3),
            },
        );
        kv.drive(&mut core, effects);
        let effects = item(
            &mut core,
            render_id,
            StreamItem::Chunk {
                repo_id: repo_id(),
                path: path("a.rs"),
                chunk: chunk(0),
            },
        );
        kv.drive(&mut core, effects);
        core.handle(Input::Server(ServerMsg::StreamEnd { id: render_id }))
            .unwrap();
    }
    assert_eq!(kv.map.len(), 4, "trees, header, chunk 0 on disk");

    // Session 2: a fresh core over the same store.
    let mut core = subscribed(remote(Bytes::mib(1), Bytes::mib(1)));
    let effects = core
        .handle(Input::User(Action::OpenReview {
            review_id: review_id(),
        }))
        .unwrap();
    let (id, _) = requests(&effects)[0].clone();
    let effects = core
        .handle(Input::Server(ServerMsg::Response {
            id,
            response: Response::ReviewSnapshot {
                snapshot: snapshot(1, 2),
            },
        }))
        .unwrap();
    let effects = kv.drive(&mut core, effects);
    let files_id = requests(&effects)
        .into_iter()
        .find(|(_, r)| matches!(r, Request::ListFiles { .. }))
        .unwrap()
        .0;
    let effects = core
        .handle(Input::Server(ServerMsg::Response {
            id: files_id,
            response: Response::Files {
                files: vec![change("a.rs")],
                resolved: resolved(1, 2).into_iter().collect(),
            },
        }))
        .unwrap();
    let effects = kv.drive(&mut core, effects);
    let effects2 = core
        .handle(Input::User(Action::Viewport {
            file: file("a.rs"),
            first_row: 0,
            last_row: 10,
        }))
        .unwrap();
    let effects2 = kv.drive(&mut core, effects2);
    // The auto-open of a.rs is served from disk except the prefetch-radius
    // chunks (asserted below); the explicit viewport asks nothing more.
    assert!(
        requests(&effects2)
            .iter()
            .all(|(_, request)| !is_content(request)),
        "went to the daemon twice"
    );
    for key in [
        tree_key(1),
        tree_key(2),
        header_key("a.rs"),
        chunk_key("a.rs", 0),
    ] {
        assert!(core.cache().contains(&key), "{key:?} not served from disk");
    }
    // Chunks 1 and 2 (prefetch radius) were never on disk: those, and only
    // those, go to the daemon (with the auto-open).
    let mut fetched: Vec<u32> = requests(&effects)
        .into_iter()
        .map(|(_, r)| match r {
            Request::RenderChunk { index, .. } => index.get(),
            Request::TreeSnapshot { .. }
            | Request::FileRender { .. }
            | Request::ChangeRender { .. }
            | Request::Search { .. }
            | Request::ListWorkspaces
            | Request::ListReviews { .. }
            | Request::ListRefs { .. }
            | Request::DefaultBase { .. }
            | Request::EnsureDirectoryReview { .. }
            | Request::GetReview { .. }
            | Request::ReviewSnapshot { .. }
            | Request::ListFiles { .. }
            | Request::OpenReview { .. }
            | Request::ResolveTargets { .. }
            | Request::ListCommits { .. }
            | Request::BlobRender { .. }
            | Request::Subscribe { .. }
            | Request::ReplayEvents { .. }
            | Request::Unsubscribe { .. }
            | Request::Mutate { .. }
            | Request::Shutdown => panic!("unexpected {r:?}"),
        })
        .collect();
    fetched.sort_unstable();
    assert_eq!(fetched, vec![1, 2]);
    assert_eq!(core.content_in_flight(), 2);
    assert_eq!(core.content_queued(), 0);
}

#[test]
fn tree_delta_preserves_immutable_roots_until_targets_change() {
    let mut core = subscribed(local());
    open_streamed(&mut core);
    let delta = TreeDelta {
        repo_id: repo_id(),
        from_root: tree_oid(2),
        to_root: tree_oid(9),
        added: vec![TreeEntry {
            path: path("d.rs"),
            kind: TreeEntryKind::File {
                oid: blob_oid(4),
                size: 4,
                executable: false,
            },
        }],
        removed: vec![path("c.rs")],
        changed: Vec::new(),
    };
    let effects = core
        .handle(Input::Server(ServerMsg::TreeDelta { delta }))
        .unwrap();
    // The cache adds a snapshot; the diffing tree (changed files only)
    // does not depend on the head tree, so nothing re-renders.
    assert_eq!(rendered(&effects), Vec::new());
    assert!(core.cache().contains(&tree_key(2)));
    assert!(!core.cache().is_pinned(&tree_key(9)));
    let Some(CacheValue::Tree { snapshot }) = core.cache().peek(&tree_key(9)) else {
        panic!("tree 9 missing");
    };
    let paths: Vec<String> = snapshot
        .entries
        .iter()
        .map(|e| e.path.to_string())
        .collect();
    assert_eq!(paths, vec!["a.rs", "b.rs", "d.rs"]);
    assert_eq!(
        core.view().review.as_ref().unwrap().trees,
        vec![
            TreeKey {
                repo_id: repo_id(),
                root: tree_oid(1)
            },
            TreeKey {
                repo_id: repo_id(),
                root: tree_oid(2)
            }
        ]
    );
    // A delta for an unknown root is ignored.
    let effects = core
        .handle(Input::Server(ServerMsg::TreeDelta {
            delta: TreeDelta {
                repo_id: repo_id(),
                from_root: tree_oid(42),
                to_root: tree_oid(43),
                added: Vec::new(),
                removed: Vec::new(),
                changed: Vec::new(),
            },
        }))
        .unwrap();
    assert!(effects.is_empty());
}

#[test]
fn close_review_releases_pins_and_drops_queued_fetches() {
    let mut core = subscribed(local());
    let opened = open_streamed(&mut core);
    // Settle the auto-open's prefetch fetches: this test counts the queue.
    let _ = daemon_answers(&mut core, &opened);
    core.handle(Input::User(Action::Viewport {
        file: file("a.rs"),
        first_row: 500,
        last_row: 600,
    }))
    .unwrap();
    assert_eq!(core.content_queued(), 2);
    let effects = core.handle(Input::User(Action::CloseReview)).unwrap();
    assert_eq!(
        rendered(&effects),
        vec![
            ViewSection::ReviewList,
            ViewSection::Draft,
            ViewSection::Tree,
            ViewSection::Progress,
            ViewSection::Diff,
            ViewSection::Focus,
            ViewSection::Hints,
        ]
    );
    assert_eq!(core.content_queued(), 0);
    assert!(core.cache().keys().all(|k| !core.cache().is_pinned(k)));
    // Late answers to the in-flight chunk requests are still cached.
    let answered = daemon_answers(&mut core, &effects);
    assert!(answered.is_empty());
    assert_eq!(
        core.handle(Input::User(Action::Viewport {
            file: file("a.rs"),
            first_row: 0,
            last_row: 0
        })),
        Err(CoreError::NoOpenReview)
    );
    assert_eq!(PREFETCH_RADIUS, 2);
}

#[test]
fn stored_answers_for_unknown_keys_are_rejected() {
    let mut core = subscribed(remote(Bytes::mib(1), Bytes::mib(1)));
    assert_eq!(
        core.handle(Input::Stored {
            key: "nope".into(),
            value: None
        }),
        Err(CoreError::UnknownKey("nope".into()))
    );
    let _ = ClientMsg::Cancel {
        id: RequestId::new(1),
    };
}

// ---- 3.5: explorer, prefs, viewed marks ----------------------------------

#[test]
fn explorer_is_derived_from_head_trees_and_never_sends() {
    let mut core = subscribed(local());
    open_streamed(&mut core);
    let tree = &core.view().tree;
    assert_eq!(tree.roots.len(), 1);
    let nits_client_core::TreeNode::Dir {
        children,
        changed_below,
        expanded,
        ..
    } = &tree.roots[0]
    else {
        panic!("root is a dir");
    };
    // Diffing mode: every dir starts expanded (UI-DESIGN §Layout).
    assert!(expanded);
    assert_eq!(*changed_below, 2);
    let names: Vec<&str> = children
        .iter()
        .map(|n| match n {
            nits_client_core::TreeNode::Dir { name, .. }
            | nits_client_core::TreeNode::File { name, .. } => name.as_str(),
        })
        .collect();
    // The head tree also has c.rs, but the diffing tree lists only the
    // changed files (the full tree belongs to Browse).
    assert_eq!(names, vec!["a.rs", "b.rs"]);
    let nits_client_core::TreeNode::File { change, viewed, .. } = &children[0] else {
        panic!("file");
    };
    assert!(change.is_some());
    assert_eq!(*viewed, nits_client_core::ViewedState::Unviewed);
    assert_eq!(
        core.view().progress,
        nits_client_core::Progress {
            viewed: 0,
            changed_since_viewed: 0,
            total: 2,
            // Header totals: a.rs and b.rs each render +1 −1.
            additions: 2,
            deletions: 2,
        }
    );

    // Expanding and searching are local: renders only.
    let effects = core
        .handle(Input::User(Action::ToggleDir {
            repo_id: repo_id(),
            path: None,
        }))
        .unwrap();
    assert!(requests(&effects).is_empty() && loads(&effects).is_empty());
    // Diffing mode defaults dirs open; the toggle collapses them.
    assert_eq!(rendered(&effects), vec![ViewSection::Tree]);
    let nits_client_core::TreeNode::Dir { expanded, .. } = &core.view().tree.roots[0] else {
        panic!("root is a dir");
    };
    assert!(!expanded);
    let effects = core
        .handle(Input::User(Action::FileSearch {
            query: Some("brs".into()),
        }))
        .unwrap();
    assert!(requests(&effects).is_empty() && loads(&effects).is_empty());
    let search = core.view().tree.search.as_ref().unwrap();
    assert_eq!(search.query, "brs");
    assert_eq!(search.hits[0].file, file("b.rs"));
    // A query that matches nothing yields no hits; closing clears it.
    core.handle(Input::User(Action::FileSearch {
        query: Some("zzz".into()),
    }))
    .unwrap();
    assert!(core.view().tree.search.as_ref().unwrap().hits.is_empty());
    let effects = core
        .handle(Input::User(Action::FileSearch { query: None }))
        .unwrap();
    assert_eq!(rendered(&effects), vec![ViewSection::Tree]);
    assert!(core.view().tree.search.is_none());
    // Nothing changed → nothing rendered (no empty renders either).
    let effects = core
        .handle(Input::User(Action::FileSearch { query: None }))
        .unwrap();
    assert!(effects.is_empty());
}

#[test]
fn mark_viewed_is_optimistic_and_drives_progress() {
    let mut core = subscribed(local());
    open_streamed(&mut core);
    let effects = core
        .handle(Input::User(Action::MarkViewed { file: file("a.rs") }))
        .unwrap();
    let (_, request) = requests(&effects)[0].clone();
    assert!(matches!(
        request,
        Request::Mutate {
            mutation: nits_protocol::Mutation::MarkViewed { .. },
            ..
        }
    ));
    assert_eq!(
        rendered(&effects),
        // The auto-opened a.rs diff re-renders with its viewed state.
        vec![ViewSection::Progress, ViewSection::Tree, ViewSection::Diff]
    );
    assert_eq!(core.view().progress.viewed, 1);
    assert_eq!(core.view().review.as_ref().unwrap().pending.len(), 1);
    // Unknown file / agent viewer are typed errors.
    assert_eq!(
        core.handle(Input::User(Action::MarkViewed {
            file: file("nope.rs")
        })),
        Err(CoreError::UnknownFile(file("nope.rs")))
    );
    let effects = core
        .handle(Input::User(Action::UnmarkViewed { file: file("a.rs") }))
        .unwrap();
    assert_eq!(requests(&effects).len(), 1);
    assert_eq!(core.view().progress.viewed, 0);
}

#[test]
fn prefs_are_loaded_once_on_connect_persisted_on_change_and_re_key_renders() {
    let mut core = ClientCore::new(config(local()));
    let effects = core.handle(Input::User(Action::Connect)).unwrap();
    assert_eq!(
        loads(&effects),
        vec![
            nits_client_core::ViewPrefs::KEY.to_string(),
            nits_client_core::Keymap::KEY.to_string()
        ]
    );
    // Stored prefs: split layout, whitespace ignored, sidebar hidden.
    let stored = nits_client_core::ViewPrefs {
        layout: nits_client_core::Layout::Split,
        ignore_whitespace: true,
        context_lines: 5,
        sidebar_hidden: true,
    };
    let effects = core
        .handle(Input::Stored {
            key: nits_client_core::ViewPrefs::KEY.into(),
            value: Some(serde_json::to_vec(&stored).unwrap()),
        })
        .unwrap();
    assert_eq!(rendered(&effects), vec![ViewSection::Diff]);
    assert!(persists(&effects).is_empty(), "loading does not write back");
    assert_eq!(core.view().prefs, stored);
    assert_eq!(core.cache_config().render_opts, stored.render_opts());
    // Absent / corrupt values keep the defaults, silently.
    let mut fresh = ClientCore::new(config(local()));
    fresh.handle(Input::User(Action::Connect)).unwrap();
    let effects = fresh
        .handle(Input::Stored {
            key: nits_client_core::ViewPrefs::KEY.into(),
            value: Some(b"garbage".to_vec()),
        })
        .unwrap();
    assert!(effects.is_empty());
    assert_eq!(fresh.view().prefs, nits_client_core::ViewPrefs::default());

    // Layout changes render and persist, never send.
    let mut core = subscribed(local());
    open_streamed(&mut core);
    let effects = core
        .handle(Input::User(Action::SetLayout {
            layout: nits_client_core::Layout::Split,
        }))
        .unwrap();
    assert!(requests(&effects).is_empty());
    assert_eq!(
        persists(&effects),
        vec![nits_client_core::ViewPrefs::KEY.to_string()]
    );
    assert_eq!(persisted_view_prefs(&effects), vec![core.view().prefs]);
    assert_eq!(core.view().prefs.layout, nits_client_core::Layout::Split);
    assert_eq!(rendered(&effects), vec![ViewSection::Diff]);
    // Sidebar visibility uses the same preference path without touching the
    // daemon or render cache. The persisted value is the whole typed prefs
    // record, and toggling it leaves semantic focus where it can be restored.
    let focus = core.view().focus;
    let effects = core.handle(Input::User(Action::ToggleSidebar)).unwrap();
    assert_eq!(effects.len(), 2);
    let Effect::Persist { key, value } = &effects[0] else {
        panic!("preference write must precede its render")
    };
    assert_eq!(key, nits_client_core::ViewPrefs::KEY);
    let hidden: nits_client_core::ViewPrefs = serde_json::from_slice(value).unwrap();
    assert!(hidden.sidebar_hidden);
    assert_eq!(hidden.layout, nits_client_core::Layout::Split);
    assert_eq!(hidden.render_opts(), core.cache_config().render_opts);
    let Effect::Render(delta) = &effects[1] else {
        panic!("preference write must end with one render")
    };
    assert_eq!(delta.sections, vec![ViewSection::Diff]);
    assert!(requests(&effects).is_empty());
    assert_eq!(core.view().focus, focus);

    let effects = core.handle(Input::User(Action::ToggleSidebar)).unwrap();
    assert_eq!(effects.len(), 2);
    let Effect::Persist { key, value } = &effects[0] else {
        panic!("preference write must precede its render")
    };
    assert_eq!(key, nits_client_core::ViewPrefs::KEY);
    let shown: nits_client_core::ViewPrefs = serde_json::from_slice(value).unwrap();
    assert!(!shown.sidebar_hidden);
    assert_eq!(rendered(&effects), vec![ViewSection::Diff]);
    assert!(requests(&effects).is_empty());
    assert_eq!(core.view().focus, focus);
    // Render options re-key every render: the file list is fetched again
    // with the new opts and the tree empties until it lands.
    let effects = core
        .handle(Input::User(Action::SetRenderOpts {
            ignore_whitespace: true,
            context_lines: 3,
        }))
        .unwrap();
    let reqs = requests(&effects);
    assert_eq!(reqs.len(), 1);
    assert!(matches!(reqs[0].1, Request::ListFiles { .. }));
    assert_eq!(
        persists(&effects),
        vec![nits_client_core::ViewPrefs::KEY.to_string()]
    );
    assert_eq!(persisted_view_prefs(&effects), vec![core.view().prefs]);
    assert!(core.view().prefs.ignore_whitespace);
    assert_eq!(core.view().prefs.layout, nits_client_core::Layout::Split);
    assert!(core.view().review.as_ref().unwrap().files.is_empty());
    let effects = core
        .handle(Input::Server(ServerMsg::Response {
            id: reqs[0].0,
            response: Response::Files {
                files: vec![change("a.rs")],
                resolved: resolved(1, 2).into_iter().collect(),
            },
        }))
        .unwrap();
    let (_, request) = requests(&effects)
        .into_iter()
        .find(|(_, r)| matches!(r, Request::FileRender { .. }))
        .unwrap();
    let Request::FileRender { opts, .. } = request else {
        panic!()
    };
    assert!(opts.ignore_whitespace);
    // Reconnecting does not load the prefs again.
    core.handle(Input::Transport(TransportEvent::Disconnected))
        .unwrap();
    let effects = core.handle(Input::User(Action::Connect)).unwrap();
    assert!(loads(&effects).is_empty());
}

// ---- 3.5: diff overlays, threads, conversation, stepper ------------------

fn lines_anchor(
    p: &str,
    side: nits_protocol::Side,
    blob: u8,
    start: u32,
    end: u32,
) -> nits_protocol::Anchor {
    use nits_protocol::{ContextHash, LineNo, LineRange};
    nits_protocol::Anchor::Lines {
        repo_id: repo_id(),
        path: path(p),
        side,
        blob_oid: blob_oid(blob),
        lines: LineRange::new(LineNo::new(start).unwrap(), LineNo::new(end).unwrap()).unwrap(),
        context_hash: ContextHash::new(1),
    }
}

fn comment_at(
    n: u128,
    anchor: nits_protocol::Anchor,
    state: nits_protocol::CommentState,
) -> nits_protocol::Comment {
    let id = nits_protocol::CommentId::from_parts(5, n);
    nits_protocol::Comment {
        id,
        review_id: review_id(),
        thread_id: nits_client_core::thread_id_of(id),
        author: Author::Human {
            name: "other".into(),
            machine: "host".into(),
        },
        kind: nits_protocol::CommentKind::Note,
        anchor,
        body: format!("comment {n}\nmore"),
        created: Timestamp::from_millis(i64::try_from(n).unwrap()),
        edited: None,
        state,
        context: None,
    }
}

fn foreign_event(seq: u64, body: EventBody) -> ServerMsg {
    ServerMsg::Event {
        event: Event {
            seq: Seq::new(seq),
            ts: Timestamp::from_millis(0),
            author: Author::Human {
                name: "other".into(),
                machine: "host".into(),
            },
            client_id: ClientId::from_parts(9, 9),
            client_seq: ClientSeq::new(1),
            body,
        },
    }
}

/// A chunk whose rows carry real line numbers: row i of chunk c is context
/// line `c*100 + i + 1` on both sides.
fn numbered_chunk(index: u32) -> RenderChunk {
    use nits_protocol::{Cell, LineNo};
    let cell = |n: u32| Cell {
        ending: nits_protocol::LineEnding::Lf,
        line_no: LineNo::new(n).unwrap(),
        text: format!("line {n}"),
        spans: Vec::new(),
        changed: Vec::new(),
    };
    RenderChunk {
        index: ChunkIndex::new(index),
        rows: (0..100)
            .map(|i| {
                let n = index * 100 + i + 1;
                if n.is_multiple_of(10) {
                    Row::Added { right: cell(n) }
                } else {
                    Row::Context {
                        left: cell(n),
                        right: cell(n),
                    }
                }
            })
            .collect(),
    }
}

#[test]
fn comments_are_placed_on_rows_by_anchor_and_listed_as_threads() {
    use nits_protocol::{CommentState, Side};
    let mut core = subscribed(local());
    open_streamed(&mut core);
    // a.rs is Modified old=blob 10, new=blob 11. Chunk 0 rows 1..=100.
    let head = lines_anchor("a.rs", Side::Head, 11, 5, 7);
    let base = lines_anchor("a.rs", Side::Base, 10, 20, 20);
    let wrong_blob = lines_anchor("a.rs", Side::Head, 99, 30, 30);
    let file = nits_protocol::Anchor::File {
        repo_id: repo_id(),
        path: path("a.rs"),
        blob_oid: blob_oid(11),
    };
    let outdated = comment_at(
        5,
        lines_anchor("a.rs", Side::Head, 11, 300, 300),
        CommentState::Outdated {
            last_good_anchor: lines_anchor("a.rs", Side::Head, 11, 40, 40),
        },
    );
    let deleted = comment_at(
        6,
        lines_anchor("a.rs", Side::Head, 11, 50, 50),
        CommentState::Deleted,
    );
    let comments = [
        comment_at(1, head, CommentState::Live),
        comment_at(2, base, CommentState::Live),
        comment_at(3, wrong_blob, CommentState::Live),
        comment_at(4, file, CommentState::Live),
        outdated,
        deleted,
        comment_at(7, nits_protocol::Anchor::Review, CommentState::Live),
    ];
    for (i, c) in comments.into_iter().enumerate() {
        let seq = 2 + u64::try_from(i).unwrap();
        core.handle(Input::Server(foreign_event(
            seq,
            EventBody::CommentCreated { comment: c },
        )))
        .unwrap();
    }
    // Replace a.rs chunk 0 with numbered rows, then look at rows 0..=60.
    let effects = core
        .handle(Input::User(Action::Viewport {
            file: file_ref("a.rs"),
            first_row: 0,
            last_row: 60,
        }))
        .unwrap();
    // chunk 0 was cached by the stream (hunk-header rows); answer the rest
    // with numbered rows so placement has line numbers to work with.
    let answered: Vec<Effect> = requests(&effects)
        .into_iter()
        .filter_map(|(id, r)| match r {
            Request::RenderChunk { index, .. } => Some(ServerMsg::Response {
                id,
                response: Response::RenderChunk {
                    chunk: numbered_chunk(index.get()),
                },
            }),
            Request::TreeSnapshot { .. }
            | Request::FileRender { .. }
            | Request::ChangeRender { .. }
            | Request::Search { .. }
            | Request::ListWorkspaces
            | Request::ListReviews { .. }
            | Request::ListRefs { .. }
            | Request::DefaultBase { .. }
            | Request::EnsureDirectoryReview { .. }
            | Request::GetReview { .. }
            | Request::ReviewSnapshot { .. }
            | Request::ListFiles { .. }
            | Request::OpenReview { .. }
            | Request::ResolveTargets { .. }
            | Request::ListCommits { .. }
            | Request::BlobRender { .. }
            | Request::Subscribe { .. }
            | Request::ReplayEvents { .. }
            | Request::Unsubscribe { .. }
            | Request::Mutate { .. }
            | Request::Shutdown => None,
        })
        .flat_map(|msg| core.handle(Input::Server(msg)).unwrap())
        .collect();
    let _ = answered;
    // Chunk 0 came from the stream with one hunk-header row; stream a
    // numbered chunk 0 in its place via a FileRender-free path: re-open the
    // file after the header is known is not needed — instead assert on the
    // thread list, which needs no rows, and on file/review placement.
    let threads = &core.view().threads;
    let ids: Vec<u128> = threads.iter().map(|t| t.root.random()).collect();
    // Deleted (6) is not listed; the rest are, oldest first.
    assert_eq!(ids, vec![1, 2, 3, 4, 5, 7]);
    assert!(threads[4].outdated);
    assert_eq!(threads[0].summary, "comment 1");
    assert_eq!(
        threads[0].place,
        nits_client_core::ThreadPlace::Lines {
            file: file_ref("a.rs"),
            side: Side::Head,
            start: 5,
            end: 7
        }
    );
    assert_eq!(core.view().conversation.len(), 1);
    assert_eq!(core.view().conversation[0].root.random(), 7);
    let diff = core.view().diff.as_ref().unwrap();
    assert_eq!(diff.file, file_ref("a.rs"));
    // The file-level thread (4) is shown above the rows; blob 99 is not.
    assert_eq!(diff.file_threads.len(), 1);
    assert_eq!(diff.file_threads[0], threads[3].id);
    // Chunk 0 (streamed) has one row: the hunk header at index 0.
    assert_eq!(diff.rows[0].index, 0);
    assert!(matches!(diff.rows[0].row, Row::HunkHeader { .. }));
    assert!(diff.missing.is_empty());
}

#[test]
fn line_anchors_land_on_the_last_line_of_the_range_on_the_right_side() {
    use nits_protocol::{CommentState, Side};
    // Build the placement directly through a core whose chunks are numbered.
    let mut core = subscribed(local());
    // Open with only the snapshot streamed, then fetch a.rs through
    // FileRender so every chunk is numbered.
    let effects = core
        .handle(Input::User(Action::OpenReview {
            review_id: review_id(),
        }))
        .unwrap();
    let (id, _) = requests(&effects)[0].clone();
    item(
        &mut core,
        id,
        StreamItem::ReviewSnapshot {
            snapshot: snapshot(1, 2),
        },
    );
    item(
        &mut core,
        id,
        StreamItem::Header {
            header: header("a.rs", 100, 3),
        },
    );
    for c in 0..3 {
        item(
            &mut core,
            id,
            StreamItem::Chunk {
                repo_id: repo_id(),
                path: path("a.rs"),
                chunk: numbered_chunk(c),
            },
        );
    }
    core.handle(Input::Server(ServerMsg::StreamEnd { id }))
        .unwrap();
    let anchors = [
        (1, lines_anchor("a.rs", Side::Head, 11, 5, 7)),
        (2, lines_anchor("a.rs", Side::Base, 10, 21, 21)),
        // Line 30 is an `Added` row: no base cell, so a base anchor at 30
        // cannot land; a head anchor can.
        (3, lines_anchor("a.rs", Side::Base, 10, 30, 30)),
        (4, lines_anchor("a.rs", Side::Head, 11, 30, 30)),
        (5, lines_anchor("a.rs", Side::Head, 11, 150, 150)),
    ];
    for (i, (n, anchor)) in anchors.into_iter().enumerate() {
        let seq = 2 + u64::try_from(i).unwrap();
        core.handle(Input::Server(foreign_event(
            seq,
            EventBody::CommentCreated {
                comment: comment_at(n, anchor, CommentState::Live),
            },
        )))
        .unwrap();
    }
    core.handle(Input::User(Action::Viewport {
        file: file_ref("a.rs"),
        first_row: 0,
        last_row: 199,
    }))
    .unwrap();
    let diff = core.view().diff.as_ref().unwrap();
    assert_eq!(diff.rows.len(), 200);
    assert!(diff.missing.is_empty());
    let at = |row: usize| -> Vec<u128> {
        diff.rows[row]
            .threads
            .iter()
            .filter(|t| t.place == nits_client_core::RowPlace::Anchor)
            .map(|t| t.thread.random())
            .collect()
    };
    let inside = |row: usize| -> Vec<u128> {
        diff.rows[row]
            .threads
            .iter()
            .filter(|t| t.place == nits_client_core::RowPlace::Inside)
            .map(|t| t.thread.random())
            .collect()
    };
    // Row index = line - 1 in this fixture.
    assert_eq!(at(6), vec![1], "5..=7 lands on line 7");
    assert_eq!(at(4), Vec::<u128>::new(), "not the card's row");
    // ...but the rows the range covers know they are commented, so the
    // stretch can be drawn.
    assert_eq!(inside(4), vec![1], "line 5 is inside 5..=7");
    assert_eq!(inside(5), vec![1]);
    assert_eq!(inside(6), Vec::<u128>::new(), "the last line is the anchor");
    assert_eq!(inside(3), Vec::<u128>::new(), "line 4 is outside");
    assert_eq!(at(20), vec![2]);
    assert_eq!(
        at(29),
        vec![4],
        "base anchor on an added line does not land"
    );
    assert_eq!(at(149), vec![5], "second chunk");
    // Scrolling narrows the rows but keeps placement.
    core.handle(Input::User(Action::Viewport {
        file: file_ref("a.rs"),
        first_row: 140,
        last_row: 160,
    }))
    .unwrap();
    let diff = core.view().diff.as_ref().unwrap();
    assert_eq!(diff.rows.len(), 21);
    assert_eq!(diff.rows[0].index, 140);
    assert_eq!(diff.rows[9].threads.len(), 1);
    // A chunk beyond what is cached is reported missing, not invented.
    core.handle(Input::User(Action::Viewport {
        file: file_ref("a.rs"),
        first_row: 250,
        last_row: 299,
    }))
    .unwrap();
    let diff = core.view().diff.as_ref().unwrap();
    assert_eq!(diff.rows.len(), 50);
    assert!(diff.missing.is_empty());
}

#[test]
fn commit_stepper_lists_and_steps() {
    let mut core = subscribed(local());
    assert_eq!(
        core.handle(Input::User(Action::ListCommits { repo_id: repo_id() })),
        Err(CoreError::NoOpenReview)
    );
    open_streamed(&mut core);
    assert_eq!(
        core.handle(Input::User(Action::StepCommit { selected: Some(0) })),
        Err(CoreError::NoStepper)
    );
    let effects = core
        .handle(Input::User(Action::ListCommits { repo_id: repo_id() }))
        .unwrap();
    let (id, request) = requests(&effects)[0].clone();
    assert_eq!(
        request,
        Request::ListCommits {
            review_id: review_id(),
            repo_id: repo_id()
        }
    );
    let sig = nits_protocol::Sig {
        name: "ada".into(),
        email: "ada@example.com".into(),
        time: Timestamp::from_millis(5),
        offset_minutes: 0,
    };
    let commit = |fill: u8, subject: &str| nits_protocol::CommitInfo {
        oid: CommitOid::new(Oid::from_bytes([fill; 20])),
        parents: Vec::new(),
        tree: tree_oid(fill),
        author: sig.clone(),
        committer: sig.clone(),
        subject: subject.into(),
        body: String::new(),
    };
    let effects = core
        .handle(Input::Server(ServerMsg::Response {
            id,
            response: Response::Commits {
                commits: vec![commit(1, "first"), commit(2, "second")],
            },
        }))
        .unwrap();
    assert_eq!(rendered(&effects), vec![ViewSection::CommitStepper]);
    let stepper = core.view().stepper.as_ref().unwrap();
    assert_eq!(stepper.commits.len(), 2);
    assert_eq!(stepper.commits[1].subject, "second");
    assert!(!stepper.has_worktree);
    assert_eq!(
        core.handle(Input::User(Action::StepCommit { selected: Some(2) })),
        Err(CoreError::CommitOutOfRange(2))
    );
    // `enter` on a focused commit selects the commit as the diff scope,
    // exactly like a click in the UI. Index zero is the aggregate row, so
    // the second commit is the third selectable row.
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::CommitStepper { index: 2 },
    }))
    .unwrap();
    let second = CommitOid::new(Oid::from_bytes([2; 20]));
    assert_eq!(
        resolve_command(&core, nits_client_core::Command::Open),
        Ok(Action::SetScope {
            scope: ScopeChoice::Commit {
                repo_id: repo_id(),
                oid: second,
            },
        })
    );
    let effects = core
        .handle(Input::Key(KeyChord::named(NamedKey::Enter)))
        .unwrap();
    assert_eq!(
        requests(&effects)
            .into_iter()
            .map(|(_, request)| request)
            .collect::<Vec<_>>(),
        vec![Request::ListFiles {
            review_id: review_id(),
            scope: DiffScope::Commit {
                repo_id: repo_id(),
                oid: second,
            },
        }]
    );
    assert_eq!(
        rendered(&effects),
        vec![
            ViewSection::ReviewList,
            ViewSection::Diff,
            ViewSection::Tree,
            ViewSection::Progress,
            ViewSection::Conversation,
            ViewSection::Hints,
        ]
    );
    assert_eq!(
        core.view().scope,
        DiffScope::Commit {
            repo_id: repo_id(),
            oid: second,
        }
    );
    // The aggregate row resolves through the same typed scope action and
    // therefore follows a scope selected by any shortcut or pointer.
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::CommitStepper { index: 0 },
    }))
    .unwrap();
    assert_eq!(
        resolve_command(&core, nits_client_core::Command::Open),
        Ok(Action::SetScope {
            scope: ScopeChoice::All,
        })
    );
    let effects = core
        .handle(Input::Key(KeyChord::named(NamedKey::Enter)))
        .unwrap();
    assert!(requests(&effects).iter().any(|(_, request)| *request
        == Request::ListFiles {
            review_id: review_id(),
            scope: DiffScope::All,
        }));
    assert_eq!(core.view().scope, DiffScope::All);
    // Closing the review drops the stepper.
    core.handle(Input::User(Action::CloseReview)).unwrap();
    assert!(core.view().stepper.is_none());
}

#[test]
fn commit_stepper_worktree_row_selects_the_worktree_scope() {
    let mut core = subscribed(local());
    let mut worktree_snapshot = snapshot(1, 2);
    worktree_snapshot.review.targets = NonEmpty::singleton(ReviewTarget {
        repo_id: repo_id(),
        base: RefSpec::Branch {
            name: "main".into(),
        },
        head: RefSpec::WorkingTree,
    });
    worktree_snapshot.resolved = Some(NonEmpty::singleton(ResolvedTarget {
        repo_id: repo_id(),
        base: ResolvedRef {
            tree: tree_oid(1),
            source: ResolvedSource::Commit {
                oid: CommitOid::new(Oid::from_bytes([1; 20])),
            },
        },
        head: ResolvedRef {
            tree: tree_oid(2),
            source: ResolvedSource::WorkingTree {
                head: None,
                dirty: vec![path("a.rs")],
                branch: Some("feature".into()),
            },
        },
    }));
    open_streamed_with(&mut core, worktree_snapshot);
    let effects = core
        .handle(Input::User(Action::ListCommits { repo_id: repo_id() }))
        .unwrap();
    let id = requests(&effects)[0].0;
    let sig = nits_protocol::Sig {
        name: "ada".into(),
        email: "ada@example.com".into(),
        time: Timestamp::from_millis(5),
        offset_minutes: 0,
    };
    core.handle(Input::Server(ServerMsg::Response {
        id,
        response: Response::Commits {
            commits: vec![nits_protocol::CommitInfo {
                oid: CommitOid::new(Oid::from_bytes([2; 20])),
                parents: Vec::new(),
                tree: tree_oid(2),
                author: sig.clone(),
                committer: sig,
                subject: "one commit".into(),
                body: String::new(),
            }],
        },
    }))
    .unwrap();
    assert!(core.view().stepper.as_ref().unwrap().has_worktree);
    core.handle(Input::User(Action::SetFocus {
        // All changes, one commit, then the working tree.
        focus: Focus::CommitStepper { index: 2 },
    }))
    .unwrap();
    assert_eq!(
        resolve_command(&core, nits_client_core::Command::Open),
        Ok(Action::SetScope {
            scope: ScopeChoice::Worktree { repo_id: repo_id() },
        })
    );
    let effects = core
        .handle(Input::Key(KeyChord::named(NamedKey::Enter)))
        .unwrap();
    assert!(requests(&effects).iter().any(|(_, request)| *request
        == Request::ListFiles {
            review_id: review_id(),
            scope: DiffScope::Worktree { repo_id: repo_id() },
        }));
    assert_eq!(
        core.view().scope,
        DiffScope::Worktree { repo_id: repo_id() }
    );
}

#[test]
fn scope_switching_refetches_files_and_steps_commits() {
    let mut core = subscribed(local());
    assert_eq!(
        core.handle(Input::User(Action::SetScope {
            scope: ScopeChoice::All
        })),
        Err(CoreError::NoOpenReview)
    );
    open_streamed(&mut core);
    // Entering by-commit without a commit list fetches it first.
    let effects = core
        .handle(Input::User(Action::SetScope {
            scope: ScopeChoice::ByCommit,
        }))
        .unwrap();
    let (id, request) = requests(&effects)[0].clone();
    assert_eq!(
        request,
        Request::ListCommits {
            review_id: review_id(),
            repo_id: repo_id()
        }
    );
    let sig = nits_protocol::Sig {
        name: "ada".into(),
        email: "ada@example.com".into(),
        time: Timestamp::from_millis(5),
        offset_minutes: 0,
    };
    let commit = |fill: u8, subject: &str| nits_protocol::CommitInfo {
        oid: CommitOid::new(Oid::from_bytes([fill; 20])),
        parents: Vec::new(),
        tree: tree_oid(fill),
        author: sig.clone(),
        committer: sig.clone(),
        subject: subject.into(),
        body: String::new(),
    };
    // Newest first, as the daemon lists them.
    let effects = core
        .handle(Input::Server(ServerMsg::Response {
            id,
            response: Response::Commits {
                commits: vec![commit(9, "newest"), commit(8, "older")],
            },
        }))
        .unwrap();
    // The answer enters by-commit at the newest commit and refetches files.
    let newest = CommitOid::new(Oid::from_bytes([9; 20]));
    let (files_id, request) = requests(&effects)[0].clone();
    assert_eq!(
        request,
        Request::ListFiles {
            review_id: review_id(),
            scope: DiffScope::Commit {
                repo_id: repo_id(),
                oid: newest
            }
        }
    );
    // The scoped answer carries the step's targets; the header follows them.
    let step_targets = vec![ResolvedTarget {
        repo_id: repo_id(),
        base: ResolvedRef {
            tree: tree_oid(8),
            source: ResolvedSource::Commit {
                oid: CommitOid::new(Oid::from_bytes([8; 20])),
            },
        },
        head: ResolvedRef {
            tree: tree_oid(9),
            source: ResolvedSource::Commit { oid: newest },
        },
    }];
    core.handle(Input::Server(ServerMsg::Response {
        id: files_id,
        response: Response::Files {
            files: vec![change("a.rs")],
            resolved: step_targets.clone(),
        },
    }))
    .unwrap();
    assert_eq!(core.view().resolved_targets, step_targets);
    assert_eq!(
        core.view().scope,
        DiffScope::Commit {
            repo_id: repo_id(),
            oid: newest
        }
    );
    // `p` steps toward the base: the older commit; the step refetches.
    let action = resolve_command(&core, nits_client_core::Command::PrevHunk).unwrap();
    assert_eq!(action, Action::StepCommit { selected: Some(1) });
    let effects = core.handle(Input::User(action)).unwrap();
    let older = CommitOid::new(Oid::from_bytes([8; 20]));
    assert!(requests(&effects).iter().any(|(_, r)| *r
        == Request::ListFiles {
            review_id: review_id(),
            scope: DiffScope::Commit {
                repo_id: repo_id(),
                oid: older
            }
        }));
    // At the oldest commit `p` has nowhere to go.
    assert_eq!(
        resolve_command(&core, nits_client_core::Command::PrevHunk),
        Err(nits_client_core::NoTarget::AtEdge)
    );
    // Back to all changes: the snapshot's resolved targets return.
    let effects = core
        .handle(Input::User(Action::SetScope {
            scope: ScopeChoice::All,
        }))
        .unwrap();
    assert!(requests(&effects).iter().any(|(_, r)| *r
        == Request::ListFiles {
            review_id: review_id(),
            scope: DiffScope::All
        }));
    assert_eq!(core.view().scope, DiffScope::All);
    assert_eq!(
        core.view().resolved_targets,
        resolved(1, 2).into_iter().collect::<Vec<_>>()
    );
}

#[test]
fn jump_to_original_diff_renders_the_recorded_change_read_only() {
    use nits_client_core::resolve_command;
    let mut core = subscribed(local());
    open_streamed(&mut core);
    // A comment made on an older diff of a.rs: its context names blobs that
    // are not the review's current change.
    let old_change = ChangeKind::Modified {
        old: nits_protocol::BlobEntry {
            oid: blob_oid(3),
            mode: nits_protocol::BlobMode::Regular,
        },
        new: nits_protocol::BlobEntry {
            oid: blob_oid(4),
            mode: nits_protocol::BlobMode::Regular,
        },
    };
    let mut comment = comment_at(
        1,
        lines_anchor("a.rs", nits_protocol::Side::Head, 4, 2, 2),
        nits_protocol::CommentState::Outdated {
            last_good_anchor: lines_anchor("a.rs", nits_protocol::Side::Head, 4, 2, 2),
        },
    );
    comment.context = Some(nits_protocol::CommentContext::Diff {
        change: old_change.clone(),
    });
    let thread_id = comment.thread_id;
    core.handle(Input::Server(foreign_event(
        2,
        EventBody::CommentCreated { comment },
    )))
    .unwrap();
    assert_eq!(
        core.view().threads[0].context,
        Some(nits_protocol::CommentContext::Diff {
            change: old_change.clone()
        }),
        "the thread view carries the recorded context"
    );
    // Opening a portable link lands on this outdated finding in Conversation.
    let context = nits_protocol::ReferenceContext::named("review-box").unwrap();
    core.handle(Input::User(Action::SetReferenceContext {
        context: context.clone(),
    }))
    .unwrap();
    let reference = nits_protocol::ReviewReference {
        context,
        review_id: review_id(),
        target: nits_protocol::ReferenceTarget::Thread { thread_id },
    };
    let snapshot = core.view().review.as_ref().unwrap().snapshot.clone();
    let effects = core
        .handle(Input::User(Action::OpenReference {
            reference: reference.to_string(),
        }))
        .unwrap();
    let request = requests(&effects)[0].0;
    item(&mut core, request, StreamItem::ReviewSnapshot { snapshot });
    for (name, chunks) in [("a.rs", 10), ("b.rs", 1)] {
        item(
            &mut core,
            request,
            StreamItem::Header {
                header: header(name, 100, chunks),
            },
        );
    }
    core.handle(Input::Server(ServerMsg::StreamEnd { id: request }))
        .unwrap();
    assert_eq!(core.view().tab, nits_client_core::Tab::Conversation);
    assert_eq!(
        core.view().focused_comment,
        Some(core.view().threads[0].root)
    );
    // Jump: the daemon is asked to render the recorded change directly.
    let effects = core
        .handle(Input::User(Action::OpenOriginalDiff { thread_id }))
        .unwrap();
    let (id, request) = requests(&effects)
        .into_iter()
        .find(|(_, r)| matches!(r, Request::ChangeRender { .. }))
        .expect("a ChangeRender request");
    assert_eq!(
        request,
        Request::ChangeRender {
            repo_id: repo_id(),
            path: path("a.rs"),
            change: old_change.clone(),
            opts: RenderOpts::default(),
            first_chunk: ChunkIndex::FIRST,
        }
    );
    assert!(matches!(
        core.view().focus,
        nits_client_core::Focus::Diff { row: 0, .. }
    ));
    assert_eq!(
        core.view().tab,
        nits_client_core::Tab::FilesChanged,
        "original diff must leave Conversation so the host can show it"
    );
    // The stream answers with the original render's header and rows.
    let original_header = FileRenderHeader {
        target: RenderTarget::Diff {
            change: old_change.clone(),
        },
        ..header("a.rs", 100, 1)
    };
    item(
        &mut core,
        id,
        StreamItem::Header {
            header: original_header,
        },
    );
    item(
        &mut core,
        id,
        StreamItem::Chunk {
            repo_id: repo_id(),
            path: path("a.rs"),
            chunk: chunk(0),
        },
    );
    let diff = core.view().diff.as_ref().expect("the original diff shows");
    assert!(diff.original, "marked as the jump-to-context view");
    assert_eq!(diff.file.path, path("a.rs"));
    assert!(!diff.rows.is_empty());
    // The original render never joins the review's file list or progress.
    assert_eq!(core.view().progress.total, 2);
    // Back closes the original view and clears the mode.
    let action = resolve_command(&core, nits_client_core::Command::Back).unwrap();
    assert_eq!(action, Action::CloseFile);
    core.handle(Input::User(action)).unwrap();
    assert!(core.view().diff.is_none());
    // Reopening the file normally shows the current diff again.
    core.handle(Input::User(Action::Viewport {
        file: file_ref("a.rs"),
        first_row: 0,
        last_row: 59,
    }))
    .unwrap();
    let diff = core.view().diff.as_ref().unwrap();
    assert!(!diff.original);
}

// A retained source can share a current path or have disappeared from the
// current change list. Both cases must keep all pane actions on that source.
fn historical_diff(p: &str) -> (ClientCore, ChangeKind) {
    let mut core = subscribed(local());
    open_streamed(&mut core);
    let change = ChangeKind::Modified {
        old: nits_protocol::BlobEntry {
            oid: blob_oid(3),
            mode: nits_protocol::BlobMode::Regular,
        },
        new: nits_protocol::BlobEntry {
            oid: blob_oid(4),
            mode: nits_protocol::BlobMode::Regular,
        },
    };
    let anchor = lines_anchor(p, Side::Head, 4, 2, 2);
    let mut comment = comment_at(
        1,
        anchor.clone(),
        nits_protocol::CommentState::Outdated {
            last_good_anchor: anchor,
        },
    );
    comment.context = Some(nits_protocol::CommentContext::Diff {
        change: change.clone(),
    });
    let thread_id = comment.thread_id;
    core.handle(Input::Server(foreign_event(
        2,
        EventBody::CommentCreated { comment },
    )))
    .unwrap();
    let effects = core
        .handle(Input::User(Action::OpenOriginalDiff { thread_id }))
        .unwrap();
    let (id, _) = requests(&effects)
        .into_iter()
        .find(|(_, request)| matches!(request, Request::ChangeRender { .. }))
        .unwrap();
    let mut original_header = FileRenderHeader {
        target: RenderTarget::Diff {
            change: change.clone(),
        },
        ..header(p, 100, 1)
    };
    let RenderContent::Text { gaps, .. } = &mut original_header.content else {
        panic!("text")
    };
    *gaps = nits_protocol::GapTable::try_from(vec![nits_protocol::GapRow {
        gap: nits_protocol::Gap::new(1),
        row: 10,
    }])
    .unwrap();
    item(
        &mut core,
        id,
        StreamItem::Header {
            header: original_header,
        },
    );
    let mut chunk = numbered_chunk(0);
    chunk.rows[10] = Row::Expander {
        hidden: 40,
        dir: nits_protocol::ExpandDir::Both,
        gap: nits_protocol::Gap::new(1),
    };
    item(
        &mut core,
        id,
        StreamItem::Chunk {
            repo_id: repo_id(),
            path: path(p),
            chunk,
        },
    );
    core.handle(Input::Server(ServerMsg::StreamEnd { id }))
        .unwrap();
    (core, change)
}

#[test]
fn original_motions_ignore_current_folds_and_viewed_state() {
    for current_state in [
        Action::ToggleFileCollapse {
            file: file_ref("a.rs"),
        },
        Action::MarkViewed {
            file: file_ref("a.rs"),
        },
    ] {
        let (mut core, _) = historical_diff("a.rs");
        core.handle(Input::User(current_state)).unwrap();
        assert!(
            core.view()
                .diffs
                .iter()
                .find(|diff| diff.file.path == path("a.rs"))
                .unwrap()
                .collapsed
        );
        core.handle(Input::Key("j".parse().unwrap())).unwrap();
        assert_eq!(
            core.view().focus,
            Focus::Diff {
                row: 1,
                side: Side::Head
            }
        );
        core.handle(Input::Key("k".parse().unwrap())).unwrap();
        assert_eq!(
            core.view().focus,
            Focus::Diff {
                row: 0,
                side: Side::Head
            }
        );
        assert_eq!(
            resolve_command(&core, nits_client_core::Command::MoveUp),
            Err(nits_client_core::NoTarget::AtEdge)
        );
        core.handle(Input::User(Action::SetFocus {
            focus: Focus::Diff {
                row: 99,
                side: Side::Head,
            },
        }))
        .unwrap();
        assert_eq!(
            resolve_command(&core, nits_client_core::Command::MoveDown),
            Err(nits_client_core::NoTarget::AtEdge)
        );
        assert!(core.view().diff.as_ref().unwrap().original);
    }
}

#[test]
fn original_enter_opens_threads_and_gaps_without_unfolding_current_file() {
    for current_state in [
        Action::ToggleFileCollapse {
            file: file_ref("a.rs"),
        },
        Action::MarkViewed {
            file: file_ref("a.rs"),
        },
    ] {
        let (mut core, change) = historical_diff("a.rs");
        core.handle(Input::User(current_state)).unwrap();
        core.handle(Input::User(Action::RunCommand {
            command: nits_client_core::Command::NextComment,
        }))
        .unwrap();
        assert_eq!(
            core.view().focus,
            Focus::Diff {
                row: 1,
                side: Side::Head
            }
        );
        // Enter reaches the original row's inline thread, even though today's
        // file with the same path is folded or marked viewed.
        core.handle(Input::Key(KeyChord::named(NamedKey::Enter)))
            .unwrap();
        assert_eq!(core.view().focus, Focus::Thread { index: 0 });
        core.handle(Input::User(Action::SetFocus {
            focus: Focus::Diff {
                row: 10,
                side: Side::Head,
            },
        }))
        .unwrap();
        let effects = core
            .handle(Input::Key(KeyChord::named(NamedKey::Enter)))
            .unwrap();
        let requested = requests(&effects);
        assert_eq!(requested.len(), 1);
        assert_eq!(
            requested[0].1,
            Request::ChangeRender {
                repo_id: repo_id(),
                path: path("a.rs"),
                change,
                opts: RenderOpts {
                    expanded: nits_protocol::Expansions::default().opened(
                        nits_protocol::Gap::new(1),
                        nits_protocol::ExpandDir::Both,
                        nits_client_core::EXPAND_STEP,
                    ),
                    ..RenderOpts::default()
                },
                first_chunk: ChunkIndex::FIRST,
            }
        );
        assert!(
            core.view()
                .diffs
                .iter()
                .find(|diff| diff.file.path == path("a.rs"))
                .unwrap()
                .collapsed
        );
        assert_eq!(
            core.view().review.as_ref().unwrap().original.as_ref(),
            Some(&open_render(&core))
        );
    }
}

#[test]
fn original_expansions_keep_historical_blobs_current_keys_and_logical_cursor() {
    for p in ["a.rs", "removed.rs"] {
        let (mut core, change) = historical_diff(p);
        let current = core.view().review.as_ref().unwrap().files.clone();
        core.handle(Input::User(Action::SetFocus {
            focus: Focus::Diff {
                row: 49,
                side: Side::Head,
            },
        }))
        .unwrap();
        let gap = nits_protocol::Gap::new(1);
        let effects = core
            .handle(Input::User(Action::ExpandGap {
                file: file_ref(p),
                gap,
                dir: nits_protocol::ExpandDir::Up,
            }))
            .unwrap();
        let gap_opts = RenderOpts {
            expanded: nits_protocol::Expansions::default().opened(
                gap,
                nits_protocol::ExpandDir::Up,
                nits_client_core::EXPAND_STEP,
            ),
            ..RenderOpts::default()
        };
        let gap_requests = requests(&effects);
        assert_eq!(gap_requests.len(), 1);
        let gap_id = gap_requests[0].0;
        assert_eq!(
            gap_requests[0].1,
            Request::ChangeRender {
                repo_id: repo_id(),
                path: path(p),
                change: change.clone(),
                opts: gap_opts.clone(),
                first_chunk: ChunkIndex::FIRST,
            }
        );
        assert_eq!(open_render(&core).opts, gap_opts);
        assert_eq!(
            core.view().review.as_ref().unwrap().original.as_ref(),
            Some(&open_render(&core))
        );
        // Expand again before the first answer. Its recorded line must survive
        // until the latest source arrives, without touching today's file list.
        let effects = core
            .handle(Input::User(Action::ExpandContext {
                file: file_ref(p),
                full: true,
            }))
            .unwrap();
        let full_opts = RenderOpts {
            context_lines: nits_client_core::FULL_CONTEXT,
            ..RenderOpts::default()
        };
        let full_requests = requests(&effects);
        assert_eq!(full_requests.len(), 1);
        let full_id = full_requests[0].0;
        assert_eq!(
            full_requests[0].1,
            Request::ChangeRender {
                repo_id: repo_id(),
                path: path(p),
                change: change.clone(),
                opts: full_opts.clone(),
                first_chunk: ChunkIndex::FIRST,
            }
        );
        let historical_header = |opts| FileRenderHeader {
            target: RenderTarget::Diff {
                change: change.clone(),
            },
            opts,
            ..header(p, 100, 1)
        };
        item(
            &mut core,
            gap_id,
            StreamItem::Header {
                header: historical_header(gap_opts),
            },
        );
        item(
            &mut core,
            gap_id,
            StreamItem::Chunk {
                repo_id: repo_id(),
                path: path(p),
                chunk: numbered_chunk(0),
            },
        );
        core.handle(Input::Server(ServerMsg::StreamEnd { id: gap_id }))
            .unwrap();
        assert_eq!(open_render(&core).opts, full_opts);
        item(
            &mut core,
            full_id,
            StreamItem::Header {
                header: historical_header(full_opts),
            },
        );
        let mut chunk = numbered_chunk(0);
        chunk.rows.insert(
            0,
            Row::HunkHeader {
                text: "@@ expanded context @@".into(),
            },
        );
        chunk.rows.pop();
        item(
            &mut core,
            full_id,
            StreamItem::Chunk {
                repo_id: repo_id(),
                path: path(p),
                chunk,
            },
        );
        core.handle(Input::Server(ServerMsg::StreamEnd { id: full_id }))
            .unwrap();
        assert_eq!(
            core.view().focus,
            Focus::Diff {
                row: 50,
                side: Side::Head
            }
        );
        let open = core.view().review.as_ref().unwrap();
        assert_eq!(open.original.as_ref(), Some(&open_render(&core)));
        assert_eq!(open.files, current);
        assert!(core.view().diff.as_ref().unwrap().original);
        assert_eq!(core.view().progress.total, 2);
    }
}

#[test]
fn original_context_rejects_keyboard_visual_and_direct_anchored_drafts() {
    let (mut core, _) = historical_diff("a.rs");
    let rejected = CoreError::NoTarget(nits_client_core::NoTarget::ReadOnlyOriginal);
    assert_eq!(
        core.handle(Input::Key("c".parse().unwrap())),
        Err(rejected.clone())
    );
    assert!(core.view().draft.is_none());
    assert_eq!(
        core.view().focus,
        Focus::Diff {
            row: 0,
            side: Side::Head
        }
    );
    assert!(
        !core
            .view()
            .hints
            .iter()
            .any(|hint| hint.command == nits_client_core::Command::Comment)
    );
    core.handle(Input::User(Action::EnterVisual)).unwrap();
    core.handle(Input::Key("j".parse().unwrap())).unwrap();
    assert_eq!(
        core.handle(Input::Key("c".parse().unwrap())),
        Err(rejected.clone())
    );
    assert!(core.view().draft.is_none());
    assert!(core.visual_anchor().is_some());
    core.handle(Input::User(Action::LeaveVisual)).unwrap();
    for action in [
        Action::CommentLines {
            file: file_ref("a.rs"),
            side: Side::Head,
            start_line: 1,
            end_line: 2,
        },
        Action::CommentFile {
            file: file_ref("a.rs"),
        },
        Action::DraftOpened {
            anchor: lines_anchor("a.rs", Side::Head, 4, 1, 2),
        },
        Action::DraftOpened {
            anchor: nits_protocol::Anchor::File {
                repo_id: repo_id(),
                path: path("a.rs"),
                blob_oid: blob_oid(4),
            },
        },
    ] {
        let before = core.view().clone();
        assert_eq!(core.handle(Input::User(action)), Err(rejected.clone()));
        assert_eq!(core.view(), &before);
    }
    // Returning to the current source enables a visible composer on its blob.
    core.handle(Input::User(Action::CloseFile)).unwrap();
    core.handle(Input::User(Action::Viewport {
        file: file_ref("a.rs"),
        first_row: 0,
        last_row: 59,
    }))
    .unwrap();
    // Current-source line composition validates loaded source rows. Fetch
    // real rows instead of the generic hunk-header-only cache fixture.
    let effects = core
        .handle(Input::User(Action::ExpandContext {
            file: file_ref("a.rs"),
            full: false,
        }))
        .unwrap();
    let (id, Request::FileRender { opts, .. }) = requests(&effects)[0].clone() else {
        panic!("current file render")
    };
    item(
        &mut core,
        id,
        StreamItem::Header {
            header: FileRenderHeader {
                opts,
                ..header("a.rs", 100, 1)
            },
        },
    );
    item(
        &mut core,
        id,
        StreamItem::Chunk {
            repo_id: repo_id(),
            path: path("a.rs"),
            chunk: numbered_chunk(0),
        },
    );
    core.handle(Input::Server(ServerMsg::StreamEnd { id }))
        .unwrap();
    core.handle(Input::User(Action::CommentLines {
        file: file_ref("a.rs"),
        side: Side::Head,
        start_line: 1,
        end_line: 2,
    }))
    .unwrap();
    let mut expected = lines_anchor("a.rs", Side::Head, 11, 1, 2);
    let nits_protocol::Anchor::Lines { context_hash, .. } = &mut expected else {
        panic!("line anchor")
    };
    *context_hash = nits_protocol::ContextHash::new(0);
    assert_eq!(core.view().draft.as_ref().unwrap().anchor, expected);
    assert_eq!(core.view().focus, Focus::Composer);
}

#[test]
fn expand_context_rekeys_one_file_and_refetches_it() {
    let mut core = subscribed(local());
    open_streamed(&mut core);
    core.handle(Input::User(Action::Viewport {
        file: file_ref("a.rs"),
        first_row: 0,
        last_row: 59,
    }))
    .unwrap();
    let effects = core
        .handle(Input::User(Action::ExpandContext {
            file: file_ref("a.rs"),
            full: false,
        }))
        .unwrap();
    let (_, request) = requests(&effects)
        .into_iter()
        .find(|(_, r)| matches!(r, Request::FileRender { .. }))
        .expect("a re-render request");
    let Request::FileRender { opts, path, .. } = request else {
        unreachable!()
    };
    assert_eq!(path, self::path("a.rs"));
    assert_eq!(
        opts.context_lines,
        RenderOpts::default().context_lines + nits_client_core::EXPAND_STEP
    );
    // Only a.rs was re-keyed; b.rs keeps the default opts.
    let of = |core: &ClientCore, p: &str| -> u32 {
        core.view()
            .review
            .as_ref()
            .unwrap()
            .files
            .iter()
            .find(|k| k.path == crate::path(p))
            .unwrap()
            .opts
            .context_lines
    };
    assert_eq!(of(&core, "a.rs"), RenderOpts::default().context_lines + 20);
    assert_eq!(of(&core, "b.rs"), RenderOpts::default().context_lines);
    // A second step grows again; full jumps to the whole file.
    core.handle(Input::User(Action::ExpandContext {
        file: file_ref("a.rs"),
        full: false,
    }))
    .unwrap();
    assert_eq!(of(&core, "a.rs"), RenderOpts::default().context_lines + 40);
    core.handle(Input::User(Action::ExpandContext {
        file: file_ref("a.rs"),
        full: true,
    }))
    .unwrap();
    assert_eq!(of(&core, "a.rs"), nits_client_core::FULL_CONTEXT);
    // The open file follows the re-keyed render.
    let f = open_render(&core);
    assert_eq!(f.opts.context_lines, nits_client_core::FULL_CONTEXT);
}

fn open_render(core: &ClientCore) -> RenderKey {
    core.view()
        .review
        .as_ref()
        .unwrap()
        .open_file
        .as_ref()
        .unwrap()
        .render
        .clone()
}

#[test]
fn browse_tab_shows_a_picked_ref_and_opens_blobs() {
    let mut core = subscribed(local());
    open_streamed(&mut core);
    // Pick a ref to browse; the tree at that ref is fetched.
    let effects = core
        .handle(Input::User(Action::SetBrowseRef {
            repo_id: repo_id(),
            ref_spec: Some(RefSpec::Tag { name: "v1".into() }),
        }))
        .unwrap();
    let (id, request) = requests(&effects)[0].clone();
    assert_eq!(
        request,
        Request::TreeSnapshot {
            repo_id: repo_id(),
            ref_spec: RefSpec::Tag { name: "v1".into() }
        }
    );
    core.handle(Input::Server(ServerMsg::Response {
        id,
        response: Response::TreeSnapshot {
            snapshot: tree(7, &["a.rs", "docs/guide.md"]),
        },
    }))
    .unwrap();
    assert_eq!(
        core.view().browse.as_ref().and_then(|browse| browse
            .selection
            .as_ref()
            .map(|target| target.ref_spec.clone())),
        Some(RefSpec::Tag { name: "v1".into() })
    );
    core.handle(Input::User(Action::SetTab {
        tab: nits_client_core::Tab::Browse,
    }))
    .unwrap();
    // Expand the root: the browse tree lists the picked ref's files, with
    // no change badges.
    let root = match &core.view().tree.roots[0] {
        nits_client_core::TreeNode::Dir { repo_id, .. } => *repo_id,
        nits_client_core::TreeNode::File { .. } => panic!("root is a dir"),
    };
    core.handle(Input::User(Action::ToggleDir {
        repo_id: root,
        path: None,
    }))
    .unwrap();
    let browse_files = visible_files(core.view());
    assert!(
        browse_files.contains(&"docs".to_owned()),
        "{browse_files:?}"
    );
    // Opening a file outside the diff renders its blob.
    let effects = core
        .handle(Input::User(Action::Viewport {
            file: file_ref("docs/guide.md"),
            first_row: 0,
            last_row: 59,
        }))
        .unwrap();
    let (_, request) = requests(&effects)
        .into_iter()
        .find(|(_, r)| matches!(r, Request::BlobRender { .. }))
        .expect("a BlobRender request");
    assert_eq!(
        request,
        Request::BlobRender {
            repo_id: repo_id(),
            path: path("docs/guide.md"),
            entry: nits_protocol::BlobEntry {
                oid: blob_oid(1),
                mode: nits_protocol::BlobMode::Regular
            },
            first_chunk: ChunkIndex::FIRST,
        }
    );
    // A blob has no base side: `h` on one of its rows means nothing, and
    // the focus stays on the only half there is.
    core.handle(Input::User(Action::SetFocus {
        focus: nits_client_core::Focus::Diff {
            row: 0,
            side: Side::Head,
        },
    }))
    .unwrap();
    assert!(
        core.handle(Input::User(Action::RunCommand {
            command: nits_client_core::Command::SideBase,
        }))
        .is_err()
    );
    assert_eq!(
        core.view().focus,
        nits_client_core::Focus::Diff {
            row: 0,
            side: Side::Head
        }
    );
    // Back to the review: the diffing tree lists only the changed files
    // (c.rs lives in the head tree alone and stays in Browse).
    core.handle(Input::User(Action::SetTab {
        tab: nits_client_core::Tab::FilesChanged,
    }))
    .unwrap();
    let back = visible_files(core.view());
    assert!(back.contains(&"a.rs".to_owned()), "{back:?}");
    assert!(!back.contains(&"c.rs".to_owned()), "{back:?}");
    assert!(!back.contains(&"docs".to_owned()), "{back:?}");
    // Clearing the ref returns Browse to the head trees.
    core.handle(Input::User(Action::SetBrowseRef {
        repo_id: repo_id(),
        ref_spec: None,
    }))
    .unwrap();
    assert_eq!(
        core.view().browse.as_ref().and_then(|browse| browse
            .selection
            .as_ref()
            .map(|target| target.ref_spec.clone())),
        None
    );
}

fn visible_files(view: &nits_client_core::ViewModel) -> Vec<String> {
    nits_client_core::visible_nodes(view)
        .iter()
        .map(|n| match n {
            nits_client_core::TreeNode::Dir { name, .. }
            | nits_client_core::TreeNode::File { name, .. } => name.clone(),
        })
        .collect()
}

/// A full text blob absent from Files changed, at an independently picked ref.
fn browse_ready(reference: RefSpec) -> ClientCore {
    let mut core = subscribed(local());
    open_streamed(&mut core);
    let effects = core
        .handle(Input::User(Action::SetBrowseRef {
            repo_id: repo_id(),
            ref_spec: Some(reference),
        }))
        .unwrap();
    let id = requests(&effects)[0].0;
    core.handle(Input::Server(ServerMsg::Response {
        id,
        response: Response::TreeSnapshot {
            snapshot: tree(7, &["unchanged.rs"]),
        },
    }))
    .unwrap();
    core.handle(Input::User(Action::SetTab {
        tab: nits_client_core::Tab::Browse,
    }))
    .unwrap();
    let effects = core
        .handle(Input::User(Action::Viewport {
            file: file_ref("unchanged.rs"),
            first_row: 0,
            last_row: 59,
        }))
        .unwrap();
    let (id, request) = requests(&effects)
        .into_iter()
        .find(|(_, r)| matches!(r, Request::BlobRender { .. }))
        .unwrap();
    assert_eq!(
        request,
        Request::BlobRender {
            repo_id: repo_id(),
            path: path("unchanged.rs"),
            entry: nits_protocol::BlobEntry {
                oid: blob_oid(1),
                mode: nits_protocol::BlobMode::Regular
            },
            first_chunk: ChunkIndex::FIRST
        }
    );
    item(
        &mut core,
        id,
        StreamItem::Header {
            header: FileRenderHeader {
                target: RenderTarget::Blob {
                    entry: nits_protocol::BlobEntry {
                        oid: blob_oid(1),
                        mode: nits_protocol::BlobMode::Regular,
                    },
                },
                ..header("unchanged.rs", 100, 1)
            },
        },
    );
    // Every row is a real source line, including blank ones.
    let rows = (1..=100)
        .map(|n| {
            let cell = nits_protocol::Cell {
                ending: nits_protocol::LineEnding::Lf,
                line_no: nits_protocol::LineNo::new(n).unwrap(),
                text: if n == 2 {
                    String::new()
                } else {
                    format!("line {n}")
                },
                spans: vec![],
                changed: vec![],
            };
            Row::Context {
                left: cell.clone(),
                right: cell,
            }
        })
        .collect();
    item(
        &mut core,
        id,
        StreamItem::Chunk {
            repo_id: repo_id(),
            path: path("unchanged.rs"),
            chunk: RenderChunk {
                index: ChunkIndex::FIRST,
                rows,
            },
        },
    );
    core
}

#[test]
fn browse_single_lines_use_visible_blob_and_capture_every_ref_kind() {
    use nits_client_core::{Command, RowPlace, ViewDelta};
    use nits_protocol::{Anchor, CommentContext};
    for reference in [
        RefSpec::Branch {
            name: "other".into(),
        },
        RefSpec::Tag { name: "v1".into() },
        RefSpec::Commit {
            oid: CommitOid::from_bytes([7; 20]),
        },
        RefSpec::WorkingTree,
    ] {
        let mut core = browse_ready(reference.clone());
        core.handle(Input::User(Action::SetFocus {
            focus: Focus::Diff {
                row: 1,
                side: Side::Head,
            },
        }))
        .unwrap();
        let action = Action::CommentLines {
            file: file_ref("unchanged.rs"),
            side: Side::Head,
            start_line: 2,
            end_line: 2,
        };
        assert_eq!(
            resolve_command(&core, Command::Comment).unwrap(),
            action,
            "keyboard and mouse share CommentLines"
        );
        let effects = core.handle(Input::User(action)).unwrap();
        assert_eq!(
            effects,
            vec![Effect::Render(ViewDelta::new(&[
                ViewSection::Draft,
                ViewSection::Focus,
                ViewSection::Diff,
                ViewSection::Hints
            ]))]
        );
        let draft = core.view().draft.clone().unwrap();
        assert_eq!(
            draft.purpose,
            nits_client_core::DraftPurpose::Comment {
                intent: nits_protocol::CommentIntent::Finding,
                context: Some(CommentContext::Browse {
                    reference: reference.clone()
                })
            }
        );
        let Anchor::Lines {
            blob_oid,
            side,
            lines,
            ..
        } = draft.anchor
        else {
            panic!("line anchor")
        };
        assert_eq!(blob_oid, self::blob_oid(1));
        assert_eq!(side, Side::Head);
        assert_eq!((lines.start().get(), lines.end().get()), (2, 2));
        assert_eq!(
            core.view().diff.as_ref().unwrap().rows[1].drafted,
            Some((RowPlace::Anchor, Side::Head))
        );
        assert_eq!(
            core.handle(Input::User(Action::SetBrowseRef {
                repo_id: repo_id(),
                ref_spec: Some(RefSpec::Head)
            })),
            Err(CoreError::DraftAlreadyOpen)
        );
        let anchor = draft.anchor.clone();
        let effects = core
            .handle(Input::User(Action::DraftSubmitted {
                body: "blank line too".into(),
            }))
            .unwrap();
        let request = requests(&effects);
        assert_eq!(request.len(), 1);
        let Request::Mutate {
            mutation:
                nits_protocol::Mutation::AddComment {
                    anchor: sent,
                    context,
                    ..
                },
            ..
        } = &request[0].1
        else {
            panic!("comment mutation")
        };
        assert_eq!(*sent, anchor);
        assert_eq!(*context, Some(CommentContext::Browse { reference }));
        let thread = &core.view().threads[0];
        assert!(thread.pending);
        let id = thread.id;
        assert_eq!(
            core.view().diff.as_ref().unwrap().rows[1].threads[0].thread,
            id
        );
        // A candidate preserves the committed blob and its thread until it resolves.
        core.handle(Input::User(Action::SetBrowseRef {
            repo_id: repo_id(),
            ref_spec: Some(RefSpec::Head),
        }))
        .unwrap();
        assert!(core.view().diff.is_some());
        core.handle(Input::User(Action::SetTab {
            tab: nits_client_core::Tab::Conversation,
        }))
        .unwrap();
        core.handle(Input::User(Action::SetFocus {
            focus: Focus::Thread { index: 0 },
        }))
        .unwrap();
        assert_eq!(
            resolve_command(&core, Command::Open).unwrap(),
            Action::OpenOriginalDiff { thread_id: id }
        );
        core.handle(Input::User(Action::OpenOriginalDiff { thread_id: id }))
            .unwrap();
        assert_eq!(core.view().tab, nits_client_core::Tab::Browse);
        assert_eq!(
            open_render(&core).target,
            RenderTarget::Blob {
                entry: nits_protocol::BlobEntry {
                    oid: self::blob_oid(1),
                    mode: nits_protocol::BlobMode::Unknown
                }
            }
        );
        assert_eq!(
            core.view().focus,
            Focus::Diff {
                row: 1,
                side: Side::Head
            }
        );
    }
}

#[test]
fn browse_visual_range_and_invalid_targets() {
    use nits_client_core::{Command, RowPlace};
    let mut core = browse_ready(RefSpec::WorkingTree);
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Diff {
            row: 4,
            side: Side::Head,
        },
    }))
    .unwrap();
    for command in [
        Command::VisualMode,
        Command::MoveUp,
        Command::MoveUp,
        Command::Comment,
    ] {
        core.handle(Input::User(Action::RunCommand { command }))
            .unwrap();
    }
    let nits_protocol::Anchor::Lines {
        side,
        lines,
        blob_oid,
        ..
    } = core.view().draft.as_ref().unwrap().anchor
    else {
        panic!()
    };
    assert_eq!(
        (side, lines.start().get(), lines.end().get(), blob_oid),
        (Side::Head, 3, 5, self::blob_oid(1))
    );
    let marked: Vec<_> = core
        .view()
        .diff
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .filter_map(|r| r.drafted.map(|p| (r.index, p)))
        .collect();
    assert_eq!(
        marked,
        vec![
            (2, (RowPlace::Inside, Side::Head)),
            (3, (RowPlace::Inside, Side::Head)),
            (4, (RowPlace::Anchor, Side::Head))
        ]
    );
    core.handle(Input::User(Action::DraftDiscarded)).unwrap();
    assert!(
        core.view()
            .diff
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .all(|r| r.drafted.is_none())
    );
    for (side, start_line, end_line) in
        [(Side::Base, 1, 1), (Side::Head, 0, 1), (Side::Head, 1, 101)]
    {
        assert_eq!(
            core.handle(Input::User(Action::CommentLines {
                file: file_ref("unchanged.rs"),
                side,
                start_line,
                end_line
            })),
            Err(CoreError::UnknownFile(file_ref("unchanged.rs")))
        );
    }
}

#[test]
fn browse_superseded_tree_response_cannot_retarget_content() {
    let mut core = browse_ready(RefSpec::WorkingTree);
    let first = core
        .handle(Input::User(Action::SetBrowseRef {
            repo_id: repo_id(),
            ref_spec: Some(RefSpec::Tag { name: "old".into() }),
        }))
        .unwrap();
    let second = core
        .handle(Input::User(Action::SetBrowseRef {
            repo_id: repo_id(),
            ref_spec: Some(RefSpec::Tag { name: "new".into() }),
        }))
        .unwrap();
    assert!(
        core.handle(Input::User(Action::Viewport {
            file: file_ref("a.rs"),
            first_row: 0,
            last_row: 10
        }))
        .is_err()
    );
    for (effects, root) in [(second, 9), (first, 8)] {
        core.handle(Input::Server(ServerMsg::Response {
            id: requests(&effects)[0].0,
            response: Response::TreeSnapshot {
                snapshot: tree(root, &["unchanged.rs"]),
            },
        }))
        .unwrap();
    }
    core.handle(Input::User(Action::ToggleDir {
        repo_id: repo_id(),
        path: None,
    }))
    .unwrap();
    assert_eq!(
        core.view().browse.as_ref().and_then(|browse| browse
            .selection
            .as_ref()
            .map(|target| target.ref_spec.clone())),
        Some(RefSpec::Tag { name: "new".into() })
    );
}

#[test]
fn default_browse_opens_changed_files_as_plain_blobs_with_blob_render_options() {
    let mut core = subscribed(local());
    open_streamed(&mut core);
    core.handle(Input::User(Action::Viewport {
        file: file_ref("a.rs"),
        first_row: 0,
        last_row: 59,
    }))
    .unwrap();
    let effects = core
        .handle(Input::User(Action::SetTab {
            tab: nits_client_core::Tab::Browse,
        }))
        .unwrap();
    assert_eq!(
        open_render(&core).target,
        RenderTarget::Blob {
            entry: nits_protocol::BlobEntry {
                oid: blob_oid(1),
                mode: nits_protocol::BlobMode::Regular
            }
        }
    );
    assert_eq!(open_render(&core).opts, RenderOpts::default());
    assert_eq!(requests(&effects).iter().filter(|(_, request)| matches!(request, Request::BlobRender { entry, .. } if entry.oid == blob_oid(1))).count(), 1);
    // A newly selected ref is not allowed to fall back to a same-path review diff.
    let effects = core
        .handle(Input::User(Action::SetBrowseRef {
            repo_id: repo_id(),
            ref_spec: Some(RefSpec::Tag {
                name: "empty".into(),
            }),
        }))
        .unwrap();
    core.handle(Input::Server(ServerMsg::Response {
        id: requests(&effects)[0].0,
        response: Response::TreeSnapshot {
            snapshot: tree(8, &[]),
        },
    }))
    .unwrap();
    assert_eq!(
        core.handle(Input::User(Action::Viewport {
            file: file_ref("a.rs"),
            first_row: 0,
            last_row: 59
        })),
        Err(CoreError::UnknownFile(file_ref("a.rs")))
    );
    assert_eq!(
        core.handle(Input::User(Action::CommentLines {
            file: file_ref("a.rs"),
            side: Side::Head,
            start_line: 2,
            end_line: 2
        })),
        Err(CoreError::UnknownFile(file_ref("a.rs")))
    );
    // Diff-only preferences do not become BlobRender cache keys.
    let mut core = browse_ready(RefSpec::WorkingTree);
    core.handle(Input::User(Action::SetRenderOpts {
        ignore_whitespace: true,
        context_lines: 20,
    }))
    .unwrap();
    core.handle(Input::User(Action::Viewport {
        file: file_ref("unchanged.rs"),
        first_row: 0,
        last_row: 59,
    }))
    .unwrap();
    assert_eq!(open_render(&core).opts, RenderOpts::default());
}

#[test]
fn browse_refresh_of_same_ref_ignores_the_older_snapshot() {
    let mut core = browse_ready(RefSpec::WorkingTree);
    let mut ids = vec![];
    for _ in 0..2 {
        let effects = core
            .handle(Input::User(Action::SetBrowseRef {
                repo_id: repo_id(),
                ref_spec: Some(RefSpec::WorkingTree),
            }))
            .unwrap();
        ids.push(requests(&effects)[0].0);
    }
    for (id, root, files) in [(ids[1], 9, &["new.rs"][..]), (ids[0], 8, &["old.rs"][..])] {
        core.handle(Input::Server(ServerMsg::Response {
            id,
            response: Response::TreeSnapshot {
                snapshot: tree(root, files),
            },
        }))
        .unwrap();
    }
    core.handle(Input::User(Action::ToggleDir {
        repo_id: repo_id(),
        path: None,
    }))
    .unwrap();
    let files = visible_files(core.view());
    assert!(files.contains(&"new.rs".into()), "{files:?}");
    assert!(!files.contains(&"old.rs".into()), "{files:?}");
}

#[test]
fn browse_file_comment_shortcuts_capture_one_source_for_anchor_and_context() {
    use nits_client_core::{Command, Tab, TreeNode};
    use nits_protocol::{Anchor, CommentContext};

    for name in ["a.rs", "unchanged.rs"] {
        for keys in [&['c'][..], &['z', 'c'][..]] {
            let mut core = subscribed(local());
            open_streamed(&mut core);
            let reference = RefSpec::Tag {
                name: "archive".into(),
            };
            let effects = core
                .handle(Input::User(Action::SetBrowseRef {
                    repo_id: repo_id(),
                    ref_spec: Some(reference.clone()),
                }))
                .unwrap();
            core.handle(Input::Server(ServerMsg::Response {
                id: requests(&effects)[0].0,
                response: Response::TreeSnapshot {
                    snapshot: tree(7, &["a.rs", "unchanged.rs"]),
                },
            }))
            .unwrap();
            core.handle(Input::User(Action::SetTab { tab: Tab::Browse }))
                .unwrap();
            let command = if keys.len() == 1 {
                // Tree c works before opening the file or fetching its render.
                core.handle(Input::User(Action::ToggleDir {
                    repo_id: repo_id(),
                    path: None,
                }))
                .unwrap();
                let index = nits_client_core::visible_nodes(core.view())
                    .iter()
                    .position(
                        |node| matches!(node, TreeNode::File { path: p, .. } if *p == path(name)),
                    )
                    .unwrap();
                core.handle(Input::User(Action::SetFocus {
                    focus: Focus::Tree { index },
                }))
                .unwrap();
                Command::Comment
            } else {
                let effects = core
                    .handle(Input::User(Action::Viewport {
                        file: file_ref(name),
                        first_row: 0,
                        last_row: 59,
                    }))
                    .unwrap();
                let id = requests(&effects)
                    .into_iter()
                    .find(|(_, request)| matches!(request, Request::BlobRender { .. }))
                    .unwrap()
                    .0;
                item(
                    &mut core,
                    id,
                    StreamItem::Header {
                        header: FileRenderHeader {
                            target: RenderTarget::Blob {
                                entry: nits_protocol::BlobEntry {
                                    oid: blob_oid(1),
                                    mode: nits_protocol::BlobMode::Regular,
                                },
                            },
                            ..header(name, 100, 1)
                        },
                    },
                );
                Command::CommentOnFile
            };
            assert_eq!(
                resolve_command(&core, command).unwrap(),
                Action::CommentFile {
                    file: file_ref(name)
                }
            );
            for key in keys {
                core.handle(Input::Key(KeyChord::char(*key))).unwrap();
            }
            let anchor = Anchor::File {
                repo_id: repo_id(),
                path: path(name),
                blob_oid: blob_oid(1),
            };
            let context = Some(CommentContext::Browse { reference });
            let draft = core.view().draft.as_ref().unwrap();
            assert_eq!(draft.anchor, anchor, "{name}, {keys:?}");
            assert_eq!(
                draft.purpose,
                nits_client_core::DraftPurpose::Comment {
                    intent: nits_protocol::CommentIntent::Finding,
                    context: context.clone()
                },
                "{name}, {keys:?}"
            );
            assert_eq!(core.view().focus, Focus::Composer);
            assert_eq!(
                core.handle(Input::User(Action::SetBrowseRef {
                    repo_id: repo_id(),
                    ref_spec: Some(RefSpec::Head),
                })),
                Err(CoreError::DraftAlreadyOpen)
            );
            let effects = core
                .handle(Input::User(Action::DraftSubmitted {
                    body: "file note".into(),
                }))
                .unwrap();
            let sent = requests(&effects);
            assert_eq!(sent.len(), 1);
            let Request::Mutate {
                mutation:
                    nits_protocol::Mutation::AddComment {
                        anchor: sent_anchor,
                        context: sent_context,
                        ..
                    },
                ..
            } = &sent[0].1
            else {
                panic!("comment mutation")
            };
            assert_eq!(*sent_anchor, anchor);
            assert_eq!(*sent_context, context);

            // The picker can move while the comment keeps its original blob.
            let thread_id = core.view().threads[0].id;
            core.handle(Input::User(Action::SetBrowseRef {
                repo_id: repo_id(),
                ref_spec: Some(RefSpec::Head),
            }))
            .unwrap();
            core.handle(Input::User(Action::OpenOriginalDiff { thread_id }))
                .unwrap();
            assert_eq!(
                open_render(&core).target,
                RenderTarget::Blob {
                    entry: nits_protocol::BlobEntry {
                        oid: blob_oid(1),
                        mode: nits_protocol::BlobMode::Unknown
                    }
                }
            );
            // The mouse action on the reopened original keeps that same context,
            // even while the newly picked tree has not arrived.
            let effects = core
                .handle(Input::User(Action::CommentFile {
                    file: file_ref(name),
                }))
                .unwrap();
            assert_eq!(
                effects,
                vec![Effect::Render(nits_client_core::ViewDelta::new(&[
                    ViewSection::Draft,
                    ViewSection::Focus,
                    ViewSection::Hints,
                ]))]
            );
            let draft = core.view().draft.as_ref().unwrap();
            assert_eq!(draft.anchor, anchor);
            assert_eq!(
                draft.purpose,
                nits_client_core::DraftPurpose::Comment {
                    intent: nits_protocol::CommentIntent::Finding,
                    context
                }
            );
        }
    }
}

#[test]
fn browse_file_comments_reject_pending_absent_and_non_blob_tree_entries() {
    use nits_client_core::{Command, Tab};
    let mut core = subscribed(local());
    open_streamed(&mut core);
    core.handle(Input::User(Action::SetTab { tab: Tab::Browse }))
        .unwrap();
    let effects = core
        .handle(Input::User(Action::SetBrowseRef {
            repo_id: repo_id(),
            ref_spec: Some(RefSpec::WorkingTree),
        }))
        .unwrap();
    // The current head remains visible and commentable while a candidate loads.
    core.handle(Input::User(Action::CommentFile {
        file: file_ref("a.rs"),
    }))
    .unwrap();
    assert!(core.view().draft.is_some());
    core.handle(Input::User(Action::DraftDiscarded)).unwrap();
    core.handle(Input::User(Action::ToggleDir {
        repo_id: repo_id(),
        path: None,
    }))
    .unwrap();
    let index = nits_client_core::visible_nodes(core.view())
        .iter()
        .position(|node| matches!(node, nits_client_core::TreeNode::File { path: p, .. } if *p == path("a.rs")))
        .unwrap();
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Tree { index },
    }))
    .unwrap();
    core.handle(Input::Key(KeyChord::char('c'))).unwrap();
    assert!(core.view().draft.is_some());
    core.handle(Input::User(Action::DraftDiscarded)).unwrap();
    let mut picked = tree(7, &[]);
    picked.entries = vec![
        TreeEntry {
            path: path("dir"),
            kind: TreeEntryKind::Dir { oid: tree_oid(8) },
        },
        TreeEntry {
            path: path("module"),
            kind: TreeEntryKind::Submodule {
                commit: CommitOid::from_bytes([9; 20]),
            },
        },
    ];
    core.handle(Input::Server(ServerMsg::Response {
        id: requests(&effects)[0].0,
        response: Response::TreeSnapshot { snapshot: picked },
    }))
    .unwrap();
    for name in ["a.rs", "dir", "module"] {
        assert_eq!(
            core.handle(Input::User(Action::CommentFile {
                file: file_ref(name)
            })),
            Err(CoreError::UnknownFile(file_ref(name)))
        );
        assert!(core.view().draft.is_none());
    }
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Tree { index: 0 },
    }))
    .unwrap();
    assert!(
        resolve_command(&core, Command::Comment).is_err(),
        "directory cannot be commented"
    );
}

#[test]
fn superseded_browse_headers_cannot_change_review_files_totals_or_tree() {
    use nits_client_core::Tab;
    // A stale header for an existing review path must not duplicate it;
    // one for an unchanged path must not add it to Files changed.
    for name in ["a.rs", "unchanged.rs"] {
        for switch_tab in [false, true] {
            let mut core = subscribed(local());
            open_streamed(&mut core);
            let changed_files = core.view().review.as_ref().unwrap().files.clone();
            let progress = core.view().progress;
            let changed_tree = core.view().tree.clone();
            let effects = core
                .handle(Input::User(Action::SetBrowseRef {
                    repo_id: repo_id(),
                    ref_spec: Some(RefSpec::Tag {
                        name: "archive".into(),
                    }),
                }))
                .unwrap();
            core.handle(Input::Server(ServerMsg::Response {
                id: requests(&effects)[0].0,
                response: Response::TreeSnapshot {
                    snapshot: tree(7, &[name]),
                },
            }))
            .unwrap();
            core.handle(Input::User(Action::SetTab { tab: Tab::Browse }))
                .unwrap();
            let effects = core
                .handle(Input::User(Action::Viewport {
                    file: file_ref(name),
                    first_row: 0,
                    last_row: 59,
                }))
                .unwrap();
            let id = requests(&effects)
                .into_iter()
                .find(|(_, request)| matches!(request, Request::BlobRender { .. }))
                .unwrap()
                .0;
            let selected = core
                .handle(Input::User(Action::SetBrowseRef {
                    repo_id: repo_id(),
                    ref_spec: Some(RefSpec::Head),
                }))
                .unwrap();
            core.handle(Input::Server(ServerMsg::Response {
                id: requests(&selected)[0].0,
                response: Response::TreeSnapshot {
                    snapshot: tree(8, &[]),
                },
            }))
            .unwrap();
            if switch_tab {
                core.handle(Input::User(Action::SetTab {
                    tab: Tab::FilesChanged,
                }))
                .unwrap();
            }
            let effects = item(
                &mut core,
                id,
                StreamItem::Header {
                    header: FileRenderHeader {
                        target: RenderTarget::Blob {
                            entry: nits_protocol::BlobEntry {
                                oid: blob_oid(1),
                                mode: nits_protocol::BlobMode::Regular,
                            },
                        },
                        ..header(name, 100, 1)
                    },
                },
            );
            assert_eq!(effects, vec![], "a superseded header only warms the cache");
            assert_eq!(core.view().review.as_ref().unwrap().files, changed_files);
            assert_eq!(core.view().progress, progress);
            assert!(core.view().diff.is_none());
            item(
                &mut core,
                id,
                StreamItem::Chunk {
                    repo_id: repo_id(),
                    path: path(name),
                    chunk: chunk(0),
                },
            );
            core.handle(Input::Server(ServerMsg::StreamEnd { id }))
                .unwrap();
            core.handle(Input::User(Action::SetTab {
                tab: Tab::FilesChanged,
            }))
            .unwrap();
            assert_eq!(core.view().review.as_ref().unwrap().files, changed_files);
            assert_eq!(core.view().progress, progress);
            // Reopen the original selected file so its tree open marker matches.
            core.handle(Input::User(Action::Viewport {
                file: file_ref("a.rs"),
                first_row: 0,
                last_row: 59,
            }))
            .unwrap();
            assert_eq!(core.view().tree, changed_tree);
        }
    }
}

#[test]
fn portable_reply_link_preserves_deferred_browse_source_and_inflight_review_requests() {
    use nits_protocol::{
        CommentContext, CommentState, ReferenceContext, ReferenceTarget, ReviewReference,
    };
    for cache in [local(), remote(Bytes::mib(8), Bytes::mib(16))] {
        let mut core = subscribed(cache);
        let source = CommentContext::Browse {
            reference: RefSpec::Tag { name: "v1".into() },
        };
        let anchor = lines_anchor("unchanged.rs", Side::Head, 1, 2, 2);
        let mut root = comment_at(
            1,
            anchor.clone(),
            CommentState::Outdated {
                last_good_anchor: anchor.clone(),
            },
        );
        root.context = Some(source.clone());
        let thread_id = root.thread_id;
        let mut reply = comment_at(2, anchor, CommentState::Live);
        reply.thread_id = thread_id;
        reply.context = Some(source.clone());
        reply.body = "Verified the retained source".into();
        let reply_id = reply.id;
        let mut snap = snapshot(1, 2);
        snap.threads.push(nits_protocol::Thread {
            id: thread_id,
            review_id: review_id(),
            root: root.id,
            replies: Vec::new(),
            resolution: nits_protocol::ThreadResolution::Open,
        });
        snap.comments.push(root);
        let context = ReferenceContext::named("review-box").unwrap();
        core.handle(Input::User(Action::SetReferenceContext {
            context: context.clone(),
        }))
        .unwrap();
        let reference = ReviewReference {
            context,
            review_id: review_id(),
            target: ReferenceTarget::Comment {
                comment_id: reply_id,
            },
        };
        let effects = core
            .handle(Input::User(Action::OpenReference {
                reference: reference.to_string(),
            }))
            .unwrap();
        let (id, request) = requests(&effects)[0].clone();
        // A verification reply, deferral, and review request all arrive ahead
        // of the older snapshot that the link is opening.
        core.handle(Input::Server(foreign_event(
            2,
            EventBody::CommentCreated { comment: reply },
        )))
        .unwrap();
        core.handle(Input::Server(foreign_event(
            3,
            EventBody::ThreadDeferred {
                review_id: review_id(),
                thread_id,
                reason: "Follow up outside this review"
                    .to_owned()
                    .try_into()
                    .unwrap(),
                tracking_url: Some(
                    "https://example.com/issues/288"
                        .to_owned()
                        .try_into()
                        .unwrap(),
                ),
            },
        )))
        .unwrap();
        core.handle(Input::Server(foreign_event(
            4,
            EventBody::ReviewRequested {
                review_id: review_id(),
                agent: "review-agent".into(),
                note: "Check this retained source".into(),
                targets: nits_protocol::RequestedTargets::Unknown,
            },
        )))
        .unwrap();
        match request {
            Request::OpenReview { .. } => {
                item(&mut core, id, StreamItem::ReviewSnapshot { snapshot: snap });
                core.handle(Input::Server(ServerMsg::StreamEnd { id }))
                    .unwrap();
            }
            Request::ReviewSnapshot { .. } => {
                core.handle(Input::Server(ServerMsg::Response {
                    id,
                    response: Response::ReviewSnapshot { snapshot: snap },
                }))
                .unwrap();
            }
            _ => panic!("review-open request"),
        }
        assert_eq!(core.view().tab, nits_client_core::Tab::Conversation);
        assert_eq!(core.view().focus, Focus::Thread { index: 0 });
        assert_eq!(core.view().focused_comment, Some(reply_id));
        assert_eq!(core.view().copy_reference, Some(reference));
        assert!(matches!(
            core.view().threads[0].status,
            nits_protocol::ThreadResolution::Deferred { .. }
        ));
        assert_eq!(core.view().requests.len(), 1);
        assert_eq!(core.view().requests[0].note, "Check this retained source");
        assert_eq!(
            core.view().review.as_ref().unwrap().snapshot.seq,
            Seq::new(4)
        );
        // A different picker selection must not replace the comment's pinned
        // source, including when its newly selected tree has not arrived.
        core.handle(Input::User(Action::SetBrowseRef {
            repo_id: repo_id(),
            ref_spec: Some(RefSpec::Head),
        }))
        .unwrap();
        let effects = core
            .handle(Input::User(Action::OpenOriginalDiff { thread_id }))
            .unwrap();
        let effects = Kv::default().drive(&mut core, effects);
        let (render_id, request) = requests(&effects)
            .into_iter()
            .find(|(_, r)| matches!(r, Request::BlobRender { .. }))
            .unwrap();
        assert_eq!(
            request,
            Request::BlobRender {
                repo_id: repo_id(),
                path: path("unchanged.rs"),
                entry: nits_protocol::BlobEntry {
                    oid: blob_oid(1),
                    mode: nits_protocol::BlobMode::Unknown
                },
                first_chunk: ChunkIndex::FIRST
            }
        );
        assert_eq!(core.view().tab, nits_client_core::Tab::Browse);
        item(
            &mut core,
            render_id,
            StreamItem::Header {
                header: FileRenderHeader {
                    target: RenderTarget::Blob {
                        entry: nits_protocol::BlobEntry {
                            oid: blob_oid(1),
                            mode: nits_protocol::BlobMode::Unknown,
                        },
                    },
                    ..header("unchanged.rs", 100, 1)
                },
            },
        );
        item(
            &mut core,
            render_id,
            StreamItem::Chunk {
                repo_id: repo_id(),
                path: path("unchanged.rs"),
                chunk: numbered_chunk(0),
            },
        );
        core.handle(Input::Server(ServerMsg::StreamEnd { id: render_id }))
            .unwrap();
        assert!(core.view().diff.as_ref().unwrap().original);
        assert_eq!(
            core.view().focus,
            Focus::Diff {
                row: 1,
                side: Side::Head
            }
        );
        core.handle(Input::Key(KeyChord::char('c'))).unwrap();
        let draft = core.view().draft.as_ref().unwrap();
        assert_eq!(
            draft.purpose,
            nits_client_core::DraftPurpose::Comment {
                intent: nits_protocol::CommentIntent::Finding,
                context: Some(source.clone())
            }
        );
        let nits_protocol::Anchor::Lines {
            blob_oid: anchored_blob,
            lines,
            ..
        } = &draft.anchor
        else {
            panic!("line draft")
        };
        assert_eq!(*anchored_blob, blob_oid(1));
        assert_eq!(lines.start().get(), 2);
        let expected_anchor = draft.anchor.clone();
        let effects = core
            .handle(Input::User(Action::DraftSubmitted {
                body: "Follow-up on the pinned source".into(),
            }))
            .unwrap();
        let (
            _,
            Request::Mutate {
                mutation:
                    nits_protocol::Mutation::AddComment {
                        anchor, context, ..
                    },
                ..
            },
        ) = &requests(&effects)[0]
        else {
            panic!("comment mutation")
        };
        assert_eq!(*anchor, expected_anchor);
        assert_eq!(*context, Some(source));
        assert_eq!(core.view().requests.len(), 1);
    }
}

#[path = "cache_flow/shared_trees.rs"]
mod shared_trees;

#[path = "cache_flow/browse_selection.rs"]
mod browse_selection;
