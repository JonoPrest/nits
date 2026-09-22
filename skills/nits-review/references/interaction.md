# MCP and CLI interaction

Use the installed MCP tool schemas or `nits … --help` as the argument authority.
The examples below use symbolic IDs (`REVIEW_ID`, `REPO_ID`, `THREAD_ID`) and
invented names/paths; substitute values returned by the selected daemon. Do not
send these literal placeholders. No extra MCP tool exists just because Core has
the underlying capability.

## MCP

At session start call `get_session_identity {}`. Its `author` is an Agent with
`name`, `model`, `session_id`, `invoked_by` (possibly null), and `via: "Mcp"`.
Initialization gets the name from MCP client info, model from `NITS_AGENT_MODEL`
(otherwise `unknown`), and session ID from `NITS_SESSION_ID` or a generated value.
Use `set_session_identity {"name":"reviewer-a","model":"ACTUAL_MODEL"}` when
needed; both fields are required. Preserve accurate existing values. Session ID,
invoking human and origin are immutable here. A successful update affects future
events; reconnects and context switches preserve it. Names route requests and
group checkpoints across sessions; they are not authenticated human identities.

Call `list_contexts {}` to read configured, active and persisted selections.
`use_context {"name":"review-box"}` switches this MCP session after a successful
handshake, discards previous subscriptions, and leaves the CLI default unchanged.
Failure retains the previous context. Save `context.name` from daemon reads with
the IDs/cursors returned. Context switches do not translate IDs between daemons.

For daemon recovery, `get_daemon_status {}` inspects the selected context and
installed candidate without starting or restarting it. `restart_daemon {}`
explicitly activates that verified installation; it accepts no executable path.
An `Accepted` operation is still running: inspect status until `Ready` or `Failed`.
Initialization, identity, context and management remain usable when the application
handshake is incompatible. A supported supervisor can replace its worker while
preserving the initialized identity and selected context; use the newly advertised
tool schemas after a tool-list change. No in-flight mutation is replayed.

Same release version with different executable bytes, as with a development
rebuild, has no automatic ordering. Automatic repair requires an incompatible
older daemon and an eligible newer installed release; compatible processes keep
running until explicit activation. Raw WebSocket contexts are unmanaged. SSH
activation needs the intended remote daemon and a compatible local MCP worker;
updating one installation does not update the other. Pre-supervisor MCP hosts need
one host restart, and pre-maintenance daemons require the supported manual
stop/start bootstrap. Report these boundaries rather than promising transparent
recovery from every old executable.

A typical opening sequence is:

```text
list_reviews {"title":"PR #247"}
get_review {"review_id":"REVIEW_ID"}
get_diff {"review_id":"REVIEW_ID","repo_id":"REPO_ID","path":"src/parser.rs"}
get_file {"review_id":"REVIEW_ID","repo_id":"REPO_ID","path":"src/parser.rs","side":"Head","start_line":10,"end_line":35}
```

`list_reviews {}` discovers all workspaces, independent of the server's working
directory. Add `workspace_id` to scope it, `title` for a case-insensitive substring,
or `awaiting` for an exact agent name with a pending request. Rows include
workspace/repository identities, open findings, pending requests and last
committed activity, newest first; archived reviews remain visible. Replies do not
increase finding counts, and deleted/resolved/deferred roots do not count. A
request remains pending until its named agent records a checkpoint explicitly
answering it; this is not an approval signal. The response's `seq` belongs to its
coherent metadata read; still open the review and use that full snapshot's cursor.

Read a complete filtered conversation with:

```text
list_comments {"review_id":"REVIEW_ID","status":"Open"}
list_comments {"review_id":"REVIEW_ID","thread_id":"THREAD_ID"}
list_comments {"review_id":"REVIEW_ID","path":"src/parser.rs","repo_id":"REPO_ID","author":"reviewer-a","since":42}
```

The result joins each thread's ordered `comments` (root first), retains its
`resolution`, and adds a `status` and filtered `summary`. Status is `Open`,
`Resolved`, `Deferred`, `Informational`, or `Deleted`. A deleted root is counted
only as deleted, even when its historical resolution was Open. Outdated anchors
remain distinct from deletion and can still be open findings. Deleted comment
bodies retained in JSON are historical tombstones, not current prose.

Filters combine at thread granularity: a matching reply author or anchor returns
the whole conversation. Names and paths match exactly; `repo_id` disambiguates
identical paths across repositories. `since` is an exclusive committed event
sequence, selecting thread creation/replies, edits/deletes, reanchors,
resolve/reopen/defer and suggestion application. Review renames, viewed marks,
requests and checkpoints alone do not select threads. Future cursors return an
empty result with the actual current `seq`; do not invent a later cursor.

The `summary` counts only returned threads and their comments, including separate
`deleted` root and `deleted_comments` counts. `suggestions` carries original patch
identities and durable receipts for returned comments. Review-wide `requests`,
`checkpoints` and `latest_checkpoints` remain available regardless of the filter.
Save the coherent `seq` with the context and query when polling. Filters inspect
current state: `--open --since` omits a thread that was resolved in the interval.
This is a query, not a removal/change feed. Use unfiltered `since` or
`subscribe_events` to maintain all conversation states; reread the full filtered
list to discover departures. A filtered list is not a complete review snapshot
for maintaining every review field.
Priority remains author prose; the tool does not infer `[P1]` metadata or counts.

Omit both file bounds for full content, or supply both as inclusive 1-based source
lines. Diff display row indices are not anchors.

For a checkout that should have a review, call
`ensure_directory_review {"path":"/work/example-project"}`. It attaches the repo
if needed and creates or reuses an open working-tree review. An omitted base
preserves a matching review's base or uses detection. Explicit `base`/`head` refs
must match for reuse; different refs may create another review. Use returned
workspace/repo/review IDs and then `get_review`; the bootstrap receipt's `seq` is
not a substitute for a review snapshot watermark.

For a deliberately new comparison use `create_review` with a title and targets,
e.g. `{"repo_id":"REPO_ID","base":{"type":"Branch","name":"main"},"head":{"type":"WorkingTree"}}`.
For the same logical review, use `update_review_target`:

```json
{"review_id":"REVIEW_ID","repo_id":"REPO_ID","revision":{"type":"Head","ref_spec":{"type":"Commit","oid":"FULL_COMMIT_OID"}}}
```

Use `type: "Base"` to change the base; a working tree cannot be a base in this
target-update/bootstrap selector. Keep other repositories' targets intact.

For a single review spanning multiple attached repositories, `create_review`
takes a `targets` array with one `repo_id`, `base` and `head` per repository.
Use the returned workspace/repository IDs. The CLI's `review create` selects one
repository; do not invent repeated `--repo` arguments as a multi-target interface.

Start a line finding and continue it using the returned `thread_id`:

```text
add_comment {"review_id":"REVIEW_ID","repo_id":"REPO_ID","path":"src/parser.rs","side":"Head","start_line":18,"end_line":20,"body":"An empty record reaches this index operation and fails. Return the existing empty-input result before indexing."}
reply {"review_id":"REVIEW_ID","thread_id":"THREAD_ID","body":"The guard now handles empty input; the regression test passes at COMMIT_OID."}
```

Omit line bounds for a file anchor; omit the path and all location fields for a
review anchor. For a summary, use `add_comment` with `intent: "Informational"`,
`review_id` and `body` only. `suggest` takes the line location, explanation `body`
and unified-diff `patch`. Neither `add_comment` nor `suggest` accepts an arbitrary
old blob OID, Browse context, caller-chosen comment ID, or historical diff scope.

Thread state operations use the actual thread ID:

```text
resolve {"review_id":"REVIEW_ID","thread_id":"THREAD_ID","resolved":true}
defer {"review_id":"REVIEW_ID","thread_id":"THREAD_ID","reason":"The agreed follow-up is outside this change","tracking_url":"https://example.com/issues/123"}
resolve {"review_id":"REVIEW_ID","thread_id":"THREAD_ID","resolved":false}
```

Only use a real existing tracking URL; omit it when there is none. Informational
threads cannot be resolved, deferred or reopened. Deferral requires an open
finding; reopen a deferred finding before treating it as current work.

To follow the snapshot above, copy its actual `seq` and `context.name`:

```json
{"review_id":"REVIEW_ID","since_seq":42,"since_context":"review-box","timeout_ms":30000,"max":100}
```

Pass every response's `last_seq` into the next `subscribe_events` call, including
after an empty response. `review_id`, `workspace_id` and `awaiting_agent` are
mutually exclusive; omit all for all events. `since_context` is mandatory with a
cursor after switching contexts; supplying it consistently also catches accidental
cross-context reuse. An `awaiting_agent` subscription uses the recipient's exact
`get_session_identity.author.name`. Existing requests for each opened review are
in `get_review`/`list_comments.requests`; discover pending requests across reviews
with `list_reviews {"awaiting":"YOUR_EXACT_AGENT_NAME"}`.

MCP mutation receipts give stable IDs; most include the primary committed `seq`.
`record_checkpoint` returns the checkpoint record with its sequence-derived `id`,
not a separate `seq` field. Keep receipts as evidence, but do not advance an
existing subscription cursor to a mutation's sequence: intervening events from
other participants would be skipped. When only following events caused after that
mutation, its `seq` (or checkpoint ID) is a valid starting point;
to read the mutation itself use an earlier cursor. `get_review` combines a
snapshot with file queries, so concurrent changes require checking target events
and refreshed content rather than assuming all subsequent reads share one state.

## CLI fallback

Use `--agent` on agent calls, including read commands that might bootstrap state.
Set `NITS_AGENT_MODEL` to the actual model and `NITS_SESSION_ID` to this agent's
stable session identity for the run. CLI otherwise uses model `unknown` and an
empty session ID. The invoking human comes from `--user`/`NITS_USER`/`USER` and the
host; do not set it to the agent's name or an invented approving human. For
example, after setting those variables to the real session values:

```sh
nits --agent reviewer-a --context review-box context show
nits --agent reviewer-a --context review-box --json workspace list
nits --agent reviewer-a --context review-box --json review list
nits --agent reviewer-a --context review-box --json review show REVIEW_ID
nits --agent reviewer-a --context review-box diff REVIEW_ID src/parser.rs --repo REPO_ID
nits --agent reviewer-a --context review-box show REVIEW_ID src/parser.rs --repo REPO_ID --side head
```

Context precedence is ad-hoc socket/data-dir/daemon-URL flags or their environment
equivalents, then `--context`, `NITS_CONTEXT`, persisted default, implicit local.
Inspect `context show`, especially before opening a portable reference. Prefer a
per-command context for this task; `context use` changes the persisted default
for future processes. MCP switches and CLI defaults do not move each other.

Discover remote reviews without a checkout with
`nits --context review-box review list --all --title 'PR #247'`; add
`--awaiting reviewer-a` for pending requests to that exact recipient, or replace
`--all` with `--workspace WORKSPACE_ID` to scope discovery. Plain `review list`
also defaults to all workspaces. `--json` returns review rows with workspace,
finding, request and activity metadata.

Bootstrap with `nits --agent reviewer-a --context review-box --json /work/example-project --headless`.
For an explicitly new review, use `review create --base main --head worktree`
with `--workspace WORKSPACE_ID` and `--repo REPO_ID` when inference is ambiguous.
CLI refs accept branch names, `tag:NAME`, full commit OIDs, `HEAD`, `upstream`, and
`worktree`. For updating targets and requesting/checking revisions, see the
[revision workflow](revisions.md).

Apply the same identity/context flags to these commands:

```sh
nits comment add REVIEW_ID --repo REPO_ID --path src/parser.rs --side head --lines 18-20 --body 'Explain the concrete concern'
nits comment add REVIEW_ID --intent informational --body 'Checked the parser changes; one empty-input finding remains open.'
nits comment reply REVIEW_ID THREAD_ID --body 'Describe the fix, checked revision and verification'
nits comment defer REVIEW_ID THREAD_ID --reason 'Agreed follow-up outside this change'
nits comment reopen REVIEW_ID THREAD_ID
nits comment resolve REVIEW_ID THREAD_ID
nits --json events --review REVIEW_ID --since SNAPSHOT_SEQ --follow
```

`comment add --patch` takes unified-diff text, not a patch filename. Pass it as
`--patch="$patch"` so the leading `---` is parsed as the value. `--line` or
`--lines` requires `--path`. Replies use the thread ID, not the reply comment ID.
Use `nits comment list REVIEW_ID --open` (alias for `--status open`), or select
`--status resolved|deferred|informational|deleted`. Combine `--thread THREAD_ID`,
`--path src/parser.rs`, `--repo REPO_ID`, `--author EXACT_NAME`, and `--since SEQ`.
`--open` and `--status` conflict. Text indents every body line and ends with status
and comment counts; `--oneline` gives one line per comment with IDs, status,
anchor and first body line. JSON is now a named joined object, replacing the old
`[threads, comments]` tuple, with the same filters and cursor semantics as MCP.
`review show --json` still supplies the complete review snapshot.

CLI `events --follow` internally resumes long-polls from `last_seq`, but prints
events only, not that watermark. Save the last **processed event's** `seq` and
the selected context/scope when managing a restart; keep the original cursor if
no events arrived. This may replay already-seen events, which can be deduplicated.
Choose only one scope (`--review`, `--workspace`, or `--awaiting`); a requests-only
follow does not receive thread replies. On process failure, restart from the saved
cursor with the same context. Stop/reap a follower started for a completed task.

## CLI maintenance within the task

Use the same context and complete author identity when correcting an existing
comment. `comment edit REVIEW_ID COMMENT_ID --body 'Corrected explanation'`
changes prose, preserving its anchor and suggestion patch; `comment delete
REVIEW_ID COMMENT_ID` leaves a tombstone and event history. Only the complete
original author can edit/delete: matching an agent display name alone is not
enough. Retain model, session, invoking human and origin. An MCP-authored comment
cannot be edited as a CLI author merely by copying its name.

`review rename REVIEW_ID 'New title'` preserves status and targets. `review
archive REVIEW_ID` preserves discussion; `review reopen REVIEW_ID` first checks
that the refs resolve. `review set-base REVIEW_ID main --repo REPO_ID` changes
only that repo's base; a working tree cannot be the base. `review delete REVIEW_ID`
removes it from listings with no undelete, while retaining history. `workspace
rename WORKSPACE_ID 'New name'` preserves membership; `workspace detach
WORKSPACE_ID REPO_ID` removes membership and retains checkout files/history.
Use these maintenance actions only when they are part of the requested task.

## Uncertain mutation outcomes

On disconnect, an error can mean the daemon committed the mutation but its reply
was lost. MCP reconnects before the next call and does not replay interrupted
calls. Inspect the fresh snapshot and replay from the previous cursor; look for
the intended comment/request/state with this agent's provenance before retrying.
If success still cannot be distinguished from failure, report uncertainty and
avoid another potentially duplicate mutation until reconciled.

The MCP and CLI convenience commands allocate mutation IDs internally and expose
receipts, not a caller-supplied replay key. Repeating a CLI invocation or MCP tool
call is a new operation. The protocol has client-generated IDs and sequence
numbers for attribution and correlation, but no general durable mutation
idempotency guarantee. Do not invent an `idempotency_key` argument or claim that
reusing a client sequence or re-running a convenience call is safe.

Suggestions remain bound to their original comment, blob and patch even after a
prose edit or reanchor. The UI can preview and explicitly apply them; current
CLI/MCP suggestion commands create suggestions, not apply them. A durable applied
receipt proves application. Proposed bytes without a receipt, or a generic error
after an attempted apply, leave the outcome uncertain: filesystem replacement and
event persistence are separate steps. Inspect and reconcile; do not automatically
replay an uncertain apply.
