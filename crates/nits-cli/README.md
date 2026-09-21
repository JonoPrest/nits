# nits

A daemon-backed code review tool. Nits are anchored to content (blobs), not to
diffs or line numbers — so they survive rebases, amends and force-pushes.

```console
$ cargo install nits
$ nits workspace add .
$ nits review create --base main --head HEAD
$ nits diff
```

- A workspace groups multiple git repos; a review spans any base vs any head
  across them, with commit stepping.
- Inline, file-level and review-level comments, with human and agent authorship
  recorded.
- Everything is served by one daemon per machine, reachable locally or over
  SSH. The daemon is this same binary — `nits daemon serve`, started on demand
  — so there is nothing else to install and no way for the two to be different
  versions. Agents attach through `nits mcp`.

Other ways to install — Homebrew, `apt`, `dnf`, the AUR — and the full
documentation are at <https://github.com/JonoPrest/nits>.

In Claude Code, ask: **Run `nits skill`, read its instructions, and use Nits for
this review.** The command prints the installed version's portable, self-contained
review guide without needing a checkout, daemon, network, or MCP setup. Claude
can use available MCP tools or continue directly through the CLI.

For optional persistent Claude Code use:

```sh
mkdir -p ~/.claude/skills/nits-review
nits skill > ~/.claude/skills/nits-review/SKILL.md
```

Invoke `/nits-review` with your task; use `.claude/skills/nits-review/SKILL.md` for
a project-local skill. The exported guide includes its references; re-export
after a Nits upgrade. Printing alone does not install or register anything.
Other coding agents can read the same command output. Codex additionally supports
exporting it to `${CODEX_HOME:-$HOME/.codex}/skills/nits-review/SKILL.md` (create the
directory first) and invoking `$nits-review`.

Select a default daemon with `nits context use <NAME>` (saved atomically in
`~/.config/nits/config.toml`, or `--config` / `NITS_CONFIG`). Offline contexts
can be selected. `nits context show` reports the effective selection and its
origin; `--json` returns `{name, context, origin}`. Ad-hoc transport flags take
precedence over `-c`, then `NITS_CONTEXT`, the persisted default, and implicit
`local`. To remove the persisted default, select another context first. Context
names must be nonempty, with no surrounding whitespace or control characters.

The internal `daemon serve` and `daemon stdio` commands default to this machine,
ignoring saved and environment client routing defaults (`NITS_CONTEXT` and
`NITS_WS_URL`), even if a context named `local` points elsewhere. Explicit local
`-c` selections and socket/data-dir flags or environment values still work;
explicit remote bindings are rejected. `daemon status|start|stop` use the normal
client selection above.

`nits mcp` uses the same startup default. Inside MCP, `list_contexts` reports
configured names and the active daemon; `use_context {"name":"build-box"}`
switches that session after a successful handshake. Failure leaves the old
connection active. A switch preserves agent identity and affects subsequent
calls only; await it before using the selected daemon's IDs. Read results
include `context: {name, kind}`. After switching, pass `since_context` alongside
`subscribe_events.since_seq`, using the source context's name. Running MCP
sessions do not follow later CLI default changes, and MCP switches do not
rewrite the default.

Select a workspace with `--workspace <ID>` anywhere in the command, including
before `review list`, `review create`, or `events`. `review create` can infer its
workspace from the current directory. `review list` defaults to all workspaces
on the selected daemon, even outside a checkout or over SSH. `nits workspace list` lists IDs,
repository names and paths; `review list` and `review show` also identify each
repository and its base/head refs, including when using a remote context.
The `nits PATH` shortcut (including `--headless`) selects its workspace from the
repository's existing attachment and rejects `--workspace`. For an explicit
workspace, use `nits --workspace <ID> review create --repo <REPO_ID> --base <REF>
--head worktree`; `nits workspace list` shows the repository IDs.

`nits review list --all` explicitly selects all workspaces; it conflicts with
`--workspace`. Narrow discovery with `--title 'PR #247'` (case-insensitive
substring) or `--awaiting reviewer-a` (exact recipient name), or combine them.
Rows include the workspace, open-finding count, pending request recipients and
last activity in UTC. Newest committed review activity sorts first, even if the
clock moved backward. Archived reviews remain discoverable; deleted reviews do
not. The query reads persisted metadata, so missing checkouts do not block it.

An open finding is an open actionable thread with a nondeleted root; replies do
not increase the count, and outdated anchors still count. A request remains
pending until that named agent records a checkpoint explicitly linked to it.
Unlinked checkpoints or another reviewer’s checkpoint do not answer it; an
answered request does not imply approval. Workspace renames change labels without
pretending there was new review activity.

`--json review list` returns an array retaining the review fields and adding
`workspace_name`, `repositories`, `open_findings`, `pending_requests` and
`last_activity: {seq, at}` (`at` is signed Unix milliseconds). MCP `list_reviews`
uses the same optional `workspace_id`, `title` and `awaiting` filters and returns
`{context, reviews, seq}` from one coherent store read. Use `get_review` or
`review show --json` for the full snapshot and its own replay cursor.

A checkout can be attached once per workspace, including through symlinks or its
`.git` directory. Repeating `workspace attach` reports the existing repository ID;
it does not create another attachment. The same checkout can belong to another
workspace, and separate Git worktrees remain distinct. Use `nits workspace detach
<WORKSPACE> <REPO>` to remove a membership or repair older duplicates. Detaching
keeps checkout files and review/comment history. Reviews that used the removed
membership cannot read checkout content through it.

```console
$ nits -c build-box --workspace <ID> review list
$ nits --daemon-url ws://reviews.example:7677 workspace list
$ nits comment list --review <REVIEW>
$ nits comment add <REVIEW> --path src/main.rs --lines 12-15 --body 'Check these lines'
```

`--daemon-url` chooses an ad-hoc WebSocket daemon; `--ws` is a deprecated alias
and `NITS_WS_URL` remains supported. `comment add`, `comment list`, `review show`,
and `files` accept either positional `<REVIEW>` or `--review <REVIEW>`, but not
both. Commands with a second positional (a path or thread) take `<REVIEW>` first. Line ranges accept
`START-END` and `START:END`; both are inclusive and require positive, ordered
line numbers. `--json` retains the protocol values for scripting.

Routine maintenance uses the same daemon mutations and author checks as the other
clients. All IDs belong to the selected context; use `workspace list`, `review
list`, and `comment list` to find them.

```sh
nits review rename <REVIEW> 'Updated title'
nits review archive <REVIEW>
nits review reopen <REVIEW>
nits review set-base <REVIEW> main --repo <REPO>
nits review set-head <REVIEW> feature --repo <REPO>
nits comment edit <REVIEW> <COMMENT> --body 'Corrected explanation'
nits comment delete <REVIEW> <COMMENT>
nits workspace rename <WORKSPACE> 'Updated workspace'
nits workspace detach <WORKSPACE> <REPO>
nits review delete <REVIEW>
```

Archive preserves discussion and can be reopened; reopening first checks that the
current refs resolve. Rename keeps open/archived status, and base/head updates
keep the other ref and review ID. A base cannot be `worktree`. Review deletion
removes the record from listings with no undelete command; its event history
remains accessible with `events --review <REVIEW> --since 0`. Comment deletion
leaves a tombstone. Detach keeps checkout files and review history. These explicit
commands execute without an interactive confirmation; inspect the selected IDs
first. `--json` returns the committed primary Event receipt, and human output
identifies the affected record and action.

Only a comment's complete original author can edit or delete it. For human CLI
comments this includes name and machine. With `--agent`, retain the same agent
name, `NITS_AGENT_MODEL`, `NITS_SESSION_ID`, and invoking human; matching the agent
name alone is insufficient. Editing prose preserves anchors, suggestion patches
and any applied receipt. Historical events remain unchanged.

For scripts, `nits --json . --headless` creates or reuses the directory's
working-tree review and prints one JSON object with `review_id`, `workspace_id`,
`repo_id`, `outcome` (`"Created"` or `"Reused"`), and `base`/`head`. The latter are
the daemon's recorded resolved refs, each containing a `tree` OID and `source`
(commit OID or working-tree details). `--ui headless` is equivalent. Without
`--json`, stdout remains just the review ID; status messages go to stderr.

## Licence

MIT.

Share a review, finding, or verification reply with a portable reference:

```sh
nits --context review-box reference REVIEW_ID --thread THREAD_ID
nits --context review-box reference REVIEW_ID --comment COMMENT_ID
nits open 'nits://context/review-box/review/REVIEW_ID/comment/COMMENT_ID'
nits open 'nits://context/review-box/review/REVIEW_ID/comment/COMMENT_ID' --headless
```

`reference` validates the target and prints its reference (`--json` prints a JSON
string). `open` validates it on the selected daemon and serves a browser UI with
that exact reply expanded and highlighted; `--headless` validates and prints the
reference without starting a UI. Open the printed HTTP URL in a browser and leave
the command running. Desktop deep-link opening is not supported yet.

References use stable IDs and survive resolving or moving an anchor. Saved
context names resolve using the recipient's `config.toml`; a missing name fails
explicitly. Context/endpoint flags and their environment equivalents override a
reference's context, which otherwise overrides the persisted default. Ad-hoc
local sockets and daemon WebSocket URLs are encoded directly. Socket references
are meaningful on the machine owning that socket; use a matching saved SSH
context (or an explicit `--context` override) from another machine. A standalone
SSH host without a saved selection uses its SSH host name as the context name.
Opening a socket reference requires its daemon to be running: the reference
cannot guess which data directory to start. Select a saved context explicitly
when daemon startup is needed. Browser bridge ports are never part of the portable
reference.

In the UI, use **Copy reference** on a thread or reply. With thread focus, `y`
copies that thread; `]` / `[` select the next/previous reply and `y` copies that
reply. Bindings and button tooltips follow `keys.toml`. The original `?review=ID`
browser route remains supported; `?reference=...` takes precedence.
