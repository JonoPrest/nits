# Implementation Plan

Companion to `ARCHITECTURE.md`. Four milestones, each shippable and tested on its own.
Two cross-cutting principles apply to every task.

## Principles

### Type-driven design

Invalid state should not be expressible. Concretely:

- **Newtypes for every ID and OID**: `WorkspaceId`, `ReviewId`, `CommentId`, `ThreadId`, `RepoId`, `BlobOid`, `CommitOid`, `Seq`, `ClientSeq`. No bare `String`/`u64` in signatures.
- **Enums over flags.** `Anchor` is `Review | File | Lines`, never `Option<path> + Option<lines>`. `CommentState` is `Live | Outdated { last_good } | Deleted`, never `deleted: bool`.
- **Parse, don't validate.** Wire input is parsed into a validated domain type once at the boundary (`TryFrom<protocol::X> for core::X`); the core never re-checks.
- **Resolved vs unresolved are different types.** `RefSpec` (what the user asked for) vs `ResolvedTarget { base: CommitOid | WorkTreeSnapshot, head: ... }`. A `Review` holds both; diffing only accepts the resolved one.
- **Pending vs committed events are different types.** `nits-client-core` has `PendingEvent { client_seq, .. }` and `CommittedEvent { seq, .. }`; you cannot render a `Seq` that doesn't exist.
- **Typestate for connections/subscriptions** where cheap (`Disconnected → Connecting → Subscribed { since: Seq }`).
- **Non-empty where required**: `Review.targets: NonEmpty<ReviewTarget>`; `Lines.range: LineRange` with `start <= end` enforced by constructor.
- **Exhaustive matching, no `_ =>` on domain enums** (clippy `wildcard_enum_match_arm` in core crates).
- **Sans-I/O boundaries** make this checkable: `nits-client-core` returns `Vec<Effect>`; a test asserts exactly which effects, no more.

### Testing

Each crate has its own test strategy, listed per milestone. Common tools: `insta` for snapshots, `proptest` for invariants, `tempfile` + real `git` for repo fixtures, `cargo nextest`. A shared `nits-test-support` crate provides repo builders (`RepoBuilder::new().commit("a", files![...]).branch("x")...`).

CI: `cargo nextest run`, `cargo clippy -D warnings`, `cargo check -p nits-protocol -p nits-client-core --target wasm32-unknown-unknown`, `cargo fmt --check`, ReScript build + tests.

---

## Milestone 1 — `nits-protocol` + `nits-review-core`

Goal: headless engine. Given repos on disk, create workspaces/reviews, produce render models, store and re-anchor comments across restarts.

### 1.1 Workspace scaffold
- Cargo workspace; crates `nits-protocol`, `nits-protocol-fixtures` (dev-only example values), `nits-review-core`, `nits-test-support`, `xtask`.
- Lints: `#![deny(missing_debug_implementations)]`, clippy pedantic subset, `wildcard_enum_match_arm` in `nits-review-core`/`nits-client-core`.
- CI as above.

### 1.2 `nits-protocol` types
- IDs (ULID-backed newtypes, `Display`/`FromStr`), OIDs, `Seq`.
- Domain: `Workspace`, `Repo`, `Review`, `ReviewTarget`, `RefSpec`, `Comment`, `Author`, `Anchor`, `CommentKind`, `CommentState`, `Thread`, `CommitInfo`, `ViewedMark`, `RenderOpts`.
- Events: `Event { seq, ts, author, client_id, client_seq, body: EventBody }` and each `EventBody` variant.
- Render model: `Row`, `Cell`, `Span`, `FileRender`, `DiffSummary`.
- RPC: `ClientMsg`, `ServerMsg`, `Request`/`Response` per method, `SubscribeScope`, `OpenReviewItem` stream items, `ReviewSnapshot`, `ViewDelta` sections.
- All `#[serde(tag = "type")]`, `deny_unknown_fields`.
- Versioning (§4.9): `ProtocolVersion`, `SchemaVersion`, `Envelope<T>`, `Hello`/`Welcome`/`Rejected`, `UnsupportedProtocol`/`VersionMismatch` errors, `UpgradeNotice`.
- **Fixtures**: `cargo xtask fixtures` writes `fixtures/protocol/<Type>/<variant>.json` for every variant (a `Fixtures` trait implemented per type; a test asserts every enum variant has a fixture via exhaustive match).
- Tests: serde round-trip per fixture; `insta` snapshot of every fixture so wire changes are visible in review; `proptest` round-trip for IDs/ranges.

### 1.3 Event store (`nits_review_core::store`)
- redb tables: `events`, `reviews`, `comments`, `threads`, `anchors_by_blob`, `workspaces`, `meta`.
- `Store::append(NewEvent) -> CommittedEvent` assigns `Seq` atomically and updates views in the same txn.
- `Store::replay_from(Seq) -> impl Iterator<CommittedEvent>`.
- `Store::rebuild_views()` from log; startup verifies `meta.view_seq == last_seq` or rebuilds.
- `meta.schema_version` stamped on create; `Store::open` runs `migrations: [fn(&WriteTxn)]` forward from the stored version, refuses newer with `StoreError::SchemaTooNew { found, supported }`. Each stored event carries its `SchemaVersion`.
- Tests: open a store stamped `CURRENT + 1` → `SchemaTooNew`; a store stamped `0` with a fixture log migrates to `CURRENT` and replays identically; append/read; views match a fold over the log (`proptest`: random event sequences, compare `rebuild_views` vs incremental); reopen after drop preserves everything; tombstoned review excluded from listings; concurrent appenders get strictly increasing `Seq`.

### 1.4 Git engine (`nits_review_core::git`)
- `Repo::open(path)`, `resolve(RefSpec) -> ResolvedRef`, `tree(CommitOid)`, `blob(BlobOid)`, `commits_between(base, head) -> [CommitInfo]` (full message body, author/committer signatures with times).
- `WorkingTree` snapshot: hash working files into a virtual tree (`WorkTreeSnapshot { tree_oid, dirty: [path] }`); unchanged files reuse index OIDs.
- `tree_snapshot(root: TreeOid) -> TreeSnapshot` (full recursive walk, flat sorted entries) and `tree_delta(from, to)`; working-tree snapshot yields a synthetic root OID.
- `changed_files(base, head) -> [FileChange { path, kind: Added|Deleted|Modified|Renamed{from}, old: Option<BlobOid>, new: Option<BlobOid> }]`.
- Tests with `nits-test-support` repos: each `RefSpec` variant resolves; renames detected; working-tree snapshot reflects unstaged edits and untracked files; binary files flagged.

### 1.5 Diff + render model (`nits_review_core::render`)
- `render_file(old: Option<&[u8]>, new: Option<&[u8]>, lang, opts) -> (FileRenderHeader, impl Iterator<RenderChunk>)`.
- Pipeline: (optional whitespace-normalised line view for `ignore_whitespace`) → `imara-diff` hunks → pair `-`/`+` into `Modified` → intra-line ranges → context collapsing with `Expander` → syntect spans (whole-file pass, size-capped) → split into ~500-row chunks. Whitespace-only files collapse to a single marker row.
- `render_blob(bytes, lang)` for explorer views (all `Context` rows), same chunked shape.
- Content-keyed disk cache `(old_oid, new_oid, opts_hash, chunk_index)`; header cached separately.
- Tests: `insta` snapshots for a corpus (add/delete/modify/rename/whitespace-only/binary/huge/no-trailing-newline/CRLF), each rendered with and without `ignore_whitespace`; with it on, re-indenting a block yields zero `Modified` rows while the text in rows is unchanged; invariants via `proptest`: every source line appears exactly once on its side, line numbers monotonic, spans within cell bounds, `Expander.hidden` sums to the omitted count, chunks concatenate to `total_rows` with no gaps; unified and split derive from the same rows. Benchmark: 10k-line and 100k-line files, header returned < 50 ms, first chunk < 200 ms; above the cap `highlighted == false`.

### 1.6 Reviews (`nits_review_core::review`)
- `create_review(workspace, NonEmpty<ReviewTarget>)`, `resolve_targets(review) -> ResolvedTargets` (emits `ReviewTargetsResolved` only when OIDs change), `files(review) -> merged tree`, `commits(review)` for commit stepping, `file_render(review, repo, path)`.
- Merged tree: repo roots at top level; deterministic ordering.
- `mark_viewed(review, repo, path)` / `unmark_viewed` emit `FileViewed{blob_oid}`/`FileUnviewed`; rejected for `Author::Agent`.
- Tests: multi-repo review yields one tree with both roots; commit stepping produces `base=parent` sub-reviews and `CommitInfo` carries full body/author/times incl. merge commits; re-resolve is idempotent (no duplicate events); viewed mark survives a head move that doesn't touch the file and is reported `ChangedSinceViewed` when it does; agent `mark_viewed` is a typed error.

### 1.7 Comments + anchoring (`nits_review_core::comments`)
- `add_comment`, `reply`, `edit`, `delete`, `resolve_thread`, `unresolve_thread`; all emit events, all validate against current review state (e.g. `Lines` range within blob length).
- `reanchor(review, old: ResolvedTargets, new: ResolvedTargets)` per §4.5, emitting `CommentReanchored { comment, anchor, state }`; pure function of blobs so milestone 2 can run it off the actor.
- Tests: table-driven anchoring cases (unchanged blob; lines shifted above; lines shifted within; lines modified → `Outdated`; file deleted; file renamed; base-side anchor when base moves). `proptest`: random edits *outside* the anchored range never produce `Outdated`. Persistence: comments survive store reopen with anchors intact.

### 1.8 `Core` façade
- One `Core` struct composing the above; every public method takes/returns `nits-protocol` types; this is the surface every transport adapts.
- Tests: end-to-end scenario tests written as scripts (`open workspace → attach 2 repos → review → comment → commit on head → re-resolve → assert comment state`).

Exit criteria: all above green; `cargo check --target wasm32-unknown-unknown -p protocol` passes.

---

## Milestone 2 — `nitsd`

Goal: `nitsd` running, multiple clients connected, events streaming, MCP working.

### 2.1 Transport framing
- `codec` module: length-prefixed JSON frames over `AsyncRead/Write`, each an `Envelope`; `ClientMsg`/`ServerMsg` mux with request ids.
- Handshake typestate: `AwaitingHello → Negotiated { protocol }`; `Hello` with an unservable version → `Rejected { UnsupportedProtocol }` + close; served-but-old minor → `Welcome.upgrade`; post-handshake frame with a different `v` → `VersionMismatch`.
- Tests: framing round-trip, partial reads, oversized frame rejected, interleaved requests answered by id; handshake table (same version, older minor, newer minor, other major); every response `Envelope.v` equals the negotiated version.

### 2.2 Unix socket server
- tokio; one task per connection; `Core` behind `Arc<RwLock>` or actor (decide during impl; prefer actor with a command channel so `Core` stays single-threaded and simple).
- `subscribe(scope, since)` → replay from store then live tail via broadcast.
- `open_review` streamed in the order of §4.8; fresh subscribers get `ReviewSnapshot` + `since = current_seq`, never a log replay.
- Re-anchoring runs on the blocking pool after `ReviewTargetsResolved`, emitting `CommentReanchored` incrementally.
- Render work leaves the actor: `file_render` resolves blobs on the actor, then runs the pure render on `spawn_blocking`; header sent as soon as the diff is done, chunks streamed as `ServerMsg::RenderChunk` with the requested index first.
- Tests: two clients, one writes, other receives with correct `Seq`; reconnect with `since` receives exactly the gap; slow subscriber doesn't block others; a 100k-line render in flight does not delay an `add_comment` from another client (latency assertion); `open_review` on a 300-file review completes headers in one stream with no client-initiated requests; re-anchoring 500 comments after a rebase does not block a concurrent `list_reviews` > 50 ms.

### 2.3 File watcher
- `notify` per repo; debounce; triggers `resolve_targets` for working-tree reviews.
- Working-tree changes also emit `TreeDelta` to subscribers of that ref.
- Tests: edit file → `ReviewTargetsResolved` emitted once for a burst of writes; no event when content unchanged; create/delete file → single `TreeDelta` with the right entries.

### 2.4 WebSocket transport
- Same codec over `tokio-tungstenite`; enables browser client later.
- Tests: shared transport test-suite run against both unix and ws (`#[test_case]` / generic harness).

### 2.5 MCP
- `nits-mcp` stdio binary proxying to daemon socket; tool per `Core` method; `Author::Agent` from MCP client info + session.
- `subscribe_events` tool for long-poll/streaming, with source-context cursor checks after switching.
- `list_contexts` / `use_context` select a session-local daemon through a successful replacement handshake; reads report source context, identity survives, and subscriptions cannot cross the switch.
- Tests: JSON-RPC conformance for tool list; each tool maps to core and round-trips; agent-authored comment carries provenance.

### 2.6 `nits` CLI
- `nits workspace add/list`, `nits review create --base --head [--repo ...]`, `nits comment ...`, `nits events --follow`. Same client lib as MCP.
- `nits context use NAME` atomically persists a startup default; ad-hoc transports and explicit context flags/environment override it. `context show` includes selection origin.
- Tests: `assert_cmd` against spawned daemons in temp dirs, including selection precedence and MCP startup/switch isolation.

### 2.7 Lifecycle
- Data dir, socket path, `nitsd --stdio` mode for `ssh host nitsd --stdio`, graceful shutdown, crash-safe reopen.
- Tests: kill -9 mid-append → reopen consistent (redb guarantees; assert view rebuild path works).

---

## Milestone 3 — `nits-client-core`

Goal: sans-I/O client that models everything the UI needs; proven under races.

### 3.0 Benchmarks gate (§10 of ARCHITECTURE)
- Add `benches/` in `nits-review-core` and `nitsd` for each "measure before optimising" trigger: worktree snapshot after single edit (50k files), `changed_files` on directory move, 200-comment agent burst, `tree_snapshot` size/time. Run in CI on a synthetic repo; record numbers in `docs/BENCHMARKS.md`. Optimisations from that table are only implemented when a benchmark trips its trigger.

### 3.1 State machine skeleton
- `ClientCore::handle(Input) -> Vec<Effect>`, `view() -> &ViewModel`; `Effect::Render(ViewDelta)` carries only changed sections.
- Draft text is not core state; `Action::DraftOpened{anchor}` / `DraftSubmitted{body}` / `DraftDiscarded` are the only crossings.
- Typestate connection: `Disconnected | Connecting | Subscribed { last_seq }`.
- Tests: every `Input` in every state either transitions or is rejected with a typed error; no panics (`proptest` over random input sequences).

### 3.2 Cache (§5.1)
- `ContentCache` keyed by OID / `(base_oid, head_oid, opts, chunk_index)`; entries are headers and chunks, never whole files; two tiers, each LRU with a byte budget.
- Memory tier in `client-core`; open-review headers and open-file chunks pinned. Disk tier via `Persist`/`Load` effects to the host KV; memory eviction writes through; memory miss → `Load` from disk → only then `Send` to daemon.
- Host KV implementations: Tauri = redb file under the app data dir; browser = IndexedDB; TUI = redb file.
- `TreeSnapshot` cached by `root_oid`, pinned while its ref is open; `TreeDelta` applied in place for working-tree refs.
- Prefetch policy: on review open request tree snapshots for all target refs, then all headers and the first chunk of each file; on file open request viewport chunk then ±2; on tree navigation request sibling entries.
- Viewport tracking: `Action::Viewport { file, first_row, last_row }` drives chunk requests; requests for chunks no longer near the viewport are cancelled (not sent if still queued).
- Tests: memory hit → no effects; memory miss + disk hit → exactly one `Load`, no `Send`; full miss → `Load` then `Send`, concurrent misses deduped; eviction respects both budgets and writes through; pinned entries survive pressure; restart simulation (new `ClientCore` over same KV) serves previous review without `Send`; scrolling a 100k-line file requests only viewport ±2 chunks and never more than N in flight.

### 3.3 Optimistic mutations
- `PendingEvent` list; local apply; on own `CommittedEvent` → drop pending; on foreign → rebase.
- LWW by `Seq` for edits/resolve.
- Tests: **two-client simulator** (`nits_test_support::Sim`) that drives two `ClientCore`s and an in-memory daemon model with controllable delivery order. Cases: concurrent replies to one thread; concurrent edit of same comment (LWW); resolve/unresolve race; disconnect mid-pending then reconnect (pending re-sent exactly once, idempotent by `CommentId`). `proptest`: any interleaving converges both clients to the daemon state.

### 3.4 Deferred refresh (§5.4)
- `Draft` state in `ViewModel`; `ReviewTargetsResolved` queued while a draft is open; drained on submit/discard; new comment anchored to the head at draft-open time and re-anchored by the daemon.
- Tests: refresh during draft → view unchanged + `pending_refresh: true`; submit → refresh applied, comment lands on new head with correct state.

### 3.5 ViewModel
- Merged file tree, current file rows + comment overlays, thread list, review conversation, commit stepper, progress.
- Explorer model built from `TreeSnapshot`: nested tree, expand state, breadcrumbs, fuzzy path index; all client-local.
- `ViewPrefs { layout, ignore_whitespace, context_lines }`, persisted; `Action::SetLayout` emits only `Render`, never `Send`.
- Keymap (§6.4): default `Keymap` table, override loading from KV, chord sequence parser with timeout (`Input::Tick`), `Input::Key` → `Action` resolution by focus context; `ViewModel.hints` (primary bindings for current focus) and `ViewModel.help` (full grouped list when open).
- Tests: every `Action` variant reachable from at least one binding (exhaustive-match test); no two bindings conflict within a context; sequences resolve/expire correctly; `?` in every context yields a non-empty help list; keymap round-trips through the override file format.
- Viewed state derivation (`Viewed | ChangedSinceViewed | Unviewed`) and collapse behaviour; progress counts.
- Commit stepper view from `CommitInfo`.
- Comment→row placement from anchors.
- Tests: `insta` snapshots of `ViewModel` for scenario scripts; placement tests for each `Anchor` variant incl. `Outdated`; expanding any folder or fuzzy-searching produces zero `Send` effects; opening a file produces exactly the header + viewport chunk requests.

Exit criteria: wasm target check passes for `nits-client-core`; simulator suite green.

---

## Milestone 4 — `ui` + Tauri

Goal: usable desktop app.

### 4.0 Scaffold
- `ui/` with ReScript 11 + React + Vite; Tailwind v4 through `@tailwindcss/vite`; `src/styles/app.css` holds `@theme` tokens (light/dark, diff add/remove/context, syntax palette) and the semantic row/cell/span classes (§6.6); `@source` covers `src/**/*.res`.
- CI: ReScript build + `vitest`; a test asserts every `SpanClass`, `Row` kind and `Cell` side has a class in `app.css` (parse the CSS, compare to the protocol fixtures).

### 4.1 Sury schemas + boundary test (§6.3)
- `ui/src/protocol/*.res` hand-written Sury schemas for `ViewModel`, `Action`, and everything they contain.
- Test: for each `fixtures/protocol/**.json`, parse with Sury, serialize, canonicalise, compare. Run in CI; a missing schema for a new fixture fails.

### 4.2 Adapters
- `Core.res` interface; `CoreTauri.res` (`invoke("dispatch")`, `listen("view")`); `CoreWasm.res` stub.
- Tests: adapter unit tests with a mocked Tauri API; assert no IPC message exceeds 64 KB during a scripted session (typing a comment, scrolling a 100k-line file).

### 4.3 Tauri host (`nits-client-tauri`)
- Owns `ClientCore`, a context-aware Local/SSH/WebSocket transport, KV via file, and clock; pushes `ViewModel` diffs to the webview. SSH children are owned and reaped by the connection they carry.
- Tests: host integration test against a real daemon in a temp dir.

### 4.4 Screens
- Review list / create (any base vs any head, multi-repo).
- Merged file tree with progress.
- Diff view: virtualized rows over `total_rows` (`@tanstack/react-virtual`), chunk fetch by index with placeholder rows, unified/split toggle (persisted, instant), hide-whitespace toggle, expanders, inline comment composer, threads, outdated collapse, per-file "viewed" checkbox that collapses the file and a "changed since viewed" badge.
- File explorer over any ref: instant expand/collapse from the snapshot, fuzzy file search, file-level comments.
- Review conversation panel (review-level comments, agent request cards).
- Commit stepper with commit panel: subject, full body, author and committer with relative + absolute times, parent links.
- Suggestions with apply.
- Tests: ReScript component tests for row rendering (each `Row` variant in both unified and split layout), placeholder → chunk swap, composer state; Playwright smoke against the Tauri dev build for the core flow, including opening a 10k-line file and scrolling end-to-end without a long task > 100 ms.

### 4.5 Keyboard UI
- Key capture component: normalises browser key events to `KeyChord`, forwards to core, swallows handled keys; text inputs (composer, search) get raw keys except `Esc`/submit chords.
- Hint bar and `?` help overlay rendered from `ViewModel.hints` / `ViewModel.help` (searchable).
- Focus rings and scroll-into-view driven by `ViewModel.focus`.
- Tests: component tests that the hint bar reflects the focus context; Playwright script performing the full review flow (open review, navigate files/hunks, comment, reply, mark viewed, step commits, toggle layout) using keyboard only — no mouse events.

### 4.6 Polish
- "changes pending" indicator, remote context picker (the underlying Local/SSH/WebSocket transport is shared by all native hosts).

---

## Later
- Browser build (`nits-client-wasm`, daemon serves `ui/`), TUI, cross-machine sync, GitHub export.

## Finding dispositions (#73)

- Implemented explicit reversible deferral of open findings, with validated reason,
  optional HTTP(S) external tracking URL and durable actor/time attribution.
- Core, protocol, MCP, CLI and keyboard-first UI share the same lifecycle. Fresh
  clients distinguish deferred unfixed work from open current-scope findings,
  resolved findings and informational conversation.
- Schema 4 → 5 preserves historical event bodies and rebuilds materialized views;
  protocol 0.9 carries the new disposition and composer purpose.

### Issue #64 — shareable thread/comment references

Typed context-aware references, CLI `reference` / `open`, copy controls in inline
and Conversation cards, configurable thread `y` and reply `[` / `]`, exact-reply
landing, and stale-snapshot/event-race coverage. Browser opening is supported;
desktop OS protocol registration remains future host work.

### Iterative revision rounds (#66)

Implemented captured review requests (including retained working-tree trees),
attributed checkpoints with stable reviewer identities, fresh-state freshness,
requested-revision and checkpoint-delta scopes, MCP/CLI inspection and keyboard
UI controls. Legacy request targets remain explicitly unknown.

### Working-tree Git metadata refresh (#97)

Implemented checkout and Git metadata watching for normal/linked worktrees,
provenance-aware resolution, refreshed directory reuse, captured HEAD retention,
and commit-list subscription refresh. Real-daemon tests cover commits, amends,
ref/index-only changes, branch switches and watcher feedback suppression.
Protocol 0.12 and schema 7 preserve honest unknown historical HEAD provenance.

### MCP cancellable event waits (#108)

Acknowledged event waits may overlap ordinary ordered calls. Each wait owns its
connection and is cancelled on request, session replacement, or MCP input EOF.
Timeouts and concurrent waits are bounded; real-daemon and subprocess tests
cover pipelining, subscription ordering, identities, contexts and disconnects.

### Resilient review creation (#103)

- Review creation retains inputs through validation and daemon errors, obtains
  per-repository defaults from the daemon, guards pending submissions, and
  reconciles lost acknowledgements by stable review identity. Browser recovery is
  tab-local and context-scoped. Form controls use the configurable keymap; shell
  RPC errors are exhaustive. Core race tests and real-browser interrupted-write
  checks cover both committed and uncommitted attempts (#103).
