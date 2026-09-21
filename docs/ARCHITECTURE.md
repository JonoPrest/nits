# Nits — Architecture

Nits is a daemon-backed code review tool. Nits are anchored to content (blobs), not to diffs or line numbers.

Status: **draft v1** — core decisions resolved (§10).

## 1. Goals

- A single always-running **daemon** per machine that owns all state: workspaces, reviews, comments.
- Multiple **clients** (desktop, browser, TUI, CLI, agents) attach to the daemon over the same protocol.
- Clients work over **SSH** to a remote daemon with no perceptible latency: all navigation and typing is served from a local cache; only mutations and cache misses touch the wire.
- **GitHub-style diff review** plus a **file explorer** over any ref, in one UI.
- Review **any base against any head** (branch, commit, tag, working tree), and **step through commits** within a range, seeing each commit's full message, author and dates.
- **Split (side-by-side) or unified** diff layout, switchable instantly; **hide whitespace** diffing as a toggle, and **mark as viewed** per file that auto-clears when that file changes in a later head.
- A **workspace** groups multiple git repos; one review can span repos.
- **Comments** are first-class, persisted, content-anchored, and record provenance (human vs agent, and which agent/session).
- Comments can be **inline** (lines of a blob), **file-level** (a whole file, whether or not it is in the diff), or **review-level** (like a non-inline GitHub PR comment).
- **Agents are peers**: everything a human can do through the UI, an agent can do through MCP/CLI, using the same core API.
- **Keyboard-first**: everything is reachable without a mouse. A persistent hint bar shows the main bindings for the current context; `?` opens a full, searchable help overlay.
- Clients apply mutations **optimistically** and reconcile after the fact.
- State **persists across daemon restarts**; reviews live until explicitly deleted.

Non-goals (for now): multi-machine sync of comments, hosting/PR integration, auth beyond SSH.

## 2. System overview

```
┌─────────────┐  ┌─────────────┐  ┌───────────┐  ┌──────────────┐
│ Tauri app   │  │ Browser     │  │ TUI       │  │ Agent / CLI  │
│ (webview UI)│  │ (wasm core) │  │ (ratatui) │  │ (MCP / nits)   │
└──────┬──────┘  └──────┬──────┘  └─────┬─────┘  └──────┬───────┘
       │ unix sock      │ websocket     │ unix sock     │ MCP (stdio/ws)
       └────────────────┴───────┬───────┴───────────────┘
                                ▼
                    ┌───────────────────────┐
                    │        daemon         │
                    │  transports: unix, ws, mcp  (thin adapters)
                    │  ┌─────────────────┐  │
                    │  │   review-core   │  │  git engine · diff · anchoring
                    │  │   event store   │  │  append-only log + views (redb)
                    │  └─────────────────┘  │
                    └───────────────────────┘
                          ▲ reads repos
                    ┌─────┴─────┐ ┌─────────┐
                    │  repo A   │ │ repo B  │  (a workspace)
                    └───────────┘ └─────────┘
```

Remote use: the client tunnels the daemon's socket/port over SSH (`ssh -L` or `ssh host nits daemon stdio`). SSH is the only auth layer.

## 3. Crate / package layout

```
crates/
  nits-protocol/      wire types: requests, events, view/render models. serde. wasm-safe.
  nits-review-core/   git access, diffing, anchoring, event store, review logic. daemon-only.
  nitsd/              daemon library: socket + websocket transports over review-core, file watcher, launch. No binary — `nits daemon serve` is it.
  nits-client-core/   sans-I/O client state machine: cache, optimistic state, ViewModel. wasm-safe.
  nits-client-wasm/   wasm-bindgen shim around client-core.
  nits-client-tauri/  Tauri host: runs client-core natively, exposes dispatch/subscribe to the webview.
  nits-client-tui/    ratatui host (later).
  nits-cli/           `nits` command; thin RPC client. Used by shell-based agents and scripts.
  nits-mcp/           MCP server library proxying to the daemon. No binary — `nits mcp` is it.
ui/              ReScript + React + Vite. Shared by Tauri and browser.
  src/core/      Core.res interface + CoreTauri.res / CoreWasm.res adapters
  src/protocol/  Protocol.res — hand-written Sury schemas mirroring the Rust types
  src/components/
  src/styles/    app.css — Tailwind v4 entry: `@theme` tokens + semantic diff classes
```

Dependency rule: `nits-protocol` ← everything. `nits-client-core` never depends on `nits-review-core`. Nothing in `nits-client-core` or `nits-protocol` may use tokio, std I/O, threads, `Instant`, or non-`js` `rand` (enforced by a CI `cargo check --target wasm32-unknown-unknown`).

## 4. Daemon

### 4.1 Core API

`nits-review-core` exposes one `Core` type with all operations. Transports (`unix`, `ws`, `mcp`) are adapters that map 1:1 onto it. There is no capability a transport adds; this is what guarantees human/agent parity.

### 4.2 Event-sourced store

All mutable state is an **append-only event log**. Materialized views are derived from it and can be rebuilt.

```
events:            seq (u64) → Event           source of truth
reviews:           review_id → ReviewView
comments_by_review review_id, comment_id → CommentView
review_requests    review_id, event_seq → ReviewRequest
review_checkpoints review_id, event_seq → ReviewCheckpoint
anchors_by_blob:   (repo, blob_oid) → [comment_id]   for fast re-anchoring
workspaces:        workspace_id → Workspace
```

Event kinds (initial set): `WorkspaceCreated/Updated`, `RepoAttached/Detached`, `ReviewCreated/Updated/Deleted`, `ReviewTargetsResolved` (snapshot of resolved OIDs), `CommentCreated/Edited/Deleted/Reanchored`, `ThreadResolved/Unresolved`, `FileViewed/Unviewed`, `ReviewRequested`, `SuggestionApplied`.

Every event carries `{ seq, ts, author, client_id, client_seq }`. `seq` is assigned by the daemon and is the global order. Deletion is a tombstone event; a later GC pass may compact.

Storage engine: **redb** (pure Rust, single file, ACID).

### 4.3 Git engine

- `gix` for object access (trees, blobs, commits, refs). Shell out to `git` for things gix does poorly (rename detection, worktree status) until it doesn't.
- Diffing via `imara-diff`; the daemon produces both raw hunks and a **render model** (see §4.6). `RenderOpts.ignore_whitespace` diffs a whitespace-normalised view of each line while rows carry the original text; a file whose diff is whitespace-only renders as a single collapsed "whitespace changes only" row.
- Working tree is a first-class "ref": `RefSpec::WorkingTree`. A `notify` watcher on each repo invalidates and emits `ReviewTargetsResolved` (debounced) for open reviews targeting the working tree. It watches checkout content plus Git HEAD/index/config/ref metadata, including the private and common Git directories outside linked worktrees. Snapshot indexes, object writes and Nits retention refs are excluded from invalidation. Resolution compares content and provenance (dirty paths, branch, captured HEAD and base commit), so commits, amends and ref-only changes refresh subscribers even when the content tree is identical. Archived reviews keep their last resolved targets, comments and anchors without automatic updates; reopening refreshes their targets and re-anchors comments immediately. Explicit refresh remains available while archived. For open reviews, **holding** the refresh is a client concern (§5.4).
- All content is addressed by OID. Diffs are cached by `(base_oid, head_oid, path, opts)`.

### 4.4 Data model

```
Workspace { id, name, repos: [Repo { id, path, display_name }] }

Review {
  id, workspace_id, title,
  targets: [ReviewTarget { repo_id, base: RefSpec, head: RefSpec }],
  created, status: Open | Archived
}
RefSpec = Branch(name) | Commit(oid) | Tag | WorkingTree | Upstream | Head

CommitInfo { oid, parents, author: Sig, committer: Sig, subject, body }
Sig        { name, email, time: Timestamp, offset }
              returned by commits(review) for stepping; shown in full in the commit panel

ViewedMark { review_id, repo_id, path, viewer: Human, blob_oid }
              "viewed" is bound to the head blob seen; if the current head blob differs the
              file shows as changed-since-viewed and the mark is cleared in the UI.
              Human-only: agents cannot set it.

RenderOpts { ignore_whitespace: bool, context_lines }
              part of every render cache key; whitespace-ignored rows keep the real text,
              only line pairing/`changed` ranges differ

Comment {
  id: ulid,               client-generated → enables optimistic create
  review_id, thread_id,
  author: Author,
  kind: Note | Informational | Suggestion { patch } | Request,
  anchor: Anchor,
  body, created, edited,
  state: Live | Outdated { last_good_anchor } | Deleted
}

Author = Human { name, machine }
       | Agent { name, model, session_id, invoked_by: Option<Human>, via: Mcp | Cli }

Anchor
  = Review                                   review-level, no file
  | File  { repo_id, path, blob_oid }        whole-file, any ref; need not be in the diff
  | Lines { repo_id, path, side: Base | Head,
            blob_oid,                        exact blob the comment was made on
            lines: Range,                    in that blob
            context_hash }                   hash of ±3 surrounding lines

Anchors reference blobs, not diffs. Opening a file in the explorer and commenting
on it uses the same path as commenting inside a diff; the diff is just a way to
navigate to a blob.
```

A root's kind sets its thread lifecycle: `Note`, `Suggestion` and `Request` create
`Open` actionable threads; `Informational` creates a review-level conversation with
`ThreadResolution::Informational`, which cannot be resolved or reopened. Replies
retain that lifecycle and the root's anchor. Informational notes imply no approval,
merge readiness, or resolution of other findings. Existing `Note` roots remain
findings, including review-wide notes: no history is reclassified automatically.
MCP `add_comment.intent` and CLI `comment add --intent` select `Finding` (default)
or `Informational` (`finding`/`informational` in CLI).

An open finding can be explicitly `Deferred { reason, tracking_url, by, at }`:
acknowledged outside the current review scope, still a real unfixed finding.
The reason is required and nonempty; an optional external tracking URL must be
absolute HTTP(S), without credentials or whitespace. `ThreadDeferred` retains
the recording actor/time in the append-only log. Replies and anchors survive;
reopening returns the finding to `Open`, and explicit resolution records a fix.
A deferral can only be recorded from `Open`; reopen first to change its reason,
so concurrent deferrals cannot silently overwrite attribution. Informational
threads cannot be deferred. No disposition implies approval or deployment safety.
Browser and desktop actions share the host's validated JSON boundary. A rejected
submission returns an error in the active draft, preserving the editor's text
and allowing correction without committing an event.
MCP `defer`, CLI `comment defer --reason ... [--tracking-url ...]`, and the UI use
the same Core mutation. `resolve` with `resolved: false`, CLI `comment reopen`,
and the UI's reopen action restore current-scope work. Fresh review snapshots
and `list_comments` include the full disposition and discussion.

### 4.5 Re-anchoring

When a review's resolved head/base changes:

1. same `blob_oid` → anchor unchanged.
2. blob changed → diff old→new blob, map `lines` through the diff.
3. mapped region's context hash mismatches → mark `Outdated`, keep last good anchor, still shown (collapsed) in the UI.

`File` anchors follow the path, including detected renames; they become `Outdated` only if the
file disappears. `Lines` anchors follow renames the same way. `Review` anchors never change.
Browse comments record `CommentContext::Browse { reference }` and stay pinned to their
original blob; review-target refresh does not reanchor them through an unrelated tree.
Their reference is provenance, not a moving content pointer. Thread navigation reopens
the anchor blob, including persisted working-tree snapshots. Diff comments record
`CommentContext::Diff { change }`; schema 4 wraps legacy diff contexts in this variant
and leaves absent contexts absent.

Comments are never dropped by ref movement, and an `Outdated` comment is re-tried from its last
good anchor on every resolution, so it returns to `Live` when the content does.

The `context_hash` covers the anchored lines plus 3 on each side. The daemon computes it from
blob content when a comment is created (a client-supplied value is replaced) and rejects line
ranges beyond the blob's length.

Re-anchoring runs off the core actor: the actor records `ReviewTargetsResolved`, then the blob
diffs run on the blocking pool and emit one `CommentReanchored` event per comment as they finish.
Clients show affected comments as "re-anchoring" in the interim rather than the daemon stalling.

### 4.6 Diff render model

Three levels of diff data:

1. **Raw diff** — hunks of `+/-/context` lines from git. Daemon.
2. **Render model** — the flat list of rows the screen shows. Daemon. Pure function of
   `(base_oid, head_oid, path, opts)`, cached on disk, identical for every client.
3. **Overlays** — comment threads, selection, hover. `nits-client-core` / UI.

Render model rows:

```
Row = HunkHeader { text }
    | Context    { left: Cell, right: Cell }
    | Removed    { left: Cell }
    | Added      { right: Cell }
    | Modified   { left: Cell, right: Cell }     paired -/+ with intra-line ranges
    | Expander   { hidden: u32, dir }            "show N more lines"
Cell = { line_no, text, spans: [{ start, end, class }], changed: [Range] }
```

Building rows involves pairing `-`/`+` lines for split view, intra-line word diff, context
collapsing, and syntax highlighting (tree-sitter/syntect, native, run once). Unified vs
split is a UI choice over the same rows. Whole-file views (explorer) are the same `Cell`
list with no diff rows.

Comment → row placement is done in `nits-client-core` (anchor `blob_oid + lines` → row by
`line_no` per side) so the daemon's render model stays comment-agnostic and cacheable.

### 4.7 Tree snapshots (file explorer)

The explorer must never load per folder. The daemon serves a whole recursive listing in one
message, keyed by the root tree OID:

```
TreeSnapshot { root_oid, entries: [TreeEntry { path, kind: File | Dir | Symlink | Submodule,
                                                oid, size }] }   flat, sorted, one pass to nest
TreeDelta    { from_root, to_root, added: [TreeEntry], removed: [path], changed: [TreeEntry] }
```

- `tree_snapshot(repo, ref)`; for reviews the daemon sends snapshots for every target ref (base
  and head) on open. Cached by `root_oid`; pinned while the ref is open.
- Working-tree refs get `TreeDelta`s from the watcher instead of repeated full snapshots.
- Fallback for very large repos (> ~200k entries, configurable): depth-limited snapshot plus lazy
  subtrees keyed by their own tree OID — same caching, not upfront.

### 4.8 Transports

- **Unix socket**, length-prefixed JSON frames. Multiplexed: `Request{id}` / `Response{id}` / `Event{seq}`.
- **WebSocket**, same JSON envelopes, one per binary (or text) message — the socket does the framing, so no length prefix. Plain TCP, opt-in via `nits daemon serve --ws-listen <addr>`, for browser clients and remote daemons. Inside `nitsd`, servers and context-aware clients share `FrameRead`/`FrameWrite`; `DaemonEndpoint = Local | Ssh | WebSocket` resolves lifecycle once and every UI reconnect dials the selected framing through the same path as CLI/MCP.
- **MCP**, `nits mcp` on stdio (newline-delimited JSON-RPC), proxying to the daemon's unix socket or ws port. Tools: `list_contexts`, `use_context`, `list_workspaces`, `list_reviews`, `get_review` (snapshot + changed files), `create_review`, `ensure_directory_review`, `update_review`, `update_review_target`, `get_diff`, `get_file` (numbered text, any side, unchanged files too), `list_comments`, `add_comment` (review / file / line anchors), `suggest`, `reply`, `resolve`, `defer`, `request_review`, `subscribe_events` (long-poll; pass `last_seq` back as `since_seq`). Author is `Agent{name: clientInfo.name, model: $NITS_AGENT_MODEL, session_id: $NITS_SESSION_ID, invoked_by: $USER@host, via: Mcp}`. `mark_viewed` is deliberately not offered. Anchors go up with a zero `context_hash`; the daemon computes the real one.
- **MCP mutation results** are compact receipts in both text and structured content. `create_review` returns `{review_id, seq}`; `update_review` returns `{review_id, status, seq}`; `update_review_target` returns `{review_id, repo_id, seq}`; `add_comment`, `suggest`, and `reply` return `{comment_id, thread_id, seq}`; `resolve` returns `{review_id, thread_id, resolution, seq}` with `resolution` as `"Open"` or `"Resolved"`; `defer` returns `{review_id, thread_id, seq}`; fresh `get_review`/`list_comments` snapshots include the complete `Deferred` disposition with reason, optional tracking URL and actor/time; `request_review` returns `{request_id, review_id, agent, seq}`. These replace the former nested `event` fields, and `create_review` no longer embeds `review` or `resolved` (use `get_review`). `seq` identifies the committed mutation's primary event, not a later snapshot watermark. Passing it to `subscribe_events.since_seq` yields subsequent full event envelopes, including target resolution and reanchoring caused by the mutation. To include the mutation itself, resume from a cursor before its `seq` (or `seq - 1`); continue subsequent polls from `last_seq`. Full heterogeneous event schemas appear only on `subscribe_events`. Daemon/CLI event responses are unchanged.
- **Directory bootstrap**: `ensure_directory_review {path, base?, head?}` discovers and canonicalizes the checkout beside the daemon, attaches it if needed, and returns `{workspace_id, repo_id, review_id, base, head, outcome, seq}`. `head` defaults to `WorkingTree`; omitted `base` preserves a matching open review's base or uses daemon detection. Explicit refs must match for reuse; differing refs create a separate review. To change the same logical review, use `update_review_target {review_id, repo_id, revision: {type: "Base" | "Head", ref_spec}}`; it retains threads/history and reanchors comments. Bootstrap validates refs before creating state and runs under the writer queue so concurrent calls reuse the same IDs. Reuse re-resolves the matching review before returning it, even if no watcher is running. Its `seq` is the first bootstrap event when created and the current log position after resolution when reused. CLI directory opening uses the same operation; remote paths are never resolved on the client.
- **CLI/MCP connection loss**: EOF, malformed frames, and failed writes close both transport halves and fail all pending requests; request registration shares the closure lock. Streams report interruption instead of accepting a partial render. MCP preserves initialization and agent provenance, reconnecting before the next tool call with the selected context/start policy and a 20-second connection deadline. Interrupted calls are never replayed automatically: a mutation may have committed before its reply was lost, so the tool error asks the agent to inspect state before repeating it.
- **CLI**, `nits`, the same recipes (`nitsd::ops`) printed as text or `--json`. Human author from `$USER`/`--user`; `--agent NAME` attributes to `Agent{via: Cli, invoked_by: user}` for scripts driven by an agent. `events --follow` is a loop of long-polls resuming from `last_seq`.
- **Contexts** (`nits-config`, `~/.config/nits/config.toml`): named places a daemon lives — `Local{data_dir?, socket?}`, `Ssh{host, bin?, args}`, `Ws{url}`. Selection precedence is ad-hoc transport flags (`--socket`, `--data-dir`, `--daemon-url`; deprecated alias `--ws`) > `--context` > `NITS_CONTEXT` > persisted `current_context` > implicit `local`. `nits context use NAME` validates the name and atomically saves the default, including offline contexts; it affects new processes. `context show` reports the effective selection and its origin. Removing the persisted default is refused until another is selected. No current workspace/review is stored: workspace and repo default from the **working directory** (`Ops::locate`). Commands: `nits context add-local|add-ssh|add-ws|list|show|use|remove`. Context names are validated at CLI parsing and config decoding: nonempty, with no surrounding whitespace or control characters.
- **MCP context switching**: `nits mcp` resolves its startup selection with the same precedence. `list_contexts` reloads configured definitions and reports the active context and persisted default; `use_context {name}` changes only that MCP session. The adapter completes the new daemon handshake before replacing the active endpoint and entire `Ops`; failures leave the old connection intact. Calls execute in order, so no previous call is in flight across the swap. Old subscriptions and queued events are dropped. The author, session ID and invoking human survive; the new connection gets a fresh client ID and mutation sequence. Reconnects use the selected endpoint, even if the config default later changes. Every daemon query result includes `context: {name, kind}` (`kind` is `Local`, `Ssh` or `Ws`). IDs and event cursors belong to that source context. `subscribe_events.since_context` identifies the name that issued `since_seq`: mismatches are rejected, and it is required for replay after any successful context switch (legacy single-context calls can still omit it). Long-running polls finish before a queued switch; clients must await `use_context` before issuing work for the newly selected daemon. Context configuration and selection remain adapter concerns, outside Core.

- **One binary** (`nits`). The daemon and the MCP server are subcommands, not executables: `nits daemon serve` and `nits mcp`, linked in from the `nitsd` and `nits-mcp` libraries. `nitsd::launch::nits_binary` starts a daemon by **re-executing the running `nits`** (`$NITS_BIN` overrides; other embedders such as the desktop app look next to themselves, then `PATH`), so a client and the daemon it started are the same build and cannot fail the version handshake — the failure two separately-packaged binaries invite every time a channel updates one and not the other. It also collapses install to one artifact per channel and removes the `Depends: nitsd` relationships from the deb/rpm.
- **Daemon lifecycle** (`nitsd::launch`, `nitsd::serve`, `nitsd::contexts`): **one daemon per machine**; the store is single-process so nothing else may open it. `nits daemon stdio` (what `ssh host nits daemon stdio` runs) is a *proxy*: it connects to the machine's socket, starting a detached daemon first if nothing answers (`nohup`, log in `<data_dir>/nitsd.log`, `--idle-exit 1800` so an auto-started daemon retires itself), then pipes bytes. With `--start-policy require-running` it exits 3 instead of starting, which is how a client probes or stops a remote without waking it. `Local` and `Ssh` contexts default to `--start-policy start-if-needed`; `Ws` is somebody else's daemon. `Request::Shutdown` → `Response::ShuttingDown` stops any daemon from any client. `nits daemon status [--all] | start | stop` is the CLI face; the desktop app shows the same per-context status and buttons; `nits mcp` auto-starts on `initialize` so an agent can always get going. Ssh specifics (keys, jumps, ports) stay in `~/.ssh/config`; `Context::Ssh.ssh` overrides the client binary (tests use a stand-in), `Context::Ssh.bin` the remote `nits`. That field is a `RemoteBin` — `Default | Nits(path) | Legacy(path)` — parsed once at the config boundary from the two wire spellings, so nothing downstream sees "both keys" or has to pick: `bin` and `nitsd` together are refused as a config error, and a lone pre-one-binary `nitsd = "..."` becomes `Legacy`, which names an executable that cannot serve `daemon stdio` and which `connect` refuses with the edit to make.
- **Daemon serving selection**: `daemon serve` and `daemon stdio` bind this machine's socket. They ignore `current_context`, `NITS_CONTEXT`, and `NITS_WS_URL`, and bypass even a configured context called `local`. Explicit `-c NAME` can select a configured local binding; explicit remote contexts or `--daemon-url` are rejected. `--socket` / `--data-dir` (including `NITS_SOCKET` / `NITS_DATA_DIR`) retain their usual precedence over named contexts. Client lifecycle commands `daemon status|start|stop` still use normal context selection. This keeps SSH destination config from redirecting or breaking the proxy.
- **Upgrade shutdown**: when the current handshake is rejected, `daemon stop` may redial using the newest advertised protocol that the current major can still serialise. Only the stable `Shutdown`/`ShuttingDown` lifecycle exchange gets this fallback; ordinary requests never pretend to speak a rejected version. This releases an older daemon's socket so the upgraded `nits daemon start` can launch its matching binary.

- **Subscriptions**: `subscribe(scope, since_seq)` streams events from `since_seq`. An explicit cursor replays the requested gap even on the same connection after earlier delivery. Long-polls acknowledge only the events returned to the caller; queued events beyond a poll's limit or timeout remain replayable. Keep a cursor for each scope when alternating reviews. Reconnect = resubscribe from last seen seq; no other sync mechanism.
- **Review open is one streamed request.** `open_review(id)` answers with an ordered stream — `ReviewSnapshot` (review, threads, comments, requests) → `TreeSnapshot` per target ref → `FileRenderHeader` per changed file → first `RenderChunk` per file — rather than the client issuing hundreds of round-trips over SSH. The client consumes and its cache fills as a side effect; per-item requests remain for cache misses and viewport-driven chunks.
- **Fresh clients never replay the log.** A client with no `last_seq` gets a materialized `ReviewSnapshot` plus `subscribe(since = current_seq)`. Only reconnects with a known `last_seq` replay, and only the gap.

Encoding is an isolated layer; the Rust↔Rust hop may move to capnproto/flatbuffers later if measured to matter. JSON is the fixed contract between `nits-client-core` and the UI.

`get_file` accepts optional `start_line` and `end_line`, supplied together as an
inclusive, 1-based range. Both omitted (or null) keep the full-file text. Zero,
reversed, incomplete, and non-integer bounds are rejected. The response preserves
the review-side pinned `blob_oid` and absolute line numbers, including unchanged
files and `Base`. Text responses add `lines: { total_lines, returned_range }`;
`returned_range` is `{ start, end }` for the actual lines returned. The end clamps
at EOF; a start beyond EOF or an empty file returns empty text and a null range.
A trailing newline adds no extra source line. Binary full-file reads keep their
placeholder and return `lines: null`; bounded binary reads fail with a tool error.
`content` still describes the full blob render. Selection uses the shared text
formatter over the existing Core blob render: the MCP payload is bounded, but
daemon rendering and the daemon-to-MCP stream still collect the full file.

### 4.9 Versioning and evolution

Two independent versions, both typed in `nits-protocol::version`.

**Wire protocol — `ProtocolVersion` (semver string, e.g. `"0.1.0"`).**

- Every frame is an `Envelope { v: ProtocolVersion, msg }`, so the version is on each message,
  not only at handshake. One socket/port serves all versions; the version selects how the
  daemon *serialises*, not where the client connects.
- Handshake: the client's first frame is `Hello { client_id, protocol, client: BuildInfo }`.
  The daemon answers `Welcome { protocol, daemon, schema, upgrade }` — `protocol` is the
  version all following frames use — or `Rejected { UnsupportedProtocol { requested, supported } }`
  and closes.
- Compatibility rule: same `major`, daemon `minor >= client minor`. Minor bumps are additive
  (new variants/fields); the daemon serialises responses at the client's requested minor so a
  strict (`deny_unknown_fields`) older client never sees fields it doesn't know. Major bumps
  are never bridged silently.
- Deprecation path: a daemon may keep serving an old minor for a time and attach
  `Welcome.upgrade: UpgradeNotice { latest, message }`; clients surface it. Once dropped, the
  handshake is rejected with the supported list, so the error is specific and actionable.
- Protocol 0.11 currently serves **only minor 0.11**: older minors are retired and
  rejected during Hello, before any event or snapshot. The daemon has one serializer;
  the same-major compatibility predicate alone does not prove it can encode an old
  minor. Adding a supported minor requires its serializer. The stable shutdown-only
  fallback can still stop older same-major daemons during upgrades.
- A frame whose `v` differs from the negotiated version is answered with `VersionMismatch`.
- Bumping: any change to a fixture under `fixtures/protocol/` requires bumping
  `ProtocolVersion::CURRENT` (minor if additive, major otherwise); CI diffs fixtures.

**Store schema — `SchemaVersion` (monotonic integer).**

- Stamped in the redb `meta` table on creation. `SchemaVersion::CURRENT` is what this build
  writes.
- On open: equal → proceed; older → run migrations forward in one transaction per step and
  restamp; newer → refuse to open with a clear error (a newer `nits` wrote this; upgrade).
- Events are stored as JSON with a per-event `schema` tag, so the event log itself migrates by
  re-serialisation, and materialised views can always be rebuilt from the migrated log.
- Schema 2 adds informational kinds/lifecycles. The 1→2 migration restamps raw
  event envelopes and rebuilds views, retaining old Note roots and all
  replies/resolutions unchanged. Older binaries refuse the schema 2 store.
- Schema 3 adds the review-request view. The 2→3 migration rebuilds views from
  unchanged historical events, including requests sent before this upgrade.
  Older binaries refuse the schema 3 store.
- Schema 4 distinguishes Diff and Browse comment provenance. The 3→4 migration
  wraps legacy non-null diff contexts, preserves absent contexts and all history,
  and rebuilds views. Earlier migration steps retain raw payloads until this
  transformation runs, so schema 1, 2 and 3 stores can upgrade directly.
- Schema 5 adds deferred finding dispositions. The 4→5 migration restamps event
  envelopes and rebuilds views, preserving Browse provenance, requests,
  informational notes and historical resolutions. Older binaries refuse this store.
- Schema 6 adds revision capture and the checkpoint view. The 5→6 migration
  rewrites raw historical request events with `RequestedTargets::Unknown` and
  rebuilds views. Earlier envelope migrations also operate on raw JSON before
  current event decoding, preserving every supported starting schema.
- The daemon reports `schema` in `Welcome` for diagnostics only; clients never depend on it.

## 5. Client core (sans-I/O)

`nits-client-core` is a pure state machine. It performs no I/O; the host injects everything.

```rust
pub struct ClientCore { ... }

impl ClientCore {
    pub fn handle(&mut self, input: Input) -> Vec<Effect>;
    pub fn view(&self) -> &ViewModel;
}

pub enum Input  { User(Action), Server(ServerMsg), Stored(Key, Bytes), Tick(Millis) }
pub enum Effect { Send(ClientMsg), Persist(Key, Bytes), Load(Key), Render }
```

Hosts (Tauri, wasm, TUI) own: transport, local KV store, clock. This makes the core testable without mocks and compilable to wasm.

### 5.1 Cache

Content-addressed, so never stale: blobs by OID, trees by OID, diff render models by `(base_oid, head_oid, opts)`. On opening a review the daemon streams the full diff set and touched blobs; the file explorer prefetches siblings. Cache hit ⇒ zero-latency navigation.

Two tiers, both LRU with a **byte budget**:

1. **Memory** (default 256 MB) — inside `client-core`. Entries for the currently open review are pinned and never evicted.
2. **Disk** (default 2 GB) — via the host KV store (`Persist`/`Load` effects). Memory eviction writes through to disk; a memory miss checks disk before asking the daemon. Survives client restarts, so reopening a review over SSH is served locally.

Both budgets are configurable. Because keys are OIDs, on-disk entries never need invalidation; the disk tier is only ever trimmed by LRU or cleared explicitly.

**Local daemon ⇒ no client disk tier.** When the client connects to a unix socket on its own host, the daemon's `render-cache.redb` already holds every header and chunk, and a local socket round-trip is sub-millisecond, so the client runs memory-only and misses go to the daemon. The disk tier is enabled only for remote daemons (SSH/WebSocket). This avoids a second copy on disk without sharing a file between processes: redb is single-process (exclusive lock), so "daemon writes, client reads the same tables" is not possible without changing the store engine. If a shared local cache is ever wanted, `RenderCache` is the isolated seam to swap for sqlite.

Cache entries are `TreeSnapshot`s (§4.7), render headers and render **chunks** (§4.6), never whole files. Chunks of the open file are pinned while it is open; on close they return to normal LRU.

**Two open flows.** With no disk tier the client sends one `OpenReview` and the daemon streams snapshot, trees, headers and first chunks; the stream fills the cache directly. With a disk tier it opens *piecewise* — `ReviewSnapshot`, then `ListFiles`, then every tree and header by cache key through the normal miss path — so a restart is served from the KV with no content request at all. Content the daemon sends is persisted **on arrival**, not only on memory eviction: the open review's entries are pinned and would otherwise never reach disk. The disk LRU index is session-local (entries written by earlier sessions are counted once they are loaded again); trimming is an `Effect::Remove`.

### 5.2 Optimistic mutations

1. Client generates the ULID, applies the event locally (marked `pending`), emits `Send`.
2. Daemon assigns `seq`, broadcasts to all subscribers including the originator.
3. On seeing its own event, client clears `pending`. On foreign events, it re-applies pending events on top.
4. Conflicts are limited to edits of the same comment and resolve/unresolve toggles: **last writer by `seq` wins**; the view re-renders.

### 5.3 Client-local state

Ephemeral UI state (hover) lives in the UI. Navigational state (focus, open file, expanded dirs, scroll, drafts) lives in `ViewModel` so all hosts behave identically; drafts are `Persist`ed via the injected KV.

**Derived view.** The explorer, diff rows with overlays, thread list, conversation, stepper, focus (clamped to the current lists), hints and help are recomputed from core state after *every* accepted input and compared with what the view held; `handle` then emits at most one `Effect::Render` per input naming the union of sections that changed, after every other effect. Hosts never see a stale panel or an interleaved render.

### 5.4 Deferred refresh

Working-tree reviews auto-refresh, but `nits-client-core` will not swap in a new render model while the user has an open comment editor. Incoming `ReviewTargetsResolved` events are queued; when the draft is submitted or discarded, the queue is drained, the comment is re-anchored against the new head, and the view updates. The UI shows a subtle "changes pending" indicator while held.

### 5.5 File explorer

Folder expand/collapse, breadcrumbs and fuzzy file search (`Cmd+P`) operate entirely on the
cached `TreeSnapshot` — no requests. Only opening a file fetches content (header + chunks),
which is then cached. Switching ref prefetches that ref's snapshot.

### 5.6 Multi-repo tree

A review across repos presents one merged file tree with each repo as a top-level root (`repo-a/…`, `repo-b/…`). Progress ("N of M files viewed"), comment lists and navigation are review-wide, not per repo.

## 6. UI (ReScript + React)

### 6.1 Principle

The UI is a renderer over `ViewModel` and a source of `Action`s. It never talks to the daemon and contains no reconciliation logic.

### 6.2 Adapters

`Core.res` defines `dispatch: Action.t => unit` and `subscribe: (ViewModel.t => unit) => unsubscribe`. Two implementations are chosen at startup: `CoreTauri.res` (`invoke`/`listen`) and `CoreWs.res` (JSON commands and patch batches over a browser WebSocket).

The browser bridge prepares process resources once, then gives every
WebSocket its own `ClientCore`, typed client identity/id seed, host task and
daemon connection. Navigation, focus, cursor and key-verdict state therefore
belong to one tab. Only the host KV is shared, for preferences and
content-addressed entries; its redb handle is opened once rather than once per
tab. Closing a socket cancels its connection (including an owned SSH child),
and reconnecting creates a fresh session whose empty model replaces the old
one before attach.

Before creating that session, the bridge checks `Host` on every HTTP request
and requires an approved `Origin` on the exact `GET /ws` upgrade. By default,
the origin must match the request's host and the actual listener address/port
(or `localhost` on loopback). Arbitrary DNS names, opaque `null` origins,
missing origins and duplicate Host/Origin headers are rejected. Native clients
use the daemon protocol rather than an Origin-free browser session. Host
validation also protects the embedded UI from DNS rebinding.

Development/proxy UI origins are explicit, per launch: run
`cargo run -p nits-client-web -- --allow-origin http://localhost:5173` alongside
`pnpm --dir ui dev`, substituting the exact origin Vite prints (no trailing
slash). Repeat `--allow-origin` for additional trusted UI origins. The Vite
proxy preserves Host and Origin; a configured proxy may instead send the
bridge's Host, but must preserve the browser's Origin. Forwarded headers do
not grant trust. A separately served UI using `?ws=` needs the same explicit
origin grant. HTTPS proxy origins are supported explicitly, including their
public Host; the bridge itself serves HTTP.

No capability token is needed for this boundary: the served UI contains no
secret, browsers cannot forge Origin, and neither Origin nor Host is inferred
from an untrusted forwarded header. This is browser isolation, not client
authentication: non-browser programs can supply both headers. Keep the bridge
on loopback (as both CLI launchers do); remote daemon access remains protected
by SSH. Adding public bridge hosting would require authentication separately.

The real Chromium regression runs against a disposable daemon and the actual
Vite proxy, proving supported origins can read and mutate while unrelated and
opaque origins cannot. After building the UI, run
`pnpm --dir ui exec playwright install chromium`, then
`cargo nextest run -p nits-client-web --run-ignored only -E 'test(real_browser_origin_boundary)'`.
CI runs this test explicitly; the ordinary Rust suite does not require a browser.

### 6.3 Type bridge

The Rust `ViewModel`/`Action`/`Event` types and their ReScript counterparts are both **hand-written types**; on the ReScript side the Sury (`rescript-schema`) schema is derived from the type by the `@schema` ppx (`@as` for field names, `@tag("type")` for enums, `@s.null` for `Option`), which gives static types plus a validator. The adapters parse at the boundary so drift is caught at runtime, not deep in a component. Rust enums use `#[serde(tag = "type")]` so they map to Sury tagged unions — and, because `@tag` also names the runtime tag field, ReScript values have exactly the wire shape.

Drift is prevented by a **boundary test**: Rust emits a JSON fixture for every type/variant (serialize, and check it deserializes back); a ReScript test parses each fixture with the Sury schema and re-serializes it, and the outputs must match byte-for-byte after canonicalisation. Both directions are covered, so a protocol change that isn't mirrored fails CI.

### 6.4 Keyboard model

Every user operation is an `Action`; keybindings are a data table mapping `(Context, KeyChord) → Action`, not ad-hoc handlers in components.

```
Keymap  { bindings: [Binding { context, chord, action, label, primary: bool }] }
Context = Global | ReviewList | Tree | Diff | Thread | Composer | CommitStepper | Help
```

- `client-core` owns the keymap (default table + user overrides from the host KV) and resolves
  `Input::Key(chord)` → `Action`. The table maps `(Context, KeySeq)` to a unit-only `Command`
  ("next file"); `resolve` then turns the command into the one `Action` it means against the
  current focus and view, or a typed `NoTarget`. Commands whose payload is text (a comment
  body) open a draft; the host submits the text. The UI captures keys, sends chords, and renders
  the results — it contains no key → behaviour logic, so bindings are identical across hosts
  (Tauri, browser, TUI) and testable without a DOM.
- Chords support sequences (`g g`, `] c`) with a short timeout; vim-style movement
  (`j`/`k`, `n`/`p` next/prev hunk, `] f`/`[ f` next/prev file, `] c` next comment,
  `v` mark viewed, `c` comment, `r` reply, `Enter` open, `Esc` back, `Cmd/Ctrl+P` file search,
  `s` toggle split/unified, `w` toggle whitespace).
- **Hint bar**: the UI renders the `primary: true` bindings for the active context along the
  bottom edge, from the keymap — never hand-written.
- **`?` help overlay**: all bindings for the active context plus Global, grouped and searchable,
  generated from the same table. Shows user overrides and conflicts.
- Focus is explicit state in the `ViewModel` (`focus: Context + target`), so "what does `j` do"
  is always determined by core state, not DOM focus.

### 6.5 Diff rendering

The UI renders the daemon's render model (§4.6) plus `nits-client-core` overlays as a virtualized list (`@tanstack/react-virtual`). Unified vs split is a view option on the same rows. Review-level comments render in a review "conversation" panel; file-level comments render at the top of the file view.

### 6.6 Styling

Tailwind v4 via `@tailwindcss/vite`; no CSS-in-JS, no runtime style computation. Utilities style the chrome (panels, review list, tree, hint bar, help overlay). The render model (§4.6) is styled through a **small semantic class set** — one class per `Row` kind, `Cell` side and `SpanClass` (`row-add`, `cell-old`, `span-kw`, …) defined once in `app.css` with `@utility`/`@layer components` — so a 10k-row virtualized diff carries short class names, not repeated utility strings. Colours (light/dark, add/remove/context, syntax palette) are `@theme` CSS variables, one token list the TUI palette can mirror. Because focus is core state (§6.4), focused rows/panels get `data-focused` and are styled with `data-[focused]:` variants, never `:focus`. `.res` sources are listed via `@source` so class scanning sees them.

## 7. Agent integration

- Agents connect via MCP (or `nits` CLI) with `Author::Agent{...}` provenance. Provenance is a structured field, not a tag.
- MCP `get_session_identity` returns this session's current `author`; `set_session_identity` takes a name and model for future events. The adapter negotiates a replacement daemon connection, preserving the session ID, invoking human and `Agent`/`Mcp` provenance. A fresh client ID accompanies the fresh mutation counter. Only after a successful handshake does the new identity become active; failure leaves the previous identity and connection intact. Reads need no live daemon. These are session-local connection settings, not core mutations or event-log entries, and cannot change another MCP session or historical authors. See [MCP identity usage](MCP-IDENTITY.md).
- `ReviewRequested` events materialize as `ReviewRequest { id: ReviewRequestId,
  review_id, requester, recipient, note, created }`. Identity is the committed
  event sequence wrapped in its own type, stable across schema upgrades and
  rebuilds. Requests carry no approval, resolution, or revision checkpoint.
- Review snapshots, MCP `get_review` / `list_comments`, and CLI `review show`
  include a separate `requests` collection. Fresh clients discover requests
  without historical replay. All snapshot fields and its subscription cursor
  come from one redb read transaction; clients retain live invitations received
  ahead of an in-flight snapshot response and merge them by identity.
- Human Conversation shows request cards and a separate request count. `g r`
  focuses the list, `j` / `k` select a card, and Enter opens the review's current
  changes. Requests never enter finding counts or resolve/approval controls.
  Agents can subscribe to subsequent requests addressed to them (`awaiting_agent`)
  starting from the snapshot's cursor.
- The exact `author.name` returned by the identity tools is the routing key for `request_review.agent` and `subscribe_events.awaiting_agent`. Choose distinct names for collaborating agents and keep them stable; changing a name does not rename already addressed requests or saved subscription cursors. A model update can keep the same name. Names are labels, not unique session IDs or authentication credentials.
- **Suggestions**: a comment kind carrying unified diff hunks against a specific `blob_oid` (an optional `---`/`+++` file-header pair is accepted). The UI renders "apply", which writes to the working tree and records `SuggestionApplied`. Both old/new hunk ranges and body counts must agree. Context and removed lines match exact bytes, including CRLF; added lines carry their literal terminators. Use the standard `\ No newline at end of file` marker after an unterminated old/new/context line. A LF-only patch against CRLF content is rejected, not normalized; binary/full multi-file diffs and truncated body lines are unsupported. Invalid patches leave the file and event log unchanged.
- Threads keep agent `session_id`, so a human reply to an agent comment can be routed back to that session.

## 8. Remote / SSH

The daemon is unaware of remoteness. Every native host can dial a local socket, a configured WebSocket, or `ssh host nits daemon stdio`; SSH remains the authentication layer. Disconnect preserves the client's last event sequence, and an explicit reconnect creates a fresh transport (and a fresh, owned SSH child) before replaying only the event gap.

The stdio proxy half-closes daemon input when client stdin reaches EOF, then
drains remaining responses to stdout. Daemon EOF or reset ends the proxy even
while client stdin remains open. Its cancellable stdin worker is joined and
restores descriptor flags on every exit, including forwarding-future cancellation;
pipes, regular files, terminals, sockets and `/dev/null` retain their input semantics.
Transport errors are reported without reconnecting or replaying requests.

## 9. Persistence & lifecycle

- One daemon per machine, data dir `~/.local/share/nits/` (`state.redb`, logs, per-repo diff cache).
- Reviews persist until `ReviewDeleted`. Deletion tombstones; compaction is offline and optional.
- Daemon restart: reopen store; clients resubscribe from `last_seq`.

## 10. Measure before optimising

Suspected bottlenecks with a ready solution, deliberately **not** built until a benchmark in the plan shows they matter. Each has a trigger and a candidate fix.

| Suspect | Trigger to act | Candidate fix |
|---------|----------------|---------------|
| Working-tree snapshot cost on large repos | snapshot > 100 ms after a single-file edit in a 50k-file repo | rehash only watcher-reported paths; ignore rules at the watcher (`.gitignore`, `target/`, `node_modules/`) so builds don't storm it |
| Rename detection on big add/delete sets | `changed_files` > 500 ms on a directory move | cap candidates (like `diff.renameLimit`); compute renames after the header stream and patch the tree with a follow-up message |
| redb write rate under agent load | appending 200 comments in a loop > 1 s, or fsync visible in profiles | batch appends per actor tick; keep ephemeral state (viewed flags) out of the durable log |
| Event log growth | log > 1M events or startup rebuild > 1 s | offline compaction of tombstoned reviews (design already permits it) |
| Cold start over SSH with empty cache | first usable frame > 1 s on a 300-file review | already mitigated by streamed `open_review` order; further: gzip frames, lower first-chunk size |
| Depth-limited tree fallback | `tree_snapshot` > 200 ms or > 5 MB | lazy subtrees (§4.7) — implement only when a real repo trips it |

## 11. Decisions

### Resolved

| # | Decision | Choice |
|---|----------|--------|
| 1 | Store engine | redb |
| 2 | Wire encoding | JSON everywhere; encoding layer isolated so Rust↔Rust can change later |
| 4 | Diff render model location | daemon (§4.6) |
| 5 | Syntax highlighting | daemon, token spans in render model |
| 6 | Review identity | persistent object, cheap to create, lives until deleted |
| 7 | Comment IDs | client-generated ULID |
| 10 | First client | Tauri (native client-core; wasm/browser second) |
| 13 | Comment scopes | review-level, file-level, inline — all via `Anchor` enum |

| 3 | Rust↔ReScript types | hand-written both sides; Sury schemas in ReScript; JSON fixture round-trip test in CI (§6.3) |
| 8 | Working-tree changes | auto-refresh, debounced; client defers while a comment draft is open (§5.4) |
| 9 | Agent event delivery | subscribe via MCP |
| 11 | MCP transport | stdio shim proxying to daemon first; direct ws later |
| 12 | Multi-repo review UI | merged tree with repo roots (§5.6) |
| 14 | Highlighter | syntect |
| 16 | Daemon concurrency | one writer thread (mutations + re-anchoring, strictly serialised) and the tokio blocking pool for reads/renders against the shared `Core`; events fan out via a broadcast channel, connections filter by scope |
| 17 | Client cache when daemon is local | memory tier only; disk tier for remote daemons (§5.1) |
| 18 | Daemon lifecycle | one daemon per machine; `nits daemon stdio` is a proxy that auto-starts it; clients hold named contexts (local/ssh/ws) and can probe/start/stop; ws contexts are unmanaged; persisted default context for new processes with explicit overrides and MCP session-local switching; workspace/repo default from cwd | herdr-style remotes without a second store opener; kubectl-style switching for the app |
| 19 | UI styling | Tailwind v4 (Vite plugin); utilities for chrome, semantic class set + `@theme` tokens for diff rows, `data-focused` variants (§6.6) | cheap rows at 10k lines; one palette for web and TUI |
| 15 | Evolution | semver `ProtocolVersion` negotiated in `Hello`/`Welcome`, on every `Envelope`; integer `SchemaVersion` in redb `meta` with forward-only migrations (§4.9) |

### Deferred

- Cross-machine comment sync (log makes it feasible; not needed now).
- Rust↔Rust wire encoding beyond JSON.
- Browser/wasm client, TUI client.
- Export to GitHub PR review / `.review/` directory.

### Portable reference routing

`nits-protocol::ReviewReference` parses and formats `nits://` references carrying
one context locator, review ID and Review/Thread/Comment target. Context locators
name a saved client context, local daemon socket or daemon WebSocket endpoint;
they never identify an ephemeral HTTP bridge. The CLI resolves the locator through
the existing context model (explicit overrides win), validates the target using
`Ops::snapshot`, and serves a browser route. The host supplies its own context
identity to `ClientCore`; browser input cannot switch the connected daemon.

The client core correlates reference navigation with the latest review request,
ignores superseded snapshots and streamed pieces, and replays events that arrived
ahead of a reference snapshot before selecting the exact reply. Copy targets and
comment selection are core view state; browser shells perform clipboard and DOM
scroll effects. Reference/view additions use protocol 0.10; persisted events and
store schema are unchanged.

### Revision requests and checkpoints

Every new review request captures all repository/base/head `ResolvedTarget`s at
request time. Working trees are real immutable git tree objects, retained with
`refs/nits/reviews/<review>/trees/<tree>` refs so GC and later checkout edits do not
remove requested or checked content. Commit sources also retain a `commits/<oid>`
ref so force pushes and GC cannot remove their provenance. Resolved target events are retained as well,
so a check submitted from a previously displayed snapshot remains valid during a
refresh race. Existing historical requests explicitly carry unknown targets.
The note is prose: requesting another head requires an explicit review target update.
Capture resolves once and records that same resolution on the review, with normal
comment reanchoring. The request remains the first committed event for mutation
acknowledgement; resolution/reanchoring events follow in writer order.

`RecordCheckpoint` records exact targets and an optional typed request/checkpoint
identity being answered. IDs are committed event sequence newtypes. Each checkpoint
keeps the full author, timestamp and session provenance. `ReviewerIdentity::Agent`
groups by the established agent routing name across sessions/models; humans group
by name and machine. `latest_checkpoints` selects the last committed check for each
identity and compares repository/base/head tree and commit identities with the
current resolved review, independently of viewing scope. Checks imply no approval,
readiness, thread resolution, or human viewed marking.

Snapshots read checkpoints, requests and their cursor in one redb transaction;
rebuilds and fresh/reconnected clients retain the same records. `Requested` scope
opens a request's captured diff. `SinceCheckpoint` scope compares each checked head
to its current head, preserving the original review base and thread history. Both
scopes use the existing file listing/rendering API and support multiple repos.
The client discards content from superseded opening streams and file-list replies.
Current checking is unavailable until matching content is loaded, including while
a draft holds back a refresh. A deliberately selected historical scope still keeps
the explicit current check separate from checking the captured requested revision.
MCP exposes `record_checkpoint`, `get_checkpoint_delta` and `get_diff.scope`;
`get_review` returns full history and latest reviewer freshness; checkpoint-delta
queries include their selected source context. CLI exposes
`review set-head REVIEW REF --repo REPO`, `review request`, `review check --request ID|--current`, optional
`--answer-request ID` / `--answer-checkpoint ID`, and `files`/`diff` with either
`--request ID` (the captured revision) or `--since-checkpoint ID` (the incremental delta).

### Working-tree HEAD provenance (#97)

Protocol 0.12 adds `ResolvedSource::WorkingTree.head`, the commit at capture time.
It is null for an unborn checkout or a historical snapshot without recorded HEAD;
readers never substitute the current checkout commit for that missing history.
Schema 6 → 7 preserves historical event bodies and rebuilds views with absent HEAD
provenance. New snapshots retain both their tree and captured commit through Git GC.
Commit lists and committed/working-tree scopes use this captured HEAD, including
archived reviews. A scope needing an unavailable HEAD returns an explicit error;
a commit list with no captured HEAD is empty. Target events also refresh the
client commit list, and older in-flight list responses cannot replace newer ones.
