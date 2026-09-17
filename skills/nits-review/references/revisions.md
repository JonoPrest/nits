# Revisions, findings and handoff references

## Requested and checked revisions

`get_review` and `list_comments` retain review requests and checkpoint history.
Match a request's `recipient` against the exact agent name. Its stable `id`,
`requester`, `note`, `created` and `targets` survive disconnected sessions.
`targets` is either `Captured { targets }` or explicitly `Unknown` for historical
requests; never fill unknown provenance with today's head.

To request a different revision, first use `update_review_target` with the real
ref, then `request_review {"review_id":"REVIEW_ID","agent":"reviewer-a","note":"Please check the empty-input fix"}`.
The request captures the then-resolved repository/base/head identities. Mentioning
a commit in the note does not select it. Read the resulting retained request to
confirm which targets were captured. CLI equivalents are:

```sh
nits review set-head REVIEW_ID FULL_COMMIT_OID --repo REPO_ID
nits review request REVIEW_ID reviewer-a --note 'Please check the empty-input fix'
```

Use the agent identity/context flags from [interaction mechanics](interaction.md).

Inspect requested content with `get_diff` using
`scope: {"type":"Requested","request_id":REQUEST_ID}`. CLI `files REVIEW_ID
--request REQUEST_ID` lists that comparison's files, and `diff REVIEW_ID PATH
--repo REPO_ID --request REQUEST_ID` reads them. Request/checkpoint IDs are the
returned sequence-derived values, not made-up comment ULIDs. `get_file` still
reads the current review side; it is not a historical request read. Use available
historical content or the exact git object in the corresponding repository when
more original context is necessary; do not substitute a moving working tree.

After actually inspecting a captured set, call `record_checkpoint` with
`review_id`, the exact `targets` inspected, and optional `in_reply_to`:

```text
{"type":"Request","request_id":REQUEST_ID}
{"type":"Checkpoint","checkpoint_id":CHECKPOINT_ID}
```

Take complete target objects from the snapshot/request; do not reconstruct them
from branch names. A checkpoint must cover each review repository exactly once;
do not record a complete check for a partially inspected review. A checkpoint
answering an old request may correctly be stale
against current head. It records inspection, not approval, thread resolution or
human viewed marks. `latest_checkpoints` groups stable reviewer identity (agent
name across sessions), retains full author provenance and reports `Current`,
`Changed` or `Unknown` relative to current resolved identities.

CLI `review check REVIEW_ID --request REQUEST_ID` records that request's captured
targets. `review check REVIEW_ID --current` reads current targets at command
time; only use it when those are the revisions actually checked, and inspect its
result if a target may have moved. For exact older captures prefer MCP's explicit
`targets` input. `--current --answer-request REQUEST_ID` or `--current
--answer-checkpoint CHECKPOINT_ID` links a new current check to that earlier round.

For another round, `get_checkpoint_delta {"review_id":"REVIEW_ID","checkpoint_id":CHECKPOINT_ID}`
lists checked-head to current-head targets and files across the review's repos.
Read each changed file with `get_diff.scope: {"type":"SinceCheckpoint","checkpoint_id":CHECKPOINT_ID}`;
CLI uses `files`/`diff --since-checkpoint CHECKPOINT_ID`. This does not change the
original review base, targets, or conversation. Since current head can move
between reads, verify the captured identities before recording what was checked.
The delta's targets use the previously checked head as their base. When recording
completion of the review round, use the actual review/request target set checked,
preserving its original base, rather than copying those derived delta targets.

## Head changes and original content

Anchors identify blobs, not visual diff rows. A head/base move can reanchor line
comments, mark them `Outdated` with a last-good anchor, or later make them live
again. Reanchoring events can arrive after the target-change event. Neither a
moved line nor an outdated anchor proves the underlying concern was fixed.

Read the original blob/range when interpreting an outdated finding, then compare
with current content and reply in its original thread. A Browse comment remains
pinned to its original blob; its recorded reference is provenance, not a pointer
that follows branch movement. UI thread navigation can reopen that persisted
content, including old working-tree snapshots. MCP `get_file`/CLI `show` reads a
current review side, and their comment commands do not accept Browse provenance
or arbitrary old blob IDs. Do not post historical line numbers against a changed
current file: locate the concern on current content or use a review-level finding
that names the inspected revision and original location.

## Portable discussion references

Use a validated Nits reference for a handoff to a review, finding, or exact reply:

```sh
nits --context review-box reference REVIEW_ID
nits --context review-box reference REVIEW_ID --thread THREAD_ID
nits --context review-box reference REVIEW_ID --comment COMMENT_ID
nits open 'REFERENCE_RETURNED_ABOVE' --headless
```

Apply agent attribution flags as in other CLI calls. `reference` validates its
target; `open --headless` validates without launching a browser. Use the returned
reference instead of hand-assembling or guessing IDs. A comment reference focuses
that exact reply; a thread reference identifies its root. Resolution and anchor
movement do not invalidate stable IDs. Deleted targets fail explicitly.

References encode a saved context, local socket or daemon WebSocket endpoint,
never the browser bridge's temporary port. Named contexts resolve through the
recipient's configuration, so a shared name must identify the intended daemon.
Socket references are machine-local and require an already-running daemon;
explicit context/endpoint flags and environment settings can override reference
routing. Verify selection before using them. Current MCP tools do not offer a
reference-generation/open tool; use the CLI or the UI's Copy reference control.
