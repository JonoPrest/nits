# UI design (settled 2026-08-31)

The product design for nits's review UI, agreed on the design canvas
(<https://claude.ai/code/artifact/f9e63a11-b6cd-4b8e-8ece-8a2d95f8c623>;
working artboards in `design/`). This is the reference the web UI is built
to and the TUI will follow. PLAN 4.4–4.6 items are superseded by this
where they differ.

## Principles

- **Keyboard-first, everywhere.** Every feature, toggle, tab, and pane is
  reachable by keyboard; a mouse click is an alias for a chord. Every
  button's hover tooltip shows its shortcut. Pane focus cycles with
  `tab`/`shift-tab`; pane resizing is keyboard-native (`<`/`>` resize the
  sidebar, `=` resets), never drag-only.
- **One keymap, all shells.** The keymap lives in `nits-client-core` and
  drives web, Tauri, and the future TUI identically. Hints, the `?` help
  overlay, and button tooltips are all derived from it — never
  hand-written. A control without a binding is a bug (testable, like the
  CSS-class coverage test).
- **Terminal-safe, modifier-light canonical bindings.** Canonical chords
  are bare printable keys (capitals allowed), so the TUI is identical and
  nothing collides with terminal/OS chords. The single modifier chord is
  `ctrl+enter` (submit from inside a text input). GUI shells may add mac
  aliases (`⌘K`, `⌘P`, `⌘⇧F` opening the palette) and show them second in
  tooltips; the bare key is the source of truth.
- **Fully modal, vim-style (supersedes the earlier "no modes" rule).**
  Modes: Normal (keys are commands), Insert (any text editor; only `esc`
  and `ctrl+enter` are chords), Visual (`V`: line selection). The
  hint bar shows the mode. The main flow stays flat single keys; `g` is
  the goto group; everything less common lives under a configurable
  `<leader>` (default `space`), with which-key style group labels: a
  pending prefix pops the group's continuations. keys.toml is
  action-centric (`toggle_layout = ["s", "<leader> s"]`), validated
  against a schemars-generated JSON schema (`nits keys schema`), and
  `nits keys init` writes the full defaults. `:` runs any action by its
  snake_case name with fuzzy autocomplete. Collisions never reject a
  config — they are reported (help overlay, `nits keys check`).
- **User-configurable bindings.** `~/.config/nits/keys.toml` overrides
  per-context bindings. Commands are an enum; an unknown command or
  unparsable chord fails loudly at load. Everything derived from the
  keymap (hints, help, tooltips) re-derives from the loaded map.

## Layout

Claude-viewer anatomy, nits tokens (`ui/src/styles/app.css`; dark and
light both):

- **Header**: review title · scope control (below) · `base → head` chips
  (resolved branch names; a working-tree head shows `branch (worktree)`) ·
  compact diff-settings menu (layout and whitespace) · totals
  (`N files · +A −D`) · connection dot.
- **Tabs**: `Files changed` (with count) · `Conversation` (with thread
  count) · `Browse`. Keys `1/2/3`.
- **Left sidebar**: a stationary top-left icon toggles the file tree;
  the tree shows per-file `+A −D` and thread-count badges, with the commits
  list below (chronological, working tree on top).
- **Center**: all changed files stacked in one scroll, sticky per-file
  headers (`path · +A −D · viewed checkbox`), viewed files collapse.
- **Hint bar** footer: connection state, viewed progress, and — always —
  the current mode's shortcuts (zellij-style): the focused context's
  primary keys by default, the group's keys while a leader is pending
  (`g: a all-changes · c by-commit · w worktree · …`), the composer's
  keys while composing (`ctrl+enter submit · esc discard`), and
  `esc back` in jump-to-context. The bar is the mode indicator; there is
  never a state whose keys it does not show.

## Diff scope

- **All changes** (default): `base → branch tip`, plus a
  **`+ working tree`** toggle that folds uncommitted changes in.
  Off = committed changes only. Base defaults to whatever the review was
  created with (upstream → `origin/HEAD` → `main`).
- **By commit**: each commit diffs against its parent; the working tree
  is the final step. `p`/`n` steps, `2 of 4` position indicator, commit
  panel (subject, full body, author, relative+absolute time, parent).
  The sidebar commit list is the step list.

## Diff rendering

- Unified and **split (side-by-side)** layouts, `s` toggles, persisted.
  In split view an added line pairs with a filler cell; inline threads
  span both columns.
- **Hide whitespace** (`w`), persisted; re-keys renders.
- **Syntax highlighting** everywhere (daemon-rendered `SpanClass` spans;
  already implemented; falls back to plain past the size cap).
- ✓ **Context expanders** on every collapsed band (GitHub-style): each
  band is a numbered *gap* on the wire (`Row::Expander { gap, dir }`),
  and expanding one opens THAT gap by 20 lines — `RenderOpts::expanded`
  carries the opened gaps, so no other hunk in the file moves. `↑`/`↓` on
  the band expand up/down, `enter` on a focused band does the same from
  the keyboard, and `z u`/`z d` open the gap above/below the cursor's
  hunk. `x` widens the whole file's context and the header's affordance
  expands it fully.
- Word-level change highlights within modified lines (already rendered).

## Comments and threads

- ✓ **Inline threads are primary**: a thread renders under its anchored
  line, in the diff or in Browse. `c` opens the composer at the focused
  line/file, `r` replies, `⌘Enter`/`ctrl+enter` submits, `Esc` discards.
  A line draft's composer takes the slot the thread will occupy — under
  the last line of its range — and only a draft with no row of its own
  (review- or file-level) docks at the bottom of the tab.
- ✓ **The lines a thread is about are marked**, the whole range and not
  just the anchor: the core says what each row is to a range
  (`RowThread { side, place }`, `DiffRow::drafted`) and the cell carries
  a rule down its edge. Four states can be on one cell at once — focused,
  selected, commented, being drafted about — so each is drawn over the
  diff-side colour rather than replacing it.
- **Conversation tab** aggregates every thread chronologically
  (GitHub-style): review-level comments, file-anchored threads quoting
  their diff lines with a "jump to diff" link, resolved threads collapsed
  to one line, a hide-resolved filter, and a review-level composer.
- **Every comment records the diff it was made on**
  (`Comment::context: Option<CommentContext>`, wired end to end). Threads
  show a context chip (`main → worktree @<oid>` or `browse @<ref>`).
  Jumping to a comment whose diff has moved on opens the **original
  diff** read-only, with a banner ("Viewing the diff this comment was
  made on… Back to current diff (Esc)"). Outdated threads in the
  Conversation tab link "open original diff".
- **Browse comments** anchor to `file@ref` with no diff pair.

Review conversation distinguishes open/resolved findings from informational notes.
The Conversation badge counts **open findings**; all conversation remains visible.
`<leader>i` starts an informational review note and `<leader>f` starts a review-wide
finding from any panel. Both have keymap-derived buttons in Conversation. Notes and
findings accept attributed replies; only findings expose resolve/reopen. Notes imply
no approval, readiness, or resolution. Legacy Note comments remain findings.

Durable review requests appear above threads in Conversation with requester,
recipient, note and creation time. Their count is separate from open findings.
`g r` focuses Requests, `j`/`k` move between cards, and Enter / Open changes
opens the captured requested diff (historical requests with unknown targets open
the current diff). Requests imply no approval or completion.

Conversation also shows each reviewer’s latest checkpoint, its exact repository,
base/head commit/tree identities and whether the current target has changed. Agent
names group checkpoints across session restarts; cards retain full author
provenance. `v` in Requests records that request’s captured revision as checked;
`<leader>k` explicitly records the current resolved revision, linking the request
or checkpoint when its scope is being inspected. `<leader>d` cycles
through latest-checkpoint deltas, with a scope banner and `g a` to return to all
changes. Buttons derive tooltips from these commands. These checks never approve,
resolve findings, or mark human ViewedMark state.

Deferred findings remain visible and explicitly labelled **deferred · unfixed**,
with their reason, optional external follow-up link, and recording actor/time.
Conversation shows separate counts for current-scope open findings and deferred
findings; its tab shows separate open and deferred badges. From a focused open
finding, `D` opens a reason/link composer (`ctrl+enter` submits, `esc` cancels).
`x` reopens a deferred finding for the current scope. These controls and their
hints derive from the keymap. Deferral never indicates approval or deployment
safety, and informational notes have no defer action.

## Browse

A third tab for reading code without a diff: full file tree (every file,
not just changed), plain syntax-highlighted file view, comments on any
line. A `viewing: <ref> ▾` picker accepts any ref — branch, tag, commit,
working tree (the daemon's tree snapshots already take an arbitrary
`RefSpec`).

## Search

Search surfaces share explicit input and result focus zones; each mode has a bare-key opener:

- **Files** (`t`, GitHub's file-finder key) — fuzzy file-name find
  (exists today).
- **Content** — keyword/regex across files, grouped by file with
  highlighted matching lines and line numbers; scope toggle
  `Changed files | All files @<ref>`; Enter jumps to the file at that
  line in the current tab. Opened with `F`. Needs a new daemon `Search`
  request.
- **Actions** (`:`, ex-style) — every command by name (toggle split,
  hide whitespace, open review…), the discoverable face of the keymap.

`/` finds within the open file.

## Canonical bindings (defaults)

| Chord | Action |
| --- | --- |
Flat (the main flow):

| Chord | Action |
| --- | --- |
| `j`/`k` (and arrows, unadvertised) | next/prev row |
| `n`/`p` | next/prev hunk (next/prev commit in By-commit mode) |
| `enter` | open the focused thing |
| `c` / `r` | comment / reply |
| `v` | toggle viewed |
| `x` | expand context at cursor |
| `t` | palette: files · `F` content · `:` actions |
| `/` | find in open file |
| `1`/`2`/`3` | Files changed / Conversation / Browse |
| `tab`/`shift-tab` | cycle pane focus |
| `ctrl+enter` | submit comment (inside the composer) |
| `<leader>b` | hide / show the file tree |
| `Esc` | close/back/cancel leader (also leaves jump-to-context) |
| `?` | help overlay |

Leader `g` (hint bar switches to the group while pending, zellij-style):

| Sequence | Action |
| --- | --- |
| `g a` / `g c` | scope: All changes / By commit |
| `g w` | toggle `+ working tree` in All changes |
| `g s` / `g h` | split layout / hide whitespace |
| `g f` / `g F` | next / prev file |
| `g g` / `g e` | top / end of the file list |
| `g <` `g >` `g =` | shrink / grow / reset the sidebar |

(`s`/`w` stay available flat too while there is no conflict; the leader
spelling is the stable one for docs and keys.toml examples.)

## Implementation notes

- UI-only: tabs, toggles + tooltip derivation, split view, arrow keys,
  syntax spans in remaining spots, palette shell.
- Core: scope switching (re-target the open review), by-commit stepping
  reusing `StepperCommit`, keys.toml loading in the hosts.
- Protocol/daemon: context expanders (render with more context or splice
  `BlobRender` rows), browse-mode comments already possible
  (`Anchor::File/Lines` need not be in the diff), content `Search`
  request.


## Settled interactions — Jono's requests ledger (2026-09-01)

Everything explicitly asked for, so no shell or rewrite loses them.
✓ = shipped; ◻ = specified, pending (see HANDOVER for pickup notes).

### Keyboard & modality
- ✓ Fully modal, vim-style; contexts (Tree/Diff/Thread/…) are per-panel
  keymaps and the footer badges the focused one (TREE/DIFF/…).
- ✓ Configurable leader, default `space`; shown in the footer as a `␣
  leader` chip; `space` renders as `␣` in every kbd.
- ✓ Which-key: pending prefix pops the group with its label and stays
  until continued or cancelled (esc/foreign key — never a timeout);
  the footer also shows `g Goto · …` while pending.
- ✓ Typed keys.toml: action-centric, snake_case names, `<leader>`
  tokens, per-mode tables, schemars-generated JSON schema for editor
  autocomplete; `nits keys init|schema|check`; collisions are reported,
  never rejected.
- ✓ `:` runs any action by name with fuzzy autocomplete (`:comment`).
- ✓ Footer shows ONE key per command (first in config order); aliases
  live in `?`. Order: mode/context badge · connection · viewed ·
  `␣ leader` · keys.
- ✓ `?` help: restyled, scrolls with j/k/arrows, floats above sticky
  file headers.
- ✓ Visual mode: `V` on a diff row selects it; `j`/`k` (and motions)
  extend the selection (within the file); `c` opens a comment draft on
  the selected line range; `esc`/`V` leaves. VISUAL badge in the footer;
  selection uses the drag highlight. keys.toml gains `[bindings.visual]`.

### File tree (neo-tree-style verbs, Tree context)
- ✓ `y` — yank (copy) the focused file's repo-relative path to the
  clipboard (also the ⧉ button in file headers; `y` works in the diff
  too).
- ✓ `c` — comment on the focused file (file-level anchor).
- ✓ `C` — collapse the parent dir (or the focused open dir); focus
  follows. `z` — collapse all dirs. Dirs default open in diffing mode
  and stay collapsible; Browse keeps its own expansion state.
- ✓ Commits moved to `<leader>C`.
- ✓ Sidebar: no resize keys — it auto-expands to fit full file names
  while the tree is focused, truncates otherwise; `<leader>b` hides it
  (rail to reopen).
- ✓ Header is the workspace name as a ⌂ home button (back to the review
  list) — never churning per-file breadcrumbs.
- ✓ Rows show `+A −D` and a thread-count badge, no checkboxes (Viewed
  lives in the file headers; `v` toggles).

### Stacked diffs (GitHub parity)
- ✓ All changed files queue in ONE scroll, tree order (ordering fixed
  daemon-side so every client agrees), first diff auto-opens.
- ✓ Per-file header: fold chevron · path · copy path · ±stats ·
  file-comment 💬 · expand file · Viewed checkbox (checking folds it).
- ✓ Folds are core state: `C` folds the focused file, `enter` unfolds,
  `X` expands full context; a folded file is ONE motion stop — j/k/n/p
  land on its header and leave with the next press.
- ✓ Cross-file motions: j/k at a file's edges, n/p past the last
  hunk/comment, and `g f`/`g F` all continue through the stack.
- ✓ Mouse drag across lines = multiline comment (keyboard version =
  Visual mode, `V`); inline threads under their rows with in-card
  reply; comments record the diff they were made on.
- ✓ A modified row is TWO comment targets, not one: the removed (red)
  cell anchors to base, the added (green) cell to head. The focus
  carries the side (`Focus::Diff { row, side }`); `h`/`l` move between
  the halves, with Left/Right as aliases. The mouse hit-tests the cell it
  is over, and a drag or Visual selection keeps the side it started on. A
  thread's marker hangs
  on the cell its anchor names.
- ✓ The viewport follows the focused row: a motion that would take the
  cursor past the edge scrolls by as little as it can, keeping 3 rows of
  context (vim's `scrolloff`) and clearing the sticky file header. This
  holds for every motion, in the stacked view and on Browse.
- ✓ Crossing a file boundary with `j`/`k` is a motion, not a jump: the
  next file opens and the view scrolls by a row, keeping the lines the
  reader was just on. Only a deliberate jump — the tree (click or
  `enter`), `] f`/`[ f` — pins the file's header to the top. The core
  says which (`Landing::Follow | Pin` on `OpenFileAt`); the host no
  longer scrolls on its own when a section becomes the open one.
- ✓ `z z`/`z t`/`z b` reposition the view around the cursor (centre, top,
  bottom) without moving it. Only the host knows the viewport's height,
  so the core records the intent — row, alignment and a counter, so the
  same chord twice scrolls twice — and the host performs it.
- ✓ `z` Expand group: `z u`/`z d` expand up/down from the focused row,
  `z c` comment-on-file from the diff (interim: both expands may share
  the more-context re-render until band splicing). Rule to uphold:
  EVERY mouse affordance has a chord — audit found comment-on-file from
  diff focus as the one gap.
- ✓ Search inputs (`?` help, `t` files, `F` content, `:` actions) own all
  printable typing, including `j`/`k`. Down or Tab enters the first
  result. Results use arrows and `j`/`k`; Up from the first result or
  Shift+Tab returns to the input. Other printable keys return to the
  input and edit its query. Forward Tab follows the accessible controls
  (scope, mode, close); mode buttons show their keymap-derived shortcut.
  Enter activates the current selected result; content queries must
  finish before activation and superseded replies cannot replace them.
  File/content selections live in the core, while locally filtered
  help/actions own theirs in the UI. All four share `SearchNavigation`'s
  explicit input/results focus model and clear result focus on an empty
  list. A chord that opens a text input is prevented from typing itself.

### Panels & focus
- ✓ Goto group: `g e` tree · `g d` diff · `g t` threads · `g m`
  commits (plus `g g` top, `G` bottom — vim-faithful).
- ✓ All behavior in nits-client-core (one ViewModel for web/Tauri/TUI);
  shells only render and own shell effects (clipboard).

### Chrome
- ✓ Toast region (`role="status"`): a transient line, dismissed after a
  few seconds. Its first use is the copy confirmation.
- ✓ Copying a path happens in the shell, inside the gesture that asks for
  it — a clipboard write needs transient user activation. Every browser
  socket owns its own host and view, so nothing about a copy travels to
  another tab. The core still decides WHICH file:
  `ViewModel.copy_target` is what `y` would copy from where the focus is.
  The keymap decides which key that is (prefix included), so a rebinding
  is followed and a bare `y` under a `g y` binding copies nothing. A
  clipboard that is absent or refuses says so rather than looking like a
  copy. Every file header carries the affordance, Browse included.
- ✓ Mac-style scrollbars (trackless, slim rounded grey, hover-darken);
  never two vertical scrollbars.
- ✓ Demo server pinned to `--port 7788`; the URL is posted on every
  restart.

### Shareable references

Thread and reply cards expose **Copy reference**, derived from the Thread
context's `copy_reference` binding (`y`). `]` / `[` select the next/previous
comment; `y` then copies that exact reply. Portable references carry a saved
context name or concrete daemon endpoint, never the browser bridge port.
`nits open <reference>` opens the correct review, expands its conversation and
centers the exact comment, including resolved and outdated findings. Stable IDs
survive anchor movement; original-context controls remain available on the
finding. Missing/deleted targets and mismatched browser contexts show explicit
errors. Legacy `?review=ID` URLs remain supported.
