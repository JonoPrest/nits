# Handover notes

Historical implementation notes, retained with their original dates. References
to the next task, local experiments and planned APIs below are not the current
backlog or an installation guide. Start with the [README](../README.md),
[quickstart](QUICKSTART.md) and [architecture](ARCHITECTURE.md) for current source.

## 2026-09-01 (later): Visual mode, z Expand group, search stepping SHIPPED

Search-result stepping is in too: `SearchView.selected` /
`ContentSearchView.selected` are core state stepped by the host-only
`Action::SearchStep { delta }` (shells forward Down/Up from the inputs;
Enter opens the selected hit); the actions palette's selection is UI
state since its filtered list is UI-side. Found and fixed en route: the
App key handler now preventDefaults printable chars, because the chord
that opens a text input (`t`/`F`/`:`) used to also type itself into the
autofocused input (queries like "Fhandover"). All three flows verified
headlessly. Current browser sessions no longer share that old process-wide
core: every WebSocket has a fresh core and daemon connection, and a reload
resets navigation/key state while retaining only shared preferences.

The `z` Expand group is also in (Commands ExpandUp/ExpandDown/
CommentOnFile bound `z u`/`z d`/`z c` in Diff, group label "Expand";
the expands share ExpandContext's whole-file re-render until band
splicing lands; the shadow-test probe moved to `q`). Verified headlessly
(which-key popup shows the group; `z c` opens the composer).

Visual mode below is built, tested and verified headlessly (Playwright
against `nits --port 7788 .`: V badge, j/k extend, c opens the composer
on the range, esc round-trips; zero console errors). Implementation
followed the plan below almost exactly; deltas worth knowing:

- Core state is `ClientCore.visual_anchor: Option<u32>` (not a struct);
  the selection crosses the boundary as `ViewModel.visual:
  Option<VisualView { start, end }>` riding the Diff patch.
- `resolve` gates on an `in_visual` flag: motions stay within the open
  file (no adjacent-file continuation), `c` maps the selected rows to
  head lines (else base) and yields `Action::CommentLines`, `esc`/`V` →
  `Action::LeaveVisual`. `V` only enters on a commentable row.
- `visual_anchor` clears on comment open/submit/discard, CloseFile,
  CloseReview.
- `modes_of`: motions + Comment + VisualMode are `[Normal, Visual]`;
  keymap/keys.toml `[bindings.visual]` maps to the Diff context rows.
- This machine's clippy (1.97.1) flagged three pre-existing nits
  (explorer.rs obfuscated_if_else, keymap doc_markdown, keys_file
  redundant closure) — fixed in the same commit.

`nits` is NOT installed on this machine (`cargo install nits` before
demoing); Playwright lives in the session scratchpad `pw/`.

## 2026-09-01 (evening): DONE — Visual mode (keyboard multiline comments)

Everything below builds on the modal-keys system that landed today
(space leader + persistent which-key, typed keys.toml + schemars schema
via `nits keys init|schema|check`, per-panel focus `ge/gd/gt/gm`,
tree verbs `y`/`c`/`C`/`z`, fold-aware cross-file motions with `C`
fold / enter unfold / `X` expand-full, GitHub-style stacked diffs).
All pushed; gates green. Read docs/UI-DESIGN.md (updated: "fully modal"
supersedes the old no-modes rule).

### Visual mode — the design (agreed with Jono)

Enter with `V` on a diff row → Mode::Visual; `j`/`k` (and arrows)
extend the line selection; `c` opens a multiline comment draft on the
selected range; `Esc` (or any resolve) returns to Normal. The footer
shows a VISUAL badge (the badge slot already renders INSERT). Rows in
the selection highlight (the mouse-drag path already has
`.row-drag-selected`).

The plumbing that already exists — use it, don't rebuild:
- `Mode { Normal, Insert }` + `modes_of(Command)` validity + per-mode
  keys.toml tables + per-mode schema (crates/nits-client-core/src/keymap.rs).
  Add `Visual` to the enum: serde alias "visual", schema/table emission
  is derived, so `nits keys init` and the schema pick it up automatically.
- `Action::CommentLines { file, side, start_line, end_line }` builds the
  ranged anchor (lib.rs) — the mouse drag uses it; Visual's `c` should
  resolve to exactly this.
- The hint bar is mode-aware (`ViewModel.mode` rides the Hints patch);
  which-key/pending machinery needs no changes.

### Implementation plan

1. **Core state** (nits-client-core):
   - `keymap.rs`: `Mode::Visual` variant. `modes_of`: MoveDown/MoveUp/
     PageDown/PageUp/GoTop/GoBottom/Comment/Back gain Visual; add a new
     `Command::VisualMode` (Normal, bound `V` in Diff, label "select
     lines").
   - `lib.rs`: core field `visual: Option<VisualSel>` where
     `VisualSel { anchor_row: u32 }` (the row where V was pressed; the
     cursor row is `Focus::Diff { row }` as usual). New
     `Action::EnterVisual`, `Action::LeaveVisual` (or fold both into one
     toggle). derive(): `view.mode` becomes Visual while `visual` is
     Some (it currently derives Insert-from-Composer; extend the match).
     Leaving: Esc (Back in Visual resolves to LeaveVisual), any comment
     submit/discard, CloseFile/CloseReview must clear it.
   - Selection to the UI: simplest is `ViewModel.visual: Option<(u32, u32)>`
     (ordered row range) riding the **Diff** patch; FileDiff marks rows
     in range with the existing `.row-drag-selected` class (only for the
     open file). Fixtures + View.res mirror (ViewPatch::Diff gains the
     field — remember fixtures/client + ClientRegistry untouched, it's
     inside the Diff patch).
2. **Resolution** (`focus.rs::resolve`): lookup already takes context;
   mode is derived from core state, so pass it in (either extend
   `Keymap::lookup(mode, context, seq)` — bindings gain a `mode` field
   defaulting Normal — or gate in `resolve`). The lighter path used so
   far: bindings stay context-keyed; in Visual, `resolve` reinterprets:
   MoveDown/Up = move cursor row (selection = anchor..=cursor),
   `Comment` = CommentLines over the selection (side/lines via the same
   row→(side,line) mapping the UI's `lineOf` uses — write it in core,
   `diff.rs`, from the cached rows; skip rows with no line), `Back` =
   LeaveVisual. Everything else falls through (or leaves Visual first).
3. **Keys**: `V` → VisualMode → Action::EnterVisual (only in Diff focus,
   only when a commentable row is focused). keys.toml: `[bindings.visual]`
   section appears in `nits keys init` output automatically once
   modes_of is updated.
4. **UI** (small): FileDiff row class when inside `model.visual` range
   and `isOpen`; VISUAL badge (`mode-badge` slot, add a `.mode-visual`
   colour); nothing else.
5. **Tests**: keys.rs — V enters, j extends, c yields a draft whose
   anchor is Lines{start..end}, esc leaves; keymap.rs —
   default_table_parses (auto), keys_file round-trip picks up
   [bindings.visual]. Watch `every_action_is_reachable`: EnterVisual/
   LeaveVisual likely host_only? No — reachable via V/esc; add to the
   reached set naturally.

Gotchas from today, for whoever picks this up:
- ALWAYS `cd /Users/.../nits` (absolute) at the start of every shell
  chain; cwd drift into ui/ or the ../nits-demo worktree burned hours.
- Gate commits on real exit codes, not `| grep | tail` pipelines.
- After core changes: `cargo install --path crates/nits-cli --force`,
  `nits keys init --force` (regenerates the user's keys.toml +
  keys.schema.json), restart the demo server with
  `nits --port 7788 <dir>` and POST THE URL.
- `INSTA_UPDATE=always cargo test -p nits-client-core` while iterating;
  full workspace gates before the final push.

### Also specified, not started (build on the other machine)

**`z` Expand group (Diff context)** — was half-built here and reverted
clean; spec: Commands `ExpandUp`/`ExpandDown`/`CommentOnFile`, bound
`z u`/`z d`/`z c`, labels "expand up"/"expand down"/"comment on file",
group label `z` → "Expand"; resolve: the expands map to
`Action::ExpandContext { file, full: false }` until band splicing gives
them distinct semantics; `z c` → existing `Action::CommentFile`.
Gotcha found when attempting: `keymap::tests::lookup_shadows_global_…`
asserts `lookup(Diff, ['z']) == Lookup::None` — a `z` prefix in Diff
makes that `Prefix`; update the test (use an unbound probe like `q`).
Remember modes_of + View.res Command variants + `nits keys init --force`.

**Search-result stepping** — in every search input (file find `t`,
content search `F`, actions `:`), Down/Up move a highlighted selection
through the results while the input keeps focus and typing keeps
filtering; Enter opens the SELECTED result (today it opens the first).
Design: selection is core state where results are core state
(`SearchView`/`ContentSearchView` gain `selected: usize`; new Actions
`SearchStep { delta }` or reuse MoveDown in a Search context/mode) —
text inputs currently stopPropagation on all keys, so the shells must
forward Down/Up/Enter from search inputs to the core (like they forward
esc/ctrl+enter from the composer). Palette selection can ride
`ViewModel.action_palette` similarly. Highlight class + scroll-into-view
in the lists.

### Other queued work (in rough priority)

- **Nested which-key groups**: `[groups]` labels exist for prefixes; the
  popup does not yet mark sub-groups (`+Diff` style) — derive "has
  deeper continuations" in `Keymap::pending_hints`.
- **Directional expanders** (`expand up`/`expand down` from the cursor):
  needs render-side band splicing (今 `x` re-renders the whole file with
  more context, `X` full). Protocol: per-band expand request or splice
  BlobRender rows into the cached render.
- **Conversation tab**: quote the anchored diff lines per thread + a
  hide-resolved filter (UI-DESIGN §Comments).
- **crates.io publish**: everything is packaged/verified (9 crates,
  names free, LICENSE in). Blocked ONLY on verifying the email at
  https://crates.io/settings/profile — then `cargo publish -p
  nits-protocol -p nits-config -p nits-review-core -p nits-client-core
  -p nitsd -p nits-client-host -p nits-client-web -p nits-mcp -p nits
  --no-verify` from a clean tree.
- **Tauri smoke run**: `pnpm --dir ui build && cargo run -p
  nits-client-tauri` — the desktop shell embeds the same dist; check
  the clipboard permission for the new copy-path button.
- **Demo worktree cleanup** when done: `git worktree remove
  ../nits-demo --force && git branch -D jp/demo-review`.

## 2026-09-01: UI-DESIGN build order COMPLETE (phases 1–3)

All three phases of the agreed build order are built, tested and
verified headlessly (Playwright against a live `nits <repo>` server;
scripts in the session scratchpad `pw/`). Every commit is pushed and
every gate is green (fmt, clippy -D warnings, all workspace tests minus
nits-client-tauri locally — CI covers it —, wasm check, fixtures no-op,
451 UI tests). Protocol bumped to 0.2.0; reinstall `nits` and
restart the daemon before demoing.

Landed since the last note, per phase:

- **Phase 2 (core)**: diff-scope switching end to end — protocol
  `DiffScope {All, Committed, Commit, Worktree}` on ListFiles/FileRender,
  `Response::Files` carries the scoped resolved targets; daemon
  `scoped_targets`/`files_scoped` (reusing `commit_step`); worktree-headed
  reviews now list their branch commits. Client: `SetScope`/`ScopeChoice`
  (`g a`/`g c`/`g w`), by-commit stepping drives the file list, `n`/`p`
  step commits in that mode; header scope control with the step position.
  keys.toml (`nits-client-host::keys_file`, `~/.config/nits/keys.toml`,
  strict parse, loaded by every host). Jump-to-comment-original-diff:
  streamed `Request::ChangeRender`, `Action::OpenOriginalDiff`, read-only
  banner, Enter on an outdated thread jumps, Esc returns.
- **Phase 3 (protocol/daemon)**: context expanders (per-file re-render
  with +20/whole-file context — content requests now use each
  RenderKey's own opts; `x`, band click, "expand file" button). Browse
  tab (`SetBrowseRef` + BrowseTree fetch, full tree at any ref, blob
  file views via BlobRender, off-diff comments record `context: None`,
  `viewing: <ref>` picker). Content search (`Request::Search`,
  case-insensitive substring, changed-files vs all-files scope, capped)
  plus the palette: `F` content / `:` actions (`Action::RunCommand`
  resolves a command like its key binding), `tab` cycles.
- **Bug fix**: `create_review` pre-flight-resolves targets — a failed
  create (Upstream with no upstream) used to commit a ghost review that
  `nits <path>` then kept finding. Old ghost reviews may still sit in
  existing stores (delete or ignore).

Known deviations / loose ends (documented, not blocking):

- The `t` file-name search is still the inline SearchBox, not a third
  mode of the palette overlay (design wants one palette, three modes).
- Expanders re-render the whole file at a bigger context rather than
  splicing the one band; the focused row is not remapped afterwards.
- Conversation tab lists threads chronologically but does not yet quote
  diff lines or offer a hide-resolved filter.
- Browse's ref picker applies to the first target repo only in
  multi-repo reviews.
- `docs/TODO.md` holds the agent event-waiting discussion (the
  requested long-poll already exists as MCP `subscribe_events`).

## 2026-08-31 (latest): phase 1 UI rendering landed — pick up at phase 2

The UI half of phase 1 below is done (commit "wip(ui-design phase 1): UI
rendering — …"): Tabs.res tab row (counts, SetTab clicks, chrome
tooltips), center pane switches on `model.tab` (Conversation =
full-width thread list, Browse placeholder), HintBar pending-leader mode
from `model.pendingKeys`, split/whitespace toggles in ReviewHeader with
keymap-derived tooltips (`Chrome.tip`), `.app-left` width from
`prefs.sidebarWidth`. Verified headlessly (`target/debug/nits .` +
Playwright, scripts in the session scratchpad `pw/check.mjs`); pnpm
gates green (417 tests). Still open from phase 1: palette shell
(`F` content / `:` actions) — deferred to phase 2 since it wants core
commands first. Next: phase 2 (core) in the section below.

## 2026-08-31 (later): phase 1 core landed — pick up at the UI rendering

Building docs/UI-DESIGN.md phase 1. The **core half is done and green**
(fmt, clippy -D warnings, all workspace tests, `cargo xtask fixtures`
no-op, `pnpm rescript` + `pnpm test` 414 passing). What landed (this WIP
commit):

- **Keymap** (`crates/nits-client-core/src/keymap.rs`): `t` opens file
  search (ctrl/meta+p stay as aliases); `1`/`2`/`3` → new commands
  `TabFiles`/`TabConversation`/`TabBrowse`; leader-`g` group per
  UI-DESIGN — `g s` split, `g h` whitespace, `g f`/`g F` next/prev file,
  `g e` bottom, `g <`/`g >`/`g =` sidebar resize; Composer `ctrl+enter` →
  new `Command::Submit` (display-only: resolve returns Err, the host's
  textarea owns the chord). New keymap methods: `pending_hints(ctx,
  pressed)` (zellij-style group bar) and `chrome()` (one hint per bound
  command, for button tooltips — tooltips must come from this, never
  hand-written).
- **View state** (`view.rs`): `Tab {FilesChanged, Conversation, Browse}`;
  `ViewModel.tab`, `.pending_keys` (text of the pending leader, "" when
  none), `.chrome`; `ViewPrefs.sidebar_width` (serde-defaulted, persisted;
  consts SIDEBAR_DEFAULT/MIN/MAX/STEP). New `Action::SetTab` /
  `Action::SetSidebar` (lib.rs user()); derive() now swaps the hint list
  to the pending group while `self.chords` is non-empty.
- **Patches**: `ViewPatch::Focus {focus, tab}`, `ViewPatch::Hints {hints,
  pending, chrome}`. Fixtures + ReScript schemas updated (`View.res`,
  `Action.res`, `ClientRegistry.res` — Tab registered; boundary test
  green).

**Next agent picks up here — the UI rendering (ReScript, ui/src):**
1. Tab row in App.res driven by `model.tab` (Files changed n / 
   Conversation n / Browse placeholder), clicks dispatch `SetTab`; center
   pane switches on it (Conversation = full-width Threads list; Browse can
   be an empty placeholder this phase). Tab buttons' tooltips from
   `model.chrome` (find by command).
2. HintBar.res: when `model.pendingKeys != ""` render leader mode —
   highlight the pending key and show the group hints (they're already in
   `model.hints`); style it distinctly (see design/ByCommit.dc.html
   footer mock).
3. Toggle buttons (split `s`, whitespace `w`) in the review header with
   tooltips from `model.chrome`; sidebar width: style `.app-left` from
   `model.prefs.sidebarWidth` (inline style, replaces fixed w-72).
4. Arrow keys already bound (unadvertised). Palette shell (`F`/`:`) is
   still open — needs core commands first if done properly; fine to defer
   to phase 2.
5. Verify headlessly: `nits .` in the repo, then Playwright scripts in
   the scratchpad `pw/` dir pattern (see below); keep every gate green.
   NOTE: `~/.cargo/bin/nits*` predate all of this — `cargo install nits`
   and `nits daemon stop` before demoing.

Then phases 2–3 as listed in the section below.

## 2026-08-31: design settled — next task is BUILDING docs/UI-DESIGN.md

`docs/UI-DESIGN.md` is the settled product design (from the design canvas
https://claude.ai/code/artifact/f9e63a11-b6cd-4b8e-8ece-8a2d95f8c623;
artboard sources in `design/`, seeded output gitignored). It supersedes
PLAN 4.4-4.6 where they differ. Read it first.

Build order agreed with Jono:
1. **UI-only**: tabs (Files changed / Conversation / Browse), toggles with
   keymap-derived tooltips, split (side-by-side) layout, syntax spans
   where the render already provides them, leader-`g` groups + the
   mode-aware hint footer (footer ALWAYS shows the current mode's keys —
   pending-leader group, composer, jump-to-context), arrow keys as
   unadvertised j/k aliases, palette shell (t files exists; F content and
   `:` actions UI first, daemon Search later).
2. **Core**: diff-scope switching (All changes ± working-tree toggle /
   By-commit stepping vs parent, worktree as last step — reuse
   StepperCommit + commit_step), keys.toml loading in hosts
   (~/.config/nits/keys.toml; commands are enums, bad entries fail
   loudly), jump-to-comment-original-diff using Comment::context.
3. **Protocol/daemon**: context expanders (±20 lines / expand-all /
   full file), content Search request, Browse tab (tree snapshots already
   take any RefSpec; browse comments = Anchor::File/Lines off-diff, and
   Comment::context stays None there).

Already landed for this (all pushed, gates green):
- `Comment::context: Option<ChangeKind>` end to end (client fills it on
  DraftSubmitted from the open file's RenderTarget; replies inherit).
- `ResolvedSource::WorkingTree { branch }` (daemon captures symbolic-ref)
  and `ViewModel.{open_review, resolved_targets}` on the ReviewList
  section; ReviewHeader shows `base → branch (worktree)`.
- Dev loop: `nits [PATH]` serves web UI (vite-style); browser testing via
  headless Playwright (scratchpad pw/ has scripts); `nits-client-web` is
  HTTP+WS on one port; the daemon was restarted on the new build.
- Live daemon has real comments incl. context-bearing ones for testing.


Written 2026-08-27 at the end of the session that finished Milestone 3 and
4.0–4.3 of Milestone 4. Read this, then `AGENTS.md`, then `docs/PLAN.md`. UI work: `docs/UI-DESIGN.md` is the settled product design (canvas-derived) and supersedes PLAN 4.4-4.6 where they differ.

## State of the tree

`main` is clean; every commit is pushed. Gates, all green:

- Rust: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings` (clippy 1.97), `cargo check -p nits-protocol -p
  nits-client-core --target wasm32-unknown-unknown`, `cargo test
  --workspace`, `cargo xtask fixtures` (writes `fixtures/protocol` and
  `fixtures/client`; must be a no-op).
- UI (`ui/`, pnpm 11, node 24): `pnpm install`, `pnpm rescript`,
  `pnpm test` (vitest: CSS coverage, boundary round-trips over every
  fixture, adapter tests with mocked Tauri), `pnpm vite build`. CI runs
  both jobs (`.github/workflows/ci.yml`).

## Where things are

| area | crate / dir | notes |
| --- | --- | --- |
| client state machine | `crates/nits-client-core` | complete (3.1–3.5): cache, optimistic mutations, explorer, diff overlays, keymap/focus, `ViewPatch` |
| client fixtures | `crates/nits-client-core-fixtures` → `fixtures/client/` | one example per type/variant; boundary test consumes them |
| host loop | `crates/nits-client-host` | transport + KV + ticks + patches; tested against a real daemon |
| two-client simulator | `crates/nits-test-support/src/sim.rs` | `Sim` drives N cores against a daemon model |
| UI scaffold | `ui/` | ReScript 12 + React 19 + Vite 8 + Tailwind v4; `src/styles/app.css` tokens + semantic classes; conventions follow the Envio UI (`~/code/ui`), see AGENTS.md |
| UI schemas | `ui/src/protocol/*.res`, `ui/src/view/*.res` | `@schema`-derived Sury schemas (rescript-schema 9.3.0-rescript12.0 + ppx 9.0.1); `Registry` / `ClientRegistry` by Rust type name |
| UI components | `ui/src/ui/*.res`, `ui/src/App.res` | design system `UI.res`; Row/DiffView (virtualized)/Tree/Threads/Composer/Stepper/HintBar/HelpOverlay/SearchBox; key capture in `App` |
| UI tests | `ui/__tests__/*_test.res` (ReScript, vitest+jsdom), `ui/tests/*.test.ts` (fixture harness) | 380 checks |
| UI adapters | `ui/src/core/{Core,CoreTauri,CoreWasm}.res` | `dispatch` / `key` / `subscribe` / `attach`; patches applied by `Core.Store` |

## The host ↔ UI contract (4.2/4.3)

- Tauri commands the UI calls: `dispatch {action}`, `key {chord}`,
  `attach {}`. Events the host emits: `view` with an array of `ViewPatch`.
  `nits_client_host::Handle::{dispatch,key,attach}` are those commands;
  the patch receiver is what `emit("view", …)` drains.
- `ViewPatch` carries exactly one `ViewSection`; `review` (raw open review
  state) is never pushed. The UI keeps its own `ViewModel` copy
  (`View.empty` + `ViewPatch.apply`). Every patch stayed under 64 KB in
  the host test (100k-line file scrolled end to end) and the adapter test.

## What landed after 4.3

- ReScript 12 + `@schema` ppx migration of every schema; `UI.res` design
  system; tests in ReScript (see AGENTS.md "UI").
- Screens: review list with create form (any base vs head, multi-repo,
  `RefSpecText` parser), tree with viewed checkboxes and repo display
  names, virtualized diff with placeholders and viewed-collapse, composer,
  threads with suggestion apply, conversation, commit stepper with commit
  panel, hint bar, help overlay, fuzzy search, key capture, focus
  scroll-into-view.
- Core: workspaces listed on subscribe (reviews = union of all
  workspaces), `Action::{ListWorkspaces, CreateReview, ApplySuggestion}`,
  `Command::{Refresh, ApplySuggestion}`, `StepperCommit` details,
  `DiffView.viewed`, `ThreadView.suggestion`.

## Not done / blocked

- **Tauri wrapper crate** (`nits-client-tauri`, binary `nits-desktop`):
  landed 2026-08-27 (macOS needs no extra libs; CI's rust job now installs
  webkit2gtk for Ubuntu). `tauri::Builder` with three commands that call
  `Handle`, a task draining the patch receiver into `app.emit("view", …)`,
  endpoint via `nits-config` → `nitsd::contexts::DaemonEndpoint`, KV at
  `app_data_dir()/kv.redb`, seed from `fastrand`. `nits-desktop [context]`
  accepts Local, SSH and WebSocket contexts through the shared host dialer;
  its typed `--socket`/`--data-dir`, `--ws`, and `--start-policy` arguments
  preserve the exact selection made by `nits --ui desktop`.
  Run: `pnpm --dir ui build` then
  `cargo run -p nits-client-tauri` (serves the embedded `ui/dist`; no
  `devUrl` is configured, because with one a debug build loads
  `localhost:5173` and shows a blank page unless `vite` is running). Icon is a placeholder square. Smoke-tested by hand:
  launches, daemon autostarts, no panic; Playwright still not wired.
  Gotcha: `nits_client_host::spawn` calls `tokio::spawn`, and Tauri's
  `setup` runs outside its runtime — `start_host` enters
  `tauri::async_runtime::handle()` first.
- 4.4: expanders — `Row::Expander` renders but has no action; the
  protocol has no "expand hidden lines" request (a render with more
  context, or `BlobRender` rows spliced in, would be needed). Playwright
  against the Tauri dev build is impossible here (no Tauri libs), so the
  smoke/keyboard-only flows exist only as component tests.
- 4.6: remote connection UX (SSH/context picker) remains a Tauri concern;
  `nits-client-host` already takes a typed Local/SSH/WebSocket endpoint.
- `nits [PATH]` (added 2026-08-31) is the vite-style entry point. No
  path: serves the workspace menu. With a path: walks up
  to the repo root (worktree-aware — each worktree is its own repo),
  auto-creates workspace+repo on first use, finds or creates the open
  review with head `WorkingTree` (base: upstream, else origin/HEAD, else
  `main`), then serves the browser UI on a free port and prints
  `http://127.0.0.1:<port>/?review=<id>`; Ctrl-C stops it. `--headless`
  (needs a path) prints the review id and exits — the remote flow is
  `nits -c <ctx> <path> --headless`, then find the review from a local
  client; remotes are named contexts only (an ad-hoc `--ssh` was tried
  and removed — one way of doing things). `--ui desktop` launches a
  sibling `nits-desktop` (no deep link there yet); `--ui tui` reserved.
  The browser bridge itself stays local, but its host can connect to any
  named Local, SSH or WebSocket daemon context.
- Browser dev/test path (added 2026-08-31): `nits-client-web` (`nits-web`
  bin) is now HTTP+WS on one port: `/` serves the `ui/dist` build embedded
  at compile time (build the UI before cargo — CI's rust job does), `/ws`
  bridges `nits-client-host`; `CoreWs.res` is the
  browser adapter and `Main.res` picks it outside Tauri
  (same-origin `/ws`; `?ws=` overrides; Vite dev proxies `/ws` → 9777). Run
  `cargo run -p nits-client-web` and open its URL, or explicitly trust the dev
  UI with `--allow-origin http://localhost:5173` before running `pnpm dev`
  (use the exact origin Vite prints; see ARCHITECTURE §6.2). Open in
  a browser — agents can drive it with headless Playwright
  (`npm i playwright && npx playwright install chromium`; verified: page
  connects, review opens, threads render, zero console errors). Tauri
  remains the shipping shell; its IPC layer is only covered by the manual
  smoke.
- `CoreWasm` is a stub that refuses actions (PLAN "Later").
- Edit-comment through the composer (an "edit draft") is not modelled;
  `Action::EditComment` takes the text directly.
- Stepping commits only moves a cursor; no per-commit render request
  exists in the protocol yet.
- The disk-tier LRU index is session-local (see `content.rs`).

## Design points worth knowing

- `ClientCore::handle` emits at most one `Render` per input, after
  `derive()` recomputed every derived panel; `ViewPatch`es are built from
  that delta. Add derived state to `derive()`, never render by hand.
- Rejected inputs leave view/connection/cache untouched (proptest), except
  the chord buffer, which resets on an unbound sequence.
- The daemon replays `Since::After` events *before* answering
  `Subscribe`; the core accepts events while `Connecting` once its
  `Subscribe` is out.
- Sury: the `-rescript12.0` line of rescript-schema is the one for
  ReScript 12 (9.5.x also uses 12 syntax but the reference pins 9.3.0);
  the package is namespaced: `-open RescriptSchema` in `rescript.json`.
  `pnpm-workspace.yaml` must allow the build scripts of `rescript`,
  `rescript-schema-ppx` and `@rescript/react` (pnpm 11 `allowBuilds`).

## 2026-09-02: DONE — one binary

`nitsd` and `nits-mcp` lost their `[[bin]]` sections and are libraries
only; the daemon is `nits daemon serve` and the MCP server is `nits mcp`,
both hidden-or-documented subcommands of the one `nits` binary.

- `nitsd::serve` holds what `nitsd`'s `main` did (`serve`, `stdio`);
  `nitsd::launch::nits_binary` starts a daemon by re-executing the
  running `nits` (`$NITS_BIN` overrides), so client and daemon are always
  the same build and cannot fail the version handshake.
- ssh contexts run `ssh <host> nits daemon stdio`; `Context::Ssh.bin` is a
  `RemoteBin` (`Default | Nits(path) | Legacy(path)`), parsed from the two
  wire spellings once at the config boundary. The old `nitsd` key is **not**
  aliased onto `bin` — it names a binary that cannot serve — it becomes
  `Legacy`, which `connect` refuses with the exact edit to make; both keys
  at once is a config error rather than a silent winner.
  `--start-policy require-running` replaced `--stdio-if-running`.
- `daemon serve|stdio` take `--ws-listen`, not `--ws`: the global `--ws`
  is the client side, a URL to connect to.
- Packaging: one tarball/formula/PKGBUILD/deb/rpm, no `Depends: nitsd`,
  `cargo install nits` is the whole instruction. `Releasable` has one
  variant; the workflow's package choice is `[nits]`.
- `nits-web` and `nits-desktop` are unchanged — a dev bridge and a GUI
  app, neither of which anyone installs from a package.
